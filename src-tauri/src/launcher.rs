//! 開 ComfyUI（生圖引擎）。對照舊版「Anima 生圖.bat」：ComfyUI 沒在跑就在背景開（這裡不開黑視窗，輸出寫到 data\logs\comfyui.log）。
//! 結束程式時只關「這個程式開的」那一個；使用者自己先開好的 ComfyUI 不碰。
//! 啟動方式照 comfy.json（gpu.rs Prefs）：額外參數、「我自己開 ComfyUI」、用 CPU 跑；
//! 另外認得 DirectML 版（自動加 --directml）和 ZLUDA 版（A 卡：照它的 bat 用 zluda.exe 包著開）
use crate::gpu::Prefs;
use crate::pe::win;
use crate::setup::ComfyLayout;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct ComfyProc {
    child: Arc<Mutex<Option<Child>>>,
    diagnostics: Arc<Mutex<Option<std::sync::Weak<crate::diagnostics::Store>>>>,
}

/// ComfyUI 沒開起來、或中途自己關掉的原因（/api/state 在離線時顯示）。生圖站是視窗程式，eprintln 使用者看不到
static NOTE: Mutex<Option<String>> = Mutex::new(None);

pub fn note() -> Option<String> {
    NOTE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_note(n: Option<String>) {
    *NOTE.lock().unwrap_or_else(|e| e.into_inner()) = n;
}

/// 開站時做過的檢查（A 卡 PyTorch 補裝…）：下一次開 ComfyUI 時寫在 comfyui.log 第一段，網友回報只給 log 也看得到
static LOG_NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn log_note(n: String) {
    LOG_NOTES.lock().unwrap_or_else(|e| e.into_inner()).push(n);
}

/// 驅動那一類錯誤要叫使用者看哪一家：backend＝gpu::comfy_backend（這份 ComfyUI 的 PyTorch 是哪一種），
/// machine()＝這台電腦自動判斷該裝哪一版（gpu::auto_plan 的 variant，要偵測顯示卡，用到才問）。
/// "nvidia"＝照舊（N 卡、認不出來）；"amd"＝ROCm（AMD 版）；"intel"＝XPU（Intel 版）；"zluda"＝ZLUDA 版；
/// "mismatch"＝PyTorch 是 NVIDIA 版（CUDA），但這台電腦只有 A 卡；"mismatch_intel"＝只有 Intel Arc
fn driver_hint(backend: Option<&str>, machine: impl FnOnce() -> &'static str) -> &'static str {
    match backend {
        Some("rocm") => "amd",
        Some("xpu") => "intel",
        Some("zluda") => "zluda",
        Some("cuda") => match machine() {
            "amd" => "mismatch",
            "intel" => "mismatch_intel",
            _ => "nvidia",
        },
        _ => "nvidia",
    }
}

/// comfyui.log 的最後一段 → (白話原因, 最後一行錯誤)。找最後一行像「XxxError: …」的，找不到就用最後一行。
/// gpu＝driver_hint 的結果（驅動提示照廠牌分開寫）
fn failure_text(log_tail: &str, gpu: &str) -> (String, String) {
    let lines: Vec<&str> = log_tail.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let is_err = |l: &str| {
        let head = l.split(':').next().unwrap_or("");
        !head.contains(' ') && (head.ends_with("Error") || head.ends_with("Exception") || head.ends_with("Exit"))
    };
    let last = lines.iter().rev().find(|l| is_err(l)).or(lines.last()).copied().unwrap_or("");
    let low = log_tail.to_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| low.contains(k));
    const NVIDIA_ERR: &[&str] = &["torch not compiled with cuda", "no kernel image", "cuda driver version is insufficient", "found no nvidia driver", "nvidia driver on your system is too old"];
    // ROCm（AMD 版）找不到顯示卡、這張卡沒有對應的程式（架構選錯）、驅動太舊載不起 amdhip64
    const AMD_ERR: &[&str] = &["no hip gpus are available", "hip error", "hiperror", "invalid device function", "invalid kernel file", "amdhip64"];
    // XPU（Intel 版）找不到顯示卡、Level Zero／SYCL 執行階段出錯（多半是驅動太舊）
    const INTEL_ERR: &[&str] = &["no xpu devices", "xpu device not", "torch not compiled with xpu", "level_zero", "ze_result", "ur_result_error", "pi_error"];
    // ZLUDA：HIP SDK 跟 ZLUDA 版本對不上（0xC0000135＝找不到 DLL）、cuBLAS 不支援
    const ZLUDA_ERR: &[&str] = &["cublas_status", "0xc0000135", "nccl.dll"];
    // PyTorch 說「…with CUDA capability sm_61 is not compatible…」：sm_75 以前＝GTX 10 系列以前的舊卡，新版 PyTorch（CUDA 13）不支援
    // （RTX 50 配舊 PyTorch 也是這句，但那是 sm_120，照舊走「更新驅動」）
    let old_sm = low.split("cuda capability sm_").skip(1)
        .any(|s| s.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u32>().is_ok_and(|n| n < 75));
    let hint = if let Some(m) = last.split("No module named ").nth(1) {
        format!(
            "少了 Python 套件 {}（多半是某個自訂節點要的）：用 ComfyUI Manager 修復那個節點，或先把它移出 custom_nodes 資料夾",
            m.trim_matches(['\'', '"', '.'])
        )
    } else if gpu == "amd" && has(&[NVIDIA_ERR, AMD_ERR].concat()) {
        "AMD 顯示卡用不起來（A 卡是實驗性支援）：先把 AMD Adrenalin 驅動更新到最新再開一次；還是不行的話，到「設定 → 安裝與修復…」第二頁看「A 卡架構」選得對不對（選錯會出現 invalid device function），或改用「雲端顯卡」".into()
    } else if gpu == "intel" && has(INTEL_ERR) {
        "Intel 顯示卡用不起來（Intel 版是實驗性支援）：先把 Intel Arc 顯示卡驅動更新到最新再開一次；UHD、Iris Xe 內顯不支援，可以改用「雲端顯卡」".into()
    } else if gpu == "zluda" && has(&[NVIDIA_ERR, AMD_ERR, ZLUDA_ERR].concat()) {
        "ZLUDA 版 ComfyUI 用不起來：確認 AMD HIP SDK 跟 ZLUDA 的版本對得上（HIP_PATH 要設好）、AMD 驅動夠新；zluda 資料夾裡的 nccl.dll 不見的話，多半是被防毒刪掉，要把資料夾加進排除清單".into()
    } else if gpu == "mismatch" && has(NVIDIA_ERR) {
        "這份 ComfyUI 的 PyTorch 是 NVIDIA 版，這台電腦是 AMD 顯示卡：到「設定 → 安裝與修復…」第二頁把「ComfyUI 版本」選成 AMD（實驗性），換一個新位置重新安裝，或換一份支援 AMD 的 ComfyUI".into()
    } else if gpu == "mismatch_intel" && has(NVIDIA_ERR) {
        "這份 ComfyUI 的 PyTorch 是 NVIDIA 版，這台電腦是 Intel 顯示卡：到「設定 → 安裝與修復…」第二頁把「ComfyUI 版本」選成 Intel（實驗性），換一個新位置重新安裝".into()
    } else if gpu == "nvidia" && old_sm {
        "這張 NVIDIA 顯示卡比較舊（GTX 10 系列以前），這份 ComfyUI 的 PyTorch 不支援它：到「設定 → 安裝與修復…」第二頁把「ComfyUI 版本」選成「NVIDIA 舊卡」，換一個新位置重新安裝（會下載官方給舊卡的版本）".into()
    } else if has(NVIDIA_ERR) {
        "顯示卡驅動太舊，或 PyTorch 跟這張顯示卡不合：先更新 NVIDIA 驅動再開一次".into()
    } else if has(&["address already in use", "10048", "only one usage of each socket address"]) {
        "連接埠被別的程式占用：關掉另一個 ComfyUI（或占用的程式）再開一次".into()
    } else if has(&["out of memory", "memoryerror", "paging file is too small", "os error 1455"]) {
        "記憶體不夠：關掉其他吃記憶體的程式，或把 Windows 虛擬記憶體（分頁檔）調大".into()
    } else {
        "看下面的錯誤，或到「設定 → 安裝與修復…」修復 ComfyUI".into()
    };
    (hint, last.chars().take(300).collect())
}

/// 盯著自己開的 ComfyUI：還沒開好就結束、或開好之後自己關掉（不是生圖站關的），把原因記到 NOTE。
/// backend＝gpu::comfy_backend（PyTorch 是 NVIDIA 版、AMD 版、Intel 版還是 ZLUDA，驅動提示才講得對）
fn watch(who: String, child: Arc<Mutex<Option<Child>>>, log: PathBuf, backend: Option<&'static str>, diagnostics: Arc<Mutex<Option<std::sync::Weak<crate::diagnostics::Store>>>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3));
        let (pid,code) = {
            let mut g = child.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_mut().map(Child::try_wait) {
                None => return, // 生圖站自己關的（stop 把它拿走了）
                Some(Ok(None)) => continue,
                Some(Ok(Some(st))) => (g.as_ref().map(|c|c.id()).unwrap_or(0),st.code()),
                Some(Err(_)) => return,
            }
        };
        let tail = (||->std::io::Result<String> {
            use std::io::{Read,Seek,SeekFrom};
            let mut f=File::open(&log)?; let len=f.metadata()?.len();f.seek(SeekFrom::Start(len.saturating_sub(64*1024)))?;
            let mut bytes=vec![];f.take(64*1024).read_to_end(&mut bytes)?;Ok(String::from_utf8_lossy(&bytes).into_owned())
        })().unwrap_or_default();
        if let Some(store)=diagnostics.lock().unwrap_or_else(|e|e.into_inner()).as_ref().and_then(std::sync::Weak::upgrade) {store.process_exit(pid,code,&tail);}
        let gpu = driver_hint(backend, || {
            let (gpus, ok) = crate::gpu::detect(false);
            crate::gpu::auto_plan(&gpus, ok).variant
        });
        let (hint, last) = failure_text(&tail, gpu);
        let code = code.map(|c| format!("（結束代碼 {c}）")).unwrap_or_default();
        let last = if last.is_empty() { String::new() } else { format!("。最後的錯誤：{last}") };
        set_note(Some(format!("{who} 關掉了{code}：{hint}{last}。完整紀錄在 {}", log.display())));
        return;
    });
}

/// port 上有沒有東西在聽（ComfyUI 已經開著）
pub fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(800))
        .is_ok()
}

fn comfy_reply(v: &serde_json::Value) -> bool {
    v.get("system")
        .and_then(|s| s.get("comfyui_version"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty())
}

fn norm_path(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// 這份 ComfyUI 吃的資料夾（input／output／models 所在）：啟動參數有 --base-directory 就是它指的那個，沒有就是 main.py 那層。
/// 同一個 main.py 帶別的 --base-directory 開出來的是另一份資料（例如隔離測試用的），input 不在生圖站存圖的地方，所以不能算同一份
fn argv_is_ours(argv: &[String], main: &str, root: &str) -> bool {
    match argv.windows(2).find(|w| w[0] == "--base-directory") {
        Some(w) => w[1] == root,
        None => argv.iter().any(|a| a == main),
    }
}

/// 這份 ComfyUI 已經被別的啟動器開在其他 port（官方桌面版預設 8000；秋葉、StabilityMatrix 也能改 port）：找出來直接用。
/// 不然生圖站會在 8188 再開第二個，同一張顯卡各載一份模型，顯存一下就爆。
/// 只認「同一份安裝」：/system_stats 的 argv 是同一個 main.py（或 --base-directory 同一個資料夾），
/// 或 /internal/folder_paths 的模型資料夾在這份 ComfyUI 底下。別份 ComfyUI 不碰
pub fn find_running(comfy_dir: &Path, ports: &[u16]) -> Option<u16> {
    let layout = crate::setup::comfy_layout(comfy_dir)?;
    let norm = norm_path;
    let (main, root) = (norm(&layout.main.to_string_lossy()), norm(&layout.root.to_string_lossy()));
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let get = |port: u16, path: &str| -> Option<serde_json::Value> {
        client.get(format!("http://127.0.0.1:{port}{path}")).send().ok()?.json().ok()
    };
    ports.iter().copied().find(|&port| {
        if !port_open(port) {
            return false;
        }
        let Some(v) = get(port, "/system_stats").filter(comfy_reply) else { return false };
        let argv: Vec<String> = v["system"]["argv"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).map(norm).collect();
        if argv_is_ours(&argv, &main, &root) {
            return true;
        }
        // argv 是相對路徑（ComfyUI\main.py）時看模型資料夾：{"checkpoints": [[路徑…], [副檔名…]], …}
        get(port, "/internal/folder_paths").is_some_and(|f| {
            f.as_object().into_iter().flatten().flat_map(|(_, v)| v.get(0).and_then(|p| p.as_array()).into_iter().flatten()).any(|p| {
                p.as_str().is_some_and(|p| norm(p).starts_with(&format!("{root}\\")))
            })
        })
    })
}

/// port 上開著的是不是「確定是別份」ComfyUI：/system_stats 的 argv 寫了完整的 main.py 路徑，而且跟這份不是同一個檔
/// （比真正的位置，junction、大小寫、斜線都不影響）。看不出來（相對路徑、讀不到）一律當作不是別份，照舊直接連
pub fn other_install_on(comfy_dir: &Path, port: u16) -> bool {
    if !port_open(port) || find_running(comfy_dir, &[port]).is_some() {
        return false;
    }
    let Some(layout) = crate::setup::comfy_layout(comfy_dir) else { return false };
    let Ok(mine) = std::fs::canonicalize(&layout.main) else { return false };
    let Some(v) = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("http://127.0.0.1:{port}/system_stats")).send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .filter(comfy_reply)
    else {
        return false;
    };
    let argv: Vec<&str> = v["system"]["argv"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).collect();
    // 同一個 main.py 但 --base-directory 指到別處：input／output 不是生圖站讀寫的那份，一樣算別份
    let root = norm_path(&layout.root.to_string_lossy());
    if argv.windows(2).any(|w| w[0] == "--base-directory" && norm_path(w[1]) != root) {
        return true;
    }
    let theirs = argv
        .into_iter()
        .map(Path::new)
        .find(|p| p.is_absolute() && p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("main.py")))
        .and_then(|p| std::fs::canonicalize(p).ok());
    theirs.is_some_and(|t| t != mine)
}

/// 連接埠被占用不代表它是 ComfyUI；核對 /system_stats 避免誤連其他服務。
fn comfy_running(port: u16) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder()
        .no_proxy() // 本機：不能被 HTTP_PROXY 送進代理
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    for attempt in 0..3 {
        if let Ok(v) = client
            .get(format!("http://127.0.0.1:{port}/system_stats"))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json::<serde_json::Value>())
        {
            if comfy_reply(&v) {
                return true;
            }
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    false
}

/// ZLUDA 版自己的 bat 預設參數（patientx/ComfyUI-Zluda 的 comfyui-n.bat，2026-10-21）：找不到 bat 時用
const ZLUDA_ARGS: &[&str] = &["--use-quad-cross-attention", "--reserve-vram", "0.9", "--disable-async-offload", "--disable-pinned-memory"];
/// 要另外加的環境變數（名稱, 值）
type Env = Vec<(String, String)>;

static PCI_ORDER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 設定裡有開本機別張卡的節點：主節點的 ComfyUI 也照 nvidia-smi 的卡號順序開（main.rs 開 ComfyUI 之前設）
pub fn set_pci_order(on: bool) {
    PCI_ORDER.store(on, std::sync::atomic::Ordering::Relaxed);
}
fn pci_order() -> bool {
    PCI_ORDER.load(std::sync::atomic::Ordering::Relaxed)
}

/// 從 ZLUDA 的 bat 帶過來的環境變數（其他的不碰）
const ZLUDA_ENV: &[&str] = &[
    "ZLUDA_COMGR_LOG_LEVEL",
    "MIOPEN_FIND_MODE",
    "MIOPEN_LOG_LEVEL",
    "TRITON_OVERRIDE_ARCH",
    "HIP_VISIBLE_DEVICES",
    "ROCR_VISIBLE_DEVICES",
    "FLASH_ATTENTION_TRITON_AMD_ENABLE",
];

/// ZLUDA 版的 bat（comfyui-user.bat＞comfyui-n.bat＞comfyui.bat）裡的 COMMANDLINE_ARGS 和環境變數。
/// 生圖站自己管的（--port、資料夾）和 --auto-launch 拿掉；沒有 bat＝None
fn zluda_bat(dir: &Path) -> Option<(Vec<String>, Env)> {
    let text = ["comfyui-user.bat", "comfyui-n.bat", "comfyui.bat"].iter().find_map(|b| std::fs::read(dir.join(b)).ok())?;
    let (mut args, mut env) = (None, vec![]);
    for line in String::from_utf8_lossy(&text).lines().map(str::trim) {
        let low = line.to_lowercase();
        if !low.starts_with("set ") {
            continue; // ::、rem 註解和其他指令
        }
        let kv = line[4..].trim().trim_matches('"');
        let Some((k, v)) = kv.split_once('=') else { continue };
        let (k, v) = (k.trim().to_uppercase(), v.trim().trim_matches('"'));
        if v.contains('%') {
            continue; // 引用別的變數的不照抄
        }
        if k == "COMMANDLINE_ARGS" {
            args = crate::gpu::split_args(v).ok();
        } else if ZLUDA_ENV.contains(&k.as_str()) {
            env.retain(|(x, _): &(String, String)| *x != k);
            env.push((k, v.to_string()));
        }
    }
    let mut out = vec![];
    let mut it = args.unwrap_or_else(|| ZLUDA_ARGS.iter().map(|s| s.to_string()).collect()).into_iter().peekable();
    while let Some(a) = it.next() {
        let name = a.split('=').next().unwrap_or("").to_string();
        if crate::gpu::MANAGED_ARGS.contains(&name.as_str()) || name == "--port" {
            if !a.contains('=') && it.peek().is_some_and(|n| !n.starts_with("--")) {
                it.next(); // 連同它的值
            }
            continue;
        }
        if name == "--auto-launch" || name == "--windows-standalone-build" {
            continue;
        }
        out.push(a);
    }
    Some((out, env))
}

/// 參數分組（同一組的只能留一個：互斥的旗標，或同一個旗標）：使用者的額外參數蓋掉生圖站自動加的
fn arg_group(flag: &str) -> String {
    let f = flag.split('=').next().unwrap_or(flag);
    if f.starts_with("--use-") && f.ends_with("-attention") {
        return "attention".into();
    }
    if ["--cpu", "--lowvram", "--novram", "--normalvram", "--highvram", "--gpu-only"].contains(&f) {
        return "vram".into();
    }
    if ["--cuda-malloc", "--disable-cuda-malloc"].contains(&f) {
        return "cuda-malloc".into();
    }
    f.to_string()
}

/// 生圖站自動加的參數（auto）＋使用者的額外參數（user）：同一組的以使用者的為準。旗標後面不是 -- 開頭的算它的值
fn merge_args(auto: &[String], user: &[String]) -> Vec<String> {
    let groups: Vec<String> = user.iter().filter(|a| a.starts_with("--")).map(|a| arg_group(a)).collect();
    let mut out = vec![];
    let mut skip = false;
    for a in auto {
        if a.starts_with("--") {
            skip = groups.contains(&arg_group(a));
        }
        if !skip {
            out.push(a.clone());
        }
    }
    out.extend(user.iter().cloned());
    out
}

/// 開 ComfyUI 的方式：程式（python，或 ZLUDA 版的 zluda.exe）、參數、另外要加的環境變數
#[derive(Debug, PartialEq)]
struct Launch {
    program: PathBuf,
    args: Vec<OsString>,
    env: Env,
}

/// 照 ComfyUI 的擺法、comfy.json 的設定、PyTorch 的種類（backend＝gpu::comfy_backend）組出啟動指令。
/// zluda＝ZLUDA 版的 zluda.exe。extra_yaml＝生圖站的外部模型資料夾設定、desktop_yaml＝官方桌面版的模型路徑設定。
/// N 卡、沒改設定時跟以前一模一樣
/// 別張卡的 ComfyUI 用自己的資料庫：拿掉原本的 --database-url（使用者全域參數裡的那個＝主節點的），換成 db
fn with_node_db(args: Vec<OsString>, db: &Path) -> Vec<OsString> {
    let mut out = vec![];
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let t = a.to_string_lossy();
        if t == "--database-url" {
            it.next();
            continue;
        }
        if t.starts_with("--database-url=") {
            continue;
        }
        out.push(a);
    }
    out.push("--database-url".into());
    out.push(format!("sqlite:///{}", db.to_string_lossy().replace('\\', "/")).into());
    out
}

#[cfg(test)]
fn launch(
    layout: &ComfyLayout,
    port: u16,
    prefs: &Prefs,
    backend: Option<&str>,
    zluda: Option<&Path>,
    extra_yaml: Option<&Path>,
    desktop_yaml: Option<&Path>,
) -> Result<Launch, String> {
    launch_node(layout, port, prefs, backend, zluda, extra_yaml, desktop_yaml, None)
}

/// 同 launch；node＝(卡號, 這個節點的額外參數)＝本機另一張卡的 ComfyUI（設定 → 工作節點）：
/// 加 --cuda-device 卡號、節點的參數排在使用者全域參數後面（同組的以節點為準），卡號照 nvidia-smi 的順序（CUDA_DEVICE_ORDER=PCI_BUS_ID）
#[allow(clippy::too_many_arguments)]
fn launch_node(
    layout: &ComfyLayout,
    port: u16,
    prefs: &Prefs,
    backend: Option<&str>,
    zluda: Option<&Path>,
    extra_yaml: Option<&Path>,
    desktop_yaml: Option<&Path>,
    node: Option<(u32, &str)>,
) -> Result<Launch, String> {
    let mut args: Vec<OsString> = vec!["-s".into(), layout.main.clone().into()];
    // 免安裝版照官方 run_nvidia_gpu.bat 加這個；其他照一般方式開。
    // AMD、Intel 免安裝版的 run_amd_gpu.bat／run_intel_gpu.bat（ComfyUI v0.38.0 .ci）參數一樣、也沒設環境變數，所以不用分開
    if layout.portable {
        // --windows-standalone-build 會順便打開瀏覽器（ComfyUI 的 auto_launch），生圖站自己就是介面，關掉
        args.extend(["--windows-standalone-build".into(), "--disable-auto-launch".into()]);
    }
    // 官方桌面版：程式和資料分開放，跟桌面版一樣用 --base-directory 指到資料，再帶它自己的模型路徑設定
    if layout.desktop {
        args.extend(["--base-directory".into(), layout.root.clone().into()]);
        if let Some(y) = desktop_yaml {
            args.extend(["--extra-model-paths-config".into(), y.into()]);
        }
    }
    args.extend(["--preview-method".into(), "auto".into(), "--port".into(), port.to_string().into()]);
    // 外部模型資料夾（設定 → 模型與工作流）：生圖站自己的 yaml，使用者的 extra_model_paths.yaml 照樣有效
    if let Some(y) = extra_yaml {
        args.extend(["--extra-model-paths-config".into(), y.into()]);
    }
    let mut user = crate::gpu::extra_args(&prefs.extra_args)?;
    let mut env = vec![];
    if let Some((gpu, node_args)) = node {
        if prefs.cpu || !matches!(backend, None | Some("cuda")) {
            return Err("一台電腦開好幾張卡只支援 NVIDIA 顯示卡（PyTorch 的 CUDA 版）".into());
        }
        let own = crate::gpu::extra_args(node_args)?;
        if own.iter().any(|a| a.starts_with("--cuda-d")) {
            return Err("卡號由「工作節點」的設定決定，不能放在額外參數裡".into());
        }
        let mut n = vec!["--cuda-device".to_string(), gpu.to_string()];
        n.extend(own);
        user = merge_args(&user, &n);
        env.push(("CUDA_DEVICE_ORDER".to_string(), "PCI_BUS_ID".to_string()));
    }
    let mut auto: Vec<String> = vec![];
    if prefs.cpu {
        auto.push("--cpu".into()); // 沒有能用的顯示卡（精靈裝的是 CPU 版）
    }
    let mut program = layout.python.clone();
    match backend {
        Some("directml") if !prefs.cpu => auto.push("--directml".into()),
        Some("zluda") => {
            let Some(exe) = zluda else {
                return Err("這份 ComfyUI 是 ZLUDA 版（A 卡），要用它自己的啟動器開（例如绘世启动器）：開好之後生圖站會自己連上；也可以到「設定 → ComfyUI 啟動方式」打開「我自己開 ComfyUI」".into());
            };
            // 照它的 bat：zluda.exe -- python main.py 參數…（cwd 是 ComfyUI 資料夾，bat 裡都是相對路徑）
            let (bat_args, bat_env) = zluda_bat(&layout.root).unwrap_or_else(|| (ZLUDA_ARGS.iter().map(|s| s.to_string()).collect(), vec![]));
            auto.extend(bat_args);
            // 上游 ComfyUI 對 +cu 的 PyTorch 預設開 cudaMallocAsync，ZLUDA 不支援（patientx 版自己關掉了，多加也無妨）
            auto.push("--disable-cuda-malloc".into());
            env = bat_env;
            if !env.iter().any(|(k, _)| k == "ZLUDA_COMGR_LOG_LEVEL") {
                env.push(("ZLUDA_COMGR_LOG_LEVEL".into(), "1".into()));
            }
            program = exe.to_path_buf();
            args.splice(0..0, ["--".into(), layout.python.clone().into()]);
        }
        _ => {}
    }
    args.extend(merge_args(&auto, &user).into_iter().map(OsString::from));
    // 使用者自己設了的環境變數照他的
    env.retain(|(k, _)| std::env::var_os(k).is_none());
    Ok(Launch { program, args, env })
}

/// A 卡（ROCm 版 PyTorch）的電腦有好幾張 A 卡（獨顯＋CPU 內顯最常見）時，ComfyUI 要用哪一號，和原因（寫進 comfyui.log）。
/// 只有一張 A 卡、使用者自己設了 HIP_VISIBLE_DEVICES 或 --cuda-device、問不到 PyTorch＝None（照原樣開）。
/// 要 import torch 一次（約幾秒到幾十秒），所以只在偵測到兩張以上 A 卡時才問
fn amd_device(layout: &ComfyLayout, prefs: &Prefs) -> Option<(usize, String)> {
    if std::env::var_os("HIP_VISIBLE_DEVICES").is_some() || prefs.extra_args.contains("--cuda-d") {
        return None;
    }
    let (gpus, _) = crate::gpu::detect(false);
    if gpus.iter().filter(|g| g["vendor"] == "amd").count() < 2 {
        return None;
    }
    let plan = crate::gpu::current_plan();
    let probe = crate::gpu::hip_probe(layout)?;
    let i = crate::gpu::pick_hip_device(&probe, plan.gfx.as_deref())?;
    let all: Vec<String> = probe["dev"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(n, d)| format!("{n}＝{}（{}）", d["name"].as_str().unwrap_or("?"), d["arch"].as_str().unwrap_or("?")))
        .collect();
    Some((i, format!("ROCm 看到好幾張 A 卡：{}", all.join("、"))))
}

impl ComfyProc {
    /// comfy_dir＝設定檔記的 ComfyUI 位置（免安裝版或自己 clone 的，見 setup::comfy_layout）。ComfyUI 已經開著回傳 None
    pub fn start_if_needed(
        comfy_dir: &Path,
        log_dir: &Path,
        port: u16,
    ) -> Result<Option<ComfyProc>, String> {
        Self::spawn(comfy_dir, log_dir, port, None)
    }

    /// 本機另一張卡的 ComfyUI（設定 → 工作節點）：同一份 ComfyUI、另一個 port、--cuda-device 卡號；紀錄檔 comfyui_gpu卡號.log。
    /// 那個 port 已經有 ComfyUI＝None（直接用它）
    pub fn start_extra(comfy_dir: &Path, log_dir: &Path, port: u16, gpu: u32, args: &str) -> Result<Option<ComfyProc>, String> {
        Self::spawn(comfy_dir, log_dir, port, Some((gpu, args)))
    }

    fn spawn(comfy_dir: &Path, log_dir: &Path, port: u16, node: Option<(u32, &str)>) -> Result<Option<ComfyProc>, String> {
        if port_open(port) {
            return if comfy_running(port) {
                Ok(None)
            } else {
                Err(format!(
                    "連接埠 {port} 已被其他程式使用，請關閉占用程式，或更換 ComfyUI 連接埠"
                ))
            };
        }
        let prefs = crate::gpu::prefs();
        if prefs.external {
            return Err(format!(
                "設定成「我自己開 ComfyUI」：請用你平常的方式把 ComfyUI 開在連接埠 {port}，開好之後生圖站會自己連上"
            ));
        }
        let Some(layout) = crate::setup::comfy_layout(comfy_dir) else {
            return Err(format!(
                "{}；ComfyUI 是不是被移動或刪除了？",
                crate::setup::comfy_problem(comfy_dir).unwrap_or_default()
            ));
        };
        let backend = crate::gpu::comfy_backend(&layout);
        if layout.desktop {
            // --base-directory 底下沒有 custom_nodes 的話 ComfyUI 一開就當掉（main.py 的 execute_prestartup_script）
            let _ = std::fs::create_dir_all(layout.root.join("custom_nodes"));
        }
        let desktop_yaml = std::env::var_os("APPDATA")
            .map(|a| PathBuf::from(a).join("ComfyUI").join("extra_models_config.yaml"))
            .filter(|y| y.is_file());
        let extra = Some(crate::model_dirs::yaml_path(&layout.root)).filter(|y| y.is_file());
        let zluda = crate::gpu::zluda_exe(&layout);
        let mut l = launch_node(&layout, port, &prefs, backend, zluda.as_deref(), extra.as_deref(), desktop_yaml.as_deref(), node)?;
        if let Some((g, _)) = node {
            // 同一份 ComfyUI 的 user\comfyui.db 只准一台開（第二台會 ASSETS_STARTUP_FAILED: in_use 直接結束）：每張卡自己一個
            let dir = log_dir.parent().unwrap_or(log_dir).join("comfy_nodes");
            std::fs::create_dir_all(&dir).map_err(|e| format!("建立 {} 失敗：{e}", dir.display()))?;
            l.args = with_node_db(l.args, &dir.join(format!("gpu{g}.db")));
        }
        // 有開別張卡：主節點也照 nvidia-smi 的順序（不然 PyTorch 的 0 號＝最快那張，可能跟別的節點搶同一張）
        if node.is_none() && pci_order() {
            l.env.push(("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()));
            l.env.retain(|(k, _)| std::env::var_os(k).is_none());
        }
        let log_name = match node {
            Some((g, _)) => format!("comfyui_gpu{g}.log"),
            None => "comfyui.log".into(),
        };
        std::fs::create_dir_all(log_dir)
            .map_err(|e| format!("建立 {} 失敗：{e}", log_dir.display()))?;
        let mut log = File::create(log_dir.join(&log_name))
            .map_err(|e| format!("建立 ComfyUI 紀錄檔失敗：{e}"))?;
        if node.is_none() {
            use std::io::Write;
            for n in std::mem::take(&mut *LOG_NOTES.lock().unwrap_or_else(|e| e.into_inner())) {
                let _ = writeln!(log, "[繪匣] {n}");
            }
        }
        if node.is_none() && !prefs.cpu && backend == Some("rocm") {
            use std::io::Write;
            // 回報常常只給 comfyui.log：偵測到的 A 卡、精靈選的架構、PyTorch 實際裝好的架構先寫一行
            let (gpus, _) = crate::gpu::detect(false);
            let cards: Vec<String> = gpus
                .iter()
                .filter(|g| g["vendor"] == "amd")
                .map(|g| format!("{}（{}）", g["name"].as_str().unwrap_or("?"), g["gfx"].as_str().unwrap_or("認不出架構")))
                .collect();
            let _ = writeln!(
                log,
                "[繪匣] A 卡：{}；精靈選的架構：{}；PyTorch {} 裝好的架構：{}",
                if cards.is_empty() { "沒偵測到".into() } else { cards.join("、") },
                if prefs.amd_gfx.is_empty() { "自動偵測" } else { &prefs.amd_gfx },
                crate::gpu::torch_build(&layout.python).unwrap_or_default(),
                crate::gpu::rocm10_devices(&layout.python).join("、"),
            );
            if let Some((i, why)) = amd_device(&layout, &prefs) {
                let _ = writeln!(log, "[繪匣] {why}：只讓 ComfyUI 用第 {i} 號（HIP_VISIBLE_DEVICES={i}）");
                l.env.push(("HIP_VISIBLE_DEVICES".into(), i.to_string()));
            }
        }
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&l.program);
        cmd.args(&l.args).envs(l.env);
        cmd.current_dir(&layout.cwd)
        .env("PYTHONIOENCODING", "utf-8") // 輸出導到檔案時預設是 cp950，遇到特殊字元會出錯
        // UTF-8 模式：自訂節點用 open() 讀 UTF-8 檔沒寫 encoding 時，簡中 Windows（GBK）會 UnicodeDecodeError。
        // 秋葉整合包的啟動器也這樣開；使用者自己設了 PYTHONUTF8 就照他的
        .envs(std::env::var_os("PYTHONUTF8").is_none().then_some(("PYTHONUTF8", "1")))
        .envs(crate::net::child_env()) // PAC／手動代理、Hugging Face 鏡像（自訂節點自己下載模型時用）
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err);
        win::no_window(&mut cmd);
        let child = cmd.spawn().map_err(|e| format!("啟動 ComfyUI 失敗：{e}"))?;
        eprintln!("ComfyUI 啟動中（pid {}）", child.id());
        set_note(None);
        let child = Arc::new(Mutex::new(Some(child)));
        let diagnostics=Arc::new(Mutex::new(None));
        watch(match node { Some((g, _)) => format!("第 {g} 號卡的 ComfyUI"), None => "ComfyUI".into() }, child.clone(), log_dir.join(&log_name), backend,diagnostics.clone());
        Ok(Some(ComfyProc { child,diagnostics }))
    }

    pub fn attach_diagnostics(&self,store:Arc<crate::diagnostics::Store>) {
        *self.diagnostics.lock().unwrap_or_else(|e|e.into_inner())=Some(Arc::downgrade(&store));
    }
    /// 還在跑嗎（啟動失敗、被關掉都算沒在跑）
    pub fn alive(&self) -> bool {
        let mut g = self.child.lock().unwrap_or_else(|e| e.into_inner());
        matches!(g.as_mut().map(|c| c.try_wait()), Some(Ok(None)))
    }

    /// 關掉（連同它開的子程序）
    pub fn stop(&self) {
        if let Some(c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            win::kill(c.id() as i64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognize_comfy_status_only() {
        assert!(comfy_reply(
            &serde_json::json!({"system": {"comfyui_version": "0.38.0"}})
        ));
        assert!(!comfy_reply(&serde_json::json!({"system": {}})));
        assert!(!comfy_reply(&serde_json::json!({"status": "ok"})));
    }

    #[test]
    fn same_main_py_other_base_directory_is_not_ours() {
        let argv = |a: &[&str]| a.iter().map(|s| norm_path(s)).collect::<Vec<_>>();
        let main = norm_path(r"D:\AI\ComfyUI_windows_portable\ComfyUI\main.py");
        let root = norm_path(r"D:\AI\ComfyUI_windows_portable\ComfyUI");
        // 整合包照常開：只有 main.py
        assert!(argv_is_ours(&argv(&[r"D:\AI\ComfyUI_windows_portable\ComfyUI\main.py", "--port", "8188"]), &main, &root));
        // 隔離測試版：同一個 main.py，--base-directory 指到別處（這次的診斷包就是連到這種，input 對不上）
        let other = argv(&[r"D:\AI\ComfyUI_windows_portable\ComfyUI\main.py", "--port", "8189", "--base-directory", r"D:\AI\anima_app\待刪除\av27\comfy"]);
        assert!(!argv_is_ours(&other, &main, &root));
        // 官方桌面版：main.py 在安裝資料夾，靠 --base-directory 指到資料夾
        let desk = norm_path(r"C:\Users\u\AppData\Local\Programs\ComfyUI\resources\ComfyUI\main.py");
        let data = norm_path(r"D:\ComfyData");
        assert!(argv_is_ours(&argv(&[r"C:\Users\u\AppData\Local\Programs\ComfyUI\resources\ComfyUI\main.py", "--base-directory", r"D:\ComfyData/"]), &desk, &data));
        assert!(!argv_is_ours(&argv(&["--port", "8000"]), &desk, &data));
    }

    #[test]
    fn startup_failure_reasons() {
        let log = "Total VRAM 24564 MB\nTraceback (most recent call last):\n  File \"nodes.py\", line 2, in load\n    import cv2\nModuleNotFoundError: No module named 'cv2'\n\nCannot import custom node\n";
        let (hint, last) = failure_text(log, "nvidia");
        assert!(hint.contains("cv2") && last.starts_with("ModuleNotFoundError"), "{hint} / {last}");
        let (hint, _) = failure_text("AssertionError: Torch not compiled with CUDA enabled\n", "nvidia");
        assert!(hint.contains("驅動"), "{hint}");
        assert!(hint.contains("NVIDIA"), "{hint}");
        let (hint, _) = failure_text("OSError: [Errno 10048] error while attempting to bind on address\n", "nvidia");
        assert!(hint.contains("連接埠"), "{hint}");
        let (hint, last) = failure_text("Starting server\nsomething odd happened\n", "nvidia");
        assert!(hint.contains("修復") && last == "something odd happened", "{hint} / {last}");
        assert_eq!(failure_text("", "nvidia").1, "");
        // GTX 10 系列配新版 PyTorch：叫他裝舊卡版；RTX 50 配舊 PyTorch：照舊叫他更新驅動
        let old = "UserWarning: NVIDIA GeForce GTX 1080 with CUDA capability sm_61 is not compatible with the current PyTorch installation.\nRuntimeError: CUDA error: no kernel image is available for execution on the device\n";
        assert!(failure_text(old, "nvidia").0.contains("NVIDIA 舊卡"));
        let new = "UserWarning: NVIDIA GeForce RTX 5090 with CUDA capability sm_120 is not compatible with the current PyTorch installation.\nRuntimeError: CUDA error: no kernel image is available for execution on the device\n";
        assert!(failure_text(new, "nvidia").0.starts_with("顯示卡驅動太舊"));
    }

    /// 真的開一份 ComfyUI，看它關掉時生圖站給的原因（例如在 N 卡電腦上開 AMD 版，或 A 卡電腦上試 AMD 版能不能開）：
    /// ANIMA_TEST_COMFY_DIR＝ComfyUI 資料夾（記錄寫到系統暫存資料夾，port 8189）
    /// cargo test --lib real_comfy_start -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_comfy_start() {
        let Some(dir) = std::env::var_os("ANIMA_TEST_COMFY_DIR") else {
            return;
        };
        let logs = std::env::temp_dir().join("anima_launcher_test_logs");
        let p = ComfyProc::start_if_needed(Path::new(&dir), &logs, 8189).expect("開不了").expect("8189 已經有 ComfyUI");
        for _ in 0..200 {
            std::thread::sleep(Duration::from_secs(3));
            if let Some(n) = note() {
                println!("原因：{n}");
                return;
            }
            if comfy_running(8189) {
                println!("ComfyUI 開起來了（8189），關掉");
                p.stop();
                return;
            }
        }
        p.stop();
        panic!("10 分鐘內沒開好也沒關掉");
    }

    /// 驅動提示照廠牌：AMD 版 ComfyUI（ROCm）講 AMD 驅動和支援的系列；NVIDIA 版裝在只有 A 卡的電腦上講「換 AMD 版」
    #[test]
    fn driver_hints_by_vendor() {
        let hip = "Total VRAM 16368 MB\nRuntimeError: HIP error: invalid device function\n";
        let (hint, last) = failure_text(hip, "amd");
        assert!(hint.contains("AMD") && hint.contains("A 卡架構") && !hint.contains("NVIDIA"), "{hint}");
        assert!(last.starts_with("RuntimeError: HIP error"), "{last}");
        let (hint, _) = failure_text("RuntimeError: No HIP GPUs are available\n", "amd");
        assert!(hint.contains("AMD Adrenalin 驅動"), "{hint}");
        let (hint, _) = failure_text("RuntimeError: Found no NVIDIA driver on your system.\n", "mismatch");
        assert!(hint.contains("AMD（實驗性）") && hint.contains("安裝與修復"), "{hint}");
        let (hint, _) = failure_text("RuntimeError: Found no NVIDIA driver on your system.\n", "mismatch_intel");
        assert!(hint.contains("Intel（實驗性）"), "{hint}");
        let (hint, _) = failure_text("RuntimeError: No XPU devices are available\n", "intel");
        assert!(hint.contains("Intel Arc") && hint.contains("雲端顯卡"), "{hint}");
        let (hint, _) = failure_text("RuntimeError: CUBLAS_STATUS_NOT_SUPPORTED when calling cublasGemmEx\n", "zluda");
        assert!(hint.contains("HIP SDK"), "{hint}");
        // N 卡的提示一個字都沒變
        let (hint, _) = failure_text("RuntimeError: Found no NVIDIA driver on your system.\n", "nvidia");
        assert_eq!(hint, "顯示卡驅動太舊，或 PyTorch 跟這張顯示卡不合：先更新 NVIDIA 驅動再開一次");
        // HIP 錯誤只在 AMD 版才算驅動問題
        let (hint, _) = failure_text(hip, "nvidia");
        assert!(hint.contains("修復"), "{hint}");
        // 少套件優先於驅動
        let (hint, _) = failure_text("ModuleNotFoundError: No module named 'cv2'\nHIP error\n", "amd");
        assert!(hint.contains("cv2"), "{hint}");
        assert_eq!(driver_hint(Some("rocm"), || "nvidia"), "amd");
        assert_eq!(driver_hint(Some("cuda"), || "amd"), "mismatch");
        assert_eq!(driver_hint(Some("cuda"), || "intel"), "mismatch_intel");
        assert_eq!(driver_hint(Some("cuda"), || "nvidia"), "nvidia");
        assert_eq!(driver_hint(Some("cuda"), || "unknown"), "nvidia");
        assert_eq!(driver_hint(None, || "amd"), "nvidia");
        assert_eq!(driver_hint(Some("cpu"), || "amd"), "nvidia");
        assert_eq!(driver_hint(Some("xpu"), || "intel"), "intel");
        assert_eq!(driver_hint(Some("zluda"), || "amd"), "zluda");
    }

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    /// 另一張卡的 ComfyUI：主節點的指令＋--cuda-device＋節點自己的參數（同組的以節點為準），卡號照 nvidia-smi 的順序
    #[test]
    fn extra_args_include_cuda_device() {
        let d = std::env::temp_dir().join(format!("anima_launch_node_{}", std::process::id()));
        let lay = ComfyLayout {
            root: d.join("ComfyUI"),
            main: d.join("ComfyUI").join("main.py"),
            python: d.join("python_embeded").join("python.exe"),
            portable: true,
            desktop: false,
            cwd: d.clone(),
        };
        let p = Prefs { extra_args: "--highvram --fast".into(), ..Default::default() };
        let l = launch_node(&lay, 8199, &p, Some("cuda"), None, None, None, Some((1, "--lowvram"))).unwrap();
        let a: Vec<String> = l.args.iter().map(|x| x.to_string_lossy().to_string()).collect();
        assert_eq!(a[a.len() - 4..], ["--fast", "--cuda-device", "1", "--lowvram"], "--highvram 被節點的 --lowvram 換掉");
        assert!(a.windows(2).any(|w| w == ["--port", "8199"]));
        if std::env::var_os("CUDA_DEVICE_ORDER").is_none() {
            assert!(l.env.contains(&("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into())));
        }
        assert!(launch_node(&lay, 8199, &p, Some("cuda"), None, None, None, Some((1, "--cuda-device 0"))).is_err(), "卡號只能從節點設定給");
        assert!(launch_node(&lay, 8199, &p, Some("directml"), None, None, None, Some((1, ""))).is_err(), "只有 N 卡能指定卡號");
        let cpu = Prefs { cpu: true, ..Default::default() };
        assert!(launch_node(&lay, 8199, &cpu, Some("cuda"), None, None, None, Some((1, ""))).is_err());
        // 每張卡自己的資料庫（同一份 ComfyUI 的 comfyui.db 只准一台開）
        let a = with_node_db(os(&["--fast", "--database-url", "sqlite:///x.db", "--database-url=y"]), Path::new(r"D:\data\comfy_nodes\gpu1.db"));
        assert_eq!(a, os(&["--fast", "--database-url", "sqlite:///D:/data/comfy_nodes/gpu1.db"]));
        // 主節點不變
        assert_eq!(launch_node(&lay, 8188, &Prefs::default(), Some("cuda"), None, None, None, None).unwrap(), launch(&lay, 8188, &Prefs::default(), Some("cuda"), None, None, None).unwrap());
    }

    /// 啟動指令：N 卡沒改設定時跟以前一模一樣；CPU、DirectML、額外參數、ZLUDA
    #[test]
    fn launch_commands() {
        let d = std::env::temp_dir().join(format!("anima_launch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let portable = ComfyLayout {
            root: d.join("P").join("ComfyUI"),
            main: d.join("P").join("ComfyUI").join("main.py"),
            python: d.join("P").join("python_embeded").join("python.exe"),
            portable: true,
            desktop: false,
            cwd: d.join("P"),
        };
        let m = portable.main.to_string_lossy().to_string();
        let def = Prefs::default();
        let base = ["-s", &m, "--windows-standalone-build", "--disable-auto-launch", "--preview-method", "auto", "--port", "8188"];
        // N 卡（cu130）、預設設定：跟改之前的參數一樣
        let l = launch(&portable, 8188, &def, Some("cuda"), None, None, None).unwrap();
        assert_eq!((l.program.clone(), l.args.clone(), l.env.len()), (portable.python.clone(), os(&base), 0));
        // 外部模型資料夾的 yaml 接在 --port 後面（跟以前一樣）
        let y = d.join("x.yaml");
        let l = launch(&portable, 8188, &def, Some("cuda"), None, Some(&y), None).unwrap();
        assert_eq!(l.args[8..], os(&["--extra-model-paths-config", &y.to_string_lossy()])[..]);
        // CPU 版＋額外參數
        let p = Prefs { cpu: true, extra_args: "--lowvram --reserve-vram 1".into(), ..Default::default() };
        let l = launch(&portable, 8188, &p, Some("cuda"), None, None, None).unwrap();
        assert_eq!(l.args[8..], os(&["--lowvram", "--reserve-vram", "1"])[..], "--lowvram 跟 --cpu 同一組，以使用者的為準");
        let p = Prefs { cpu: true, extra_args: "--fast".into(), ..Default::default() };
        assert_eq!(launch(&portable, 8188, &p, Some("cuda"), None, None, None).unwrap().args[8..], os(&["--cpu", "--fast"])[..]);
        // DirectML 自動加 --directml；額外參數裡生圖站自己管的不准放
        assert_eq!(launch(&portable, 8188, &def, Some("directml"), None, None, None).unwrap().args[8..], os(&["--directml"])[..]);
        let bad = Prefs { extra_args: "--port 9000".into(), ..Default::default() };
        assert!(launch(&portable, 8188, &bad, Some("cuda"), None, None, None).is_err());
        // ZLUDA（patientx 版）：zluda.exe -- python main.py…，照 comfyui-n.bat 的參數和環境變數
        let z = ComfyLayout {
            root: d.join("Z"),
            main: d.join("Z").join("main.py"),
            python: d.join("Z").join("venv").join("Scripts").join("python.exe"),
            portable: false,
            desktop: false,
            cwd: d.join("Z"),
        };
        std::fs::create_dir_all(&z.root).unwrap();
        std::fs::write(
            z.root.join("comfyui-n.bat"),
            "@echo off\r\nset \"MIOPEN_FIND_MODE=2\"\r\nset \"PYTHON=%~dp0venv\\Scripts\\python.exe\"\r\n\
             set \"COMMANDLINE_ARGS=--auto-launch --use-quad-cross-attention --reserve-vram 0.9 --port 8188\"\r\n\
             :: set \"TRITON_OVERRIDE_ARCH=gfx1030\"\r\nset \"ZLUDA_COMGR_LOG_LEVEL=1\"\r\n.\\zluda\\zluda.exe -- %PYTHON% main.py %COMMANDLINE_ARGS%\r\n",
        )
        .unwrap();
        let exe = z.root.join("zluda").join("zluda.exe");
        let zm = z.main.to_string_lossy().to_string();
        let zp = z.python.to_string_lossy().to_string();
        let p = Prefs { extra_args: "--use-sage-attention".into(), ..Default::default() };
        let l = launch(&z, 8188, &p, Some("zluda"), Some(&exe), None, None).unwrap();
        assert_eq!(l.program, exe);
        assert_eq!(
            l.args,
            os(&["--", &zp, "-s", &zm, "--preview-method", "auto", "--port", "8188", "--reserve-vram", "0.9", "--disable-cuda-malloc", "--use-sage-attention"])
        );
        let mut env = l.env.clone();
        env.sort();
        assert_eq!(env, vec![("MIOPEN_FIND_MODE".to_string(), "2".to_string()), ("ZLUDA_COMGR_LOG_LEVEL".to_string(), "1".to_string())]);
        // 沒有 bat：用 patientx 的預設參數
        std::fs::remove_file(z.root.join("comfyui-n.bat")).unwrap();
        let l = launch(&z, 8188, &def, Some("zluda"), Some(&exe), None, None).unwrap();
        assert!(l.args.contains(&OsString::from("--use-quad-cross-attention")) && l.args.contains(&OsString::from("--disable-pinned-memory")));
        // 找不到 zluda.exe 的 ZLUDA（秋葉整合包這類）：不自己開，請使用者用它的啟動器
        assert!(launch(&z, 8188, &def, Some("zluda"), None, None, None).unwrap_err().contains("啟動器"));
        let _ = std::fs::remove_dir_all(&d);
    }
}

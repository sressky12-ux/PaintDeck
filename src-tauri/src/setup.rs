//! 第一次啟動精靈（之後也當「安裝與修復」用）：決定資料位置和 ComfyUI 位置，裝齊 AI 改寫節點、llama.cpp、反推模型、生圖模型。
//! 對照舊版 tools\install_share.py＋fetch.py＋restore_models.py。每一步都可以重跑：已經有的略過，下載斷掉從 .part 接著下載。
//!
//! 設定檔 config.json（桌面版放在 %APPDATA%\local.anima.studio\）：{"data_root": 資料根目錄, "comfy_dir": ComfyUI_windows_portable}
//! res：程式附帶的檔案（extras\anima_prompt_enhancer、extras\workflows、extras\loras、extras\comfy_moss、share\models_*.json、share\lora設定.json）
//! 測試：環境變數 ANIMA_DL_MIRROR＝本機網址時，所有下載改從「那個網址/檔名」抓；
//!       ANIMA_FAKE_GPUS＝跟 WMI 偵測一樣格式的 JSON（{"vc": [...], "reg": [...]}，見 gpu.rs gpus_from_wmi）時，不問 nvidia-smi、改用這張假清單
//!       （在 N 卡電腦上測 A 卡、Intel、沒有顯示卡的流程）；"fail"＝模擬偵測失敗
//!
//!   GET  /api/setup/scan      找電腦裡現有的生圖站資料、ComfyUI，顯示卡、硬碟空間、建議的位置
//!   POST /api/setup/check     {data_root, comfy_dir, gpu?, amd_gfx?} → 每一項裝了沒、缺多少模型（gpu、amd_gfx＝第二頁還沒存的顯示卡選擇）
//!   POST /api/setup/run       {data_root, comfy_dir, packs: [功能包…]（或舊的 models: basic|full|none）, gpu?, amd_gfx?} → 背景開始安裝（顯示卡選擇存到 comfy.json）；
//!                             {only: "tools"}＝只補影音工作台的 Node.js、ffmpeg
//!   GET  /api/setup/status    安裝進度（網頁每秒問一次）
//!   POST /api/setup/cancel    停止下載（已經下載的部分留著，下次接著下載）
//!   POST /api/setup/finish    完成：設定檔要已經有效；桌面版會切到正式畫面
//!   POST /api/setup/model_scan             掃描這台電腦已經有的模型（背景跑）；…/status {data_root, comfy_dir} 看結果、…/cancel 停止
//!   POST /api/setup/model_dirs  {data_root, comfy_dir, dirs: [{kind, path}]} → 加進外部模型資料夾（原地使用、不搬檔，見 model_dirs.rs）
//!   POST /api/setup/pick_folder {title?, start?} → 跳出 Windows 的選資料夾視窗，{path | null}（設定頁加模型資料夾也用這個）
//!   GET  /api/setup/net         代理、下載來源的設定和偵測結果（net.rs）；POST {proxy, proxy_url, hf} 存設定
//!   POST /api/setup/net/test    每個下載來源（Hugging Face、鏡像、GitHub、Civitai、Node.js）連不連得到
//!   GET  /api/setup/comfy_opts  ComfyUI 啟動方式（gpu.rs 的 comfy.json：額外參數、我自己開 ComfyUI、用 CPU 跑）和現在這份 ComfyUI 認出來的種類；
//!                               POST {extra_args?, external?, cpu?, gpu?, amd_gfx?} 存設定（下次開 ComfyUI 生效）
use crate::db::Db;
use crate::gpu;
use crate::maint;
use crate::paths::Paths;
use crate::pe::win;
use crate::settings;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// ComfyUI 官方 Windows 免安裝版（NVIDIA）。固定 0.43.0：新版站台是對這一版測過的（2026-10-31 從 0.38.0 升上來，
/// MOSS-Image、MOSS 編輯、MOSS 色彩／深度控制都是官方內建，不用再蓋補丁）。大小是 2026-10-31 從 GitHub 查的
pub const COMFY_VERSION: &str = "0.43.0";
pub const COMFY_URL: &str = "https://github.com/Comfy-Org/ComfyUI/releases/download/v0.43.0/ComfyUI_windows_portable_nvidia.7z";
pub const COMFY_SIZE: u64 = 2_039_673_203;
/// 同一版的 AMD 免安裝版（A 卡：實驗性支援，開發機沒有 A 卡、沒實測過）。2026-10-21 從 GitHub 查到的大小，
/// 並列出壓縮檔內容：PyTorch 2.9.1＋ROCm 7.2.1、Python 3.12；附的顯示卡程式只有 gfx1100～1102、gfx1150～1151、gfx1200～1201
/// （RX 7000、Ryzen AI 300／Max 內顯、RX 9000）。其他 A 卡（RX 5000／6000、780M…）裝完再用 pip 換成 ROCm 10 PyTorch（gpu.rs rocm10_*）。
/// 啟動方式跟 NVIDIA 版一樣（官方 run_amd_gpu.bat）
pub const COMFY_AMD_URL: &str = "https://github.com/Comfy-Org/ComfyUI/releases/download/v0.43.0/ComfyUI_windows_portable_amd.7z";
pub const COMFY_AMD_SIZE: u64 = 1_656_949_572;
/// 同一版的 Intel 免安裝版（Arc 獨顯、Core Ultra 的 Arc 內顯：實驗性支援，沒實測過）。2026-10-21 查的大小，列過壓縮檔內容：
/// PyTorch 2.14.0＋xpu、oneAPI 2026.1 執行檔，解開約 5.3GB。啟動方式跟 NVIDIA 版一樣
pub const COMFY_INTEL_URL: &str = "https://github.com/Comfy-Org/ComfyUI/releases/download/v0.43.0/ComfyUI_windows_portable_intel.7z";
pub const COMFY_INTEL_SIZE: u64 = 1_472_205_875;
/// 同一版的 NVIDIA 舊卡版（GTX 10 系列以前：Maxwell、Pascal、Volta）：PyTorch 是 cu126（CUDA 12.6）。主版（cu130）的 CUDA 13 已經拿掉這幾代，
/// 裝上去會 no kernel image。官方 README：「Supports Nvidia 10 series and older GPUs, DO NOT USE THIS ON NEWER 20 SERIES AND ABOVE GPUS」。
/// 大小是 2026-10-21 從 GitHub 查到的（1,886,111,296 位元組）。驅動門檻 gpu::DRIVER_MIN_LEGACY
pub const COMFY_CU126_URL: &str = "https://github.com/Comfy-Org/ComfyUI/releases/download/v0.43.0/ComfyUI_windows_portable_nvidia_cu126.7z";
pub const COMFY_CU126_SIZE: u64 = 1_930_972_692;
/// 生圖站是對這一版測的：掃到比它舊的 ComfyUI 會提醒（缺的功能要到畫圖時才出錯）
pub const COMFY_MIN_VERSION: &str = "0.38.0";
/// llama.cpp b11505（CUDA 12.4）：AI 改寫用的 llama-server 和 CUDA 執行檔
/// （網址, 大小）；大小是 2026-10-10 用 HEAD 查到的，下載完會核對
const LLAMA: &[(&str, u64)] = &[
    ("https://github.com/ggml-org/llama.cpp/releases/download/b11505/llama-b11505-bin-win-cuda-12.4-x64.zip", 265_086_800),
    ("https://github.com/ggml-org/llama.cpp/releases/download/b11505/cudart-llama-bin-win-cuda-12.4-x64.zip", 391_443_627),
];
/// 同一版的 Vulkan 版（A 卡、Intel、沒有顯示卡的電腦用：Vulkan 由顯示卡驅動提供，不用 CUDA 執行檔，沒有能用的顯示卡就用 CPU；解開約 95MB）。
/// 大小是 2026-10-21 查的。
/// pe.rs 用的 -ngl、--device none、-ncmoe、-ctk/-ctv q8_0、-fa on、--no-mmproj-offload 這版都有（在 RTX 3070 上走 Vulkan 實跑過）
const LLAMA_VULKAN: &[(&str, u64)] =
    &[("https://github.com/ggml-org/llama.cpp/releases/download/b11505/llama-b11505-bin-win-vulkan-x64.zip", 33_442_611)];

/// 要下載的 ComfyUI 免安裝版：（網址, 大小）。variant＝gpu::GpuPlan 的 variant；沒有能用的顯示卡（cpu）也用 NVIDIA 版（啟動時加 --cpu）。
/// legacy＝GpuPlan 的 legacy（N 卡是 GTX 10 系列以前的舊卡：cu126 版）
fn comfy_package(variant: &str, legacy: bool) -> (&'static str, u64) {
    match variant {
        "amd" => (COMFY_AMD_URL, COMFY_AMD_SIZE),
        "intel" => (COMFY_INTEL_URL, COMFY_INTEL_SIZE),
        "nvidia" if legacy => (COMFY_CU126_URL, COMFY_CU126_SIZE),
        _ => (COMFY_URL, COMFY_SIZE),
    }
}

/// 要下載的 llama.cpp：N 卡照舊 CUDA 版，其他用 Vulkan 版
fn llama_files(variant: &str) -> &'static [(&'static str, u64)] {
    if variant == "nvidia" {
        LLAMA
    } else {
        LLAMA_VULKAN
    }
}
/// 固定在 2026-10-21 查過大小的 commit（resolve/main 會跟著上游換檔，換了就大小對不上）
const TAGGER: &str = "https://huggingface.co/SmilingWolf/wd-eva02-large-tagger-v3/resolve/b25b82a03f7282e41aa2f257a52c7583b710bd1c/";
const TAGGER_FILES: &[(&str, u64)] = &[
    ("model.onnx", 1_260_435_999),
    ("selected_tags.csv", 308_468),
];

/// 反推模型齊全（大小對：下載到一半或下載成網頁都不算）
fn tagger_ok(dir: &Path) -> bool {
    TAGGER_FILES.iter().all(|(f, n)| {
        std::fs::metadata(dir.join(f))
            .map(|m| m.len() == *n)
            .unwrap_or(false)
    })
}
/// 影音工作台的工具（剪輯台 FableCut 要 Node.js；拆幀、接回影片、FableCut 快速匯出要 ffmpeg）。
/// 大小是 2026-10-19 用 HEAD 查到的；解壓到 <資料>\data\tools\node、tools\ffmpeg（studio::find_node_in／find_ffmpeg_in 會找）
const NODEJS: (&str, u64) = ("https://nodejs.org/dist/v24.21.0/node-v24.21.0-win-x64.zip", 37_618_919);
const FFMPEG: (&str, u64) = (
    "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip",
    114_768_076,
);
const UA: &str = "Mozilla/5.0 (anima-app setup)";
const NODE: &str = "anima_prompt_enhancer";
const MOSS_NODES: &str = "anima_moss_nodes";

mod reuse;

// ---------------------------------------------------------------- 設定檔
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub data_root: PathBuf,
    /// 使用者選的 ComfyUI 位置：官方免安裝版 ComfyUI_windows_portable（裡面有 python_embeded、ComfyUI），
    /// 或自己 git clone 的 ComfyUI（main.py 就在裡面，Python 在 venv／.venv）。見 comfy_layout
    pub comfy_dir: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Option<Config> {
        let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let c = Config {
            data_root: v["data_root"].as_str()?.into(),
            comfy_dir: v["comfy_dir"].as_str()?.into(),
        };
        (c.data_root.is_absolute() && c.comfy_dir.is_absolute()).then_some(c)
    }

    /// 先寫暫存檔再改名（寫到一半當掉不會留下壞掉的設定檔）
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let v = json!({"data_root": self.data_root.to_string_lossy(), "comfy_dir": self.comfy_dir.to_string_lossy()});
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, settings::to_text(&v))?;
        std::fs::rename(&tmp, path)
    }

    pub fn paths(&self, web: &Path) -> Paths {
        Paths::new(&self.data_root, web, Some(comfyui_dir(&self.comfy_dir)))
    }

    /// 能不能直接開始用（資料夾在、ComfyUI 在）。不行的話回傳原因
    pub fn problem(&self) -> Option<String> {
        if !self.data_root.join("data").is_dir() {
            return Some(format!(
                "找不到資料夾 {}",
                self.data_root.join("data").display()
            ));
        }
        comfy_problem(&self.comfy_dir)
    }
}

/// 認得的 ComfyUI 擺法（官方免安裝版、秋葉整合包、StabilityMatrix、Pinokio、SwarmUI、官方桌面版、自己 clone 的）
#[derive(Debug, Clone, PartialEq)]
pub struct ComfyLayout {
    /// models、custom_nodes、input、output 所在的資料夾（官方桌面版是它的資料夾，其他就是 main.py 那層）
    pub root: PathBuf,
    /// ComfyUI 的 main.py
    pub main: PathBuf,
    /// 用來開 ComfyUI 的 python.exe
    pub python: PathBuf,
    /// 官方免安裝版（python_embeded 在外層）：啟動時照官方 bat 加 --windows-standalone-build
    pub portable: bool,
    /// 官方桌面版（舊版 Electron）：程式在安裝資料夾，資料在 root，啟動時加 --base-directory
    pub desktop: bool,
    /// 啟動時的工作資料夾（跟各家啟動器一樣：Python 放外層的在外層開，venv 在 ComfyUI 裡的在 ComfyUI 開）
    pub cwd: PathBuf,
}

/// 是不是 ComfyUI 的程式資料夾：有 main.py，自己 clone 的再多看 nodes.py（只有 main.py 的 Python 專案很多）
fn comfy_src(dir: &Path, strict: bool) -> bool {
    dir.join("main.py").is_file() && (!strict || dir.join("nodes.py").is_file())
}

/// 官方桌面版（舊版 Electron）選的資料夾：有 .venv、models，沒有 main.py（程式裝在別處）
fn desktop_base(dir: &Path) -> bool {
    dir.join(".venv").join("Scripts").join("python.exe").is_file()
        && dir.join("models").is_dir()
        && !comfy_src(dir, false)
        && !comfy_src(&dir.join("ComfyUI"), false)
}

/// 官方桌面版的 ComfyUI 程式（<安裝資料夾>\resources\ComfyUI\main.py）。桌面版是 Electron（NSIS）安裝程式：
/// 預設裝在目前使用者的 %LOCALAPPDATA%\Programs\ComfyUI；選「所有使用者」就在 %ProgramFiles%\ComfyUI（一般是 C:\Program Files\ComfyUI）；
/// 也可能裝在別的資料夾，所以最後再看解除安裝登錄檔（HKCU、HKLM 的 Uninstall，只讀）裡名字有 ComfyUI 的那幾筆記的安裝位置
fn desktop_main() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut parents: Vec<PathBuf> = env("LOCALAPPDATA").map(|l| l.join("Programs")).into_iter().collect();
    parents.extend(["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|k| env(k)));
    parents.push(PathBuf::from(r"C:\Program Files"));
    let mut apps = desktop_apps(&parents);
    apps.extend(uninstall_locations("comfyui"));
    desktop_main_in(&apps)
}

/// 桌面版可能的安裝資料夾：parents 底下的 ComfyUI、@comfyorgcomfyui-electron
fn desktop_apps(parents: &[PathBuf]) -> Vec<PathBuf> {
    parents.iter().flat_map(|d| ["ComfyUI", "@comfyorgcomfyui-electron"].map(|app| d.join(app))).collect()
}

/// apps（桌面版的安裝資料夾）裡第一個有 resources\ComfyUI\main.py 的
fn desktop_main_in(apps: &[PathBuf]) -> Option<PathBuf> {
    apps.iter().map(|a| a.join("resources").join("ComfyUI").join("main.py")).find(|p| p.is_file())
}

/// 解除安裝登錄檔（HKCU、HKLM、HKLM 的 32 位元那份）裡，顯示名稱含 name（小寫）的程式安裝在哪：InstallLocation，沒有就看 DisplayIcon 的資料夾。
/// 只讀登錄檔
#[cfg(windows)]
fn uninstall_locations(name: &str) -> Vec<PathBuf> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
        KEY_WOW64_32KEY, KEY_WOW64_64KEY, RRF_RT_REG_SZ,
    };
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let text = |h: HKEY, sub: &[u16], val: &str| -> Option<String> {
        let (val, mut buf, mut n) = (w(val), vec![0u16; 1024], 2048u32);
        let r = unsafe { RegGetValueW(h, sub.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut n) };
        if r != 0 {
            return None;
        }
        let s = String::from_utf16_lossy(&buf).trim_end_matches('\0').trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    let key = w(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall");
    let mut out = vec![];
    for (hive, view) in [(HKEY_CURRENT_USER, 0), (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY)] {
        let mut h: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(hive, key.as_ptr(), 0, KEY_READ | view, &mut h) } != 0 {
            continue;
        }
        for i in 0..2000u32 {
            let (mut buf, mut n) = (vec![0u16; 256], 256u32);
            let r = unsafe { RegEnumKeyExW(h, i, buf.as_mut_ptr(), &mut n, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if r != 0 {
                break;
            }
            let mut sub = buf[..n as usize].to_vec();
            sub.push(0);
            if !text(h, &sub, "DisplayName").is_some_and(|d| d.to_lowercase().contains(name)) {
                continue;
            }
            let loc = text(h, &sub, "InstallLocation").map(|s| PathBuf::from(s.trim_matches('"'))).or_else(|| {
                let icon = text(h, &sub, "DisplayIcon")?;
                let exe = icon.trim_start_matches('"').split(['"', ',']).next()?.to_string();
                Path::new(&exe).parent().map(Path::to_path_buf)
            });
            out.extend(loc);
        }
        unsafe { RegCloseKey(h) };
    }
    out
}
#[cfg(not(windows))]
fn uninstall_locations(_name: &str) -> Vec<PathBuf> {
    vec![]
}

/// ComfyUI 的資料夾（models、custom_nodes 在這裡）：dir\ComfyUI（免安裝版、秋葉 v1.6 以後）或 dir 自己（clone 的、
/// 秋葉 v1.5 以前、桌面版的資料夾）；都不是就當免安裝版（新下載的會放在 dir\ComfyUI）
pub fn comfyui_dir(dir: &Path) -> PathBuf {
    if !comfy_src(&dir.join("ComfyUI"), false) && (comfy_src(dir, true) || desktop_base(dir)) {
        dir.to_path_buf()
    } else {
        dir.join("ComfyUI")
    }
}

/// 找 Python：d\name\python.exe 或 venv 的 d\name\Scripts\python.exe
fn python_in(d: &Path, names: &[&str], venv: bool) -> Option<PathBuf> {
    names
        .iter()
        .map(|n| if venv { d.join(n).join("Scripts").join("python.exe") } else { d.join(n).join("python.exe") })
        .find(|p| p.is_file())
}

/// 找 ComfyUI 和它的 Python。Python 依序找（dir 是使用者選的資料夾，root 是 main.py 那層，outer 是 root 的上一層）：
/// - outer\python_embeded：官方免安裝版、ComfyUI-Easy-Install、SwarmUI（dlbackend\comfy）
/// - outer\python、root\python：秋葉整合包（v1.6 以後 main.py 在 ComfyUI\，以前直接在外層）
/// - outer\standalone-env：Comfy Desktop 2.0
/// - root、outer 的 venv、.venv、env、.env：自己 clone 的、StabilityMatrix（venv）、Pinokio（env）
/// - 官方桌面版（舊版）：選的資料夾有 .venv、models，程式在 %LOCALAPPDATA%\Programs
///
/// 另外照 comfy.json：手動指定了 python.exe 就優先用它（conda、放在 ComfyUI 外面的 venv、系統 Python 這類自動找不到的）；
/// 設成「我自己開 ComfyUI」時，只要 main.py 在就算找到（python 是空的，生圖站不會去開它）
pub fn comfy_layout(dir: &Path) -> Option<ComfyLayout> {
    comfy_layout_with(dir, &PyOpt::prefs())
}

/// 同上，Python 的選項由呼叫的人給（精靈第二頁的選擇還沒存進 comfy.json）
pub fn comfy_layout_with(dir: &Path, opt: &PyOpt) -> Option<ComfyLayout> {
    layout_with(dir, desktop_main, opt)
}

/// 找 ComfyUI 的 Python 的選項（comfy.json 的 python、external）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyOpt {
    /// 手動指定的 python.exe（空＝自動找）
    pub python: String,
    /// 「我自己開 ComfyUI」：不用找得到 Python
    pub external: bool,
}

impl PyOpt {
    pub fn prefs() -> PyOpt {
        let p = gpu::prefs();
        PyOpt { python: p.python, external: p.external }
    }
}

/// desktop＝找官方桌面版程式的方法（測試時換成假的位置）
fn layout_with(dir: &Path, desktop: impl Fn() -> Option<PathBuf>, opt: &PyOpt) -> Option<ComfyLayout> {
    let root = comfyui_dir(dir);
    if desktop_base(&root) {
        return Some(ComfyLayout {
            main: desktop()?,
            python: root.join(".venv").join("Scripts").join("python.exe"),
            cwd: root.clone(),
            root,
            portable: false,
            desktop: true,
        });
    }
    if !comfy_src(&root, false) {
        return None;
    }
    let main = root.join("main.py");
    let outer = root.parent().unwrap_or(&root).to_path_buf();
    let found = |python: PathBuf, cwd: &Path, portable: bool| ComfyLayout {
        root: root.clone(),
        main: main.clone(),
        python,
        portable,
        desktop: false,
        cwd: cwd.to_path_buf(),
    };
    let auto = (|| {
        if let Some(p) = python_in(&outer, &["python_embeded"], false) {
            return Some(found(p, &outer, true));
        }
        for (d, names) in [(&outer, &["python", "standalone-env"][..]), (&root, &["python", "python_embeded"][..])] {
            if let Some(p) = python_in(d, names, false) {
                return Some(found(p, d, false));
            }
        }
        const VENVS: [&str; 4] = ["venv", ".venv", "env", ".env"];
        for d in [&root, &outer] {
            if let Some(p) = python_in(d, &VENVS, true) {
                return Some(found(p, &root, false));
            }
        }
        // conda 環境：python.exe 直接在環境資料夾根目錄（沒有 Scripts）
        for d in [&root, &outer] {
            if let Some(p) = python_in(d, &VENVS, false) {
                return Some(found(p, &root, false));
            }
        }
        None
    })();
    // 手動指定的 python.exe（檔案不見了、或不是 python.exe 就當沒指定）
    let manual = Some(opt.python.trim()).filter(|s| !s.is_empty() && gpu::check_python(s).is_ok()).map(PathBuf::from);
    if let Some(py) = manual {
        return Some(match auto {
            Some(a) if a.python == py => a,
            _ => found(py, &root, false),
        });
    }
    // 「我自己開」：生圖站不開 ComfyUI，不用 Python
    auto.or_else(|| opt.external.then(|| found(PathBuf::new(), &root, false)))
}

fn comfy_ok(dir: &Path) -> bool {
    comfy_layout(dir).is_some()
}

/// 掃硬碟找 ComfyUI 用：dir 本身就是一份安裝（Python 放外層的整包，或 clone 的 ComfyUI 資料夾）。
/// 只是「裡面有一個 ComfyUI」的上層資料夾（例如 D:\AI\ComfyUI 的 D:\AI）不算，要繼續往下找
fn comfy_install(dir: &Path) -> bool {
    // 手動指定的 python 只對現在設定的那份有效，掃別的資料夾不能套用
    comfy_layout_with(dir, &PyOpt::default()).is_some_and(|l| {
        l.root == dir || (l.root == dir.join("ComfyUI") && l.python.starts_with(dir) && !l.python.starts_with(&l.root))
    })
}

/// 不能用的原因（能用＝None）
pub fn comfy_problem(dir: &Path) -> Option<String> {
    comfy_problem_with(dir, &PyOpt::prefs())
}

fn comfy_problem_with(dir: &Path, opt: &PyOpt) -> Option<String> {
    if comfy_layout_with(dir, opt).is_some() {
        return None;
    }
    let root = comfyui_dir(dir);
    Some(if desktop_base(&root) {
        format!(
            "{} 看起來是 ComfyUI 桌面版的資料夾，但找不到桌面版的程式（%LOCALAPPDATA%\\Programs\\ComfyUI）",
            dir.display()
        )
    } else if comfy_src(&root, false) {
        let bad = Some(opt.python.trim()).filter(|s| !s.is_empty()).and_then(|s| gpu::check_python(s).err());
        format!(
            "{} 有 ComfyUI，但找不到它的 Python（python_embeded、python、venv、.venv、env 都沒有）{}。ComfyUI 的 Python 放在別的地方（conda 環境、外面的 venv、系統 Python）的話，可以到「ComfyUI 啟動方式」（精靈第二頁「自己指定」底下也有）指定 python.exe，或選「我自己開」（生圖站只連過去，不用 Python）",
            dir.display(),
            bad.map(|e| format!("；你指定的 python.exe 也不能用（{e}）")).unwrap_or_default()
        )
    } else {
        format!("{} 裡找不到 ComfyUI（main.py）", dir.display())
    })
}

/// 掃三層找不到的固定位置：官方桌面版記的資料夾、Comfy Desktop 2.0、StabilityMatrix 的預設位置
fn known_installs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut out = vec![];
    if let Some(appdata) = env("APPDATA") {
        let cfg = appdata.join("ComfyUI").join("config.json");
        if let Some(base) = std::fs::read(&cfg)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["basePath"].as_str().map(PathBuf::from))
        {
            out.push(base);
        }
        out.extend(children(&appdata.join("StabilityMatrix").join("Data").join("Packages")));
    }
    if let Some(local) = env("LOCALAPPDATA") {
        out.extend(children(&local.join("Comfy-Desktop").join("ComfyUI-Installs")));
    }
    out
}

fn children(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// 各家啟動器放在深處的 ComfyUI（掃到 x 的時候順便看）：Pinokio、StabilityMatrix 免安裝模式、SwarmUI
fn nested_installs(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![dir.join("api").join("comfy.git").join("app"), dir.join("dlbackend").join("comfy")];
    out.extend(children(&dir.join("Data").join("Packages")));
    out
}

/// 使用者選了整包的最外層、ComfyUI 其實在裡面（StabilityMatrix、Pinokio、SwarmUI、多包一層的整合包）：找出來給精靈換掉
fn inner_install(dir: &Path) -> Option<PathBuf> {
    nested_installs(dir).into_iter().chain(children(dir)).find(|d| comfy_install(d))
}

/// ComfyUI 的程式資料夾（main.py 那層；官方桌面版跟資料分開放）
fn comfy_code(dir: &Path) -> PathBuf {
    comfy_layout(dir)
        .and_then(|l| l.main.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| comfyui_dir(dir))
}

fn comfy_version(dir: &Path) -> Option<String> {
    let s = std::fs::read_to_string(comfy_code(dir).join("comfyui_version.py")).ok()?;
    let line = s
        .lines()
        .find(|l| l.trim_start().starts_with("__version__"))?;
    Some(line.split(['"', '\'']).nth(1)?.to_string())
}

/// "0.38.0"、"v0.4.1"、"0.38.0.dev1" → (0, 38, 0)；看不懂＝None
fn version_parts(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|x| x.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// 這個 ComfyUI 版本比生圖站要的（COMFY_MIN_VERSION）舊嗎？版本讀不到＝不知道，不算舊（不亂提醒）
pub fn comfy_too_old(version: Option<&str>) -> bool {
    version_parts(COMFY_MIN_VERSION).zip(version.and_then(version_parts)).is_some_and(|(min, v)| v < min)
}

/// 這份 ComfyUI 要不要（能不能）由精靈升級到 COMFY_VERSION：官方免安裝版（python_embeded＋git 資料夾）、不是桌面版、
/// 版本讀得到而且比 COMFY_VERSION 舊。自己 clone 的、桌面版、秋葉這類不動（升級方式各不同，亂動會壞）
pub fn comfy_upgradable(comfy_dir: &Path) -> bool {
    let Some(l) = comfy_layout(comfy_dir) else { return false };
    let code = comfy_code(comfy_dir);
    l.portable
        && !l.desktop
        && code.join(".git").is_dir()
        && version_parts(COMFY_VERSION).zip(comfy_version(comfy_dir).as_deref().and_then(version_parts)).is_some_and(|(want, v)| v < want)
}

/// 把官方免安裝版的 ComfyUI 程式（code＝ComfyUI 資料夾）升級到 COMFY_VERSION，開站自動升級和安裝精靈共用：
/// ① comfy_upgrade.py --req-only 讀出新版要的套件 → ② pip 先裝好 → ③ 都成功才換程式（自己改過、會被蓋掉的檔先備份到 backup）。
/// ①② 失敗時程式還是舊版，照樣開得起來（沒網路的人不會被升級弄壞）
pub fn upgrade_comfy_code(python: &Path, code: &Path, res: &Path, backup: &Path) -> Result<(), String> {
    let script = res.join("extras").join("comfy_upgrade.py");
    let s = |x: &Path| x.to_string_lossy().to_string();
    let tag = format!("v{COMFY_VERSION}");
    let req = std::env::temp_dir().join(format!("anima_comfy_req_{}.txt", std::process::id()));
    let never = || false;
    run_pip(python, &["-s".into(), s(&script), s(code), tag.clone(), s(backup), "--req-only".into(), s(&req)], None, never)
        .map_err(|e| format!("讀不到新版要的套件（ComfyUI 沒動）：{e}"))?;
    let r = run_pip(python, &["-s".into(), "-m".into(), "pip".into(), "install".into(), "--no-warn-script-location".into(), "-r".into(), s(&req)], None, never);
    let _ = std::fs::remove_file(&req);
    r.map_err(|e| format!("新版要的套件裝不起來（ComfyUI 還是舊版）：{e}"))?;
    run_pip(python, &["-s".into(), s(&script), s(code), tag, s(backup)], None, never).map_err(|e| format!("換新版程式失敗：{e}"))
}

/// 升級時換掉的舊檔放哪：ComfyUI 程式資料夾旁邊的 待刪除\ComfyUI升級備份_<時間>
fn upgrade_backup(code: &Path) -> PathBuf {
    code.parent().unwrap_or(code).join("待刪除").join(format!("ComfyUI升級備份_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")))
}

/// 開站時自動升級（main.rs 在開 ComfyUI 之前呼叫；ComfyUI 已經開著、「我自己開 ComfyUI」的不要叫）：
/// 不用升級＝None；試了＝Some(結果)，失敗時還是舊版
pub fn auto_upgrade_comfy(comfy_dir: &Path, res: &Path) -> Option<Result<(), String>> {
    if !comfy_upgradable(comfy_dir) {
        return None;
    }
    let l = comfy_layout(comfy_dir)?;
    let code = comfy_code(comfy_dir);
    Some(upgrade_comfy_code(&l.python, &code, res, &upgrade_backup(&code)))
}

/// 這份 ComfyUI 能不能跑 MOSS-Image。官方正式版到 0.40.0 都還沒有（2026-10-30 查），只有 2026-10-16 之後的 master 有
#[derive(Debug, PartialEq)]
pub enum MossSupport {
    /// nodes.py 的 CLIPLoader 認得 moss_image（官方 master，或補丁蓋好了）
    Ready,
    /// 0.38.0：安裝時會把程式附的補丁蓋進去
    Patchable,
    /// 官方桌面版（讀不到版本＝None）：程式在它自己的安裝資料夾，桌面版更新時會整個換掉，不去改
    Desktop(Option<String>),
    /// 別的版本（讀不到版本＝None）：補丁是 0.38.0 的，蓋到別版會壞
    Unsupported(Option<String>),
}

impl MossSupport {
    /// 不能用的原因和解法（能用、或安裝時會補好＝None）
    pub fn problem(&self) -> Option<String> {
        match self {
            MossSupport::Ready | MossSupport::Patchable => None,
            MossSupport::Desktop(v) => Some(format!(
                "ComfyUI 桌面版（{}）還沒有 MOSS-Image 支援，生圖站也不去改桌面版的程式：要用 MOSS 請等桌面版更新到含 MOSS 的版本，或在安裝精靈第二頁選「自動下載 ComfyUI」另外裝一份；模型檔不用重新下載",
                version_label(v)
            )),
            MossSupport::Unsupported(v) => Some(format!(
                "這份 ComfyUI（{}）的程式還沒有 MOSS-Image 支援（官方正式版 0.43.0 起內建）。解法：官方免安裝版按「安裝與修復」會自動升級；自己 clone 的切到 v0.43.0 以後（git checkout v0.43.0）再裝 requirements.txt；別的整合包用它自己的更新器更新到 0.43 以後；或在安裝精靈第二頁選「自動下載 ComfyUI」另外裝一份。模型檔不用重新下載",
                version_label(v)
            )),
        }
    }
}

/// 「版本 0.40.0」；讀不到＝「讀不到版本」
fn version_label(v: &Option<String>) -> String {
    v.as_deref().map(|v| format!("版本 {v}")).unwrap_or_else(|| "讀不到版本".into())
}

/// 看 nodes.py 的 CLIPLoader 有沒有 moss_image。只看 nodes_moss_image.py 在不在不準：ComfyUI 更新會把改過的 nodes.py 換回官方版，
/// 沒被 git 追蹤的 nodes_moss_image.py 卻留著（2026-10-30 有人這樣中招）
pub fn moss_support(comfy_dir: &Path) -> MossSupport {
    let code = comfy_code(comfy_dir);
    if std::fs::read_to_string(code.join("nodes.py")).is_ok_and(|s| s.contains("\"moss_image\"")) {
        // 0.38 上的舊版補丁（2026-10-30 以前）沒有 MOSS 編輯節點：再蓋一次新補丁
        let edit = std::fs::read_to_string(code.join("comfy_extras").join("nodes_moss_image.py")).is_ok_and(|s| s.contains("TextEncodeMOSSImageEdit"));
        let old_patch = comfy_version(comfy_dir).is_some_and(|v| v == "0.38.0") && !comfy_layout(comfy_dir).is_some_and(|l| l.desktop);
        return if edit || !old_patch { MossSupport::Ready } else { MossSupport::Patchable };
    }
    if comfy_layout(comfy_dir).is_some_and(|l| l.desktop) {
        return MossSupport::Desktop(comfy_version(comfy_dir));
    }
    match comfy_version(comfy_dir) {
        Some(v) if v == "0.38.0" => MossSupport::Patchable,
        v => MossSupport::Unsupported(v),
    }
}

// ---------------------------------------------------------------- 電腦要有的元件
/// VC++ v14 執行階段（x64）：ComfyUI 的 PyTorch、反推的 onnxruntime.dll、AI 改寫的 ggml-cuda.dll 都要 msvcp140.dll，
/// 生圖站只附了 vcruntime140.dll。微軟的永久網址（2026-10-21 指到 VS 2026 版，約 18MB）
const VCREDIST_URL: &str = "https://aka.ms/vc14/vc_redist.x64.exe";
/// 14.40 以前的 msvcp140 配新版 MSVC（VS 2022 17.10 以後）編的程式，std::mutex 會當掉
const VCREDIST_MIN: (u32, u32) = (14, 40);
/// 裝了哪一版 VC++ v14 執行階段（微軟文件說的登錄檔 HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64），沒裝＝None
#[cfg(windows)]
pub fn vcredist_version() -> Option<(u32, u32, u32)> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY,
    };
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = w(r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64");
    let get = |view: u32, name: &str| -> Option<u32> {
        let (mut v, mut n) = (0u32, 4u32);
        let name = w(name);
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD | view,
                std::ptr::null_mut(),
                (&mut v as *mut u32).cast(),
                &mut n,
            )
        };
        (r == 0).then_some(v)
    };
    [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY].into_iter().find_map(|view| {
        (get(view, "Installed")? == 1).then_some(())?;
        Some((get(view, "Major")?, get(view, "Minor")?, get(view, "Bld").unwrap_or(0)))
    })
}
#[cfg(not(windows))]
pub fn vcredist_version() -> Option<(u32, u32, u32)> {
    None
}

/// VC++ 執行階段夠新、而且 msvcp140.dll 真的在系統資料夾
pub fn vcredist_ok() -> bool {
    let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    vcredist_version().is_some_and(|(a, b, _)| (a, b) >= VCREDIST_MIN) && sys.join("System32").join("msvcp140.dll").is_file()
}

/// 等 VC++ 安裝程式超過這麼久還沒結果，就提醒使用者看一下工作列
const VC_UAC_HINT_AFTER: Duration = Duration::from_secs(20);
const VC_UAC_HINT: &str = "還在等：Windows 的權限確認視窗（問「要允許這個應用程式變更您的裝置嗎」）可能在背景跳出來了，請看一下工作列有沒有閃爍的圖示，點開按「是」。不想裝的話按「停止下載」";

/// vc_redist 的結束代碼 → 成功（要不要重開機）或給人看的原因
pub(crate) fn vcredist_result(code: u32) -> Result<bool, String> {
    match code {
        0 | 1638 => Ok(false), // 1638：已經有一樣或更新的版本
        3010 | 1641 => Ok(true), // 裝好了，重開機後才完全生效
        1602 | 1223 => Err("安裝被取消了（系統確認視窗按了「否」）".into()),
        c => Err(format!("安裝失敗（代碼 {c}）")),
    }
}

/// 用檔案總管的方式開程式（需要系統管理員權限的會跳 UAC 確認視窗），等它結束、回傳結束代碼。
/// std::process::Command 開不了要提升權限的程式（錯誤 740），所以用 ShellExecuteEx。
/// 等程式結束時每 0.5 秒看一次 abandon：變成 true 就不等了（回「已取消」；程式本身已經在跑的話不會被砍，裝完就裝完）。
/// 注意 ShellExecuteEx 本身會一直卡到使用者回答 UAC 視窗：呼叫端要放在另一條執行緒，才能在等的時候被「停止」打斷（install_vcredist）
#[cfg(windows)]
pub fn run_and_wait(exe: &Path, args: &str, abandon: &AtomicBool) -> Result<u32, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let w = |s: &std::ffi::OsStr| {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain([0]).collect::<Vec<u16>>()
    };
    if !exe.is_file() {
        return Err(format!("找不到 {}", exe.display()));
    }
    let (file, params) = (w(exe.as_os_str()), w(std::ffi::OsStr::new(args)));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let e = unsafe { GetLastError() };
        return if e == 1223 { Ok(1223) } else { Err(format!("開不了 {}（錯誤 {e}）", exe.display())) };
    }
    if info.hProcess.is_null() {
        return Err(format!("開不了 {}", exe.display()));
    }
    let mut code = 0u32;
    unsafe {
        while WaitForSingleObject(info.hProcess, 500) == WAIT_TIMEOUT {
            if abandon.load(Ordering::SeqCst) {
                CloseHandle(info.hProcess);
                return Err("已取消".into());
            }
        }
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}
#[cfg(not(windows))]
pub fn run_and_wait(_exe: &Path, _args: &str, _abandon: &AtomicBool) -> Result<u32, String> {
    Err("只有 Windows 能用".into())
}

// ---------------------------------------------------------------- 記憶體、安裝期間不讓電腦睡著
#[cfg(windows)]
#[repr(C)]
struct MemoryStatusEx {
    length: u32,
    load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page: u64,
    avail_page: u64,
    total_virt: u64,
    avail_virt: u64,
    avail_ext: u64,
}
#[cfg(windows)]
extern "system" {
    fn GlobalMemoryStatusEx(buf: *mut MemoryStatusEx) -> i32;
    fn SetThreadExecutionState(flags: u32) -> u32;
}

/// 這台電腦的記憶體（RAM）總量，位元組。問不到＝None。
/// 測試用：環境變數 ANIMA_FAKE_RAM_GB＝假的記憶體大小（GB，可以有小數）
pub fn ram_total() -> Option<u64> {
    if let Some(g) = std::env::var("ANIMA_FAKE_RAM_GB").ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|g| *g > 0.0) {
        return Some((g * (1u64 << 30) as f64) as u64);
    }
    #[cfg(windows)]
    {
        let mut m: MemoryStatusEx = unsafe { std::mem::zeroed() };
        m.length = std::mem::size_of::<MemoryStatusEx>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 && m.total_phys > 0 {
            return Some(m.total_phys);
        }
    }
    None
}

/// 這台電腦夠不夠力（精靈第三頁決定預設要不要只裝動漫）：顯存 8GB 以下、記憶體 16GB 以下都算「低配」；
/// 記憶體不到 16GB（Windows 顯示的總量會比貼紙上的少一點：16GB 的電腦常顯示 15.x，有內顯分走的更少，這裡抓 15GB）另外提醒會很慢。
/// 顯存取所有顯示卡裡最大的（N 卡加內顯，看 N 卡）；不知道顯存（偵測失敗、沒有顯示卡）不算低。
fn lite_view(gpus: &[Value], ram: Option<u64>) -> Value {
    const GIB: u64 = 1 << 30;
    let vram_mb = gpus.iter().filter_map(|g| g["vram_mb"].as_i64()).filter(|&m| m > 0).max();
    // 8GB 的卡回報 8188～8192，留一點空間
    let low_vram = vram_mb.is_some_and(|m| m <= 8704);
    let low_ram = ram.is_some_and(|r| r <= 17 * GIB);
    let tiny_ram = ram.is_some_and(|r| r < 15 * GIB);
    json!({"ram": ram, "vram_mb": vram_mb, "low_vram": low_vram, "low_ram": low_ram, "tiny_ram": tiny_ram, "lite": low_vram || low_ram})
}

/// 安裝期間不讓 Windows 睡著（下載動輒幾十 GB，睡著就斷線）。SetThreadExecutionState 只管「呼叫的那條執行緒」，
/// 所以另開一條專用執行緒設定、等放掉（或結束、取消、失敗、當掉：執行緒結束 Windows 也會自己恢復）再恢復
pub struct SleepGuard {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// 現在有幾個 SleepGuard 在擋睡眠（測試用）
static AWAKE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl SleepGuard {
    pub fn start() -> SleepGuard {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("anima-keep-awake".into())
            .spawn(move || {
                #[cfg(windows)]
                unsafe {
                    // ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                    SetThreadExecutionState(0x8000_0000 | 0x0000_0001);
                }
                AWAKE.fetch_add(1, Ordering::SeqCst);
                let _ = ready_tx.send(());
                let _ = wait.recv(); // 發送端放掉（或送了訊號）就往下
                #[cfg(windows)]
                unsafe {
                    SetThreadExecutionState(0x8000_0000); // ES_CONTINUOUS：清掉
                }
                AWAKE.fetch_sub(1, Ordering::SeqCst);
            })
            .ok();
        let _ = ready.recv_timeout(Duration::from_secs(2));
        SleepGuard { release: Some(release), thread }
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------- 找電腦裡現有的東西
#[cfg(windows)]
pub(crate) fn fixed_drives() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:\\", (b'A' + i) as char))
        .filter(|d| {
            let w: Vec<u16> = d.encode_utf16().chain([0]).collect();
            unsafe { GetDriveTypeW(w.as_ptr()) == DRIVE_FIXED }
        })
        .map(PathBuf::from)
        .collect()
}
#[cfg(not(windows))]
pub(crate) fn fixed_drives() -> Vec<PathBuf> {
    vec![]
}

/// 看起來是備份、垃圾桶、系統的資料夾：不往裡面找
pub(crate) fn skip_dir(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with('$')
        || n.starts_with('.')
        || n.contains("待刪除")
        || n.contains("snapshot")
        || n.contains("備份")
        || n.contains("备份")
        || n.contains("_trash")
        || [
            "windows",
            "program files",
            "program files (x86)",
            "programdata",
            "system volume information",
            "recovery",
            "steamlibrary",
            "node_modules",
        ]
        .contains(&n.as_str())
}

/// 找舊安裝最多花這麼久。2026-10-30 有人回報精靈第一步一直「超過 240 秒沒有回應」：硬碟多、硬碟在睡、防毒攔檔案、
/// 網盤掛成本機磁碟（CloudDrive2、RaiDrive、rclone）時，三層掃下來好幾分鐘。時間到就先用找到的，沒掃完的硬碟另外講
const FIND_BUDGET: Duration = Duration::from_secs(45);
/// 時間到了再多等這麼久收結果（卡在讀資料夾的那顆硬碟不等）
const FIND_GRACE: Duration = Duration::from_secs(2);

/// 找到的舊安裝；unfinished＝時間內沒掃完的硬碟（照傳進來的順序）
#[derive(Debug, Default)]
struct Found {
    data: Vec<PathBuf>,
    comfy: Vec<PathBuf>,
    unfinished: Vec<PathBuf>,
}

/// 每個 item 各開一條執行緒跑 f，最多等到 deadline；沒跑完的是 None（那條執行緒自己跑完或卡著，不影響這裡）
fn each_until<I, T>(items: &[I], f: impl Fn(I) -> T + Send + Sync + 'static, deadline: Instant) -> Vec<Option<T>>
where
    I: Clone + Send + 'static,
    T: Send + 'static,
{
    let f = Arc::new(f);
    let (tx, rx) = std::sync::mpsc::channel();
    for (i, item) in items.iter().cloned().enumerate() {
        let (tx, f) = (tx.clone(), f.clone());
        std::thread::spawn(move || {
            let _ = tx.send((i, f(item)));
        });
    }
    drop(tx);
    let mut out: Vec<Option<T>> = items.iter().map(|_| None).collect();
    while let Ok((i, v)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        out[i] = Some(v);
    }
    out
}

/// 每顆硬碟往下找三層（X:\a\b\c），收集看起來是生圖站資料、ComfyUI 的資料夾；再加上掃不到的固定位置（known_installs）
fn find_installs() -> Found {
    let mut f = find_installs_in(&fixed_drives(), FIND_BUDGET);
    for k in known_installs() {
        if comfy_install(&k) && !f.comfy.iter().any(|c| crate::model_dirs::same(c, &k)) {
            f.comfy.push(k);
        }
    }
    f
}

/// 正在掃的硬碟。上一次時間到放棄了、執行緒卻還卡在那顆硬碟上的，重掃時不再開新的執行緒（不然越重試越多條搶同一顆慢硬碟）
static WALKING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// 掃完（或 panic）就從 WALKING 拿掉
struct Walking(PathBuf);
impl Drop for Walking {
    fn drop(&mut self) {
        WALKING.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| x != &self.0);
    }
}

/// 每個 root 一條執行緒同時掃，全部最多 budget；結果照 roots 的順序接起來
fn find_installs_in(roots: &[PathBuf], budget: Duration) -> Found {
    let deadline = Instant::now() + budget;
    let busy = WALKING.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let todo: Vec<PathBuf> = roots.iter().filter(|r| !busy.contains(r)).cloned().collect();
    let walk = move |r: PathBuf| {
        WALKING.lock().unwrap_or_else(|e| e.into_inner()).push(r.clone());
        let _w = Walking(r.clone());
        walk_installs(&r, deadline)
    };
    let mut results = todo.iter().zip(each_until(&todo, walk, deadline + FIND_GRACE));
    let mut f = Found::default();
    for root in roots {
        let r = if busy.contains(root) { None } else { results.next().and_then(|(_, r)| r) };
        let Some((data, comfy, done)) = r else {
            f.unfinished.push(root.clone());
            continue;
        };
        f.data.extend(data);
        f.comfy.extend(comfy);
        if !done {
            f.unfinished.push(root.clone());
        }
    }
    f
}

/// 一顆硬碟往下找三層；過了 deadline 就停（第三個值＝有沒有掃完）
fn walk_installs(root: &Path, deadline: Instant) -> (Vec<PathBuf>, Vec<PathBuf>, bool) {
    let (mut data, mut comfy) = (vec![], vec![]);
    let mut level: Vec<PathBuf> = vec![root.to_path_buf()];
    for depth in 0..3 {
        let mut next = vec![];
        for dir in &level {
            if Instant::now() >= deadline {
                return (data, comfy, false);
            }
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in rd.flatten() {
                if Instant::now() >= deadline {
                    return (data, comfy, false);
                }
                let name = e.file_name().to_string_lossy().to_string();
                if skip_dir(&name) || !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let p = e.path();
                if p.join("data").join("anima.db").is_file() {
                    data.push(p.clone());
                }
                if comfy_install(&p) {
                    comfy.push(p.clone());
                    continue; // ComfyUI 裡面不用再找
                }
                comfy.extend(nested_installs(&p).into_iter().filter(|n| comfy_install(n)));
                if depth < 2 {
                    next.push(p);
                }
            }
        }
        level = next;
    }
    (data, comfy, true)
}

fn image_count(data_root: &Path) -> Option<i64> {
    let db = data_root.join("data").join("anima.db");
    let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()?;
    c.query_row(
        "SELECT COUNT(*) FROM images WHERE trashed IS NULL",
        [],
        |r| r.get(0),
    )
    .or_else(|_| c.query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0)))
    .ok()
}

fn mtime(p: &Path) -> Value {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            json!(chrono::DateTime::<chrono::Local>::from(t)
                .format("%Y-%m-%d %H:%M")
                .to_string())
        })
        .unwrap_or(Value::Null)
}

/// 偵測顯示卡最多等這麼久。gpu.rs 的 PowerShell 偵測自己 20 秒會放棄，但 nvidia-smi 沒有逾時（驅動卡死時一直不回來），
/// 精靈第一頁就會永遠「讀取中」。逾時＝None，呼叫端當成偵測失敗（請使用者在第二頁自己選 ComfyUI 版本）
const DETECT_TIMEOUT: Duration = Duration::from_secs(40);

/// 在另一條執行緒跑 f，最多等 limit；逾時就不等了（那條執行緒自己跑完或卡著，不影響這裡）
fn call_with_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static, limit: Duration) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit).ok()
}

fn detect_guarded(fresh: bool) -> Option<(Vec<Value>, bool)> {
    let r = call_with_timeout(move || gpu::detect(fresh), DETECT_TIMEOUT);
    if r.is_none() {
        eprintln!("[精靈] 偵測顯示卡超過 {} 秒沒有回應，當成偵測失敗", DETECT_TIMEOUT.as_secs());
    }
    r
}

pub fn scan(current: Option<&Config>) -> Value {
    // 偵測顯示卡（最多 DETECT_TIMEOUT）跟掃硬碟同時做，以前一個接一個，最慢要加起來
    let gpu_job = std::thread::spawn(|| detect_guarded(true));
    let Found { mut data, mut comfy, unfinished } = find_installs();
    let installations = reuse::installations();
    let program_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    reuse::include_known(current, &installations, program_dir.as_deref(), &mut data, &mut comfy);
    let data: Vec<Value> = data
        .iter()
        .map(|d| {
            let sib = d.parent().map(|p| p.join("ComfyUI_windows_portable")).filter(|p| comfy_ok(p));
            let mut info = reuse::data_info(d);
            info["modified"] = mtime(&d.join("data/anima.db"));
            info["comfy"] = json!(current.filter(|c| crate::model_dirs::same(&c.data_root, d)).map(|c| c.comfy_dir.clone()).or(sib));
            info
        })
        .collect();
    let comfy: Vec<Value> = comfy
        .iter()
        .map(|c| {
            let v = comfy_version(c);
            json!({"path": c.to_string_lossy(), "version": v, "old": comfy_too_old(v.as_deref())})
        })
        .collect();
    // 硬碟空間也可能卡住（掛載的網盤斷線）：每顆各自問，最多等 10 秒，問不到的不列，併進「沒掃完」一起提醒
    let drives = fixed_drives();
    let usage = each_until(&drives, |d| maint::disk_usage(&d), Instant::now() + Duration::from_secs(10));
    let mut unscanned = unfinished;
    for (d, _) in drives.iter().zip(&usage).filter(|(_, u)| u.is_none()) {
        if !unscanned.contains(d) {
            unscanned.push(d.clone());
        }
    }
    let disks: Vec<Value> = drives
        .iter()
        .zip(usage)
        .filter_map(|(d, u)| {
            u.flatten().map(
                |(free, total)| json!({"drive": d.to_string_lossy(), "free": free, "total": total}),
            )
        })
        .collect();
    // 全新安裝的建議位置：可用空間最大的硬碟（C 槽留給系統，其他硬碟有 80GB 以上就不選 C）
    let best = disks
        .iter()
        .filter(|d| {
            !d["drive"].as_str().unwrap_or("").starts_with('C')
                || disks
                    .iter()
                    .all(|x| x["free"].as_u64().unwrap_or(0) < 80 << 30 || x == *d)
        })
        .max_by_key(|d| d["free"].as_u64().unwrap_or(0))
        .and_then(|d| d["drive"].as_str())
        .unwrap_or("C:\\")
        .to_string();
    let base = PathBuf::from(best).join("AnimaStudio");
    // 顯示卡：偵測結果、自動判斷的版本、使用者在第二頁選過的（comfy.json）
    let guarded = gpu_job.join().ok().flatten();
    let detect_timeout = guarded.is_none();
    let (gpus, detected) = guarded.unwrap_or_default();
    let prefs = gpu::prefs();
    let plan = gpu::plan_for(&prefs.gpu, &prefs.amd_gfx, &gpus, detected);
    let variant = plan.variant;
    json!({
        "current": current.map(|c| {
            let v = comfy_version(&c.comfy_dir);
            json!({"data_root": c.data_root.to_string_lossy(), "comfy_dir": c.comfy_dir.to_string_lossy(), "problem": c.problem(),
                   "version": v, "old": comfy_too_old(v.as_deref())})
        }),
        "installations": installations, "program_dir": program_dir,
        "data": data, "comfy": comfy, "disks": disks, "gpus": gpus, "driver_min": gpu::DRIVER_MIN, "driver_min_legacy": gpu::DRIVER_MIN_LEGACY,
        // 時間內沒掃完的硬碟（找舊安裝）：精靈提醒「在那裡的話第二頁自己指定」
        "unscanned": unscanned.iter().map(|d| d.to_string_lossy()).collect::<Vec<_>>(),
        "comfy_min_version": COMFY_MIN_VERSION,
        "variant": variant, "windows_build": gpu::windows_build(), "lite": lite_view(&gpus, ram_total()),
        "detected": detected, "detect_timeout": detect_timeout, "gpu": plan.view(), "auto": gpu::auto_plan(&gpus, detected).view(),
        "prefs": prefs, "amd_choices": gpu::AMD_GFX_CHOICES.iter().map(|(g, l)| json!({"gfx": g, "label": l})).collect::<Vec<_>>(),
        "vcredist": {"ok": vcredist_ok(), "version": vcredist_version().map(|(a, b, c)| format!("{a}.{b}.{c}"))},
        "fresh": {"data_root": base.join("anima_data").to_string_lossy(), "comfy_dir": base.join("ComfyUI_windows_portable").to_string_lossy()},
        "comfy_download": {"version": COMFY_VERSION, "size": comfy_package(variant, plan.legacy).1, "variant": variant, "legacy": plan.legacy},
    })
}

// ---------------------------------------------------------------- 檢查每一項
struct Plan {
    data_root: PathBuf,
    comfy_dir: PathBuf,
    /// 找 ComfyUI 的 Python 的選項（精靈第二頁填的、還沒存進 comfy.json 的值；沒給就照 comfy.json）
    opt: PyOpt,
}

impl Plan {
    fn comfyui(&self) -> PathBuf {
        comfyui_dir(&self.comfy_dir)
    }
    fn models(&self) -> PathBuf {
        self.comfyui().join("models")
    }
    fn node(&self) -> PathBuf {
        self.comfyui().join("custom_nodes").join(NODE)
    }
    fn tagger(&self) -> PathBuf {
        self.data_root.join("data").join("models").join("tagger")
    }
    fn dl(&self) -> PathBuf {
        self.data_root.join("data").join("_dl") // 下載暫存（.part、壓縮檔）
    }
    fn tools(&self) -> PathBuf {
        self.data_root.join("data").join("tools")
    }
    /// llama.cpp 裝好了沒。A 卡、Intel、CPU 還要是 Vulkan 版：舊版精靈在 A 卡電腦上也裝 CUDA 版，那版用不了 A 卡（只會用 CPU），要補裝。
    /// 偵測失敗（unknown）不知道是哪張卡：有裝就算，不去蓋掉別人的 CUDA 版
    fn llama_ok(&self, variant: &str) -> bool {
        let bin = self.node().join("bin");
        bin.join("llama-server.exe").is_file() && (matches!(variant, "nvidia" | "unknown") || bin.join("ggml-vulkan.dll").is_file())
    }
}

/// 影音工作台的一項工具：（項目、網址與大小、tools 底下的資料夾、顯示名稱、裝了沒）
type StudioTool = (&'static str, (&'static str, u64), &'static str, &'static str, bool);

fn studio_tools(p: &Plan) -> [StudioTool; 2] {
    let t = p.tools();
    [
        ("tools/node", NODEJS, "node", "Node.js 24（剪輯台）", crate::studio::find_node_in(&t).is_some()),
        ("tools/ffmpeg", FFMPEG, "ffmpeg", "ffmpeg 9.0.2（拆幀、接影片）", crate::studio::find_ffmpeg_in(&t).is_some()),
    ]
}

#[derive(Clone)]
struct ModelFile {
    rel: String,
    size: u64,
    url: Option<String>,
    /// 清單裡的說明（LoRA 是它的名字，例如 Flat Color - Style）
    note: Option<String>,
    /// 來源公布的 SHA256（小寫十六進位；tools\check_models.py --fix 補的）。有寫就在下載完核對
    sha256: Option<String>,
}

/// 進度頁上一個模型檔的名字：「功能包・種類：檔名」（清單裡 LoRA 有自己的名字就用那個），不露出 diffusion_models/… 這種資料夾路徑
fn model_label(m: &ModelFile) -> String {
    let (kind, name) = m.rel.split_once('/').unwrap_or(("", m.rel.as_str()));
    let file = name.rsplit('/').next().unwrap_or(name);
    let pack = PACKS.iter().find(|p| p.0 == pack_of(&m.rel)).map(|p| p.1).unwrap_or("其他");
    let what = match kind.to_lowercase().as_str() {
        "diffusion_models" => "主模型",
        "text_encoders" => "文字編碼器",
        "vae" => "VAE（影像解碼器）",
        "loras" => "LoRA",
        "model_patches" => "構圖控制",
        "upscale_models" => "放大模型",
        "checkpoints" => "偵測模型",
        "background_removal" => "去背模型",
        "geometry_estimation" => "深度估計模型",
        "llm" => "語言模型",
        _ => "模型",
    };
    match m.note.as_deref().filter(|n| kind.eq_ignore_ascii_case("loras") && !n.trim().is_empty()) {
        Some(n) => format!("{pack}・{what}：{} （{file}）", n.trim()),
        None => format!("{pack}・{what}：{file}"),
    }
}

fn manifest(res: &Path, which: &str) -> Vec<ModelFile> {
    let p = res.join("share").join(format!("models_{which}.json"));
    let v: Value = std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!([]));
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(ModelFile {
                rel: e["path"].as_str()?.to_string(),
                size: e["size"].as_u64()?,
                url: e["url"].as_str().map(str::to_string),
                note: e["note"].as_str().map(str::to_string),
                sha256: e["sha256"]
                    .as_str()
                    .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_ascii_lowercase),
            })
        })
        .filter(|m| {
            m.rel
                .split('/')
                .all(|s| !s.is_empty() && s != ".." && !s.contains([':', '\\']))
        }) // 只能在 models 底下
        .collect()
}

fn rel_path(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, s| p.join(s))
}

/// 清單裡還缺的模型。外部模型資料夾（model_dirs.rs，電腦裡本來就有的）和使用者自己 ComfyUI 的 extra_model_paths.yaml
/// 指到的資料夾裡檔名、大小、子資料夾都對的也算有
fn missing_models(res: &Path, models: &Path, which: &str, extra: &[crate::model_dirs::Dir]) -> Vec<ModelFile> {
    let mut all: Vec<crate::model_dirs::Dir> = extra.to_vec();
    if let Some(c) = models.parent() {
        all.extend(crate::model_dirs::comfy_extra(c).into_iter().filter(|d| d.path.is_dir()).map(|d| crate::model_dirs::Dir {
            kind: d.kind,
            path: d.path,
            source: "comfy_yaml".into(),
            added: String::new(),
        }));
    }
    let newer = adopted_sizes(models);
    let alias = aliases(models);
    manifest(res, which)
        .into_iter()
        .filter(|m| !alias.get(&m.rel).is_some_and(|f| std::fs::metadata(f).is_ok_and(|md| md.len() == m.size)))
        .filter(|m| {
            // 清單的版本，或之前照來源新版下載的版本（adopted_sizes），哪個都算有
            let sizes = [Some(m.size), newer.get(&m.rel).copied()];
            let (kind, sub) = m.rel.split_once('/').unwrap_or((&m.rel, ""));
            !sizes.into_iter().flatten().any(|s| {
                std::fs::metadata(rel_path(models, &m.rel)).is_ok_and(|md| md.len() == s)
                    || crate::model_dirs::find(&all, kind, sub, Some(s)).is_some()
            })
        })
        .collect()
}

/// 精靈照來源新版下載的模型（上傳的人換了檔，大小跟清單不同）記在模型資料夾這個檔：{"loras/x.safetensors": {"size":…, "url":…, "at":…}}。
/// 記在模型旁邊（不是資料夾的 data），換資料位置或用別的方式開精靈也找得到
const ADOPTED_FILE: &str = "anima_model_versions.json";

/// 模型相對路徑 → 照來源新版下載的大小
fn adopted_sizes(models: &Path) -> std::collections::HashMap<String, u64> {
    let v: Value = std::fs::read(models.join(ADOPTED_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, e)| Some((k.clone(), e["size"].as_u64()?)))
        .collect()
}

fn remember_adopted(models: &Path, rel: &str, size: u64, url: &str) -> std::io::Result<()> {
    let p = models.join(ADOPTED_FILE);
    let mut v: Value = std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(Value::is_object)
        .unwrap_or(json!({}));
    v[rel] = json!({"size": size, "url": url, "at": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()});
    std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap_or_default())
}

/// 清單裡的 LoRA 在別的地方已經有一模一樣的檔（same_lora_elsewhere 找到的）：{"loras/x.safetensors": "D:\…\實際的檔"}，記在模型資料夾。
/// 2026-10-31 使用者：「如果檢測到同樣的個別lora則不重覆安裝那個各別的lora」
const ALIAS_FILE: &str = "anima_model_aliases.json";

fn aliases(models: &Path) -> std::collections::HashMap<String, PathBuf> {
    let v: Value = std::fs::read(models.join(ALIAS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, e)| Some((k.clone(), PathBuf::from(e.as_str()?))))
        .collect()
}

fn remember_alias(models: &Path, rel: &str, found: &Path) -> std::io::Result<()> {
    let p = models.join(ALIAS_FILE);
    let mut v: Value = std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(Value::is_object)
        .unwrap_or(json!({}));
    v[rel] = json!(found.to_string_lossy());
    std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap_or_default())
}

/// 清單裡的這顆 LoRA，在 loras 底下（含外部資料夾）別的路徑已經有一模一樣的檔？大小一樣的才算 SHA256（cache 記算過的，
/// 同一次安裝很多顆一樣大的 LoRA 不重算）；清單沒寫 SHA256 的不猜
fn same_lora_elsewhere(index: &[(PathBuf, u64)], cache: &mut std::collections::HashMap<PathBuf, String>, m: &ModelFile) -> Option<PathBuf> {
    let want = m.sha256.as_deref()?;
    index.iter().filter(|(_, n)| *n == m.size && m.size > 0).map(|(p, _)| p).find(|p| {
        if !cache.contains_key(*p) {
            let h = sha256_file(p).unwrap_or_default();
            cache.insert(p.to_path_buf(), h);
        }
        cache[*p] == want
    }).cloned()
}

/// 清單裡的這顆 LoRA（rel）在別的地方已經有一模一樣的檔：先看記過的別名，沒有再找大小＋SHA256 一樣的（找到就記下來）。
/// 生成頁缺檔一鍵下載（/api/models/fetch_listed）用；不是 LoRA、清單沒有、沒寫 SHA256 的都回 None
pub(crate) fn lora_elsewhere(res: &Path, models: &Path, extra: &[crate::model_dirs::Dir], rel: &str) -> Option<PathBuf> {
    if !rel.starts_with("loras/") {
        return None;
    }
    let m = manifest(res, "full").into_iter().find(|m| m.rel == rel)?;
    if let Some(f) = aliases(models).remove(rel).filter(|f| std::fs::metadata(f).is_ok_and(|md| md.len() == m.size)) {
        return Some(f);
    }
    let index = crate::model_dirs::size_index(models, extra, "loras");
    let f = same_lora_elsewhere(&index, &mut Default::default(), &m)?;
    if let Err(e) = remember_alias(models, rel, &f) {
        eprintln!("[LoRA 別名] 記不下來：{e}");
    }
    Some(f)
}

/// 程式附的備註裡，由程式管的欄位（使用者在 LoRA 頁改的也是這些；AI 認識 LoRA 寫的 agent 不在內）
const NOTE_FIELDS: &[&str] = &["title", "note", "triggers", "strength", "min", "max", "base_model"];

/// 26.1031.2 附的備註（那時還沒記 bundle_sig）後來改過的原文：資料庫裡還是這段＝使用者沒改過，可以換新版
const LEGACY_NOTES: &[&str] = &[
    "【改圖】風格混搭的編輯 LoRA。原作者是配 ostris 的 MOSS Edit 節點包用；繪匣的「照指令改圖」沒驗證過能不能直接用",
    "【改圖】給一張角色圖當 Picture 1，再寫新場景，畫出同一個角色（臉、髮型、衣服、畫風）。原作者配 ostris 的 MOSS Edit 節點包；繪匣的「照指令改圖」沒驗證過",
];

/// 這筆備註「程式管的欄位」的指紋：補進資料庫時記在 bundle_sig，之後一樣＝使用者沒改過，程式改版可以跟著更新
fn note_sig(m: &Value) -> String {
    use sha2::{Digest, Sha256};
    let sub: Vec<Value> = NOTE_FIELDS.iter().map(|k| m.get(*k).cloned().unwrap_or(Value::Null)).collect();
    Sha256::digest(Value::Array(sub).to_string().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// 開站時把程式附的 LoRA 備註（resources\share\lora設定.json 的 lora_meta）併進資料庫，回傳補／更新了幾筆：
/// 資料庫沒有的 key 補上；有的話，程式管的欄位使用者沒改過（bundle_sig 對得上）才換成新版，改過的不蓋、AI 寫的 agent 留著。
/// roots 底下的 LoRA 檔名跟某個備註一樣、卻放在別的資料夾（例如照合集 README 放進 loras\Excellent-Loras-for-MossImage\），用實際的名稱也一樣處理。
/// 2026-10-31 使用者：「把每個LoRa都分別寫個備註…讓助手和各別的user一看就知道那個lora是幹啥用的」
pub fn merge_lora_notes(db: &Db, res: &Path, roots: &[PathBuf]) -> usize {
    let Ok(v) = std::fs::read(res.join("share").join("lora設定.json")).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string())) else {
        return 0;
    };
    let Some(bundled) = v["lora_meta"].as_object() else { return 0 };
    let Ok(have) = db.lora_meta() else { return 0 };
    let base = |k: &str| k.rsplit(['/', '\\']).next().unwrap_or(k).to_lowercase();
    let by_base: std::collections::HashMap<String, &Value> = bundled.iter().map(|(k, m)| (base(k), m)).collect();
    let mut want: Vec<(String, &Value)> = bundled.iter().filter(|(_, m)| m.is_object()).map(|(k, m)| (k.clone(), m)).collect();
    for r in roots {
        for f in files_under(r).into_iter().filter(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("safetensors"))) {
            let Ok(rel) = f.strip_prefix(r) else { continue };
            let name = rel.to_string_lossy().replace('/', "\\");
            if want.iter().any(|(k, _)| *k == name) {
                continue;
            }
            if let Some(m) = by_base.get(&base(&name)).filter(|m| m.is_object()) {
                want.push((name, m));
            }
        }
    }
    let mut n = 0;
    for (k, b) in want {
        let sig = note_sig(b);
        let next = match have.get(&k) {
            None => {
                let mut m = b.clone();
                m["bundle_sig"] = json!(sig);
                m
            }
            Some(cur) => {
                let untouched = match cur.get("bundle_sig").and_then(Value::as_str) {
                    Some(s) => s == note_sig(cur),
                    None => cur.get("note").and_then(Value::as_str).is_some_and(|n| LEGACY_NOTES.contains(&n)),
                };
                if !untouched || note_sig(cur) == sig {
                    continue;
                }
                let mut m = cur.clone();
                for f in NOTE_FIELDS {
                    match b.get(*f) {
                        Some(x) => m[*f] = x.clone(),
                        None => {
                            m.as_object_mut().map(|o| o.remove(*f));
                        }
                    }
                }
                m["bundle_sig"] = json!(sig);
                m
            }
        };
        if db.set_lora_meta(&k, &next).is_ok() {
            n += 1;
        }
    }
    n
}

/// 設定頁「模型總覽」：安裝精靈的清單（基本版、完整版）還缺哪些（外部模型資料夾裡有的也算有）。
/// 每一項的 maybe＝沒找到同名的檔，但同一類裡有大小完全一樣、檔名不同的檔（多半是改過名字的同一個模型）：附上那個檔的路徑，
/// 沒有就是 null。這種檔不算「有」（內建流程寫死檔名，直接當有會找不到），只是提醒使用者可能不用重新下載
pub(crate) fn missing_report(res: &Path, models: &Path, extra: &[crate::model_dirs::Dir]) -> Value {
    let mut indexes: std::collections::HashMap<String, Vec<(PathBuf, u64)>> = Default::default();
    let mut list = |which: &str| -> Vec<Value> {
        missing_models(res, models, which, extra)
            .iter()
            .map(|m| {
                let kind = m.rel.split('/').next().unwrap_or("").to_string();
                let idx = indexes.entry(kind.clone()).or_insert_with(|| crate::model_dirs::size_index(models, extra, &kind));
                let maybe = crate::model_dirs::find_renamed(idx, &m.rel, m.size).map(|p| p.to_string_lossy().to_string());
                json!({"rel": m.rel, "size": m.size, "download": m.url.is_some(), "maybe": maybe})
            })
            .collect()
    };
    json!({"basic": list("basic"), "full": list("full"),
           "total": {"basic": manifest(res, "basic").len(), "full": manifest(res, "full").len()}})
}

/// 安裝精靈「掃描這台電腦已有的模型」要比對的清單（完整版包含基本版）
pub(crate) fn manifest_wants(res: &Path) -> Vec<crate::model_dirs::Want> {
    let mut v: Vec<crate::model_dirs::Want> = vec![];
    for which in ["basic", "full"] {
        for m in manifest(res, which) {
            if !v.iter().any(|w| w.rel == m.rel) {
                v.push(crate::model_dirs::Want { rel: m.rel, size: m.size });
            }
        }
    }
    v
}

/// 「AI 改寫、生圖助手」包裡的語言模型檔（完整版清單 LLM/ 底下、歸到 pe 包的：9B、它的看圖檔、挑段模型）。
/// 回傳（檔名, 大小, 網址, sha256）；升級換掉官方 PE 時照這份補下載（server\api_pe.rs retire_old_models）
pub(crate) fn llm_pack_files(res: &Path) -> Vec<(String, u64, String, Option<String>)> {
    manifest(res, "full")
        .into_iter()
        .filter(|m| pack_of(&m.rel) == "pe")
        .filter_map(|m| {
            let name = m.rel.strip_prefix("LLM/")?.to_string();
            (!name.contains('/')).then_some(())?;
            Some((name, m.size, m.url?, m.sha256))
        })
        .collect()
}

/// 安裝清單（完整版）裡某個檔（相對 models 的路徑，例如 loras/moss/xxx.safetensors）的（大小, 網址, sha256）；
/// 清單沒有＝None。生成頁缺 MOSS LoRA 時一鍵下載用（/api/models/fetch_listed），只認清單、不收網頁給的網址
pub(crate) fn listed_file(res: &Path, rel: &str) -> Option<(u64, String, Option<String>)> {
    manifest(res, "full").into_iter().find(|m| m.rel == rel).and_then(|m| Some((m.size, m.url?, m.sha256)))
}

/// 開站時把程式附帶、已經裝進這份 ComfyUI 的東西換成這一版的（2026-10-31 使用者：「不管更新了什麼，能上的就直接上。
/// 儘量讓別人用到的跟我是一樣的」）：AI 改寫節點（有裝才換）、工作流程、附帶 LoRA、MOSS 補丁（moss_support＝Patchable 才蓋）。
/// 內容不一樣的舊檔（舊版，或使用者自己改過的）先搬到 ComfyUI 旁邊的 待刪除\繪匣更新備份_<時間>\。
/// 回傳換了哪幾樣（給畫面提醒重開 ComfyUI）；都一樣＝空的。在 ComfyUI 開起來之前呼叫（main.rs start_main）
pub fn sync_bundled(comfy_dir: &Path, res: &Path) -> Vec<String> {
    let Some(layout) = comfy_layout(comfy_dir) else { return vec![] };
    let code = comfy_code(comfy_dir);
    let comfyui = layout.root.clone();
    let extras = res.join("extras");
    let backup = code.parent().unwrap_or(&code).join("待刪除").join(format!("繪匣更新備份_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")));
    let not_cache = |f: &Path| !f.components().any(|c| c.as_os_str() == "__pycache__") && f.extension().is_none_or(|x| x != "pyc");
    let node = comfyui.join("custom_nodes").join(NODE);
    let mut jobs: Vec<(&str, PathBuf, PathBuf, PathBuf)> = vec![];
    if node.join("__init__.py").is_file() || node.join("pe_core.py").is_file() {
        jobs.push(("AI 改寫節點", extras.join(NODE), node, backup.join("custom_nodes").join(NODE)));
    }
    // 繪匣自己的 MOSS 小節點（4B 文字編碼器）：不管有沒有勾過什麼都放，沒用到也不佔資源
    jobs.push(("MOSS 小節點", extras.join(MOSS_NODES), comfyui.join("custom_nodes").join(MOSS_NODES), backup.join("custom_nodes").join(MOSS_NODES)));
    jobs.push(("工作流程", extras.join("workflows"), comfyui.join("user").join("default").join("workflows"), backup.join("user").join("default").join("workflows")));
    jobs.push(("附帶 LoRA", extras.join("loras"), comfyui.join("models").join("loras"), backup.join("models").join("loras")));
    let mut done = vec![];
    for (label, src, dst, bak) in jobs {
        match overlay(&src, &dst, &bak, true, not_cache) {
            Ok((0, _)) => {}
            Ok((n, _)) => {
                eprintln!("[更新元件] {label}：換了 {n} 個檔");
                done.push(label.to_string());
            }
            Err(e) => eprintln!("[更新元件] {label} 失敗：{e}"),
        }
    }
    if !layout.desktop && moss_support(comfy_dir) == MossSupport::Patchable {
        match moss_apply(&extras.join("comfy_moss"), &code, &backup, true) {
            Ok((0, _)) => {}
            Ok((n, _)) => {
                eprintln!("[更新元件] MOSS-Image 支援：換了 {n} 個檔");
                done.push("MOSS-Image 支援".to_string());
            }
            Err(e) => eprintln!("[更新元件] MOSS-Image 支援失敗：{e}"),
        }
    }
    done
}

/// sync_bundled 換了東西、ComfyUI 卻早就開著（不是生圖站開的，或生圖站重開前就在跑）：要重開 ComfyUI 才生效，
/// 生成頁顯示提醒（/api/state 的 models.comfy_updated）；重開 ComfyUI（/api/models/restart_comfy）後清掉
static UPDATED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
pub fn set_updated(v: Vec<String>) {
    *UPDATED.lock().unwrap_or_else(|e| e.into_inner()) = v;
}
pub fn updated() -> Vec<String> {
    UPDATED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 讀圖用的 mmproj 從哪下載：完整版清單裡 LLM/ 底下、檔名有 mmproj 又有 fam（qwen3.5／qwen3.6，跟 pe.rs 找檔的規則一樣）的那一筆。
/// 回傳（檔名, 大小, 網址, sha256）；清單裡沒有（例如 35B 是使用者自己下載的）＝None
pub(crate) fn mmproj_source(res: &Path, fam: &str) -> Option<(String, u64, String, Option<String>)> {
    manifest(res, "full").into_iter().find_map(|m| {
        let name = m.rel.strip_prefix("LLM/")?.to_string();
        let low = name.to_lowercase();
        (low.contains("mmproj") && low.contains(fam) && !name.contains('/')).then_some(())?;
        Some((name, m.size, m.url?, m.sha256))
    })
}

/// 檔案的 SHA256（小寫十六進位）。大檔要好幾秒，放在 spawn_blocking 裡呼叫
pub(crate) fn sha256_file(p: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---------------------------------------------------------------- 功能包（安裝精靈第三頁打勾自選）
/// （代號, 名字, 說明, 要先有哪一包）。模型檔照路徑歸包（pack_of）；基本版＝BASIC_PACKS，完整版＝全部
pub const PACKS: &[(&str, &str, &str, &str)] = &[
    ("anima", "Anima 動漫", "動漫生圖、圖生圖、局部重繪、構圖控制、放大的基礎，大部分功能都要它", ""),
    ("qwen", "Qwen 2.1 編輯", "改圖、多圖參考、四視圖，也能畫寫實照片（官方 Qwen Image 2.1 Turbo）", ""),
    ("qwen_pose", "Qwen 2.1 姿勢參考", "照人偶的姿勢畫（Fun ControlNet）", "qwen"),
    ("moss", "MOSS-Image 文生圖繪圖", "用文字畫圖：海報、招牌、版面框、漫畫台的對白（中英文都能寫）", ""),
    ("face", "修臉／修手", "生成後自動找出臉和手再重畫（SAM3.1）", ""),
    ("bg", "一鍵去背", "BiRefNet 去背模型", ""),
    ("pe", "AI 改寫、生圖助手", "通用語言模型 Qwen3.5-9B Defiant Fable（含看圖檔）＋挑段用的 Qwen3-Embedding-0.6B＋llama.cpp：AI 改寫提示詞、生圖助手、導演台分段", ""),
    // 2026-10-29 使用者：「把35b做成選配」。原本只能手動下載；17GB、要 32GB 記憶體，所以「完整版」也不勾
    ("pe_35b", "35B 大腦（選配）", "通用語言模型 Qwen3.6-35B-A3B Heretic（含看圖檔，約 17.9GB）：生圖助手、漫畫台、導演台寫得更準。要 32GB 記憶體以上；8GB 顯卡也能跑（一部分放 CPU）", "pe"),
    ("studio", "影音工作台、剪輯台工具", "Node.js、ffmpeg（約 150MB）。影片模型在下面的「H3 影片」包", ""),
    // 2026-10-30 LTX-2.3 退場：原本的「影片」包（10Eros v1.5、gemma、LTX 放大，約 43GB）拿掉，內建影片改用下面的 H3
    // 2026-10-30 使用者選「安裝精靈加 w4a8 包」：3070 8GB 實測 512×768 5 秒約 200 秒（第一百八十五筆）
    ("h3", "H3 影片（本機，選配）", "MiniMax H3＋10Eros-Max w4a8：在自己電腦做有聲影片（H3 影片台；生成頁「動起來」、導演台也用它）。主模型 14GB＋文字編碼器 15.7GB＋VAE 3.4GB，約 33GB；要 32GB 記憶體以上，8GB 顯卡能跑。8～12GB 顯卡建議另外裝 ComfyUI-KJNodes（低顯存切塊）", ""),
    // 2026-10-31 使用者：「把那幾個 LoRa 就我們剛剛下載的那個組合包…加進去新的安裝包裡面 讓人們可以用」
    ("moss_4b", "MOSS 省記憶體文字編碼器（選配）", "Qwen3.5-4B＋投影轉接器（約 4.8GB）：生成頁「MOSS 文字編碼器」切 4B，比 9B 省記憶體，畫面會有點不一樣", "moss"),
    ("moss_loras", "MOSS LoRA 合集（選配）", "gordonz 整理的 MOSS-Image LoRA 合集（約 14GB）：畫風、照片風、臉模、滑桿、加速、去色帶、動漫轉寫實。每顆在 LoRA 選單都有中文備註", "moss"),
    ("anima_loras", "Anima 常用 LoRA", "畫風、細節、光影、身材滑桿…（有些來自 Civitai，要 API key）", "anima"),
    ("qwen_loras", "Qwen 2.1 常用 LoRA", "寫實風、加速、滑桿…（有些來自 Civitai，要 API key）", "qwen"),
];
/// 「完整版」也不會自動勾的選配包（要自己勾）
pub const OPTIONAL_PACKS: &[&str] = &["pe_35b", "h3", "moss_4b", "moss_loras"];
/// 基本版：三種畫風的生圖功能都能用（models_basic.json 要跟這個一致，測試會檢查）
pub const BASIC_PACKS: &[&str] = &["anima", "qwen", "qwen_pose", "moss", "face", "bg"];

/// 模型檔（清單裡的相對路徑）屬於哪一包
pub fn pack_of(rel: &str) -> &'static str {
    let r = rel.to_lowercase();
    let (kind, name) = r.split_once('/').unwrap_or(("", r.as_str()));
    match kind {
        "llm" if name.contains("35b") => "pe_35b",
        "llm" => "pe",
        "loras" if name.starts_with("qwen21/") => "qwen_loras",
        "loras" if name.starts_with("anima-turbo-lora") => "anima", // Anima 的官方加速 LoRA：Turbo 選單要用
        "loras" if name.starts_with("moss/moss_speed_dmd") => "moss", // MOSS 快速（gen.js MOSS_FAST）要的加速 LoRA
        "loras" if name.starts_with("moss/moss_edit_chinoll") => "moss", // MOSS 照指令改圖預設的編輯 LoRA
        "loras" if name.starts_with("moss/moss-color-depth-control") => "moss", // MOSS 構圖控制（graphs.rs MOSS_CONTROL_LORA）
        "loras" if name.starts_with("moss/excellent-loras/") => "moss_loras",
        "moss_encoder_adapters" => "moss_4b",
        "text_encoders" if name.starts_with("qwen3.5-4b") => "moss_4b",
        "loras" => "anima_loras",
        "checkpoints" if name.starts_with("sam3") => "face",
        // H3 要排在 qwen3vl（Qwen 2.1）前面：它的文字編碼器也叫 qwen3vl_32b_minimax_h3…
        "diffusion_models" if name.starts_with("10eros_max_h3") => "h3",
        "text_encoders" if name.starts_with("qwen3vl_32b_minimax_h3") => "h3",
        "vae" if name.starts_with("minimax_h3") => "h3",
        "background_removal" => "bg",
        "geometry_estimation" | "upscale_models" => "anima",
        "model_patches" if name.starts_with("qwen_image") => "qwen_pose",
        "model_patches" => "anima",
        "diffusion_models" if name.starts_with("qwen_image") => "qwen",
        "diffusion_models" if name.starts_with("moss") => "moss",
        "diffusion_models" if name.starts_with("anima") => "anima",
        "text_encoders" if name.starts_with("qwen3vl") => "qwen",
        "text_encoders" if name.starts_with("qwen3.5") => "moss",
        "text_encoders" if name.starts_with("qwen_3_06b") => "anima",
        "vae" if name.starts_with("qwen_image_2.1") => "qwen",
        "vae" if name.starts_with("flux2") => "moss",
        "vae" if name.starts_with("qwen_image_vae") => "anima",
        _ => "other", // 以後清單加了新種類的檔：只有完整版會裝
    }
}

/// 2026-10-30 LTX-2.3 退場：以前「影片」包下載的檔（分類, 檔名）。只認這幾個名字，使用者自己放的其他 LTX 檔不碰
pub const LTX_LEFTOVERS: &[(&str, &str)] = &[
    ("checkpoints", "10Eros_v1.5_DMD_INT8_checkpoint.safetensors"),
    ("text_encoders", "gemma-3-12b-it-ablit-norms-biproj-fp8mixed.safetensors"),
    ("latent_upscale_models", "ltx-2.3-spatial-upscaler-x2-1.1.safetensors"),
];

/// 找得到的舊 LTX 檔（路徑, 大小）：ComfyUI 的 models\<分類>＋同分類的外部模型資料夾（checkpoints 也看主模型資料夾）；
/// junction 指到同一個檔只算一次
fn ltx_leftovers(models: &Path, extra: &[crate::model_dirs::Dir]) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = vec![];
    let mut seen: Vec<PathBuf> = vec![];
    for (kind, name) in LTX_LEFTOVERS {
        let dirs = std::iter::once(models.join(kind))
            .chain(extra.iter().filter(|d| d.kind == *kind || (*kind == "checkpoints" && d.kind == "diffusion_models")).map(|d| d.path.clone()));
        for d in dirs {
            let f = d.join(name);
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            let real = std::fs::canonicalize(&f).unwrap_or_else(|_| f.clone());
            if meta.is_file() && !seen.contains(&real) {
                seen.push(real);
                out.push((f, meta.len()));
            }
        }
    }
    out
}

/// 刪掉舊 LTX 檔 → (空出幾 bytes, 刪不掉的說明)
fn remove_ltx(files: &[(PathBuf, u64)]) -> (u64, Vec<String>) {
    let (mut freed, mut fails) = (0, vec![]);
    for (f, size) in files {
        match std::fs::remove_file(f) {
            Ok(()) => freed += size,
            Err(e) => fails.push(format!("{}：{e}", f.display())),
        }
    }
    (freed, fails)
}

/// 要裝哪幾包：{packs: [...]}，或舊的 {models: basic|full|none}。會自動補上「要先有」的那一包
pub fn packs_from(body: &Value) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = match (&body["packs"], body["models"].as_str()) {
        (Value::Array(a), _) => {
            let mut out = vec![];
            for x in a {
                let id = x.as_str().unwrap_or("");
                // 2026-10-30 拿掉的官方 PE 包、LTX 影片包：舊網頁、舊 MCP 還可能送來，安靜略過
                if id == "pe_official" || id == "video" {
                    continue;
                }
                if !PACKS.iter().any(|p| p.0 == id) {
                    return Err(format!("不認得的功能包：{id}"));
                }
                out.push(id.to_string());
            }
            out
        }
        (_, Some("basic")) => BASIC_PACKS.iter().map(|s| s.to_string()).collect(),
        (_, Some("full")) => PACKS.iter().map(|p| p.0).filter(|p| !OPTIONAL_PACKS.contains(p)).map(str::to_string).chain(["other".to_string()]).collect(),
        (_, Some("none")) | (_, None) => vec![],
        (_, Some(m)) => return Err(format!("models 只能是 basic、full、none（收到 {m}）")),
    };
    // 要先有的那一包
    for _ in 0..2 {
        let need: Vec<String> = PACKS
            .iter()
            .filter(|p| v.iter().any(|x| x == p.0) && !p.3.is_empty() && !v.iter().any(|x| x == p.3))
            .map(|p| p.3.to_string())
            .collect();
        v.extend(need);
    }
    let mut seen: Vec<String> = vec![];
    v.retain(|x| !seen.contains(x) && { seen.push(x.clone()); true });
    Ok(v)
}

/// 完整版清單裡、屬於這幾包、還沒有的模型
fn missing_in(res: &Path, models: &Path, packs: &[String], extra: &[crate::model_dirs::Dir]) -> Vec<ModelFile> {
    missing_models(res, models, "full", extra)
        .into_iter()
        .filter(|m| packs.iter().any(|p| p == pack_of(&m.rel)))
        .collect()
}

/// 精靈第三頁：每一包的大小、還缺多少、工具裝了沒（variant＝gpu::GpuPlan 的 variant：N 卡以外的 llama.cpp 是小很多的 Vulkan 版）
fn pack_view(res: &Path, p: &Plan, extra: &[crate::model_dirs::Dir], variant: &str) -> Vec<Value> {
    let all = manifest(res, "full");
    let miss = missing_models(res, &p.models(), "full", extra);
    let tools_ok = studio_tools(p).iter().all(|t| t.4);
    let llama_ok = p.llama_ok(variant);
    PACKS
        .iter()
        .map(|(id, label, desc, req)| {
            let mine: Vec<&ModelFile> = all.iter().filter(|m| pack_of(&m.rel) == *id).collect();
            let gone: Vec<ModelFile> = miss.iter().filter(|m| pack_of(&m.rel) == *id).cloned().collect();
            // 工具（llama.cpp 約 1.3GB 解開後、Vulkan 版約 0.1GB；Node.js＋ffmpeg 約 0.6GB）也算進大小
            let (tool_ok, tool_bytes) = match *id {
                "pe" => (llama_ok, if variant == "nvidia" { 1_300_000_000u64 } else { 130_000_000u64 }),
                "pe_35b" => (llama_ok, 0),
                "studio" => (tools_ok, 600_000_000u64),
                _ => (true, 0),
            };
            let mut s = model_summary(&gone);
            s["bytes"] = json!(s["bytes"].as_u64().unwrap_or(0) + if tool_ok { 0 } else { tool_bytes });
            json!({"id": id, "label": label, "desc": desc, "requires": req, "files": mine.len(),
                   "total": mine.iter().map(|m| m.size).sum::<u64>() + tool_bytes,
                   "tool": !matches!(*id, "pe" | "pe_35b" | "studio") || tool_ok, "optional": OPTIONAL_PACKS.contains(id), "missing": s})
        })
        .collect()
}

fn model_summary(miss: &[ModelFile]) -> Value {
    let nosrc: Vec<&str> = miss
        .iter()
        .filter(|m| m.url.is_none())
        .map(|m| m.rel.as_str())
        .collect();
    let civ = miss
        .iter()
        .filter(|m| m.url.as_deref().is_some_and(|u| u.contains("civitai.com")))
        .count();
    json!({"missing": miss.len(), "bytes": miss.iter().filter(|m| m.url.is_some()).map(|m| m.size).sum::<u64>(), "civitai": civ, "no_source": nosrc})
}

pub(crate) fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p))
            } else {
                out.push(p)
            }
        }
    }
    out
}

/// g＝這台電腦的 ComfyUI 該怎麼裝（gpu::plan_for：第二頁的選擇＋偵測到的顯示卡）
fn check(res: &Path, p: &Plan, g: &gpu::GpuPlan) -> Value {
    let data_info = reuse::data_info(&p.data_root);
    let data_ok = data_info["valid"] == true;
    let extra = crate::model_dirs::load(&p.data_root.join("data"));
    let ltx_left = ltx_leftovers(&p.models(), &extra);
    let layout = comfy_layout_with(&p.comfy_dir, &p.opt);
    let comfy = layout.is_some();
    let wf_src = res.join("extras").join("workflows");
    let wf_dst = p.comfyui().join("user").join("default").join("workflows");
    let wf_missing = files_under(&wf_src)
        .iter()
        .filter(|f| !wf_dst.join(f.file_name().unwrap_or_default()).exists())
        .count();
    // 程式附帶的 LoRA（私人版有 jgdxstyle；公開版沒附，精靈就不顯示這一列）
    let lora_src = res.join("extras").join("loras");
    let lora_bundled = files_under(&lora_src).len();
    let lora_missing = files_under(&lora_src)
        .iter()
        .filter(|f| {
            f.strip_prefix(&lora_src)
                .map(|r| !p.models().join("loras").join(r).exists())
                .unwrap_or(false)
        })
        .count();
    let free = maint::disk_usage(
        p.data_root
            .ancestors()
            .find(|a| a.exists())
            .unwrap_or(&p.data_root),
    )
    .map(|d| d.0);
    let free_comfy = maint::disk_usage(
        p.comfy_dir
            .ancestors()
            .find(|a| a.exists())
            .unwrap_or(&p.comfy_dir),
    )
    .map(|d| d.0);
    let variant = g.variant;
    let version = comfy.then(|| comfy_version(&p.comfy_dir)).flatten();
    let has_python = layout.as_ref().is_some_and(|l| !l.python.as_os_str().is_empty());
    // PyTorch 的版本（cu130、cu126、rocm7.2.1…）。N 卡舊卡（GTX 10 系列以前）配到 CUDA 13 的 PyTorch：這份 ComfyUI 不支援這張卡
    let torch_build = layout.as_ref().and_then(|l| gpu::torch_build(&l.python));
    let legacy_conflict = variant == "nvidia" && g.legacy && !g.own && torch_build.as_deref().is_some_and(|b| b.starts_with("cu13"));
    // 官方 AMD 免安裝版、這張 A 卡要的架構還沒裝：要換成 ROCm 10 PyTorch（裝完 ComfyUI 之後做；還沒裝 ComfyUI、會下載 AMD 版的也算）
    let rocm10_need = match &layout {
        Some(l) => gpu::rocm10_needed(l, g),
        None => !comfy && variant == "amd" && !g.own && !g.rocm10.is_empty(),
    };
    json!({
        "data": {"path": p.data_root.to_string_lossy(), "ok": data_ok, "valid": data_info["valid"], "problem": data_info["problem"], "images": data_info["images"],
                 "has_key": p.data_root.join("data").is_dir() && !settings::civitai_key(&settings::load(&p.data_root.join("data").join("settings.json"))).is_empty(),
                 "free": free},
        "comfy": {"path": p.comfy_dir.to_string_lossy(), "ok": comfy, "version": version,
                  "old": comfy_too_old(version.as_deref()), "min_version": COMFY_MIN_VERSION,
                  "has_python": has_python, "external": p.opt.external, "legacy_conflict": legacy_conflict,
                  "portable": layout.as_ref().map(|l| l.portable), "problem": comfy_problem_with(&p.comfy_dir, &p.opt),
                  "torch": layout.as_ref().and_then(|l| gpu::torch_kind(&l.python)),
                  "backend": layout.as_ref().and_then(gpu::comfy_backend),
                  "zluda_exe": layout.as_ref().and_then(gpu::zluda_exe).is_some(),
                  "rocm10_have": layout.as_ref().map(|l| gpu::rocm10_devices(&l.python)).unwrap_or_default(),
                  "inner": (!comfy).then(|| inner_install(&p.comfy_dir)).flatten().map(|d| d.to_string_lossy().to_string()),
                  // 這份 ComfyUI 跑不了 MOSS 的原因（能跑、安裝時會補好、或會下載新的 0.38.0＝null）：精靈的 MOSS 卡照這個警告
                  "moss_problem": comfy.then(|| moss_support(&p.comfy_dir).problem()).flatten().filter(|_| !comfy_upgradable(&p.comfy_dir)),
                  // 官方免安裝版比 COMFY_VERSION 舊：安裝與修復會升級（精靈 ComfyUI 卡顯示）
                  "upgrade_to": comfy_upgradable(&p.comfy_dir).then_some(COMFY_VERSION),
                  "exists": p.comfy_dir.exists(), "free": free_comfy, "download_size": comfy_package(variant, g.legacy).1},
        "variant": variant,
        "gpu": g.view(),
        "rocm10": {"need": rocm10_need, "devices": g.rocm10, "bytes": if g.rocm10.is_empty() { 0 } else { gpu::rocm10_bytes(&g.rocm10) }},
        "node": p.node().join("__init__.py").is_file(),
        "llama": p.llama_ok(variant),
        "tagger": tagger_ok(&p.tagger()),
        "studio_tools": studio_tools(p).iter().all(|t| t.4),
        "vcredist": vcredist_ok(),
        "workflows_missing": wf_missing,
        "loras_bundled": lora_bundled,
        "loras_missing": lora_missing,
        "models": {"basic": model_summary(&missing_models(res, &p.models(), "basic", &extra)),
                   "full": model_summary(&missing_models(res, &p.models(), "full", &extra))},
        "model_dirs": extra.len(),
        // 以前影片包留下的 LTX-2.3 檔：精靈提醒、使用者按了才刪（/api/setup/ltx_remove）
        "ltx_leftovers": {"files": ltx_left.iter().map(|f| f.0.to_string_lossy().to_string()).collect::<Vec<_>>(), "bytes": ltx_left.iter().map(|f| f.1).sum::<u64>()},
        "packs": pack_view(res, p, &extra, variant),
        "basic_packs": BASIC_PACKS,
    })
}

/// 把 src 底下的 .py 補丁蓋進 root。內容一樣的跳過；root 裡本來就有、不一樣的，backup_old＝true 就先搬到 backup（保留相對路徑）再蓋，
/// 搬不動就整個停下來、不蓋。回傳（寫了幾個檔, 被搬走的檔的相對路徑）
fn moss_apply(src: &Path, root: &Path, backup: &Path, backup_old: bool) -> Result<(usize, Vec<String>), String> {
    overlay(src, root, backup, backup_old, |f| f.extension().is_some_and(|x| x == "py")) // 說明.txt 不放進去
}

/// moss_apply 的本體：src 底下 keep 認可的檔蓋進 root（規則同 moss_apply）
fn overlay(src: &Path, root: &Path, backup: &Path, backup_old: bool, keep: impl Fn(&Path) -> bool) -> Result<(usize, Vec<String>), String> {
    let (mut written, mut backed) = (0, vec![]);
    for f in files_under(src) {
        if !keep(&f) {
            continue;
        }
        let Ok(rel) = f.strip_prefix(src) else {
            continue;
        };
        let to = root.join(rel);
        let new = std::fs::read(&f).map_err(|e| format!("讀 {} 失敗：{e}", f.display()))?;
        if to.is_file() {
            if std::fs::read(&to).is_ok_and(|old| old == new) {
                continue;
            }
            if backup_old {
                maint::move_path(&to, &backup.join(rel)).map_err(|e| format!("備份 {} 失敗（沒有蓋掉它）：{e}", rel.display()))?;
                backed.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        to.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| std::fs::write(&to, &new)).map_err(|e| format!("放入 {} 失敗：{e}", rel.display()))?;
        written += 1;
    }
    Ok((written, backed))
}

// ---------------------------------------------------------------- 安裝進度
#[derive(Default)]
struct Progress {
    running: bool,
    finished: bool,
    phase: String,
    /// 每一項：{key, label, status: wait|run|done|skip|error, done, total, error}
    items: Vec<Map<String, Value>>,
    log: Vec<String>,
    problems: Vec<String>,
    /// 這次安裝裡「整個檔都連不上」的網站和次數：同一個網站兩個檔都這樣（被擋、梯子沒開），後面的檔直接跳過
    down_hosts: std::collections::HashMap<String, u32>,
    /// 使用者按了「停止」而結束的（網頁用來分「取消」跟「失敗」）
    cancelled: bool,
    /// 這次安裝實際收到的位元組數（只增不減；續傳時 .part 本來就有的那段不算，算速度才準）
    recv: u64,
    /// 最近幾秒的（時間, recv），算下載速度用
    samples: std::collections::VecDeque<(Instant, u64)>,
}

/// 算下載速度看多久以內的紀錄
const SPEED_WINDOW: Duration = Duration::from_secs(20);

/// 總進度：（要下載的總位元組, 已經下載的位元組）。只算有大小的下載項（單位是 % 的解壓縮不算）；
/// 已經有了（skip）的不算進去；完成的算滿，其他照目前進度（最多算到總大小，免得估計值偏小時超過 100%）
fn bytes_overall(items: &[Map<String, Value>]) -> (u64, u64) {
    let (mut total, mut done) = (0u64, 0u64);
    for it in items {
        let t = it["total"].as_u64().unwrap_or(0);
        if t == 0 || it.get("unit").is_some_and(|u| u == "%") || it["status"] == "skip" {
            continue;
        }
        total += t;
        done += if it["status"] == "done" { t } else { it["done"].as_u64().unwrap_or(0).min(t) };
    }
    (total, done)
}

/// 位元組 → 「1.9 GB」這種（給錯誤訊息用）
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["位元組", "KB", "MB", "GB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} 位元組") } else { format!("{v:.1} {}", UNITS[i]) }
}

/// 檔案已經在、但大小不對：說清楚是哪個檔、為什麼、怎麼做
fn wrong_size_msg(dest: &Path, have: u64, want: u64) -> String {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let dir = dest.parent().map(|d| d.display().to_string()).unwrap_or_default();
    format!(
        "資料夾 {dir} 裡已經有一個叫「{name}」的檔案，但大小不對（現在 {}，需要 {}）：可能是上次下載到一半被中斷，或是別的版本的同名檔。請用檔案總管打開這個資料夾，把這個檔案刪掉（想留著的話，先改成別的名字或搬到別處），再按「重試」，程式會重新下載正確的檔案",
        human_bytes(have),
        human_bytes(want)
    )
}

/// 伺服器給的檔大小跟清單不一樣（part＝下載暫存檔 xxx.part）：對方換了檔或給錯檔，不下載；講清楚只少這一個、其他照裝
fn changed_msg(part: &Path, got: u64, want: u64) -> String {
    let name = part.file_name().map(|n| n.to_string_lossy().trim_end_matches(".part").to_string()).unwrap_or_default();
    format!(
        "「{name}」來源網站上的檔案（{got} 位元組）跟程式要的版本（{want} 位元組）不一樣，多半是上傳的人換了檔案，所以先不下載這一個。其他項目照常安裝，只是少了這個檔。請把這則訊息截圖給作者，等作者更新清單後再按「重試」"
    )
}

/// 下載完的檔 SHA256 跟清單（來源公布的）不一樣
fn sha_mismatch_msg(dest: &Path, url: &str) -> String {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    format!(
        "「{name}」下載完了，但內容跟 {host} 公布的檔案核對不上（SHA256 不同）：可能是下載途中壞掉，也可能是上傳的人換了檔案。壞掉的暫存檔已經丟掉，按「重試」會重新下載；一直出現這個訊息的話，請截圖給作者"
    )
}

/// GitHub 的檔案沒有鏡像（大陸沒開梯子常常連不上）：失敗時教使用者用別的方法自己下載、放到程式找檔案的位置
/// （fetch 看到檔案已經在、大小也對就直接跳過下載）。不是 GitHub 的網址回 None
fn manual_hint(url: &str, dest: &Path, size: Option<u64>) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    let host = u.host_str()?;
    if host != "github.com" && !host.ends_with(".githubusercontent.com") {
        return None;
    }
    let name = dest.file_name()?.to_string_lossy().to_string();
    let dir = dest.parent()?.display().to_string();
    let sz = size.map(|n| format!("，大小要剛好是 {n} 位元組（約 {}）", human_bytes(n))).unwrap_or_default();
    Some(format!(
        "也可以用別的方法把檔案下載下來（換個網路、開梯子、用瀏覽器或下載工具抓，或請朋友下載後傳給你）。網址：{url} 。下載好後，把檔案放進資料夾 {dir}（沒有這個資料夾就自己建一個），檔名要叫「{name}」{sz}，再按「重試」（剪輯台那邊是「再試一次」），程式發現檔案已經在、大小也對，就會跳過下載直接往下裝"
    ))
}

/// 放進一筆新的 (時間, recv) 並丟掉太舊的；回傳每秒位元組數（紀錄不到 1 秒、或沒在動＝0）
fn speed_push(samples: &mut std::collections::VecDeque<(Instant, u64)>, now: Instant, recv: u64) -> f64 {
    samples.push_back((now, recv));
    while samples.len() > 2 && now.duration_since(samples[0].0) > SPEED_WINDOW {
        samples.pop_front();
    }
    let (t0, r0) = samples[0];
    let dt = now.duration_since(t0).as_secs_f64();
    if dt < 1.0 {
        0.0
    } else {
        recv.saturating_sub(r0) as f64 / dt
    }
}

pub struct Setup {
    activity: std::sync::OnceLock<Arc<crate::cache::Control>>,
    settings_lock: std::sync::OnceLock<Arc<tokio::sync::Mutex<()>>>,
    local_settings_lock: Arc<tokio::sync::Mutex<()>>,
    pub config_path: PathBuf,
    pub res: PathBuf,
    http: reqwest::Client,
    prog: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    /// 桌面版：精靈完成時要做的事（第一次啟動＝切換到正式畫面；已經在正式畫面、換了位置＝重開程式）。回傳 "reload" 或 "restart"
    on_finish: Mutex<Option<FinishFn>>,
    /// 正式畫面：關掉／再開生圖站自己開的 ComfyUI（換 PyTorch 時 DLL 不能被占用）。第一次啟動的精靈沒有 ComfyUI 在跑，不用設
    comfy_hooks: Mutex<Option<(HookFn, HookFn)>>,
}

struct Cancelled;

type FinishFn = Box<dyn Fn(&Config) -> &'static str + Send + Sync>;
type HookFn = Box<dyn Fn() + Send + Sync>;

impl Setup {
    pub fn busy(&self) -> bool {
        self.prog.lock().unwrap_or_else(|e| e.into_inner()).running
    }
    pub fn new(config_path: &Path, res: &Path) -> Arc<Setup> {
        // 代理和下載來源設定：config.json 旁邊的 net.json（net.rs）
        crate::net::init(&config_path.with_file_name("net.json"));
        // 顯示卡選擇、ComfyUI 啟動方式：config.json 旁邊的 comfy.json（gpu.rs）
        gpu::init(&config_path.with_file_name("comfy.json"));
        let http = reqwest::Client::builder()
            .proxy(crate::net::proxy())
            // 不留閒置連線：代理是「開新連線時」才決定的，留著舊連線的話，改了代理設定或梯子開關後
            // 「測試連線」和下載還會走舊的路（測試員實測：關掉代理後還是從舊通道出去）
            .pool_max_idle_per_host(0)
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()
            .expect("建立 HTTP client");
        Arc::new(Setup {
            activity: std::sync::OnceLock::new(),
            settings_lock: std::sync::OnceLock::new(),
            local_settings_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_path: config_path.to_path_buf(),
            res: res.to_path_buf(),
            http,
            prog: Arc::new(Mutex::new(Progress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            on_finish: Mutex::new(None),
            comfy_hooks: Mutex::new(None),
        })
    }

    /// 正式畫面設定：stop＝關掉生圖站開的 ComfyUI、start＝再開一次（換 PyTorch 用）
    pub fn bind_runtime(&self, activity: Arc<crate::cache::Control>, settings_lock: Arc<tokio::sync::Mutex<()>>) {
        let _ = self.activity.set(activity);
        let _ = self.settings_lock.set(settings_lock);
    }

    pub fn set_comfy_hooks(&self, stop: impl Fn() + Send + Sync + 'static, start: impl Fn() + Send + Sync + 'static) {
        *self.comfy_hooks.lock().unwrap_or_else(|e| e.into_inner()) = Some((Box::new(stop), Box::new(start)));
    }

    fn comfy_hook(&self, start: bool) {
        if let Some((stop_f, start_f)) = self.comfy_hooks.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if start {
                start_f()
            } else {
                stop_f()
            }
        }
    }

    /// 設定（或換掉）精靈完成時要做的事：第一次啟動時是「切到正式畫面」，切過去之後換成「位置變了就重開」
    pub fn set_on_finish(&self, f: impl Fn(&Config) -> &'static str + Send + Sync + 'static) {
        *self.on_finish.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    pub fn config(&self) -> Option<Config> {
        Config::load(&self.config_path)
    }

    fn with<T>(&self, f: impl FnOnce(&mut Progress) -> T) -> T {
        f(&mut self.prog.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn say(&self, msg: impl Into<String>) {
        let line = format!("{} {}", chrono::Local::now().format("%H:%M:%S"), msg.into());
        eprintln!("[精靈] {line}");
        self.with(|p| {
            p.log.push(line);
            let n = p.log.len();
            if n > 200 {
                p.log.drain(..n - 200);
            }
        });
    }

    fn item(&self, key: &str, f: impl FnOnce(&mut Map<String, Value>)) {
        self.with(|p| {
            if let Some(it) = p.items.iter_mut().find(|i| i["key"] == key) {
                f(it);
            }
        });
    }

    fn add_item(&self, key: &str, label: &str, total: u64) {
        self.with(|p| {
            if !p.items.iter().any(|i| i["key"] == key) {
                let m = json!({"key": key, "label": label, "status": "wait", "done": 0, "total": total});
                p.items.push(m.as_object().cloned().unwrap_or_default());
            }
        });
    }

    fn set_status(&self, key: &str, status: &str, error: Option<String>) {
        self.item(key, |it| {
            it.insert("status".into(), json!(status));
            if let Some(e) = error {
                it.insert("error".into(), json!(e));
            }
        });
    }

    /// 給背景執行緒更新某一項的進度
    fn prog_handle(&self, key: &str) -> ItemProgress {
        ItemProgress {
            prog: self.prog.clone(),
            key: key.to_string(),
        }
    }

    fn status(&self) -> Value {
        self.with(|p| {
            // 總進度：已下載／總共、速度、大約剩多久（速度太慢或還沒開始就不給時間）
            let (total, done) = bytes_overall(&p.items);
            let speed = if p.running { speed_push(&mut p.samples, Instant::now(), p.recv) } else { 0.0 };
            let eta = (speed >= 1024.0 && total > done).then(|| ((total - done) as f64 / speed).ceil() as u64);
            json!({"running": p.running, "finished": p.finished, "cancelled": p.cancelled, "phase": p.phase, "items": p.items, "log": p.log, "problems": p.problems,
                   "overall": {"total": total, "done": done, "speed": speed.round() as u64, "eta": eta}})
        })
    }

    // ------------------------------------------------------------ 下載（可續傳）
    /// 下載到 dest（先寫 dest.part）；dest 已經存在且大小對就略過（所以自己用別的方法把檔案放到 dest、再按重試也行）。
    /// Civitai 網址會帶 token（金鑰從資料夾的 settings.json 讀，不印出）。
    /// 失敗的是 GitHub 的檔（沒有鏡像）：訊息後面補上「自己下載、放到哪裡」的做法
    async fn fetch(
        &self,
        key: &str,
        url: &str,
        dest: &Path,
        size: Option<u64>,
        data_root: &Path,
    ) -> Result<(), String> {
        self.fetch_checked(key, url, dest, size, None, false, data_root).await.map(|_| ())
    }

    /// 同 fetch，另外下載完核對 SHA256（清單有寫的話）：大小一樣、內容不同的檔（作者重傳、下載壞掉）也擋得下來。
    /// adopt＝來源的檔換了版本（大小跟清單不同）時照來源的新版下載，不擋；成功時回 Some(新版的大小)
    #[allow(clippy::too_many_arguments)]
    async fn fetch_checked(
        &self,
        key: &str,
        url: &str,
        dest: &Path,
        size: Option<u64>,
        sha256: Option<&str>,
        adopt: bool,
        data_root: &Path,
    ) -> Result<Option<u64>, String> {
        if let Ok(m) = std::fs::metadata(dest) {
            if size.is_none_or(|s| s == m.len()) {
                self.set_status(key, "skip", None);
                return Ok(None);
            }
            return Err(wrong_size_msg(dest, m.len(), size.unwrap_or(0)));
        }
        match self.fetch_core(key, url, dest, size, sha256, adopt, data_root).await {
            Err(e) if !self.cancel.load(Ordering::SeqCst) => match manual_hint(url, dest, size) {
                Some(h) => Err(format!("{}。{h}", e.trim_end_matches('。'))),
                None => Err(e),
            },
            r => r,
        }
    }

    /// 這一項在網頁上顯示的名字（記錄裡用，比檔案路徑好懂）
    fn label_of(&self, key: &str) -> String {
        self.with(|p| p.items.iter().find(|i| i["key"] == key).and_then(|i| i["label"].as_str().map(str::to_string))).unwrap_or_else(|| key.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_core(
        &self,
        key: &str,
        url: &str,
        dest: &Path,
        mut size: Option<u64>,
        sha256: Option<&str>,
        adopt: bool,
        data_root: &Path,
    ) -> Result<Option<u64>, String> {
        if let Some(d) = dest.parent() {
            tokio::fs::create_dir_all(d)
                .await
                .map_err(|e| format!("建立 {} 失敗：{e}", d.display()))?;
        }
        // 測試用：ANIMA_DL_MIRROR＝本機的鏡像網址，所有下載改抓「鏡像/檔名」（不用真的從網路下載幾 GB）。
        // 假鏡像的檔多半只有大小對，不核對 SHA256；要核對（鏡像放的是真檔）加 ANIMA_DL_VERIFY=1
        let mirror = std::env::var("ANIMA_DL_MIRROR").is_ok_and(|m| !m.is_empty());
        let mut sha256 = sha256.filter(|_| !mirror || std::env::var("ANIMA_DL_VERIFY").is_ok_and(|v| v == "1"));
        // 照來源新版下載了的話，新版的大小
        let mut adopted = None;
        let mut real = match std::env::var("ANIMA_DL_MIRROR") {
            Ok(m) if !m.is_empty() => format!(
                "{}/{}",
                m.trim_end_matches('/'),
                basename(url.split('?').next().unwrap_or(url))
            ),
            _ => url.to_string(),
        };
        if real.contains("civitai.com/api/download") {
            let k = settings::civitai_key(&settings::load(
                &data_root.join("data").join("settings.json"),
            ));
            if !k.is_empty() {
                real += if real.contains('?') { "&" } else { "?" };
                real += &format!("token={k}");
            }
        }
        let part = PathBuf::from(format!("{}.part", dest.display()));
        self.set_status(key, "run", None);
        // 同一個檔的幾個來源（Hugging Face 連不上換 hf-mirror.com…，見 net.rs）：這個來源連不上或不給檔就換下一個。
        // 換來源也接著 .part 下載（同一個檔，大小照樣核對）
        let urls = crate::net::candidates(&real);
        let host_of = |u: &str| reqwest::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        if urls.iter().all(|u| self.with(|p| p.down_hosts.get(&host_of(u)).copied().unwrap_or(0) >= 2)) {
            return Err(format!(
                "前面已經有兩個檔連不上 {}，這個先跳過了（開好梯子、或到「網路」換下載來源後，按「重試」會接著下載）",
                host_of(&urls[0])
            ));
        }
        let part_len = || std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut which = 0;
        // 連續「沒有進度」的失敗次數：有收到資料就歸零（大檔在常斷線的網路上也能一段一段下完）。
        // 從來沒連上過的來源只試 3 次，連得上但一直斷的試 8 次
        let (mut fails, mut responded) = (0u32, false);
        let last_err = loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("已取消".into());
            }
            let before = part_len();
            let r = self.fetch_once(key, &urls[which], &part, size).await;
            let progressed = part_len() > before;
            let r = match r {
                Err(Changed(got, _)) if adopt && adopted.is_none() => {
                    // 上傳的人換了新版：照來源的新版重新下載。舊的 .part 是舊版下載到一半的，丟掉；
                    // 清單的 SHA256 是舊版的，不核對
                    let _ = tokio::fs::remove_file(&part).await;
                    self.say(format!(
                        "{}：來源網站換了新版的檔案（清單記的 {} 位元組，現在是 {got} 位元組），改下載新版",
                        self.label_of(key),
                        size.unwrap_or(0)
                    ));
                    (size, sha256, adopted) = (Some(got), None, Some(got));
                    self.item(key, |it| {
                        it.insert("total".into(), json!(got));
                    });
                    continue;
                }
                Err(Changed(_, e)) => Err(NoFile(e)),
                r => r,
            };
            match r {
                Ok(()) => {
                    if let Some(want) = sha256 {
                        self.say(format!("{} 下載好了，核對檔案內容中…", self.label_of(key)));
                        let p = part.clone();
                        let got = tokio::task::spawn_blocking(move || sha256_file(&p))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r.map_err(|e| e.to_string()))
                            .map_err(|e| format!("核對檔案失敗：{e}"))?;
                        if !got.eq_ignore_ascii_case(want) {
                            // 內容跟作者公布的不一樣（下載壞了或對方換了檔）：.part 是程式自己的暫存檔，丟掉下次重抓
                            let _ = tokio::fs::remove_file(&part).await;
                            return Err(sha_mismatch_msg(dest, url));
                        }
                    }
                    tokio::fs::rename(&part, dest)
                        .await
                        .map_err(|e| format!("改名失敗：{e}"))?;
                    crate::net::used(&urls[which]);
                    self.set_status(key, "done", None);
                    return Ok(adopted);
                }
                Err(Down(e) | NoFile(e)) if which + 1 < urls.len() => {
                    which += 1;
                    let next = reqwest::Url::parse(&urls[which]).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                    self.say(format!("{}：{e} → 改從 {next} 下載", self.label_of(key)));
                }
                Err(Fatal(e) | NoFile(e) | Changed(_, e)) => return Err(e),
                Err(err) => {
                    // Retry＝連上了才斷（伺服器有回應）；Down＝根本連不上
                    let (was_retry, e) = match err {
                        Retry(e) => (true, e),
                        Down(e) => (false, e),
                        Fatal(e) | NoFile(e) | Changed(_, e) => return Err(e),
                    };
                    responded |= progressed || was_retry;
                    fails = if progressed { 1 } else { fails + 1 };
                    if fails >= if responded { 8 } else { 3 } {
                        break e;
                    }
                    let wait = 5 * fails.min(6);
                    self.say(format!("{} 中斷（連續第 {fails} 次）：{}。{wait} 秒後接著下載", self.label_of(key), e.trim_end_matches('。')));
                    tokio::time::sleep(Duration::from_secs(wait as u64)).await;
                }
            }
        };
        if !responded {
            self.with(|p| {
                for u in &urls {
                    *p.down_hosts.entry(host_of(u)).or_default() += 1;
                }
            });
        }
        Err(format!("下載一直失敗：{last_err}"))
    }

    async fn fetch_once(
        &self,
        key: &str,
        url: &str,
        part: &Path,
        size: Option<u64>,
    ) -> Result<(), FetchErr> {
        let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if size == Some(have) && have > 0 {
            return Ok(()); // 上次已經下載完、只差改名
        }
        let mut rb = self.http.get(url);
        if have > 0 {
            rb = rb.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut r = rb.send().await.map_err(|e| Down(crate::net::explain(&e, url)))?;
        let code = r.status().as_u16();
        let ctype = r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if code == 401
            || code == 403
            || r.url().path().contains("/login")
            || ctype.starts_with("text/html")
        {
            return Err(NoFile(if url.contains("civitai.com") {
                "要登入才能下載（Civitai 的模型：到「設定」填入你的 Civitai API key 再重試）".into()
            } else {
                format!("伺服器沒有給檔案（HTTP {code}，可能要登入或網址失效）")
            }));
        }
        if code == 416 {
            // 伺服器說範圍不對：.part 比檔案還大，從頭來
            let _ = std::fs::remove_file(part);
            return Err(Retry("續傳位置不對，重新下載".into()));
        }
        if code >= 400 {
            return Err(if code >= 500 || code == 429 {
                Retry(format!("HTTP {code}"))
            } else {
                NoFile(format!("下載失敗（HTTP {code}）"))
            });
        }
        let resume = have > 0 && code == 206;
        let start = if resume { have } else { 0 };
        let total = r.content_length().map(|n| n + start).or(size);
        if let (Some(t), Some(want)) = (total, size) {
            if t != want {
                // 伺服器上的檔案跟清單不一樣（換版了或給錯檔）：不下載，免得存成大小不對的檔
                return Err(Changed(t, changed_msg(part, t, want)));
            }
        }
        self.item(key, |it| {
            it.insert("done".into(), json!(start));
            if let Some(t) = total {
                it.insert("total".into(), json!(t));
            }
        });
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume)
            .truncate(!resume)
            .open(part)
            .await
            .map_err(|e| Fatal(format!("寫入 {} 失敗：{e}", part.display())))?;
        let (mut done, mut tick) = (start, Instant::now());
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                let _ = f.flush().await;
                return Err(Fatal("已取消".into()));
            }
            let chunk = match tokio::time::timeout(Duration::from_secs(90), r.chunk()).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    let _ = f.flush().await;
                    return Err(Retry(crate::net::explain(&e, url)));
                }
                Err(_) => {
                    let _ = f.flush().await;
                    return Err(Retry("90 秒沒有收到資料".into()));
                }
            };
            f.write_all(&chunk)
                .await
                .map_err(|e| Fatal(format!("寫入失敗（硬碟滿了？）：{e}")))?;
            done += chunk.len() as u64;
            self.with(|p| p.recv += chunk.len() as u64);
            if tick.elapsed() > Duration::from_millis(400) {
                tick = Instant::now();
                self.item(key, |it| {
                    it.insert("done".into(), json!(done));
                });
            }
        }
        f.flush().await.map_err(|e| Fatal(e.to_string()))?;
        drop(f);
        self.item(key, |it| {
            it.insert("done".into(), json!(done));
        });
        if let Some(t) = total.filter(|&t| t != done) {
            return Err(Retry(format!("大小不符：收到 {done}，應該是 {t}")));
        }
        Ok(())
    }

    // ------------------------------------------------------------ 安裝流程
    fn begin(&self) -> bool {
        self.with(|p| {
            if p.running {
                return false;
            }
            *p = Progress {
                running: true,
                ..Default::default()
            };
            true
        })
    }

    /// packs＝要裝的功能包；None＝只補影音工作台的工具（剪輯台的「下載剪輯工具」）
    async fn run(self: Arc<Self>, plan: Plan, packs: Option<Vec<String>>) {
        self.cancel.store(false, Ordering::SeqCst);
        // 安裝期間不讓電腦睡著；這個函式怎麼結束（完成、取消、失敗）guard 都會放掉
        let _awake = SleepGuard::start();
        // 放在另一個工作裡跑：裡面就算出錯當掉，這裡也一定會把「安裝中」清掉，不會卡住
        let me = self.clone();
        let r = match tokio::spawn(async move { me.run_steps(&plan, packs.as_deref()).await }).await {
            Ok(r) => r,
            Err(e) => {
                self.problem(format!("安裝程式內部錯誤：{e}"));
                Ok(())
            }
        };
        let stopped = r.is_err();
        if stopped {
            self.say("已取消；已經下載的部分留著，下次會接著下載");
            self.with(|p| {
                p.cancelled = true;
                // 停在一半的項目不要還顯示「進行中」
                for it in p.items.iter_mut().filter(|i| i["status"] == "run") {
                    it.insert("status".into(), json!("stop"));
                }
                p.problems
                    .push("安裝被停止了，還沒裝完的項目再安裝一次會接著做（已經下載的部分不會重抓）".into())
            });
        }
        let saved = self.config().is_some_and(|c| c.problem().is_none());
        self.with(|p| {
            p.running = false;
            p.finished = true;
            p.phase = if p.cancelled {
                "已停止".into()
            } else if p.problems.is_empty() {
                "完成".into()
            } else {
                "安裝還沒完成，有幾件事要處理".into()
            };
            if !saved {
                p.problems
                    .insert(0, "資料夾或 ComfyUI 還沒準備好，暫時不能開始使用".into());
            }
        });
    }

    fn phase(&self, s: &str) {
        self.with(|p| p.phase = s.to_string());
        self.say(s);
    }

    fn problem(&self, s: String) {
        self.say(format!("問題：{s}"));
        self.with(|p| p.problems.push(s));
    }

    fn check_cancel(&self) -> Result<(), Cancelled> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    async fn run_steps(&self, p: &Plan, packs: Option<&[String]>) -> Result<(), Cancelled> {
        // 只補影音工作台的工具（剪輯台頁面的「下載剪輯工具」按鈕）
        let Some(packs) = packs else {
            for t in studio_tools(p).iter().filter(|t| !t.4) {
                self.add_item(t.0, &format!("下載 {}", t.3), t.1 .1);
            }
            return self.install_tools(p).await;
        };
        let has = |id: &str| packs.iter().any(|x| x == id);
        // 先列出所有項目，網頁才看得到整張清單
        self.add_item("data", "資料夾", 0);
        let need_vc = !vcredist_ok();
        if need_vc {
            self.add_item("vcredist", "下載並安裝 VC++ 執行階段（會跳出系統確認視窗）", 0);
        }
        // 顯示卡：照第二頁的選擇（api_run 已經存到 comfy.json）和偵測結果。N 卡照舊；
        // A 卡、Intel 下載各自的 ComfyUI 版本（實驗性），N 卡以外的 llama.cpp 用 Vulkan 版；沒有能用的顯示卡用 NVIDIA 版＋CPU
        let g = gpu::current_plan();
        let variant = g.variant;
        let llama = llama_files(variant);
        let need_comfy = !comfy_ok(&p.comfy_dir);
        if need_comfy && !g.own && variant != "unknown" {
            match variant {
                "amd" => self.add_item("comfy.7z", "下載 ComfyUI 0.43.0（AMD 版，實驗性）", COMFY_AMD_SIZE),
                "intel" => self.add_item("comfy.7z", "下載 ComfyUI 0.43.0（Intel 版，實驗性）", COMFY_INTEL_SIZE),
                "nvidia" if g.legacy => self.add_item("comfy.7z", "下載 ComfyUI 0.43.0（NVIDIA 舊卡版）", COMFY_CU126_SIZE),
                _ => self.add_item("comfy.7z", "下載 ComfyUI 0.43.0", COMFY_SIZE),
            }
            self.add_item("comfy.unpack", "解壓縮 ComfyUI", 100);
            self.item("comfy.unpack", |it| {
                it.insert("unit".into(), json!("%")); // 進度是百分比，不是位元組
            });
        }
        // 已經有的官方免安裝版比 COMFY_VERSION 舊：升級（git 換版本＋補 Python 套件），換掉的檔備份到 待刪除
        let upgrade = !need_comfy && comfy_upgradable(&p.comfy_dir);
        if upgrade {
            self.add_item("comfy_upgrade", &format!("把 ComfyUI 升級到 {COMFY_VERSION}"), 0);
        }
        // 免安裝版沒有這張 A 卡的架構（RX 5000／6000、780M…）：裝完再換成 ROCm 10 PyTorch
        let rocm10 = match comfy_layout(&p.comfy_dir) {
            Some(l) => gpu::rocm10_needed(&l, &g),
            None => need_comfy && variant == "amd" && !g.own && !g.rocm10.is_empty(),
        };
        if rocm10 {
            self.add_item("rocm10", "下載支援這張 A 卡的 PyTorch（ROCm 10，實驗性）", gpu::rocm10_bytes(&g.rocm10));
            self.add_item("rocm10.install", "換上新的 PyTorch", 0);
        }
        for (k, l) in [("node", "AI 改寫節點"), ("moss_nodes", "MOSS 小節點"), ("workflows", "ComfyUI 工作流程")] {
            self.add_item(k, l, 0);
        }
        // 程式附帶的畫風 LoRA：公開版沒附（資料夾是空的），就不要在進度頁列一個根本沒東西的項目
        if !files_under(&self.res.join("extras").join("loras")).is_empty() {
            self.add_item("loras", "程式附帶的畫風 LoRA（jgdxstyle）", 0);
        }
        self.add_item("comfy_moss", "ComfyUI 的 MOSS-Image 支援", 0);
        // 工具跟著功能包：AI 改寫要 llama.cpp、影音工作台要 Node.js／ffmpeg
        let extras = has("studio");
        let need_llama = has("pe") && !p.llama_ok(variant);
        if need_llama {
            for (u, n) in llama {
                self.add_item(basename(u), &format!("下載 {}", basename(u)), *n);
            }
            self.add_item("llama.unpack", "解壓縮 llama.cpp", 0);
        }
        if extras {
            for t in studio_tools(p).iter().filter(|t| !t.4) {
                self.add_item(t.0, &format!("下載 {}", t.3), t.1 .1);
            }
        }
        for (f, n) in TAGGER_FILES {
            self.add_item(&format!("tagger/{f}"), &format!("反推模型 {f}"), *n);
        }
        self.add_item("lora_meta", "LoRA 說明與觸發詞", 0);
        let miss = missing_in(&self.res, &p.models(), packs, &crate::model_dirs::load(&p.data_root.join("data")));
        for m in &miss {
            self.add_item(&format!("model/{}", m.rel), &model_label(m), m.size);
        }

        // 1. 資料夾
        self.phase("建立資料夾");
        match std::fs::create_dir_all(p.data_root.join("data")) {
            Ok(()) => self.set_status("data", "done", None),
            Err(e) => {
                self.set_status("data", "error", Some(e.to_string()));
                self.problem(format!(
                    "建立 {} 失敗：{e}",
                    p.data_root.join("data").display()
                ));
                return Ok(());
            }
        }

        // 1b. VC++ 執行階段：裝不起來不擋住其他項目，列在「要處理的事」
        if need_vc {
            self.check_cancel()?;
            self.phase("安裝 VC++ 執行階段（請在系統確認視窗按「是」）");
            if let Err(e) = self.install_vcredist(p).await {
                self.check_cancel()?;
                self.set_status("vcredist", "error", Some(e.clone()));
                self.problem(format!(
                    "VC++ 執行階段：{e}。沒有它 ComfyUI 會開不起來；可以重按一次安裝，或自己到 {VCREDIST_URL} 下載安裝"
                ));
            }
        }

        // 2. ComfyUI
        if need_comfy {
            self.check_cancel()?;
            if g.own {
                self.problem("選了「用我自己已經能跑的 ComfyUI」，但這個位置找不到 ComfyUI：回第二頁選 ComfyUI 所在的資料夾".into());
                return Ok(());
            }
            if variant == "unknown" {
                self.problem("偵測不到這台電腦的顯示卡，不知道該下載哪一版 ComfyUI：回第二頁在「ComfyUI 版本」選一個".into());
                return Ok(());
            }
            if let Err(e) = self.install_comfy(p, variant, g.legacy).await {
                self.check_cancel()?;
                self.problem(format!("ComfyUI：{e}"));
                return Ok(());
            }
        }
        let cfg = Config {
            data_root: p.data_root.clone(),
            comfy_dir: p.comfy_dir.clone(),
        };
        match cfg.save(&self.config_path) {
            Ok(()) => self.say(format!(
                "位置已記住：資料 {}、ComfyUI {}",
                cfg.data_root.display(),
                cfg.comfy_dir.display()
            )),
            Err(e) => self.problem(format!("存設定檔 {} 失敗：{e}", self.config_path.display())),
        }

        // 2a. 升級舊的免安裝版。失敗不擋其他項目：舊版照樣能開（只是新功能用不到），檔案有備份
        if upgrade {
            self.check_cancel()?;
            if let Err(e) = self.upgrade_comfy(p).await {
                self.check_cancel()?;
                self.problem(format!("ComfyUI 升級：{e}"));
            }
        }

        // 2b. A 卡：換成支援這張卡的 PyTorch（ROCm 10）。失敗不擋其他項目，ComfyUI 照樣能開（只是用不到這張卡）
        if rocm10 {
            self.check_cancel()?;
            match comfy_layout(&p.comfy_dir).filter(|l| gpu::rocm10_needed(l, &g)) {
                Some(l) => {
                    if let Err(e) = self.install_rocm10(p, &l.python, &g.rocm10).await {
                        self.check_cancel()?;
                        self.problem(format!("A 卡的 PyTorch：{e}"));
                    }
                }
                None => {
                    self.set_status("rocm10", "skip", None);
                    self.set_status("rocm10.install", "skip", None);
                }
            }
        }

        // 3. 節點、工作流程、LoRA（從程式附帶的檔案複製，已經有的不覆蓋）
        self.phase("放入 AI 改寫節點與工作流程");
        // 2026-10-31 起已經有的也換成這一版的（不一樣的舊檔搬到 待刪除\繪匣更新備份_<時間>\），跟開站時的 sync_bundled 一樣
        let bak = p.comfyui().join("待刪除").join(format!("繪匣更新備份_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")));
        self.sync_into("node", &self.res.join("extras").join(NODE), &p.node(), &bak.join("custom_nodes").join(NODE));
        self.sync_into("moss_nodes", &self.res.join("extras").join(MOSS_NODES), &p.comfyui().join("custom_nodes").join(MOSS_NODES), &bak.join("custom_nodes").join(MOSS_NODES));
        self.sync_into(
            "workflows",
            &self.res.join("extras").join("workflows"),
            &p.comfyui().join("user").join("default").join("workflows"),
            &bak.join("user").join("default").join("workflows"),
        );
        self.sync_into("loras", &self.res.join("extras").join("loras"), &p.models().join("loras"), &bak.join("models").join("loras"));
        // 剛下載的 ComfyUI（need_comfy）核心檔是官方原版，不用備份
        self.patch_moss(p, need_comfy);

        // 4. llama.cpp（勾了 AI 改寫才裝）
        if need_llama {
            self.phase(if variant == "nvidia" { "下載 llama.cpp（AI 改寫用）" } else { "下載 llama.cpp Vulkan 版（AI 改寫用）" });
            let mut zips = vec![];
            for (u, n) in llama {
                self.check_cancel()?;
                let z = p.dl().join(basename(u));
                match self.fetch(basename(u), u, &z, Some(*n), &p.data_root).await {
                    Ok(()) => zips.push(z),
                    Err(e) => {
                        self.check_cancel()?;
                        self.set_status(basename(u), "error", Some(e.clone()));
                        self.problem(format!("llama.cpp 下載失敗：{e}"));
                    }
                }
            }
            if zips.len() == llama.len() {
                self.set_status("llama.unpack", "run", None);
                let bin = p.node().join("bin");
                let zs = zips.clone();
                match tokio::task::spawn_blocking(move || unzip_all(&zs, &bin))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                {
                    Ok(()) => {
                        self.set_status("llama.unpack", "done", None);
                        self.stash(&zips, &p.data_root);
                        let _ = std::fs::remove_dir(p.dl()); // 空了才會刪得掉
                    }
                    Err(e) => {
                        // 壓縮檔是這次自己下載的：壞掉就丟掉，下次重新下載（不然會一直「已經有了」然後一直解失敗）
                        for z in &zips {
                            let _ = std::fs::remove_file(z);
                        }
                        self.set_status("llama.unpack", "error", Some(e.clone()));
                        self.problem(format!("llama.cpp 解壓縮失敗（下次會重新下載）：{e}"));
                    }
                }
            }
        }

        // 5. 影音工作台的工具（勾了影音工作台才裝）
        if extras {
            self.install_tools(p).await?;
        }

        // 6. 反推模型
        self.phase("下載反推模型");
        for (f, n) in TAGGER_FILES {
            self.check_cancel()?;
            let key = format!("tagger/{f}");
            if let Err(e) = self
                .fetch(
                    &key,
                    &format!("{TAGGER}{f}"),
                    &p.tagger().join(f),
                    Some(*n),
                    &p.data_root,
                )
                .await
            {
                self.check_cancel()?;
                self.set_status(&key, "error", Some(e.clone()));
                self.problem(format!("反推模型 {f}：{e}"));
            }
        }

        // 7. LoRA 說明（資料庫裡還沒有任何 LoRA 說明時，匯入程式附的 lora設定.json）
        self.import_lora_meta(p);

        // 8. 生圖模型（同時下載 3 個）。LoRA 在別的資料夾已經有同一顆（大小、SHA256 都對）就不下載
        let miss: Vec<ModelFile> = {
            let lora_miss = miss.iter().any(|m| m.rel.starts_with("loras/") && m.sha256.is_some());
            let index = if lora_miss { crate::model_dirs::size_index(&p.models(), &crate::model_dirs::load(&p.data_root.join("data")), "loras") } else { vec![] };
            let mut cache = Default::default();
            let mut left = vec![];
            for m in miss {
                let found = if m.rel.starts_with("loras/") { same_lora_elsewhere(&index, &mut cache, &m) } else { None };
                match found {
                    Some(f) if remember_alias(&p.models(), &m.rel, &f).is_ok() => {
                        self.set_status(&format!("model/{}", m.rel), "skip", Some(format!("已經有同一顆：{}", f.display())));
                    }
                    _ => left.push(m),
                }
            }
            left
        };
        if !miss.is_empty() {
            self.phase(&format!("下載生圖模型（{} 個檔）", miss.len()));
            let jobs: Vec<(ModelFile, String)> = miss
                .iter()
                .filter_map(|m| m.url.clone().map(|u| (m.clone(), u)))
                .collect();
            let results: Vec<(String, Result<(), String>)> = futures_util::stream::iter(jobs)
                .map(|(m, u)| async move {
                    let key = format!("model/{}", m.rel);
                    // Civitai 的作者常在同一個版本重傳檔案（清單的網址有釘 fileId，大小還是會變）：照來源的新版下載。
                    // Hugging Face、GitHub 的網址釘了 commit／版本，大小不同代表給錯檔，照樣擋
                    let adopt = u.contains("civitai.com/");
                    let r = self
                        .fetch_checked(
                            &key,
                            &u,
                            &rel_path(&p.models(), &m.rel),
                            Some(m.size),
                            m.sha256.as_deref(),
                            adopt,
                            &p.data_root,
                        )
                        .await;
                    if let Ok(Some(n)) = r {
                        if let Err(e) = remember_adopted(&p.models(), &m.rel, n, &u) {
                            self.say(format!("{}：新版下載好了，但記錄版本失敗（{e}），下次檢查可能會再當成缺檔", model_label(&m)));
                        }
                    }
                    let r = r.map(|_| ());
                    if let Err(e) = &r {
                        self.set_status(&key, "error", Some(e.clone()));
                    }
                    (m.rel, r)
                })
                .buffer_unordered(3)
                .collect()
                .await;
            self.check_cancel()?;
            for m in miss.iter().filter(|m| m.url.is_none()) {
                self.set_status(
                    &format!("model/{}", m.rel),
                    "error",
                    Some("沒有下載來源".into()),
                );
            }
            let bad: Vec<&(String, Result<(), String>)> =
                results.iter().filter(|r| r.1.is_err()).collect();
            if !bad.is_empty() {
                let login = bad
                    .iter()
                    .filter(|r| r.1.as_ref().err().is_some_and(|e| e.contains("登入")))
                    .count();
                let mut s = format!("還有 {} 個模型沒下載成功", bad.len());
                if login > 0 {
                    s += &format!("（其中 {login} 個是 Civitai 要登入：先到「設定」填 Civitai API key，再回來按「重試」）");
                }
                self.problem(s);
            }
            let nosrc = miss.iter().filter(|m| m.url.is_none()).count();
            if nosrc > 0 {
                self.problem(format!(
                    "{nosrc} 個模型沒有下載來源（進度頁裡標紅色的那幾項），要自己下載好，放進 ComfyUI 的 models 資料夾裡對應的子資料夾"
                ));
            }
        }
        Ok(())
    }

    /// Node.js、ffmpeg：下載 zip → 解到 tools\<名稱>\ → zip 搬到待刪除。已經有的略過；失敗只記問題、不中斷其他項目
    async fn install_tools(&self, p: &Plan) -> Result<(), Cancelled> {
        let todo: Vec<_> = studio_tools(p).into_iter().filter(|t| !t.4).collect();
        if todo.is_empty() {
            return Ok(());
        }
        self.phase("下載影音工作台的工具（Node.js、ffmpeg，約 150MB）");
        let mut used = vec![];
        for (key, (u, n), sub, label, _) in todo {
            self.check_cancel()?;
            let z = p.dl().join(basename(u));
            if let Err(e) = self.fetch(key, u, &z, Some(n), &p.data_root).await {
                self.check_cancel()?;
                self.set_status(key, "error", Some(e.clone()));
                self.problem(format!("{label} 下載失敗：{e}"));
                continue;
            }
            self.set_status(key, "run", None);
            self.say(format!("解壓縮 {}", basename(u)));
            let (zs, dir) = (vec![z.clone()], p.tools().join(sub));
            match tokio::task::spawn_blocking(move || unzip_all(&zs, &dir))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            {
                Ok(()) => {
                    self.set_status(key, "done", None);
                    used.push(z);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&z); // 壞掉的壓縮檔丟掉，下次重新下載
                    self.set_status(key, "error", Some(e.clone()));
                    self.problem(format!("{label} 解壓縮失敗（下次會重新下載）：{e}"));
                }
            }
        }
        if !used.is_empty() {
            self.stash(&used, &p.data_root);
        }
        let _ = std::fs::remove_dir(p.dl()); // 空了才會刪得掉
        Ok(())
    }

    /// 下載微軟的 vc_redist.x64.exe，用安靜模式裝（會跳 UAC 確認視窗），裝完再檢查一次。安裝檔搬去「待刪除」
    async fn install_vcredist(&self, p: &Plan) -> Result<(), String> {
        let exe = p.dl().join("vc_redist.x64.exe");
        self.fetch("vcredist", VCREDIST_URL, &exe, None, &p.data_root).await?;
        self.set_status("vcredist", "run", None);
        // ShellExecuteEx 會一直卡到使用者回答 UAC 視窗：放在另一條執行緒，這裡每 0.5 秒看一次有沒有按「停止」；
        // 等超過 20 秒還沒有結果，畫面提示「權限確認視窗可能躲在後面」
        let (tx, rx) = std::sync::mpsc::channel();
        let abandon = Arc::new(AtomicBool::new(false));
        let (e, ab) = (exe.clone(), abandon.clone());
        std::thread::spawn(move || {
            let _ = tx.send(run_and_wait(&e, "/install /passive /norestart", &ab));
        });
        let t0 = Instant::now();
        let mut warned = false;
        let code = loop {
            match rx.try_recv() {
                Ok(r) => break r?,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Err("安裝程式沒有回應就結束了".into()),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            if self.cancel.load(Ordering::SeqCst) {
                abandon.store(true, Ordering::SeqCst);
                return Err("已取消".into());
            }
            if !warned && t0.elapsed() > VC_UAC_HINT_AFTER {
                warned = true;
                self.say(VC_UAC_HINT.to_string());
                self.item("vcredist", |it| {
                    it.insert("hint".into(), json!(VC_UAC_HINT));
                });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        self.item("vcredist", |it| {
            it.remove("hint");
        });
        let reboot = vcredist_result(code)?;
        self.stash(&[exe], &p.data_root);
        if !vcredist_ok() {
            return Err(format!("安裝程式結束了（代碼 {code}），但系統裡還是找不到新版的 msvcp140.dll"));
        }
        self.set_status("vcredist", "done", None);
        self.say(if reboot {
            "VC++ 執行階段裝好了（Windows 說重開機後才完全生效：畫圖有問題的話重開一次機）".to_string()
        } else {
            "VC++ 執行階段裝好了".to_string()
        });
        Ok(())
    }

    /// A 卡：用免安裝版自己的 Python 跑 pip，把 PyTorch 換成 AMD 官方的 ROCm 10 多架構版（只裝這張卡的架構）。
    /// 先 pip download 到 <資料>\data\_dl\rocm10\（斷掉再按一次會接著下載，已經下載完的檔不重抓），再從那裡離線安裝；
    /// 裝好把下載的檔搬到「待刪除」。2026-10-21 在這台實測 gfx1031：下載 1.1GB 約 76 秒、安裝約 77 秒
    async fn install_rocm10(&self, p: &Plan, python: &Path, devices: &[String]) -> Result<(), String> {
        let dl = p.dl().join("rocm10");
        std::fs::create_dir_all(&dl).map_err(|e| format!("建立 {} 失敗：{e}", dl.display()))?;
        let specs = gpu::rocm10_specs(devices);
        let pip = |cmd: &str, extra: &[&str]| -> Vec<String> {
            let mut a: Vec<String> = ["-s", "-m", "pip", cmd, "--disable-pip-version-check", "--no-input"].map(String::from).to_vec();
            a.extend(extra.iter().map(|x| x.to_string()));
            a.extend(specs.iter().cloned());
            a
        };
        let dls = dl.to_string_lossy().to_string();
        self.phase("下載支援這張 A 卡的 PyTorch（ROCm 10，約 1.1～1.5GB）");
        self.set_status("rocm10", "run", None);
        let args = pip("download", &["--no-cache-dir", "--progress-bar", "raw", "--index-url", gpu::ROCM10_INDEX, "-d", &dls]);
        if let Err(e) = self.pip_blocking(python, args, Some(self.prog_handle("rocm10")), true).await {
            self.set_status("rocm10", "error", Some(e.clone()));
            return Err(e);
        }
        self.set_status("rocm10", "done", None);
        // 換 PyTorch 時 ComfyUI 不能開著（Windows 不讓刪正在用的 DLL）：生圖站開的先關掉，裝完再開
        self.phase("換上新的 PyTorch（約 1～3 分鐘）");
        if self.cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        self.comfy_hook(false);
        self.set_status("rocm10.install", "run", None);
        // 這一步不能中途停：pip 先移掉舊的 torch 再解開新的，砍在中間 ComfyUI 就沒有 PyTorch 了（取消要等它裝完才生效）
        let args = pip("install", &["--no-index", "--no-warn-script-location", "--find-links", &dls]);
        let r = self.pip_blocking(python, args, None, false).await;
        let have = gpu::rocm10_devices(python);
        let r = r.and_then(|()| {
            devices.iter().all(|d| have.contains(d)).then_some(()).ok_or_else(|| "pip 說裝好了，但找不到這張卡的架構套件".to_string())
        });
        self.comfy_hook(true);
        if let Err(e) = r {
            self.set_status("rocm10.install", "error", Some(e.clone()));
            return Err(e);
        }
        self.set_status("rocm10.install", "done", None);
        self.say(format!("PyTorch 換成 ROCm 10 版了（架構：{}）", devices.join("、")));
        let files = files_under(&dl);
        self.stash(&files, &p.data_root);
        let _ = std::fs::remove_dir(&dl);
        let _ = std::fs::remove_dir(p.dl()); // 空了才會刪得掉
        Ok(())
    }

    /// run_pip 放到背景執行緒跑（cancellable＝使用者按取消會關掉 pip）
    async fn pip_blocking(&self, python: &Path, args: Vec<String>, prog: Option<ItemProgress>, cancellable: bool) -> Result<(), String> {
        let (py, stop) = (python.to_path_buf(), self.cancel.clone());
        tokio::task::spawn_blocking(move || run_pip(&py, &args, prog, || cancellable && stop.load(Ordering::SeqCst)))
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
    }

    /// 升級官方免安裝版到 COMFY_VERSION：先關掉生圖站開的 ComfyUI → resources\extras\comfy_upgrade.py（pygit2 換版本，
    /// models、input、output、user、custom_nodes 不動；自己改過、會被蓋掉的檔先備份）→ pip 補新版要的套件 → 再開 ComfyUI
    async fn upgrade_comfy(&self, p: &Plan) -> Result<(), String> {
        let l = comfy_layout(&p.comfy_dir).ok_or("找不到 ComfyUI")?;
        let code = comfy_code(&p.comfy_dir);
        let backup = upgrade_backup(&code);
        self.phase(&format!("把 ComfyUI 升級到 {COMFY_VERSION}（約 1～3 分鐘）"));
        self.set_status("comfy_upgrade", "run", None);
        // 這一步不能中途停：換到一半的 ComfyUI 開不起來（取消要等它做完才生效）
        self.comfy_hook(false);
        let (py, res, bak) = (l.python.clone(), self.res.clone(), backup.clone());
        let r = tokio::task::spawn_blocking(move || upgrade_comfy_code(&py, &code, &res, &bak)).await.unwrap_or_else(|e| Err(e.to_string()));
        self.comfy_hook(true);
        match r {
            Ok(()) => {
                self.set_status("comfy_upgrade", "done", None);
                self.say(format!("ComfyUI 升級到 {COMFY_VERSION} 了；換掉的舊檔（以前的 MOSS 補丁、自己改過的檔）在 {}", backup.display()));
                Ok(())
            }
            Err(e) => {
                self.set_status("comfy_upgrade", "error", Some(e.clone()));
                Err(e)
            }
        }
    }

    /// variant＝gpu::GpuPlan 的 variant："amd"、"intel" 下載各自的免安裝包（資料夾結構、啟動方式跟 NVIDIA 版一樣）
    async fn install_comfy(&self, p: &Plan, variant: &str, legacy: bool) -> Result<(), String> {
        if p.comfy_dir.exists()
            && (!p.comfy_dir.is_dir()
                || std::fs::read_dir(&p.comfy_dir)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(true))
        {
            return Err(format!(
                "{} 已經有東西但不是完整的 ComfyUI；請換一個位置，或先把它移走",
                p.comfy_dir.display()
            ));
        }
        let parent = p
            .comfy_dir
            .parent()
            .ok_or("ComfyUI 位置不對")?
            .to_path_buf();
        let dl = parent.join("_anima_dl"); // 跟 ComfyUI 放同一顆硬碟，解完直接改名、不用搬
        let (url, size) = comfy_package(variant, legacy);
        let archive = dl.join(basename(url));
        self.phase(match variant {
            "amd" => "下載 ComfyUI AMD 版（約 1.5GB）",
            "intel" => "下載 ComfyUI Intel 版（約 1.4GB）",
            "nvidia" if legacy => "下載 ComfyUI NVIDIA 舊卡版（約 1.9GB）",
            _ => "下載 ComfyUI（約 1.9GB）",
        });
        self.fetch(
            "comfy.7z",
            url,
            &archive,
            Some(size),
            &p.data_root,
        )
        .await
        .inspect_err(|e| self.set_status("comfy.7z", "error", Some(e.clone())))?;
        self.phase("解壓縮 ComfyUI（約 3～5 分鐘）");
        self.set_status("comfy.unpack", "run", None);
        let out = unpack_dir(&p.comfy_dir, &parent);
        let ownership = p.data_root.join("data").join("installer_cache.json");
        let temporary = ownership.with_extension("json.tmp");
        let recorded = std::fs::write(&temporary, settings::to_text(&json!({"comfy_dir":p.comfy_dir, "unpack":out})))
            .and_then(|_| std::fs::rename(&temporary, &ownership));
        if recorded.is_err() { self.say("無法記錄解壓暫存的位置，之後可能需要手動清理"); }
        let (a, o) = (archive.clone(), out.clone());
        let (me, stop) = (self.prog_handle("comfy.unpack"), self.cancel.clone());
        let r = tokio::task::spawn_blocking(move || {
            un7z(&a, &o, |n| me.set(n), || stop.load(Ordering::SeqCst))
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = r {
            self.set_status("comfy.unpack", "error", Some(e.clone()));
            return Err(format!("解壓縮失敗：{e}"));
        }
        // 壓縮檔裡是一個 ComfyUI_windows_portable 資料夾
        let inner = std::fs::read_dir(&out)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|d| comfy_layout_with(d, &PyOpt::default()).is_some())
            .ok_or("壓縮檔裡找不到 ComfyUI")?;
        if p.comfy_dir.exists() {
            let _ = std::fs::remove_dir(&p.comfy_dir); // 只會是空資料夾（上面檢查過）
        }
        maint::move_path(&inner, &p.comfy_dir)
            .map_err(|e| format!("搬到 {} 失敗：{e}", p.comfy_dir.display()))?;
        self.set_status("comfy.unpack", "done", None);
        self.stash(&[archive], &p.data_root);
        let _ = std::fs::remove_dir(&out);
        let _ = std::fs::remove_dir(&dl);
        Ok(())
    }

    /// 裝完的壓縮檔不刪：搬到 <資料>\待刪除\安裝用的壓縮檔_<時間>\
    fn stash(&self, files: &[PathBuf], data_root: &Path) {
        let dst = data_root.join("待刪除").join(format!(
            "安裝用的壓縮檔_{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S")
        ));
        for f in files {
            if let Some(n) = f.file_name() {
                if let Err(e) = maint::move_path(f, &dst.join(n)) {
                    self.say(format!("搬 {} 到待刪除失敗：{e}", f.display()));
                }
            }
        }
    }

    /// src 底下的每個檔案，dst 沒有的才複製（保留子資料夾）
    /// 程式附帶的檔放進 dst：沒有的補上、不一樣的換新（舊檔先搬到 bak），__pycache__ 不放
    fn sync_into(&self, key: &str, src: &Path, dst: &Path, bak: &Path) {
        let keep = |f: &Path| !f.components().any(|c| c.as_os_str() == "__pycache__") && f.extension().is_none_or(|x| x != "pyc");
        match overlay(src, dst, bak, true, keep) {
            Ok((n, backed)) => {
                self.set_status(key, if n > 0 { "done" } else { "skip" }, None);
                if !backed.is_empty() {
                    self.say(format!("換成新版的 {} 個檔，舊的搬到 {}", backed.len(), bak.display()));
                }
            }
            Err(e) => {
                self.set_status(key, "error", Some(e.clone()));
                self.problem(format!("放入 {} 失敗：{e}", dst.display()));
            }
        }
    }

    /// ComfyUI 0.38.0 還沒有 MOSS-Image：把程式附的官方 commit e087d8f 那 9 個檔蓋進 ComfyUI（extras\comfy_moss\說明.txt）。
    /// 已經有 MOSS（nodes.py 認得 moss_image，見 moss_support）就不動；桌面版、不是 0.38.0 也不動（那 6 個改過的檔是 0.38 的版本，
    /// 蓋到別版會壞），狀態標「要手動處理」並講解法（以前標 skip，進度頁顯示成「已經有了」，2026-10-30 有人因此一直以為裝好了）。
    /// 要蓋的檔跟補丁內容一樣就跳過；原本就有、而且不一樣的（官方原版，或使用者自己改過的）先搬到 ComfyUI 旁邊的
    /// 待刪除\ComfyUI核心檔備份_<時間>\ 再蓋，安裝結果裡會講。fresh＝這次剛下載的 ComfyUI（核心檔一定是官方原版），不備份
    fn patch_moss(&self, p: &Plan, fresh: bool) {
        let key = "comfy_moss";
        let src = self.res.join("extras").join("comfy_moss");
        let root = comfy_code(&p.comfy_dir);
        // ComfyUI 不見了（正常流程下載失敗會更早停下，走到這裡多半是資料夾被防毒搬走、網路磁碟斷線）：標失敗並列進要處理的事
        if comfy_layout_with(&p.comfy_dir, &p.opt).is_none() {
            let e = "找不到 ComfyUI，沒辦法放入 MOSS-Image 支援".to_string();
            self.set_status(key, "error", Some(e.clone()));
            self.problem(e);
            return;
        }
        let support = moss_support(&p.comfy_dir);
        if support == MossSupport::Ready {
            self.set_status(key, "skip", None);
            return;
        }
        if let Some(why) = support.problem() {
            self.set_status(key, "warn", Some(why.clone()));
            self.say(why);
            return;
        }
        let backup = root
            .parent()
            .unwrap_or(&root)
            .join("待刪除")
            .join(format!("ComfyUI核心檔備份_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")));
        match moss_apply(&src, &root, &backup, !fresh) {
            Ok((written, backed)) => {
                self.set_status(key, if written == 0 { "skip" } else { "done" }, None);
                if !backed.is_empty() {
                    self.say(format!(
                        "MOSS-Image 支援換掉了 ComfyUI 的 {} 個核心檔（{}）：原本的檔（官方原版，或你自己改過的）已搬到 {}；要還原就搬回去",
                        backed.len(),
                        backed.join("、"),
                        backup.display()
                    ));
                }
            }
            Err(e) => {
                self.set_status(key, "error", Some(e.clone()));
                self.problem(format!("放入 MOSS 支援失敗：{e}"));
            }
        }
    }

    fn import_lora_meta(&self, p: &Plan) {
        let src = self.res.join("share").join("lora設定.json");
        let r = (|| -> Result<usize, String> {
            let db =
                Db::open(&p.data_root.join("data").join("anima.db")).map_err(|e| e.to_string())?;
            if !db.lora_meta().map_err(|e| e.to_string())?.is_empty() {
                return Ok(0);
            }
            let v: Value = serde_json::from_slice(&std::fs::read(&src).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let mut n = 0;
            for (name, m) in v["lora_meta"].as_object().into_iter().flatten() {
                if m.is_object() {
                    db.set_lora_meta(name, m).map_err(|e| e.to_string())?;
                    n += 1;
                }
            }
            Ok(n)
        })();
        match r {
            Ok(0) => self.set_status("lora_meta", "skip", None),
            Ok(n) => {
                self.set_status("lora_meta", "done", None);
                self.say(format!("匯入 {n} 個 LoRA 的說明與觸發詞"));
            }
            Err(e) => {
                self.set_status("lora_meta", "error", Some(e.clone()));
                self.problem(format!("匯入 LoRA 說明失敗：{e}"));
            }
        }
    }
}

struct ItemProgress {
    prog: Arc<Mutex<Progress>>,
    key: String,
}

impl ItemProgress {
    fn set(&self, done: u64) {
        let mut p = self.prog.lock().unwrap_or_else(|e| e.into_inner());
        let mut added = 0;
        if let Some(it) = p.items.iter_mut().find(|i| i["key"] == self.key.as_str()) {
            if it.get("unit").is_none_or(|u| u != "%") {
                added = done.saturating_sub(it["done"].as_u64().unwrap_or(0)); // 解壓縮的百分比不是下載量
            }
            it.insert("done".into(), json!(done));
        }
        p.recv += added;
    }
}

enum FetchErr {
    Retry(String),
    Fatal(String),
    /// 連不上這個來源（還沒收到回應）：有別的來源就換，沒有就照 Retry 重試
    Down(String),
    /// 這個來源回應了但不給檔（要登入、404、大小不對）：有別的來源就換，沒有就照 Fatal
    NoFile(String),
    /// 來源的檔大小（第一個值）跟清單不一樣：上傳的人換了新版。fetch_core 照 adopt 決定改下載新版或當 NoFile
    Changed(u64, String),
}
use FetchErr::{Changed, Down, Fatal, NoFile, Retry};

/// 跑 pip（python -s -m pip …）：代理照 net.rs（跟 ComfyUI 一樣），輸出用 UTF-8。
/// prog＝pip download 的進度：--progress-bar raw 每個檔各自印「Progress 目前 of 總共」（從 0 算起），加起來就是整體進度。
/// stop()＝使用者按了取消：關掉 pip。失敗回傳白話原因（看輸出的最後幾行）
fn run_pip(python: &Path, args: &[String], prog: Option<ItemProgress>, stop: impl Fn() -> bool) -> Result<(), String> {
    use std::io::BufRead;
    let mut cmd = std::process::Command::new(python);
    cmd.args(args)
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .envs(crate::net::child_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    win::no_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("開不了 Python（{}）：{e}", python.display()))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let outs: Vec<Box<dyn std::io::Read + Send>> = [
        child.stdout.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    for r in outs {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for l in std::io::BufReader::new(r).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut tail: std::collections::VecDeque<String> = Default::default();
    let (mut done, mut cur, mut total) = (0u64, 0u64, 0u64);
    loop {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(l) => {
                let n = l.trim().strip_prefix("Progress ").and_then(|r| r.split_once(" of "));
                if let Some((Ok(a), Ok(b))) = n.map(|(a, b)| (a.trim().parse::<u64>(), b.trim().parse::<u64>())) {
                    if a < cur {
                        done += total; // 下一個檔從 0 開始
                    }
                    (cur, total) = (a, b);
                    if let Some(p) = &prog {
                        p.set(done + cur);
                    }
                    continue;
                }
                tail.push_back(l);
                if tail.len() > 40 {
                    tail.pop_front();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if stop() {
            win::kill(child.id() as i64);
            let _ = child.wait();
            return Err("已取消".into());
        }
    }
    let st = child.wait().map_err(|e| e.to_string())?;
    if st.success() {
        return Ok(());
    }
    Err(pip_error(&tail.into_iter().collect::<Vec<_>>().join("\n")))
}

/// pip 失敗的輸出 → 白話原因＋最後一行錯誤
fn pip_error(out: &str) -> String {
    let low = out.to_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| low.contains(k));
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    // 連不上時 pip 先重試幾次（WARNING: Retrying … connection broken by …），最後才印「No matching distribution」：
    // 網路要比「找不到套件」先判斷，最後的訊息也改給連線那行，不然會誤導成「AMD 沒有這個版本」
    let network = has(&[
        "connection broken", "connection aborted", "connection refused", "newconnectionerror", "connecttimeout", "readtimeout", "timed out",
        "proxyerror", "cannot connect to proxy", "sslerror", "ssl:", "max retries", "getaddrinfo", "name resolution", "10054", "10060", "10061",
        "remote end closed", "incompleteread",
    ]);
    let find = |p: fn(&str) -> bool| lines.iter().rev().find(|l| p(l)).copied();
    let last: String = network
        .then(|| find(|l| l.to_lowercase().contains("connection") || l.contains("Error")))
        .flatten()
        .or_else(|| find(|l| l.starts_with("ERROR:")))
        .or(lines.last().copied())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    let why = if has(&["no space left", "errno 28", "not enough space"]) {
        "硬碟空間不夠：清出空間再按「重試」"
    } else if has(&["winerror 5]", "winerror 32]", "access is denied", "permission denied", "being used by another process"]) {
        "檔案被占用：先關掉正在跑的 ComfyUI（工作管理員裡 ComfyUI 的 python.exe）再按「重試」"
    } else if network {
        "連不到 AMD 的套件庫（stable.repo.amd.com）：檢查網路或代理設定（設定 → 網路 可以測試連線）；已經下載完的檔留著，再按「重試」會接著下載"
    } else if has(&["do not match the hashes", "hash mismatch"]) {
        "下載的檔案壞掉了：再按「重試」重新下載"
    } else if has(&["no matching distribution", "could not find a version"]) {
        "AMD 的套件庫上找不到這張卡要的 PyTorch（可能 AMD 改了版本）：ComfyUI 先照原本的 PyTorch 用，或改用「雲端顯卡」"
    } else {
        "pip 執行失敗"
    };
    if last.is_empty() {
        why.to_string()
    } else {
        format!("{why}。最後的訊息：{last}")
    }
}

/// ComfyUI 壓縮檔的暫時解壓位置。Windows 路徑超過 260 字元會出錯，ComfyUI 裡的路徑本來就很深，
/// 所以暫存要比最後的位置短：放在同一顆硬碟的根目錄底下、取短名字（D:\_anima_u）；根目錄不能寫就放在 ComfyUI 旁邊（上一層\_anima_u）。
/// 兩種都在同一顆硬碟，解完搬到最後位置是改名，不用複製。
fn unpack_dir(comfy_dir: &Path, parent: &Path) -> PathBuf {
    unpack_dir_in(comfy_dir.ancestors().last().filter(|r| is_drive_root(r)), parent)
}

/// root＝磁碟根目錄（不是磁碟、例如網路路徑，就是 None）
fn unpack_dir_in(root: Option<&Path>, parent: &Path) -> PathBuf {
    let cands = root.map(|r| r.join(UNPACK_NAME)).into_iter().chain([parent.join(UNPACK_NAME)]);
    for c in cands {
        // 已經有東西、又不是我們上次留下的（裡面只會有 ComfyUI_windows_portable）：不去動使用者的資料夾
        let ours = match std::fs::read_dir(&c) {
            Ok(rd) => rd.flatten().all(|e| e.file_name() == "ComfyUI_windows_portable"),
            Err(_) => !c.exists(),
        };
        if ours && std::fs::create_dir_all(&c).is_ok() {
            return c;
        }
    }
    parent.join(UNPACK_NAME)
}

const UNPACK_NAME: &str = "_anima_u";

/// 像 D:\ 這樣的磁碟根目錄（網路路徑不算）
fn is_drive_root(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn basename(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// 解 zip 到 dir（zip 裡的路徑會擋掉 ..\）
fn unzip_all(zips: &[PathBuf], dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for z in zips {
        let f = std::fs::File::open(z).map_err(|e| format!("{}：{e}", z.display()))?;
        let mut a = zip::ZipArchive::new(std::io::BufReader::new(f))
            .map_err(|e| format!("{}：{e}", z.display()))?;
        a.extract(dir)
            .map_err(|e| format!("{}：{e}", z.display()))?;
    }
    Ok(())
}

/// 7-Zip 的解壓程式：程式附帶的 7zr.exe（官方免安裝版）→ 電腦裝的 7-Zip。
/// 不用 Rust 的 7z 套件：ComfyUI 的壓縮檔是 LZMA2（768MB 字典）＋BCJ2 單一區塊，sevenz-rust2 0.23 解到 4GB 會溢位當掉、吃 7.6GB 記憶體；
/// 7zr.exe 實測 79 秒、記憶體高峰 0.8GB（2026-10-10）
fn seven_zip() -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = vec![];
    if let Some(p) = std::env::var_os("ANIMA_7ZR").filter(|p| !p.is_empty()) {
        c.push(p.into());
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        c.push(dir.join("7zr.exe"));
    }
    if cfg!(debug_assertions) {
        c.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("vendor")
                .join("7zip")
                .join("7zr.exe"),
        );
    }
    for v in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(pf) = std::env::var_os(v) {
            c.push(PathBuf::from(pf).join("7-Zip").join("7z.exe"));
        }
    }
    c.into_iter().find(|p| p.is_file())
}

/// 解 7z 到 dir（先清掉上次解到一半的）；progress(百分比)；stop() 回傳 true 就中止
fn un7z(
    archive: &Path,
    dir: &Path,
    progress: impl Fn(u64),
    stop: impl Fn() -> bool,
) -> Result<(), String> {
    use std::io::Read;
    let exe = seven_zip().ok_or("找不到 7-Zip 的解壓程式（7zr.exe），請重新安裝 繪匣 PaintDeck")?;
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| format!("清掉上次解到一半的 {} 失敗：{e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("x")
        .arg(archive)
        .arg(format!("-o{}", dir.display()))
        // -sccUTF-8：錯誤訊息用 UTF-8（不然簡中、繁中 Windows 會印成 GBK／Big5，讀出來是空的）
        .args(["-y", "-bso0", "-bse2", "-bsp1", "-sccUTF-8"]);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    win::no_window(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("執行 {} 失敗：{e}", exe.display()))?;
    let mut err_pipe = child.stderr.take();
    let err_text = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(e) = err_pipe.as_mut() {
            let _ = e.read_to_end(&mut b);
        }
        String::from_utf8_lossy(&b).into_owned()
    });
    // 進度：-bsp1 會在標準輸出一直印「 37% 1234 - 檔名」，抓最後一個百分比
    let pct = regex::Regex::new(r"(\d{1,3})%").expect("regex");
    let mut out = child.stdout.take().ok_or("讀不到 7-Zip 的輸出")?;
    let mut buf = [0u8; 4096];
    loop {
        if stop() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("已取消".into());
        }
        match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let s = String::from_utf8_lossy(&buf[..n]);
                if let Some(m) = pct.captures_iter(&s).last() {
                    progress(m[1].parse().unwrap_or(0));
                }
            }
        }
    }
    let st = child.wait().map_err(|e| e.to_string())?;
    if !st.success() {
        let e = err_text.join().unwrap_or_default();
        return Err(format!(
            "7-Zip 解壓失敗（代碼 {}）：{}",
            st.code().unwrap_or(-1),
            e.trim().chars().take(300).collect::<String>()
        ));
    }
    progress(100);
    Ok(())
}

// ---------------------------------------------------------------- API
type S = State<Arc<Setup>>;

fn reply(v: Value) -> Response {
    Json(v).into_response()
}

fn bad(msg: impl Into<String>) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"error": msg.into()})),
    )
        .into_response()
}

fn plan_from(body: &Value) -> Result<Plan, String> {
    let get = |k: &str| -> Result<PathBuf, String> {
        let s = body[k].as_str().map(str::trim).unwrap_or("");
        let p = PathBuf::from(s);
        if s.is_empty() || !p.is_absolute() {
            let (what, eg) = if k == "data_root" {
                ("資料資料夾", "D:\\AnimaStudio\\anima_data")
            } else {
                ("ComfyUI 資料夾", "D:\\AnimaStudio\\ComfyUI_windows_portable")
            };
            return Err(format!("「{what}」要填完整的路徑（從磁碟代號開始，例如 {eg}）"));
        }
        Ok(p)
    };
    let mut opt = PyOpt::prefs();
    if let Some(py) = body["python"].as_str() {
        opt.python = py.trim().trim_matches('"').trim().to_string();
    }
    if let Some(b) = body["external"].as_bool() {
        opt.external = b;
    }
    Ok(Plan {
        data_root: reuse::normalize_data_root(&get("data_root")?, body["data_mode"].as_str()),
        comfy_dir: get("comfy_dir")?,
        opt,
    })
}

/// 同時只跑一次掃描：以前按「重試」會再開一次，前一次還在後台跑，兩三次一起搶同一批硬碟，越重試越慢
static SCAN_LOCK: Mutex<()> = Mutex::new(());

async fn api_scan(State(s): S) -> Response {
    let cur = s.config();
    reply(
        tokio::task::spawn_blocking(move || {
            let _one = SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            scan(cur.as_ref())
        })
            .await
            .unwrap_or(Value::Null),
    )
}

async fn api_check(State(s): S, Json(body): Json<Value>) -> Response {
    let p = match plan_from(&body) {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    // 顯示卡選擇照第二頁還沒存的值；python 另外處理（填錯路徑不能讓整個檢查失敗，check 會在「這個資料夾不能用」說明）
    let prefs = match gpu::prefs().merged(&json!({"gpu": body["gpu"], "amd_gfx": body["amd_gfx"]})) {
        Ok(v) => v,
        Err(e) => return bad(e),
    };
    let res = s.res.clone();
    reply(
        tokio::task::spawn_blocking(move || {
            let (gpus, ok) = detect_guarded(false).unwrap_or_default();
            let mut result = check(&res, &p, &gpu::plan_for(&prefs.gpu, &prefs.amd_gfx, &gpus, ok));
            result["data"]["selection_problem"] = json!(reuse::validate_choice(&p.data_root, body["data_mode"].as_str()).err());
            result
        })
        .await
        .unwrap_or(Value::Null),
    )
}

/// 刪掉以前影片包留下的 LTX-2.3 檔（精靈裡使用者按了「刪掉」）：{data_root, comfy_dir…}，伺服器自己重新找一次，不收網頁送來的路徑
async fn api_ltx_remove(State(s): S, Json(body): Json<Value>) -> Response {
    let p = match plan_from(&body) {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    if s.busy() {
        return bad("正在安裝，等裝完再刪");
    }
    let extra = crate::model_dirs::load(&p.data_root.join("data"));
    let files = ltx_leftovers(&p.models(), &extra);
    let (freed, fails) = tokio::task::spawn_blocking(move || remove_ltx(&files)).await.unwrap_or_default();
    reply(json!({"freed": freed, "fails": fails}))
}

/// 掃描這台電腦已經有的模型（model_dirs.rs）：開始
async fn api_model_scan(State(s): S) -> Response {
    let wants = manifest_wants(&s.res);
    if !crate::model_dirs::scan_start(wants, vec![]) {
        return bad("已經在掃描了");
    }
    reply(json!({"ok": true}))
}

/// 掃描進度與結果 {data_root, comfy_dir}（拿來標出哪些已經在用）
async fn api_model_scan_status(Json(body): Json<Value>) -> Response {
    let p = match plan_from(&body) {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    let dirs = crate::model_dirs::load(&p.data_root.join("data"));
    reply(crate::model_dirs::scan_view(&p.comfyui(), &dirs))
}

/// 跳出 Windows 的選資料夾視窗，選好回傳路徑、取消回傳 None（會卡住直到使用者選完，要在 spawn_blocking 裡叫）
#[cfg(windows)]
pub fn pick_folder(title: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut d = native_dialog(title);
    // 從輸入框現在的位置開始（還不存在的話，用最近一層存在的上層）
    if let Some(s) = start.and_then(|s| s.ancestors().find(|a| a.is_dir())) {
        d = d.set_directory(s);
    }
    d.pick_folder()
}
#[cfg(not(windows))]
pub fn pick_folder(_title: &str, _start: Option<&Path>) -> Option<PathBuf> {
    None
}

/// 跳出 Windows 的「另存新檔」視窗（存 JSON），選好回傳完整路徑、取消回傳 None（一樣會卡住，要在 spawn_blocking 裡叫）。
/// start：從這個資料夾開始（不存在就照 Windows 自己記的）。
/// 測試用：環境變數 ANIMA_FAKE_SAVE＝資料夾 → 不跳視窗，直接存到那裡；＝cancel → 當成按了取消
#[cfg(windows)]
pub fn save_json_dialog(title: &str, name: &str, start: Option<&Path>) -> Option<PathBuf> {
    if let Ok(f) = std::env::var("ANIMA_FAKE_SAVE") {
        return (f != "cancel").then(|| PathBuf::from(f).join(name));
    }
    let mut d = native_dialog(title).set_file_name(name).add_filter("JSON", &["json"]);
    if let Some(s) = start.filter(|s| s.is_dir()) {
        d = d.set_directory(s);
    }
    d.save_file()
}
#[cfg(not(windows))]
pub fn save_json_dialog(_title: &str, _name: &str, _start: Option<&Path>) -> Option<PathBuf> {
    None
}

/// Windows 原生的檔案對話框，掛在目前的前景視窗上（使用者剛在生圖站按了按鈕，前景就是生圖站），才不會跑到後面去
#[cfg(windows)]
fn native_dialog(title: &str) -> rfd::FileDialog {
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
    };
    struct Owner(std::num::NonZeroIsize);
    impl HasWindowHandle for Owner {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // SAFETY：前景視窗在對話框開著的期間都還在（它就是被擋住的那個視窗）
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(self.0))) })
        }
    }
    impl HasDisplayHandle for Owner {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Ok(DisplayHandle::windows())
        }
    }
    let mut d = rfd::FileDialog::new().set_title(title);
    let fg = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } as isize;
    if let Some(h) = std::num::NonZeroIsize::new(fg) {
        d = d.set_parent(&Owner(h));
    }
    d
}

/// 選資料夾 {title?, start?} → {path: 選的資料夾 | null（取消）}
async fn api_pick_folder(Json(body): Json<Value>) -> Response {
    let title = body["title"].as_str().unwrap_or("選資料夾").to_string();
    let start = body["start"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from);
    let picked = tokio::task::spawn_blocking(move || pick_folder(&title, start.as_deref())).await.ok().flatten();
    reply(json!({"path": picked.map(|p| p.to_string_lossy().to_string())}))
}

async fn api_model_scan_cancel() -> Response {
    crate::model_dirs::scan_cancel();
    reply(json!({"ok": true}))
}

// ---------------------------------------------------------------- 網路（代理、下載來源，net.rs）
/// 「測試連線」要試的網站：安裝會用到的每一種來源各挑一個（只抓第一個位元組）
fn net_targets() -> Vec<(&'static str, &'static str, String)> {
    let hf = format!("{TAGGER}selected_tags.csv");
    let node_mirror = crate::net::candidates(NODEJS.0).pop().unwrap_or_default();
    vec![
        ("hf", "Hugging Face（大部分模型）", hf.clone()),
        ("hf_mirror", "hf-mirror.com（Hugging Face 的鏡像）", hf.replacen(crate::net::HF, crate::net::HF_MIRROR, 1)),
        ("github", "GitHub（ComfyUI、llama.cpp、ffmpeg）", LLAMA[0].0.to_string()),
        ("civitai", "Civitai（常用 LoRA）", "https://civitai.com/api/v1/models?limit=1".to_string()),
        ("node", "nodejs.org（剪輯台的 Node.js）", NODEJS.0.to_string()),
        ("node_mirror", "npmmirror（Node.js 的鏡像）", node_mirror),
    ]
}

/// GET：目前的代理設定和偵測結果
async fn api_net() -> Response {
    reply(crate::net::status())
}

/// POST {proxy?: auto|off|manual, proxy_url?, hf?: auto|hf|mirror}：存到 config.json 旁邊的 net.json，立刻生效
async fn api_net_set(Json(body): Json<Value>) -> Response {
    match crate::net::cfg().merged(&body).and_then(crate::net::save) {
        Ok(()) => reply(crate::net::status()),
        Err(e) => bad(e),
    }
}

/// POST：每個下載來源連不連得到（同時測，最多約 15 秒）。Hugging Face 不通、鏡像通 → 之後的下載先用鏡像
async fn api_net_test(State(s): S) -> Response {
    let tests = net_targets().into_iter().map(|(id, label, url)| {
        let h = s.http.clone();
        async move {
            let mut v = crate::net::probe(&h, &url).await;
            v["id"] = json!(id);
            v["label"] = json!(label);
            (url, v)
        }
    });
    let res = futures_util::future::join_all(tests).await;
    let ok = |id: &str| res.iter().any(|(_, v)| v["id"] == id && v["ok"] == true);
    if !ok("hf") && ok("hf_mirror") {
        if let Some((u, _)) = res.iter().find(|(_, v)| v["id"] == "hf_mirror") {
            crate::net::used(u);
        }
    }
    reply(json!({"status": crate::net::status(), "results": res.into_iter().map(|(_, v)| v).collect::<Vec<_>>()}))
}

/// 把勾選的資料夾加進外部模型清單 {data_root, comfy_dir, dirs: [{kind, path}]}（原本的保留）
async fn api_model_dirs(Json(body): Json<Value>) -> Response {
    let p = match plan_from(&body) {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    let data = p.data_root.join("data");
    let mut dirs = crate::model_dirs::load(&data);
    for d in body["dirs"].as_array().into_iter().flatten() {
        let (Some(kind), Some(path)) = (d["kind"].as_str(), d["path"].as_str()) else {
            return bad("dirs 每一項要有 kind、path");
        };
        dirs.push(crate::model_dirs::Dir { kind: kind.into(), path: path.into(), source: "scan".into(), added: String::new() });
    }
    let comfyui = p.comfyui();
    match tokio::task::spawn_blocking(move || crate::model_dirs::save(&data, &comfyui, dirs)).await {
        Ok(Ok(v)) => reply(json!({"ok": true, "dirs": v})),
        Ok(Err(e)) => bad(e),
        Err(e) => bad(e.to_string()),
    }
}

async fn api_run(State(s): S, Json(body): Json<Value>) -> Response {
    // {only: "tools"}：只補影音工作台的工具，位置用已經記住的設定
    if body["only"].as_str() == Some("tools") {
        let Some(c) = s.config().filter(|c| c.problem().is_none()) else {
            return bad("還沒完成安裝：先跑一次安裝精靈");
        };
        if !s.begin() {
            return bad("已經在安裝了");
        }
        let p = Plan {
            data_root: c.data_root,
            comfy_dir: c.comfy_dir,
            opt: PyOpt::prefs(),
        };
        tokio::spawn(s.clone().run(p, None));
        return reply(json!({"ok": true}));
    }
    let p = match plan_from(&body) {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    let packs = match packs_from(&body) {
        Ok(v) => v,
        Err(e) => return bad(e),
    };
    let validate_root = p.data_root.clone();
    let mode = body["data_mode"].as_str().map(str::to_string);
    if let Err(e) = tokio::task::spawn_blocking(move || reuse::validate_choice(&validate_root, mode.as_deref()))
        .await.unwrap_or_else(|_| Err("無法確認資料位置，請重新檢查".into())) { return bad(e); }
    if let Err(e) = install_key(&body) { return bad(e); }
    if !s.begin() { return bad("已經在安裝了"); }
    let lock = s.settings_lock.get().unwrap_or(&s.local_settings_lock).clone();
    let _settings = lock.lock().await;
    let (comfy_dir, data_root) = (p.comfy_dir.clone(), p.data_root.clone());
    let chosen = tokio::task::spawn_blocking(move || {
        save_gpu_choice(&body, &comfy_dir)?;
        save_install_key(&data_root, &body)
    }).await.unwrap_or_else(|_| Err("準備安裝設定失敗".into()));
    if let Err(e) = chosen {
        s.with(|p| p.running = false);
        return bad(e);
    }
    tokio::spawn(s.clone().run(p, Some(packs)));
    reply(json!({"ok": true}))
}

/// 第二頁的顯示卡選擇 {gpu?, amd_gfx?} 存到 comfy.json（重跑精靈、安裝與修復都照它）；
/// 裝的是 CPU 版（沒有能用的顯示卡）就記下「啟動時加 --cpu」。還沒有 ComfyUI、又不知道該下載哪一版的，先擋下來
fn install_key(body: &Value) -> Result<Option<&str>, String> {
    let Some(value) = body.get("civitai_key") else { return Ok(None); };
    let key = value.as_str().ok_or("Civitai 金鑰必須是文字")?.trim();
    if key.len() > 4096 || key.chars().any(char::is_control) { return Err("Civitai 金鑰格式不正確".into()); }
    Ok((!key.is_empty()).then_some(key))
}

fn save_install_key(root: &Path, body: &Value) -> Result<(), String> {
    let Some(key) = install_key(body)? else { return Ok(()); };
    let data = root.join("data");
    std::fs::create_dir_all(&data).map_err(|_| "無法建立資料資料夾，金鑰尚未儲存")?;
    let file = data.join("settings.json");
    let mut current = settings::load(&file);
    current.insert(settings::KEY.into(), json!(key));
    settings::save(&file, &current).map_err(|_| "金鑰儲存失敗，請確認資料位置可以寫入".into())
}

fn save_gpu_choice(body: &Value, comfy_dir: &Path) -> Result<(), String> {
    let cur = gpu::prefs();
    // python、external：精靈第二頁「自己指定」才會送；沒送就維持原本的（沒送 python 代表這次不是自己指定的，網頁會送空字串清掉）
    let mut p = cur.merged(&json!({"gpu": body["gpu"], "amd_gfx": body["amd_gfx"], "python": body["python"], "external": body["external"]}))?;
    let (gpus, ok) = detect_guarded(false).unwrap_or_default();
    let g = gpu::plan_for(&p.gpu, &p.amd_gfx, &gpus, ok);
    let opt = PyOpt { python: p.python.clone(), external: p.external };
    let layout = comfy_layout_with(comfy_dir, &opt);
    if layout.is_none() {
        if g.own {
            return Err("選了「用我自己已經能跑的 ComfyUI」：請在上面選 ComfyUI 所在的資料夾".into());
        }
        if g.variant == "unknown" {
            return Err("偵測不到這台電腦的顯示卡：請在「ComfyUI 版本」選一個（NVIDIA、AMD、Intel 或用 CPU 跑），或選「用我自己已經能跑的 ComfyUI」".into());
        }
    }
    let backend = layout.as_ref().and_then(gpu::comfy_backend);
    p.cpu = cpu_after(cur.cpu, &g, backend, layout.is_none());
    if p != cur {
        gpu::save_prefs(&p)?;
    }
    Ok(())
}

/// 按「開始安裝」後，comfy.json 的「用 CPU 跑」（啟動時加 --cpu）該是什麼。cur＝現在的值（可能是設定頁自己勾的），
/// backend＝要沿用的 ComfyUI 是哪一種（gpu::comfy_backend；還沒有 ComfyUI＝None），installing＝這次要下載 ComfyUI。
/// - 第二頁自己選「用 CPU 跑」：一定加。
/// - 這次要下載：照要下載的版本（CPU 版才加）。
/// - 沿用現成的、自動判斷說沒有能用的顯示卡：CUDA／CPU 版就加；老手的 ZLUDA／DirectML 版（還有 AMD、Intel 版）能用
///   RX 580、Vega 這些卡，不加（以前留下來的也清掉）。
/// - 其他（有能用的顯示卡、偵測失敗）：照設定頁原本的，不偷改
fn cpu_after(cur: bool, g: &gpu::GpuPlan, backend: Option<&str>, installing: bool) -> bool {
    let gpu_build = backend.is_some_and(|b| matches!(b, "zluda" | "directml" | "rocm" | "xpu"));
    if g.variant == "unknown" {
        cur
    } else if g.why == "manual" && g.variant == "cpu" {
        true
    } else if installing {
        g.variant == "cpu"
    } else if g.variant == "cpu" {
        !gpu_build
    } else {
        cur
    }
}

/// GET：ComfyUI 啟動方式（comfy.json）和現在這份 ComfyUI 認出來的種類（設定頁「ComfyUI 啟動方式」用）
async fn api_comfy_opts(State(s): S) -> Response {
    let cfg = s.config();
    reply(tokio::task::spawn_blocking(move || comfy_opts_view(cfg.as_ref())).await.unwrap_or(Value::Null))
}

/// POST {extra_args?, external?, cpu?, gpu?, amd_gfx?}：存到 comfy.json，下次開 ComfyUI 生效
async fn api_comfy_opts_set(State(s): S, Json(body): Json<Value>) -> Response {
    // 沒有改到東西就不寫檔（N 卡、沒動過設定的電腦不會多出 comfy.json）
    let cur = gpu::prefs();
    let r = cur.merged(&body).and_then(|p| if p == cur { Ok(()) } else { gpu::save_prefs(&p) });
    if let Err(e) = r {
        return bad(e);
    }
    let cfg = s.config();
    reply(tokio::task::spawn_blocking(move || comfy_opts_view(cfg.as_ref())).await.unwrap_or(Value::Null))
}

fn comfy_opts_view(cfg: Option<&Config>) -> Value {
    let layout = cfg.and_then(|c| comfy_layout(&c.comfy_dir));
    let version = cfg.and_then(|c| comfy_version(&c.comfy_dir));
    json!({
        "prefs": gpu::prefs(),
        "version": version, "old": comfy_too_old(version.as_deref()), "min_version": COMFY_MIN_VERSION,
        "python": layout.as_ref().map(|l| l.python.to_string_lossy().to_string()).filter(|s| !s.is_empty()),
        "backend": layout.as_ref().and_then(gpu::comfy_backend),
        "torch": layout.as_ref().and_then(|l| gpu::torch_build(&l.python)),
        "zluda_exe": layout.as_ref().and_then(gpu::zluda_exe).map(|p| p.to_string_lossy().to_string()),
        "comfy_dir": cfg.map(|c| c.comfy_dir.to_string_lossy().to_string()),
    })
}

async fn api_status(State(s): S) -> Response {
    reply(s.status())
}

async fn api_cancel(State(s): S) -> Response {
    s.cancel.store(true, Ordering::SeqCst);
    reply(json!({"ok": true}))
}

async fn api_finish(State(s): S) -> Response {
    let Some(cfg) = s.config() else {
        return bad("還沒完成安裝（資料位置還沒記住）");
    };
    if let Some(p) = cfg.problem() {
        return bad(p);
    }
    let action = s
        .on_finish
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|f| f(&cfg))
        .unwrap_or("reload");
    reply(json!({"ok": true, "action": action}))
}

/// 精靈的 API 和頁面（/setup）。桌面版第一次啟動時另外掛 "/"、"/static"（見 main.rs）
pub fn router(setup: Arc<Setup>, web: &Path) -> Router {
    let page = web.join("setup.html");
    Router::new()
        .route("/setup", get(move || setup_page(page.clone())))
        .route("/api/setup/scan", get(api_scan))
        .route("/api/setup/check", post(api_check))
        .route("/api/setup/run", post(api_run))
        .route("/api/setup/status", get(api_status))
        .route("/api/setup/cancel", post(api_cancel))
        .route("/api/setup/finish", post(api_finish))
        .route("/api/setup/model_scan", post(api_model_scan))
        .route("/api/setup/model_scan/status", post(api_model_scan_status))
        .route("/api/setup/model_scan/cancel", post(api_model_scan_cancel))
        .route("/api/setup/model_dirs", post(api_model_dirs))
        .route("/api/setup/ltx_remove", post(api_ltx_remove))
        .route("/api/setup/pick_folder", post(api_pick_folder))
        .route("/api/setup/net", get(api_net).post(api_net_set))
        .route("/api/setup/net/test", post(api_net_test))
        .route("/api/setup/comfy_opts", get(api_comfy_opts).post(api_comfy_opts_set))
        .route_layer(axum::middleware::from_fn_with_state(setup.clone(), only_own_page))
        .with_state(setup)
}

/// 精靈的 API 只給自己的網頁用：Host 要是本機（擋 DNS rebinding），POST 要帶 X-Anima-Setup（別的網站的表單送不出自訂標頭）
async fn only_own_page(State(s): S, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let local = crate::server::local_request(&req);
    let marked =
        req.method() != axum::http::Method::POST || req.headers().contains_key("x-anima-setup");
    if !local || !marked {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({"error": "只能從本機的安裝精靈頁面操作"})),
        )
            .into_response();
    }
    let control = s.activity.get();
    let _activity = if req.method() == axum::http::Method::POST {
        if let Some(c) = control {
            if c.cleaning.load(Ordering::SeqCst) { return bad("正在清理快取，請稍後再操作"); }
            let guard = c.gate.read().await;
            if c.cleaning.load(Ordering::SeqCst) { return bad("正在清理快取，請稍後再操作"); }
            Some(guard)
        } else { None }
    } else { None };
    next.run(req).await
}

pub async fn setup_page(page: PathBuf) -> Response {
    match tokio::fs::read_to_string(&page).await {
        Ok(s) => (
            [
                (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (axum::http::header::CACHE_CONTROL, "no-store"),
            ],
            crate::server::versioned_html(&s),
        )
            .into_response(),
        Err(_) => (axum::http::StatusCode::NOT_FOUND, "找不到 setup.html").into_response(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn install_key_is_encrypted_blank_preserves_and_invalid_input_never_echoes() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除")
            .join(format!("setup-key-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let file = root.join("data/settings.json");
        super::save_install_key(&root, &serde_json::json!({"civitai_key":"  TEST-ONLY-INSTALL-KEY  "})).unwrap();
        let mut current = crate::settings::load(&file);
        assert_eq!(crate::settings::civitai_key(&current), "TEST-ONLY-INSTALL-KEY");
        current.insert("negative".into(), serde_json::json!("keep my setting"));
        crate::settings::save(&file, &current).unwrap();
        let before = std::fs::read(&file).unwrap();
        super::save_install_key(&root, &serde_json::json!({"civitai_key":"  "})).unwrap();
        super::save_install_key(&root, &serde_json::json!({})).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), before);
        #[cfg(windows)] assert!(!String::from_utf8_lossy(&before).contains("TEST-ONLY-INSTALL-KEY"));
        let view = crate::settings::public_view(&crate::settings::load(&file));
        assert_eq!(view["has_civitai_key"], true);
        assert!(!view.to_string().contains("TEST-ONLY-INSTALL-KEY"));
        assert_eq!(view["negative"], "keep my setting");
        assert!(super::install_key(&serde_json::json!({"civitai_key":12})).is_err());
        let error = super::install_key(&serde_json::json!({"civitai_key":"TEST-ONLY\nKEY"})).unwrap_err();
        assert!(!error.contains("TEST-ONLY"));
    }
    use super::*;

    #[test]
    fn skip_backup_and_system_dirs() {
        for n in [
            "待刪除",
            "anima_ui_snapshots",
            "$Recycle.Bin",
            "Windows",
            "Program Files",
            "Anima生圖站_備份_20261009",
        ] {
            assert!(skip_dir(n), "{n}");
        }
        for n in ["AI", "anima_ui", "ComfyUI_windows_portable", "Games"] {
            assert!(!skip_dir(n), "{n}");
        }
    }

    /// 免安裝版、自己 clone 的（venv）、選到免安裝版裡面那層、只有 main.py 沒有 Python、隨便一個有 main.py 的專案
    #[test]
    fn comfy_layouts() {
        let d = std::env::temp_dir().join(format!("anima_layout_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let touch = |p: PathBuf| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        };
        // 官方免安裝版
        let port = d.join("ComfyUI_windows_portable");
        touch(port.join("python_embeded").join("python.exe"));
        touch(port.join("ComfyUI").join("main.py"));
        touch(port.join("ComfyUI").join("nodes.py"));
        let l = comfy_layout(&port).unwrap();
        assert!(l.portable);
        assert_eq!(l.root, port.join("ComfyUI"));
        assert!(comfy_install(&port));
        // 選到裡面那層也認得
        let inner = comfy_layout(&port.join("ComfyUI")).unwrap();
        assert!(inner.portable && inner.root == port.join("ComfyUI"));
        // 自己 clone 的：D:\ComfyUI\main.py＋venv
        let git = d.join("ComfyUI");
        touch(git.join("main.py"));
        touch(git.join("nodes.py"));
        assert!(comfy_layout(&git).is_none());
        assert!(comfy_problem(&git).unwrap().contains("Python"));
        touch(git.join("venv").join("Scripts").join("python.exe"));
        let l = comfy_layout(&git).unwrap();
        assert!(!l.portable);
        assert_eq!(l.root, git);
        assert_eq!(l.python, git.join("venv").join("Scripts").join("python.exe"));
        let cfg = Config { data_root: d.join("data"), comfy_dir: git.clone() };
        assert_eq!(cfg.paths(&d).comfy_root, git);
        assert!(comfy_install(&git));
        // 上一層（d 裡面有個 ComfyUI）可以手動選，但掃硬碟時不算一份安裝（要繼續往下找）
        assert!(comfy_layout(&d).is_some());
        assert!(!comfy_install(&d));
        // 有 main.py、venv 但沒有 nodes.py：不是 ComfyUI
        let other = d.join("some_project");
        touch(other.join("main.py"));
        touch(other.join(".venv").join("Scripts").join("python.exe"));
        assert!(comfy_layout(&other).is_none());
        assert!(comfy_problem(&other).unwrap().contains("main.py"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-31 使用者：「不管更新了什麼，能上的就直接上」。開站時把程式附帶、已經裝進 ComfyUI 的東西換成新版：
    /// AI 改寫節點、工作流程、附帶 LoRA、MOSS 補丁；不一樣的舊檔先搬到 待刪除\繪匣更新備份_<時間>\，沒裝過的不主動裝
    #[test]
    fn sync_bundled_updates_installed_parts() {
        let d = std::env::temp_dir().join(format!("anima_sync_bundled_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let write = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        let port = d.join("ComfyUI_windows_portable");
        let code = port.join("ComfyUI");
        write(port.join("python_embeded").join("python.exe"), "");
        write(code.join("main.py"), "");
        write(code.join("comfyui_version.py"), "__version__ = \"0.38.0\"\n");
        write(code.join("nodes.py"), r#""type": (["stable_diffusion", "moss_image"], ),"#);
        write(code.join("comfy_extras").join("nodes_moss_image.py"), "# old patch\n");
        let node = code.join("custom_nodes").join(NODE);
        write(node.join("pe_core.py"), "# old node\n");
        write(node.join("bin").join("llama-server.exe"), "exe");
        let wf = code.join("user").join("default").join("workflows");
        write(wf.join("a.json"), "{\"old\":1}");
        let res = d.join("res").join("extras");
        write(res.join(NODE).join("pe_core.py"), "# new node\n");
        write(res.join(NODE).join("__init__.py"), "# init\n");
        write(res.join(NODE).join("__pycache__").join("pe_core.cpython-313.pyc"), "x");
        write(res.join("workflows").join("a.json"), "{\"new\":1}");
        write(res.join("workflows").join("b.json"), "{\"b\":1}");
        write(res.join("loras").join("l.safetensors"), "lora");
        write(res.join(MOSS_NODES).join("__init__.py"), "# moss nodes
");
        write(res.join("comfy_moss").join("comfy_extras").join("nodes_moss_image.py"), "class TextEncodeMOSSImageEdit(io.ComfyNode):");
        write(res.join("comfy_moss").join("說明.txt"), "readme");
        let done = sync_bundled(&port, &d.join("res"));
        assert_eq!(done.len(), 5, "{done:?}");
        // MOSS 小節點：沒裝過也放（不像 AI 改寫節點要先裝過）
        assert_eq!(read(code.join("custom_nodes").join(MOSS_NODES).join("__init__.py")), "# moss nodes
");
        assert_eq!(read(node.join("pe_core.py")), "# new node\n");
        assert_eq!(read(node.join("__init__.py")), "# init\n");
        assert_eq!(read(node.join("bin").join("llama-server.exe")), "exe");
        assert!(!node.join("__pycache__").exists());
        assert_eq!(read(wf.join("a.json")), "{\"new\":1}");
        assert_eq!(read(wf.join("b.json")), "{\"b\":1}");
        assert_eq!(read(code.join("models").join("loras").join("l.safetensors")), "lora");
        assert!(read(code.join("comfy_extras").join("nodes_moss_image.py")).contains("TextEncodeMOSSImageEdit"));
        assert!(!code.join("說明.txt").exists());
        assert_eq!(moss_support(&port), MossSupport::Ready);
        // 舊檔有備份
        let bak: Vec<PathBuf> = files_under(&port.join("待刪除"));
        let has = |s: &str| bak.iter().any(|p| p.to_string_lossy().replace('\\', "/").ends_with(s));
        assert!(has("custom_nodes/anima_prompt_enhancer/pe_core.py") && has("user/default/workflows/a.json") && has("comfy_extras/nodes_moss_image.py"), "{bak:?}");
        // 再開一次站：都一樣了，什麼都不做
        assert!(sync_bundled(&port, &d.join("res")).is_empty());
        // 沒裝過 AI 改寫節點：不主動裝
        std::fs::remove_dir_all(&node).unwrap();
        assert!(sync_bundled(&port, &d.join("res")).is_empty());
        assert!(!node.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-31 升級到 0.43：只有官方免安裝版（python_embeded＋git）、版本比 COMFY_VERSION 舊的才由精靈升級
    #[test]
    fn comfy_upgradable_only_old_portable_git() {
        let d = std::env::temp_dir().join(format!("anima_comfy_upgradable_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let write = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        let port = d.join("ComfyUI_windows_portable");
        let code = port.join("ComfyUI");
        write(port.join("python_embeded").join("python.exe"), "");
        write(code.join("main.py"), "");
        write(code.join("comfyui_version.py"), "__version__ = \"0.38.0\"\n");
        assert!(!comfy_upgradable(&port), "沒有 .git 不能升級");
        std::fs::create_dir_all(code.join(".git")).unwrap();
        assert!(comfy_upgradable(&port));
        write(code.join("comfyui_version.py"), &format!("__version__ = \"{COMFY_VERSION}\"\n"));
        assert!(!comfy_upgradable(&port));
        write(code.join("comfyui_version.py"), "__version__ = \"0.44.1\"\n");
        assert!(!comfy_upgradable(&port), "比較新的不降版");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-31 使用者：「如果檢測到同樣的個別lora則不重覆安裝那個各別的lora」。清單裡的 LoRA 在別的資料夾、別的檔名已經有
    /// 一模一樣的（大小＋SHA256 都對）就不下載，記在模型資料夾的 anima_model_aliases.json，之後檢查也算有
    #[test]
    fn same_lora_elsewhere_counts_as_installed() {
        let d = std::env::temp_dir().join(format!("anima_lora_alias_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let models = d.join("ComfyUI").join("models");
        let write = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        write(models.join("loras").join("civitai").join("my_style.safetensors"), "lora-a");
        write(models.join("loras").join("decoy.safetensors"), "lora-b"); // 一樣大、內容不同
        let sha = sha256_file(&models.join("loras").join("civitai").join("my_style.safetensors")).unwrap();
        let rel = "loras/moss/Excellent-Loras/风格/style.safetensors";
        let res = d.join("res");
        write(res.join("share").join("models_full.json"), &json!([{"path": rel, "size": 6, "sha256": sha, "url": "https://example.invalid/x"}]).to_string());
        assert_eq!(missing_models(&res, &models, "full", &[]).len(), 1);
        let index = crate::model_dirs::size_index(&models, &[], "loras");
        let mut cache = Default::default();
        let m = manifest(&res, "full").remove(0);
        let found = same_lora_elsewhere(&index, &mut cache, &m).expect("要找到改過名字的同一顆");
        assert!(found.ends_with("my_style.safetensors"), "{found:?}");
        remember_alias(&models, rel, &found).unwrap();
        assert!(missing_models(&res, &models, "full", &[]).is_empty());
        // 那顆被刪掉：又算缺
        std::fs::remove_file(&found).unwrap();
        assert_eq!(missing_models(&res, &models, "full", &[]).len(), 1);
        // 沒有 SHA256 的不猜
        let mut m2 = m.clone();
        m2.sha256 = None;
        assert!(same_lora_elsewhere(&index, &mut cache, &m2).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-31 使用者：「要確保他們升級後都能很直接地簡單的，能享受到跟我一樣」。開站自動把 0.38 升到 COMFY_VERSION：
    /// 先裝新版套件、成功才換程式。要真的 ComfyUI 程式複本（git）和它的 python，平常略過：
    /// ANIMA_UPG_CODE=<0.38 的 ComfyUI 程式複本> ANIMA_UPG_PY=<python_embeded\python.exe> cargo test upgrade_comfy_code_real -- --ignored
    #[test]
    #[ignore]
    fn upgrade_comfy_code_real() {
        let code = PathBuf::from(std::env::var("ANIMA_UPG_CODE").expect("ANIMA_UPG_CODE"));
        let py = PathBuf::from(std::env::var("ANIMA_UPG_PY").expect("ANIMA_UPG_PY"));
        let ver = |c: &Path| std::fs::read_to_string(c.join("comfyui_version.py")).unwrap_or_default();
        assert!(ver(&code).contains("0.38"), "要拿 0.38 的複本測");
        let backup = code.parent().unwrap().join("upg_backup");
        upgrade_comfy_code(&py, &code, &crate::paths::default_res(), &backup).unwrap();
        assert!(ver(&code).contains(COMFY_VERSION), "{}", ver(&code));
        assert!(std::fs::read_to_string(code.join("comfy_extras").join("nodes_moss_image.py")).unwrap().contains("MOSSImageControl"));
    }

    /// 生成頁缺 LoRA 一鍵下載（fetch_listed）也一樣：別的資料夾有同一顆就不下載，回那顆的位置，記成別名
    #[test]
    fn lora_elsewhere_for_fetch_listed() {
        let d = std::env::temp_dir().join(format!("anima_lora_elsewhere_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let models = d.join("models");
        let f = models.join("loras").join("my").join("deband_copy.safetensors");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "deband").unwrap();
        let sha = sha256_file(&f).unwrap();
        let res = d.join("res");
        std::fs::create_dir_all(res.join("share")).unwrap();
        let rel = "loras/moss/Excellent-Loras/SFT伪影修复/fix.safetensors";
        std::fs::write(res.join("share").join("models_full.json"), json!([{"path": rel, "size": 6, "sha256": sha, "url": "https://example.invalid/x"},
            {"path": "text_encoders/t.safetensors", "size": 6, "sha256": sha, "url": "https://example.invalid/y"}]).to_string()).unwrap();
        assert_eq!(lora_elsewhere(&res, &models, &[], rel), Some(f.clone()));
        assert_eq!(aliases(&models).get(rel), Some(&f));
        // 只管 LoRA；清單沒有的不管
        assert!(lora_elsewhere(&res, &models, &[], "text_encoders/t.safetensors").is_none());
        assert!(lora_elsewhere(&res, &models, &[], "loras/none.safetensors").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-31 使用者：「把每個LoRa都分別寫個備註…讓助手和各別的user一看就知道那個lora是幹啥用的」。
    /// 開站時把程式附的 LoRA 備註併進資料庫：沒有的補上（使用者改過的不蓋）；同一顆放在別的資料夾，照檔名對上實際的名稱也補一份
    #[test]
    fn merge_lora_notes_fills_missing_only() {
        let d = std::env::temp_dir().join(format!("anima_lora_notes_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let write = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        let res = d.join("res");
        write(res.join("share").join("lora設定.json"), &json!({"lora_meta": {
            "moss\\Excellent-Loras\\风格\\ink.safetensors": {"note": "【畫風】水墨", "triggers": ["ink"]},
            "mine.safetensors": {"note": "內建的說明"},
        }}).to_string());
        let loras = d.join("loras");
        write(loras.join("Excellent-Loras-for-MossImage").join("风格").join("ink.safetensors"), "x"); // 照 README 放在別的資料夾
        let db = Db::open(&d.join("anima.db")).unwrap();
        db.set_lora_meta("mine.safetensors", &json!({"note": "我自己改的"})).unwrap();
        let n = merge_lora_notes(&db, &res, std::slice::from_ref(&loras));
        let meta = db.lora_meta().unwrap();
        assert_eq!(n, 2, "{meta:?}");
        assert_eq!(meta["mine.safetensors"]["note"], "我自己改的");
        assert_eq!(meta["moss\\Excellent-Loras\\风格\\ink.safetensors"]["note"], "【畫風】水墨");
        assert_eq!(meta["Excellent-Loras-for-MossImage\\风格\\ink.safetensors"]["triggers"][0], "ink");
        assert_eq!(merge_lora_notes(&db, &res, std::slice::from_ref(&loras)), 0);
        // 程式附的備註改版了：使用者沒動過的跟著更新（AI 認識過加的欄位留著），使用者改過的不蓋
        let ink = "moss\\Excellent-Loras\\风格\\ink.safetensors";
        let mut m = db.lora_meta().unwrap()[ink].clone();
        m["agent"] = json!({"desc": "AI 寫的"});
        db.set_lora_meta(ink, &m).unwrap();
        write(res.join("share").join("lora設定.json"), &json!({"lora_meta": {
            ink: {"note": "【畫風】水墨（實測過）", "triggers": ["ink"]},
            "mine.safetensors": {"note": "內建的說明 v2"},
        }}).to_string());
        assert_eq!(merge_lora_notes(&db, &res, std::slice::from_ref(&loras)), 2, "ink 兩個名字都更新");
        let meta = db.lora_meta().unwrap();
        assert_eq!(meta[ink]["note"], "【畫風】水墨（實測過）");
        assert_eq!(meta[ink]["agent"]["desc"], "AI 寫的");
        assert_eq!(meta["Excellent-Loras-for-MossImage\\风格\\ink.safetensors"]["note"], "【畫風】水墨（實測過）");
        assert_eq!(meta["mine.safetensors"]["note"], "我自己改的");
        let mut m = meta[ink].clone();
        m["note"] = json!("使用者改的");
        db.set_lora_meta(ink, &m).unwrap();
        write(res.join("share").join("lora設定.json"), &json!({"lora_meta": {ink: {"note": "v3"}}}).to_string());
        merge_lora_notes(&db, &res, std::slice::from_ref(&loras));
        assert_eq!(db.lora_meta().unwrap()[ink]["note"], "使用者改的");
        // 26.1031.2 補進去的還沒有 bundle_sig：備註還是當時附的原文（LEGACY_NOTES）就當沒改過，照樣更新
        let old = "moss\\Excellent-Loras\\EDIT\\x.safetensors";
        db.set_lora_meta(old, &json!({"note": LEGACY_NOTES[0], "title": "舊"})).unwrap();
        write(res.join("share").join("lora設定.json"), &json!({"lora_meta": {old: {"note": "實測過的新備註", "title": "新"}}}).to_string());
        assert_eq!(merge_lora_notes(&db, &res, &[]), 1);
        assert_eq!(db.lora_meta().unwrap()[old]["note"], "實測過的新備註");
        drop(db);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 生成頁缺 MOSS 加速／編輯 LoRA 時一鍵下載：只認安裝清單裡的檔（不收網頁給的網址）
    #[test]
    fn listed_file_only_knows_the_manifest() {
        let res = crate::paths::default_res();
        let (size, url, sha) = listed_file(&res, "loras/moss/moss_speed_dmd_lodestones.safetensors").unwrap();
        assert_eq!(size, 1189176800);
        assert!(url.starts_with("https://huggingface.co/") && sha.is_some());
        assert!(listed_file(&res, "loras/moss/moss_edit_chinoll.safetensors").is_some());
        assert!(listed_file(&res, "../evil.safetensors").is_none());
        assert!(listed_file(&res, "loras/nope.safetensors").is_none());
    }

    /// 2026-10-30 有人回報 MOSS 報「'moss_image' not in (list of length 29)」，精靈卻說已經有了：補丁蓋過之後 ComfyUI 更新
    /// 把 nodes.py 換回官方版，沒被追蹤的 nodes_moss_image.py 還留著，以前看到它就當成有 MOSS。要看 nodes.py 的 CLIPLoader 有沒有 moss_image
    #[test]
    fn moss_support_reads_nodes_py() {
        let d = std::env::temp_dir().join(format!("anima_moss_support_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let write = |p: PathBuf, s: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        let port = d.join("ComfyUI_windows_portable");
        let code = port.join("ComfyUI");
        write(port.join("python_embeded").join("python.exe"), "");
        write(code.join("main.py"), "");
        write(code.join("comfyui_version.py"), "__version__ = \"0.38.0\"\n");
        let official = r#""type": (["stable_diffusion", "flux2", "yue2"], ),"#;
        write(code.join("nodes.py"), official);
        write(code.join("comfy_extras").join("nodes_moss_image.py"), "");
        assert_eq!(moss_support(&port), MossSupport::Patchable);
        write(code.join("nodes.py"), r#""type": (["stable_diffusion", "yue2", "flux3", "moss_image"], ),"#);
        // 2026-10-30 補丁加了 MOSS 編輯（TextEncodeMOSSImageEdit）：0.38 上舊版補丁要再蓋一次
        assert_eq!(moss_support(&port), MossSupport::Patchable);
        write(code.join("comfy_extras").join("nodes_moss_image.py"), "class TextEncodeMOSSImageEdit(io.ComfyNode):");
        assert_eq!(moss_support(&port), MossSupport::Ready);
        // 新版 ComfyUI 自己就有 MOSS（0.42 起）：不蓋補丁
        write(code.join("comfy_extras").join("nodes_moss_image.py"), "");
        write(code.join("comfyui_version.py"), "__version__ = \"0.42.1\"\n");
        assert_eq!(moss_support(&port), MossSupport::Ready);
        write(code.join("comfyui_version.py"), "__version__ = \"0.38.0\"\n");
        // 官方正式版到 0.40.0 都沒有 MOSS；補丁是 0.38.0 的，別版不能蓋
        write(code.join("nodes.py"), official);
        write(code.join("comfyui_version.py"), "__version__ = \"0.40.0\"\n");
        assert_eq!(moss_support(&port), MossSupport::Unsupported(Some("0.40.0".into())));
        let msg = moss_support(&port).problem().unwrap();
        assert!(msg.contains("0.40.0") && msg.contains("0.43.0") && msg.contains("安裝與修復"), "{msg}");
        assert!(MossSupport::Ready.problem().is_none() && MossSupport::Patchable.problem().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-30 有人回報精靈第一步一直「超過 240 秒沒有回應」：找舊安裝要有時間上限，時間到就回傳找到的，沒掃完的硬碟另外列
    #[test]
    fn find_installs_has_a_time_limit() {
        let d = std::env::temp_dir().join(format!("anima_find_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let touch = |p: PathBuf| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        };
        let (a, b) = (d.join("A"), d.join("B"));
        let port = a.join("AI").join("ComfyUI_windows_portable");
        touch(port.join("python_embeded").join("python.exe"));
        touch(port.join("ComfyUI").join("main.py"));
        touch(port.join("ComfyUI").join("nodes.py"));
        touch(b.join("x").join("anima_data").join("data").join("anima.db"));
        let roots = vec![a.clone(), b.clone()];
        let f = find_installs_in(&roots, Duration::from_secs(30));
        assert_eq!(f.comfy, vec![port]);
        assert_eq!(f.data, vec![b.join("x").join("anima_data")]);
        assert!(f.unfinished.is_empty());
        let t = Instant::now();
        let f = find_installs_in(&roots, Duration::ZERO);
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(f.unfinished, roots);
        // 上一次放棄、執行緒還卡在那顆硬碟上：不再開新的執行緒，直接算沒掃完
        WALKING.lock().unwrap().push(b.clone());
        let f = find_installs_in(&roots, Duration::from_secs(30));
        WALKING.lock().unwrap().retain(|x| x != &b);
        assert_eq!(f.unfinished, vec![b.clone()]);
        assert!(f.data.is_empty() && f.comfy.len() == 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 時間到還沒回來的項目是 None，其他照順序回來
    #[test]
    fn each_until_drops_late_items() {
        let r = each_until(&[0u64, 3000], |ms| {
            std::thread::sleep(Duration::from_millis(ms));
            ms
        }, Instant::now() + Duration::from_millis(300));
        assert_eq!(r, vec![Some(0), None]);
    }

    /// 各家整合包、啟動器的擺法（照 2026-10-21 查到的資料夾結構做假的）：
    /// （名字, 要建的檔, 使用者選的資料夾, main.py 那層, Python, 工作資料夾, 免安裝版?, 掃硬碟時算一份安裝?）
    #[test]
    fn distro_layouts() {
        let d = std::env::temp_dir().join(format!("anima_distro_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let src = ["main.py", "nodes.py"];
        type Case<'a> = (&'a str, Vec<String>, &'a str, &'a str, &'a str, &'a str, bool, bool);
        let with = |pre: &str, py: &str| -> Vec<String> {
            let mut v: Vec<String> = src.iter().map(|f| format!("{pre}{f}")).collect();
            v.push(py.to_string());
            v
        };
        let cases: Vec<Case> = vec![
            ("秋葉 v1.6 以後", with("aki16/ComfyUI/", "aki16/python/python.exe"), "aki16", "aki16/ComfyUI", "aki16/python/python.exe", "aki16", false, true),
            ("秋葉 v1.5 以前（平的）", with("aki15/", "aki15/python/python.exe"), "aki15", "aki15", "aki15/python/python.exe", "aki15", false, true),
            ("Easy-Install", with("easy/ComfyUI/", "easy/python_embeded/python.exe"), "easy", "easy/ComfyUI", "easy/python_embeded/python.exe", "easy", true, true),
            ("Comfy Desktop 2.0", with("cd2/ComfyUI/", "cd2/standalone-env/python.exe"), "cd2", "cd2/ComfyUI", "cd2/standalone-env/python.exe", "cd2", false, true),
            ("StabilityMatrix", with("sm/Data/Packages/ComfyUI/", "sm/Data/Packages/ComfyUI/venv/Scripts/python.exe"), "sm/Data/Packages/ComfyUI", "sm/Data/Packages/ComfyUI", "sm/Data/Packages/ComfyUI/venv/Scripts/python.exe", "sm/Data/Packages/ComfyUI", false, true),
            ("Pinokio", with("pk/api/comfy.git/app/", "pk/api/comfy.git/app/env/Scripts/python.exe"), "pk/api/comfy.git/app", "pk/api/comfy.git/app", "pk/api/comfy.git/app/env/Scripts/python.exe", "pk/api/comfy.git/app", false, true),
            ("SwarmUI", with("sw/dlbackend/comfy/ComfyUI/", "sw/dlbackend/comfy/python_embeded/python.exe"), "sw/dlbackend/comfy", "sw/dlbackend/comfy/ComfyUI", "sw/dlbackend/comfy/python_embeded/python.exe", "sw/dlbackend/comfy", true, true),
            ("clone＋外層 venv", with("ov/ComfyUI/", "ov/.venv/Scripts/python.exe"), "ov", "ov/ComfyUI", "ov/.venv/Scripts/python.exe", "ov/ComfyUI", false, true),
        ];
        let p = |s: &str| d.join(s.replace('/', "\\"));
        for (name, files, pick, root, py, cwd, portable, install) in &cases {
            for f in files {
                std::fs::create_dir_all(p(f).parent().unwrap()).unwrap();
                std::fs::write(p(f), b"").unwrap();
            }
            let l = comfy_layout(&p(pick)).unwrap_or_else(|| panic!("{name}：認不出來"));
            assert_eq!(l.root, p(root), "{name}");
            assert_eq!(l.main, p(root).join("main.py"), "{name}");
            assert_eq!(l.python, p(py), "{name}");
            assert_eq!(l.cwd, p(cwd), "{name}");
            assert_eq!((l.portable, l.desktop), (*portable, false), "{name}");
            assert_eq!(comfy_install(&p(pick)), *install, "{name}");
        }
        // 掃硬碟時順便看的深處位置
        assert_eq!(inner_install(&p("pk")), Some(p("pk/api/comfy.git/app")));
        assert_eq!(inner_install(&p("sm")), Some(p("sm/Data/Packages/ComfyUI")));
        assert_eq!(inner_install(&p("sw")), Some(p("sw/dlbackend/comfy")));
        assert_eq!(inner_install(&p("aki16/python")), None);
        let nested = nested_installs(&p("pk"));
        assert!(nested.contains(&p("pk/api/comfy.git/app")));
        assert!(nested_installs(&p("sm")).contains(&p("sm/Data/Packages/ComfyUI")));
        assert!(nested_installs(&p("sw")).contains(&p("sw/dlbackend/comfy")));
        // 官方桌面版（舊版）：選的資料夾只有 .venv、models、custom_nodes，程式在 %LOCALAPPDATA%\Programs\ComfyUI
        let base = p("Documents/ComfyUI");
        std::fs::create_dir_all(base.join(".venv").join("Scripts")).unwrap();
        std::fs::write(base.join(".venv").join("Scripts").join("python.exe"), b"").unwrap();
        std::fs::create_dir_all(base.join("models")).unwrap();
        let local = p("Local");
        let main = local.join("Programs").join("ComfyUI").join("resources").join("ComfyUI").join("main.py");
        assert!(layout_with(&base, || desktop_main_in(&desktop_apps(&[local.join("Programs")])), &PyOpt::default()).is_none()); // 程式還沒裝
        std::fs::create_dir_all(main.parent().unwrap()).unwrap();
        std::fs::write(&main, b"").unwrap();
        let l = layout_with(&base, || desktop_main_in(&desktop_apps(&[local.join("Programs")])), &PyOpt::default()).unwrap();
        assert!(l.desktop && !l.portable);
        assert_eq!((l.root.clone(), l.main.clone(), l.cwd.clone()), (base.clone(), main, base.clone()));
        assert_eq!(comfyui_dir(&base), base);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-10-30 LTX 退場：精靈找出以前「影片」包下載的三個 LTX 檔（ComfyUI 的 models＋外部模型資料夾），使用者按了才刪；
    /// 名字不一樣的（使用者自己放的其他 LTX 檔）不碰
    #[test]
    fn ltx_leftovers_found_and_removed() {
        let root = std::env::temp_dir().join(format!("anima_ltx_left_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let models = root.join("models");
        let ext = root.join("ext_ckpt");
        for (dir, name) in [(models.join("text_encoders"), "gemma-3-12b-it-ablit-norms-biproj-fp8mixed.safetensors"),
                            (models.join("latent_upscale_models"), "ltx-2.3-spatial-upscaler-x2-1.1.safetensors"),
                            (ext.clone(), "10Eros_v1.5_DMD_INT8_checkpoint.safetensors"),
                            (models.join("checkpoints"), "ltx-2.3-my-own.safetensors")] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), b"1234").unwrap();
        }
        let extra = vec![crate::model_dirs::Dir { kind: "checkpoints".into(), path: ext.clone(), source: "manual".into(), added: String::new() }];
        let mut found: Vec<String> = ltx_leftovers(&models, &extra).into_iter().map(|f| f.0.file_name().unwrap().to_string_lossy().to_string()).collect();
        found.sort();
        assert_eq!(found, ["10Eros_v1.5_DMD_INT8_checkpoint.safetensors", "gemma-3-12b-it-ablit-norms-biproj-fp8mixed.safetensors", "ltx-2.3-spatial-upscaler-x2-1.1.safetensors"]);
        let (freed, fails) = remove_ltx(&ltx_leftovers(&models, &extra));
        assert_eq!((freed, fails.len()), (12, 0));
        assert!(ltx_leftovers(&models, &extra).is_empty());
        assert!(models.join("checkpoints").join("ltx-2.3-my-own.safetensors").is_file(), "使用者自己的檔不能刪");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 升級換新 9B 要下載的：AI 改寫包裡的語言模型檔（9B、它的看圖檔、挑段模型），不含選配的 35B、官方 PE
    #[test]
    fn llm_pack_files_are_the_9b_set() {
        let names: Vec<String> = llm_pack_files(&crate::paths::default_res()).into_iter().map(|f| f.0).collect();
        assert!(names.iter().any(|n| n.contains("Defiant-Fable") && !n.contains("mmproj")), "{names:?}");
        assert!(names.iter().any(|n| n.contains("Defiant-Fable-mmproj")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("35B") || n.to_lowercase().starts_with("pe_") || n.contains('/')), "{names:?}");
    }

    /// 功能包：完整版每個檔都有歸包；models_basic.json 跟 BASIC_PACKS 一致；舊的 models 參數照樣能用
    #[test]
    fn packs_cover_manifests() {
        let res = crate::paths::default_res();
        let full = manifest(&res, "full");
        assert!(full.len() > 25); // 公開版拿掉收藏 LoRA 後約 33 個，私人版 80 多個
        let other: Vec<&str> = full.iter().map(|m| m.rel.as_str()).filter(|r| pack_of(r) == "other").collect();
        assert!(other.is_empty(), "沒歸包的檔：{other:?}");
        let mut basic: Vec<String> = manifest(&res, "basic").into_iter().map(|m| m.rel).collect();
        let mut from_packs: Vec<String> = full.iter().filter(|m| BASIC_PACKS.contains(&pack_of(&m.rel))).map(|m| m.rel.clone()).collect();
        basic.sort();
        from_packs.sort();
        assert_eq!(basic, from_packs);
        assert_eq!(packs_from(&json!({"models": "basic"})).unwrap(), BASIC_PACKS);
        assert!(packs_from(&json!({"models": "none"})).unwrap().is_empty());
        let full_packs = packs_from(&json!({"models": "full"})).unwrap();
        assert!(full_packs.len() == PACKS.len() - OPTIONAL_PACKS.len() + 1 && !full_packs.iter().any(|p| OPTIONAL_PACKS.contains(&p.as_str())));
        // 2026-10-30 有人回報「點 moss 快速提示找不到 MOSS 加速 LoRA」：MOSS 包（基本版也有）要帶 gen.js MOSS_FAST 找的那顆
        let speed = "loras/moss/moss_speed_dmd_lodestones.safetensors";
        let m = full.iter().find(|m| m.rel == speed).expect("清單沒有 MOSS 加速 LoRA");
        assert_eq!(pack_of(speed), "moss");
        assert!(m.sha256.is_some() && m.url.as_deref().is_some_and(|u| u.contains("/resolve/f95df83b")), "{:?}", m.url);
        assert!(manifest(&res, "basic").iter().any(|m| m.rel == speed));
        // MOSS 照指令改圖（2026-10-30）：預設的編輯 LoRA（chinoll/Moss-Image-Edit）也在 MOSS 包
        let edit = "loras/moss/moss_edit_chinoll.safetensors";
        let m = full.iter().find(|m| m.rel == edit).expect("清單沒有 MOSS 編輯 LoRA");
        assert_eq!(pack_of(edit), "moss");
        assert!(m.sha256.is_some() && m.url.as_deref().is_some_and(|u| u.contains("/resolve/3be9c400")), "{:?}", m.url);
        // 2026-10-31 MOSS 色彩／深度控制：生成頁構圖控制直接用，放 MOSS 包（基本版也有），路徑跟 graphs.rs MOSS_CONTROL_LORA 一致
        let ctrl = format!("loras/{}", crate::graphs::MOSS_CONTROL_LORA.replace('\\', "/"));
        assert_eq!(pack_of(&ctrl), "moss");
        assert!(manifest(&res, "basic").iter().any(|m| m.rel == ctrl), "基本版沒有 {ctrl}");
        // 4B 文字編碼器＋轉接器：選配包 moss_4b，檔名跟 graphs.rs 一致；9B 照舊在 MOSS 包
        for rel in [format!("text_encoders/{}", crate::graphs::MOSS_TE_4B), format!("moss_encoder_adapters/{}", crate::graphs::MOSS_4B_ADAPTER)] {
            let m = full.iter().find(|m| m.rel == rel).unwrap_or_else(|| panic!("清單沒有 {rel}"));
            assert_eq!(pack_of(&rel), "moss_4b");
            assert!(m.sha256.is_some(), "{rel}");
        }
        assert_eq!(pack_of("text_encoders/qwen3.5_9b_w4a8_gptq.safetensors"), "moss");
        // MOSS LoRA 合集（gordonz/Excellent-Loras-for-MossImage）：選配包；不收真人肖像；清單裡不能有兩個一模一樣的檔
        let bundle: Vec<&ModelFile> = full.iter().filter(|m| m.rel.starts_with("loras/moss/Excellent-Loras/")).collect();
        assert!(bundle.len() >= 75 && bundle.iter().all(|m| pack_of(&m.rel) == "moss_loras" && m.sha256.is_some() && m.url.as_deref().is_some_and(|u| u.contains("modelscope.cn"))));
        assert!(!full.iter().any(|m| m.rel.contains("Gracie_Phoenix")));
        let mut shas: Vec<&str> = full.iter().filter_map(|m| m.sha256.as_deref()).collect();
        let n = shas.len();
        shas.sort();
        shas.dedup();
        assert_eq!(shas.len(), n, "清單裡有 SHA256 一樣的檔");
        // 去色帶在合集包，生成頁 gen.js MOSS_DEBAND 用的就是這個路徑
        assert!(bundle.iter().any(|m| m.rel == "loras/moss/Excellent-Loras/SFT伪影修复/moss_image_sft_banding_fix_L37-41_bf16.safetensors"));
        // 2026-10-30 LTX 退場：影片包（10Eros v1.5、gemma、LTX 放大）整個拿掉，舊網頁送來的 video 安靜略過
        assert_eq!(OPTIONAL_PACKS, ["pe_35b", "h3", "moss_4b", "moss_loras"]);
        assert!(!PACKS.iter().any(|p| p.0 == "video"));
        assert_eq!(packs_from(&json!({"packs": ["video", "anima"]})).unwrap(), vec!["anima"]);
        for gone in ["10eros_v1.5", "gemma-3-12b", "ltx-2.3"] {
            assert!(!full.iter().any(|m| m.rel.to_lowercase().contains(gone)), "清單還有 {gone}");
        }
        // 本機 H3 包（2026-10-30）：h3.rs 寫死的四個檔都在「h3」包，網址鎖 commit、有 SHA256；Qwen 2.1 的 qwen3vl 不能被搶走
        for name in [crate::h3::MODEL_W4A8, crate::h3::TE, crate::h3::VAE, crate::h3::AVAE] {
            let m = full.iter().find(|m| m.rel.ends_with(&format!("/{name}"))).unwrap_or_else(|| panic!("清單沒有 {name}"));
            assert_eq!(pack_of(&m.rel), "h3", "{name}");
            assert!(m.sha256.is_some() && m.url.as_deref().is_some_and(|u| u.contains("/resolve/") && u.ends_with(name)), "{name}");
        }
        assert_eq!(pack_of("text_encoders/qwen3vl_8b_w4a8.safetensors"), "qwen");
        // 2026-10-30 官方 PE 整個拿掉：清單裡不能再有，舊網頁／舊 MCP 送來的 pe_official 安靜略過；9B 跟它的看圖檔在 AI 改寫包
        assert!(!full.iter().any(|m| m.rel.to_lowercase().contains("/pe_")), "清單還有官方 PE");
        assert!(!PACKS.iter().any(|p| p.0 == "pe_official"));
        assert_eq!(packs_from(&json!({"packs": ["pe_official", "anima"]})).unwrap(), vec!["anima"]);
        assert_eq!(pack_of("LLM/Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf"), "pe");
        assert_eq!(pack_of("LLM/Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-Q4_K_M.gguf"), "pe");
        assert_eq!(pack_of("LLM/Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf"), "pe");
        // 35B 是選配，要先有 AI 改寫包（llama.cpp）
        assert_eq!(pack_of("LLM/Qwen3.6-35B-A3B-uncensored-heretic-Native-MTP-Preserved-APEX-I-Compact.gguf"), "pe_35b");
        assert_eq!(pack_of("LLM/mmproj-Qwen3.6-35B-A3B-uncensored-heretic-Native-MTP-Preserved-APEX-F16.gguf"), "pe_35b");
        assert_eq!(packs_from(&json!({"packs": ["pe_35b"]})).unwrap(), vec!["pe_35b", "pe"]);
        // 要先有的那一包會自動補上
        let v = packs_from(&json!({"packs": ["qwen_loras", "qwen_pose"]})).unwrap();
        assert_eq!(v, vec!["qwen_loras", "qwen_pose", "qwen"]);
        // 安裝精靈只留官方 Qwen Image 2.1 Turbo（2026-10-22 使用者要求拿掉 Noct Q 這些社群微調）
        assert!(full.iter().filter(|m| m.rel.starts_with("diffusion_models/")).all(|m| !m.rel.to_lowercase().starts_with("diffusion_models/noctq")));
        assert!(packs_from(&json!({"packs": ["xyz"]})).is_err());
        assert!(packs_from(&json!({"models": "huge"})).is_err());
    }

    /// 清單：Civitai 網址一定指定 fileId（同一個版本可能有好幾個檔，不指定會拿到「預設」那個，作者換預設就對不上大小，
    /// 2026-10-22 ZodaPlus 出過事）；有下載來源的都附 SHA256；AI 改寫的 9B 有讀圖用的 mmproj（生圖助手、導演台看圖要）
    #[test]
    fn manifest_pins_and_hashes() {
        let res = crate::paths::default_res();
        let civ = regex::Regex::new(r"^https://civitai\.com/api/download/models/\d+\?fileId=\d+$").unwrap();
        for which in ["basic", "full"] {
            for m in manifest(&res, which) {
                let Some(u) = m.url.as_deref() else { continue };
                if u.contains("civitai.com") {
                    assert!(civ.is_match(u), "沒指定 fileId：{u}");
                }
                if !u.contains("github.com") {
                    assert!(m.sha256.is_some(), "沒有 SHA256：{}", m.rel);
                }
            }
        }
        let (name, size, url, sha) = mmproj_source(&res, "qwen3.5").expect("9B 的 mmproj");
        // 2026-10-29 起預設 9B 換成 Defiant Fable；它的看圖檔在 HF 上叫 mmproj-F16.gguf，存檔時用清單裡的名字
        assert_eq!(name, "Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf");
        assert!(size > 500_000_000 && url.contains("/resolve/") && url.ends_with("/mmproj-F16.gguf") && sha.is_some());
        assert_eq!(pack_of(&format!("LLM/{name}")), "pe");
        // 2026-10-29 起 35B 是選配包，它的看圖檔也在清單裡：有 35B 主檔、缺 mmproj 時可以自動補
        let (name, _, url, sha) = mmproj_source(&res, "qwen3.6").expect("35B 的 mmproj");
        assert!(name.starts_with("mmproj-Qwen3.6-35B") && url.ends_with(&name) && sha.is_some());
        assert_eq!(pack_of(&format!("LLM/{name}")), "pe_35b");
        let d = std::env::temp_dir().join("anima_sha_test.bin");
        std::fs::write(&d, b"abc").unwrap();
        assert_eq!(sha256_file(&d).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(&d);
        let m = changed_msg(Path::new(r"D:\m\loras\zodaplus_v2_anima.safetensors.part"), 34798680, 34803536);
        assert!(m.contains("「zodaplus_v2_anima.safetensors」") && m.contains("34798680") && m.contains("重試"), "{m}");
        assert!(sha_mismatch_msg(Path::new(r"D:\m\x.gguf"), "https://huggingface.co/x").contains("huggingface.co"));
    }

    /// 照來源新版下載的模型：記下新版大小後，大小跟清單不同也算「有」（不會每次都被當成缺檔重下）；大小都不對的還是缺
    #[test]
    fn adopted_newer_version_counts_as_installed() {
        let res = crate::paths::default_res();
        // 拿清單裡最小的檔來測（測試要寫一個差不多大的假檔）；公開版沒有收藏 LoRA，不能指定 ZodaPlus
        let zoda = manifest(&res, "full").into_iter().filter(|m| m.size > 10_000).min_by_key(|m| m.size).expect("清單有檔");
        let models = std::env::temp_dir().join(format!("anima_adopt_test_{}", std::process::id()));
        let f = rel_path(&models, &zoda.rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        let newer = zoda.size - 4856;
        std::fs::write(&f, vec![0u8; newer as usize]).unwrap();
        let missing = |m: &Path| missing_models(&res, m, "full", &[]).iter().any(|x| x.rel == zoda.rel);
        assert!(missing(&models), "大小跟清單不同、又沒記過新版：缺");
        remember_adopted(&models, &zoda.rel, newer, "https://civitai.com/api/download/models/3201207?fileId=1").unwrap();
        remember_adopted(&models, "loras/other.safetensors", 1, "u").unwrap();
        assert_eq!(adopted_sizes(&models).get(&zoda.rel), Some(&newer));
        assert_eq!(adopted_sizes(&models).len(), 2, "記第二個檔不能蓋掉第一個");
        assert!(!missing(&models), "記過新版：算有");
        std::fs::write(&f, vec![0u8; 10]).unwrap();
        assert!(missing(&models), "兩個大小都不對：缺");
        let _ = std::fs::remove_dir_all(&models);
    }

    /// 「用 CPU 跑」：沒有能用的顯示卡才加；沿用老手的 ZLUDA／DirectML 版不自動加（以前留下的也清掉），自己選的照選；
    /// N 卡、A 卡不加，設定頁自己勾的不偷改
    #[test]
    fn cpu_flag() {
        let auto_cpu = gpu::auto_plan(&[], true);
        let manual_cpu = gpu::plan_for("cpu", "", &[], true);
        let nv = vec![json!({"vendor": "nvidia", "name": "NVIDIA GeForce RTX 3070", "vram_mb": 8192})];
        let nv_auto = gpu::auto_plan(&nv, true);
        // 這次要下載 ComfyUI：照版本
        assert!(cpu_after(false, &auto_cpu, None, true));
        assert!(!cpu_after(true, &nv_auto, None, true));
        assert!(!cpu_after(false, &gpu::plan_for("amd", "gfx1031", &[], false), None, true));
        // 沿用現成的、沒有能用的顯示卡
        assert!(cpu_after(false, &auto_cpu, Some("cuda"), false));
        assert!(!cpu_after(false, &auto_cpu, Some("zluda"), false) && !cpu_after(true, &auto_cpu, Some("directml"), false));
        assert!(cpu_after(false, &manual_cpu, Some("zluda"), false));
        // 「用我自己的 ComfyUI」（RX 580 這類）選了 ZLUDA 版：以前裝 CPU 版留下的 --cpu 清掉
        let own_old = gpu::plan_for("own", "", &[], true);
        assert!(!cpu_after(true, &own_old, Some("zluda"), false));
        // N 卡沿用現成的：設定頁勾了就照勾，沒勾就不加
        assert!(cpu_after(true, &nv_auto, Some("cuda"), false));
        assert!(!cpu_after(false, &nv_auto, Some("cuda"), false));
        // 偵測失敗：不動
        assert!(cpu_after(true, &gpu::auto_plan(&[], false), Some("cuda"), false));
    }

    /// pip 失敗的白話原因（照 2026-10-21 實際跑出來的輸出）：連不上時最後也會印「No matching distribution」，不能說成 AMD 沒有這個版本
    #[test]
    fn pip_errors() {
        let net = "Looking in indexes: https://stable.repo.amd.com/rocm/whl-next/\n\
            WARNING: Retrying (Retry(total=0, connect=None, read=None, redirect=None, status=None)) after connection broken by 'NewConnectionError(\"HTTPSConnection(host='127.0.0.1', port=9): Failed to establish a new connection: [WinError 10061] 無法連線，因為目標電腦拒絕連線。\")': /rocm/whl-next/torch/\n\
            ERROR: Could not find a version that satisfies the requirement torch==2.13.0+rocm10.0.0 (from versions: none)\n\
            ERROR: No matching distribution found for torch==2.13.0+rocm10.0.0";
        let e = pip_error(net);
        assert!(e.starts_with("連不到 AMD 的套件庫") && e.contains("WinError 10061"), "{e}");
        let gone = "Looking in indexes: https://stable.repo.amd.com/rocm/whl-next/\n\
            ERROR: Could not find a version that satisfies the requirement torch==2.13.0+rocm10.0.0 (from versions: 2.13.0+rocm10.1.0, 2.14.0+rocm10.1.0)\n\
            ERROR: No matching distribution found for torch==2.13.0+rocm10.0.0";
        let e = pip_error(gone);
        assert!(e.starts_with("AMD 的套件庫上找不到") && e.ends_with("No matching distribution found for torch==2.13.0+rocm10.0.0"), "{e}");
        assert!(pip_error("ERROR: Could not install packages due to an OSError: [WinError 32] 程序無法存取檔案").starts_with("檔案被占用"));
        assert!(pip_error("ERROR: Could not install packages due to an OSError: [Errno 28] No space left on device").starts_with("硬碟空間不夠"));
        assert_eq!(pip_error(""), "pip 執行失敗");
    }

    /// 這台電腦（開發機）裝了 VC++；結束代碼的解讀
    #[test]
    fn vcredist_codes() {
        assert_eq!(vcredist_result(0), Ok(false));
        assert_eq!(vcredist_result(1638), Ok(false));
        assert_eq!(vcredist_result(3010), Ok(true));
        assert!(vcredist_result(1602).unwrap_err().contains("取消"));
        assert!(vcredist_result(1223).unwrap_err().contains("取消"));
        assert!(vcredist_result(1603).unwrap_err().contains("1603"));
        if let Some(v) = vcredist_version() {
            assert!(v.0 == 14, "{v:?}");
        }
    }

    #[test]
    fn plan_needs_absolute_paths() {
        assert!(plan_from(&json!({"data_root": "anima", "comfy_dir": "C:\\x"})).is_err());
        assert!(plan_from(&json!({"data_root": "", "comfy_dir": "C:\\x"})).is_err());
        assert!(plan_from(&json!({"data_root": "D:\\a", "comfy_dir": "D:\\b"})).is_ok());
    }

    #[test]
    fn config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("anima_cfg_test_{}", std::process::id()));
        let p = dir.join("config.json");
        let c = Config {
            data_root: "D:\\資料\\anima".into(),
            comfy_dir: "D:\\ComfyUI_windows_portable".into(),
        };
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p), Some(c));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 路徑填錯的訊息講白話（不露出 data_root、comfy_dir 欄位名）
    #[test]
    fn plan_error_is_plain() {
        let e = plan_from(&json!({"data_root": "anima", "comfy_dir": "C:\\x"})).err().unwrap();
        assert!(e.contains("資料資料夾") && !e.contains("data_root"), "{e}");
        let e = plan_from(&json!({"data_root": "D:\\a", "comfy_dir": "rel"})).err().unwrap();
        assert!(e.contains("ComfyUI 資料夾") && !e.contains("comfy_dir"), "{e}");
    }

    fn item(status: &str, done: u64, total: u64, unit: Option<&str>) -> Map<String, Value> {
        let mut m = json!({"key": "k", "label": "l", "status": status, "done": done, "total": total}).as_object().cloned().unwrap();
        if let Some(u) = unit {
            m.insert("unit".into(), json!(u));
        }
        m
    }

    /// 總進度：完成的算滿、進行中照目前、已經有了的和解壓縮（%）不算、超過總大小的壓回總大小
    #[test]
    fn overall_bytes() {
        let items = vec![
            item("done", 5, 100, None),
            item("run", 30, 200, None),
            item("wait", 0, 50, None),
            item("skip", 0, 999, None),
            item("run", 40, 100, Some("%")),
            item("run", 5, 0, None),
            item("error", 500, 100, None),
        ];
        assert_eq!(bytes_overall(&items), (450, 100 + 30 + 100));
        assert_eq!(bytes_overall(&[]), (0, 0));
    }

    /// 速度：看最近 20 秒；不到 1 秒的紀錄不算；舊的紀錄會被丟掉
    #[test]
    fn speed_window() {
        let t0 = Instant::now();
        let mut s = std::collections::VecDeque::new();
        assert_eq!(speed_push(&mut s, t0, 0), 0.0);
        assert_eq!(speed_push(&mut s, t0 + Duration::from_millis(500), 1000), 0.0);
        let v = speed_push(&mut s, t0 + Duration::from_secs(10), 10_000_000);
        assert!((v - 1_000_000.0).abs() < 1.0, "{v}");
        // 30 秒後：10 秒以前的紀錄被丟掉，只看最近的區間
        let v = speed_push(&mut s, t0 + Duration::from_secs(30), 30_000_000);
        assert!((v - 1_000_000.0).abs() < 1.0, "{v}");
        // 沒有新資料：速度 0
        assert_eq!(speed_push(&mut s, t0 + Duration::from_secs(60), 30_000_000), 0.0);
    }

    /// 進度頁的模型名字：不露出資料夾路徑；LoRA 用清單裡的名字；全部清單的檔都有名字
    #[test]
    fn model_labels() {
        let mf = |rel: &str, note: Option<&str>| ModelFile { rel: rel.into(), size: 1, url: None, note: note.map(str::to_string), sha256: None };
        assert_eq!(model_label(&mf("diffusion_models/anima-aesthetic-v1.1.safetensors", None)), "Anima 動漫・主模型：anima-aesthetic-v1.1.safetensors");
        assert!(model_label(&mf("text_encoders/qwen3vl_8b_w4a8.safetensors", None)).starts_with("Qwen 2.1 編輯・文字編碼器："));
        assert_eq!(
            model_label(&mf("loras/anima-base-1-flat-color-v3.safetensors", Some("Flat Color - Style"))),
            "Anima 常用 LoRA・LoRA：Flat Color - Style （anima-base-1-flat-color-v3.safetensors）"
        );
        assert!(model_label(&mf("loras/qwen21/x.safetensors", None)).ends_with("：x.safetensors"));
        let res = crate::paths::default_res();
        for m in manifest(&res, "full") {
            let l = model_label(&m);
            let dir = m.rel.rsplit_once('/').map(|x| x.0).unwrap_or("");
            assert!(!l.contains(&format!("{dir}/")) && !l.contains("_models") && !l.starts_with("其他"), "{l}");
        }
    }

    /// 清單裡 Hugging Face 的網址都固定在 commit（40 位十六進位），不用會跟著上游換的 main；鏡像換網址後 sha 還在
    #[test]
    fn manifest_urls_pinned() {
        let res = crate::paths::default_res();
        let re = regex::Regex::new(r"^https://huggingface\.co/[^/]+/[^/]+/resolve/[0-9a-f]{40}/.+").unwrap();
        let mut n = 0;
        for which in ["basic", "full"] {
            for m in manifest(&res, which) {
                if let Some(u) = m.url.as_deref().filter(|u| u.contains("huggingface.co")) {
                    assert!(re.is_match(u), "沒固定 commit：{u}");
                    n += 1;
                }
            }
        }
        assert!(n > 20);
        assert!(TAGGER.contains("/resolve/") && !TAGGER.contains("/resolve/main"));
        let c = crate::net::candidates(&format!("{TAGGER}model.onnx"));
        assert_eq!(c[0], format!("{TAGGER}model.onnx"));
        assert_eq!(c[1], format!("{TAGGER}model.onnx").replacen("huggingface.co", "hf-mirror.com", 1));
    }

    /// 檔案大小不對、GitHub 失敗的說明：講清楚是哪個檔、放哪裡
    #[test]
    fn download_messages() {
        let dest = PathBuf::from(r"D:\AnimaStudio\_anima_dl\ComfyUI_windows_portable_nvidia.7z");
        let gh = "https://github.com/Comfy-Org/ComfyUI/releases/download/v0.38.0/ComfyUI_windows_portable_nvidia.7z";
        let h = manual_hint(gh, &dest, Some(1_994_326_521)).unwrap();
        assert!(h.contains(r"D:\AnimaStudio\_anima_dl") && h.contains("ComfyUI_windows_portable_nvidia.7z") && h.contains("1994326521") && h.contains("重試"), "{h}");
        assert!(manual_hint("https://huggingface.co/a/b/resolve/main/x", &dest, None).is_none());
        assert!(manual_hint("https://civitai.com/api/download/models/1", &dest, None).is_none());
        let w = wrong_size_msg(&dest, 100, 1_994_326_521);
        assert!(w.contains("ComfyUI_windows_portable_nvidia.7z") && w.contains(r"D:\AnimaStudio\_anima_dl") && w.contains("刪掉") && w.contains("重試") && !w.contains("移走"), "{w}");
        assert_eq!(human_bytes(512), "512 位元組");
        assert_eq!(human_bytes(1_994_326_521), "1.9 GB");
    }

    /// 低配判斷：顯存 8GB 以下或記憶體 16GB 以下；記憶體不到 15GB 另外提醒；不知道就不算低
    #[test]
    fn lite_spec() {
        const G: u64 = 1 << 30;
        let gpu = |mb: i64| vec![json!({"vendor": "nvidia", "vram_mb": mb})];
        let v = lite_view(&gpu(8192), Some(32 * G));
        assert_eq!((v["low_vram"].as_bool(), v["low_ram"].as_bool(), v["lite"].as_bool()), (Some(true), Some(false), Some(true)));
        let v = lite_view(&gpu(12288), Some(16 * G));
        assert_eq!((v["low_vram"].as_bool(), v["low_ram"].as_bool(), v["tiny_ram"].as_bool()), (Some(false), Some(true), Some(false)));
        let v = lite_view(&gpu(24576), Some(32 * G));
        assert_eq!(v["lite"].as_bool(), Some(false));
        let v = lite_view(&gpu(12288), Some(8 * G));
        assert_eq!(v["tiny_ram"].as_bool(), Some(true));
        // N 卡加內顯：看最大的
        let both = vec![json!({"vram_mb": 512}), json!({"vram_mb": 16384})];
        assert_eq!(lite_view(&both, Some(32 * G))["lite"].as_bool(), Some(false));
        // 不知道：不算低
        let v = lite_view(&[], None);
        assert_eq!(v["lite"].as_bool(), Some(false));
    }

    #[cfg(windows)]
    #[test]
    fn ram_is_readable() {
        assert!(ram_total().is_some_and(|r| r > (1 << 30)));
    }

    /// 解壓暫存：磁碟根目錄底下的短名字（比最後位置短）；沒有根目錄可用就放在旁邊；別人的同名資料夾不去動
    #[test]
    fn unpack_dir_is_short() {
        assert!(is_drive_root(Path::new(r"D:\")) && is_drive_root(Path::new("c:/")));
        assert!(!is_drive_root(Path::new(r"D:\a")) && !is_drive_root(Path::new(r"\\srv\share\")));
        assert_eq!(Path::new(r"D:\AnimaStudio\ComfyUI_windows_portable").ancestors().last(), Some(Path::new(r"D:\")));
        let base = std::env::temp_dir().join(format!("anima_unpack_test_{}", std::process::id()));
        let (root, parent) = (base.join("root"), base.join("很長很長的資料夾名字").join("再一層"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&parent).unwrap();
        // 有磁碟根目錄：用它底下的短名字
        let u = unpack_dir_in(Some(&root), &parent);
        assert_eq!(u, root.join(UNPACK_NAME));
        assert!(u.is_dir() && u.to_string_lossy().len() < parent.join("ComfyUI_windows_portable").to_string_lossy().len());
        // 沒有（網路路徑）：放旁邊
        let u = unpack_dir_in(None, &parent);
        assert_eq!(u, parent.join(UNPACK_NAME));
        // 根目錄底下已經有別人的同名資料夾（裡面有別的東西）：不動它，改放旁邊
        std::fs::write(root.join(UNPACK_NAME).join("別人的檔案.txt"), "x").unwrap();
        assert_eq!(unpack_dir_in(Some(&root), &parent), parent.join(UNPACK_NAME));
        assert!(root.join(UNPACK_NAME).join("別人的檔案.txt").is_file());
        // 上次留下的（只有 ComfyUI_windows_portable）：照用
        std::fs::remove_file(root.join(UNPACK_NAME).join("別人的檔案.txt")).unwrap();
        std::fs::create_dir_all(root.join(UNPACK_NAME).join("ComfyUI_windows_portable")).unwrap();
        assert_eq!(unpack_dir_in(Some(&root), &parent), root.join(UNPACK_NAME));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 安裝期間擋睡眠：開始就在擋、放掉就恢復（SetThreadExecutionState 只管呼叫的執行緒，所以是專用執行緒）
    #[test]
    fn sleep_guard_releases() {
        let before = AWAKE.load(Ordering::SeqCst);
        let g = SleepGuard::start();
        assert_eq!(AWAKE.load(Ordering::SeqCst), before + 1);
        drop(g);
        assert_eq!(AWAKE.load(Ordering::SeqCst), before);
    }

    #[test]
    fn timeout_helper() {
        assert_eq!(call_with_timeout(|| 7, Duration::from_secs(5)), Some(7));
        assert_eq!(call_with_timeout(|| { std::thread::sleep(Duration::from_millis(600)); 1 }, Duration::from_millis(50)), None);
    }

    /// 可以被「停止」打斷的等待（不是真的跑安裝程式：abandon 一開始就是 true，開一個不存在的程式只會得到錯誤）
    #[cfg(windows)]
    #[test]
    fn run_and_wait_missing_exe() {
        let r = run_and_wait(Path::new(r"C:\anima_no_such_dir\nope.exe"), "", &AtomicBool::new(true));
        assert!(r.is_err());
    }
}

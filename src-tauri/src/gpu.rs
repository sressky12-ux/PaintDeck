//! 顯示卡和 ComfyUI 版本：偵測顯示卡（nvidia-smi → WMI）、這台電腦該裝哪一包 ComfyUI（NVIDIA／AMD／Intel／CPU）、
//! A 卡的架構（gfx）對照、使用者在精靈第二頁的選擇和 ComfyUI 的啟動方式（config.json 旁邊的 comfy.json），
//! 還有認得別人裝好的 ComfyUI 用的是哪一種 PyTorch（CUDA、ROCm、XPU、DirectML、ZLUDA）。
//!
//! 原則：N 卡照舊（nvidia-smi 看得到就裝 NVIDIA 版，什麼都不變）。其他情況盡量給一條能走的路：
//!   - A 卡：官方 AMD 免安裝版（PyTorch 2.9.1＋ROCm 7.2.1）本身就有的架構直接用；RX 5000／6000、780M 這類沒有的，
//!     裝完再用 pip 從 AMD 官方的 ROCm 10 多架構套件庫把 PyTorch 換成有那個架構的版本（setup.rs rocm10_*）。
//!   - Intel Arc 獨顯、Core Ultra 的 Arc 內顯：官方 Intel 免安裝版（PyTorch XPU）。
//!   - 什麼都不能用（沒有獨顯、太舊的卡）：照樣能裝（圖庫、提示詞庫、雲端顯卡都能用），ComfyUI 用 CPU 跑。
//!   - 偵測失敗（精簡版 Windows 沒有 PowerShell、WMI 壞掉）：不再默默裝 NVIDIA 版，請使用者自己選。
//!
//! 測試：環境變數 ANIMA_FAKE_GPUS＝跟 WMI 偵測一樣格式的 JSON（{"vc": [...], "reg": [...]}，見 gpus_from_wmi），
//!       或 "fail"（模擬偵測失敗）。有設就不問 nvidia-smi。JSON 裡多一個 "nvidia": [{name, vram_mb?, driver?, compute_cap?}]
//!       ＝假的 nvidia-smi 結果（有驅動、算「看得到」的 N 卡；WMI 找到的 N 卡一律標驅動沒裝好）
use crate::pe::win;
use crate::setup::ComfyLayout;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// ComfyUI 0.38 免安裝版的 PyTorch 是 cu130（CUDA 13）：驅動要 R580 以上（NVIDIA CUDA Toolkit 版本說明）
pub const DRIVER_MIN: u32 = 580;
/// 舊卡版（GTX 10 系列以前，PyTorch cu126＝CUDA 12.6.x）的驅動門檻：NVIDIA CUDA Toolkit 版本說明
/// 「CUDA Toolkit and Corresponding Driver Versions」表，CUDA 12.6 Windows 最低驅動 GA 560.76、Update 1／2 是 560.94、Update 3 是 561.17；
/// PyTorch 的 cu126 輪子帶 12.6 的最新小版本，所以取 561（2026-10-21 查證）
pub const DRIVER_MIN_LEGACY: u32 = 561;

// ---------------------------------------------------------------- 偵測
/// 顯示卡：[{vendor: nvidia|amd|intel, name, vram_mb, driver, driver_old?（N 卡）, gfx?, route?（A 卡）, arc?（Intel）, driver_missing?}]，
/// 和偵測有沒有成功（nvidia-smi 有回應、或 WMI 有回應；兩個都沒有＝不知道這台有什麼卡）。
/// N 卡照舊問 nvidia-smi；問不到（沒有 N 卡、或 N 卡驅動沒裝好）才用 WMI 找。
/// WMI 找到的 N 卡標 driver_missing（nvidia-smi 看不到它＝驅動有問題）：精靈照舊顯示「找不到 NVIDIA 顯示卡」，但照樣算 N 卡
fn detect_now() -> (Vec<Value>, bool) {
    if let Some(fake) = std::env::var("ANIMA_FAKE_GPUS").ok().filter(|s| !s.is_empty()) {
        if fake == "fail" {
            return (vec![], false);
        }
        return serde_json::from_str::<Value>(&fake)
            .map(|v| {
                let mut g: Vec<Value> = v["nvidia"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|n| {
                        nv_entry(
                            n["name"].as_str().unwrap_or(""),
                            n["vram_mb"].as_i64(),
                            n["driver"].as_str().unwrap_or("999.0"),
                            n["compute_cap"].as_str(),
                        )
                    })
                    .collect();
                if g.is_empty() {
                    gpus_from_wmi(&v)
                } else {
                    // 有假的 nvidia-smi 結果時，WMI 裡的 N 卡不重複算
                    g.extend(gpus_from_wmi(&v).into_iter().filter(|x| x["vendor"] != "nvidia"));
                    g
                }
            })
            .map(|g| (g, true))
            .unwrap_or((vec![], false));
    }
    let n = nvidia_gpus();
    if !n.is_empty() {
        return (n, true);
    }
    match wmi_json() {
        Some(v) => (gpus_from_wmi(&v), true),
        None => (vec![], false),
    }
}

/// 上次偵測到的顯示卡（偵測要開 nvidia-smi／PowerShell，約 1 秒；檢查位置每打一個字就會問一次）
static DETECTED: Mutex<Option<(Vec<Value>, bool)>> = Mutex::new(None);

/// 顯示卡清單和偵測有沒有成功。fresh＝重新偵測（精靈第一頁「檢查電腦」），不然用上次的結果
pub fn detect(fresh: bool) -> (Vec<Value>, bool) {
    let mut g = DETECTED.lock().unwrap_or_else(|e| e.into_inner());
    if fresh || g.is_none() {
        *g = Some(detect_now());
    }
    g.clone().unwrap_or_default()
}

/// N 卡（nvidia-smi）：[{vendor, name, vram_mb, driver, driver_old, compute_cap, legacy}]
fn nvidia_gpus() -> Vec<Value> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args(["--query-gpu=name,memory.total,driver_version", "--format=csv,noheader,nounits"]);
    win::no_window(&mut cmd);
    let Ok(out) = cmd.output() else { return vec![] };
    let caps = nvidia_caps();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .filter_map(|(i, l)| {
            let mut it = l.rsplitn(3, ',');
            let (driver, mb, name) = (it.next()?.trim(), it.next()?, it.next()?);
            // 卡數對得上才配對算力（兩次查詢之間不會插拔顯示卡，不一致就不信，改看名字）
            let cap = caps.as_ref().and_then(|c| c.get(i)).map(String::as_str);
            Some(nv_entry(name.trim(), mb.trim().parse::<i64>().ok(), driver, cap))
        })
        .collect()
}

/// 每張 N 卡的運算能力（nvidia-smi --query-gpu=compute_cap：「8.6」）。舊驅動沒有這個欄位（整個查詢會失敗）＝None，改看名字。
/// 另外查一次、不併進主查詢：併進去的話，舊驅動連名字、顯存都問不到
fn nvidia_caps() -> Option<Vec<String>> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args(["--query-gpu=compute_cap", "--format=csv,noheader,nounits"]);
    win::no_window(&mut cmd);
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let v: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    (!v.is_empty() && v.iter().all(|c| parse_cap(c).is_some())).then_some(v)
}

/// 「7.5」→ (7, 5)
fn parse_cap(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.trim().split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// 一張 N 卡的資料。舊卡（GTX 10 系列以前）的驅動門檻照 CUDA 12.6 算，其他照舊 580
fn nv_entry(name: &str, vram_mb: Option<i64>, driver: &str, cap: Option<&str>) -> Value {
    // 驅動版本 617.14 → 主版號 617（太舊的話精靈會提醒去更新）
    let major = driver.split('.').next().and_then(|m| m.parse::<u32>().ok());
    let legacy = nv_legacy(name, cap);
    let min = if legacy { DRIVER_MIN_LEGACY } else { DRIVER_MIN };
    json!({"vendor": "nvidia", "name": name, "vram_mb": vram_mb, "driver": driver,
           "driver_old": major.is_some_and(|m| m < min), "compute_cap": cap, "legacy": legacy})
}

/// 這張 N 卡是不是「GTX 10 系列以前」（Maxwell、Pascal、Volta 以前，運算能力 7.5 以下不含）：ComfyUI 0.38 的 PyTorch cu130（CUDA 13）
/// 已經把這幾代拿掉，要改用 cu126 的舊卡版。有運算能力就看它（< 7.5＝舊卡），沒有（舊驅動、WMI）就看名字：
/// GTX 9xx／10xx 以前（GTX 16 系列是 Turing，不算）、GT 7xx／10xx、MX 1xx～3xx、GTX TITAN、TITAN X／Xp／V、
/// Quadro M／P／K／GP100／GV100 和舊數字型號、Tesla M／P／K／V、挖礦卡 P10x。認不出來一律當新卡（N 卡新卡的流程不能動）
pub fn nv_legacy(name: &str, cap: Option<&str>) -> bool {
    if let Some(c) = cap.and_then(parse_cap) {
        return c < (7, 5);
    }
    static RES: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            r"\bgtx\s*titan\b",
            r"\btitan\s*(?:x|xp|v|black|z)\b",
            r"\bquadro\s*(?:[mpk]\d|gp100|gv100|nvs|\d{3,4}\b)",
            r"\btesla\s*[mpkvc]\d",
            r"\b[pP]10\d-\d+",
            r"\b(?:v100|p100|p40|p4|m40|m60|m4|k80|k40|k20)\b",
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("N 卡舊型號的規則"))
        .collect()
    });
    let n = name.to_lowercase();
    if res.iter().any(|r| r.is_match(&n)) {
        return true;
    }
    // 數字型號：GTX／GT／GeForce／MX 後面的號碼
    let num = |pat: &str| -> Option<u32> {
        regex::Regex::new(pat).ok()?.captures(&n)?.get(1)?.as_str().parse().ok()
    };
    if let Some(v) = num(r"\b(?:gtx|gt)\b\s*(\d{3,4})") {
        return v < 1100; // 3 位數（4xx～9xx）和 10xx 是 Fermi／Kepler／Maxwell／Pascal；16xx（Turing）以後不算
    }
    if let Some(v) = num(r"\bgeforce\s+(\d{3,4})") {
        return v < 1100 || (8000..10000).contains(&v); // GeForce 940MX、840M、9800 GT
    }
    if let Some(v) = num(r"\bmx\s*(\d{3})") {
        return v < 400; // MX110～MX350 是 Maxwell／Pascal；MX450 是 Turing
    }
    false
}

/// WMI 列出現在插著的顯示卡（Win32_VideoController），顯存另外從顯示卡類別的登錄檔讀：
/// AdapterRAM 是 32 位元，4GB 以上的卡都只顯示 4GB；驅動寫的 HardwareInformation.qwMemorySize（舊驅動是 MemorySize）才準。
/// AMD 驅動另外寫了 RadeonSoftwareVersion（Adrenalin 的版號，比 Windows 的驅動版號好認）
#[cfg_attr(not(windows), allow(dead_code))] // 只有 Windows 用（Linux 沒有這個機制）
const WMI_GPUS: &str = r#"$ErrorActionPreference='SilentlyContinue'
[Console]::OutputEncoding=[Text.Encoding]::UTF8
$cls='HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}'
$reg=@(Get-ChildItem $cls | ForEach-Object { $p=Get-ItemProperty $_.PSPath; if ($p.MatchingDeviceId) { [pscustomobject]@{id=[string]$p.MatchingDeviceId; desc=[string]$p.DriverDesc; qw=$p.'HardwareInformation.qwMemorySize'; mem=$p.'HardwareInformation.MemorySize'; radeon=[string]$p.RadeonSoftwareVersion} } })
$vc=@(Get-CimInstance Win32_VideoController | ForEach-Object { [pscustomobject]@{name=[string]$_.Name; pnp=[string]$_.PNPDeviceID; vendor=[string]$_.AdapterCompatibility; driver=[string]$_.DriverVersion; ram=$_.AdapterRAM} })
ConvertTo-Json -Compress -Depth 4 -InputObject @{vc=$vc; reg=$reg}"#;

/// 跑 WMI_GPUS（PowerShell 5.1，用 -EncodedCommand 免得引號被吃掉），最多等 20 秒。PowerShell 不能用、WMI 壞掉＝None
#[cfg(windows)]
pub(crate) fn wmi_json() -> Option<Value> {
    use base64::Engine;
    use std::io::Read;
    let script: Vec<u8> = WMI_GPUS.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let ps = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell"));
    let mut cmd = std::process::Command::new(ps);
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(base64::engine::general_purpose::STANDARD.encode(script))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    win::no_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t0 = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if t0.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let b = reader.join().ok()?;
    // 前面可能有 BOM 或空白
    let start = b.iter().position(|&c| c == b'{')?;
    let v: Value = serde_json::from_slice(&b[start..]).ok()?;
    v["vc"].is_array().then_some(v)
}
/// Linux 沒有 WMI：照 sysfs（/sys/bus/pci/devices 裡顯示類的裝置）組出一樣格式的結果，後面 gpus_from_wmi 照用。
/// 名字問 lspci（沒裝就用「廠商 顯示卡 (vvvv:dddd)」），顯存只有 amdgpu 有（mem_info_vram_total），驅動版號看 /sys/module
#[cfg(not(windows))]
pub(crate) fn wmi_json() -> Option<Value> {
    let rd = |p: &Path| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
    let hex = |s: Option<String>| s.map(|v| v.trim_start_matches("0x").to_lowercase());
    let mut vc = vec![];
    let mut reg = vec![];
    for e in std::fs::read_dir("/sys/bus/pci/devices").ok()?.flatten() {
        let d = e.path();
        // PCI class 0x03xxxx＝顯示控制器
        if !rd(&d.join("class")).is_some_and(|c| c.starts_with("0x03")) {
            continue;
        }
        let (Some(ven), Some(dev)) = (hex(rd(&d.join("vendor"))), hex(rd(&d.join("device")))) else { continue };
        let slot = e.file_name().to_string_lossy().to_string();
        let name = std::process::Command::new("lspci")
            .args(["-mm", "-s", &slot])
            .output()
            .ok()
            .and_then(|o| lspci_name(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|| {
                let brand = match ven.as_str() {
                    "10de" => "NVIDIA",
                    "1002" => "AMD Radeon",
                    "8086" => "Intel",
                    _ => "",
                };
                format!("{brand} 顯示卡 ({ven}:{dev})").trim().to_string()
            });
        let driver = std::fs::read_link(d.join("driver"))
            .ok()
            .and_then(|l| l.file_name().map(|n| n.to_string_lossy().to_string()))
            .map(|drv| rd(&Path::new("/sys/module").join(&drv).join("version")).map(|v| format!("{drv} {v}")).unwrap_or(drv))
            .unwrap_or_default();
        let pnp = format!("PCI\\VEN_{}&DEV_{}", ven.to_uppercase(), dev.to_uppercase());
        vc.push(json!({"name": name, "pnp": pnp, "vendor": "", "driver": driver, "ram": null}));
        if let Some(bytes) = rd(&d.join("mem_info_vram_total")).and_then(|s| s.parse::<u64>().ok()) {
            reg.push(json!({"id": format!("pci\\ven_{ven}&dev_{dev}"), "desc": "", "qw": bytes, "mem": null, "radeon": ""}));
        }
    }
    Some(json!({"vc": vc, "reg": reg}))
}

/// `lspci -mm -s <slot>` 的一行（slot "class" "vendor" "device" …）→「vendor device」，例如 "Advanced Micro Devices, Inc. [AMD/ATI] Navi 31 [Radeon RX 7900 XT/7900 XTX]"
#[cfg(not(windows))]
fn lspci_name(line: &str) -> Option<String> {
    let f: Vec<&str> = line.split('"').collect();
    // f = [slot, class, " ", vendor, " ", device, ...]
    let (vendor, device) = (f.get(3)?.trim(), f.get(5)?.trim());
    (!device.is_empty()).then(|| format!("{vendor} {device}"))
}

/// 顯示卡是哪一家：PCI 的廠商代號（VEN_10DE＝NVIDIA、VEN_1002＝AMD、VEN_8086＝Intel），沒有再看名字。
/// 遠端桌面、向日葵、Parsec 這類虛擬顯示卡回傳 None（不算）
fn gpu_vendor(pnp: &str, name: &str, compat: &str) -> Option<&'static str> {
    let p = pnp.to_uppercase();
    let s = format!("{name} {compat}").to_lowercase();
    if p.contains("VEN_10DE") || s.contains("nvidia") {
        Some("nvidia")
    } else if p.contains("VEN_1002") || s.contains("radeon") || s.contains("advanced micro devices") {
        Some("amd")
    } else if p.contains("VEN_8086") || s.contains("intel") {
        Some("intel")
    } else {
        None
    }
}

/// 登錄檔的 MatchingDeviceId（pci\ven_1002&dev_744c&rev_c8）是不是這張卡（PNPDeviceID＝PCI\VEN_1002&DEV_744C&SUBSYS_…&REV_C8\4&…）：
/// 每一段都要在 PNPDeviceID 的硬體代號裡（順序不管，SUBSYS 可以多出來）
fn same_device(pnp: &str, id: &str) -> bool {
    let (pnp, id) = (pnp.to_lowercase(), id.to_lowercase());
    let (Some((bus, hw)), Some((bus2, want))) = (pnp.split_once('\\'), id.split_once('\\')) else {
        return false;
    };
    let hw = hw.split('\\').next().unwrap_or("");
    bus == bus2 && !want.is_empty() && want.split('&').all(|t| hw.split('&').any(|h| h == t))
}

/// 登錄檔裡的顯存（位元組）：qwMemorySize 是 QWORD；舊驅動的 MemorySize 可能是 DWORD 或 REG_BINARY（PowerShell 給的是位元組陣列，小端序）
fn reg_vram(r: &Value) -> Option<u64> {
    let num = |x: &Value| -> Option<u64> {
        match x {
            // PowerShell 把 REG_DWORD 當有號的 Int32：2GB 以上會變負數，要照 32 位元無號數讀回來（不然會變成幾十 TB）
            Value::Number(n) => n.as_u64().or_else(|| n.as_i64().filter(|&i| i >= i32::MIN as i64).map(|i| i as i32 as u32 as u64)),
            Value::Array(a) if !a.is_empty() && a.len() <= 8 => {
                Some(a.iter().rev().fold(0u64, |acc, b| acc << 8 | (b.as_u64().unwrap_or(0) & 0xff)))
            }
            _ => None,
        }
    };
    num(&r["qw"]).or_else(|| num(&r["mem"])).filter(|&b| b > 0)
}

/// PNPDeviceID 裡的裝置代號（PCI\VEN_1002&DEV_73DF&… → "73df"）
fn pci_dev(pnp: &str) -> Option<String> {
    let p = pnp.to_lowercase();
    let i = p.find("dev_")? + 4;
    let d = p.get(i..i + 4)?;
    d.chars().all(|c| c.is_ascii_hexdigit()).then(|| d.to_string())
}

/// WMI 的結果 {"vc": [{name, pnp, vendor, driver, ram}], "reg": [{id, desc, qw, mem, radeon}]} → 顯示卡清單（只留 NVIDIA、AMD、Intel）
pub(crate) fn gpus_from_wmi(v: &Value) -> Vec<Value> {
    let regs: Vec<&Value> = v["reg"].as_array().into_iter().flatten().collect();
    v["vc"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let s = |k: &str| c[k].as_str().unwrap_or("").trim().to_string();
            let (name, pnp) = (s("name"), s("pnp"));
            let vendor = gpu_vendor(&pnp, &name, &s("vendor"))?;
            let mine: Vec<&&Value> = regs.iter().filter(|r| same_device(&pnp, r["id"].as_str().unwrap_or(""))).collect();
            // AdapterRAM 頂到 4GB（0xFFF00000 附近）的不準，當作不知道
            let ram = c["ram"].as_u64().filter(|&b| b > 0 && b < 0xFFF0_0000);
            let bytes = mine.iter().find_map(|r| reg_vram(r)).or(ram);
            let radeon = mine.iter().find_map(|r| r["radeon"].as_str().filter(|x| !x.is_empty()));
            let mut g = json!({"vendor": vendor, "name": name, "vram_mb": bytes.map(|b| (b >> 20) as i64),
                               "driver": radeon.map(str::to_string).unwrap_or_else(|| s("driver"))});
            // 沒裝驅動時 Windows 用「Microsoft 基本顯示卡」頂著（PCI 代號還是真的那張卡）；N 卡能走到這裡＝nvidia-smi 看不到它
            if vendor == "nvidia" || name.to_lowercase().contains("basic display") {
                g["driver_missing"] = json!(true);
            }
            match vendor {
                "nvidia" => g["legacy"] = json!(nv_legacy(&name, None)),
                "amd" => {
                    let gfx = amd_gfx(pci_dev(&pnp).as_deref(), &name);
                    g["gfx"] = json!(gfx);
                    g["route"] = json!(amd_route(gfx, &name, pci_dev(&pnp).as_deref()));
                }
                "intel" => g["arc"] = json!(intel_arc(&name)),
                _ => {}
            }
            Some(g)
        })
        .collect()
}

/// Windows 的組建編號（22000 以上是 Windows 11）：AMD、Intel 版 ComfyUI 官方只寫 Windows 11
#[cfg(windows)]
pub fn windows_build() -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key, name) = (w(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"), w("CurrentBuildNumber"));
    let mut buf = [0u16; 32];
    let mut n = std::mem::size_of_val(&buf) as u32;
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut n)
    };
    if r != 0 {
        return None;
    }
    String::from_utf16_lossy(&buf).trim_end_matches('\0').trim().parse().ok()
}
#[cfg(not(windows))]
pub fn windows_build() -> Option<u32> {
    None
}

// ---------------------------------------------------------------- A 卡的架構
/// PCI 裝置代號 → gfx。代號和晶片名稱查自 pciutils 的 pci.ids（2026-10-21），晶片 → gfx 照 LLVM AMDGPUUsage 的處理器表。
/// RDNA1／2 是整段範圍（Linux amdgpu 驅動也這樣分），RDNA3 以後一個一個列（73f0 雖然在 73xx，是 Navi 33）
const AMD_DEVS: &[(u16, u16, &str)] = &[
    (0x7310, 0x731f, "gfx1010"), // Navi 10：RX 5700／5600、PRO W5700
    (0x7360, 0x7362, "gfx1011"), // Navi 12：Pro 5600M、V520
    (0x7340, 0x734f, "gfx1012"), // Navi 14：RX 5500、PRO W5500／W5300
    (0x73f0, 0x73f0, "gfx1102"), // Navi 33：RX 7600M XT
    (0x73a0, 0x73bf, "gfx1030"), // Navi 21：RX 6800／6900／6950、PRO W6800、V620
    (0x73c0, 0x73df, "gfx1031"), // Navi 22：RX 6700／6750、6800M
    (0x73e0, 0x73ff, "gfx1032"), // Navi 23：RX 6600／6650、PRO W6600
    (0x163f, 0x163f, "gfx1033"), // Van Gogh：Steam Deck
    (0x7420, 0x743f, "gfx1034"), // Navi 24：RX 6500／6400／6300、PRO W6400
    (0x164d, 0x164d, "gfx1035"), // Rembrandt
    (0x1681, 0x1681, "gfx1035"), // Rembrandt：680M／660M
    (0x164e, 0x164e, "gfx1036"), // Raphael：Ryzen 7000 桌機內顯 610M
    (0x13c0, 0x13c0, "gfx1036"), // Granite Ridge：Ryzen 9000 桌機內顯 610M
    (0x1506, 0x1506, "gfx1037"), // Mendocino 610M（沒有 ROCm 套件）
    (0x7448, 0x745f, "gfx1100"), // Navi 31：RX 7900、PRO W7900／W7800
    (0x7460, 0x747f, "gfx1101"), // Navi 32：RX 7800／7700、PRO W7700、V710
    (0x7480, 0x749f, "gfx1102"), // Navi 33：RX 7600／7500／7400、PRO W7600／W7500
    (0x15bf, 0x15bf, "gfx1103"), // Phoenix1：780M／760M
    (0x15c8, 0x15c8, "gfx1103"), // Phoenix2：740M
    (0x164f, 0x164f, "gfx1103"), // Phoenix
    (0x1900, 0x1901, "gfx1103"), // Hawk Point：780M／760M／740M
    (0x150e, 0x150e, "gfx1150"), // Strix：890M／880M（Ryzen AI 300）
    (0x1586, 0x1586, "gfx1151"), // Strix Halo：8060S／8050S（Ryzen AI Max）
    (0x1114, 0x1114, "gfx1152"), // Krackan：860M／840M
    (0x1902, 0x1902, "gfx1152,gfx1153"), // Krackan2：820M／840M（gfx1152 還是 1153 沒查到，兩個都裝，約多 180MB）
    (0x7590, 0x759f, "gfx1200"), // Navi 44：RX 9060／9050
    (0x7550, 0x755f, "gfx1201"), // Navi 48：RX 9070、AI PRO R9700
];

/// PCI 代號查不到時看名字（桌機卡的名字比較固定；筆電、內顯靠 PCI 代號）
const AMD_NAMES: &[(&str, &str)] = &[
    (r"rx\s*9070|r9700", "gfx1201"),
    (r"rx\s*90[56]0", "gfx1200"),
    (r"rx\s*79\d0|\bw7[89]00\b", "gfx1100"),
    (r"rx\s*7[78]00|\bw7700\b|\bv710\b", "gfx1101"),
    (r"rx\s*7[3-6][05]0|\bw7[4-6]00\b", "gfx1102"),
    (r"\b(780|760|740)m\b", "gfx1103"),
    (r"\b(890|880)m\b", "gfx1150"),
    (r"\b80[4-6]0s\b", "gfx1151"),
    (r"\b(860|840|820)m\b", "gfx1152,gfx1153"),
    (r"rx\s*6[89]\d0|\bw6800\b|\bv620\b", "gfx1030"),
    (r"rx\s*67\d0", "gfx1031"),
    (r"rx\s*66\d0|\bw6600\b", "gfx1032"),
    (r"rx\s*6[3-5]\d0|\bw6[34]00\b", "gfx1034"),
    (r"\b(680|660)m\b", "gfx1035"),
    (r"\b610m\b", "gfx1036"),
    (r"rx\s*5[67]\d0|\bw5700\b", "gfx1010"),
    (r"rx\s*5[3-5]\d0|\bw5[35]00\b", "gfx1012"),
];

/// A 卡的架構："gfx1031"，或不確定時好幾個（"gfx1152,gfx1153"）。PCI 代號為主、名字為輔；都對不出來＝None
pub fn amd_gfx(dev: Option<&str>, name: &str) -> Option<&'static str> {
    if let Some(d) = dev.and_then(|d| u16::from_str_radix(d, 16).ok()) {
        if let Some((_, _, g)) = AMD_DEVS.iter().find(|(a, b, _)| (*a..=*b).contains(&d)) {
            return Some(g);
        }
    }
    let n = name.to_lowercase();
    AMD_NAMES.iter().find(|(p, _)| regex::Regex::new(p).is_ok_and(|r| r.is_match(&n))).map(|(_, g)| *g)
}

/// 官方 AMD 免安裝版（PyTorch 2.9.1＋ROCm 7.2.1）本身就有的架構（2026-10-21 列壓縮檔內容查到）
pub const PORTABLE_GFX: &[&str] = &["gfx1100", "gfx1101", "gfx1102", "gfx1150", "gfx1151", "gfx1200", "gfx1201"];
/// AMD 官方 ROCm 10 多架構套件庫（stable.repo.amd.com/rocm/whl-next）有 torch device 套件的消費級架構（2026-10-21 查）
pub const ROCM10_GFX: &[&str] = &[
    "gfx1010", "gfx1011", "gfx1012", "gfx1030", "gfx1031", "gfx1032", "gfx1033", "gfx1034", "gfx1035", "gfx1036", "gfx1100", "gfx1101",
    "gfx1102", "gfx1103", "gfx1150", "gfx1151", "gfx1152", "gfx1153", "gfx1200", "gfx1201",
];

/// 這張 A 卡怎麼裝："portable"＝免安裝版就能用；"rocm10"＝免安裝版＋換 ROCm 10 PyTorch；
/// "old"＝太舊（Vega、RX 500 以前、Mendocino）沒有 ROCm 版 PyTorch；"unknown"＝認不出型號（先照免安裝版，第二頁可以手動選架構）
pub fn amd_route(gfx: Option<&str>, name: &str, dev: Option<&str>) -> &'static str {
    match gfx {
        Some(g) if g.split(',').all(|x| PORTABLE_GFX.contains(&x)) => "portable",
        Some(g) if g.split(',').all(|x| ROCM10_GFX.contains(&x)) => "rocm10",
        Some(_) => "old",
        None => {
            let n = name.to_lowercase();
            // GCN、Vega 世代：RX 400／500、Vega、Radeon VII、R5／R7／R9、HD、FirePro，和 Raven／Renoir／Cezanne 這些 Vega 內顯
            const OLD_DEVS: &[(u16, u16)] = &[(0x6600, 0x6fff), (0x15d8, 0x15dd), (0x1636, 0x1638), (0x15e7, 0x15e7), (0x164c, 0x164c), (0x9800, 0x98ff)];
            let old_dev = dev.and_then(|d| u16::from_str_radix(d, 16).ok()).is_some_and(|d| OLD_DEVS.iter().any(|(a, b)| (*a..=*b).contains(&d)));
            let old_name = regex::Regex::new(r"\bvega\b|rx\s*[45]\d0\b|\br[579]\s|\bhd\s*\d{4}|firepro|radeon\s*vii|\bwx\s*\d")
                .is_ok_and(|r| r.is_match(&n));
            if old_dev || old_name {
                "old"
            } else {
                "unknown"
            }
        }
    }
}

/// 精靈第二頁「A 卡架構」手動選單：（gfx, 說明）
pub const AMD_GFX_CHOICES: &[(&str, &str)] = &[
    ("gfx1201", "RX 9070 系列、AI PRO R9700"),
    ("gfx1200", "RX 9060／9050 系列"),
    ("gfx1100", "RX 7900 系列、PRO W7900／W7800"),
    ("gfx1101", "RX 7800／7700 系列、PRO W7700"),
    ("gfx1102", "RX 7600／7500／7400 系列、PRO W7600／W7500"),
    ("gfx1103", "780M／760M／740M 內顯"),
    ("gfx1150", "890M／880M 內顯（Ryzen AI 300）"),
    ("gfx1151", "8060S／8050S 內顯（Ryzen AI Max）"),
    ("gfx1152,gfx1153", "860M／840M／820M 內顯"),
    ("gfx1030", "RX 6800／6900／6950 系列、PRO W6800"),
    ("gfx1031", "RX 6700／6750 系列"),
    ("gfx1032", "RX 6600／6650 系列、PRO W6600"),
    ("gfx1034", "RX 6500／6400 系列"),
    ("gfx1035", "680M／660M 內顯"),
    ("gfx1036", "610M 內顯（Ryzen 7000／9000 桌機）"),
    ("gfx1033", "Steam Deck"),
    ("gfx1010", "RX 5700／5600 系列"),
    ("gfx1012", "RX 5500 系列"),
    ("gfx1011", "Radeon Pro V520／5600M"),
];

/// ROCm 10 PyTorch 要下載多少（2026-10-21 實測：gfx1031 那組 pip download 共 1,118,840,273 位元組，
/// 其中跟架構無關的 1,021,729,915；各架構的 device 套件用 HEAD 查大小）
pub fn rocm10_bytes(devices: &[String]) -> u64 {
    if gfx103_family(devices) {
        return GFX103_BYTES;
    }
    const COMMON: u64 = 1_021_729_915;
    let one = |g: &str| -> u64 {
        match g {
            "gfx1010" | "gfx1011" | "gfx1012" => 92_000_000,
            "gfx1030" | "gfx1031" | "gfx1032" | "gfx1033" | "gfx1034" | "gfx1035" | "gfx1036" => 97_100_000,
            "gfx1100" | "gfx1101" | "gfx1102" => 240_000_000,
            "gfx1103" => 106_400_000,
            "gfx1150" | "gfx1151" | "gfx1152" | "gfx1153" => 188_000_000,
            "gfx1200" => 428_900_000,
            "gfx1201" => 363_500_000,
            _ => 200_000_000,
        }
    };
    COMMON + devices.iter().map(|d| one(d)).sum::<u64>()
}

/// AMD 官方 ROCm 10 多架構套件庫（pip 的 --index-url；依賴的 numpy、sympy… 也鏡像在裡面）
pub const ROCM10_INDEX: &str = "https://stable.repo.amd.com/rocm/whl-next/";

/// pip 要裝的套件（2026-10-21 在這台用 AMD 免安裝版的 python_embeded 實裝過 gfx1031 這組：import torch、ComfyUI 自測都過）。
/// 好幾個架構時一起裝：torch[device-gfx1152,device-gfx1153]
pub fn rocm10_specs(devices: &[String]) -> Vec<String> {
    let ex = devices.iter().map(|d| format!("device-{d}")).collect::<Vec<_>>().join(",");
    vec![
        format!("torch[{ex}]==2.13.0+rocm10.0.0"),
        format!("torchvision[{ex}]==0.28.0+rocm10.0.0"),
        "torchaudio==2.11.0.2+rocm10.0.0".to_string(),
    ]
}

/// 這份 ComfyUI 要不要換成 ROCm 10 PyTorch：只動官方 AMD 免安裝版（外層有 run_amd_gpu.bat），而且 PyTorch 是
/// 它原本附的 ROCm 7.2、或之前生圖站換的 ROCm 10（老手自己換過的別種 ROCm 版不碰），計畫要的架構還沒裝齊才換。
/// PyTorch 不見了（換到一半被打斷）也算，再按一次安裝就補回來。使用者選「用我自己的 ComfyUI」就不動
pub fn rocm10_needed(layout: &ComfyLayout, plan: &GpuPlan) -> bool {
    if plan.own || plan.rocm10.is_empty() || !layout.portable || !layout.cwd.join("run_amd_gpu.bat").is_file() {
        return false;
    }
    match torch_build(&layout.python) {
        None => true,
        Some(b) if b.starts_with("rocm7.2") || b.starts_with("rocm10") || b == format!("rocm{GFX103_ROCM}") => {
            if gfx103_family(&plan.rocm10) {
                return !gfx103_installed(&layout.python);
            }
            let have = rocm10_devices(&layout.python);
            !plan.rocm10.iter().all(|d| have.contains(d))
        }
        Some(_) => false,
    }
}

// ---------------------------------------------------------------- A 卡：RX 6000（gfx103x）專用套件
/// RX 6000 這一族（gfx1030～1036，含 680M／610M 內顯）改用 AMD 的 gfx103X 專用 nightly，不走多架構 ROCm 10。
/// 2026-11-05 回報：RX 6750 GRE（gfx1031）用多架構 ROCm 10.0.0，每個 kernel 都 hipErrorInvalidImage，補裝 device 套件也一樣。
/// 這組版本是 comfyui-rinsei（github.com/eviscerations/comfyui-rinsei）在 RX 6700 XT＋Windows 確認能跑 ComfyUI 的；
/// kernel 直接編在套件裡（不是 kpack），舊的 gfx103X-dgpu 庫 gfx1031 的 kernel 是壞的，所以用 gfx103X-all。
/// 每個都 --no-deps（不然 pip 會在幾個月的 nightly 裡來回找）。rocm-sdk-devel（1.8GB，編譯用）不裝
pub const GFX103_INDEX: &str = "https://rocm.nightlies.amd.com/v2-staging/gfx103X-all/";
pub const GFX103_ROCM: &str = "7.14.0a20260603";
/// 下面六個檔加起來（2026-11-05 用 HEAD 查）
pub const GFX103_BYTES: u64 = 1_127_216_670;

pub fn is_gfx103(g: &str) -> bool {
    g.starts_with("gfx103")
}

/// 這些架構都是 RX 6000 這一族：整組改裝 gfx103X 專用套件
pub fn gfx103_family(devices: &[String]) -> bool {
    !devices.is_empty() && devices.iter().all(|d| is_gfx103(d))
}

pub fn gfx103_specs() -> Vec<String> {
    let v = GFX103_ROCM;
    vec![
        format!("torch==2.10.0+rocm{v}"),
        // torchvision 0.27 配 torch 2.10 會 DLL 載入失敗（comfyui-rinsei 的紀錄），固定 0.25
        format!("torchvision==0.25.0+rocm{v}"),
        format!("torchaudio==2.11.0+rocm{v}"),
        format!("rocm=={v}"),
        format!("rocm-sdk-core=={v}"),
        format!("rocm-sdk-libraries-gfx103X-all=={v}"),
    ]
}

/// gfx103X 專用套件裝好了：torch 是這一版，函式庫套件在、RECORD 列的檔都在
pub fn gfx103_installed(python: &Path) -> bool {
    let Some(sp) = site_packages(python) else { return false };
    let want = format!("rocm{GFX103_ROCM}");
    torch_build(python).is_some_and(|b| b == want)
        && std::fs::read_dir(&sp)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .find(|n| n.starts_with("rocm_sdk_libraries_gfx103x_all-") && n.ends_with(".dist-info"))
            .is_some_and(|d| record_complete(&sp, &sp.join(d)))
}

/// 換成 gfx103X 專用套件之後要移掉的多架構 ROCm 10 套件（同一個 site-packages 裡兩套 ROCm 函式庫會互搶）：pip 的套件名稱
pub fn rocm10_leftovers(python: &Path) -> Vec<String> {
    let Some(sp) = site_packages(python) else { return vec![] };
    let mut v: Vec<String> = std::fs::read_dir(sp)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            let (name, _) = n.strip_suffix(".dist-info")?.split_once('-')?;
            // 多架構 ROCm 10 的 device／函式庫套件、免安裝版原本 ROCm 7.2 的函式庫套件；rocm-sdk-devel 我們不裝，版本對不上的留著只會亂
            let old = ["amd_torch_device_", "amd_torchvision_device_", "rocm_sdk_device_", "rocm_sdk_libraries", "rocm_sdk_devel"].iter().any(|p| name.starts_with(p))
                && name != "rocm_sdk_libraries_gfx103x_all";
            old.then(|| name.to_string())
        })
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- Intel
/// Intel 顯示卡能不能用 Intel 版 ComfyUI（PyTorch XPU 官方支援：Arc A／B 系列獨顯、Core Ultra 的 Arc 內顯；
/// UHD、Iris Xe 不在清單裡）。Windows 顯示的名字都有「Arc」
pub fn intel_arc(name: &str) -> bool {
    name.to_lowercase().contains("arc")
}

// ---------------------------------------------------------------- 該裝哪一包
/// 這台電腦的 ComfyUI 該怎麼裝
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GpuPlan {
    /// nvidia｜amd｜intel｜cpu｜unknown（偵測失敗又沒手動選：不能自動下載 ComfyUI）
    pub variant: &'static str,
    /// 使用者選「用我自己已經能跑的 ComfyUI」：不下載 ComfyUI、不檢查 PyTorch 版本
    pub own: bool,
    /// 照哪一張卡決定的（顯示用）
    pub card: Option<String>,
    /// A 卡的架構（"gfx1031"，不確定時 "gfx1152,gfx1153"）
    pub gfx: Option<String>,
    /// A 卡要另外用 pip 換成 ROCm 10 PyTorch 的 device（空＝不用換）
    pub rocm10: Vec<String>,
    /// 原因代號（網頁照這個說明）：nvidia｜amd_portable｜amd_rocm10｜amd_unknown｜amd_old｜intel_arc｜intel_old｜none｜failed｜manual
    pub why: &'static str,
    /// variant＝nvidia 時：要用的那張是 GTX 10 系列以前的舊卡，裝 cu126 的舊卡版 ComfyUI（新卡一律 false）
    pub legacy: bool,
}

impl GpuPlan {
    pub fn view(&self) -> Value {
        json!({"variant": self.variant, "own": self.own, "card": self.card, "gfx": self.gfx, "rocm10": self.rocm10,
               "rocm10_bytes": (!self.rocm10.is_empty()).then(|| rocm10_bytes(&self.rocm10)), "why": self.why, "legacy": self.legacy})
    }
}

fn amd_plan(card: Option<String>, gfx: Option<&str>, route: &str, why: &'static str) -> GpuPlan {
    let rocm10 = if route == "rocm10" { gfx.map(|g| g.split(',').map(str::to_string).collect()).unwrap_or_default() } else { vec![] };
    GpuPlan { variant: "amd", card, gfx: gfx.map(str::to_string), rocm10, why, ..Default::default() }
}

/// 自動：有 N 卡＝NVIDIA 版（照舊）；A 卡挑顯存最大、有路可走的那張；Intel Arc；都沒有＝CPU；偵測失敗＝unknown
pub fn auto_plan(gpus: &[Value], detected: bool) -> GpuPlan {
    let s = |g: &Value, k: &str| g[k].as_str().unwrap_or("").to_string();
    // 有 N 卡就是 NVIDIA 版。要用的那張：優先第一張新卡（跟以前一樣是清單第一張；ComfyUI 預設用最快的那張，混插時新卡才是它要用的）；
    // 全都是 GTX 10 系列以前的舊卡才裝 cu126 的舊卡版
    let nv: Vec<&Value> = gpus.iter().filter(|g| g["vendor"] == "nvidia").collect();
    if let Some(g) = nv.iter().find(|g| g["legacy"] != true).or(nv.first()) {
        return GpuPlan { variant: "nvidia", card: Some(s(g, "name")), why: "nvidia", legacy: g["legacy"] == true, ..Default::default() };
    }
    let rank = |r: &str| match r {
        "portable" => 0,
        "rocm10" => 1,
        _ => 2,
    };
    let vram = |g: &Value| g["vram_mb"].as_i64().unwrap_or(0);
    let best = gpus
        .iter()
        .filter(|g| g["vendor"] == "amd" && g["route"] != "old")
        .max_by_key(|g| (vram(g), -rank(g["route"].as_str().unwrap_or(""))));
    let arc = gpus.iter().filter(|g| g["vendor"] == "intel" && g["arc"] == true).max_by_key(|g| vram(g));
    // Ryzen 7000／9000 桌機一定有 A 卡內顯（610M）：插的是 Intel Arc 獨顯時要選顯存大的那張
    let best = best.filter(|a| arc.is_none_or(|i| vram(a) >= vram(i)));
    if let Some(g) = best {
        let route = g["route"].as_str().unwrap_or("unknown");
        let why = match route {
            "portable" => "amd_portable",
            "rocm10" => "amd_rocm10",
            _ => "amd_unknown",
        };
        return amd_plan(Some(s(g, "name")), g["gfx"].as_str(), route, why);
    }
    if let Some(g) = arc {
        return GpuPlan { variant: "intel", card: Some(s(g, "name")), why: "intel_arc", ..Default::default() };
    }
    let why = if !detected {
        "failed"
    } else if gpus.iter().any(|g| g["vendor"] == "amd") {
        "amd_old"
    } else if gpus.iter().any(|g| g["vendor"] == "intel") {
        "intel_old"
    } else {
        "none"
    };
    GpuPlan { variant: if detected { "cpu" } else { "unknown" }, why, ..Default::default() }
}

/// 照使用者的選擇（精靈第二頁）：gpu＝auto｜nvidia｜amd｜intel｜cpu｜own；amd_gfx＝手動選的 A 卡架構（空＝照偵測）
pub fn plan_for(gpu: &str, amd_gfx: &str, gpus: &[Value], detected: bool) -> GpuPlan {
    let auto = auto_plan(gpus, detected);
    let manual = |variant| GpuPlan { variant, card: auto.card.clone(), why: "manual", ..Default::default() };
    match gpu {
        "nvidia" => manual("nvidia"),
        "nvidia_legacy" => GpuPlan { legacy: true, ..manual("nvidia") },
        "intel" => manual("intel"),
        "cpu" => manual("cpu"),
        "own" => GpuPlan { own: true, ..auto },
        "amd" => {
            let picked = AMD_GFX_CHOICES.iter().find(|(g, _)| *g == amd_gfx).map(|(g, _)| *g);
            let card = gpus.iter().find(|g| g["vendor"] == "amd").and_then(|g| g["name"].as_str()).map(str::to_string);
            let gfx = picked.or_else(|| if auto.variant == "amd" { auto.gfx.as_deref().and_then(static_gfx) } else { None });
            match gfx {
                Some(g) => amd_plan(card, Some(g), amd_route(Some(g), "", None), "manual"),
                None => amd_plan(card, None, "unknown", "manual"),
            }
        }
        _ => auto,
    }
}

/// auto_plan 的 gfx（String）換回表裡的 &'static str
fn static_gfx(g: &str) -> Option<&'static str> {
    AMD_DEVS.iter().map(|x| x.2).chain(AMD_NAMES.iter().map(|x| x.1)).find(|x| *x == g)
}

/// 照 comfy.json 的選擇和這台的顯示卡
pub fn current_plan() -> GpuPlan {
    let p = prefs();
    let (gpus, ok) = detect(false);
    plan_for(&p.gpu, &p.amd_gfx, &gpus, ok)
}

// ---------------------------------------------------------------- 使用者的選擇、ComfyUI 啟動方式（config.json 旁邊的 comfy.json）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// 精靈第二頁「顯示卡／ComfyUI 版本」：auto｜nvidia｜amd｜intel｜cpu｜own
    pub gpu: String,
    /// 手動選的 A 卡架構（AMD_GFX_CHOICES 的代號；空＝照偵測）
    pub amd_gfx: String,
    /// ComfyUI 額外啟動參數（接在生圖站自己的參數後面）
    pub extra_args: String,
    /// 「我自己開 ComfyUI，生圖站只連過去」：生圖站不自己開 ComfyUI
    pub external: bool,
    /// 用 CPU 跑（沒有能用的顯示卡）：啟動時加 --cpu。精靈裝成 CPU 版時打開
    pub cpu: bool,
    /// 手動指定 ComfyUI 用的 python.exe（conda、放在 ComfyUI 外面的 venv、系統 Python 這類自動找不到的）；空＝自動找。
    /// 只對「現在設定的那份 ComfyUI」有效（精靈換成別的位置時會清掉）
    pub python: String,
    /// ComfyUI 的連接埠；0＝自動（8188，被別份 ComfyUI 占走時換一個空的，見 main.rs）。
    /// 「我自己開」又不是開在 8188 的（SwarmUI、改過 port 的整合包）填這個
    pub port: u16,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { gpu: "auto".into(), amd_gfx: String::new(), extra_args: String::new(), external: false, cpu: false, python: String::new(), port: 0 }
    }
}

/// 設定頁填的連接埠：空白、0＝自動；其他要是 1024～65535，而且不能是生圖站自己的
pub fn check_port(v: &Value) -> Result<u16, String> {
    let n = match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) if s.trim().is_empty() => Some(0),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| "ComfyUI 的連接埠要填數字（例如 8188），空白＝自動".to_string())?;
    let own: u64 = std::env::var("ANIMA_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8190);
    if n == 0 {
        Ok(0)
    } else if !(1024..=65535).contains(&n) {
        Err(format!("ComfyUI 的連接埠要在 1024～65535 之間（你填的是 {n}）"))
    } else if n == own {
        Err(format!("{n} 是生圖站自己用的連接埠，ComfyUI 不能用同一個"))
    } else {
        Ok(n as u16)
    }
}

const GPU_CHOICES: &[&str] = &["auto", "nvidia", "nvidia_legacy", "amd", "intel", "cpu", "own"];

static PREFS_FILE: OnceLock<PathBuf> = OnceLock::new();
static PREFS_LOCK: Mutex<()> = Mutex::new(());

/// 設定 comfy.json 的位置（Setup::new 呼叫；沒呼叫過就一律用預設值）
pub fn init(file: &Path) {
    let _ = PREFS_FILE.set(file.to_path_buf());
}

pub fn prefs() -> Prefs {
    PREFS_FILE
        .get()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

impl Prefs {
    /// 網頁送來的 {gpu?, amd_gfx?, extra_args?, external?, cpu?} 蓋在現在的值上，順便檢查
    pub fn merged(&self, body: &Value) -> Result<Prefs, String> {
        let mut p = self.clone();
        if let Some(g) = body["gpu"].as_str() {
            if !GPU_CHOICES.contains(&g) {
                return Err(format!("不認得的顯示卡選項：{g}"));
            }
            p.gpu = g.into();
        }
        if let Some(g) = body["amd_gfx"].as_str() {
            if !g.is_empty() && !AMD_GFX_CHOICES.iter().any(|c| c.0 == g) {
                return Err(format!("不認得的 A 卡架構：{g}"));
            }
            p.amd_gfx = g.into();
        }
        if let Some(a) = body["extra_args"].as_str() {
            extra_args(a)?;
            p.extra_args = a.trim().into();
        }
        if let Some(b) = body["external"].as_bool() {
            p.external = b;
        }
        if let Some(b) = body["cpu"].as_bool() {
            p.cpu = b;
        }
        if let Some(py) = body["python"].as_str() {
            p.python = check_python(py)?;
        }
        if !body["port"].is_null() {
            p.port = check_port(&body["port"])?;
        }
        Ok(p)
    }
}

/// 手動指定的 python.exe：空白＝不指定；有填就要是存在的檔案、檔名是 python.exe（網頁貼上的路徑可能帶引號）。回傳整理過的路徑
pub fn check_python(s: &str) -> Result<String, String> {
    let s = s.trim().trim_matches('"').trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    let p = Path::new(s);
    if !p.file_name().is_some_and(|n| crate::paths::is_python_name(&n.to_string_lossy())) {
        return Err(if cfg!(windows) {
            format!("Python 位置要選 python.exe 這個檔（你填的是 {s}）：conda 環境通常在 …\\envs\\環境名稱\\python.exe，venv 在 …\\Scripts\\python.exe")
        } else {
            format!("Python 位置要選 python（或 python3）這個檔（你填的是 {s}）：conda 環境通常在 …/envs/環境名稱/bin/python，venv 在 …/venv/bin/python")
        });
    }
    if !p.is_file() {
        return Err(format!("找不到這個 python.exe：{s}。請確認路徑對不對（可以用旁邊的「選檔案…」選）"));
    }
    Ok(s.to_string())
}

/// comfy.json 旁邊的另一個檔（還沒設定 comfy.json 的位置＝None）
pub fn prefs_sibling(name: &str) -> Option<PathBuf> {
    PREFS_FILE.get().map(|f| f.with_file_name(name))
}

pub fn save_prefs(p: &Prefs) -> Result<(), String> {
    let _g = PREFS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(f) = PREFS_FILE.get() else {
        return Err("還沒設定 comfy.json 的位置".into());
    };
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = f.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, f).map_err(|e| format!("存 {} 失敗：{e}", f.display()))
}

/// 生圖站自己管的參數，額外參數不能蓋掉（port 要跟生圖站連的一致；資料夾要跟圖庫、模型清單一致）
pub(crate) const MANAGED_ARGS: &[&str] = &["--port", "--base-directory", "--output-directory", "--input-directory"];

/// 「ComfyUI 額外啟動參數」拆成一個一個，生圖站自己管的參數不准放。
/// ComfyUI 用 Python 的 argparse，縮寫也認（--base-dir、--output-dir、--por 都會被當成完整的），所以開頭三個字以上對得上的一起擋
pub fn extra_args(s: &str) -> Result<Vec<String>, String> {
    let out = split_args(s)?;
    let managed = |a: &str| {
        let name = a.split('=').next().unwrap_or("");
        name.len() >= 5 && name.starts_with("--") && MANAGED_ARGS.iter().any(|m| m.starts_with(name))
    };
    if let Some(a) = out.iter().find(|a| managed(a)) {
        return Err(format!("{a} 由生圖站自己設定，不能放在額外參數裡"));
    }
    Ok(out)
}

/// 命令列拆成一個一個（空白分開；"…" 包起來的算一個，可以放有空白的路徑）
pub fn split_args(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut quoted, mut any) = (vec![], String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted {
        return Err("額外啟動參數的引號沒有成對".into());
    }
    if any {
        out.push(cur);
    }
    Ok(out)
}

// ---------------------------------------------------------------- 認得 ComfyUI 用的是哪一種 PyTorch
/// Python 旁邊的 Lib\site-packages（venv 的 python.exe 在 Scripts 裡，要再往上一層）
/// Linux：<環境>/bin/python 配 <環境>/lib/python3.X/site-packages（有好幾個就挑有 torch 的，再挑版本新的）
pub fn site_packages(python: &Path) -> Option<PathBuf> {
    let mut base = python.parent()?;
    if cfg!(windows) {
        if base.file_name().is_some_and(|n| n.eq_ignore_ascii_case("Scripts")) {
            base = base.parent()?;
        }
        return Some(base.join("Lib").join("site-packages"));
    }
    if base.file_name().is_some_and(|n| n == "bin") {
        base = base.parent()?;
    }
    let lib = base.join("lib");
    let minor = |p: &Path| -> Option<u32> { p.parent()?.file_name()?.to_str()?.strip_prefix("python3.")?.parse().ok() };
    let mut found: Vec<PathBuf> = std::fs::read_dir(&lib)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("python3"))
        .map(|e| e.path().join("site-packages"))
        .filter(|p| p.is_dir())
        .collect();
    found.sort_by_key(|p| (p.join("torch").is_dir(), minor(p)));
    found.pop().or_else(|| Some(lib.join("python3").join("site-packages")))
}

/// site-packages 裡 <名稱>-<版本>.dist-info 的版本（名稱用 pip 的寫法，- 和 _ 都認）
fn dist_version(sp: &Path, name: &str) -> Option<String> {
    let want = name.to_lowercase().replace('-', "_");
    std::fs::read_dir(sp).ok()?.flatten().find_map(|e| {
        let n = e.file_name().to_string_lossy().to_lowercase();
        let rest = n.strip_suffix(".dist-info")?;
        let (pkg, ver) = rest.split_once('-')?;
        (pkg.replace('-', "_") == want).then(|| ver.to_string())
    })
}

/// torch 版本「+」後面那段：cu130、rocm7.2.1、xpu…；沒有後綴（PyPI 的 CPU 版）＝""；沒裝 torch＝None
pub fn torch_build(python: &Path) -> Option<String> {
    let v = dist_version(&site_packages(python)?, "torch")?;
    Some(v.split_once('+').map(|(_, b)| b.to_string()).unwrap_or_default())
}

/// torch 版本「+」後面那段 → 種類
fn torch_kind_of(build: &str) -> &'static str {
    match build {
        b if b.starts_with("cu") => "cuda",
        b if b.starts_with("rocm") => "rocm",
        b if b.starts_with("xpu") => "xpu",
        "" | "cpu" => "cpu",
        _ => "other",
    }
}

/// 這份 ComfyUI 的 PyTorch 是給哪種顯示卡的："cuda"（NVIDIA）、"rocm"（AMD）、"xpu"（Intel）、"cpu"、"other"；找不到 torch＝None
pub fn torch_kind(python: &Path) -> Option<&'static str> {
    torch_build(python).map(|b| torch_kind_of(&b))
}

/// 已經裝好的 ROCm 10 架構套件（amd_torch_device_gfx1031 → "gfx1031"）。只看 dist-info 在不在不準：
/// 檔案不完整（torch\.kpack\torch_gfx1031.kpack 不見）時 PyTorch 一個 kernel 都跑不了（hipErrorInvalidImage）。
/// 所以 amd_torch_device 和它依賴的 rocm_sdk_device 都要在、RECORD 列的檔都要在，才算裝好
pub fn rocm10_devices(python: &Path) -> Vec<String> {
    let Some(sp) = site_packages(python) else { return vec![] };
    let names: Vec<String> = std::fs::read_dir(&sp).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect();
    let dist = |prefix: &str| names.iter().find(|n| n.starts_with(prefix) && n.ends_with(".dist-info")).cloned();
    names
        .iter()
        .filter_map(|n| n.strip_prefix("amd_torch_device_")?.split_once('-').map(|(g, _)| (g.to_string(), n)))
        .filter(|(g, torch_dev)| {
            torch_dev.ends_with(".dist-info")
                && record_complete(&sp, &sp.join(torch_dev))
                && dist(&format!("rocm_sdk_device_{g}-")).is_some_and(|d| record_complete(&sp, &sp.join(d)))
        })
        .map(|(g, _)| g)
        .collect()
}

/// dist-info 的 RECORD 列的檔（.pyc 和 dist-info 自己的檔除外）都在。沒有 RECORD＝當作完整（不是 pip 裝的）
fn record_complete(sp: &Path, dist_info: &Path) -> bool {
    let Ok(rec) = std::fs::read_to_string(dist_info.join("RECORD")) else { return true };
    rec.lines()
        .filter_map(|l| l.split(',').next())
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.ends_with(".pyc") && !p.contains(".dist-info/") && !p.starts_with(".."))
        .all(|p| sp.join(p).is_file())
}

/// 重裝這些架構的 device 套件（pip install --force-reinstall --no-deps）：版本照這份 Python 已經裝的 torch／torchvision／rocm-sdk-core。
/// torch 不是 ROCm 10 版＝None
pub fn rocm10_device_specs(python: &Path, devices: &[String]) -> Option<Vec<String>> {
    let sp = site_packages(python)?;
    let torch = dist_version(&sp, "torch").filter(|v| v.contains("+rocm10"))?;
    let sdk = dist_version(&sp, "rocm_sdk_core").or_else(|| torch.split_once("+rocm").map(|(_, r)| r.to_string()))?;
    let tv = dist_version(&sp, "torchvision").filter(|v| v.contains("+rocm10"));
    Some(
        devices
            .iter()
            .flat_map(|g| {
                [Some(format!("amd-torch-device-{g}=={torch}")), Some(format!("rocm-sdk-device-{g}=={sdk}")), tv.as_ref().map(|v| format!("amd-torchvision-device-{g}=={v}"))]
            })
            .flatten()
            .collect(),
    )
}

/// 開站時要補裝哪些 ROCm 10 架構：偵測到的 A 卡（auto_plan 挑的那張；認不出型號時照精靈手動選的）有、PyTorch 還沒裝好的。
/// 精靈手動選的架構跟偵測到的對不上（2026-11-05 回報：RX 6750 GRE 的人不確定當時選對沒）時，第二個值＝要改成的選擇（""＝照偵測）
pub fn rocm10_repair(gpus: &[Value], detected: bool, prefs: &Prefs, have: &[String]) -> (Vec<String>, Option<String>) {
    let auto = auto_plan(gpus, detected);
    let found: Vec<String> = if auto.variant == "amd" { auto.gfx.as_deref().unwrap_or("").split(',').filter(|g| !g.is_empty()).map(str::to_string).collect() } else { vec![] };
    let picked: Vec<&str> = prefs.amd_gfx.split(',').filter(|g| !g.is_empty()).collect();
    let fix = (prefs.gpu == "amd" && !found.is_empty() && !picked.is_empty() && !picked.iter().any(|p| found.iter().any(|f| f == p)))
        .then(|| AMD_GFX_CHOICES.iter().find(|(g, _)| *g == found.join(",")).map(|(g, _)| g.to_string()).unwrap_or_default());
    let want: Vec<String> = if found.is_empty() { picked.iter().map(|g| g.to_string()).collect() } else { found };
    (want.into_iter().filter(|g| ROCM10_GFX.contains(&g.as_str()) && !have.contains(g)).collect(), fix)
}

/// 有沒有裝 torch-directml（DirectML 版：任何 DirectX 12 顯示卡都能跑、很慢，ComfyUI 要加 --directml）。
/// 它把 torch 鎖在 2.4.1（PyPI 的 CPU 版，沒有版本後綴），所以 torch 看起來像 CPU 版
pub fn has_directml(python: &Path) -> bool {
    site_packages(python).is_some_and(|sp| sp.join("torch_directml").is_dir() || dist_version(&sp, "torch_directml").is_some())
}

/// ComfyUI 用的是哪一種：cuda｜rocm｜xpu｜cpu｜other，另外認得
/// "zluda"（torch 是 +cu，但被 ZLUDA 換成 A 卡用的）和 "directml"（torch 是 CPU 版＋torch-directml）。找不到 torch＝None
pub fn comfy_backend(layout: &ComfyLayout) -> Option<&'static str> {
    match torch_kind(&layout.python) {
        Some("cuda") if zluda_exe(layout).is_some() || zluda_patched(&layout.python) => Some("zluda"),
        Some("cpu" | "other") | None if has_directml(&layout.python) => Some("directml"),
        k => k,
    }
}

/// ZLUDA 版的 zluda.exe（patientx/ComfyUI-Zluda：<ComfyUI>\zluda\；python\＋zluda\ 放外層的整合包：外層\zluda\）。
/// 要用它包著 python 開（zluda.exe -- python main.py），直接開 python 的話 PyTorch 找不到顯示卡
pub fn zluda_exe(layout: &ComfyLayout) -> Option<PathBuf> {
    let outer = layout.root.parent().unwrap_or(&layout.root);
    [layout.root.join("zluda"), outer.join("zluda")]
        .into_iter()
        .map(|d| d.join("zluda.exe"))
        .find(|f| f.is_file())
}

/// PyTorch 的 CUDA 函式庫被 ZLUDA 換掉了（找不到 zluda.exe 也認得出來，例如秋葉整合包的啟動器自己帶 ZLUDA）：
/// 安裝腳本會把原本的 nvrtc64_112_0.dll 備份成 nvrtc_cuda.dll（原版 PyTorch 沒有這個檔），
/// 或 cublas64_11.dll 只有零點幾 MB（原版 cu118 約 88MB）
pub fn zluda_patched(python: &Path) -> bool {
    let Some(lib) = site_packages(python).map(|sp| sp.join("torch").join("lib")) else { return false };
    lib.join("nvrtc_cuda.dll").is_file()
        || std::fs::metadata(lib.join("cublas64_11.dll")).is_ok_and(|m| m.len() < 10 << 20)
}

// ---------------------------------------------------------------- A 卡：好幾張卡時挑 ComfyUI 要用的那張
/// 問 ROCm 版 PyTorch 看得到哪些卡、它本身編了哪些架構。印一行 ANIMA_HIP={"arch": [...], "dev": [{name, arch, mem}]}
const HIP_PROBE: &str = r#"import json, torch
c = torch.cuda
d = [{"name": p.name, "arch": getattr(p, "gcnArchName", ""), "mem": p.total_memory} for p in (c.get_device_properties(i) for i in range(c.device_count()))] if c.is_available() else []
try:
    a = c.get_arch_list()
except Exception:
    a = []
print("ANIMA_HIP=" + json.dumps({"arch": a, "dev": d}))"#;

/// ROCm 看到好幾張卡時要用哪一號（HIP_VISIBLE_DEVICES 的值）。probe＝HIP_PROBE 的結果，want＝精靈裝的那張卡的架構（"gfx1101"，可能是空的）。
/// Ryzen 7000／9000 桌機的 610M、筆電的 780M 這類內顯也會被 ROCm 列進來，常常排在 0 號；ComfyUI 預設用 0 號，
/// PyTorch 又沒編那個架構 → 第一個用到顯示卡的節點就 hipErrorInvalidKernelFile（invalid kernel file）。
/// 挑法：PyTorch 有編的架構（"gfx1100:xnack-" 只看冒號前）→ 精靈裝的那張優先 → 顯存最大。只有一張、或一張都不能用＝None（照原樣開）
pub fn pick_hip_device(probe: &Value, want: Option<&str>) -> Option<usize> {
    let base = |s: &str| s.split(':').next().unwrap_or("").trim().to_lowercase();
    let built: Vec<String> = probe["arch"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(base)).collect();
    let devs: Vec<(usize, String, u64)> = probe["dev"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, d)| (i, base(d["arch"].as_str().unwrap_or("")), d["mem"].as_u64().unwrap_or(0)))
        .collect();
    if devs.len() < 2 {
        return None;
    }
    let want: Vec<String> = want.unwrap_or("").split(',').map(base).filter(|g| !g.is_empty()).collect();
    devs.iter()
        .filter(|(_, a, _)| built.is_empty() || built.contains(a))
        .max_by_key(|(_, a, mem)| (want.contains(a), *mem))
        .map(|(i, _, _)| *i)
}

/// 跑 HIP_PROBE（最多等 2 分鐘：第一次 import torch 在傳統硬碟上很慢）。PyTorch 開不起來、逾時＝None
pub fn hip_probe(layout: &ComfyLayout) -> Option<Value> {
    use std::io::Read;
    let mut cmd = std::process::Command::new(&layout.python);
    cmd.args(["-s", "-c", HIP_PROBE])
        .current_dir(&layout.cwd)
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env_remove("HIP_VISIBLE_DEVICES")
        .env_remove("CUDA_VISIBLE_DEVICES")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    win::no_window(&mut cmd);
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t0 = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if t0.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let b = reader.join().ok()?;
    let text = String::from_utf8_lossy(&b);
    text.lines().find_map(|l| l.trim().strip_prefix("ANIMA_HIP=")).and_then(|j| serde_json::from_str(j).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WMI 偵測顯示卡（照真的 PowerShell 輸出格式做的假資料）：廠商、顯存（登錄檔 QWORD／REG_BINARY／AdapterRAM 4GB 上限）、
    /// 虛擬顯示卡不算、沒裝驅動、Adrenalin 版號、A 卡架構、要裝哪一家
    #[test]
    fn wmi_gpu_detection() {
        let v = json!({
            "vc": [
                {"name": "AMD Radeon RX 7900 XTX", "pnp": "PCI\\VEN_1002&DEV_744C&SUBSYS_471E1043&REV_C8\\6&2A5A6A0D&0&00000019",
                 "vendor": "Advanced Micro Devices, Inc.", "driver": "32.0.21025.1024", "ram": 4293918720u64},
                {"name": "AMD Radeon(TM) Graphics", "pnp": "PCI\\VEN_1002&DEV_164E&SUBSYS_88771043&REV_C1\\4&1B1D27F&0&0041",
                 "vendor": "Advanced Micro Devices, Inc.", "driver": "32.0.21025.1024", "ram": 536870912u64},
                {"name": "OrayIddDriver Device", "pnp": "ROOT\\DISPLAY\\0000", "vendor": "Oray", "driver": "1.0", "ram": 0},
                {"name": "Microsoft Remote Display Adapter", "pnp": "SWD\\REMOTEDISPLAYENUM\\1", "vendor": "", "driver": "10.0", "ram": null}
            ],
            "reg": [
                // 第一張的顯存：REG_QWORD；MatchingDeviceId 少了 SUBSYS（每一段都在就算同一張）
                {"id": "pci\\ven_1002&dev_744c&rev_c8", "desc": "AMD Radeon RX 7900 XTX", "qw": 25753026560u64, "mem": null, "radeon": "25.10.2"},
                // 內顯：舊式 REG_BINARY（512MB＝00 00 00 20）
                {"id": "pci\\ven_1002&dev_164e", "desc": "AMD Radeon(TM) Graphics", "qw": null, "mem": [0, 0, 0, 32], "radeon": ""},
                // 拔掉的舊卡留下的機碼：對不到任何一張
                {"id": "pci\\ven_10de&dev_2484", "desc": "NVIDIA GeForce RTX 3070", "qw": 8589934592u64, "mem": null, "radeon": ""}
            ]
        });
        let g = gpus_from_wmi(&v);
        assert_eq!(g.len(), 2, "{g:?}");
        assert_eq!(g[0]["vendor"], "amd");
        assert_eq!(g[0]["vram_mb"], 24560);
        assert_eq!(g[0]["driver"], "25.10.2");
        assert_eq!((g[0]["gfx"].as_str(), g[0]["route"].as_str()), (Some("gfx1100"), Some("portable")));
        assert!(g[0].get("driver_missing").is_none());
        assert_eq!(g[1]["vram_mb"], 512);
        assert_eq!(g[1]["driver"], "32.0.21025.1024");
        assert_eq!((g[1]["gfx"].as_str(), g[1]["route"].as_str()), (Some("gfx1036"), Some("rocm10")));
        let p = auto_plan(&g, true);
        assert_eq!((p.variant, p.why, p.gfx.as_deref()), ("amd", "amd_portable", Some("gfx1100")));
        assert!(p.rocm10.is_empty());
        // AdapterRAM 頂到 4GB、登錄檔又沒有：不知道（不要當成 4GB）
        let v2 = json!({"vc": [{"name": "AMD Radeon RX 6800", "pnp": "PCI\\VEN_1002&DEV_73BF&SUBSYS_0E3A1002&REV_C3\\1", "vendor": "", "driver": "31.0", "ram": 4293918720u64}], "reg": []});
        let g2 = gpus_from_wmi(&v2);
        assert!(g2[0]["vram_mb"].is_null());
        assert_eq!((g2[0]["gfx"].as_str(), g2[0]["route"].as_str()), (Some("gfx1030"), Some("rocm10")));
        // N 卡驅動沒裝好（nvidia-smi 看不到）＋A 卡內顯：還是 N 卡，不能改裝 AMD 版
        let v3 = json!({"vc": [
            {"name": "Microsoft Basic Display Adapter", "pnp": "PCI\\VEN_10DE&DEV_2684&SUBSYS_16F110DE&REV_A1\\4&1", "vendor": "(Standard display types)", "driver": "10.0", "ram": 0},
            {"name": "AMD Radeon(TM) Graphics", "pnp": "PCI\\VEN_1002&DEV_164E&SUBSYS_1&REV_C1\\4&2", "vendor": "Advanced Micro Devices, Inc.", "driver": "31.0", "ram": 536870912u64}
        ], "reg": []});
        let g3 = gpus_from_wmi(&v3);
        assert_eq!((g3[0]["vendor"].as_str(), g3[0]["driver_missing"].as_bool()), (Some("nvidia"), Some(true)));
        assert_eq!(auto_plan(&g3, true).variant, "nvidia");
        // A 卡沒裝驅動：PCI 代號還是認得出架構
        let v4 = json!({"vc": [{"name": "Microsoft Basic Display Adapter", "pnp": "PCI\\VEN_1002&DEV_7550&SUBSYS_1&REV_C0\\4&3", "vendor": "(Standard display types)", "driver": "10.0", "ram": 0}], "reg": []});
        let g4 = gpus_from_wmi(&v4);
        assert_eq!((g4[0]["vendor"].as_str(), g4[0]["driver_missing"].as_bool(), g4[0]["gfx"].as_str()), (Some("amd"), Some(true), Some("gfx1201")));
        assert!(gpus_from_wmi(&json!({})).is_empty());
    }

    /// 精靈要測的幾種電腦（主工作階段指定的）：每一種該裝什麼
    #[test]
    fn plans_for_common_machines() {
        let card = |name: &str, pnp: &str| {
            let v = json!({"vc": [{"name": name, "pnp": pnp, "vendor": "", "driver": "1.0", "ram": 0}], "reg": []});
            gpus_from_wmi(&v)
        };
        // RX 6700 XT：免安裝版沒有 gfx1031 → AMD 版＋換 ROCm 10 PyTorch
        let p = auto_plan(&card("AMD Radeon RX 6700 XT", "PCI\\VEN_1002&DEV_73DF&SUBSYS_1&REV_C1\\1"), true);
        assert_eq!((p.variant, p.why, p.rocm10.clone()), ("amd", "amd_rocm10", vec!["gfx1031".to_string()]));
        assert!(p.view()["rocm10_bytes"].as_u64().unwrap() > 1_000_000_000);
        // 780M 內顯
        let p = auto_plan(&card("AMD Radeon 780M Graphics", "PCI\\VEN_1002&DEV_15BF&SUBSYS_1&REV_C4\\1"), true);
        assert_eq!((p.why, p.rocm10.clone()), ("amd_rocm10", vec!["gfx1103".to_string()]));
        // RX 5700
        let p = auto_plan(&card("AMD Radeon RX 5700", "PCI\\VEN_1002&DEV_731F&SUBSYS_1&REV_C4\\1"), true);
        assert_eq!(p.rocm10, vec!["gfx1010".to_string()]);
        // Krackan2 不確定是 1152 還是 1153：兩個都裝
        let p = auto_plan(&card("AMD Radeon(TM) 840M Graphics", "PCI\\VEN_1002&DEV_1902&SUBSYS_1&REV_C1\\1"), true);
        assert_eq!(p.rocm10, vec!["gfx1152".to_string(), "gfx1153".to_string()]);
        // Intel Arc B580：Intel 版
        let p = auto_plan(&card("Intel(R) Arc(TM) B580 Graphics", "PCI\\VEN_8086&DEV_E20B&SUBSYS_1&REV_00\\1"), true);
        assert_eq!((p.variant, p.why), ("intel", "intel_arc"));
        // Ryzen 桌機內顯 610M（512MB）＋Arc A770 16GB：選 Arc；A 卡獨顯＋Intel 內顯 Arc：選 A 卡（顯存大）
        let mix = |a: (&str, &str, u64), b: (&str, &str, u64)| {
            let c = |(name, pnp, mb): (&str, &str, u64)| json!({"name": name, "pnp": pnp, "vendor": "", "driver": "1", "ram": mb << 20});
            gpus_from_wmi(&json!({"vc": [c(a), c(b)], "reg": []}))
        };
        let g = mix(("AMD Radeon(TM) Graphics", "PCI\\VEN_1002&DEV_164E&SUBSYS_1&REV_C1\\1", 512), ("Intel(R) Arc(TM) A770 Graphics", "PCI\\VEN_8086&DEV_56A0&SUBSYS_1&REV_08\\1", 3000));
        assert_eq!(auto_plan(&g, true).variant, "intel");
        let g = mix(("AMD Radeon RX 7800 XT", "PCI\\VEN_1002&DEV_747E&SUBSYS_1&REV_C8\\1", 3500), ("Intel(R) Arc(TM) Graphics", "PCI\\VEN_8086&DEV_7D55&SUBSYS_1&REV_08\\1", 2048));
        assert_eq!(auto_plan(&g, true).variant, "amd");
        // 只有 UHD：CPU，原因講 Intel 內顯不行
        let p = auto_plan(&card("Intel(R) UHD Graphics 770", "PCI\\VEN_8086&DEV_A780&SUBSYS_1&REV_04\\1"), true);
        assert_eq!((p.variant, p.why), ("cpu", "intel_old"));
        // RX 580（Polaris）：太舊
        let p = auto_plan(&card("Radeon RX 580 Series", "PCI\\VEN_1002&DEV_67DF&SUBSYS_1&REV_E7\\1"), true);
        assert_eq!((p.variant, p.why), ("cpu", "amd_old"));
        // 什麼都沒有、偵測失敗
        assert_eq!((auto_plan(&[], true).variant, auto_plan(&[], true).why), ("cpu", "none"));
        assert_eq!((auto_plan(&[], false).variant, auto_plan(&[], false).why), ("unknown", "failed"));
        // 認不出型號的新 A 卡：照免安裝版，原因 amd_unknown（第二頁可以手動選架構）
        let p = auto_plan(&card("AMD Radeon RX 9999 Future", "PCI\\VEN_1002&DEV_7FFF&SUBSYS_1&REV_00\\1"), true);
        assert_eq!((p.variant, p.why, p.rocm10.len()), ("amd", "amd_unknown", 0));
        // N 卡照舊（nvidia-smi 的格式）
        let n = vec![json!({"vendor": "nvidia", "name": "NVIDIA GeForce RTX 3070", "vram_mb": 8192})];
        assert_eq!(auto_plan(&n, true), GpuPlan { variant: "nvidia", card: Some("NVIDIA GeForce RTX 3070".into()), why: "nvidia", ..Default::default() });
    }

    /// 手動選：偵測失敗也能選；A 卡手動選架構；own 不下載
    #[test]
    fn manual_choices() {
        let none: Vec<Value> = vec![];
        assert_eq!(plan_for("nvidia", "", &none, false).variant, "nvidia");
        assert_eq!(plan_for("intel", "", &none, false).variant, "intel");
        assert_eq!(plan_for("cpu", "", &none, false).variant, "cpu");
        assert_eq!(plan_for("auto", "", &none, false).variant, "unknown");
        let p = plan_for("amd", "gfx1031", &none, false);
        assert_eq!((p.variant, p.rocm10.clone()), ("amd", vec!["gfx1031".to_string()]));
        let p = plan_for("amd", "gfx1201", &none, false);
        assert!(p.rocm10.is_empty());
        // A 卡、沒選架構、偵測失敗：照免安裝版
        let p = plan_for("amd", "", &none, false);
        assert_eq!((p.variant, p.gfx.clone(), p.rocm10.len()), ("amd", None, 0));
        // A 卡、沒選架構：照偵測到的
        let rx6600 = gpus_from_wmi(&json!({"vc": [{"name": "AMD Radeon RX 6600", "pnp": "PCI\\VEN_1002&DEV_73FF&SUBSYS_1&REV_C7\\1", "vendor": "", "driver": "1", "ram": 0}], "reg": []}));
        assert_eq!(plan_for("amd", "", &rx6600, true).rocm10, vec!["gfx1032".to_string()]);
        let own = plan_for("own", "", &rx6600, true);
        assert!(own.own && own.variant == "amd");
        assert_eq!(plan_for("whatever", "", &rx6600, true), auto_plan(&rx6600, true));
        // Prefs 的檢查
        let p = Prefs::default();
        assert!(p.merged(&json!({"gpu": "quantum"})).is_err());
        assert!(p.merged(&json!({"amd_gfx": "gfx9999"})).is_err());
        let m = p.merged(&json!({"gpu": "amd", "amd_gfx": "gfx1031", "extra_args": " --lowvram ", "cpu": true})).unwrap();
        assert_eq!((m.gpu.as_str(), m.amd_gfx.as_str(), m.extra_args.as_str(), m.cpu, m.external), ("amd", "gfx1031", "--lowvram", true, false));
    }

    #[test]
    fn extra_arg_parsing() {
        assert_eq!(extra_args("").unwrap(), Vec::<String>::new());
        assert_eq!(extra_args("  --lowvram   --use-quad-cross-attention ").unwrap(), vec!["--lowvram", "--use-quad-cross-attention"]);
        assert_eq!(extra_args(r#"--extra-model-paths-config "D:\my models\a.yaml""#).unwrap(), vec!["--extra-model-paths-config", r"D:\my models\a.yaml"]);
        assert_eq!(extra_args(r#"--x """#).unwrap(), vec!["--x", ""]);
        assert!(extra_args(r#"--x "abc"#).is_err());
        assert!(extra_args("--port 9000").unwrap_err().contains("--port"));
        assert!(extra_args("--output-directory=D:\\x").is_err());
        assert!(extra_args("--portable-thing").is_ok());
        // argparse 的縮寫
        assert!(extra_args("--base-dir D:\\x").is_err() && extra_args("--output-dir=D:\\y").is_err() && extra_args("--por 9000").is_err());
        assert!(extra_args("--lowvram --fast --cpu").is_ok());
    }

    #[test]
    fn wmi_helpers() {
        assert!(same_device("PCI\\VEN_1002&DEV_744C&SUBSYS_471E1043&REV_C8\\6&2A", "pci\\ven_1002&dev_744c"));
        assert!(same_device("PCI\\VEN_1002&DEV_744C&SUBSYS_471E1043&REV_C8\\6&2A", "PCI\\VEN_1002&DEV_744C&REV_C8"));
        assert!(!same_device("PCI\\VEN_1002&DEV_744C&SUBSYS_471E1043&REV_C8\\6&2A", "pci\\ven_1002&dev_744c&rev_c0"));
        assert!(!same_device("PCI\\VEN_1002&DEV_744C\\6&2A", "pci\\ven_1002&dev_73bf"));
        assert!(!same_device("ROOT\\DISPLAY\\0000", "pci\\ven_1002&dev_744c"));
        assert!(!same_device("PCI\\VEN_1002&DEV_744C\\6", ""));
        assert_eq!(reg_vram(&json!({"qw": 17163091968u64})), Some(17163091968));
        assert_eq!(reg_vram(&json!({"qw": null, "mem": 1073741824})), Some(1073741824));
        assert_eq!(reg_vram(&json!({"mem": [0, 0, 0, 64]})), Some(1 << 30));
        assert_eq!(reg_vram(&json!({"qw": 0})), None);
        // PowerShell 給的 REG_DWORD 是有號數：3GB（0xC0000000）會變成 -1073741824
        assert_eq!(reg_vram(&json!({"qw": null, "mem": -1073741824})), Some(3 << 30));
        assert_eq!(gpu_vendor("PCI\\VEN_8086&DEV_A780\\3", "Intel(R) UHD Graphics 770", "Intel Corporation"), Some("intel"));
        assert_eq!(gpu_vendor("ROOT\\DISPLAY\\0001", "Parsec Virtual Display Adapter", "Parsec Cloud, Inc."), None);
        assert_eq!(pci_dev("PCI\\VEN_1002&DEV_73DF&SUBSYS_1"), Some("73df".into()));
        assert_eq!(pci_dev("ROOT\\DISPLAY\\0000"), None);
        // 名字對照（沒有 PCI 代號時）
        for (name, want) in [
            ("AMD Radeon RX 9070 XT", Some("gfx1201")),
            ("AMD Radeon AI PRO R9700", Some("gfx1201")),
            ("AMD Radeon RX 9060 XT", Some("gfx1200")),
            ("AMD Radeon RX 7900 GRE", Some("gfx1100")),
            ("AMD Radeon PRO W7800", Some("gfx1100")),
            ("AMD Radeon RX 7800 XT", Some("gfx1101")),
            ("AMD Radeon RX 7600", Some("gfx1102")),
            ("AMD Radeon 780M Graphics", Some("gfx1103")),
            ("AMD Radeon(TM) 890M Graphics", Some("gfx1150")),
            ("AMD Radeon(TM) 8060S Graphics", Some("gfx1151")),
            ("AMD Radeon RX 6950 XT", Some("gfx1030")),
            ("AMD Radeon RX 6700 XT", Some("gfx1031")),
            ("AMD Radeon RX 6600", Some("gfx1032")),
            ("AMD Radeon RX 6500 XT", Some("gfx1034")),
            ("AMD Radeon 680M", Some("gfx1035")),
            ("AMD Radeon RX 5700 XT", Some("gfx1010")),
            ("AMD Radeon RX 5500 XT", Some("gfx1012")),
            ("AMD Radeon(TM) Graphics", None),
            ("Radeon RX 580 Series", None),
        ] {
            assert_eq!(amd_gfx(None, name), want, "{name}");
        }
        // PCI 代號優先（73f0 在 73xx 但是 Navi 33）
        assert_eq!(amd_gfx(Some("73f0"), "AMD Radeon RX 7600M XT"), Some("gfx1102"));
        assert_eq!(amd_gfx(Some("73ef"), ""), Some("gfx1032"));
        assert_eq!(amd_gfx(Some("1506"), ""), Some("gfx1037"));
        assert_eq!(amd_route(Some("gfx1037"), "", None), "old");
        assert_eq!(amd_route(None, "Radeon RX 580 Series", Some("67df")), "old");
        assert_eq!(amd_route(None, "AMD Radeon(TM) Vega 8 Graphics", Some("1638")), "old");
        assert_eq!(amd_route(None, "AMD Radeon Something New", Some("7fff")), "unknown");
        assert!(intel_arc("Intel(R) Arc(TM) A770 Graphics") && intel_arc("Intel(R) Arc(TM) 140V GPU (16GB)") && intel_arc("Intel(R) Arc(TM) Graphics"));
        assert!(!intel_arc("Intel(R) Iris(R) Xe Graphics") && !intel_arc("Intel(R) UHD Graphics 770"));
        // 選單的每個架構都有路
        for (g, _) in AMD_GFX_CHOICES {
            assert_ne!(amd_route(Some(g), "", None), "old", "{g}");
        }
    }

    /// PyTorch 是哪一種：免安裝版（python_embeded）、venv（Scripts 外一層）、沒裝 torch、DirectML、ZLUDA、ROCm 10 的架構套件
    #[cfg(windows)] // 測的是 Windows 的擺法（python.exe、Scripts、Lib\site-packages）；Linux 的在 linux_layouts
    #[test]
    fn torch_kinds() {
        let d = std::env::temp_dir().join(format!("anima_torch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mk = |py: &PathBuf, sp: PathBuf, dists: &[&str]| {
            std::fs::create_dir_all(py.parent().unwrap()).unwrap();
            std::fs::write(py, b"").unwrap();
            for x in dists.iter().chain(&["torchaudio-2.9.1+rocm7.2.1.dist-info", "torchsde-0.2.6.dist-info"]) {
                std::fs::create_dir_all(sp.join(x)).unwrap();
            }
        };
        let amd = d.join("amd").join("python_embeded").join("python.exe");
        let amd_sp = d.join("amd").join("python_embeded").join("Lib").join("site-packages");
        mk(&amd, amd_sp.clone(), &["torch-2.9.1+rocm7.2.1.dist-info"]);
        assert_eq!(torch_kind(&amd), Some("rocm"));
        assert_eq!(torch_build(&amd).as_deref(), Some("rocm7.2.1"));
        assert!(rocm10_devices(&amd).is_empty());
        std::fs::remove_dir_all(amd_sp.join("torch-2.9.1+rocm7.2.1.dist-info")).unwrap();
        for x in ["torch-2.13.0+rocm10.0.0.dist-info", "amd_torch_device_gfx1031-2.13.0+rocm10.0.0.dist-info"] {
            std::fs::create_dir_all(amd_sp.join(x)).unwrap();
        }
        assert_eq!(torch_build(&amd).as_deref(), Some("rocm10.0.0"));
        assert!(rocm10_devices(&amd).is_empty(), "依賴的 rocm_sdk_device 不在");
        std::fs::create_dir_all(amd_sp.join("rocm_sdk_device_gfx1031-10.0.0.dist-info")).unwrap();
        assert_eq!(rocm10_devices(&amd), vec!["gfx1031".to_string()]);
        // 2026-11-05 回報：dist-info 在、kpack 不見＝PyTorch 一個 kernel 都跑不了，不能算裝好
        let rec = amd_sp.join("amd_torch_device_gfx1031-2.13.0+rocm10.0.0.dist-info").join("RECORD");
        std::fs::write(&rec, "torch/.kpack/torch_gfx1031.kpack,sha256=x,45053513\namd_torch_device_gfx1031-2.13.0+rocm10.0.0.dist-info/RECORD,,\n").unwrap();
        assert!(rocm10_devices(&amd).is_empty(), "kpack 不見");
        std::fs::create_dir_all(amd_sp.join("torch").join(".kpack")).unwrap();
        std::fs::write(amd_sp.join("torch").join(".kpack").join("torch_gfx1031.kpack"), b"KPAK").unwrap();
        assert_eq!(rocm10_devices(&amd), vec!["gfx1031".to_string()]);
        std::fs::create_dir_all(amd_sp.join("torchvision-0.28.0+rocm10.0.0.dist-info")).unwrap();
        assert_eq!(
            rocm10_device_specs(&amd, &["gfx1031".into()]).unwrap(),
            ["amd-torch-device-gfx1031==2.13.0+rocm10.0.0", "rocm-sdk-device-gfx1031==10.0.0", "amd-torchvision-device-gfx1031==0.28.0+rocm10.0.0"]
        );
        std::fs::remove_dir_all(amd_sp.join("torchvision-0.28.0+rocm10.0.0.dist-info")).unwrap();
        // 要不要換 ROCm 10：官方 AMD 免安裝版（外層有 run_amd_gpu.bat）、架構還沒裝齊；老手自己換的別種 ROCm 版不碰；torch 不見了要補
        let portable = ComfyLayout {
            root: d.join("amd").join("ComfyUI"),
            main: d.join("amd").join("ComfyUI").join("main.py"),
            python: amd.clone(),
            portable: true,
            desktop: false,
            cwd: d.join("amd"),
        };
        let plan = |g: &str| plan_for("amd", g, &[], true);
        assert!(!rocm10_needed(&portable, &plan("gfx1031")), "沒有 run_amd_gpu.bat 不是官方 AMD 版");
        std::fs::write(d.join("amd").join("run_amd_gpu.bat"), b"").unwrap();
        assert!(rocm10_needed(&portable, &plan("gfx1031")), "多架構的 gfx1031 裝了也要換成 RX 6000 專用版（gfx103_stack 測詳細的）");
        assert!(rocm10_needed(&portable, &plan("gfx1010")));
        assert!(!rocm10_needed(&portable, &plan("gfx1100")), "免安裝版本身就有的不用換");
        assert!(!rocm10_needed(&portable, &plan_for("own", "", &[], true)));
        std::fs::remove_dir_all(amd_sp.join("torch-2.13.0+rocm10.0.0.dist-info")).unwrap();
        assert!(rocm10_needed(&portable, &plan("gfx1031")), "torch 不見了（換到一半被打斷）要補回來");
        std::fs::create_dir_all(amd_sp.join("torch-2.7.0a0+rocm7.0.0rc20250101.dist-info")).unwrap();
        assert!(!rocm10_needed(&portable, &plan("gfx1010")), "老手自己換的 ROCm 版不碰");
        std::fs::remove_dir_all(amd_sp.join("torch-2.7.0a0+rocm7.0.0rc20250101.dist-info")).unwrap();
        let nv = d.join("nv").join("venv").join("Scripts").join("python.exe");
        mk(&nv, d.join("nv").join("venv").join("Lib").join("site-packages"), &["torch-2.11.0+cu130.dist-info"]);
        assert_eq!(torch_kind(&nv), Some("cuda"));
        let cpu = d.join("cpu").join("python").join("python.exe");
        mk(&cpu, d.join("cpu").join("python").join("Lib").join("site-packages"), &["torch-2.9.1.dist-info"]);
        assert_eq!(torch_kind(&cpu), Some("cpu"));
        assert_eq!(torch_kind(&d.join("none").join("python.exe")), None);
        assert_eq!(torch_kind_of("xpu"), "xpu");
        assert_eq!(torch_kind_of("git1234"), "other");
        // DirectML：torch 是 CPU 版＋torch_directml
        let dml = d.join("dml").join("venv").join("Scripts").join("python.exe");
        mk(&dml, d.join("dml").join("venv").join("Lib").join("site-packages"), &["torch-2.4.1.dist-info", "torch_directml-0.2.5.dev240914.dist-info"]);
        assert!(has_directml(&dml));
        assert!(!has_directml(&nv));
        let layout = |root: PathBuf, python: PathBuf| ComfyLayout { main: root.join("main.py"), cwd: root.clone(), root, python, portable: false, desktop: false };
        assert_eq!(comfy_backend(&layout(d.join("dml"), dml.clone())), Some("directml"));
        // ZLUDA：torch 是 +cu118。torch\lib 被換過（cublas64_11.dll 只有零點幾 MB）或有 zluda.exe 就認得；原版 cu118 還是 NVIDIA 版
        let zroot = d.join("zl");
        let zpy = zroot.join("venv").join("Scripts").join("python.exe");
        let zlib = zroot.join("venv").join("Lib").join("site-packages").join("torch").join("lib");
        mk(&zpy, zroot.join("venv").join("Lib").join("site-packages"), &["torch-2.7.0+cu118.dist-info"]);
        std::fs::create_dir_all(&zlib).unwrap();
        std::fs::File::create(zlib.join("cublas64_11.dll")).unwrap().set_len(20 << 20).unwrap();
        let zl = layout(zroot.clone(), zpy.clone());
        assert!(!zluda_patched(&zpy));
        assert_eq!(comfy_backend(&zl), Some("cuda"));
        std::fs::write(zlib.join("cublas64_11.dll"), vec![0u8; 250_000]).unwrap();
        assert!(zluda_patched(&zpy));
        assert_eq!((comfy_backend(&zl), zluda_exe(&zl)), (Some("zluda"), None));
        std::fs::remove_file(zlib.join("cublas64_11.dll")).unwrap();
        std::fs::create_dir_all(zroot.join("zluda")).unwrap();
        std::fs::write(zroot.join("zluda").join("zluda.exe"), b"").unwrap();
        assert_eq!((comfy_backend(&zl), zluda_exe(&zl)), (Some("zluda"), Some(zroot.join("zluda").join("zluda.exe"))));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 真的跑一次 WMI（PowerShell）看這台電腦偵測到什麼：cargo test --lib wmi_real -- --ignored --nocapture
    /// （A 卡、Intel 電腦上拿來確認型號、顯存、架構判斷對不對）
    #[test]
    #[ignore]
    fn wmi_real() {
        let raw = wmi_json().expect("PowerShell／WMI 沒有回應");
        println!("WMI 原始結果：{raw}");
        let g = gpus_from_wmi(&raw);
        println!("顯示卡：{g:?}\n自動：{:?}　Windows 組建：{:?}", auto_plan(&g, true), windows_build());
        assert!(raw["vc"].is_array());
    }

    /// A 卡好幾張：內顯排 0 號、PyTorch 沒編它的架構 → 挑獨顯（2026-11-04 回報 invalid kernel file）
    /// 2026-11-05：RX 6000（gfx103x）改裝 gfx103X 專用 PyTorch；裝好的判斷、要移掉的舊 ROCm 套件、要不要換
    #[cfg(windows)] // 測的是 Windows 的擺法（python.exe、Scripts、Lib\site-packages）；Linux 的在 linux_layouts
    #[test]
    fn gfx103_stack() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(gfx103_family(&s(&["gfx1031"])) && gfx103_family(&s(&["gfx1030", "gfx1036"])));
        assert!(!gfx103_family(&[]) && !gfx103_family(&s(&["gfx1031", "gfx1100"])) && !gfx103_family(&s(&["gfx1010"])));
        assert_eq!(rocm10_bytes(&s(&["gfx1031"])), GFX103_BYTES);
        assert!(gfx103_specs().iter().all(|x| x.ends_with(GFX103_ROCM)) && gfx103_specs()[1].starts_with("torchvision==0.25.0"));
        let d = std::env::temp_dir().join(format!("anima_gfx103_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let py = d.join("python_embeded").join("python.exe");
        let sp = d.join("python_embeded").join("Lib").join("site-packages");
        std::fs::create_dir_all(&sp).unwrap();
        std::fs::write(&py, b"").unwrap();
        std::fs::write(d.join("run_amd_gpu.bat"), b"").unwrap();
        let layout = ComfyLayout { root: d.join("ComfyUI"), main: d.join("ComfyUI").join("main.py"), python: py.clone(), portable: true, desktop: false, cwd: d.clone() };
        let plan = plan_for("amd", "gfx1031", &[], true);
        // 網友現在的樣子：多架構 ROCm 10＋gfx1031 device 套件（RECORD 齊全）→ 還是要換
        for x in ["torch-2.13.0+rocm10.0.0.dist-info", "amd_torch_device_gfx1031-2.13.0+rocm10.0.0.dist-info", "rocm_sdk_device_gfx1031-10.0.0.dist-info", "rocm_sdk_libraries-10.0.0.dist-info", "rocm_sdk_core-10.0.0.dist-info"] {
            std::fs::create_dir_all(sp.join(x)).unwrap();
        }
        assert!(!gfx103_installed(&py));
        assert!(rocm10_needed(&layout, &plan), "RX 6000 不認多架構 ROCm 10");
        assert_eq!(rocm10_leftovers(&py), s(&["amd_torch_device_gfx1031", "rocm_sdk_device_gfx1031", "rocm_sdk_libraries"]));
        // 換好：torch 是 gfx103X 這一版、函式庫套件在
        std::fs::remove_dir_all(sp.join("torch-2.13.0+rocm10.0.0.dist-info")).unwrap();
        std::fs::create_dir_all(sp.join(format!("torch-2.10.0+rocm{GFX103_ROCM}.dist-info"))).unwrap();
        let lib = sp.join(format!("rocm_sdk_libraries_gfx103x_all-{GFX103_ROCM}.dist-info"));
        std::fs::create_dir_all(&lib).unwrap();
        assert!(gfx103_installed(&py));
        assert!(!rocm10_needed(&layout, &plan));
        assert!(!rocm10_leftovers(&py).contains(&"rocm_sdk_libraries_gfx103x_all".to_string()));
        // 函式庫檔案不完整＝沒裝好
        std::fs::write(lib.join("RECORD"), "_rocm_sdk_libraries_gfx103x_all/bin/rocblas.dll,sha256=x,1\n").unwrap();
        assert!(!gfx103_installed(&py));
        // 別族的卡照舊看多架構 ROCm 10 的 device 套件
        assert!(rocm10_needed(&layout, &plan_for("amd", "gfx1010", &[], true)));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 2026-11-05 回報：i5-12400F＋RX 6750 GRE（gfx1031），精靈可能選錯架構 → 開站自動補裝偵測到的架構、把選擇改回來
    #[test]
    fn rocm10_repair_follows_detected_card() {
        let gpus = [json!({"vendor": "amd", "name": "AMD Radeon RX 6750 GRE 10GB", "vram_mb": 10240, "gfx": "gfx1031", "route": "rocm10"})];
        let p = |gpu: &str, gfx: &str| Prefs { gpu: gpu.into(), amd_gfx: gfx.into(), ..Default::default() };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // 選錯成 RX 6800（gfx1030）：補 gfx1031，選擇改成 gfx1031
        assert_eq!(rocm10_repair(&gpus, true, &p("amd", "gfx1030"), &s(&["gfx1030"])), (s(&["gfx1031"]), Some("gfx1031".into())));
        // 選對了、kpack 不見（have 是空的）：只補，不改選擇
        assert_eq!(rocm10_repair(&gpus, true, &p("amd", "gfx1031"), &[]), (s(&["gfx1031"]), None));
        // 自動偵測、已經裝好：什麼都不做
        assert_eq!(rocm10_repair(&gpus, true, &p("auto", ""), &s(&["gfx1031"])), (vec![], None));
        // 認不出型號的卡：照手動選的
        let odd = [json!({"vendor": "amd", "name": "AMD Radeon Graphics", "vram_mb": 8192, "route": "unknown"})];
        assert_eq!(rocm10_repair(&odd, true, &p("amd", "gfx1032"), &[]), (s(&["gfx1032"]), None));
        // N 卡電腦：不碰
        let nv = [json!({"vendor": "nvidia", "name": "NVIDIA GeForce RTX 4090", "vram_mb": 24564})];
        assert_eq!(rocm10_repair(&nv, true, &p("auto", ""), &[]), (vec![], None));
    }

    #[test]
    fn hip_device_skips_unsupported_igpu() {
        let built = json!(["gfx1100", "gfx1101", "gfx1102", "gfx1150", "gfx1151", "gfx1200", "gfx1201"]);
        let igpu = json!({"name": "AMD Radeon(TM) Graphics", "arch": "gfx1036", "mem": 2u64 << 30});
        let dgpu = json!({"name": "AMD Radeon RX 7800 XT", "arch": "gfx1101:sramecc-:xnack-", "mem": 16u64 << 30});
        assert_eq!(pick_hip_device(&json!({"arch": built, "dev": [igpu, dgpu]}), Some("gfx1101")), Some(1));
        // 精靈記的架構沒有也照樣挑得到（看 PyTorch 有編的＋顯存）
        assert_eq!(pick_hip_device(&json!({"arch": built, "dev": [igpu, dgpu]}), None), Some(1));
        // 問不到 PyTorch 編了哪些：照精靈裝的那張，再看顯存
        let small = json!({"name": "AMD Radeon RX 7600", "arch": "gfx1102", "mem": 8u64 << 30});
        assert_eq!(pick_hip_device(&json!({"arch": [], "dev": [dgpu, small]}), Some("gfx1102")), Some(1));
        assert_eq!(pick_hip_device(&json!({"arch": [], "dev": [igpu, dgpu]}), None), Some(1));
        // 只有一張、一張都不能用：照原樣開
        assert_eq!(pick_hip_device(&json!({"arch": built, "dev": [dgpu]}), None), None);
        assert_eq!(pick_hip_device(&json!({"arch": built, "dev": [igpu, igpu]}), None), None);
    }
}

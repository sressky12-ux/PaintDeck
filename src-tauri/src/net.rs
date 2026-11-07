//! 對外連線：代理（梯子）和下載來源（2026-10-21）
//!
//! 大陸使用者連 Hugging Face、GitHub、Civitai 要開「梯子」，常見開法：
//!   - 系統代理（Clash、v2rayN…的「系統代理」）：登錄檔 Internet Settings 的 ProxyServer，
//!     寫法有三種：`127.0.0.1:7897`、`http=…;https=…;socks=…`、只有 `socks=…`
//!   - PAC 模式：登錄檔 AutoConfigURL 指到一個 .pac 檔
//!   - TUN／虛擬網卡：不用設定，直連就會被接走（這裡看到的是「直連」，也能通）
//!   - 環境變數 HTTPS_PROXY／HTTP_PROXY／ALL_PROXY
//!
//! reqwest 關了預設功能，讀不到系統代理（只讀環境變數），而且環境變數會連本機的請求一起送進代理。
//! 所以每個請求在這裡決定（proxy()，掛在每個對外的 reqwest client 上）：
//!   - 本機和區網（127.0.0.1、localhost、192.168.x…）一律直連：ComfyUI、llama-server、雲端的 SSH 通道
//!   - 設定「不用代理」→ 直連；「手動」→ 用填的網址
//!   - 「自動」（預設）→ 環境變數 → 系統代理 → PAC 檔裡的第一個代理 → 直連
//!   - 自動找到的代理在本機、那個 port 卻沒人聽（梯子關了、當掉了，系統代理還開著）→ 直連，不會整個卡死
//!
//! 下載來源（candidates）：Hugging Face 連不上時自動改用 hf-mirror.com，成功過一次之後先用鏡像；
//! nodejs.org 連不上改用 npmmirror。換來源下載的檔照樣核對大小。
//! 設定存在 config.json 旁邊的 net.json：{"proxy": "auto"|"off"|"manual", "proxy_url": "…", "hf": "auto"|"hf"|"mirror"}
use reqwest::Url;
use serde_json::{json, Value};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

pub const HF: &str = "huggingface.co";
pub const HF_MIRROR: &str = "hf-mirror.com";
/// npmmirror（阿里巴巴維護、大陸常用）的 Node.js 鏡像：路徑跟 nodejs.org/dist 一樣
const NODE_MIRROR: &str = "https://registry.npmmirror.com/-/binary/node";

#[derive(Debug, Clone, PartialEq)]
pub struct Cfg {
    /// auto＝環境變數→系統代理→PAC→直連；off＝一律直連；manual＝用 proxy_url
    pub proxy: String,
    pub proxy_url: String,
    /// Hugging Face 的下載來源：auto＝連不上自動換鏡像；hf＝只用官方；mirror＝只用 hf-mirror.com
    pub hf: String,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg { proxy: "auto".into(), proxy_url: String::new(), hf: "auto".into() }
    }
}

impl Cfg {
    /// 網頁送來的設定（缺的欄位照目前的）
    pub fn merged(&self, v: &Value) -> Result<Cfg, String> {
        let mut c = self.clone();
        if let Some(p) = v.get("proxy") {
            c.proxy = p.as_str().unwrap_or("").to_string();
            if !matches!(c.proxy.as_str(), "auto" | "off" | "manual") {
                return Err("代理只能是 auto、off、manual".into());
            }
        }
        if let Some(u) = v.get("proxy_url") {
            c.proxy_url = u.as_str().unwrap_or("").trim().to_string();
        }
        if let Some(h) = v.get("hf") {
            c.hf = h.as_str().unwrap_or("").to_string();
            if !matches!(c.hf.as_str(), "auto" | "hf" | "mirror") {
                return Err("下載來源只能是 auto、hf、mirror".into());
            }
        }
        if c.proxy == "manual" && norm(&c.proxy_url).is_none() {
            return Err("手動代理的格式不對：要像 127.0.0.1:7890、http://127.0.0.1:7890 或 socks5://127.0.0.1:7891".into());
        }
        Ok(c)
    }

    fn json(&self) -> Value {
        json!({"proxy": self.proxy, "proxy_url": self.proxy_url, "hf": self.hf})
    }
}

/// 自動模式找到的代理（背景每 5 秒更新：梯子開開關關不用重開生圖站）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    /// env／system／pac；None＝沒設定代理（直連，開 TUN 也是這樣）
    pub source: Option<&'static str>,
    /// 已經整理成 scheme://host:port
    pub http: Option<String>,
    pub https: Option<String>,
    /// 不走代理的網址（NO_PROXY 或系統代理的「例外」）
    pub bypass: Vec<String>,
    pub pac_url: Option<String>,
    /// 代理在本機、但那個 port 沒人聽
    pub dead: bool,
}

static CFG: LazyLock<RwLock<Cfg>> = LazyLock::new(|| RwLock::new(Cfg::default()));
static FOUND: LazyLock<RwLock<Found>> = LazyLock::new(|| RwLock::new(Found::default()));
static FILE: OnceLock<PathBuf> = OnceLock::new();
static WATCH: OnceLock<()> = OnceLock::new();
/// 自動模式下 Hugging Face 連不上、改用鏡像下載成功過：之後先用鏡像
static HF_MIRROR_ON: AtomicBool = AtomicBool::new(false);
/// PAC 檔的內容不常變：同一個網址一分鐘抓一次（網址、抓的時間、找到的代理）
#[cfg_attr(not(windows), allow(dead_code))] // 只有 Windows 用（Linux 沒有這個機制）
type PacEntry = (String, Instant, Option<String>);
#[cfg_attr(not(windows), allow(dead_code))] // 只有 Windows 用（Linux 沒有這個機制）
static PAC_CACHE: LazyLock<Mutex<Option<PacEntry>>> = LazyLock::new(|| Mutex::new(None));

/// 讀 net.json、開始偵測代理。設定檔放在 config.json 旁邊（一台電腦一份，換資料位置也跟著）
pub fn init(file: &Path) {
    let _ = FILE.set(file.to_path_buf());
    if let Ok(t) = std::fs::read_to_string(file) {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            // 檔案被改壞（例如手動代理格式不對）：當作預設的「自動」
            *CFG.write().unwrap_or_else(|e| e.into_inner()) = Cfg::default().merged(&v).unwrap_or_default();
        }
    }
    start();
}

/// 背景偵測（第一次用到時才開）：先同步讀一次環境變數和登錄檔，PAC 交給背景執行緒去抓
fn start() {
    WATCH.get_or_init(|| {
        refresh(false);
        let _ = std::thread::Builder::new().name("net-watch".into()).spawn(|| loop {
            refresh(true);
            std::thread::sleep(Duration::from_secs(5));
        });
    });
}

pub fn cfg() -> Cfg {
    CFG.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn found() -> Found {
    start();
    FOUND.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 存設定（net.json）並立刻生效
pub fn save(c: Cfg) -> Result<(), String> {
    if let Some(f) = FILE.get() {
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("建立 {} 失敗：{e}", d.display()))?;
        }
        let tmp = f.with_extension("json.tmp");
        std::fs::write(&tmp, crate::settings::to_text(&c.json())).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, f).map_err(|e| e.to_string())?;
    }
    *CFG.write().unwrap_or_else(|e| e.into_inner()) = c;
    Ok(())
}

// ---------------------------------------------------------------- 每個請求走哪個代理
/// 掛在對外的 reqwest client 上：.proxy(net::proxy())
pub fn proxy() -> reqwest::Proxy {
    start();
    reqwest::Proxy::custom(|u: &Url| pick_for(u).and_then(|p| Url::parse(&p).ok()))
}

/// 這個網址要走的代理（None＝直連）
pub fn pick_for(url: &Url) -> Option<String> {
    let f = FOUND.read().unwrap_or_else(|e| e.into_inner());
    decide(&cfg(), &f, url)
}

pub fn decide(cfg: &Cfg, f: &Found, url: &Url) -> Option<String> {
    let host = url.host_str()?;
    if local_host(host) {
        return None;
    }
    match cfg.proxy.as_str() {
        "off" => None,
        "manual" => norm(&cfg.proxy_url),
        _ => {
            if f.dead || bypassed(host, &f.bypass) {
                return None;
            }
            if url.scheme() == "https" { f.https.clone() } else { f.http.clone() }
        }
    }
}

/// 本機、區網、沒有點的主機名（<local>）：一律直連
pub fn local_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || !h.contains(['.', ':']) {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified(),
        Ok(IpAddr::V6(a)) => {
            a.is_loopback() || a.is_unspecified() || (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

/// NO_PROXY／系統代理「例外」：*＝全部；<local>＝沒有點的名字；*.example.com、10.*；example.com 也算子網域
fn bypassed(host: &str, list: &[String]) -> bool {
    let h = host.to_ascii_lowercase();
    list.iter().map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).any(|e| {
        if e == "*" {
            return true;
        }
        if e == "<local>" {
            return !h.contains('.');
        }
        if e.contains('*') {
            return glob(&e, &h);
        }
        let d = e.trim_start_matches('.');
        h == d || h.ends_with(&format!(".{d}"))
    })
}

fn glob(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else if let Some(k) = rest.find(p) {
            rest = &rest[k + p.len()..];
        } else {
            return false;
        }
    }
    rest.is_empty()
}

/// 代理寫法整理成 scheme://host:port（沒寫 scheme 當 http；socks 一律讓代理查 DNS，避開被汙染的 DNS）
pub fn norm(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let with = if s.contains("://") { s.to_string() } else { format!("http://{s}") };
    let u = Url::parse(&with).ok()?;
    let scheme = match u.scheme() {
        "http" | "https" => u.scheme(),
        "socks5" | "socks5h" | "socks" => "socks5h",
        "socks4" | "socks4a" => "socks4a",
        _ => return None,
    };
    let host = u.host_str()?;
    let port = u.port_or_known_default()?;
    let auth = match (u.username(), u.password()) {
        ("", _) => String::new(),
        (n, Some(p)) => format!("{n}:{p}@"),
        (n, None) => format!("{n}@"),
    };
    Some(format!("{scheme}://{auth}{host}:{port}"))
}

/// 登錄檔 ProxyServer → (http 用的, https 用的)。
/// `h:p`＝全部；`http=h:p;https=h:p;socks=h:p`＝各自（沒寫的那種走 socks，跟 Windows 一樣）
pub fn parse_proxy_server(s: &str) -> (Option<String>, Option<String>) {
    let s = s.trim();
    if !s.contains('=') {
        let p = norm(s);
        return (p.clone(), p);
    }
    let mut map = std::collections::HashMap::new();
    for part in s.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            map.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let socks = map.get("socks").and_then(|v| norm(&if v.contains("://") { v.clone() } else { format!("socks5h://{v}") }));
    let get = |k: &str| map.get(k).and_then(|v| norm(v)).or_else(|| socks.clone());
    (get("http"), get("https"))
}

/// PAC 檔裡第一個代理（Clash、v2rayN 的 PAC 都是「PROXY 127.0.0.1:port; SOCKS5 …; DIRECT」）。
/// 我們連的都是國外網站，PAC 本來就會讓它們走代理，所以不用真的執行 PAC 的 JavaScript
pub fn parse_pac(text: &str) -> Option<String> {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b(PROXY|HTTPS|SOCKS5|SOCKS)\s+([A-Za-z0-9.\-]+|\[[0-9a-fA-F:]+\]):(\d{1,5})"#).expect("PAC regex")
    });
    let c = RE.captures(text)?;
    let scheme = match c[1].to_ascii_uppercase().as_str() {
        "HTTPS" => "https",
        "SOCKS5" | "SOCKS" => "socks5h",
        _ => "http",
    };
    norm(&format!("{scheme}://{}:{}", &c[2], &c[3]))
}

// ---------------------------------------------------------------- 偵測
fn env_any(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.trim().is_empty()))
}

fn from_env() -> Option<Found> {
    let all = env_any(&["ALL_PROXY", "all_proxy"]).and_then(|v| norm(&v));
    let http = env_any(&["HTTP_PROXY", "http_proxy"]).and_then(|v| norm(&v)).or_else(|| all.clone());
    let https = env_any(&["HTTPS_PROXY", "https_proxy"]).and_then(|v| norm(&v)).or_else(|| all.clone());
    if http.is_none() && https.is_none() {
        return None;
    }
    let bypass = env_any(&["NO_PROXY", "no_proxy"]).map(|v| v.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default();
    Some(Found { source: Some("env"), http, https, bypass, ..Default::default() })
}

#[cfg(windows)]
mod reg {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }
    pub fn dword(name: &str) -> Option<u32> {
        let (k, n) = (w(KEY), w(name));
        let (mut v, mut len) = (0u32, 4u32);
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut v as *mut u32).cast(), &mut len) };
        (r == 0).then_some(v)
    }
    pub fn string(name: &str) -> Option<String> {
        let (k, n) = (w(KEY), w(name));
        let mut len = 0u32;
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) };
        if r != 0 || len < 2 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end])).filter(|s| !s.trim().is_empty())
    }
}

/// Windows 的系統代理（Clash、v2rayN 開「系統代理」就是改這裡）。PAC 只在 with_pac 時去抓
#[cfg(windows)]
fn from_system(with_pac: bool) -> Option<Found> {
    let bypass: Vec<String> = reg::string("ProxyOverride").map(|s| s.split(';').map(|x| x.trim().to_string()).collect()).unwrap_or_default();
    if reg::dword("ProxyEnable").unwrap_or(0) != 0 {
        if let Some(s) = reg::string("ProxyServer") {
            let (http, https) = parse_proxy_server(&s);
            if http.is_some() || https.is_some() {
                return Some(Found { source: Some("system"), http, https, bypass, ..Default::default() });
            }
        }
    }
    let pac = reg::string("AutoConfigURL")?;
    let p = if with_pac { pac_proxy(&pac) } else { cached_pac(&pac) };
    Some(Found { source: Some("pac"), http: p.clone(), https: p, bypass, pac_url: Some(pac), dead: false })
}
#[cfg(not(windows))]
fn from_system(_: bool) -> Option<Found> {
    None
}

#[cfg_attr(not(windows), allow(dead_code))] // 只有 Windows 用（Linux 沒有這個機制）
fn cached_pac(url: &str) -> Option<String> {
    PAC_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|c| c.0 == url).and_then(|c| c.2.clone())
}

/// 抓 PAC 檔（本機的 http 網址或 file:///）找出第一個代理；一分鐘內同一個網址用上次的結果
#[cfg_attr(not(windows), allow(dead_code))] // 只有 Windows 用（Linux 沒有這個機制）
fn pac_proxy(url: &str) -> Option<String> {
    if let Some(c) = PAC_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if c.0 == url && c.1.elapsed() < Duration::from_secs(60) {
            return c.2.clone();
        }
    }
    let text = if let Some(path) = url.strip_prefix("file:///").or_else(|| url.strip_prefix("file://")) {
        std::fs::read_to_string(path.replace("%20", " ")).ok()
    } else {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .ok()
            .and_then(|c| c.get(url).send().ok())
            .filter(|r| r.status().is_success())
            .and_then(|r| r.text().ok())
    };
    let p = text.as_deref().and_then(parse_pac);
    *PAC_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((url.to_string(), Instant::now(), p.clone()));
    p
}

/// 代理在本機（或區網）時，看那個 port 有沒有人聽；遠端的代理不檢查
fn alive(proxy: &str) -> bool {
    let Ok(u) = Url::parse(proxy) else { return true };
    let (Some(h), Some(p)) = (u.host_str(), u.port_or_known_default()) else { return true };
    if !local_host(h) {
        return true;
    }
    let h = h.trim_start_matches('[').trim_end_matches(']');
    match (h, p).to_socket_addrs() {
        Ok(mut a) => a.any(|sa| TcpStream::connect_timeout(&sa, Duration::from_millis(400)).is_ok()),
        Err(_) => false,
    }
}

/// 重新偵測：環境變數 → 系統代理 → PAC；找到的第一個「活著的」就用它
fn refresh(with_pac: bool) {
    let cands: Vec<Found> = [from_env(), from_system(with_pac)].into_iter().flatten().collect();
    let mut pick = None;
    for mut f in cands.iter().cloned() {
        let p = f.https.clone().or_else(|| f.http.clone());
        f.dead = p.as_deref().is_some_and(|p| !alive(p));
        if !f.dead {
            pick = Some(f);
            break;
        }
        pick.get_or_insert(f); // 全部都連不上：記第一個（給狀態列看），請求照樣直連
    }
    let f = pick.unwrap_or_default();
    let mut w = FOUND.write().unwrap_or_else(|e| e.into_inner());
    if *w != f {
        eprintln!("[網路] {}", describe_found(&cfg(), &f));
        *w = f;
    }
}

// ---------------------------------------------------------------- 下載來源
pub fn hf_mirror_active() -> bool {
    match cfg().hf.as_str() {
        "mirror" => true,
        "hf" => false,
        _ => HF_MIRROR_ON.load(Ordering::Relaxed),
    }
}

/// 同一個檔可以從哪幾個網址下載（照順序試）。只換主機，路徑一樣，下載完照樣核對大小
pub fn candidates(url: &str) -> Vec<String> {
    let Ok(u) = Url::parse(url) else { return vec![url.to_string()] };
    match u.host_str() {
        Some(HF) => {
            let mut m = u.clone();
            let _ = m.set_host(Some(HF_MIRROR));
            let (o, m) = (url.to_string(), m.to_string());
            match cfg().hf.as_str() {
                "hf" => vec![o],
                "mirror" => vec![m],
                _ if HF_MIRROR_ON.load(Ordering::Relaxed) => vec![m, o],
                _ => vec![o, m],
            }
        }
        Some("nodejs.org") if u.path().starts_with("/dist/") => {
            vec![url.to_string(), format!("{NODE_MIRROR}{}", &u.path()["/dist".len()..])]
        }
        _ => vec![url.to_string()],
    }
}

/// 下載成功時記下用的是哪個來源：Hugging Face 改用鏡像成功，之後就先用鏡像
pub fn used(url: &str) {
    if Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h == HF_MIRROR)).unwrap_or(false) && !HF_MIRROR_ON.swap(true, Ordering::Relaxed) {
        eprintln!("[網路] Hugging Face 連不上、改用 hf-mirror.com 成功：之後的下載先用鏡像");
    }
}

/// 子程式（ComfyUI）的環境變數：PAC、手動代理 Python 讀不到 → 帶成 HTTP(S)_PROXY；
/// 用鏡像時帶 HF_ENDPOINT（自訂節點用 huggingface_hub 自己下載模型時也走鏡像）。使用者自己設了就不動
pub fn child_env() -> Vec<(String, String)> {
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let mut v = vec![];
    if !["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"].iter().any(|k| has(k)) {
        let hf = Url::parse("https://huggingface.co/").expect("網址");
        // socks 代理不帶：Python 的 requests 沒裝 PySocks 會直接報錯
        if let Some(p) = pick_for(&hf).filter(|p| p.starts_with("http")) {
            for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                v.push((k.to_string(), p.clone()));
            }
            if !has("NO_PROXY") && !has("no_proxy") {
                for k in ["NO_PROXY", "no_proxy"] {
                    v.push((k.to_string(), "localhost,127.0.0.1,::1".to_string()));
                }
            }
        }
    }
    if !has("HF_ENDPOINT") && hf_mirror_active() {
        v.push(("HF_ENDPOINT".into(), format!("https://{HF_MIRROR}")));
    }
    v
}

// ---------------------------------------------------------------- 給人看的
fn hide_auth(p: &str) -> String {
    match p.split_once("://").and_then(|(s, r)| r.split_once('@').map(|(_, h)| format!("{s}://***@{h}"))) {
        Some(x) => x,
        None => p.to_string(),
    }
}

fn describe_found(c: &Cfg, f: &Found) -> String {
    match c.proxy.as_str() {
        "off" => "不用代理（一律直連）".into(),
        "manual" => match norm(&c.proxy_url) {
            Some(p) => format!("手動代理 {}{}", hide_auth(&p), if alive(&p) { "" } else { "（沒有回應：梯子開了嗎？）" }),
            None => "手動代理的格式不對".into(),
        },
        _ => {
            let p = f.https.clone().or_else(|| f.http.clone()).map(|p| hide_auth(&p));
            let what = match (f.source, &p) {
                (Some("env"), Some(p)) => format!("環境變數的代理 {p}"),
                (Some("system"), Some(p)) => format!("系統代理 {p}"),
                (Some("pac"), Some(p)) => format!("PAC 自動設定 → {p}"),
                (Some("pac"), None) => "PAC 自動設定（讀不到裡面的代理，先直連）".into(),
                _ => return "沒有設定代理：直連（梯子開 TUN／虛擬網卡模式的話，直連也會被接走）".into(),
            };
            if f.dead {
                format!("{what}，但它沒有回應（梯子關了？），先直連")
            } else {
                what
            }
        }
    }
}

/// 設定頁、精靈顯示用
pub fn status() -> Value {
    let (c, f) = (cfg(), found());
    json!({"cfg": c.json(), "text": describe_found(&c, &f), "source": f.source, "dead": f.dead,
           "proxy": decide(&c, &f, &Url::parse("https://huggingface.co/").expect("網址")).map(|p| hide_auth(&p)),
           "hf_mirror": hf_mirror_active()})
}

/// 連線錯誤 → 看得懂的原因＋怎麼辦。只寫主機名（Civitai 的網址帶 token，不能出現在訊息裡）
pub fn explain(e: &reqwest::Error, url: &str) -> String {
    let u = Url::parse(url).ok();
    let host = u.as_ref().and_then(|u| u.host_str()).unwrap_or("").to_string();
    let via = u.as_ref().and_then(pick_for);
    // 只看下層的錯誤（最外層的字串有完整網址）
    let mut chain = String::new();
    let mut inner = String::new();
    let mut cur = std::error::Error::source(e);
    while let Some(x) = cur {
        inner = x.to_string();
        chain.push_str(&inner.to_lowercase());
        chain.push(' ');
        cur = x.source();
    }
    let has = |ks: &[&str]| ks.iter().any(|k| chain.contains(k));
    let refused = has(&["refused", "10061"]);
    let what = if e.is_timeout() || has(&["timed out", "timeout", "10060"]) {
        "連線逾時"
    } else if refused {
        "對方拒絕連線"
    } else if has(&["dns", "no such host", "11001", "failed to lookup", "name or service"]) {
        "查不到網址（DNS）"
    } else if has(&["certificate", "tls", "ssl", "handshake"]) {
        "加密連線（TLS）失敗"
    } else if has(&["reset", "10054", "forcibly closed", "unexpected eof", "connection closed", "broken pipe", "aborted"]) {
        "連線被中斷"
    } else if has(&["proxy", "socks"]) {
        "代理回了錯誤"
    } else if e.is_body() || e.is_decode() {
        "下載到一半斷線"
    } else {
        "連不上"
    };
    let foreign = !host.is_empty() && !local_host(&host);
    let hint = match &via {
        Some(p) if refused || has(&["proxy", "socks"]) => format!(
            "代理 {} 好像沒開或 port 不對：請確認梯子（Clash、v2rayN…）在執行，或到「設定 → 網路」改代理設定。",
            hide_auth(p)
        ),
        Some(p) => format!(
            "已經經過代理 {}，還是連不到 {host}：換個節點再試，或到「設定 → 網路」按「測試連線」。",
            hide_auth(p)
        ),
        None if foreign => {
            let mut s = "現在是直連。在中國大陸要開梯子：系統代理、PAC、TUN 模式都會自動套用；也可以到「設定 → 網路」手動填代理。".to_string();
            if host == HF {
                s += "Hugging Face 也可以改用鏡像 hf-mirror.com（設定 → 網路 → 下載來源）。";
            }
            s
        }
        None => String::new(),
    };
    static URL_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"https?://\S+").expect("url regex"));
    let detail = URL_RE.replace_all(inner.trim(), "…").chars().take(160).collect::<String>();
    // 逾時的原文只是 operation timed out／deadline has elapsed，沒多說什麼，不附
    let detail = if detail.is_empty() || what == "連線逾時" { String::new() } else { format!("（{detail}）") };
    format!("{what}：{host}{detail}。{hint}").trim_end_matches('。').to_string() + "。"
}

/// 測試一個網址連不連得到（只要第一個位元組；會跟著轉址走）。給設定頁、精靈的「測試連線」
pub async fn probe(http: &reqwest::Client, url: &str) -> Value {
    let t = Instant::now();
    let via = Url::parse(url).ok().and_then(|u| pick_for(&u)).map(|p| hide_auth(&p));
    let r = http
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    let ms = t.elapsed().as_millis() as u64;
    match r {
        Ok(r) => {
            let code = r.status().as_u16();
            // 401／403：伺服器有回應（只是要登入），網路是通的
            json!({"ok": code < 400 || code == 401 || code == 403, "code": code, "ms": ms, "via": via})
        }
        Err(e) => json!({"ok": false, "ms": ms, "via": via, "error": explain(&e, url)}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn proxy_server_forms() {
        let all = Some("http://127.0.0.1:7897".to_string());
        assert_eq!(parse_proxy_server("127.0.0.1:7897"), (all.clone(), all.clone()));
        assert_eq!(parse_proxy_server("http://127.0.0.1:7897"), (all.clone(), all.clone()));
        assert_eq!(
            parse_proxy_server("http=127.0.0.1:10809;https=127.0.0.1:10810;ftp=1.2.3.4:21"),
            (Some("http://127.0.0.1:10809".into()), Some("http://127.0.0.1:10810".into()))
        );
        // 只有 socks：http、https 都走 socks（讓代理查 DNS）
        let s = Some("socks5h://127.0.0.1:10808".to_string());
        assert_eq!(parse_proxy_server("socks=127.0.0.1:10808"), (s.clone(), s.clone()));
        assert_eq!(parse_proxy_server("https=127.0.0.1:7890;socks=127.0.0.1:7891"), (Some("socks5h://127.0.0.1:7891".into()), Some("http://127.0.0.1:7890".into())));
        assert_eq!(parse_proxy_server(""), (None, None));
    }

    #[test]
    fn norm_forms() {
        assert_eq!(norm("127.0.0.1:7890").as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(norm(" socks5://127.0.0.1:7891/ ").as_deref(), Some("socks5h://127.0.0.1:7891"));
        assert_eq!(norm("http://user:pw@10.0.0.2:3128").as_deref(), Some("http://user:pw@10.0.0.2:3128"));
        assert_eq!(norm("ftp://x:1"), None);
        assert_eq!(norm(""), None);
    }

    #[test]
    fn pac_forms() {
        assert_eq!(parse_pac("var proxy = 'PROXY 127.0.0.1:10809; SOCKS5 127.0.0.1:10808; DIRECT;';").as_deref(), Some("http://127.0.0.1:10809"));
        assert_eq!(parse_pac(r#"function FindProxyForURL(u,h){return "SOCKS5 127.0.0.1:7891; DIRECT"}"#).as_deref(), Some("socks5h://127.0.0.1:7891"));
        assert_eq!(parse_pac("function FindProxyForURL(u,h){return 'DIRECT'}"), None);
    }

    /// PAC 檔：本機檔案（file:///）和本機網址（v2rayN 的 PAC 伺服器）都讀得到
    #[test]
    fn pac_fetch() {
        let dir = std::env::temp_dir().join(format!("anima_pac_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("proxy pac.js");
        std::fs::write(&f, "function FindProxyForURL(u,h){return 'PROXY 127.0.0.1:10811; DIRECT'}").unwrap();
        let url = format!("file:///{}", f.display().to_string().replace('\\', "/").replace(' ', "%20"));
        assert_eq!(pac_proxy(&url).as_deref(), Some("http://127.0.0.1:10811"));
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut s, _)) = l.accept() {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                let body = "var proxy = \"SOCKS5 127.0.0.1:10808; DIRECT\";";
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        assert_eq!(pac_proxy(&format!("http://127.0.0.1:{port}/pac?t=1")).as_deref(), Some("socks5h://127.0.0.1:10808"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_and_bypass() {
        for h in ["127.0.0.1", "localhost", "192.168.1.5", "10.0.0.8", "172.20.1.1", "[::1]", "mypc", "nas.local"] {
            assert!(local_host(h), "{h}");
        }
        for h in ["huggingface.co", "8.8.8.8", "civitai.com"] {
            assert!(!local_host(h), "{h}");
        }
        let l: Vec<String> = ["*.example.com", "10.*", "civitai.com", "<local>"].iter().map(|s| s.to_string()).collect();
        assert!(bypassed("a.example.com", &l));
        assert!(bypassed("image.civitai.com", &l));
        assert!(!bypassed("huggingface.co", &l));
        assert!(bypassed("anything", &["*".to_string()]));
    }

    #[test]
    fn decide_rules() {
        let f = Found { source: Some("system"), http: Some("http://127.0.0.1:7897".into()), https: Some("http://127.0.0.1:7897".into()), ..Default::default() };
        let auto = Cfg::default();
        assert_eq!(decide(&auto, &f, &u("https://huggingface.co/x")).as_deref(), Some("http://127.0.0.1:7897"));
        // 本機一律直連：ComfyUI、llama-server
        assert_eq!(decide(&auto, &f, &u("http://127.0.0.1:8188/prompt")), None);
        // 梯子關了：直連
        assert_eq!(decide(&auto, &Found { dead: true, ..f.clone() }, &u("https://huggingface.co/x")), None);
        let off = Cfg { proxy: "off".into(), ..Cfg::default() };
        assert_eq!(decide(&off, &f, &u("https://huggingface.co/x")), None);
        let man = Cfg { proxy: "manual".into(), proxy_url: "socks5://127.0.0.1:1080".into(), ..Cfg::default() };
        assert_eq!(decide(&man, &Found::default(), &u("https://civitai.com/")).as_deref(), Some("socks5h://127.0.0.1:1080"));
        assert_eq!(decide(&man, &Found::default(), &u("http://localhost:8199/")), None);
        assert!(Cfg::default().merged(&json!({"proxy": "manual", "proxy_url": "abc def"})).is_err());
        assert!(Cfg::default().merged(&json!({"hf": "xx"})).is_err());
    }

    #[test]
    fn mirror_candidates() {
        let c = candidates("https://huggingface.co/a/b/resolve/main/m.safetensors");
        assert_eq!(c[0], "https://huggingface.co/a/b/resolve/main/m.safetensors");
        assert_eq!(c[1], "https://hf-mirror.com/a/b/resolve/main/m.safetensors");
        assert_eq!(
            candidates("https://nodejs.org/dist/v24.21.0/node-v24.21.0-win-x64.zip")[1],
            "https://registry.npmmirror.com/-/binary/node/v24.21.0/node-v24.21.0-win-x64.zip"
        );
        assert_eq!(candidates("https://github.com/x/y.zip").len(), 1);
    }
}

//! 影音工作台（導演頁的「剪輯台」「拆幀台」）。
//!
//! 剪輯台＝開源的 FableCut 1.15.0（MIT，https://github.com/ronak-create/FableCut），原封不動放在 resources\extras\fablecut。
//!   生圖站用 Node.js 開它的伺服器（127.0.0.1:8194），導演頁直接嵌它的網頁；專案、素材、匯出放 <資料>\data\studio。
//!   外面的 AI（Claude Desktop／Claude Code／Codex…）用它附的 MCP 伺服器（mcp-server.js）操作：設定見 Studio::mcp_config。
//!   MCP 伺服器另外接上生圖站的全站工具 anima_*（同資料夾的 anima-tools.js、anima-guide.md；tools\fablecut_mcp.py 插兩行），
//!   所以設定裡的名字叫 anima：剪片、生圖、圖庫、各台都用這一個。
//!   Node 放進 Windows 的 Job Object（KILL_ON_JOB_CLOSE）：生圖站關掉或當掉，系統都會把它一起收掉。
//! 拆幀台＝生圖站自己的（找不到能嵌進來的開源專案）：ffmpeg 把圖庫的影片拆成一張張 PNG → 換掉壞掉的幀 → 用原本的聲音接回影片、存回圖庫。
//!   放在 <資料>\data\frames\<編號>\：meta.json、f\000001.png（原尺寸）、t\000001.jpg（縮圖）、orig\（被換掉前的原圖）、audio.mka
//! 工具：<資料>\data\tools 底下（安裝精靈下載的）優先，沒有就找 PATH 上的 node.exe、ffmpeg.exe
use crate::pe::win;
use image::{DynamicImage, GenericImageView, RgbImage};
use serde_json::{json, Map, Value};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

mod look;
pub use look::{parse_crop, SheetOpts};

/// FableCut 自己的預設是 7777，但有些電腦的系統服務占著（2026-10-19 使用者電腦上是 mNHIICCService.exe），改用生圖站這一段的 port
pub const DEFAULT_PORT: u16 = 8194;
/// MCP 設定裡的伺服器名稱（AI 程式裡的工具會叫 mcp__anima__…）
pub const MCP_NAME: &str = "anima";
/// 一次最多拆幾幀：幀的檔名是 %06d（000001～999999），24fps 約 11.5 小時。
/// 2026-10-24 拿掉原本固定的 3600 幀上限（使用者：「能否無上限?」），改成看硬碟夠不夠（split_room）
pub const MAX_FRAMES: u64 = 999_999;
/// 拆完至少要留這麼多空間給系統和其他程式
const KEEP_FREE: u64 = 5 << 30;
/// 縮圖（格子、對照表）的高度
const THUMB_H: u32 = 160;

// ---------------------------------------------------------------- 工具
/// dir\rel 或 dir\<任一子資料夾>\rel（壓縮檔解開是 node-v24.21.0-win-x64\node.exe、ffmpeg-9.0.2-essentials_build\bin\ffmpeg.exe）
fn first_with(dir: &Path, rel: &[&str]) -> Option<PathBuf> {
    let at = |d: &Path| {
        let p = rel.iter().fold(d.to_path_buf(), |p, s| p.join(s));
        p.is_file().then_some(p)
    };
    if let Some(p) = at(dir) {
        return Some(p);
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subs.sort();
    subs.iter().rev().find_map(|d| at(d)) // 版號大的排後面：先用新的
}

/// 安裝精靈放的 Node.js（只看 tools 資料夾）
pub fn find_node_in(tools: &Path) -> Option<PathBuf> {
    first_with(&tools.join("node"), &["node.exe"])
}

/// 安裝精靈放的 ffmpeg（ffprobe.exe 也要在旁邊）
pub fn find_ffmpeg_in(tools: &Path) -> Option<PathBuf> {
    first_with(&tools.join("ffmpeg"), &["bin", "ffmpeg.exe"])
        .filter(|f| f.with_file_name("ffprobe.exe").is_file())
}

fn on_path(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

#[derive(Clone, Default)]
pub struct Tools {
    pub node: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
}

/// data＝<資料>\data
pub fn tools(data: &Path) -> Tools {
    let t = data.join("tools");
    let node = find_node_in(&t).or_else(|| on_path("node.exe"));
    let ffmpeg = find_ffmpeg_in(&t).or_else(|| {
        on_path("ffmpeg.exe").filter(|f| f.with_file_name("ffprobe.exe").is_file())
    });
    let ffprobe = ffmpeg.as_ref().map(|f| f.with_file_name("ffprobe.exe"));
    Tools {
        node,
        ffmpeg,
        ffprobe,
    }
}

impl Tools {
    fn ffmpeg(&self) -> Result<&Path, String> {
        self.ffmpeg.as_deref().ok_or_else(|| {
            "找不到 ffmpeg：到剪輯台按「下載剪輯工具」，或打開「安裝與修復」再按一次開始安裝".to_string()
        })
    }
    fn ffprobe(&self) -> Result<&Path, String> {
        self.ffmpeg()?;
        self.ffprobe.as_deref().ok_or_else(|| "找不到 ffprobe".to_string())
    }
}

/// 把 ffmpeg 的資料夾加到 PATH 最前面（FableCut 用 PATH 找 ffmpeg／ffprobe）
fn path_with(ffmpeg: Option<&Path>) -> OsString {
    let mut v: Vec<PathBuf> = ffmpeg.and_then(Path::parent).map(Path::to_path_buf).into_iter().collect();
    if let Some(p) = std::env::var_os("PATH") {
        v.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(v).unwrap_or_default()
}

/// 跑 ffmpeg／ffprobe（不開黑視窗）；失敗回傳 stderr 的最後幾行
fn run(exe: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
    let mut c = Command::new(exe);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    win::no_window(&mut c);
    let out = c
        .output()
        .map_err(|e| format!("執行 {} 失敗：{e}", exe.display()))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = err.lines().rev().take(4).collect();
    Err(format!(
        "{} 失敗：{}",
        exe.file_stem().unwrap_or_default().to_string_lossy(),
        tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
    ))
}

fn os(s: impl AsRef<OsStr>) -> OsString {
    s.as_ref().to_os_string()
}

// ---------------------------------------------------------------- Job Object
#[cfg(windows)]
pub(crate) mod job {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// 這個 Job 的最後一個 handle 關掉時（生圖站結束、當掉），裡面的程序全部被系統結束
    pub struct Job(HANDLE);
    // HANDLE 是指標；Job 只在建立、加程序、關閉時用，跨執行緒沒有問題
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close() -> Option<Job> {
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(h);
                    return None;
                }
                Some(Job(h))
            }
        }

        pub fn add(&self, child: &std::process::Child) -> bool {
            unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) != 0 }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod job {
    pub struct Job;
    impl Job {
        pub fn kill_on_close() -> Option<Job> {
            None
        }
        pub fn add(&self, _child: &std::process::Child) -> bool {
            false
        }
    }
}

// ---------------------------------------------------------------- 剪輯台（FableCut）
struct Proc {
    child: Child,
    _job: Option<job::Job>,
}

pub struct Studio {
    pub port: u16,
    /// resources\extras\fablecut（程式本體，唯讀）
    app_dir: PathBuf,
    /// <資料>\data
    data: PathBuf,
    proc: Mutex<Option<Proc>>,
    starting: tokio::sync::Mutex<()>,
    http: reqwest::Client,
}

/// 剪輯台收的檔案種類（跟 FableCut 的 KIND_BY_EXT 一樣）
pub fn media_kind(name: &str) -> Option<&'static str> {
    let ext = Path::new(name)
        .extension()?
        .to_string_lossy()
        .to_lowercase();
    Some(match ext.as_str() {
        "mp4" | "webm" | "mov" | "mkv" | "m4v" | "avi" => "video",
        "mp3" | "wav" | "ogg" | "m4a" | "aac" | "flac" => "audio",
        "png" | "jpg" | "jpeg" | "gif" | "webp" => "image",
        "svg" => "svg",
        _ => return None,
    })
}

impl Studio {
    /// 連接埠：環境變數 ANIMA_STUDIO_PORT（測試用），預設 8194
    pub fn new(res: &Path, data: &Path) -> Studio {
        let port = std::env::var("ANIMA_STUDIO_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        Studio {
            port,
            app_dir: res.join("extras").join("fablecut"),
            data: data.to_path_buf(),
            proc: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
            http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(600))
                .build()
                .expect("建立 HTTP client"),
        }
    }

    /// 給 AI 的全站說明（MCP 的 anima_docs 讀同一份）
    pub fn guide_file(&self) -> PathBuf {
        self.app_dir.join("anima-guide.md")
    }

    pub fn studio_dir(&self) -> PathBuf {
        self.data.join("studio")
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// port 上是不是 FableCut（GET /api/project 回專案，裡面有 clips 陣列）
    async fn answering(&self) -> bool {
        let r = self
            .http
            .get(self.url("/api/project"))
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => r
                .json::<Value>()
                .await
                .map(|v| v["clips"].is_array())
                .unwrap_or(false),
            _ => false,
        }
    }

    fn ours_alive(&self) -> bool {
        let mut g = self.proc.lock().unwrap_or_else(|e| e.into_inner());
        matches!(g.as_mut().map(|p| p.child.try_wait()), Some(Ok(None)))
    }

    pub async fn status(&self) -> Value {
        let t = tools(&self.data);
        let running = self.answering().await;
        json!({
            "installed": self.app_dir.join("server.js").is_file(),
            "running": running,
            "ours": running && self.ours_alive(),
            "port": self.port,
            "url": format!("http://127.0.0.1:{}/", self.port),
            "node": t.node,
            "ffmpeg": t.ffmpeg,
            "data_dir": self.studio_dir(),
            "app_dir": self.app_dir,
        })
    }

    /// 開剪輯台；已經開著（自己開的、或外面的 AI 用 MCP 開的）就直接用
    pub async fn start(&self) -> Result<Value, String> {
        let _g = self.starting.lock().await;
        if self.answering().await {
            return Ok(self.status().await);
        }
        if crate::launcher::port_open(self.port) {
            return Err(format!(
                "連接埠 {} 被別的程式占用了（不是剪輯台），請先關掉那個程式",
                self.port
            ));
        }
        let t = tools(&self.data);
        let node = t.node.clone().ok_or(
            "找不到 Node.js：按「下載剪輯工具」，或打開「安裝與修復」再按一次開始安裝",
        )?;
        let server = self.app_dir.join("server.js");
        if !server.is_file() {
            return Err(format!("找不到剪輯台程式 {}", server.display()));
        }
        let logs = self.data.join("logs");
        let dir = self.studio_dir();
        for d in [&logs, &dir] {
            std::fs::create_dir_all(d).map_err(|e| format!("建立 {} 失敗：{e}", d.display()))?;
        }
        let log_path = logs.join("fablecut.log");
        let log = std::fs::File::create(&log_path).map_err(|e| format!("建立紀錄檔失敗：{e}"))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&node);
        cmd.arg(&server)
            .current_dir(&self.app_dir)
            .env("PORT", self.port.to_string())
            .env("HOST", "127.0.0.1")
            .env("FABLECUT_DATA_DIR", &dir)
            .env("PATH", path_with(t.ffmpeg.as_deref()))
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        win::no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("啟動剪輯台失敗：{e}"))?;
        let job = job::Job::kill_on_close().filter(|j| j.add(&child));
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(Some(st)) = child.try_wait() {
                let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
                let tail: Vec<&str> = tail.lines().rev().take(6).collect();
                return Err(format!(
                    "剪輯台一啟動就結束了（{st}）。Node.js 要 18 版以上。紀錄：{}",
                    tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
                ));
            }
            if self.answering().await {
                eprintln!("剪輯台已啟動（pid {}，port {}）", child.id(), self.port);
                *self.proc.lock().unwrap_or_else(|e| e.into_inner()) = Some(Proc { child, _job: job });
                return Ok(self.status().await);
            }
        }
        let pid = child.id() as i64;
        tokio::task::spawn_blocking(move || win::kill(pid)).await.ok();
        Err(format!(
            "剪輯台 10 秒內沒有回應（紀錄在 {}）",
            log_path.display()
        ))
    }

    /// 關掉自己開的那一個；外面開的（AI 的 MCP 開的）不碰
    pub async fn stop(&self) -> Result<(), String> {
        let p = self.proc.lock().unwrap_or_else(|e| e.into_inner()).take();
        match p {
            Some(p) => {
                let pid = p.child.id() as i64;
                tokio::task::spawn_blocking(move || win::kill(pid)).await.ok();
                drop(p);
                Ok(())
            }
            None if self.answering().await => Err(
                "這個剪輯台不是生圖站開的（可能是 AI 用 MCP 開的），要從開它的地方關".into(),
            ),
            None => Ok(()),
        }
    }

    /// 把檔案放進剪輯台的素材區：POST /api/upload 存進它的 media，再登記進專案（跟 fablecut_import_media 一樣只加素材、不動時間軸）
    pub async fn import(&self, file: &Path, name: &str) -> Result<Value, String> {
        if !self.answering().await {
            self.start().await?;
        }
        let kind = media_kind(name).ok_or("剪輯台不收這種檔案")?;
        let body = tokio::fs::read(file)
            .await
            .map_err(|e| format!("讀 {} 失敗：{e}", file.display()))?;
        let net = |e: reqwest::Error| format!("連不上剪輯台：{}", e.without_url());
        let r: Value = self
            .http
            .post(self.url("/api/upload"))
            .query(&[("name", name)])
            .body(body)
            .send()
            .await
            .map_err(net)?
            .json()
            .await
            .map_err(net)?;
        let src = r["src"]
            .as_str()
            .ok_or_else(|| format!("放進剪輯台失敗：{}", r["error"]))?
            .to_string();
        let info: Value = self
            .http
            .get(self.url("/api/media-info"))
            .query(&[("src", &src)])
            .send()
            .await
            .map_err(net)?
            .json()
            .await
            .unwrap_or(Value::Null);
        let shown = src.rsplit('/').next().unwrap_or(name);
        let shown = percent_decode(shown);
        let mut entry = json!({"id": format!("m_{:08x}", rand::random::<u32>()), "name": shown, "kind": kind, "src": src});
        for k in [
            "duration", "width", "height", "fps", "codec", "audioCodec", "audioChannels",
            "sampleRate", "size", "bitrate",
        ] {
            let v = &info["info"][k];
            if !v.is_null() && !(kind == "image" && k == "duration") {
                entry[k] = v.clone();
            }
        }
        // 專案的 revision 被別人（網頁、AI）先改了就重讀再加
        for _ in 0..5 {
            let mut proj: Value = self
                .http
                .get(self.url("/api/project"))
                .send()
                .await
                .map_err(net)?
                .json()
                .await
                .map_err(net)?;
            let rev = proj["revision"].as_i64().unwrap_or(0);
            if !proj["media"].is_array() {
                proj["media"] = json!([]);
            }
            if let Some(m) = proj["media"].as_array_mut() {
                m.push(entry.clone());
            }
            proj["revision"] = json!(rev + 1);
            let r = self
                .http
                .put(self.url("/api/project"))
                .json(&proj)
                .send()
                .await
                .map_err(net)?;
            if r.status() == reqwest::StatusCode::CONFLICT {
                continue;
            }
            if r.status().is_success() {
                return Ok(entry);
            }
            return Err(format!("登記素材失敗：{}", r.text().await.unwrap_or_default()));
        }
        Err("剪輯台的專案一直在變，等一下再試".into())
    }

    /// 剪輯台匯出的影片（<資料>\data\studio\exports），新的在前
    pub fn exports(&self) -> Vec<Value> {
        let dir = self.studio_dir().join("exports");
        let mut v: Vec<(f64, Value)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let low = name.to_lowercase();
                if !(low.ends_with(".mp4") || low.ends_with(".webm") || low.ends_with(".mov")) {
                    return None;
                }
                let md = e.metadata().ok()?;
                let t = md
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                Some((t, json!({"name": name, "size": md.len(), "time": t})))
            })
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        v.into_iter().map(|x| x.1).collect()
    }

    /// 打開剪輯台的資料夾（檔案總管）：exports、media、library\sfx
    pub fn open_folder(&self, which: &str) -> Result<(), String> {
        let d = match which {
            "exports" => self.studio_dir().join("exports"),
            "media" => self.studio_dir().join("media"),
            "sfx" => self.studio_dir().join("library").join("sfx"),
            _ => return Err("沒有這個資料夾".into()),
        };
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        Command::new("explorer.exe")
            .arg(&d)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打不開檔案總管：{e}"))
    }

    pub fn export_path(&self, name: &str) -> Option<PathBuf> {
        let n = Path::new(name).file_name()?;
        let p = self.studio_dir().join("exports").join(n);
        p.is_file().then_some(p)
    }

    /// 給外面 AI 用的 MCP 設定（Claude Desktop 的 claude_desktop_config.json、Claude Code 的 .mcp.json 都是這個格式）。
    /// station＝生圖站網址（http://127.0.0.1:<port>），全站工具打它的 API
    pub fn mcp_config(&self, station: &str) -> Value {
        let t = tools(&self.data);
        let node = t
            .node
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "node".into());
        let mut env = json!({
            "FABLECUT_DATA_DIR": self.studio_dir(),
            "FABLECUT_PORT": self.port.to_string(),
            "PORT": self.port.to_string(),
            "ANIMA_URL": station,
        });
        if let Some(bin) = t.ffmpeg.as_deref().and_then(Path::parent) {
            // MCP 自己開剪輯台時也找得到 ffmpeg；其餘是 Windows 本來就有的
            let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            env["PATH"] = json!(format!("{};{sys}\\System32;{sys}", bin.display()));
        }
        json!({"mcpServers": {MCP_NAME: {
            "command": node,
            "args": [self.app_dir.join("mcp-server.js")],
            "env": env,
        }}})
    }

    /// 同一份設定的 Codex CLI 版（~/.codex/config.toml）。路徑用 TOML 的單引號字串，反斜線不用跳脫
    pub fn mcp_toml(&self, station: &str) -> String {
        let c = self.mcp_config(station);
        let f = &c["mcpServers"][MCP_NAME];
        let q = |v: &Value| format!("'{}'", v.as_str().unwrap_or("").replace('\'', ""));
        let env: Vec<String> = f["env"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| format!("{k} = {}", q(v))).collect())
            .unwrap_or_default();
        let args: Vec<String> = f["args"].as_array().map(|a| a.iter().map(q).collect()).unwrap_or_default();
        format!(
            "[mcp_servers.{MCP_NAME}]\ncommand = {}\nargs = [{}]\nenv = {{ {} }}\n",
            q(&f["command"]),
            args.join(", "),
            env.join(", ")
        )
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

// ---------------------------------------------------------------- 影片資訊
pub struct Probe {
    pub width: u32,
    pub height: u32,
    /// ffmpeg 的寫法（"24/1"、"24000/1001"），接回影片時原樣用
    pub fps_r: String,
    pub fps: f64,
    pub duration: f64,
    pub audio: bool,
}

fn rate(s: &str) -> Option<f64> {
    let (a, b) = s.split_once('/').unwrap_or((s, "1"));
    let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

pub fn probe(t: &Tools, file: &Path) -> Result<Probe, String> {
    let out = run(
        t.ffprobe()?,
        &[
            os("-v"),
            os("error"),
            os("-show_entries"),
            os("stream=codec_type,width,height,r_frame_rate,avg_frame_rate:format=duration"),
            os("-of"),
            os("json"),
            file.as_os_str().to_os_string(),
        ],
    )?;
    let v: Value = serde_json::from_slice(&out).map_err(|e| format!("看不懂 ffprobe 的結果：{e}"))?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let vs = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or("這個檔案沒有影像")?;
    let pick = |k: &str| vs[k].as_str().filter(|r| rate(r).is_some()).map(str::to_string);
    let fps_r = pick("avg_frame_rate")
        .or_else(|| pick("r_frame_rate"))
        .unwrap_or_else(|| "24/1".into());
    Ok(Probe {
        width: vs["width"].as_u64().unwrap_or(0) as u32,
        height: vs["height"].as_u64().unwrap_or(0) as u32,
        fps: rate(&fps_r).unwrap_or(24.0),
        fps_r,
        duration: v["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse().ok())
            .unwrap_or(0.0),
        audio: streams.iter().any(|s| s["codec_type"] == "audio"),
    })
}

/// 影片的第一格存成 PNG（圖庫影片的封面）
pub fn first_frame(t: &Tools, video: &Path, png: &Path) -> Result<(), String> {
    if let Some(d) = png.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    run(
        t.ffmpeg()?,
        &[
            os("-hide_banner"),
            os("-v"),
            os("error"),
            os("-y"),
            os("-i"),
            video.as_os_str().to_os_string(),
            os("-frames:v"),
            os("1"),
            png.as_os_str().to_os_string(),
        ],
    )
    .map(|_| ())
}

// ---------------------------------------------------------------- 拆幀台
mod cleanup;

pub struct Frames {
    /// <資料>\data\frames
    root: PathBuf,
}

fn valid_sid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 第 n 幀改過幾次（網頁的圖片網址帶這個數字，改過才重抓、沒改過用快取）
fn bump(m: &mut Value, n: u64) {
    if !m["rev"].is_object() {
        m["rev"] = json!({});
    }
    let k = n.to_string();
    let v = m["rev"][&k].as_u64().unwrap_or(0) + 1;
    m["rev"][k] = json!(v);
}

fn name(n: u64, ext: &str) -> String {
    format!("{n:06}.{ext}")
}

/// 縮圖：高 160 的 JPEG
fn save_thumb(im: &DynamicImage, dst: &Path) -> Result<(), String> {
    let (w, h) = im.dimensions();
    let tw = ((w as f64 * THUMB_H as f64 / h.max(1) as f64).round() as u32).max(1);
    let t = im.resize_exact(tw, THUMB_H, image::imageops::FilterType::Triangle).to_rgb8();
    let f = std::fs::File::create(dst).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), 85)
        .encode_image(&t)
        .map_err(|e| e.to_string())
}

/// 3×5 的數字點陣（對照表上的幀號；畫字在 look.rs 的 draw_text）
const DIGITS: [[u8; 5]; 10] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
];

/// 拆幀的範圍與尺寸（都可不給：整支、原尺寸）
#[derive(Default, Debug, Clone, PartialEq)]
pub struct SplitOpts {
    /// 從第幾秒開始
    pub start: f64,
    /// 到第幾秒（不含；None＝到結尾）
    pub end: Option<f64>,
    /// 原圖最高幾像素（比這高才縮，縮圖照樣 160 高）
    pub max_h: Option<u32>,
    /// 每秒只取幾幀（長片先粗看用；None＝每一幀都拆）
    pub fps: Option<f64>,
    /// true＝只用 CPU；預設優先要求 CUDA，失敗後改 CPU。
    pub cpu: bool,
}

/// 拆 frames 幀（原片 w×h，max_h＝縮小後最高幾像素）放得下嗎：
/// 每幀估 PNG 約每像素 1.5 位元組＋縮圖 30KB，拆完還要留 KEEP_FREE；free＝None（量不到）就不擋
fn split_room(frames: u64, w: u32, h: u32, max_h: Option<u32>, free: Option<u64>) -> Result<(), String> {
    if frames > MAX_FRAMES {
        return Err(format!("這段大約 {frames} 幀，超過一次能拆的 {MAX_FRAMES} 幀（檔名只編到 6 位數）。用「從／到」分段拆，或填「每秒幾幀」抽樣"));
    }
    let oh = max_h.filter(|m| *m >= THUMB_H && *m < h).unwrap_or(h).max(1) as u64;
    let ow = (w as u64 * oh / h.max(1) as u64).max(1);
    let need = frames * (ow * oh * 3 / 2 + 30_000);
    match free {
        Some(free) if need + KEEP_FREE > free => {
            let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
            Err(format!(
                "硬碟空間不夠：這段大約 {frames} 幀，{ow}×{oh} 拆開約要 {:.1} GB，資料所在的磁碟只剩 {:.1} GB（還要留 5 GB）。\
                 可以填「最高（像素）」縮小（例如 540，空間約剩四分之一）、填「每秒幾幀」抽樣，或用「從／到」分段拆",
                gb(need),
                gb(free)
            ))
        }
        _ => Ok(()),
    }
}

impl SplitOpts {
    /// (開始秒數, 長度)；超出影片長度的部分自動截掉
    fn window(&self, duration: f64) -> Result<(f64, f64), String> {
        let start = self.start.max(0.0);
        let end = self.end.unwrap_or(duration).min(if duration > 0.0 { duration } else { f64::MAX });
        if !(start.is_finite() && end.is_finite()) || end - start <= 0.0 {
            return Err(format!("範圍不對：從 {start:.2} 秒到 {end:.2} 秒（影片 {duration:.2} 秒）"));
        }
        Ok((start, end - start))
    }

    fn is_cut(&self, duration: f64) -> bool {
        self.start > 0.0 || self.end.is_some_and(|e| e < duration)
    }
}

/// 只拆一段時放在 -i 前面的 -ss／-t（整支就不放）
fn cut_args(start: f64, len: f64, duration: f64) -> Vec<OsString> {
    let mut a = vec![];
    if start > 0.0 {
        a.extend([os("-ss"), os(format!("{start:.3}"))]);
    }
    if start + len < duration {
        a.extend([os("-t"), os(format!("{len:.3}"))]);
    }
    a
}

/// 拆幀的 ffmpeg 參數：一次讀檔，同時寫原圖 PNG 和 160 高的縮圖 JPEG
fn split_args(src: &Path, d: &Path, opts: &SplitOpts, start: f64, len: f64, duration: f64) -> Vec<OsString> {
    // 每一幀都拆：照原本的時間戳一幀不漏（passthrough）；抽樣：fps 濾鏡
    let (fps_mode, pick) = match opts.fps {
        Some(f) => (vec![], vec![format!("fps={f}")]),
        None => (vec![os("-fps_mode"), os("passthrough")], vec![]),
    };
    let chain = |extra: Option<String>| -> Vec<OsString> {
        let f: Vec<String> = pick.iter().cloned().chain(extra).collect();
        if f.is_empty() { vec![] } else { vec![os("-vf"), os(f.join(","))] }
    };
    let mut a = vec![os("-hide_banner"), os("-v"), os("error"), os("-y")];
    if !opts.cpu { a.extend([os("-hwaccel"), os("cuda")]); }
    a.extend(cut_args(start, len, duration));
    a.extend([os("-i"), src.as_os_str().to_os_string()]);
    a.extend([os("-map"), os("0:v:0")]);
    a.extend(fps_mode.clone());
    a.extend(chain(opts.max_h.filter(|h| *h >= THUMB_H).map(|h| format!("scale=-2:'min(ih,{h})'"))));
    a.push(d.join("f").join("%06d.png").into_os_string());
    a.extend([os("-map"), os("0:v:0")]);
    a.extend(fps_mode);
    a.extend(chain(Some(format!("scale=-2:{THUMB_H}"))));
    a.extend([os("-q:v"), os("4")]);
    a.push(d.join("t").join("%06d.jpg").into_os_string());
    a
}

fn split_decode(opts: &SplitOpts, root: &Path, d: &Path, mut attempt: impl FnMut(&SplitOpts) -> Result<Vec<u8>, String>) -> Result<&'static str, String> {
    Ok(match attempt(opts) {
        Ok(_) => if opts.cpu { "cpu" } else { "cuda_preferred" },
        Err(gpu_error) if !opts.cpu => {
            // 先清掉這次嘗試留下的幀，避免回退後混入殘留的尾幀。
            for sub in ["f", "t"] {
                cleanup::remove_tree(root, &d.join(sub))?;
                std::fs::create_dir(d.join(sub)).map_err(|e| e.to_string())?;
            }
            let cpu = SplitOpts { cpu: true, ..opts.clone() };
            attempt(&cpu).map_err(|e| format!("CUDA 解碼失敗：{gpu_error}；CPU 重試也失敗：{e}"))?;
            "cpu_fallback"
        }
        Err(e) => return Err(e),
    })
}

impl Frames {
    pub fn new(data: &Path) -> Frames {
        Frames {
            root: data.join("frames"),
        }
    }

    fn dir(&self, sid: &str) -> Result<PathBuf, String> {
        if !valid_sid(sid) {
            return Err("拆幀編號不對".into());
        }
        let d = self.root.join(sid);
        if crate::cache::safe_path(&self.root, &d.join("meta.json")) && d.join("meta.json").is_file() {
            Ok(d)
        } else {
            Err("找不到這次拆幀（可能移到待刪除了）".into())
        }
    }

    pub fn meta(&self, sid: &str) -> Result<Value, String> {
        let d = self.dir(sid)?;
        let b = std::fs::read(d.join("meta.json")).map_err(|e| e.to_string())?;
        serde_json::from_slice(&b).map_err(|e| format!("meta.json 壞了：{e}"))
    }

    fn save_meta(&self, sid: &str, m: &Value) -> Result<(), String> {
        let d = self.root.join(sid);
        let tmp = d.join("meta.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(m).unwrap_or_default()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, d.join("meta.json")).map_err(|e| e.to_string())
    }

    /// 拆過的影片，新的在前
    pub fn list(&self) -> Vec<Value> {
        let mut v: Vec<Value> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let sid = e.file_name().to_string_lossy().to_string();
                let m = self.meta(&sid).ok()?;
                Some(json!({"id": sid, "created": m["created"], "source": m["source"], "frames": m["frames"],
                            "fps": m["fps"], "width": m["width"], "height": m["height"], "range": m["range"],
                            "edited": m["edited"].as_object().map(|o| o.len()).unwrap_or(0)}))
            })
            .collect();
        v.sort_by(|a, b| {
            b["created"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&a["created"].as_f64().unwrap_or(0.0))
        });
        v
    }

    /// 拆成一張張 PNG＋縮圖，聲音另存。source＝{id, filename, prompt}（圖庫的那支）或 {path, filename, external:true}（外部檔）。
    /// opts：只拆 start～end 秒、原圖縮到 max_h 高、每秒只取幾幀（長片、大圖用）；硬碟不夠才擋（split_room）
    pub fn split(&self, t: &Tools, src: &Path, source: Value, opts: &SplitOpts) -> Result<Value, String> {
        let ffmpeg = t.ffmpeg()?.to_path_buf();
        let mut p = probe(t, src)?;
        let (start, len) = opts.window(p.duration)?;
        if let Some(f) = opts.fps {
            if !(f > 0.0 && f <= p.fps) {
                return Err(format!("fps 要在 0～{:.2} 之間（原片 {:.2}fps）", p.fps, p.fps));
            }
        }
        let full_fps = p.fps;
        if let Some(f) = opts.fps {
            // 抽樣：之後的秒數、接回影片都照抽樣後的 fps
            p.fps = f;
            p.fps_r = format!("{}/1000", (f * 1000.0).round() as u64);
        }
        let est = (len * p.fps).round() as u64;
        let free = crate::maint::disk_usage(&self.root).map(|(free, _)| free);
        split_room(est, p.width, p.height, opts.max_h, free)?;
        let sid = format!(
            "{}_{}",
            source["id"].as_i64().map(|i| i.to_string()).unwrap_or_else(|| "ext".into()),
            chrono::Local::now().format("%Y%m%d_%H%M%S_%f")
        );
        let d = self.root.join(&sid);
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        std::fs::create_dir(&d).map_err(|e| e.to_string())?;
        for sub in ["f", "t"] {
            std::fs::create_dir(d.join(sub)).map_err(|e| format!("建立 {} 失敗：{e}", d.display()))?;
        }
        let res = (|| -> Result<Value, String> {
            let decode = split_decode(opts, &self.root, &d, |o| run(&ffmpeg, &split_args(src, &d, o, start, len, p.duration)))?;
            let mut audio = false;
            if p.audio {
                let mut a = vec![os("-hide_banner"), os("-v"), os("error"), os("-y")];
                a.extend(cut_args(start, len, p.duration));
                a.extend([os("-i"), src.as_os_str().to_os_string(), os("-map"), os("0:a:0")]);
                // 從中間切：copy 會從前一個封包開始，聲音跟畫面差一點點；切段時重新編碼成 FLAC（無損）
                a.extend(if opts.is_cut(p.duration) { [os("-c:a"), os("flac")] } else { [os("-c"), os("copy")] });
                a.push(d.join("audio.mka").into_os_string());
                audio = run(&ffmpeg, &a).is_ok();
            }
            let frames = std::fs::read_dir(d.join("f"))
                .map_err(|e| e.to_string())?
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".png"))
                .count() as u64;
            if frames == 0 {
                return Err("一幀都沒拆出來".into());
            }
            // 有縮小的話以拆出來的實際尺寸為準（換幀時照這個尺寸裁）
            let (w, h) = image::image_dimensions(d.join("f").join(name(1, "png"))).unwrap_or((p.width, p.height));
            let m = json!({
                "id": sid, "created": crate::py::now(), "source": source,
                "width": w, "height": h, "fps": p.fps, "fps_r": p.fps_r,
                "frames": frames, "duration": frames as f64 / p.fps, "audio": audio,
                "range": {"start": start, "end": start + len, "of": p.duration, "src_w": p.width, "src_h": p.height, "src_fps": full_fps, "sampled": opts.fps.is_some()},
                "edited": {}, "rev": {}, "builds": [], "decode": decode,
            });
            self.save_meta(&sid, &m)?;
            Ok(m)
        })();
        if res.is_err() {
            let _ = cleanup::remove_tree(&self.root, &d); // 只清這次新建的資料夾，不跟隨連結
        }
        res
    }

    fn check_n(m: &Value, n: u64) -> Result<(), String> {
        let total = m["frames"].as_u64().unwrap_or(0);
        if n == 0 || n > total {
            return Err(format!("沒有第 {n} 幀（共 {total} 幀）"));
        }
        Ok(())
    }

    /// 第 n 幀的檔案：thumb＝縮圖 JPEG，否則原尺寸 PNG
    pub fn frame_path(&self, sid: &str, n: u64, thumb: bool) -> Result<PathBuf, String> {
        let d = self.dir(sid)?;
        Ok(if thumb {
            d.join("t").join(name(n, "jpg"))
        } else {
            d.join("f").join(name(n, "png"))
        })
    }

    /// 換掉第 n 幀（尺寸不同就等比放大裁切成原尺寸）；第一次換之前把原圖留在 orig\
    fn put(&self, sid: &str, n: u64, im: DynamicImage, label: String) -> Result<Value, String> {
        let mut m = self.meta(sid)?;
        Self::check_n(&m, n)?;
        let d = self.dir(sid)?;
        let (w, h) = (m["width"].as_u64().unwrap_or(0) as u32, m["height"].as_u64().unwrap_or(0) as u32);
        let im = if w > 0 && h > 0 && im.dimensions() != (w, h) {
            im.resize_to_fill(w, h, image::imageops::FilterType::Lanczos3)
        } else {
            im
        };
        let f = d.join("f").join(name(n, "png"));
        let orig = d.join("orig").join(name(n, "png"));
        if !orig.exists() {
            std::fs::create_dir_all(d.join("orig")).map_err(|e| e.to_string())?;
            std::fs::copy(&f, &orig).map_err(|e| format!("留原圖失敗：{e}"))?;
        }
        im.to_rgb8().save(&f).map_err(|e| format!("存第 {n} 幀失敗：{e}"))?;
        save_thumb(&im, &d.join("t").join(name(n, "jpg")))?;
        if !m["edited"].is_object() {
            m["edited"] = json!({});
        }
        m["edited"][n.to_string()] = json!(label);
        bump(&mut m, n);
        self.save_meta(sid, &m)?;
        Ok(m)
    }

    /// 用一張圖換掉第 n 幀
    pub fn replace(&self, sid: &str, n: u64, im: DynamicImage, label: &str) -> Result<Value, String> {
        self.put(sid, n, im, label.to_string())
    }

    /// 第 n 幀改成第 from 幀（定格：蓋掉閃一下的壞幀）
    pub fn copy_frame(&self, sid: &str, n: u64, from: u64) -> Result<Value, String> {
        let m = self.meta(sid)?;
        Self::check_n(&m, from)?;
        let im = image::open(self.frame_path(sid, from, false)?).map_err(|e| e.to_string())?;
        self.put(sid, n, im, format!("第 {from} 幀"))
    }

    /// 第 n 幀改成前後兩幀的平均（補一張壞掉的中間幀）
    pub fn blend(&self, sid: &str, n: u64) -> Result<Value, String> {
        let m = self.meta(sid)?;
        Self::check_n(&m, n)?;
        let total = m["frames"].as_u64().unwrap_or(0);
        if n == 1 || n == total {
            return Err("第一幀和最後一幀沒有前後可以平均，改用「用旁邊那幀蓋掉」".into());
        }
        let a = image::open(self.frame_path(sid, n - 1, false)?).map_err(|e| e.to_string())?.to_rgb8();
        let b = image::open(self.frame_path(sid, n + 1, false)?).map_err(|e| e.to_string())?.to_rgb8();
        if a.dimensions() != b.dimensions() {
            return Err("前後兩幀尺寸不同".into());
        }
        let mut out = a.clone();
        for (o, q) in out.pixels_mut().zip(b.pixels()) {
            for c in 0..3 {
                o[c] = (o[c] as u16 + q[c] as u16).div_ceil(2) as u8;
            }
        }
        self.put(sid, n, DynamicImage::ImageRgb8(out), format!("第 {} 和 {} 幀平均", n - 1, n + 1))
    }

    /// 第 n 幀換回原本的
    pub fn restore(&self, sid: &str, n: u64) -> Result<Value, String> {
        let mut m = self.meta(sid)?;
        Self::check_n(&m, n)?;
        let d = self.dir(sid)?;
        let orig = d.join("orig").join(name(n, "png"));
        if !orig.is_file() {
            return Err("這一幀沒有換過".into());
        }
        let f = d.join("f").join(name(n, "png"));
        std::fs::copy(&orig, &f).map_err(|e| e.to_string())?;
        let im = image::open(&f).map_err(|e| e.to_string())?;
        save_thumb(&im, &d.join("t").join(name(n, "jpg")))?;
        std::fs::remove_file(&orig).map_err(|e| e.to_string())?; // 已經複製回 f\，不會掉資料
        if let Some(o) = m["edited"].as_object_mut() {
            o.remove(&n.to_string());
        }
        bump(&mut m, n);
        self.save_meta(sid, &m)?;
        Ok(m)
    }

    /// 接回影片（H.264＋原本的聲音）寫到 out（先寫 .part 再改名）
    pub fn build(&self, t: &Tools, sid: &str, out: &Path) -> Result<(), String> {
        let m = self.meta(sid)?;
        let d = self.dir(sid)?;
        let ffmpeg = t.ffmpeg()?;
        let fps = m["fps_r"].as_str().filter(|r| rate(r).is_some()).unwrap_or("24/1").to_string();
        let audio = m["audio"].as_bool().unwrap_or(false) && d.join("audio.mka").is_file();
        let part = out.with_extension("mp4.part");
        let mut a = vec![os("-hide_banner"), os("-v"), os("error"), os("-y"), os("-framerate"), os(&fps), os("-i"), d.join("f").join("%06d.png").into_os_string()];
        if audio {
            a.extend([os("-i"), d.join("audio.mka").into_os_string()]);
        }
        a.extend([os("-map"), os("0:v:0")]);
        if audio {
            a.extend([os("-map"), os("1:a:0"), os("-c:a"), os("aac"), os("-b:a"), os("192k"), os("-shortest")]);
        }
        a.extend([
            os("-vf"), os("pad=ceil(iw/2)*2:ceil(ih/2)*2"),
            os("-c:v"), os("libx264"), os("-preset"), os("medium"), os("-crf"), os("16"), os("-pix_fmt"), os("yuv420p"),
            os("-movflags"), os("+faststart"), os("-f"), os("mp4"), part.clone().into_os_string(),
        ]);
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        if let Err(e) = run(ffmpeg, &a) {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, out).map_err(|e| format!("改名失敗：{e}"))
    }

    pub fn add_build(&self, sid: &str, image_id: i64) -> Result<(), String> {
        let mut m = self.meta(sid)?;
        if !m["builds"].is_array() {
            m["builds"] = json!([]);
        }
        if let Some(b) = m["builds"].as_array_mut() {
            b.push(json!(image_id));
        }
        self.save_meta(sid, &m)
    }

    /// 不真的刪：整個資料夾搬到 <root>\待刪除\拆幀台\
    pub fn trash(&self, sid: &str, root: &Path) -> Result<(), String> {
        let d = self.dir(sid)?;
        let to = root.join("待刪除").join("拆幀台");
        std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
        crate::maint::move_path(&d, &to.join(sid)).map_err(|e| e.to_string())
    }
}

/// 編號以外的欄位都當成 JSON 物件（給 api 回傳的 source 用）
pub fn source_of(row: &Map<String, Value>) -> Value {
    json!({"id": row.get("id"), "filename": row.get("filename"), "prompt": row.get("prompt"), "idea": row.get("idea")})
}

/// H3 長片接成一支的 ffmpeg 參數。parts＝(影片, 秒數)。畫面直接接（第七十八筆實測：接續的接縫本來就乾淨，交叉淡化沒幫助、
/// 還會讓聲音和畫面錯開）；聲音每段頭尾 15ms 淡入淡出去爆音，長度不變
pub fn chain_join_args(parts: &[(PathBuf, f64)], dst: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["-y".into(), "-hide_banner".into(), "-loglevel".into(), "error".into()];
    for (p, _) in parts {
        a.push("-i".into());
        a.push(p.into());
    }
    let mut f = String::new();
    for (i, (_, len)) in parts.iter().enumerate() {
        let out = (len - 0.015).max(0.0);
        f += &format!("[{i}:a]afade=t=in:d=0.015,afade=t=out:st={out:.3}:d=0.015[a{i}];");
    }
    for i in 0..parts.len() {
        f += &format!("[{i}:v][a{i}]");
    }
    f += &format!("concat=n={}:v=1:a=1[v][a]", parts.len());
    for s in ["-filter_complex", &f, "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-crf", "18", "-preset", "medium", "-pix_fmt", "yuv420p", "-r", "24", "-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"] {
        a.push(s.into());
    }
    a.push(dst.into());
    a
}

/// 切掉影片開頭 frames 格（畫面、聲音一起；24fps 重新編碼，格數才準）。RunningHub 版長片：每段開頭是上一段尾巴的重疊
pub fn trim_head_args(src: &Path, dst: &Path, frames: i64) -> Vec<OsString> {
    let secs = frames.max(0) as f64 / 24.0;
    let mut a: Vec<OsString> = vec!["-y".into(), "-hide_banner".into(), "-loglevel".into(), "error".into(), "-i".into(), src.into()];
    let f = format!("[0:v]trim=start_frame={},setpts=PTS-STARTPTS[v];[0:a]atrim=start={secs:.4},asetpts=PTS-STARTPTS[a]", frames.max(0));
    for s in ["-filter_complex", &f, "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-crf", "18", "-preset", "medium", "-pix_fmt", "yuv420p", "-r", "24", "-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"] {
        a.push(s.into());
    }
    a.push(dst.into());
    a
}

pub fn trim_head(t: &Tools, src: &Path, dst: &Path, frames: i64) -> Result<(), String> {
    let tmp = dst.with_extension("part.mp4");
    run(t.ffmpeg()?, &trim_head_args(src, &tmp, frames))?;
    std::fs::rename(&tmp, dst).map_err(|e| format!("存影片失敗：{e}"))
}

/// 接成一支存到 dst（先寫暫存檔，成功才改名）
pub fn join_chain(t: &Tools, parts: &[(PathBuf, f64)], dst: &Path) -> Result<(), String> {
    if parts.is_empty() {
        return Err("沒有段落可以接".into());
    }
    let tmp = dst.with_extension("part.mp4");
    run(t.ffmpeg()?, &chain_join_args(parts, &tmp))?;
    std::fs::rename(&tmp, dst).map_err(|e| format!("存長片失敗：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H3 長片接片：每段一個輸入，聲音頭尾 15ms 淡入淡出（去爆音、長度不變），畫面直接接
    #[test]
    fn chain_join_args_shape() {
        let a: Vec<String> = chain_join_args(&[(PathBuf::from("a.mp4"), 10.0), (PathBuf::from("b.mp4"), 9.5)], Path::new("out.mp4"))
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(a.iter().filter(|x| *x == "-i").count(), 2);
        let f = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(f.contains("concat=n=2:v=1:a=1") && f.contains("afade=t=out:st=9.985:d=0.015") && f.contains("afade=t=out:st=9.485:d=0.015") && f.contains("afade=t=in:d=0.015"), "{f}");
        assert!(!f.contains("xfade"));
        assert_eq!(a.last().unwrap(), "out.mp4");
    }

    #[test]
    fn trim_head_cuts_frames_and_audio_together() {
        let a: Vec<String> = trim_head_args(Path::new("in.mp4"), Path::new("out.mp4"), 22).iter().map(|s| s.to_string_lossy().to_string()).collect();
        let f = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(f.contains("trim=start_frame=22") && f.contains("atrim=start=0.9167"), "{f}");
        assert_eq!(a.last().unwrap(), "out.mp4");
    }

    #[test]
    fn rates() {
        assert_eq!(rate("24/1"), Some(24.0));
        assert!((rate("24000/1001").unwrap() - 23.976).abs() < 0.001);
        assert_eq!(rate("0/0"), None);
        assert_eq!(rate("30"), Some(30.0));
    }

    #[test]
    fn kinds_and_names() {
        assert_eq!(media_kind("a.MP4"), Some("video"));
        assert_eq!(media_kind("b.png"), Some("image"));
        assert_eq!(media_kind("c.txt"), None);
        assert_eq!(percent_decode("a%20b_%E8%B2%93.mp4"), "a b_貓.mp4");
        assert_eq!(percent_decode("100%"), "100%");
        assert!(valid_sid("2469_20261019_221500"));
        assert!(!valid_sid("..\\x"));
        assert_eq!(name(7, "png"), "000007.png");
    }

    #[test]
    fn split_range() {
        let o = |s: f64, e: Option<f64>| SplitOpts { start: s, end: e, ..Default::default() };
        assert_eq!(o(0.0, None).window(480.0), Ok((0.0, 480.0)));
        assert_eq!(o(170.0, Some(230.0)).window(480.0), Ok((170.0, 60.0)));
        assert_eq!(o(450.0, Some(999.0)).window(480.0), Ok((450.0, 30.0))); // 超過結尾就截掉
        assert!(o(500.0, None).window(480.0).is_err());
        assert!(o(10.0, Some(5.0)).window(480.0).is_err());
        assert!(!o(0.0, None).is_cut(480.0) && o(1.0, None).is_cut(480.0) && o(0.0, Some(60.0)).is_cut(480.0));
        let s = |v: Vec<OsString>| v.iter().map(|x| x.to_string_lossy().to_string()).collect::<Vec<_>>().join(" ");
        assert_eq!(s(cut_args(0.0, 480.0, 480.0)), "");
        assert_eq!(s(cut_args(170.0, 60.0, 480.0)), "-ss 170.000 -t 60.000");
        assert_eq!(s(cut_args(450.0, 30.0, 480.0)), "-ss 450.000");
        // 縮小只套在原圖（第一個輸出）；-ss／-t 在 -i 前面
        let a = s(split_args(Path::new("in.mp4"), Path::new("d"), &SplitOpts { start: 5.0, end: Some(10.0), max_h: Some(960), fps: None, cpu: false }, 5.0, 5.0, 480.0));
        assert!(a.contains("-hwaccel cuda"));
        let cpu = s(split_args(Path::new("in.mp4"), Path::new("d"), &SplitOpts { cpu: true, ..Default::default() }, 0.0, 5.0, 5.0));
        assert!(!cpu.contains("-hwaccel"));
        assert!(a.contains("-ss 5.000 -t 5.000 -i in.mp4"), "{a}");
        assert_eq!(a.matches("scale=-2:'min(ih,960)'").count(), 1, "{a}");
        assert!(!s(split_args(Path::new("in.mp4"), Path::new("d"), &SplitOpts::default(), 0.0, 480.0, 480.0)).contains("min(ih"));
        // 抽樣：兩個輸出都套 fps=，不用 passthrough
        let a = s(split_args(Path::new("in.mp4"), Path::new("d"), &SplitOpts { fps: Some(2.0), max_h: Some(480), ..Default::default() }, 0.0, 480.0, 480.0));
        assert!(a.contains("fps=2,scale=-2:'min(ih,480)'") && a.contains("fps=2,scale=-2:160") && !a.contains("passthrough"), "{a}");
    }

    #[test]
    fn cuda_failure_clears_partial_frames_before_cpu_retry() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除").join(format!("decode-fallback-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap()));
        let d = root.join("new");
        for sub in ["f", "t"] { std::fs::create_dir_all(d.join(sub)).unwrap(); }
        let mut calls = 0;
        let result = split_decode(&SplitOpts::default(), &root, &d, |opts| {
            calls += 1;
            if !opts.cpu {
                std::fs::write(d.join("f/000003.png"), b"partial").unwrap();
                std::fs::write(d.join("t/000003.jpg"), b"partial").unwrap();
                Err("CUDA unavailable".into())
            } else {
                assert!(!d.join("f/000003.png").exists());
                assert!(!d.join("t/000003.jpg").exists());
                Ok(vec![])
            }
        }).unwrap();
        assert_eq!(result, "cpu_fallback"); assert_eq!(calls, 2);
        calls = 0;
        assert!(split_decode(&SplitOpts { cpu: true, ..Default::default() }, &root, &d, |_| { calls += 1; Err("bad video".into()) }).is_err());
        assert_eq!(calls, 1);
    }

    /// 僅使用自行產生的測試影片；由開發者提供 FFmpeg，不依賴使用者資料。
    #[test]
    #[ignore = "需設定 ANIMA_TEST_FFMPEG 指向 FFmpeg 執行檔"]
    fn ffmpeg_split_smoke() {
        let ffmpeg = PathBuf::from(std::env::var_os("ANIMA_TEST_FFMPEG").expect("ANIMA_TEST_FFMPEG"));
        let ffprobe = ffmpeg.with_file_name(if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" });
        let t = Tools { node: None, ffmpeg: Some(ffmpeg.clone()), ffprobe: Some(ffprobe) };
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除").join(format!("frames-smoke-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("synthetic.mp4");
        run(&ffmpeg, &[os("-hide_banner"), os("-v"), os("error"), os("-f"), os("lavfi"), os("-i"), os("testsrc2=size=640x360:rate=24:duration=3"), os("-c:v"), os("libx264"), src.clone().into_os_string()]).unwrap();
        let f = Frames::new(&root);
        for cpu in [true, false] {
            let m = f.split(&t, &src, json!({"filename":"synthetic.mp4"}), &SplitOpts { cpu, start: 1.0, end: Some(2.0), fps: Some(6.0), max_h: Some(180) }).unwrap();
            assert_eq!(m["frames"], 6); assert_eq!(m["height"], 180); assert_eq!(m["width"], 320);
            assert_eq!(m["fps"], 6.0); assert_eq!(m["duration"], 1.0); assert_eq!(m["range"]["start"], 1.0);
            assert_eq!(image::image_dimensions(f.frame_path(m["id"].as_str().unwrap(), 1, true).unwrap()).unwrap().1, 160);
            if cpu { assert_eq!(m["decode"], "cpu"); } else { assert!(["cuda_preferred", "cpu_fallback"].contains(&m["decode"].as_str().unwrap())); }
            eprintln!("synthetic split: cpu={cpu}, decode={}, frames={}", m["decode"], m["frames"]);
        }
        assert!(src.is_file());
    }

    /// 沒有固定 3600 幀上限了：只看檔名能編到幾號（%06d）和硬碟夠不夠
    #[test]
    fn split_room_by_disk() {
        const GB: u64 = 1 << 30;
        assert!(split_room(3601, 1280, 720, None, Some(1000 * GB)).is_ok());
        assert!(split_room(100_000, 1280, 720, None, Some(1000 * GB)).is_ok());
        assert!(split_room(100_000, 1280, 720, None, None).is_ok(), "量不到剩餘空間就不擋");
        assert!(split_room(1_000_000, 64, 64, None, Some(1000 * GB)).is_err(), "檔名只編到 999999");
        // 1080p 約 3MB 一張：兩萬張約 60GB，剩 50GB 不夠；縮到 540 高就夠
        let e = split_room(20_000, 1920, 1080, None, Some(50 * GB)).unwrap_err();
        assert!(e.contains("GB") && e.contains("最高"), "{e}");
        assert!(split_room(20_000, 1920, 1080, Some(540), Some(50 * GB)).is_ok());
    }

}

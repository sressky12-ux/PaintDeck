//! 混音台：呼叫小克的調音台工具 mixdesk（`D:\AI\tools\mixdesk`，Python 指令列）。
//! 設計 docs/superpowers/specs/2026-11-04-mixdesk-panel-design.md。面板不自己解析譜檔，一律透過 mixdesk 的 JSON 指令
//! （inspect／apply／render --json／instruments --json／audition）。這裡負責：找到 mixdesk、跑指令、
//! 渲染排隊（一次一件、可取消）、限制網頁能讀的檔案只在 mixdesk 資料夾底下。
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

pub const DEFAULT_DIR: &str = r"D:\AI\tools\mixdesk";
/// 網頁可以讀的檔案種類（音檔、頻譜圖、報告、影片）
const SERVE_EXT: [&str; 6] = ["wav", "png", "json", "mp4", "webm", "mov"];
/// 渲染最久等多久（長曲＋台詞 TTS）
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// mixdesk 資料夾：設定的 mixdesk_dir → 環境變數 ANIMA_MIXDESK（測試用）→ 預設（`fallback`）
pub fn locate_from(setting: Option<&str>, env: Option<String>, fallback: PathBuf) -> PathBuf {
    if let Some(s) = setting.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    if let Some(e) = env.filter(|e| !e.trim().is_empty()) {
        return PathBuf::from(e.trim());
    }
    fallback
}

/// 沒設定時的預設：`D:\AI\tools\mixdesk`，不在就找「資料資料夾旁邊」的 `tools\mixdesk`
/// （settings.json 在 `<資料資料夾>\data\` 底下；資料放 `F:\AI\anima_data` 的人就是 `F:\AI\tools\mixdesk`）。
/// 2026-11-17：使用者的東西從 D 槽搬到 F 槽，混音台一直說找不到 mixdesk。兩個都不在就回 D 槽那個（錯誤訊息照舊講它）
pub fn default_dir(settings_file: &Path) -> PathBuf {
    let def = PathBuf::from(DEFAULT_DIR);
    if def.exists() {
        return def;
    }
    settings_file.ancestors().nth(3).map(|p| p.join("tools").join("mixdesk")).filter(|p| p.exists()).unwrap_or(def)
}

pub fn locate(settings: &Map<String, Value>, settings_file: &Path) -> PathBuf {
    locate_from(settings.get("mixdesk_dir").and_then(Value::as_str), std::env::var("ANIMA_MIXDESK").ok(), default_dir(settings_file))
}

#[derive(Clone, Debug, Default)]
pub struct Job {
    pub song: String,
    pub state: String,
    pub events: Vec<Value>,
    pub error: String,
    pub out_dir: String,
}

impl Job {
    fn view(&self, id: &str) -> Value {
        json!({"id": id, "song": self.song, "state": self.state, "events": self.events,
               "error": self.error, "out_dir": self.out_dir})
    }
}

pub struct Mixdesk {
    settings: PathBuf,
    /// 測試用：固定資料夾與程式（正式執行時都是 None，每次照設定找）
    fixed_dir: Option<PathBuf>,
    program: Option<(PathBuf, Vec<OsString>)>,
    jobs: Mutex<HashMap<String, Job>>,
    stops: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    run_lock: tokio::sync::Mutex<()>,
    seq: AtomicU64,
    /// 譜檔 → 它配的影片（inspect 時記下來，影片要一直被要 Range，不能每次都跑 Python）
    videos: Mutex<HashMap<String, String>>,
    /// 樂器目錄（mixdesk 有更新要重開站才會重讀）
    instruments: Mutex<Option<Value>>,
}

/// 檔案修改時間（秒，含小數）；讀不到回 0
pub fn mtime(p: &Path) -> f64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 子程序不要跳黑窗；輸出一律 UTF-8
fn prepare(c: &mut tokio::process::Command) {
    c.env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1").kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
}

/// stderr 去掉 qwen-tts 匯入時的雜訊，只留真正的錯誤訊息
fn clean_err(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| !l.contains("SoX could not be found") && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn last_json(stdout: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .rev()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .find(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str(l).ok())
}

impl Mixdesk {
    pub fn new(settings: PathBuf) -> Self {
        Mixdesk {
            settings,
            fixed_dir: None,
            program: None,
            jobs: Mutex::new(HashMap::new()),
            stops: Mutex::new(HashMap::new()),
            run_lock: tokio::sync::Mutex::new(()),
            seq: AtomicU64::new(1),
            videos: Mutex::new(HashMap::new()),
            instruments: Mutex::new(None),
        }
    }

    fn key(song: &Path) -> String {
        song.canonicalize().unwrap_or_else(|_| song.to_path_buf()).display().to_string()
    }

    /// inspect 完記下這首配的影片
    pub fn remember_video(&self, song: &Path, inspect: &Value) {
        let mut v = self.videos.lock().unwrap();
        match inspect["video"]["path"].as_str().filter(|s| !s.is_empty()) {
            Some(path) => v.insert(Self::key(song), path.to_string()),
            None => v.remove(&Self::key(song)),
        };
    }

    pub fn video_of(&self, song: &Path) -> Option<String> {
        self.videos.lock().unwrap().get(&Self::key(song)).cloned()
    }

    pub async fn instruments(&self) -> Result<Value, String> {
        if let Some(v) = self.instruments.lock().unwrap().clone() {
            return Ok(v);
        }
        let v = self.json(vec!["instruments".into(), "--json".into()], Duration::from_secs(120)).await?;
        *self.instruments.lock().unwrap() = Some(v.clone());
        Ok(v)
    }

    /// 測試用：固定 mixdesk 資料夾
    pub fn with_dir(dir: PathBuf) -> Self {
        Mixdesk { fixed_dir: Some(dir), ..Mixdesk::new(PathBuf::new()) }
    }

    /// 測試用：把 mixdesk.exe 換成別的程式（prefix_args 會放在指令參數前面）
    pub fn with_program(dir: PathBuf, program: PathBuf, prefix_args: Vec<OsString>) -> Self {
        Mixdesk { program: Some((program, prefix_args)), ..Mixdesk::with_dir(dir) }
    }

    pub fn dir(&self) -> PathBuf {
        match &self.fixed_dir {
            Some(d) => d.clone(),
            None => locate(&crate::settings::load(&self.settings), &self.settings),
        }
    }

    pub fn exe(&self) -> PathBuf {
        self.dir().join(".venv").join("Scripts").join("mixdesk.exe")
    }

    /// 找不到 mixdesk 時，回傳給使用者看的說明（不當機、不噴英文錯誤）
    pub fn check(&self) -> Result<(), String> {
        if self.program.is_some() {
            return Ok(());
        }
        let exe = self.exe();
        if exe.is_file() {
            Ok(())
        } else {
            Err(format!(
                "找不到 mixdesk：{} 不存在。請到設定的 mixdesk_dir 填 mixdesk 資料夾（預設 {}），並確認裡面的 .venv 裝好了。",
                exe.display(),
                DEFAULT_DIR
            ))
        }
    }

    fn command(&self, args: &[OsString]) -> tokio::process::Command {
        let (prog, prefix) = self.program.clone().unwrap_or_else(|| (self.exe(), vec![]));
        let mut c = tokio::process::Command::new(prog);
        c.args(prefix).args(args).current_dir(self.dir());
        prepare(&mut c);
        c
    }

    /// 跑一個 mixdesk 指令，回傳 stdout 最後一行的 JSON；失敗回 stderr 的繁中訊息。參數不經 shell，中文、空白照原樣傳。
    pub async fn json(&self, args: Vec<OsString>, timeout: Duration) -> Result<Value, String> {
        self.check()?;
        let mut c = self.command(&args);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = c.spawn().map_err(|e| format!("執行 mixdesk 失敗：{e}"))?;
        let out = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| format!("mixdesk 超過 {} 秒沒有回應", timeout.as_secs()))?
            .map_err(|e| format!("等 mixdesk 失敗：{e}"))?;
        if !out.status.success() {
            let err = clean_err(&out.stderr);
            return Err(if err.is_empty() { format!("mixdesk 失敗（{}）", out.status) } else { err });
        }
        last_json(&out.stdout).ok_or_else(|| "mixdesk 的輸出看不懂（不是 JSON）".to_string())
    }

    /// 網頁要讀的檔案：必須在 mixdesk 資料夾底下，而且是音檔／圖／報告／影片
    pub fn allowed(&self, p: &Path) -> bool {
        let ext = p.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
        if !SERVE_EXT.contains(&ext.as_str()) {
            return false;
        }
        match (p.canonicalize(), self.dir().canonicalize()) {
            (Ok(f), Ok(d)) => f.starts_with(&d) && f.is_file(),
            _ => false,
        }
    }

    /// 譜檔路徑：要在 mixdesk 資料夾底下的 .yaml
    pub fn song_path(&self, p: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(p);
        let ok_ext = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("yaml"));
        match (path.canonicalize(), self.dir().canonicalize()) {
            (Ok(f), Ok(d)) if ok_ext && f.starts_with(&d) && f.is_file() => Ok(path),
            _ => Err(format!("譜檔要是 mixdesk 資料夾底下的 .yaml：{p}")),
        }
    }

    /// 曲子清單：songs\ 底下（含子資料夾，跳過輸出的 out\）與 examples\ 的 .yaml
    pub fn songs(&self) -> Result<Vec<Value>, String> {
        self.check()?;
        let dir = self.dir();
        let mut out = vec![];
        fn walk(d: &Path, depth: u32, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if depth < 3 && e.file_name() != "out" {
                        walk(&p, depth + 1, out);
                    }
                } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("yaml")) {
                    out.push(p);
                }
            }
        }
        for (kind, d, depth) in [("songs", dir.join("songs"), 0), ("examples", dir.join("examples"), 3)] {
            let mut found = vec![];
            walk(&d, depth, &mut found);
            found.sort();
            for p in found {
                out.push(json!({
                    "path": p.display().to_string(),
                    "name": p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                    "dir": kind, "mtime": mtime(&p),
                }));
            }
        }
        Ok(out)
    }

    /// 新曲子：songs\<標題>\<標題>.yaml（同名就加編號），一軌鋼琴、一段空白
    pub fn new_song(&self, title: &str, tempo: f64, time: &str) -> Result<PathBuf, String> {
        self.check()?;
        let safe: String = title
            .trim()
            .chars()
            .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
            .collect();
        let safe = if safe.trim().is_empty() { "新曲子".to_string() } else { safe.trim().to_string() };
        if !(tempo > 0.0 && tempo <= 400.0) {
            return Err("tempo 要在 1～400 之間".into());
        }
        if !time.split_once('/').is_some_and(|(a, b)| a.parse::<u32>().is_ok_and(|n| n > 0) && ["1", "2", "4", "8", "16"].contains(&b)) {
            return Err(format!("拍號「{time}」格式不對，要像 4/4、3/4、6/8"));
        }
        let songs = self.dir().join("songs");
        let mut name = safe.clone();
        let mut n = 2;
        while songs.join(&name).exists() {
            name = format!("{safe} {n}");
            n += 1;
        }
        let d = songs.join(&name);
        std::fs::create_dir_all(&d).map_err(|e| format!("建資料夾失敗：{e}"))?;
        let p = d.join(format!("{name}.yaml"));
        let text = format!(
            "title: {}\ntempo: {tempo}\ntime: {time}\ntracks:\n  - name: 鋼琴\n    instrument: 鋼琴\n    parts:\n      - {{at: 1, notes: \"r/4\"}}\n",
            serde_json::to_string(&name).unwrap_or_default()
        );
        std::fs::write(&p, text).map_err(|e| format!("寫譜檔失敗：{e}"))?;
        Ok(p)
    }

    /// 開始渲染（背景跑 `render <譜檔> --json`，一次只跑一個，其他排隊），回傳工作編號
    pub fn start_render(self: &Arc<Self>, song: PathBuf) -> String {
        let id = format!("md{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let stop = Arc::new(tokio::sync::Notify::new());
        self.jobs.lock().unwrap().insert(
            id.clone(),
            Job { song: song.display().to_string(), state: "queued".into(), ..Default::default() },
        );
        self.stops.lock().unwrap().insert(id.clone(), stop.clone());
        let me = self.clone();
        let jid = id.clone();
        tokio::spawn(async move {
            let _turn = me.run_lock.lock().await;
            let stopped = me.jobs.lock().unwrap().get(&jid).is_some_and(|j| j.state == "stopped");
            if !stopped {
                let r = me.run_render(&jid, &song, &stop).await;
                me.update(&jid, |j| match r {
                    Ok(()) if j.state == "running" => j.state = "done".into(),
                    Ok(()) => {}
                    Err(e) => {
                        j.state = "error".into();
                        j.error = e;
                    }
                });
            }
            me.stops.lock().unwrap().remove(&jid);
        });
        id
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.lock().unwrap().get_mut(id) {
            f(j);
        }
    }

    async fn run_render(&self, id: &str, song: &Path, stop: &tokio::sync::Notify) -> Result<(), String> {
        self.check()?;
        self.update(id, |j| j.state = "running".into());
        let mut c = self.command(&[OsString::from("render"), song.as_os_str().to_owned(), OsString::from("--json")]);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("執行 mixdesk 失敗：{e}"))?;
        let stdout = child.stdout.take().ok_or("讀不到 mixdesk 的輸出")?;
        let mut stderr = child.stderr.take().ok_or("讀不到 mixdesk 的錯誤輸出")?;
        let err_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf).await.ok();
            buf
        });
        let mut lines = BufReader::new(stdout).lines();
        let read = async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(ev) = serde_json::from_str::<Value>(line.trim()) {
                    self.update(id, |j| {
                        if ev["event"] == "done" {
                            j.out_dir = ev["out_dir"].as_str().unwrap_or_default().to_string();
                        }
                        j.events.push(ev);
                    });
                }
            }
            child.wait().await
        };
        let status = tokio::select! {
            s = tokio::time::timeout(RENDER_TIMEOUT, read) => match s {
                Ok(s) => s.map_err(|e| format!("等 mixdesk 失敗：{e}"))?,
                Err(_) => return Err(format!("渲染超過 {} 分鐘，已經停掉", RENDER_TIMEOUT.as_secs() / 60)),
            },
            _ = stop.notified() => {
                self.update(id, |j| j.state = "stopped".into());
                return Ok(()); // child 在這裡被 drop，kill_on_drop 會殺掉它
            }
        };
        let err = clean_err(&err_task.await.unwrap_or_default());
        if status.success() {
            Ok(())
        } else {
            Err(if err.is_empty() { format!("渲染失敗（{status}）") } else { err })
        }
    }

    pub fn job(&self, id: &str) -> Option<Value> {
        self.jobs.lock().unwrap().get(id).map(|j| j.view(id))
    }

    /// 停掉渲染（排隊中的直接取消）；回傳有沒有這個工作
    pub fn stop(&self, id: &str) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(j) = jobs.get_mut(id) else { return false };
        if j.state == "queued" {
            j.state = "stopped".into();
        }
        drop(jobs);
        if let Some(n) = self.stops.lock().unwrap().get(id) {
            n.notify_one();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("anima_mixdesk_test").join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn locate_prefers_setting_then_env() {
        let fb = || PathBuf::from(DEFAULT_DIR);
        assert_eq!(locate_from(Some(r"E:\x"), Some(r"F:\y".into()), fb()), PathBuf::from(r"E:\x"));
        assert_eq!(locate_from(Some("  "), Some(r"F:\y".into()), fb()), PathBuf::from(r"F:\y"));
        assert_eq!(locate_from(None, None, fb()), PathBuf::from(DEFAULT_DIR));
    }

    #[test]
    fn default_dir_finds_tools_next_to_data_folder() {
        // <base>\anima_data\data\settings.json → <base>\tools\mixdesk（D 槽預設不在的時候）
        let base = tmp("default_dir");
        let settings = base.join("anima_data").join("data").join("settings.json");
        if PathBuf::from(DEFAULT_DIR).exists() {
            assert_eq!(default_dir(&settings), PathBuf::from(DEFAULT_DIR));
            return;
        }
        assert_eq!(default_dir(&settings), PathBuf::from(DEFAULT_DIR), "旁邊沒有 tools\\mixdesk 就回 D 槽預設");
        std::fs::create_dir_all(base.join("tools").join("mixdesk")).unwrap();
        assert_eq!(default_dir(&settings), base.join("tools").join("mixdesk"));
    }

    #[test]
    fn check_missing_dir_is_chinese_message() {
        let m = Mixdesk::with_dir(tmp("missing").join("沒有這個資料夾"));
        let e = m.check().unwrap_err();
        assert!(e.contains("mixdesk_dir") && e.contains("找不到"), "{e}");
        assert!(!e.contains("os error"), "{e}");
    }

    #[test]
    fn allowed_rejects_outside_and_dotdot() {
        let d = tmp("allowed");
        let out = d.join("songs").join("a").join("out").join("x");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("mix.wav"), b"RIFF").unwrap();
        std::fs::write(d.join("tool.exe"), b"MZ").unwrap();
        let outside = tmp("allowed_outside").join("x.wav");
        std::fs::write(&outside, b"RIFF").unwrap();
        let m = Mixdesk::with_dir(d.clone());
        assert!(m.allowed(&out.join("mix.wav")));
        assert!(!m.allowed(&d.join("tool.exe")));
        assert!(!m.allowed(&outside));
        assert!(!m.allowed(&d.join("songs").join("..").join("..").join("allowed_outside").join("x.wav")));
        assert!(!m.allowed(&d.join("nope.wav")));
    }

    #[test]
    fn song_path_must_be_yaml_inside() {
        let d = tmp("songpath");
        std::fs::write(d.join("a.yaml"), "title: a\n").unwrap();
        std::fs::write(d.join("a.txt"), "x").unwrap();
        let m = Mixdesk::with_dir(d.clone());
        assert!(m.song_path(d.join("a.yaml").to_str().unwrap()).is_ok());
        assert!(m.song_path(d.join("a.txt").to_str().unwrap()).is_err());
        assert!(m.song_path(r"C:\Windows\win.ini").is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn json_passes_non_ascii_args() {
        // 用 PowerShell 當假的 mixdesk：把收到的參數原樣吐回成 JSON
        let d = tmp("args");
        let ps1 = d.join("echo.ps1");
        std::fs::write(&ps1, "\u{feff}[Console]::OutputEncoding = [Text.Encoding]::UTF8\n$args | ConvertTo-Json -Compress\n").unwrap();
        let m = Mixdesk::with_program(
            d.clone(),
            PathBuf::from("powershell"),
            vec!["-NoProfile".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), ps1.into_os_string()],
        );
        let v = m
            .json(vec!["inspect".into(), r"D:\AI\小克工作區\音樂\我的 曲子.yaml".into()], Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(v, json!(["inspect", r"D:\AI\小克工作區\音樂\我的 曲子.yaml"]));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn json_failure_returns_stderr_text() {
        let d = tmp("fail");
        let ps1 = d.join("fail.ps1");
        std::fs::write(&ps1, "\u{feff}[Console]::OutputEncoding = [Text.Encoding]::UTF8\n[Console]::Error.WriteLine('SoX could not be found!')\n[Console]::Error.WriteLine('錯誤：譜檔壞了')\nexit 2\n").unwrap();
        let m = Mixdesk::with_program(
            d.clone(),
            PathBuf::from("powershell"),
            vec!["-NoProfile".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), ps1.into_os_string()],
        );
        let e = m.json(vec!["inspect".into()], Duration::from_secs(60)).await.unwrap_err();
        assert!(e.contains("譜檔壞了") && !e.contains("SoX"), "{e}");
    }
}

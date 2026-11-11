//! 訓練台的「在哪煉」這一半：本機訓練器的安裝、跟訓練小幫手（resources\train\agent.py）講話、RunPod 訓練機、C 站訓練 API、驗收。
//! 專案／訓練圖／描述在 api_train.rs；純邏輯在 crate::train。
//!
//! - GET  /api/train/trainer[?size=1]、POST /api/train/trainer/install、POST /api/train/trainer/remove {confirm}
//! - POST /api/train/{id}/quote {target, gpu?, upload_ok?}：報價（雲端＝估計；C 站＝試算，會先上傳訓練圖）
//! - POST /api/train/{id}/start {target, confirm?, model?, url?, token?, gpu?, max_cost?}
//! - GET  /api/train/{id}/sample/{file}
//! - POST /api/train/{id}/compare {params, strengths?, epochs?}：對照表（每一版 × 每個強度，同種子）
//! - POST /api/train/{id}/adopt {epoch, strength}：就用這版（寫 LoRA 資訊）
use super::api_train::{emit, fam_of, job_begin, job_end, job_kind, live, load, pdir, read_caption, training_id, update, vram_gb};
use super::{bad, blocking, file_response, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::director::tx;
use crate::train::{self, Fam};
use axum::extract::{Path as UrlPath, Query, State};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

const CIVITAI_API: &str = "https://orchestration.civitai.com/v2/consumer";
const CIVITAI_DONE: [&str; 4] = ["succeeded", "failed", "expired", "canceled"];
const RUNPOD_POD: &str = "anima-train";
const RUNPOD_PORT: u16 = 8000;
/// 花錢的地方：開始前一定要 confirm
const PAID: [&str; 3] = ["runpod", "civitai", "modelscope"];

// ================================================================ 本機訓練器

#[derive(Default)]
struct Install {
    busy: bool,
    log: Vec<String>,
    error: String,
}

static INSTALL: LazyLock<Mutex<Install>> = LazyLock::new(|| Mutex::new(Install::default()));

fn inst() -> std::sync::MutexGuard<'static, Install> {
    INSTALL.lock().unwrap_or_else(|e| e.into_inner())
}

fn trainer_dir(app: &Shared) -> PathBuf {
    app.paths.root.join("trainer")
}

fn toolkit_dir(app: &Shared) -> PathBuf {
    trainer_dir(app).join("ai-toolkit")
}

/// 訓練器自己的 Python（ai-toolkit 的安裝管理器建在它資料夾裡的 .venv）
fn venv_python(toolkit: &Path) -> Option<PathBuf> {
    [".venv", "venv"]
        .iter()
        .map(|v| if cfg!(windows) { toolkit.join(v).join("Scripts").join("python.exe") } else { toolkit.join(v).join("bin").join("python") })
        .find(|p| p.is_file())
}

fn installed(app: &Shared) -> bool {
    trainer_dir(app).join("installed.json").is_file() && venv_python(&toolkit_dir(app)).is_some()
}

pub(super) fn trainer_view(app: &Shared) -> Value {
    let i = inst();
    json!({
        "installed": installed(app), "busy": i.busy, "error": i.error, "log": i.log.iter().rev().take(15).rev().collect::<Vec<_>>(),
        "commit": train::TOOLKIT_COMMIT, "dir": trainer_dir(app).to_string_lossy(), "download_gb": 8,
    })
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

pub async fn trainer(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let mut v = trainer_view(&app);
    if q.contains_key("size") {
        let d = trainer_dir(&app);
        v["size_gb"] = json!((blocking(move || dir_size(&d)).await as f64 / 1e9 * 10.0).round() / 10.0);
    }
    ok(v)
}

fn quiet(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.stdin(Stdio::null()).env_remove("PYTHONPATH").env_remove("PYTHONHOME");
}

/// 找一顆能跑 ai-toolkit 安裝管理器的 Python（只用標準庫、3.8 以上就行；它會自己另外建環境，不會裝東西進這顆）
async fn bootstrap_python(app: &Shared) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("ANIMA_TRAIN_PYTHON") {
        cands.push(p.into());
    }
    let pref = crate::gpu::prefs().python;
    if !pref.trim().is_empty() {
        cands.push(pref.trim().into());
    }
    if let Some(up) = app.paths.comfy_root.parent() {
        cands.push(up.join("python_embeded").join(crate::paths::exe("python")));
    }
    cands.push(app.paths.comfy_root.join(".venv").join(if cfg!(windows) { "Scripts" } else { "bin" }).join(crate::paths::exe("python")));
    cands.extend(["python3", "python", "py"].map(PathBuf::from));
    for c in cands {
        let mut cmd = tokio::process::Command::new(&c);
        cmd.args(["-c", "import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)"]).stdout(Stdio::null()).stderr(Stdio::null());
        quiet(&mut cmd);
        if matches!(tokio::time::timeout(Duration::from_secs(20), cmd.status()).await, Ok(Ok(s)) if s.success()) {
            return Some(c);
        }
    }
    None
}

fn say(app: &Shared, line: impl Into<String>) {
    let line = line.into();
    {
        let mut i = inst();
        i.log.push(line.clone());
        let n = i.log.len();
        if n > 400 {
            i.log.drain(..n - 400);
        }
    }
    app.hub.send(json!({"type": "train", "id": "", "phase": "install", "state": "running", "msg": line}));
}

/// 跑一個指令，把它印的每一行轉給網頁
async fn run_logged(app: &Shared, mut cmd: tokio::process::Command) -> Result<(), String> {
    quiet(&mut cmd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("開不了：{e}"))?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    let pump = |app: Shared, r: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| async move {
        let Some(r) = r else { return };
        let mut lines = BufReader::new(r).split(b'\n');
        while let Ok(Some(l)) = lines.next_segment().await {
            // 進度條用 \r 重畫同一行：只留最後一段
            let s = String::from_utf8_lossy(&l);
            if let Some(last) = s.split('\r').map(str::trim).rfind(|x| !x.is_empty()) {
                say(&app, last.chars().take(300).collect::<String>());
            }
        }
    };
    let a = tokio::spawn(pump(app.clone(), out.map(|o| Box::new(o) as _)));
    let b = tokio::spawn(pump(app.clone(), err.map(|o| Box::new(o) as _)));
    let st = child.wait().await.map_err(|e| e.to_string())?;
    let _ = tokio::join!(a, b);
    if st.success() {
        Ok(())
    } else {
        Err(format!("指令失敗（代碼 {}），往上看最後幾行訊息", st.code().unwrap_or(-1)))
    }
}

const SITE_FIX: &str = "import glob, locale, os, sys
for pat in ('Lib/site-packages/sitecustomize.py', 'lib/python*/site-packages/sitecustomize.py'):
    for p in glob.glob(os.path.join(sys.argv[1], pat)):
        b = open(p, 'rb').read()
        try:
            b.decode('utf-8')
        except UnicodeDecodeError:
            open(p, 'w', encoding='utf-8', newline='').write(b.decode(locale.getpreferredencoding(False)))
            print('sitecustomize.py -> UTF-8')
";

async fn install_run(app: &Shared) -> Result<(), String> {
    let dir = trainer_dir(app);
    let kit = toolkit_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建不了訓練器資料夾：{e}"))?;
    let py = bootstrap_python(app).await.ok_or("找不到可以用的 Python（3.8 以上）。繪匣裝好 ComfyUI 就會有；或設環境變數 ANIMA_TRAIN_PYTHON 指到 python.exe")?;
    if !kit.join("run.py").is_file() {
        say(app, "下載 ai-toolkit（約 36MB）…");
        let url = format!("https://codeload.github.com/ostris/ai-toolkit/zip/{}", train::TOOLKIT_COMMIT);
        let http = reqwest::Client::builder().proxy(crate::net::proxy()).build().map_err(|e| e.to_string())?;
        let bytes = http
            .get(&url)
            .timeout(Duration::from_secs(900))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("下載 ai-toolkit 失敗：{}", crate::net::explain(&e, &url)))?
            .bytes()
            .await
            .map_err(|e| format!("下載 ai-toolkit 中斷：{}", e.without_url()))?;
        say(app, "解壓縮…");
        let (d, k) = (dir.clone(), kit.clone());
        blocking(move || -> Result<(), String> {
            let tmp = d.join("_unpack");
            let _ = std::fs::remove_dir_all(&tmp);
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).and_then(|mut z| z.extract(&tmp)).map_err(|e| format!("解壓縮失敗：{e}"))?;
            let inner = tmp.join(format!("ai-toolkit-{}", train::TOOLKIT_COMMIT));
            let _ = std::fs::remove_dir_all(&k);
            std::fs::rename(&inner, &k).map_err(|e| format!("搬不動 ai-toolkit：{e}"))?;
            let _ = std::fs::remove_dir_all(&tmp);
            Ok(())
        })
        .await?;
    }
    say(app, "安裝訓練環境（PyTorch 和相依套件，約 6～8GB，要十幾分鐘到半小時）…");
    let mut cmd = tokio::process::Command::new(&py);
    cmd.args(["-m", "manager", "install"]).current_dir(&kit).env("PYTHONUNBUFFERED", "1").env("PYTHONIOENCODING", "utf-8");
    run_logged(app, cmd).await?;
    let vpy = venv_python(&kit).ok_or("安裝跑完了，但找不到訓練器的 Python（.venv）")?;
    // 安裝管理器會在環境裡寫一支 sitecustomize.py（掛 ffmpeg 的路徑）。安裝路徑有中文時它用系統編碼寫，
    // Python 當 UTF-8 讀不了：每次啟動都噴錯、ffmpeg 也沒掛上。轉成 UTF-8（-S＝先不要載入它）
    let mut cmd = tokio::process::Command::new(&vpy);
    cmd.args(["-I", "-S", "-c", SITE_FIX]).arg(vpy.parent().and_then(Path::parent).unwrap_or(&kit));
    run_logged(app, cmd).await?;
    say(app, "檢查 PyTorch 看不看得到顯卡…");
    let mut cmd = tokio::process::Command::new(&vpy);
    cmd.args(["-c", "import torch; print('torch', torch.__version__, 'cuda', torch.cuda.is_available())"]).current_dir(&kit);
    run_logged(app, cmd).await?;
    crate::director::write_atomic(&dir.join("installed.json"), &json!({"commit": train::TOOLKIT_COMMIT, "at": chrono::Utc::now().timestamp()})).map_err(|e| e.to_string())?;
    say(app, "訓練器裝好了");
    Ok(())
}

pub async fn trainer_install(State(app): State<Shared>) -> ApiResult {
    {
        let mut i = inst();
        if i.busy {
            return Err(bad("訓練器正在安裝"));
        }
        *i = Install { busy: true, ..Install::default() };
    }
    let app2 = app.clone();
    tokio::spawn(async move {
        let r = install_run(&app2).await;
        let mut i = inst();
        i.busy = false;
        let (state, msg) = match r {
            Ok(()) => ("done", String::new()),
            Err(e) => {
                i.error.clone_from(&e);
                ("error", e)
            }
        };
        drop(i);
        app2.hub.send(json!({"type": "train", "id": "", "phase": "install", "state": state, "msg": msg}));
    });
    ok(json!({"ok": true}))
}

pub async fn trainer_remove(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    if b["confirm"].as_bool() != Some(true) {
        return Err(bad("移除訓練器要帶 confirm: true（會刪掉訓練器和它下載的底模，之後要用得重裝；煉好的 LoRA 和訓練專案不會動）"));
    }
    if inst().busy || training_id().is_some() {
        return Err(bad("訓練器正在安裝或訓練中，先等它結束"));
    }
    let d = trainer_dir(&app);
    blocking(move || std::fs::remove_dir_all(&d)).await.or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(internal(format!("刪不掉：{e}"))) })?;
    ok(json!({"ok": true}))
}

// ================================================================ 跟小幫手講話

struct Conn {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Conn {
    fn new(base: &str, token: &str, local: bool) -> Result<Conn, String> {
        let b = reqwest::Client::builder().connect_timeout(Duration::from_secs(15));
        let b = if local { b.no_proxy() } else { b.proxy(crate::net::proxy()) };
        Ok(Conn { base: base.trim_end_matches('/').to_string(), token: token.to_string(), http: b.build().map_err(|e| e.to_string())? })
    }

    async fn send(&self, rb: reqwest::RequestBuilder, secs: u64) -> Result<reqwest::Response, String> {
        let r = rb.header("X-Train-Token", &self.token).timeout(Duration::from_secs(secs)).send().await.map_err(|e| format!("連不到訓練機：{}", e.without_url()))?;
        if r.status().is_success() {
            return Ok(r);
        }
        let code = r.status().as_u16();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        Err(match (code, body["error"].as_str()) {
            (401, _) => "訓練機說口令不對".to_string(),
            (_, Some(e)) => format!("訓練機回應 {code}：{e}"),
            _ => format!("訓練機回應 {code}"),
        })
    }

    async fn status(&self) -> Result<Value, String> {
        self.send(self.http.get(format!("{}/status", self.base)), 30).await?.json().await.map_err(|e| e.without_url().to_string())
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        self.send(self.http.post(format!("{}{path}", self.base)).json(body), 60).await?.json().await.map_err(|e| e.without_url().to_string())
    }

    async fn download(&self, name: &str, dest: &Path) -> Result<(), String> {
        let mut r = self.send(self.http.get(format!("{}/file", self.base)).query(&[("name", name)]), 3600).await?;
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let part = dest.with_extension("part");
        let mut f = tokio::fs::File::create(&part).await.map_err(|e| format!("寫不了 {}：{e}", dest.display()))?;
        while let Some(chunk) = r.chunk().await.map_err(|e| format!("下載中斷：{}", e.without_url()))? {
            tokio::io::AsyncWriteExt::write_all(&mut f, &chunk).await.map_err(|e| e.to_string())?;
        }
        tokio::io::AsyncWriteExt::flush(&mut f).await.map_err(|e| e.to_string())?;
        drop(f);
        std::fs::rename(&part, dest).map_err(|e| format!("存不了 {}：{e}", dest.display()))
    }
}

/// 把訓練圖和描述包成 zip（圖已經是壓過的格式，不再壓）
fn pack_dataset(dir: &Path, p: &Value, dest: &Path) -> Result<usize, String> {
    use std::io::Write;
    let f = std::fs::File::create(dest).map_err(|e| format!("建不了壓縮檔：{e}"))?;
    let mut z = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut n = 0;
    for file in live(p).iter().filter_map(|i| i["file"].as_str()) {
        let data = std::fs::read(dir.join("images").join(file)).map_err(|e| format!("讀不到訓練圖 {file}：{e}"))?;
        z.start_file(file, opt).map_err(|e| e.to_string())?;
        z.write_all(&data).map_err(|e| e.to_string())?;
        z.start_file(Path::new(file).with_extension("txt").to_string_lossy(), opt).map_err(|e| e.to_string())?;
        z.write_all(read_caption(dir, file).as_bytes()).map_err(|e| e.to_string())?;
        n += 1;
    }
    z.finish().map_err(|e| e.to_string())?;
    Ok(n)
}

async fn set_run(app: &Shared, id: &str, f: impl FnOnce(&mut Map<String, Value>)) {
    let _ = update(app, id, |p| {
        if !p["run"].is_object() {
            p["run"] = json!({});
        }
        if let Some(r) = p["run"].as_object_mut() {
            f(r);
        }
        Ok(())
    })
    .await;
}

fn lora_rel(fam: &Fam, name: &str, n: u32) -> String {
    let file = format!("{name}_e{n}.safetensors");
    if fam.lora_sub.is_empty() {
        format!("{name}\\{file}")
    } else {
        format!("{}\\{name}\\{file}", fam.lora_sub)
    }
}

/// 把遠端的檔案抓回來：LoRA 各版進 loras\<家族>\<名字>\，樣張進專案的 out\samples\。回這次有沒有新東西
#[allow(clippy::too_many_arguments)]
async fn collect(app: &Shared, id: &str, conn: &Conn, st: &Value, fam: &Fam, name: &str, seen: &mut HashMap<String, u64>, got: &mut HashSet<String>, finished: bool) -> Result<bool, String> {
    let files: Vec<(String, u64, String)> = st["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some((f["name"].as_str()?.to_string(), f["size"].as_u64()?, f["kind"].as_str()?.to_string())))
        .collect();
    // 還在寫的檔不抓：大小要跟上一輪一樣（或訓練已經結束）
    let stable = |n: &str, size: u64, seen: &HashMap<String, u64>| finished || seen.get(n) == Some(&size);
    let loras: Vec<String> = files.iter().filter(|f| f.2 == "lora").map(|f| f.0.clone()).collect();
    let mut changed = false;
    let mut new_epochs = Vec::new();
    for (n, src) in train::epoch_names(&loras, name) {
        let size = files.iter().find(|f| f.0 == src).map_or(0, |f| f.1);
        if got.contains(&src) || size == 0 || !stable(&src, size, seen) {
            continue;
        }
        let rel = lora_rel(fam, name, n);
        conn.download(&src, &app.paths.loras.join(&rel)).await?;
        got.insert(src.clone());
        new_epochs.push(json!({"n": n, "file": rel, "src": src}));
        changed = true;
    }
    let mut new_samples = Vec::new();
    for (src, size, _) in files.iter().filter(|f| f.2 == "sample") {
        if got.contains(src) || *size == 0 || !stable(src, *size, seen) {
            continue;
        }
        let base = src.rsplit('/').next().unwrap_or(src).to_string();
        conn.download(src, &pdir(app, id).join("out").join("samples").join(&base)).await?;
        got.insert(src.clone());
        let (step, idx) = train::sample_step(src).unwrap_or((0, 0));
        new_samples.push(json!({"file": base, "step": step, "idx": idx}));
        changed = true;
    }
    for (n, size, _) in &files {
        seen.insert(n.clone(), *size);
    }
    if changed {
        set_run(app, id, |r| {
            for (key, new) in [("epochs", new_epochs), ("samples", new_samples)] {
                let mut all = r.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
                all.extend(new);
                r.insert(key.into(), json!(all));
            }
        })
        .await;
    }
    Ok(changed)
}

/// 小幫手已經連得上之後的整段：傳資料集 → 開始 → 追進度、把每一版抓回來。回最後的狀態（done／error／stopped）
async fn drive(app: &Shared, id: &str, conn: &Conn, cancel: &CancellationToken, model: &str, vram: f64) -> Result<&'static str, String> {
    let p = load(app, id).map_err(|e| e.1)?;
    let fam = fam_of(&p).map_err(|e| e.1)?;
    let name = train::lora_name(&p).to_string();
    let first = conn.status().await?;
    let resumed = first["state"] == "running";
    if !resumed {
        emit(app, id, "train", "running", json!({"msg": "打包訓練圖…", "step": 0, "total": 0}));
        let zip_path = pdir(app, id).join("dataset.zip");
        let (dir, p2, zp) = (pdir(app, id), p.clone(), zip_path.clone());
        let n = blocking(move || pack_dataset(&dir, &p2, &zp)).await?;
        emit(app, id, "train", "running", json!({"msg": format!("上傳 {n} 筆訓練資料…"), "step": 0, "total": 0}));
        let bytes = tokio::fs::read(&zip_path).await.map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&zip_path);
        conn.send(conn.http.post(format!("{}/dataset", conn.base)).body(bytes), 3600).await?;
        let data_dir = first["data_dir"].as_str().ok_or("訓練機沒有回報資料夾位置（小幫手版本太舊？）")?;
        let out_dir = first["out_dir"].as_str().unwrap_or("");
        let cfg = train::toolkit_config(&p, fam, data_dir, out_dir, model, vram);
        let _ = std::fs::create_dir_all(pdir(app, id).join("out"));
        let _ = std::fs::write(pdir(app, id).join("out").join("config.yaml"), &cfg);
        conn.post("/start", &json!({"config": cfg, "name": name})).await?;
        set_run(app, id, |r| {
            r.insert("epochs".into(), json!([]));
            r.insert("samples".into(), json!([]));
        })
        .await;
    }
    set_run(app, id, |r| {
        r.insert("state".into(), json!("running"));
        r.insert("error".into(), json!(""));
    })
    .await;
    let (mut seen, mut got) = (HashMap::new(), HashSet::new());
    if resumed {
        // 接回去：已經抓過的版本不要再抓一次
        got.extend(p["run"]["epochs"].as_array().into_iter().flatten().filter_map(|e| e["src"].as_str()).map(str::to_string));
    }
    let mut fails = 0u32;
    let mut last_step = u64::MAX;
    loop {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(3)) => {}
            () = cancel.cancelled() => {
                let _ = conn.post("/stop", &json!({})).await;
                return Ok("stopped");
            }
        }
        let st = match conn.status().await {
            Ok(s) => {
                fails = 0;
                s
            }
            Err(e) => {
                fails += 1;
                if fails >= 40 {
                    return Err(format!("訓練機連續兩分鐘沒有回應：{e}"));
                }
                continue;
            }
        };
        let state = st["state"].as_str().unwrap_or("");
        let finished = matches!(state, "done" | "error" | "stopped");
        let changed = collect(app, id, conn, &st, fam, &name, &mut seen, &mut got, finished).await?;
        let step = st["step"].as_u64().unwrap_or(0);
        if changed || step != last_step || finished {
            last_step = step;
            set_run(app, id, |r| {
                r.insert("step".into(), json!(step));
                r.insert("total".into(), st["total"].clone());
                r.insert("loss".into(), st["loss"].clone());
            })
            .await;
        }
        emit(app, id, "train", "running", json!({"step": step, "total": st["total"], "loss": st["loss"], "log": st["log"], "new": changed}));
        match state {
            "done" => return Ok("done"),
            "stopped" => return Ok("stopped"),
            "error" => return Err(st["error"].as_str().unwrap_or("訓練失敗").to_string()),
            "idle" => return Err("訓練機重開過，訓練已經不在了".into()),
            _ => {}
        }
    }
}

/// 訓練收尾：寫回專案檔、通知網頁
async fn finish(app: &Shared, id: &str, r: Result<&'static str, String>) {
    let (state, error) = match r {
        Ok(s) => (s, String::new()),
        Err(e) => ("error", e),
    };
    set_run(app, id, |run| {
        run.insert("state".into(), json!(state));
        run.insert("error".into(), json!(error));
        run.insert("ended".into(), json!(chrono::Utc::now().timestamp()));
    })
    .await;
    job_end(id);
    emit(app, id, "train", state, json!({"msg": error}));
}

// ================================================================ 本機

/// 使用者選的 SDXL 整包模型 → 絕對路徑
fn checkpoint_path(app: &Shared, name: &str) -> Option<PathBuf> {
    if name.contains("..") {
        return None;
    }
    app.paths.unet_roots().into_iter().map(|r| r.join(name)).find(|p| p.is_file())
}

async fn local_run(app: &Shared, id: &str, model: String, cancel: &CancellationToken) -> Result<&'static str, String> {
    let kit = toolkit_dir(app);
    let vpy = venv_python(&kit).ok_or("訓練器還沒裝好")?;
    // 把顯存讓出來：ComfyUI 卸模型、語言模型關掉
    let _ = app.comfy.post("/free", &json!({"unload_models": true, "free_memory": true})).await;
    let _ = app.pe.stop("訓練 LoRA 要用顯存").await;
    let token = format!("{}{}", train::new_id(), train::new_id());
    let mut cmd = tokio::process::Command::new(&vpy);
    cmd.arg(app.paths.res.join("train").join("agent.py"))
        .args(["--host", "127.0.0.1", "--port", "0", "--token", &token, "--watchdog", "180"])
        .arg("--work")
        .arg(trainer_dir(app).join("work"))
        .arg("--toolkit")
        .arg(&kit)
        .arg("--python")
        .arg(&vpy)
        .env("HF_HOME", trainer_dir(app).join("hf"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    quiet(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("訓練小幫手開不起來：{e}"))?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("拿不到小幫手的輸出")?).lines();
    let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await.map_err(|_| "訓練小幫手 30 秒沒有回應".to_string())?.ok().flatten().unwrap_or_default();
    let port = first.split("port=").nth(1).and_then(|p| p.trim().parse::<u16>().ok()).ok_or_else(|| format!("訓練小幫手沒有報 port：{first}"))?;
    let conn = Conn::new(&format!("http://127.0.0.1:{port}"), &token, true)?;
    let r = drive(app, id, &conn, cancel, &model, vram_gb()).await;
    // 不管怎麼結束都叫它停（已經結束就沒事），再關掉小幫手；不然訓練行程會變孤兒繼續占顯卡
    let _ = conn.post("/stop", &json!({})).await;
    let _ = child.kill().await;
    r
}

// ================================================================ RunPod

#[cfg(windows)]
fn seal(s: &str) -> Option<String> {
    crate::settings::protect(s).ok()
}
#[cfg(not(windows))]
fn seal(s: &str) -> Option<String> {
    Some(s.to_string())
}
#[cfg(windows)]
fn unseal(s: &str) -> Option<String> {
    crate::settings::unprotect(s).ok()
}
#[cfg(not(windows))]
fn unseal(s: &str) -> Option<String> {
    Some(s.to_string())
}

fn runpod_body(gpu_ids: &[String], token: &str, agent_py: &[u8]) -> Value {
    json!({
        "name": RUNPOD_POD, "imageName": super::api_cloud::IMAGE, "gpuTypeIds": gpu_ids, "gpuCount": 1, "cloudType": "SECURE",
        "containerDiskInGb": 80, "volumeInGb": 0, "ports": [format!("{RUNPOD_PORT}/http")],
        "env": {"TRAIN_TOKEN": token, "TRAIN_AGENT_B64": base64::engine::general_purpose::STANDARD.encode(agent_py)},
        "dockerStartCmd": ["bash", "-c", train::boot_script(RUNPOD_PORT)],
    })
}

async fn runpod_delete(app: &Shared, pod: &str) -> Result<(), String> {
    app.agent.cloud.rp(reqwest::Method::DELETE, &format!("/pods/{pod}"), None).await.map(|_| ())
}

async fn runpod_run(app: &Shared, id: &str, gpu: &str, cancel: &CancellationToken) -> Result<&'static str, String> {
    let p = load(app, id).map_err(|e| e.1)?;
    let remote = &p["run"]["remote"];
    // 上次開的訓練機還在就接回去，不重開
    let (pod, token) = match (remote["pod"].as_str(), remote["token"].as_str().and_then(unseal)) {
        (Some(pod), Some(token)) if !pod.is_empty() => (pod.to_string(), token),
        _ => {
            let cloud = &app.agent.cloud;
            cloud.refresh_gpus(false).await?;
            let opt = cloud.choices().into_iter().find(|c| c.key == gpu).ok_or("顯卡清單裡找不到選的那張卡，重新整理再選一次")?;
            let agent_py = std::fs::read(app.paths.res.join("train").join("agent.py")).map_err(|e| format!("讀不到訓練小幫手：{e}"))?;
            let token = format!("{}{}", train::new_id(), train::new_id());
            emit(app, id, "train", "running", json!({"msg": format!("租 {}…", opt.name)}));
            let r = cloud.rp(reqwest::Method::POST, "/pods", Some(&runpod_body(&opt.ids, &token, &agent_py))).await?;
            let pod = tx(&r["id"]);
            if pod.is_empty() {
                return Err("RunPod 沒有回機器編號".into());
            }
            let sealed = seal(&token).ok_or("口令加密失敗")?;
            let pod2 = pod.clone();
            set_run(app, id, |r| {
                r.insert("remote".into(), json!({"pod": pod2, "token": sealed, "gpu": opt.name, "usd_per_hour": opt.price_hi}));
            })
            .await;
            (pod, token)
        }
    };
    let conn = Conn::new(&format!("https://{pod}-{RUNPOD_PORT}.proxy.runpod.net"), &token, false)?;
    emit(app, id, "train", "running", json!({"msg": "訓練機開機、裝環境中（第一次約 10～20 分鐘，這段也在計費）…"}));
    let mut waited = 0u64;
    loop {
        if conn.status().await.is_ok() {
            break;
        }
        waited += 10;
        if waited > 45 * 60 {
            let _ = runpod_delete(app, &pod).await;
            return Err("訓練機 45 分鐘還沒準備好，已經把它關掉（不再計費）".into());
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(10)) => {}
            () = cancel.cancelled() => {
                runpod_delete(app, &pod).await.map_err(|e| format!("停止了，但訓練機關不掉（還在計費！到 RunPod 網站手動刪除 {RUNPOD_POD}）：{e}"))?;
                set_run(app, id, |r| { r.remove("remote"); }).await;
                return Ok("stopped");
            }
        }
    }
    let r = drive(app, id, &conn, cancel, fam_of(&p).map_err(|e| e.1)?.base_hint, 48.0).await;
    // 抓不回來的時候不關機（關了就沒了）；其他情況一律關機停止計費
    if matches!(&r, Err(e) if e.contains("下載中斷") || e.contains("沒有回應")) {
        return r.map_err(|e| format!("{e}。訓練機還開著（還在計費）：按「開始」可以接回去繼續抓，或按「停止」把它關掉"));
    }
    match runpod_delete(app, &pod).await {
        Ok(()) => set_run(app, id, |r| { r.remove("remote"); }).await,
        Err(e) => return Err(format!("訓練機關不掉（還在計費！到 RunPod 網站手動刪除 {RUNPOD_POD}）：{e}")),
    }
    r
}

/// 沒有背景工作在跑的時候按停止：把還開著的雲端訓練機關掉
pub(super) async fn stop_remote(app: &Shared, id: &str) -> ApiResult {
    let p = load(app, id)?;
    if let Some(pod) = p["run"]["remote"]["pod"].as_str().filter(|x| !x.is_empty()) {
        runpod_delete(app, pod).await.map_err(|e| bad(format!("訓練機關不掉：{e}")))?;
        set_run(app, id, |r| {
            r.remove("remote");
            r.insert("state".into(), json!("stopped"));
        })
        .await;
        return ok(json!({"ok": true, "msg": "訓練機已經關掉"}));
    }
    if p["run"]["remote"]["workflow"].is_string() {
        return ok(json!({"ok": true, "msg": "繪匣不再追蹤了；C 站那邊的訓練要到 C 站網站取消"}));
    }
    ok(json!({"ok": true}))
}

// ================================================================ C 站

fn civitai_key(app: &Shared) -> Result<String, String> {
    std::env::var("CIVITAI_TOKEN")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .or_else(|| Some(app.civitai.key()).filter(|k| !k.is_empty()))
        .ok_or_else(|| "找不到 Civitai 金鑰：到「設定」填入你的 Civitai API key".to_string())
}

async fn civ(app: &Shared, method: reqwest::Method, path: &str, json_body: Option<&Value>, raw: Option<(Vec<u8>, &str)>) -> Result<Value, String> {
    let key = civitai_key(app)?;
    let http = reqwest::Client::builder().proxy(crate::net::proxy()).build().map_err(|e| e.to_string())?;
    let url = format!("{CIVITAI_API}{path}");
    let mut last = String::new();
    for attempt in 0..3u64 {
        let mut rb = http.request(method.clone(), &url).bearer_auth(&key).header("User-Agent", "paintdeck").timeout(Duration::from_secs(300));
        if let Some(b) = json_body {
            rb = rb.json(b);
        }
        if let Some((bytes, ctype)) = &raw {
            rb = rb.header("Content-Type", *ctype).body(bytes.clone());
        }
        match rb.send().await {
            Ok(r) => {
                let code = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                if (200..300).contains(&code) {
                    return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
                }
                last = format!("C 站回應 {code}：{}", text.chars().take(400).collect::<String>());
                if code < 500 {
                    return Err(last);
                }
            }
            Err(e) => last = format!("連不到 C 站：{}", e.without_url()),
        }
        tokio::time::sleep(Duration::from_secs(5 * (attempt + 1))).await;
    }
    Err(last)
}

/// 把訓練圖傳上 C 站（傳過的不重傳），回 [(blob 編號, 描述)]
async fn civ_upload(app: &Shared, id: &str, cancel: &CancellationToken) -> Result<Vec<(String, String)>, String> {
    let p = load(app, id).map_err(|e| e.1)?;
    let dir = pdir(app, id);
    let mut blobs: Map<String, Value> = p["civitai"]["blobs"].as_object().cloned().unwrap_or_default();
    let files: Vec<String> = live(&p).iter().filter_map(|i| i["file"].as_str()).map(str::to_string).collect();
    let mut items = Vec::new();
    for (n, file) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err("已停止".into());
        }
        if !blobs.contains_key(file) {
            emit(app, id, "train", "running", json!({"msg": format!("上傳到 C 站 {}/{}…", n + 1, files.len())}));
            let ctype = match Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or("") {
                "png" => "image/png",
                "webp" => "image/webp",
                "mp4" => "video/mp4",
                "webm" => "video/webm",
                "mov" => "video/quicktime",
                _ => "image/jpeg",
            };
            let bytes = tokio::fs::read(dir.join("images").join(file)).await.map_err(|e| format!("讀不到 {file}：{e}"))?;
            let r = civ(app, reqwest::Method::POST, "/blobs", None, Some((bytes, ctype))).await?;
            let air = r["id"].as_str().ok_or("C 站沒有回 blob 編號")?.to_string();
            blobs.insert(file.clone(), json!(air));
            let snapshot = blobs.clone();
            let _ = update(app, id, |p| {
                p["civitai"]["blobs"] = Value::Object(snapshot);
                Ok(())
            })
            .await;
        }
        items.push((blobs[file].as_str().unwrap_or("").to_string(), read_caption(&dir, file)));
    }
    Ok(items)
}

async fn civ_quote(app: &Shared, id: &str, cancel: &CancellationToken) -> Result<(f64, Vec<(String, String)>), String> {
    let items = civ_upload(app, id, cancel).await?;
    let p = load(app, id).map_err(|e| e.1)?;
    let est = civ(app, reqwest::Method::POST, "/workflows?whatif=true", Some(&train::civitai_body(&p, fam_of(&p).map_err(|e| e.1)?, &items)), None).await?;
    let cost = est["cost"]["total"].as_f64().ok_or("C 站沒有回價格")?;
    let _ = update(app, id, |p| {
        p["civitai"]["quote"] = json!({"cost": cost, "steps": p["params"]["steps"], "epochs": p["params"]["epochs"], "images": items.len(), "at": chrono::Utc::now().timestamp()});
        Ok(())
    })
    .await;
    Ok((cost, items))
}

async fn civ_run(app: &Shared, id: &str, max_cost: f64, cancel: &CancellationToken) -> Result<&'static str, String> {
    let p = load(app, id).map_err(|e| e.1)?;
    let fam = fam_of(&p).map_err(|e| e.1)?;
    let name = train::lora_name(&p).to_string();
    let wf_id = match p["run"]["remote"]["workflow"].as_str() {
        Some(w) => w.to_string(),
        None => {
            let (cost, items) = civ_quote(app, id, cancel).await?;
            if cost > max_cost {
                return Err(format!("試算要 {cost} Buzz，超過你同意的上限 {max_cost}，沒有送出"));
            }
            let p = load(app, id).map_err(|e| e.1)?;
            let wf = civ(app, reqwest::Method::POST, "/workflows?wait=0", Some(&train::civitai_body(&p, fam, &items)), None).await?;
            let w = wf["id"].as_str().ok_or("C 站沒有回任務編號")?.to_string();
            let w2 = w.clone();
            set_run(app, id, |r| {
                r.insert("remote".into(), json!({"workflow": w2, "cost": wf["cost"]["total"]}));
                r.insert("state".into(), json!("running"));
                r.insert("epochs".into(), json!([]));
            })
            .await;
            w
        }
    };
    loop {
        let wf = civ(app, reqwest::Method::GET, &format!("/workflows/{wf_id}"), None, None).await?;
        let status = wf["status"].as_str().unwrap_or("").to_string();
        let step = &wf["steps"][0];
        let have: HashSet<u64> = load(app, id).map_err(|e| e.1)?["run"]["epochs"].as_array().into_iter().flatten().filter_map(|e| e["n"].as_u64()).collect();
        let mut new = Vec::new();
        for ep in step["output"]["epochs"].as_array().into_iter().flatten() {
            let (Some(n), Some(url)) = (ep["epochNumber"].as_u64(), ep["model"]["url"].as_str()) else { continue };
            if have.contains(&n) || ep["model"]["available"].as_bool() != Some(true) {
                continue;
            }
            let rel = lora_rel(fam, &name, n as u32);
            let dest = app.paths.loras.join(&rel);
            let http = reqwest::Client::builder().proxy(crate::net::proxy()).build().map_err(|e| e.to_string())?;
            let bytes = http.get(url).header("User-Agent", "paintdeck").timeout(Duration::from_secs(1800)).send().await.and_then(reqwest::Response::error_for_status).map_err(|e| format!("下載第 {n} 版失敗：{}", e.without_url()))?.bytes().await.map_err(|e| e.without_url().to_string())?;
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            tokio::fs::write(&dest, &bytes).await.map_err(|e| format!("存不了第 {n} 版：{e}"))?;
            new.push(json!({"n": n, "file": rel}));
        }
        let rate = step["estimatedProgressRate"].as_f64();
        let fresh = !new.is_empty();
        set_run(app, id, |r| {
            let mut all = r.get("epochs").and_then(Value::as_array).cloned().unwrap_or_default();
            all.extend(new);
            r.insert("epochs".into(), json!(all));
            if let Some(x) = rate {
                r.insert("step".into(), json!((x * 100.0).round()));
                r.insert("total".into(), json!(100));
            }
        })
        .await;
        emit(app, id, "train", "running", json!({"msg": format!("C 站：{status}"), "step": rate.map(|x| (x * 100.0).round()), "total": 100, "new": fresh}));
        if CIVITAI_DONE.contains(&status.as_str()) {
            return if status == "succeeded" { Ok("done") } else { Err(format!("C 站的訓練結束了：{status}")) };
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            () = cancel.cancelled() => return Ok("stopped"),
        }
    }
}

// ================================================================ 報價、開始

pub async fn quote(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let p = load(&app, &id)?;
    let fam = fam_of(&p)?;
    let steps = p["params"]["steps"].as_u64().unwrap_or(1000);
    match b["target"].as_str().unwrap_or("") {
        "runpod" => {
            let cloud = &app.agent.cloud;
            cloud.refresh_gpus(false).await.map_err(bad)?;
            let gpus: Vec<Value> = cloud
                .choices()
                .iter()
                .map(|c| {
                    let (hours, usd) = train::estimate(fam, steps, c.price_hi);
                    json!({"key": c.key, "name": c.name, "vram": c.vram, "usd_per_hour": c.price_hi, "stock": c.stock,
                           "hours": (hours * 10.0).round() / 10.0, "usd": (usd * 100.0).round() / 100.0})
                })
                .collect();
            ok(json!({"target": "runpod", "gpus": gpus, "note": "估計值：實際看顯卡和排隊，開機裝環境那十幾分鐘也算錢"}))
        }
        "civitai" => {
            if fam.civitai.is_none() {
                return Err(bad("C 站還不能煉這個家族"));
            }
            if b["upload_ok"].as_bool() != Some(true) {
                return Err(bad("試算要先把訓練圖和描述上傳到 Civitai（放在你的帳號底下）。同意的話帶 upload_ok: true"));
            }
            ready(&app, &p)?;
            let cancel = job_begin(&id, "train")?;
            let r = civ_quote(&app, &id, &cancel).await;
            job_end(&id);
            let (cost, items) = r.map_err(bad)?;
            ok(json!({"target": "civitai", "buzz": cost, "images": items.len(), "steps": steps, "note": "試算不扣點；按開始才會扣"}))
        }
        _ => Err(bad("這個地方不用報價")),
    }
}

/// 能不能開始：有圖、每張都有描述
fn ready(app: &Shared, p: &Value) -> Result<(), ApiError> {
    let id = p["id"].as_str().unwrap_or("");
    let files: Vec<&str> = live(p).iter().filter_map(|i| i["file"].as_str()).collect();
    if files.is_empty() {
        return Err(bad("還沒有訓練圖，先到「訓練圖」放圖"));
    }
    let dir = pdir(app, id);
    let empty = files.iter().filter(|f| read_caption(&dir, f).is_empty()).count();
    if empty > 0 {
        return Err(bad(format!("還有 {empty} 張沒有描述，先到「描述」補齊（或把那幾張勾掉）")));
    }
    Ok(())
}

pub async fn start(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let p = load(&app, &id)?;
    let fam = fam_of(&p)?;
    let target = b["target"].as_str().unwrap_or("").to_string();
    if !["local", "remote", "runpod", "civitai", "modelscope"].contains(&target.as_str()) {
        return Err(bad("target 要是 local、remote、runpod、civitai、modelscope 其中一個"));
    }
    if PAID.contains(&target.as_str()) && b["confirm"].as_bool() != Some(true) {
        return Err(bad("這個地方要花錢（或用掉額度），要先確認：看過報價後帶 confirm: true"));
    }
    ready(&app, &p)?;
    // 參數還沒存過就用自動算的
    if !p["params"].is_object() || !p["samples"].as_array().is_some_and(|s| !s.is_empty()) {
        update(&app, &id, |p| {
            let kind = p["kind"].as_str().unwrap_or("style").to_string();
            if !p["params"].is_object() {
                p["params"] = train::auto_params(fam, &kind, live(p).len(), p["level"].as_str().unwrap_or("std"));
            }
            if !p["samples"].as_array().is_some_and(|s| !s.is_empty()) {
                p["samples"] = json!(train::sample_prompts(fam, &kind, p["trigger"].as_str().unwrap_or("")));
            }
            Ok(())
        })
        .await?;
    }
    let mut model = fam.base_hint.to_string();
    match target.as_str() {
        "local" => {
            if !installed(&app) {
                return Err(bad("本機訓練器還沒裝：先按「安裝訓練器」"));
            }
            if let Some(other) = training_id().filter(|o| o != &id) {
                return Err(bad(format!("已經有另一個專案在訓練（{other}），本機一次只能煉一顆")));
            }
            let vram = blocking(vram_gb).await;
            if vram + 0.5 < fam.min_vram_gb && b["force"].as_bool() != Some(true) {
                return Err(bad(format!("這台的顯存 {vram:.0}GB 不夠煉 {}（至少要 {:.0}GB）：改用雲端顯卡或 C 站", fam.label, fam.min_vram_gb)));
            }
            if let Some(m) = b["model"].as_str().filter(|m| !m.is_empty()) {
                model = checkpoint_path(&app, m).ok_or_else(|| bad(format!("找不到底模 {m}")))?.to_string_lossy().into_owned();
            }
        }
        "remote" => {
            if !tx(&b["url"]).starts_with("http") || tx(&b["token"]).len() < 8 {
                return Err(bad("自己的訓練機要填網址（http 開頭）和口令（至少 8 個字）"));
            }
        }
        "runpod" => {
            if tx(&b["gpu"]).is_empty() && !p["run"]["remote"]["pod"].is_string() {
                return Err(bad("要選一張雲端顯卡"));
            }
        }
        "civitai" => {
            if fam.civitai.is_none() {
                return Err(bad("C 站還不能煉這個家族"));
            }
        }
        _ => {
            return super::train_ms::start(&app, &id, &p).await;
        }
    }
    let cancel = job_begin(&id, "train")?;
    let t2 = target.clone();
    let keep_remote = p["run"]["target"].as_str() == Some(target.as_str());
    set_run(&app, &id, |r| {
        let remote = r.get("remote").cloned().filter(|_| keep_remote);
        let (epochs, samples) = (r.get("epochs").cloned().filter(|_| keep_remote), r.get("samples").cloned().filter(|_| keep_remote));
        r.clear();
        r.insert("target".into(), json!(t2));
        r.insert("state".into(), json!("starting"));
        r.insert("started".into(), json!(chrono::Utc::now().timestamp()));
        for (k, v) in [("remote", remote), ("epochs", epochs), ("samples", samples)] {
            if let Some(v) = v {
                r.insert(k.into(), v);
            }
        }
    })
    .await;
    emit(&app, &id, "train", "running", json!({"msg": "準備中…"}));
    let (app2, id2) = (app.clone(), id.clone());
    tokio::spawn(async move {
        let (app, id) = (app2, id2);
        let r = match target.as_str() {
            "local" => local_run(&app, &id, model, &cancel).await,
            "remote" => match Conn::new(&tx(&b["url"]), &tx(&b["token"]), false) {
                Ok(conn) => drive(&app, &id, &conn, &cancel, &model, b["vram_gb"].as_f64().unwrap_or(24.0)).await,
                Err(e) => Err(e),
            },
            "runpod" => runpod_run(&app, &id, &tx(&b["gpu"]), &cancel).await,
            _ => civ_run(&app, &id, b["max_cost"].as_f64().unwrap_or(0.0), &cancel).await,
        };
        finish(&app, &id, r).await;
    });
    ok(json!({"ok": true}))
}

// ================================================================ 驗收

pub async fn sample(State(app): State<Shared>, UrlPath((id, file)): UrlPath<(String, String)>) -> ApiResult {
    let p = load(&app, &id)?;
    if !p["run"]["samples"].as_array().into_iter().flatten().any(|s| s["file"].as_str() == Some(file.as_str())) {
        return Err(not_found("找不到這張樣張"));
    }
    file_response(pdir(&app, &id).join("out").join("samples").join(file), None).await
}

fn epoch_file(p: &Value, n: u64) -> Option<String> {
    p["run"]["epochs"].as_array()?.iter().find(|e| e["n"].as_u64() == Some(n))?["file"].as_str().map(str::to_string)
}

pub async fn compare(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let p = load(&app, &id)?;
    if job_kind(&id) == Some("train") {
        return Err(bad("還在訓練，顯卡讓不出來；等它煉完再出對照表"));
    }
    let Some(Value::Object(base)) = b.get("params").cloned() else { return Err(bad("要帶 params（生成頁現在的設定）")) };
    let all: Vec<u32> = p["run"]["epochs"].as_array().into_iter().flatten().filter_map(|e| e["n"].as_u64()).map(|n| n as u32).collect();
    let mut epochs: Vec<u32> = b["epochs"].as_array().map_or_else(|| all.clone(), |a| a.iter().filter_map(Value::as_u64).map(|n| n as u32).filter(|n| all.contains(n)).collect());
    epochs.sort_unstable();
    epochs.dedup();
    let strengths: Vec<f64> = b["strengths"].as_array().map_or_else(|| vec![0.6, 0.8, 1.0], |a| a.iter().filter_map(Value::as_f64).filter(|s| (0.05..=2.0).contains(s)).take(5).collect());
    if epochs.is_empty() || strengths.is_empty() {
        return Err(bad("還沒有煉好的版本可以比"));
    }
    if epochs.len() * strengths.len() > 60 {
        return Err(bad("格子太多（超過 60 張），少選幾版或幾個強度"));
    }
    // 家族對不上的 LoRA 會被生圖流程默默略過：先擋，並替還沒登記的檔補上底模（檔頭認不出來時靠它）
    let fam = fam_of(&p)?;
    let model = base.get("model").and_then(Value::as_str).unwrap_or("");
    let workflow_set = model.starts_with(crate::image_wf::PREFIX);
    let model_fam = crate::families::family_of(&app.paths.unet_roots(), model);
    let lora_roots = app.paths.lora_roots();
    for n in &epochs {
        let Some(file) = epoch_file(&p, u64::from(*n)) else { continue };
        let mut meta = app.db.lora_meta().unwrap_or_default();
        if !meta.contains_key(&file) {
            let data = json!({"title": format!("{}（第 {n} 版）", p["name"].as_str().unwrap_or("")), "triggers": [p["trigger"]], "base_model": crate::modelhub::meta_base(Some(fam.id)), "source": "train", "train_project": id});
            app.db.set_lora_meta(&file, &data)?;
            meta.insert(file.clone(), data);
        }
        let lora_fam = crate::families::lora_family(&lora_roots, &file, meta.get(&file));
        if let Some(why) = train::compare_block(fam, model_fam, lora_fam, workflow_set) {
            return Err(bad(why));
        }
    }
    let seed = base.get("seed").and_then(crate::py::int).filter(|s| *s >= 0).unwrap_or(20_261_110);
    let mut cells = Vec::new();
    for (n, strength, seed) in train::compare_jobs(&epochs, &strengths, seed) {
        let Some(file) = epoch_file(&p, u64::from(n)) else { continue };
        let mut q = base.clone();
        q.insert("seed".into(), json!(seed));
        q.insert("loras".into(), json!([{"name": file, "strength": strength, "enabled": true}]));
        let jobs = super::api_gen::submit_jobs(&app, q, 1).await?;
        cells.push(json!({"epoch": n, "strength": strength, "seed": seed, "job": jobs.first().map(|j| j["job"].clone())}));
    }
    ok(json!({"cells": cells, "epochs": epochs, "strengths": strengths}))
}

pub async fn adopt(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let p = load(&app, &id)?;
    let fam = fam_of(&p)?;
    let n = b["epoch"].as_u64().ok_or_else(|| bad("要給 epoch（第幾版）"))?;
    let file = epoch_file(&p, n).ok_or_else(|| bad("沒有這一版"))?;
    let strength = b["strength"].as_f64().filter(|s| (0.05..=2.0).contains(s)).unwrap_or(1.0);
    let trigger = p["trigger"].as_str().unwrap_or("");
    app.db.set_lora_meta(&file, &json!({
        "title": format!("{}（第 {n} 版）", p["name"].as_str().unwrap_or(trigger)),
        "triggers": [trigger],
        "strength": strength,
        "base_model": crate::modelhub::meta_base(Some(fam.id)),
        "source": "train",
        "train_project": id,
    }))?;
    let p = update(&app, &id, |p| {
        p["adopted"] = json!({"epoch": n, "file": file, "strength": strength});
        Ok(())
    })
    .await?;
    ok(json!({"ok": true, "project": p}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lora_paths_follow_family_folder() {
        assert_eq!(lora_rel(train::fam("anima").unwrap(), "xk", 3), "xk\\xk_e3.safetensors");
        assert_eq!(lora_rel(train::fam("qwen21").unwrap(), "xk", 10), "qwen21\\xk\\xk_e10.safetensors");
    }

    #[test]
    fn runpod_body_keeps_secrets_out_of_command() {
        let b = runpod_body(&["NVIDIA A40".to_string()], "secret-token-xyz", b"print('agent')");
        let cmd = b["dockerStartCmd"][2].as_str().unwrap();
        assert!(!cmd.contains("secret-token-xyz"), "口令不能寫在開機指令裡");
        assert!(cmd.contains(train::TOOLKIT_COMMIT) && cmd.contains("--port 8000"));
        assert_eq!(b["env"]["TRAIN_TOKEN"], "secret-token-xyz");
        assert_eq!(b["ports"], json!(["8000/http"]));
        assert_eq!(b["name"], "anima-train");
        let agent = base64::engine::general_purpose::STANDARD.decode(b["env"]["TRAIN_AGENT_B64"].as_str().unwrap()).unwrap();
        assert_eq!(agent, b"print('agent')");
    }
}

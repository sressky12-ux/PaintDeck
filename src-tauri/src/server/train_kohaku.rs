//! KohakuMoss 引擎（作者 KBlueLeaf）的 Windows 分段安裝與訓練前健檢。
use super::{api_models, bad, blocking, ok, train_run, ApiResult, Body, Shared};
use crate::{director::write_atomic, train};
use axum::extract::{Query, State};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const UV_VERSION: &str = "0.13.7";
const UV_SIZE: usize = 15236317;
const UV_SHA256: &str = "0cdc5b38fe5a8168ed6a25814f5ba43c21d313dd3187314fccb37b7cec46e021";
const UV_INCOMPLETE: &str = "uv 下載不完整，請再按一次安裝";
const TORCH: &str = "torch>=2.13,<2.15";
const CU130: &str = "https://download.pytorch.org/whl/cu130";
const STAGES: [&str; 6] = ["uv", "source", "venv", "torch", "engine", "doctor"];
const CANCELED: &str = "已取消";

#[derive(Default)]
struct Install {
    cancel: Option<CancellationToken>,
    stage: Option<&'static str>,
    log: Vec<String>,
    error: String,
}

static INSTALL: LazyLock<Mutex<Install>> = LazyLock::new(|| Mutex::new(Install::default()));
// 安裝與移除在整段檔案操作期間互斥；查詢進度和取消仍能取得 INSTALL。
pub(super) static OPERATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn inst() -> MutexGuard<'static, Install> {
    INSTALL.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn kohaku_dir(app: &Shared) -> PathBuf {
    train_run::trainer_dir(app).join("kohakumoss")
}

pub(super) fn kohaku_kogine(app: &Shared) -> Option<PathBuf> {
    let exe = kohaku_dir(app).join("repo/.venv/Scripts/kogine.exe");
    exe.is_file().then_some(exe)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub(super) fn kohaku_installed(app: &Shared) -> bool {
    kohaku_kogine(app).is_some()
        && read_json(&kohaku_dir(app).join("installed.json")).is_some_and(|v| v["commit"].as_str() == Some(train::KOHAKU_COMMIT))
}

pub(super) fn next_stage(done: &[String]) -> Option<&'static str> {
    STAGES.into_iter().find(|stage| !done.iter().any(|s| s == stage))
}

fn load_stages(dir: &Path) -> Vec<String> {
    let Some(v) = read_json(&dir.join("stages.json")).filter(|v| v["commit"] == train::KOHAKU_COMMIT) else {
        return Vec::new();
    };
    // 有缺段的紀錄只能接到第一個缺口；後面的階段依賴前面的環境。
    STAGES
        .iter()
        .enumerate()
        .take_while(|(i, stage)| v["stages_done"][i].as_str() == Some(stage))
        .map(|(_, s)| s.to_string())
        .collect()
}

fn save_stages(dir: &Path, done: &[String]) -> io::Result<()> {
    write_atomic(
        &dir.join("stages.json"),
        &json!({"commit":train::KOHAKU_COMMIT, "stages_done":done}),
    )
}

pub(super) fn kohaku_view(app: &Shared) -> Value {
    let dir = kohaku_dir(app);
    let i = inst();
    json!({
        "installed":kohaku_installed(app), "busy":i.cancel.is_some(), "stage":i.stage.unwrap_or("idle"),
        "stages_done":load_stages(&dir), "log":i.log.iter().rev().take(15).rev().collect::<Vec<_>>(),
        "error":i.error, "commit":train::KOHAKU_COMMIT,
        "speed":read_json(&dir.join("speed.json")).filter(Value::is_object).unwrap_or_else(|| json!({}))
    })
}

fn emit(app: &Shared, stage: &str, state: &str, msg: &str) {
    app.hub
        .send(json!({"type":"train", "id":"", "phase":"install", "engine":"kohakumoss", "stage":stage, "state":state, "msg":msg}));
}

fn say(app: &Shared, line: String) {
    let mut i = inst();
    let stage = i.stage.unwrap_or("idle");
    log_event(app, &mut i, stage, "running", line);
}

fn log_event(app: &Shared, i: &mut Install, stage: &str, state: &str, line: String) {
    emit(app, stage, state, &line);
    i.log.push(line);
    let discard = i.log.len().saturating_sub(400);
    i.log.drain(..discard);
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err(CANCELED.to_owned())
    } else {
        Ok(())
    }
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 保留 zip 自己的安全解壓縮規則，也讓解到一半時能停止讀取。
struct ArchiveReader<'a> {
    data: Cursor<bytes::Bytes>,
    cancel: &'a CancellationToken,
}

impl Read for ArchiveReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other(CANCELED));
        }
        self.data.read(buf)
    }
}

impl Seek for ArchiveReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.data.seek(pos)
    }
}

async fn download(url: &str, cancel: &CancellationToken, uv: bool) -> Result<bytes::Bytes, String> {
    let http = reqwest::Client::builder()
        .proxy(crate::net::proxy())
        .build()
        .map_err(|e| e.to_string())?;
    let fetch = async {
        let response = http
            .get(url)
            .timeout(Duration::from_secs(900))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("下載失敗：{}", crate::net::explain(&e, url)))?;
        response.bytes().await.map_err(|e| {
            if uv {
                UV_INCOMPLETE.to_owned()
            } else {
                format!("下載中斷：{}", e.without_url())
            }
        })
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CANCELED.to_owned()),
        bytes = fetch => bytes,
    }
}

fn unpack_uv(dir: &Path, data: bytes::Bytes, cancel: &CancellationToken) -> Result<(), String> {
    if data.len() != UV_SIZE || format!("{:x}", Sha256::digest(&data)) != UV_SHA256 {
        return Err(UV_INCOMPLETE.to_owned());
    }
    let mut zip = zip::ZipArchive::new(ArchiveReader {
        data: Cursor::new(data),
        cancel,
    })
    .map_err(|e| e.to_string())?;
    let index = zip
        .file_names()
        .position(|name| name.is_ok_and(|name| name.rsplit('/').next() == Some("uv.exe")))
        .ok_or("壓縮檔裡找不到 uv.exe")?;
    let mut exe = zip.by_index(index).map_err(|e| e.to_string())?;
    let tmp = dir.join("uv.exe.part");
    let mut unpack = || -> io::Result<()> {
        io::copy(&mut exe, &mut fs::File::create(&tmp)?)?;
        fs::rename(&tmp, dir.join("uv.exe"))
    };
    unpack().map_err(|e| format!("解開 uv.exe 失敗：{e}"))
}

async fn archive_stage(app: &Shared, stage: &str, dir: &Path, cancel: &CancellationToken) -> Result<(), String> {
    if stage == "uv" {
        say(app, format!("下載 uv {UV_VERSION}…"));
        let url = format!("https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-x86_64-pc-windows-msvc.zip");
        let bytes = download(&url, cancel, true).await?;
        let dir = dir.to_owned();
        let cancel = cancel.clone();
        return blocking(move || unpack_uv(&dir, bytes, &cancel)).await;
    }
    let repo = dir.join("repo");
    let tmp = dir.join("_unpack");
    let dest = repo.clone();
    blocking(move || ignore_missing(fs::remove_dir_all(dest)))
        .await
        .map_err(|e| format!("清理未完成的原始碼失敗：{e}"))?;
    say(app, "下載 KohakuMoss 原始碼…".into());
    let url = format!(
        "https://codeload.github.com/KohakuBlueleaf/KohakuMossTrainer/zip/{}",
        train::KOHAKU_COMMIT
    );
    let bytes = download(&url, cancel, false).await?;
    say(app, "解開 KohakuMoss 原始碼…".into());
    let cancel = cancel.clone();
    blocking(move || {
        ignore_missing(fs::remove_dir_all(&tmp)).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(ArchiveReader {
            data: Cursor::new(bytes),
            cancel: &cancel,
        })
        .map_err(|e| e.to_string())?;
        zip.extract(&tmp).map_err(|e| format!("解壓縮失敗：{e}"))?;
        check_cancel(&cancel)?;
        fs::rename(tmp.join(format!("KohakuMossTrainer-{}", train::KOHAKU_COMMIT)), repo).map_err(|e| format!("搬不動原始碼：{e}"))?;
        fs::remove_dir(&tmp).map_err(|e| e.to_string())
    })
    .await
}

async fn command_stage(app: &Shared, stage: &str, dir: &Path, cancel: &CancellationToken) -> Result<(), String> {
    let repo = dir.join("repo");
    let venv = repo.join(".venv");
    let scripts = venv.join("Scripts");
    let report = dir.join("doctor.json");
    let mut cmd = Command::new(if stage == "doctor" {
        scripts.join("kohakumoss-doctor.exe")
    } else {
        dir.join("uv.exe")
    });
    let message = match stage {
        "venv" => {
            let partial = venv.clone();
            blocking(move || ignore_missing(fs::remove_dir_all(partial)))
                .await
                .map_err(|e| format!("清理未完成的 Python 環境失敗：{e}"))?;
            cmd.args(["venv", "-p", "3.12"]).arg(&venv);
            "建立 Python 3.12 環境…"
        }
        "torch" | "engine" => {
            cmd.args(["pip", "install", "--python"]).arg(scripts.join("python.exe"));
            if stage == "torch" {
                cmd.args([TORCH, "torchvision", "--index-url", CU130]);
                "安裝 PyTorch 與 torchvision…"
            } else {
                cmd.args([
                    "-e",
                    ".[cuda]",
                    TORCH,
                    "--extra-index-url",
                    CU130,
                    "--index-strategy",
                    "unsafe-best-match",
                ]);
                "安裝 KohakuMoss 引擎…"
            }
        }
        "doctor" => {
            ignore_missing(fs::remove_file(&report)).map_err(|e| format!("清理舊健檢報告失敗：{e}"))?;
            cmd.arg("--json").arg(&report);
            "執行 KohakuMoss 健檢…"
        }
        _ => unreachable!(),
    };
    say(app, message.to_owned());
    cmd.current_dir(repo)
        .env("UV_CACHE_DIR", dir.join("uv-cache"))
        .env("UV_PYTHON_INSTALL_DIR", dir.join("python"))
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    check_cancel(cancel)?;
    let status = train_run::run_logged_to(app, cmd, say, Some(cancel)).await?;
    check_cancel(cancel)?;
    if stage == "doctor" && read_json(&report).is_some() {
        return Ok(());
    }
    if !status.success() {
        return Err(format!("指令失敗（{status}），請查看最後幾行訊息"));
    }
    if stage == "doctor" {
        return Err("健檢沒有產生有效的 doctor.json".to_owned());
    }
    Ok(())
}

// 用標準庫建立行程，才能加入現有的 Windows Job。繪匣意外結束時，系統也會回收整棵樹。
#[cfg(windows)]
pub(super) async fn run_windows_stage(
    app: &Shared,
    mut cmd: Command,
    sink: fn(&Shared, String),
    cancel: &CancellationToken,
) -> io::Result<std::process::ExitStatus> {
    use crate::studio::job::Job;
    use tokio::process::{ChildStderr, ChildStdout};
    let job = Job::kill_on_close().ok_or_else(io::Error::last_os_error)?;
    let mut child = cmd.as_std_mut().spawn()?;
    if !job.add(&child) {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let pid = child.id();
    let out = ChildStdout::from_std(child.stdout.take().unwrap())?;
    let err = ChildStderr::from_std(child.stderr.take().unwrap())?;
    let run = async {
        let wait = tokio::task::spawn_blocking(move || (child.wait(), child));
        let (result, (), ()) = tokio::join!(
            wait,
            train_run::forward_lines(app, out, sink),
            train_run::forward_lines(app, err, sink)
        );
        result?.0
    };
    tokio::pin!(run);
    tokio::select! {
        status = &mut run => status,
        _ = cancel.cancelled() => {
            let mut kill = Command::new("taskkill");
            train_run::quiet(&mut kill);
            kill.args(["/PID", &pid.to_string(), "/T", "/F"]);
            let _ = kill.output().await;
            drop(job); // 即使父行程已退出，Job 仍涵蓋存活的子行程。
            run.await
        }
    }
}

async fn install_run(app: &Shared, cancel: &CancellationToken) -> Result<(), String> {
    let dir = kohaku_dir(app);
    fs::create_dir_all(&dir).map_err(|e| format!("建立安裝資料夾失敗：{e}"))?;
    let mut done = load_stages(&dir);
    // 修復使用者刪除過的環境；斷點之前的產物必須還在。
    for (index, file) in [
        (0, "uv.exe"),
        (1, "repo/pyproject.toml"),
        (2, "repo/.venv/Scripts/python.exe"),
        (4, "repo/.venv/Scripts/kogine.exe"),
        (5, "doctor.json"),
    ] {
        if done.len() > index && !dir.join(file).is_file() {
            done.truncate(index);
            save_stages(&dir, &done).map_err(|e| e.to_string())?;
            break;
        }
    }
    if next_stage(&done).is_some() {
        ignore_missing(fs::remove_file(dir.join("installed.json"))).map_err(|e| format!("更新安裝紀錄失敗：{e}"))?;
    }
    let mut completed: Option<&str> = None;
    loop {
        let stage = {
            // 取消、記錄上一段、切換下一段（或宣告整套裝好）共用一把鎖。
            let mut i = inst();
            check_cancel(cancel)?;
            if let Some(stage) = completed {
                done.push(stage.to_owned());
                save_stages(&dir, &done).map_err(|e| format!("記錄安裝進度失敗：{e}"))?;
            }
            let Some(stage) = next_stage(&done) else {
                kohaku_kogine(app).ok_or("找不到安裝好的 kogine.exe")?;
                write_atomic(
                    &dir.join("installed.json"),
                    &json!({"commit":train::KOHAKU_COMMIT, "at":chrono::Utc::now().timestamp()}),
                )
                .map_err(|e| format!("記錄安裝結果失敗：{e}"))?;
                i.cancel = None;
                i.stage = None;
                emit(app, "doctor", "done", "KohakuMoss 引擎裝好了");
                return Ok(());
            };
            i.stage = Some(stage);
            stage
        };
        match stage {
            "uv" | "source" => archive_stage(app, stage, &dir, cancel).await?,
            _ => command_stage(app, stage, &dir, cancel).await?,
        }
        completed = Some(stage);
    }
}

pub(super) async fn install(State(app): State<Shared>) -> ApiResult {
    if !cfg!(windows) {
        return Err(bad("KohakuMoss 引擎安裝目前支援 Windows"));
    }
    let operation = OPERATION.try_lock().map_err(|_| bad("KohakuMoss 引擎正在安裝或移除"))?;
    let token = CancellationToken::new();
    *inst() = Install {
        cancel: Some(token.clone()),
        ..Install::default()
    };
    tokio::spawn(async move {
        if let Err(e) = install_run(&app, &token).await {
            let mut i = inst();
            let stage = i.stage.take().unwrap_or("idle");
            i.cancel = None;
            let (state, msg) = if token.is_cancelled() {
                ("idle", CANCELED.to_owned())
            } else {
                ("error", e)
            };
            emit(&app, stage, state, &msg);
            i.error = msg;
        }
        drop(operation);
    });
    ok(json!({"ok":true}))
}

pub(super) async fn cancel() -> ApiResult {
    if let Some(token) = &inst().cancel {
        token.cancel();
    }
    ok(json!({"ok":true}))
}

const BUILD_CONFIRM: &str = "會用 winget 叫出微軟的 Visual Studio Build Tools 安裝程式，約下載 2～6GB，會跳出系統權限確認。要繼續嗎？";
const BUILD_URL: &str = "https://visualstudio.microsoft.com/visual-cpp-build-tools/";
const HEAL_INCOMPLETE: &str = "heal 檔案不完整（大小或 SHA256 不對），請再下載一次";

fn row(id: &str, state: &str, text: &str) -> Value {
    json!({"id":id, "state":state, "text":text})
}

pub(super) fn parse_compute_cap(s: &str) -> Option<f64> {
    s.rsplit(',')
        .next()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|c| c.is_finite() && *c >= 0.0)
}

pub(super) fn gpu_row(name: &str, cap: Option<f64>, vram_gb: f64) -> Value {
    let Some(cap) = cap else {
        return row("gpu", "bad", "KohakuMoss 只支援 NVIDIA 顯卡");
    };
    if cap < 8.0 {
        return row("gpu", "bad", "這張卡不支援 KohakuMoss（需要 bf16）");
    }
    if vram_gb < 6.0 {
        return row("gpu", "bad", &format!("{name} 顯存 {vram_gb:.1}GB，訓練至少需要 6GB"));
    }
    if vram_gb <= 7.0 {
        return row("gpu", "warn", &format!("{name} 顯存 {vram_gb:.1}GB，6GB 模式沒實測"));
    }
    row("gpu", "ok", &format!("{name} 顯存 {vram_gb:.1}GB，運算能力 {cap:.1}"))
}

fn gpu_and_driver(gpus: &[Value]) -> (Value, Value) {
    let card = gpus
        .iter()
        .filter(|g| g["vendor"] == "nvidia")
        .max_by_key(|g| g["vram_mb"].as_u64().unwrap_or(0));
    let Some(g) = card else {
        return (gpu_row("", None, 0.0), row("driver", "bad", "沒有 NVIDIA 顯卡"));
    };
    let gpu = gpu_row(
        g["name"].as_str().unwrap_or("NVIDIA 顯卡"),
        g["compute_cap"].as_str().and_then(parse_compute_cap),
        g["vram_mb"].as_f64().unwrap_or(0.0) / 1024.0,
    );
    let version = g["driver"].as_str().unwrap_or("").trim();
    let driver = if g["driver_missing"] == true || !version.starts_with(|c: char| c.is_ascii_digit()) {
        let mut r = row("driver", "bad", "NVIDIA 驅動沒裝好，先更新驅動");
        r["fix"] = json!({"action":"open_url", "label":"下載驅動", "url":"https://www.nvidia.com/Download/index.aspx"});
        r
    } else {
        row("driver", "ok", &format!("NVIDIA 驅動版本 {version}"))
    };
    (gpu, driver)
}

pub(super) fn doctor_rows(doctor_json: &Value) -> Vec<Value> {
    let mut rows = Vec::new();
    for item in doctor_json["missing"].as_array().into_iter().flatten() {
        let Some(what) = item["what"].as_str().or_else(|| item.as_str()).filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let lower = what.to_ascii_lowercase();
        if lower.contains("msvc") || lower.contains("build tools") || lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "cl") {
            rows.push(row("buildtools", "warn", "上次引擎健檢找不到 C++ 編譯器"));
            continue;
        }
        let text = if let Some(package) = what.strip_suffix(" is not installed") {
            format!("缺少套件 {package}")
        } else if let Some((installed, required)) = what.split_once(" does not satisfy ") {
            format!("套件 {installed} 不符合版本需求 {required}")
        } else if lower.contains("torch is a cpu build") {
            "PyTorch 只有 CPU 版，需要安裝 CUDA 版".into()
        } else if lower.contains("sees no device") {
            "PyTorch 找不到顯卡，請檢查 NVIDIA 驅動與 CUDA 版本".into()
        } else if lower.starts_with("no cuda toolkit nvcc") {
            "找不到 CUDA 編譯器 nvcc，請檢查 CUDA Toolkit 安裝位置".into()
        } else if lower.starts_with("no windows sdk") {
            "缺少 Windows SDK，請在 Build Tools 安裝程式加入此元件".into()
        } else if let Some((backend, why)) = what.strip_prefix("backend ").and_then(|s| s.split_once(" unavailable: ")) {
            format!("{backend} 無法使用（{why}）")
        } else {
            format!("引擎健檢發現問題（{what}）")
        };
        rows.push(row("engine", "bad", &text));
    }
    rows
}

fn engine_and_buildtools(installed: bool, has_tools: bool, doctor: &Value) -> (Value, Value) {
    let mut engine = if installed {
        row(
            "engine",
            "ok",
            &format!("KohakuMoss 引擎（作者 KBlueLeaf）已安裝，版本 {}", &train::KOHAKU_COMMIT[..7]),
        )
    } else {
        let mut r = row("engine", "bad", "還沒安裝 KohakuMoss 引擎");
        r["fix"] = json!({"action":"install_engine", "label":"安裝引擎"});
        r
    };
    let mut tools = if has_tools {
        row("buildtools", "ok", "已安裝 Visual Studio C++ 編譯工具")
    } else {
        let mut r = row("buildtools", "warn", "找不到 Visual Studio C++ 編譯工具");
        r["fix"] = json!({"action":"install_buildtools", "label":"幫我裝", "confirm":BUILD_CONFIRM});
        r
    };
    for issue in doctor_rows(doctor) {
        if has_tools && issue["id"] == "buildtools" {
            continue;
        }
        let target = if issue["id"] == "buildtools" { &mut tools } else { &mut engine };
        target["text"] = json!(format!(
            "{}。{}",
            target["text"].as_str().unwrap_or_default(),
            issue["text"].as_str().unwrap_or_default()
        ));
        target["state"] = issue["state"].clone();
    }
    (engine, tools)
}

fn find_model(dir: &Path, names: &[&str]) -> Option<String> {
    let wanted: Vec<_> = names.iter().map(|n| n.to_lowercase()).collect();
    fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        (wanted.contains(&name.to_lowercase()) && entry.path().is_file()).then_some(name)
    })
}

fn model_rows(root: &Path, base: &str) -> [Value; 3] {
    let encoders = [
        "qwen3.5_9b_int8_convrot.safetensors",
        "qwen3.5_9b_bf16.safetensors",
        "qwen3.5_9b_w4a8_gptq.safetensors",
    ];
    let encoder = match find_model(&root.join("text_encoders"), &encoders) {
        Some(name) => row("text_encoder", "ok", &name),
        None => {
            let mut r = row("text_encoder", "bad", "找不到 MOSS 9B 文字編碼器");
            r["fix"] = json!({"action":"open_hub", "label":"去下載中心找 MOSS 文字編碼器"});
            r
        }
    };
    let vae = match find_model(&root.join("vae"), &["flux2-vae_small.safetensors", "flux2-vae.safetensors"]) {
        Some(name) => row("vae", "ok", &name),
        None => row("vae", "bad", "找不到 flux2-vae_small 或 flux2-vae 的 safetensors 檔案"),
    };
    let heal = base.eq_ignore_ascii_case(train::HEAL_FILE);
    let base = match find_model(&root.join("diffusion_models"), &[base]) {
        Some(name) if heal => row("base", "ok", &name),
        Some(name) => row("base", "ok", &format!("{name}（沒實測）")),
        None => {
            let mut r = row("base", "bad", &format!("找不到底模 {base}"));
            if heal {
                r["fix"] = json!({"action":"download_heal", "label":"下載 heal（5.4GB）", "confirm":"要下載 heal 底模（5.4GB）到 diffusion_models 嗎？"});
            }
            r
        }
    };
    [encoder, vae, base]
}

pub(super) fn buildtools_installed() -> bool {
    let Some(root) = std::env::var_os("ProgramFiles(x86)") else {
        return false;
    };
    let vswhere = PathBuf::from(root).join("Microsoft Visual Studio/Installer/vswhere.exe");
    if !vswhere.is_file() {
        return false;
    }
    let mut cmd = Command::new(vswhere);
    train_run::quiet(&mut cmd);
    cmd.args([
        "-products",
        "*",
        "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
        "-property",
        "installationPath",
    ])
    .kill_on_drop(true);
    tokio::runtime::Handle::current().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), cmd.output())
            .await
            .is_ok_and(|r| r.is_ok_and(|out| out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty()))
    })
}

fn disk_row(free: Option<u64>, installed: bool) -> Value {
    let Some(bytes) = free else {
        return row("disk", "warn", "讀不到資料磁碟的剩餘空間");
    };
    let gb = bytes as f64 / (1u64 << 30) as f64;
    let (state, reason) = if bytes >= 8 << 30 {
        ("ok", "")
    } else if installed {
        ("warn", "，訓練存檔還需要空間")
    } else {
        ("bad", "，安裝引擎至少需要 8GB")
    };
    row("disk", state, &format!("資料磁碟剩下 {gb:.1}GB{reason}"))
}

fn ok_to_start(rows: &[Value]) -> bool {
    rows.iter().all(|r| r["state"] != "bad")
}

pub(super) async fn checks(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    ok(blocking(move || {
        let base = q.get("base").map(String::as_str).unwrap_or(train::HEAL_FILE);
        let free = crate::maint::disk_usage(&app.paths.data).map(|(free, _)| free);
        checklist(&app, base, &crate::gpu::detect(false).0, buildtools_installed(), free)
    })
    .await)
}

pub(super) fn checklist(app: &Shared, base: &str, gpus: &[Value], has_tools: bool, free: Option<u64>) -> Value {
    let (gpu, driver) = gpu_and_driver(gpus);
    let installed = kohaku_installed(app);
    let doctor = read_json(&kohaku_dir(app).join("doctor.json")).unwrap_or_default();
    let (engine, tools) = engine_and_buildtools(installed, has_tools, &doctor);
    let [encoder, vae, base] = model_rows(&api_models::models_root(app), base);
    let rows = [gpu, driver, engine, encoder, vae, base, tools, disk_row(free, installed)];
    json!({"ok_to_start":ok_to_start(&rows), "rows":rows})
}

pub(super) async fn download_heal(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if body["confirm"] != true {
        return Err(bad("請先確認下載 heal 底模"));
    }
    let model = api_models::Known {
        url: train::HEAL_URL.into(),
        name: train::HEAL_FILE.into(),
        kind: Some("diffusion_models".into()),
        size: Some(train::HEAL_SIZE),
        sha256: Some(train::HEAL_SHA256.into()),
        sub: None,
        bearer: None,
        cookie: None,
    };
    let id = api_models::start_known_download(&app, model, None, Some(HEAL_INCOMPLETE));
    ok(json!({"ok":true, "id":id}))
}

fn build_log(app: &Shared, line: String) {
    log_event(app, &mut inst(), "buildtools", "running", line);
}

fn winget_fallback() -> ApiResult {
    ok(json!({"ok":false, "url":BUILD_URL}))
}

pub(super) async fn buildtools(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if body["confirm"] != true {
        return Err(bad("請先確認安裝 Visual Studio Build Tools"));
    }
    let mut lookup = Command::new("where.exe");
    train_run::quiet(&mut lookup);
    if !lookup.arg("winget").output().await.is_ok_and(|r| r.status.success()) {
        return winget_fallback();
    }
    let mut cmd = Command::new("winget");
    cmd.args([
        "install",
        "--id",
        "Microsoft.VisualStudio.2022.BuildTools",
        "-e",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--override",
        "--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended",
    ]);
    launch_buildtools(app, cmd)
}

fn launch_buildtools(app: Shared, mut cmd: Command) -> ApiResult {
    // 重複點擊時沿用已啟動的安裝工作。
    static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let Ok(guard) = RUNNING.try_lock() else {
        return ok(json!({"ok":true}));
    };
    train_run::quiet(&mut cmd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let Ok(mut child) = cmd.spawn() else { return winget_fallback() };
    build_log(&app, "已啟動 winget，準備安裝 Visual Studio Build Tools".into());
    tokio::spawn(async move {
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let (result, (), ()) = tokio::join!(
            child.wait(),
            train_run::forward_lines(&app, out, build_log),
            train_run::forward_lines(&app, err, build_log)
        );
        let (state, msg) = match result {
            Ok(status) if status.success() => ("done", "Visual Studio Build Tools 安裝完成".to_owned()),
            Ok(status) => ("error", format!("Build Tools 安裝未完成（{status}）")),
            Err(e) => ("error", format!("讀取 Build Tools 安裝結果失敗：{e}")),
        };
        log_event(&app, &mut inst(), "buildtools", state, msg);
        drop(guard);
    });
    ok(json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::super::{api_train, train_run, Body};
    use super::*;

    // 這兩個路由測試共用正式程式的單一安裝狀態。
    static ROUTE_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn compute_cap_and_gpu_thresholds() {
        assert_eq!(parse_compute_cap("NVIDIA GeForce RTX 3070, 8192 MiB, 8.6"), Some(8.6));
        assert_eq!(parse_compute_cap("8.6"), Some(8.6));
        assert_eq!(parse_compute_cap("N/A"), None);
        for (cap, gb, expected) in [
            (Some(8.6), 8.0, "ok"),
            (Some(7.5), 8.0, "bad"),
            (None, 8.0, "bad"),
            (Some(8.0), 6.0, "warn"),
            (Some(8.0), 7.0, "warn"),
            (Some(8.0), 5.9, "bad"),
        ] {
            assert_eq!(gpu_row("測試卡", cap, gb)["state"], expected);
        }
    }

    #[test]
    fn missing_compiler_warns_without_blocking_start() {
        assert!(doctor_rows(&json!({"missing":[]})).is_empty());
        let rows = doctor_rows(&json!({"missing":[{"what":"MSVC C++ Build Tools", "fix":"install …"}]}));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "buildtools");
        assert_eq!(rows[0]["state"], "warn");
        assert!(ok_to_start(&rows));
        assert!(!ok_to_start(&[gpu_row("", None, 0.0)]));
    }

    #[test]
    fn driver_uses_largest_nvidia_card_and_reports_missing_version() {
        let amd = json!({"vendor":"amd", "vram_mb":32768});
        let small = json!({"vendor":"nvidia", "name":"小卡", "vram_mb":6144, "driver":"580.1", "compute_cap":"8.0"});
        let large = json!({"vendor":"nvidia", "name":"大卡", "vram_mb":8192, "driver":"600.1", "compute_cap":"8.6"});
        let (gpu, driver) = gpu_and_driver(&[large.clone(), amd.clone(), small]);
        assert_eq!(gpu["state"], "ok");
        assert!(gpu["text"].as_str().unwrap().contains("大卡"));
        assert!(driver["text"].as_str().unwrap().contains("600.1"));
        assert_eq!(gpu_and_driver(&[amd]).1["text"], "沒有 NVIDIA 顯卡");
        for (key, value) in [("driver", json!("N/A")), ("driver_missing", json!(true))] {
            let mut card = large.clone();
            card[key] = value;
            let driver = gpu_and_driver(&[card]).1;
            assert_eq!(driver["state"], "bad");
            assert_eq!(driver["fix"]["action"], "open_url");
        }
    }

    #[test]
    fn vswhere_overrides_stale_compiler_issue() {
        let (_, tools) = engine_and_buildtools(true, true, &json!({"missing":["cl.exe missing"]}));
        assert_eq!(tools["state"], "ok");
        assert!(tools["fix"].is_null());
    }

    #[test]
    fn doctor_merges_missing_into_engine_and_tools() {
        let doctor = json!({"missing":[{"what":"cl.exe missing"}, {"what":"torch is not installed"}]});
        for installed in [false, true] {
            let (engine, tools) = engine_and_buildtools(installed, false, &doctor);
            assert_eq!(engine["state"], "bad");
            assert!(engine["text"].as_str().unwrap().contains("缺少套件 torch"));
            assert_eq!(tools["state"], "warn");
            assert!(tools["text"].as_str().unwrap().contains("找不到 C++ 編譯器"));
        }
        let (engine, tools) = engine_and_buildtools(true, false, &Value::Null);
        assert!(engine["text"].as_str().unwrap().contains(&train::KOHAKU_COMMIT[..7]));
        assert_eq!(tools["fix"]["confirm"], BUILD_CONFIRM);
        assert!(ok_to_start(&[engine, tools]));
        assert_eq!(
            engine_and_buildtools(false, false, &Value::Null).0["fix"]["action"],
            "install_engine"
        );
    }

    #[test]
    fn disk_threshold_depends_on_engine_installation() {
        for installed in [false, true] {
            assert_eq!(disk_row(Some(8 << 30), installed)["state"], "ok");
            assert_eq!(
                disk_row(Some((8 << 30) - 1), installed)["state"],
                if installed { "warn" } else { "bad" }
            );
        }
    }

    #[test]
    fn models_require_matching_files_in_their_own_directory() {
        let root = temp("模型");
        for dir in ["text_encoders", "vae", "diffusion_models"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("text_encoders/qwen3.5_4b_bf16.safetensors"), []).unwrap();
        fs::create_dir(root.join("vae/flux2-vae.safetensors")).unwrap();
        let [enc, vae, base] = model_rows(&root, train::HEAL_FILE);
        assert_eq!(enc["state"], "bad");
        assert_eq!(vae["state"], "bad");
        assert_eq!(base["fix"]["action"], "download_heal");
        fs::write(root.join("text_encoders/QWEN3.5_9B_BF16.SAFETENSORS"), []).unwrap();
        fs::write(root.join("vae/FLUX2-VAE_SMALL.safetensors"), []).unwrap();
        fs::write(root.join("diffusion_models").join(train::HEAL_FILE.to_uppercase()), []).unwrap();
        assert!(model_rows(&root, train::HEAL_FILE).iter().all(|r| r["state"] == "ok"));
        fs::write(root.join("diffusion_models/其他.safetensors"), []).unwrap();
        let other = &model_rows(&root, "其他.safetensors")[2];
        assert_eq!(other["state"], "ok");
        assert!(other["text"].as_str().unwrap().contains("沒實測"));
        assert!(model_rows(&root, "../其他.safetensors")[2].get("fix").is_none());
        assert_eq!(model_rows(&root, "../其他.safetensors")[2]["state"], "bad");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn repair_routes_require_explicit_boolean_confirmation() {
        use tower::ServiceExt;
        let app = station(&temp("confirm")).await;
        for route in ["download_heal", "buildtools"] {
            for body in ["", "{}", r#"{"confirm":false}"#, r#"{"confirm":"true"}"#] {
                let req = axum::http::Request::post(format!("/api/train/kohaku/{route}"))
                    .header("Host", "localhost:8190")
                    .body(axum::body::Body::from(body))
                    .unwrap();
                assert_eq!(super::super::router(app.clone()).oneshot(req).await.unwrap().status().as_u16(), 400);
            }
        }
    }

    #[tokio::test]
    async fn checklist_reads_installed_engine_doctor_and_comfy_models() {
        let app = station(&temp("清單")).await;
        let dir = kohaku_dir(&app);
        let exe = dir.join("repo/.venv/Scripts/kogine.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, []).unwrap();
        write_atomic(&dir.join("installed.json"), &json!({"commit": train::KOHAKU_COMMIT})).unwrap();
        let root = api_models::models_root(&app);
        for (kind, name) in [
            ("diffusion_models", train::HEAL_FILE),
            ("text_encoders", "qwen3.5_9b_int8_convrot.safetensors"),
            ("vae", "flux2-vae.safetensors"),
        ] {
            fs::create_dir_all(root.join(kind)).unwrap();
            fs::write(root.join(kind).join(name), []).unwrap();
        }
        let cards = [json!({"vendor":"nvidia", "name":"RTX 3070", "compute_cap":"8.6", "vram_mb":8192, "driver":"600.1"})];
        let check = || checklist(&app, train::HEAL_FILE, &cards, false, Some(12 << 30));
        let result = check();
        let rows = result["rows"].as_array().unwrap();
        let ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            ["gpu", "driver", "engine", "text_encoder", "vae", "base", "buildtools", "disk"]
        );
        assert_eq!(rows.iter().filter(|r| r["state"] == "warn").count(), 1);
        assert_eq!(result["ok_to_start"], true);
        write_atomic(&dir.join("doctor.json"), &json!({"missing":[{"what":"torch is not installed"}]})).unwrap();
        assert_eq!(check()["ok_to_start"], false);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn buildtools_fake_process_streams_log_and_missing_program_gives_url() {
        let _serial = ROUTE_TEST.lock().await;
        let app = station(&temp("buildtools")).await;
        let res = launch_buildtools(app.clone(), Command::new(temp("不存在.exe"))).unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"ok":false, "url":BUILD_URL})
        );
        let mut events = app.hub.subscribe();
        let mut fake = Command::new("cmd.exe");
        fake.args(["/d", "/c", "echo fake-compiler-log"]);
        let res = launch_buildtools(app.clone(), fake).unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), json!({"ok":true}));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let crate::hub::Msg::Text(s) = events.recv().await.unwrap() {
                    let event: Value = serde_json::from_str(&s).unwrap();
                    if event["stage"] == "buildtools" && event["state"] == "done" {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(inst().log.iter().any(|s| s == "fake-compiler-log"));
    }

    #[tokio::test]
    async fn checked_download_reports_integrity_and_only_places_verified_file() {
        let app = station(&temp("下載")).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().fallback(|| async { "abc" }))
                .await
                .unwrap();
        });
        let digest = format!("{:x}", Sha256::digest(b"abc"));
        let url = format!("http://{addr}/test.safetensors");
        let root = api_models::models_root(&app);
        let placed = root.join("diffusion_models/test.safetensors");
        fs::create_dir_all(placed.parent().unwrap()).unwrap();
        fs::write(&placed, b"existing").unwrap();
        for (size, sha, valid) in [(4, digest.as_str(), false), (3, "wrong", false), (3, digest.as_str(), true)] {
            if valid {
                fs::remove_file(&placed).unwrap();
            }
            let start = || {
                api_models::start_known_download(
                    &app,
                    api_models::Known {
                        url: url.clone(),
                        name: "test.safetensors".into(),
                        size: Some(size),
                        sha256: Some(sha.into()),
                        kind: Some("diffusion_models".into()),
                        sub: None,
                        bearer: None,
                        cookie: None,
                    },
                    None,
                    Some(HEAL_INCOMPLETE),
                )
            };
            let id = start();
            assert_eq!(start(), id);
            let errors = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let (_, _, over, errors) = api_models::progress_of(&[id]);
                    if over {
                        break errors;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if valid {
                assert!(errors.is_empty());
                assert_eq!(fs::read(&placed).unwrap(), b"abc");
            } else {
                assert_eq!(errors, [HEAL_INCOMPLETE]);
                assert_eq!(fs::read(&placed).unwrap(), b"existing");
            }
            let staging = root.join("_incoming").join(format!("verified-{}", crate::dl::tag(&url)));
            assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
        }
        server.abort();
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("繪匣 kohaku {name} {}", crate::director::new_id()))
    }

    async fn station(root: &Path) -> Shared {
        let mut paths = crate::paths::Paths::new(root, root, Some(root.join("comfy")));
        paths.res = root.join("resources");
        super::super::start(paths, "http://127.0.0.1:9").await.unwrap()
    }

    #[test]
    fn damaged_uv_never_installs_an_executable() {
        let dir = temp("checksum");
        fs::create_dir_all(&dir).unwrap();
        for size in [128, UV_SIZE] {
            assert_eq!(
                unpack_uv(&dir, vec![0; size].into(), &CancellationToken::new()).unwrap_err(),
                UV_INCOMPLETE
            );
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        }
        fs::remove_dir(dir).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancel_terminates_child_and_grandchild() {
        let app = station(&temp("cancel")).await;
        let mut events = app.hub.subscribe();
        let token = CancellationToken::new();
        // 假安裝指令：父、子 PowerShell 各自存活一分鐘，透過共用的 log 管線回報 PID。
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command",
            "$child = Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep 60' -PassThru; [Console]::WriteLine(\"tree=$PID,$($child.Id)\"); $child.WaitForExit()"]);
        let work = {
            let app = app.clone();
            let token = token.clone();
            tokio::spawn(async move {
                train_run::run_logged_to(&app, cmd, |app, line| emit(app, "torch", "running", &line), Some(&token)).await
            })
        };
        let ids = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let crate::hub::Msg::Text(text) = events.recv().await.unwrap() {
                    let v: Value = serde_json::from_str(&text).unwrap();
                    if let Some(ids) = v["msg"].as_str().and_then(|s| s.strip_prefix("tree=")) {
                        assert_eq!(v["engine"], "kohakumoss");
                        return ids.split(',').map(|id| id.parse::<u32>().unwrap()).collect::<Vec<_>>();
                    }
                }
            }
        })
        .await
        .unwrap();
        token.cancel();
        tokio::time::timeout(Duration::from_secs(15), work).await.unwrap().unwrap().unwrap();
        assert_eq!(ids.len(), 2);
        let mut check = Command::new("powershell");
        train_run::quiet(&mut check);
        check.args(["-NoProfile", "-NonInteractive", "-Command"]).arg(format!(
            "if (Get-Process -Id {},{} -ErrorAction SilentlyContinue) {{ exit 1 }}; exit 0",
            ids[0], ids[1]
        ));
        assert!(check.status().await.unwrap().success());
    }

    #[test]
    fn install_resumes_from_unfinished_stage() {
        assert_eq!(next_stage(&[]), Some("uv"));
        let dir = temp("resume");
        let done = ["uv", "source", "venv"].map(String::from);
        save_stages(&dir, &done).unwrap();
        assert_eq!(next_stage(&load_stages(&dir)), Some("torch"));
        assert_eq!(next_stage(&STAGES.map(String::from)), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn installed_needs_matching_commit() {
        let app = station(&temp("commit")).await;
        let dir = kohaku_dir(&app);
        let exe = dir.join("repo/.venv/Scripts/kogine.exe");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, []).unwrap();
        for (commit, expected) in [("old", false), (train::KOHAKU_COMMIT, true)] {
            crate::director::write_atomic(&dir.join("installed.json"), &json!({ "commit": commit })).unwrap();
            assert_eq!(kohaku_installed(&app), expected);
        }
        std::fs::remove_file(exe).unwrap();
        assert!(!kohaku_installed(&app));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancel_preserves_checkpoint_and_restart_publishes_install() {
        let _serial = ROUTE_TEST.lock().await;
        let app = station(&temp("restart")).await;
        let dir = kohaku_dir(&app);
        for file in [
            "uv.exe",
            "repo/pyproject.toml",
            "repo/.venv/Scripts/python.exe",
            "repo/.venv/Scripts/kogine.exe",
        ] {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, []).unwrap();
        }
        let stages = STAGES.map(String::from);
        save_stages(&dir, &stages[..3]).unwrap();
        // 單執行緒 runtime：背景工作第一次被排程前送出取消，不執行任何安裝指令。
        assert_eq!(install(State(app.clone())).await.unwrap().status().as_u16(), 200);
        assert_eq!(cancel().await.unwrap().status().as_u16(), 200);
        drop(OPERATION.lock().await);
        let view = kohaku_view(&app);
        assert_eq!(view["error"], CANCELED);
        assert_eq!(view["stage"], "idle");
        assert_eq!(view["busy"], false);
        assert_eq!(view["stages_done"], json!(stages[..3]));
        assert!(!dir.join("installed.json").exists());

        // 模擬最後一段存檔之後、寫 installed.json 之前關掉繪匣，從正式安裝路由補完。
        save_stages(&dir, &stages).unwrap();
        write_atomic(&dir.join("doctor.json"), &json!({"missing":["測試"]})).unwrap();
        use tower::ServiceExt;
        let router = super::super::router(app.clone());
        let request = axum::http::Request::post("/api/train/kohaku/install")
            .header("Host", "localhost:8190")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(router.oneshot(request).await.unwrap().status().as_u16(), 200);
        drop(OPERATION.lock().await);
        let trainer = train_run::trainer_view(&app);
        assert_eq!(trainer["engines"]["kohakumoss"]["installed"], true);
        assert_eq!(trainer["engines"]["kohakumoss"]["error"], "");
        assert_eq!(trainer["installed"], trainer["engines"]["toolkit"]["installed"]);
        assert!(read_json(&dir.join("installed.json")).unwrap()["at"].is_i64());
    }

    #[tokio::test]
    async fn remove_requires_confirm_and_idle() {
        let _serial = ROUTE_TEST.lock().await;
        let app = station(&temp("remove")).await;
        let remove = |body| train_run::trainer_remove(State(app.clone()), Body(body));
        assert_eq!(remove(json!({"engine":"kohakumoss"})).await.unwrap_err().0.as_u16(), 400);
        let body = json!({"engine":"kohakumoss", "confirm":true});
        {
            let _operation = OPERATION.lock().await;
            assert_eq!(remove(body.clone()).await.unwrap_err().0.as_u16(), 400);
        }
        let id = "kohaku-remove-test";
        api_train::job_begin(id, "train").unwrap();
        for engine in ["toolkit", "kohakumoss"] {
            assert_eq!(remove(json!({"engine":engine, "confirm":true})).await.unwrap_err().0.as_u16(), 400);
        }
        api_train::job_end(id);
        std::fs::create_dir_all(kohaku_dir(&app)).unwrap();
        let marker = train_run::trainer_dir(&app).join("installed.json");
        std::fs::write(&marker, []).unwrap();
        assert_eq!(remove(body).await.unwrap().status().as_u16(), 200);
        assert!(marker.is_file());
        assert!(!kohaku_dir(&app).exists());
    }

    #[test]
    fn remove_toolkit_keeps_kohakumoss() {
        let root = temp("toolkit");
        let dir = root.join("trainer");
        for file in ["ai-toolkit/x", "installed.json", "kohakumoss/repo/y"] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file).unwrap();
        }
        train_run::remove_toolkit_files(&dir).unwrap();
        assert_eq!(dir.read_dir().unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(dir.join("kohakumoss/repo/y")).unwrap(), "kohakumoss/repo/y");
        std::fs::remove_dir_all(root).unwrap();
    }
}

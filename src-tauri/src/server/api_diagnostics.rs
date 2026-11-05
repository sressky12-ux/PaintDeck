//! Diagnostic ownership and local download endpoints. No model/image bytes or credentials are read.
use super::*;
use crate::diagnostics::{self, Policy};
use futures_util::StreamExt;

fn tracked(path: &str) -> bool {
    matches!(
        path,
        "/api/generate"
            | "/api/jobs/retry"
            | "/api/pe/rewrite"
            | "/api/agent/say"
            | "/api/agent/loras/learn"
            | "/api/director/describe"
            | "/api/director/write"
            | "/api/director/run"
            | "/api/director/concat"
            | "/api/comic/run"
            | "/api/comic/script"
            | "/api/h3/generate"
            | "/api/h3/write"
            | "/api/h3/chain/run"
            | "/api/h3/chain/join"
            | "/api/shotsheet/polish"
            | "/api/voices/tts"
    )
}

pub async fn capture(State(app): State<Shared>, req: Request, next: Next) -> Response {
    if req.method() != axum::http::Method::POST || !tracked(req.uri().path()) {
        return next.run(req).await;
    }
    let route = req.uri().path().to_string();
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error":"請求資料過大或無法讀取"})),
            )
                .into_response()
        }
    };
    let data = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| json!({"status":"invalid_json","bytes":bytes.len()}));
    let req = Request::from_parts(parts, axum::body::Body::from(bytes));
    let a = app.clone();
    let r = route.clone();
    let run = blocking(move || a.diagnostics.begin(&r, &data)).await;
    let Some(run) = run else {
        let mut response = next.run(req).await;
        response.headers_mut().insert(
            "x-anima-diagnostic",
            HeaderValue::from_static("unavailable"),
        );
        return response;
    };
    let id = run.id.clone();
    let mut response = diagnostics::scope(run.clone(), next.run(req)).await;
    run.http_finished(response.status().as_u16());
    if response.status().is_client_error() || response.status().is_server_error() {
        let (parts, body) = response.into_parts();
        match axum::body::to_bytes(body, 1024 * 1024).await {
            Ok(bytes) => {
                let data = serde_json::from_slice::<Value>(&bytes)
                    .unwrap_or_else(|_| json!({"message":String::from_utf8_lossy(&bytes)}));
                run.snapshot("http_error", &data);
                if let Some(s) = data["error"].as_str() {
                    run.fail(s);
                }
                response = Response::from_parts(parts, axum::body::Body::from(bytes));
            }
            Err(_) => {
                response = (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":"錯誤回應無法讀取，請查看診斷紀錄"})),
                )
                    .into_response()
            }
        }
    }
    if let Ok(value) = HeaderValue::from_str(&id) {
        response
            .headers_mut()
            .insert("x-anima-diagnostic-id", value);
    }
    response
}

pub async fn list(State(app): State<Shared>) -> ApiResult {
    let store = app.diagnostics.clone();
    ok(blocking(move || store.status()).await)
}
pub async fn job(
    State(app): State<Shared>,
    axum::extract::Path(job): axum::extract::Path<String>,
) -> ApiResult {
    ok(json!({"diagnostic_id":app.diagnostics.job_id(&job)}))
}
pub async fn preferences(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let policy: Policy = serde_json::from_value(body).map_err(|_| bad("診斷保留設定格式不對"))?;
    let store = app.diagnostics.clone();
    blocking(move || store.preferences(policy))
        .await
        .map_err(|e| bad(e.to_string()))?;
    ok(app.diagnostics.status())
}
pub async fn clear(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if body["confirm"] != true {
        return Err(bad("清除診斷紀錄需要確認"));
    }
    let all = body["all"] == true;
    let store = app.diagnostics.clone();
    ok(blocking(move || store.cleanup(all)).await)
}
pub async fn report(
    State(app): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Body(body): Body,
) -> ApiResult {
    let store = app.diagnostics.clone();
    blocking(move || {
        store.report(
            &id,
            &json!({"expected":body["expected"],"actual":body["actual"]}),
        )
    })
    .await
    .map_err(|e| bad(e.to_string()))?;
    ok(json!({"ok":true}))
}
pub async fn export(
    State(app): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult {
    let store = app.diagnostics.clone();
    let filename = format!("paintdeck-{id}.zip");
    let export = blocking(move || store.export(&id))
        .await
        .map_err(|e| bad(e.to_string()))?;
    let file = tokio::fs::File::open(&export.path)
        .await
        .map_err(internal)?;
    let stream = futures_util::stream::unfold(
        (tokio_util::io::ReaderStream::new(file), export),
        |(mut reader, lease)| async move { reader.next().await.map(|result| (result, (reader, lease))) },
    );
    let mut response = Response::new(axum::body::Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")).map_err(internal)?,
    );
    Ok(response)
}

/// POST /api/diagnostics/{id}/save：診斷包直接存進這台電腦的「下載」資料夾，再用檔案總管選取它。
/// 2026-11-05 回報：在繪匣視窗裡按「下載診斷包」只跳提示、沒有下載（視窗內 blob 下載不可靠）；網頁在本機開時改走這條
pub async fn save(
    State(app): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult {
    let store = app.diagnostics.clone();
    let dest = blocking(move || -> std::io::Result<std::path::PathBuf> {
        let export = store.export(&id)?;
        let dir = downloads_dir();
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(format!("paintdeck-{id}.zip"));
        std::fs::copy(&export.path, &dest)?;
        Ok(dest)
    })
    .await
    .map_err(|e| bad(e.to_string()))?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 路徑有空白時 /select, 後面要整段加引號（同 api_wf::reveal）
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", dest.display())).spawn();
    }
    ok(json!({"ok": true, "path": dest.to_string_lossy()}))
}

/// 使用者的「下載」資料夾：登錄檔 User Shell Folders 的 Downloads（搬到 D 槽的也認得），讀不到＝%USERPROFILE%\Downloads
fn downloads_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (k, n) = (w(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders"), w("{374DE290-123F-4565-9164-39C4925E467B}"));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // RRF_RT_REG_SZ 會順便展開 REG_EXPAND_SZ 的 %USERPROFILE%
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r == 0 {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let p = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..end]));
            if p.is_absolute() {
                return p;
            }
        }
    }
    std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Downloads")
}

async fn quick_get(app: &Shared, path: &str) -> Result<Value, ()> {
    tokio::time::timeout(std::time::Duration::from_millis(1500), app.comfy.get(path))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
pub fn start_monitor(app: &Shared) {
    let cleanup_weak = Arc::downgrade(app);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
            let Some(app) = cleanup_weak.upgrade() else {
                break;
            };
            let store = app.diagnostics.clone();
            drop(app);
            blocking(move || store.cleanup(false)).await;
        }
    });
    let weak = Arc::downgrade(app);
    tokio::spawn(async move {
        let mut ticks = 0u64;
        let mut absent = std::collections::HashMap::<String, u8>::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let Some(app) = weak.upgrade() else { break };
            ticks += 1;
            let ids = app.diagnostics.active_ids();
            if !ids.is_empty() {
                let stats = tokio::time::timeout(
                    std::time::Duration::from_millis(1500),
                    app.comfy.get("/system_stats"),
                )
                .await;
                let metrics = match stats {
                    Ok(Ok(stats)) => {
                        json!({"source":"comfyui_system_stats","scope":"backend reported counters, not per-process attribution","sampled_at":diagnostics::now(),
                        "ram_total":stats["system"]["ram_total"],"ram_free":stats["system"]["ram_free"],
                        "devices":stats["devices"].as_array().map(|a|a.iter().map(|d|json!({"name":d["name"],"type":d["type"],"index":d["index"],
                            "vram_total":d["vram_total"],"vram_free":d["vram_free"],"torch_vram_total":d["torch_vram_total"],"torch_vram_free":d["torch_vram_free"]})).collect::<Vec<_>>()),
                        "versions":{"comfyui":stats["system"]["comfyui_version"],"python":stats["system"]["python_version"],"pytorch":stats["system"]["pytorch_version"]}})
                    }
                    _ => {
                        json!({"source":"comfyui_system_stats","status":"unavailable","sampled_at":diagnostics::now(),"reason":"not_reachable_or_timed_out"})
                    }
                };
                let store = app.diagnostics.clone();
                blocking(move || {
                    for id in ids {
                        store.sample_logs(&id);
                        store.event(&id, "resources", metrics.clone());
                    }
                })
                .await;
            }
            if ticks.is_multiple_of(5) {
                let jobs = app
                    .diagnostics
                    .pending_jobs()
                    .into_iter()
                    .filter(|p| !p.starts_with("cloud-"))
                    .collect::<Vec<_>>();
                if !jobs.is_empty() {
                    if let Ok(queue) = quick_get(&app, "/queue").await {
                        let live = ["queue_running", "queue_pending"]
                            .iter()
                            .flat_map(|k| queue[*k].as_array().into_iter().flatten())
                            .filter_map(|x| x[1].as_str())
                            .collect::<Vec<_>>();
                        let len = jobs.len();
                        for job in jobs
                            .into_iter()
                            .cycle()
                            .skip((ticks as usize / 5) * 8 % len)
                            .take(len.min(8))
                        {
                            if let Ok(history) = quick_get(&app, &format!("/history/{job}")).await {
                                if let Some(entry) = history.get(&job) {
                                    absent.remove(&job);
                                    app.diagnostics.job_event(&job,"history_snapshot",&json!({"status":entry["status"],"outputs":entry["outputs"]}));
                                    if entry["status"]["status_str"] == "success" {
                                        app.diagnostics.job_event(
                                            &job,
                                            "execution_success",
                                            &entry["status"],
                                        );
                                    } else if entry["status"]["completed"] == true
                                        || entry["status"]["status_str"] == "error"
                                    {
                                        app.diagnostics.job_event(
                                            &job,
                                            "execution_error",
                                            &entry["status"],
                                        );
                                    }
                                } else if !live.contains(&job.as_str()) {
                                    let n = absent.entry(job.clone()).or_default();
                                    *n += 1;
                                    if *n >= 3 {
                                        app.diagnostics.job_missing(&job);
                                        absent.remove(&job);
                                    }
                                } else {
                                    absent.remove(&job);
                                }
                            }
                        }
                    }
                }
            }
        }
    });
}

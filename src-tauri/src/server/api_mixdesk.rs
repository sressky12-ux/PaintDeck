//! 混音台 API（設計 docs/superpowers/specs/2026-11-04-mixdesk-panel-design.md，後端邏輯在 crate::mixdesk）。
//!   GET  /mixdesk                          頁面（導演 → 混音台的 iframe）
//!   GET  /api/mixdesk/status               {ok, dir, error?}
//!   GET  /api/mixdesk/songs                曲子清單
//!   GET  /api/mixdesk/song?path=           mixdesk inspect 的結果＋mtime
//!   GET  /api/mixdesk/mtime?path=          {mtime}（面板每 2 秒問一次，不跑 Python）
//!   POST /api/mixdesk/new {title, tempo, time}
//!   POST /api/mixdesk/apply {path, edits}  mixdesk apply（edits＝{v, base_hash, ops}）
//!   POST /api/mixdesk/render {path}        → {job}；GET /api/mixdesk/job/{id}；POST /api/mixdesk/job/{id}/stop
//!   GET  /api/mixdesk/file?path=           音檔／圖／報告（支援 Range）；不在 mixdesk 資料夾底下一律 403
//!   GET  /api/mixdesk/video?song=          這首配的影片：gallery:<編號> 轉到 /img/<編號>，檔案路徑直接送（支援 Range）
//!   GET  /api/mixdesk/instruments          樂器目錄
//!   POST /api/mixdesk/audition {instrument, notes?}  → {path}
//!   POST /api/mixdesk/to_studio {path, stems}       把 mix.wav（和各軌分軌）放進剪輯台素材
use super::{bad, internal, ok, ApiError, ApiResult, Body, Shared};
use crate::director::tx;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const QUICK: Duration = Duration::from_secs(120);

fn forbidden(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, msg.into())
}

pub async fn page(State(app): State<Shared>) -> ApiResult {
    let html = tokio::fs::read_to_string(app.paths.web.join("mixdesk.html"))
        .await
        .map_err(|_| super::not_found("找不到混音台頁面"))?;
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], super::versioned_html(&html)).into_response())
}

pub async fn status(State(app): State<Shared>) -> ApiResult {
    let dir = app.mixdesk.dir();
    match app.mixdesk.check() {
        Ok(()) => ok(json!({"ok": true, "dir": dir})),
        Err(e) => ok(json!({"ok": false, "dir": dir, "error": e})),
    }
}

pub async fn songs(State(app): State<Shared>) -> ApiResult {
    let md = app.mixdesk.clone();
    let list = super::blocking(move || md.songs()).await.map_err(bad)?;
    ok(json!({"songs": list}))
}

fn song_arg(app: &Shared, q: &HashMap<String, String>, key: &str) -> Result<PathBuf, ApiError> {
    app.mixdesk.check().map_err(bad)?;
    let p = q.get(key).map(String::as_str).unwrap_or_default();
    app.mixdesk.song_path(p).map_err(bad)
}

pub async fn song(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let p = song_arg(&app, &q, "path")?;
    let mut v = app
        .mixdesk
        .json(vec!["inspect".into(), p.clone().into_os_string(), "--json".into()], QUICK)
        .await
        .map_err(bad)?;
    app.mixdesk.remember_video(&p, &v);
    v["mtime"] = json!(crate::mixdesk::mtime(&p));
    ok(v)
}

pub async fn mtime(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let p = song_arg(&app, &q, "path")?;
    ok(json!({"mtime": crate::mixdesk::mtime(&p)}))
}

pub async fn new_song(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let tempo = b["tempo"].as_f64().unwrap_or(120.0);
    let time = b["time"].as_str().unwrap_or("4/4").to_string();
    let title = tx(&b["title"]);
    let md = app.mixdesk.clone();
    let p = super::blocking(move || md.new_song(&title, tempo, &time)).await.map_err(bad)?;
    ok(json!({"path": p}))
}

pub async fn apply(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = song_arg(&app, &HashMap::from([("path".into(), tx(&b["path"]))]), "path")?;
    if !b["edits"].is_object() {
        return Err(bad("缺少 edits"));
    }
    let dir = app.mixdesk.dir().join(".tmp").join("panel_edits");
    tokio::fs::create_dir_all(&dir).await.map_err(internal)?;
    let tmp = dir.join(format!("{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S-%6f")));
    tokio::fs::write(&tmp, serde_json::to_vec(&b["edits"]).map_err(internal)?).await.map_err(internal)?;
    let r = app
        .mixdesk
        .json(vec!["apply".into(), p.into_os_string(), tmp.clone().into_os_string(), "--json".into()], QUICK)
        .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    ok(r.map_err(bad)?)
}

pub async fn render(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = song_arg(&app, &HashMap::from([("path".into(), tx(&b["path"]))]), "path")?;
    ok(json!({"job": app.mixdesk.start_render(p)}))
}

pub async fn job(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    app.mixdesk.job(&id).map(ok).unwrap_or_else(|| Err(super::not_found("沒有這個渲染工作")))
}

pub async fn job_stop(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    if app.mixdesk.stop(&id) {
        ok(json!({"ok": true}))
    } else {
        Err(super::not_found("沒有這個渲染工作"))
    }
}

async fn serve(path: &Path, req: Request) -> ApiResult {
    let res = ServeFile::new(path).oneshot(req).await.map_err(internal)?;
    Ok(res.map(axum::body::Body::new).into_response())
}

fn query(req: &Request) -> HashMap<String, String> {
    Query::<HashMap<String, String>>::try_from_uri(req.uri()).map(|q| q.0).unwrap_or_default()
}

pub async fn file(State(app): State<Shared>, req: Request) -> ApiResult {
    let p = PathBuf::from(query(&req).get("path").cloned().unwrap_or_default());
    if !app.mixdesk.allowed(&p) {
        return Err(forbidden("只能讀 mixdesk 資料夾底下的音檔、圖和報告"));
    }
    serve(&p, req).await
}

pub async fn video(State(app): State<Shared>, req: Request) -> ApiResult {
    let q = query(&req);
    let song = song_arg(&app, &q, "song")?;
    let v = app.mixdesk.video_of(&song).ok_or_else(|| super::not_found("這首還沒有設定影片"))?;
    if let Some(id) = v.strip_prefix("gallery:") {
        let id: i64 = id.trim().parse().map_err(|_| bad(format!("影片編號看不懂：{v}")))?;
        return Ok(Redirect::temporary(&format!("/img/{id}")).into_response());
    }
    let mut p = PathBuf::from(&v);
    if p.is_relative() {
        p = song.parent().unwrap_or(Path::new(".")).join(p);
    }
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    if crate::studio::media_kind(&format!("x.{ext}")) != Some("video") || !p.is_file() {
        return Err(super::not_found(format!("找不到影片檔：{}", p.display())));
    }
    serve(&p, req).await
}

pub async fn instruments(State(app): State<Shared>) -> ApiResult {
    ok(app.mixdesk.instruments().await.map_err(bad)?)
}

pub async fn audition(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let inst = tx(&b["instrument"]);
    if inst.is_empty() {
        return Err(bad("缺少 instrument"));
    }
    let mut args: Vec<OsString> = vec!["audition".into(), inst.into()];
    if let Some(n) = b["notes"].as_str().filter(|s| !s.trim().is_empty()) {
        args.push("--notes".into());
        args.push(n.into());
    }
    args.push("--json".into());
    ok(app.mixdesk.json(args, QUICK).await.map_err(bad)?)
}

pub async fn to_studio(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = song_arg(&app, &HashMap::from([("path".into(), tx(&b["path"]))]), "path")?;
    let r = app
        .mixdesk
        .json(vec!["inspect".into(), p.into_os_string(), "--json".into()], QUICK)
        .await
        .map_err(bad)?;
    let render = &r["render"];
    let mix = render["mix"].as_str().ok_or_else(|| bad("這首還沒有渲染過，先按「套用＋重新渲染」"))?;
    let title = tx(&r["title"]);
    // 剪輯台的檔名只留英數（中文會變底線），所以用 _mix、_stem_<軌名>；回給面板的是剪輯台實際登記的名字
    let mut files = vec![(PathBuf::from(mix), format!("{title}_mix.wav"))];
    if b["stems"].as_bool().unwrap_or(false) {
        let stems = PathBuf::from(tx(&render["out_dir"])).join("stems");
        if let Ok(rd) = std::fs::read_dir(&stems) {
            let mut list: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "wav")).collect();
            list.sort();
            for f in list {
                let stem = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                files.push((f, format!("{title}_stem_{stem}.wav")));
            }
        }
    }
    let mut imported = vec![];
    for (f, name) in files {
        let e = app.studio.import(&f, &name).await.map_err(bad)?;
        imported.push(e["name"].as_str().map(str::to_string).unwrap_or(name));
    }
    let srt = PathBuf::from(tx(&render["out_dir"])).join("dialogue.srt");
    // 剪輯台沒有匯入字幕檔的入口：字幕檔路徑回給面板，請使用者自己拖進去
    ok(json!({"imported": imported, "srt": if srt.is_file() { json!(srt) } else { Value::Null }}))
}

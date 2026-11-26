//! 影音工作台的 API（導演頁的「剪輯台」「拆幀台」；外面的 AI 也用這些）。程式本體在 crate::studio。
//!
//!   GET  /api/studio/status                 剪輯台開了沒、工具在哪
//!   POST /api/studio/start | stop           開／關剪輯台（FableCut，127.0.0.1:8194）
//!   POST /api/studio/import {id}            圖庫的圖或影片放進剪輯台的素材區
//!   POST /api/studio/open {which}          檔案總管打開 exports／media／sfx 資料夾
//!   GET  /api/studio/exports                剪輯台匯出的影片
//!   POST /api/studio/exports/save {name}    匯出的影片存回圖庫
//!   GET  /api/studio/ai                     給 AI 的說明（中文）＋ MCP 設定
//!   GET  /api/studio/frames                 拆過的影片
//!   POST /api/studio/frames/split {id}      圖庫的影片拆成一張張
//!   GET  /api/studio/frames/{sid}           幾幀、fps、換過哪幾幀
//!   GET  /api/studio/frames/{sid}/f/{n}     第 n 幀原圖（PNG）；/t/{n} 縮圖（JPEG）
//!   GET  /api/studio/frames/{sid}/sheet?from=&to=&cols=   對照表（縮圖排成格子、標幀號）
//!   POST /api/studio/frames/{sid}/edit {n, op: gallery|copy|blend|restore, image_id?, from?}
//!   POST /api/studio/frames/{sid}/upload?n= （內容是圖檔）  用上傳的圖換掉第 n 幀
//!   POST /api/studio/frames/{sid}/to_input {n}            第 n 幀送到 ComfyUI input（拿去局部重繪、圖生圖）
//!   POST /api/studio/frames/{sid}/build     接回影片存進圖庫
//!   POST /api/studio/frames/{sid}/delete    整份移到待刪除
use super::api_gallery::{file_of, is_video};
use super::{bad, blocking, file_response, internal, not_found, ok, ApiResult, Body, Shared};
use crate::director::tx;
use crate::studio::{self, Tools};
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use serde_json::{json, Value};
use std::collections::HashMap;

fn tools(app: &Shared) -> Tools {
    studio::tools(&app.paths.data)
}

fn int(v: &Value) -> Option<i64> {
    crate::py::int(v)
}

pub async fn status(State(app): State<Shared>) -> ApiResult {
    ok(app.studio.status().await)
}

pub async fn start(State(app): State<Shared>) -> ApiResult {
    app.studio.start().await.map_err(bad).and_then(ok)
}

pub async fn stop(State(app): State<Shared>) -> ApiResult {
    app.studio.stop().await.map_err(bad)?;
    ok(json!({"ok": true}))
}

/// {id}：圖庫的一張圖或一支影片 → 剪輯台的素材
pub async fn import(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = int(&b["id"]).ok_or_else(|| bad("缺少 id"))?;
    let r = app.db.image(id)?.ok_or_else(|| not_found("圖庫裡沒有這一張"))?;
    let src = file_of(&app.paths, &r);
    if !src.is_file() {
        return Err(not_found("檔案不在硬碟上了"));
    }
    let fname = tx(&r["filename"]);
    let ext = std::path::Path::new(&fname)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "png".into());
    // 素材名稱：圖庫編號＋原檔名（剪輯台裡認得出是哪一張）
    let name = format!("anima_{id}.{ext}");
    let m = app.studio.import(&src, &name).await.map_err(bad)?;
    ok(json!({"ok": true, "media": m}))
}

/// {which: exports|media|sfx}：用檔案總管打開剪輯台的資料夾
pub async fn open_folder(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    app.studio.open_folder(&tx(&b["which"])).map_err(bad)?;
    ok(json!({"ok": true}))
}

pub async fn exports(State(app): State<Shared>) -> ApiResult {
    ok(json!({"exports": app.studio.exports(), "dir": app.studio.studio_dir().join("exports")}))
}

/// 一支影片檔存進圖庫：檔案搬到 output\AnimaWeb\、第一格存成封面，登記成 mode=video
pub(crate) async fn register_video(
    app: &Shared,
    file: std::path::PathBuf,
    stem: &str,
    prompt: String,
    extra: Value,
) -> Result<Value, super::ApiError> {
    let t = tools(app);
    let out_dir = app.paths.output.join("AnimaWeb");
    let fname = format!("{stem}_00001_.mp4");
    let dst = out_dir.join(&fname);
    let poster = stem.to_string();
    let poster_png = out_dir.join("_poster").join(format!("{poster}_00001_.png"));
    let (d2, p2, t2) = (dst.clone(), poster_png.clone(), t.clone());
    let probe = blocking(move || -> Result<studio::Probe, String> {
        if file != d2 {
            std::fs::create_dir_all(d2.parent().unwrap_or(&d2)).map_err(|e| e.to_string())?;
            std::fs::copy(&file, &d2).map_err(|e| format!("複製影片失敗：{e}"))?;
        }
        studio::first_frame(&t2, &d2, &p2)?;
        studio::probe(&t2, &d2)
    })
    .await
    .map_err(bad)?;
    let mut params = json!({"mode": "video", "model": "影音工作台", "prompt": prompt, "poster": poster, "seed": 0,
                            "width": probe.width, "height": probe.height, "video_seconds": (probe.duration * 10.0).round() / 10.0});
    if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            p.insert(k.clone(), v.clone());
        }
    }
    let info = json!({"mode": "video", "prompt": prompt, "negative": "", "seed": 0, "width": probe.width, "height": probe.height,
                      "idea": extra.get("idea").cloned().unwrap_or(Value::Null)});
    let iid = app.db.add_image(&fname, "AnimaWeb", "output", &info, &params, None)?;
    let image = app
        .db
        .image(iid)?
        .map(|r| {
            let mut rows = [r];
            let _ = app.db.add_gen_seconds(&mut rows);
            let [r] = rows;
            Value::Object(r)
        })
        .unwrap_or(Value::Null);
    app.hub.send(json!({"type": "image", "job": Value::Null, "image": image}));
    Ok(image)
}

/// {name}：剪輯台 exports 裡的一支影片 → 圖庫
pub async fn export_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let name = tx(&b["name"]);
    let src = app.studio.export_path(&name).ok_or_else(|| not_found("找不到這支匯出的影片"))?;
    if !name.to_lowercase().ends_with(".mp4") {
        return Err(bad("目前只收 MP4（剪輯台匯出時選 MP4）"));
    }
    let stem = format!("studio_{}", chrono::Local::now().format("%Y%m%d_%H%M%S"));
    let image = register_video(&app, src, &stem, format!("剪輯台匯出：{name}"), json!({"studio": {"export": name}})).await?;
    ok(json!({"ok": true, "image": image}))
}

// ---------------------------------------------------------------- 給 AI 的說明
fn ai_guide(app: &Shared, host: &str) -> String {
    let station = format!("http://{host}");
    let head = format!(
        "# 開工前先讀\n\n\
         如果你已經接上 MCP（工具名稱有 anima_status、anima_docs 的那個），這段不用讀：呼叫 anima_status、anima_docs 就好。\n\
         沒有 MCP 的 AI（只能打 HTTP）：底下每個 API 都可以直接用 {station} 加路徑呼叫（只接受本機，POST 送 JSON）。\n\
         說明裡寫「anima_generate」的地方等於 POST /api/generate {{params, count}}，「anima_api」等於直接打那個路徑；\n\
         花錢、刪除、改設定的請求一樣要先問使用者。\n\n"
    );
    match std::fs::read_to_string(app.studio.guide_file()) {
        Ok(md) => head + &md.replace("{{ANIMA_URL}}", &station),
        Err(e) => format!("{head}（找不到說明檔 {}：{e}。生圖站安裝可能不完整）\n", app.studio.guide_file().display()),
    }
}

pub async fn ai(State(app): State<Shared>, headers: HeaderMap) -> ApiResult {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:8190")
        .to_string();
    let station = format!("http://{host}");
    let mcp = app.studio.mcp_config(&station);
    ok(json!({
        "guide": ai_guide(&app, &host),
        "mcp": mcp,
        "mcp_json": serde_json::to_string_pretty(&mcp).unwrap_or_default(),
        "dsh_yaml": app.studio.mcp_dsh_yaml(&station),
        "codex_toml": app.studio.mcp_toml(&station),
        "mcp_name": studio::MCP_NAME,
        "station": station,
    }))
}

// ---------------------------------------------------------------- 拆幀台

/// 背景工作持有鎖，網頁斷線不會讓清理搶先刪檔。
pub(super) async fn frame_work<T: Send + 'static>(app: &Shared, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let guard = app.frames_lock.clone().try_lock_owned().map_err(|_| "拆幀台正在處理，請稍後再試".to_string())?;
    blocking(move || { let _guard = guard; work() }).await
}

pub async fn frames_storage(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let f = studio::Frames::new(&app.paths.data);
    let root = app.paths.root.clone();
    let sid = q.get("sid").cloned().unwrap_or_default();
    ok(frame_work(&app, move || f.storage(&root, &sid)).await.map_err(bad)?)
}

pub async fn frames_clear(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    if b["confirm"].as_bool() != Some(true) { return Err(bad("清除拆幀會永久刪除圖片及修幀紀錄，請先確認")); }
    let (scope, sid) = (tx(&b["scope"]), tx(&b["sid"]));
    let f = studio::Frames::new(&app.paths.data);
    let root = app.paths.root.clone();
    ok(frame_work(&app, move || f.clear(&root, &scope, &sid)).await.map_err(bad)?)
}

/// 幀的圖片網址帶 ?v=<rev>（換過才變），所以可以放心快取
const FRAME_CACHE: &str = "private, max-age=86400";

pub async fn frames_list(State(app): State<Shared>) -> ApiResult {
    let f = studio::Frames::new(&app.paths.data);
    ok(json!({"sessions": blocking(move || f.list()).await}))
}

/// start／end（秒）、max_h（原圖最高幾像素）、fps（每秒只取幾幀）：JSON 或網址參數都用這個讀
fn split_opts(get: impl Fn(&str) -> Option<f64>, cpu: bool) -> studio::SplitOpts {
    studio::SplitOpts {
        cpu,
        start: get("start").unwrap_or(0.0),
        end: get("end").filter(|e| *e > 0.0),
        max_h: get("max_h").filter(|h| *h > 0.0).map(|h| h as u32),
        fps: get("fps").filter(|f| *f > 0.0),
    }
}

fn external_source(path: &std::path::Path, name: &str) -> Value {
    json!({"id": Value::Null, "filename": name, "path": path, "external": true, "prompt": ""})
}

/// {id} 圖庫的影片，或 {path} 電腦裡任何影片檔（不用是生圖站做的；原檔不動）；可加 start、end、max_h。
/// 10 秒 24fps 約 240 張，要幾秒到十幾秒；沒有固定幀數上限，硬碟不夠才擋（studio::split_room）
pub async fn frames_split(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let opts = split_opts(|k| b[k].as_f64().or_else(|| b[k].as_str().and_then(|s| s.trim().parse().ok())), b["cpu"].as_bool().unwrap_or(false));
    let (src, source) = if let Some(id) = int(&b["id"]) {
        let r = app.db.image(id)?.ok_or_else(|| not_found("圖庫裡沒有這一支"))?;
        if !is_video(&tx(&r["filename"])) {
            return Err(bad("這不是影片"));
        }
        (file_of(&app.paths, &r), studio::source_of(&r))
    } else {
        let p = std::path::PathBuf::from(tx(&b["path"]).trim().trim_matches('"'));
        if p.as_os_str().is_empty() {
            return Err(bad("缺少 id（圖庫的影片）或 path（電腦裡的影片檔）"));
        }
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        (p.clone(), external_source(&p, &name))
    };
    if !src.is_file() {
        return Err(not_found(format!("影片檔不在硬碟上：{}", src.display())));
    }
    let (t, f) = (tools(&app), studio::Frames::new(&app.paths.data));
    let m = frame_work(&app, move || f.split(&t, &src, source, &opts)).await.map_err(bad)?;
    ok(m)
}

/// ?name=&start=&end=&max_h= ，內容是影片檔本身（網頁「從電腦選影片」用）。
/// 先串流存到 data\frames\_incoming\，拆完就刪掉這份暫存（使用者的原檔在他自己電腦上，沒動）
pub async fn frames_split_file(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    body: axum::body::Body,
) -> ApiResult {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let name: String = std::path::Path::new(q.get("name").map(String::as_str).unwrap_or(""))
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if !is_video(&name) {
        return Err(bad(format!("看不出是影片檔：「{name}」（要 mp4、webm、mov、mkv 這類）")));
    }
    let opts = split_opts(|k| q.get(k).and_then(|v| v.trim().parse().ok()), q.get("cpu").is_some_and(|v| v == "true"));
    let dir = app.paths.data.join("frames").join("_incoming");
    tokio::fs::create_dir_all(&dir).await.map_err(internal)?;
    let tmp = dir.join(format!("{}_{name}", chrono::Local::now().format("%H%M%S%3f")));
    let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| internal(format!("建立 {} 失敗：{e}", tmp.display())))?;
    let mut stream = body.into_data_stream();
    let mut n: u64 = 0;
    let mut fail = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => {
                n += c.len() as u64;
                if let Err(e) = f.write_all(&c).await {
                    fail = Some(internal(format!("寫入失敗（硬碟滿了？）：{e}")));
                    break;
                }
            }
            Err(e) => {
                fail = Some(bad(format!("上傳中斷：{e}")));
                break;
            }
        }
    }
    let _ = f.flush().await;
    drop(f);
    if fail.is_none() && n == 0 {
        fail = Some(bad("收到的檔案是空的"));
    }
    if let Some(e) = fail {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    let (t, fr, src) = (tools(&app), studio::Frames::new(&app.paths.data), tmp.clone());
    let source = json!({"id": Value::Null, "filename": name, "external": true, "uploaded": true, "prompt": ""});
    let r = frame_work(&app, move || {
        let result = fr.split(&t, &src, source, &opts);
        let _ = std::fs::remove_file(&src);
        result
    }).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    ok(r.map_err(bad)?)
}

pub async fn frames_get(State(app): State<Shared>, UrlPath(sid): UrlPath<String>) -> ApiResult {
    let f = studio::Frames::new(&app.paths.data);
    ok(f.meta(&sid).map_err(not_found)?)
}

pub async fn frame_full(State(app): State<Shared>, UrlPath((sid, n)): UrlPath<(String, u64)>) -> ApiResult {
    let p = studio::Frames::new(&app.paths.data).frame_path(&sid, n, false).map_err(not_found)?;
    file_response(p, Some(FRAME_CACHE)).await
}

pub async fn frame_thumb(State(app): State<Shared>, UrlPath((sid, n)): UrlPath<(String, u64)>) -> ApiResult {
    let p = studio::Frames::new(&app.paths.data).frame_path(&sid, n, true).map_err(not_found)?;
    file_response(p, Some(FRAME_CACHE)).await
}

fn crop_q(q: &HashMap<String, String>) -> Result<Option<[f64; 4]>, super::ApiError> {
    match q.get("crop").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => studio::parse_crop(s).map(Some).ok_or_else(|| bad("crop 要寫 x,y,w,h（都是 0～1 的比例，例如 0.25,0.3,0.5,0.4）")),
    }
}

fn image_reply(mime: &'static str, bytes: Vec<u8>) -> ApiResult {
    Ok(([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "no-cache")], bytes).into_response())
}

/// ?from=&to=&cols=&step=&h=&crop=x,y,w,h ：對照表（每格標幀號＋原片秒數；h 大於 160 或有 crop 時讀原圖）
pub async fn frames_sheet(
    State(app): State<Shared>,
    UrlPath(sid): UrlPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let num = |k: &str, d: u64| q.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let o = studio::SheetOpts {
        from: num("from", 1),
        to: num("to", 48),
        step: num("step", 1),
        cols: num("cols", 8) as u32,
        h: num("h", 160) as u32,
        crop: crop_q(&q)?,
    };
    let f = studio::Frames::new(&app.paths.data);
    image_reply("image/jpeg", frame_work(&app, move || f.sheet_ex(&sid, &o)).await.map_err(bad)?)
}

/// 每幀變化量、硬剪接、小跳動、重複幀、鏡頭段落、每幀音量（第一次要幾秒，之後用快取）
pub async fn frames_analyze(State(app): State<Shared>, UrlPath(sid): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let (t, f) = (tools(&app), studio::Frames::new(&app.paths.data));
    let mut v = frame_work(&app, move || f.analyze(&t, &sid)).await.map_err(bad)?;
    // 預設不回每幀的長陣列（太長）；要的話 ?full=1
    if q.get("full").map(String::as_str) != Some("1") {
        if let Some(o) = v.as_object_mut() {
            for k in ["score", "loud_db", "dup_frames"] {
                o.remove(k);
            }
        }
    }
    ok(v)
}

/// ?a=&b=&h=&crop= ：兩幀並排＋差異熱度圖（b 不給＝a 的下一幀）
pub async fn frames_diff(State(app): State<Shared>, UrlPath(sid): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let num = |k: &str| q.get(k).and_then(|v| v.trim().parse::<u64>().ok());
    let a = num("a").filter(|n| *n > 0).ok_or_else(|| bad("缺少 a（第幾幀）"))?;
    let (b, h, crop) = (num("b").unwrap_or(a + 1), num("h").unwrap_or(480) as u32, crop_q(&q)?);
    let f = studio::Frames::new(&app.paths.data);
    image_reply("image/jpeg", frame_work(&app, move || f.diff(&sid, a, b, h, crop)).await.map_err(bad)?)
}

/// ?from=&to=&w= ：時間軸圖（變化量長條、剪接點、重複幀、音量、原片秒數）
pub async fn frames_graph(State(app): State<Shared>, UrlPath(sid): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let num = |k: &str, d: u64| q.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let (from, to, w) = (num("from", 1), num("to", u64::MAX), num("w", 1800) as u32);
    let (t, f) = (tools(&app), studio::Frames::new(&app.paths.data));
    image_reply("image/png", frame_work(&app, move || f.graph(&t, &sid, from, to, w)).await.map_err(bad)?)
}

/// {n, op: gallery|copy|blend|restore, image_id?, from?}
pub async fn frames_edit(State(app): State<Shared>, UrlPath(sid): UrlPath<String>, Body(b): Body) -> ApiResult {
    let n = int(&b["n"]).filter(|n| *n > 0).ok_or_else(|| bad("缺少 n（第幾幀）"))? as u64;
    let f = studio::Frames::new(&app.paths.data);
    let m = match tx(&b["op"]).as_str() {
        "gallery" => {
            let id = int(&b["image_id"]).ok_or_else(|| bad("缺少 image_id"))?;
            let r = app.db.image(id)?.ok_or_else(|| not_found("圖庫裡沒有這一張"))?;
            if is_video(&tx(&r["filename"])) {
                return Err(bad("要選圖片，不是影片"));
            }
            let src = file_of(&app.paths, &r);
            frame_work(&app, move || {
                let im = image::open(&src).map_err(|e| format!("讀不到這張圖：{e}"))?;
                f.replace(&sid, n, im, &format!("圖庫 #{id}"))
            })
            .await
        }
        "copy" => {
            let from = int(&b["from"]).filter(|n| *n > 0).ok_or_else(|| bad("缺少 from（用第幾幀蓋）"))? as u64;
            frame_work(&app, move || f.copy_frame(&sid, n, from)).await
        }
        "blend" => frame_work(&app, move || f.blend(&sid, n)).await,
        "restore" => frame_work(&app, move || f.restore(&sid, n)).await,
        _ => return Err(bad("op 只能是 gallery、copy、blend、restore")),
    }
    .map_err(bad)?;
    ok(m)
}

/// ?n=  內容是圖檔（PNG／JPEG／WebP）
pub async fn frames_upload(
    State(app): State<Shared>,
    UrlPath(sid): UrlPath<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult {
    let n: u64 = q.get("n").and_then(|v| v.parse().ok()).filter(|n| *n > 0).ok_or_else(|| bad("缺少 n（第幾幀）"))?;
    let f = studio::Frames::new(&app.paths.data);
    let m = frame_work(&app, move || {
        let im = image::load_from_memory(&body).map_err(|e| format!("看不懂這個圖檔：{e}"))?;
        f.replace(&sid, n, im, "上傳的圖")
    })
    .await
    .map_err(bad)?;
    ok(m)
}

/// {n}：第 n 幀複製到 ComfyUI input，回傳 {name, width, height}（跟 /api/to_input 一樣，網頁拿去局部重繪）
pub async fn frames_to_input(State(app): State<Shared>, UrlPath(sid): UrlPath<String>, Body(b): Body) -> ApiResult {
    let n = int(&b["n"]).filter(|n| *n > 0).ok_or_else(|| bad("缺少 n（第幾幀）"))? as u64;
    let f = studio::Frames::new(&app.paths.data);
    let src = f.frame_path(&sid, n, false).map_err(not_found)?;
    if !src.is_file() {
        return Err(not_found(format!("沒有第 {n} 幀")));
    }
    let name = format!("aw_frame_{sid}_{n:06}.png");
    let dst = app.paths.input.join(&name);
    let (w, h) = frame_work(&app, move || -> Result<(u32, u32), String> {
        std::fs::copy(&src, &dst).map_err(|e| e.to_string())?;
        image::image_dimensions(&dst).map_err(|e| e.to_string())
    })
    .await
    .map_err(internal)?;
    ok(json!({"name": name, "width": w, "height": h}))
}

/// 接回影片存進圖庫
pub async fn frames_build(State(app): State<Shared>, UrlPath(sid): UrlPath<String>) -> ApiResult {
    let f = studio::Frames::new(&app.paths.data);
    let m = f.meta(&sid).map_err(not_found)?;
    let stem = format!("frames_{sid}_{}", chrono::Local::now().format("%H%M%S"));
    let tmp = app.paths.output.join("AnimaWeb").join(format!("{stem}_00001_.mp4"));
    let (t, s2, out) = (tools(&app), sid.clone(), tmp.clone());
    let guard = std::sync::Arc::new(app.frames_lock.clone().try_lock_owned().map_err(|_| bad("拆幀台正在處理，請稍後再試"))?);
    let worker_guard = guard.clone();
    blocking(move || { let _guard = worker_guard; f.build(&t, &s2, &out) }).await.map_err(bad)?;
    let edited = m["edited"].as_object().map(|o| o.len()).unwrap_or(0);
    let src_id = m["source"]["id"].clone();
    let from = match src_id.as_i64() {
        Some(i) => format!("圖庫 #{i}"),
        None => format!("外部影片「{}」", tx(&m["source"]["filename"])),
    };
    let prompt = format!("拆幀台：從{from}修改 {edited} 幀後接回\n{}", tx(&m["source"]["prompt"]));
    let image = register_video(
        &app,
        tmp,
        &stem,
        prompt,
        json!({"studio": {"frames": sid, "from": src_id, "edited": edited}, "idea": m["source"]["idea"]}),
    )
    .await?;
    if let Some(iid) = image["id"].as_i64() {
        let _ = studio::Frames::new(&app.paths.data).add_build(&sid, iid);
    }
    ok(json!({"ok": true, "image": image}))
}

pub async fn frames_delete(State(app): State<Shared>, UrlPath(sid): UrlPath<String>) -> ApiResult {
    let f = studio::Frames::new(&app.paths.data);
    let root = app.paths.root.clone();
    frame_work(&app, move || f.trash(&sid, &root)).await.map_err(bad)?;
    ok(json!({"ok": true}))
}

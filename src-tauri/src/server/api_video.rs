//! 影片模型：列出、匯入、刪除自訂的 ComfyUI 影片工作流（video_wf.rs）。選哪個用 /api/settings 的 video_engine 存
use super::{api_wf, bad, blocking, internal, ok, ApiResult, Body, Shared};
use crate::{settings, video_wf};
use axum::extract::{Multipart, Path as UrlPath, Query, State};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;

/// Keep video/audio files intact; image uploads continue to use /api/upload.
/// The server creates a unique basename, never a destination supplied by the client.
pub async fn upload_media(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    mut mp: Multipart,
) -> ApiResult {
    let kind = q.get("kind").map(String::as_str).unwrap_or("");
    if !matches!(kind, "video" | "audio") {
        return Err(bad("素材類型要選 video 或 audio"));
    }
    let field = mp
        .next_field()
        .await
        .map_err(|e| bad(format!("讀不到上傳檔案：{e}")))?
        .ok_or_else(|| bad("沒有上傳檔案"))?;
    let original = field.file_name().unwrap_or("").to_string();
    let ext = std::path::Path::new(&original)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !video_wf::media_extension(kind, &ext) {
        return Err(bad("這個檔案的副檔名與素材類型不符"));
    }
    let bytes = field
        .bytes()
        .await
        .map_err(|e| bad(format!("讀不到上傳檔案：{e}")))?;
    if bytes.is_empty() || bytes.len() > 220 * 1024 * 1024 {
        return Err(bad("素材要有內容，單檔最多 220 MB"));
    }
    if !media_header_matches(&bytes, &ext) {
        return Err(bad("素材內容與副檔名不符，請使用一般影片或聲音檔"));
    }
    let kind = kind.to_string();
    let input = app.paths.input.clone();
    let response = blocking(move || -> Result<Value, String> {
        std::fs::create_dir_all(&input).map_err(|e| format!("建立 input 失敗：{e}"))?;
        let name = format!(
            "aw_{kind}_{}_{:016x}.{ext}",
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            rand::random::<u64>()
        );
        let path = input.join(&name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("儲存素材失敗：{e}"))?;
        if let Err(error) = file.write_all(&bytes) {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(format!("儲存素材失敗：{error}"));
        }
        Ok(json!({"name": name, "kind": kind, "bytes": bytes.len()}))
    })
    .await
    .map_err(bad)?;
    ok(response)
}

fn media_header_matches(bytes: &[u8], ext: &str) -> bool {
    let riff = |tag: &[u8]| bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(tag);
    let mp4 = || bytes.get(4..8) == Some(b"ftyp");
    // 舊式 QuickTime（相機、剪輯軟體匯出的 .mov）開頭沒有 ftyp，直接是 moov／mdat／wide／free／skip
    let old_mov = || bytes.get(4..8).is_some_and(|t| [b"moov", b"mdat", b"wide", b"free", b"skip"].iter().any(|k| t == *k));
    match ext {
        "mov" => mp4() || old_mov(),
        "mp4" | "m4v" | "m4a" => mp4(),
        "webm" | "mkv" => bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "avi" => riff(b"AVI "),
        "wav" => riff(b"WAVE"),
        "flac" => bytes.starts_with(b"fLaC"),
        "ogg" | "opus" => bytes.starts_with(b"OggS"),
        "mp3" => {
            bytes.starts_with(b"ID3")
                || bytes
                    .get(..2)
                    .is_some_and(|b| b[0] == 0xff && b[1] & 0xe0 == 0xe0)
        }
        "aac" => bytes
            .get(..2)
            .is_some_and(|b| b[0] == 0xff && b[1] & 0xf6 == 0xf0),
        _ => false,
    }
}

/// {"engine": 現在用的, "builtin": {id, name}, "workflows": [{id, name, added, info | error}]}
pub async fn list(State(app): State<Shared>) -> ApiResult {
    let engine = settings::load(&app.paths.settings)
        .get("video_engine")
        .and_then(Value::as_str)
        .unwrap_or(video_wf::BUILTIN)
        .to_string();
    let dir = video_wf::dir(&app.paths.data);
    let workflows = blocking(move || video_wf::list(&dir)).await;
    ok(json!({"engine": engine, "builtin": {"id": video_wf::BUILTIN, "name": video_wf::BUILTIN_NAME}, "workflows": workflows}),
    )
}

/// 匯入 {"name", "graph"}，或 {"name", "file_name", "data_b64"}（API／一般「儲存」的 JSON、ComfyUI 生成的 PNG／WebP，跟生圖工作流一樣轉）：
/// 看得懂才存，同名的覆蓋。回 {"ok", "workflow", "warnings": [轉換時的提醒]}
pub async fn add(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err(bad("幫這個工作流取個名字"));
    }
    let (graph, warnings) = if let Some(g) = body.get("graph").cloned() {
        (g, vec![])
    } else if let Some(b64) = body.get("data_b64").and_then(Value::as_str) {
        let file_name = body.get("file_name").and_then(Value::as_str).unwrap_or("").to_string();
        let i = api_wf::file_to_api(&app, &file_name, api_wf::decode_b64(b64)?).await?;
        (i.graph, i.warnings)
    } else {
        return Err(bad("送來的資料格式不對：要有 graph，或 file_name 加 data_b64",
        ));
    };
    let dir = video_wf::dir(&app.paths.data);
    // 存不了的時候把轉換的提醒一起講（缺自訂節點被略過，常常就是找不到存影片節點的原因）
    let w2 = warnings.clone();
    let wf = blocking(move || video_wf::save(&dir, &name, &graph)).await.map_err(|e| {
        bad(if w2.is_empty() { e } else { format!("{e}\n轉換時的提醒：\n・{}", w2.iter().take(8).cloned().collect::<Vec<_>>().join("\n・")) })
    })?;
    ok(json!({"ok": true, "workflow": wf, "warnings": warnings}))
}

/// 對應設定的編輯畫面（video_wf::detail）；ComfyUI 沒連線也給（欄位型別用值猜、沒有下拉選項，info_known＝false）
pub async fn detail(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let oi = app.comfy.object_info_all().await.ok();
    let known = oi.is_some();
    let dir = video_wf::dir(&app.paths.data);
    let mut d = blocking(move || video_wf::detail(&dir, &id, oi.as_deref().unwrap_or(&Value::Null))).await.map_err(bad)?;
    d["info_known"] = json!(known);
    ok(d)
}

/// 存對應設定 {"map": {...} | null（清掉、全部自動）} → {"ok", "workflow": list() 那樣的一筆}
pub async fn set_map(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(body): Body,
) -> ApiResult {
    let map = body.get("map").cloned().unwrap_or(Value::Null);
    let dir = video_wf::dir(&app.paths.data);
    let wf = blocking(move || video_wf::save_map(&dir, &id, &map)).await.map_err(bad)?;
    ok(json!({"ok": true, "workflow": wf}))
}

/// 刪掉；正在用的被刪了就換回內建
pub async fn remove(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let _g = app.settings_lock.lock().await;
    video_wf::remove(&video_wf::dir(&app.paths.data), &id).map_err(bad)?;
    let mut s = settings::load(&app.paths.settings);
    if s.get("video_engine").and_then(Value::as_str) == Some(id.as_str()) {
        s.insert("video_engine".into(), json!(video_wf::BUILTIN));
        settings::save(&app.paths.settings, &s).map_err(internal)?;
    }
    ok(json!({"ok": true}))
}

#[cfg(test)]
mod media_tests {
    use super::media_header_matches;
    #[test]
    fn media_upload_checks_container_header() {
        assert!(media_header_matches(b"\0\0\0\x18ftypisom", "mp4"));
        assert!(media_header_matches(b"RIFF\0\0\0\0WAVE", "wav"));
        assert!(media_header_matches(b"fLaC\0\0", "flac"));
        assert!(!media_header_matches(b"RIFF\0\0\0\0WAVE", "avi"));
        assert!(!media_header_matches(b"some arbitrary text", "mp4"));
        assert!(!media_header_matches(&[], "mp3"));
        // 舊式 QuickTime 開頭沒有 ftyp；mp4 不放寬
        assert!(media_header_matches(b"\0\0\0\x08wide\0\0\0\0mdat", "mov"));
        assert!(media_header_matches(b"\0\0\0\x14ftypqt  ", "mov"));
        assert!(!media_header_matches(b"\0\0\0\x08wide\0\0\0\0mdat", "mp4"));
    }
}

//! 導演台（內建 H3／自訂圖生影片）的人工接續選幀。只讀取使用者明確指定鏡頭的成片。
use super::{bad, blocking, file_response, internal, not_found, ok, ApiResult, Body, Shared};
use crate::{director::{self as d, tx}, film_takes as takes, video_review as vr};
use axum::extract::{Path as UrlPath, State};
use serde_json::{json, Value};

fn review_dir(app: &Shared, id: &str) -> std::path::PathBuf {
    app.paths.data.join("director").join("review").join(id)
}
fn index(p: &Value, b: &Value) -> Result<usize, super::ApiError> {
    b["seg"].as_u64().map(|n| n as usize).filter(|i| p["segments"].get(*i).is_some()).ok_or_else(|| bad("沒有這一鏡"))
}
pub async fn candidates(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    if !d::valid_id(&id) { return Err(bad("專案編號不正確")); }
    let _lock = app.director.lock.lock().await;
    if app.director.running(&id) { return Err(bad("拍攝中不能選接續畫面")); }
    let p = app.director.store.project(&id).ok_or_else(|| not_found("找不到專案"))?;
    let i = index(&p, &b)?;
    let video_id = p["segments"][i]["video_id"].as_i64().ok_or_else(|| bad("這一鏡還沒有選用的影片"))?;
    let row = app.db.image(video_id)?.ok_or_else(|| not_found("找不到影片"))?;
    let src = super::api_gallery::file_of(&app.paths, &row);
    let (t, dir) = (crate::studio::tools(&app.paths.data), review_dir(&app, &id));
    let mut out = blocking(move || vr::extract(&t, &src, &dir, video_id)).await.map_err(bad)?;
    out["selected_take"] = p["segments"][i]["selected_take"].clone();
    d::write_atomic(&review_dir(&app, &id).join(format!("v{video_id}.json")), &out).map_err(internal)?;
    for c in out["candidates"].as_array_mut().into_iter().flatten() {
        c["url"] = json!(format!("/api/director/review/{id}/{}", tx(&c["file"])));
    }
    ok(out)
}
pub async fn image(State(app): State<Shared>, UrlPath((id, name)): UrlPath<(String, String)>) -> ApiResult {
    if !d::valid_id(&id) || !valid_image_name(&name) || app.director.store.project(&id).is_none() {
        return Err(not_found("找不到候選影格"));
    }
    file_response(review_dir(&app, &id).join(name), None).await
}
fn valid_image_name(name: &str) -> bool {
    let Some((v, f)) = name.strip_prefix('v').and_then(|n| n.strip_suffix(".png")).and_then(|n| n.split_once("_f")) else { return false; };
    !v.is_empty() && !f.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && f.bytes().all(|c| c.is_ascii_digit())
}
pub async fn select(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    if !d::valid_id(&id) { return Err(bad("專案編號不正確")); }
    let _lock = app.director.lock.lock().await;
    if app.director.running(&id) { return Err(bad("拍攝中不能選接續畫面")); }
    let mut p = app.director.store.project(&id).ok_or_else(|| not_found("找不到專案"))?;
    let i = index(&p, &b)?;
    let vid = b["video_id"].as_i64().filter(|n| Some(*n) == p["segments"][i]["video_id"].as_i64()).ok_or_else(|| bad("選用影片已變更，請重新挑選接續畫面"))?;
    let manifest: Value = serde_json::from_slice(&std::fs::read(review_dir(&app, &id).join(format!("v{vid}.json"))).map_err(internal)?).map_err(internal)?;
    if manifest["selected_take"] != p["segments"][i]["selected_take"] || b["expected_take"] != manifest["selected_take"] {
        return Err(bad("拍攝版本已變更，請重新挑選接續畫面"));
    }
    let frame = b["frame"].as_u64().ok_or_else(|| bad("請選一張候選影格"))?;
    let candidate = manifest["candidates"].as_array().and_then(|a| a.iter().find(|c| c["frame"].as_u64() == Some(frame))).ok_or_else(|| bad("這張影格不在本次候選清單"))?;
    let name = format!("{}{}_v{vid}_f{frame}.png", d::INPUT_PREFIX, id);
    std::fs::copy(review_dir(&app, &id).join(tx(&candidate["file"])), app.paths.input.join(&name)).map_err(internal)?;
    let seg = &mut p["segments"][i];
    takes::ensure_legacy(seg, d::DIRECTOR_RESULT_KEYS);
    let mut result = takes::result_of(seg, d::DIRECTOR_RESULT_KEYS);
    result["last_frame"] = json!(name);
    result["trim_out"] = candidate["trim_out"].clone();
    result["selected_frame"] = candidate.clone();
    let old_take = tx(&seg["selected_take"]);
    let original = seg["takes"].as_array().and_then(|a| a.iter().find(|t| t["id"] == old_take)).cloned().unwrap_or_else(|| json!({}));
    let source_take = original["source_take"].as_str().map(str::to_string);
    let mut recipe = original["recipe"].as_object().cloned().map(Value::Object).unwrap_or_else(|| json!({}));
    recipe["derived_from"] = json!(old_take);
    recipe["tail_selection"] = candidate.clone();
    let take_id = takes::record(seg, recipe, result, source_take.as_deref());
    takes::select(seg, &take_id, d::DIRECTOR_RESULT_KEYS).map_err(bad)?;
    let mut j = i + 1;
    while j < p["segments"].as_array().map_or(0, Vec::len) && p["segments"][j]["continue"] == true {
        takes::ensure_legacy(&mut p["segments"][j], d::DIRECTOR_RESULT_KEYS);
        takes::clear_current(&mut p["segments"][j], d::DIRECTOR_RESULT_KEYS);
        j += 1;
    }
    p["final"] = Value::Null;
    app.director.store.save_project(&mut p).map_err(internal)?;
    ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidate_image_paths_never_escape_review_directory() {
        assert!(valid_image_name("v12_f119.png"));
        for n in ["../v12_f119.png", "v12_f119.png/../x", "v_f1.png", "v1_f.png", "v1_f1.jpg"] { assert!(!valid_image_name(n)); }
    }
}

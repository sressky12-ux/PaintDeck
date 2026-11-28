//! 角色設定圖工坊（design\角色設定圖工坊_設計.md）：
//!   POST /api/charsheet/prompt {kind: turnaround｜face｜portrait｜pose, desc, family} → {prompt, width, height}（pose＝定稿補字，只有 prompt）
//!   POST /api/charsheet/tags   {name（input 檔名）} → {tags}：反推後只留外觀類
//!   POST /api/charsheet/spec   {images（input 檔名，最多 4 張）, name, sex, desc, desc_zh, tags, brain} → {fields, unverified}：讀圖模型填角色設定表
//! 生圖走 /api/generate，存檔走 /api/lib＋/api/assets/{id}/file。
use super::{bad, basename, blocking, not_found, ok, ApiResult, Body, Shared};
use crate::charsheet;
use crate::director::tx;
use axum::extract::State;
use serde_json::json;

pub async fn prompt(Body(b): Body) -> ApiResult {
    let (kind, family) = (tx(&b["kind"]), tx(&b["family"]));
    if kind == "pose" {
        return ok(json!({"prompt": charsheet::pose_suffix(&family)}));
    }
    let engine = tx(&b["engine"]);
    let (p, w, h) = charsheet::prompt_for(&kind, &tx(&b["desc"]), &family, if engine.is_empty() { "qwen21" } else { &engine })
        .ok_or_else(|| bad("kind 只能是 turnaround、face、portrait 或 pose"))?;
    ok(json!({"prompt": p, "width": w, "height": h}))
}

pub async fn tags(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    if !app.tagger.available() {
        return Err(bad("反推模型尚未下載"));
    }
    let name = basename(&tx(&b["name"]));
    let path = app.paths.input.join(&name);
    if name.is_empty() || !path.is_file() {
        return Err(not_found("找不到定稿圖"));
    }
    let t = app.tagger.clone();
    let v = blocking(move || t.tag(&path, 0.35, 0.85)).await.map_err(|e| bad(format!("反推失敗：{e}")))?;
    // 角色名在前（最能認出是誰），再來是一般 tag（已照機率排好）
    let all: Vec<String> = ["character", "general"]
        .iter()
        .flat_map(|k| v[*k].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p[0].as_str().map(str::to_owned))
        .collect();
    ok(json!({"tags": charsheet::filter_tags(&all)}))
}

/// 讀圖模型看定稿、正面特寫、轉身圖，填角色設定表（2026-10-27 角色卡）。只回欄位，使用者改完按鎖定、存檔時才組成固定文字
pub async fn spec(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let images: Vec<String> = b["images"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| basename(&tx(v)))
        .filter(|n| !n.is_empty() && app.paths.input.join(n).is_file())
        .take(4)
        .collect();
    if images.is_empty() {
        return Err(bad("先放定稿（或選好正面特寫、轉身圖），AI 才有圖可以看"));
    }
    let brain = if tx(&b["brain"]) == "35b" { "35b" } else { "9b" };
    let sex = match tx(&b["sex"]).as_str() {
        "m" => "male",
        "f" => "female",
        _ => "",
    };
    let user = format!(
        "Notes — name: {}; sex: {}; description: {} {}; tags: {}\n\nFill in the spec sheet from the {} image(s).",
        or_dash(&tx(&b["name"])),
        or_dash(sex),
        tx(&b["desc"]),
        tx(&b["desc_zh"]),
        or_dash(&tx(&b["tags"])),
        images.len()
    );
    let r = super::api_llm::chat_seeing(&app, "charsheet", brain, "charsheet-spec", charsheet::SYSTEM_SPEC, &user, &images, 1500, 0.2)
        .await
        .map_err(bad)?;
    let fields = charsheet::spec_fields(&r).ok_or_else(|| bad("AI 回的設定表讀不出來，再按一次試試，或換 35B"))?;
    // tag 表裡找不到的列出來提醒（照樣留著，使用者自己決定）
    let unverified: Vec<String> = match app.tags.get() {
        Some(ix) => crate::charspec::FIELDS
            .iter()
            .flat_map(|(k, _)| fields[*k]["tags"].as_array().cloned().unwrap_or_default())
            .filter_map(|t| t.as_str().filter(|t| !ix.has(t)).map(str::to_owned))
            .collect(),
        None => vec![],
    };
    ok(json!({"fields": fields, "unverified": unverified}))
}

fn or_dash(s: &str) -> &str {
    if s.trim().is_empty() {
        "-"
    } else {
        s
    }
}

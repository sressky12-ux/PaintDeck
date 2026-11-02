//! 左邊「API」頁：大模型連線、誰用哪顆腦袋、指令預設（2026-11-02，設計：design\API總機與指令預設_設計.md）。
//! 資料與規則在 crate::brains；這裡只管 HTTP。舊的 /api/agent/llm_api* 也在這裡，改成讀寫「預設」那組連線。
use super::{bad, ok, ApiResult, Body, Shared};
use crate::brains::{self, Profile, DEFAULT_ID};
use axum::extract::State;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// 抓模型清單用：走使用者的代理（net.rs），改代理設定馬上生效
fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .proxy(crate::net::proxy())
            .pool_max_idle_per_host(0)
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

/// 這組連線看不看得到圖：試過就用記著的，沒試過現在試（會花一點點 token）
pub async fn vision(app: &Shared, p: &Profile) -> Result<bool, String> {
    if let Some(v) = p.vision_known() {
        return Ok(v);
    }
    let v = app.agent.cloud.api_detect(&p.remote()).await?;
    app.brains.profiles.set_vision(&p.id, v)?;
    Ok(v)
}

fn test_msg(vision: bool) -> String {
    let tail = if vision { "這個模型看得到圖，助手會直接看圖驗收。" } else { "這個模型看不到圖，助手會改用反推模型讀 tag 來驗收。" };
    format!("連上了。{tail}")
}

fn profile(app: &Shared, id: &str) -> Result<Profile, super::ApiError> {
    app.brains.profiles.get(id).ok_or_else(|| bad("找不到這組連線"))
}

/// GET /api/llm：API 頁要的全部東西（金鑰只說有沒有填）
pub async fn overview(State(app): State<Shared>) -> ApiResult {
    let b = &app.brains;
    let features: Vec<Value> = brains::FEATURES.iter().map(|(id, name)| json!({"id": id, "name": name})).collect();
    let providers: Vec<Value> = brains::PRESETS.iter().map(|(id, name, url)| json!({"id": id, "name": name, "url": url})).collect();
    ok(json!({"profiles": b.profiles.list_view(), "default": b.profiles.default_id(), "presets": b.presets_view(),
              "routes": b.routes_view(), "features": features, "providers": providers}))
}

/// POST /api/llm/profile {id?, name, provider, url, model, key?}：key 沒帶＝不動，空字串＝清掉
pub async fn profile_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = app.brains.profiles.upsert(&b).map_err(bad)?;
    ok(p.view())
}

pub async fn profile_delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    app.brains.profiles.remove(&s(&b, "id")).map_err(bad)?;
    ok(json!({"ok": true}))
}

/// POST /api/llm/profile/test {id}：連得上、金鑰對、模型存在，順便重測看不看得到圖
pub async fn profile_test(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = profile(&app, &s(&b, "id"))?;
    if !p.ready() {
        return Err(bad("先填好網址和模型"));
    }
    let v = app.agent.cloud.api_detect(&p.remote()).await.map_err(bad)?;
    app.brains.profiles.set_vision(&p.id, v).map_err(bad)?;
    let mut out = profile(&app, &p.id)?.view();
    out["msg"] = json!(test_msg(v));
    ok(out)
}

/// chat_url 的前半段（…/v1）＋ /models
pub fn models_url(base: &str) -> String {
    let c = crate::pe::chat_url(base);
    format!("{}/models", c.trim_end_matches("/chat/completions"))
}

/// POST /api/llm/profile/models {id}：OpenAI 相容的 GET /models
pub async fn profile_models(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = profile(&app, &s(&b, "id"))?;
    if p.url.is_empty() {
        return Err(bad("先填網址"));
    }
    if let Some(run) = crate::diagnostics::current() {
        crate::diagnostics::remember_secret(&run.store, p.key.as_deref());
    }
    let mut req = http().get(models_url(&p.url));
    if let Some(k) = &p.key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.map_err(|e| bad(format!("連不到：{}", e.without_url())))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status != 200 {
        let said = [&body["error"]["message"], &body["message"]].iter().filter_map(|x| x.as_str()).next().unwrap_or("").to_string();
        return Err(bad(format!("抓不到模型清單（{status}）：{said}。可以直接自己打模型名稱")));
    }
    let mut models: Vec<String> = body["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_owned)).collect();
    models.sort();
    ok(json!({"models": models}))
}

/// POST /api/llm/preset/get {id}
pub async fn preset_get(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = app.brains.preset(&s(&b, "id")).ok_or_else(|| bad("找不到這組預設"))?;
    ok(serde_json::to_value(p).unwrap_or(Value::Null))
}

/// POST /api/llm/preset：整組存（沒 id＝新增）
pub async fn preset_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = app.brains.preset_save(&b).map_err(bad)?;
    ok(serde_json::to_value(p).unwrap_or(Value::Null))
}

pub async fn preset_delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    app.brains.preset_delete(&s(&b, "id")).map_err(bad)?;
    ok(json!({"ok": true}))
}

/// POST /api/llm/preset/import {json: SillyTavern 預設（物件或字串）, name: 檔名}：匯入並存成新的一組
pub async fn preset_import(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let raw = match &b["json"] {
        Value::String(t) => serde_json::from_str::<Value>(t).map_err(|e| bad(format!("讀不懂這個檔（不是 JSON）：{e}")))?,
        v => v.clone(),
    };
    let name = Some(s(&b, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| "匯入的預設.json".into());
    let (p, report) = brains::import_st(&raw, &name).map_err(bad)?;
    let saved = app.brains.preset_save(&serde_json::to_value(p).unwrap_or(Value::Null)).map_err(bad)?;
    ok(json!({"preset": saved, "report": report}))
}

/// POST /api/llm/routes {功能: {brain, preset}}
pub async fn routes_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    ok(app.brains.routes_save(&b).map_err(bad)?)
}

/// 一般功能（翻譯、導演台）叫腦袋：照 API 頁對照表的 feature 那列選腦袋、套預設，再交給 pe.general_chat_with。
/// page＝頁面送來的選擇（9b／35b／cloud／api／api:<id>；沒有就 "page"＝設定頁的通用模型）
#[allow(clippy::too_many_arguments)]
pub async fn chat(app: &Shared, feature: &str, page: &str, rid: &str, system: &str, user: &str, images: &[String], max_tokens: i64, temperature: f64) -> Result<String, String> {
    let (sel, preset, hint) = app.brains.resolve(feature, page)?;
    if let Some(h) = hint {
        eprintln!("[{feature}] {h}");
    }
    let cloud = match sel {
        brains::Brain::Cloud => Some(app.agent.cloud.endpoints().ok_or("雲端還沒開好：先到「雲端」開機")?.remote()),
        _ => None,
    };
    app.pe.general_chat_with(rid, system, user, images, max_tokens, temperature, &sel, cloud.as_ref(), preset.as_ref()).await
}

// ---------------------------------------------------------------- 舊的單組設定（助手 API 視窗、MCP 還在用）：讀寫「預設」那組

fn legacy_view(app: &Shared) -> Value {
    match app.brains.profiles.get(DEFAULT_ID) {
        Some(p) => json!({"provider": p.provider, "url": p.url, "model": p.model, "has_key": p.key.is_some(), "vision": p.vision_known()}),
        None => json!({"provider": "", "url": "", "model": "", "has_key": false, "vision": null}),
    }
}

/// GET /api/agent/llm_api
pub async fn llm_api(State(app): State<Shared>) -> ApiResult {
    ok(legacy_view(&app))
}

/// POST /api/agent/llm_api {provider, url, model, key?}
pub async fn llm_api_config(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let mut v = b.clone();
    v["id"] = json!(DEFAULT_ID);
    if s(&v, "name").is_empty() {
        let prov = s(&v, "provider");
        v["name"] = json!(brains::PRESETS.iter().find(|p| p.0 == prov).map(|p| p.1).unwrap_or("預設"));
    }
    app.brains.profiles.upsert(&v).map_err(bad)?;
    ok(legacy_view(&app))
}

/// POST /api/agent/llm_api/test
pub async fn llm_api_test(State(app): State<Shared>) -> ApiResult {
    let p = app.brains.profiles.get(DEFAULT_ID).filter(Profile::ready).ok_or_else(|| bad("外接 API 還沒設定：填好網址和模型"))?;
    let v = app.agent.cloud.api_detect(&p.remote()).await.map_err(bad)?;
    app.brains.profiles.set_vision(DEFAULT_ID, v).map_err(bad)?;
    let mut out = legacy_view(&app);
    out["msg"] = json!(test_msg(v));
    ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_url_follows_chat_url() {
        assert_eq!(models_url("https://api.deepseek.com/v1"), "https://api.deepseek.com/v1/models");
        assert_eq!(models_url("https://api.deepseek.com"), "https://api.deepseek.com/v1/models");
        assert_eq!(models_url("https://open.bigmodel.cn/api/paas/v4/"), "https://open.bigmodel.cn/api/paas/v4/models");
        assert_eq!(models_url("https://generativelanguage.googleapis.com/v1beta/openai"), "https://generativelanguage.googleapis.com/v1beta/openai/models");
    }
}

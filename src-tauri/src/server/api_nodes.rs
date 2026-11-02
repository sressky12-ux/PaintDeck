//! 設定頁「工作節點」：多張顯卡、區網／租的 ComfyUI（2026-11-02，設計：design\多節點生成_設計.md）。
//! 資料與挑節點的規則在 crate::nodes；這裡只管 HTTP。
//!   GET  /api/nodes        {nodes:[…設定＋online/queue/running], gpus:[{index,name,vram_mb}], ram_gb, bind, ram_guard, main_port}
//!   POST /api/nodes        {nodes?, bind?, ram_guard?} → 存檔、遠端節點馬上生效；本機節點改了回 restart_needed（要重開 ComfyUI）
//!   POST /api/nodes/test   {url, token?, id?} → 那台的顯卡、顯存、ComfyUI 版本（id＝用存著的 token）
use super::{bad, ok, ApiResult, Body, Shared};
use crate::nodes::{self, Cfg};
use axum::extract::State;
use serde_json::{json, Value};
use std::time::Duration;

fn main_port(app: &Shared) -> u16 {
    nodes::port_of(app.comfy.url()).unwrap_or(8188)
}

pub async fn overview(State(app): State<Shared>) -> ApiResult {
    let cfg = app.pool.cfg();
    let live: Vec<Value> = match app.pool.status() {
        Value::Array(a) => a,
        _ => vec![],
    };
    let rows: Vec<Value> = std::iter::once(json!({"id": nodes::MAIN, "name": "主節點", "kind": "main", "url": app.comfy.url(), "enabled": true}))
        .chain(cfg.nodes.iter().map(|n| n.view()))
        .map(|mut v| {
            if let Some(l) = live.iter().find(|l| l["id"] == v["id"]) {
                for k in ["online", "queue", "running"] {
                    v[k] = l[k].clone();
                }
            } else {
                v["online"] = json!(false);
            }
            v
        })
        .collect();
    let (gpus, _) = crate::gpu::detect(false);
    ok(json!({"nodes": rows, "gpus": nodes::cuda_list(&gpus), "ram_gb": (nodes::ram_gb() * 10.0).round() / 10.0,
              "ram_guard_gb": nodes::RAM_GUARD_GB, "bind": cfg.bind, "ram_guard": cfg.ram_guard, "main_port": main_port(&app),
              "can_restart": app.restart_comfy.get().is_some()}))
}

pub async fn save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let (cfg, restart) = Cfg::update(&app.pool.cfg(), &b, main_port(&app)).map_err(bad)?;
    app.pool.reload(cfg).map_err(bad)?;
    ok(json!({"ok": true, "restart_needed": restart}))
}

/// 連一台 ComfyUI 看看：/system_stats 的顯卡、顯存、版本
pub async fn test(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let url = b["url"].as_str().unwrap_or("").trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(bad("網址要 http:// 或 https:// 開頭"));
    }
    let token = b["token"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned).or_else(|| {
        let id = b["id"].as_str().unwrap_or("");
        app.pool.cfg().get(id).and_then(|n| n.token.clone())
    });
    let http = reqwest::Client::builder()
        .proxy(crate::net::proxy())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| bad(e.to_string()))?;
    let mut req = http.get(format!("{url}/system_stats"));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let r = req.send().await.map_err(|e| bad(format!("連不上 {url}：{e}")))?;
    let st = r.status();
    if st.as_u16() == 401 || st.as_u16() == 403 {
        return Err(bad(format!("{url} 要密碼（token 不對或沒填）：HTTP {st}")));
    }
    let v: Value = r.json().await.map_err(|_| bad(format!("{url} 回的不是 ComfyUI（HTTP {st}）")))?;
    let dev = &v["devices"][0];
    if v["system"].is_null() {
        return Err(bad(format!("{url} 回的不是 ComfyUI")));
    }
    let gb = |x: &Value| x.as_f64().map(|b| (b / 1024f64.powi(3) * 10.0).round() / 10.0);
    ok(json!({"ok": true, "comfy": v["system"]["comfyui_version"], "gpu": dev["name"], "vram_gb": gb(&dev["vram_total"]),
              "ram_gb": gb(&v["system"]["ram_total"])}))
}

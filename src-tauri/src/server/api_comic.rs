//! 漫畫台 API（/api/comic/*）與背景工作（MOSS-Image 一次畫整頁）。提示詞與存檔格式在 crate::comic。
//!
//! 背景工作一個作品同時只跑一個；進度用 hub 送 {"type":"comic", id, phase, msg, image?}：
//! phase＝msg（進度文字）／page（畫好一頁）／end／error。網頁收到 page／end 就重新讀作品。
//! 2026-10-20 起只用 MOSS-Image（Anima＋Qwen 填字、Qwen 一次畫完都拿掉了）。換家族前先叫 ComfyUI /free：
//! 同一個 ComfyUI 同時留著兩個家族的模型，32GB 會被吃光卡死（2026-10-16 Anima＋Qwen 實測剩 3.4GB）。
use super::api_director::{job_image, random_seed, wait_job};
use super::api_gen::submit_jobs;
use super::{bad, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::comic::{self as c, Store};
use crate::director::tx;
use crate::families::{self, MOSS};
use crate::py::now;
use axum::extract::{Path as UrlPath, State};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct Comic {
    pub store: Store,
    /// 作品檔「讀 → 改 → 寫」排隊
    lock: tokio::sync::Mutex<()>,
    /// 正在跑背景工作的作品 → 停止用
    runs: Mutex<HashMap<String, CancellationToken>>,
}

impl Comic {
    pub fn any_running(&self) -> bool {
        !self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
    pub fn new(data: &Path) -> Comic {
        Comic { store: Store::new(data), lock: tokio::sync::Mutex::new(()), runs: Mutex::new(HashMap::new()) }
    }
    fn running(&self, id: &str) -> bool {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
    }
}

fn emit(app: &Shared, id: &str, phase: &str, msg: impl Into<String>, image: Option<i64>) {
    app.hub.send(json!({"type": "comic", "id": id, "phase": phase, "msg": msg.into(), "image": image}));
}

fn load(app: &Shared, id: &str) -> Result<Value, ApiError> {
    app.comic.store.project(id).ok_or_else(|| not_found("找不到這個作品"))
}

async fn update(app: &Shared, id: &str, f: impl FnOnce(&mut Value)) -> Result<Value, ApiError> {
    let _g = app.comic.lock.lock().await;
    let mut p = load(app, id)?;
    f(&mut p);
    app.comic.store.save(&mut p).map_err(internal)?;
    Ok(p)
}

// ================================================================ 選項、作品
/// 版型、景別、對話框位置（網頁的下拉選單都從這裡來，跟組提示詞用同一份）
pub async fn meta() -> ApiResult {
    let layouts: Vec<Value> = c::LAYOUTS
        .iter()
        .map(|(id, zh, _, slots)| json!({"id": id, "name": zh, "slots": slots.iter().map(|s| s.1).collect::<Vec<_>>()}))
        .collect();
    let pairs = |v: &[(&str, &str)]| v.iter().map(|(en, zh)| json!([en, zh])).collect::<Vec<_>>();
    ok(json!({"layouts": layouts, "custom": {"id": "custom", "max_rows": 6, "max_cells": 4, "max_panels": c::MAX_PANELS, "max_insets": c::MAX_INSETS},
              "shots": pairs(c::SHOTS), "bubbles": pairs(c::BUBBLES)}))
}

pub async fn projects(State(app): State<Shared>) -> ApiResult {
    let running: Vec<String> = app.comic.runs.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    ok(json!({"projects": app.comic.store.projects(), "running": running}))
}

pub async fn project_get(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let mut p = load(&app, &id)?;
    p["running"] = json!(app.comic.running(&id));
    ok(p)
}

/// {title, copy_from?}：copy_from＝複製那個作品的設定（不複製結果）
pub async fn project_new(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let mut p = c::blank(&tx(&b["title"]));
    if let Some(src) = b.get("copy_from").map(tx).filter(|v| !v.is_empty()).and_then(|v| app.comic.store.project(&v)) {
        for k in EDITABLE {
            if k != "title" && !src[k].is_null() {
                p[k] = src[k].clone();
            }
        }
    }
    app.comic.store.save(&mut p).map_err(internal)?;
    ok(p)
}

/// 網頁可以改的欄位（runs 是背景工作寫的，網頁存檔不能蓋掉）
const EDITABLE: [&str; 11] = ["title", "layout", "trigger", "cast", "light", "mood", "panels", "custom", "script", "image", "count"];

pub async fn project_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let p = update(&app, &id, |p| {
        for k in EDITABLE {
            if let Some(v) = b.get(k) {
                p[k] = v.clone();
            }
        }
    })
    .await?;
    ok(json!({"ok": true, "updated": p["updated"]}))
}

pub async fn project_delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    if app.comic.running(&id) {
        return Err(bad("這個作品正在畫，先按「停止」"));
    }
    let _g = app.comic.lock.lock().await;
    app.comic.store.trash(&id, &app.paths.root.join("待刪除")).map_err(internal)?;
    ok(json!({"ok": true}))
}

/// 結果清單裡拿掉一筆（圖還在圖庫）：{id, page}
pub async fn run_remove(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let page = b["page"].as_i64().ok_or_else(|| bad("要給 page"))?;
    update(&app, &id, |p| {
        if let Some(a) = p["runs"].as_array_mut() {
            a.retain(|r| r["page"].as_i64() != Some(page));
        }
    })
    .await?;
    ok(json!({"ok": true}))
}

// ================================================================ 組參數
/// 自動挑 MOSS 模型：優先官方 SFT 版，沒有就挑第一個 MOSS（生成頁帶來的 MOSS 設定另外優先，見 page_params）
async fn pick_moss(app: &Shared) -> Option<String> {
    let all = app.comfy.options("UNETLoader", "unet_name").await.ok()?;
    let names: Vec<String> = all
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .filter(|n| families::family_of(&app.paths.unet_roots(), n) == MOSS)
        .collect();
    names.iter().find(|n| n.to_lowercase().contains("moss_image_sft")).or_else(|| names.first()).cloned()
}

/// 自動挑的模型用的步數：25 步 CFG 3（生圖站的 MOSS 預設）
fn auto_params(model: &str) -> Map<String, Value> {
    let v = json!({"model": model, "turbo_lora": "", "negative": "worst quality, bad anatomy, watermark, jpeg artifact", "loras": [],
                   "steps": 25, "cfg": 3.0, "sampler": "euler", "scheduler": "sgm_uniform"});
    v.as_object().cloned().unwrap_or_default()
}

/// 畫整頁的參數：生成頁帶來的設定是 MOSS 才用，不然自動挑
async fn page_params(app: &Shared, p: &Value) -> Result<Map<String, Value>, String> {
    let base = p["image"].as_object().filter(|b| families::family_of(&app.paths.unet_roots(), &tx(&b["model"])) == MOSS);
    let mut q = match base {
        Some(b) => {
            let mut q = Map::new();
            for k in ["model", "turbo_lora", "negative", "steps", "cfg", "sampler", "scheduler", "loras"] {
                if let Some(v) = b.get(k) {
                    q.insert(k.into(), v.clone());
                }
            }
            q
        }
        None => auto_params(&pick_moss(app).await.ok_or("找不到 MOSS-Image 模型（漫畫台只用 MOSS-Image）")?),
    };
    let (w, h) = c::MOSS_SIZE;
    q.insert("mode".into(), json!("txt2img"));
    q.insert("seed".into(), json!(random_seed()));
    q.insert("prompt".into(), json!(c::moss_page(p)?));
    q.insert("width".into(), json!(w));
    q.insert("height".into(), json!(h));
    q.insert("idea".into(), json!(format!("漫畫：{}", tx(&p["title"]))));
    q.insert("comic".into(), json!({"id": p["id"], "kind": "page"}));
    Ok(q)
}

/// 換模型家族前先叫 ComfyUI 卸載（回應是空的，不是 JSON，錯誤不管）。
/// 看的是全站最後送出的家族（生成頁、導演台用過別的家族也算）；開站後還沒送過＝不知道 ComfyUI 留著什麼，也卸載
async fn ensure_family(app: &Shared, fam: &str) {
    let prev = app.last_family.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if prev != fam {
        let _ = app.comfy.post("/free", &json!({"unload_models": true, "free_memory": true})).await;
    }
}

// ================================================================ 背景工作
type Step = Result<(), String>;

fn begin(app: &Shared, id: &str) -> Result<CancellationToken, ApiError> {
    let mut runs = app.comic.runs.lock().unwrap_or_else(|e| e.into_inner());
    if runs.contains_key(id) {
        return Err(bad("這個作品已經在畫了"));
    }
    let t = CancellationToken::new();
    runs.insert(id.to_string(), t.clone());
    Ok(t)
}

fn spawn(app: &Shared, id: &str, token: CancellationToken, work: impl std::future::Future<Output = Step> + Send + 'static) {
    let (app, id) = (app.clone(), id.to_string());
    crate::diagnostics::spawn(async move {
        let r = work.await;
        app.comic.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        match r {
            Ok(()) => emit(&app, &id, "end", "完成", None),
            Err(_) if token.is_cancelled() => emit(&app, &id, "end", "已停止", None),
            Err(e) => {
                eprintln!("漫畫台 {id}：{e}");
                emit(&app, &id, "error", e, None)
            }
        }
    });
}

pub async fn stop(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let t = app.comic.runs.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
    if let Some(t) = &t {
        t.cancel();
    }
    ok(json!({"ok": t.is_some()}))
}

/// 一次送出的其他張（還在佇列或正在跑）：停止或失敗時一起拿掉，不然會留在 ComfyUI 繼續跑，結果也不會記到作品
async fn cancel_jobs(app: &Shared, pids: &[String]) {
    // 工作節點：每件找它所在的那台，正在跑的中斷、還在排的拿掉
    app.pool.cancel(pids).await;
}

/// 結果清單：最新的放最前面，最多留 60 筆
fn push_run(p: &mut Value, run: Value) {
    let mut list = p["runs"].as_array().cloned().unwrap_or_default();
    list.insert(0, run);
    list.truncate(60);
    p["runs"] = json!(list);
}

/// 畫這一頁：{id, count?}。MOSS-Image 一次畫完整頁（格子、台詞、狀聲詞都在同一張圖裡）
pub async fn run(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let p = load(&app, &id)?;
    if c::filled_panels(&p) == 0 {
        return Err(bad("每一格的「內容」至少要寫一格"));
    }
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線"));
    }
    let count = b.get("count").and_then(crate::py::int).or_else(|| p["count"].as_i64()).unwrap_or(4).clamp(1, 8);
    let q = page_params(&app, &p).await.map_err(bad)?;
    let token = begin(&app, &id)?;
    let (a, id2, t2) = (app.clone(), id.clone(), token.clone());
    spawn(&app, &id, token, async move {
        let (app, id, token) = (a, id2, t2);
        // 畫圖前先關掉 AI 改寫的 llama-server（35B 會占 7～8GB 顯存），跟生成頁走同一套；
        // 排分鏡反過來：PE 載入前會叫 ComfyUI /free（pe.rs 的 ensure），兩邊互相讓路
        app.pe.release_for_generation().await;
        ensure_family(&app, MOSS).await;
        emit(&app, &id, "msg", format!("畫 {count} 張整頁…"), None);
        let jobs = submit_jobs(&app, q, count).await.map_err(|e| e.1)?;
        for (k, j) in jobs.iter().enumerate() {
            let pid = tx(&j["job"]);
            if let Err(e) = wait_job(&app, &pid, &token).await {
                let rest: Vec<String> = jobs[k + 1..].iter().map(|j| tx(&j["job"])).collect();
                cancel_jobs(&app, &rest).await;
                return Err(e);
            }
            let img = job_image(&app, &pid)?["id"].as_i64().ok_or("結果沒有編號")?;
            update(&app, &id, |p| push_run(p, json!({"at": now(), "page": img, "seed": j["seed"]})))
                .await
                .map_err(|e| e.1)?;
            emit(&app, &id, "page", format!("第 {} / {count} 張畫好了", k + 1), Some(img));
        }
        Ok(())
    });
    ok(json!({"ok": true}))
}

/// 預覽組好的提示詞：{project}（網頁還沒存的內容也可以）→ {page, family}
pub async fn prompts(Body(b): Body) -> ApiResult {
    ok(json!({"page": c::moss_page(&b["project"]).map_err(bad)?, "family": MOSS}))
}

/// 這一頁的版面長什麼樣：{project} → {desc, slots:[{en, zh}]}。網頁的自訂版面編輯器用它算格子名字，
/// 跟組提示詞用同一份程式，兩邊不會各算一套。
pub async fn layout_preview(Body(b): Body) -> ApiResult {
    let (desc, slots) = c::layout_of(&b["project"]).map_err(bad)?;
    ok(json!({
        "desc": desc,
        "slots": slots.iter().map(|(en, zh)| json!({"en": en, "zh": zh})).collect::<Vec<_>>(),
    }))
}

/// 用說的排版（站內助手幫忙拉格子）：{project, ask, brain} → {custom}（排版工作台的 v2 格式，已經整理過）。
/// ask 空白就照這頁的劇情自己排。叫本機語言模型（預設 35B，可以指定 9B）；網頁套上後可以 Ctrl＋Z。
pub async fn layout_ai(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let p = &b["project"];
    let ask = tx(&b["ask"]);
    if ask.trim().is_empty() && tx(&p["script"]).trim().is_empty() {
        return Err(bad("寫一句想怎麼排（例：左邊一格大特寫貫穿整頁，右邊切三格），或先寫這一頁的劇情"));
    }
    // 預設 35B：2026-10-27 實測 9B 常漏排、左右顛倒，35B 全對（載入後一次 3～5 秒）；機器上沒有 35B 會自動用 9B
    let brain = if tx(&b["brain"]) == "9b" { "9b" } else { "35b" };
    let (sys, user) = c::layout_messages(p, &ask);
    let rid = format!("comic-layout-{}", tx(&p["id"]));
    for attempt in 0..2 {
        let r = app
            .pe
            .general_chat_as(&rid, &sys, &user, &[], 1500, if attempt == 0 { 0.4 } else { 0.7 }, Some(brain))
            .await
            .map_err(bad)?;
        if let Some(v) = c::parse_layout(&r) {
            return ok(json!({"custom": v}));
        }
        eprintln!("[comic] AI 排版讀不出版面（第 {} 次）：{}", attempt + 1, r.chars().take(300).collect::<String>());
    }
    Err(bad("AI 回的版面讀不出來，再按一次試試，或換 35B"))
}

// ================================================================ AI 排分鏡（35B）
/// {id, story, mode}：叫通用版的 35B 把這段劇情拆成每一格，填回作品。
/// 背景跑，進度用 type=comic phase=script；結束一樣走 end／error（網頁重讀作品就拿到新的格）。
/// mode＝"fill"（只填空白的格，預設）／"all"（整頁重寫，原本寫好的會被蓋掉）
pub async fn script(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let story = tx(&b["story"]);
    if story.trim().is_empty() {
        return Err(bad("先寫這段劇情（中文就好），AI 才有東西可以拆"));
    }
    let all = tx(&b["mode"]) == "all";
    // 排分鏡用哪顆：預設 35B（台詞好）；記憶體不夠可以指定 9b
    let brain = if tx(&b["brain"]) == "9b" { "9b" } else { "35b" };
    load(&app, &id)?;
    let token = begin(&app, &id)?;
    let (a, id2, story2) = (app.clone(), id.clone(), story.clone());
    spawn(&app, &id, token, async move { write_script(&a, &id2, &story2, all, brain).await });
    ok(json!({"ok": true}))
}

/// 排分鏡本體：叫 LLM → 解析 → 填回作品。JSON 讀不出來重試一次。
async fn write_script(app: &Shared, id: &str, story: &str, all: bool, brain: &str) -> Step {
    let p = load(app, id).map_err(|e| e.1)?;
    let n = c::layout_of(&p)?.1.len();
    if n == 0 {
        return Err("版型不對，先重新選一個版型".into());
    }
    emit(app, id, "script", format!("AI 排分鏡中…（{brain} 第一次要載入模型，可能要一兩分鐘）"), None);
    let (sys, user) = c::script_messages(&p, story);
    let rid = format!("comic-{id}");
    let mut last = String::new();
    for attempt in 0..2 {
        let r = app
            .pe
            .general_chat_as(&rid, &sys, &user, &[], 2000, if attempt == 0 { 0.7 } else { 0.9 }, Some(brain))
            .await?;
        let Some(panels) = c::parse_script(&r, n) else {
            eprintln!(
                "漫畫台 {id}：分鏡 JSON 讀不出來（第 {} 次）：{}",
                attempt + 1,
                r.chars().take(300).collect::<String>()
            );
            last = r;
            continue;
        };
        let filled = panels.iter().filter(|c| !tx(&c["what"]).trim().is_empty()).count();
        update(app, id, |p| {
            let olds = p["panels"].as_array().cloned().unwrap_or_default();
            let mut out: Vec<Value> = (0..n)
                .map(|i| {
                    let new = panels[i].clone();
                    let old = olds.get(i).cloned().unwrap_or_else(|| json!({}));
                    // 只填空白的格：原本寫好內容的格不動；台詞／對話框位置有寫過就留著
                    if !all && !tx(&old["what"]).trim().is_empty() {
                        return old;
                    }
                    if all {
                        return new;
                    }
                    let mut v = new;
                    for k in ["line", "sfx", "bubble"] {
                        let ov = tx(&old[k]).trim().to_string();
                        if !ov.is_empty() {
                            v[k] = json!(ov);
                        }
                    }
                    v
                })
                .collect();
            // 版型外還留著的格（換到格數多的版型時會用到）不要動
            if olds.len() > n {
                out.extend(olds[n..].iter().cloned());
            }
            p["panels"] = json!(out);
        })
        .await
        .map_err(|e| e.1)?;
        emit(app, id, "script", format!("AI 排好 {filled} 格"), None);
        return Ok(());
    }
    Err(format!(
        "模型輸出的格式壞掉了，可以再按一次。輸出開頭：{}",
        last.chars().take(120).collect::<String>()
    ))
}

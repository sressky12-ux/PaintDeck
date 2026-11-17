//! 漫畫台 API（/api/comic/*）與背景工作（MOSS-Image 一次畫整頁）。提示詞與存檔格式在 crate::comic。
//!
//! 背景工作一個作品同時只跑一個；進度用 hub 送 {"type":"comic", id, phase, msg, image?}：
//! phase＝msg（進度文字）／page（畫好一頁）／end／error。網頁收到 page／end 就重新讀作品。
//! 2026-10-20 起只用 MOSS-Image（Anima＋Qwen 填字、Qwen 一次畫完都拿掉了）。換家族前先叫 ComfyUI /free：
//! 同一個 ComfyUI 同時留著兩個家族的模型，32GB 會被吃光卡死（2026-10-16 Anima＋Qwen 實測剩 3.4GB）。
use super::api_director::{job_image, random_seed, wait_job};
use super::api_gen::submit_jobs;
use super::{bad, blocking, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::comic::{self as c, Store};
use crate::comic_panels as c_p;
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
/// chars＝角色卡（資產抽屜加的，見 comic::chars）、loras＝漫畫台自己掛的 LoRA（2026-11-16）
const EDITABLE: [&str; 13] = ["title", "layout", "trigger", "cast", "light", "mood", "panels", "custom", "script", "image", "count", "chars", "loras"];

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
    let loras = merge_loras(q.get("loras"), &p["loras"]);
    q.insert("loras".into(), loras);
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

/// 漫畫台自己的 LoRA 清單（有勾的）接在生成頁帶來的後面；同一個 LoRA 以漫畫台的強度為準。
/// 不是 MOSS 用的 LoRA 由送出時的家族檢查略過（跟生成頁同一套，回應的 skipped_loras）
fn merge_loras(base: Option<&Value>, own: &Value) -> Value {
    let mut out: Vec<Value> = base.and_then(Value::as_array).cloned().unwrap_or_default();
    for l in own.as_array().into_iter().flatten() {
        let name = tx(&l["name"]);
        if !super::api_lib::safe_lora_name(&name) || l["enabled"] == false {
            continue;
        }
        out.retain(|o| tx(&o["name"]) != name);
        out.push(json!({"name": name, "strength": l["strength"].as_f64().unwrap_or(0.8).clamp(-3.0, 3.0)}));
    }
    json!(out)
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
                    // 對話框位置跟著台詞走：使用者自己寫了台詞才留他選的位置。
                    // 空白格的 bubble 預設就是 "top"（不是空字串），以前照「有寫過就留著」會把 AI 排的位置全部蓋回 top
                    let own_line = !tx(&old["line"]).trim().is_empty();
                    for k in ["line", "sfx", "bubble"] {
                        let ov = tx(&old[k]).trim().to_string();
                        if !ov.is_empty() && (k != "bubble" || own_line) {
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

// ================================================================ 照資產修角色（2026-11-16）
// 做法（2026-11-16 實測後定案，對照圖在 F:\AI\小克工作區\測試區\comicfix\exp\）：
//   ① 換頭：Qwen 2.1 多圖參考，<image1>＝這一格（畫布），後面是角色卡的參考圖，指令用官方「換頭」寫法
//      （先講操作、長相指著參考圖不用文字描述、要留的只點名不重畫）。
//   ② 補表情：Qwen 換頭會把參考圖的表情一起搬過來（笑臉變面無表情；換指令、掛 LoRA、調重繪強度都救不回來），
//      所以先用反推（WD tagger，CPU）從原本那一格抓表情 tag，翻成英文，再叫 Qwen 只改表情。
/// 修角色一格最多帶幾張參考圖：每個角色 2 張（正面特寫＋多視圖），整件最多 6 張＋要修的那張
const FIX_REFS_PER_CHAR: usize = 2;
const FIX_MAX_REFS: usize = 6;
/// 換頭和補表情都掛的「對齊原圖」LoRA（ComfyUI 有這個檔才掛）：結果跟原格位置對得上、不亂改沒要求的地方
const FIX_CONSISTENCY_LORA: &str = "qwen21\\qwen-image-2.1-consistency.safetensors";
/// 社群換頭 LoRA（BFS Best Face Swap，MIT，HF Alissonerdx/BFS-Best-Face-Swap）：表情、視線、頭的角度照原格，長相照參考圖
const BFS_LORA: &str = "qwen21\\bfs_head_v1.1_qwen_2.1.safetensors";
/// BFS 作者的原版指令（2026-11-16 實測：改寫成「漫畫版」會讓表情變平，所以一字不改）
const BFS_PROMPT: &str = "head_swap: start with <image1> as the base image, keeping its lighting, environment, and background. remove the head from <image1> completely and replace it with the head from <image2>, strictly preserving the hair, eye color, nose structure from <image2>. copy the direction of the eye, head rotation, micro expressions from <image1>, high quality, sharp details, 4k";

/// 修角色放進 ComfyUI input 的檔名：每一輪（seed0）都不同。ComfyUI 讀的 input 跟繪匣的不同時，
/// 繪匣只有在 ComfyUI 說「圖無效」才上傳，同名的舊檔會被直接拿去用（2026-11-17：整頁被貼進第 1 格）
fn fix_input_name(id: &str, page: i64, seed0: i64, k: usize, tag: &str) -> String {
    if tag.is_empty() {
        format!("{}{k}.png", fix_input_prefix(id, page, seed0))
    } else {
        format!("{}{k}_{tag}.png", fix_input_prefix(id, page, seed0))
    }
}

/// 這一輪所有 input 檔共同的開頭
fn fix_input_prefix(id: &str, page: i64, seed0: i64) -> String {
    format!("comicfix_{id}_{page}_{seed0}_")
}

/// 這一輪放進 input 的格子圖：離開時（成功、失敗、按停）都清掉；每輪檔名都不同，不清會越堆越多
struct FixInputs {
    dir: std::path::PathBuf,
    prefix: String,
}

impl Drop for FixInputs {
    fn drop(&mut self) {
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&self.prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// 一格走哪條路：只換頭（BFS＋裁頭節點）或兩段式（換頭＋補表情）
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FixRoute {
    Head,
    TwoPass,
}

/// 逐格修、ComfyUI 有 BFS LoRA、有漫畫小節點，就走只換頭（一格幾個角色都行：比對參考圖各找各的頭）；其他照舊兩段式
fn fix_route(who: usize, panel: bool, have_bfs: bool, have_node: bool) -> FixRoute {
    if who >= 1 && panel && have_bfs && have_node {
        FixRoute::Head
    } else {
        FixRoute::TwoPass
    }
}

/// {id, page}：畫好的那一頁（圖庫編號）照角色卡的參考圖修角色。找得到每一格就一格一格修，
/// 格數對不上版型（破格、斜切小格黏在一起）就整頁一次修（只換頭、不補表情）。修好的整頁另存一張，記在那一筆結果的 fix。
pub async fn fix(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let page = b["page"].as_i64().ok_or_else(|| bad("要給 page（哪一張整頁）"))?;
    let p = load(&app, &id)?;
    let Some(run) = p["runs"].as_array().into_iter().flatten().find(|r| r["page"].as_i64() == Some(page)) else {
        return Err(bad("這一張不在這個作品的結果裡"));
    };
    if tx(&run["flow"]) == "a" {
        return Err(bad("這是舊版（Anima 畫完再填字）的紀錄，不能修角色：用現在的漫畫台重畫一頁"));
    }
    if !c::chars(&p).iter().any(|ch| ch["refs"].as_array().is_some_and(|a| !a.is_empty())) {
        return Err(bad("角色卡沒有參考圖：在「這頁的角色」用資產抽屜加角色（資產要有正面特寫或多視圖）"));
    }
    let row = app.db.image(page)?.ok_or_else(|| not_found("圖庫裡找不到這一張"))?;
    let src = local_file(&app, &row).await.map_err(|_| not_found("這一張的檔案不在硬碟上了，ComfyUI 那邊也要不回來"))?;
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線"));
    }
    let token = begin(&app, &id)?;
    let (a, id2, t2) = (app.clone(), id.clone(), token.clone());
    spawn(&app, &id, token, async move { fix_page(&a, &id2, page, src, &t2).await });
    ok(json!({"ok": true}))
}

/// 一件修圖工作：要修的那一格（None＝整頁）、裡面有誰、這一格的內容（反推用不了時拿來補表情）
struct FixItem {
    panel: Option<c_p::Panel>,
    who: Vec<String>,
    label: String,
    scene: String,
}

/// 圖庫那一筆在硬碟上的檔；不在生圖站認定的位置（ComfyUI 的 output 跟設定的資料夾不同、工作節點在別台）就先跟 ComfyUI 要回來
/// 檔案在但比這一筆圖庫紀錄還舊（2 分鐘以上）＝別的 ComfyUI 輸出同名的舊檔，也要重新要（2026-11-16 測試站實測踩到：拿到以前測試留下的同名圖）
async fn local_file(app: &Shared, row: &Map<String, Value>) -> Result<std::path::PathBuf, String> {
    let p = super::api_gallery::file_of(&app.paths, row);
    let created = row.get("created").and_then(Value::as_f64).unwrap_or(0.0);
    let stale = |p: &std::path::Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|t| t.as_secs_f64() < created - 120.0)
    };
    if !p.is_file() || stale(&p) {
        super::api_gallery::fetch_from_comfy(app, row).await.map_err(|e| format!("拿不到 ComfyUI 的結果：{}", e.1))?;
    }
    Ok(super::api_gallery::file_of(&app.paths, row))
}

fn card_name(ch: &Value) -> String {
    tx(&ch["name"]).trim().to_string()
}

/// 這一格有誰：勾的；沒勾就看內容有沒有寫到角色的名字（英數字要整個字比對，Lin 不會對到 Linda；中文名字前後是漢字也算）
pub(super) fn who_of(cell: &Value, names: &[String]) -> Vec<String> {
    let who = c::panel_who(cell, names);
    if !who.is_empty() {
        return who;
    }
    let what = tx(&cell["what"]).to_lowercase();
    let word = |n: &str| {
        let n = n.to_lowercase();
        what.match_indices(&n).any(|(i, _)| {
            let before = what[..i].chars().next_back();
            let after = what[i + n.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        })
    };
    names.iter().filter(|n| word(n)).cloned().collect()
}

/// ① 換頭的指令（Qwen 2.1 多圖參考：<image1> 是畫布，後面是角色參考圖）
pub(super) fn swap_prompt(whole: bool, cast: &[(String, Vec<usize>)]) -> String {
    let refs: Vec<String> = cast
        .iter()
        .map(|(name, ns)| format!("{name} from {}", ns.iter().map(|n| format!("<image{n}>")).collect::<Vec<_>>().join(" and ")))
        .collect();
    let keep = "the art style, line art, coloring and lighting of <image1> stay unchanged.";
    if whole {
        return format!(
            "In every panel of <image1>, replace the head of each character with the head of the matching reference character: {}. Take each face, eyes, hairstyle and hair color from its reference. <image1> is the canvas: every facial expression, head angle, pose, hand, piece of clothing and background, the panel borders, the speech bubbles and every letter of their text, and {keep}",
            refs.join("; ")
        );
    }
    let head = if cast.len() == 1 {
        format!("Replace the head of the character in <image1> with the head of {}, taking the face, eyes, hairstyle and hair color from the reference.", refs[0])
    } else {
        format!("Replace the head of each character in <image1> with the head of the matching reference character: {}. Take each face, eyes, hairstyle and hair color from its reference.", refs.join("; "))
    };
    format!("{head} <image1> is the canvas: the facial expressions, head angles, poses, hands, clothing, the background, the speech bubble and its text, and {keep}")
}

/// 反推抓得到的表情 tag → 英文片語（照這個順序寫進指令）
const EXPRESSIONS: &[(&str, &str)] = &[
    (":d", "laughing with the mouth wide open"),
    (";d", "laughing with the mouth wide open"),
    ("laughing", "laughing"),
    ("grin", "grinning"),
    ("smirk", "smirking"),
    ("nervous smile", "smiling nervously"),
    ("smile", "smiling"),
    ("open mouth", "with the mouth open"),
    ("closed mouth", "with the mouth closed"),
    ("parted lips", "with slightly parted lips"),
    ("clenched teeth", "gritting the teeth"),
    ("teeth", "showing the teeth"),
    ("tongue out", "sticking the tongue out"),
    (":o", "with a small round open mouth"),
    ("pout", "pouting"),
    ("one eye closed", "winking with one eye closed"),
    ("closed eyes", "with both eyes closed"),
    ("half-closed eyes", "with half-closed eyes"),
    ("wide-eyed", "wide-eyed"),
    ("blush", "blushing"),
    ("tears", "with tears in the eyes"),
    ("crying", "crying"),
    ("frown", "frowning"),
    ("angry", "looking angry"),
    ("annoyed", "looking annoyed"),
    ("surprised", "looking surprised"),
    ("scared", "looking scared"),
    ("sad", "looking sad"),
    ("embarrassed", "looking embarrassed"),
    ("sweatdrop", "with a sweat drop"),
    ("shaded face", "with a shadowed face"),
    ("serious", "looking serious"),
    ("expressionless", "with a blank expression"),
    ("sleepy", "looking sleepy"),
    ("head tilt", "tilting the head"),
];

/// 反推結果（{general: [[tag, 機率], …]}）→ 表情片語。:d／;d 已經包含笑和張嘴，就不再重複寫
pub(super) fn expression_of(tagged: &Value, min: f64) -> String {
    let have: Vec<String> = tagged["general"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t[1].as_f64().unwrap_or(0.0) >= min)
        .map(|t| tx(&t[0]).to_lowercase())
        .collect();
    let has = |k: &str| have.iter().any(|h| h == k);
    let laugh = has(":d") || has(";d");
    let mut out: Vec<&str> = vec![];
    for (tag, phrase) in EXPRESSIONS {
        if !has(tag) || (laugh && matches!(*tag, "smile" | "open mouth" | "laughing")) || out.contains(phrase) {
            continue;
        }
        out.push(phrase);
    }
    out.join(", ")
}

/// 換頭後一定要留住的表情（反推抓得到、看得出差別的那幾種）；blush、smile 這類淡的不算
const KEY_EXPRESSIONS: &[&str] = &[":d", ";d", "open mouth", "one eye closed", "closed eyes", "tongue out", "crying", "tears", ":o", "pout", "clenched teeth", "angry", "surprised", "frown"];

/// 原格有、換完頭卻不見的重點表情 → 回傳原格的整組表情片語（拿去補表情）；沒有掉就 None
pub(super) fn lost_expression(before: &Value, after: &Value, min: f64) -> Option<String> {
    let has = |v: &Value, k: &str| v["general"].as_array().into_iter().flatten().any(|t| tx(&t[0]).eq_ignore_ascii_case(k) && t[1].as_f64().unwrap_or(0.0) >= min);
    KEY_EXPRESSIONS.iter().any(|k| has(before, k) && !has(after, k)).then(|| expression_of(before, min)).filter(|s| !s.is_empty())
}

/// ② 補表情的指令（單張圖，照官方規則不用 <image1> 標籤）。expr＝反推翻好的片語；scene＝反推用不了時改寫這一格的內容
pub(super) fn expression_prompt(expr: &str, scene: &str) -> Option<String> {
    let keep = "Everything else in the image, including each character's identity, face shape, eye colors, hairstyle, accessories, pose and clothing, the background, the speech bubble and its text, and the art style, stays unchanged.";
    let (expr, scene) = (expr.trim(), scene.trim().trim_end_matches(['.', ',']));
    if !expr.is_empty() {
        return Some(format!("Change only the character's facial expression: the character is {expr}. {keep}"));
    }
    (!scene.is_empty()).then(|| format!("Change only the facial expressions so they fit this moment: {scene}. {keep}"))
}

/// 要修哪幾件：找到的格數跟版型一樣就一格一格（只修有角色的格），不然整頁一件（裡面出現過的角色全帶）。
/// 角色卡沒有參考圖的人拿掉；回傳 (每一件, 是不是逐格)
fn plan_fix(p: &Value, panels: Vec<c_p::Panel>) -> Result<(Vec<FixItem>, bool), String> {
    let cards = c::chars(p);
    let names = c::char_names(p);
    let (_, slots) = c::layout_of(p)?;
    let cells = p["panels"].as_array().cloned().unwrap_or_default();
    let mut items: Vec<FixItem> = vec![];
    let split = panels.len() == slots.len() && panels.len() > 1;
    if split {
        for (i, panel) in panels.into_iter().enumerate() {
            let cell = cells.get(i).unwrap_or(&Value::Null);
            let who = who_of(cell, &names);
            if !who.is_empty() {
                items.push(FixItem { panel: Some(panel), who, label: format!("第 {} 格", i + 1), scene: tx(&cell["what"]).trim().to_string() });
            }
        }
    } else {
        let mut who: Vec<String> = vec![];
        for cell in cells.iter().take(slots.len()) {
            for n in who_of(cell, &names) {
                if !who.contains(&n) {
                    who.push(n);
                }
            }
        }
        if who.is_empty() {
            who = names.clone();
        }
        items.push(FixItem { panel: None, who, label: "整頁".into(), scene: String::new() });
    }
    let has_refs = |n: &String| cards.iter().any(|ch| card_name(ch) == *n && ch["refs"].as_array().is_some_and(|a| !a.is_empty()));
    for it in items.iter_mut() {
        it.who.retain(has_refs);
    }
    items.retain(|it| !it.who.is_empty());
    if items.is_empty() {
        return Err("沒有哪一格認得出角色：在每一格勾「這格有誰」（或內容裡寫角色的名字），而且角色卡要有參考圖".into());
    }
    Ok((items, split))
}

/// 一次 Qwen 2.1 多圖參考（官方 Turbo 8 步）：跑完把原始輸出搬到 keep（不留在圖庫），回傳 keep
#[allow(clippy::too_many_arguments)]
async fn qwen_pass(app: &Shared, id: &str, page: i64, title: &str, refs: Vec<Value>, prompt: String, loras: &Value, seed: i64, keep: std::path::PathBuf, token: &CancellationToken) -> Result<std::path::PathBuf, String> {
    qwen_job(app, id, page, title, json!({"mode": "multiref", "refs": refs}), prompt, loras, seed, keep, token).await
}

/// 只換頭（graphs 的 headswap：AnimaHeadCrop 照 mask 那顆頭裁切 → Qwen＋BFS 只重畫頭 → AnimaHeadStitch）。target、mask＝input 裡的檔
#[allow(clippy::too_many_arguments)]
async fn head_pass(app: &Shared, id: &str, page: i64, title: &str, target: &str, mask: &str, refs: Vec<Value>, seed: i64, keep: std::path::PathBuf, token: &CancellationToken) -> Result<std::path::PathBuf, String> {
    let mode = json!({"mode": "headswap", "init_image": target, "refs": refs, "head": head_opts(mask)});
    qwen_job(app, id, page, title, mode, BFS_PROMPT.to_string(), &json!([{"name": BFS_LORA, "strength": 1.0}]), seed, keep, token).await
}

/// headswap 的 head 參數：指定那顆頭的遮罩檔，頭附近多裁 0.6 倍、放大到約 1MP
fn head_opts(mask: &str) -> Value {
    json!({"mask": mask, "context": 0.6, "pixels": crate::graphs::QWEN21_PIXELS})
}

/// 頭跟參考圖的頭至少要這麼像才換（head_match::score，0～1）；2026-11-17 用便利商店那頁校正
const HEAD_MATCH_MIN: f32 = 0.35;

/// 遮罩的外框往外多留 margin 倍（長邊），夾在圖內；遮罩空的回 None。回傳 (x0, y0, x1, y1)，x1／y1 不含
fn head_box(mask: &image::GrayImage, margin: f32) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = mask.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, p) in mask.enumerate_pixels() {
        if p.0[0] > 127 {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x1 == 0 {
        return None;
    }
    let pad = ((x1 - x0).max(y1 - y0) as f32 * margin) as u32;
    Some((x0.saturating_sub(pad), y0.saturating_sub(pad), (x1 + pad).min(w), (y1 + pad).min(h)))
}

/// SAM3 找 input 裡這張圖的每顆頭：每顆一張跟圖一樣大的黑白遮罩（太小的雜點不算），大的在前
async fn detect_heads(app: &Shared, input: &str, (w, h): (u32, u32)) -> Result<Vec<image::GrayImage>, String> {
    let nodes = crate::graphs::detect_graph(input, &[crate::graphs::HEAD_PROMPT], 0.3);
    let pid = app.comfy.submit(&Value::Object(nodes), &json!({})).await.map_err(|e| e.0)?;
    let his = super::api_play::wait_history(app, &pid, "找頭").await?;
    let mut pngs = vec![];
    for o in his["outputs"].as_object().into_iter().flat_map(|o| o.values()) {
        for im in o["images"].as_array().into_iter().flatten() {
            let s = |k: &str| im[k].as_str().unwrap_or("").to_string();
            pngs.push(app.comfy.view(&s("filename"), &s("subfolder"), &s("type")).await.map_err(|e| e.0)?);
        }
    }
    blocking(move || -> Result<Vec<image::GrayImage>, String> {
        let mut out: Vec<image::GrayImage> = vec![];
        for bytes in pngs {
            let m = image::load_from_memory(&bytes).map_err(|e| format!("讀不了找頭的結果：{e}"))?.to_luma8();
            let m = if m.dimensions() == (w, h) { m } else { image::imageops::resize(&m, w, h, image::imageops::FilterType::Nearest) };
            let area = m.pixels().filter(|p| p.0[0] > 127).count() as u64;
            if area * 3000 >= w as u64 * h as u64 {
                out.push(m);
            }
        }
        // 同一顆頭被拆成好幾塊（雙馬尾、瀏海）的併掉；由大到小
        Ok(crate::head_match::dedupe_masks(out))
    })
    .await
}

/// 一顆頭的長相特徵：顏色分布＋反推的長相 tag（頭附近裁下來反推；反推不能用就只有顏色）
async fn head_feature(app: &Shared, img: &image::RgbImage, mask: &image::GrayImage, tmp: std::path::PathBuf) -> crate::head_match::HeadFeat {
    let (img, mask, tg) = (img.clone(), mask.clone(), app.tagger.clone());
    blocking(move || -> Result<crate::head_match::HeadFeat, String> {
        let hist = crate::head_match::color_hist(&img, &mask);
        let mut tags = vec![];
        if let (true, Some((x0, y0, x1, y1))) = (tg.available(), head_box(&mask, 0.25)) {
            let crop = image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image();
            if let Some(d) = tmp.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if crop.save(&tmp).is_ok() {
                tags = tg.tag(&tmp, 0.35, 0.85).map(|v| crate::head_match::look_tags(&v, 0.35)).unwrap_or_default();
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Ok(crate::head_match::HeadFeat { hist, tags })
    })
    .await
    .unwrap_or_default()
}

/// 角色卡參考圖裡那個人的頭（第一張找得到頭的參考圖，挑最大的那顆）；都找不到回 None
async fn ref_head_feature(app: &Shared, files: &[std::path::PathBuf], tmp: &std::path::Path) -> Option<crate::head_match::HeadFeat> {
    for (n, f) in files.iter().enumerate() {
        let Ok(img) = image::open(f).map(|i| i.to_rgb8()) else { continue };
        let name = format!("aw_asset_{}", f.file_name()?.to_string_lossy());
        let Ok(iname) = super::api_director::to_input(app, f, &name) else { continue };
        let Ok(heads) = detect_heads(app, &iname, img.dimensions()).await else { continue };
        if let Some(m) = heads.first() {
            return Some(head_feature(app, &img, m, tmp.join(format!("ref_{n}.png"))).await);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn qwen_job(app: &Shared, id: &str, page: i64, title: &str, mode: Value, prompt: String, loras: &Value, seed: i64, keep: std::path::PathBuf, token: &CancellationToken) -> Result<std::path::PathBuf, String> {
    let mut params = json!({
        "model": crate::graphs::STRIP_QWEN, "size_mode": "auto", "pixels": crate::graphs::QWEN21_PIXELS,
        "prompt": prompt, "negative": "", "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple", "seed": seed % (1i64 << 48),
        "width": 1024, "height": 1024, "loras": loras, "count": 1, "idea": format!("漫畫修角色：{title}"),
        "comic": {"id": id, "kind": "fix_part", "from": page},
    });
    for (k, v) in mode.as_object().into_iter().flatten() {
        params[k] = v.clone();
    }
    let jobs = submit_jobs(app, params.as_object().cloned().unwrap_or_default(), 1).await.map_err(|e| e.1)?;
    let pid = jobs.first().map(|j| tx(&j["job"])).ok_or("沒送出任務")?;
    wait_job(app, &pid, token).await?;
    let row = job_image(app, &pid)?;
    let raw = local_file(app, &row).await?;
    let k2 = keep.clone();
    blocking(move || -> Result<(), String> {
        std::fs::create_dir_all(k2.parent().unwrap_or(&k2)).map_err(|e| e.to_string())?;
        if std::fs::rename(&raw, &k2).is_err() {
            std::fs::copy(&raw, &k2).map_err(|e| format!("搬原始輸出失敗：{e}"))?;
            let _ = std::fs::remove_file(&raw);
        }
        Ok(())
    })
    .await?;
    if let Some(iid) = row.get("id").and_then(Value::as_i64) {
        app.db.run("UPDATE images SET deleted=2 WHERE id=?", [iid]).map_err(|e| e.to_string())?;
    }
    Ok(keep)
}

/// 修角色第一輪（找頭、比對）的結果，第二輪照這個換
struct HeadPlan {
    pimg: image::RgbImage,
    heads: Vec<image::GrayImage>,
    cnames: Vec<String>,
    pick: Vec<Option<usize>>,
}

struct FixPrep {
    k: usize,
    it: FixItem,
    tname: String,
    expr: String,
    refs: Vec<Value>,
    cast: Vec<(String, Vec<usize>)>,
    head: Option<HeadPlan>,
}

async fn fix_page(app: &Shared, id: &str, page: i64, src: std::path::PathBuf, token: &CancellationToken) -> Step {
    let p = load(app, id).map_err(|e| e.1)?;
    let title = tx(&p["title"]);
    let cards = c::chars(&p);
    let slots = c::layout_of(&p)?.1.len();
    emit(app, id, "msg", "找格子…", None);
    let (mut img, panels) = blocking(move || -> Result<(image::RgbImage, Vec<c_p::Panel>), String> {
        let img = image::open(&src).map_err(|e| format!("讀整頁失敗：{e}"))?.to_rgb8();
        let panels = c_p::detect(&img);
        Ok((img, panels))
    })
    .await?;
    let found = panels.len();
    let (items, split) = plan_fix(&p, panels)?;
    app.pe.release_for_generation().await;
    ensure_family(app, families::QWEN21).await;
    let how = if split {
        format!("找到 {slots} 格，要修 {} 格", items.len())
    } else {
        format!("格子找不齊（找到 {found} 塊，版型是 {slots} 格），整頁一次修")
    };
    emit(app, id, "msg", how, None);
    // 對齊原圖的 LoRA：ComfyUI 有才掛（沒有照樣能修，只是位置可能偏一點）
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let have_cons = app
        .comfy
        .options("LoraLoaderModelOnly", "lora_name")
        .await
        .unwrap_or_default()
        .iter()
        .any(|v| v.as_str().is_some_and(|n| norm(n) == norm(FIX_CONSISTENCY_LORA)));
    let loras = if have_cons { json!([{"name": FIX_CONSISTENCY_LORA, "strength": 1.0}]) } else { json!([]) };
    // 只換頭要 BFS LoRA 和繪匣的漫畫小節點（resources\extras\anima_comic_nodes，開站同步；ComfyUI 要重開過才認得）
    let have_bfs = app
        .comfy
        .options("LoraLoaderModelOnly", "lora_name")
        .await
        .unwrap_or_default()
        .iter()
        .any(|v| v.as_str().is_some_and(|n| norm(n) == norm(BFS_LORA)));
    let have_node = app.comfy.object_info("AnimaHeadCrop").await.is_ok_and(|v| v.get("input").is_some());
    let assets = super::api_assets::assets_dir(&app.paths);
    let fixdir = app.paths.data.join("comic").join("fix").join(id);
    let seed0 = random_seed();
    let _inputs = FixInputs { dir: app.paths.input.clone(), prefix: fix_input_prefix(id, page, seed0) };
    // 比對長相用的暫存（頭的裁切拿去反推），角色參考圖的特徵整頁算一次
    let tmpdir = fixdir.join("tmp");
    let mut ref_feats: std::collections::HashMap<String, Option<crate::head_match::HeadFeat>> = std::collections::HashMap::new();
    let total = items.len();
    let mut done = 0usize;
    // 兩輪：① 每一格的頭全部找完、比對完（只用 SAM3＋反推）② 一口氣做全部的 Qwen 換頭。
    // 每格來回換 SAM3／Qwen 會讓 ComfyUI 的動態 VRAM 反覆搬模型，2026-11-17 實測兩次卡死在 Model Initializing
    let mut preps: Vec<FixPrep> = vec![];
    for (k, it) in items.into_iter().enumerate() {
        let who = it.who.join("、");
        // 要修的圖放進 ComfyUI input；逐格修時順便反推原本的表情（CPU，跟顯卡不搶）
        let target = it.panel.as_ref().map(|pn| c_p::crop(&img, pn)).unwrap_or_else(|| img.clone());
        let tname = fix_input_name(id, page, seed0, k, "");
        let tpath = app.paths.input.join(&tname);
        let route = fix_route(it.who.len(), it.panel.is_some(), have_bfs, have_node);
        let (tg, single, tp2) = (app.tagger.clone(), route == FixRoute::TwoPass && it.panel.is_some() && it.who.len() == 1, tpath.clone());
        let expr = blocking(move || -> Result<String, String> {
            target.save(&tp2).map_err(|e| format!("存要修的圖失敗：{e}"))?;
            // 表情片語只在格子裡只有一個角色時寫（兩個人分不出誰是誰的表情）；反推不能用就空的，改寫格子內容
            Ok(if single && tg.available() { tg.tag(&tp2, 0.35, 0.85).map(|v| expression_of(&v, 0.35)).unwrap_or_default() } else { String::new() })
        })
        .await?;
        // 角色參考圖：每個角色前 2 張，全部最多 6 張；<image1> 是要修的圖
        let mut refs = vec![json!({"image": tname})];
        let mut cast: Vec<(String, Vec<usize>)> = vec![];
        for name in &it.who {
            let Some(ch) = cards.iter().find(|ch| card_name(ch) == *name) else { continue };
            let mut ns = vec![];
            for f in ch["refs"].as_array().into_iter().flatten().take(FIX_REFS_PER_CHAR) {
                let f = tx(f);
                let file = std::path::Path::new(&f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                if file.is_empty() || refs.len() > FIX_MAX_REFS || !assets.join(&file).is_file() {
                    continue;
                }
                let iname = super::api_director::to_input(app, &assets.join(&file), &format!("aw_asset_{file}"))?;
                refs.push(json!({"image": iname}));
                ns.push(refs.len());
            }
            if !ns.is_empty() {
                cast.push((name.clone(), ns));
            }
        }
        if cast.is_empty() {
            continue;
        }
        let head = if route == FixRoute::Head {
            // 只換頭：SAM3 找出這格每顆頭 → 跟角色卡參考圖的頭比長相 → 每個角色換自己那顆（路人不動；都不像就不換）
            emit(app, id, "msg", format!("修{}（{who}）：找頭、比對長相… {}/{total}", it.label, k + 1), None);
            let tp = tpath.clone();
            let pimg = blocking(move || image::open(&tp).map(|i| i.to_rgb8()).map_err(|e| format!("讀格子失敗：{e}"))).await?;
            let heads = detect_heads(app, &tname, pimg.dimensions()).await?;
            let mut feats = vec![];
            for (h, m) in heads.iter().enumerate() {
                feats.push(head_feature(app, &pimg, m, tmpdir.join(format!("{k}_head{h}.png"))).await);
            }
            let mut cnames: Vec<String> = vec![];
            let mut cfeats = vec![];
            for (name, _) in &cast {
                if !ref_feats.contains_key(name) {
                    let files: Vec<std::path::PathBuf> = cards
                        .iter()
                        .find(|ch| card_name(ch) == *name)
                        .map(|ch| {
                            ch["refs"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .take(FIX_REFS_PER_CHAR)
                                .filter_map(|f| std::path::Path::new(&tx(f)).file_name().map(|x| assets.join(x)))
                                .filter(|f| f.is_file())
                                .collect()
                        })
                        .unwrap_or_default();
                    let f = ref_head_feature(app, &files, &tmpdir).await;
                    ref_feats.insert(name.clone(), f);
                }
                if let Some(f) = ref_feats.get(name).cloned().flatten() {
                    cnames.push(name.clone());
                    cfeats.push(f);
                }
            }
            let pick = crate::head_match::assign(&cfeats, &feats, HEAD_MATCH_MIN);
            for (ci, name) in cnames.iter().enumerate() {
                let scores: Vec<String> = feats.iter().map(|hf| format!("{:.2}", crate::head_match::score(&cfeats[ci], hf))).collect();
                eprintln!("[修角色] {} {name}：{} 顆頭 分數 [{}] → {:?}", it.label, feats.len(), scores.join(", "), pick[ci]);
            }
            Some(HeadPlan { pimg, heads, cnames, pick })
        } else {
            None
        };
        preps.push(FixPrep { k, it, tname, expr, refs, cast, head });
    }
    // 找頭用的 SAM3 先卸掉，Qwen 才有乾淨的空間載入
    if preps.iter().any(|p| p.head.is_some()) {
        let _ = app.comfy.post("/free", &json!({"unload_models": true, "free_memory": true})).await;
    }
    for FixPrep { k, it, tname, expr, refs, cast, head } in preps {
        let who = it.who.join("、");
        let seed = seed0 + k as i64 * 7919;
        let fixed = if let Some(HeadPlan { pimg, heads, cnames, pick }) = head {
            let mut cur = tname.clone();
            let mut out: Option<std::path::PathBuf> = None;
            for (ci, name) in cnames.iter().enumerate() {
                let Some(h) = pick[ci] else {
                    emit(app, id, "msg", format!("修{}：找不到像 {name} 的頭（可能被擋住或太小），{name} 這格不換", it.label), None);
                    continue;
                };
                let mname = fix_input_name(id, page, seed0, k, &format!("m{ci}"));
                let (m, mp) = (heads[h].clone(), app.paths.input.join(&mname));
                blocking(move || m.save(&mp).map_err(|e| format!("存頭的遮罩失敗：{e}"))).await?;
                emit(app, id, "msg", format!("修{}（{name}）：換頭… {}/{total}", it.label, k + 1), None);
                let crefs: Vec<Value> = cast.iter().find(|(n, _)| n == name).map(|(_, ns)| ns.iter().map(|&n| refs[n - 1].clone()).collect()).unwrap_or_default();
                let seed = seed + ci as i64 * 101;
                let keep = fixdir.join(format!("{page}_{seed0}_{k}_{ci}_h.png"));
                let swapped = head_pass(app, id, page, &title, &cur, &mname, crefs, seed, keep, token).await?;
                // 這顆頭的表情（只看頭附近）：原格的重點表情（張嘴、眨眼、閉眼…）換完不見了，就只對這顆頭補一次
                let (tg, before, after, mk, tmp) = (app.tagger.clone(), pimg.clone(), swapped.clone(), heads[h].clone(), tmpdir.join(format!("{k}_{ci}_expr.png")));
                let lost = blocking(move || -> Result<Option<String>, String> {
                    let Some((x0, y0, x1, y1)) = head_box(&mk, 0.15).filter(|_| tg.available()) else { return Ok(None) };
                    let after = image::open(&after).map_err(|e| e.to_string())?.to_rgb8();
                    let tag = |img: &image::RgbImage| -> Result<Value, String> {
                        image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image().save(&tmp).map_err(|e| e.to_string())?;
                        tg.tag(&tmp, 0.35, 0.85)
                    };
                    let (b, a) = (tag(&before)?, tag(&after)?);
                    let _ = std::fs::remove_file(&tmp);
                    Ok(lost_expression(&b, &a, 0.35))
                })
                .await
                .unwrap_or(None);
                let result = match lost.and_then(|e| expression_prompt(&e, "")) {
                    Some(prompt) => {
                        emit(app, id, "msg", format!("修{}（{name}）：把表情改回原本的樣子… {}/{total}", it.label, k + 1), None);
                        let ename = fix_input_name(id, page, seed0, k, &format!("e{ci}"));
                        super::api_director::to_input(app, &swapped, &ename)?;
                        let mode = json!({"mode": "headswap", "init_image": ename, "refs": [], "head": head_opts(&mname)});
                        let keep = fixdir.join(format!("{page}_{seed0}_{k}_{ci}_e.png"));
                        qwen_job(app, id, page, &title, mode, prompt, &loras, seed + 1, keep, token).await?
                    }
                    None => swapped,
                };
                // 下一個角色接著在這張上面換
                cur = fix_input_name(id, page, seed0, k, &format!("s{ci}"));
                super::api_director::to_input(app, &result, &cur)?;
                out = Some(result);
            }
            match out {
                Some(p) => p,
                None => continue,
            }
        } else {
            two_pass(app, id, page, &title, &it, &who, (k, total), refs, &cast, &expr, &loras, seed, (&fixdir, seed0), token).await?
        };
        // 貼回去
        let panel = it.panel;
        img = blocking(move || -> Result<image::RgbImage, String> {
            let fixed = image::open(&fixed).map_err(|e| format!("讀 Qwen 的結果失敗：{e}"))?.to_rgb8();
            let mut img = img;
            match &panel {
                Some(pn) => c_p::paste(&mut img, pn, &fixed),
                None => img = image::imageops::resize(&fixed, img.width(), img.height(), image::imageops::FilterType::Lanczos3),
            }
            Ok(img)
        })
        .await?;
        done += 1;
    }
    finish_fix(app, id, page, &title, img, seed0, done).await
}

/// 兩段式：① 換頭（多圖參考）② 補表情（反推片語或格子內容，整頁一次修不補）
#[allow(clippy::too_many_arguments)]
async fn two_pass(
    app: &Shared,
    id: &str,
    page: i64,
    title: &str,
    it: &FixItem,
    who: &str,
    (k, total): (usize, usize),
    refs: Vec<Value>,
    cast: &[(String, Vec<usize>)],
    expr: &str,
    loras: &Value,
    seed: i64,
    (fixdir, seed0): (&std::path::Path, i64),
    token: &CancellationToken,
) -> Result<std::path::PathBuf, String> {
    // ① 換頭
    emit(app, id, "msg", format!("修{}（{who}）：換成角色卡的長相… {}/{total}", it.label, k + 1), None);
    let swapped = qwen_pass(app, id, page, title, refs, swap_prompt(it.panel.is_none(), cast), loras, seed, fixdir.join(format!("{page}_{seed0}_{k}_a.png")), token).await?;
    // ② 補表情（整頁一次修不補：一頁裡每個人的表情不一樣）
    let second = if it.panel.is_some() { expression_prompt(expr, &it.scene) } else { None };
    match second {
        Some(prompt) => {
            emit(app, id, "msg", format!("修{}（{who}）：把表情改回原本的樣子… {}/{total}", it.label, k + 1), None);
            let bname = fix_input_name(id, page, seed0, k, "b");
            super::api_director::to_input(app, &swapped, &bname)?;
            qwen_pass(app, id, page, title, vec![json!({"image": bname})], prompt, loras, seed + 1, fixdir.join(format!("{page}_{seed0}_{k}_b.png")), token).await
        }
        None => Ok(swapped),
    }
}

/// 修好的整頁存成新的一張（進圖庫），記在那一筆結果的 fix
async fn finish_fix(app: &Shared, id: &str, page: i64, title: &str, img: image::RgbImage, seed0: i64, done: usize) -> Step {
    if done == 0 {
        return Err("角色卡的參考圖檔都找不到了（資產可能被刪掉）：重新從資產抽屜加角色".into());
    }
    let fname = format!("comicfix_{id}_{page}_{seed0}_00001_.png");
    let out = app.paths.output.join("AnimaWeb").join(&fname);
    let (w, h) = img.dimensions();
    blocking(move || -> Result<(), String> {
        std::fs::create_dir_all(out.parent().unwrap_or(&out)).map_err(|e| e.to_string())?;
        img.save(&out).map_err(|e| format!("存修好的整頁失敗：{e}"))
    })
    .await?;
    let title = format!("漫畫修角色：{title}");
    let info = json!({"mode": "multiref", "prompt": title, "negative": "", "seed": seed0, "width": w, "height": h, "idea": title});
    let params = json!({"mode": "multiref", "model": crate::graphs::STRIP_QWEN, "prompt": title, "seed": seed0, "width": w, "height": h,
                        "comic": {"id": id, "kind": "fix", "from": page}});
    let nid = app.db.add_image(&fname, "AnimaWeb", "output", &info, &params, None).map_err(|e| e.to_string())?;
    update(app, id, |p| {
        if let Some(r) = p["runs"].as_array_mut().and_then(|a| a.iter_mut().find(|r| r["page"].as_i64() == Some(page))) {
            r["fix"] = json!(nid);
        }
    })
    .await
    .map_err(|e| e.1)?;
    emit(app, id, "page", format!("角色修好了（{done} 件）"), Some(nid));
    Ok(())
}

#[cfg(test)]
mod fix_tests {
    use super::*;

    #[test]
    fn who_comes_from_ticks_or_whole_word_names() {
        let names = vec!["Lin".to_string(), "Ken".to_string()];
        assert_eq!(who_of(&json!({"who": ["Ken"], "what": "Lin waves"}), &names), vec!["Ken"], "勾的優先");
        assert_eq!(who_of(&json!({"what": "lin hugs Ken."}), &names), vec!["Lin", "Ken"]);
        assert!(who_of(&json!({"what": "Linda waves at Kenji"}), &names).is_empty(), "整個字才算");
        let zh = vec!["小美".to_string()];
        assert_eq!(who_of(&json!({"what": "教室裡的小美在笑"}), &zh), vec!["小美"], "中文名字前後是漢字也算");
    }

    #[test]
    fn swap_prompt_leads_with_the_operation_and_names_each_reference() {
        let one = swap_prompt(false, &[("Lin".into(), vec![2, 3])]);
        assert!(one.starts_with("Replace the head of the character in <image1> with the head of Lin from <image2> and <image3>,"), "{one}");
        assert!(one.contains("<image1> is the canvas: the facial expressions,") && one.contains("art style, line art, coloring and lighting of <image1>"), "{one}");
        let two = swap_prompt(false, &[("Lin".into(), vec![2]), ("Ken".into(), vec![3])]);
        assert!(two.contains("matching reference character: Lin from <image2>; Ken from <image3>."), "{two}");
        let whole = swap_prompt(true, &[("Lin".into(), vec![2])]);
        assert!(whole.starts_with("In every panel of <image1>") && whole.contains("every letter of their text"), "{whole}");
    }

    #[test]
    fn head_box_adds_margin_and_stays_inside() {
        let mut m = image::GrayImage::new(100, 80);
        for y in 10..30 {
            for x in 40..60 {
                m.put_pixel(x, y, image::Luma([255]));
            }
        }
        assert_eq!(head_box(&m, 0.25), Some((35, 5, 65, 35)));
        assert_eq!(head_box(&m, 5.0), Some((0, 0, 100, 80)), "夾在圖內");
        assert_eq!(head_box(&image::GrayImage::new(10, 10), 0.25), None);
    }

    #[test]
    fn fix_inputs_are_cleaned_even_when_the_run_fails() {
        let d = std::env::temp_dir().join(format!("aw_fix_inputs_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for f in ["comicfix_p1_438_111_0.png", "comicfix_p1_438_111_2_e.png", "comicfix_p1_438_222_0.png", "other.png"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        let failed = || -> Result<(), String> {
            let _guard = FixInputs { dir: d.clone(), prefix: fix_input_prefix("p1", 438, 111) };
            Err("Qwen 失敗".into())
        };
        assert!(failed().is_err());
        let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, vec!["comicfix_p1_438_222_0.png", "other.png"], "只清這一輪的");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fix_inputs_get_a_new_name_every_run() {
        // ComfyUI 讀的 input 跟繪匣的不同時，同名的舊檔會被拿去用（2026-11-17 實測：整頁被貼進第 1 格）→ 每一輪名字都不同
        let a = fix_input_name("p1", 438, 111, 0, "");
        assert_eq!(a, "comicfix_p1_438_111_0.png");
        assert_ne!(a, fix_input_name("p1", 438, 222, 0, ""));
        assert_eq!(fix_input_name("p1", 438, 111, 2, "e"), "comicfix_p1_438_111_2_e.png");
    }

    #[test]
    fn fix_route_picks_head_swap_only_when_safe() {
        assert_eq!(fix_route(1, true, true, true), FixRoute::Head);
        assert_eq!(fix_route(2, true, true, true), FixRoute::Head, "兩個人：比對參考圖各找各的頭");
        assert_eq!(fix_route(0, true, true, true), FixRoute::TwoPass);
        assert_eq!(fix_route(1, false, true, true), FixRoute::TwoPass, "整頁一次修");
        assert_eq!(fix_route(1, true, false, true), FixRoute::TwoPass, "沒有 BFS");
        assert_eq!(fix_route(1, true, true, false), FixRoute::TwoPass, "節點包沒同步到");
        assert!(BFS_PROMPT.starts_with("head_swap: start with <image1> as the base image") && BFS_PROMPT.contains("micro expressions from <image1>"));
    }

    #[test]
    fn lost_expression_only_when_a_key_tag_disappears() {
        let before = json!({"general": [["smile", 0.9], [":d", 0.7], ["one eye closed", 0.6], ["blush", 0.5]]});
        let kept = json!({"general": [["smile", 0.8], [":d", 0.6], ["one eye closed", 0.5]]});
        assert_eq!(lost_expression(&before, &kept, 0.35), None, "重點表情都還在（blush 不算重點）");
        let lost = json!({"general": [["smile", 0.8], [":d", 0.6]]});
        assert_eq!(lost_expression(&before, &lost, 0.35).as_deref(), Some("laughing with the mouth wide open, winking with one eye closed, blushing"), "眨眼不見了：整組表情重寫");
        assert_eq!(lost_expression(&json!({"general": [["smile", 0.9]]}), &json!({}), 0.35), None, "原本沒有重點表情");
    }

    #[test]
    fn expression_comes_from_tagger_tags() {
        let t = json!({"general": [["smile", 0.9], [":d", 0.6], ["open mouth", 0.8], ["one eye closed", 0.5], ["blush", 0.2], ["1girl", 0.99]]});
        assert_eq!(expression_of(&t, 0.35), "laughing with the mouth wide open, winking with one eye closed", ":d 已含笑和張嘴；低於門檻的 blush 不算");
        let t = json!({"general": [["smile", 0.7], ["closed mouth", 0.6]]});
        assert_eq!(expression_of(&t, 0.35), "smiling, with the mouth closed");
        assert_eq!(expression_of(&json!({}), 0.35), "");
        let p = expression_prompt("smiling", "Lin drinks tea").unwrap();
        assert!(p.starts_with("Change only the character's facial expression: the character is smiling. Everything else"), "{p}");
        assert!(!p.contains("<image"), "單張圖不用標籤");
        let p = expression_prompt("", "Lin laughs at Ken.").unwrap();
        assert!(p.contains("so they fit this moment: Lin laughs at Ken. Everything"), "{p}");
        assert!(expression_prompt(" ", "").is_none());
    }

    fn rect(w: u32, h: u32) -> c_p::Panel {
        c_p::Panel { x: 0, y: 0, w, h, mask: image::GrayImage::new(w, h) }
    }

    #[test]
    fn plan_splits_only_when_panel_count_matches() {
        let p = json!({"layout": "L3", "chars": [{"name": "Lin", "refs": ["1_2.png"]}, {"name": "Ken", "refs": []}],
                       "panels": [{"what": "Lin smiles"}, {"what": "a cup of tea", "who": []}, {"what": "Ken and Lin", "who": ["Ken"]}]});
        let (items, split) = plan_fix(&p, vec![rect(10, 10), rect(10, 10), rect(10, 10)]).unwrap();
        assert!(split);
        // 第二格沒人；第三格只勾 Ken，但 Ken 沒有參考圖 → 拿掉
        assert_eq!(items.iter().map(|i| (i.label.as_str(), i.who.clone())).collect::<Vec<_>>(), vec![("第 1 格", vec!["Lin".to_string()])]);
        // 找到的格數不對：整頁一件
        let (items, split) = plan_fix(&p, vec![rect(10, 10)]).unwrap();
        assert!(!split);
        assert_eq!((items.len(), items[0].panel.is_none(), items[0].who.clone()), (1, true, vec!["Lin".to_string()]));
        // 沒人有參考圖：講清楚
        let none = json!({"layout": "L3", "chars": [{"name": "Ken", "refs": []}], "panels": [{"what": "Ken"}]});
        assert!(plan_fix(&none, vec![]).is_err());
    }

    #[test]
    fn own_loras_follow_the_base_ones() {
        let base = json!([{"name": "style.safetensors", "strength": 0.6}, {"name": "lin.safetensors", "strength": 1.0}]);
        let own = json!([{"name": "lin.safetensors", "strength": 0.7, "enabled": true}, {"name": "off.safetensors", "enabled": false}, {"name": "ken.safetensors"}]);
        assert_eq!(merge_loras(Some(&base), &own), json!([{"name": "style.safetensors", "strength": 0.6},
            {"name": "lin.safetensors", "strength": 0.7}, {"name": "ken.safetensors", "strength": 0.8}]));
        assert_eq!(merge_loras(None, &Value::Null), json!([]));
        let odd = json!([{"name": "../x.safetensors"}, {"name": "C:/x.safetensors"}, {"name": "moss\\a.safetensors", "strength": 99}]);
        assert_eq!(merge_loras(None, &odd), json!([{"name": "moss\\a.safetensors", "strength": 3.0}]), "反斜線原樣保留，ComfyUI 才對得到");
    }
}

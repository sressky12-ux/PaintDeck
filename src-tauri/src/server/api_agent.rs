//! 生圖助手 API（/api/agent/*）與背景工作。規則、對話組裝、檢查在 crate::agent；這裡負責叫模型、送生圖、等結果、存事件。
//!
//! 一輪：想（agent_chat）→ 檢查動作（不合格退回再想，最多 MAX_REJECTS 次）→ 畫（本機 submit_jobs，或雲端）→ 看（下一輪把圖給它）…
//! 直到它交卷（finish）或只回話（reply）。每多一個事件就存檔、送 hub {"type":"agent", id, phase:"event", event}；
//! 進度文字送 phase "think"／"render"；結束送 phase "end"（網頁收到就重新讀對話）。
//! 圖片全部照一般生成的路進圖庫：params 帶 idea「助手：使用者的話」和 agent {id, round}。
use super::api_cloud;
use super::api_gallery::file_of;
use super::api_gen::submit_jobs;
use super::{bad, blocking, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::agent::{self as ag, LoraInfo, Store};
use crate::agent_tools as at;
use crate::director::tx;
use crate::families::{self, ANIMA, MOSS, QWEN21};
use crate::graphs;
use crate::py::{now, truthy};
use axum::extract::{Path as UrlPath, Query, State};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// 「選了 35B、實際用 9B」那則提示的開頭（拿來判斷這段對話講過了沒）
const NO_35B: &str = "腦袋選了 35B，但";

pub struct Agent {
    pub store: Store,
    /// 對話檔「讀 → 改 → 寫」排隊
    lock: tokio::sync::Mutex<()>,
    /// 正在跑的對話 → 停止用
    runs: Mutex<HashMap<String, CancellationToken>>,
    /// 「認識 LoRA」背景工作（同時只跑一個）
    learn: Mutex<Option<CancellationToken>>,
    pub cloud: api_cloud::Cloud,
}

impl Agent {
    pub fn new(data: &Path) -> Agent {
        Agent {
            store: Store::new(data),
            lock: tokio::sync::Mutex::new(()),
            runs: Mutex::new(HashMap::new()),
            learn: Mutex::new(None),
            cloud: api_cloud::Cloud::new(&data.join("agent").join("cloud")),
        }
    }
    fn running(&self, id: &str) -> bool {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
    }
    pub fn any_running(&self) -> bool {
        !self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
            || self.learn.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

fn rid(id: &str) -> String {
    format!("agent-{id}")
}

fn emit(app: &Shared, id: &str, phase: &str, msg: impl Into<String>) {
    app.hub.send(json!({"type": "agent", "id": id, "phase": phase, "msg": msg.into()}));
}

fn load(app: &Shared, id: &str) -> Result<Value, ApiError> {
    app.agent.store.session(id).ok_or_else(|| not_found("找不到這個對話"))
}

async fn update(app: &Shared, id: &str, f: impl FnOnce(&mut Value)) -> Result<Value, ApiError> {
    let _g = app.agent.lock.lock().await;
    let mut s = load(app, id)?;
    f(&mut s);
    app.agent.store.save(&mut s).map_err(internal)?;
    Ok(s)
}

/// 加一個事件（存檔＋通知網頁）
async fn push(app: &Shared, id: &str, mut ev: Value) -> Result<(), String> {
    ev["t"] = json!(now());
    let e2 = ev.clone();
    update(app, id, move |s| {
        if let Some(a) = s["events"].as_array_mut() {
            a.push(e2);
        }
    })
    .await
    .map_err(|e| e.1)?;
    app.hub.send(json!({"type": "agent", "id": id, "phase": "event", "event": ev}));
    Ok(())
}

async fn note(app: &Shared, id: &str, text: impl Into<String>) {
    let _ = push(app, id, json!({"kind": "note", "text": text.into()})).await;
}

// ================================================================ 對話
pub async fn sessions(State(app): State<Shared>) -> ApiResult {
    let running: Vec<String> = app.agent.runs.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    let a = app.clone();
    ok(json!({"sessions": blocking(move || a.agent.store.sessions()).await, "running": running}))
}

pub async fn session_get(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let mut s = load(&app, &id)?;
    s["running"] = json!(app.agent.running(&id));
    ok(s)
}

pub async fn session_new(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let mut s = ag::blank_session(&b["settings"]);
    app.agent.store.save(&mut s).map_err(internal)?;
    ok(s)
}

/// 改設定（腦袋、輪數、張數、自動重畫、先想再答、讓它挑 LoRA、生圖模型）；跑到一半改，下一輪生效
pub async fn session_settings(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let s = update(&app, &id, |s| merge_settings(s, &b["settings"])).await?;
    ok(s)
}

/// 助手頁直接換生圖模型：{id, model}。步數、CFG、取樣器、反向詞照家族預設（跟助手自己換模型同一套），家族變了就拿掉固定 LoRA
pub async fn base_model(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let want = tx(&b["model"]);
    let (list, neg) = super::api_agent_tools::models(&app).await.map_err(bad)?;
    let Some((name, fam)) = list.into_iter().find(|(n, _)| *n == want) else {
        return Err(bad(format!("找不到模型 {want}")));
    };
    if !families::is_builtin(&fam) {
        return Err(bad("助手只能用 Anima、Qwen 2.1、MOSS-Image 的模型"));
    }
    let s = update(&app, &id, |s| {
        let base = s["settings"]["base"].as_object().cloned().unwrap_or_default();
        let nb = super::api_agent_tools::switch_base(&base, &name, &fam, &neg);
        merge_settings(s, &json!({"base": nb}));
    })
    .await?;
    ok(s)
}

fn merge_settings(s: &mut Value, patch: &Value) {
    let mut cur = s["settings"].as_object().cloned().unwrap_or_default();
    if let Some(p) = patch.as_object() {
        for (k, v) in p {
            cur.insert(k.clone(), v.clone());
        }
    }
    s["settings"] = ag::clean_settings(&Value::Object(cur));
}

pub async fn session_delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    if app.agent.running(&id) {
        return Err(bad("這個對話還在跑，先按「停止」"));
    }
    let _g = app.agent.lock.lock().await;
    app.agent.store.trash(&id, &app.paths.root.join("待刪除")).map_err(internal)?;
    ok(json!({"ok": true}))
}

/// 說話：{id?（沒有就開新對話）, text, images?, settings?}。助手正在跑就當插話（下一輪照你的話改），沒在跑就開始做。
/// images＝[{name, width, height}]：已經上傳到 ComfyUI input 的圖（網頁用 /api/upload 或 /api/to_input 先放進去）
pub async fn say(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let text = tx(&b["text"]).trim().to_string();
    let mut images: Vec<Value> = vec![];
    let list = b["images"].as_array().cloned().unwrap_or_default();
    if list.len() > ag::MAX_USER_IMAGES {
        return Err(bad(format!("一次最多附 {} 張圖", ag::MAX_USER_IMAGES)));
    }
    for im in &list {
        let name = super::basename(&tx(&im["name"]));
        if name.is_empty() || !app.paths.input.join(&name).is_file() {
            return Err(bad(format!("找不到附的圖 {name}，請重新加一次")));
        }
        images.push(json!({"name": name, "width": im["width"], "height": im["height"]}));
    }
    if text.is_empty() && images.is_empty() {
        return Err(bad("要先說想畫什麼"));
    }
    if text.chars().count() > 4000 {
        return Err(bad("太長了（上限四千字）"));
    }
    let mut id = tx(&b["id"]);
    if id.is_empty() || app.agent.store.session(&id).is_none() {
        let mut s = ag::blank_session(&b["settings"]);
        app.agent.store.save(&mut s).map_err(internal)?;
        id = tx(&s["id"]);
    } else if b["settings"].is_object() {
        update(&app, &id, |s| merge_settings(s, &b["settings"])).await?;
    }
    // 附的圖編號：整個對話接著數（模型看到的是 User image N）
    let before = load(&app, &id)?["events"].as_array().map(|e| ag::user_images(e).len()).unwrap_or(0);
    let numbered: Vec<Value> = images
        .into_iter()
        .enumerate()
        .map(|(k, mut im)| {
            im["n"] = json!((before + k + 1) as i64);
            im
        })
        .collect();
    let user_ev = |mid: bool| {
        let mut e = json!({"kind": "user", "text": text});
        if let Some(n) = app.pe.poses.term_note(&text) {
            e["pose"] = n;
        }
        if mid {
            e["mid"] = json!(true);
        }
        if !numbered.is_empty() {
            e["images"] = json!(numbered);
        }
        e
    };
    if app.agent.running(&id) {
        push(&app, &id, user_ev(true)).await.map_err(bad)?;
        return ok(json!({"ok": true, "id": id, "queued": true}));
    }
    let s = load(&app, &id)?;
    let st = &s["settings"];
    let words = st["mode"] == "prompt";
    if !words && !st["base"].is_object() {
        return Err(bad("還沒選生圖模型：按「用生成頁目前的設定」"));
    }
    let cloud = st["brain"] == "cloud";
    if cloud && app.agent.cloud.endpoints().is_none() {
        return Err(bad("雲端還沒開好：先按「開雲端」，等燈號變綠"));
    }
    if cloud && app.agent.cloud.live_packs().is_some_and(|p| !p.agent) {
        return Err(bad("雲端這台只裝了 H3 影片包，沒有助手的腦袋：關機後在雲端設定勾「生圖助手」再開"));
    }
    // 雲端只裝 MOSS-Image（api_cloud::render）：先講，免得每輪都想完才在送圖時失敗（2026-10-29 盤點）
    if cloud && !words && families::family_of(&app.paths.unet_roots(), &tx(&st["base"]["model"])) != MOSS {
        return Err(bad("雲端目前只裝 MOSS-Image：生成頁請換成 MOSS 模型再按「用生成頁目前的設定」"));
    }
    if !words && !cloud && !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線"));
    }
    deny_waiting(&app, &id, "The user did not approve it and asked for something else instead.").await.map_err(bad)?;
    let t2 = if text.is_empty() { "看圖".to_string() } else { text.clone() };
    update(&app, &id, move |s| {
        if s["title"] == "新對話" {
            s["title"] = json!(t2.chars().take(30).collect::<String>());
        }
    })
    .await?;
    push(&app, &id, user_ev(false)).await.map_err(bad)?;
    start(&app, &id)?;
    ok(json!({"ok": true, "id": id}))
}

fn start(app: &Shared, id: &str) -> Result<(), ApiError> {
    start_with(app, id, None)
}

/// pending＝使用者剛准的工具 {tool, args}：先做它再接著想
fn start_with(app: &Shared, id: &str, pending: Option<Value>) -> Result<(), ApiError> {
    let token = {
        let mut runs = app.agent.runs.lock().unwrap_or_else(|e| e.into_inner());
        if runs.contains_key(id) {
            return Err(bad("這個對話已經在跑了"));
        }
        let t = CancellationToken::new();
        runs.insert(id.to_string(), t.clone());
        t
    };
    let (app, id) = (app.clone(), id.to_string());
    crate::diagnostics::spawn(async move {
        let r = match pending {
            Some(c) => match run_tool(&app, &id, &tx(&c["tool"]), &c["args"], &token).await {
                Ok(()) => run(&app, &id, &token).await,
                Err(e) => Err(e),
            },
            None => run(&app, &id, &token).await,
        };
        app.agent.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        app.agent.cloud.touch();
        match r {
            Ok(()) => emit(&app, &id, "end", "完成"),
            Err(_) if token.is_cancelled() => {
                note(&app, &id, "已停止").await;
                emit(&app, &id, "end", "已停止")
            }
            Err(e) => {
                eprintln!("生圖助手 {id}：{e}");
                let _ = push(&app, &id, json!({"kind": "error", "text": e})).await;
                emit(&app, &id, "end", "出錯了")
            }
        }
    });
    Ok(())
}

/// 等確認的工具：最後一張 state=wait 的確認卡
fn waiting(events: &[Value]) -> Option<usize> {
    let k = events.iter().rposition(|e| ag::ev_kind(e) == "confirm")?;
    (events[k]["state"] == "wait").then_some(k)
}

/// 把等確認的卡標成不准（使用者改說別的、按了不准），並告訴助手
async fn deny_waiting(app: &Shared, id: &str, why: &str) -> Result<(), String> {
    let s = load(app, id).map_err(|e| e.1)?;
    let events = s["events"].as_array().cloned().unwrap_or_default();
    let Some(k) = waiting(&events) else { return Ok(()) };
    update(app, id, |s| s["events"][k]["state"] = json!("no")).await.map_err(|e| e.1)?;
    let tool = tx(&events[k]["call"]["tool"]);
    push(app, id, json!({"kind": "tool", "tool": tool, "ok": false, "text": why, "zh": "沒有准，不做"})).await
}

/// 確認卡的「准／不准」：{id, ok}。准就先做那個工具再接著跑；不准就告訴助手、讓它接著想
pub async fn confirm(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    if app.agent.running(&id) {
        return Err(bad("助手還在跑，等它停下來再按"));
    }
    let s = load(&app, &id)?;
    let events = s["events"].as_array().cloned().unwrap_or_default();
    let k = waiting(&events).ok_or_else(|| bad("沒有等你確認的事"))?;
    if !truthy(b.get("ok")) {
        deny_waiting(&app, &id, "The user did not approve this. Do not try it again; carry on without it, or tell the user.").await.map_err(bad)?;
        start(&app, &id)?;
        return ok(json!({"ok": true}));
    }
    update(&app, &id, |s| s["events"][k]["state"] = json!("yes")).await?;
    start_with(&app, &id, Some(events[k]["call"].clone()))?;
    ok(json!({"ok": true}))
}

/// 做一次工具：結果記成 tool 事件，有新圖再記一個 result（下一輪給它看）。
/// 「讓我看」模式先叫主視窗切到那一頁，做完把參數和圖送過去。工具失敗不算整段出錯：告訴助手讓它自己想辦法
async fn run_tool(app: &Shared, id: &str, tool: &str, args: &Value, token: &CancellationToken) -> Step {
    let s = load(app, id).map_err(|e| e.1)?;
    let st = s["settings"].clone();
    let events = s["events"].as_array().cloned().unwrap_or_default();
    let base = st["base"].as_object().cloned().unwrap_or_default();
    let show = truthy(st.get("show"));
    let page = at::page_of(tool, args);
    if let (true, Some(page)) = (show, page) {
        app.hub.send(json!({"type": "agent_ui", "id": id, "op": "goto", "page": page, "tool": tool}));
    }
    let zh = at::find(tool).map(|t| t.zh).unwrap_or(tool);
    emit(app, id, "render", format!("{zh}…"));
    let (uall, ask) = (ag::user_images(&events), ag::request_text(&events));
    let brain = tx(&st["brain"]);
    let cx = super::api_agent_tools::Ctx { sid: id, base: &base, count: st["count"].as_i64().unwrap_or(2), brain: &brain, user_images: &uall, ask: &ask };
    let t0 = Instant::now();
    match super::api_agent_tools::exec(app, &cx, tool, args, token).await {
        Ok(o) => {
            if let Some(b) = o.base {
                update(app, id, move |s| s["settings"]["base"] = b).await.map_err(|e| e.1)?;
            }
            push(app, id, json!({"kind": "tool", "tool": tool, "args": args, "ok": true, "text": o.text, "zh": o.zh, "secs": t0.elapsed().as_secs_f64()})).await?;
            if !o.ids.is_empty() {
                let round = events.iter().filter(|e| ag::ev_kind(e) == "result").count() as i64 + 1;
                push(app, id, json!({"kind": "result", "round": round, "ids": o.ids, "errors": [], "tool": tool, "secs": t0.elapsed().as_secs_f64()})).await?;
            }
            if let (true, Some(page)) = (show, page) {
                app.hub.send(json!({"type": "agent_ui", "id": id, "op": "show", "page": page, "tool": tool, "params": o.params, "ids": o.ids}));
            }
        }
        Err(e) if token.is_cancelled() => return Err(e),
        Err(e) => {
            push(app, id, json!({"kind": "tool", "tool": tool, "ok": false, "text": e, "zh": format!("{zh}失敗：{e}")})).await?;
        }
    }
    Ok(())
}

/// 只寫詞的「填入生成頁」：助手視窗按了 → 叫主視窗把提示詞（和負面詞、尺寸）填進生成頁，其他欄位不動
pub async fn fill(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let prompt = tx(&b["prompt"]);
    if prompt.trim().is_empty() {
        return Err(bad("沒有提示詞可以填"));
    }
    let mut f = json!({"prompt": prompt, "negative": b["negative"]});
    if let (Some(w), Some(h)) = (b["width"].as_i64(), b["height"].as_i64()) {
        f["width"] = json!(w);
        f["height"] = json!(h);
    }
    app.hub.send(json!({"type": "agent_ui", "op": "fill", "page": "gen", "fill": f}));
    ok(json!({"ok": true}))
}

pub async fn stop(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let t = app.agent.runs.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
    match t {
        Some(t) => {
            t.cancel();
            app.pe.cancel(&rid(&id));
            ok(json!({"ok": true}))
        }
        None => ok(json!({"ok": false})),
    }
}

// ================================================================ LoRA 目錄
/// 這個家族能用的 LoRA（ComfyUI 看得到的、不含加速 LoRA），收藏和認識的排前面。
/// 本機 ComfyUI 沒開（只用雲端時常見）就直接看 loras 資料夾，名字照 ComfyUI 的寫法（相對路徑、系統分隔字元）；
/// 資料夾也是空的就是沒有 LoRA（不能當成錯誤：雲端腦袋本來就不需要本機 ComfyUI，2026-10-20 RunPod 實測踩到）
async fn catalog(app: &Shared, family: &str) -> Result<Vec<LoraInfo>, String> {
    let names = match app.comfy.options("LoraLoaderModelOnly", "lora_name").await {
        Ok(n) => n,
        Err(_) => {
            let roots = app.paths.lora_roots();
            let found: Vec<Value> = blocking(move || {
                let mut v: Vec<Value> = vec![];
                for root in &roots {
                    for p in crate::setup::files_under(root) {
                        if !p.extension().and_then(|x| x.to_str()).is_some_and(|x| ["safetensors", "ckpt", "pt", "pth", "bin"].contains(&x.to_ascii_lowercase().as_str())) {
                            continue;
                        }
                        if let Some(n) = p.strip_prefix(root).ok().map(|r| json!(r.to_string_lossy())) {
                            if !v.contains(&n) {
                                v.push(n);
                            }
                        }
                    }
                }
                v
            })
            .await;
            found
        }
    };
    let meta = app.db.lora_meta().map_err(|e| e.to_string())?;
    let (roots, fam) = (app.paths.lora_roots(), family.to_string());
    let mut list: Vec<LoraInfo> = blocking(move || {
        names
            .iter()
            .filter_map(Value::as_str)
            .filter(|n| !graphs::is_turbo_lora(n) && families::lora_family(&roots, n, meta.get(*n)) == fam)
            .map(|n| LoraInfo::from_meta(n, meta.get(n).unwrap_or(&Value::Null)))
            .collect()
    })
    .await;
    list.sort_by(|a, b| b.fav.cmp(&a.fav).then(b.desc.is_empty().cmp(&a.desc.is_empty()).reverse()).then(a.name.cmp(&b.name)));
    Ok(list)
}

/// 生成頁固定掛的 LoRA（強度照生成頁）
fn fixed_loras(base: &Map<String, Value>, catalog: &[LoraInfo]) -> Vec<LoraInfo> {
    base.get("loras")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let name = tx(&l["name"]);
            let mut info = catalog.iter().find(|c| c.name == name).cloned()?;
            if let Some(s) = l["strength"].as_f64() {
                info.strength = s;
            }
            Some(info)
        })
        .collect()
}

#[derive(serde::Deserialize)]
pub struct FamQ {
    family: Option<String>,
}

/// 網頁列表：{loras: [{name, title, triggers, desc, desc_zh, kind, src, sure, known, fav, pickable, preview}], learning}
pub async fn loras(State(app): State<Shared>, Query(q): Query<FamQ>) -> ApiResult {
    let fam = q.family.unwrap_or_else(|| families::MOSS.to_string());
    let list = catalog(&app, &fam).await.map_err(bad)?;
    let meta = app.db.lora_meta()?;
    let out: Vec<Value> = list
        .iter()
        .map(|l| {
            let a = meta.get(&l.name).map(|m| m["agent"].clone()).unwrap_or(Value::Null);
            json!({"name": l.name, "title": l.title, "triggers": l.triggers, "desc": tx(&a["desc"]), "desc_zh": tx(&a["desc_zh"]),
                   "kind": a["kind"], "src": a["src"], "sure": a["sure"], "known": !tx(&a["desc"]).is_empty(), "fav": l.fav, "pickable": l.agent_may_pick(),
                   "preview": meta.get(&l.name).map(|m| m["preview"].clone()).unwrap_or(Value::Null)})
        })
        .collect();
    let learning = app.agent.learn.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    ok(json!({"loras": out, "learning": learning}))
}

// ================================================================ 背景工作：想 → 畫 → 看
type Step = Result<(), String>;

/// 退回說明（英文，給模型）→ 給使用者看的一句中文
fn reject_zh(fb: &str) -> &'static str {
    if fb.starts_with("Your prompt omitted") {
        "提示詞漏掉指定文字，退回請它原樣補齊"
    } else if fb.starts_with("Unknown tool") {
        "工具名稱寫錯，退回"
    } else if fb.starts_with("Not allowed") {
        "這件事助手不能做（要你自己按），退回"
    } else if fb.starts_with("Redrawing with refs keeps") {
        "圖生圖轉不了角度，請它改用「角色多視圖」"
    } else if fb.starts_with("Redrawing cannot change only the pose") {
        "圖生圖改不了姿勢（只會畫成另一個人），請它改用「改圖」直接改你的圖"
    } else if fb.starts_with("Your previous round built on user image") {
        "它這輪不用你的圖、要照文字重畫一張新的，提醒它回到你的圖上改（確定要新圖就放行）"
    } else if fb.contains("tool calls for this request") {
        "工具叫太多次了，請它收尾"
    } else if fb.contains(" needs \"") || fb.contains("must be") && fb.contains("api") {
        "工具的參數寫錯，退回"
    } else if fb.contains("copied the writing") {
        "它把「怎麼寫提示詞」的說明抄進提示詞，退回重寫"
    } else if fb.contains("English") {
        "提示詞寫成中文，退回請它改寫成英文"
    } else if fb.starts_with("Fill \"check\"") {
        "它沒有逐項對照你的要求，退回請它先打勾"
    } else if fb.contains("identical") {
        "提示詞跟上一輪一模一樣，退回請它真的改"
    } else if fb.contains("Unknown LoRA") {
        "LoRA 名稱寫錯，退回"
    } else if fb.starts_with("You marked") {
        "它自己說還有地方不對卻想交卷，請它再修一輪"
    } else if fb.starts_with("You were shown") {
        "它看完圖就停下來問你，退回請它自己決定交卷或再修"
    } else if fb.starts_with("Everything in your check") {
        "它自己打勾全部都對卻還想重畫，請它直接交卷"
    } else if fb.starts_with("Your prompt names <image1>") {
        "提示詞點名了你的圖卻沒真的附上，退回請它補上"
    } else if fb.starts_with("Remove the <imageN>") {
        "這個模型吃不了圖，請它把 <image1> 換成文字描述"
    } else if fb.starts_with("You wrote that you use refs") {
        "它說要用你的圖，JSON 裡卻沒寫，退回請它補上"
    } else if fb.starts_with("The user attached") {
        "你附了圖它卻沒拿來用，提醒它一次（它確定只是參考就放行）"
    } else if fb.starts_with("No rounds") {
        "輪數用完了，請它交卷"
    } else if fb.contains("best") {
        "交卷挑的圖編號不對，退回"
    } else {
        "回覆格式不對，退回重寫"
    }
}

/// 使用者附的圖裡最新的幾張（給模型看）：(第幾張, image_url)
async fn user_parts(app: &Shared, all: &[Value]) -> Vec<(i64, Value)> {
    let pick: Vec<(i64, String)> = all
        .iter()
        .rev()
        .take(ag::SHOW_USER_IMAGES)
        .filter_map(|i| Some((i["n"].as_i64()?, tx(&i["name"]))))
        .collect();
    let input = app.paths.input.clone();
    blocking(move || {
        pick.into_iter()
            .rev()
            .filter_map(|(n, name)| match crate::pe::image_url_jpeg(&input.join(&name), 768) {
                Ok(part) => Some((n, part)),
                Err(e) => {
                    eprintln!("生圖助手看使用者的圖：{e}");
                    None
                }
            })
            .collect()
    })
    .await
}

/// 這次要給它看的圖（最新一輪）：(圖庫編號, image_url)
async fn look_images(app: &Shared, ids: Vec<i64>) -> Vec<(i64, Value)> {
    let a = app.clone();
    blocking(move || {
        ids.into_iter()
            .filter_map(|i| {
                let row = a.db.image(i).ok().flatten()?;
                match crate::pe::image_url_jpeg(&file_of(&a.paths, &row), 768) {
                    Ok(part) => Some((i, part)),
                    Err(e) => {
                        eprintln!("生圖助手看圖：{e}");
                        None
                    }
                }
            })
            .collect()
    })
    .await
}

async fn run(app: &Shared, id: &str, token: &CancellationToken) -> Step {
    let mut rejects = 0usize;
    let mut nudged = false;
    // 同一個「想」被退回時，退回的對話接在後面再問一次
    let mut extra: Vec<Value> = vec![];
    loop {
        if token.is_cancelled() {
            return Err("已停止".into());
        }
        let s = load(app, id).map_err(|e| e.1)?;
        let st = s["settings"].clone();
    crate::diagnostics::snapshot("assistant_settings", &st);
        let events = s["events"].as_array().cloned().unwrap_or_default();
        let words = st["mode"] == "prompt";
        let base = match st["base"].as_object().cloned() {
            Some(base) => base,
            None if words => Map::new(),
            None => return Err("還沒選生圖模型：按「用生成頁目前的設定」".into()),
        };
        let model = base.get("model").map(tx).unwrap_or_default();
        let family = if words && model.is_empty() { ANIMA } else { families::family_of(&app.paths.unet_roots(), &model) };
        let target = tx(&st["target"]);
        let family = if words { ag::prompt_family(&target, family) } else { family };
        let cat = catalog(app, family).await?;
        let fixed = fixed_loras(&base, &cat);
        let allow = truthy(st.get("loras"));
        let ask = ag::request_text(&events);
        // 工作說明每一輪重讀：設定頁改了下一輪就生效。這個家族接了外掛寫法：寫法後面接照這次請求挑的段＋目錄
        let mut guide = app.pe.agent_text(ag::guide_id(family));
        if let Some(s) = crate::skills::for_guide(&app.paths.data, &app.paths.settings, ag::family_guide(family)) {
            guide += "\n\n";
            guide += &app.pe.embed.agent_block(&s, ag::family_guide(family), &ask).await;
        }
        let system = ag::system_prompt(&app.pe.agent_text("agent"), &guide, family, &ag::loras_text(&cat, &fixed, allow));
        let (rounds, count) = (st["rounds"].as_i64().unwrap_or(3), st["count"].as_i64().unwrap_or(2));
        let left = ag::rounds_left(&events, rounds, truthy(st.get("auto")));
        let shown: Vec<i64> = ag::latest_result(&events)
            .map(|e| e["ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        let look = look_images(app, shown).await;
        let images = ag::request_images(&events);
        let last = ag::last_generate(&events).cloned();
        let brain = tx(&st["brain"]);
        let think = ag::think_level(&st["think"]);
        // 使用者附的圖：模型看得到最新幾張；本機的 Qwen 2.1（多圖參考）／Anima、MOSS（圖生圖一張）還能把圖餵給生圖模型（雲端只畫文字）
        let uall = ag::user_images(&events);
        let uids: Vec<i64> = uall.iter().filter_map(|i| i["n"].as_i64()).collect();
        // 附了圖卻沒用 refs：只在這個請求的第一輪提醒（它說只是參考，之後就不再囉嗦）
        let req_imgs = if ag::rounds_used(&events) == 0 { ag::request_user_images(&events) } else { vec![] };
        let refs_mode = match (brain == "cloud", family) {
            (false, f) if f == QWEN21 => "multi",
            (false, f) if f == ANIMA || f == MOSS => "one",
            _ => "",
        };
        let system = if uall.is_empty() { system } else { ag::with_images(&system, refs_mode) };
        // 用 look 看的圖不是它畫的：不要求逐項打勾、可以直接回話
        let looked_only = ag::latest_result(&events).is_some_and(|e| e["tool"] == "look");
        let uparts = user_parts(app, &uall).await;
        let remote = if brain == "cloud" {
            Some(app.agent.cloud.endpoints().ok_or("雲端斷線了：到「雲端」那裡看狀態")?.remote())
        } else {
            None
        };
        // 選了 35B 但機器上只有 9B：model_files 會默默用 9B，這裡講出來（同一段對話只講一次）
        if remote.is_none() && !events.iter().any(|e| ag::ev_kind(e) == "note" && tx(&e["text"]).starts_with(NO_35B)) {
            let a = app.clone();
            let b = brain.clone();
            if let Some(m) = blocking(move || a.pe.agent_model(&b)).await.filter(|m| crate::pe::brain_fell_back(&brain, m)) {
                note(app, id, format!("{NO_35B}這台電腦找不到 Qwen3.6-35B-A3B 的模型檔，這次實際用的是 {m}（9B）。要用 35B 得先下載它的 gguf 和 mmproj。")).await;
            }
        }
        // 只寫詞：照挑好的那份寫法寫一張提示詞卡就停（不畫圖、不叫工具）
        if st["mode"] == "prompt" {
            let target = tx(&st["target"]);
            let guide = ag::GUIDES.iter().find(|g| g.0 == target).map(|g| g.0).unwrap_or_else(|| ag::pick_guide(family, uall.len(), &ask));
            let mut gtext = app.pe.agent_text(ag::guide_text_id(guide).unwrap_or("agent_other"));
            // 只寫詞不能叫工具：外掛寫法照請求挑段一次給
            let picked = match crate::skills::for_guide(&app.paths.data, &app.paths.settings, guide) {
                Some(s) => app.pe.embed.pick_block(&s, &ask, crate::skills::PICK_WRITE).await,
                None => String::new(),
            };
            if !picked.is_empty() {
                gtext += "\n\n";
                gtext += &picked;
            }
            let system = ag::prompt_system(&app.pe.agent_text("agent_prompt"), &gtext, guide, family);
            let mut msgs = ag::build_prompt_messages(&system, &events, &uparts);
            if let Some(a) = msgs.as_array_mut() {
                a.extend(extra.iter().cloned());
            }
            emit(app, id, "think", format!("照「{}」的寫法寫提示詞…", ag::guide_zh(guide)));
            let t0 = Instant::now();
            app.agent.cloud.touch();
            let (content, reasoning) = app
                .pe
                .agent_chat(&rid(id), &msgs, ag::think_max_tokens(think), 0.7, ag::think_budget(think), &brain, remote.as_ref())
                .await?;
            app.agent.cloud.touch();
            let secs = t0.elapsed().as_secs_f64();
            let parsed = ag::parse_prompt_action(&content, family, guide).and_then(|a| {
                if a.get("action").and_then(Value::as_str) == Some("prompt") {
                    let prompt = a.get("prompt").map(tx).unwrap_or_default();
                    let missing = crate::pe::missing_visible_text(&prompt, &ask);
                    if !missing.is_empty() {
                        return Err(format!("Your prompt omitted these exact strings to display in the image: {}. Include every string verbatim, in quotes, at its requested position. Give the whole corrected prompt.", serde_json::to_string(&missing).unwrap_or_default()));
                    }
                }
                Ok(a)
            });
            match parsed {
                Ok(action) => {
                    let thought: String = reasoning.chars().take(6000).collect();
                    push(app, id, json!({"kind": "act", "action": action, "secs": secs, "thought": thought})).await?;
                    return Ok(());
                }
                Err(fb) => {
                    rejects += 1;
                    eprintln!("生圖助手 {id} 只寫詞退回（{rejects}）：{fb}");
                    if rejects > ag::MAX_REJECTS {
                        return Err(format!("助手連續 {rejects} 次寫不出能用的回覆，先停下來。可以換一顆腦袋或把要求說得更簡單再試一次。"));
                    }
                    note(app, id, reject_zh(&fb)).await;
                    extra.push(json!({"role": "assistant", "content": content}));
                    extra.push(json!({"role": "user", "content": format!("Rejected: {fb}")}));
                    continue;
                }
            }
        }
        let mut msgs = ag::build_messages_with(&system, &events, &look, &uparts, left, count);
        if let Some(a) = msgs.as_array_mut() {
            a.extend(extra.iter().cloned());
        }
        emit(app, id, "think", if look.is_empty() { "想提示詞…" } else { "看圖、逐項對照中…" });
        let t0 = Instant::now();
        app.agent.cloud.touch();
        let (content, reasoning) = app
            .pe
            .agent_chat(&rid(id), &msgs, ag::think_max_tokens(think), 0.7, ag::think_budget(think), &brain, remote.as_ref())
            .await?;
        app.agent.cloud.touch();
        let secs = t0.elapsed().as_secs_f64();
        let check = ag::Check {
            family,
            rounds_left: left,
            last: last.as_ref(),
            saw_images: !look.is_empty() && !looked_only,
            images: &images,
            catalog: &cat,
            fixed: &fixed,
            allow_loras: allow,
            nudged,
            user_images: &uids,
            request_user_images: &req_imgs,
            refs_mode,
            ask: &ask,
            calls_left: at::MAX_CALLS - ag::calls_used(&events),
        };
        let action = match ag::parse_action(&content, &check) {
            Ok(a) => {
                if cfg!(debug_assertions) {
                    eprintln!("生圖助手 {id} 原文：{}", content.chars().take(800).collect::<String>());
                }
                a
            }
            Err(fb) => {
                rejects += 1;
                eprintln!("生圖助手 {id} 退回（{rejects}）：{fb}｜輸出開頭：{}", content.chars().take(if cfg!(debug_assertions) { 3000 } else { 200 }).collect::<String>());
                if rejects > ag::MAX_REJECTS {
                    return Err(format!("助手連續 {rejects} 次寫不出能用的回覆，先停下來。可以換一顆腦袋或把要求說得更簡單再試一次。"));
                }
                note(app, id, reject_zh(&fb)).await;
                if fb.starts_with("You marked") || fb.starts_with("You were shown") || fb.starts_with("Everything in your check") || fb.starts_with("The user attached")
                    || fb.starts_with("Your previous round built on user image")
                {
                    nudged = true;
                }
                extra.push(json!({"role": "assistant", "content": content}));
                extra.push(json!({"role": "user", "content": format!("Rejected: {fb}")}));
                continue;
            }
        };
        // 改圖、影片、Qwen 用附圖畫：這段對話還沒讀過那份寫法 → 先把寫法給它看，讓它照著重寫（這次的動作不算）
        if let Some(g) = ag::needed_guide(&action, family, refs_mode).filter(|g| !ag::guide_read(&events, g)) {
            note(app, id, format!("先讀「{}」的寫法，照著重寫", ag::guide_zh(g))).await;
            run_tool(app, id, "guide", &json!({"name": g}), token).await?;
            extra.clear();
            continue;
        }
        extra.clear();
        rejects = 0;
        nudged = false;
        let thought: String = reasoning.chars().take(6000).collect();
        let kind = tx(&action["action"]);
        match kind.as_str() {
            "generate" => {
                let round = events.iter().filter(|e| ag::ev_kind(e) == "result").count() as i64 + 1;
                // 觸發詞、成人內容的安全處理（跟導演台同一套），實際送出的提示詞另外記
                let chosen: Vec<LoraInfo> = action["loras"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| cat.iter().find(|c| c.name == tx(&l["name"])).cloned())
                    .collect();
                let refs: Vec<&LoraInfo> = fixed.iter().chain(chosen.iter()).collect();
                let (mut used, added) = ag::add_triggers(&tx(&action["prompt"]), &refs, family);
                used = crate::director::safe_text(&used, family == ANIMA);
                // 使用者講的是體位庫裡的體位：程式保證那個體位真的寫進去（2026-10-28 擂台：沒有這步 9B 46 題全錯）
                let (posed, pose_msg) = ag::pose_fix(&app.pe.poses, &events, family, &used);
                used = posed;
                if family == ANIMA {
                    used = crate::pe::fix_anima_tags(&used);
                }
                push(app, id, json!({"kind": "act", "action": action, "round": round, "secs": secs, "thought": thought, "used_prompt": used})).await?;
                if !added.is_empty() {
                    note(app, id, format!("自動補上 LoRA 觸發詞：{}", added.join(", "))).await;
                }
                if let Some(m) = pose_msg {
                    note(app, id, m).await;
                }
                let ask = events
                    .iter()
                    .rev()
                    .find(|e| ag::ev_kind(e) == "user" && !truthy(e.get("mid")))
                    .map(|e| tx(&e["text"]))
                    .unwrap_or_default();
                let mut q = base.clone();
                for k in ["init_image", "refs", "mask", "inpaint", "upscale", "control", "idea", "pe_prompt", "prompt_resolved", "size_mode", "pixels", "ref_pixels", "denoise", "family", "hires"] {
                    q.remove(k);
                }
                let mut loras: Vec<Value> = fixed.iter().map(|l| json!({"name": l.name, "strength": l.strength})).collect();
                loras.extend(action["loras"].as_array().cloned().unwrap_or_default());
                q.insert("mode".into(), json!("txt2img"));
                q.insert("prompt".into(), json!(used));
                q.insert("width".into(), action["width"].clone());
                q.insert("height".into(), action["height"].clone());
                q.insert("loras".into(), json!(loras));
                q.insert("seed".into(), json!(rand::random_range(0..=(1i64 << 48))));
                q.insert("idea".into(), json!(format!("助手：{}", ask.chars().take(200).collect::<String>())));
                q.insert("agent".into(), json!({"id": id, "round": round}));
                // 用使用者的圖畫：Qwen 2.1＝多圖參考（照它給的長寬）、Anima／MOSS＝圖生圖（照原圖比例）
                let chosen_refs: Vec<Value> = action["refs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| uall.iter().find(|i| i["n"] == *n).cloned())
                    .collect();
                if !chosen_refs.is_empty() {
                    if refs_mode == "one" {
                        q.insert("mode".into(), json!("img2img"));
                        q.insert("init_image".into(), chosen_refs[0]["name"].clone());
                        q.insert("denoise".into(), action["denoise"].clone());
                        q.insert("size_mode".into(), json!("auto"));
                    } else {
                        let list: Vec<Value> = chosen_refs
                            .iter()
                            .enumerate()
                            .map(|(k, r)| json!({"image": r["name"], "role": if k == 0 { "base" } else { "object" }, "width": r["width"], "height": r["height"]}))
                            .collect();
                        q.insert("mode".into(), json!("multiref"));
                        q.insert("refs".into(), json!(list));
                        // 改一張圖：跟著原圖比例（不然正方形會把構圖裁掉）；合成多張：照它給的長寬
                        q.insert("size_mode".into(), json!(if list.len() == 1 { "auto" } else { "fixed" }));
                        q.insert("pixels".into(), json!(1024 * 1024));
                    }
                }
                emit(app, id, "render", format!("畫第 {round} 輪（{count} 張）…"));
                let t1 = Instant::now();
                let (ids, errors) = if brain == "cloud" {
                    api_cloud::render(app, q, count, token).await?
                } else {
                    render_local(app, q, count, token).await?
                };
                push(app, id, json!({"kind": "result", "round": round, "ids": ids, "errors": errors, "secs": t1.elapsed().as_secs_f64()})).await?;
                if ids.is_empty() {
                    return Err(format!("第 {round} 輪一張圖都沒畫出來：{}", errors.join("；")));
                }
            }
            "call" => {
                let tool = tx(&action["tool"]);
                let args = action["args"].clone();
                let why = action.get("confirm").map(tx);
                push(app, id, json!({"kind": "act", "action": action, "secs": secs, "thought": thought})).await?;
                if let Some(why) = why {
                    // 花錢、刪東西、改設定：停下來等使用者在助手視窗按准（POST /api/agent/confirm）
                    push(app, id, json!({"kind": "confirm", "call": {"tool": tool, "args": args}, "why": why, "state": "wait"})).await?;
                    return Ok(());
                }
                run_tool(app, id, &tool, &args, token).await?;
            }
            _ => {
                // finish／reply：交卷或回話，等使用者下一句
                push(app, id, json!({"kind": "act", "action": action, "secs": secs, "thought": thought})).await?;
                return Ok(());
            }
        }
    }
}

/// 本機：送生成、等全部跑完、回傳圖庫編號和錯誤
pub(super) async fn render_local(app: &Shared, q: Map<String, Value>, count: i64, token: &CancellationToken) -> Result<(Vec<i64>, Vec<String>), String> {
    let jobs = submit_jobs(app, q, count).await.map_err(|e| e.1)?;
    let pids: Vec<String> = jobs.iter().map(|j| tx(&j["job"])).collect();
    let errors = wait_all(app, &pids, token).await?;
    let mut ids = vec![];
    for pid in &pids {
        let rows = app
            .db
            .q("SELECT id FROM images WHERE job=? AND deleted=0 ORDER BY id", [pid])
            .map_err(|e| e.to_string())?;
        ids.extend(rows.iter().filter_map(|r| r["id"].as_i64()));
    }
    Ok((ids, errors))
}

/// 等這幾個 ComfyUI 任務跑完；按停止就把還沒跑的從佇列拿掉、正在跑的中斷
pub(super) async fn wait_all(app: &Shared, pids: &[String], token: &CancellationToken) -> Result<Vec<String>, String> {
    let mut errors = vec![];
    let mut pending: Vec<String> = pids.to_vec();
    while !pending.is_empty() {
        if token.is_cancelled() {
            let running = app.comfy.live().running;
            let queued: Vec<&String> = pending.iter().filter(|p| running.as_deref() != Some(p.as_str())).collect();
            if !queued.is_empty() {
                let _ = app.comfy.post("/queue", &json!({"delete": queued})).await;
                for p in &queued {
                    let _ = app.db.set_job(p, "cancelled", None);
                }
            }
            if running.as_ref().is_some_and(|r| pending.contains(r)) {
                let _ = app.comfy.post("/interrupt", &json!({})).await;
            }
            return Err("已停止".into());
        }
        let mut still = vec![];
        for pid in &pending {
            let job = app.db.job(pid).map_err(|e| e.to_string())?;
            match job.as_ref().and_then(|j| j["status"].as_str()) {
                Some("done") => {}
                Some("queued" | "running") => still.push(pid.clone()),
                Some(s) => errors.push(format!("任務沒有完成（{s}）{}", job.as_ref().map(|j| tx(&j["error"])).unwrap_or_default())),
                None => errors.push("任務紀錄不見了".into()),
            }
        }
        pending = still;
        if !pending.is_empty() {
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }
    }
    Ok(errors)
}

// ================================================================ 認識 LoRA
fn emit_learn(app: &Shared, phase: &str, msg: impl Into<String>, done: usize, total: usize) {
    app.hub.send(json!({"type": "agent_lora", "phase": phase, "msg": msg.into(), "done": done, "total": total}));
}

/// 存進 lora_meta 的 agent 欄位（其他欄位不動）
fn save_desc(app: &Shared, name: &str, desc: Value, preview: Option<String>) -> Result<(), String> {
    let all = app.db.lora_meta().map_err(|e| e.to_string())?;
    let mut m = all.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
    m.insert("agent".into(), desc);
    if let Some(p) = preview {
        if tx(m.get("preview").unwrap_or(&Value::Null)).is_empty() {
            m.insert("preview".into(), json!(p));
        }
    }
    app.db.set_lora_meta(name, &Value::Object(m)).map_err(|e| e.to_string())
}

/// 沒有預覽圖的 LoRA：試畫的圖縮成 512 存成預覽（LoRA 頁看得到長什麼樣）
fn make_preview(app: &Shared, src: &Path, info: &LoraInfo) -> Option<String> {
    let safe: String = info.stem().chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    let name = format!("agent_{safe}.jpg");
    let im = image::open(src).ok()?.thumbnail(512, 512).to_rgb8();
    std::fs::create_dir_all(&app.paths.previews).ok()?;
    im.save(app.paths.previews.join(&name)).ok()?;
    Some(name)
}

/// {base（生成頁設定，決定家族和怎麼試畫）, brain?（9b／35b）, names?（只做這幾個）, redo?（已經認識的也重做）, probe?（線索不夠就試畫比較，預設要）}
pub async fn learn(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let Some(base) = b["base"].as_object().cloned() else {
        return Err(bad("還沒選生圖模型：按「用生成頁目前的設定」"));
    };
    let family = families::family_of(&app.paths.unet_roots(), &tx(&base["model"]));
    if family == families::OTHER {
        return Err(bad("目前的生圖模型認不出是 Anima、Qwen 2.1 還是 MOSS-Image"));
    }
    let cat = catalog(&app, family).await.map_err(bad)?;
    let meta = app.db.lora_meta()?;
    let names: Vec<String> = b["names"].as_array().into_iter().flatten().map(tx).collect();
    let redo = truthy(b.get("redo"));
    // 調年齡的 LoRA 助手不會自己挑，也就不用認識（不拿它試畫）
    let targets: Vec<LoraInfo> = cat
        .into_iter()
        .filter(|l| l.agent_may_pick())
        .filter(|l| names.is_empty() || names.contains(&l.name))
        .filter(|l| redo || !names.is_empty() || tx(&meta.get(&l.name).map(|m| m["agent"]["desc"].clone()).unwrap_or(Value::Null)).is_empty())
        .collect();
    if targets.is_empty() {
        return ok(json!({"ok": true, "total": 0}));
    }
    let token = {
        let mut g = app.agent.learn.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_some() {
            return Err(bad("已經在認識 LoRA 了"));
        }
        let t = CancellationToken::new();
        *g = Some(t.clone());
        t
    };
    let brain = if b["brain"] == "35b" { "35b" } else { "9b" }.to_string();
    let probe = b.get("probe").map(|v| truthy(Some(v))).unwrap_or(true);
    let total = targets.len();
    let a = app.clone();
    crate::diagnostics::spawn(async move {
        let r = learn_run(&a, targets, meta, base, family, &brain, probe, &token).await;
        *a.agent.learn.lock().unwrap_or_else(|e| e.into_inner()) = None;
        match r {
            Ok(n) => emit_learn(&a, "end", format!("認識了 {n} 個 LoRA"), n, total),
            Err(_) if token.is_cancelled() => emit_learn(&a, "end", "已停止", 0, total),
            Err(e) => {
                eprintln!("認識 LoRA：{e}");
                emit_learn(&a, "error", e, 0, total)
            }
        }
    });
    ok(json!({"ok": true, "total": total}))
}

pub async fn learn_stop(State(app): State<Shared>) -> ApiResult {
    let t = app.agent.learn.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match t {
        Some(t) => {
            t.cancel();
            app.pe.cancel("agent-learn");
            ok(json!({"ok": true}))
        }
        None => ok(json!({"ok": false})),
    }
}

#[allow(clippy::too_many_arguments)]
async fn learn_run(
    app: &Shared,
    targets: Vec<LoraInfo>,
    meta: Map<String, Value>,
    base: Map<String, Value>,
    family: &'static str,
    brain: &str,
    probe: bool,
    token: &CancellationToken,
) -> Result<usize, String> {
    let total = targets.len();
    let mut done = 0usize;
    // 每個 LoRA 的線索（檔頭、備註、Civitai 標籤）
    let roots = app.paths.lora_roots();
    let t2 = targets.clone();
    let clues: Vec<Vec<String>> = blocking(move || {
        t2.iter()
            .map(|l| families::resolve(&roots, &l.name).map(|p| ag::header_clues(&ag::header_meta(&p))).unwrap_or_default())
            .collect()
    })
    .await;
    let note_of = |l: &LoraInfo| meta.get(&l.name).map(|m| tx(&m["note"])).unwrap_or_default();
    let tags_of = |l: &LoraInfo| -> Vec<String> { meta.get(&l.name).and_then(|m| m["tags"].as_array().cloned()).unwrap_or_default().iter().map(tx).collect() };
    let (mut by_text, mut by_probe) = (vec![], vec![]);
    for (l, c) in targets.iter().zip(clues.iter()) {
        if !probe || ag::clues_enough(&note_of(l), &tags_of(l), c) {
            by_text.push((l, c));
        } else {
            by_probe.push((l, c));
        }
    }
    // 一、線索夠：只讀文字（不用畫圖）
    for &(l, c) in &by_text {
        if token.is_cancelled() {
            return Err("已停止".into());
        }
        emit_learn(app, "progress", format!("讀線索：{}", l.stem()), done, total);
        let msgs = json!([{"role": "system", "content": ag::SYSTEM_LORA_META},
                          {"role": "user", "content": ag::lora_clues_text(l, &note_of(l), &tags_of(l), c)}]);
        let (r, _) = app.pe.agent_chat("agent-learn", &msgs, 500, 0.3, None, brain, None).await?;
        match ag::parse_lora_desc(&r, "meta") {
            Some(d) => save_desc(app, &l.name, d, None)?,
            None => eprintln!("認識 LoRA {}：讀不出說明：{}", l.name, r.chars().take(200).collect::<String>()),
        }
        done += 1;
    }
    if by_probe.is_empty() {
        return Ok(done);
    }
    // 二、線索不夠：同一個種子「不掛／掛」各畫一張，再請它比較。先全部畫完再載入腦袋（顯卡放不下兩邊）
    let seed = 20261013i64;
    let probe_q = |prompt: &str, loras: Value, idea: String| {
        let mut q = base.clone();
        for k in ["init_image", "refs", "mask", "inpaint", "upscale", "control", "idea", "pe_prompt", "prompt_resolved", "size_mode", "pixels", "ref_pixels", "denoise", "family", "hires"] {
            q.remove(k);
        }
        q.insert("mode".into(), json!("txt2img"));
        q.insert("prompt".into(), json!(prompt));
        q.insert("width".into(), json!(768));
        q.insert("height".into(), json!(1024));
        q.insert("seed".into(), json!(seed));
        q.insert("loras".into(), loras);
        q.insert("idea".into(), json!(idea));
        q
    };
    let p0 = ag::probe_prompt(family);
    emit_learn(app, "progress", format!("試畫：不掛 LoRA 的對照圖＋{} 張", by_probe.len()), done, total);
    let base_job = submit_jobs(app, probe_q(p0, json!([]), "助手認識 LoRA：對照（不掛）".into()), 1).await.map_err(|e| e.1)?;
    let mut pids = vec![tx(&base_job[0]["job"])];
    for &(l, _) in &by_probe {
        let (p, _) = ag::add_triggers(p0, &[l], family);
        let j = submit_jobs(app, probe_q(&p, json!([{"name": l.name, "strength": l.strength}]), format!("助手認識 LoRA：{}", l.stem())), 1)
            .await
            .map_err(|e| e.1)?;
        pids.push(tx(&j[0]["job"]));
    }
    let errs = wait_all(app, &pids, token).await?;
    if !errs.is_empty() {
        eprintln!("認識 LoRA 試畫：{}", errs.join("；"));
    }
    let first_file = |pid: &str| -> Option<std::path::PathBuf> {
        let row = app.db.q1("SELECT * FROM images WHERE job=? AND deleted=0 ORDER BY id LIMIT 1", [pid]).ok().flatten()?;
        Some(file_of(&app.paths, &row))
    };
    let a_file = first_file(&pids[0]).ok_or("對照圖沒有畫出來")?;
    let a_part = crate::pe::image_url_jpeg(&a_file, 640)?;
    for (i, &(l, c)) in by_probe.iter().enumerate() {
        if token.is_cancelled() {
            return Err("已停止".into());
        }
        emit_learn(app, "progress", format!("比較：{}", l.stem()), done, total);
        let Some(b_file) = first_file(&pids[i + 1]) else {
            done += 1;
            continue;
        };
        let b_part = crate::pe::image_url_jpeg(&b_file, 640)?;
        let content = json!([
            {"type": "text", "text": "Image A (WITHOUT the LoRA):"}, a_part.clone(),
            {"type": "text", "text": "Image B (WITH the LoRA):"}, b_part,
            {"type": "text", "text": ag::lora_clues_text(l, &note_of(l), &tags_of(l), c)},
        ]);
        let msgs = json!([{"role": "system", "content": ag::SYSTEM_LORA_PROBE}, {"role": "user", "content": content}]);
        let (r, _) = app.pe.agent_chat("agent-learn", &msgs, 500, 0.3, None, brain, None).await?;
        match ag::parse_lora_desc(&r, "probe") {
            Some(d) => {
                let (a2, l2, f2) = (app.clone(), l.clone(), b_file.clone());
                let preview = blocking(move || make_preview(&a2, &f2, &l2)).await;
                save_desc(app, &l.name, d, preview)?
            }
            None => eprintln!("認識 LoRA {}：讀不出說明：{}", l.name, r.chars().take(200).collect::<String>()),
        }
        done += 1;
    }
    Ok(done)
}

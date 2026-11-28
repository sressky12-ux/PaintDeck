//! H3 長片 API（2026-10-23）：導演頁「長片」分頁。一條長片好幾段，雲端或本機（2026-11-13 起）一段一段拍，第 2 段起吃上一段的 AV latent 接著拍。
//! 存檔格式、提示詞、節點圖在 crate::h3chain；雲端送圖、上傳下載在 api_cloud.rs；檢查跳點用拆幀台（studio::Frames）。
//!
//! - GET  /api/h3/chains：清單
//! - GET  /api/h3/chain/{id}：一條（加 est＝還沒拍的段約要算幾秒、running）
//! - POST /api/h3/chain/save {chain}：存（沒 id 就新建）；拍攝中不能存
//! - POST /api/h3/chain/delete {id}：搬到 待刪除\H3長片\
//! - POST /api/h3/chain/run {id, from?}：從第一個還沒拍好的段開始拍（from＝第幾段起重拍，0 起算，後面的段一起作廢），全部拍好自動接成一支進圖庫
//! - POST /api/h3/chain/stop {id}
//! - POST /api/h3/chain/join {id}：再接一次
//! - GET  /api/h3/chain/{id}/file/{name}：segNN_cut.mp4／segNN_end.mp4／final.mp4
//!
//! 進度：websocket {"type":"h3chain","id","seg":段號(0 起算)|null,"state":"running|seg_done|done|error|stopped","msg"}，網頁收到就重讀這條
use super::{bad, blocking, file_response, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::director::tx;
use crate::h3chain::{self as hc, Store};
use crate::studio;
use axum::extract::{Path as UrlPath, State};
use serde_json::{json, Value};
use crate::{film_takes as takes, h3_takes as ht};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// 長片存檔「讀 → 改 → 寫」排隊（網頁存檔和背景工作會同時寫）
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn store(app: &Shared) -> Store {
    Store::new(&app.paths.data)
}

/// 登記在 app.h3 的工作名稱（雲端閒置自動關機會看 app.h3 有沒有工作在跑）
fn task_of(id: &str) -> String {
    format!("chain-{id}")
}

fn emit(app: &Shared, id: &str, seg: Option<usize>, state: &str, msg: impl Into<String>) {
    app.hub.send(json!({"type": "h3chain", "id": id, "seg": seg, "state": state, "msg": msg.into()}));
}

async fn update(app: &Shared, id: &str, f: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let _g = LOCK.lock().await;
    let mut c = store(app).get(id).ok_or("找不到這條長片")?;
    f(&mut c);
    store(app).save(&c)
}

async fn update_seg(app: &Shared, id: &str, i: usize, f: impl FnOnce(&mut Value)) -> Result<Value, String> {
    update(app, id, |c| {
        if let Some(s) = c["segments"].get_mut(i) {
            f(s);
        }
    })
    .await
}

fn load(app: &Shared, id: &str) -> Result<Value, ApiError> {
    store(app).get(id).ok_or_else(|| not_found("找不到這條長片"))
}

fn with_extra(app: &Shared, mut c: Value) -> Value {
    c["est"] = json!(hc::est_secs(&c));
    c["running"] = json!(app.h3.has(&task_of(&tx(&c["id"]))));
    c["size_locked"] = json!(hc::size_locked(&c));
    c
}

pub async fn list(State(app): State<Shared>) -> ApiResult {
    ok(json!({"chains": store(&app).list()}))
}

pub async fn get(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let _g = LOCK.lock().await;
    let mut c = load(&app, &id)?;
    if !app.h3.has(&task_of(&id)) {
        let before = c.clone();
        let shape = c["size"].clone();
        for seg in c["segments"].as_array_mut().into_iter().flatten() {
            if seg["status"] == "running" {
                seg["status"] = json!("stopped");
                seg["error"] = json!("上次拍到一半就中斷了（程式關掉或當掉）：再按一次開拍");
            }
            takes::ensure_legacy(seg, hc::RESULT_KEYS);
            let selected = tx(&seg["selected_take"]);
            for take in seg["takes"].as_array_mut().into_iter().flatten() {
                if take["id"] == selected && take["recipe"]["legacy"] == true && take["recipe"]["size"].is_null() && !shape.is_null() {
                    take["recipe"]["size"] = shape.clone();
                }
            }
        }
        if c != before { c = store(&app).save(&c).map_err(internal)?; }
    }
    ok(with_extra(&app, c))
}

/// {chain}：網頁改的欄位（共用設定、每段的文字、台詞、尾幀）。每段拍好的結果（狀態、檔案、檢查）以存檔裡的為準，網頁送的不算
pub async fn save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let mut c = hc::normalize(&b["chain"]).map_err(bad)?;
    let id = tx(&c["id"]);
    let _g = LOCK.lock().await;
    if !id.is_empty() {
        if app.h3.has(&task_of(&id)) {
            return Err(bad("拍攝中不能改：先按「停」"));
        }
        if let Some(old) = store(&app).get(&id) {
            hc::check_edit(&old, &c).map_err(bad)?;
            for (i, s) in c["segments"].as_array_mut().into_iter().flatten().enumerate() {
                for k in hc::RESULT_KEYS.iter().copied().chain(["takes", "selected_take", "pending_take"]) {
                    s[k] = old["segments"][i].get(k).cloned().unwrap_or(Value::Null);
                }
                if s["status"].is_null() {
                    s["status"] = json!("todo");
                }
            }
            for k in ["created", "size", "final"] {
                c[k] = old[k].clone();
            }
            let global_changed = ["first", "ref", "refs", "look", "scene", "style", "voice", "sound", "sheet", "aspect", "quality"].iter().any(|k| old[*k] != c[*k]);
            let changed = if global_changed { Some(0) } else {
                c["segments"].as_array().into_iter().flatten().enumerate().find_map(|(i,seg)| {
                    old["segments"].get(i).and_then(|prev| ["secs","text","lines","last","refs","entry","exit","inherit_entry"].iter().any(|k| prev[*k]!=seg[*k]).then_some(i))
                })
            };
            if let Some(i) = changed {
                let dir = store(&app).seg_dir(&id);
                for seg in c["segments"].as_array_mut().into_iter().flatten() { ht::preserve(seg,&dir).map_err(bad)?; }
                hc::reset_from(&mut c,i);
            }

        }
    }
    let saved = store(&app).save(&c).map_err(internal)?;
    ok(with_extra(&app, saved))
}

/// 選片只切換索引與工作 latent，不會重新呼叫生成服務。
pub async fn select_take(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let _g = LOCK.lock().await;
    if app.h3.has(&task_of(&id)) { return Err(bad("拍攝中不能切換版本")); }
    let mut c = load(&app, &id)?;
    let i = b["seg"].as_u64().map(|n| n as usize).filter(|i| c["segments"].get(*i).is_some()).ok_or_else(|| bad("沒有這一段"))?;
    let dir = store(&app).seg_dir(&id);
    for seg in c["segments"].as_array_mut().into_iter().flatten() { ht::preserve(seg, &dir).map_err(bad)?; }
    let changed = ht::select(&mut c, &dir, i, &tx(&b["take_id"])).map_err(bad)?;
    if changed { super::api_cloud::invalidate_output(&app, &hc::remote_dir(&id), &hc::latent_name(i + 1)); }
    let c = store(&app).save(&c).map_err(internal)?;
    ok(with_extra(&app,c))
}

pub async fn delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let _g = LOCK.lock().await;
    if app.h3.has(&task_of(&id)) {
        return Err(bad("拍攝中不能刪：先按「停」"));
    }
    store(&app).trash(&id, &app.paths.root.join("待刪除")).map_err(bad)?;
    ok(json!({"ok": true}))
}

/// 開始一個背景工作（拍或接）：登記在 app.h3（雲端閒置不會關機、/api/h3/stop 也停得掉）
fn begin(app: &Shared, id: &str) -> Result<CancellationToken, ApiError> {
    let task = task_of(id);
    if app.h3.has(&task) {
        return Err(bad("這條長片已經在拍了"));
    }
    let token = CancellationToken::new();
    app.h3.put(&task, token.clone());
    Ok(token)
}

fn spawn(app: &Shared, id: &str, token: CancellationToken, work: impl std::future::Future<Output = Result<(), String>> + Send + 'static) {
    struct Unlist(Shared, String);
    impl Drop for Unlist {
        fn drop(&mut self) {
            self.0.h3.take(&task_of(&self.1));
        }
    }
    let (a, id) = (app.clone(), id.to_string());
    crate::diagnostics::spawn(async move {
        let unlist = Unlist(a.clone(), id.clone());
        let r = work.await;
        drop(unlist);
        match r {
            Ok(()) => {
                let pending = store(&a).get(&id).is_some_and(|c| c["segments"].as_array().is_some_and(|ss| ss.iter().any(|s| s["pending_take"].is_string())));
                emit(&a, &id, None, if pending { "review" } else { "done" }, if pending { "這一鏡已拍好，請比較版本並採用後繼續" } else { "完成" });
            },
            Err(_) if token.is_cancelled() => emit(&a, &id, None, "stopped", "已停止"),
            Err(e) => emit(&a, &id, None, "error", super::api_h3::friendly_error(&e)),
        }
    });
}

/// {id, from?}
pub async fn run(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let guard = LOCK.lock().await;
    if app.h3.has(&task_of(&id)) { return Err(bad("這條長片已經在拍了")); }
    let mut c = load(&app, &id)?;
    if hc::on_runninghub(&c) {
        super::api_runninghub::ready(&app).map_err(bad)?;
        if !crate::refmod::names_of(&c).map_err(bad)?.is_empty() {
            return Err(bad("RunningHub 上沒有生圖站的 RefMod：拍攝單取消「用 RefMod 鎖臉」，或改用雲端顯卡拍"));
        }
    } else if hc::on_local(&c) {
        local_opts(&app).await.map_err(bad)?;
        super::api_refmod::ensure_local(&app, &crate::refmod::names_of(&c).map_err(bad)?).await.map_err(bad)?;
    } else {
        let packs = app.agent.cloud.live_packs().ok_or_else(|| bad("雲端還沒開好：到「H3 影片」分頁按「開雲端」，等燈號變綠（或把這條長片的「在哪拍」改成本機、RunningHub）"))?;
        if !packs.h3 {
            return Err(bad("雲端這台沒裝 H3 影片包：關機後到雲端設定勾「H3 影片」再開"));
        }
    }
    let n = c["segments"].as_array().map_or(0, Vec::len);
    if let Some(from) = b.get("from").and_then(crate::py::int) {
        if from < 0 || from as usize >= n {
            return Err(bad("沒有這一段"));
        }
        let dir = store(&app).seg_dir(&id);
        let shape = c["size"].clone();
        for seg in c["segments"].as_array_mut().into_iter().flatten() {
            ht::preserve(seg, &dir).map_err(bad)?;
            let selected = tx(&seg["selected_take"]);
            for take in seg["takes"].as_array_mut().into_iter().flatten() {
                if take["id"] == selected && take["recipe"]["size"].is_null() && !shape.is_null() { take["recipe"]["size"] = shape.clone(); }
            }
        }
        hc::reset_from(&mut c, from as usize);
    }
    for (i, s) in c["segments"].as_array().into_iter().flatten().enumerate() {
        // 拍好的段不會再送圖：它的素材就算清掉了也不擋後面的段開拍
        if s["status"] == "done" {
            continue;
        }
        if tx(&s["text"]).trim().is_empty() {
            return Err(bad(format!("第 {} 段還沒寫畫面", i + 1)));
        }
        for f in hc::inputs_of(&c, i) {
            if !app.paths.input.join(&f).is_file() {
                return Err(bad(format!("素材 {f} 不見了：重新放一次")));
            }
        }
    }
    if c["segments"].as_array().is_some_and(|ss| ss.iter().any(|s| s["pending_take"].is_string())) {
        return Err(bad("還有未採用的拍攝版本，請先選片再繼續"));
    }
    let token = begin(&app, &id)?;
    if let Err(e) = store(&app).save(&c) {
        app.h3.take(&task_of(&id));
        return Err(internal(e));
    }
    drop(guard);
    if !hc::on_runninghub(&c) && !hc::on_local(&c) {
        app.agent.cloud.touch();
    }
    emit(&app, &id, None, "running", "開拍");
    let (a, id2, t2) = (app.clone(), id.clone(), token.clone());
    spawn(&app, &id, token, async move { run_chain(&a, &id2, &t2).await });
    ok(json!({"ok": true}))
}

/// 發停止信號；背景工作收尾（清雲端佇列、標「已停止」）完才除名，收尾期間不能再開拍
fn stop_signal(h3: &super::api_h3::H3, id: &str) -> bool {
    h3.token(&task_of(id)).map(|t| t.cancel()).is_some()
}

pub async fn stop(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    if stop_signal(&app.h3, &tx(&b["id"])) {
        ok(json!({"ok": true}))
    } else {
        Err(bad("這條長片沒有在拍"))
    }
}

pub async fn join_api(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let guard = LOCK.lock().await;
    let c = load(&app, &id)?;
    if c["segments"].as_array().is_some_and(|ss| ss.iter().any(|s| s["pending_take"].is_string())) { return Err(bad("請先選定所有拍攝版本再合片")); }
    let token = begin(&app, &id)?;
    drop(guard);
    let (a, id2) = (app.clone(), id.clone());
    spawn(&app, &id, token, async move { join(&a, &id2).await });
    ok(json!({"ok": true}))
}

/// 段的影片、接好的長片（給網頁預覽）
pub fn file_ok(name: &str) -> bool {
    let b = name.as_bytes();
    name == "final.mp4"
        || (name.starts_with("take_") && name.ends_with(".mp4") && ht::safe_file(name))
        || (name.len() == 13 && name.starts_with("seg") && b[3].is_ascii_digit() && b[4].is_ascii_digit() && (name.ends_with("_cut.mp4") || name.ends_with("_end.mp4")))
}

pub async fn file(State(app): State<Shared>, UrlPath((id, name)): UrlPath<(String, String)>) -> ApiResult {
    if !file_ok(&name) || store(&app).get(&id).is_none() {
        return Err(not_found("找不到"));
    }
    file_response(store(&app).seg_dir(&id).join(name), None).await
}

// ---------------------------------------------------------------- 背景工作
async fn run_chain(app: &Shared, id: &str, token: &CancellationToken) -> Result<(), String> {
    loop {
        if token.is_cancelled() {
            return Err("已停止".into());
        }
        let c = store(app).get(id).ok_or("長片不見了")?;
        let Some(i) = c["segments"].as_array().into_iter().flatten().position(|s| s["status"] != "done") else {
            break;
        };
        if let Err(e) = run_seg(app, id, i, token).await {
            let stopped = token.is_cancelled();
            let _ = update_seg(app, id, i, |s| {
                s["status"] = json!(if stopped { "stopped" } else { "error" });
                s["error"] = json!(if stopped { "已停止".to_string() } else { super::api_h3::friendly_error(&e) });
            })
            .await;
            return Err(format!("第 {} 段：{e}", i + 1));
        }
        emit(app, id, Some(i), "seg_done", format!("第 {} 段拍好了", i + 1));
        if store(app).get(id).is_some_and(|c| c["review"] == true) { return Ok(()); }
    }
    join(app, id).await
}

/// 第一段開拍前定下畫面尺寸：有首幀就照首幀的比例（跟 H3 影片台一樣，不然首幀會被裁），沒有照比例設定
fn pick_size(app: &Shared, c: &Value) -> [i64; 2] {
    let dims = c["first"].as_str().and_then(|f| image::image_dimensions(app.paths.input.join(f)).ok()).map(|(w, h)| (w as i64, h as i64));
    let (w, h) = crate::h3::canvas(&tx(&c["aspect"]), &tx(&c["quality"]), dims);
    [w, h]
}

async fn run_seg(app: &Shared, id: &str, i: usize, token: &CancellationToken) -> Result<(), String> {
    if store(app).get(id).is_some_and(|c| hc::on_runninghub(&c)) {
        return run_seg_rh(app, id, i, token).await;
    }
    if store(app).get(id).is_some_and(|c| hc::on_local(&c)) {
        return run_seg_local(app, id, i, token).await;
    }
    let n = i + 1;
    update_seg(app, id, i, |s| {
        s["status"] = json!("running");
        s["error"] = Value::Null;
    })
    .await?;
    let status = |m: String| emit(app, id, Some(i), "running", format!("第 {n} 段：{m}"));
    status("準備…".into());
    super::api_cloud::ensure_chain_nodes(app, &status).await?;
    // 裝節點要一兩分鐘、中途停不下來：裝完先看有沒有按過停，別接著上傳素材、開拍
    if token.is_cancelled() {
        return Err("已停止".into());
    }
    let c = store(app).get(id).ok_or("長片不見了")?;
    if c["size"].is_null() {
        let size = pick_size(app, &c);
        update(app, id, |c| c["size"] = json!(size)).await?;
    }
    let seg = &c["segments"][i];
    for f in hc::inputs_of(&c, i) {
        status(format!("上傳素材 {f}…"));
        super::api_cloud::upload_input(app, &app.paths.input.join(&f)).await?;
    }
    let refmods = super::api_refmod::asset_files(app, &crate::refmod::names_of(&c)?)?;
    super::api_cloud::upload_refmods(app, &refmods, &status).await?;
    let sub = hc::remote_dir(id);
    let dir = store(app).seg_dir(id);
    if i > 0 {
        let name = hc::latent_name(i);
        if !super::api_cloud::has_output(app, &sub, &name) {
            let local = dir.join(&name);
            if !local.is_file() {
                return Err(format!("第 {i} 段的接續資料不見了：從第 {i} 段重拍"));
            }
            status("上傳上一段的接續資料…".into());
            super::api_cloud::upload_output(app, &local, &sub).await?;
        }
    }
    let seed = seg["seed"].as_i64().unwrap_or_else(super::api_director::random_seed);
    update_seg(app, id, i, |s| s["seed"] = json!(seed)).await?;
    let c = store(app).get(id).ok_or("長片不見了")?;
    // 參考圖也送進 DiT：雲端要有繪匣的小節點、圖要還在（接續段拿第 1 段首幀當參考，首幀被清掉就照舊只給文字編碼器）
    let dit_files = hc::dit_refs(&c, i);
    let (model, te) = super::api_h3::cloud_files(app, "", &c["te"]).await?;
    let opts = hc::GraphOpts { turbo_lora: crate::h3::official_ref2va(&model), model, te, ref_node: dit_files.iter().all(|f| app.paths.input.join(f).is_file()) && super::api_cloud::chain_ref_node_ready(app).await, ..Default::default() };
    for f in dit_files.iter().filter(|_| opts.ref_node) {
        super::api_cloud::upload_input(app, &app.paths.input.join(f)).await?;
    }
    if !dit_files.is_empty() && !opts.ref_node {
        status("這台雲端沒有參考圖節點（或參考圖檔不見了）：這段參考圖只給文字編碼器".into());
    }
    let g = hc::seg_graph_with(&c, i, seed, &opts)?;
    let recipe = json!({"engine":"h3", "seed":seed, "graph":g, "size":c["size"], "aspect":c["aspect"], "quality":c["quality"],
        "ref_dit":opts.ref_node && !dit_files.is_empty(),
        "prompt":hc::seg_prompt_with(&c, i, if opts.ref_node { dit_files.len() } else { 0 }), "sheet":c["sheet"], "first":c["first"], "refs":if seg["refs"].is_array() { seg["refs"].clone() } else { c["refs"].clone() }, "segment":{"secs":seg["secs"],"text":seg["text"],"lines":seg["lines"],"last":seg["last"],"entry":seg["entry"],"exit":seg["exit"],"inherit_entry":seg["inherit_entry"]}});
    let source_take = i.checked_sub(1).map(|prev| tx(&c["segments"][prev]["selected_take"]));
    let review = c["review"] == true;
    let t0 = std::time::Instant::now();
    let (_, entry) = super::api_cloud::run_raw(app, g, token, &status).await?;
    status("下載成品…".into());
    let mut files = json!({});
    for tag in ["cut", "end"] {
        let prefix = format!("seg{n:02}_{tag}");
        let remote = pick_output_file(&entry, &sub, &prefix).ok_or_else(|| format!("雲端沒有產生 {prefix} 的影片"))?;
        let local = format!("{prefix}.mp4");
        super::api_cloud::fetch_output(app, &sub, &remote, &dir.join(&local)).await?;
        files[tag] = json!(local);
    }
    let lat = hc::latent_name(n);
    super::api_cloud::fetch_output(app, &sub, &lat, &dir.join(&lat)).await?;
    super::api_cloud::mark_output(app, &sub, &lat);
    files["lat"] = json!(lat);
    let texts = preview_texts(&entry);
    let took = t0.elapsed().as_secs();
    status("用拆幀台檢查跳點…".into());
    let check = check_video(app, format!("H3 長片 {id} 第 {n} 段"), &dir.join(format!("seg{n:02}_end.mp4"))).await;
    let result = json!({"status":"done", "error":null, "seed":seed, "took":took, "files":files, "handover":texts.join("\n"),
        "head":parse_num(&texts, r"head context (\d+)").unwrap_or(0), "frames":parse_num(&texts, r"(\d+) frames \|").unwrap_or(0), "check":check});
    let _g = LOCK.lock().await;
    let mut c = store(app).get(id).ok_or("長片不見了")?;
    let seg = &mut c["segments"][i];
    let new_take = takes::record(seg, recipe, result, source_take.as_deref());
    ht::preserve(seg, &dir)?;
    if review {
        seg["status"] = json!(if seg["selected_take"].is_null() { "review" } else { "done" });
    } else {
        ht::select(&mut c, &dir, i, &new_take)?;
    }
    store(app).save(&c)?;
    Ok(())
}

/// 本機 ComfyUI 拍長片要的東西：連得上、有接續節點、有 H3 主模型。回傳這台的節點圖選項
/// （主模型挑最省顯存的、小顯存模型自動切塊、有沒有參考圖節點）
async fn local_opts(app: &Shared) -> Result<hc::GraphOpts, String> {
    if !app.comfy.live().online {
        return Err("本機的 ComfyUI 沒有連線：先把 ComfyUI 開起來，或把「在哪拍」改成雲端".into());
    }
    if app.comfy.object_info("H3ContinuousStartV14").await.is_err() {
        return Err(install_local_chain_nodes(app).await?);
    }
    let models = super::api_h3::local_models(app).await;
    let model = super::api_h3::pick_local_model(&models)
        .or_else(|| models.iter().find(|m| super::basename(m) == crate::h3::MODEL).cloned())
        .or_else(|| models.iter().find(|m| super::basename(m).to_lowercase().contains("10eros")).cloned())
        .ok_or("本機沒有長片能用的 H3 主模型（10Eros）：到下載中心或安裝精靈裝「H3 影片」，或把「在哪拍」改成雲端")?;
    super::api_h3::check_local_files(app, &model, crate::h3::TE).await?;
    let chunks = (crate::h3::low_vram_model(&model) && super::api_h3::local_chunk_node(app).await).then_some(8);
    Ok(hc::GraphOpts { model, chunks, ref_node: app.comfy.object_info(hc::REF_NODE).await.is_ok(), ..Default::default() })
}

/// 本機 ComfyUI 沒有接續節點：把程式附帶的那份（resources\extras\h3_infinite.tar.gz，跟雲端裝的同一份）解到 custom_nodes。
/// 回傳要顯示給使用者的話（裝好了要重開 ComfyUI 才載得到，所以這次還是不能拍）
async fn install_local_chain_nodes(app: &Shared) -> Result<String, String> {
    let tgz = app.paths.res.join("extras").join("h3_infinite.tar.gz");
    let nodes = app.paths.comfy_root.join("custom_nodes");
    let dest = nodes.join("HerrgottsH3Infinite");
    if dest.join("__init__.py").is_file() {
        return Ok("長片的接續節點已經在 custom_nodes 裡，但 ComfyUI 還沒載入：重開 ComfyUI 再按一次開拍".into());
    }
    if !tgz.is_file() {
        return Err(format!("找不到 {}：重新安裝繪匣", tgz.display()));
    }
    blocking(move || -> Result<(), String> {
        // 先解到 .disabled 結尾的暫存資料夾（ComfyUI 不載），解完才改名，中途失敗不會留半套
        let tmp = nodes.join("HerrgottsH3Infinite-installing.disabled");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("建立 {} 失敗：{e}", tmp.display()))?;
        let mut c = std::process::Command::new("tar");
        c.arg("-xzf").arg(&tgz).arg("-C").arg(&tmp).arg("--strip-components=1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x08000000);
        }
        let out = c.output().map_err(|e| format!("叫不到 tar（Windows 10 以上內建）：{e}"))?;
        if !out.status.success() || !tmp.join("__init__.py").is_file() {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("接續節點解不開：{}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("裝到 custom_nodes 失敗：{e}"))
    })
    .await?;
    Ok("第一次在本機拍長片：接續節點已經裝好，重開 ComfyUI 再按一次開拍".into())
}

/// 送一張節點圖到本機 ComfyUI、等它跑完，回 history 那一筆。停止＝從佇列拿掉＋中斷（只中斷這一筆）
async fn run_local_raw(app: &Shared, nodes: serde_json::Map<String, Value>, token: &CancellationToken, on_status: &(dyn Fn(String) + Send + Sync)) -> Result<Value, String> {
    let pid = app.comfy.submit(&Value::Object(nodes), &json!({})).await.map_err(|e| e.0)?;
    on_status("排隊中…".into());
    let t0 = std::time::Instant::now();
    let (mut miss, mut said) = (0u32, u64::MAX);
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                let _ = app.comfy.post("/queue", &json!({"delete": [pid]})).await;
                let _ = app.comfy.post("/interrupt", &json!({"prompt_id": pid})).await;
                return Err("已停止".into());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
        }
        let secs = t0.elapsed().as_secs();
        if said != secs / 10 {
            said = secs / 10;
            on_status(format!("本機生成中…已 {secs} 秒"));
        }
        match app.comfy.get(&format!("/history/{pid}")).await {
            Ok(h) => {
                miss = 0;
                let Some(entry) = h.get(&pid) else { continue };
                if entry["status"]["status_str"] != "success" {
                    return Err(format!("本機生成失敗：{}", super::api_cloud::exec_error(entry)));
                }
                return Ok(entry.clone());
            }
            // ComfyUI 算到很忙的時候偶爾不回應：連續 5 分鐘都連不到才放棄
            Err(e) => {
                miss += 1;
                if miss > 100 {
                    return Err(format!("本機 ComfyUI 連不上了：{}", e.0));
                }
            }
        }
    }
}

/// 本機版的一段：跟雲端同一張節點圖（Herrgott latent 接續），差在不用上傳下載——素材本來就在 ComfyUI 的 input，
/// 成品（影片和 latent）從 ComfyUI 的 output\h3chain\<id>\ 搬進長片資料夾；下一段開拍時再把上一段的 latent 用不重複的檔名放回去
async fn run_seg_local(app: &Shared, id: &str, i: usize, token: &CancellationToken) -> Result<(), String> {
    let n = i + 1;
    update_seg(app, id, i, |s| {
        s["status"] = json!("running");
        s["error"] = Value::Null;
    })
    .await?;
    let status = |m: String| emit(app, id, Some(i), "running", format!("第 {n} 段：{m}"));
    status("準備…".into());
    let mut opts = local_opts(app).await?;
    let c = store(app).get(id).ok_or("長片不見了")?;
    let dit_files = hc::dit_refs(&c, i);
    if !dit_files.is_empty() && !(opts.ref_node && dit_files.iter().all(|f| app.paths.input.join(f).is_file())) {
        opts.ref_node = false;
        status("本機 ComfyUI 還沒載入繪匣的參考圖節點（重開 ComfyUI 就有），或參考圖檔不見了：這段參考圖只給文字編碼器".into());
    }
    if c["size"].is_null() {
        let size = pick_size(app, &c);
        update(app, id, |c| c["size"] = json!(size)).await?;
    }
    super::api_refmod::ensure_local(app, &crate::refmod::names_of(&c)?).await?;
    let sub = hc::remote_dir(id);
    let dir = store(app).seg_dir(id);
    let out_dir = sub.split('/').fold(app.paths.output.clone(), |p, part| p.join(part));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("建立 {} 失敗：{e}", out_dir.display()))?;
    if i > 0 {
        // 上一段選用的版本可能換過：每次都拿長片資料夾裡那份 latent 給 ComfyUI 讀。
        // 檔名帶來源檔的時間和大小，內容換了就是新檔名：ComfyUI 讀過的檔在 Windows 上覆寫不了（見 GraphOpts::latent_in）
        let local = dir.join(hc::latent_name(i));
        let meta = std::fs::metadata(&local).map_err(|_| format!("第 {i} 段的接續資料不見了：從第 {i} 段重拍"))?;
        let stamp = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
        let name = format!("prev_{i:05}_{stamp}_{}.safetensors", meta.len());
        // 以前放的用不到了；還被 ComfyUI 佔著的刪不掉就留著，下次再清
        for old in std::fs::read_dir(&out_dir).into_iter().flatten().flatten() {
            let n = old.file_name().to_string_lossy().into_owned();
            if n.starts_with("prev_") && n != name {
                let _ = std::fs::remove_file(old.path());
            }
        }
        let placed = out_dir.join(&name);
        if std::fs::metadata(&placed).map(|m| m.len()).ok() != Some(meta.len()) {
            std::fs::copy(&local, &placed).map_err(|e| format!("準備上一段的接續資料失敗：{e}"))?;
        }
        opts.latent_in = Some(format!("{sub}/{name}"));
    }
    let seg = &c["segments"][i];
    let seed = seg["seed"].as_i64().unwrap_or_else(super::api_director::random_seed);
    update_seg(app, id, i, |s| s["seed"] = json!(seed)).await?;
    let c = store(app).get(id).ok_or("長片不見了")?;
    let g = hc::seg_graph_with(&c, i, seed, &opts)?;
    let dit = if opts.ref_node { dit_files.len() } else { 0 };
    let recipe = json!({"engine":"h3", "where":"local", "model":opts.model, "ref_dit":dit > 0, "seed":seed, "graph":g, "size":c["size"], "aspect":c["aspect"], "quality":c["quality"],
        "prompt":hc::seg_prompt_with(&c, i, dit), "sheet":c["sheet"], "first":c["first"], "refs":if seg["refs"].is_array() { seg["refs"].clone() } else { c["refs"].clone() }, "segment":{"secs":seg["secs"],"text":seg["text"],"lines":seg["lines"],"last":seg["last"],"entry":seg["entry"],"exit":seg["exit"],"inherit_entry":seg["inherit_entry"]}});
    let source_take = i.checked_sub(1).map(|prev| tx(&c["segments"][prev]["selected_take"]));
    let review = c["review"] == true;
    // H3 主模型＋文字編碼器很大：前一個任務不是 H3 就先清掉它留下的模型（同一條長片連拍不清，省掉重新載入）
    app.pe.release_for_generation().await;
    let was_h3 = std::mem::replace(&mut *app.last_family.lock().unwrap_or_else(|e| e.into_inner()), "h3".into()) == "h3";
    if !was_h3 {
        let _ = app.comfy.post("/free", &json!({"unload_models": true, "free_memory": true})).await;
    }
    let t0 = std::time::Instant::now();
    let entry = run_local_raw(app, g, token, &status).await?;
    status("收成品…".into());
    let mut files = json!({});
    for tag in ["cut", "end"] {
        let prefix = format!("seg{n:02}_{tag}");
        let made = pick_output_file(&entry, &sub, &prefix).ok_or_else(|| format!("本機沒有產生 {prefix} 的影片"))?;
        let local = format!("{prefix}.mp4");
        let src = out_dir.join(&made);
        std::fs::copy(&src, dir.join(&local)).map_err(|e| format!("搬 {made} 失敗：{e}"))?;
        let _ = std::fs::remove_file(&src);
        files[tag] = json!(local);
    }
    let lat = hc::latent_name(n);
    std::fs::copy(out_dir.join(&lat), dir.join(&lat)).map_err(|e| format!("搬 {lat} 失敗：{e}"))?;
    // output 那份不留：下一段讀的是另外放的 prev_ 檔，這個固定檔名沒人讀過，下次重拍才覆寫得了
    let _ = std::fs::remove_file(out_dir.join(&lat));
    files["lat"] = json!(lat);
    let texts = preview_texts(&entry);
    let took = t0.elapsed().as_secs();
    status("用拆幀台檢查跳點…".into());
    let check = check_video(app, format!("H3 長片 {id} 第 {n} 段"), &dir.join(format!("seg{n:02}_end.mp4"))).await;
    let result = json!({"status":"done", "error":null, "seed":seed, "took":took, "files":files, "handover":texts.join("\n"),
        "head":parse_num(&texts, r"head context (\d+)").unwrap_or(0), "frames":parse_num(&texts, r"(\d+) frames \|").unwrap_or(0), "check":check});
    let _g = LOCK.lock().await;
    let mut c = store(app).get(id).ok_or("長片不見了")?;
    let seg = &mut c["segments"][i];
    let new_take = takes::record(seg, recipe, result, source_take.as_deref());
    ht::preserve(seg, &dir)?;
    if review {
        seg["status"] = json!(if seg["selected_take"].is_null() { "review" } else { "done" });
    } else {
        ht::select(&mut c, &dir, i, &new_take)?;
    }
    store(app).save(&c)?;
    Ok(())
}

/// RunningHub 版的一段：一段一個任務。第 2 段起把上一段的影片（end）複製進 input 當「接著拍」，
/// 拍好的影片開頭 CONTINUE_FRAMES 格是上一段尾巴的重疊，切掉後當 cut／end（沒有 latent、沒有交棒分析）
async fn run_seg_rh(app: &Shared, id: &str, i: usize, token: &CancellationToken) -> Result<(), String> {
    let n = i + 1;
    update_seg(app, id, i, |s| {
        s["status"] = json!("running");
        s["error"] = Value::Null;
    })
    .await?;
    let status = |m: String| emit(app, id, Some(i), "running", format!("第 {n} 段：{m}"));
    status("準備…".into());
    let c = store(app).get(id).ok_or("長片不見了")?;
    if c["size"].is_null() {
        let size = pick_size(app, &c);
        update(app, id, |c| c["size"] = json!(size)).await?;
    }
    let dir = store(app).seg_dir(id);
    let prev = if i > 0 {
        let name = tx(&c["segments"][i - 1]["files"]["end"]);
        let src = dir.join(&name);
        if !ht::safe_file(&name) || !src.is_file() {
            return Err(format!("第 {i} 段的影片不見了：從第 {i} 段重拍"));
        }
        // input 裡的名字每條長片、每段固定：重拍就蓋掉
        let dst = format!("h3_rhchain_{id}_{i:02}.mp4");
        std::fs::copy(&src, app.paths.input.join(&dst)).map_err(|e| format!("準備上一段的影片失敗：{e}"))?;
        Some(dst)
    } else {
        None
    };
    let seg = c["segments"][i].clone();
    let seed = seg["seed"].as_i64().unwrap_or_else(super::api_director::random_seed);
    update_seg(app, id, i, |s| s["seed"] = json!(seed)).await?;
    let c = store(app).get(id).ok_or("長片不見了")?;
    let mut p = hc::rh_params(&c, i, prev.as_deref(), seed);
    crate::runninghub::h3_defaults(&mut p);
    let (q, input) = (p.clone(), app.paths.input.clone());
    let (g, _info) = blocking(move || crate::h3::build(&q, &input, 0)).await.map_err(|e| e.0)?;
    let recipe = json!({"engine":"h3", "where":"runninghub", "seed":seed, "graph":g, "params":p, "size":c["size"], "aspect":c["aspect"], "quality":c["quality"],
        "prompt":hc::seg_prompt(&c,i), "sheet":c["sheet"], "first":c["first"], "refs":hc::seg_refs(&c, i), "segment":{"secs":seg["secs"],"text":seg["text"],"lines":seg["lines"],"last":seg["last"],"entry":seg["entry"],"exit":seg["exit"],"inherit_entry":seg["inherit_entry"]}});
    let source_take = i.checked_sub(1).map(|prev| tx(&c["segments"][prev]["selected_take"]));
    let review = c["review"] == true;
    let t0 = std::time::Instant::now();
    let (_, got) = super::api_runninghub::run_files(app, g, None, token, &status).await?;
    let video = got.into_iter().find(|(sub, base, _)| sub.is_empty() && base.to_ascii_lowercase().ends_with(".mp4")).ok_or("RunningHub 沒有回影片")?;
    let raw = dir.join(format!("seg{n:02}_rh.mp4"));
    std::fs::write(&raw, &video.2).map_err(|e| format!("存影片失敗：{e}"))?;
    status("切掉開頭重疊的那一截…".into());
    let (end, cut) = (format!("seg{n:02}_end.mp4"), format!("seg{n:02}_cut.mp4"));
    let head = if i > 0 { crate::h3::CONTINUE_FRAMES } else { 0 };
    {
        let (t, raw, end_p, cut_p) = (studio::tools(&app.paths.data), raw.clone(), dir.join(&end), dir.join(&cut));
        blocking(move || -> Result<(), String> {
            if head > 0 {
                studio::trim_head(&t, &raw, &end_p, head)?;
            } else {
                std::fs::copy(&raw, &end_p).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&end_p, &cut_p).map_err(|e| e.to_string())?;
            Ok(())
        })
        .await?;
    }
    let took = t0.elapsed().as_secs();
    status("用拆幀台檢查跳點…".into());
    let check = check_video(app, format!("H3 長片 {id} 第 {n} 段"), &dir.join(&end)).await;
    let result = json!({"status":"done", "error":null, "seed":seed, "took":took, "files":{"cut":cut, "end":end}, "handover":"RunningHub：接著上一支拍",
        "head":head, "frames":0, "check":check});
    let _g = LOCK.lock().await;
    let mut c = store(app).get(id).ok_or("長片不見了")?;
    let seg = &mut c["segments"][i];
    let new_take = takes::record(seg, recipe, result, source_take.as_deref());
    ht::preserve(seg, &dir)?;
    if review {
        seg["status"] = json!(if seg["selected_take"].is_null() { "review" } else { "done" });
    } else {
        ht::select(&mut c, &dir, i, &new_take)?;
    }
    store(app).save(&c)?;
    Ok(())
}

/// 用拆幀台拆這段（縮到 480 高）再分析：{sid, cuts:[格], jumps:[格]}。失敗不擋（只是少了檢查）：{error}
async fn check_video(app: &Shared, label: String, video: &Path) -> Value {
    let (t, f, v) = (studio::tools(&app.paths.data), studio::Frames::new(&app.paths.data), video.to_path_buf());
    let source = json!({"id": Value::Null, "filename": label, "path": video, "external": true, "prompt": ""});
    let r = super::api_studio::frame_work(app, move || -> Result<Value, String> {
        let m = f.split(&t, &v, source, &studio::SplitOpts { max_h: Some(480), ..Default::default() })?;
        let sid = tx(&m["id"]);
        let a = f.analyze(&t, &sid)?;
        let frames = |k: &str| a[k].as_array().into_iter().flatten().filter_map(|x| x["n"].as_u64()).collect::<Vec<_>>();
        Ok(json!({"sid": sid, "cuts": frames("cuts"), "jumps": frames("jumps")}))
    })
    .await;
    r.unwrap_or_else(|e| json!({"error": e}))
}

/// 全部段落接成一支：第 1～N-1 段用 cut（切掉重疊和定住的尾巴），最後一段用 end（保留結尾）；本機 ffmpeg 接，進圖庫
async fn join(app: &Shared, id: &str) -> Result<(), String> {
    emit(app, id, None, "running", "接成一支…");
    let c = store(app).get(id).ok_or("長片不見了")?;
    let dir = store(app).seg_dir(id);
    let parts = join_parts(&c, &dir)?;
    let t = studio::tools(&app.paths.data);
    let dst = dir.join("final.mp4");
    let (t2, d2) = (t.clone(), dst.clone());
    let lens = blocking(move || -> Result<Vec<f64>, String> {
        let mut timed = vec![];
        for p in parts {
            let len = studio::probe(&t2, &p)?.duration;
            timed.push((p, len));
        }
        studio::join_chain(&t2, &timed, &d2)?;
        Ok(timed.iter().map(|x| x.1).collect())
    })
    .await?;
    let segs = c["segments"].as_array().cloned().unwrap_or_default();
    let summary: Vec<String> = segs.iter().enumerate().map(|(i, s)| format!("{}. {}", i + 1, tx(&s["text"]))).collect();
    let stem = format!("h3chain_{id}_{}", chrono::Local::now().format("%H%M%S"));
    let prompt = format!("H3 長片：{}（{} 段）\n{}", tx(&c["title"]), segs.len(), summary.join("\n"));
    let image = super::api_studio::register_video(app, dst.clone(), &stem, prompt, json!({"model": "H3 長片", "h3chain": id, "idea": tx(&c["title"])}))
        .await
        .map_err(|e| e.1)?;
    // 每段各自檢查看不到接縫：整支再檢查一次，標出落在接縫上的硬剪接、跳點
    emit(app, id, None, "running", "用拆幀台檢查接縫…");
    let mut check = check_video(app, format!("H3 長片 {id}（接好的）"), &dst).await;
    let seams = seam_frames(&lens);
    let flagged: Vec<u64> = ["cuts", "jumps"].iter().flat_map(|k| check[*k].as_array().cloned().unwrap_or_default()).filter_map(|v| v.as_u64()).collect();
    check["seams"] = json!(seams);
    check["seam_hits"] = json!(seam_hits(&seams, &flagged));
    update(app, id, |c| c["final"] = json!({"id": image["id"], "at": crate::py::now(), "check": check})).await?;
    Ok(())
}

/// 接好的長片裡，每個接縫是第幾格（下一段的第一格，1 起算）。lens＝每段的秒數（24fps）
pub fn seam_frames(lens: &[f64]) -> Vec<u64> {
    let mut at = 0u64;
    let mut out = vec![];
    for l in lens.iter().take(lens.len().saturating_sub(1)) {
        at += (l * crate::h3::FPS as f64).round() as u64;
        out.push(at + 1);
    }
    out
}

/// 拆幀台標到的格子（硬剪接、跳點）落在接縫 ±3 格內的：{seam: 第幾個接縫（1＝第 1、2 段之間）, frame}
pub fn seam_hits(seams: &[u64], flagged: &[u64]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for &f in flagged {
        if let Some(i) = seams.iter().position(|&s| s.abs_diff(f) <= 3) {
            if !out.iter().any(|v| v["seam"] == i + 1) {
                out.push(json!({"seam": i + 1, "frame": f}));
            }
        }
    }
    out
}

/// 接片要用的檔（照段落順序）
pub fn join_parts(c: &Value, dir: &Path) -> Result<Vec<PathBuf>, String> {
    let segs = c["segments"].as_array().cloned().unwrap_or_default();
    let last = segs.len().saturating_sub(1);
    segs.iter()
        .enumerate()
        .map(|(i, s)| {
            if s["status"] != "done" {
                return Err(format!("第 {} 段還沒拍好", i + 1));
            }
            let tag = if i == last { "end" } else { "cut" };
            let name = s["files"][tag].as_str().map(str::to_string).unwrap_or_else(|| format!("seg{:02}_{tag}.mp4", i + 1));
            if !ht::safe_file(&name) { return Err("影片檔名不正確".into()); }
            let f = dir.join(name);
            if f.is_file() {
                Ok(f)
            } else {
                Err(format!("第 {} 段的影片檔不見了：從第 {} 段重拍", i + 1, i + 1))
            }
        })
        .collect()
}

/// history 的 outputs 裡，子資料夾 sub、檔名以 prefix_ 開頭的檔（SaveVideo 會在後面加 _00001_）；有好幾個取最新的
pub fn pick_output_file(entry: &Value, sub: &str, prefix: &str) -> Option<String> {
    let mut names: Vec<String> = entry["outputs"]
        .as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .flat_map(|o| ["images", "gifs", "videos"].into_iter().flat_map(move |k| o[k].as_array().cloned().unwrap_or_default()))
        .filter(|f| tx(&f["subfolder"]).replace('\\', "/") == sub && tx(&f["filename"]).starts_with(&format!("{prefix}_")))
        .map(|f| tx(&f["filename"]))
        .collect();
    names.sort();
    names.pop()
}

/// PreviewAny 顯示的文字（交棒資訊、存檔資訊）
pub fn preview_texts(entry: &Value) -> Vec<String> {
    entry["outputs"].as_object().into_iter().flat_map(|o| o.values()).flat_map(|o| o["text"].as_array().cloned().unwrap_or_default()).map(|t| tx(&t)).collect()
}

fn parse_num(texts: &[String], re: &str) -> Option<i64> {
    let re = regex::Regex::new(re).ok()?;
    texts.iter().find_map(|t| re.captures(t).and_then(|c| c[1].parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_allowlist() {
        assert!(file_ok("seg01_end.mp4") && file_ok("seg12_cut.mp4") && file_ok("final.mp4"));
        assert!(!file_ok("../chain.json") && !file_ok("seg1_end.mp4") && !file_ok("segab_cut.mp4") && !file_ok("lat_00001.safetensors"));
    }

    #[test]
    fn join_parts_pick_cut_then_end() {
        let dir = std::env::temp_dir().join(format!("anima_chain_join_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["seg01_cut.mp4", "seg02_cut.mp4", "seg03_end.mp4"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let c = json!({"segments": [{"status": "done"}, {"status": "done"}, {"status": "done"}]});
        let names: Vec<String> = join_parts(&c, &dir).unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["seg01_cut.mp4", "seg02_cut.mp4", "seg03_end.mp4"]);
        let not_done = json!({"segments": [{"status": "done"}, {"status": "todo"}]});
        assert!(join_parts(&not_done, &dir).unwrap_err().contains("第 2 段"));
        let missing = json!({"segments": [{"status": "done"}, {"status": "done"}, {"status": "done"}, {"status": "done"}]});
        assert!(join_parts(&missing, &dir).unwrap_err().contains("第 3 段"), "第 3 段不是最後一段，要 cut 那支");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pick_output_file_by_prefix() {
        let e = json!({"outputs": {"30": {"images": [{"filename": "seg02_cut_00001_.mp4", "subfolder": "h3chain/abc", "type": "output"}]},
                                   "33": {"images": [{"filename": "seg02_end_00001_.mp4", "subfolder": "h3chain/abc", "type": "output"}]},
                                   "8": {"text": ["preset Balanced | ... | head context 39 | x"]}}});
        assert_eq!(pick_output_file(&e, "h3chain/abc", "seg02_cut").as_deref(), Some("seg02_cut_00001_.mp4"));
        assert_eq!(pick_output_file(&e, "h3chain/xyz", "seg02_cut"), None);
        assert_eq!(pick_output_file(&e, "h3chain/abc", "seg03_cut"), None);
        let t = preview_texts(&e);
        assert_eq!(parse_num(&t, r"head context (\d+)"), Some(39));
        assert_eq!(parse_num(&["277 frames | video".to_string()], r"(\d+) frames \|"), Some(277));
    }

    /// 長片拍攝期間登記在 app.h3：雲端閒置自動關機看得到它在忙
    #[test]
    fn chain_task_counts_as_busy() {
        let h = super::super::api_h3::H3::default();
        let t = CancellationToken::new();
        h.put(&task_of("abc"), t);
        assert!(h.any_running() && h.has("chain-abc"));
        h.take("chain-abc");
        assert!(!h.any_running());
    }

    /// 接好的長片：每段的長度 → 接縫在第幾格（下一段的第一格，1 起算）；整支檢查標到的格子落在接縫 ±3 格內 → 哪個接縫跳了
    #[test]
    fn seams_found_in_final() {
        let seams = seam_frames(&[9.416667, 9.208344, 9.916667]);
        assert_eq!(seams, [227, 448]);
        assert_eq!(seam_hits(&seams, &[448, 30]), vec![json!({"seam": 2, "frame": 448})]);
        assert_eq!(seam_hits(&seams, &[225]), vec![json!({"seam": 1, "frame": 225})]);
        assert!(seam_hits(&seams, &[300]).is_empty());
    }

    /// 按「停」只發停止信號、不馬上除名：背景工作收尾完才除名，收尾期間不能再按開拍（不然會兩條同時拍）
    #[test]
    fn stop_keeps_task_until_finished() {
        let h = super::super::api_h3::H3::default();
        h.put(&task_of("abc"), CancellationToken::new());
        assert!(stop_signal(&h, "abc"));
        assert!(h.has("chain-abc"), "收尾前還登記著");
        assert!(!stop_signal(&h, "zzz"));
    }
}

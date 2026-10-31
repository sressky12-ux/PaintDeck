//! 導演台 API（/api/director/*）與背景工作（寫提示詞、一段一段生第一格和影片、接成長片）。
//! 提示詞、存檔格式在 crate::director；這裡負責叫模型、送 ComfyUI、等任務跑完。
//!
//! 背景工作一個專案同時只跑一個；進度用 hub 送 {"type":"director", id, seg, phase, msg}，結束送 phase "end"，網頁收到就重新讀專案。
use super::api_assets::{assets_dir, attach_file, detach_file, first_image, replace_image, trash_asset};
use super::api_gallery::file_of;
use super::api_lib::item_from;
use super::api_gen::submit_jobs;
use super::{bad, blocking, file_response, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::db::Thumb;
use crate::director::{self as d, tx, Store, INPUT_PREFIX};
use crate::families;
use crate::py::{now, truthy};
use axum::extract::{Path as UrlPath, State};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct Director {
    pub store: Store,
    /// 專案檔「讀 → 改 → 寫」排隊
    pub(super) lock: tokio::sync::Mutex<()>,
    /// 正在跑背景工作的專案 → 停止用
    runs: Mutex<HashMap<String, CancellationToken>>,
}

impl Director {
    pub fn any_running(&self) -> bool {
        !self.runs.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
    pub fn new(data: &Path) -> Director {
        Director {
            store: Store::new(data),
            lock: tokio::sync::Mutex::new(()),
            runs: Mutex::new(HashMap::new()),
        }
    }
    pub(super) fn running(&self, id: &str) -> bool {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
    }
}

/// 改寫模型請求編號（取消用）
fn rid(id: &str) -> String {
    format!("director-{id}")
}

fn emit(app: &Shared, id: &str, seg: Option<usize>, phase: &str, msg: impl Into<String>) {
    app.hub.send(json!({"type": "director", "id": id, "seg": seg, "phase": phase, "msg": msg.into()}));
}

/// 改專案的一部分：排隊讀 → 改 → 寫
async fn update(app: &Shared, id: &str, f: impl FnOnce(&mut Value)) -> Result<Value, ApiError> {
    let _g = app.director.lock.lock().await;
    let mut p = app.director.store.project(id).ok_or_else(|| not_found("找不到這個專案"))?;
    f(&mut p);
    app.director.store.save_project(&mut p).map_err(internal)?;
    Ok(p)
}

async fn update_seg(app: &Shared, id: &str, i: usize, f: impl FnOnce(&mut Map<String, Value>)) -> Result<Value, ApiError> {
    update(app, id, |p| {
        if let Some(Value::Object(s)) = p["segments"].get_mut(i) {
            f(s);
        }
    })
    .await
}

fn load(app: &Shared, id: &str) -> Result<Value, ApiError> {
    app.director.store.project(id).ok_or_else(|| not_found("找不到這個專案"))
}

// ================================================================ 角色卡
// 角色存在資產庫：kind=char 的資產，data.cid 是導演台用的角色 id（舊專案 settings.characters 記的就是它）。
// 這裡的 API 回應形狀跟舊版一樣（id、name、desc、desc_zh、src、sheet、*_in、*_url），網頁不用改。

/// 角色圖複製到 ComfyUI input（缺的或大小不同才複製），回傳 input 裡的檔名。
/// 先找資產資料夾，找不到再找舊版的角色資料夾
fn stage(app: &Shared, file: &str) -> Option<String> {
    let file = super::basename(file);
    if file.is_empty() {
        return None;
    }
    let mut src = assets_dir(&app.paths).join(&file);
    if !src.is_file() {
        src = app.director.store.chars_dir().join(&file);
    }
    let name = format!("{INPUT_PREFIX}{file}");
    let dst = app.paths.input.join(&name);
    let same = match (std::fs::metadata(&src), std::fs::metadata(&dst)) {
        (Ok(a), Ok(b)) => a.len() == b.len(),
        _ => false,
    };
    if !same {
        if let Err(e) = std::fs::copy(&src, &dst) {
            eprintln!("複製角色圖 {} 失敗：{e}", src.display());
            return None;
        }
    }
    Some(name)
}

/// 角色 id → 資產 id。沒有 cid 的角色資產（從資產庫頁新增、還沒存過的）用 "lib<資產 id>"
fn char_asset(app: &Shared, cid: &str) -> Result<Option<i64>, ApiError> {
    if cid.is_empty() {
        return Ok(None);
    }
    if let Some(r) = app
        .db
        .q1("SELECT id FROM prompt_lib WHERE kind='char' AND json_extract(data,'$.cid')=? ORDER BY id LIMIT 1", [cid])?
    {
        return Ok(r.get("id").and_then(Value::as_i64));
    }
    let Some(id) = cid.strip_prefix("lib").and_then(|n| n.parse::<i64>().ok()) else {
        return Ok(None);
    };
    Ok(app
        .db
        .lib_item(id)?
        .filter(|r| r.get("kind").and_then(Value::as_str) == Some("char"))
        .map(|_| id))
}

/// 舊版角色卡（data\director\characters.json）搬進資產庫，回傳新建了幾個。舊檔、舊圖只讀不改。
/// 用 cid 查重、缺哪張圖補哪張，搬到一半當掉再跑也不會重複；全部做完才寫標記檔。呼叫的人要拿著 assets_lock
fn migrate_chars(app: &Shared) -> Result<usize, ApiError> {
    let store = &app.director.store;
    if store.chars_marker().exists() || !store.legacy_chars_file().exists() {
        return Ok(0);
    }
    let mut n = 0;
    for c in store.legacy_chars().map_err(internal)? {
        let cid = tx(&c["id"]);
        if cid.is_empty() {
            continue;
        }
        let id = match char_asset(app, &cid)? {
            Some(id) => id,
            None => {
                let name = tx(&c["name"]).trim().to_string();
                let body = json!({"kind": "char", "name": if name.is_empty() { "未命名" } else { &name },
                                  "data": {"cid": cid, "look_en": c["desc"], "look_zh": c["desc_zh"]}});
                n += 1;
                app.db
                    .lib_save(None, &item_from(&body, None)?, &Thumb::Keep)?
                    .ok_or_else(|| internal("角色搬進資產庫失敗"))?
            }
        };
        let data = app.db.lib_item(id)?.and_then(|r| r.get("data").cloned()).unwrap_or(Value::Null);
        for role in ["src", "sheet"] {
            let f = super::basename(&tx(&c[role]));
            let p = store.chars_dir().join(&f);
            if f.is_empty() || first_image(&data, role).is_some() {
                continue;
            }
            // 舊清單記著、硬碟上早就沒有的圖：永遠補不回來，跳過（不然搬家永遠完成不了，導演台角色一直報錯）。
            // 檔案在、只是這次複製失敗才回錯，下次再試
            if !p.exists() {
                eprintln!("角色「{cid}」的{role}圖 {} 不在了，跳過", p.display());
                continue;
            }
            attach_file(app, id, &p, role)?;
        }
    }
    std::fs::write(store.chars_marker(), chrono::Local::now().to_rfc3339()).map_err(internal)?;
    Ok(n)
}

/// 每個用到角色的路由開頭呼叫（開站時也在背景跑一次，資產庫頁才看得到搬過來的角色）：還沒搬家就先搬
pub(super) async fn migrated(app: &Shared) -> Result<(), ApiError> {
    if app.director.store.chars_marker().exists() {
        return Ok(());
    }
    let _g = app.assets_lock.lock().await;
    let a = app.clone();
    blocking(move || migrate_chars(&a)).await.map(|_| ())
}

/// 一筆角色資產 → 舊版角色卡的形狀
fn char_view(app: &Shared, r: &Value) -> Value {
    let data = &r["data"];
    let lib_id = r["id"].as_i64().unwrap_or(0);
    let cid = tx(&data["cid"]);
    let mut c = json!({
        "id": if cid.is_empty() { format!("lib{lib_id}") } else { cid },
        "asset_id": lib_id, "name": r["name"], "desc": tx(&data["look_en"]), "desc_zh": tx(&data["look_zh"]),
        "created": r["created"], "updated": r["updated"],
    });
    let t = r["updated"].as_f64().unwrap_or(0.0) as i64;
    for (role, ki, ku) in [("src", "src_in", "src_url"), ("sheet", "sheet_in", "sheet_url")] {
        if let Some(f) = first_image(data, role) {
            c[ki] = json!(stage(app, &f));
            c[ku] = json!(format!("/api/assets/file/{f}?t={t}"));
            c[role] = json!(f);
        }
    }
    c
}

fn chars_view(app: &Shared) -> Vec<Value> {
    match app.db.lib() {
        Ok(all) => all
            .into_iter()
            .filter(|r| r.get("kind").and_then(Value::as_str) == Some("char"))
            .map(|r| char_view(app, &json!(r)))
            .collect(),
        Err(e) => {
            eprintln!("讀角色資產失敗：{e}");
            vec![]
        }
    }
}

pub async fn chars_list(State(app): State<Shared>) -> ApiResult {
    migrated(&app).await?;
    let a = app.clone();
    ok(json!({"chars": blocking(move || chars_view(&a)).await}))
}

/// 角色圖的來源：input 檔名或圖庫編號；都沒給回 None
fn pick_source(app: &Shared, input: Option<&str>, gallery: Option<i64>) -> Result<Option<PathBuf>, ApiError> {
    let src: PathBuf = if let Some(n) = input.filter(|n| !n.is_empty()) {
        app.paths.input.join(super::basename(n))
    } else if let Some(gid) = gallery {
        let r = app.db.image(gid)?.ok_or_else(|| not_found("找不到這張圖"))?;
        file_of(&app.paths, &r)
    } else {
        return Ok(None);
    };
    if !src.exists() {
        return Err(not_found("找不到圖片檔（上傳暫存可能被清掉了，請重新選一次）"));
    }
    Ok(Some(src))
}

/// chars_save 的本體（拿著 assets_lock、在 blocking 裡跑）
fn save_char(app: &Shared, b: &Value, name: &str) -> Result<String, ApiError> {
    let id = tx(&b["id"]);
    let found = char_asset(app, &id)?;
    let dup = app.db.lib()?.into_iter().any(|r| {
        r.get("kind").and_then(Value::as_str) == Some("char")
            && r.get("name").and_then(Value::as_str) == Some(name)
            && r.get("id").and_then(Value::as_i64) != found
    });
    if dup {
        return Err(bad(format!("已經有叫「{name}」的角色了")));
    }
    let (lib_id, cid) = match found {
        Some(lid) => (lid, id),
        None => {
            let cid = d::new_id();
            let body = json!({"kind": "char", "name": name, "data": {"cid": cid}});
            let lid = app
                .db
                .lib_save(None, &item_from(&body, None)?, &Thumb::Keep)?
                .ok_or_else(|| internal("新增角色失敗"))?;
            (lid, cid)
        }
    };
    let row = app.db.lib_item(lib_id)?.ok_or_else(|| not_found("找不到這個角色"))?;
    let mut data = row.get("data").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    if data["cid"].as_str().unwrap_or("").is_empty() {
        data["cid"] = json!(cid);
    }
    for (k, dk) in [("desc", "look_en"), ("desc_zh", "look_zh")] {
        if let Some(v) = b.get(k) {
            data[dk] = json!(tx(v).trim());
        }
    }
    app.db
        .lib_save(Some(lib_id), &item_from(&json!({"name": name, "data": data}), Some(&row))?, &Thumb::Keep)?;
    for (role, key, gid) in [("src", "src", "src_id"), ("sheet", "sheet", "sheet_id")] {
        if let Some(p) = pick_source(app, b[key].as_str(), b.get(gid).and_then(crate::py::int))? {
            replace_image(app, lib_id, &p, role)?;
        }
    }
    if truthy(b.get("clear_sheet")) {
        let data = app.db.lib_item(lib_id)?.and_then(|r| r.get("data").cloned()).unwrap_or(Value::Null);
        if let Some(f) = first_image(&data, "sheet") {
            detach_file(app, lib_id, &f)?;
        }
    }
    Ok(cid)
}

/// 新增／修改角色：{id?, name, desc, desc_zh, src?（input 檔名）, src_id?（圖庫編號）, sheet?, sheet_id?, clear_sheet?}
pub async fn chars_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let name = tx(&b["name"]).trim().to_string();
    if name.is_empty() {
        return Err(bad("角色要有名字"));
    }
    migrated(&app).await?;
    let _g = app.assets_lock.lock().await;
    let a = app.clone();
    let cid = blocking(move || save_char(&a, &b, &name)).await?;
    ok(json!({"ok": true, "id": cid}))
}

/// 刪除角色：整筆資產（JSON＋圖）搬到 待刪除\資產庫\<名字>_<時間>\
pub async fn chars_delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    migrated(&app).await?;
    let _g = app.assets_lock.lock().await;
    let lib_id = char_asset(&app, &id)?.ok_or_else(|| not_found("找不到這個角色"))?;
    let row = app.db.lib_item(lib_id)?.ok_or_else(|| not_found("找不到這個角色"))?;
    let paths = app.paths.clone();
    let to = blocking(move || trash_asset(&paths, &row)).await.map_err(internal)?;
    app.db.run("DELETE FROM prompt_lib WHERE id=?", [lib_id])?;
    ok(json!({"ok": true, "moved_to": to.to_string_lossy()}))
}

/// 角色圖：先找資產資料夾，找不到再找舊版的角色資料夾（舊網址照樣能用）
pub async fn file(State(app): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult {
    let name = super::basename(&name);
    let p = assets_dir(&app.paths).join(&name);
    let p = if !name.is_empty() && p.is_file() { p } else { app.director.store.chars_dir().join(&name) };
    file_response(p, Some("max-age=3600")).await
}

/// 看圖寫外觀：{image（input 檔名）} → {desc, desc_zh}
pub async fn describe(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let img = super::basename(&tx(&b["image"]));
    if img.is_empty() || !app.paths.input.join(&img).exists() {
        return Err(bad("先選一張角色圖"));
    }
    let r = app
        .pe
        .general_chat("director-describe", d::SYSTEM_DESCRIBE, "Describe this character.", &[img], 800, 0.3)
        .await
        .map_err(bad)?;
    let m = d::json_in(&r);
    let g = |k: &str| m.as_ref().and_then(|m| m.get(k)).map(tx).unwrap_or_default();
    let desc = if g("desc").is_empty() { r.clone() } else { g("desc") };
    ok(json!({"desc": desc, "desc_zh": g("desc_zh")}))
}

// ================================================================ 專案
pub async fn projects(State(app): State<Shared>) -> ApiResult {
    let running: Vec<String> = app.director.runs.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    ok(json!({"projects": app.director.store.projects(), "running": running}))
}

pub async fn project_get(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let mut p = load(&app, &id)?;
    p["running"] = json!(app.director.running(&id));
    ok(p)
}

pub async fn project_new(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let mut p = d::blank_project(&tx(&b["title"]));
    if let Some(v) = b.get("copy_from").map(tx).filter(|v| !v.is_empty()) {
        // 複製設定（不複製段落）
        if let Some(src) = app.director.store.project(&v) {
            p["settings"] = src["settings"].clone();
        }
    }
    app.director.store.save_project(&mut p).map_err(internal)?;
    ok(p)
}

/// 拍攝單套用為導演台專案（內建 H3 或自訂工作流）；不改使用者的全域影片模型設定。
pub async fn from_sheet(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let engine = crate::video_wf::normalize_engine(&tx(&b["engine"])).to_string();
    if engine.is_empty() { return Err(bad("請選內建 H3 或已匯入的影片工作流")); }
    let needs_frame = engine_needs_frame(&app, &engine).map_err(bad)?;
    let prompts: Vec<String> = b["prompts"].as_array().ok_or_else(|| bad("缺少分鏡提示詞"))?.iter().map(|v| {
        v.as_str().map(str::to_string).or_else(|| v["prompt"].as_str().map(str::to_string)).unwrap_or_default()
    }).collect();
    let mut p = crate::shot_spec::director_project(&b["sheet"], &prompts, &engine).map_err(bad)?;
    if !needs_frame {
        for seg in p["segments"].as_array_mut().into_iter().flatten() { seg["continue"] = json!(false); }
    }
    if p["segments"].as_array().is_none_or(|a| a.is_empty() || a.len() != prompts.len() || prompts.iter().any(|p| p.trim().is_empty())) {
        return Err(bad("每一鏡都要有對應的影片提示詞"));
    }
    // 只複製生成設定；金鑰與全域使用偏好不進專案。
    if let Some(image) = b.get("image").filter(|v| v.is_object()) { p["settings"]["image"] = image.clone(); }
    let _g = app.director.lock.lock().await;
    app.director.store.save_project(&mut p).map_err(internal)?;
    ok(p)
}

/// 存網頁改過的專案（標題、劇情、參數、段落的提示詞）。執行中不能存：段落的結果正在被背景工作寫
pub async fn project_save(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let _g = app.director.lock.lock().await;
    if app.director.running(&id) {
        return Err(bad("這個專案正在執行，先按「停止」再修改"));
    }
    let mut p = load(&app, &id)?;
    {
        for k in ["title", "story", "settings"] {
            if let Some(v) = b.get(k) {
                p[k] = v.clone();
            }
        }
        if let Some(Value::Array(segs)) = b.get("segments") {
            let old = p["segments"].as_array().cloned().unwrap_or_default();
            if old.len() != segs.len() { return Err(bad("段落數已變更，請重新開啟專案再修改")); }
            let mut merged = old;
            for (i, incoming) in segs.iter().enumerate() {
                for key in ["title", "summary", "characters", "continue", "scene", "frame", "frame_zh", "video", "video_zh", "layout", "frame_lock", "engine", "seconds", "shot_spec", "entry", "exit", "inherit_entry", "media_slots", "wf_overrides", "video_loras", "video_params"] {
                    if let Some(v) = incoming.get(key) { merged[i][key] = v.clone(); }
                }
                // 取消指定第一格是明確的編輯動作，其餘結果欄位一律以伺服器存檔為準。
                if incoming.get("frame_lock").is_none() { merged[i].as_object_mut().map(|m| m.remove("frame_lock")); }
            }
            p["segments"] = json!(merged);
        }
    }
    app.director.store.save_project(&mut p).map_err(internal)?;
    ok(p)
}

pub async fn project_delete(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let _g = app.director.lock.lock().await;
    if app.director.running(&id) {
        return Err(bad("這個專案正在執行，先按「停止」"));
    }
    app.director.store.trash_project(&id, &app.paths.root.join("待刪除")).map_err(internal)?;
    ok(json!({"ok": true}))
}

/// POST {id,index,take_id}：只採用已拍好的版本，不提交任何生成任務。
pub async fn take_select(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let index = b["index"].as_u64().and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad("鏡頭編號不對"))?;
    let take_id = tx(&b["take_id"]);
    let _g = app.director.lock.lock().await;
    if app.director.running(&id) { return Err(bad("這個專案正在執行，先停止再採用版本")); }
    let mut p = load(&app, &id)?;
    freeze_legacy(&app, &mut p).map_err(bad)?;
    let seg = p["segments"].get(index).ok_or_else(|| bad("沒有這一鏡"))?;
    let take = seg["takes"].as_array().and_then(|a| a.iter().find(|t| t["id"] == take_id)).ok_or_else(|| bad("找不到這個拍攝版本"))?;
    let vid = take["result"]["video_id"].as_i64().ok_or_else(|| bad("這個版本沒有影片"))?;
    let row = app.db.image(vid)?.filter(|r| !truthy(r.get("deleted"))).ok_or_else(|| bad("這個版本的影片已不在圖庫，請選別版或重拍"))?;
    if !file_of(&app.paths, &row).is_file() { return Err(bad("這個版本的影片檔不見了，請選別版或重拍")); }
    if index > 0 && truthy(seg.get("continue")) && !take["source_take"].is_null() && take["source_take"] != p["segments"][index - 1]["selected_take"] {
        return Err(bad("這版接續的是上一鏡的另一個版本；請先採用相容的上一鏡，或重拍這一鏡"));
    }
    let changed = crate::film_takes::select(&mut p["segments"][index], &take_id, d::RESULT_KEYS).map_err(bad)?;
    d::adopt_take_recipe(&mut p["segments"][index], &take_id);
    if changed { d::invalidate_dependents(&mut p, index); }
    app.director.store.save_project(&mut p).map_err(internal)?;
    ok(p)
}

/// 舊版首末幀曾使用固定 input 檔名；第一次重拍前複製成各版獨立的檔案。
fn freeze_legacy(app: &Shared, p: &mut Value) -> Step {
    let project = tx(&p["id"]);
    for (i, seg) in p["segments"].as_array_mut().into_iter().flatten().enumerate() {
        crate::film_takes::ensure_legacy(seg, d::RESULT_KEYS);
        let selected = tx(&seg["selected_take"]);
        if let Some(takes) = seg["takes"].as_array_mut() {
            for take in takes {
                if truthy(take.get("media_frozen")) { continue; }
                let tid = tx(&take["id"]);
                if !d::valid_id(&tid) { return Err("拍攝版本編號不對".into()); }
                for (key, suffix) in [("frame_image", "first"), ("last_frame", "last")] {
                    let name = tx(&take["result"][key]);
                    if name.is_empty() { continue; }
                    if super::basename(&name) != name { return Err("版本的首末幀檔名不對".into()); }
                    let src = app.paths.input.join(&name);
                    if src.is_file() {
                        let frozen = format!("{INPUT_PREFIX}{project}_{i:02}_{tid}_{suffix}.png");
                        let dest = app.paths.input.join(&frozen);
                        if !dest.exists() { std::fs::copy(src, dest).map_err(|e| e.to_string())?; }
                        take["result"][key] = json!(frozen);
                    }
                }
                take["media_frozen"] = json!(true);
            }
        }
        if !selected.is_empty() {
            // 保留待審狀態；凍結只更新檔案索引。
            let result = seg["takes"].as_array().and_then(|a| a.iter().find(|t| t["id"] == selected)).map(|t| t["result"].clone());
            if let Some(result) = result { for key in ["frame_image", "last_frame"] { seg[key] = result[key].clone(); } }
        }
    }
    Ok(())
}

// ================================================================ 背景工作
/// 登記一個背景工作；同一個專案已經在跑就拒絕
fn begin(app: &Shared, id: &str) -> Result<CancellationToken, ApiError> {
    let mut runs = app.director.runs.lock().unwrap_or_else(|e| e.into_inner());
    if runs.contains_key(id) {
        return Err(bad("這個專案已經在執行了"));
    }
    let t = CancellationToken::new();
    runs.insert(id.to_string(), t.clone());
    Ok(t)
}

fn end(app: &Shared, id: &str) {
    app.director.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

type Step = Result<(), String>;

fn spawn(app: &Shared, id: &str, token: CancellationToken, work: impl std::future::Future<Output = Step> + Send + 'static) {
    let (app, id) = (app.clone(), id.to_string());
    crate::diagnostics::spawn(async move {
        let r = work.await;
        end(&app, &id);
        match r {
            Ok(()) => {
                let review = app.director.store.project(&id).is_some_and(|p| p["segments"].as_array().is_some_and(|a| a.iter().any(|s| !s["pending_take"].is_null())));
                emit(&app, &id, None, "end", if review { "這一鏡拍好了，請先審片採用，再繼續下一鏡" } else { "完成" });
            }
            Err(_) if token.is_cancelled() => emit(&app, &id, None, "end", "已停止"),
            Err(e) => {
                eprintln!("導演台 {id}：{e}");
                emit(&app, &id, None, "error", e)
            }
        }
    });
}

pub async fn stop(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let t = app.director.runs.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
    match t {
        Some(t) => {
            t.cancel();
            app.pe.cancel(&rid(&id));
            ok(json!({"ok": true}))
        }
        None => ok(json!({"ok": false})),
    }
}

/// 選了的角色（照設定裡的順序）
fn picked_chars(app: &Shared, p: &Value) -> Vec<Value> {
    let all = chars_view(app);
    p["settings"]["characters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| all.iter().find(|c| c["id"] == *id).cloned())
        .collect()
}

/// 生第一格用的模型是哪個家族
fn image_family(app: &Shared, p: &Value) -> &'static str {
    let model = tx(&p["settings"]["image"]["model"]);
    families::family_of(&app.paths.unet_roots(), &model)
}

/// 這段要拿哪些角色的設定圖當參考圖（只有 Qwen 2.1 能用多圖參考；最多 3 張）
fn seg_refs(app: &Shared, p: &Value, chars: &[Value], seg: &Value) -> Vec<(String, String)> {
    frame_reference_list(image_family(app, p), p, chars, seg)
}

fn frame_reference_list(family: &str, p: &Value, chars: &[Value], seg: &Value) -> Vec<(String, String)> {
    if family != families::QWEN21 {
        return vec![];
    }
    let present: Vec<String> = seg["characters"].as_array().into_iter().flatten().map(tx).collect();
    let sheet = enabled_sheet_refs(p, seg);
    let mut refs: Vec<(String, String)> = sheet.iter().map(|r| {
        let who = tx(&r["who"]);
        let name = p["sheet"]["cast"].as_array().and_then(|a| a.iter().find(|c| c["speaker"] == who)).map(|c| tx(&c["name"]))
            .filter(|n| !n.is_empty()).unwrap_or_else(|| if who.is_empty() { "場景參考".into() } else { who });
        (name, tx(&r["file"]))
    }).collect();
    let legacy = chars
        .iter()
        .filter(|c| present.contains(&tx(&c["name"])))
        .filter_map(|c| Some((tx(&c["name"]), c["sheet_in"].as_str().or(c["src_in"].as_str())?.to_string())));
    for item in legacy {
        if !refs.iter().any(|r| r.1 == item.1) { refs.push(item); }
    }
    let mut seen = std::collections::HashSet::new();
    refs.into_iter().filter(|r| seen.insert(r.1.clone())).take(3).collect()
}

/// 叫通用模型；JSON 讀不出來就再試一次
async fn ask(app: &Shared, id: &str, sys: &str, user: &str, max_tokens: i64, parse: impl Fn(&str) -> bool) -> Result<String, String> {
    let mut last = String::new();
    for attempt in 0..2 {
        let r = app.pe.general_chat(&rid(id), sys, user, &[], max_tokens, if attempt == 0 { 0.7 } else { 0.9 }).await?;
        if parse(&r) {
            return Ok(r);
        }
        eprintln!("導演台：模型輸出讀不出來（第 {} 次）：{}", attempt + 1, r.chars().take(300).collect::<String>());
        last = r;
    }
    Err(format!("模型輸出的格式壞掉了，可以再按一次。輸出開頭：{}", last.chars().take(120).collect::<String>()))
}

async fn write_shot(app: &Shared, id: &str, i: usize, token: &CancellationToken) -> Step {
    if token.is_cancelled() {
        return Err("已停止".into());
    }
    let p = load(app, id).map_err(|e| e.1)?;
    let n = p["segments"].as_array().map(|a| a.len()).unwrap_or(0);
    emit(app, id, Some(i), "write", format!("寫第 {} / {n} 段的提示詞…", i + 1));
    let a = app.clone();
    let p2 = p.clone();
    let chars = blocking(move || picked_chars(&a, &p2)).await;
    let anima = image_family(app, &p) == families::ANIMA;
    let refs: Vec<String> = seg_refs(app, &p, &chars, &p["segments"][i]).into_iter().map(|r| r.0).collect();
    let (sys, mut user) = d::shot_messages(&p, i, &chars, anima, &refs);
    // MOSS 的版面框：讓第一格的描述跟框一致（接上一段的不畫第一格，不用）
    let seg = &p["segments"][i];
    if image_family(app, &p) == families::MOSS && !(i > 0 && truthy(seg.get("continue"))) {
        user.push_str(&d::layout_rule(seg));
    }
    let r = ask(app, id, &sys, &user, 2500, |t| d::parse_shot(t).is_some()).await?;
    let shot = d::parse_shot(&r).ok_or("模型輸出的格式壞掉了")?;
    update_seg(app, id, i, |s| {
        for (k, v) in shot {
            let v = tx(&v);
            let v = match k.as_str() {
                "frame" => d::safe_text(&v, anima),
                "video" => d::safe_text(&v, false),
                _ => v,
            };
            s.insert(k, json!(v));
        }
    })
    .await
    .map_err(|e| e.1)?;
    emit(app, id, Some(i), "seg", "提示詞寫好了");
    Ok(())
}

/// {id, what: "plan"（重新分段＋寫全部提示詞）| "shots"（只寫還沒有提示詞的段）| "shot"（重寫第 index 段）, index?}
pub async fn write(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    migrated(&app).await?;
    let edit_guard = app.director.lock.lock().await;
    let p = load(&app, &id)?;
    let what = tx(&b["what"]);
    if what == "plan" && tx(&p["story"]).trim().is_empty() {
        return Err(bad("先寫劇情"));
    }
    let n = p["segments"].as_array().map(|a| a.len()).unwrap_or(0);
    let index = b.get("index").and_then(crate::py::int).map(|i| i as usize);
    if what == "shot" && index.is_none_or(|i| i >= n) {
        return Err(bad("沒有這一段"));
    }
    if !matches!(what.as_str(), "plan" | "shots" | "shot") {
        return Err(bad("what 只能是 plan／shots／shot"));
    }
    let token = begin(&app, &id)?;
    drop(edit_guard);
    let (a, id2, t2) = (app.clone(), id.clone(), token.clone());
    spawn(&app, &id, token, async move {
        let (app, id, token) = (a, id2, t2);
        if what == "plan" {
            emit(&app, &id, None, "write", "AI 分段中…（第一次要載入模型，約 30 秒）");
            let p = load(&app, &id).map_err(|e| e.1)?;
            let a = app.clone();
            let p2 = p.clone();
            let chars = blocking(move || picked_chars(&a, &p2)).await;
            let (sys, user) = d::plan_messages(&p, &chars);
            let r = ask(&app, &id, &sys, &user, 6000, |t| d::parse_plan(t).is_some()).await?;
            let segs = d::parse_plan(&r).ok_or("分段結果讀不出來")?;
            update(&app, &id, |p| {
                p["segments"] = json!(segs);
                p["final"] = Value::Null;
            })
            .await
            .map_err(|e| e.1)?;
            emit(&app, &id, None, "seg", "分好段了，接著寫每一段的提示詞");
        }
        let p = load(&app, &id).map_err(|e| e.1)?;
        let segs = p["segments"].as_array().cloned().unwrap_or_default();
        let todo: Vec<usize> = match (what.as_str(), index) {
            ("shot", Some(i)) => vec![i],
            ("shots", _) => (0..segs.len()).filter(|&i| tx(&segs[i]["video"]).is_empty()).collect(),
            _ => (0..segs.len()).collect(),
        };
        for i in todo {
            write_shot(&app, &id, i, &token).await?;
        }
        Ok(())
    });
    ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- 生成
/// 等 ComfyUI 任務跑完；停止時把它從佇列拿掉（或中斷）
pub(super) async fn wait_job(app: &Shared, pid: &str, token: &CancellationToken) -> Step {
    loop {
        if token.is_cancelled() {
            if app.comfy.live().running.as_deref() == Some(pid) {
                let _ = app.comfy.post("/interrupt", &json!({})).await;
            } else {
                let _ = app.comfy.post("/queue", &json!({"delete": [pid]})).await;
                let _ = app.db.set_job(pid, "cancelled", None);
            }
            return Err("已停止".into());
        }
        let job = app.db.job(pid).map_err(|e| e.to_string())?;
        match job.as_ref().and_then(|j| j["status"].as_str()) {
            Some("done") => return Ok(()),
            Some("queued" | "running") => {}
            Some(s) => {
                let e = job.as_ref().map(|j| tx(&j["error"])).unwrap_or_default();
                return Err(format!("ComfyUI 任務沒有完成（{s}）{e}"));
            }
            None => return Err("任務紀錄不見了".into()),
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
}

/// 任務產生、還在圖庫裡的第一張圖／影片
pub(super) fn job_image(app: &Shared, pid: &str) -> Result<Map<String, Value>, String> {
    app.db
        .q1("SELECT * FROM images WHERE job=? AND deleted=0 ORDER BY id LIMIT 1", [pid])
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "任務跑完了，但圖庫裡找不到結果".into())
}

/// 影片任務的影片檔（自訂的影片工作流可能另外存了圖，不能拿第一個結果）
fn job_video(app: &Shared, pid: &str) -> Result<Map<String, Value>, String> {
    app.db
        .q1(
            "SELECT * FROM images WHERE job=? AND deleted=0 AND (lower(filename) LIKE '%.mp4' OR lower(filename) LIKE '%.webm' OR lower(filename) LIKE '%.mkv') ORDER BY id LIMIT 1",
            [pid],
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "影片任務跑完了，但圖庫裡找不到影片檔（自訂工作流要存成 mp4／webm）".into())
}

/// 複製到 ComfyUI input，回傳 input 裡的檔名
pub(super) fn to_input(app: &Shared, src: &Path, name: &str) -> Result<String, String> {
    std::fs::copy(src, app.paths.input.join(name)).map_err(|e| format!("複製 {} 失敗：{e}", src.display()))?;
    Ok(name.to_string())
}

pub(super) fn random_seed() -> i64 {
    rand::random_range(0..=(1i64 << 48))
}

/// 第一格的尺寸：16:9，約一百萬像素（兩個家族都是 64 的倍數）
const FRAME_W: i64 = 1344;
const FRAME_H: i64 = 768;

/// 關閉參考圖會保留草稿 items，但它們不應進入任何生成請求。
fn enabled_sheet_refs(p: &Value, seg: &Value) -> Vec<Value> {
    if !truthy(p["sheet"]["refs"].get("on")) { return vec![]; }
    let present = seg["characters"].as_array();
    p["sheet"]["refs"]["items"].as_array().into_iter().flatten().filter(|r| {
        if tx(&r["file"]).trim().is_empty() { return false; }
        let who = tx(&r["who"]);
        if who.is_empty() { return true; }
        // 若本鏡有明確出場名單，只取該角色的圖；無對應角色的通用參考仍保留。
        let character = p["sheet"]["cast"].as_array().and_then(|a| a.iter().find(|c| c["speaker"] == who));
        character.is_none_or(|c| present.is_none_or(|a| a.contains(&c["name"])))
    }).cloned().collect()
}

/// 每鏡明確保存的槽位是一份完整選擇；空物件能確實清除專案其他版本的預設槽位。
fn shot_slots(p: &Value, seg: &Value) -> Map<String, Value> {
    seg["media_slots"].as_object().or_else(|| p["settings"]["media_slots"].as_object()).cloned().unwrap_or_default()
}

/// 先決定真正的首幀，再決定是否需要生圖。接續鏡頭以採用的上游尾幀為唯一來源。
fn frame_source(p: &Value, index: usize, mapping: &crate::video_wf::Mapping) -> Result<Option<String>, String> {
    let seg = &p["segments"][index];
    if index > 0 && truthy(seg.get("continue")) {
        let previous = tx(&p["segments"][index - 1]["last_frame"]);
        return if previous.is_empty() { Err("上一鏡還沒有可用的採用尾幀，請先審片採用上一鏡或取消「接上一段」".into()) } else { Ok(Some(previous)) };
    }
    let slots = shot_slots(p, seg);
    let mut named = None;
    for slot in mapping.media_slots.iter().filter(|s| s.role == "first" && s.kind == "image") {
        let Some(file) = slots.get(&slot.key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else { continue; };
        crate::video_wf::media_basename(file, "image")?;
        if named.as_deref().is_some_and(|old| old != file) { return Err("同一鏡的首幀槽指定了不同圖片，請選同一張首幀".into()); }
        named = Some(file.to_string());
    }
    if named.is_some() { return Ok(named); }
    Ok(seg["frame_lock"].as_str().filter(|s| !s.is_empty()).map(str::to_string))
}

/// 最終槽位綁定：首幀全部使用已決定且凍結的 frame，其餘槽位才採用本鏡選圖或啟用的參考。
fn bind_video_slots(p: &Value, seg: &Value, mapping: &crate::video_wf::Mapping, frame: &str) -> Map<String, Value> {
    let mut slots = shot_slots(p, seg);
    let mut next_ref = 0usize;
    let refs = enabled_sheet_refs(p, seg);
    for slot in &mapping.media_slots {
        if slot.role == "first" && slot.kind == "image" && !frame.is_empty() {
            slots.insert(slot.key.clone(), json!(frame));
            continue;
        }
        if slots.get(&slot.key).is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty())) { continue; }
        if slot.kind == "image" && matches!(slot.role.as_str(), "character" | "reference") {
            let r = if let Some(who) = &slot.who { refs.iter().find(|r| tx(&r["who"]) == *who) }
                else { let r = refs.get(next_ref); next_ref += 1; r };
            if let Some(file) = r.and_then(|r| r["file"].as_str()) { slots.insert(slot.key.clone(), json!(file)); }
        }
    }
    // 空字串是 UI 的「沿用」，不傳給工作流當成無效檔名。
    slots.retain(|_, v| !v.as_str().is_some_and(str::is_empty));
    slots
}

fn applied_video_slots(workflow: &Value, mapping: &crate::video_wf::Mapping, slots: &Map<String, Value>) -> Value {
    Value::Object(mapping.media_slots.iter().filter_map(|slot| {
        let file = slots.get(&slot.key).cloned().unwrap_or_else(|| workflow["graph"][&slot.node]["inputs"][&slot.input].clone());
        if file.is_null() { None } else { Some((slot.key.clone(), json!({"file":file,"kind":slot.kind,"role":slot.role,"who":slot.who}))) }
    }).collect())
}

/// 這段用哪個影片模型：段落自己的 → 專案的 → 全域設定；舊作品記的 ltx23 一律當內建 H3
fn engine_of(p: &Value, seg: &Value, global: &str) -> String {
    let e = seg["engine"].as_str().filter(|e| !e.is_empty()).or_else(|| p["settings"]["video_engine"].as_str().filter(|e| !e.is_empty())).unwrap_or(global);
    crate::video_wf::normalize_engine(e).to_string()
}

fn shot_engine(app: &Shared, p: &Value, seg: &Value) -> String {
    let global = crate::settings::load(&app.paths.settings).get("video_engine").and_then(Value::as_str).unwrap_or(crate::video_wf::BUILTIN).to_string();
    engine_of(p, seg, &global)
}

/// 內建 H3 走雲端（專案設定 h3_where，預設本機）
fn h3_cloud(p: &Value) -> bool {
    p["settings"]["h3_where"] == "cloud"
}

/// 內建影片一定要首幀（圖生影片）；自訂工作流回 None，要讀工作流才知道
fn builtin_needs_frame(engine: &str) -> Option<bool> {
    (crate::video_wf::normalize_engine(engine) == crate::video_wf::BUILTIN).then_some(true)
}

fn engine_needs_frame(app: &Shared, engine: &str) -> Result<bool, String> {
    if let Some(b) = builtin_needs_frame(engine) { return Ok(b); }
    let wf = crate::video_wf::load(&crate::video_wf::dir(&app.paths.data), engine)?;
    let mapping = crate::video_wf::Mapping::from_value(&wf["map"]);
    let analysis = crate::video_wf::analyze_with(&wf["graph"], &mapping)?;
    Ok(analysis.image.is_some() || mapping.media_slots.iter().any(|s| s.kind == "image" && s.role == "first"))
}

async fn make_frame(app: &Shared, id: &str, i: usize, p: &Value, tag: &str, token: &CancellationToken) -> Result<(String, Value), String> {
    let seg = &p["segments"][i];
    let Value::Object(base) = p["settings"]["image"].clone() else {
        return Err("還沒選生第一格用的模型（導演台左邊「第一格的畫法」按「用生成頁目前的設定」）".into());
    };
    // 模型檔不在了（或舊作品記著認不出的模型）：請使用者重選
    if image_family(app, p) == families::OTHER {
        return Err(format!(
            "生第一格用的模型 {} 找不到：先到生成頁選 Anima、Qwen 2.1 或 MOSS，再回導演台按「用生成頁目前的設定」",
            tx(&base["model"])
        ));
    }
    let a = app.clone();
    let p2 = p.clone();
    let chars = blocking(move || picked_chars(&a, &p2)).await;
    let refs = seg_refs(app, p, &chars, seg);
    let mut prompt = tx(&seg["frame"]);
    if prompt.trim().is_empty() {
        prompt = format!("{}, {}", tx(&seg["scene"]), d::style_words(&tx(&p["settings"]["style"])));
    }
    let mut q = base.clone();
    for k in ["init_image", "refs", "mask", "inpaint", "upscale", "control", "idea", "pe_prompt", "layout"] {
        q.remove(k);
    }
    // 這段拉的版面框（只有 MOSS 吃；後端 graphs.rs 照 FRAME_W×FRAME_H 換算）
    if image_family(app, p) == families::MOSS {
        if let Some(l) = seg["layout"].as_array().filter(|a| !a.is_empty()) {
            q.insert("layout".into(), json!(l));
        }
    }
    q.insert("prompt".into(), json!(prompt));
    q.insert("seed".into(), json!(random_seed()));
    q.insert("width".into(), json!(FRAME_W));
    q.insert("height".into(), json!(FRAME_H));
    if refs.is_empty() {
        q.insert("mode".into(), json!("txt2img"));
    } else {
        q.insert("mode".into(), json!("multiref"));
        q.insert("refs".into(), json!(refs.iter().map(|r| json!({"image": r.1})).collect::<Vec<_>>()));
        q.insert("size_mode".into(), json!("fixed"));
        q.insert("pixels".into(), json!(1024 * 1024));
        q.insert("ref_pixels".into(), json!(512 * 1024));
    }
    q.insert("director".into(), json!({"id": id, "seg": i, "kind": "frame"}));
    emit(app, id, Some(i), "frame", format!("第 {} 段：畫第一格…", i + 1));
    let jobs = submit_jobs(app, q, 1).await.map_err(|e| e.1)?;
    let pid = tx(&jobs[0]["job"]);
    update_seg(app, id, i, |s| {
        s.insert("frame_job".into(), json!(pid));
    })
    .await
    .map_err(|e| e.1)?;
    wait_job(app, &pid, token).await?;
    let row = job_image(app, &pid)?;
    let src = file_of(&app.paths, &row);
    let name = to_input(app, &src, &format!("{INPUT_PREFIX}{id}_{i:02}_{tag}_first.png"))?;
    Ok((name, row["id"].clone()))
}

async fn make_video(app: &Shared, id: &str, i: usize, p: &Value, frame: &str, take_context: (bool, &Value), token: &CancellationToken) -> Step {
    let (review, previous) = take_context;
    let seg = &p["segments"][i];
    let s = &p["settings"];
    let engine = shot_engine(app, p, seg);
    let workflow = if engine == crate::video_wf::BUILTIN { Value::Null } else {
        crate::video_wf::load(&crate::video_wf::dir(&app.paths.data), &engine)?
    };
    let mut q = s["video_params"].as_object().cloned().unwrap_or_default();
    if let Some(own) = seg["video_params"].as_object() { q.extend(own.clone()); }
    q.extend(json!({
        "mode": "video", "prompt": d::strip_refs(&tx(&seg["video"])), "init_image": frame, "seed": random_seed(),
        "video_engine": engine,
        "video_seconds": seg["seconds"].as_f64().or_else(|| s["seconds"].as_f64()).unwrap_or(5.0), "video_res": tx(&s["res"]),
        "save_last": true, "director": {"id": id, "seg": i, "kind": "video"},
    })
    .as_object()
    .cloned()
    .unwrap_or_default());
    for key in ["video_loras", "wf_overrides"] {
        if let Some(v) = seg.get(key).or_else(|| s.get(key)).or_else(|| s["image"].get(key)) { q.insert(key.into(), v.clone()); }
    }
    let mapping = crate::video_wf::Mapping::from_value(&workflow["map"]);
    if frame.is_empty() || (engine != crate::video_wf::BUILTIN && crate::video_wf::analyze_with(&workflow["graph"], &mapping)?.image.is_none()) { q.remove("init_image"); }
    // H3 照首幀的長寬挑比例（直的首幀拍直的片）
    if engine == crate::video_wf::BUILTIN && !frame.is_empty() {
        if let Ok((w, h)) = image::image_dimensions(app.paths.input.join(frame)) { q.insert("width".into(), json!(w)); q.insert("height".into(), json!(h)); }
    }
    let slots = bind_video_slots(p, seg, &mapping, frame);
    let applied_slots = applied_video_slots(&workflow, &mapping, &slots);
    // 即使本鏡明確清空槽位，也要覆蓋 video_params 裡另一版留下的舊值。
    q.insert("media_slots".into(), json!(slots));
    let requested = json!(q.clone());
    let source_take = if i > 0 && truthy(seg.get("continue")) { p["segments"][i - 1]["selected_take"].as_str().map(str::to_string) } else { None };
    let cloud = engine == crate::video_wf::BUILTIN && h3_cloud(p);
    emit(app, id, Some(i), "video", format!("第 {} 段：生影片…（{}）", i + 1, if cloud { "雲端 H3，10 秒約 4～8 分鐘" } else { "本機 H3，8GB 顯卡 5 秒約 4 分鐘" }));
    let pid = if cloud {
        let (a, sid) = (app.clone(), id.to_string());
        let status = move |m: String| emit(&a, &sid, Some(i), "video", format!("第 {} 段：{m}", i + 1));
        super::api_h3::cloud_one(app, &q, token, &status).await?.0
    } else {
        let jobs = submit_jobs(app, q, 1).await.map_err(|e| e.1)?;
        tx(&jobs[0]["job"])
    };
    let actual = app.db.job(&pid).map_err(|e| e.to_string())?.map(|j| j["params"].clone()).unwrap_or_else(|| requested.clone());
    let frame_recipe = seg["frame_job"].as_str().and_then(|j| app.db.job(j).ok().flatten()).map(|j| j["params"].clone()).unwrap_or(Value::Null);
    let references = json!({"first_frame":frame,"frame_refs":frame_recipe["refs"],"video_slots":applied_slots});
    let recipe = json!({"engine":engine, "params":actual, "requested_params":requested, "workflow":workflow,
        "frame_params":frame_recipe, "image_settings":s["image"], "first_frame":frame, "shot_spec":seg["shot_spec"],
        "entry":seg["entry"], "exit":seg["exit"], "characters":seg["characters"], "references":references});
    update_seg(app, id, i, |s| {
        s.insert("video_job".into(), json!(pid));
    })
    .await
    .map_err(|e| e.1)?;
    if !cloud { wait_job(app, &pid, token).await?; }
    let row = job_video(app, &pid)?;
    // 最後一格：h3::build 存在 _poster\<poster>-last_00001_.png（雲端的跟成品一起下載回這裡）
    let poster = app.db.job(&pid).ok().flatten().map(|j| tx(&j["params"]["poster"])).unwrap_or_default();
    let dir = app.paths.output.join("AnimaWeb").join("_poster");
    let last = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|f| f.file_name().and_then(|n| n.to_str()).is_some_and(|n| !poster.is_empty() && n.starts_with(&format!("{poster}-last")) && n.ends_with(".png")))
        .min();
    let last_in = match last {
        Some(f) => Some(to_input(app, &f, &format!("{INPUT_PREFIX}{id}_{i:02}_{poster}_last.png"))?),
        None => None,
    };
    let vfile = format!("{}/{}", tx(&row["subfolder"]), tx(&row["filename"]));
    update(app, id, |p| {
        let result = json!({"video_id":row["id"], "video_file":vfile, "last_frame":last_in, "video_job":pid,
            "frame_image":if frame.is_empty() { Value::Null } else { json!(frame) }, "frame_id":seg["frame_id"], "frame_job":seg["frame_job"], "references":references,"status":"done", "error":""});
        for key in d::RESULT_KEYS { p["segments"][i][*key] = previous.get(*key).cloned().unwrap_or(Value::Null); }
        let take = crate::film_takes::record(&mut p["segments"][i], recipe, result, source_take.as_deref());
        if review {
            p["segments"][i]["status"] = json!("review");
        } else {
            let _ = crate::film_takes::select(&mut p["segments"][i], &take, d::RESULT_KEYS);
            d::invalidate_dependents(p, i);
        }
    })
    .await
    .map_err(|e| e.1)?;
    Ok(())
}

/// 跑一段：第一格（鎖定的圖 → 接上一段最後一格 → 畫一張）→ 影片
async fn run_seg(app: &Shared, id: &str, i: usize, review: bool, token: &CancellationToken) -> Step {
    let p = load(app, id).map_err(|e| e.1)?;
    let seg = p["segments"][i].clone();
    let previous = crate::film_takes::result_of(&seg, d::RESULT_KEYS);
    if tx(&seg["video"]).trim().is_empty() {
        return Err(format!("第 {} 段還沒有影片提示詞，先按「寫提示詞」", i + 1));
    }
    update_seg(app, id, i, |s| {
        s.insert("status".into(), json!("running"));
        s.insert("error".into(), json!(""));
    })
    .await
    .map_err(|e| e.1)?;
    let engine = shot_engine(app, &p, &seg);
    if !engine_needs_frame(app, &engine)? {
        if i > 0 && truthy(seg.get("continue")) { return Err("這個文生影片工作流沒有首幀輸入；請取消「接上一段」，或換成支援首幀的工作流".into()); }
        update_seg(app, id, i, |s| { s.insert("frame_id".into(), Value::Null); s.insert("frame_image".into(), Value::Null); s.insert("frame_job".into(), Value::Null); }).await.map_err(|e| e.1)?;
        let p = load(app, id).map_err(|e| e.1)?;
        return make_video(app, id, i, &p, "", (review, &previous), token).await;
    }
    let mapping = if engine == crate::video_wf::BUILTIN { crate::video_wf::Mapping::default() } else {
        let wf = crate::video_wf::load(&crate::video_wf::dir(&app.paths.data), &engine)?;
        crate::video_wf::Mapping::from_value(&wf["map"])
    };
    let frame = if let Some(file) = frame_source(&p, i, &mapping)? {
        crate::video_wf::media_basename(&file, "image")?;
        if !app.paths.input.join(&file).is_file() { return Err("指定或接續的第一格找不到，請重新選圖或先採用上一鏡".into()); }
        if i > 0 && truthy(seg.get("continue")) { emit(app, id, Some(i), "frame", format!("第 {} 段：接上一段採用版本的最後一格", i + 1)); }
        update_seg(app, id, i, |s| { s.insert("frame_id".into(), Value::Null); s.insert("frame_job".into(), Value::Null); }).await.map_err(|e| e.1)?;
        file
    } else {
        let (name, fid) = make_frame(app, id, i, &p, &d::new_id(), token).await?;
        update_seg(app, id, i, |s| {
            s.insert("frame_id".into(), fid);
        })
        .await
        .map_err(|e| e.1)?;
        name
    };
    // 指定圖與接續尾幀也凍結成這一次拍攝的獨立檔名，避免日後換圖影響舊版配方。
    let frame = to_input(app, &app.paths.input.join(&frame), &format!("{INPUT_PREFIX}{id}_{i:02}_{}_input.png", d::new_id()))?;
    update_seg(app, id, i, |s| {
        s.insert("frame_image".into(), json!(frame));
    })
    .await
    .map_err(|e| e.1)?;
    let p = load(app, id).map_err(|e| e.1)?;
    make_video(app, id, i, &p, &frame, (review, &previous), token).await
}

/// 全部段落的影片照順序接成一支（ComfyUI 的 ConcatenateVideo，不重新編碼），存進圖庫
async fn concat(app: &Shared, id: &str, token: &CancellationToken) -> Step {
    let p = load(app, id).map_err(|e| e.1)?;
    let segs = p["segments"].as_array().cloned().unwrap_or_default();
    if segs.is_empty() {
        return Err("沒有段落".into());
    }
    for (i, seg) in segs.iter().enumerate() {
        if !d::ready_for_concat(seg) { return Err(format!("第 {} 鏡尚未採用完成的版本，請先審片", i + 1)); }
    }
    // 段落影片都沒換、長片也還在圖庫：沿用（ComfyUI 對一模一樣的工作流會走快取、不產生新檔）
    let vids: Vec<Value> = segs.iter().map(|s| s["video_id"].clone()).collect();
    let trims: Vec<Value> = segs.iter().map(|s| s["trim_out"].clone()).collect();
    let selected: Vec<Value> = segs.iter().map(|s| s["selected_take"].clone()).collect();
    let fin = &p["final"];
    let old_ok = fin["id"].as_i64().and_then(|f| app.db.image(f).ok().flatten()).is_some_and(|r| file_of(&app.paths, &r).exists());
    if old_ok && fin["videos"].as_array() == Some(&vids) && fin["trims"].as_array() == Some(&trims) && fin["takes"].as_array() == Some(&selected) {
        emit(app, id, None, "concat", "段落沒有變，沿用已經接好的長片".to_string());
        return Ok(());
    }
    let mut files = vec![];
    for (i, s) in segs.iter().enumerate() {
        if !d::ready_for_concat(s) { return Err(format!("第 {} 鏡尚未採用完成的版本，請先審片", i + 1)); }
        let vid = s["video_id"].as_i64().ok_or_else(|| format!("第 {} 段還沒有影片", i + 1))?;
        let row = app.db.image(vid).map_err(|e| e.to_string())?.ok_or_else(|| format!("第 {} 段的影片不在圖庫了", i + 1))?;
        let src = file_of(&app.paths, &row);
        if !src.exists() {
            return Err(format!("第 {} 段的影片檔不見了", i + 1));
        }
        if let Some(out) = s["trim_out"].as_f64() {
            if !out.is_finite() || out <= 0.0 { return Err(format!("第 {} 鏡的裁切出點不正確", i + 1)); }
            let name = format!("{INPUT_PREFIX}{id}_{i:02}_v{vid}_trim{}.mp4", (out * 1_000_000.0).round() as u64);
            let dest = app.paths.input.join(&name);
            if !dest.is_file() {
                let tools = crate::studio::tools(&app.paths.data);
                blocking(move || -> Step {
                    let part = dest.with_extension("part.mp4");
                    if let Err(e) = crate::video_review::trim(&tools, &src, &part, out) { let _ = std::fs::remove_file(&part); return Err(e); }
                    std::fs::rename(part, dest).map_err(|e| e.to_string())
                }).await?;
            }
            files.push(name);
        } else if !s["trim_out"].is_null() {
            return Err(format!("第 {} 鏡的裁切出點不正確", i + 1));
        } else {
            files.push(to_input(app, &src, &format!("{INPUT_PREFIX}{id}_{i:02}_v{vid}.mp4"))?);
        }
    }
    emit(app, id, None, "concat", format!("把 {} 段接成一支…", files.len()));
    let mut nodes = Map::new();
    let mut cat = Map::new();
    for (i, f) in files.iter().enumerate() {
        nodes.insert(format!("v{i}"), json!({"class_type": "LoadVideo", "inputs": {"file": f}}));
        cat.insert(format!("videos.video{i}"), json!([format!("v{i}"), 0]));
    }
    cat.insert("codec".into(), json!("auto"));
    nodes.insert("cat".into(), json!({"class_type": "ConcatenateVideo", "inputs": cat}));
    nodes.insert(
        "save".into(),
        json!({"class_type": "SaveVideo", "inputs": {"video": ["cat", 0], "filename_prefix": "AnimaWeb/director", "format": "auto", "format.codec": "auto"}}),
    );
    // 封面沿用第一段影片的
    let first = app.db.image(segs[0]["video_id"].as_i64().unwrap_or(0)).ok().flatten();
    let poster = first.as_ref().map(|r| r["params"]["poster"].clone()).unwrap_or(Value::Null);
    let (w, h) = first.as_ref().map(|r| (r["width"].clone(), r["height"].clone())).unwrap_or((Value::Null, Value::Null));
    let title = tx(&p["title"]);
    let summary: Vec<String> = segs.iter().enumerate().map(|(i, s)| format!("{}. {}", i + 1, tx(&s["title"]))).collect();
    let params = json!({"mode": "video", "model": "導演台", "prompt": format!("導演台：{title}"), "poster": poster,
                        "director": {"id": id, "kind": "final"}, "idea": summary.join(" / ")});
    let secs: f64 = segs.iter().map(|s| s["trim_out"].as_f64().or_else(|| s["seconds"].as_f64()).or_else(|| p["settings"]["seconds"].as_f64()).unwrap_or(5.0)).sum();
    let info = json!({"prompt": format!("導演台：{title}（{} 段，約 {secs:.0} 秒）\n{}", segs.len(), summary.join("\n")),
                      "negative": "", "seed": 0, "width": w, "height": h, "mode": "video", "idea": summary.join(" / ")});
    let pid = app
        .comfy
        .submit(&Value::Object(nodes), &json!({"anima_params": params}))
        .await
        .map_err(|e| e.0)?;
    app.db.add_job(&pid, &params, &info).map_err(|e| e.to_string())?;
    app.comfy.sync_job(&pid).await;
    wait_job(app, &pid, token).await?;
    let row = match job_image(app, &pid) {
        Ok(r) => r,
        Err(e) => return Err(e),
    };
    update(app, id, |p| {
        p["final"] = json!({"id": row["id"], "at": now(), "segments": files.len(), "videos": vids, "trims":trims, "takes":selected});
    })
    .await
    .map_err(|e| e.1)?;
    Ok(())
}

/// {id, from?: 從第幾段開始（預設 0）, only?: 只跑這一段, concat?: 跑完要不要接成長片（預設要）, skip_done?: 已經有影片的段跳過（預設要）}
pub async fn run(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    migrated(&app).await?;
    let edit_guard = app.director.lock.lock().await;
    let mut p = load(&app, &id)?;
    if app.director.running(&id) { return Err(bad("這個專案已經在執行了")); }
    freeze_legacy(&app, &mut p).map_err(bad)?;
    let n = p["segments"].as_array().map(|a| a.len()).unwrap_or(0);
    if n == 0 {
        return Err(bad("還沒有段落：先按「AI 分段」"));
    }
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線"));
    }
    let only = b.get("only").and_then(crate::py::int).map(|i| i as usize);
    let review = b.get("review").and_then(Value::as_bool).or_else(|| p["settings"]["review"].as_bool()).unwrap_or(false);
    if only.is_none() && p["segments"].as_array().is_some_and(|a| a.iter().any(|s| !s["pending_take"].is_null())) {
        return Err(bad("有鏡頭還沒審片，請先採用其中一版再繼續"));
    }
    let from = b.get("from").and_then(crate::py::int).unwrap_or(0).max(0) as usize;
    let do_concat = only.is_none() && b.get("concat").map(|v| truthy(Some(v))).unwrap_or(true);
    let skip_done = b.get("skip_done").map(|v| truthy(Some(v))).unwrap_or(true);
    let todo: Vec<usize> = match only {
        Some(i) if i < n => vec![i],
        Some(_) => return Err(bad("沒有這一段")),
        None => (from.min(n)..n).collect(),
    };
    let token = begin(&app, &id)?;
    if let Err(e) = app.director.store.save_project(&mut p) { end(&app, &id); return Err(internal(e)); }
    drop(edit_guard);
    let (a, id2, t2) = (app.clone(), id.clone(), token.clone());
    spawn(&app, &id, token, async move {
        let (app, id, token) = (a, id2, t2);
        for i in todo {
            if token.is_cancelled() {
                return Err("已停止".into());
            }
            let p = load(&app, &id).map_err(|e| e.1)?;
            let seg = &p["segments"][i];
            if skip_done && only.is_none() && seg["video_id"].as_i64().is_some() && tx(&seg["status"]) == "done" {
                continue;
            }
            if i > 0 && truthy(seg.get("continue")) && !d::ready_for_concat(&p["segments"][i - 1]) {
                return Err(format!("第 {} 鏡尚未採用，不能接著拍第 {} 鏡", i, i + 1));
            }
            let previous = crate::film_takes::result_of(seg, d::RESULT_KEYS);
            if let Err(e) = run_seg(&app, &id, i, review, &token).await {
                let msg = if token.is_cancelled() { "已停止".to_string() } else { e.clone() };
                let _ = update_seg(&app, &id, i, |s| {
                    for key in d::RESULT_KEYS { s.insert((*key).into(), previous.get(*key).cloned().unwrap_or(Value::Null)); }
                    s.insert("status".into(), json!(if token.is_cancelled() { "stopped" } else { "error" }));
                    s.insert("error".into(), json!(msg));
                })
                .await;
                return Err(format!("第 {} 段：{e}", i + 1));
            }
            emit(&app, &id, Some(i), "seg", format!("第 {} 段完成", i + 1));
            if review { return Ok(()); }
        }
        if do_concat {
            concat(&app, &id, &token).await?;
        }
        Ok(())
    });
    ok(json!({"ok": true}))
}

/// 只把現有的影片接起來
pub async fn concat_api(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]);
    let edit_guard = app.director.lock.lock().await;
    load(&app, &id)?;
    let token = begin(&app, &id)?;
    drop(edit_guard);
    let (a, id2, t2) = (app.clone(), id.clone(), token.clone());
    spawn(&app, &id, token, async move { concat(&a, &id2, &t2).await });
    ok(json!({"ok": true}))
}

#[cfg(test)]
mod take_integration_tests {
    use super::*;

    /// 內建影片（H3，舊專案記的 ltx23 也算）一定要首幀；舊版本記的 ltx23 讀出來是 h3；專案選雲端才走雲端
    #[test]
    fn builtin_engine_is_h3() {
        assert_eq!(builtin_needs_frame("h3"), Some(true));
        assert_eq!(builtin_needs_frame("ltx23"), Some(true));
        assert_eq!(builtin_needs_frame("my-wf"), None);
        let p = json!({"settings":{"video_engine":"ltx23"}});
        assert_eq!(engine_of(&p, &json!({}), "x"), "h3");
        assert_eq!(engine_of(&json!({}), &json!({"engine":"ltx23"}), "x"), "h3");
        assert_eq!(engine_of(&json!({}), &json!({}), "ltx23"), "h3");
        assert_eq!(engine_of(&p, &json!({"engine":"my-wf"}), "h3"), "my-wf");
        assert!(!h3_cloud(&json!({"settings":{}})));
        assert!(h3_cloud(&json!({"settings":{"h3_where":"cloud"}})));
    }

    fn mapping() -> crate::video_wf::Mapping {
        crate::video_wf::Mapping::from_value(&json!({"media_slots":[
            {"key":"first","label":"首幀","kind":"image","role":"first","node":"1","input":"image"},
            {"key":"actor","label":"角色","kind":"image","role":"character","node":"2","input":"image","who":"S1"}
        ]}))
    }

    fn project() -> Value {
        json!({"settings":{"characters":[]},"sheet":{"cast":[{"speaker":"S1","name":"Ada"},{"speaker":"S2","name":"Ben"}],
            "refs":{"on":true,"items":[{"who":"S1","file":"ada.png"},{"who":"S2","file":"ben.png"}]}},
            "segments":[{"characters":["Ada"],"frame_lock":"locked.png","media_slots":{"first":"chosen.png"}},
                {"characters":["Ada"],"continue":true,"frame_lock":"wrong_lock.png","media_slots":{"first":"wrong_slot.png"}}]})
    }

    #[test]
    fn explicit_first_slot_skips_an_unnecessary_generated_frame() {
        let p = project();
        assert_eq!(frame_source(&p, 0, &mapping()).unwrap(), Some("chosen.png".into()));
        let bound = bind_video_slots(&p, &p["segments"][0], &mapping(), "frozen_chosen.png");
        assert_eq!(bound["first"], "frozen_chosen.png");
        let applied = applied_video_slots(&Value::Null, &mapping(), &bound);
        assert_eq!(applied["first"]["file"], "frozen_chosen.png");
    }

    #[test]
    fn continuation_first_always_matches_the_selected_predecessor() {
        let mut p = project();
        p["segments"][0]["last_frame"] = json!("adopted_tail.png");
        assert_eq!(frame_source(&p, 1, &mapping()).unwrap(), Some("adopted_tail.png".into()));
        let bound = bind_video_slots(&p, &p["segments"][1], &mapping(), "frozen_adopted_tail.png");
        assert_eq!(bound["first"], "frozen_adopted_tail.png", "槽位不得再換回另選的首幀");
        p["segments"][0]["last_frame"] = Value::Null;
        assert!(frame_source(&p, 1, &mapping()).is_err(), "尾幀遺失不能偷用指定圖破壞接續");
    }

    #[test]
    fn disabled_sheet_references_do_not_leak_into_the_video_or_qwen_frame() {
        let mut p = project();
        p["sheet"]["refs"]["on"] = json!(false);
        assert!(enabled_sheet_refs(&p, &p["segments"][0]).is_empty());
        let bound = bind_video_slots(&p, &p["segments"][0], &mapping(), "first.png");
        assert!(!bound.contains_key("actor"));
        assert!(frame_reference_list(families::QWEN21, &p, &[], &p["segments"][0]).is_empty());
    }

    #[test]
    fn sheet_character_references_reach_qwen_without_legacy_character_ids() {
        let p = project();
        let refs = frame_reference_list(families::QWEN21, &p, &[], &p["segments"][0]);
        assert_eq!(refs, vec![("Ada".into(), "ada.png".into())]);
        assert!(frame_reference_list(families::ANIMA, &p, &[], &p["segments"][0]).is_empty());
        let bound = bind_video_slots(&p, &p["segments"][0], &mapping(), "first.png");
        assert_eq!(bound["actor"], "ada.png");
    }

    #[test]
    fn empty_per_shot_slots_clear_other_version_project_defaults() {
        let mut p = project();
        p["settings"]["media_slots"] = json!({"first":"old_version.png"});
        p["segments"][0]["media_slots"] = json!({});
        p["segments"][0]["frame_lock"] = Value::Null;
        assert!(shot_slots(&p, &p["segments"][0]).is_empty());
        assert_eq!(frame_source(&p, 0, &mapping()).unwrap(), None);
    }
}

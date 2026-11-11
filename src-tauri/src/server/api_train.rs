//! 訓練台（煉 LoRA）API。純邏輯（專案檔、家族表、參數、描述、設定檔）在 crate::train；設計在 design\訓練台_設計.md。
//!
//! 專案
//!
//! - GET    /api/train：{projects, families, trainer, vram_gb}
//! - POST   /api/train {name, kind, family, trigger?}：新專案（沒給觸發詞就自動產生）
//! - GET    /api/train/{id}：{project, report, captions, running, family}
//! - POST   /api/train/{id} {name?, kind?, trigger?, level?, params?, samples?}
//! - DELETE /api/train/{id} {confirm:true}：搬到 待刪除\訓練專案\
//!
//! 訓練圖（一律複製進專案資料夾，原圖不動）
//! - POST /api/train/{id}/images {paths:[檔或資料夾]} | {gallery_ids:[…]} | {file_name, data_b64}
//! - POST /api/train/{id}/images/set {files, off}：勾掉／勾回
//! - POST /api/train/{id}/images/remove {files}
//! - GET  /api/train/{id}/img/{file}
//!
//! 描述
//! - POST /api/train/{id}/caption {mode: auto|tags|sentence, only_empty}：背景打標
//! - POST /api/train/{id}/captions {file, text} | {add} | {drop:[…]}
//! - GET  /api/train/{id}/traits：角色每張都有的固定外觀（建議拿掉）
//! - GET  /api/train/{id}/params?level=light|std|full：自動算的參數＋樣張提示詞（不存檔）
//!
//! 訓練（在 train_run.rs）
//! - POST /api/train/{id}/start {target: local|remote|runpod|civitai|modelscope, confirm?, …}
//! - POST /api/train/{id}/stop
//!
//! 進度：websocket {"type":"train","id","phase":"caption|train|install","state":"running|done|error|stopped", …}
use super::{bad, blocking, file_response, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::director::tx;
use crate::train::{self, Caption, Fam};
use axum::extract::{Path as UrlPath, Query, State};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use tokio_util::sync::CancellationToken;

/// 專案檔「讀 → 改 → 寫」排隊（網頁存檔和背景工作會同時寫）
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 正在跑的背景工作：專案代號 → (種類 caption｜train, 取消鈕)。一個專案同時只有一個
static JOBS: LazyLock<Mutex<HashMap<String, (&'static str, CancellationToken)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

const IMAGE_EXT: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
const VIDEO_EXT: [&str; 4] = ["mp4", "mov", "webm", "mkv"];
const MAX_IMPORT: usize = 3000;

pub(super) fn root(app: &Shared) -> PathBuf {
    train::dir(&app.paths.data)
}

pub(super) fn pdir(app: &Shared, id: &str) -> PathBuf {
    root(app).join(id)
}

pub(super) fn load(app: &Shared, id: &str) -> Result<Value, ApiError> {
    if !train::id_ok(id) {
        return Err(bad("專案代號不對"));
    }
    train::load(&root(app), id).map_err(not_found)
}

pub(super) fn fam_of(p: &Value) -> Result<&'static Fam, ApiError> {
    train::fam(p["family"].as_str().unwrap_or("")).ok_or_else(|| bad("這個專案的模型家族不認得"))
}

/// 讀 → 改 → 寫。f 回 Err 就不存
pub(super) async fn update(app: &Shared, id: &str, f: impl FnOnce(&mut Value) -> Result<(), String>) -> Result<Value, ApiError> {
    let _g = LOCK.lock().await;
    let mut p = load(app, id)?;
    f(&mut p).map_err(bad)?;
    p["updated"] = json!(chrono::Utc::now().timestamp());
    train::save(&root(app), &p).map_err(internal)?;
    Ok(p)
}

pub(super) fn job_kind(id: &str) -> Option<&'static str> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|j| j.0)
}

/// 登記背景工作；這個專案已經有工作在跑就回 Err
pub(super) fn job_begin(id: &str, kind: &'static str) -> Result<CancellationToken, ApiError> {
    let mut j = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, _)) = j.get(id) {
        return Err(bad(if *k == "train" { "這個專案已經在訓練了" } else { "這個專案正在打標，等它做完或先按停止" }));
    }
    let t = CancellationToken::new();
    j.insert(id.to_string(), (kind, t.clone()));
    Ok(t)
}

pub(super) fn job_end(id: &str) {
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

/// 有沒有任何專案正在訓練（本機一次只能煉一顆）
pub(super) fn training_id() -> Option<String> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(_, j)| j.0 == "train").map(|(k, _)| k.clone())
}

fn busy(id: &str) -> Result<(), ApiError> {
    match job_kind(id) {
        Some("train") => Err(bad("訓練中不能改，先按停止")),
        Some(_) => Err(bad("正在打標，等它做完或先按停止")),
        None => Ok(()),
    }
}

pub(super) fn emit(app: &Shared, id: &str, phase: &str, state: &str, extra: Value) {
    let mut m = json!({"type": "train", "id": id, "phase": phase, "state": state});
    if let (Some(o), Some(e)) = (m.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    app.hub.send(m);
}

pub(super) fn images(p: &Value) -> &[Value] {
    p["images"].as_array().map_or(&[], Vec::as_slice)
}

pub(super) fn live(p: &Value) -> Vec<&Value> {
    images(p).iter().filter(|i| i["off"].as_bool() != Some(true)).collect()
}

fn has_file(p: &Value, file: &str) -> bool {
    images(p).iter().any(|i| i["file"].as_str() == Some(file))
}

pub(super) fn cap_path(dir: &Path, file: &str) -> PathBuf {
    dir.join("captions").join(Path::new(file).with_extension("txt"))
}

pub(super) fn read_caption(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(cap_path(dir, file)).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn write_caption(dir: &Path, file: &str, text: &str) -> Result<(), String> {
    let p = cap_path(dir, file);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, text).map_err(|e| format!("描述存不進去：{e}"))
}

fn captions(dir: &Path, p: &Value) -> Map<String, Value> {
    images(p).iter().filter_map(|i| i["file"].as_str()).map(|f| (f.to_string(), json!(read_caption(dir, f)))).collect()
}

fn fam_view(f: &Fam) -> Value {
    json!({
        "id": f.id, "label": f.label, "caption": if f.caption == Caption::Tags { "tags" } else { "sentence" }, "video": f.video,
        "base": f.base_hint, "base_note": f.base_note, "min_vram_gb": f.min_vram_gb, "rank": f.rank,
        "civitai": f.civitai.is_some(), "modelscope": f.modelscope.is_some(), "gate": f.gate,
    })
}

fn view(app: &Shared, p: &Value) -> Value {
    let id = p["id"].as_str().unwrap_or("");
    let dir = pdir(app, id);
    let mut shown = p.clone();
    train::fix_stale(&mut shown, job_kind(id) == Some("train"));
    json!({
        "project": shown,
        "report": train::dataset_report(images(p), p["kind"].as_str().unwrap_or("style")),
        "captions": captions(&dir, p),
        "running": job_kind(id),
        "family": train::fam(p["family"].as_str().unwrap_or("")).map(fam_view),
    })
}

fn summary(p: &Value) -> Value {
    let id = p["id"].as_str().unwrap_or("");
    let mut shown = p.clone();
    train::fix_stale(&mut shown, job_kind(id) == Some("train"));
    let p = &shown;
    json!({
        "id": id, "name": p["name"], "kind": p["kind"], "family": p["family"], "trigger": p["trigger"],
        "created": p["created"], "updated": p["updated"], "images": live(p).len(),
        "state": p["run"]["state"], "target": p["run"]["target"], "step": p["run"]["step"], "total": p["run"]["total"],
        "adopted": p["adopted"], "running": job_kind(id),
    })
}

/// 這台機器最大那張 NVIDIA 顯卡的顯存（GB）；沒有就 0
pub(super) fn vram_gb() -> f64 {
    let (gpus, _) = crate::gpu::detect(false);
    gpus.iter().filter(|g| g["vendor"] == "nvidia").filter_map(|g| g["vram_mb"].as_f64()).fold(0.0, f64::max) / 1024.0
}

pub async fn list(State(app): State<Shared>) -> ApiResult {
    let dir = root(&app);
    let (projects, vram) = blocking(move || (train::list(&dir), vram_gb())).await;
    ok(json!({
        "projects": projects.iter().map(summary).collect::<Vec<_>>(),
        "families": train::FAMS.iter().map(fam_view).collect::<Vec<_>>(),
        "trainer": super::train_run::trainer_view(&app),
        "vram_gb": (vram * 10.0).round() / 10.0,
        "training": training_id(),
    }))
}

pub async fn create(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let (name, kind, family) = (tx(&b["name"]), tx(&b["kind"]), tx(&b["family"]));
    let mut trigger = tx(&b["trigger"]).trim().to_string();
    if trigger.is_empty() {
        trigger = train::suggest_trigger(&name, &kind, rand::random_range(0..10_000u32));
    }
    let p = train::new_project(&name, &kind, &family, &trigger).map_err(bad)?;
    let _g = LOCK.lock().await;
    train::save(&root(&app), &p).map_err(internal)?;
    ok(view(&app, &p))
}

pub async fn get(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    ok(view(&app, &load(&app, &id)?))
}

/// 使用者改過的參數併進自動算的那一份（只收認得的欄位、夾在合理範圍）
fn merge_params(base: &mut Value, user: &Value) {
    let num = |k: &str, lo: f64, hi: f64| user.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(lo, hi));
    for (k, lo, hi) in [("steps", 50.0, 20000.0), ("epochs", 1.0, 30.0), ("rank", 1.0, 128.0), ("alpha", 1.0, 128.0), ("batch", 1.0, 8.0)] {
        if let Some(v) = num(k, lo, hi) {
            base[k] = json!(v.round() as u64);
        }
    }
    if let Some(v) = num("lr", 1e-6, 1e-2) {
        base["lr"] = json!(v);
    }
    if let Some(v) = num("caption_dropout", 0.0, 0.5) {
        base["caption_dropout"] = json!(v);
    }
    if let Some(v) = user.get("flip").and_then(Value::as_bool) {
        base["flip"] = json!(v);
    }
    if let Some(o) = user.get("optimizer").and_then(Value::as_str).filter(|o| ["adamw8bit", "adamw", "prodigy"].contains(o)) {
        base["optimizer"] = json!(o);
    }
    if let Some(r) = user.get("resolution").and_then(Value::as_array) {
        let r: Vec<u64> = r.iter().filter_map(Value::as_u64).filter(|v| (256..=2048).contains(v)).take(4).collect();
        if !r.is_empty() {
            base["resolution"] = json!(r);
        }
    }
    // 使用者動過步數或版數，那句算式就不準了
    if user.get("steps").is_some() || user.get("epochs").is_some() {
        let (steps, epochs) = (base["steps"].as_u64().unwrap_or(1), base["epochs"].as_u64().unwrap_or(1).max(1));
        base["why"] = json!(format!("你自己設的：{steps} 步，每 {} 步存一版，共 {epochs} 版", (steps / epochs).max(1)));
    }
}

pub async fn set(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    busy(&id)?;
    let dir = pdir(&app, &id);
    let p = update(&app, &id, |p| {
        if let Some(n) = b.get("name").and_then(Value::as_str) {
            let n = n.trim();
            if n.is_empty() || n.chars().count() > 60 {
                return Err("專案名字要 1～60 個字".into());
            }
            p["name"] = json!(n);
        }
        if let Some(k) = b.get("kind").and_then(Value::as_str) {
            if !train::KINDS.contains(&k) {
                return Err("煉什麼只能選畫風、角色、概念".into());
            }
            p["kind"] = json!(k);
        }
        let f = train::fam(p["family"].as_str().unwrap_or("")).ok_or("這個專案的模型家族不認得")?;
        if let Some(t) = b.get("trigger").and_then(Value::as_str) {
            if !train::trigger_ok(t) {
                return Err("觸發詞只能用英文、數字、底線、連字號，2～32 個字".into());
            }
            let old = p["trigger"].as_str().unwrap_or("").to_string();
            if old != t {
                // 描述開頭的舊觸發詞跟著換
                for file in images(p).iter().filter_map(|i| i["file"].as_str()) {
                    let c = read_caption(&dir, file);
                    if c.is_empty() {
                        continue;
                    }
                    let rest = c.strip_prefix(old.as_str()).map(|r| r.trim_start_matches([',', ' '])).unwrap_or(&c);
                    write_caption(&dir, file, &train::with_trigger(rest, t, f.caption == Caption::Tags))?;
                }
                p["trigger"] = json!(t);
            }
        }
        if let Some(l) = b.get("level").and_then(Value::as_str).filter(|l| ["light", "std", "full"].contains(l)) {
            p["level"] = json!(l);
        }
        if b.get("params").is_some() || b.get("level").is_some() {
            let n = live(p).len();
            let mut auto = train::auto_params(f, p["kind"].as_str().unwrap_or("style"), n, p["level"].as_str().unwrap_or("std"));
            if let Some(u) = b.get("params").filter(|u| u.is_object()) {
                merge_params(&mut auto, u);
            }
            p["params"] = auto;
        }
        if let Some(s) = b.get("samples").and_then(Value::as_array) {
            let s: Vec<String> = s.iter().filter_map(Value::as_str).map(|x| x.trim().chars().take(600).collect::<String>()).filter(|x| !x.is_empty()).take(8).collect();
            p["samples"] = json!(s);
        }
        Ok(())
    })
    .await?;
    ok(view(&app, &p))
}

pub async fn delete(State(app): State<Shared>, UrlPath(id): UrlPath<String>, body: axum::body::Bytes) -> ApiResult {
    load(&app, &id)?;
    if serde_json::from_slice::<Value>(&body).ok().and_then(|b| b["confirm"].as_bool()) != Some(true) {
        return Err(bad("刪除訓練專案要帶 confirm: true（專案會搬到「待刪除」，煉好的 LoRA 不會動）"));
    }
    busy(&id)?;
    let _g = LOCK.lock().await;
    let to = app.paths.root.join("待刪除").join("訓練專案");
    std::fs::create_dir_all(&to).map_err(internal)?;
    std::fs::rename(pdir(&app, &id), to.join(format!("{id}_{}", chrono::Local::now().format("%Y%m%d_%H%M%S")))).map_err(|e| internal(format!("搬到待刪除失敗：{e}")))?;
    ok(json!({"ok": true}))
}

// ================================================================ 訓練圖

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

/// 把使用者給的路徑攤開：資料夾只看第一層、只收認得的副檔名，照檔名排
fn expand(paths: &[PathBuf], video: bool) -> Vec<PathBuf> {
    let wanted = |p: &Path| {
        let e = ext_of(p);
        IMAGE_EXT.contains(&e.as_str()) || (video && VIDEO_EXT.contains(&e.as_str()))
    };
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut inner: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|f| f.is_file() && wanted(f)).collect();
            inner.sort();
            out.extend(inner);
        } else {
            out.push(p.clone());
        }
    }
    out.truncate(MAX_IMPORT);
    out
}

/// 一個檔整理進專案：圖片走 train::import_image，影片（影片家族才收）原樣複製
fn import_one(src: &Path, dst: &Path, index: usize, video: bool) -> Result<Value, String> {
    let ext = ext_of(src);
    if video && VIDEO_EXT.contains(&ext.as_str()) {
        let size = std::fs::metadata(src).map_err(|_| "找不到這個檔".to_string())?.len();
        if size == 0 {
            return Err("檔案是空的".into());
        }
        if size > 500 * 1024 * 1024 {
            return Err("影片超過 500MB，請先剪成幾秒一段的短片".into());
        }
        std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
        let file = format!("{index:04}.{ext}");
        std::fs::copy(src, dst.join(&file)).map_err(|e| format!("複製失敗：{e}"))?;
        return Ok(json!({"file": file, "video": true, "size": size, "small": false,
            "src_name": src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}));
    }
    if !src.is_file() {
        return Err("找不到這個檔".into());
    }
    train::import_image(src, dst, index)
}

pub async fn images_add(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    busy(&id)?;
    let p0 = load(&app, &id)?;
    let video = fam_of(&p0)?.video;
    let dir = pdir(&app, &id);
    // (來源, 顯示名, 用完要刪的暫存檔)
    let mut sources: Vec<(PathBuf, Option<String>, bool)> = Vec::new();
    if let Some(a) = b.get("paths").and_then(Value::as_array) {
        let ps: Vec<PathBuf> = a.iter().filter_map(Value::as_str).map(PathBuf::from).collect();
        sources.extend(blocking(move || expand(&ps, video)).await.into_iter().map(|p| (p, None, false)));
    }
    for gid in b.get("gallery_ids").and_then(Value::as_array).into_iter().flatten().filter_map(crate::py::int) {
        let r = app.db.image(gid)?.ok_or_else(|| not_found("找不到這個圖庫項目"))?;
        sources.push((super::api_gallery::file_of(&app.paths, &r), None, false));
    }
    if let Some(data) = b.get("data_b64").and_then(Value::as_str) {
        let name = super::basename(&tx(&b["file_name"]));
        let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|_| bad("檔案內容讀不懂（base64 壞了）"))?;
        let tmp = dir.join(format!("_upload_{}.{}", train::new_id(), ext_of(Path::new(&name))));
        std::fs::create_dir_all(&dir).map_err(internal)?;
        std::fs::write(&tmp, bytes).map_err(internal)?;
        sources.push((tmp, Some(name), true));
    }
    if sources.is_empty() {
        return Err(bad("沒有給圖：paths（檔案或資料夾）、gallery_ids 或 file_name＋data_b64 擇一"));
    }
    let mut skipped = Vec::new();
    let mut added = 0usize;
    let p = {
        let _g = LOCK.lock().await;
        let mut p = load(&app, &id)?;
        let mut seq = p["seq"].as_u64().unwrap_or(0) as usize;
        let img_dir = dir.join("images");
        let (items, skips, seq2) = blocking(move || {
            let (mut items, mut skips) = (Vec::new(), Vec::new());
            for (src, name, temp) in sources {
                let shown = name.clone().unwrap_or_else(|| src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                match import_one(&src, &img_dir, seq + 1, video) {
                    Ok(mut item) => {
                        seq += 1;
                        if let Some(n) = name {
                            item["src_name"] = json!(n);
                        }
                        items.push(item);
                    }
                    Err(why) => skips.push(json!({"name": shown, "why": why})),
                }
                if temp {
                    let _ = std::fs::remove_file(&src);
                }
            }
            (items, skips, seq)
        })
        .await;
        added += items.len();
        skipped.extend(skips);
        if let Some(a) = p["images"].as_array_mut() {
            a.extend(items);
        } else {
            p["images"] = json!(items);
        }
        p["seq"] = json!(seq2);
        p["updated"] = json!(chrono::Utc::now().timestamp());
        train::save(&root(&app), &p).map_err(internal)?;
        p
    };
    let mut v = view(&app, &p);
    v["added"] = json!(added);
    v["skipped"] = json!(skipped);
    ok(v)
}

fn files_of(b: &Value) -> Vec<String> {
    b["files"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

pub async fn images_set(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    busy(&id)?;
    let (files, off) = (files_of(&b), b["off"].as_bool().unwrap_or(true));
    let p = update(&app, &id, |p| {
        for i in p["images"].as_array_mut().into_iter().flatten() {
            if i["file"].as_str().is_some_and(|f| files.iter().any(|x| x == f)) {
                i["off"] = json!(off);
            }
        }
        Ok(())
    })
    .await?;
    ok(view(&app, &p))
}

pub async fn images_remove(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    busy(&id)?;
    let files = files_of(&b);
    let dir = pdir(&app, &id);
    let p = update(&app, &id, |p| {
        let Some(a) = p["images"].as_array_mut() else { return Ok(()) };
        a.retain(|i| {
            let Some(f) = i["file"].as_str().filter(|f| files.iter().any(|x| x == f)) else { return true };
            // 專案資料夾裡的是整理過的副本（原圖沒動），直接刪
            let _ = std::fs::remove_file(dir.join("images").join(f));
            let _ = std::fs::remove_file(cap_path(&dir, f));
            false
        });
        Ok(())
    })
    .await?;
    ok(view(&app, &p))
}

pub async fn img(State(app): State<Shared>, UrlPath((id, file)): UrlPath<(String, String)>) -> ApiResult {
    let p = load(&app, &id)?;
    if !has_file(&p, &file) {
        return Err(not_found("找不到這張訓練圖"));
    }
    file_response(pdir(&app, &id).join("images").join(file), None).await
}

// ================================================================ 描述

pub async fn captions_set(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    busy(&id)?;
    let _g = LOCK.lock().await;
    let p = load(&app, &id)?;
    let tags = fam_of(&p)?.caption == Caption::Tags;
    let trigger = p["trigger"].as_str().unwrap_or("").to_string();
    let dir = pdir(&app, &id);
    if let Some(file) = b.get("file").and_then(Value::as_str) {
        if !has_file(&p, file) {
            return Err(bad("沒有這張訓練圖"));
        }
        let text: String = tx(&b["text"]).split_whitespace().collect::<Vec<_>>().join(" ");
        write_caption(&dir, file, &train::with_trigger(&text, &trigger, tags)).map_err(internal)?;
    } else if let Some(add) = b.get("add").and_then(Value::as_str).map(str::trim).filter(|a| !a.is_empty()) {
        if add.contains(['\n', '\r']) {
            return Err(bad("要加的詞不能換行"));
        }
        for file in live(&p).iter().filter_map(|i| i["file"].as_str()) {
            let c = read_caption(&dir, file);
            let has = c.split(',').any(|t| t.trim().eq_ignore_ascii_case(add));
            let next = if has { c } else if c.is_empty() { add.to_string() } else { format!("{c}, {add}") };
            write_caption(&dir, file, &train::with_trigger(&next, &trigger, tags)).map_err(internal)?;
        }
    } else if let Some(drop) = b.get("drop").and_then(Value::as_array) {
        let drop: Vec<String> = drop.iter().filter_map(Value::as_str).map(str::to_string).filter(|d| !d.eq_ignore_ascii_case(&trigger)).collect();
        for file in images(&p).iter().filter_map(|i| i["file"].as_str()) {
            let c = read_caption(&dir, file);
            if !c.is_empty() {
                write_caption(&dir, file, &train::with_trigger(&train::drop_tags(&c, &drop), &trigger, tags)).map_err(internal)?;
            }
        }
    } else {
        return Err(bad("要給 file＋text、add 或 drop"));
    }
    ok(view(&app, &p))
}

pub async fn traits(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let p = load(&app, &id)?;
    let dir = pdir(&app, &id);
    let trigger = p["trigger"].as_str().unwrap_or("");
    let caps: Vec<Vec<String>> = live(&p)
        .iter()
        .filter_map(|i| i["file"].as_str())
        .map(|f| read_caption(&dir, f))
        .filter(|c| !c.is_empty())
        .map(|c| c.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && t != trigger).collect())
        .collect();
    let t = if fam_of(&p)?.caption == Caption::Tags { train::fixed_traits(&caps, 0.8) } else { Vec::new() };
    ok(json!({"traits": t.iter().map(|(tag, r)| json!({"tag": tag, "ratio": (r * 100.0).round() / 100.0})).collect::<Vec<_>>(), "captioned": caps.len()}))
}

pub async fn params(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let p = load(&app, &id)?;
    let f = fam_of(&p)?;
    let kind = p["kind"].as_str().unwrap_or("style");
    let level = q.get("level").map(String::as_str).filter(|l| ["light", "std", "full"].contains(l)).unwrap_or_else(|| p["level"].as_str().unwrap_or("std"));
    ok(json!({
        "level": level,
        "params": train::auto_params(f, kind, live(&p).len(), level),
        "samples": train::sample_prompts(f, kind, p["trigger"].as_str().unwrap_or("")),
        "saved": p["params"],
        "saved_samples": p["samples"],
    }))
}

fn pairs(v: &Value) -> Vec<(String, f64)> {
    v.as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_f64().unwrap_or(0.0)))).collect()
}

/// 背景打標。tag 家族＝站內反推（WD）＋自動清理；句子家族＝站內語言模型看圖寫英文句子
pub async fn caption(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let p = load(&app, &id)?;
    let f = fam_of(&p)?;
    let tags = match b["mode"].as_str().unwrap_or("auto") {
        "tags" => true,
        "sentence" => false,
        _ => f.caption == Caption::Tags,
    };
    let only_empty = b["only_empty"].as_bool().unwrap_or(true);
    let dir = pdir(&app, &id);
    let todo: Vec<String> = live(&p)
        .iter()
        .filter_map(|i| i["file"].as_str())
        .filter(|f| !only_empty || read_caption(&dir, f).is_empty())
        .map(str::to_string)
        .collect();
    if live(&p).is_empty() {
        return Err(bad("還沒有訓練圖，先到上一步放圖"));
    }
    if todo.is_empty() {
        return ok(json!({"ok": true, "total": 0, "msg": "每張都已經有描述了"}));
    }
    if f.video && tags {
        return Err(bad("影片素材只能用句子描述"));
    }
    if tags && !app.tagger.available() {
        return Err(bad("反推模型還沒下載：到生成頁按一次「反推」會自動下載，裝好再回來打標"));
    }
    let cancel = job_begin(&id, "caption")?;
    let total = todo.len();
    let (kind, trigger) = (p["kind"].as_str().unwrap_or("style").to_string(), p["trigger"].as_str().unwrap_or("").to_string());
    let (app2, id2) = (app.clone(), id.clone());
    tokio::spawn(async move {
        let (app, id) = (app2, id2);
        let mut failed: Vec<String> = Vec::new();
        let mut stopped = false;
        for (n, file) in todo.iter().enumerate() {
            if cancel.is_cancelled() {
                stopped = true;
                break;
            }
            emit(&app, &id, "caption", "running", json!({"done": n, "total": total, "file": file}));
            let path = dir.join("images").join(file);
            let text: Result<String, String> = if tags {
                let (tg, k) = (app.tagger.clone(), kind.clone());
                blocking(move || tg.tag(&path, 0.35, 0.85).map(|v| train::tags_caption(&pairs(&v["general"]), &pairs(&v["character"]), &k))).await
            } else {
                sentence(&app, &id, &path, file, &kind, f.video, &cancel).await
            };
            match text {
                Ok(t) if !t.trim().is_empty() => {
                    let _g = LOCK.lock().await;
                    if let Err(e) = write_caption(&dir, file, &train::with_trigger(&t, &trigger, tags)) {
                        failed.push(format!("{file}：{e}"));
                    }
                }
                Ok(_) => failed.push(format!("{file}：寫出來是空的")),
                Err(e) => {
                    failed.push(format!("{file}：{e}"));
                    // 連第一張都失敗多半是模型沒準備好，不用整包試完
                    if n == 0 {
                        break;
                    }
                }
            }
        }
        job_end(&id);
        let state = if stopped { "stopped" } else if failed.len() == total || (failed.len() == 1 && total > 1 && failed[0].starts_with(&todo[0])) { "error" } else { "done" };
        let msg = if failed.is_empty() { String::new() } else { format!("有 {} 張沒寫成：{}", failed.len(), failed.iter().take(3).cloned().collect::<Vec<_>>().join("；")) };
        emit(&app, &id, "caption", state, json!({"done": total - failed.len(), "total": total, "msg": msg}));
    });
    ok(json!({"ok": true, "total": total, "mode": if tags { "tags" } else { "sentence" }}))
}

/// 句子描述：把圖（影片取不到畫面，就只給檔名說明）放進 ComfyUI input 給語言模型看
async fn sentence(app: &Shared, id: &str, path: &Path, file: &str, kind: &str, video: bool, cancel: &CancellationToken) -> Result<String, String> {
    if VIDEO_EXT.contains(&ext_of(path).as_str()) {
        return Err("影片還不能自動寫描述，請自己寫一句英文（講內容、動作、鏡頭）".into());
    }
    let name = format!("train_cap_{id}_{file}");
    std::fs::create_dir_all(&app.paths.input).map_err(|e| e.to_string())?;
    std::fs::copy(path, app.paths.input.join(&name)).map_err(|e| format!("圖放不進 ComfyUI input：{e}"))?;
    let rid = format!("train-cap-{id}");
    let system = train::caption_system(kind, video);
    let r = tokio::select! {
        r = app.pe.general_chat(&rid, &system, crate::pe::CAPTION_ASK, std::slice::from_ref(&name), 400, 0.3) => r,
        () = cancel.cancelled() => {
            app.pe.cancel(&rid);
            Err("已停止".into())
        }
    };
    let _ = std::fs::remove_file(app.paths.input.join(&name));
    r.map(|t| t.split_whitespace().collect::<Vec<_>>().join(" ").trim_matches('"').to_string())
}

pub async fn stop(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    load(&app, &id)?;
    let j = JOBS.lock().unwrap_or_else(|e| e.into_inner()).get(&id).map(|j| j.1.clone());
    match j {
        Some(t) => {
            t.cancel();
            ok(json!({"ok": true}))
        }
        None => super::train_run::stop_remote(&app, &id).await,
    }
}

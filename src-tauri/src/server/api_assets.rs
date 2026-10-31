//! 資產庫：資產（角色、場景、道具…）的圖和聲音檔。
//! 資產本身是 prompt_lib 的一列（kind 見 api_lib::MEDIA_KINDS），data 只記檔名和用途（api_lib::clean_asset）；
//! 檔案放 data\assets\ 這個平的資料夾，檔名 <資產 id>_<n>.<副檔名>。
//! 不真的刪：拿掉的檔案、刪掉的整筆資產一律搬到 <root>\待刪除\資產庫\<名字>_<時間>\
use super::api_gallery::{file_of, is_video, small_webp};
use super::api_lib::{item_from, text, MEDIA_KINDS, ROLES};
use super::{bad, basename, blocking, file_response, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::db::{Row, Thumb};
use crate::paths::Paths;
use crate::shotsheet::layout_line;
use axum::extract::{Path as UrlPath, State};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp"];
const AUDIO_EXT: &[&str] = &["mp3", "wav", "flac", "ogg", "m4a"];
/// 縮圖跟提示詞庫一樣：長邊 320、webp 品質 80
const THUMB_MAX: u32 = 320;

pub fn assets_dir(paths: &Paths) -> PathBuf {
    paths.data.join("assets")
}

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

/// data 裡記著的所有檔名（圖片＋聲音）
fn files_of(data: &Value) -> Vec<String> {
    let mut v: Vec<String> = data["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|im| im["file"].as_str().map(str::to_owned))
        .collect();
    if let Some(a) = data["audio"]["file"].as_str() {
        v.push(a.to_owned());
    }
    if let Some(r) = data["refmod"]["file"].as_str() {
        v.push(r.to_owned());
    }
    v
}

/// 這筆資產下一個沒人用的檔名：<id>_<n>.<ext>，n 從 1 開始
fn next_name(dir: &Path, id: i64, ext: &str, data: &Value) -> String {
    let used = files_of(data);
    (1..)
        .map(|n| format!("{id}_{n}.{ext}"))
        .find(|f| !used.contains(f) && !dir.join(f).exists())
        .unwrap_or_default()
}

/// 待刪除\資產庫\<名字>_<時間>\（同一秒撞名就加 _2、_3…）；Windows 檔名不能有的字換成 _
fn trash_dir(root: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let base = root
        .join("待刪除")
        .join("資產庫")
        .join(format!("{}_{}", safe.trim(), chrono::Local::now().format("%Y%m%d_%H%M%S")));
    let mut dir = base.clone();
    let mut n = 2;
    while dir.exists() {
        dir = PathBuf::from(format!("{}_{n}", base.display()));
        n += 1;
    }
    dir
}

/// 搬檔：跨磁碟 rename 會失敗，改成複製再刪
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
}

/// 讀出一筆資產（不是資產類型的回 400）
fn asset_row(app: &Shared, id: i64) -> Result<Row, ApiError> {
    let row = app
        .db
        .lib_item(id)?
        .ok_or_else(|| not_found("找不到這個資產（可能已經刪掉了）"))?;
    let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
    if !MEDIA_KINDS.contains(&kind) {
        return Err(bad("這一筆不是資產（整套設定、片段不能放檔案）"));
    }
    Ok(row)
}

fn row_name(row: &Row) -> String {
    row.get("name").and_then(Value::as_str).unwrap_or("").to_string()
}

/// 只換 data，其他欄位照舊
fn save_data(app: &Shared, id: i64, row: &Row, data: Value, thumb: &Thumb) -> Result<(), ApiError> {
    let it = item_from(&json!({ "data": data }), Some(row))?;
    app.db
        .lib_save(Some(id), &it, thumb)?
        .ok_or_else(|| not_found("找不到這個資產（可能已經刪掉了）"))?;
    Ok(())
}

/// 先搬舊檔，存檔失敗就搬回。讀回回應放在提交之後，不能把「讀回失敗」當作「沒存成」。
fn save_with_trash(app: &Shared, id: i64, row: &Row, data: Value, thumb: &Thumb, old: &[String]) -> Result<(), ApiError> {
    let moved = trash_files(&app.paths, &row_name(row), old).map_err(internal)?;
    if let Err(e) = save_data(app, id, row, data, thumb) {
        if let Err(restore) = restore_files(&moved) {
            return Err(internal(format!("{}；還原資產檔失敗：{restore}", e.1)));
        }
        return Err(e);
    }
    Ok(())
}

/// 把一個檔加進資產（不上鎖，呼叫的人要拿著 assets_lock；會碰硬碟，放在 blocking 裡跑）。
/// role＝圖片用途（src、sheet…），"audio"＝聲音。先複製成功才記進 data；
/// 這筆還沒有縮圖、加的又是圖片，就順便做縮圖。回傳整筆資產
pub fn attach_file(app: &Shared, id: i64, src: &Path, role: &str) -> Result<Value, ApiError> {
    store_file(app, id, src, role, false)
}

fn store_file(app: &Shared, id: i64, src: &Path, role: &str, replace: bool) -> Result<Value, ApiError> {
    let row = asset_row(app, id)?;
    let audio = role == "audio";
    if !src.is_file() {
        return Err(not_found("找不到這個檔案"));
    }
    let ext = ext_of(src);
    if audio && !AUDIO_EXT.contains(&ext.as_str()) {
        return Err(bad("聲音只收 mp3、wav、flac、ogg、m4a"));
    }
    if !audio && !IMAGE_EXT.contains(&ext.as_str()) {
        return Err(bad("圖片只收 png、jpg、webp"));
    }
    let mut data = row.get("data").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    let dir = assets_dir(&app.paths);
    let file = next_name(&dir, id, &ext, &data);
    let dst = dir.join(&file);
    std::fs::create_dir_all(&dir).map_err(internal)?;
    if let Err(e) = std::fs::copy(src, &dst) {
        let _ = std::fs::remove_file(&dst); // 複製到一半失敗的副本
        return Err(internal(e));
    }
    let old = if audio {
        data["audio"]["file"].as_str().map(str::to_owned)
    } else if replace {
        first_image(&data, role)
    } else {
        None
    };
    if audio {
        data["audio"] = json!({ "file": file });
    } else {
        if !data["images"].is_array() {
            data["images"] = json!([]);
        }
        if let Some(a) = data["images"].as_array_mut() {
            let new = json!({ "file": file, "role": role });
            if let Some(i) = old.as_ref().and_then(|f| a.iter().position(|im| im["file"] == f.as_str())) {
                a[i] = new;
            } else {
                a.push(new);
            }
        }
    }
    let need_thumb = !audio && row.get("has_thumb").and_then(Value::as_i64).unwrap_or(0) == 0;
    let thumb = match need_thumb.then(|| small_webp(&dst, THUMB_MAX, 80.0)) {
        Some(Ok(webp)) => Thumb::Set(STANDARD.encode(webp)),
        _ => Thumb::Keep,
    };
    let old: Vec<String> = old.into_iter().collect();
    if let Err(e) = save_with_trash(app, id, &row, data, &thumb, &old) {
        // lib_save 的 transaction 已回滾；只清掉這次自己複製的新檔。
        let _ = std::fs::remove_file(&dst);
        return Err(e);
    }
    Ok(json!(asset_row(app, id)?))
}

/// 從資產拿掉一個檔（不上鎖，同 attach_file）：搬到待刪除成功才改 data；存檔失敗搬回
pub fn detach_file(app: &Shared, id: i64, file: &str) -> Result<Value, ApiError> {
    let row = asset_row(app, id)?;
    let file = basename(file);
    let mut data = row.get("data").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    if file.is_empty() || !files_of(&data).contains(&file) {
        return Err(not_found("這個資產沒有這個檔案"));
    }
    if data["audio"]["file"].as_str() == Some(file.as_str()) {
        if let Some(o) = data.as_object_mut() {
            o.remove("audio");
        }
    }
    if let Some(a) = data["images"].as_array_mut() {
        a.retain(|im| im["file"].as_str() != Some(file.as_str()));
    }
    save_with_trash(app, id, &row, data, &Thumb::Keep, &[file])?;
    Ok(json!(asset_row(app, id)?))
}

/// 這個用途的第一張圖（導演台角色卡的「原圖」「設定圖」）
pub fn first_image(data: &Value, role: &str) -> Option<String> {
    data["images"]
        .as_array()?
        .iter()
        .find(|im| im["role"] == role)
        .and_then(|im| im["file"].as_str())
        .map(str::to_owned)
}

/// 換掉這個用途的第一張圖（不上鎖，同 attach_file）：新的放在舊的那個位置，舊的搬到待刪除；原本沒有就加在最後
pub fn replace_image(app: &Shared, id: i64, src: &Path, role: &str) -> Result<Value, ApiError> {
    store_file(app, id, src, role, true)
}

fn restore_files(moved: &[(PathBuf, PathBuf)]) -> std::io::Result<()> {
    for (from, to) in moved.iter().rev() {
        move_file(to, from).map_err(|e| std::io::Error::other(format!("{} → {}：{e}", to.display(), from.display())))?;
    }
    Ok(())
}

/// 搬不動就回復先前已搬的檔，讓呼叫端保留原本的 data。
fn trash_files(paths: &Paths, name: &str, files: &[String]) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let dir = assets_dir(paths);
    let to = trash_dir(&paths.root, name);
    let mut moved = vec![];
    for f in files {
        let from = dir.join(basename(f));
        if !from.try_exists()? {
            continue;
        }
        let dst = to.join(basename(f));
        if let Err(e) = std::fs::create_dir_all(&to).and_then(|_| move_file(&from, &dst)) {
            restore_files(&moved)?;
            return Err(e);
        }
        moved.push((from, dst));
    }
    Ok(moved)
}

/// 加一個檔：{input: ComfyUI input 的檔名} 或 {gallery_id: 圖庫那張}，role 見 attach_file；
/// replace＝true 時換掉這個用途的第一張（舊的搬待刪除），角色設定圖工坊重做設定圖用
pub async fn add_file(State(app): State<Shared>, UrlPath(id): UrlPath<i64>, Body(body): Body) -> ApiResult {
    let _g = app.assets_lock.lock().await;
    let role = text(body.get("role"), 20);
    let src = if let Some(gid) = body.get("gallery_id").and_then(Value::as_i64) {
        let r = app.db.image(gid)?.ok_or_else(|| not_found("圖庫裡找不到這張圖"))?;
        if is_video(r.get("filename").and_then(Value::as_str).unwrap_or("")) {
            return Err(bad("影片不能當資產圖"));
        }
        file_of(&app.paths, &r)
    } else {
        let name = basename(&text(body.get("input"), 300));
        if name.is_empty() {
            return Err(bad("缺少 input 或 gallery_id"));
        }
        app.paths.input.join(name)
    };
    let replace = crate::py::truthy(body.get("replace"));
    let a = app.clone();
    ok(blocking(move || if replace { replace_image(&a, id, &src, &role) } else { attach_file(&a, id, &src, &role) }).await?)
}

/// 拿掉一個檔：{file}
pub async fn remove_file(State(app): State<Shared>, UrlPath(id): UrlPath<i64>, Body(body): Body) -> ApiResult {
    let _g = app.assets_lock.lock().await;
    let file = text(body.get("file"), 300);
    let a = app.clone();
    ok(blocking(move || detach_file(&a, id, &file)).await?)
}

pub async fn get_file(State(app): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult {
    let name = basename(&name);
    if name.is_empty() {
        return Err(not_found("找不到"));
    }
    file_response(assets_dir(&app.paths).join(name), Some("max-age=3600")).await
}

/// 資產的一個檔複製到 ComfyUI input（檔名 aw_asset_<檔名>），回應跟 /api/to_input 一樣 {name, width, height}
pub async fn to_input(State(app): State<Shared>, UrlPath(id): UrlPath<i64>, Body(body): Body) -> ApiResult {
    let row = asset_row(&app, id)?;
    let file = basename(&text(body.get("file"), 300));
    let data = row.get("data").cloned().unwrap_or(Value::Null);
    if file.is_empty() || !files_of(&data).contains(&file) {
        return Err(not_found("這個資產沒有這個檔案"));
    }
    let src = assets_dir(&app.paths).join(&file);
    if !src.exists() {
        return Err(not_found("資產的檔案不在硬碟上了"));
    }
    let name = format!("aw_asset_{file}");
    let dst = app.paths.input.join(&name);
    let is_img = IMAGE_EXT.contains(&ext_of(&src).as_str());
    let (w, h) = blocking(move || -> std::io::Result<(u32, u32)> {
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::copy(&src, &dst)?;
        if is_img {
            image::image_dimensions(&dst).map_err(std::io::Error::other)
        } else {
            Ok((0, 0))
        }
    })
    .await
    .map_err(internal)?;
    ok(json!({"name": name, "width": w, "height": h}))
}

/// 刪整筆資產之前：整筆的 JSON（資產.json）和所有檔案搬到 待刪除\資產庫\<名字>_<時間>\
pub fn trash_asset(paths: &Paths, row: &Row) -> std::io::Result<PathBuf> {
    let to = trash_dir(&paths.root, &row_name(row));
    std::fs::create_dir_all(&to)?;
    std::fs::write(to.join("資產.json"), serde_json::to_vec_pretty(row).unwrap_or_default())?;
    let dir = assets_dir(paths);
    for f in files_of(row.get("data").unwrap_or(&Value::Null)) {
        let from = dir.join(basename(&f));
        if from.is_file() {
            move_file(&from, &to.join(basename(&f)))?;
        }
    }
    Ok(to)
}

// ---------------------------------------------------------------- 拿去用：照模型轉換
/// 圖片照用途排：src → sheet → face → sheet_nude → detail → pose → outfit → other（同用途照原本順序）
fn images_by_role(data: &Value) -> Vec<String> {
    let mut v: Vec<(usize, String)> = data["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|im| {
            let rank = ROLES.iter().position(|r| im["role"] == *r).unwrap_or(ROLES.len());
            im["file"].as_str().map(|f| (rank, f.to_owned()))
        })
        .collect();
    v.sort_by_key(|(rank, _)| *rank);
    v.into_iter().map(|(_, f)| f).collect()
}

/// H3 和助手要放幾張：角色＝正面特寫（沒有就原圖）＋多視圖，其他＝前 2 張。
/// 參考圖每一步取樣都要跟著算（「match」時跟畫面一樣大），不是越多越好；裸體多視圖、表情表不自動放，要的人到素材板自己換（2026-10-30）
fn few_refs(kind: &str, data: &Value, all: &[String]) -> Vec<String> {
    if kind != "char" {
        return all.iter().take(2).cloned().collect();
    }
    let mut out: Vec<String> = first_image(data, "portrait").or_else(|| first_image(data, "src")).into_iter().collect();
    out.extend(first_image(data, "sheet"));
    if out.is_empty() {
        out.extend(all.iter().find(|f| data["images"].as_array().is_none_or(|a| !a.iter().any(|im| im["file"] == f.as_str() && im["role"] == "sheet_nude"))).cloned());
    }
    out
}

/// 這幾張圖各是什麼用途：{檔名: role}（H3 素材板寫「臉在第幾張、全身在第幾張」用）
fn views_of(data: &Value, images: &[String]) -> Value {
    let list = data["images"].as_array().cloned().unwrap_or_default();
    let map: serde_json::Map<String, Value> = images
        .iter()
        .filter_map(|f| list.iter().find(|im| im["file"] == f.as_str()).map(|im| (f.clone(), im["role"].clone())))
        .collect();
    Value::Object(map)
}

/// 一筆資產拿去給某個模型家族用：{text, images（資產檔名）, views, audio, warn, voice, lang}。
/// 規則見設計文件 4.5：anima＝tag＋外觀英文、全部圖給前端選底圖或控制圖；moss 只有文字；
/// qwen21 最多 10 張參考圖、超過 4 張提醒；h3 中文外觀、少量參考圖（few_refs）、帶聲音語言和音檔；agent（助手）中文外觀、少量參考圖；
/// comic＝tag＋外觀；其他（含以前導演台用的 ltx）只有外觀文字
pub fn apply_plan(item: &Row, family: &str) -> Value {
    let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
    let null = Value::Null;
    let data = item.get("data").unwrap_or(&null);
    let s = |k: &str| data[k].as_str().unwrap_or("").trim().to_string();
    if kind == "snippet" || kind == "set" {
        let t = if kind == "snippet" { s("text") } else { s("prompt") };
        return json!({"text": t, "images": [], "warn": [], "voice": "", "lang": ""});
    }
    let (look_en, look_zh) = (s("look_en"), s("look_zh"));
    let either = |a: &str, b: &str| if a.is_empty() { b.to_string() } else { a.to_string() };
    let tags: Vec<&str> = data["tags_anima"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let tag_text = [tags.join(", "), look_en.clone()]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(". ");
    let all = images_by_role(data);
    let mut warn: Vec<String> = vec![];
    // 角色卡鎖住的設定表（2026-10-27）：文字用固定的那一段，參考圖只送正面特寫＋多視圖（圖文雙管）
    let locked = if kind == "char" { crate::charspec::locked_blocks(data) } else { None };
    if let Some((bz, be, bt)) = &locked {
        let pick = |a: &str, b: &str| if a.is_empty() { b.to_string() } else { a.to_string() };
        let fixed: Vec<String> = ["portrait", "sheet"].iter().filter_map(|r| first_image(data, r)).collect();
        let fixed = if fixed.is_empty() { all.iter().take(2).cloned().collect() } else { fixed };
        let (text, images) = match family {
            "anima" => (pick(bt, be), all.clone()),
            "comic" => (pick(bt, be), vec![]),
            "moss" => (pick(be, bz), vec![]),
            "qwen21" => (pick(be, bz), fixed),
            "h3" | "agent" => (pick(bz, be), fixed),
            _ => (pick(be, bz), vec![]),
        };
        let av = family == "h3";
        return json!({"text": text, "views": views_of(data, &images), "images": images, "warn": warn, "locked": true, "spec": {"zh": bz, "en": be, "tags": bt},
                      "refmod": data["refmod"]["name"].as_str().unwrap_or(""), "audio": if av { data["audio"]["file"].clone() } else { Value::Null },
                      "voice": if av { s("voice") } else { String::new() }, "lang": if av { s("lang") } else { String::new() }});
    }
    let (mut text, images, zh) = match family {
        "anima" => (tag_text, all.clone(), false),
        "comic" => (tag_text, vec![], false),
        "moss" => (either(&look_en, &look_zh), vec![], true),
        "qwen21" => {
            if all.len() > 10 {
                warn.push(format!("參考圖有 {} 張：最多 10 張，只送前 10 張；超過 4 張細節會變少", all.len()));
            } else if all.len() > 4 {
                warn.push(format!("參考圖有 {} 張：超過 4 張，細節會變少", all.len()));
            }
            (either(&look_en, &look_zh), all.iter().take(10).cloned().collect(), true)
        }
        "h3" | "agent" => (either(&look_zh, &look_en), few_refs(kind, data, &all), true),
        _ => (either(&look_en, &look_zh), vec![], false),
    };
    if text.is_empty() {
        text = s("text");
    }
    let line = layout_line(&data["layout"], zh);
    if !line.is_empty() {
        text = if text.is_empty() { line } else { format!("{text}{}{line}", if zh { "。" } else { ". " }) };
    }
    let av = family == "h3";
    json!({"text": text, "views": views_of(data, &images), "images": images, "warn": warn, "locked": false, "audio": if av { data["audio"]["file"].clone() } else { Value::Null },
           "voice": if av { s("voice") } else { String::new() }, "lang": if av { s("lang") } else { String::new() }})
}

/// 角色做 RefMod 用哪張圖：正面特寫 → 原圖 → 第一張
fn refmod_source(data: &Value) -> Option<String> {
    first_image(data, "portrait")
        .or_else(|| first_image(data, "src"))
        .or_else(|| data["images"].as_array().and_then(|a| a.first()).and_then(|im| im["file"].as_str()).map(str::to_owned))
}

/// POST：角色的正面特寫做成 RefMod（本機 ComfyUI，只用 H3 影片 VAE 編碼），檔案收進資產（data.refmod）；回整筆資產。
/// 重做會換成新檔，舊檔搬到待刪除
pub async fn make_refmod(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    let row = asset_row(&app, id)?;
    if row.get("kind").and_then(Value::as_str) != Some("char") {
        return Err(bad("只有角色能做 RefMod"));
    }
    let file = refmod_source(row.get("data").unwrap_or(&Value::Null)).ok_or_else(|| bad("這個角色還沒有圖：先到角色卡工坊畫正面特寫"))?;
    super::api_refmod::require_nodes(&app).await?;
    let input = format!("aw_asset_{file}");
    let (src, dst) = (assets_dir(&app.paths).join(&file), app.paths.input.join(&input));
    blocking(move || std::fs::copy(&src, &dst)).await.map_err(|_| not_found("資產的圖不在硬碟上了"))?;
    let name = crate::refmod::mod_name(id);
    let full = crate::refmod::loader_name(&name);
    let graph = crate::refmod::extract_graph(&input, &name, crate::h3::VAE, &row_name(&row));
    app.pe.release_for_generation().await;
    let pid = app.comfy.submit(&Value::Object(graph), &json!({})).await.map_err(|e| bad(e.0))?;
    super::api_play::wait_history(&app, &pid, "做 RefMod ").await.map_err(bad)?;
    let made = super::api_refmod::local_path(&app, &full);
    if !made.is_file() {
        return Err(bad(format!("ComfyUI 說做好了，卻找不到 {}（refmods 資料夾被設定到別處了？）", made.display())));
    }
    let _g = app.assets_lock.lock().await;
    let row = asset_row(&app, id)?;
    let mut data = row.get("data").cloned().unwrap_or_else(|| json!({}));
    let dir = assets_dir(&app.paths);
    let keep = next_name(&dir, id, "safetensors", &data);
    let old: Vec<String> = data["refmod"]["file"].as_str().map(|f| vec![f.to_string()]).unwrap_or_default();
    let to = dir.join(&keep);
    blocking(move || -> std::io::Result<u64> {
        std::fs::create_dir_all(&dir)?;
        std::fs::copy(&made, &to)
    })
    .await
    .map_err(internal)?;
    data["refmod"] = json!({"file": keep, "name": full});
    save_with_trash(&app, id, &row, data, &Thumb::Keep, &old)?;
    let r = app.db.lib_item(id)?.ok_or_else(|| not_found("找不到這個資產（可能已經刪掉了）"))?;
    ok(Value::Object(r))
}

/// 拿去用：{family} → apply_plan 的結果，並記一次「用過」
pub async fn apply(State(app): State<Shared>, UrlPath(id): UrlPath<i64>, Body(body): Body) -> ApiResult {
    let row = app.db.lib_item(id)?.ok_or_else(|| not_found("找不到這個資產（可能已經刪掉了）"))?;
    let plan = apply_plan(&row, &text(body.get("family"), 30));
    app.db.lib_used(id)?;
    ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &str, data: Value) -> Row {
        let mut r = Row::new();
        r.insert("kind".into(), json!(kind));
        r.insert("name".into(), json!("x"));
        r.insert("data".into(), data);
        r
    }

    fn twelve() -> Value {
        let roles = ["other", "detail", "sheet", "src", "pose", "outfit", "sheet_nude", "other", "detail", "other", "pose", "other"];
        json!(roles.iter().enumerate().map(|(i, r)| json!({"file": format!("1_{}.png", i + 1), "role": r})).collect::<Vec<_>>())
    }

    #[test]
    fn apply_anima_gives_tags_and_look() {
        let it = item("char", json!({"tags_anima": ["orange hair", "hair flower"], "look_en": "an adult woman",
                                      "images": [{"file": "1_1.png", "role": "sheet"}, {"file": "1_2.png", "role": "src"}], "voice": "soft"}));
        let p = apply_plan(&it, "anima");
        assert_eq!(p["text"], "orange hair, hair flower. an adult woman");
        assert_eq!(p["images"], json!(["1_2.png", "1_1.png"]), "全部圖片，src 排前面");
        assert_eq!(p["voice"], "", "生圖不帶聲音");
    }

    #[test]
    fn apply_moss_text_only() {
        let it = item("char", json!({"look_zh": "橘髮", "images": [{"file": "1_1.png", "role": "src"}]}));
        let p = apply_plan(&it, "moss");
        assert_eq!(p["images"], json!([]));
        assert_eq!(p["text"], "橘髮", "沒有英文就用中文");
    }

    #[test]
    fn apply_qwen_caps_10_warns_over_4() {
        let it = item("char", json!({"look_en": "a woman", "images": twelve()}));
        let p = apply_plan(&it, "qwen21");
        assert_eq!(p["images"].as_array().unwrap().len(), 10);
        assert_eq!(p["images"][0], "1_4.png", "src 排第一");
        assert_eq!(p["images"][1], "1_3.png", "接著 sheet");
        assert_eq!(p["warn"].as_array().unwrap().len(), 1);
        let few = item("char", json!({"images": [{"file": "a.png", "role": "src"}, {"file": "b.png", "role": "sheet"}]}));
        assert_eq!(apply_plan(&few, "qwen21")["warn"], json!([]));
    }

    #[test]
    fn apply_h3_has_voice_lang() {
        let it = item("char", json!({"look_zh": "橘髮女生", "look_en": "orange hair", "voice": "slightly low", "lang": "ja", "images": twelve()}));
        let p = apply_plan(&it, "h3");
        assert_eq!(p["text"], "橘髮女生");
        assert_eq!((p["voice"].as_str(), p["lang"].as_str()), (Some("slightly low"), Some("ja")));
        // 不是全塞：原圖＋穿衣多視圖，裸體多視圖和其他的不自動放
        assert_eq!(p["images"], json!(["1_4.png", "1_3.png"]));
        assert_eq!((p["views"]["1_4.png"].as_str(), p["views"]["1_3.png"].as_str()), (Some("src"), Some("sheet")));
        let portrait = item("char", json!({"images": [{"file": "a.png", "role": "src"}, {"file": "b.png", "role": "portrait"}, {"file": "c.png", "role": "sheet"}, {"file": "d.png", "role": "face"}], "audio": {"file": "v.wav"}}));
        assert_eq!(apply_plan(&portrait, "h3")["images"], json!(["b.png", "c.png"]), "有正面特寫就用它");
        assert_eq!(apply_plan(&portrait, "h3")["audio"], "v.wav");
        assert_eq!(apply_plan(&portrait, "agent")["images"], json!(["b.png", "c.png"]));
        assert!(apply_plan(&portrait, "agent")["audio"].is_null(), "助手不用音檔");
        let only_nude = item("char", json!({"images": [{"file": "n.png", "role": "sheet_nude"}, {"file": "o.png", "role": "other"}]}));
        assert_eq!(apply_plan(&only_nude, "h3")["images"], json!(["o.png"]), "裸體多視圖不自動放");
        let scene = item("scene", json!({"images": [{"file": "1.png", "role": "src"}, {"file": "2.png", "role": "other"}, {"file": "3.png", "role": "other"}]}));
        assert_eq!(apply_plan(&scene, "h3")["images"], json!(["1.png", "2.png"]));
        // 2026-10-30 LTX 退場：舊的 ltx 家族（導演台以前用的）當一般家族，不帶圖、聲音
        let ltx = apply_plan(&it, "ltx");
        assert_eq!(ltx["images"], json!([]));
        assert_eq!(ltx["lang"], "");
        assert_eq!(apply_plan(&it, "comic")["voice"], "");
        assert_eq!(apply_plan(&it, "comic")["images"], json!([]), "漫畫只帶文字與 tag");
    }

    #[test]
    fn refmod_name_and_source_image() {
        let data = json!({"images": [{"file": "7_1.png", "role": "src"}, {"file": "7_2.png", "role": "portrait"}],
                          "refmod": {"file": "7_3.safetensors", "name": "paintdeck/pd_char_7"},
                          "spec": {"locked": true, "block_zh": "女性。", "block_en": "female.", "block_tags": "1girl"}});
        assert_eq!(refmod_source(&data).as_deref(), Some("7_2.png"));
        assert_eq!(refmod_source(&json!({"images": [{"file": "a.png", "role": "sheet"}]})).as_deref(), Some("a.png"));
        assert_eq!(refmod_source(&json!({})), None);
        let it = item("char", data);
        assert_eq!(apply_plan(&it, "h3")["refmod"], "paintdeck/pd_char_7");
    }

    #[test]
    fn apply_locked_spec_uses_fixed_text_and_portrait_sheet() {
        let spec = crate::charspec::merge(
            Some(&json!({"locked": true, "fields": {"sex": {"zh": "女性", "en": "female", "tags": ["1girl"]}, "hair_color": {"zh": "橘髮", "en": "orange hair", "tags": ["orange hair"]}}})),
            None,
        )
        .unwrap();
        let mut imgs = twelve();
        imgs.as_array_mut().unwrap().push(json!({"file": "p.png", "role": "portrait"}));
        let it = item("char", json!({"look_en": "OLD", "look_zh": "舊", "spec": spec, "images": imgs, "voice": "soft", "lang": "ja"}));
        let q = apply_plan(&it, "qwen21");
        assert_eq!(q["text"], "female, orange hair.");
        assert_eq!(q["images"], json!(["p.png", "1_3.png"]), "正面特寫＋多視圖");
        assert_eq!(q["locked"], true);
        assert_eq!(q["spec"]["zh"], "女性，橘髮。");
        let h = apply_plan(&it, "h3");
        assert_eq!((h["text"].as_str(), h["lang"].as_str()), (Some("女性，橘髮。"), Some("ja")));
        assert_eq!(h["images"], json!(["p.png", "1_3.png"]));
        assert_eq!(apply_plan(&it, "anima")["text"], "1girl, orange hair");
        assert_eq!(apply_plan(&it, "anima")["images"].as_array().unwrap().len(), 13, "Anima 照舊全部給前端選");
        assert_eq!(apply_plan(&it, "moss")["images"], json!([]));
        // 沒鎖：跟以前一樣用外觀描述
        let open = item("char", json!({"look_en": "OLD", "spec": {"locked": false, "fields": {}}, "images": twelve()}));
        assert_eq!(apply_plan(&open, "qwen21")["text"], "OLD");
        assert_eq!(apply_plan(&open, "qwen21")["locked"], false);
        // 只有一張原圖的鎖定角色：退回前兩張
        let few = item("char", json!({"spec": spec, "images": [{"file": "s.png", "role": "src"}]}));
        assert_eq!(apply_plan(&few, "h3")["images"], json!(["s.png"]));
    }

    #[test]
    fn apply_scene_layout_line() {
        let it = item("scene", json!({"look_zh": "教室", "layout": {"top": "書桌", "left": "窗戶"}}));
        let p = apply_plan(&it, "h3");
        let t = p["text"].as_str().unwrap();
        assert!(t.starts_with("教室"), "{t}");
        assert!(t.contains("上方 書桌") && t.contains("左邊 窗戶"), "{t}");
        let en = apply_plan(&item("scene", json!({"look_en": "classroom", "layout": {"top": "desk"}})), "anima");
        assert!(en["text"].as_str().unwrap().contains("top: desk"), "{en}");
        // 只有 text 的資產（特效、道具）
        let fx = apply_plan(&item("fx", json!({"text": "sparkles"})), "anima");
        assert_eq!(fx["text"], "sparkles");
        let sn = apply_plan(&item("snippet", json!({"text": "sunset, orange sky"})), "h3");
        assert_eq!(sn["text"], "sunset, orange sky");
        assert_eq!(sn["images"], json!([]));
    }
}

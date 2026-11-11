//! 提示詞庫：整套設定（生成頁表單）和片段，分資料夾、標籤、適用模型、置頂收藏，附縮圖。
//! 取代舊的「預設組」（presets 表原樣保留，第一次開啟新版時抄進來，見 db::lib_from_presets_sql）
//! 和存在瀏覽器裡的「我的片段」（網頁第一次打開時搬進來，見 ui/js/library.js）。
use super::api_gallery::{small_webp, thumb_source};
use super::{bad, blocking, internal, not_found, ok, ApiError, ApiResult, Body, Shared};
use crate::db::{Db, LibItem, Row, Thumb};
use crate::py::truthy;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

/// set、snippet 是提示詞庫原本的兩種；其他是資產庫的資產（可以帶圖、聲音）
pub const KINDS: &[&str] = &["set", "snippet", "char", "scene", "frame", "outfit", "prop", "fx", "sound", "shot"];
pub const MEDIA_KINDS: &[&str] = &["char", "scene", "frame", "outfit", "prop", "fx", "sound", "shot"];
/// 資產圖片的用途：原圖、正面特寫（2026-10-27 角色卡）、多視圖（穿衣／裸體）、臉部特寫表、細節、姿勢、服裝、其他
pub const ROLES: &[&str] = &["src", "portrait", "sheet", "face", "sheet_nude", "detail", "pose", "outfit", "other"];
const LAYOUT_KEYS: &[&str] = &["top", "bottom", "left", "right", "fg", "mid", "bg", "light"];
/// 縮圖：長邊 320、webp 品質 80（一張約 10～20 KB）
const THUMB_MAX: u32 = 320;
/// 匯入的縮圖最大多少（base64 字數），太大的不收
const THUMB_B64_MAX: usize = 2_000_000;

pub(super) fn text(v: Option<&Value>, max: usize) -> String {
    v.and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// 標籤：陣列或逗號分隔的字串都收；去空白、去重複
fn tags_of(v: Option<&Value>) -> Vec<String> {
    let raw: Vec<String> = match v {
        Some(Value::Array(a)) => a.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect(),
        Some(Value::String(s)) => s.split([',', '，', '、']).map(str::to_owned).collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = vec![];
    for t in raw {
        let t: String = t.trim().trim_start_matches('#').trim().chars().take(30).collect();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out.truncate(30);
    out
}

/// 只留檔名本身：`..\..\x.png`、`a/b/x.png` 都變 `x.png`；只剩 `.`、`..` 或空的回空字串
fn base_name(v: Option<&Value>) -> String {
    let s = text(v, 300);
    let name = s.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name == "." || name == ".." {
        String::new()
    } else {
        name.to_string()
    }
}

/// 資產的 data：不認得的鍵丟掉、檔名只取 basename、Anima tag 統一成小寫空白。
/// shot（拍攝單範本）原樣保留，格式由影片台決定
pub fn clean_asset(kind: &str, data: &Value) -> Value {
    if kind == "shot" {
        return data.clone();
    }
    let mut out = serde_json::Map::new();
    let images: Vec<Value> = data
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|im| {
            let file = base_name(im.get("file"));
            let role = text(im.get("role"), 20);
            let role = if ROLES.contains(&role.as_str()) { role } else { "other".into() };
            (!file.is_empty()).then(|| json!({"file": file, "role": role}))
        })
        .collect();
    if !images.is_empty() {
        out.insert("images".into(), json!(images));
    }
    let audio = base_name(data.get("audio").and_then(|a| a.get("file")));
    if !audio.is_empty() {
        out.insert("audio".into(), json!({"file": audio}));
    }
    for (k, max) in [("look_zh", 2000), ("look_en", 2000), ("text", 2000), ("voice", 300), ("lang", 300), ("cid", 300)] {
        let s = text(data.get(k), max);
        if !s.is_empty() {
            out.insert(k.into(), json!(s));
        }
    }
    let mut tags: Vec<String> = vec![];
    for t in data.get("tags_anima").and_then(Value::as_array).into_iter().flatten() {
        let t = text(Some(t), 300).replace('_', " ").to_lowercase();
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    }
    tags.truncate(60);
    if !tags.is_empty() {
        out.insert("tags_anima".into(), json!(tags));
    }
    let layout: serde_json::Map<String, Value> = LAYOUT_KEYS
        .iter()
        .filter_map(|k| {
            let s = text(data.get("layout").and_then(|l| l.get(*k)), 300);
            (!s.is_empty()).then(|| (k.to_string(), json!(s)))
        })
        .collect();
    if !layout.is_empty() {
        out.insert("layout".into(), Value::Object(layout));
    }
    // 角色卡：設定表（鎖定規則在存檔時 charspec::merge 決定）、RefMod 檔（只有 /api/assets/{id}/refmod 會改，存檔時照舊）
    if kind == "char" && data.get("spec").is_some_and(Value::is_object) {
        out.insert("spec".into(), crate::charspec::clean(&data["spec"]));
    }
    let refmod = base_name(data.get("refmod").and_then(|r| r.get("file")));
    if !refmod.is_empty() {
        out.insert("refmod".into(), json!({"file": refmod, "name": text(data["refmod"].get("name"), 80)}));
    }
    Value::Object(out)
}

/// 資產的圖片、聲音清單以伺服器上的為準（加檔、拿檔只走 /api/assets/*）：送來的清單只拿來改用途。
/// 免得舊畫面存檔蓋掉別處剛加的圖，或塞進別筆資產的檔名（刪這筆時會把別人的檔搬走）
fn keep_files(new: &mut Value, old: Option<&Value>) {
    let Some(obj) = new.as_object_mut() else { return };
    let sent: Vec<(String, String)> = obj
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|im| Some((im["file"].as_str()?.to_owned(), im["role"].as_str()?.to_owned())))
        .collect();
    let images: Vec<Value> = old
        .and_then(|o| o["images"].as_array())
        .into_iter()
        .flatten()
        .map(|im| {
            let f = im["file"].as_str().unwrap_or("");
            let role = sent.iter().find(|(sf, _)| sf == f).map(|(_, r)| r.as_str()).unwrap_or(im["role"].as_str().unwrap_or("other"));
            json!({"file": f, "role": role})
        })
        .collect();
    if images.is_empty() {
        obj.remove("images");
    } else {
        obj.insert("images".into(), json!(images));
    }
    match old.map(|o| &o["audio"]).filter(|a| a.is_object()) {
        Some(a) => obj.insert("audio".into(), a.clone()),
        None => obj.remove("audio"),
    };
}

/// 送來的欄位 → LibItem；沒送的欄位沿用 base（修改時是原本那一筆，所以可以只送要改的，例如只改收藏）
pub(super) fn item_from(body: &Value, base: Option<&Row>) -> Result<LibItem, ApiError> {
    let pick = |k: &str| body.get(k).or_else(|| base.and_then(|b| b.get(k)));
    let kind = text(pick("kind"), 20);
    let kind = if kind.is_empty() { "set".to_string() } else { kind };
    if !KINDS.contains(&kind.as_str()) {
        return Err(bad("不認得的類型"));
    }
    let name = text(pick("name"), 100);
    if name.is_empty() {
        return Err(bad("請輸入名稱"));
    }
    let data = pick("data")
        .filter(|d| d.is_object())
        .cloned()
        .ok_or_else(|| bad("缺少內容（data）"))?;
    if kind == "snippet" && text(data.get("text"), 1).is_empty() {
        return Err(bad("片段的內容是空的"));
    }
    let data = if MEDIA_KINDS.contains(&kind.as_str()) {
        clean_asset(&kind, &data)
    } else {
        data
    };
    Ok(LibItem {
        kind,
        name,
        folder: text(pick("folder"), 50),
        tags: tags_of(pick("tags")),
        family: text(pick("family"), 30),
        fav: truthy(pick("fav")),
        note: text(pick("note"), 1000),
        data,
    })
}

pub async fn list(State(app): State<Shared>) -> ApiResult {
    ok(json!(app.db.lib()?))
}

/// 圖庫那張圖 → 縮圖（webp 的 base64）
async fn thumb_of(app: &Shared, iid: i64) -> Result<String, String> {
    let src = thumb_source(app, iid).map_err(|e| e.1)?;
    let webp = blocking(move || small_webp(&src, THUMB_MAX, 80.0)).await?;
    Ok(STANDARD.encode(webp))
}

/// 新增或修改：有 id 是改那一筆（只送要改的欄位也可以）。
/// 縮圖：image_id＝拿圖庫那張圖做縮圖；thumb: null＝拿掉縮圖；都沒送＝不動。
/// 縮圖做不出來（圖檔不見了之類）照樣存，回傳 thumb_error 說明
pub async fn save(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let _g = app.assets_lock.lock().await;
    let id = body.get("id").and_then(Value::as_i64);
    let base = match id {
        Some(id) => Some(
            app.db
                .lib_item(id)?
                .ok_or_else(|| not_found("找不到這一筆（可能已經刪掉了）"))?,
        ),
        None => None,
    };
    let mut it = item_from(&body, base.as_ref())?;
    let base_kind = base.as_ref().and_then(|b| b.get("kind")).and_then(Value::as_str).unwrap_or("");
    if MEDIA_KINDS.contains(&base_kind) && !MEDIA_KINDS.contains(&it.kind.as_str()) {
        return Err(bad("資產不能改成整套設定或片段（它的圖片和聲音會沒人管）"));
    }
    if MEDIA_KINDS.contains(&it.kind.as_str()) {
        let old = base.as_ref().filter(|_| MEDIA_KINDS.contains(&base_kind)).and_then(|b| b.get("data"));
        keep_files(&mut it.data, old);
        if let Some(obj) = it.data.as_object_mut() {
            // 鎖住的設定表原封不動；沒送就留舊的
            match crate::charspec::merge(obj.get("spec"), old.and_then(|d| d.get("spec"))) {
                Some(sp) if it.kind == "char" => obj.insert("spec".into(), sp),
                _ => obj.remove("spec"),
            };
            // RefMod 檔跟圖片一樣以伺服器上的為準
            match old.and_then(|d| d.get("refmod")).filter(|r| r.is_object()) {
                Some(r) => obj.insert("refmod".into(), r.clone()),
                None => obj.remove("refmod"),
            };
        }
        // cid 是既有角色的身分，不能因只更新描述而換號。
        if let Some(cid) = old.and_then(|d| d["cid"].as_str()).filter(|s| !s.is_empty()) {
            it.data["cid"] = json!(cid);
        } else if base_kind == "char" {
            it.data["cid"] = json!(format!("lib{}", id.unwrap_or_default()));
        }
    }
    // 角色資產一定要有 cid（導演台記角色用的 id）；從資產庫頁新增的這時候補上
    if it.kind == "char" && it.data["cid"].as_str().unwrap_or("").is_empty() {
        it.data["cid"] = json!(crate::director::new_id());
    }
    let (thumb, thumb_error) = match body.get("image_id").and_then(Value::as_i64) {
        Some(iid) => match thumb_of(&app, iid).await {
            Ok(b64) => (Thumb::Set(b64), None),
            Err(e) => (Thumb::Keep, Some(format!("縮圖沒做成（{e}），其他都存好了"))),
        },
        None if body.get("thumb").is_some_and(Value::is_null) => (Thumb::Clear, None),
        None => (Thumb::Keep, None),
    };
    let id = app
        .db
        .lib_save(id, &it, &thumb)?
        .ok_or_else(|| not_found("找不到這一筆（可能已經刪掉了）"))?;
    let mut r = json!(app.db.lib_item(id)?);
    if let Some(e) = thumb_error {
        r["thumb_error"] = json!(e);
    }
    // 資產的 Anima tag：tag 索引裡找不到的列出來提醒，但照樣存（使用者自己決定要不要留）
    if let Some(ix) = app.tags.get() {
        // 設定表每一格的 tag 也一起檢查
        let spec_tags = crate::charspec::FIELDS.iter().flat_map(|(k, _)| it.data["spec"]["fields"][*k]["tags"].as_array().cloned().unwrap_or_default());
        let mut unverified: Vec<String> = vec![];
        for t in it.data["tags_anima"].as_array().cloned().unwrap_or_default().into_iter().chain(spec_tags) {
            if let Some(t) = t.as_str().filter(|t| !ix.has(t) && !unverified.iter().any(|u| u == t)) {
                unverified.push(t.to_string());
            }
        }
        if it.data.get("tags_anima").is_some() || it.data.get("spec").is_some() {
            r["unverified"] = json!(unverified);
        }
    }
    ok(r)
}

/// 刪除：資產類型先把整筆的 JSON 和它的檔案搬到 待刪除\資產庫\，搬成功才刪那一列
pub async fn delete(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    let _g = app.assets_lock.lock().await;
    if let Some(row) = app.db.lib_item(id)? {
        let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
        if MEDIA_KINDS.contains(&kind) {
            let paths = app.paths.clone();
            blocking(move || super::api_assets::trash_asset(&paths, &row))
                .await
                .map_err(internal)?;
        }
    }
    app.db.run("DELETE FROM prompt_lib WHERE id=?", [id])?;
    ok(json!({"ok": true}))
}

pub async fn thumb(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    let b64 = app.db.lib_thumb(id)?.ok_or_else(|| not_found("沒有縮圖"))?;
    let raw = STANDARD
        .decode(b64)
        .map_err(|_| internal("縮圖資料壞掉了"))?;
    // 網址帶 ?v=修改時間，換縮圖網址就會變
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/webp"),
            (header::CACHE_CONTROL, "max-age=31536000"),
        ],
        raw,
    )
        .into_response())
}

/// 套用或插入過一次：記下時間、次數（「最近用過」「最常用」排序用）
pub async fn used(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let id = body
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("缺少 id"))?;
    app.db.lib_used(id)?;
    ok(json!({"ok": true}))
}

/// 拖拉排序：{ids: [照新順序的 id…]}
pub async fn order(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let ids: Vec<i64> = body
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("缺少 ids"))?
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    app.db.lib_order(&ids)?;
    ok(json!({"ok": true}))
}

/// 資料夾改名：{from, to}；to 空白＝資料夾拿掉，裡面的變回未分類
pub async fn folder(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let _g = app.assets_lock.lock().await;
    let from = text(body.get("from"), 50);
    if from.is_empty() {
        return Err(bad("未分類不能改名"));
    }
    let to = text(body.get("to"), 50);
    let n = app.db.lib_rename_folder(&from, &to)?;
    ok(json!({"ok": true, "moved": n}))
}

/// 資產 data 裡的檔案引用（圖片、聲音、RefMod）只留 keep 說可以的；回傳有沒有拿掉東西
fn retain_files(data: &mut Value, keep: &dyn Fn(&str) -> bool) -> bool {
    let mut cut = false;
    if let Some(a) = data.get_mut("images").and_then(Value::as_array_mut) {
        let n = a.len();
        a.retain(|im| im["file"].as_str().is_some_and(keep));
        cut |= a.len() != n;
    }
    for k in ["audio", "refmod"] {
        if data[k]["file"].as_str().is_some_and(|f| !keep(f)) {
            if let Some(o) = data.as_object_mut() {
                o.remove(k);
                cut = true;
            }
        }
    }
    cut
}

fn file_refs(data: &Value) -> Vec<String> {
    let mut v: Vec<String> = data["images"].as_array().into_iter().flatten().filter_map(|im| im["file"].as_str().map(str::to_owned)).collect();
    v.extend(["audio", "refmod"].iter().filter_map(|k| data[*k]["file"].as_str().map(str::to_owned)));
    v
}

/// 設定檔匯入（api_gen::settings_import）：每筆檢查過才收；回傳（收了幾筆，幾筆資產的檔案沒跟來）。
/// 設定檔只帶資產的文字和縮圖，圖片、聲音檔留在原本那台電腦的 data\assets（檔名 <id>_<n>）：
/// 換一台電腦匯入時檔案不在、或同名的檔其實是這台別筆資產的 → 把引用拿掉（不然點開全是破圖，刪掉還會連累別筆的檔）。
/// 同一台電腦匯出再匯回、檔案還在的照舊接回去
pub fn import(db: &Db, items: Option<&Value>, assets: &std::path::Path) -> Result<(usize, usize), ApiError> {
    let (mut n, mut lost) = (0, 0);
    let col = |r: &Row, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let same = |r: &Row, it: &LibItem| col(r, "kind") == it.kind && col(r, "folder") == it.folder && col(r, "name") == it.name;
    let rows = db.lib_export()?;
    // 新增的會排到最前面：倒著匯入，順序才會跟匯出時一樣
    for v in items.and_then(Value::as_array).into_iter().flatten().rev() {
        let Ok(mut it) = item_from(v, None) else { continue };
        if MEDIA_KINDS.contains(&it.kind.as_str()) && it.kind != "shot" {
            let taken: Vec<String> = rows.iter().filter(|r| !same(r, &it)).flat_map(|r| file_refs(r.get("data").unwrap_or(&Value::Null))).collect();
            let keep = |f: &str| assets.join(f).is_file() && !taken.iter().any(|t| t == f);
            if retain_files(&mut it.data, &keep) {
                lost += 1;
            }
        }
        let thumb = v
            .get("thumb")
            .and_then(Value::as_str)
            .filter(|t| t.len() <= THUMB_B64_MAX && STANDARD.decode(t).is_ok())
            .map(str::to_owned);
        db.lib_import(&it, thumb)?;
        n += 1;
    }
    Ok((n, lost))
}

/// 舊格式的預設組（設定檔沒有 prompt_lib 時）也收進提示詞庫
pub fn import_preset(db: &Db, name: &str, data: &Value) -> Result<(), ApiError> {
    let it = LibItem {
        kind: "set".into(),
        name: name.into(),
        folder: String::new(),
        tags: vec![],
        family: String::new(),
        fav: false,
        note: String::new(),
        data: data.clone(),
    };
    db.lib_import(&it, None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn asset_save_waits_for_file_lock() {
        let root = std::env::temp_dir().join(format!("anima_save_lock_{}", crate::director::new_id()));
        let paths = crate::paths::Paths::new(&root, &root.join("web"), Some(root.join("comfy")));
        let app = super::super::start(paths, "http://127.0.0.1:9").await.unwrap();
        let guard = app.assets_lock.lock().await;
        let save = save(State(app.clone()), Body(json!({"kind":"char", "name":"lock test", "data":{}})));
        tokio::pin!(save);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut save).await.is_err());
        drop(guard);
        assert!(tokio::time::timeout(std::time::Duration::from_secs(3), save).await.unwrap().is_ok());
    }

    async fn body(r: axum::response::Response) -> Value {
        serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn locked_spec_survives_later_saves_and_refmod_is_server_owned() {
        let root = std::env::temp_dir().join(format!("anima_spec_lock_{}", crate::director::new_id()));
        let paths = crate::paths::Paths::new(&root, &root.join("web"), Some(root.join("comfy")));
        let app = super::super::start(paths, "http://127.0.0.1:9").await.unwrap();
        let spec = json!({"locked": true, "fields": {"sex": {"zh": "女性", "en": "female"}, "hair_color": {"zh": "橘髮", "en": "orange hair"}}});
        let r = body(save(State(app.clone()), Body(json!({"kind": "char", "name": "小克", "data": {"spec": spec, "refmod": {"file": "fake.safetensors"}}}))).await.unwrap()).await;
        let id = r["id"].as_i64().unwrap();
        assert_eq!(r["data"]["spec"]["block_en"], "female, orange hair.");
        assert!(r["data"].get("refmod").is_none(), "新存的不能自己塞 RefMod 檔");
        // 導演台之類只改外觀描述、沒送 spec：設定表留著
        let r = body(save(State(app.clone()), Body(json!({"id": id, "data": {"look_en": "x"}}))).await.unwrap()).await;
        assert_eq!(r["data"]["spec"]["block_en"], "female, orange hair.");
        // 鎖著硬改欄位：不理
        let bad = json!({"locked": true, "fields": {"sex": {"en": "male"}}, "block_en": "male."});
        let r = body(save(State(app.clone()), Body(json!({"id": id, "data": {"spec": bad}}))).await.unwrap()).await;
        assert_eq!(r["data"]["spec"]["block_en"], "female, orange hair.");
        assert_eq!(r["data"]["spec"]["fields"]["sex"]["en"], "female");
        // 解鎖才改得動
        let open = json!({"locked": false, "fields": {"sex": {"en": "male"}}});
        let r = body(save(State(app.clone()), Body(json!({"id": id, "data": {"spec": open}}))).await.unwrap()).await;
        assert_eq!((r["data"]["spec"]["locked"].as_bool(), r["data"]["spec"]["fields"]["sex"]["en"].as_str()), (Some(false), Some("male")));
        // 場景不收設定表
        let r = body(save(State(app.clone()), Body(json!({"kind": "scene", "name": "教室", "data": {"spec": spec}}))).await.unwrap()).await;
        assert!(r["data"].get("spec").is_none());
    }

    #[test]
    fn tags_are_cleaned() {
        assert_eq!(tags_of(Some(&json!("角色, #原創，  , 角色、夜景"))), vec!["角色", "原創", "夜景"]);
        assert_eq!(tags_of(Some(&json!(["a", " b ", "a", 3]))), vec!["a", "b"]);
        assert!(tags_of(None).is_empty());
    }

    #[test]
    fn asset_kinds_accepted() {
        for k in ["char", "scene", "frame", "outfit", "prop", "fx", "sound", "shot"] {
            assert!(item_from(&json!({"kind": k, "name": "x", "data": {}}), None).is_ok(), "{k}");
        }
        assert!(item_from(&json!({"kind": "weird", "name": "x", "data": {}}), None).is_err());
        assert!(item_from(&json!({"kind": "snippet", "name": "x", "data": {}}), None).is_err(), "片段還是要有 text");
    }

    #[test]
    fn asset_data_cleaned() {
        let d = clean_asset(
            "char",
            &json!({"images": [{"file": "..\\..\\evil.png", "role": "boss"}, {"file": "a/b/ok.webp", "role": "sheet"}, {"file": "..", "role": "src"}],
                    "audio": {"file": "../x.mp3"},
                    "tags_anima": ["Orange_Hair", "orange hair", " ", 3], "layout": {"top": " desk ", "zzz": "x"},
                    "look_en": "  long hair  ", "cid": "c1", "junk": 1}),
        );
        assert_eq!(d["images"][0], json!({"file": "evil.png", "role": "other"}));
        assert_eq!(d["images"][1], json!({"file": "ok.webp", "role": "sheet"}));
        assert_eq!(d["images"].as_array().unwrap().len(), 2, "沒有檔名的丟掉");
        assert_eq!(d["audio"], json!({"file": "x.mp3"}));
        assert_eq!(d["tags_anima"], json!(["orange hair"]));
        assert_eq!(d["layout"], json!({"top": "desk"}));
        assert_eq!(d["look_en"], "long hair");
        assert_eq!(d["cid"], "c1");
        assert!(d.get("junk").is_none());
        let many: Vec<String> = (0..80).map(|i| format!("t{i}")).collect();
        assert_eq!(clean_asset("prop", &json!({"tags_anima": many}))["tags_anima"].as_array().unwrap().len(), 60);
        // 拍攝單範本原樣保留
        let shot = json!({"segments": [{"t": "00:01"}], "anything": true});
        assert_eq!(clean_asset("shot", &shot), shot);
        // 存進去的時候也會清
        let it = item_from(&json!({"kind": "char", "name": "x", "data": {"junk": 1, "look_zh": "橘髮"}}), None).unwrap();
        assert_eq!(it.data, json!({"look_zh": "橘髮"}));
    }

    #[test]
    fn partial_update_keeps_other_fields() {
        let mut base = Row::new();
        for (k, v) in [
            ("kind", json!("snippet")),
            ("name", json!("夕陽")),
            ("folder", json!("光影")),
            ("tags", json!(["暖色"])),
            ("fav", json!(0)),
            ("data", json!({"text": "sunset, orange sky"})),
        ] {
            base.insert(k.into(), v);
        }
        let it = item_from(&json!({"fav": true}), Some(&base)).unwrap();
        assert!(it.fav);
        assert_eq!((it.kind.as_str(), it.name.as_str(), it.folder.as_str()), ("snippet", "夕陽", "光影"));
        assert_eq!(it.tags, vec!["暖色"]);
        assert!(item_from(&json!({"kind": "x", "name": "a", "data": {}}), None).is_err());
        assert!(item_from(&json!({"kind": "snippet", "name": "a", "data": {"text": "  "}}), None).is_err());
        assert!(item_from(&json!({"name": " ", "data": {}}), None).is_err());
    }
}

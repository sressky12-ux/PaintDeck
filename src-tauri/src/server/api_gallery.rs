//! 圖庫、回收區、匯出 zip、縮圖、上傳輸入圖（對照舊版 server.py 同名的函式）。
//!
//! 不真的刪除：移到回收區是搬進 output\AnimaWeb\_trash（檔名前面加圖庫編號），可以還原；
//! 「清空」是丟進 Windows 資源回收筒，還救得回來。
use super::{
    bad, basename, blocking, file_response, internal, not_found, ok, ApiResult, Body, Shared,
};
use crate::db::{GalleryQuery, Row};
use crate::imgmeta;
use crate::maint;
use crate::paths::Paths;
use crate::py::{self, now, truthy};
use axum::body::Body as HttpBody;
use axum::extract::{Multipart, Path as UrlPath, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- 檔案位置
fn s<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}

fn id_of(r: &Row) -> i64 {
    r.get("id").and_then(Value::as_i64).unwrap_or(0)
}

pub fn out_path(p: &Paths, r: &Row) -> PathBuf {
    p.output.join(s(r, "subfolder")).join(s(r, "filename"))
}

pub fn trash_path(p: &Paths, r: &Row) -> PathBuf {
    p.trash.join(format!("{}_{}", id_of(r), s(r, "filename")))
}

pub fn is_video(name: &str) -> bool {
    let n = name.to_lowercase();
    n.ends_with(".mp4") || n.ends_with(".webm") || n.ends_with(".mkv")
}

/// 影片的封面：生成時另存在 output\AnimaWeb\_poster\<params.poster>_00001_.png（graphs::build_video）
fn video_poster(p: &Paths, r: &Row) -> Option<PathBuf> {
    let poster = r.get("params")?.get("poster")?.as_str()?.to_string();
    let dir = p.output.join("AnimaWeb").join("_poster");
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{poster}_")) && n.ends_with(".png"))
        })
        .min()
}

/// 圖檔現在在哪：回收區的圖在 _trash 裡
pub fn file_of(p: &Paths, r: &Row) -> PathBuf {
    if r.get("deleted").and_then(Value::as_i64) == Some(1) {
        trash_path(p, r)
    } else {
        out_path(p, r)
    }
}

fn in_trash(r: &Row) -> bool {
    r.get("deleted").and_then(Value::as_i64) == Some(1)
}

/// 生圖站認定的 output 找不到檔案時，直接跟 ComfyUI 要（/view）寫回預期的位置。
///
/// 會變成這樣的情形都救得回來：生圖站連到的 ComfyUI 跟設定裡的「ComfyUI 資料夾」不是同一份
/// （例如自己開的官方桌面版在 8188、生圖站卻記著另一份）、ComfyUI 自己用 --output-directory
/// 指到別的資料夾、沿用舊資料以後 ComfyUI 換了位置。圖還在 ComfyUI 手上就抓回來。
pub async fn fetch_from_comfy(app: &super::App, r: &Row) -> Result<(), super::ApiError> {
    let dst = out_path(&app.paths, r);
    let bytes = app
        .comfy
        .view(s(r, "filename"), s(r, "subfolder"), "output")
        .await
        .map_err(|e| bad(e.0))?;
    if bytes.is_empty() {
        return Err(bad("ComfyUI 回傳的是空檔案"));
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(internal)?;
    }
    // 先寫 .part 再改名：中途斷掉不會留下半張圖
    let mut tmp = dst.clone().into_os_string();
    tmp.push(".recover.part");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &bytes).map_err(internal)?;
    std::fs::rename(&tmp, &dst).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        internal(e)
    })
}

/// ComfyUI 現在有沒有反應：websocket 說在線就算，還沒連上（剛開、剛重連）就實際打一次
async fn comfy_up(app: &super::App) -> bool {
    app.comfy.live().online || app.comfy.get("/system_stats").await.is_ok()
}

/// 這張圖不在生圖站認定的位置時，先問 ComfyUI 要回來再回報結果（ComfyUI 沒開就什麼都不做）
async fn recover(app: &super::App, r: &Row) -> bool {
    if in_trash(r) || out_path(&app.paths, r).exists() || !comfy_up(app).await {
        return false;
    }
    fetch_from_comfy(app, r).await.is_ok()
}

fn qint(q: &HashMap<String, String>, k: &str, default: i64) -> Result<i64, super::ApiError> {
    match q.get(k) {
        None => Ok(default),
        Some(v) => v.trim().parse().map_err(|_| bad(format!("{k} 要是整數"))),
    }
}

fn qtext(q: &HashMap<String, String>, k: &str) -> Option<String> {
    q.get(k).filter(|v| !v.is_empty()).cloned()
}

// ---------------------------------------------------------------- 圖庫
pub async fn gallery(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let num = |k: &str| -> Result<Option<f64>, super::ApiError> {
        qtext(&q, k)
            .map(|v| v.trim().parse().map_err(|_| bad(format!("{k} 要是數字"))))
            .transpose()
    };
    let g = GalleryQuery {
        offset: qint(&q, "offset", 0)?,
        limit: qint(&q, "limit", 60)?.clamp(1, 500),  // 負數在 SQLite 等於不限筆數
        fav: q.get("fav").map(String::as_str) == Some("1"),
        mode: qtext(&q, "mode"),
        q: qtext(&q, "q"),
        trash: q.get("trash").map(String::as_str) == Some("1"),
        model: qtext(&q, "model"),
        family: qtext(&q, "family"),
        lora: qtext(&q, "lora"),
        date_from: num("from")?,
        date_to: num("to")?,
        sort: qtext(&q, "sort").unwrap_or_else(|| "new".into()),
    };
    let (total, mut rows) = app.db.gallery(&g)?;
    for r in rows.iter_mut() {
        // 檔案被手動搬走或刪掉的，網頁上標「檔案不見了」
        let p = if g.trash {
            trash_path(&app.paths, r)
        } else {
            out_path(&app.paths, r)
        };
        r.insert("missing".into(), json!(!p.exists()));
    }
    app.db.add_gen_seconds(&mut rows)?;
    ok(json!({"total": total, "items": rows}))
}

pub async fn facets(State(app): State<Shared>) -> ApiResult {
    ok(app.db.facets()?)
}

pub async fn image_info(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    match app.db.image(id)? {
        Some(r) => {
            let mut rows = [r];
            app.db.add_gen_seconds(&mut rows)?;
            let [r] = rows;
            ok(Value::Object(r))
        }
        None => Err(not_found("找不到")),
    }
}

pub async fn image_update(
    State(app): State<Shared>,
    UrlPath(id): UrlPath<i64>,
    Body(body): Body,
) -> ApiResult {
    if let Some(f) = body.get("fav") {
        app.db.run(
            "UPDATE images SET fav=? WHERE id=?",
            rusqlite::params![crate::py::truthy(Some(f)) as i64, id],
        )?;
    }
    ok(json!({"ok": true}))
}

/// 移到回收區：檔案搬進 output\AnimaWeb\_trash（檔名前面加圖庫編號），可以還原
fn trash_one(app: &Shared, r: &Row) -> Result<(), super::ApiError> {
    let src = out_path(&app.paths, r);
    if !src.exists() {
        return Err(not_found("原圖已不存在；請到圖庫的「篩選」按「清理遺失的紀錄」"));
    }
    let dst = trash_path(&app.paths, r);
    if dst.exists() {
        return Err(bad("回收區已有同名檔案，請先檢查後再試"));
    }
    std::fs::create_dir_all(&app.paths.trash).map_err(internal)?;
    maint::move_path(&src, &dst).map_err(internal)?;
    if let Err(e) = app.db.run(
        "UPDATE images SET deleted=1, trashed=? WHERE id=?",
        rusqlite::params![now(), id_of(r)],
    ) {
        if let Err(rollback) = maint::move_path(&dst, &src) {
            return Err(internal(format!(
                "資料庫更新失敗：{e}；原圖搬回也失敗：{rollback}"
            )));
        }
        return Err(e.into());
    }
    Ok(())
}

fn restore_one(app: &Shared, r: &mut Row) -> Result<(), super::ApiError> {
    let src = trash_path(&app.paths, r);
    if !src.exists() {
        return Err(not_found("回收區的原圖已不存在"));
    }
    let mut dst = out_path(&app.paths, r);
    let mut newname = None;
    let occupied = |name: &str| -> Result<bool, super::ApiError> {
        let row = app.db.q1(
            "SELECT id FROM images WHERE subfolder=? AND filename=? AND deleted=0 AND id<>?",
            rusqlite::params![s(r, "subfolder"), name, id_of(r)],
        )?;
        Ok(row.is_some())
    };
    if dst.exists() || occupied(s(r, "filename"))? {
        // 原位已經有同名的新圖：改名放回，紀錄跟著改
        let fname = s(r, "filename").to_string();
        let (base, ext) = match fname.rfind('.') {
            Some(i) if i > 0 => (fname[..i].to_string(), fname[i..].to_string()),
            _ => (fname.clone(), String::new()),
        };
        let dir = app.paths.output.join(s(r, "subfolder"));
        let mut n = 1;
        while dir.join(format!("{base}_r{n}{ext}")).exists()
            || occupied(&format!("{base}_r{n}{ext}"))?
        {
            n += 1;
        }
        let name = format!("{base}_r{n}{ext}");
        dst = dir.join(&name);
        newname = Some(name);
    }
    maint::move_path(&src, &dst).map_err(internal)?;
    let changed = app.db.run(
        "UPDATE images SET filename=COALESCE(?, filename), deleted=0, trashed=NULL WHERE id=?",
        rusqlite::params![newname, id_of(r)],
    );
    if let Err(e) = changed {
        if let Err(rollback) = maint::move_path(&dst, &src) {
            return Err(internal(format!(
                "資料庫更新失敗：{e}；圖片搬回回收區也失敗：{rollback}"
            )));
        }
        return Err(e.into());
    }
    Ok(())
}

/// 清空：回收區的檔案丟進 Windows 資源回收筒（還救得回來），圖庫紀錄標成已清空
fn purge(app: &Shared, recs: &[Row]) -> Result<usize, super::ApiError> {
    let paths: Vec<PathBuf> = recs.iter().map(|r| trash_path(&app.paths, r)).collect();
    let gone: Vec<PathBuf> = maint::to_recycle_bin(&paths).map_err(internal)?;
    let gone: Vec<String> = gone
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase())
        .collect();
    let mut n = 0;
    for (r, p) in recs.iter().zip(&paths) {
        let abs = std::path::absolute(p)
            .unwrap_or_else(|_| p.clone())
            .to_string_lossy()
            .to_lowercase();
        if gone.contains(&abs) || !p.exists() {
            app.db
                .run("UPDATE images SET deleted=2 WHERE id=?", [id_of(r)])?;
            let t = app.paths.thumbs.join(format!("{}.webp", id_of(r)));
            if t.exists() {
                let _ = std::fs::remove_file(t); // 縮圖是快取，隨時能重做
            }
            n += 1;
        }
    }
    Ok(n)
}

pub async fn image_delete(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    let r = app.db.image(id)?.ok_or_else(|| not_found("找不到"))?;
    let a = app.clone();
    blocking(move || trash_one(&a, &r)).await?;
    ok(json!({"ok": true}))
}

/// 多選批次操作：{ids, action: trash|restore|purge|fav|unfav}
pub async fn images_batch(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let act = body
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let ids: Vec<i64> = body
        .get("ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(5000)
        .map(|v| crate::py::int(v).ok_or_else(|| bad("ids 格式不對")))
        .collect::<Result<_, _>>()?;
    if ids.is_empty() {
        return Err(bad("沒有選任何圖"));
    }
    let recs = app.db.images_by_ids(&ids)?;
    let deleted = |r: &Row, d: i64| r.get("deleted").and_then(Value::as_i64) == Some(d);
    // 回收／還原：一張失敗（例如原檔被手動刪掉）不要卡住後面的；失敗的編號回給網頁
    let mut failed: Vec<i64> = vec![];
    let mut fail_msg: Option<String> = None;
    let n = match act.as_str() {
        "fav" | "unfav" => {
            let sql = format!(
                "UPDATE images SET fav=? WHERE id IN ({})",
                vec!["?"; ids.len()].join(",")
            );
            let mut args = vec![(act == "fav") as i64];
            args.extend(&ids);
            app.db.run(&sql, rusqlite::params_from_iter(args))?;
            recs.len()
        }
        "trash" => {
            let recs: Vec<Row> = recs.into_iter().filter(|r| deleted(r, 0)).collect();
            let a = app.clone();
            let (n, f, m) = blocking(move || each_ok(recs, |r| trash_one(&a, &r))).await;
            (failed, fail_msg) = (f, m);
            n
        }
        "restore" => {
            let recs: Vec<Row> = recs.into_iter().filter(|r| deleted(r, 1)).collect();
            let a = app.clone();
            let (n, f, m) = blocking(move || each_ok(recs, |mut r| restore_one(&a, &mut r))).await;
            (failed, fail_msg) = (f, m);
            n
        }
        "purge" => {
            let recs: Vec<Row> = recs.into_iter().filter(|r| deleted(r, 1)).collect();
            let a = app.clone();
            blocking(move || purge(&a, &recs)).await?
        }
        _ => return Err(bad("不認得的動作")),
    };
    if n == 0 {
        if let Some(m) = fail_msg {
            return Err(bad(m));
        }
    }
    ok(json!({"ok": true, "count": n, "failed": failed, "error": fail_msg}))
}

/// 一張一張做，失敗的跳過：回傳（成功張數、失敗的圖庫編號、第一個錯誤訊息）
fn each_ok(
    recs: Vec<Row>,
    mut f: impl FnMut(Row) -> Result<(), super::ApiError>,
) -> (usize, Vec<i64>, Option<String>) {
    let (mut n, mut failed, mut msg) = (0, vec![], None);
    for r in recs {
        let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
        match f(r) {
            Ok(()) => n += 1,
            Err(e) => {
                failed.push(id);
                msg.get_or_insert(e.1);
            }
        }
    }
    (n, failed, msg)
}

pub async fn trash_empty(State(app): State<Shared>) -> ApiResult {
    let recs = app.db.q("SELECT * FROM images WHERE deleted=1", [])?;
    let a = app.clone();
    let n = blocking(move || purge(&a, &recs)).await?;
    ok(json!({"ok": true, "count": n}))
}

/// 圖庫裡檔案已經不在硬碟上的紀錄：從圖庫隱藏（deleted=2），不動任何檔案
pub async fn prune_missing(State(app): State<Shared>) -> ApiResult {
    let a = app.clone();
    let n = blocking(move || -> Result<usize, super::ApiError> {
        let mut n = 0;
        for r in a.db.q(
            "SELECT id, filename, subfolder, deleted FROM images WHERE deleted IN (0, 1)",
            [],
        )? {
            if !file_of(&a.paths, &r).exists() {
                a.db.run("UPDATE images SET deleted=2 WHERE id=?", [id_of(&r)])?;
                n += 1;
            }
        }
        Ok(n)
    })
    .await?;
    ok(json!({"count": n}))
}

/// 把「生圖站找不到檔案」的紀錄，改從正在跑的 ComfyUI 要回來（寫回預期的 output 位置，見 fetch_from_comfy）。
/// ComfyUI 手上也沒有的（真的被刪掉）就維持原狀，交給「清理遺失的紀錄」處理。
pub async fn repair_missing(State(app): State<Shared>) -> ApiResult {
    if !comfy_up(&app).await {
        return Err(bad(
            "ComfyUI 沒開：先開 ComfyUI（設定頁有「開 ComfyUI」）再回來按修復",
        ));
    }
    let mut rows = app
        .db
        .q("SELECT id, filename, subfolder, deleted FROM images WHERE deleted=0", [])?;
    let total = rows.len();
    let mut repaired = 0usize;
    let mut first_err: Option<String> = None;
    // 只修「現在真的找不到」的，正常的不碰；一次最多 500 張，避免整庫幾千張卡住
    rows.retain(|r| !out_path(&app.paths, r).exists());
    let missing = rows.len();
    for r in rows.into_iter().take(500) {
        match fetch_from_comfy(&app, &r).await {
            Ok(()) => repaired += 1,
            Err(e) => {
                first_err.get_or_insert(e.1);
            }
        }
    }
    if repaired > 0 {
        eprintln!("圖庫修復：從 ComfyUI 要回 {repaired} 張（原本有 {missing} 張找不到，共 {total} 筆紀錄）");
    }
    ok(json!({"ok": true, "total": total, "missing": missing, "still": missing - repaired,
              "repaired": repaired, "error": if repaired == 0 { first_err } else { None }}))
}

/// 自己建的暫存檔：回應送完（或中斷）就刪掉
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 把選的圖打包成 zip 下載（PNG 本身已壓縮，用不壓縮的 zip 比較快）
pub async fn export_zip(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let ids: Vec<i64> = body
        .get("ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(5000)
        .filter_map(crate::py::int)
        .collect();
    let recs: Vec<Row> = app
        .db
        .images_by_ids(&ids)?
        .into_iter()
        .filter(|r| file_of(&app.paths, r).exists())
        .collect();
    if recs.is_empty() {
        return Err(bad("選的圖都找不到檔案"));
    }
    let n = recs.len();
    let tmp = app.paths.data.join(format!(
        "export_{}_{:08x}.zip",
        std::process::id(),
        rand::random::<u32>()
    ));
    let guard = TempFile(tmp.clone());
    let (a, t) = (app.clone(), tmp.clone());
    blocking(move || -> std::io::Result<()> {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&t)?);
        let opt = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        let mut used = std::collections::HashSet::new();
        for r in &recs {
            let mut name = s(r, "filename").to_string();
            if used.contains(&name) {
                name = format!("{}_{name}", id_of(r));
            }
            used.insert(name.clone());
            z.start_file(name, opt).map_err(std::io::Error::other)?;
            std::io::copy(&mut std::fs::File::open(file_of(&a.paths, r))?, &mut z)?;
        }
        z.finish().map_err(std::io::Error::other)?;
        Ok(())
    })
    .await
    .map_err(|e| internal(format!("打包失敗：{e}")))?;
    let file = tokio::fs::File::open(&tmp).await.map_err(internal)?;
    let len = file.metadata().await.map_err(internal)?.len();
    // 先關檔（串流結束）再刪暫存檔：guard 放在 map 的閉包裡，比檔案晚釋放
    let stream = tokio_util::io::ReaderStream::new(file).map(move |c| {
        let _ = &guard;
        c
    });
    let fname = format!(
        "attachment; filename=anima_{}_{n}.zip",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, fname),
            (header::CONTENT_LENGTH, len.to_string()),
        ],
        HttpBody::from_stream(stream),
    )
        .into_response())
}

pub async fn img(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    let r = app.db.image(id)?.ok_or_else(|| not_found("找不到"))?;
    if !file_of(&app.paths, &r).exists() && recover(&app, &r).await {
        // 從 ComfyUI 要回來了，直接往下讀（file_of 會拿到新位置）
    }
    file_response(file_of(&app.paths, &r), Some("max-age=31536000")).await
}

/// PIL Image.thumbnail 的尺寸算法：只縮不放，保持比例，四捨五入取最接近原比例的整數
fn thumb_size(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let aspect = w as f64 / h as f64;
    let (mut x, mut y) = (max as f64, max as f64);
    let pick = |n: f64, key: &dyn Fn(f64) -> f64| -> f64 {
        let (a, b) = (n.floor(), n.ceil());
        (if key(b) < key(a) { b } else { a }).max(1.0)
    };
    if x / y >= aspect {
        x = pick(y * aspect, &|n| (aspect - n / y).abs());
    } else {
        y = pick(x / aspect, &|n| {
            if n == 0.0 {
                0.0
            } else {
                (aspect - x / n).abs()
            }
        });
    }
    (x as u32, y as u32)
}

/// 縮成長邊 max 的 webp（圖庫縮圖 512、提示詞庫縮圖 320）
pub fn small_webp(src: &Path, max: u32, quality: f32) -> Result<Vec<u8>, String> {
    let dy = image::open(src).map_err(|e| e.to_string())?;
    let (w, h) = thumb_size(dy.width(), dy.height(), max);
    let fit = |im: image::DynamicImage| if (w, h) == (im.width(), im.height()) { im } else { im.resize_exact(w, h, image::imageops::FilterType::Lanczos3) };
    // 透明圖（一鍵去背）：鋪在灰白棋盤格上，不然透明的地方會露出原本的背景
    let small = if dy.color().has_alpha() {
        let mut im = fit(dy).to_rgba8();
        for (x, y, p) in im.enumerate_pixels_mut() {
            let bg = if (x / 12 + y / 12) % 2 == 0 { 255.0 } else { 214.0 };
            let a = p[3] as f32 / 255.0;
            for c in 0..3 {
                p[c] = (p[c] as f32 * a + bg * (1.0 - a)).round() as u8;
            }
        }
        image::DynamicImage::ImageRgba8(im).to_rgb8()
    } else {
        fit(dy).to_rgb8()
    };
    Ok(webp::Encoder::from_rgb(small.as_raw(), w, h).encode(quality).to_vec())
}

fn make_thumb(src: &Path, dst: &Path) -> Result<(), String> {
    let webp = small_webp(src, 512, 82.0)?;
    let tmp = dst.with_extension("webp.tmp");
    std::fs::write(&tmp, &webp).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dst).map_err(|e| e.to_string())
}

/// 圖庫一張圖拿來做縮圖的檔案（影片用封面）
pub fn thumb_source(app: &super::App, id: i64) -> Result<PathBuf, super::ApiError> {
    let r = app.db.image(id)?.ok_or_else(|| not_found("找不到"))?;
    let mut src = file_of(&app.paths, &r);
    if is_video(s(&r, "filename")) {
        src = video_poster(&app.paths, &r).ok_or_else(|| not_found("這部影片沒有封面"))?;
    }
    if !src.exists() {
        return Err(not_found("找不到"));
    }
    Ok(src)
}

pub async fn thumb(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    let dst = app.paths.thumbs.join(format!("{id}.webp"));
    if !dst.exists() {
        let mut src = thumb_source(&app, id);
        if src.is_err() {
            // 檔案不在生圖站認定的 output（換過 ComfyUI、output 指到別處）：先問 ComfyUI 要回來
            if let Some(r) = app.db.image(id)? {
                if recover(&app, &r).await {
                    src = thumb_source(&app, id);
                }
            }
        }
        let src = src?;
        let d = dst.clone();
        blocking(move || make_thumb(&src, &d))
            .await
            .map_err(|e| internal(format!("做縮圖失敗：{e}")))?;
    }
    file_response(dst, Some("max-age=31536000")).await
}

// ---------------------------------------------------------------- 上傳／送到輸入
/// 存進 ComfyUI\input。遮罩：白色＝要重繪；透明底的筆觸圖用 alpha，不透明的圖（網頁送來的黑底白字）用亮度
fn save_input(raw: &[u8], kind: &str, input: &Path) -> Result<Value, String> {
    let im = image::load_from_memory(raw).map_err(|e| e.to_string())?;
    let kind: String = kind
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let name = format!(
        "aw_{kind}_{}_{:06x}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S"),
        rand::random::<u32>() & 0xff_ffff
    );
    let path = input.join(&name);
    let (w, h) = (im.width(), im.height());
    let res = if kind == "mask" {
        let rgba = im.to_rgba8();
        let transparent = im.color().has_alpha() && rgba.pixels().any(|p| p[3] < 255);
        let l = image::GrayImage::from_fn(w, h, |x, y| {
            let p = rgba.get_pixel(x, y);
            if transparent {
                image::Luma([p[3]])
            } else {
                // PIL convert("L")
                image::Luma([((p[0] as u32 * 19595
                    + p[1] as u32 * 38470
                    + p[2] as u32 * 7471
                    + 0x8000)
                    >> 16) as u8])
            }
        });
        l.save(&path)
    } else if im.color().has_alpha() {
        im.to_rgba8().save(&path)
    } else {
        im.to_rgb8().save(&path)
    };
    res.map_err(|e| e.to_string())?;
    Ok(json!({"name": name, "width": w, "height": h}))
}

pub async fn upload(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    mut mp: Multipart,
) -> ApiResult {
    let kind = q.get("kind").cloned().unwrap_or_else(|| "image".into());
    let field = mp
        .next_field()
        .await
        .map_err(|e| bad(format!("讀不到上傳的檔案：{e}")))?
        .ok_or_else(|| bad("沒有上傳檔案"))?;
    let raw = field
        .bytes()
        .await
        .map_err(|e| bad(format!("讀不到上傳的檔案：{e}")))?;
    let input = app.paths.input.clone();
    let k = kind.clone();
    let raw2 = raw.clone();
    let mut r = blocking(move || save_input(&raw2, &k, &input))
        .await
        .map_err(|e| bad(format!("不是可讀取的圖片：{e}")))?;
    if kind == "image" {
        // 從原始上傳的檔讀參數（舊版讀的是另存後的檔，文字區塊已經不見，永遠讀不到）
        r["params"] = blocking(move || imgmeta::read_params_bytes(&raw))
            .await
            .unwrap_or(Value::Null);
    }
    ok(r)
}

/// 「讀圖片參數」（生成頁的「讀圖片參數…」）：上傳一張圖（multipart，一個檔案），不存檔。回傳
/// {"params": 生成頁參數或 null, "source": "anima"|"comfyui"|"webui"|"novelai"|"other"|null, "workflow": 圖裡有沒有 ComfyUI 工作流, "notes": [提醒]}
pub async fn image_meta(mut mp: Multipart) -> ApiResult {
    let field = mp
        .next_field()
        .await
        .map_err(|e| bad(format!("讀不到上傳的檔案：{e}")))?
        .ok_or_else(|| bad("沒有上傳檔案"))?;
    let raw = field
        .bytes()
        .await
        .map_err(|e| bad(format!("讀不到上傳的檔案：{e}")))?;
    if imgmeta::image_kind(&raw).is_none() {
        return Err(bad("只看得懂 PNG、WebP、JPEG 圖片"));
    }
    ok(blocking(move || imgmeta::read_found_json(&raw)).await)
}

pub async fn to_input(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let id = body
        .get("id")
        .and_then(crate::py::int)
        .ok_or_else(|| bad("缺少 id"))?;
    let r = app.db.image(id)?.ok_or_else(|| not_found("找不到這張圖"))?;
    if is_video(s(&r, "filename")) {
        return Err(bad("影片不能當輸入圖：請改用影片的來源圖片"));
    }
    let src = file_of(&app.paths, &r);
    if !src.exists() {
        return Err(not_found("這張圖的檔案已經不在硬碟上了（可能被手動搬走或刪掉）。可以到圖庫按「清理遺失的紀錄」把它從圖庫拿掉。"));
    }
    let name = format!("aw_from_{id}.png");
    let dst = app.paths.input.join(&name);
    let (w, h) = blocking(move || -> std::io::Result<(u32, u32)> {
        std::fs::copy(&src, &dst)?;
        image::image_dimensions(&dst).map_err(std::io::Error::other)
    })
    .await
    .map_err(internal)?;
    ok(json!({"name": name, "width": w, "height": h}))
}

pub async fn input_file(State(app): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult {
    file_response(app.paths.input.join(basename(&name)), None).await
}

// ---------------------------------------------------------------- 反推
/// 反推提示詞：圖庫的圖（id）或輸入圖（name）
pub async fn tagger(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if !app.tagger.available() {
        return Err(bad("反推模型尚未下載"));
    }
    let path = match body.get("id").filter(|v| truthy(Some(v))) {
        Some(id) => {
            let id = py::int(id).ok_or_else(|| bad("id 要是整數"))?;
            let rec = app
                .db
                .image(id)?
                .ok_or_else(|| not_found("找不到這張圖的檔案"))?;
            let p = file_of(&app.paths, &rec);
            if !p.exists() {
                return Err(not_found("找不到這張圖的檔案"));
            }
            p
        }
        None => {
            let name = body
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("缺少 name"))?;
            app.paths.input.join(basename(name))
        }
    };
    let num = |k: &str, d: f64| match body.get(k) {
        None => Ok(d),
        Some(v) => py::float(v).ok_or_else(|| bad(format!("{k} 要是數字"))),
    };
    let (general, character) = (num("general", 0.35)?, num("character", 0.85)?);
    let style_lora = truthy(body.get("style_lora"));
    let t = app.tagger.clone();
    let mut v = blocking(move || t.tag(&path, general, character))
        .await
        .map_err(|e| bad(format!("反推失敗：{e}")))?;
    // 自動清理的建議（網頁決定要不要套用，見 tagclean.rs）＋每個 tag 的中文對照
    let list: Vec<(String, f64)> = ["character", "general"]
        .iter()
        .flat_map(|k| v[*k].as_array().cloned().unwrap_or_default())
        .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_f64()?)))
        .collect();
    let (order, notes) = crate::tagclean::clean(&list, style_lora);
    let mut zh = serde_json::Map::new();
    if let Some(ix) = app.tags.get() {
        for (t, _) in &list {
            if let Some(z) = ix.zh_of(t) {
                zh.insert(t.clone(), Value::String(z));
            }
        }
    }
    v["order"] = json!(order);
    v["notes"] = Value::Object(notes.into_iter().map(|(t, w)| (t, Value::String(w))).collect());
    v["zh"] = Value::Object(zh);
    Ok(Json(v).into_response())
}

#[cfg(test)]
mod tests {
    use super::thumb_size;

    #[test]
    fn thumb_size_like_pil() {
        // PIL: Image.new("RGB", (w, h)).thumbnail((512, 512)) 的結果
        assert_eq!(thumb_size(896, 1152, 512), (398, 512));
        assert_eq!(thumb_size(1152, 896, 512), (512, 398));
        assert_eq!(thumb_size(1024, 1024, 512), (512, 512));
        assert_eq!(thumb_size(300, 200, 512), (300, 200));
        assert_eq!(thumb_size(1600, 900, 512), (512, 288));
        assert_eq!(thumb_size(777, 1001, 512), (397, 512));
        assert_eq!(thumb_size(513, 10000, 512), (26, 512));
        assert_eq!(thumb_size(10000, 513, 512), (512, 26));
        assert_eq!(thumb_size(1086, 1448, 512), (384, 512));
    }
}

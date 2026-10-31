//! 整本上色 API（/api/colorbook/*）與背景工作。純邏輯在 crate::colorbook，設計在 design\整本上色_設計.md。
//!   GET  /api/colorbook                         書的清單（摘要）＋正在跑的
//!   POST /api/colorbook/new（multipart）          zip 或圖片（可多個）、title → 建一本（自然排序、分彩頁／黑白／空白、做縮圖）
//!   GET  /api/colorbook/{id}                    整本資料（book.json）
//!   POST /api/colorbook/{id}/sheet {auto?, text?} AI 看彩頁寫角色配色表（35B，沒有就 9B）／存使用者改的
//!   POST /api/colorbook/{id}/page {i, prompt?, kind?}    改一頁的上色指令（空字串＝下次讓 AI 重寫）或分類
//!   POST /api/colorbook/{id}/run {pages?, redo?, ai?, keep_lines?}、POST …/stop   背景上色
//!   GET  /api/colorbook/{id}/file/{src|thumb|out|othumb}/{檔名}、GET …/zip     看圖、下載整本
//!   POST /api/colorbook/{id}/delete              整本搬到 待刪除\整本上色\
//!
//! 背景工作：① 沒有配色表又有彩頁 → 先寫配色表 ② 每頁還沒有指令的 → 35B 看這頁寫指令（全部寫完才換 Qwen：LLM 和 Qwen 不能同時佔顯存）
//! ③ Qwen 2.1 一頁一頁上色（走一般的生圖任務：任務面板看得到）→ 原稿亮度回貼 → out\。原始輸出搬到 raw\、圖庫那筆隱藏（deleted=2），成品不進圖庫。
//! 進度用 hub 送 {"type":"colorbook", id, phase: msg|page|end|error, msg, page?}；網頁收到 page／end 就重讀這本。
use super::api_director::{job_image, random_seed, wait_job};
use super::api_gen::submit_jobs;
use super::{bad, blocking, not_found, ok, ApiResult, Body, Shared};
use crate::colorbook as cb;
use crate::director::tx;
use axum::extract::{Multipart, Path as UrlPath, State};
use axum::response::IntoResponse;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

/// 一次最多幾頁（再多就分開丟）
const MAX_PAGES: usize = 400;
/// 寫配色表最多看幾張彩頁（多張圖會被縮小，看太多反而看不清楚）
const SHEET_PAGES: usize = 4;

fn runs() -> &'static Mutex<HashMap<String, CancellationToken>> {
    static R: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn running(id: &str) -> bool {
    runs().lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
}

/// book.json「讀 → 改 → 寫」排隊（背景工作和網頁改指令可能同時來）
static BOOK_LOCK: Mutex<()> = Mutex::new(());

fn update(data: &Path, id: &str, f: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let _g = BOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut b = cb::load(data, id).ok_or("找不到這本")?;
    f(&mut b);
    cb::save(data, &b).map_err(|e| format!("存檔失敗：{e}"))?;
    Ok(b)
}

fn load(app: &Shared, id: &str) -> Result<Value, super::ApiError> {
    cb::load(&app.paths.data, id).ok_or_else(|| not_found("找不到這本"))
}

fn emit(app: &Shared, id: &str, phase: &str, msg: impl Into<String>, page: Option<usize>) {
    app.hub.send(json!({"type": "colorbook", "id": id, "phase": phase, "msg": msg.into(), "page": page}));
}

// ---------------------------------------------------------------- 清單、建立
pub async fn list(State(app): State<Shared>) -> ApiResult {
    let data = app.paths.data.clone();
    let books = blocking(move || cb::list(&data)).await;
    let busy: Vec<String> = runs().lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    ok(json!({"books": books, "running": busy}))
}

pub async fn get(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let mut b = load(&app, &id)?;
    b["running"] = json!(running(&id));
    ok(b)
}

/// multipart：檔案欄位（zip 或圖片，可以好幾個）＋ title（可空）
pub async fn new(State(app): State<Shared>, mut mp: Multipart) -> ApiResult {
    let (mut title, mut files) = (String::new(), Vec::<(String, Vec<u8>)>::new());
    while let Some(f) = mp.next_field().await.map_err(|e| bad(format!("讀不到上傳的檔案：{e}")))? {
        let (field, name) = (f.name().unwrap_or("").to_string(), f.file_name().unwrap_or("").to_string());
        let bytes = f.bytes().await.map_err(|e| bad(format!("讀不到上傳的檔案：{e}")))?;
        if field == "title" {
            title = String::from_utf8_lossy(&bytes).trim().chars().take(80).collect();
        } else {
            files.push((name, bytes.to_vec()));
        }
    }
    if files.is_empty() {
        return Err(bad("沒有收到檔案：把整本的 zip 或全部的頁面圖拖進來"));
    }
    let data = app.paths.data.clone();
    let book = blocking(move || create(&data, title, files)).await.map_err(bad)?;
    ok(book)
}

fn ext_of(name: &str) -> String {
    let e = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    if e == "jpeg" { "jpg".into() } else { e }
}

fn save_thumb(img: &image::RgbImage, to: &Path) -> Result<(), String> {
    let h = (img.height() * 300 / img.width().max(1)).max(1);
    let t = image::imageops::thumbnail(img, 300, h);
    image::DynamicImage::ImageRgb8(t).save_with_format(to, image::ImageFormat::Jpeg).map_err(|e| format!("做縮圖失敗：{e}"))
}

/// zip 解開（只要圖片，跳過 __MACOSX）、圖片直接收 → 自然排序 → 存 src\0001.ext、縮圖、分類
fn create(data: &Path, title: String, files: Vec<(String, Vec<u8>)>) -> Result<Value, String> {
    let mut pages: Vec<(String, Vec<u8>)> = vec![];
    let mut zip_title = None;
    for (name, bytes) in files {
        if bytes.starts_with(b"PK") {
            zip_title.get_or_insert_with(|| Path::new(&name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("壓縮檔打不開：{e}"))?;
            for i in 0..z.len() {
                let mut f = z.by_index(i).map_err(|e| e.to_string())?;
                let n = f.name().map_err(|e| e.to_string())?.replace('\\', "/");
                if f.is_dir() || n.starts_with("__MACOSX") || !cb::is_image_name(&n) {
                    continue;
                }
                let mut buf = vec![];
                (&mut f).take(80 * 1024 * 1024).read_to_end(&mut buf).map_err(|e| format!("解壓 {n} 失敗：{e}"))?;
                pages.push((n, buf));
            }
        } else if cb::is_image_name(&name) {
            pages.push((name, bytes));
        }
    }
    if pages.is_empty() {
        return Err("裡面沒有看得懂的圖片（收 PNG、JPG、WebP；GIF 不收）".into());
    }
    if pages.len() > MAX_PAGES {
        return Err(format!("一次最多 {MAX_PAGES} 頁（這次 {} 頁）：分開丟", pages.len()));
    }
    pages.sort_by(|a, b| cb::natural_cmp(&a.0, &b.0));
    let id = format!("cb_{}_{:04x}", chrono::Local::now().format("%Y%m%d%H%M%S"), rand::random::<u16>());
    let dir = cb::book_dir(data, &id);
    for sub in ["src", "thumb"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|e| format!("建資料夾失敗：{e}"))?;
    }
    let mut list = vec![];
    for (k, (name, bytes)) in pages.iter().enumerate() {
        let n = k + 1;
        let orig = name.rsplit('/').next().unwrap_or(name).to_string();
        let img = match image::load_from_memory(bytes) {
            Ok(i) => i.to_rgb8(),
            // 壞掉的圖：照樣列出來（分類成空白、不上色），使用者看得到是哪一頁
            Err(e) => {
                list.push(json!({"n": n, "file": "", "orig": orig, "w": 0, "h": 0, "kind": "blank", "status": "error", "prompt": "", "prompt_by": "", "out": "", "error": format!("打不開：{e}")}));
                continue;
            }
        };
        let file = format!("{n:04}.{}", ext_of(name));
        std::fs::write(dir.join("src").join(&file), bytes).map_err(|e| format!("存 {file} 失敗：{e}"))?;
        save_thumb(&img, &dir.join("thumb").join(format!("{n:04}.jpg")))?;
        list.push(json!({"n": n, "file": file, "orig": orig, "w": img.width(), "h": img.height(), "kind": cb::classify(&img),
                         "status": "todo", "prompt": "", "prompt_by": "", "out": "", "error": "", "tries": 0}));
    }
    let title = Some(title).filter(|t| !t.is_empty()).or(zip_title.filter(|t| !t.is_empty())).unwrap_or_else(|| format!("整本上色 {}", chrono::Local::now().format("%m-%d %H:%M")));
    let book = json!({"id": id, "title": title, "created": crate::py::now(), "seed": rand::random_range(0..(1i64 << 31)), "sheet": "", "pages": list});
    cb::save(data, &book).map_err(|e| format!("存檔失敗：{e}"))?;
    Ok(book)
}

// ---------------------------------------------------------------- 配色表、改一頁
/// 這一頁放進 ComfyUI input（LLM 看圖和 Qwen 都從 input 讀），回傳 input 裡的檔名
fn ensure_input(app: &Shared, id: &str, page: &Value) -> Result<String, String> {
    let file = tx(&page["file"]);
    let name = format!("{id}_{file}");
    let to = app.paths.input.join(&name);
    if !to.is_file() {
        std::fs::copy(cb::book_dir(&app.paths.data, id).join("src").join(&file), &to).map_err(|e| format!("複製第 {} 頁到 input 失敗：{e}", page["n"]))?;
    }
    Ok(name)
}

/// LLM 回的配色表：拿掉 ``` 圍欄
fn clean_sheet(raw: &str) -> String {
    raw.lines().filter(|l| !l.trim_start().starts_with("```")).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// 用哪顆通用模型：{brain: "9b"} 用 9B，其他（沒給）照舊 35B（2026-10-31 使用者：「可以從35b/9b進行自由切換」）
fn brain_of(b: &Value) -> &'static str {
    if tx(&b["brain"]) == "9b" { "9b" } else { "35b" }
}

async fn write_sheet(app: &Shared, id: &str, book: &Value, brain: &str) -> Result<String, String> {
    let colors: Vec<&Value> = book["pages"].as_array().into_iter().flatten().filter(|p| p["kind"] == "color").take(SHEET_PAGES).collect();
    if colors.is_empty() {
        return Err("這本沒有彩頁：配色表可以自己寫（例：長捲髮的媽媽＝暗赤褐色頭髮；短髮的女兒＝藍黑色頭髮），或留空用自然配色".into());
    }
    let mut names = vec![];
    for p in &colors {
        names.push(ensure_input(app, id, p)?);
    }
    let r = app.pe.general_chat_as(&format!("colorbook-sheet-{id}"), cb::SYS_SHEET, &cb::sheet_request(names.len()), &names, 1500, 0.3, Some(brain)).await?;
    let s = clean_sheet(&r);
    if s.is_empty() {
        return Err("AI 沒寫出配色表：再按一次，或自己寫".into());
    }
    Ok(s)
}

/// {auto: true}＝AI 看彩頁寫；{text}＝存使用者改的（空字串＝用自然配色）
pub async fn sheet(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let book = load(&app, &id)?;
    if running(&id) {
        return Err(bad("正在上色：等跑完或先停止再改配色表"));
    }
    let text = if crate::py::truthy(b.get("auto")) {
        emit(&app, &id, "msg", "AI 看彩頁寫配色表…（第一次要載入模型，約半分鐘）", None);
        write_sheet(&app, &id, &book, brain_of(&b)).await.map_err(bad)?
    } else {
        tx(&b["text"]).chars().take(8000).collect()
    };
    let data = app.paths.data.clone();
    let b2 = blocking(move || update(&data, &id, |bk| bk["sheet"] = json!(text))).await.map_err(bad)?;
    ok(b2)
}

/// {i（頁碼，1 開始）, prompt?（空字串＝下次讓 AI 重寫）, kind?（color｜bw｜blank）}
pub async fn page(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let i = b.get("i").and_then(crate::py::int).filter(|i| *i >= 1).ok_or_else(|| bad("缺頁碼 i"))? as usize;
    let kind = b.get("kind").map(tx);
    if kind.as_deref().is_some_and(|k| !matches!(k, "color" | "bw" | "blank")) {
        return Err(bad("分類只有 color（彩頁）、bw（黑白）、blank（跳過）"));
    }
    let prompt = b.get("prompt").map(|v| tx(v).trim().chars().take(4000).collect::<String>());
    let data = app.paths.data.clone();
    let book = blocking(move || {
        update(&data, &id, |bk| {
            let Some(p) = bk["pages"].get_mut(i - 1) else { return };
            if let Some(t) = prompt {
                p["prompt_by"] = json!(if t.is_empty() { "" } else { "user" });
                p["prompt"] = json!(t);
            }
            if let Some(k) = kind {
                p["kind"] = json!(k);
            }
        })
    })
    .await
    .map_err(bad)?;
    if book["pages"].get(i - 1).is_none() {
        return Err(bad("沒有這一頁"));
    }
    ok(book)
}

// ---------------------------------------------------------------- 上色
/// {pages?:[頁碼], redo?, ai?（預設開：AI 看每頁寫指令）, keep_lines?（預設開：原稿亮度回貼）}
pub async fn run(State(app): State<Shared>, UrlPath(id): UrlPath<String>, Body(b): Body) -> ApiResult {
    let book = load(&app, &id)?;
    if running(&id) {
        return Err(bad("這本正在上色"));
    }
    if !app.comfy.live().online {
        return Err(bad("ComfyUI 沒有連線"));
    }
    let want: Vec<usize> = b["pages"].as_array().into_iter().flatten().filter_map(crate::py::int).filter(|i| *i >= 1).map(|i| i as usize).collect();
    let flag = |k: &str| b.get(k).map(|v| crate::py::truthy(Some(v))).unwrap_or(true);
    let (redo, ai, keep) = (crate::py::truthy(b.get("redo")), flag("ai"), flag("keep_lines"));
    let todo = cb::targets(&book, &want, redo);
    if todo.is_empty() {
        return Err(bad("沒有要上色的黑白頁：都上好了（要重上就勾「全部重上」或按那一頁的「重上」）"));
    }
    let token = CancellationToken::new();
    runs().lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), token.clone());
    let (a, id2, n, brain) = (app.clone(), id.clone(), todo.len(), brain_of(&b));
    crate::diagnostics::spawn(async move {
        let r = run_book(&a, &id2, &todo, ai, keep, brain, &token).await;
        runs().lock().unwrap_or_else(|e| e.into_inner()).remove(&id2);
        match r {
            Ok(msg) => emit(&a, &id2, "end", msg, None),
            Err(e) => emit(&a, &id2, if token.is_cancelled() { "end" } else { "error" }, e, None),
        }
    });
    ok(json!({"ok": true, "pages": n}))
}

pub async fn stop(UrlPath(id): UrlPath<String>) -> ApiResult {
    match runs().lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
        Some(t) => {
            t.cancel();
            ok(json!({"ok": true}))
        }
        None => Err(bad("這本沒有在上色")),
    }
}

async fn run_book(app: &Shared, id: &str, todo: &[usize], ai: bool, keep: bool, brain: &str, token: &CancellationToken) -> Result<String, String> {
    let data = app.paths.data.clone();
    let mut book = cb::load(&data, id).ok_or("找不到這本")?;
    // ① 配色表：沒有又有彩頁 → 先寫
    let has_color = book["pages"].as_array().is_some_and(|ps| ps.iter().any(|p| p["kind"] == "color"));
    if ai && tx(&book["sheet"]).trim().is_empty() && has_color {
        emit(app, id, "msg", "AI 看彩頁寫配色表…（第一次要載入模型，約半分鐘）", None);
        match write_sheet(app, id, &book, brain).await {
            Ok(s) => book = update(&data, id, |b| b["sheet"] = json!(s))?,
            Err(e) => emit(app, id, "msg", format!("配色表沒寫成（{e}），先用自然配色"), None),
        }
    }
    // ② 每頁的上色指令：全部寫完才換 Qwen
    let mut llm = ai;
    for (k, &i) in todo.iter().enumerate() {
        if token.is_cancelled() {
            return Err("已停止".into());
        }
        let p = book["pages"][i].clone();
        if !tx(&p["prompt"]).trim().is_empty() {
            continue;
        }
        let sheet = tx(&book["sheet"]);
        let (prompt, by) = if llm {
            emit(app, id, "msg", format!("AI 看第 {} 頁寫上色指令（{}／{}）…", p["n"], k + 1, todo.len()), None);
            let name = ensure_input(app, id, &p)?;
            match app.pe.general_chat_as(&format!("colorbook-page-{id}"), cb::SYS_PAGE, &cb::page_request(&sheet), &[name], 900, 0.3, Some(brain)).await {
                Ok(r) if !r.trim().is_empty() => (cb::fix_page_prompt(&r), "ai"),
                Ok(_) => (cb::plain_prompt(&sheet), "plain"),
                Err(e) => {
                    llm = false;
                    emit(app, id, "msg", format!("看圖模型用不了（{e}）：其他頁改用通用指令"), None);
                    (cb::plain_prompt(&sheet), "plain")
                }
            }
        } else {
            (cb::plain_prompt(&sheet), "plain")
        };
        book = update(&data, id, |b| {
            b["pages"][i]["prompt"] = json!(prompt);
            b["pages"][i]["prompt_by"] = json!(by);
        })?;
    }
    // ③ Qwen 2.1 一頁一頁上色（submit_jobs 會先關掉 LLM 讓出顯存）
    let (mut done, mut failed) = (0, 0);
    for (k, &i) in todo.iter().enumerate() {
        if token.is_cancelled() {
            return Err(format!("已停止：這次上好 {done} 頁"));
        }
        let p = cb::load(&data, id).ok_or("找不到這本")?["pages"][i].clone();
        let n = p["n"].as_u64().unwrap_or(i as u64 + 1) as usize;
        emit(app, id, "msg", format!("上色第 {n} 頁（{}／{}）…", k + 1, todo.len()), Some(n));
        match color_page(app, id, &p, keep, token).await {
            Ok(out) => {
                done += 1;
                update(&data, id, |b| {
                    let q = &mut b["pages"][i];
                    q["status"] = json!("done");
                    q["out"] = json!(out);
                    q["error"] = json!("");
                    q["tries"] = json!(q["tries"].as_i64().unwrap_or(0) + 1);
                })?;
                emit(app, id, "page", format!("第 {n} 頁上好了（{}／{}）", k + 1, todo.len()), Some(n));
            }
            Err(e) if token.is_cancelled() => return Err(format!("已停止：這次上好 {done} 頁（{e}）")),
            Err(e) => {
                failed += 1;
                update(&data, id, |b| {
                    b["pages"][i]["status"] = json!("error");
                    b["pages"][i]["error"] = json!(e);
                })?;
                emit(app, id, "page", format!("第 {n} 頁失敗：{e}"), Some(n));
            }
        }
    }
    Ok(if failed == 0 { format!("完成：上好 {done} 頁") } else { format!("跑完了：上好 {done} 頁、失敗 {failed} 頁（失敗的可以按「重上」）") })
}

/// 一頁：Qwen 2.1 改圖（只給這一頁，官方 Turbo 8 步）→ 原始輸出搬到 raw\、圖庫那筆隱藏 → 亮度回貼（或直接放大）→ out\0001.png、縮圖
async fn color_page(app: &Shared, id: &str, p: &Value, keep: bool, token: &CancellationToken) -> Result<String, String> {
    let book = cb::load(&app.paths.data, id).ok_or("找不到這本")?;
    let name = ensure_input(app, id, p)?;
    let n = p["n"].as_i64().unwrap_or(0);
    let seed = book["seed"].as_i64().unwrap_or_else(random_seed) + n * 7919 + p["tries"].as_i64().unwrap_or(0) * 104_729;
    let (w, h) = (p["w"].as_u64().unwrap_or(1024) as u32, p["h"].as_u64().unwrap_or(1024) as u32);
    let params = json!({
        "mode": "multiref", "model": crate::graphs::STRIP_QWEN, "refs": [{"image": name}], "size_mode": "auto", "pixels": cb::qwen_pixels(w, h),
        "prompt": tx(&p["prompt"]), "negative": "", "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple", "seed": seed % (1i64 << 48),
        "width": 1024, "height": 1024, "loras": [], "count": 1, "colorbook": id,
    });
    let jobs = submit_jobs(app, params.as_object().cloned().unwrap_or_default(), 1).await.map_err(|e| e.1)?;
    let pid = jobs.first().map(|j| tx(&j["job"])).ok_or("沒送出任務")?;
    wait_job(app, &pid, token).await?;
    let row = job_image(app, &pid)?;
    let raw = super::api_gallery::file_of(&app.paths, &row);
    let iid = row.get("id").and_then(Value::as_i64);
    let dir = cb::book_dir(&app.paths.data, id);
    let (file, tries) = (tx(&p["file"]), p["tries"].as_i64().unwrap_or(0));
    let out_name = format!("{n:04}.png");
    let (d2, out2) = (dir.clone(), out_name.clone());
    blocking(move || -> Result<(), String> {
        let colored = image::open(&raw).map_err(|e| format!("讀 Qwen 的結果失敗：{e}"))?.to_rgb8();
        let orig = image::open(d2.join("src").join(&file)).map_err(|e| format!("讀原頁失敗：{e}"))?.to_rgb8();
        let out = if keep {
            cb::keep_lines(&orig, &colored)
        } else {
            image::imageops::resize(&colored, orig.width(), orig.height(), image::imageops::FilterType::Lanczos3)
        };
        for sub in ["out", "othumb", "raw"] {
            std::fs::create_dir_all(d2.join(sub)).map_err(|e| e.to_string())?;
        }
        out.save(d2.join("out").join(&out2)).map_err(|e| format!("存成品失敗：{e}"))?;
        save_thumb(&out, &d2.join("othumb").join(format!("{n:04}.jpg")))?;
        // 原始輸出不留在圖庫：搬進這本的 raw\（搬不動就複製）
        let to: PathBuf = d2.join("raw").join(format!("{n:04}_{tries}.png"));
        if std::fs::rename(&raw, &to).is_err() {
            std::fs::copy(&raw, &to).map_err(|e| format!("搬原始輸出失敗：{e}"))?;
            let _ = std::fs::remove_file(&raw);
        }
        Ok(())
    })
    .await?;
    if let Some(iid) = iid {
        app.db.run("UPDATE images SET deleted=2 WHERE id=?", [iid]).map_err(|e| e.to_string())?;
    }
    Ok(out_name)
}

// ---------------------------------------------------------------- 看圖、下載、刪除
pub async fn file(State(app): State<Shared>, UrlPath((id, dir, name)): UrlPath<(String, String, String)>) -> ApiResult {
    if !cb::id_ok(&id) || !matches!(dir.as_str(), "src" | "thumb" | "out" | "othumb") {
        return Err(not_found("找不到"));
    }
    super::file_response(cb::book_dir(&app.paths.data, &id).join(dir).join(super::basename(&name)), Some("no-cache")).await
}

/// 整本打包：上好的頁用成品，彩頁、空白頁、還沒上的用原頁，照原本頁序（0001.png、0002.webp…）
pub async fn zip(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let book = load(&app, &id)?;
    let dir = cb::book_dir(&app.paths.data, &id);
    let bytes = blocking(move || -> Result<Vec<u8>, String> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for p in book["pages"].as_array().into_iter().flatten() {
            let out = tx(&p["out"]);
            let (path, name) = if p["status"] == "done" && p["kind"] == "bw" && !out.is_empty() {
                (dir.join("out").join(&out), out)
            } else {
                let f = tx(&p["file"]);
                if f.is_empty() {
                    continue;
                }
                (dir.join("src").join(&f), f)
            };
            let data = std::fs::read(&path).map_err(|e| format!("讀 {name} 失敗：{e}"))?;
            z.start_file(name, opt).map_err(|e| e.to_string())?;
            z.write_all(&data).map_err(|e| e.to_string())?;
        }
        Ok(z.finish().map_err(|e| e.to_string())?.into_inner())
    })
    .await
    .map_err(bad)?;
    let cd = format!("attachment; filename=\"colorbook.zip\"; filename*=UTF-8''{}.zip", pct(&tx(&load(&app, &id)?["title"])));
    Ok(([(axum::http::header::CONTENT_TYPE, "application/zip".to_string()), (axum::http::header::CONTENT_DISPOSITION, cd)], bytes).into_response())
}

/// Content-Disposition 的 filename*：英數和 -_. 以外都 %XX
fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// 整本搬到 <資料根>\待刪除\整本上色\<id>（不是真的刪）
pub async fn delete(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult {
    load(&app, &id)?;
    if running(&id) {
        return Err(bad("這本正在上色：先停止"));
    }
    let from = cb::book_dir(&app.paths.data, &id);
    let to = app.paths.root.join("待刪除").join("整本上色").join(&id);
    std::fs::create_dir_all(to.parent().unwrap_or(&app.paths.root)).map_err(|e| bad(e.to_string()))?;
    std::fs::rename(&from, &to).map_err(|e| bad(format!("搬到待刪除失敗：{e}")))?;
    ok(json!({"ok": true, "moved_to": to.to_string_lossy()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(color: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_fn(60, 80, |x, y| if (x + y) % 9 == 0 { image::Rgb([10, 10, 10]) } else { image::Rgb(color) });
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn create_from_zip_sorts_and_classifies() {
        let tmp = std::env::temp_dir().join(format!("cbtest_{}", rand::random::<u32>()));
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default();
            for (n, c) in [("book/10.png", [250, 250, 250]), ("book/2.png", [250, 250, 250]), ("book/1.png", [220, 60, 90]), ("__MACOSX/._1.png", [0, 0, 0])] {
                w.start_file(n, opt).unwrap();
                w.write_all(&png(c)).unwrap();
            }
            w.start_file("meta.json", opt).unwrap();
            w.write_all(b"{}").unwrap();
            w.finish().unwrap();
        }
        let book = create(&tmp, String::new(), vec![("我的本子.zip".into(), buf.into_inner())]).unwrap();
        let pages = book["pages"].as_array().unwrap();
        let got: Vec<(&str, &str)> = pages.iter().map(|p| (p["orig"].as_str().unwrap(), p["kind"].as_str().unwrap())).collect();
        assert_eq!(got, vec![("1.png", "color"), ("2.png", "bw"), ("10.png", "bw")]);
        assert_eq!(book["title"], "我的本子");
        let id = book["id"].as_str().unwrap();
        assert!(cb::id_ok(id));
        assert!(cb::book_dir(&tmp, id).join("src").join("0003.png").is_file() && cb::book_dir(&tmp, id).join("thumb").join("0001.jpg").is_file());
        assert_eq!(cb::list(&tmp)[0]["bw"], 2);
        assert!(create(&tmp, String::new(), vec![("a.txt".into(), b"hi".to_vec())]).unwrap_err().contains("沒有看得懂的圖片"));
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(pct("本 a.zip"), "%E6%9C%AC%20a.zip");
        assert_eq!(clean_sheet("```json\n{\"a\":1}\n```"), "{\"a\":1}");
    }
}

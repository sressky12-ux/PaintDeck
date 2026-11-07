//! 恢復原廠、tag 自動完成、預設組、LoRA 備註與移到待刪除（前幾樣對照舊版 server.py 同名的函式）。
use super::{bad, basename, blocking, file_response, internal, not_found, ok, ApiResult, Body, Shared};
use crate::maint;
use crate::director::tx;
use crate::py::now;
use axum::extract::{Path as UrlPath, Query, State};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

// ---------------------------------------------------------------- AI（MCP）連線狀態
/// MCP 伺服器（mcp-server.js）每 15 秒回報一次心跳；這裡只記最後一次，
/// 標題列的「AI MCP」燈號就看這個。85 秒沒收到＝AI 程式關掉了（或沒接）。
const AI_LINK_TTL: f64 = 85.0;

#[derive(Clone)]
struct AiSeen {
    at: f64,
    client: String,
    pid: i64,
}

static AI_LINK: Mutex<Option<AiSeen>> = Mutex::new(None);

/// POST /api/ai/heartbeat {client?, pid?}：MCP 伺服器說「我在」（不需要 confirm，什麼都不會改）
pub async fn ai_heartbeat(Body(b): Body) -> ApiResult {
    let client = {
        let c = tx(&b["client"]).trim().to_string();
        if c.is_empty() { "MCP".to_string() } else { c.chars().take(40).collect() }
    };
    let pid = b["pid"].as_i64().unwrap_or(0);
    if let Ok(mut g) = AI_LINK.lock() {
        *g = Some(AiSeen { at: now(), client, pid });
    }
    ok(json!({"ok": true}))
}

/// GET /api/ai/link：{online, client, age, ttl}。網頁拿來點 AI MCP 的燈號
pub async fn ai_link() -> ApiResult {
    let seen = AI_LINK.lock().ok().and_then(|g| g.clone());
    let age = seen.as_ref().map(|s| (now() - s.at).max(0.0)).unwrap_or(f64::MAX);
    let online = age <= AI_LINK_TTL;
    let (client, pid) = seen
        .filter(|_| online)
        .map(|s| (s.client, s.pid))
        .unwrap_or_default();
    ok(json!({
        "online": online,
        "client": client,
        "pid": pid,
        "age": if age.is_finite() { age } else { -1.0 },
        "ttl": AI_LINK_TTL,
    }))
}

// ---------------------------------------------------------------- 一鍵恢復原廠（可救回）
pub async fn reset_info(State(app): State<Shared>) -> ApiResult {
    ok(json!({"parts": maint::parts_json(), "history": maint::list_resets(&app.paths.root)}))
}

pub async fn reset_do(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    if body.get("confirm").and_then(Value::as_str) != Some("恢復原廠") {
        return Err(bad("請在確認框輸入「恢復原廠」四個字"));
    }
    let l = app.comfy.live();
    if l.running.is_some() || l.queue_remaining > 0 || !app.db.open_jobs()?.is_empty() {
        return Err(bad("還有任務在排隊或執行中，先按「全停」或等它跑完"));
    }
    let parts: Vec<String> = body
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    let a = app.clone();
    let r = blocking(move || {
        let p = &a.paths;
        let t = maint::ResetTarget {
            root: &p.root,
            output: &p.output,
            input: &p.input,
            thumbs: &p.thumbs,
            settings: &p.settings,
        };
        maint::reset(&parts, &t, &a.db)
    })
    .await
    .map_err(|e| bad(format!("恢復原廠失敗：{e}")))?;
    eprintln!(
        "恢復原廠：{}（搬走 {} 項到 待刪除\\{}）",
        r["parts"],
        r["moved"],
        r["name"].as_str().unwrap_or("")
    );
    ok(r)
}

pub async fn reset_undo(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let a = app.clone();
    let r = blocking(move || {
        maint::undo(
            &name,
            &a.paths.root,
            &a.paths.output,
            &a.db,
            &a.paths.settings,
        )
    })
    .await
    .map_err(|e| bad(format!("救回失敗：{e}")))?;
    ok(r)
}

// ---------------------------------------------------------------- 提示詞工具
pub async fn tags(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let Some(ix) = app.tags.get() else {
        return ok(json!([]));
    };
    let limit = q
        .get("limit")
        .map(|v| v.trim().parse::<usize>().map_err(|_| bad("limit 要是整數")))
        .transpose()?
        .unwrap_or(15);
    ok(json!(ix.search(
        q.get("q").map(String::as_str).unwrap_or(""),
        limit
    )))
}

/// POST /api/tags/zh {"tags": [...]}：提示詞框的中英對照，查對照表（不用模型，立刻回）。
/// 回 {"zh": {原字串: 中文}}，查不到的不放；一次最多 500 個
pub async fn tags_zh(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let tags = b["tags"].as_array().ok_or_else(|| bad("tags 要是字串陣列"))?;
    let mut zh = serde_json::Map::new();
    if let Some(ix) = app.tags.get() {
        for t in tags.iter().filter_map(Value::as_str).take(500) {
            if let Some(z) = ix.zh_of(t) {
                zh.insert(t.to_string(), Value::String(z));
            }
        }
    }
    ok(json!({"zh": zh}))
}

pub async fn presets(State(app): State<Shared>) -> ApiResult {
    ok(json!(app.db.presets()?))
}

pub async fn save_preset(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| bad("請輸入預設名稱"))?;
    let data = body.get("data").ok_or_else(|| bad("缺少 data"))?;
    let id = app.db.save_preset(name.trim(), data)?;
    ok(json!({"id": id}))
}

pub async fn delete_preset(State(app): State<Shared>, UrlPath(id): UrlPath<i64>) -> ApiResult {
    app.db.run("DELETE FROM presets WHERE id=?", [id])?;
    ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- LoRA
/// GET /api/features/strip：一鍵脫衣的內容補丁裝了沒＋UI 文案（前端據此顯示入口、填文字）
pub async fn feature_strip(State(app): State<Shared>) -> ApiResult {
    let pack = crate::feature_strip::load(&app.paths.data, &app.paths.res);
    ok(json!({"enabled": pack.is_some(), "ui": pack.map(|p| p.ui()).unwrap_or(Value::Null)}))
}

/// POST /api/features/strip/install：把補丁檔內容寫進 <資料>\plugins\strip\pack.json（body 是 pack 本身，或 {pack:…}）
pub async fn feature_strip_install(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let pack = body.get("pack").cloned().unwrap_or(body);
    if pack.get("qwen").is_none() || pack.get("ui").is_none() {
        return Err(bad("這不像「一鍵脫衣」的補丁檔（缺 qwen 或 ui）"));
    }
    let dir = crate::feature_strip::dir(&app.paths.data);
    std::fs::create_dir_all(&dir).map_err(|e| bad(e.to_string()))?;
    std::fs::write(dir.join(crate::feature_strip::PACK), serde_json::to_vec_pretty(&pack).unwrap()).map_err(|e| bad(e.to_string()))?;
    ok(json!({"ok": true, "ui": pack.get("ui").cloned().unwrap_or(Value::Null)}))
}

pub async fn civitai_bases(State(app): State<Shared>) -> ApiResult {
    let bases = app.civitai.bases().await.map_err(bad)?;
    ok(json!({"bases": bases}))
}

pub async fn lora_meta(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("缺少 name"))?;
    let mut meta = app
        .db
        .lora_meta()?
        .get(name)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for k in [
        "triggers", "strength", "note", "category", "tags", "fav", "title", "min", "max", "slider", "agent",
    ] {
        if let Some(v) = body.get(k) {
            meta.insert(k.into(), v.clone());
        }
    }
    let meta = Value::Object(meta);
    app.db.set_lora_meta(name, &meta)?;
    ok(meta)
}

/// LoRA 檔在哪：name 是 ComfyUI 清單裡的名字（可能帶子資料夾，例 `sub\x.safetensors`），只能指到 LoRA 資料夾裡面
fn lora_file(roots: &[PathBuf], name: &str) -> Result<PathBuf, String> {
    // Windows 的 ComfyUI 給的 LoRA 名稱是 sub\x.safetensors；Linux 的 Path 不認 \，換成 /（..\ 一樣擋得到）
    let norm = if cfg!(windows) { name.to_string() } else { name.replace('\\', "/") };
    let rel = Path::new(&norm);
    if name.trim().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("LoRA 名稱不對：{name}"));
    }
    roots
        .iter()
        .map(|r| r.join(rel))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("找不到這個 LoRA 檔（可能已經搬走了）：{name}。按「重新整理」更新清單"))
}

/// 匯入或手動放入的 LoRA 常把預覽圖放在模型旁邊；只接受同名圖片，且路徑必須落在 LoRA 根目錄內。
pub(super) fn local_lora_preview(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    let model = lora_file(roots, name).ok()?;
    let stem = model.file_stem()?.to_str()?;
    let parent = model.parent()?;
    for suffix in [".preview.png", ".preview.jpg", ".preview.jpeg", ".preview.webp", ".png", ".jpg", ".jpeg", ".webp"] {
        let path = parent.join(format!("{stem}{suffix}"));
        if path.is_file() { return Some(path); }
    }
    None
}

pub async fn lora_local_preview(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let name = q.get("name").ok_or_else(|| bad("缺少 LoRA 名稱"))?;
    let path = local_lora_preview(&app.paths.lora_roots(), name).ok_or_else(|| not_found("找不到預覽圖"))?;
    file_response(path, Some("max-age=86400")).await
}

#[cfg(test)]
mod local_preview_tests {
    use super::local_lora_preview;

    #[test]
    fn finds_sidecar_only_beside_a_lora_inside_its_root() {
        let temp = std::env::temp_dir().join(format!("anima_lora_preview_{}", std::process::id()));
        let root = temp.join("loras");
        let group = root.join("krea2");
        std::fs::create_dir_all(&group).unwrap();
        let model = group.join("sample.safetensors");
        let image = group.join("sample.preview.png");
        std::fs::write(&model, b"test").unwrap();
        std::fs::write(&image, b"png").unwrap();
        assert_eq!(local_lora_preview(std::slice::from_ref(&root), "krea2\\sample.safetensors"), Some(image.clone()));
        assert!(local_lora_preview(std::slice::from_ref(&root), "..\\outside.safetensors").is_none());
        std::fs::remove_file(image).unwrap();
        std::fs::remove_file(model).unwrap();
        std::fs::remove_dir(group).unwrap();
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(temp).unwrap();
    }
}

/// 移到待刪除（不真的刪）：{name} → LoRA 檔和它的預覽圖搬到 <資料>\待刪除\LoRA\<時間>_<檔名>\。
/// 觸發詞、強度、備註留在資料庫：檔案搬回 LoRA 資料夾就全部回來。有任務在排隊時不搬（排到它會找不到檔）
pub async fn lora_delete(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let l = app.comfy.live();
    if l.running.is_some() || l.queue_remaining > 0 || !app.db.open_jobs()?.is_empty() {
        return Err(bad("還有任務在排隊或執行中：等跑完再移"));
    }
    let src = lora_file(&app.paths.lora_roots(), &name).map_err(not_found)?;
    let file = src.file_name().map(|f| f.to_os_string()).unwrap_or_default();
    let stem = Path::new(&file).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = app.paths.root.join("待刪除").join("LoRA").join(format!("{}_{stem}", chrono::Local::now().format("%Y%m%d_%H%M%S")));
    let preview = app.db.lora_meta()?.get(&name).and_then(|m| m.get("preview")).and_then(Value::as_str).map(|p| app.paths.previews.join(basename(p)));
    let d = dir.clone();
    blocking(move || -> std::io::Result<()> {
        maint::move_path(&src, &d.join(&file))?;
        if let Some(p) = preview.filter(|p| p.is_file()) {
            // 預覽圖搬不動不影響（LoRA 本體已經搬好了）
            let _ = maint::move_path(&p, &d.join(p.file_name().unwrap_or_default()));
        }
        Ok(())
    })
    .await
    .map_err(internal)?;
    ok(json!({"ok": true, "moved_to": dir.display().to_string()}))
}

pub async fn lora_preview(State(app): State<Shared>, UrlPath(file): UrlPath<String>) -> ApiResult {
    file_response(
        app.paths.previews.join(basename(&file)),
        Some("max-age=86400"),
    )
    .await
}

// ---------------------------------------------------------------- LoRA 封面（2026-11-07，純邏輯在 crate::lora_cover）
/// 正在抓的名稱（同一個 LoRA 不重複抓）、抓不到的名稱和時間（30 分鐘內不再試，免得每次打開 LoRA 庫都去敲 C 站）
static COVER_BUSY: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
static COVER_FAILED: Mutex<Option<HashMap<String, std::time::Instant>>> = Mutex::new(None);
const COVER_RETRY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

fn cover_http() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .proxy(crate::net::proxy())
            .user_agent("Mozilla/5.0 (anima-app lora-cover)")
            // 連不上的來源（例如大陸連 C 站）8 秒就換下一個
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(40))
            .build()
            .expect("建立 HTTP client")
    })
}

/// 抓一張圖：要是真的圖、不超過 15MB。回傳錯誤時第二個值＝連不上這台主機（這一批後面的就跳過它）
async fn cover_get(url: &str) -> Result<Vec<u8>, (String, bool)> {
    let r = cover_http().get(url).send().await.map_err(|e| (crate::net::explain(&e, url), e.is_connect() || e.is_timeout()))?;
    if !r.status().is_success() {
        return Err((format!("HTTP {}", r.status().as_u16()), false));
    }
    if r.content_length().is_some_and(|n| n > 15 << 20) {
        return Err(("圖太大".into(), false));
    }
    let b = r.bytes().await.map_err(|e| (crate::net::explain(&e, url), e.is_timeout()))?;
    if b.len() > 15 << 20 || !crate::lora_cover::is_image(&b) {
        return Err(("不是圖片".into(), false));
    }
    Ok(b.to_vec())
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_lowercase()
}

/// 照來源順序試，第一個抓得到的 → (圖, 尺度等級)
async fn cover_fetch(app: &Shared, sources: &[crate::lora_cover::Source], down: &Mutex<std::collections::HashSet<String>>) -> Option<(Vec<u8>, Option<i64>)> {
    use crate::lora_cover::{pick_civitai, Source, CIVITAI_API};
    let is_down = |h: &str| down.lock().map(|d| d.contains(h)).unwrap_or(false);
    let mark = |h: String| {
        if let Ok(mut d) = down.lock() {
            d.insert(h);
        }
    };
    for s in sources {
        let (url, nsfw) = match s {
            Source::Img { url, nsfw } => (url.clone(), *nsfw),
            Source::CivitaiVersion(id) => {
                if is_down("civitai.com") {
                    continue;
                }
                // 走 civitai.rs（有填金鑰就帶，跟下載中心看到的一樣）
                match app.civitai.get_json(&format!("{CIVITAI_API}/model-versions/{id}"), &[]).await {
                    Ok(v) => match pick_civitai(&v) {
                        Some((u, n)) => (u, Some(n)),
                        None => continue,
                    },
                    Err(e) => {
                        if !e.starts_with("Civitai 回應") {
                            mark("civitai.com".into());
                        }
                        continue;
                    }
                }
            }
        };
        let host = host_of(&url);
        if is_down(&host) {
            continue;
        }
        match cover_get(&url).await {
            Ok(b) => return Some((b, nsfw)),
            Err((_, unreachable)) => {
                if unreachable {
                    mark(host);
                }
            }
        }
    }
    None
}

/// POST /api/loras/covers {names:[…]}：網頁打開 LoRA 庫時，把沒有預覽圖、有封面來源的 LoRA（/api/state 的 cover: true）送來。
/// 背景照 lora_covers.json 的順序抓（C 站 → 魔塔 → Hugging Face），存進 <資料>\previews\、寫回 LoRA 資訊的 preview／preview_nsfw，
/// 每抓完一個廣播 {type:"lora_cover", name, preview, nsfw}（抓不到是 failed: true）。回 {queued}：這次真的排進去幾個
pub async fn lora_covers(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let names: Vec<String> = body["names"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).take(2000).collect();
    let covers = crate::lora_cover::load(&app.paths.res);
    let meta = app.db.lora_meta()?;
    let roots = app.paths.lora_roots();
    let previews = app.paths.previews.clone();
    let mut todo: Vec<(String, Vec<crate::lora_cover::Source>)> = vec![];
    {
        let mut busy = COVER_BUSY.lock().unwrap_or_else(|e| e.into_inner());
        let busy = busy.get_or_insert_with(Default::default);
        let mut failed = COVER_FAILED.lock().unwrap_or_else(|e| e.into_inner());
        let failed = failed.get_or_insert_with(Default::default);
        failed.retain(|_, at| at.elapsed() < COVER_RETRY);
        for n in names {
            if busy.contains(&n) || failed.contains_key(&n) || lora_file(&roots, &n).is_err() {
                continue;
            }
            let m = meta.get(&n);
            if m.and_then(|m| m["preview"].as_str()).is_some_and(|p| previews.join(basename(p)).is_file()) {
                continue;
            }
            let s = covers.sources(&n, m);
            if !s.is_empty() {
                busy.insert(n.clone());
                todo.push((n, s));
            }
        }
    }
    let queued = todo.len();
    if queued > 0 {
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let _ = tokio::fs::create_dir_all(&previews).await;
            let down = Mutex::new(std::collections::HashSet::new());
            let (app, down, previews) = (&app, &down, &previews);
            futures_util::stream::iter(todo)
                .for_each_concurrent(4, |(name, sources)| async move {
                    let got = cover_fetch(app, &sources, down).await;
                    let saved = match got {
                        Some((bytes, nsfw)) => {
                            // 寫回前重讀一次（使用者可能剛改了觸發詞），只動 preview／preview_nsfw
                            let cur = app.db.lora_meta().ok().and_then(|m| m.get(&name).cloned());
                            let file = crate::lora_cover::file_name(&name, cur.as_ref(), previews);
                            let mut m = cur.and_then(|v| v.as_object().cloned()).unwrap_or_default();
                            if tokio::fs::write(previews.join(&file), &bytes).await.is_ok() {
                                m.insert("preview".into(), json!(file));
                                m.insert("preview_nsfw".into(), json!(nsfw.unwrap_or(1)));
                                app.db.set_lora_meta(&name, &Value::Object(m)).ok().map(|_| (file, nsfw.unwrap_or(1)))
                            } else {
                                None
                            }
                        }
                        None => None,
                    };
                    match &saved {
                        Some((file, nsfw)) => app.hub.send(json!({"type": "lora_cover", "name": name, "preview": file, "nsfw": nsfw})),
                        None => {
                            if let Ok(mut f) = COVER_FAILED.lock() {
                                f.get_or_insert_with(Default::default).insert(name.clone(), std::time::Instant::now());
                            }
                            app.hub.send(json!({"type": "lora_cover", "name": name, "failed": true}));
                        }
                    }
                    if let Ok(mut b) = COVER_BUSY.lock() {
                        b.get_or_insert_with(Default::default).remove(&name);
                    }
                })
                .await;
        });
    }
    ok(json!({"queued": queued}))
}

// ---------------------------------------------------------------- Civitai
pub async fn civitai_search(
    State(app): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let g = |k: &str, d: &str| q.get(k).cloned().unwrap_or_else(|| d.to_string());
    let base = g("base", "Anima");
    let mut r = app
        .civitai
        .search(
            &g("query", ""),
            &g("sort", "Most Downloaded"),
            &g("period", "AllTime"),
            g("nsfw", "1") == "1",
            q.get("cursor").map(String::as_str),
            q.get("tag").map(String::as_str),
            &base,
        )
        .await
        .map_err(|e| bad(format!("Civitai 搜尋失敗：{e}")))?;
    let installed = app.civitai.installed_names(&base);
    let has = |f: &Value| f.as_str().map(|f| installed.contains(f)).unwrap_or(false);
    for it in r["items"].as_array_mut().into_iter().flatten() {
        it["installed"] = json!(!it["version"].is_null() && has(&it["version"]["file"]));
        for v in it["versions"].as_array_mut().into_iter().flatten() {
            v["installed"] = json!(has(&v["file"]));
        }
    }
    ok(r)
}

pub async fn civitai_download(State(app): State<Shared>, Body(body): Body) -> ApiResult {
    let model = body
        .get("modelId")
        .and_then(crate::py::int)
        .ok_or_else(|| bad("缺少 modelId"))?;
    let version = body
        .get("versionId")
        .filter(|v| crate::py::truthy(Some(v)))
        .and_then(crate::py::int);
    let base = body.get("base").and_then(Value::as_str).unwrap_or("Anima");
    ok(json!({"key": app.civitai.start_download(model, version, base)}))
}

pub async fn downloads(State(app): State<Shared>) -> ApiResult {
    let all = app
        .civitai
        .downloads
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();
    ok(json!(all))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lora_file_stays_inside_roots() {
        let t = std::env::temp_dir().join(format!("anima_lora_del_{}", std::process::id()));
        let (a, b) = (t.join("a"), t.join("b"));
        std::fs::create_dir_all(b.join("sub")).unwrap();
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(b.join("sub").join("x.safetensors"), b"x").unwrap();
        std::fs::write(t.join("secret.safetensors"), b"s").unwrap();
        let roots = vec![a.clone(), b.clone()];
        assert_eq!(lora_file(&roots, "sub\\x.safetensors").unwrap(), b.join("sub").join("x.safetensors"));
        assert_eq!(lora_file(&roots, "sub/x.safetensors").unwrap(), b.join("sub").join("x.safetensors"));
        assert!(lora_file(&roots, "..\\secret.safetensors").is_err());
        assert!(lora_file(&roots, &t.join("secret.safetensors").display().to_string()).is_err());
        assert!(lora_file(&roots, "").is_err());
        assert!(lora_file(&roots, "nope.safetensors").unwrap_err().contains("找不到"));
        let _ = std::fs::remove_dir_all(&t);
    }
}

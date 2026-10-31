//! 下載中心 API（2026-10-29，純邏輯在 crate::modelhub，設計見 design\下載中心_設計.md）：
//!   GET  /api/hub/sources                         來源、各來源的種類、家族、金鑰有沒有填
//!   GET  /api/hub/search?source&kind&q&family&sort&page&cursor&nsfw&tag（tag＝魔塔的分類標籤）
//!   GET  /api/hub/tags?source=modelscope          魔塔的分類標籤清單
//!   GET  /api/hub/files?source=modelscope&id=作者/名稱     魔塔的檔案清單（C 站的檔案搜尋時就有）
//!   POST /api/hub/download {source, id, kind, family, base, version, file:{name, path, size, sha256, url}}（確認類）
//!   POST /api/hub/workflow {source, id, version}  抓工作流檔（C 站 Workflows、RunningHub）→ {file_name, data_b64}，網頁再交給工作流匯入
//!   GET、POST /api/hub/modelscope {key}、POST /api/hub/modelscope/test     魔塔金鑰（DPAPI 加密存在 data\hub\modelscope.json）
//!   POST /api/hub/rh_run {id, name?, prompt?, image?, dry?}、POST /api/hub/rh_stop {task}   在 RunningHub 跑那個工作流（花錢，確認類）
//!
//! 下載：C 站的 LoRA 照舊走 civitai.rs（會存觸發詞、預覽圖）；其他（C 站主模型／VAE、魔塔全部）走 api_models::download_known
//! （先存 _incoming、核對 SHA256、看檔頭歸位），進度都在網頁原本的下載清單。下載網址由這裡依來源組，不收網頁給的任意網址。
use super::{bad, blocking, ok, ApiResult, Body, Shared};
use crate::director::tx;
use crate::modelhub as mh;
use axum::extract::{Query, State};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const RH_SEARCH: &str = "https://www.runninghub.cn/api/sku/list";

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .proxy(crate::net::proxy())
            .user_agent("Mozilla/5.0 (anima-app hub)")
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()
            .expect("建立 HTTP client")
    })
}

fn q<'a>(m: &'a HashMap<String, String>, k: &str) -> &'a str {
    m.get(k).map(String::as_str).unwrap_or("").trim()
}

// ---------------------------------------------------------------- 魔塔金鑰
fn ms_cfg_path(app: &Shared) -> PathBuf {
    app.paths.data.join("hub").join("modelscope.json")
}

fn ms_key(app: &Shared) -> String {
    std::fs::read(ms_cfg_path(app))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["key_dpapi"].as_str().and_then(super::api_runninghub::unseal))
        .unwrap_or_default()
}

/// 登入後的 Cookie（魔塔 SDK 的做法：POST /api/v1/login {AccessToken} 換 Cookie），同一把金鑰快取 6 小時
static MS_COOKIE: Mutex<Option<(u64, String, Instant)>> = Mutex::new(None);

fn key_tag(k: &str) -> u64 {
    k.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// 沒填金鑰是 Ok(None)；登入失敗回魔塔的說明
async fn ms_login(app: &Shared) -> Result<Option<String>, String> {
    let key = ms_key(app);
    if key.is_empty() {
        return Ok(None);
    }
    let tag = key_tag(&key);
    if let Some((t, c, at)) = MS_COOKIE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if t == tag && at.elapsed() < Duration::from_secs(6 * 3600) {
            return Ok(Some(c));
        }
    }
    let r = http()
        .post(format!("{}/login", mh::MS_API))
        .json(&json!({"AccessToken": key}))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| crate::net::explain(&e, mh::MS_API))?;
    let cookie: Vec<String> = r
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next().map(|s| s.trim().to_string()))
        .filter(|s| s.contains('='))
        .collect();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if v["Success"] != true || cookie.is_empty() {
        return Err(format!("魔塔登入失敗：{}", v["Message"].as_str().unwrap_or("（沒有說明）")));
    }
    let c = cookie.join("; ");
    *MS_COOKIE.lock().unwrap_or_else(|e| e.into_inner()) = Some((tag, c.clone(), Instant::now()));
    Ok(Some(c))
}

/// 搜尋、檔案清單用：登入失敗也照樣查（公開模型不用金鑰）
async fn ms_cookie_soft(app: &Shared) -> Option<String> {
    ms_login(app).await.ok().flatten()
}

pub async fn ms_get(State(app): State<Shared>) -> ApiResult {
    ok(json!({"has_key": !ms_key(&app).is_empty()}))
}

/// {key}：空字串＝清掉
pub async fn ms_set(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let k = tx(&b["key"]).trim().to_string();
    let p = ms_cfg_path(&app);
    let mut m = Map::new();
    if !k.is_empty() {
        if k.len() > 200 || !k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return Err(bad("魔塔 Access Token 的格式不對：到魔塔「個人中心 → 訪問令牌」整串複製過來"));
        }
        m.insert("key_dpapi".into(), json!(super::api_runninghub::seal(&k).map_err(bad)?));
    }
    std::fs::create_dir_all(p.parent().unwrap_or(&app.paths.data)).map_err(|e| bad(e.to_string()))?;
    crate::director::write_atomic(&p, &Value::Object(m)).map_err(|e| bad(format!("存魔塔設定失敗：{e}")))?;
    *MS_COOKIE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    ok(json!({"has_key": !k.is_empty()}))
}

pub async fn ms_test(State(app): State<Shared>) -> ApiResult {
    *MS_COOKIE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    match ms_login(&app).await {
        Ok(Some(_)) => ok(json!({"ok": true})),
        Ok(None) => Err(bad("還沒填魔塔 Access Token")),
        Err(e) => Err(bad(e)),
    }
}

// ---------------------------------------------------------------- 來源
pub async fn sources(State(app): State<Shared>) -> ApiResult {
    let s = crate::settings::load(&app.paths.settings);
    let kinds = |src: &str| mh::kinds_of(src).to_vec();
    ok(json!({
        "sources": [
            {"id": "civitai", "label": "C 站（Civitai）", "kinds": kinds("civitai"), "has_key": !crate::settings::civitai_key(&s).is_empty()},
            {"id": "modelscope", "label": "魔塔（ModelScope）", "kinds": kinds("modelscope"), "has_key": !ms_key(&app).is_empty()},
            {"id": "runninghub", "label": "RunningHub", "kinds": kinds("runninghub"), "has_key": !super::api_runninghub::cfg(&app).key.is_empty()},
        ],
        "families": mh::FAMILIES.iter().map(|f| json!({"id": f.id, "label": f.label, "usable": f.usable})).collect::<Vec<_>>(),
    }))
}

/// 這台電腦已經有的模型檔名（小寫）：loras、主模型、VAE、文字編碼器的資料夾（含外部模型資料夾），最多往下看 4 層
fn installed(app: &Shared) -> HashSet<String> {
    let models = app.paths.comfy_root.join("models");
    let mut roots: Vec<PathBuf> = ["loras", "diffusion_models", "unet", "checkpoints", "vae", "text_encoders", "clip"].iter().map(|k| models.join(k)).collect();
    roots.extend(super::api_wf::model_roots(&app.paths));
    for k in ["loras", "vae"] {
        roots.extend(crate::model_dirs::roots(k));
    }
    let mut out = HashSet::new();
    let mut stack: Vec<(PathBuf, u8)> = roots.into_iter().map(|r| (r, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
            } else if out.len() < 50_000 {
                out.insert(e.file_name().to_string_lossy().to_lowercase());
            }
        }
    }
    out
}

/// 每筆（和每個版本）標上 installed：主要檔案這台電腦已經有了
fn mark_installed(items: &mut [Value], have: &HashSet<String>) {
    for it in items.iter_mut() {
        let mut any = false;
        if let Some(vs) = it["versions"].as_array_mut() {
            for v in vs {
                let hit = v["files"].as_array().into_iter().flatten().any(|f| f["primary"] == true && f["name"].as_str().is_some_and(|n| have.contains(&n.to_lowercase())));
                v["installed"] = json!(hit);
                any |= hit;
            }
        }
        it["installed"] = json!(any);
    }
}

pub async fn search(State(app): State<Shared>, Query(m): Query<HashMap<String, String>>) -> ApiResult {
    let (source, kind, fam, text, sort) = (q(&m, "source"), q(&m, "kind"), q(&m, "family"), q(&m, "q"), q(&m, "sort"));
    if !mh::kinds_of(source).contains(&kind) {
        return Err(bad(format!("{source} 沒有「{kind}」這個種類")));
    }
    if !fam.is_empty() && mh::family(fam).is_none() {
        return Err(bad(format!("不認得底模「{fam}」")));
    }
    let page: u32 = q(&m, "page").parse().unwrap_or(1);
    let mut out = match source {
        "civitai" => {
            let nsfw = matches!(q(&m, "nsfw"), "1" | "true");
            let query = mh::civitai_query(kind, fam, text, sort, nsfw, q(&m, "cursor"));
            let d = app.civitai.get_json(&format!("{}/models", crate::civitai::API), &query).await.map_err(bad)?;
            let items: Vec<Value> = d["items"].as_array().into_iter().flatten().filter_map(|x| mh::civitai_item(x, kind, fam)).collect();
            json!({"items": items, "next": d["metadata"]["nextCursor"].clone(), "total": Value::Null})
        }
        "modelscope" => {
            let cookie = ms_cookie_soft(&app).await;
            let tag = q(&m, "tag");
            // Anima 這種靠關鍵字找的家族：撈回來會濾掉一部分（別的底模的「anima 風」），一頁拿 48 筆；整頁都被濾光才再翻（最多 3 頁）
            let by_keyword = mh::family(fam).is_some_and(|f| f.modelscope.is_none());
            let size: u32 = if by_keyword { 48 } else { 24 };
            let (mut items, mut p) = (vec![], page.max(1));
            let total = loop {
                let v = ms_search(cookie.as_deref(), &mh::ms_body(kind, fam, text, p, size, sort, tag)).await.map_err(bad)?;
                let (got, t) = mh::ms_items(&v, kind, fam).map_err(bad)?;
                items.extend(got);
                if !by_keyword || !items.is_empty() || u64::from(p) * u64::from(size) >= t || p >= page.max(1) + 2 {
                    break t;
                }
                p += 1;
            };
            let more = u64::from(p) * u64::from(size) < total;
            json!({"items": items, "total": total, "next": if more { json!(p + 1) } else { Value::Null }})
        }
        _ => {
            let r = http().post(RH_SEARCH).json(&mh::rh_body(text, page)).timeout(Duration::from_secs(40)).send().await.map_err(|e| bad(crate::net::explain(&e, RH_SEARCH)))?;
            let v: Value = r.json().await.map_err(|_| bad("RunningHub 回應看不懂，可能是網站改版：先到 RunningHub 網頁版找"))?;
            let (items, total) = mh::rh_items(&v).map_err(bad)?;
            let more = u64::from(page) * 24 < total;
            json!({"items": items, "total": total, "next": if more { json!(page + 1) } else { Value::Null }})
        }
    };
    if kind != "workflow" {
        let a = app.clone();
        let have = blocking(move || installed(&a)).await;
        if let Some(items) = out["items"].as_array_mut() {
            mark_installed(items, &have);
        }
    }
    ok(out)
}

/// 魔塔的搜尋偶爾整個卡住不回（2026-10-30 實測：同一個請求一次 3 秒、一次 3 分鐘還沒回完）：每次最多等 15 秒，逾時或連不上重試，最多 3 次
async fn ms_search(cookie: Option<&str>, body: &Value) -> Result<Value, String> {
    let url = format!("{}/dolphin/models", mh::MS_API);
    let mut last = String::new();
    for _ in 0..3 {
        let mut rb = http().put(&url).json(body);
        if let Some(c) = cookie {
            rb = rb.header(reqwest::header::COOKIE, c);
        }
        let r = match rb.timeout(Duration::from_secs(15)).send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() || e.is_connect() => {
                last = crate::net::explain(&e, &url);
                continue;
            }
            Err(e) => return Err(crate::net::explain(&e, &url)),
        };
        let status = r.status();
        match r.json::<Value>().await {
            Ok(v) => return Ok(v),
            Err(e) if e.is_timeout() => last = crate::net::explain(&e, &url),
            Err(_) => return Err(format!("魔塔回應看不懂（HTTP {status}），可能是網站改版：先到魔塔網頁版找")),
        }
    }
    Err(format!("魔塔一直沒回應（試了 3 次）：{last}"))
}

/// 魔塔的分類標籤（網頁版上面那排「寫實攝影、二次元…」），快取一天；別的來源沒有，回空的
static MS_TAGS: Mutex<Option<(Vec<Value>, Instant)>> = Mutex::new(None);

pub async fn tags(Query(m): Query<HashMap<String, String>>) -> ApiResult {
    if q(&m, "source") != "modelscope" {
        return ok(json!({"tags": []}));
    }
    if let Some((list, at)) = MS_TAGS.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if at.elapsed() < Duration::from_secs(24 * 3600) {
            return ok(json!({"tags": list}));
        }
    }
    let url = format!("{}/models/aigc/tags", mh::MS_API);
    let r = http().get(&url).timeout(Duration::from_secs(30)).send().await.map_err(|e| bad(crate::net::explain(&e, &url)))?;
    let v: Value = r.json().await.map_err(|_| bad("魔塔的分類標籤看不懂"))?;
    let list = mh::ms_tags(&v).map_err(bad)?;
    *MS_TAGS.lock().unwrap_or_else(|e| e.into_inner()) = Some((list.clone(), Instant::now()));
    ok(json!({"tags": list}))
}

pub async fn files(State(app): State<Shared>, Query(m): Query<HashMap<String, String>>) -> ApiResult {
    let id = q(&m, "id");
    if q(&m, "source") != "modelscope" {
        return Err(bad("只有魔塔要另外查檔案清單"));
    }
    if !mh::ms_id_ok(id) {
        return Err(bad("魔塔的模型代號看不懂（要是 作者/名稱）"));
    }
    let url = format!("{}/models/{id}/repo/files?Revision=master&Recursive=true", mh::MS_API);
    let mut rb = http().get(&url);
    if let Some(c) = ms_cookie_soft(&app).await {
        rb = rb.header(reqwest::header::COOKIE, c);
    }
    let r = rb.timeout(Duration::from_secs(40)).send().await.map_err(|e| bad(crate::net::explain(&e, mh::MS_API)))?;
    let v: Value = r.json().await.map_err(|_| bad("魔塔的檔案清單看不懂"))?;
    let mut fs = mh::ms_files(&v, id).map_err(bad)?;
    let a = app.clone();
    let have = blocking(move || installed(&a)).await;
    for f in fs.iter_mut() {
        f["installed"] = json!(f["name"].as_str().is_some_and(|n| have.contains(&n.to_lowercase())));
    }
    ok(json!({"files": fs}))
}

// ---------------------------------------------------------------- 下載
/// C 站非 LoRA 的檔：下載網址只收 https://civitai.com/api/download/models/…
fn civitai_url_ok(u: &str) -> bool {
    reqwest::Url::parse(u).is_ok_and(|u| u.scheme() == "https" && u.host_str() == Some("civitai.com") && u.path().starts_with("/api/download/models/"))
}

pub async fn download(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let (source, kind, id) = (tx(&b["source"]), tx(&b["kind"]), tx(&b["id"]).trim().to_string());
    let fam = Some(tx(&b["family"])).filter(|f| mh::family(f).is_some());
    let base = tx(&b["base"]);
    let f = &b["file"];
    let name = super::basename(&tx(&f["name"]));
    if !matches!(kind.as_str(), "lora" | "model" | "vae") {
        return Err(bad("只能下載 LoRA、主模型、VAE（工作流用匯入）"));
    }
    if name.is_empty() || !crate::model_dirs::is_model_file(Path::new(&name)) {
        return Err(bad(format!("「{name}」不是模型檔")));
    }
    let class = match kind.as_str() {
        "lora" => Some("loras".to_string()),
        "vae" => Some("vae".to_string()),
        _ => None, // 主模型：看檔頭分 UNET／整包 checkpoint
    };
    let sub = (kind == "lora").then(|| mh::lora_subdir(fam.as_deref(), &base));
    let sha = f["sha256"].as_str().filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())).map(str::to_lowercase);
    match source.as_str() {
        "civitai" => {
            let model_id: i64 = id.parse().map_err(|_| bad("C 站模型編號要是數字"))?;
            let version = b["version"].as_i64();
            if kind == "lora" {
                // 照舊走 civitai.rs：會存觸發詞、預覽圖、LoRA 資訊
                let key = app.civitai.start_download(model_id, version, if base.is_empty() { "*" } else { &base });
                return ok(json!({"ok": true, "via": "civitai", "key": key}));
            }
            let url = tx(&f["url"]);
            if !civitai_url_ok(&url) {
                return Err(bad("C 站的下載網址不對"));
            }
            let key = crate::settings::civitai_key(&crate::settings::load(&app.paths.settings));
            let k = super::api_models::Known { url, name, size: None, sha256: sha, kind: class, sub: None, bearer: Some(key).filter(|k| !k.is_empty()), cookie: None };
            ok(json!({"ok": true, "via": "models", "id": super::api_models::download_known(&app, k, None)}))
        }
        "modelscope" => {
            if !mh::ms_id_ok(&id) {
                return Err(bad("魔塔的模型代號看不懂（要是 作者/名稱）"));
            }
            let path = tx(&f["path"]);
            if path.is_empty() || path.split('/').any(|s| s == ".." || s == ".") || path.contains('\\') {
                return Err(bad("魔塔的檔案路徑不對"));
            }
            let cookie = ms_cookie_soft(&app).await;
            let url = mh::ms_download_url(&id, &path);
            // LoRA：歸位後存 LoRA 資訊（觸發詞、底模、來源網址），LoRA 庫、家族判斷才看得到
            let done: Option<super::api_models::OnPlaced> = (kind == "lora").then(|| {
                let (db, sub, file) = (app.db.clone(), sub.clone().unwrap_or_default(), name.clone());
                let meta_name = if sub.is_empty() { file } else { format!("{sub}\\{file}") };
                let mut info = Map::new();
                info.insert("title".into(), json!(tx(&b["title"]).chars().take(200).collect::<String>()));
                info.insert("modelId".into(), json!(id));
                info.insert("source".into(), json!("modelscope"));
                info.insert("url".into(), json!(format!("https://modelscope.cn/models/{id}")));
                info.insert("base_model".into(), json!(mh::meta_base(fam.as_deref()).unwrap_or(base.as_str())));
                let triggers: Vec<Value> = b["triggers"].as_array().into_iter().flatten().filter_map(Value::as_str).take(20).map(|s| json!(s.chars().take(200).collect::<String>())).collect();
                info.insert("triggers".into(), json!(triggers));
                Box::new(move |_res: &Value| {
                    let mut meta = db.lora_meta().ok().and_then(|m| m.get(&meta_name).and_then(Value::as_object).cloned()).unwrap_or_default();
                    meta.extend(info);
                    meta.entry("strength").or_insert(json!(0.8));
                    let _ = db.set_lora_meta(&meta_name, &Value::Object(meta));
                }) as super::api_models::OnPlaced
            });
            let k = super::api_models::Known { url, name, size: f["size"].as_u64(), sha256: sha, kind: class, sub, bearer: None, cookie };
            ok(json!({"ok": true, "via": "models", "id": super::api_models::download_known(&app, k, done)}))
        }
        _ => Err(bad("這個來源沒有模型檔可以下載")),
    }
}

// ---------------------------------------------------------------- 工作流
/// 下載回來的工作流檔：.json／.png／.webp 直接用；.zip 拿裡面第一個 .json
fn unpack_workflow(name: &str, bytes: Vec<u8>) -> Result<(String, Vec<u8>), String> {
    if !bytes.starts_with(b"PK") {
        return Ok((name.to_string(), bytes));
    }
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("壓縮檔打不開：{e}"))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        let n = f.name().map_err(|e| e.to_string())?.to_string();
        if n.to_lowercase().ends_with(".json") && !n.starts_with("__MACOSX") {
            let mut out = vec![];
            std::io::Read::read_to_end(&mut std::io::Read::take(&mut f, 64 * 1024 * 1024), &mut out).map_err(|e| e.to_string())?;
            return Ok((super::basename(&n.replace('\\', "/")), out));
        }
    }
    Err("壓縮檔裡沒有工作流的 .json".into())
}

fn rh_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 30 && id.chars().all(|c| c.is_ascii_digit())
}

/// RunningHub 工作流的節點圖（API 格式，getJsonApiFormat 的 data.prompt）：要 RH 金鑰
async fn rh_prompt(app: &Shared, id: &str) -> Result<Vec<u8>, String> {
    if !rh_id_ok(id) {
        return Err("RunningHub 工作流編號只能是數字".into());
    }
    let c = super::api_runninghub::cfg(app);
    if c.key.is_empty() {
        return Err("要先填 RunningHub 的 API Key（H3 影片台「在哪跑 → RunningHub」），才讀得到工作流內容".into());
    }
    let url = format!("{}/api/openapi/getJsonApiFormat", crate::runninghub::base(&c.site));
    let r = http().post(&url).bearer_auth(&c.key).json(&json!({"apiKey": c.key, "workflowId": id})).timeout(Duration::from_secs(60)).send().await
        .map_err(|e| crate::net::explain(&e, &url))?;
    let v: Value = r.json().await.map_err(|_| "RunningHub 回應看不懂".to_string())?;
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(crate::runninghub::err_msg(code, v["msg"].as_str().unwrap_or("")));
    }
    match &v["data"]["prompt"] {
        Value::String(s) => Ok(s.clone().into_bytes()),
        Value::Object(_) => Ok(v["data"]["prompt"].to_string().into_bytes()),
        _ => Err("RunningHub 沒給工作流內容（這個工作流可能不公開）".into()),
    }
}

/// {source, id, version?} → {file_name, data_b64}：網頁拿去 POST /api/image/workflows 匯入（缺節點、指定節點的流程照舊）
pub async fn workflow(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let (source, id) = (tx(&b["source"]), tx(&b["id"]).trim().to_string());
    let (name, bytes) = match source.as_str() {
        "runninghub" => (format!("runninghub_{id}.json"), rh_prompt(&app, &id).await.map_err(bad)?),
        "civitai" => {
            let model_id: i64 = id.parse().map_err(|_| bad("C 站模型編號要是數字"))?;
            let m = app.civitai.get_json(&format!("{}/models/{model_id}", crate::civitai::API), &[]).await.map_err(bad)?;
            let vs = m["modelVersions"].as_array().ok_or_else(|| bad("這個工作流沒有版本"))?;
            let v = b["version"].as_i64().and_then(|want| vs.iter().find(|v| v["id"].as_i64() == Some(want))).or(vs.first()).ok_or_else(|| bad("這個工作流沒有版本"))?;
            let files = v["files"].as_array().ok_or_else(|| bad("這個版本沒有檔案"))?;
            let f = files.iter().find(|f| crate::py::truthy(f.get("primary"))).or(files.first()).ok_or_else(|| bad("這個版本沒有檔案"))?;
            let url = f["downloadUrl"].as_str().filter(|u| civitai_url_ok(u)).ok_or_else(|| bad("C 站的下載網址不對"))?.to_string();
            let key = crate::settings::civitai_key(&crate::settings::load(&app.paths.settings));
            let mut rb = http().get(&url);
            if !key.is_empty() {
                rb = rb.bearer_auth(key);
            }
            let r = rb.timeout(Duration::from_secs(120)).send().await.map_err(|e| bad(crate::net::explain(&e, &url)))?;
            if r.status().as_u16() == 401 || r.status().as_u16() == 403 {
                return Err(bad("C 站要登入才能下載這個工作流：到設定頁填 Civitai API key"));
            }
            let r = r.error_for_status().map_err(|e| bad(crate::net::explain(&e, &url)))?;
            let bytes = r.bytes().await.map_err(|e| bad(crate::net::explain(&e, &url)))?.to_vec();
            if bytes.len() > 64 * 1024 * 1024 {
                return Err(bad("工作流檔太大（超過 64MB）"));
            }
            let n = f["name"].as_str().map(super::basename).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("civitai_{model_id}.json"));
            blocking(move || unpack_workflow(&n, bytes)).await.map_err(bad)?
        }
        _ => return Err(bad("這個來源沒有工作流")),
    };
    ok(json!({"file_name": name, "data_b64": base64::engine::general_purpose::STANDARD.encode(bytes)}))
}

/// 節點圖用到、這台電腦沒有的模型檔：[{name, field, kind}]（匯入工作流後列出來，網頁每個給一顆「到下載中心找」）。
/// 只看 installed() 掃得到的那幾類（LoRA、主模型、VAE、文字編碼器），放大模型、ControlNet 不在裡面，列了會一直說缺
pub(super) fn missing_models(app: &Shared, graph: &Value) -> Vec<Value> {
    const FIELDS: &[(&str, &str)] = &[
        ("lora_name", "lora"), ("unet_name", "model"), ("ckpt_name", "model"), ("vae_name", "vae"),
        ("clip_name", "te"), ("clip_name1", "te"), ("clip_name2", "te"), ("clip_name3", "te"),
    ];
    let have = installed(app);
    let mut out: Vec<Value> = vec![];
    for n in graph.as_object().into_iter().flat_map(|m| m.values()) {
        for (k, kind) in FIELDS {
            let Some(v) = n["inputs"][*k].as_str() else { continue };
            let name = super::basename(&v.replace('\\', "/"));
            if name.is_empty() || !crate::model_dirs::is_model_file(Path::new(&name)) || have.contains(&name.to_lowercase()) || out.iter().any(|o| o["name"] == name.as_str()) {
                continue;
            }
            out.push(json!({"name": name, "field": k, "kind": kind}));
        }
    }
    out
}

// ---------------------------------------------------------------- 在 RunningHub 跑
fn hub_emit(app: &Shared, task: &str, state: &str, msg: impl Into<String>, extra: Value) {
    let mut v = json!({"type": "hubrun", "task": task, "state": state, "msg": msg.into()});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    app.hub.send(v);
}

/// 看懂 RunningHub 的工作流：用匯入工作流同一套找法（image_wf::analyze），模式照 wf_import::guess_mode。局部重繪要畫遮罩，不收
fn rh_analysis(graph: &Value) -> Result<crate::image_wf::Analysis, String> {
    let mode = crate::wf_import::guess_mode(graph).map_or("txt2img", |(m, _)| m);
    if mode == "inpaint" {
        return Err("這是局部重繪的工作流（要畫遮罩）".into());
    }
    crate::image_wf::analyze(graph, mode)
}

/// 使用者寫的提示詞、輸入圖、新種子放進節點圖（提示詞空的＝照工作流原本的）
fn rh_apply(graph: &mut Value, a: &crate::image_wf::Analysis, prompt: &str, image: Option<&str>, seed: i64) {
    let Some(m) = graph.as_object_mut() else { return };
    if !prompt.is_empty() {
        for (id, k) in &a.prompt {
            if let Some(v) = m.get_mut(id).and_then(|n| n["inputs"].get_mut(k)) {
                let old = v.as_str().unwrap_or("").to_string();
                *v = json!(if a.placeholder { old.replace("{{prompt}}", prompt) } else { prompt.to_string() });
            }
        }
    }
    if let (Some(id), Some(img)) = (&a.image, image) {
        if let Some(n) = m.get_mut(id).filter(|n| n["inputs"].is_object()) {
            n["inputs"]["image"] = json!(img);
        }
    }
    for (k, (id, f)) in a.seeds.iter().enumerate() {
        if let Some(n) = m.get_mut(id).filter(|n| n["inputs"].is_object()) {
            n["inputs"][f.as_str()] = json!(seed + k as i64);
        }
    }
}

/// 下載中心「在 RunningHub 跑」（照秒扣 RH 幣，MCP／助手的危險名單有列）：{id, name?, prompt?, image?, dry?}
/// 讀那個工作流的節點圖 → 找提示詞格、輸入圖 → 換成這邊寫的、種子換新 → 整份送上 RunningHub（workflowId 帶它自己的編號）。
/// 生圖站看不懂的工作流只能「照原樣跑」（不寫提示詞、不給圖）。image＝/api/upload 回的檔名。成品跟 H3 一樣進圖庫（AnimaWeb/cloud）。
/// dry＝只回看懂了什麼、機器、每秒扣幾個幣（網頁拿來問使用者要不要送），不送出。
/// 進度：ws {"type": "hubrun", task, state: queued|running|done|error|stopped, msg, ids?}；停止：/api/hub/rh_stop。
/// 停止信號借放在 app.h3（雲端閒置自動關機會把它算成忙，最多晚一個任務的時間關機）
pub async fn rh_run(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let id = tx(&b["id"]).trim().to_string();
    let bytes = rh_prompt(&app, &id).await.map_err(bad)?;
    let mut graph: Value = serde_json::from_slice(&bytes).ok().filter(Value::is_object).ok_or_else(|| bad("RunningHub 給的工作流看不懂"))?;
    let an = rh_analysis(&graph);
    let c = super::api_runninghub::cfg(&app);
    let rate = crate::runninghub::COINS_PER_SEC.iter().find(|(k, _)| *k == c.instance).map_or(0.0, |(_, r)| *r);
    let prompt = tx(&b["prompt"]).trim().to_string();
    let image = Some(super::basename(&tx(&b["image"]))).filter(|s| !s.is_empty());
    if crate::py::truthy(b.get("dry")) {
        let (why, summary) = match &an {
            Ok(a) => (Value::Null, a.summary(&graph)),
            Err(e) => (json!(e), Value::Null),
        };
        return ok(json!({"understood": an.is_ok(), "why": why, "summary": summary,
            "needs_image": an.as_ref().is_ok_and(|a| a.image.is_some()), "has_prompt": an.as_ref().is_ok_and(|a| !a.prompt.is_empty()),
            "instance": c.instance, "coins_per_sec": rate, "site": c.site}));
    }
    match &an {
        Ok(a) if a.image.is_some() && image.is_none() => return Err(bad("這個工作流要一張輸入圖：先放一張圖再送")),
        Ok(a) if a.prompt.is_empty() && !prompt.is_empty() => return Err(bad("這個工作流沒有寫提示詞的地方：清掉提示詞再送")),
        Err(e) if !prompt.is_empty() || image.is_some() => {
            return Err(bad(format!("生圖站看不懂這個工作流（{e}），只能照原樣跑：清掉提示詞和輸入圖再送，或到 RunningHub 網頁上跑")));
        }
        _ => {}
    }
    if let Some(img) = &image {
        if !app.paths.input.join(img).is_file() {
            return Err(bad(format!("輸入圖 {img} 不見了：重新放一次")));
        }
    }
    let seed = super::api_director::random_seed() & 0xFFFF_FFFF_FFFF;
    if let Ok(a) = &an {
        rh_apply(&mut graph, a, &prompt, image.as_deref(), seed);
    }
    let nodes = graph.as_object().cloned().unwrap_or_default();
    let name: String = tx(&b["name"]).chars().take(120).collect();
    let params = json!({"mode": "runninghub", "prompt": prompt, "seed": seed, "runninghub": true, "rh_workflow": id, "rh_name": name, "image": image});
    let info = json!({"prompt": prompt, "seed": seed, "source": "runninghub", "workflow": id});
    let task = format!("hub-{:x}", super::api_director::random_seed());
    let token = tokio_util::sync::CancellationToken::new();
    app.h3.put(&task, token.clone());
    hub_emit(&app, &task, "queued", "準備送到 RunningHub…", json!({"id": id}));
    let (a2, t) = (app.clone(), task.clone());
    crate::diagnostics::spawn(async move {
        let (a3, t3) = (a2.clone(), t.clone());
        let status = move |m: String| hub_emit(&a3, &t3, "running", m, json!({}));
        let r = super::api_runninghub::run_graph_in(&a2, nodes, &params, &info, &token, &status, Some(&id)).await;
        a2.h3.take(&t);
        match r {
            Ok((_, ids)) => hub_emit(&a2, &t, "done", "完成：成品在圖庫", json!({"ids": ids})),
            Err(e) if token.is_cancelled() => hub_emit(&a2, &t, "stopped", e, json!({})),
            Err(e) => hub_emit(&a2, &t, "error", e, json!({})),
        }
    });
    ok(json!({"ok": true, "task": task, "instance": c.instance, "coins_per_sec": rate}))
}

/// {task}：停掉「在 RunningHub 跑」（RunningHub 那邊也會取消；背景工作自己收尾、除名）
pub async fn rh_stop(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    let task = tx(&b["task"]);
    match app.h3.token(&task).filter(|_| task.starts_with("hub-")) {
        Some(t) => {
            t.cancel();
            ok(json!({"ok": true}))
        }
        None => Err(bad("這個工作已經結束了")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_and_installed_marks() {
        assert!(civitai_url_ok("https://civitai.com/api/download/models/123?type=VAE"));
        assert!(!civitai_url_ok("https://evil.com/api/download/models/1") && !civitai_url_ok("http://civitai.com/api/download/models/1"));
        let have: HashSet<String> = ["cute.safetensors".to_string()].into_iter().collect();
        let mut items = vec![json!({"versions": [{"files": [{"name": "Cute.safetensors", "primary": true}]}, {"files": [{"name": "x.safetensors", "primary": true}]}]})];
        mark_installed(&mut items, &have);
        assert_eq!((items[0]["installed"].as_bool(), items[0]["versions"][1]["installed"].as_bool()), (Some(true), Some(false)));
    }

    #[test]
    fn workflow_zip_and_plain() {
        assert_eq!(unpack_workflow("a.json", b"{}".to_vec()).unwrap(), ("a.json".to_string(), b"{}".to_vec()));
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default();
            w.start_file("readme.txt", opt).unwrap();
            std::io::Write::write_all(&mut w, b"hi").unwrap();
            w.start_file("sub/flow.json", opt).unwrap();
            std::io::Write::write_all(&mut w, b"{\"1\":{}}").unwrap();
            w.finish().unwrap();
        }
        let (n, b) = unpack_workflow("x.zip", buf.into_inner()).unwrap();
        assert_eq!((n.as_str(), b.as_slice()), ("flow.json", b"{\"1\":{}}".as_slice()));
    }

    #[test]
    fn rh_graph_gets_prompt_image_seed() {
        // RunningHub 常見的樣子：Load Image → VAE Encode → KSampler，正負面兩格文字
        let mut g = json!({
            "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "x.safetensors"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["1", 1]}},
            "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["1", 1]}},
            "4": {"class_type": "LoadImage", "inputs": {"image": "author_sample.png"}},
            "5": {"class_type": "VAEEncode", "inputs": {"pixels": ["4", 0], "vae": ["1", 2]}},
            "6": {"class_type": "KSampler", "inputs": {"model": ["1", 0], "positive": ["2", 0], "negative": ["3", 0], "latent_image": ["5", 0],
                  "seed": 1, "steps": 20, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 0.6}},
            "7": {"class_type": "VAEDecode", "inputs": {"samples": ["6", 0], "vae": ["1", 2]}},
            "8": {"class_type": "SaveImage", "inputs": {"images": ["7", 0], "filename_prefix": "rh"}}
        });
        let a = rh_analysis(&g).unwrap();
        assert_eq!(a.image.as_deref(), Some("4"));
        rh_apply(&mut g, &a, "a dog", Some("mine.png"), 42);
        assert_eq!((g["2"]["inputs"]["text"].as_str(), g["3"]["inputs"]["text"].as_str()), (Some("a dog"), Some("blurry")));
        assert_eq!((g["4"]["inputs"]["image"].as_str(), g["6"]["inputs"]["seed"].as_i64()), (Some("mine.png"), Some(42)));
        // 提示詞空的：照工作流原本的
        let mut g2 = g.clone();
        rh_apply(&mut g2, &a, "", None, 7);
        assert_eq!((g2["2"]["inputs"]["text"].as_str(), g2["4"]["inputs"]["image"].as_str()), (Some("a dog"), Some("mine.png")));
        // 局部重繪不收
        let mut inp = g.clone();
        inp["9"] = json!({"class_type": "SetLatentNoiseMask", "inputs": {"samples": ["5", 0], "mask": ["4", 1]}});
        assert!(rh_analysis(&inp).unwrap_err().contains("局部重繪"));
        assert!(rh_id_ok("2004520621293486081") && !rh_id_ok("") && !rh_id_ok("12a") && !rh_id_ok(&"9".repeat(31)));
    }

    #[test]
    fn key_tag_differs() {
        assert_ne!(key_tag("a"), key_tag("b"));
        assert_eq!(key_tag("a"), key_tag("a"));
    }
}

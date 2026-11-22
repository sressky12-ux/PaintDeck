//! 生成頁「衣姿」抽屜的 API（2026-11-21，設計：docs\superpowers\specs\2026-11-21-posewear-drawer-design.md；純函式在 posewear.rs）。
//! 原站（ギア 的 AI 服裝／姿勢圖庫）規矩：一張一張當生成參考可以；禁止大量下載（含工具自動抓）與轉載。所以：
//! - 目錄（首頁內嵌的 const D）一次抓一頁、存快取 7 天；
//! - 原圖只在使用者拖放／按按鈕時抓那一張（use），抓過存本機，不預抓、沒有「全部下載」；
//! - 縮圖由網頁直接連原站，不經過這裡。
use super::{bad, blocking, ok, ApiResult, Body, Shared};
use crate::posewear::{self, Text};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 目錄快取多久重抓（秒）：7 天
pub const INDEX_TTL: u64 = 7 * 24 * 3600;
/// 一張原圖最大 30MB
const MAX_BYTES: usize = 30 * 1024 * 1024;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn fresh(fetched: u64, now: u64) -> bool {
    now.saturating_sub(fetched) < INDEX_TTL
}

fn dir(data: &Path) -> PathBuf {
    data.join("posewear")
}

fn index_lib(lib: &str) -> Option<&'static str> {
    match lib {
        "anime" => Some("anime"),
        "costume" => Some("costume"),
        "pose" | "depth" => Some("pose"), // 深度圖跟線稿同一套 id，共用姿勢目錄
        _ => None,
    }
}

/// 快取檔 {fetched, d}
pub fn read_cache(path: &Path) -> Option<(Vec<Value>, u64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((v.get("d")?.as_array()?.clone(), v.get("fetched")?.as_u64()?))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("建不了 {}：{e}", p.display()))?;
    }
    let tmp = path.with_extension(format!("part{:06x}", rand::random::<u32>() & 0xff_ffff));
    std::fs::write(&tmp, bytes).map_err(|e| format!("寫不進 {}：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("寫不進 {}：{e}", path.display())
    })
}

/// 抓到的結果和舊快取合在一起：抓成功用新的；失敗有舊快取就回舊的（stale）；都沒有回錯誤
pub fn resolve(
    cache: Option<(Vec<Value>, u64)>,
    fetched: Result<Vec<Value>, String>,
    now: u64,
) -> Result<(Vec<Value>, u64, bool, Option<String>), String> {
    match (fetched, cache) {
        (Ok(d), _) => Ok((d, now, false, None)),
        (Err(e), Some((d, t))) => Ok((d, t, true, Some(e))),
        (Err(e), None) => Err(format!("連不上原站，服裝和姿勢的目錄、縮圖都需要網路（{e}）")),
    }
}

fn client(total: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .proxy(crate::net::proxy()) // 梯子（系統代理、PAC、環境變數）：net.rs
        .user_agent("Mozilla/5.0 (PaintDeck posewear drawer)")
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(total))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch_bytes(url: &str, max: usize) -> Result<Vec<u8>, String> {
    fetch_with(client(60)?, url, max).await
}

async fn fetch_with(c: reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut r = c.get(url).send().await.map_err(|e| format!("連不上原站：{}", e.without_url()))?;
    if !r.status().is_success() {
        return Err(format!("原站回 HTTP {}", r.status().as_u16()));
    }
    let mut buf = vec![];
    while let Some(c) = r.chunk().await.map_err(|e| format!("下載中斷：{}", e.without_url()))? {
        buf.extend_from_slice(&c);
        if buf.len() > max {
            return Err(format!("檔案超過 {}MB，不抓", max / 1024 / 1024));
        }
    }
    Ok(buf)
}

/// 讀目錄：快取 7 天內直接用；過期或 refresh 才抓原站首頁（一次一頁）
async fn load_d(data: &Path, lib: &'static str, refresh: bool) -> Result<(Vec<Value>, u64, bool, Option<String>), String> {
    let path = dir(data).join(format!("index_{lib}.json"));
    let p2 = path.clone();
    let cache = blocking(move || read_cache(&p2)).await;
    if let Some((d, t)) = &cache {
        if !refresh && fresh(*t, now()) {
            return Ok((d.clone(), *t, false, None));
        }
    }
    let base = posewear::base(lib).ok_or("不認得的素材庫")?;
    let got = match fetch_bytes(base, 8 * 1024 * 1024).await {
        Ok(b) => posewear::parse_inline_d(&String::from_utf8_lossy(&b)),
        Err(e) => Err(e),
    };
    if let Ok(d) = &got {
        let bytes = serde_json::to_vec(&json!({"fetched": now(), "d": d})).unwrap_or_default();
        let p3 = path.clone();
        if let Err(e) = blocking(move || write_atomic(&p3, &bytes)).await {
            eprintln!("衣姿：目錄快取寫不進去：{e}");
        }
    }
    resolve(cache, got, now())
}

/// GET ?lib=anime|costume|pose[&refresh=1] → {lib, fetched, stale, error?, items}
pub async fn index(State(app): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let lib = q.get("lib").map(String::as_str).unwrap_or("");
    let il = index_lib(lib).filter(|l| *l == lib).ok_or_else(|| bad("lib 只能是 anime、costume、pose"))?;
    let refresh = q.get("refresh").is_some_and(|v| v == "1" || v == "true");
    let (d, fetched, stale, error) = load_d(&app.paths.data, il, refresh).await.map_err(bad)?;
    let res = app.paths.res.clone();
    let items = blocking(move || -> Result<Vec<Value>, String> {
        let t = Text::load(&res)?;
        Ok(if il == "pose" { posewear::poses(&t, &d) } else { posewear::costumes(&t, il, &d) })
    })
    .await
    .map_err(bad)?;
    ok(json!({"lib": lib, "fetched": fetched, "stale": stale, "error": error, "items": items}))
}

/// 存進 ComfyUI input：固定檔名，已經有就直接回（同一張拖幾次都只有一個檔）
pub fn to_input(raw: Option<&[u8]>, input: &Path, name: &str) -> Result<Value, String> {
    let path = input.join(name);
    if let Ok((w, h)) = image::image_dimensions(&path) {
        return Ok(json!({"name": name, "width": w, "height": h}));
    }
    let raw = raw.ok_or("沒有圖")?;
    let im = image::load_from_memory(raw).map_err(|e| format!("原站給的不是可讀取的圖片：{e}"))?;
    let mut png = std::io::Cursor::new(vec![]);
    im.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    write_atomic(&path, png.get_ref())?;
    Ok(json!({"name": name, "width": im.width(), "height": im.height()}))
}

// ---------------------------------------------------------------- 衣姿整頁：官網資料檔（2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-page-design.md）

/// 網址裡的名稱 → 固定名稱（可帶 .json）；不認得的一律不收
fn spa_name(s: &str) -> Option<&'static str> {
    let s = s.strip_suffix(".json").unwrap_or(s);
    posewear::SPA_NAMES.iter().copied().find(|n| *n == s)
}

/// 跟 resolve 一樣：抓成功用新的；失敗有舊快取回舊的（stale）；都沒有回白話錯誤
pub fn resolve_spa(cache: Option<(Value, u64)>, fetched: Result<Value, String>, now: u64) -> Result<(Value, u64, bool), String> {
    match (fetched, cache) {
        (Ok(v), _) => Ok((v, now, false)),
        (Err(e), Some((v, t))) => {
            eprintln!("衣姿整頁：抓不到官網，先用舊的快取（{e}）");
            Ok((v, t, true))
        }
        (Err(e), None) => Err(format!("連不上 posewear 官網，衣姿頁的資料要網路才抓得到（{e}）")),
    }
}

fn read_spa_cache(path: &Path, key: &str) -> Option<(Value, u64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((v.get(key)?.clone(), v.get("fetched")?.as_u64()?))
}

async fn get_text(url: &str, max: usize) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&fetch_bytes(url, max).await?).into_owned())
}

/// 官網首頁 → app.js → 12 個資料檔的網址；存快取 7 天
async fn spa_map(data: &Path, refresh: bool) -> Result<HashMap<String, String>, String> {
    let path = dir(data).join("spa").join("_assets.json");
    let p2 = path.clone();
    let cache = blocking(move || read_spa_cache(&p2, "map")).await;
    if let Some((m, t)) = &cache {
        if !refresh && fresh(*t, now()) {
            return serde_json::from_value(m.clone()).map_err(|e| e.to_string());
        }
    }
    let got = async {
        let html = get_text(posewear::SPA_SITE, 2 * 1024 * 1024).await?;
        let js = posewear::spa_app_js(&html).ok_or("官網首頁找不到 app.js，可能改版了")?;
        let map = posewear::spa_assets(&get_text(&format!("{}{js}", posewear::SPA_SITE), 4 * 1024 * 1024).await?);
        if map.is_empty() {
            return Err("官網 app.js 裡找不到資料檔，可能改版了".to_string());
        }
        Ok(json!(map))
    }
    .await;
    if let Ok(m) = &got {
        let bytes = serde_json::to_vec(&json!({"fetched": now(), "map": m})).unwrap_or_default();
        let p3 = path.clone();
        if let Err(e) = blocking(move || write_atomic(&p3, &bytes)).await {
            eprintln!("衣姿整頁：資料檔清單寫不進快取：{e}");
        }
    }
    let (m, ..) = resolve_spa(cache, got, now())?;
    serde_json::from_value(m).map_err(|e| e.to_string())
}

/// 讀一個官網資料檔（已改寫 ギア 四庫的圖片網址）；快取 7 天
async fn load_spa(data: &Path, name: &'static str, refresh: bool) -> Result<Value, String> {
    let path = dir(data).join("spa").join(format!("{name}.json"));
    let p2 = path.clone();
    let cache = blocking(move || read_spa_cache(&p2, "v")).await;
    if let Some((v, t)) = &cache {
        if !refresh && fresh(*t, now()) {
            return Ok(v.clone());
        }
    }
    let got = async {
        let map = spa_map(data, refresh).await?;
        let rel = map.get(name).ok_or_else(|| format!("官網沒有「{name}」這個資料檔，可能改版了"))?;
        let mut v: Value = serde_json::from_str(&get_text(&format!("{}{rel}", posewear::SPA_SITE), 16 * 1024 * 1024).await?)
            .map_err(|e| format!("官網的資料檔讀不懂：{e}"))?;
        let cat_lib = match name {
            "poses" => Some("pose"),
            "costumes" => Some("costume"),
            "anime" => Some("anime"),
            _ => None,
        };
        let cat = match cat_lib {
            Some(l) => match load_d(data, l, false).await {
                Ok((d, ..)) => Some(d),
                Err(e) => {
                    eprintln!("衣姿整頁：{l} 原站目錄抓不到，圖片先照官網原樣（{e}）");
                    None
                }
            },
            None => None,
        };
        posewear::rewrite_spa(name, &mut v, cat.as_deref());
        Ok(v)
    }
    .await;
    if let Ok(v) = &got {
        let bytes = serde_json::to_vec(&json!({"fetched": now(), "v": v})).unwrap_or_default();
        let p3 = path.clone();
        if let Err(e) = blocking(move || write_atomic(&p3, &bytes)).await {
            eprintln!("衣姿整頁：{name} 寫不進快取：{e}");
        }
    }
    resolve_spa(cache, got, now()).map(|(v, ..)| v)
}

/// GET /api/posewear/spa/{name}[?refresh=1] → 官網資料檔本體（陣列或物件，不包外層）
pub async fn spa(State(app): State<Shared>, AxPath(name): AxPath<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let name = spa_name(&name).ok_or_else(|| bad("不認得的資料檔"))?;
    let refresh = q.get("refresh").is_some_and(|v| v == "1" || v == "true");
    let v = load_spa(&app.paths.data, name, refresh).await.map_err(bad)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response())
}

/// use 的 {url} 形式：網址要過白名單，而且要出現在已快取的某個官網資料檔裡（只讀快取，不為了檢查去抓網路）
fn url_in_cache(data: &Path, url: &str) -> bool {
    posewear::SPA_NAMES
        .iter()
        .filter_map(|n| read_spa_cache(&dir(data).join("spa").join(format!("{n}.json")), "v"))
        .any(|(v, _)| posewear::spa_has_url(&v, url))
}

async fn use_url(app: &Shared, url: String) -> ApiResult {
    if !posewear::url_host_ok(&url) {
        return Err(bad("這個網址不是衣姿頁的素材，不抓"));
    }
    let (data, u2) = (app.paths.data.clone(), url.clone());
    if !blocking(move || url_in_cache(&data, &u2)).await {
        return Err(bad("衣姿頁的資料裡沒有這張圖，不抓"));
    }
    let name = posewear::url_input_name(&url);
    let input = app.paths.input.clone();
    let (n2, in2) = (name.clone(), input.clone());
    if let Ok(r) = blocking(move || to_input(None, &in2, &n2)).await {
        return ok(r);
    }
    let ext = Path::new(url.rsplit('/').next().unwrap_or("")).extension().and_then(|e| e.to_str()).filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("img").to_string();
    let cache = dir(&app.paths.data).join("img").join("url").join(format!("{}.{ext}", name.trim_start_matches("pw_url_").trim_end_matches(".png")));
    let c2 = cache.clone();
    let raw = match blocking(move || std::fs::read(&c2).ok()).await {
        Some(r) => r,
        None => {
            eprintln!("衣姿整頁：抓一張圖 {url}");
            let r = fetch_bytes(&url, MAX_BYTES).await.map_err(bad)?;
            let (c3, r2) = (cache.clone(), r.clone());
            if let Err(e) = blocking(move || write_atomic(&c3, &r2)).await {
                eprintln!("衣姿整頁：圖片快取寫不進去：{e}");
            }
            r
        }
    };
    let r = blocking(move || to_input(Some(&raw), &input, &name)).await.map_err(bad)?;
    ok(r)
}

/// POST {lib: anime|costume|pose|depth, id} → {name, width, height}（跟 /api/upload 一樣）。
/// id 一定要在目錄裡查得到，網址只用固定 base＋目錄給的相對路徑。
/// 衣姿整頁另外可以送 {url}（ギア 以外的素材：表情、人像、場景…），規則見 use_url。
pub async fn use_item(State(app): State<Shared>, Body(b): Body) -> ApiResult {
    if let Some(url) = b.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
        return use_url(&app, url.to_string()).await;
    }
    let lib = b.get("lib").and_then(Value::as_str).unwrap_or("").to_string();
    let id = b.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let il = index_lib(&lib).ok_or_else(|| bad("lib 只能是 anime、costume、pose、depth"))?;
    let name = posewear::input_name(&lib, &id);
    let input = app.paths.input.clone();
    // 已經在 input 了就不用查目錄、不用抓
    let (n2, in2) = (name.clone(), input.clone());
    if let Ok(r) = blocking(move || to_input(None, &in2, &n2)).await {
        return ok(r);
    }
    let (d, ..) = load_d(&app.paths.data, il, false).await.map_err(bad)?;
    let item = posewear::find_item(&d, &id).ok_or_else(|| bad(format!("目錄裡沒有 {id}")))?;
    let rel = posewear::image_rel(&lib, item).ok_or_else(|| bad("這張圖的路徑不對，不抓"))?;
    let ext = Path::new(&rel).extension().and_then(|e| e.to_str()).unwrap_or("img").to_string();
    let cache = dir(&app.paths.data).join("img").join(&lib).join(format!("{}.{ext}", name.trim_start_matches("pw_").trim_end_matches(".png")));
    let c2 = cache.clone();
    let raw = match blocking(move || std::fs::read(&c2).ok()).await {
        Some(r) => r,
        None => {
            let url = format!("{}{rel}", posewear::base(&lib).unwrap_or_default());
            eprintln!("衣姿：抓一張原圖 {url}");
            let r = fetch_bytes(&url, MAX_BYTES).await.map_err(bad)?;
            let (c3, r2) = (cache.clone(), r.clone());
            if let Err(e) = blocking(move || write_atomic(&c3, &r2)).await {
                eprintln!("衣姿：原圖快取寫不進去：{e}");
            }
            r
        }
    };
    let r = blocking(move || to_input(Some(&raw), &input, &name)).await.map_err(bad)?;
    ok(r)
}

// ---------------------------------------------------------------- 衣姿頁前端：執行時跟官網同步（2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md）
// 使用者選「執行時直接同步」、不另開 port（風險由使用者承擔）。留下的防護：只抓 https://posewear.pages.dev、
// 轉址到別的主機不跟、單檔 2MB；規則沒命中整套退回安裝檔內建版（ui\posewear\，loader.js 決定載哪套）。
// 快取：<資料>\posewear\front\（5 個套好規則的檔＋_front.json）；7 天重抓。

/// 同步失敗（離線）後多久才再試（秒）：10 分鐘，免得每次開衣姿頁都卡在連線逾時
pub const FRONT_RETRY: u64 = 600;

/// `_front.json`：最後一次成功的那一套（fetched、hash）＋最後一次檢查的結果
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug, PartialEq)]
pub struct FrontMeta {
    #[serde(default)]
    pub fetched: u64,
    #[serde(default)]
    pub checked: u64,
    /// 檔名 → 官網上的路徑（含雜湊），顯示「目前跑的是哪一版」用
    #[serde(default)]
    pub hash: HashMap<String, String>,
    /// 官網改版、規則沒命中：規則名稱（有值就用內建版）
    #[serde(default)]
    pub failed_rule: Option<String>,
    /// 最後一次抓不到官網的原因
    #[serde(default)]
    pub error: Option<String>,
}

/// 一次同步的結果
pub enum FrontGot {
    Ok(HashMap<String, String>),
    Rule(String),
    Net(String),
}

/// 現在該用哪一套：live＝7 天內同步成功的官網版；cache＝官網版但過期了（抓不到新的）；builtin＝內建版
pub fn front_source(m: Option<&FrontMeta>, files_ok: bool, now: u64) -> &'static str {
    match m {
        Some(m) if m.failed_rule.is_none() && !m.hash.is_empty() && files_ok => {
            if fresh(m.fetched, now) {
                "live"
            } else {
                "cache"
            }
        }
        _ => "builtin",
    }
}

/// 這次要不要去官網抓
pub fn front_should_fetch(m: Option<&FrontMeta>, files_ok: bool, refresh: bool, now: u64) -> bool {
    if refresh {
        return true;
    }
    let Some(m) = m else { return true };
    if m.failed_rule.is_some() {
        return !fresh(m.checked, now);
    }
    if !m.hash.is_empty() && files_ok && fresh(m.fetched, now) {
        return false;
    }
    now.saturating_sub(m.checked) >= FRONT_RETRY
}

/// 把這次的結果記進 meta：失敗時留著上一套（離線就用舊快取），規則沒命中記下名稱
pub fn front_after(m: Option<FrontMeta>, got: FrontGot, now: u64) -> FrontMeta {
    let mut m = m.unwrap_or_default();
    m.checked = now;
    match got {
        FrontGot::Ok(hash) => {
            m.fetched = now;
            m.hash = hash;
            m.failed_rule = None;
            m.error = None;
        }
        FrontGot::Rule(r) => {
            m.failed_rule = Some(r);
            m.error = None;
        }
        FrontGot::Net(e) => m.error = Some(e),
    }
    m
}

/// ui\posewear\VENDOR.txt → 內建版的上游 commit
pub fn vendor_commit(text: &str) -> Option<String> {
    Regex::new(r"commit ([0-9a-f]{7,40})").ok()?.captures(text).map(|c| c[1].to_string())
}

fn front_dir(data: &Path) -> PathBuf {
    dir(data).join("front")
}

fn front_files_ok(d: &Path) -> bool {
    posewear::FRONT_FILES.iter().all(|n| d.join(n).is_file())
}

fn read_front_meta(d: &Path) -> Option<FrontMeta> {
    serde_json::from_str(&std::fs::read_to_string(d.join("_front.json")).ok()?).ok()
}

/// 只抓官網：HTTPS、轉址也要在 posewear.pages.dev（最多 3 次）
fn front_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .proxy(crate::net::proxy())
        .user_agent("Mozilla/5.0 (PaintDeck posewear page)")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|a| {
            if a.previous().len() < 3 && posewear::front_url_ok(a.url().as_str()) {
                a.follow()
            } else {
                a.error("官網轉址到別的主機，不跟")
            }
        }))
        .build()
        .map_err(|e| e.to_string())
}

async fn front_get(url: &str) -> Result<String, String> {
    if !posewear::front_url_ok(url) {
        return Err(format!("不是官網的網址：{url}"));
    }
    let b = fetch_with(front_client()?, url, posewear::FRONT_MAX).await?;
    String::from_utf8(b).map_err(|_| "官網的檔不是 UTF-8".to_string())
}

/// 抓首頁和 5 個檔、套規則、寫進快取（先寫檔，最後才由呼叫的人寫 meta）
async fn front_fetch(fdir: &Path) -> FrontGot {
    let html = match front_get(posewear::SPA_SITE).await {
        Ok(h) => h,
        Err(e) => return FrontGot::Net(e),
    };
    let assets = match posewear::front_assets(&html) {
        Ok(a) => a,
        Err(r) => return FrontGot::Rule(r),
    };
    let mut out = vec![];
    for n in posewear::FRONT_FILES {
        let text = match front_get(&format!("{}{}", posewear::SPA_SITE, assets[n])).await {
            Ok(t) => t,
            Err(e) => return FrontGot::Net(format!("{n}：{e}")),
        };
        match posewear::front_patch(n, &text) {
            Ok(t) => out.push((n, t)),
            Err(r) => return FrontGot::Rule(r),
        }
    }
    let d = fdir.to_path_buf();
    match blocking(move || out.iter().try_for_each(|(n, t)| write_atomic(&d.join(n), t.as_bytes()))).await {
        Ok(()) => FrontGot::Ok(assets),
        Err(e) => FrontGot::Net(e),
    }
}

/// 同一時間只做一次同步（衣姿頁會同時要 status 和 5 個檔）
static FRONT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 需要時跟官網同步，回傳狀態 {source, hash, fetched_at, checked_at, failed_rule, error, builtin}
async fn front_load(app: &Shared, refresh: bool) -> Value {
    let _g = FRONT_LOCK.lock().await;
    let fdir = front_dir(&app.paths.data);
    let d2 = fdir.clone();
    let (mut meta, files_ok) = blocking(move || (read_front_meta(&d2), front_files_ok(&d2))).await;
    if front_should_fetch(meta.as_ref(), files_ok, refresh, now()) {
        let got = front_fetch(&fdir).await;
        match &got {
            FrontGot::Rule(r) => eprintln!("衣姿頁：官網改版了（{r}），先用內建版"),
            FrontGot::Net(e) => eprintln!("衣姿頁：抓不到官網前端（{e}）"),
            FrontGot::Ok(_) => {}
        }
        let m = front_after(meta, got, now());
        let bytes = serde_json::to_vec(&m).unwrap_or_default();
        let d3 = fdir.clone();
        if let Err(e) = blocking(move || write_atomic(&d3.join("_front.json"), &bytes)).await {
            eprintln!("衣姿頁：前端同步狀態寫不進快取：{e}");
        }
        meta = Some(m);
    }
    let d4 = fdir.clone();
    let files_ok = blocking(move || front_files_ok(&d4)).await;
    let vf = app.paths.web.join("posewear").join("VENDOR.txt");
    let vendor = blocking(move || std::fs::read_to_string(vf).ok()).await;
    let m = meta.clone().unwrap_or_default();
    json!({
        "source": front_source(meta.as_ref(), files_ok, now()),
        "hash": m.hash, "fetched_at": m.fetched, "checked_at": m.checked,
        "failed_rule": m.failed_rule, "error": m.error,
        "builtin": vendor.as_deref().and_then(vendor_commit),
    })
}

/// GET /api/posewear/front/status → 目前衣姿頁該用哪一套（需要時順便同步）
pub async fn front_status(State(app): State<Shared>) -> ApiResult {
    let v = front_load(&app, false).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response())
}

/// GET /api/posewear/front/refresh → 強制重抓官網（衣姿頁角落「檢查官網更新」）
pub async fn front_refresh(State(app): State<Shared>) -> ApiResult {
    let v = front_load(&app, true).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response())
}

/// GET /api/posewear/front/file/{name} → 套好規則的官網前端檔；只收白名單裡的 5 個名稱，現在用內建版時也回 404
pub async fn front_file(State(app): State<Shared>, AxPath(name): AxPath<String>) -> ApiResult {
    let name = posewear::front_name(&name).ok_or_else(|| super::not_found("沒有這個檔"))?;
    let st = front_load(&app, false).await;
    if st["source"] == "builtin" {
        return Err(super::not_found("衣姿頁現在用內建版"));
    }
    let p = front_dir(&app.paths.data).join(name);
    let body = blocking(move || std::fs::read(p)).await.map_err(|_| super::not_found("快取裡沒有這個檔"))?;
    let ct = if name.ends_with(".css") { "text/css; charset=utf-8" } else { "text/javascript; charset=utf-8" };
    Ok(([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-store")], body).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pw_test_{name}_{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fresh_window() {
        assert!(fresh(1000, 1000 + INDEX_TTL - 1));
        assert!(!fresh(1000, 1000 + INDEX_TTL));
        assert!(fresh(5000, 4000)); // 時鐘倒退也算新的
    }

    #[test]
    fn stale_fallback_uses_cache() {
        let d = tmp("stale");
        let p = d.join("index_anime.json");
        write_atomic(&p, br#"{"fetched": 123, "d": [{"id": "SPR-0001"}]}"#).unwrap();
        let (items, t, stale, err) = resolve(read_cache(&p), Err("逾時".into()), 999_999).unwrap();
        assert_eq!((items.len(), t, stale, err.as_deref()), (1, 123, true, Some("逾時")));
        let (_, t, stale, err) = resolve(read_cache(&p), Ok(vec![]), 999_999).unwrap();
        assert_eq!((t, stale, err), (999_999, false, None));
        assert!(resolve(None, Err("逾時".into()), 1).unwrap_err().contains("需要網路"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn input_reused_when_exists() {
        let d = tmp("input");
        let mut png = std::io::Cursor::new(vec![]);
        image::RgbImage::new(3, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let r = to_input(Some(png.get_ref()), &d, "pw_depth_a.png").unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(3), Some(2)));
        // 第二次不給圖也拿得到（直接用已經存在的檔），資料夾裡只有一個檔
        let r2 = to_input(None, &d, "pw_depth_a.png").unwrap();
        assert_eq!(r2["name"], "pw_depth_a.png");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        assert!(to_input(None, &d, "pw_depth_b.png").is_err());
        assert!(to_input(Some(b"not an image"), &d, "pw_depth_c.png").unwrap_err().contains("不是可讀取的圖片"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn spa_resolve_stale() {
        let (v, t, stale) = resolve_spa(Some((json!([1]), 5)), Err("逾時".into()), 99).unwrap();
        assert_eq!((v, t, stale), (json!([1]), 5, true));
        let (v, t, stale) = resolve_spa(Some((json!([1]), 5)), Ok(json!([2])), 99).unwrap();
        assert_eq!((v, t, stale), (json!([2]), 99, false));
        assert!(resolve_spa(None, Err("逾時".into()), 1).unwrap_err().contains("posewear 官網"));
    }

    #[test]
    fn spa_name_only_known() {
        assert_eq!(spa_name("poses.json"), Some("poses"));
        assert_eq!(spa_name("hair"), Some("hair"));
        assert_eq!(spa_name("../x"), None);
        assert_eq!(spa_name("poses.json.json"), None);
    }

    #[test]
    fn use_url_rules() {
        let d = tmp("useurl");
        let p = dir(&d).join("spa").join("expressions.json");
        write_atomic(&p, br#"{"fetched": 1, "v": [{"angles": [{"t": "e/t/a.webp"}]}]}"#).unwrap();
        assert!(url_in_cache(&d, "https://pwimg.0vo.pro/e/t/a.webp"));
        assert!(!url_in_cache(&d, "https://pwimg.0vo.pro/e/t/b.webp"));
        assert!(!url_in_cache(&d, "https://pwimg.0vo.pro/e/t/../a.webp"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn index_lib_only_known() {
        assert_eq!(index_lib("depth"), Some("pose"));
        assert_eq!(index_lib("../x"), None);
        assert_eq!(index_lib("portrait"), None);
    }
    // ---------------------------------------------------------------- 前端同步（2026-11-22）
    fn meta(fetched: u64, checked: u64, rule: Option<&str>) -> FrontMeta {
        FrontMeta {
            fetched,
            checked,
            hash: HashMap::from([("app.js".into(), "assets/app.1.js".into())]),
            failed_rule: rule.map(str::to_string),
            error: None,
        }
    }

    #[test]
    fn front_source_states() {
        let now = 10 * INDEX_TTL;
        // 沒同步過 → 內建
        assert_eq!(front_source(None, false, now), "builtin");
        // 7 天內同步成功 → 官網版；過期（抓不到新的）→ 舊快取
        assert_eq!(front_source(Some(&meta(now - 5, now - 5, None)), true, now), "live");
        assert_eq!(front_source(Some(&meta(now - INDEX_TTL - 1, now, None)), true, now), "cache");
        // 規則沒命中 → 內建（就算以前有好的快取）
        assert_eq!(front_source(Some(&meta(now - 5, now, Some("app.js：圖片主機 AH"))), true, now), "builtin");
        // 快取檔被刪 → 內建
        assert_eq!(front_source(Some(&meta(now - 5, now - 5, None)), false, now), "builtin");
    }

    #[test]
    fn front_fetch_when() {
        let now = 10 * INDEX_TTL;
        assert!(front_should_fetch(None, false, false, now));
        assert!(!front_should_fetch(Some(&meta(now - 5, now - 5, None)), true, false, now));
        assert!(front_should_fetch(Some(&meta(now - 5, now - 5, None)), true, true, now)); // 手動「檢查官網更新」
        assert!(front_should_fetch(Some(&meta(now - INDEX_TTL, now - INDEX_TTL, None)), true, false, now));
        // 離線剛失敗過：10 分鐘內不再試，免得每次開頁都卡在連線逾時
        assert!(!front_should_fetch(Some(&meta(now - INDEX_TTL - 9, now - 60, None)), true, false, now));
        assert!(front_should_fetch(Some(&meta(now - INDEX_TTL - 9, now - FRONT_RETRY, None)), true, false, now));
        // 官網改版、規則沒命中：7 天後才再試
        assert!(!front_should_fetch(Some(&meta(0, now - 60, Some("x"))), true, false, now));
        assert!(front_should_fetch(Some(&meta(0, now - INDEX_TTL, Some("x"))), true, false, now));
    }

    #[test]
    fn front_after_keeps_old_on_failure() {
        let now = 10 * INDEX_TTL;
        let old = meta(now - INDEX_TTL - 9, now - INDEX_TTL - 9, None);
        // 離線：舊的那套留著（source 會是 cache），記下錯誤
        let m = front_after(Some(old.clone()), FrontGot::Net("逾時".into()), now);
        assert_eq!((m.fetched, m.checked, m.error.as_deref()), (old.fetched, now, Some("逾時")));
        assert_eq!(front_source(Some(&m), true, now), "cache");
        // 規則沒命中：退回內建，記規則名稱
        let m = front_after(Some(old.clone()), FrontGot::Rule("studio.js：交出整套 WB.bundle".into()), now);
        assert_eq!(front_source(Some(&m), true, now), "builtin");
        assert_eq!(m.failed_rule.as_deref(), Some("studio.js：交出整套 WB.bundle"));
        // 成功：清掉錯誤
        let m = front_after(Some(m), FrontGot::Ok(HashMap::from([("app.js".into(), "assets/app.2.js".into())])), now);
        assert_eq!((m.fetched, m.failed_rule, m.error), (now, None, None));
    }

    #[test]
    fn builtin_commit_from_vendor() {
        assert_eq!(vendor_commit("posewear 前端（MIT，https://github.com/atuizz/posewear），commit ed01734。\n").as_deref(), Some("ed01734"));
        assert_eq!(vendor_commit("x"), None);
    }
}

//! 內建網頁伺服器（只聽 127.0.0.1）。API 路徑、參數、回應都跟舊版 anima_ui/app/server.py 一樣，方便逐一比對。
//!
//!   網頁        GET  /                     web\index.html（其餘靜態檔在 /static/）
//!   即時事件    WS   /ws                    進度、即時預覽、完成的圖、下載進度
//!   其餘 API 見 router()
mod api_agent;
mod api_agent_tools;
mod api_assets;
mod api_charsheet;
mod api_bundle;
mod api_cloud;
mod api_colorbook;
mod api_comic;
mod api_director;
mod api_gallery;
mod api_gen;
mod api_h3;
mod api_h3chain;
mod api_shotsheet;
mod api_skills;
mod api_lib;
mod api_hub;
mod api_misc;
mod api_pe;
mod api_play;
mod api_refmod;
mod api_runninghub;
mod api_studio;
mod api_video;
mod api_video_review;
mod api_voice;
mod api_wf;
mod api_models;
mod api_update;
mod api_cache;
mod api_diagnostics;

use crate::civitai::Civitai;
use crate::comfy::Comfy;
use crate::db::Db;
use crate::hub::{Hub, Msg};
use crate::paths::Paths;
use crate::tags::TagIndex;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tower_http::services::ServeDir;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// /api/app/show 回應裡的程式名稱（main.rs 用來認出已經在跑的自己）
pub const APP_ID: &str = "anima-2";

pub struct App {
    pub diagnostics: Arc<crate::diagnostics::Store>,
    pub cache: Arc<crate::cache::Control>,
    pub setup: OnceLock<Arc<crate::setup::Setup>>,
    pub updates: Arc<crate::updater::Service>,
    pub paths: Paths,
    pub db: Arc<Db>,
    pub hub: Hub,
    pub comfy: Arc<Comfy>,
    pub civitai: Arc<Civitai>,
    /// AI 改寫（llama-server）
    pub pe: Arc<crate::pe::Service>,
    /// 反推（WD tagger）
    pub tagger: Arc<crate::tagger::Tagger>,
    /// 背景載入完才有（約一兩秒）；載入前自動完成回傳空清單
    pub tags: OnceLock<TagIndex>,
    /// 設定檔「讀 → 改 → 寫」要排隊，免得兩個請求互相蓋掉
    pub settings_lock: Arc<tokio::sync::Mutex<()>>,
    /// 資產庫：加檔、拿檔、刪資產要排隊（免得兩個請求同時加檔撞號、互相蓋掉 data）
    pub assets_lock: tokio::sync::Mutex<()>,
    pub frames_lock: Arc<tokio::sync::Mutex<()>>,
    /// 導演台（專案、角色卡、背景工作）
    pub director: api_director::Director,
    /// 漫畫台（作品、背景工作）
    pub comic: api_comic::Comic,
    /// 最後送進 ComfyUI 的模型家族（submit_jobs 記；漫畫台換家族前看它決定要不要 /free）。空的＝開站後還沒送過
    pub last_family: std::sync::Mutex<String>,
    /// 生圖助手（對話、背景工作、雲端模式）
    pub agent: api_agent::Agent,
    /// 影音工作台的剪輯台（FableCut 伺服器）
    pub studio: crate::studio::Studio,
    /// H3 影片台：背景工作（停止用）
    pub h3: api_h3::H3,
    /// 桌面版才有：叫出主視窗（第二次開程式時，新的那個會打 /api/app/show 叫舊的出來）
    pub show_window: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// 桌面版才有：重開生圖站自己開的 ComfyUI（外部模型資料夾改了要重開才生效）。Err＝不能重開的原因
    pub restart_comfy: OnceLock<Box<dyn Fn() -> Result<(), String> + Send + Sync>>,
}

pub type Shared = Arc<App>;

// ---------------------------------------------------------------- 錯誤與請求格式
/// 錯誤回應：{"error": 訊息}
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

pub fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
pub fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}
pub fn internal(msg: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg.to_string())
}
impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        internal(format!("資料庫錯誤：{e}"))
    }
}

pub type ApiResult = Result<Response, ApiError>;

pub fn ok(v: Value) -> ApiResult {
    Ok(Json(v).into_response())
}

/// JSON 請求內容；格式壞掉回 400（舊版大多回 500，生成那支回 400，這裡統一 400）
pub struct Body(pub Value);

impl<S: Send + Sync> FromRequest<S> for Body {
    type Rejection = ApiError;
    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let b = Bytes::from_request(req, state)
            .await
            .map_err(|e| bad(format!("讀不到送來的資料：{e}")))?;
        serde_json::from_slice(&b)
            .map(Body)
            .map_err(|e| bad(format!("送來的資料格式不對：{e}")))
    }
}

/// 在背景執行緒跑會卡住的工作（搬檔案、縮圖、打包 zip、組節點圖）
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    crate::diagnostics::blocking(f).await
}

/// 只留檔名（擋掉 ..\ 之類的路徑）
pub fn basename(name: &str) -> String {
    Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 讀檔回應（圖片、預覽）；找不到回 404
pub async fn file_response(path: PathBuf, cache: Option<&'static str>) -> ApiResult {
    let data = tokio::fs::read(&path)
        .await
        .map_err(|_| not_found("找不到"))?;
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("json") => "application/json",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    };
    let mut r = (StatusCode::OK, [(header::CONTENT_TYPE, mime)], data).into_response();
    if let Some(c) = cache {
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(c));
    }
    Ok(r)
}

// ---------------------------------------------------------------- 網頁與即時事件
/// 網頁的 JS／CSS 網址前綴：每次程式啟動都不同（/static-<啟動時間>）。
/// WebView2 會留著舊版的 JS 模組不重新驗證（2026-10-10 實測：更新後畫面是新的、deck.js 還是舊的），換網址就一定拿到新檔
pub fn static_prefix() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| format!("/static-{:x}", chrono::Local::now().timestamp_millis()))
}

/// 把頁面裡的 "/static/ 換成這次啟動的前綴
pub fn versioned_html(html: &str) -> String {
    html.replace("\"/static/", &format!("\"{}/", static_prefix()))
}

async fn index(State(app): State<Shared>) -> ApiResult {
    let html = tokio::fs::read_to_string(app.paths.web.join("index.html"))
        .await
        .map_err(|_| not_found("找不到"))?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        versioned_html(&html),
    )
        .into_response())
}

async fn ws(State(app): State<Shared>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |socket| ws_client(app, socket))
}

async fn ws_client(app: Shared, mut socket: WebSocket) {
    let l = app.comfy.live();
    let hello = json!({"type": "hello", "online": l.online, "remaining": l.queue_remaining, "running": l.running, "progress": l.progress});
    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let mut rx = app.hub.subscribe();
    loop {
        tokio::select! {
            m = rx.recv() => {
                let out = match m {
                    Ok(Msg::Text(t)) => Message::Text(t.to_string().into()),
                    Ok(Msg::Bin(b)) => Message::Binary(b),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue, // 網頁太慢：跳過舊的預覽
                    Err(_) => break,
                };
                if socket.send(out).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(_)) => {} // 網頁不會送東西過來
                _ => break,
            },
        }
    }
}

/// 叫出桌面版主視窗；回應帶 app 名稱，讓第二個執行個體分得出 port 上是自己還是舊版生圖站
async fn app_show(State(app): State<Shared>) -> ApiResult {
    let shown = app.show_window.get().map(|f| f()).is_some();
    ok(json!({"ok": shown, "app": APP_ID}))
}

/// 網頁的 JS／CSS 每次都向伺服器確認有沒有更新，避免改版後還在用舊檔
async fn no_stale_static(req: Request, next: Next) -> Response {
    let is_static = req.uri().path().starts_with("/static");
    let mut r = next.run(req).await;
    if is_static {
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    r
}

/// 限制本機服務只接受以本機名稱開啟的頁面及同源瀏覽器請求。
/// 否則其他網站可以透過 DNS rebinding 或表單向本機 API 送出指令。
pub(crate) fn local_request(req: &Request) -> bool {
    let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    if !matches!(
        authority.host(),
        "127.0.0.1" | "localhost" | "[::1]" | "::1"
    ) {
        return false;
    }
    if authority.port_u16().is_none() {
        return false;
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        let Ok(uri) = origin.parse::<axum::http::Uri>() else {
            return false;
        };
        if uri.scheme_str() != Some("http")
            || uri.authority() != Some(&authority)
            || uri.path() != "" && uri.path() != "/"
        {
            return false;
        }
    }
    true
}

async fn only_local(req: Request, next: Next) -> Response {
    if !local_request(&req) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "只接受本機頁面的請求"})),
        )
            .into_response();
    }
    next.run(req).await
}

pub fn attach_setup(app: &Shared, setup: Arc<crate::setup::Setup>) {
    setup.bind_runtime(app.cache.clone(), app.settings_lock.clone());
    let w = setup.clone();
    let _ = app.updates.setup_busy.set(Box::new(move || w.busy()));
    let _ = app.setup.set(setup);
}

pub fn router(app: Shared) -> Router {
    let web = app.paths.web.clone();
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws))
        // 狀態、設定
        .route("/api/state", get(api_gen::state))
        .route("/api/settings", post(api_gen::set_settings))
        .route("/api/cache", get(api_cache::status))
        .route("/api/cache/clear", post(api_cache::clear))
        .route("/api/settings/export", get(api_gen::settings_export))
        .route("/api/settings/import", post(api_gen::settings_import))
        // 影片模型（內建 MiniMax H3 或匯入的 ComfyUI 工作流）
        .route("/api/video/workflows", get(api_video::list).post(api_video::add))
        .route("/api/video/media/upload", post(api_video::upload_media).layer(DefaultBodyLimit::max(220 * 1024 * 1024)))
        .route("/api/video/workflows/{id}", get(api_video::detail).post(api_video::set_map).delete(api_video::remove))
        // 生圖工作流（匯入的 ComfyUI 工作流，生成頁模型選單選「⚙ 工作流：…」）
        .route("/api/image/workflows", get(api_wf::list).post(api_wf::import))
        .route("/api/image/workflows/{id}", delete(api_wf::remove))
        .route("/api/image/workflows/{id}/options", post(api_wf::set_options))
        .route("/api/image/workflows/{id}/fields", get(api_wf::fields))
        .route("/api/image/workflows/{id}/export", get(api_wf::export).post(api_wf::export_save))
        // 取代內建功能（settings 的 feature_flows）、把內建功能匯出成 ComfyUI 工作流
        .route("/api/feature_flows", get(api_wf::flows_get).post(api_wf::flows_set))
        .route("/api/gen/export_graph", post(api_gen::export_graph))
        // 存好的工作流檔：檔案總管打開並選取
        .route("/api/reveal", post(api_wf::reveal))
        // 模型與工作流：外部模型資料夾、掃描、總覽、加裝（api_models.rs）
        .route("/api/models/dirs", get(api_models::dirs))
        .route("/api/models/dirs/add", post(api_models::dirs_add))
        .route("/api/models/dirs/remove", post(api_models::dirs_remove))
        .route("/api/models/scan", get(api_models::scan_status).post(api_models::scan_start))
        .route("/api/models/scan/cancel", post(api_models::scan_cancel))
        .route("/api/models/scan/apply", post(api_models::scan_apply))
        .route("/api/models/inventory", get(api_models::inventory))
        .route("/api/models/other_families", get(api_models::other_families_get))
        .route("/api/models/classify", post(api_models::classify))
        .route("/api/models/upload", put(api_models::upload).layer(DefaultBodyLimit::disable()))
        .route("/api/models/import", post(api_models::import_path))
        .route("/api/models/place", post(api_models::place))
        .route("/api/models/download", post(api_models::download))
        .route("/api/models/downloads", get(api_models::downloads))
        .route("/api/models/restart_comfy", post(api_models::restart_comfy))
        .route("/api/models/fetch_listed", post(api_models::fetch_listed))
        // 生成與任務
        .route("/api/diagnostics", get(api_diagnostics::list))
        .route("/api/diagnostics/job/{job}", get(api_diagnostics::job))
        .route("/api/diagnostics/preferences", post(api_diagnostics::preferences))
        .route("/api/diagnostics/clear", post(api_diagnostics::clear))
        .route("/api/diagnostics/{id}/report", post(api_diagnostics::report))
        .route("/api/diagnostics/{id}/export", get(api_diagnostics::export))
        .route("/api/generate", post(api_gen::generate))
        .route("/api/detail", post(api_play::detail))
        .route("/api/play/models", get(api_play::models))
        .route("/api/play/save_grid", post(api_play::save_grid))
        .route("/api/jobs", get(api_gen::jobs_list))
        .route("/api/jobs/retry", post(api_gen::jobs_retry))
        .route("/api/jobs/clear", post(api_gen::jobs_clear))
        .route("/api/interrupt", post(api_gen::interrupt))
        .route("/api/clear_queue", post(api_gen::clear_queue))
        .route("/api/cancel", post(api_gen::cancel))
        // 系統、維護
        .route("/api/system", get(api_gen::system))
        .route("/api/free", post(api_gen::free_vram))
        .route(
            "/api/reset",
            get(api_misc::reset_info).post(api_misc::reset_do),
        )
        .route("/api/reset/undo", post(api_misc::reset_undo))
        // 輸入圖
        .route("/api/upload", post(api_gallery::upload))
        .route("/api/image/meta", post(api_gallery::image_meta))
        .route("/api/to_input", post(api_gallery::to_input))
        .route("/input/{name}", get(api_gallery::input_file))
        // 圖庫
        .route("/api/gallery", get(api_gallery::gallery))
        .route("/api/gallery/facets", get(api_gallery::facets))
        .route(
            "/api/gallery/prune_missing",
            post(api_gallery::prune_missing),
        )
        .route(
            "/api/gallery/repair_missing",
            post(api_gallery::repair_missing),
        )
        .route(
            "/api/image/{id}",
            get(api_gallery::image_info)
                .post(api_gallery::image_update)
                .delete(api_gallery::image_delete),
        )
        .route("/api/images/batch", post(api_gallery::images_batch))
        .route("/api/trash/empty", post(api_gallery::trash_empty))
        .route("/api/export", post(api_gallery::export_zip))
        .route("/img/{id}", get(api_gallery::img))
        .route("/thumb/{id}", get(api_gallery::thumb))
        // 提示詞工具、預設組、LoRA
        .route("/api/tags", get(api_misc::tags))
        // AI（MCP）連線狀態：標題列的燈號
        .route("/api/ai/heartbeat", post(api_misc::ai_heartbeat))
        .route("/api/ai/link", get(api_misc::ai_link))
        .route("/api/tagger", post(api_gallery::tagger))
        .route(
            "/api/presets",
            get(api_misc::presets).post(api_misc::save_preset),
        )
        .route("/api/presets/{id}", delete(api_misc::delete_preset))
        // 提示詞庫（整套設定＋片段）
        // 外掛寫法：使用者自備的提示詞 skill（api_skills.rs）
        .route("/api/skills", get(api_skills::list))
        .route("/api/skills/install", post(api_skills::install))
        .route("/api/skills/bind", post(api_skills::bind))
        .route("/api/skills/remove", post(api_skills::remove))
        .route("/api/skills/open", post(api_skills::open))
        .route("/api/skills/{id}", get(api_skills::get_one))
        .route("/api/lib", get(api_lib::list).post(api_lib::save))
        .route("/api/lib/used", post(api_lib::used))
        .route("/api/lib/order", post(api_lib::order))
        .route("/api/lib/folder", post(api_lib::folder))
        .route("/api/lib/{id}", delete(api_lib::delete))
        .route("/api/lib/{id}/thumb", get(api_lib::thumb))
        .route("/api/assets/{id}/file", post(api_assets::add_file))
        .route("/api/assets/{id}/file/remove", post(api_assets::remove_file))
        .route("/api/assets/{id}/to_input", post(api_assets::to_input))
        .route("/api/assets/{id}/apply", post(api_assets::apply))
        .route("/api/assets/{id}/refmod", post(api_assets::make_refmod))
        .route("/api/refmod/status", get(api_refmod::status))
        .route("/api/refmod/install", post(api_refmod::install))
        .route("/api/runninghub", get(api_runninghub::get))
        .route("/api/runninghub/config", post(api_runninghub::config))
        .route("/api/runninghub/test", post(api_runninghub::test))
        .route("/api/assets/file/{name}", get(api_assets::get_file))
        .route("/api/charsheet/prompt", post(api_charsheet::prompt))
        .route("/api/charsheet/tags", post(api_charsheet::tags))
        .route("/api/charsheet/spec", post(api_charsheet::spec))
        .route("/api/loras/meta", post(api_misc::lora_meta))
        .route("/api/loras/delete", post(api_misc::lora_delete))
        .route("/lora_preview/{file}", get(api_misc::lora_preview))
        .route("/api/loras/local-preview", get(api_misc::lora_local_preview))
        .route("/api/features/strip", get(api_misc::feature_strip))
        .route("/api/features/strip/install", post(api_misc::feature_strip_install))
        .route("/api/civitai/bases", get(api_misc::civitai_bases))
        .route("/api/civitai/search", get(api_misc::civitai_search))
        .route("/api/civitai/download", post(api_misc::civitai_download))
        .route("/api/hub/sources", get(api_hub::sources))
        .route("/api/hub/search", get(api_hub::search))
        .route("/api/hub/files", get(api_hub::files))
        .route("/api/hub/tags", get(api_hub::tags))
        .route("/api/hub/download", post(api_hub::download))
        .route("/api/hub/workflow", post(api_hub::workflow))
        .route("/api/hub/modelscope", get(api_hub::ms_get).post(api_hub::ms_set))
        .route("/api/hub/modelscope/test", post(api_hub::ms_test))
        .route("/api/hub/rh_run", post(api_hub::rh_run))
        .route("/api/hub/rh_stop", post(api_hub::rh_stop))
        .route("/api/downloads", get(api_misc::downloads))
        // AI 改寫提示詞
        .route("/api/pe/rewrite", post(api_pe::rewrite))
        .route("/api/pe/cancel", post(api_pe::cancel))
        .route("/api/pe/status", get(api_pe::status))
        .route("/api/pe/stop", post(api_pe::stop))
        .route("/api/pe/mmproj", post(api_pe::mmproj))
        .route("/api/poses", get(api_pe::poses))
        .route(
            "/api/pe/prompts",
            get(api_pe::prompts).post(api_pe::prompt_save),
        )
        // 導演台
        .route("/api/director/chars", get(api_director::chars_list).post(api_director::chars_save))
        .route("/api/director/chars/delete", post(api_director::chars_delete))
        .route("/api/director/describe", post(api_director::describe))
        .route("/api/director/file/{name}", get(api_director::file))
        .route("/api/director/projects", get(api_director::projects))
        .route("/api/director/project", post(api_director::project_save))
        .route("/api/director/project/new", post(api_director::project_new))
        .route("/api/director/from_sheet", post(api_director::from_sheet))
        .route("/api/director/take/select", post(api_director::take_select))
        .route("/api/director/tail/candidates", post(api_video_review::candidates))
        .route("/api/director/tail/select", post(api_video_review::select))
        .route("/api/director/review/{id}/{name}", get(api_video_review::image))
        .route("/api/director/project/delete", post(api_director::project_delete))
        .route("/api/director/project/{id}", get(api_director::project_get))
        .route("/api/director/write", post(api_director::write))
        .route("/api/director/run", post(api_director::run))
        .route("/api/director/concat", post(api_director::concat_api))
        .route("/api/director/stop", post(api_director::stop))
        // 漫畫台
        .route("/api/comic/meta", get(api_comic::meta))
        .route("/api/comic/projects", get(api_comic::projects))
        .route("/api/comic/project", post(api_comic::project_save))
        .route("/api/comic/project/new", post(api_comic::project_new))
        .route("/api/comic/project/delete", post(api_comic::project_delete))
        .route("/api/comic/project/{id}", get(api_comic::project_get))
        .route("/api/comic/run", post(api_comic::run))
        .route("/api/comic/stop", post(api_comic::stop))
        .route("/api/comic/prompts", post(api_comic::prompts))
        .route("/api/comic/layout", post(api_comic::layout_preview))
        .route("/api/comic/layout/ai", post(api_comic::layout_ai))
        .route("/api/comic/script", post(api_comic::script))
        .route("/api/comic/run/remove", post(api_comic::run_remove))
        // 整本上色（黑白漫畫，整本丟進來自動上色）
        .route("/api/colorbook", get(api_colorbook::list))
        .route("/api/colorbook/new", post(api_colorbook::new).layer(DefaultBodyLimit::max(800 * 1024 * 1024)))
        .route("/api/colorbook/{id}", get(api_colorbook::get))
        .route("/api/colorbook/{id}/sheet", post(api_colorbook::sheet))
        .route("/api/colorbook/{id}/page", post(api_colorbook::page))
        .route("/api/colorbook/{id}/run", post(api_colorbook::run))
        .route("/api/colorbook/{id}/stop", post(api_colorbook::stop))
        .route("/api/colorbook/{id}/file/{dir}/{name}", get(api_colorbook::file))
        .route("/api/colorbook/{id}/zip", get(api_colorbook::zip))
        .route("/api/colorbook/{id}/delete", post(api_colorbook::delete))
        // 生圖助手
        .route("/api/agent/sessions", get(api_agent::sessions))
        .route("/api/agent/session/new", post(api_agent::session_new))
        .route("/api/agent/session/settings", post(api_agent::session_settings))
        .route("/api/agent/base/model", post(api_agent::base_model))
        .route("/api/agent/session/delete", post(api_agent::session_delete))
        .route("/api/agent/session/{id}", get(api_agent::session_get))
        .route("/api/agent/say", post(api_agent::say))
        .route("/api/agent/stop", post(api_agent::stop))
        .route("/api/agent/confirm", post(api_agent::confirm))
        .route("/api/agent/fill", post(api_agent::fill))
        .route("/api/agent/loras", get(api_agent::loras))
        .route("/api/agent/loras/learn", post(api_agent::learn))
        .route("/api/agent/loras/stop", post(api_agent::learn_stop))
        .route("/api/agent/cloud", get(api_cloud::status))
        .route("/api/agent/cloud/gpus", get(api_cloud::gpus))
        .route("/api/agent/cloud/config", post(api_cloud::config))
        .route("/api/agent/cloud/test", post(api_cloud::test))
        .route("/api/agent/cloud/start", post(api_cloud::start))
        .route("/api/agent/cloud/stop", post(api_cloud::stop))
        .route("/api/agent/cloud/idle", post(api_cloud::idle))
        // H3 影片台（導演頁的「H3 影片」分頁）
        .route("/api/h3/info", get(api_h3::info))
        // 素材用 base64 包在 JSON 裡送（影片 15 秒以內、網頁擋 150MB 的檔，base64 多三分之一）：放寬全站 64MB 的限制。再大要改成串流上傳（大 JSON 很吃記憶體）
        .route("/api/h3/media", post(api_h3::media).layer(DefaultBodyLimit::max(220 * 1024 * 1024)))
        .route("/api/h3/generate", post(api_h3::generate))
        .route("/api/h3/stop", post(api_h3::stop))
        .route("/api/h3/write", post(api_h3::write))
        .route("/api/h3/chains", get(api_h3chain::list))
        .route("/api/h3/chain/save", post(api_h3chain::save))
        .route("/api/h3/chain/take/select", post(api_h3chain::select_take))
        .route("/api/h3/chain/delete", post(api_h3chain::delete))
        .route("/api/h3/chain/run", post(api_h3chain::run))
        .route("/api/h3/chain/stop", post(api_h3chain::stop))
        .route("/api/h3/chain/join", post(api_h3chain::join_api))
        .route("/api/h3/chain/{id}", get(api_h3chain::get))
        .route("/api/h3/chain/{id}/file/{name}", get(api_h3chain::file))
        .route("/api/shotsheet/meta", get(api_shotsheet::meta))
        .route("/api/shotsheet/compose", post(api_shotsheet::compose))
        .route("/api/shotsheet/check", post(api_shotsheet::check))
        .route("/api/shotsheet/danger", post(api_shotsheet::danger))
        .route("/api/shotsheet/polish", post(api_shotsheet::polish))
        .route("/api/shotsheet/template", post(api_shotsheet::template))
        .route("/api/voices", get(api_voice::list).post(api_voice::add).layer(DefaultBodyLimit::max(48 * 1024 * 1024)))
        .route("/api/voices/tts", post(api_voice::tts))
        .route("/api/voices/tts_workflow", post(api_voice::set_tts).layer(DefaultBodyLimit::max(48 * 1024 * 1024)))
        .route("/api/voices/{id}/file", get(api_voice::file))
        .route("/api/voices/{id}", delete(api_voice::remove))
        // 影音工作台（剪輯台＝FableCut、拆幀台）
        .route("/api/studio/status", get(api_studio::status))
        .route("/api/studio/start", post(api_studio::start))
        .route("/api/studio/stop", post(api_studio::stop))
        .route("/api/studio/import", post(api_studio::import))
        .route("/api/studio/open", post(api_studio::open_folder))
        .route("/api/studio/exports", get(api_studio::exports))
        .route("/api/studio/exports/save", post(api_studio::export_save))
        .route("/api/studio/ai", get(api_studio::ai))
        .route("/api/studio/frames", get(api_studio::frames_list))
        .route("/api/studio/frames/storage", get(api_studio::frames_storage))
        .route("/api/studio/frames/clear", post(api_studio::frames_clear))
        .route("/api/studio/frames/split", post(api_studio::frames_split))
        .route("/api/studio/frames/split_file", post(api_studio::frames_split_file).layer(DefaultBodyLimit::disable()))
        .route("/api/studio/frames/{sid}/analyze", get(api_studio::frames_analyze))
        .route("/api/studio/frames/{sid}/diff", get(api_studio::frames_diff))
        .route("/api/studio/frames/{sid}/graph", get(api_studio::frames_graph))
        .route("/api/studio/frames/{sid}", get(api_studio::frames_get))
        .route("/api/studio/frames/{sid}/f/{n}", get(api_studio::frame_full))
        .route("/api/studio/frames/{sid}/t/{n}", get(api_studio::frame_thumb))
        .route("/api/studio/frames/{sid}/sheet", get(api_studio::frames_sheet))
        .route("/api/studio/frames/{sid}/edit", post(api_studio::frames_edit))
        .route("/api/studio/frames/{sid}/upload", post(api_studio::frames_upload))
        .route("/api/studio/frames/{sid}/to_input", post(api_studio::frames_to_input))
        .route("/api/studio/frames/{sid}/build", post(api_studio::frames_build))
        .route("/api/studio/frames/{sid}/delete", post(api_studio::frames_delete))
        // 桌面版
        .route("/api/app/show", post(app_show))
        .route("/api/app/update", get(api_update::status))
        .route("/api/app/update/check", post(api_update::check))
        .route("/api/app/update/preferences", post(api_update::preferences))
        .route("/api/app/update/install", post(api_update::install))
        .nest_service(static_prefix(), ServeDir::new(web.clone()))
        .nest_service("/static", ServeDir::new(web))
        .layer(middleware::from_fn(no_stale_static))
        .layer(middleware::from_fn_with_state(app.clone(), api_diagnostics::capture))
        .layer(middleware::from_fn(only_local))
        .layer(middleware::from_fn_with_state(app.clone(), update_gate))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(app)
}

/// 更新中擋住新的修改請求；取得寫鎖後，先前已進入 handler 的請求也已經結束。
pub async fn update_gate(State(app): State<Shared>, req: Request, next: Next) -> Response {
    use std::sync::atomic::Ordering;
    let mutation = !matches!(*req.method(), axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS);
    let _cache_guard = if (mutation && req.uri().path() != "/api/cache/clear") || req.uri().path().starts_with("/thumb/") {
        if app.cache.cleaning.load(Ordering::SeqCst) { return (StatusCode::CONFLICT, Json(json!({"error":"正在清理快取，請稍後再操作"}))).into_response(); }
        let guard = app.cache.gate.read().await;
        if app.cache.cleaning.load(Ordering::SeqCst) { return (StatusCode::CONFLICT, Json(json!({"error":"正在清理快取，請稍後再操作"}))).into_response(); }
        Some(guard)
    } else { None };
    if matches!(*req.method(), axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS)
        || req.uri().path().starts_with("/api/app/update") {
        return next.run(req).await;
    }
    let blocked = || (StatusCode::CONFLICT, Json(json!({"error": "正在更新程式，請稍後再操作"}))).into_response();
    if app.updates.installing.load(Ordering::SeqCst) { return blocked(); }
    let _guard = app.updates.gate.read().await;
    if app.updates.installing.load(Ordering::SeqCst) { return blocked(); }
    next.run(req).await
}

/// 除了生圖佇列，也涵蓋兩段生圖之間的助手／漫畫／影片背景工作，以及各種下載。
pub async fn update_idle(app: &Shared) -> Result<(), String> {
    if app.frames_lock.try_lock().is_err() { return Err("拆幀台正在處理，請稍後再試".into()); }
    let live = app.comfy.live();
    let pe = app.pe.status();
    let civitai = app.civitai.downloads.lock().unwrap_or_else(|e| e.into_inner())
        .values().any(|d| matches!(d["status"].as_str(), Some("starting" | "downloading")));
    let busy = live.running.is_some() || live.queue_remaining > 0
        || !app.db.open_jobs().map_err(|_| "無法確認任務狀態，請稍後再更新")?.is_empty()
        || app.h3.any_running() || app.agent.any_running() || app.agent.cloud.preparing() || app.director.any_running() || app.comic.any_running()
        || pe["busy"] == true || pe["waiting"].as_i64().unwrap_or(0) > 0
        || civitai || api_models::any_downloading()
        || app.updates.setup_busy.get().is_some_and(|f| f());
    if busy { return Err("生圖、影片、改寫或模型下載還沒完成，請等工作結束再更新".into()); }
    if live.online {
        let q = app.comfy.get("/queue").await.map_err(|_| "無法確認 ComfyUI 佇列，請稍後再更新")?;
        for key in ["queue_running", "queue_pending"] {
            let a = q[key].as_array().ok_or("無法確認 ComfyUI 佇列，請稍後再更新")?;
            if !a.is_empty() { return Err("ComfyUI 還有工作，請等佇列完成再更新".into()); }
        }
    }
    Ok(())
}

/// 建立資料夾、開資料庫、連 ComfyUI、背景載入 tag 索引並匯入既有圖片
/// 姿勢參考圖（3D 人偶的深度圖／骨架）放進 ComfyUI input\anima_pose，生圖時 LoadImage 讀得到。只補缺的或大小不同的
pub use crate::graphs::POSE_INPUT;

fn copy_pose_controls(res: &Path, input: &Path) {
    let (src, dst) = (res.join("poses").join("control"), input.join(POSE_INPUT));
    let Ok(rd) = std::fs::read_dir(&src) else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dst) {
        eprintln!("建立 {} 失敗：{e}", dst.display());
        return;
    }
    for e in rd.flatten() {
        let to = dst.join(e.file_name());
        let same = match (e.metadata(), std::fs::metadata(&to)) {
            (Ok(a), Ok(b)) => a.len() == b.len(),
            _ => false,
        };
        if !same {
            if let Err(err) = std::fs::copy(e.path(), &to) {
                eprintln!("複製姿勢參考圖 {} 失敗：{err}", to.display());
            }
        }
    }
}

pub async fn start(paths: Paths, comfy_url: &str) -> Result<Shared, String> {
    crate::settings::migrate_plaintext_key(&paths.settings)
        .map_err(|e| format!("保護 Civitai 金鑰失敗：{e}"))?;
    for d in [&paths.thumbs, &paths.previews, &paths.input] {
        std::fs::create_dir_all(d).map_err(|e| format!("建立 {} 失敗：{e}", d.display()))?;
    }
    // 外部模型資料夾（model_dirs.rs）要比 AI 改寫早讀：AI 改寫的語言模型可能放在外部資料夾
    crate::model_dirs::init(&paths.data);
    crate::model_dirs::ensure_yaml(&paths.comfy_root);
    // 使用者自己 ComfyUI 的 extra_model_paths.yaml 裡的資料夾也算（model_dirs::roots）
    crate::model_dirs::init_comfy(&paths.comfy_root);
    let db = Arc::new(Db::open(&paths.db).map_err(|e| format!("開資料庫失敗：{e}"))?);
    // 程式附的 LoRA 備註（MOSS 合集等）併進資料庫：沒有的才補，要掃 LoRA 資料夾所以放背景
    {
        let (db, res, roots) = (db.clone(), paths.res.clone(), paths.lora_roots());
        std::thread::spawn(move || match crate::setup::merge_lora_notes(&db, &res, &roots) {
            0 => {}
            n => eprintln!("[LoRA 備註] 補上或更新了 {n} 筆"),
        });
    }
    let hub = Hub::new();
    let diagnostics = crate::diagnostics::Store::new(&paths);
    let comfy = Comfy::new(comfy_url, db.clone(), hub.clone());
    comfy.attach_diagnostics(diagnostics.clone());
    comfy.start();
    let civitai = Civitai::new(
        &paths.loras,
        &paths.previews,
        db.clone(),
        hub.clone(),
        &paths.settings,
    );
    let pe = crate::pe::Service::new(
        &paths.pe_node,
        &paths.input,
        comfy.clone(),
        &paths.settings,
        hub.clone(),
        &paths.res,
    );
    pe.start();
    copy_pose_controls(&paths.res, &paths.input);
    let tagger = Arc::new(crate::tagger::Tagger::new(
        &paths.data.join("models").join("tagger"),
    ));
    let data_dir = paths.data.clone();
    let studio = crate::studio::Studio::new(&paths.res, &paths.data);
    let app = Arc::new(App {
        diagnostics,
        cache: Arc::new(crate::cache::Control::default()),
        setup: OnceLock::new(),
        updates: Arc::new(crate::updater::Service::new(&data_dir)),
        paths,
        db,
        hub,
        comfy,
        civitai,
        pe,
        tagger,
        tags: OnceLock::new(),
        settings_lock: Arc::new(tokio::sync::Mutex::new(())),
        assets_lock: tokio::sync::Mutex::new(()),
        frames_lock: Arc::new(tokio::sync::Mutex::new(())),
        director: api_director::Director::new(&data_dir),
        comic: api_comic::Comic::new(&data_dir),
        last_family: std::sync::Mutex::new(String::new()),
        agent: api_agent::Agent::new(&data_dir),
        studio,
        h3: api_h3::H3::default(),
        show_window: OnceLock::new(),
        restart_comfy: OnceLock::new(),
    });
    api_cloud::spawn_idle_watch(&app);
    api_diagnostics::start_monitor(&app);
    // 舊版導演台角色卡搬進資產庫（只做一次；失敗下次開導演台或重開站會再試）
    let a = app.clone();
    tokio::spawn(async move {
        if let Err(e) = api_director::migrated(&a).await {
            eprintln!("舊角色卡搬進資產庫失敗：{}", e.1);
        }
    });
    // 升級換模型：刪掉官方 PE、換上新的通用 9B（只做一次；下載失敗下次開站再試，見 api_pe.rs）
    tokio::spawn(api_pe::retire_old_models(app.clone()));
    // ComfyUI 開著時，問它看得到哪些模型資料夾（補 yaml 讀不到的，例如別的啟動器用參數帶進去的資料夾）；每分鐘更新一次
    let live_url = comfy_url.to_string();
    tokio::spawn(async move {
        loop {
            let u = live_url.clone();
            let got = blocking(move || crate::model_dirs::refresh_live(&u)).await;
            // 加了資料夾、ComfyUI 還沒讀到：問勤一點，重開好了提示才會很快消失
            let quick = got.is_none() || !crate::model_dirs::pending_restart().is_empty();
            tokio::time::sleep(std::time::Duration::from_secs(if quick { 10 } else { 60 })).await;
        }
    });
    let bg = app.clone();
    tokio::spawn(async move {
        let a = bg.clone();
        // 自己的 data\tags 有就用（可以自己換新版）；全新安裝沒有，就用程式附帶的 resources\tags
        let ix = blocking(move || {
            let mine = a.paths.data.join("tags");
            let dir = if mine.join("danbooru.csv").exists() { mine } else { a.paths.res.join("tags") };
            TagIndex::load(&dir)
        })
        .await;
        let _ = bg.tags.set(ix);
        let a = bg.clone();
        let n = blocking(move || crate::imgmeta::import_existing(&a.db, &a.paths.output)).await;
        if n > 0 {
            eprintln!("圖庫匯入既有圖片 {n} 張");
        }
    });
    Ok(app)
}

pub async fn serve(app: Shared, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_on(router(app), listener).await
}

/// 用已經搶到的 port 開伺服器（桌面版先搶 port，搶不到就知道已經有人在跑；路由另外加了精靈的 API）
pub async fn serve_on(router: Router, listener: tokio::net::TcpListener) -> std::io::Result<()> {
    eprintln!("繪匣 PaintDeck：http://{}", listener.local_addr()?);
    axum::serve(listener, router).await
}

#[cfg(test)]
mod security_tests {
    use super::*;

    fn request(host: &str, origin: Option<&str>) -> Request {
        let mut req = Request::new(axum::body::Body::empty());
        req.headers_mut()
            .insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            req.headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
        }
        req
    }

    #[test]
    fn local_host_and_origin() {
        assert!(local_request(&request(
            "127.0.0.1:8190",
            Some("http://127.0.0.1:8190")
        )));
        assert!(local_request(&request("localhost:8190", None)));
        assert!(!local_request(&request("example.invalid:8190", None)));
        assert!(!local_request(&request("127.0.0.1.evil:8190", None)));
        assert!(!local_request(&request(
            "127.0.0.1:8190",
            Some("https://evil.invalid")
        )));
        assert!(!local_request(&request("127.0.0.1:8190", Some("null"))));
    }
}

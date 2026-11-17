//! 「隨機」頁的隨機產生器（2026-11-17，設計：docs\superpowers\specs\2026-11-17-wardrobe-and-danbooru-library-design.md）：
//! 用 Comfyui-Anima-Wardrobe-V6（GPL-3.0，作者灵冻不冻；底層腳本作者飞蓬）當引擎。它是 ComfyUI 外掛，自己在 ComfyUI 上掛了
//! /anima-wardrobe-v6/* 的 HTTP API（/roll、/characters、/scenes、/outfit-library…）。繪匣只負責裝外掛（跟 RefMod 同一套，
//! 釘在審過的 commit）和原樣轉送那個前綴底下的請求，抽籤全是它的引擎做的——同參數、同種子，跟它的節點實跑一模一樣。
//! 審過的內容：沒有連網、沒有執行外部程式，只寫它自己的 presets\ 資料夾；寫 PNG 中繼資料那支只在它自己的節點跑時生效，繪匣不放它的節點。
use super::api_refmod::{clone_pinned, installed_commit};
use super::{bad, blocking, ok, ApiError, ApiResult, Shared};
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, RawQuery, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::path::PathBuf;

pub const REPO: &str = "https://github.com/lingdongbudong/Comfyui-Anima-Wardrobe-V6";
pub const COMMIT: &str = "bb45472fcd4d52f7dc80ecae79316fbfb8cd72e8";
pub const DIR: &str = "Comfyui-Anima-Wardrobe-V6";
/// 它在 ComfyUI 上掛的前綴（另有 v5／v4／v3 的舊別名，只用新的）
const PREFIX: &str = "/anima-wardrobe-v6/";

fn node_dir(app: &Shared) -> PathBuf {
    app.paths.comfy_root.join("custom_nodes").join(DIR)
}

/// 轉送的路徑：只收一般的段（不准 ..、.、空段、反斜線），擋掉跳出前綴
pub fn safe_segments(rest: &str) -> Option<Vec<&str>> {
    let segs: Vec<&str> = rest.split('/').collect();
    let good = segs.iter().all(|s| !s.is_empty() && *s != "." && *s != ".." && !s.contains('\\'));
    (good && !segs.is_empty()).then_some(segs)
}

async fn loaded(app: &Shared) -> bool {
    let Ok(url) = reqwest::Url::parse(&format!("{}{PREFIX}health", app.comfy.url())) else { return false };
    matches!(app.comfy.forward(reqwest::Method::GET, url, None).await, Ok((200, _, _)))
}

/// GET → {installed, same, loaded, online, remote, repo, commit}
pub async fn status(State(app): State<Shared>) -> ApiResult {
    let dir = node_dir(&app);
    let (installed, head) = blocking(move || (dir.join("__init__.py").is_file(), installed_commit(&dir))).await;
    let online = app.comfy.live().online;
    let loaded = online && loaded(&app).await;
    ok(json!({"installed": installed, "same": head.as_deref() == Some(COMMIT), "loaded": loaded, "online": online,
              "remote": app.comfy.is_remote(), "repo": REPO, "commit": COMMIT}))
}

/// POST → 裝到本機 ComfyUI 的 custom_nodes，回 {installed, fresh, restart}
pub async fn install(State(app): State<Shared>) -> ApiResult {
    static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _g = INSTALLING.lock().await;
    if app.comfy.is_remote() {
        return Err(bad("現在連的是雲端的 ComfyUI：隨機產生器只能裝在本機的 ComfyUI"));
    }
    let dest = node_dir(&app);
    let fresh = blocking(move || clone_pinned(&dest, REPO, COMMIT, DIR)).await.map_err(bad)?;
    ok(json!({"installed": true, "fresh": fresh, "restart": fresh || !loaded(&app).await}))
}

/// GET／POST /api/wardrobe/x/<路徑> → 本機 ComfyUI 的 /anima-wardrobe-v6/<路徑>（query、body、content-type 原樣轉）
pub async fn forward(State(app): State<Shared>, method: Method, UrlPath(rest): UrlPath<String>, RawQuery(query): RawQuery,
                     headers: HeaderMap, body: Bytes) -> Result<Response, ApiError> {
    let segs = safe_segments(&rest).ok_or_else(|| bad("路徑不對"))?;
    if !app.comfy.live().online {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "本機的 ComfyUI 沒有連線：隨機產生器要 ComfyUI 開著".into()));
    }
    let mut url = reqwest::Url::parse(&format!("{}{PREFIX}", app.comfy.url())).map_err(|e| bad(e.to_string()))?;
    url.path_segments_mut().map_err(|_| bad("ComfyUI 網址不對"))?.pop_if_empty().extend(segs);
    url.set_query(query.as_deref());
    let m = if method == Method::POST { reqwest::Method::POST } else { reqwest::Method::GET };
    let send = (m == reqwest::Method::POST).then(|| {
        let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
        (body.to_vec(), ct)
    });
    let (code, ct, bytes) = app.comfy.forward(m, url, send).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.0))?;
    if code == 404 && !loaded(&app).await {
        return Err(ApiError(StatusCode::NOT_FOUND, "本機 ComfyUI 還沒有隨機產生器（Wardrobe 外掛）：到「隨機」頁按「安裝隨機產生器」，裝好重開 ComfyUI".into()));
    }
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
    // 縮圖不太會變：讓網頁快取一天
    let cache = if rest.starts_with("thumb/") || rest.contains("-thumb/") { "max-age=86400" } else { "no-store" };
    Ok((status, [(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, cache.to_string())], bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cannot_escape_the_prefix() {
        assert_eq!(safe_segments("roll"), Some(vec!["roll"]));
        assert_eq!(safe_segments("thumb/hatsune_miku"), Some(vec!["thumb", "hatsune_miku"]));
        assert_eq!(safe_segments("../prompt"), None);
        assert_eq!(safe_segments("thumb/../../x"), None);
        assert_eq!(safe_segments("thumb//x"), None);
        assert_eq!(safe_segments("a\\..\\b"), None);
        assert_eq!(safe_segments("./roll"), None);
    }
}

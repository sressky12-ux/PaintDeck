//! 「畫廊」頁（2026-11-23）：搜 Danbooru／Gelbooru／Safebooru（還有 AI TAG）的參考圖，圖丟進圖生圖、多圖參考，或一鍵複用原圖的 tag。
//! 圖源用 ComfyUI-Aaalice-Nodes（MIT，作者 Aaalice233）的 Booru Gallery：它是 ComfyUI 外掛，自己在 ComfyUI 上掛了
//! /aaalice/booru-gallery/* 的 HTTP API（sources、search、ranking、detail、media、settings…）。繪匣跟隨機頁的 Wardrobe 一樣，
//! 只負責裝外掛（釘在審過的 commit）和原樣轉送那個前綴底下的請求；搜尋、快取、各站的帳號金鑰都是它自己管。
//! 審過的內容（commit 2395128）：Booru Gallery 只連 danbooru.donmai.us、gelbooru.com、safebooru.org、aitag.win 和各站的圖床；
//! 金鑰存在 ComfyUI 使用者資料夾（user\default\aaalice-nodes），GET settings 只回「有沒有填」（credentialStatus），不回金鑰本身。
//! 同一包還有 Krita 橋接（會跑 tasklist 看 Krita 有沒有開）、Discord 分享（只在 ComfyUI 網頁裡按分享才連它的轉發站），繪匣都不碰。
use super::api_refmod::{clone_pinned, installed_commit};
use super::api_wardrobe::safe_segments;
use super::{bad, blocking, ok, ApiError, ApiResult, Shared};
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, RawQuery, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::path::PathBuf;

pub const REPO: &str = "https://github.com/Aaalice233/ComfyUI-Aaalice-Nodes";
pub const COMMIT: &str = "2395128e6f02d60ccc4c2b514201533cd8fd3f92";
pub const DIR: &str = "ComfyUI-Aaalice-Nodes";
/// 它在 ComfyUI 上掛的前綴
const PREFIX: &str = "/aaalice/booru-gallery/";

fn node_dir(app: &Shared) -> PathBuf {
    app.paths.comfy_root.join("custom_nodes").join(DIR)
}

/// 外掛有沒有載進 ComfyUI：它的 sources 回 200 才算
async fn loaded(app: &Shared) -> bool {
    let Ok(url) = reqwest::Url::parse(&format!("{}{PREFIX}sources", app.comfy.url())) else { return false };
    matches!(app.comfy.forward(reqwest::Method::GET, url, None).await, Ok((200, _, _)))
}

/// 圖（縮圖、原圖）不太會變：讓網頁快取一天；其他（搜尋結果、設定）一律不快取
pub fn cache_header(rest: &str) -> &'static str {
    if rest == "media" { "private, max-age=86400" } else { "no-store" }
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

/// POST → 裝到本機 ComfyUI 的 custom_nodes，回 {installed, fresh, restart}。它只多要 certifi（ComfyUI 的 Python 本來就有）
pub async fn install(State(app): State<Shared>) -> ApiResult {
    static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _g = INSTALLING.lock().await;
    if app.comfy.is_remote() {
        return Err(bad("現在連的是雲端的 ComfyUI：畫廊的圖源外掛只能裝在本機的 ComfyUI"));
    }
    let dest = node_dir(&app);
    let fresh = blocking(move || clone_pinned(&dest, REPO, COMMIT, DIR)).await.map_err(bad)?;
    ok(json!({"installed": true, "fresh": fresh, "restart": fresh || !loaded(&app).await}))
}

/// GET／POST /api/booru/x/<路徑> → 本機 ComfyUI 的 /aaalice/booru-gallery/<路徑>（query、body、content-type 原樣轉）
pub async fn forward(State(app): State<Shared>, method: Method, UrlPath(rest): UrlPath<String>, RawQuery(query): RawQuery,
                     headers: HeaderMap, body: Bytes) -> Result<Response, ApiError> {
    let segs = safe_segments(&rest).ok_or_else(|| bad("路徑不對"))?;
    if !app.comfy.live().online {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "本機的 ComfyUI 沒有連線：畫廊要 ComfyUI 開著".into()));
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
        return Err(ApiError(StatusCode::NOT_FOUND, "本機 ComfyUI 還沒有畫廊的圖源外掛（ComfyUI-Aaalice-Nodes）：到「畫廊」頁按「安裝圖源外掛」，裝好重開 ComfyUI".into()));
    }
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, [(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, cache_header(&rest).to_string())], bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_media_is_cached() {
        assert_eq!(cache_header("media"), "private, max-age=86400");
        assert_eq!(cache_header("search"), "no-store");
        assert_eq!(cache_header("settings"), "no-store");
        assert_eq!(cache_header("media/x"), "no-store");
    }

    #[test]
    fn pinned_commit_is_a_full_sha() {
        assert_eq!(COMMIT.len(), 40);
        assert!(COMMIT.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

//! 衣姿整頁「試穿工作台」的轉發代理（2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-page-design.md）。
//! 照搬 posewear 官網的 Cloudflare Function `functions/api/proxy.js`（MIT，github.com/atuizz/posewear）：
//! 火山方舟（豆包 Seedream）等介面不准瀏覽器跨網域呼叫，試穿頁把請求送到這裡，由這裡轉給官方介面。
//! 規則照原檔：只收本站頁面的請求、只轉發到官方介面網域和固定路徑、結果圖只從官方 CDN 下載、
//! 自訂中轉擋內網、金鑰只經過不記錄、每分鐘限次數。錯誤訊息照原檔（簡體，衣姿頁的繁體模式會自己轉）。
use axum::extract::Request;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 官方介面的路徑（原檔 API_PATH）
const API_PATH: &str = r"^/(api/v3|v\d+(beta|alpha)?)/(images/(edits|generations)|models(/[^/]+(:generateContent)?)?)$";
/// 自訂中轉准許的路徑
const CUSTOM_PATH: &str = r"/(images/(edits|generations)|models(/[^/]+(:generateContent)?)?)$";
const OFFICIAL: [&str; 4] = [
    r"^api\.openai\.com$",
    r"^generativelanguage\.googleapis\.com$",
    r"^ark\.[a-z0-9-]+\.volces\.com$",
    r"^ark\.[a-z0-9-]+\.bytepluses\.com$",
];
const IMG_HOSTS: [&str; 4] = [
    r"^oaidalleapiprodscus\.blob\.core\.windows\.net$",
    r"(^|\.)volces\.com$",
    r"(^|\.)bytepluses\.com$",
    r"(^|\.)googleusercontent\.com$",
];
const INTRANET: &str = r"^(localhost|127\.|10\.|192\.168\.|169\.254\.|\[)";
/// 轉發時丟掉的標頭（原檔 DROP；另外 cf- 開頭的也丟）
const DROP: [&str; 14] = [
    "host", "x-target-url", "x-proxy-kind", "x-proxy-custom", "cookie", "origin", "referer", "x-forwarded-for",
    "x-forwarded-proto", "x-real-ip", "content-length", "sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest",
];
const WIN: Duration = Duration::from_secs(60);
const MAX_API: u32 = 20;
const MAX_IMG: u32 = 40;

fn any(pats: &[&str], s: &str) -> bool {
    pats.iter().any(|p| Regex::new(p).is_ok_and(|r| r.is_match(s)))
}

fn j(status: u16, msg: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, [(header::CACHE_CONTROL, "no-store")], axum::Json(json!({"error": {"message": msg}}))).into_response()
}

/// 目標網址檢查（原檔 onRequest 裡的規則，順序也一樣）。錯誤回 (狀態碼, 訊息)
pub fn check_target(url: &str, image: bool, custom: bool, get: bool) -> Result<reqwest::Url, (u16, &'static str)> {
    let t = reqwest::Url::parse(url).map_err(|_| (400, "缺少或无效的 x-target-url"))?;
    if t.scheme() != "https" || !t.username().is_empty() || t.password().is_some() || t.port().is_some_and(|p| p != 443) {
        return Err((400, "目标地址必须是标准 https 地址"));
    }
    let host = t.host_str().unwrap_or("");
    if image {
        if !get {
            return Err((405, "图片下载只支持 GET"));
        }
        if !any(&IMG_HOSTS, host) {
            return Err((403, "该图片地址不在允许范围内"));
        }
        return Ok(t);
    }
    let official = any(&OFFICIAL, host);
    if !official && !custom {
        return Err((403, "该地址不是官方接口；自定义中转需在设置中选择「自定义」"));
    }
    let path = t.path().trim_end_matches('/');
    let path_ok = if official { any(&[API_PATH], path) } else { any(&[CUSTOM_PATH], path) };
    if !path_ok {
        return Err((403, "该路径不允许通过代理转发"));
    }
    if !official && any(&[INTRANET], host) {
        return Err((403, "不能转发到内网地址"));
    }
    Ok(t)
}

/// 只接受本站頁面發起的請求：Origin（沒有就看 Referer）的主機要等於這次請求的 Host
pub fn same_site(h: &HeaderMap) -> bool {
    let me = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    for k in [header::ORIGIN, header::REFERER] {
        if let Some(v) = h.get(&k).and_then(|v| v.to_str().ok()) {
            return reqwest::Url::parse(v).ok().is_some_and(|u| {
                let host = u.host_str().unwrap_or("");
                let hp = match u.port() {
                    Some(p) => format!("{host}:{p}"),
                    None => host.to_string(),
                };
                !me.is_empty() && hp == me
            });
        }
    }
    false
}

/// 每分鐘次數限制（記憶體計數）
fn limited(kind: &str) -> bool {
    static RL: OnceLock<Mutex<HashMap<String, (Instant, u32)>>> = OnceLock::new();
    let mut m = RL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let e = m.entry(kind.to_string()).or_insert((now, 0));
    if now.duration_since(e.0) > WIN {
        *e = (now, 0);
    }
    e.1 += 1;
    e.1 > if kind == "img" { MAX_IMG } else { MAX_API }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .proxy(crate::net::proxy())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600)) // 生圖介面可能要好幾分鐘
        .build()
        .map_err(|e| e.to_string())
}

pub async fn preflight() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// GET／POST /api/proxy：x-target-url 指定目標；x-proxy-kind: image 是下載結果圖；x-proxy-custom: 1 是自訂中轉
pub async fn proxy(req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let get = parts.method == Method::GET;
    if !get && parts.method != Method::POST {
        return j(405, "只支持 GET / POST");
    }
    let h = &parts.headers;
    if !same_site(h) {
        return j(403, "只接受本站页面发起的请求");
    }
    let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let image = hv("x-proxy-kind") == "image";
    let custom = hv("x-proxy-custom") == "1";
    let t = match check_target(&hv("x-target-url"), image, custom, get) {
        Ok(t) => t,
        Err((s, m)) => return j(s, m),
    };
    if limited(if image { "img" } else { "api" }) {
        return j(429, "请求过于频繁，请稍后再试");
    }
    let c = match client() {
        Ok(c) => c,
        Err(_) => return j(502, "代理无法连接目标服务"),
    };
    // 只記網域和結果，不記內容和金鑰
    let host = t.host_str().unwrap_or("").to_string();
    if image {
        let r = match c.get(t).send().await {
            Ok(r) => r,
            Err(_) => return j(502, "结果图片下载失败"),
        };
        let ct = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if !r.status().is_success() || !ct.starts_with("image/") {
            eprintln!("衣姿試穿代理：結果圖 {host} 失敗（{}）", r.status().as_u16());
            return j(502, "结果图片下载失败");
        }
        return match r.bytes().await {
            Ok(b) => ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-store".into()), (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into())], b)
                .into_response(),
            Err(_) => j(502, "结果图片下载失败"),
        };
    }
    let mut out = reqwest::header::HeaderMap::new();
    for (k, v) in h {
        let l = k.as_str().to_ascii_lowercase();
        if DROP.contains(&l.as_str()) || l.starts_with("cf-") {
            continue;
        }
        if let (Ok(k), Ok(v)) = (reqwest::header::HeaderName::from_bytes(k.as_str().as_bytes()), reqwest::header::HeaderValue::from_bytes(v.as_bytes())) {
            out.insert(k, v);
        }
    }
    let mut rb = c.request(if get { reqwest::Method::GET } else { reqwest::Method::POST }, t).headers(out);
    if !get {
        match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
            Ok(b) => rb = rb.body(reqwest::Body::from(b)),
            Err(_) => return j(413, "请求内容太大"),
        }
    }
    match rb.send().await {
        Ok(r) => {
            let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            eprintln!("衣姿試穿代理：{host} → {}", status.as_u16());
            let ct = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
            let b = r.bytes().await.unwrap_or_default();
            let mut resp = (status, b).into_response();
            if let Some(ct) = ct.and_then(|c| c.parse().ok()) {
                resp.headers_mut().insert(header::CONTENT_TYPE, ct);
            }
            resp.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().expect("header"));
            resp
        }
        Err(_) => j(502, "代理无法连接目标服务"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_rules() {
        for u in [
            "https://api.openai.com/v1/images/edits",
            "https://ark.cn-beijing.volces.com/api/v3/images/generations",
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-image:generateContent",
            "https://api.openai.com/v1/models",
            "https://api.openai.com:443/v1/images/edits",
        ] {
            assert!(check_target(u, false, false, false).is_ok(), "{u}");
        }
        assert!(check_target("https://ark-content-generation-v2-cn-beijing.tos-cn-beijing.volces.com/x.png", true, false, true).is_ok());
        assert!(check_target("https://my-relay.example.com/v1/images/edits", false, true, false).is_ok());
        let cases = [
            ("http://api.openai.com/v1/images/edits", false, false, false, 400),
            ("https://u:p@api.openai.com/v1/images/edits", false, false, false, 400),
            ("https://api.openai.com:8443/v1/images/edits", false, false, false, 400),
            ("not a url", false, false, false, 400),
            ("https://api.openai.com/v1/files", false, false, false, 403),
            ("https://evil.com/v1/images/edits", false, false, false, 403),
            ("https://127.0.0.1/v1/images/edits", false, true, false, 403),
            ("https://localhost/v1/images/edits", false, true, false, 403),
            ("https://192.168.1.5/v1/images/edits", false, true, false, 403),
            ("https://my-relay.example.com/v1/chat/completions", false, true, false, 403),
            ("https://evil.com/x.png", true, false, true, 403),
            ("https://evilvolces.com/x.png", true, false, true, 403),
            ("https://x.volces.com/x.png", true, false, false, 405),
        ];
        for (u, img, custom, get, code) in cases {
            assert_eq!(check_target(u, img, custom, get).map(|_| ()).unwrap_err().0, code, "{u}");
        }
    }

    #[test]
    fn same_site_rules() {
        let mk = |pairs: &[(&str, &str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
            }
            h
        };
        assert!(same_site(&mk(&[("host", "127.0.0.1:8190"), ("origin", "http://127.0.0.1:8190")])));
        assert!(same_site(&mk(&[("host", "127.0.0.1:8190"), ("referer", "http://127.0.0.1:8190/static/posewear/index.html")])));
        assert!(!same_site(&mk(&[("host", "127.0.0.1:8190"), ("origin", "https://evil.com")])));
        assert!(!same_site(&mk(&[("host", "127.0.0.1:8190")])));
        assert!(!same_site(&mk(&[("host", "127.0.0.1:8190"), ("origin", "null")])));
    }
}

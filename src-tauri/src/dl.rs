//! 可續傳的下載（2026-10-21）：貼網址下載模型（server/api_models.rs）、Civitai 的 LoRA（civitai.rs）共用。
//! 做法跟安裝精靈的 Setup::fetch（setup.rs）一樣，為的是「梯子不穩、網路忽斷忽續」也能把幾 GB 的檔案下載完：
//!   - 先寫 檔名.part，下載到一半斷線就留著；自動重試、或使用者過一會兒再按一次，都用 HTTP Range 接著下載
//!   - 逾時：送出請求後 60 秒沒有回應、收資料時 60 秒一個位元組都沒來，都當作斷線（不會卡死）
//!   - 重試只算「連續沒有進展」的失敗：這次 .part 長得比以前都大（有收到新資料）就歸零，所以很慢但有在走的線路最後也下載得完；
//!     連續 8 次沒有進展才放棄，每次多等 5 秒（最多 30 秒）
//!   - 同一個檔有好幾個來源（net::candidates：Hugging Face → hf-mirror.com…）：連不上或不給檔就換下一個，.part 接著用
//!   - 收到網頁（要登入、網址失效）不當成檔案；下載完核對 Content-Length，有預期大小也核對
//!   - 訊息一律中文（net::explain），只寫主機名不寫網址（Civitai 的網址帶 token，Civitai 的金鑰只會送給 civitai.com）
//!
//! 失敗時 .part 都留著，下次接著下載；只有伺服器說續傳位置不對（檔案換版了）才會刪掉重來。
use crate::net;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// 最多試幾次（不管有沒有進展）：伺服器每次只多給一點點資料這種怪情況的保險
const MAX_TRIES: u32 = 400;

#[derive(Debug, Clone)]
pub struct Policy {
    /// 送出請求到收到回應標頭最多等多久
    pub head: Duration,
    /// 收資料時，這麼久一個位元組都沒來就當作斷線
    pub stall: Duration,
    /// 連續幾次沒有進展就放棄
    pub max_fails: u32,
    /// 失敗後等多久再試＝這個時間 × 連續失敗次數（最多 6 倍）
    pub backoff: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { head: Duration::from_secs(60), stall: Duration::from_secs(60), max_fails: 8, backoff: Duration::from_secs(5) }
    }
}

/// 一個下載工作
pub struct Job<'a> {
    /// 要自己加 connect_timeout、proxy(net::proxy())（civitai.rs、api_models.rs 的 client 都有）
    pub http: &'a reqwest::Client,
    /// 下載網址。Civitai 的帶 token：不會出現在任何訊息、紀錄裡
    pub url: &'a str,
    /// Civitai 的 API key（放 Authorization 標頭；只會送給 civitai.com）
    pub bearer: Option<&'a str>,
    /// 魔塔登入後拿到的 Cookie（下載私有模型用；只會送給 modelscope.cn）
    pub cookie: Option<&'a str>,
    /// 預期的檔案大小（清單上寫的確切大小）；None＝只核對伺服器給的 Content-Length
    pub size: Option<u64>,
    /// .part 檔名的記號：檔名.記號.part。檔名相同但來源（版本）不同的下載才不會接錯檔
    pub tag: Option<&'a str>,
    /// 設成 true 就停（等待中也會馬上停）
    pub cancel: Option<&'a AtomicBool>,
    pub policy: Policy,
}

impl<'a> Job<'a> {
    pub fn new(http: &'a reqwest::Client, url: &'a str) -> Self {
        Job { http, url, bearer: None, cookie: None, size: None, tag: None, cancel: None, policy: Policy::default() }
    }
}

/// 第一個回應的資訊（檔名要看回應才知道時用）
pub struct Head {
    /// Content-Disposition 標頭
    pub disposition: Option<String>,
    /// 轉址之後的網址路徑最後一段（沒解碼）
    pub last_segment: Option<String>,
    /// 檔案的總大小（伺服器有給的話）
    pub total: Option<u64>,
}

pub type PickFn<'a> = dyn FnMut(&Head) -> Result<PathBuf, String> + Send + 'a;
/// 進度：（已下載, 總大小）。開始、下載中（約每 0.4 秒）、結束都會呼叫
pub type Progress<'a> = dyn FnMut(u64, Option<u64>) + Send + 'a;

pub enum Dest<'a> {
    /// 檔案的最後位置已經知道。已經有的同名檔會被換掉
    Path(PathBuf),
    /// 要看第一個回應（Content-Disposition、網址最後一段）才知道檔名：收到回應就呼叫，回傳最後位置；回傳 Err 就停止下載
    Pick(Box<PickFn<'a>>),
}

enum Fail {
    /// 中斷了（斷線、逾時、伺服器忙）：等一下接著下載
    Retry(String),
    /// 不用再試了（寫檔失敗、已取消）
    Fatal(String),
    /// 連不上這個來源（還沒收到回應）：有別的來源就換，沒有就照 Retry
    Down(String),
    /// 這個來源回應了但不給檔（要登入、404、大小不對）：有別的來源就換，沒有就停止
    NoFile(String),
}
use Fail::{Down, Fatal, NoFile, Retry};

/// 下載到 dest（先寫 dest.part，下載完才改名）。成功回傳最後的位置
pub async fn fetch(job: &Job<'_>, dest: Dest<'_>, progress: &mut Progress<'_>) -> Result<PathBuf, String> {
    let (mut dest, mut pick) = match dest {
        Dest::Path(p) => (Some(p), None),
        Dest::Pick(f) => (None, Some(f)),
    };
    if let Some(d) = &dest {
        make_parent(d).await?;
    }
    let urls = net::candidates(job.url);
    let pol = &job.policy;
    let mut best = dest.as_deref().map_or(0, |d| part_len(&part_path(d, job.tag)));
    let (mut which, mut fails, mut tries) = (0usize, 0u32, 0u32);
    let mut last = String::new();
    loop {
        if cancelled(job) {
            return Err("已取消".into());
        }
        tries += 1;
        if tries > MAX_TRIES {
            return Err(format!("下載重試太多次了：{}已下載的部分留著，再試一次會接著下載。", sentence(&last)));
        }
        let r = once(job, &urls[which], &mut Slot { dest: &mut dest, pick: &mut pick, best: &mut best }, progress).await;
        // 進展＝.part 比以前任何時候都大（伺服器不給續傳、每次從頭來的話，不會一直重新算進展）
        let have = dest.as_deref().map_or(0, |d| part_len(&part_path(d, job.tag)));
        let moved = have > best;
        best = best.max(have);
        match r {
            Ok(()) => {
                let Some(d) = dest else { return Err("內部錯誤：下載完了卻不知道檔案位置".into()) };
                let part = part_path(&d, job.tag);
                if d.is_file() {
                    let _ = tokio::fs::remove_file(&d).await;
                }
                tokio::fs::rename(&part, &d).await.map_err(|e| format!("改名失敗：{e}"))?;
                net::used(&urls[which]);
                return Ok(d);
            }
            Err(Fatal(e)) => return Err(e),
            Err(Down(e) | NoFile(e)) if which + 1 < urls.len() => {
                which += 1;
                fails = 0;
                eprintln!("[下載] {e} → 改從 {} 下載", host_of(&urls[which]));
                last = e;
            }
            Err(NoFile(e)) => return Err(e),
            Err(Retry(e) | Down(e)) => {
                last = e;
                fails = if moved { 0 } else { fails + 1 };
                if fails >= pol.max_fails {
                    return Err(format!(
                        "下載一直失敗（連續 {fails} 次沒有進展）：{}已下載的部分留著，再試一次會接著下載。",
                        sentence(&last)
                    ));
                }
                let wait = pol.backoff * if moved { 1 } else { fails.min(6) };
                eprintln!("[下載] {}：{}（{} 秒後接著下載）", host_of(&urls[which]), last.trim_end_matches('。'), wait.as_secs());
                nap(wait, job.cancel).await;
            }
        }
    }
}

/// 好幾次下載之間共用的狀態
struct Slot<'a, 'b> {
    /// 檔案的最後位置（Dest::Pick 要等第一個回應才知道）
    dest: &'a mut Option<PathBuf>,
    pick: &'a mut Option<Box<PickFn<'b>>>,
    /// .part 到目前為止最長的長度：這次比它長才算有進展
    best: &'a mut u64,
}

/// 對一個來源下載一次：.part 已經有東西就用 Range 接著下載
async fn once(job: &Job<'_>, url: &str, st: &mut Slot<'_, '_>, progress: &mut Progress<'_>) -> Result<(), Fail> {
    let mut have = st.dest.as_deref().map_or(0, |d| part_len(&part_path(d, job.tag)));
    if have > 0 && job.size == Some(have) {
        progress(have, Some(have));
        return Ok(()); // 上次已經下載完、只差改名
    }
    let mut r = loop {
        let part = st.dest.as_deref().map(|d| part_path(d, job.tag));
        let Some(r) = open(job, url, have, part.as_deref()).await? else {
            progress(have, Some(have));
            return Ok(()); // .part 其實已經是完整的檔
        };
        if st.dest.is_some() {
            break r;
        }
        // 檔名要看第一個回應才知道
        let head = Head {
            disposition: r.headers().get(reqwest::header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()).map(str::to_string),
            last_segment: r.url().path_segments().and_then(|mut s| s.next_back()).map(str::to_string),
            total: r.content_length(),
        };
        let Some(f) = st.pick.as_mut() else { return Err(Fatal("內部錯誤：沒有檔案位置".into())) };
        let d = f(&head).map_err(Fatal)?;
        make_parent(&d).await.map_err(Fatal)?;
        have = part_len(&part_path(&d, job.tag));
        *st.dest = Some(d);
        if have == 0 {
            break r;
        }
        // 以前下載過一半：這個從頭給的回應不要，改用 Range 接著下載
        drop(r);
        if job.size == Some(have) {
            progress(have, Some(have));
            return Ok(());
        }
    };
    let Some(d) = st.dest.as_deref() else { return Err(Fatal("內部錯誤：沒有檔案位置".into())) };
    let part = part_path(d, job.tag);
    *st.best = (*st.best).max(have); // 原本就有的部分不算這次的進展
    let code = r.status().as_u16();
    let cr = r.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(parse_range);
    if code == 206 && cr.and_then(|c| c.span).is_some_and(|s| s.0 != have) {
        let _ = std::fs::remove_file(&part);
        return Err(Retry("伺服器給的續傳位置對不上，重新下載".into()));
    }
    let resume = have > 0 && code == 206;
    let start = if resume { have } else { 0 };
    let server_total =
        if code == 206 { cr.and_then(|c| c.total).or(r.content_length().map(|n| n + start)) } else { r.content_length() };
    if let (Some(t), Some(want)) = (server_total, job.size) {
        if t != want {
            // 伺服器上的檔案跟清單不一樣（換版了或給錯檔）：不下載，免得存成大小不對的檔
            return Err(NoFile(format!("伺服器上的檔案大小（{t}）跟預期的（{want}）不一樣，可能已經換版")));
        }
    }
    let total = server_total.or(job.size);
    progress(start, total);
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resume)
        .truncate(!resume)
        .open(&part)
        .await
        .map_err(|e| Fatal(format!("寫入 {} 失敗：{e}", part.display())))?;
    let (mut done, mut tick) = (start, Instant::now());
    loop {
        if cancelled(job) {
            let _ = f.flush().await;
            return Err(Fatal("已取消".into()));
        }
        let chunk = match tokio::time::timeout(job.policy.stall, r.chunk()).await {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                let _ = f.flush().await;
                return Err(Retry(net::explain(&e, url)));
            }
            Err(_) => {
                let _ = f.flush().await;
                return Err(Retry(format!("{} 秒沒有收到資料（連線卡住了）", job.policy.stall.as_secs())));
            }
        };
        f.write_all(&chunk).await.map_err(|e| Fatal(format!("寫入失敗（硬碟滿了？）：{e}")))?;
        done += chunk.len() as u64;
        if tick.elapsed() > Duration::from_millis(400) {
            tick = Instant::now();
            progress(done, total);
        }
    }
    f.flush().await.map_err(|e| Fatal(format!("寫入失敗：{e}")))?;
    drop(f);
    progress(done, total);
    if done == 0 {
        return Err(Retry("收到的檔案是空的".into()));
    }
    if let Some(t) = total.filter(|&t| t != done) {
        return Err(Retry(format!("大小不符：收到 {done}，應該是 {t}")));
    }
    Ok(())
}

/// 送出請求（have > 0 就帶 Range）並檢查回應：登入頁、網頁、HTTP 錯誤都在這裡擋下。
/// Ok(None)＝.part 其實已經是完整的檔（伺服器回 416，說總大小剛好等於 .part）
async fn open(job: &Job<'_>, url: &str, have: u64, part: Option<&Path>) -> Result<Option<reqwest::Response>, Fail> {
    let host = host_of(url);
    let mut rb = job.http.get(url);
    if let Some(k) = job.bearer.filter(|k| !k.is_empty() && is_civitai(&host)) {
        rb = rb.bearer_auth(k);
    }
    if let Some(c) = job.cookie.filter(|c| !c.is_empty() && is_modelscope(&host)) {
        rb = rb.header(reqwest::header::COOKIE, c);
    }
    if have > 0 {
        rb = rb.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let r = match tokio::time::timeout(job.policy.head, rb.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(Down(net::explain(&e, url))),
        Err(_) => {
            return Err(Down(format!(
                "連線逾時：{host} 在 {} 秒內沒有回應（網路不穩，或梯子沒開、節點太慢）。",
                job.policy.head.as_secs()
            )))
        }
    };
    let code = r.status().as_u16();
    if code == 401 || code == 403 {
        return Err(NoFile(denied_msg(&host, code)));
    }
    if code == 416 && have > 0 {
        // 範圍不對：.part 剛好就是完整的檔 → 完成；否則比檔案還大或換版了，從頭來
        let total = r
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_range)
            .and_then(|c| c.total);
        if total == Some(have) {
            return Ok(None);
        }
        if let Some(p) = part {
            let _ = std::fs::remove_file(p);
        }
        return Err(Retry("續傳位置不對，重新下載".into()));
    }
    if code >= 400 {
        return Err(if code >= 500 || code == 429 { Retry(format!("伺服器忙線中（HTTP {code}）")) } else { NoFile(format!("下載失敗（HTTP {code}）")) });
    }
    if !r.status().is_success() {
        return Err(NoFile(format!("下載失敗（HTTP {code}）")));
    }
    let ctype = r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if r.url().path().contains("/login") || is_page(&ctype, &host) {
        return Err(NoFile(login_msg(&host)));
    }
    Ok(Some(r))
}

// ---------------------------------------------------------------- 小工具
/// 網址的主機名（訊息裡只寫主機：網址可能帶金鑰）
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default()
}

fn is_civitai(host: &str) -> bool {
    host == "civitai.com" || host.ends_with(".civitai.com")
}

fn is_modelscope(host: &str) -> bool {
    host == "modelscope.cn" || host.ends_with(".modelscope.cn")
}

fn is_hf(host: &str) -> bool {
    host == net::HF || host.ends_with(".huggingface.co") || host == net::HF_MIRROR
}

/// 回應是網頁不是檔案。Civitai 連純文字也不是檔案（錯誤、登入頁）
fn is_page(ctype: &str, host: &str) -> bool {
    ctype.starts_with("text/html") || ctype.starts_with("application/xhtml") || (is_civitai(host) && ctype.starts_with("text/"))
}

fn login_msg(host: &str) -> String {
    if is_civitai(host) {
        "Civitai 要求登入才能下載這個模型：請到「設定」填入你的 Civitai API key（有些模型還要先在網站上登入並同意授權），再重試".into()
    } else {
        "下載到的是網頁不是檔案：網址可能要登入、已經失效，或是貼成介紹頁了（要貼檔案的下載連結）".into()
    }
}

fn denied_msg(host: &str, code: u16) -> String {
    if is_civitai(host) {
        login_msg(host)
    } else if is_hf(host) {
        format!("Hugging Face 不讓下載（HTTP {code}）：這個模型要登入、同意授權才能下載，請在瀏覽器下載後用「從電腦加入」")
    } else {
        format!("網站不讓下載（HTTP {code}）：可能要登入，或這個網址不開放下載")
    }
}

/// 把一段訊息收成一句（結尾補「。」），後面可以直接接下一句
fn sentence(s: &str) -> String {
    format!("{}。", s.trim().trim_end_matches('。'))
}

/// 檔名.part，有記號的話是 檔名.記號.part
pub fn part_path(dest: &Path, tag: Option<&str>) -> PathBuf {
    let mut s: OsString = dest.as_os_str().to_os_string();
    if let Some(t) = tag.filter(|t| !t.is_empty()) {
        s.push(".");
        s.push(t);
    }
    s.push(".part");
    PathBuf::from(s)
}

fn part_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// 字串的短記號（FNV-1a）：.part 檔名用，同一個網址每次都一樣
pub fn tag(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", (h ^ (h >> 32)) as u32)
}

/// 殘留的 .part 放多久沒動就清掉（失敗的下載會留著等續傳，不能永遠佔硬碟）
pub const STALE: Duration = Duration::from_secs(14 * 24 * 3600);

/// 清掉 dir 裡很久沒動的殘留 .part。只清自己下載留下的（檔名.記號.part，記號是 tag() 的 8 位十六進位或版本編號），
/// 瀏覽器、別的工具留下的 .part 不碰
pub fn sweep(dir: &Path, older_than: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > older_than);
        if old && ours(&p) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

fn ours(p: &Path) -> bool {
    let Some(stem) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".part")) else { return false };
    let Some((name, tag)) = stem.rsplit_once('.') else { return false };
    !name.is_empty() && !tag.is_empty() && (tag.bytes().all(|b| b.is_ascii_digit()) || (tag.len() == 8 && tag.bytes().all(|b| b.is_ascii_hexdigit())))
}

async fn make_parent(p: &Path) -> Result<(), String> {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => tokio::fs::create_dir_all(d).await.map_err(|e| format!("建立 {} 失敗：{e}", d.display())),
        _ => Ok(()),
    }
}

fn cancelled(job: &Job<'_>) -> bool {
    job.cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

/// 等一段時間，被取消就馬上醒
async fn nap(d: Duration, cancel: Option<&AtomicBool>) {
    let end = Instant::now() + d;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() || cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return;
        }
        tokio::time::sleep(left.min(Duration::from_millis(250))).await;
    }
}

/// Content-Range：`bytes 100-199/200`、`bytes */200`（416 時）、`bytes 100-199/*`
#[derive(Debug, Clone, Copy, PartialEq)]
struct CRange {
    /// （開頭, 結尾）；`*` 時是 None
    span: Option<(u64, u64)>,
    total: Option<u64>,
}

fn parse_range(v: &str) -> Option<CRange> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (span, total) = rest.split_once('/')?;
    let span = span.trim();
    let span = if span == "*" {
        None
    } else {
        let (a, b) = span.split_once('-')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    Some(CRange { span, total: total.trim().parse().ok() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    #[test]
    fn range_headers() {
        assert_eq!(parse_range("bytes 100-199/200"), Some(CRange { span: Some((100, 199)), total: Some(200) }));
        assert_eq!(parse_range("bytes */200"), Some(CRange { span: None, total: Some(200) }));
        assert_eq!(parse_range("bytes 0-9/*"), Some(CRange { span: Some((0, 9)), total: None }));
        assert_eq!(parse_range("items 0-9/10"), None);
        assert_eq!(parse_range("bytes abc"), None);
    }

    #[test]
    fn part_names_and_tags() {
        assert_eq!(part_path(Path::new("a/m.safetensors"), None), PathBuf::from("a/m.safetensors.part"));
        assert_eq!(part_path(Path::new("a/m.safetensors"), Some("1f")), PathBuf::from("a/m.safetensors.1f.part"));
        assert_eq!(part_path(Path::new("a/m.safetensors"), Some("")), PathBuf::from("a/m.safetensors.part"));
        let (a, b) = (tag("https://huggingface.co/x/resolve/main/m.safetensors"), tag("https://civitai.com/api/download/models/1"));
        assert_eq!(a.len(), 8);
        assert_ne!(a, b);
        assert_eq!(a, tag("https://huggingface.co/x/resolve/main/m.safetensors"));
    }

    #[test]
    fn sweep_only_old_parts_of_ours() {
        let dir = tmp("sweep");
        let old = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
        let mk = |name: &str, aged: bool| {
            let p = dir.join(name);
            std::fs::write(&p, b"x").unwrap();
            if aged {
                std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
            }
        };
        mk("a.safetensors.0123abcd.part", true); // 自己的、很舊 → 清掉
        mk("b.safetensors.0123abcd.part", false); // 自己的、剛下載 → 留著（還能續傳）
        mk("c.safetensors.987654.part", true); // Civitai 版本編號 → 清掉
        mk("d.safetensors.part", true); // 沒有記號（瀏覽器、上傳留下的）→ 不碰
        mk("e.safetensors", true); // 正式的檔 → 不碰
        mk("f.zip.part", true); // 別的工具留下的 → 不碰
        sweep(&dir, STALE);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["b.safetensors.0123abcd.part", "d.safetensors.part", "e.safetensors", "f.zip.part"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hosts_and_messages() {
        // 訊息只寫主機名，不帶網址（Civitai 的網址有 token）
        assert_eq!(host_of("https://Civitai.com/api/download/models/1?token=SECRET"), "civitai.com");
        assert_eq!(host_of("not a url"), "");
        assert!(is_civitai("civitai.com") && is_civitai("image.civitai.com") && !is_civitai("notcivitai.com"));
        assert!(is_hf("huggingface.co") && is_hf("hf-mirror.com") && !is_hf("example.com"));
        let m = login_msg("civitai.com");
        assert!(m.contains("Civitai API key") && m.contains("登入"), "{m}");
        assert!(!login_msg("example.com").contains("API key"));
        assert!(denied_msg("huggingface.co", 401).contains("從電腦加入"));
        assert!(denied_msg("civitai.com", 403).contains("Civitai API key"));
        assert!(denied_msg("example.com", 403).contains("403"));
        assert!(is_page("text/html; charset=utf-8", "example.com"));
        assert!(!is_page("text/plain", "example.com"));
        assert!(is_page("text/plain", "civitai.com"));
        assert!(!is_page("application/octet-stream", "civitai.com"));
        assert_eq!(sentence("連線被中斷。 "), "連線被中斷。");
        assert_eq!(sentence("HTTP 503"), "HTTP 503。");
    }

    // ------------------------------------------------ 本機的小伺服器（只在 127.0.0.1，不連網路）
    type Log = Arc<Mutex<Vec<Option<String>>>>;

    #[derive(Clone)]
    enum Plan {
        /// 照 Range 給檔（有 Range 回 206，超出範圍回 416），這個連線正常送完
        Normal,
        /// 一樣，但只送 n 個位元組就斷線
        Cut(usize),
        /// 一樣，但送 n 個位元組之後不再送、也不斷線（卡住）
        Stall(usize),
        /// 不理 Range，一律 200 從頭給
        IgnoreRange,
        /// 固定回應：（狀態碼, Content-Type, 內容）
        Fixed(u16, &'static str, &'static str),
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    /// 每個連線依序用一個 plan；回傳（網址, 每個請求的 Range 標頭）
    fn serve(plans: Vec<Plan>) -> (String, Log) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/dir/model.safetensors", l.local_addr().unwrap().port());
        let log = Arc::new(Mutex::new(Vec::new()));
        let lg = log.clone();
        std::thread::spawn(move || {
            for plan in plans {
                let Ok((s, _)) = l.accept() else { return };
                let lg = lg.clone();
                std::thread::spawn(move || handle(s, &plan, &lg));
            }
        });
        (url, log)
    }

    fn handle(mut s: TcpStream, plan: &Plan, log: &Mutex<Vec<Option<String>>>) {
        let (mut buf, mut tmp) = (Vec::new(), [0u8; 1024]);
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut tmp) {
                Ok(n) if n > 0 => buf.extend_from_slice(&tmp[..n]),
                _ => return,
            }
        }
        let req = String::from_utf8_lossy(&buf).to_string();
        let range = req.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("range").then(|| v.trim().to_string())
        });
        log.lock().unwrap().push(range.clone());
        let all = body();
        let total = all.len();
        let start = range.as_deref().and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse::<usize>().ok());
        let common = "Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"model.safetensors\"\r\nConnection: close\r\n";
        let (head, data, limit, keep): (String, Vec<u8>, usize, bool) = match (plan, start) {
            (Plan::Fixed(code, ctype, text), _) => (
                format!("HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", text.len()),
                text.as_bytes().to_vec(),
                usize::MAX,
                false,
            ),
            (Plan::IgnoreRange, _) | (_, None) => (format!("HTTP/1.1 200 OK\r\n{common}Content-Length: {total}\r\n\r\n"), all, cut(plan), hold(plan)),
            (_, Some(st)) if st >= total => {
                // 416 的內容故意是網頁：續傳位置不對不能被當成「要登入的網頁」
                (format!("HTTP/1.1 416 X\r\nContent-Type: text/html\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"), vec![], usize::MAX, false)
            }
            (_, Some(st)) => (
                format!("HTTP/1.1 206 Partial Content\r\n{common}Content-Range: bytes {st}-{}/{total}\r\nContent-Length: {}\r\n\r\n", total - 1, total - st),
                all[st..].to_vec(),
                cut(plan),
                hold(plan),
            ),
        };
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(&data[..limit.min(data.len())]);
        let _ = s.flush();
        if keep {
            std::thread::sleep(Duration::from_millis(1500));
        }
    }

    fn cut(p: &Plan) -> usize {
        match p {
            Plan::Cut(n) | Plan::Stall(n) => *n,
            _ => usize::MAX,
        }
    }

    fn hold(p: &Plan) -> bool {
        matches!(p, Plan::Stall(_))
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn fast() -> Policy {
        Policy { head: Duration::from_secs(3), stall: Duration::from_millis(400), max_fails: 3, backoff: Duration::from_millis(10) }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anima_dl_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ranges(log: &Mutex<Vec<Option<String>>>) -> Vec<Option<String>> {
        log.lock().unwrap().clone()
    }

    fn start_of(r: &Option<String>) -> usize {
        r.as_deref().and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok()).unwrap_or(0)
    }

    #[tokio::test]
    async fn resumes_after_disconnect() {
        let dir = tmp("cut");
        let dest = dir.join("m.safetensors");
        let (url, log) = serve(vec![Plan::Cut(70_000), Plan::Normal]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        let mut seen = vec![];
        let got = fetch(&job, Dest::Path(dest.clone()), &mut |d, t| seen.push((d, t))).await.unwrap();
        assert_eq!(got, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), body());
        assert!(!part_path(&dest, None).exists());
        let r = ranges(&log);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], None);
        let from = start_of(&r[1]);
        assert!(from > 0 && from <= 70_000, "{r:?}");
        // 進度：最後一筆是全部
        assert_eq!(seen.last(), Some(&(200_000, Some(200_000))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn progress_resets_the_failure_count() {
        // 3 次中斷、max_fails 才 3：每次都有新資料進來，不算「連續沒進展」，照樣下載得完
        let dir = tmp("progress");
        let dest = dir.join("m.safetensors");
        let (url, log) = serve(vec![Plan::Cut(50_000), Plan::Cut(50_000), Plan::Cut(50_000), Plan::Normal]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body());
        assert_eq!(ranges(&log).len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stalled_connection_times_out_then_resumes() {
        let dir = tmp("stall");
        let dest = dir.join("m.safetensors");
        let (url, log) = serve(vec![Plan::Stall(30_000), Plan::Normal]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        let t = Instant::now();
        fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(1400), "卡住的連線要靠逾時放掉，不是等它自己結束");
        assert_eq!(std::fs::read(&dest).unwrap(), body());
        assert!(start_of(&ranges(&log)[1]) > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn gives_up_after_no_progress_failures() {
        let dir = tmp("giveup");
        let dest = dir.join("m.safetensors");
        let (url, log) = serve(vec![Plan::Fixed(503, "text/plain", "busy"); 6]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        let e = fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap_err();
        assert!(e.starts_with("下載一直失敗") && e.contains("503"), "{e}");
        assert!(!e.contains("http"), "訊息不能有網址：{e}");
        assert_eq!(ranges(&log).len(), 3);
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn html_page_is_not_a_file() {
        let dir = tmp("html");
        let dest = dir.join("m.safetensors");
        let (url, log) = serve(vec![Plan::Fixed(200, "text/html; charset=utf-8", "<html>請登入</html>")]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        let e = fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap_err();
        assert!(e.contains("網頁"), "{e}");
        assert!(!e.contains("127.0.0.1"), "{e}");
        assert_eq!(ranges(&log).len(), 1, "不是檔案就不用重試");
        assert!(!dest.exists() && !part_path(&dest, None).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn expected_size_must_match() {
        let dir = tmp("size");
        let dest = dir.join("m.safetensors");
        let (url, _) = serve(vec![Plan::Normal]);
        let http = client();
        let job = Job { size: Some(123), policy: fast(), ..Job::new(&http, &url) };
        let e = fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap_err();
        assert!(e.contains("不一樣"), "{e}");
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pick_name_and_resume_existing_part() {
        // 使用者上次下載到一半就關掉了：這次貼同一個網址，看了檔名發現有 .part，改用 Range 接著下載
        let dir = tmp("pick");
        let dest = dir.join("sub").join("model.safetensors");
        let part = part_path(&dest, Some("t1"));
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, &body()[..1000]).unwrap();
        let (url, log) = serve(vec![Plan::Normal, Plan::Normal]);
        let http = client();
        let job = Job { tag: Some("t1"), policy: fast(), ..Job::new(&http, &url) };
        let (d2, mut head) = (dir.clone(), None);
        let pick = Dest::Pick(Box::new(|h: &Head| {
            head = Some((h.disposition.clone(), h.last_segment.clone(), h.total));
            Ok(d2.join("sub").join("model.safetensors"))
        }));
        let got = fetch(&job, pick, &mut |_, _| {}).await.unwrap();
        assert_eq!(got, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), body());
        assert_eq!(ranges(&log), vec![None, Some("bytes=1000-".to_string())]);
        let (disp, seg, total) = head.unwrap();
        assert!(disp.unwrap().contains("model.safetensors"));
        assert_eq!(seg.as_deref(), Some("model.safetensors"));
        assert_eq!(total, Some(200_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pick_can_refuse() {
        let dir = tmp("refuse");
        let (url, log) = serve(vec![Plan::Normal]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        let pick = Dest::Pick(Box::new(|_: &Head| Err::<PathBuf, String>("不是模型檔".to_string())));
        let e = fetch(&job, pick, &mut |_, _| {}).await.unwrap_err();
        assert_eq!(e, "不是模型檔");
        assert_eq!(ranges(&log).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn complete_part_gets_416_and_is_kept() {
        // .part 其實已經下載完（只差改名）：伺服器回 416（內容是網頁也一樣）→ 直接完成，不重抓
        let dir = tmp("done416");
        let dest = dir.join("m.safetensors");
        std::fs::write(part_path(&dest, None), body()).unwrap();
        let (url, log) = serve(vec![Plan::Normal]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body());
        assert_eq!(ranges(&log), vec![Some("bytes=200000-".to_string())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn server_without_range_restarts_cleanly() {
        let dir = tmp("norange");
        let dest = dir.join("m.safetensors");
        std::fs::write(part_path(&dest, None), vec![0xFFu8; 1000]).unwrap();
        let (url, log) = serve(vec![Plan::IgnoreRange]);
        let http = client();
        let job = Job { policy: fast(), ..Job::new(&http, &url) };
        fetch(&job, Dest::Path(dest.clone()), &mut |_, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body(), "不給續傳的伺服器：舊的 .part 要整個換掉");
        assert_eq!(ranges(&log), vec![Some("bytes=1000-".to_string())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancel_stops_waiting() {
        let dir = tmp("cancel");
        let dest = dir.join("m.safetensors");
        let (url, _) = serve(vec![Plan::Fixed(503, "text/plain", "busy"); 3]);
        let http = client();
        let stop = AtomicBool::new(true);
        let job = Job { cancel: Some(&stop), policy: fast(), ..Job::new(&http, &url) };
        let e = fetch(&job, Dest::Path(dest), &mut |_, _| {}).await.unwrap_err();
        assert_eq!(e, "已取消");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

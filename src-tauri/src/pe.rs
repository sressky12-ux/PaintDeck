//! AI 改寫提示詞（llama-server 跑通用模型：Qwen3.5 Defiant Fable 9B／Qwen3.6 35B；官方 PE 2026-10-30 起拿掉）。
//!
//! 前半（Core 和純函式）對照 ComfyUI 自訂節點的 pe_core.py；後半（Service）對照舊站的 app\pe.py。
//! ComfyUI 的「Anima Prompt Enhance」節點繼續用 Python 的 pe_core.py，兩邊共存：
//!   - 同一個 port（127.0.0.1:8199），同一時間只會有一個 llama-server
//!   - 狀態寫在同一個 server_state.json（pid、哪個模型、GPU 層數、最後使用時間），誰啟動的都看得到、都能關
//!   - 啟動／關閉用同一個 server.lock 互斥：Python 用 msvcrt.locking 鎖第 0 個位元組，這裡用 LockFileEx 鎖同一個範圍
//!   - 同一個 cache.sqlite，cache_key 算法一樣，兩邊的快取互通（有圖的請求除外：縮圖後的 PNG 位元組跟 PIL 不同）
use crate::comfy::Comfy;
use crate::hub::Hub;
use crate::py::{self, dumps, now, squash, str_of, strip, truthy};
use crate::settings;
use base64::Engine;
use bytes::Bytes;
use futures_util::StreamExt;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

pub const HOST: &str = "127.0.0.1";
pub const PORT: u16 = 8199;
pub const CTX: i64 = 16384;
/// MoE 模型 GPU 模式放 CPU 的專家層數（-ncmoe）
pub const MOE_CPU_LAYERS: i64 = 30;
pub const CAPTION_ASK: &str = "Write the caption for this image.";
pub const NOCT_ANIME_HEAD: &str = "An anime illustration of";
/// 缺讀圖用 mmproj 時錯誤訊息裡的按鈕名字（網頁 ui\js\pe.js 認這幾個字附上按鈕）
pub const MMPROJ_BUTTON: &str = "下載讀圖檔";
/// 「下載讀圖檔」要抓的檔：（存到哪, 網址, 大小, sha256）
pub type MmprojGet = (PathBuf, String, u64, Option<String>);

/// 任務 → (系統提示詞檔, 用哪個模型)。改寫、微調、中文摘要都可以用目前已載入的模型，只有讀圖一定要 i2i
pub fn task_info(task: &str) -> Option<(&'static str, &'static str)> {
    Some(match task {
        "t2i" => ("t2i_official.txt", "t2i"), // 文生圖（Qwen-Image 官方 JSON 版，MOSS 用）
        "i2i" => ("i2i_official.txt", "i2i"), // 圖生圖、局部重繪、多圖參考（官方 JSON 版，要讀圖）
        "anima" => ("anima.txt", "t2i"),      // Anima 動漫：tag＋英文句子
        "refine" => ("refine.txt", "t2i"),    // 微調：上一版＋調整指令，只改提到的部分
        "zh" => ("zh_summary.txt", "t2i"),    // 英文結果 → 繁體中文摘要
        "caption" => ("caption.txt", "i2i"),  // 看圖寫英文說明（含畫風）
        "caption_qwen" => ("caption_qwen.txt", "i2i"), // 看圖寫 Qwen-Image 官方格式的長段落（JSON，MOSS 用）
        "adult" => ("", "adult"), // 成人（體位）：通用版去審查模型＋姿勢大彙整，系統提示詞在 poses.rs
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct PeError(pub String);
impl std::fmt::Display for PeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PeError {}

fn err<T>(msg: impl Into<String>) -> Result<T, PeError> {
    Err(PeError(msg.into()))
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("內建的正規表示式")
}

// ================================================================ 純函式：請求組裝
/// 圖片 → OpenAI 格式的 image_url（data URL）
pub fn image_part(png: &[u8]) -> Value {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{b64}")}})
}

/// AI 改寫要載入哪一種模型。2026-10-30 官方 PE 整個拿掉（使用者：「直接砍掉，砍死，再也不見，半強迫的使用Qwen 通用的9B模型」），
/// 只剩通用模型：看圖寫說明、改圖、有參考圖＝通用讀圖模型（adult_vision），其他＝通用模型（adult）。缺檔就照實報錯、教人去下載，不再退回 PE
pub fn rewrite_kind(task: &str, captioning: bool, has_images: bool) -> &'static str {
    if captioning || task == "i2i" || has_images {
        "adult_vision"
    } else {
        "adult"
    }
}

/// 舊版留下的 kind 名稱（server_state.json、舊呼叫端的 t2i／i2i＝以前的官方 PE）一律換成通用模型
fn general_kind(kind: &str) -> &str {
    match kind {
        "t2i" => "adult",
        "i2i" => "adult_vision",
        k => k,
    }
}

/// 官方 PE 改寫模型（pe_t2i_heretic*、pe_i2i_heretic*，含看圖檔、下載到一半的檔）：升級後真刪除
fn is_official_pe(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("pe_t2i_heretic") || n.starts_with("pe_i2i_heretic")
}

/// 程式以前發的 9B（Huihui abliterated，模型＋看圖檔）：換上 Defiant Fable 之後真刪除
fn is_old_huihui(name: &str) -> bool {
    name.to_lowercase().starts_with("huihui-qwen3.5-9b-abliterated")
}

/// 現在預設的 9B（Qwen3.5 Defiant Fable）
fn is_fable(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("qwen3.5") && n.contains("defiant-fable") && !n.contains("mmproj")
}

/// 升級換模型要做的事（Core::retire_plan）
#[derive(Debug, Default)]
pub struct RetirePlan {
    /// 要真刪除的官方 PE
    pub pe: Vec<PathBuf>,
    /// 新 9B 換好才刪的舊 Huihui 9B
    pub old: Vec<PathBuf>,
    /// 已經有 Defiant Fable 9B
    pub has_fable: bool,
    /// 用過 AI 改寫（有 PE 或任何通用模型）：這種人才自動下載新 9B，從沒裝過改寫的不硬塞 6GB
    pub had_llm: bool,
}

/// 明確指定畫面文字時逐字保留；一般用引號強調衣服／風格不算文字內容。
pub fn missing_visible_text(prompt: &str, request: &str) -> Vec<String> {
    static TEXT: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)(?:寫(?:著|上|成)?|标[题语]|標[題語]|字樣|字样|副標|副标|招牌|下面|上面|下方|上方|第一行|第二行|reads?|says?|text|caption)\s*[:：]?\s*[「『“\"]([^」』”\"\n]+)[」』”\"]"#));
    let mut missing = Vec::new();
    for c in TEXT.captures_iter(request) {
        let text = c[1].trim();
        if !text.is_empty() && !prompt.contains(text) && !missing.iter().any(|s| s == text) {
            missing.push(text.to_string());
        }
    }
    missing
}

/// 官方模型卡（Qwen/Qwen-Image-2.1-PE-T2I／I2I）的 max_new_tokens：文生圖 16256、改圖 24000（實際會被 -c 的長度擋住）
pub fn official_max_tokens(kind: &str) -> i64 {
    if kind == "i2i" {
        24000
    } else {
        16256
    }
}

/// 取樣參數照官方模型卡的範例：temperature 1、top_k 20、top_p 0.95，不加重複懲罰
/// （舊版 pe_core.py 照 ComfyUI 範本加了 repeat_penalty 1.05；2026-10-09 使用者決定新版對齊官方）。
/// max_tokens 沒給（或 0）就跟舊版一樣：思考 6144、不思考 2048
pub fn payload(
    messages: &Value,
    temperature: f64,
    seed: i64,
    think: bool,
    max_tokens: Option<i64>,
    stream: bool,
) -> Value {
    let max_tokens = max_tokens
        .filter(|n| *n != 0)
        .unwrap_or(if think { 6144 } else { 2048 });
    json!({
        "messages": messages, "stream": stream,
        "temperature": temperature, "top_k": 20, "top_p": 0.95, "min_p": 0.0, "repeat_penalty": 1.0,
        "seed": seed.rem_euclid(1 << 31), "max_tokens": max_tokens,
        "chat_template_kwargs": {"enable_thinking": think},
        "timings_per_token": false,
    })
}

/// 助手不准用的軟詞（模型拿來代替性行為的委婉字，2026-10-26／28 擂台一直出現）。llama-server 的 logit_bias 字串版
/// 會用模型自己的分詞器換成 token：只放 Qwen 分詞器裡是單一 token 的寫法（前面帶空白）。
/// 會拆成好幾段的（" Intimate"＝" Int"＋"imate"、" tasteful"）不放，不然會連帶擋掉別的字；剩下的交給 poses 的 SOFT_TAGS 事後拿掉
pub const SOFT_WORDS: [&str; 4] = [" intimate", " sensual", " suggestive", " implied"];

pub fn ban_soft_words(mut body: Value) -> Value {
    body["logit_bias"] = json!(SOFT_WORDS.iter().map(|w| json!([w, false])).collect::<Vec<_>>());
    body
}

/// AI 改寫開「思考」時的思考預算。沒給的話，官方 max_tokens 16256 幾乎等於整個 -c 16384，模型可以一路想到上下文用完
/// （2026-10-28 有人回報「用 PE 直接雷霆大思考幾萬字」）。同一天實測 9B 三題（雨中撐傘、乳交、後入）：不限 4333～7702 token，
/// 2048／1024／512 每次都想到上限才停，畫出來跟不限、不思考品質一樣（錯都出在 tag，不在想多久）；
/// 在系統提示詞或使用者訊息叫它「想短一點」9B、35B 都不理。取 1024（約 20 秒）
pub const PE_THINK_BUDGET: i64 = 1024;

/// 模型鬼打牆：最後 LOOP_TAIL 個位元組在前面已經整段出現過兩次以上（2026-10-28 用擂台 356 份真實輸出校準：
/// 只抓到 9B 把整份提示詞重抄到 max_tokens、gemma 一直重貼同一個 JSON 這兩份，正常的長輸出沒有誤判）
const LOOP_TAIL: usize = 200;

pub fn looping(text: &str) -> bool {
    if text.len() < LOOP_TAIL * 3 {
        return false;
    }
    let mut cut = text.len() - LOOP_TAIL;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[..cut].matches(&text[cut..]).nth(1).is_some()
}

/// 思考預算：llama-server 每次請求的 thinking_budget_tokens（2026-10-27 用 build 11505 實測：64 會截斷、0＝不想、
/// 不給就想到 max_tokens 用完都可能沒答案）。budget：None＝不想（body 本來就關掉 enable_thinking）、-1＝不限
pub fn with_think_budget(mut body: Value, budget: Option<i64>) -> Value {
    if let Some(n) = budget.filter(|n| *n >= 0) {
        body["thinking_budget_tokens"] = json!(n);
    }
    body
}

/// SSE 一行的種類
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Reasoning(String),
    Content(String),
    Timings(Value),
    Done,
}

/// 解析一行 SSE（"data: {...}"）；不是資料或沒有內容回傳 None
pub fn sse_delta(line: &str) -> Option<Delta> {
    let data = strip(line.strip_prefix("data:")?);
    if data == "[DONE]" {
        return Some(Delta::Done);
    }
    let j: Value = serde_json::from_str(data).ok()?;
    let j = j.as_object()?;
    // (j.get("choices") or [{}])[0]
    let ch = match j.get("choices") {
        Some(Value::Array(a)) if !a.is_empty() => a[0].clone(),
        Some(c) if truthy(Some(c)) => return None, // 不是清單：Python 這裡會出錯，當成沒內容
        _ => json!({}),
    };
    let d = ch.get("delta").filter(|d| truthy(Some(d)));
    if truthy(j.get("timings")) && d.is_none() {
        return Some(Delta::Timings(j["timings"].clone()));
    }
    let text = |k: &str| {
        d.and_then(|d| d.get(k))
            .filter(|v| truthy(Some(v)))
            .map(str_of)
    };
    if let Some(t) = text("reasoning_content") {
        return Some(Delta::Reasoning(t));
    }
    if let Some(t) = text("content") {
        return Some(Delta::Content(t));
    }
    if truthy(j.get("timings")) {
        return Some(Delta::Timings(j["timings"].clone()));
    }
    None
}

// ================================================================ 純函式：微調（切句、套補丁）
// 微調不讓模型整段重寫（實測 PE 會順手改掉沒提到的句子、又漏改該改的），改成：程式把提示詞切成編號段落，
// 模型只回「第幾段換成什麼、刪哪段、在哪段後面加什麼」，沒點到的段落原封不動。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegKind {
    Tags,
    Prose,
}

impl SegKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SegKind::Tags => "tags",
            SegKind::Prose => "prose",
        }
    }
}

pub type Seg = (String, SegKind);

fn ends_sentence(s: &str) -> bool {
    s.ends_with(['.', '!', '?'])
}

/// 看起來是 tag 串：小寫開頭、至少兩個逗號、不是句號結尾
pub fn is_tags(s: &str) -> bool {
    let s = strip(s);
    match s.chars().next() {
        Some(c) => !c.is_uppercase() && s.matches(',').count() >= 2 && !ends_sentence(s),
        None => false,
    }
}

/// Python 的 _SENT.split()：在「句尾標點 + 空白 + 大寫／引號／括號開頭」的空白處切開。
/// 文字已經是單一空格分隔（squash 過），所以空白一定是一個空格；Rust 的 regex 沒有前後看，手寫。
fn split_sentences(t: &str) -> Vec<&str> {
    let cs: Vec<(usize, char)> = t.char_indices().collect();
    let mut out = vec![];
    let mut start = 0;
    for i in 1..cs.len().saturating_sub(1) {
        let (pos, c) = cs[i];
        if c == ' '
            && matches!(cs[i - 1].1, '.' | '!' | '?' | '。')
            && matches!(cs[i + 1].1, 'A'..='Z' | '"' | '“' | '<' | '(' | '（')
        {
            out.push(&t[start..pos]);
            start = pos + 1;
        }
    }
    out.push(&t[start..]);
    out
}

/// → [(段落文字, tags|prose)]。Anima 格式（tag 串＋英文句子）的 tag 串算一段，其餘一句一段。
pub fn split_segments(text: &str) -> Vec<Seg> {
    let full = squash(text);
    let mut t: &str = &full;
    let mut out = vec![];
    if t.chars().next().is_some_and(|c| !c.is_uppercase()) {
        // tag 串和後面的英文句子之間可能是逗號，也可能是句點（"…, atmospheric depth. A single…"）
        // 對照 re.search(r"[,.]\s+(?=[A-Z])", t)：空白只會是一個空格
        let b = t.as_bytes();
        let m = (0..b.len().saturating_sub(2)).find(|&i| {
            matches!(b[i], b',' | b'.') && b[i + 1] == b' ' && b[i + 2].is_ascii_uppercase()
        });
        let head = m.map(|i| &t[..i]).unwrap_or(t);
        if is_tags(&format!("{head},")) {
            let end = if m.is_some_and(|i| b[i] == b'.') {
                "."
            } else {
                ""
            };
            out.push((
                format!("{}{end}", strip(head).trim_end_matches(',')),
                SegKind::Tags,
            ));
            t = m.map(|i| &t[i + 2..]).unwrap_or("");
        }
    }
    for s in split_sentences(t) {
        let s = strip(s);
        if !s.is_empty() {
            out.push((s.to_string(), SegKind::Prose));
        }
    }
    out
}

pub fn join_segments(segs: &[Seg]) -> String {
    let mut out = String::new();
    for (i, (s, _)) in segs.iter().enumerate() {
        if i > 0 {
            let (p, pk) = &segs[i - 1];
            out.push_str(if *pk == SegKind::Tags && !ends_sentence(p) {
                ", "
            } else {
                " "
            });
        }
        out.push_str(s);
    }
    out
}

/// 把模型回的補丁套到上一版。回傳 (新提示詞, 改了幾段)；補丁格式不對回傳 None
pub fn apply_patch(prev: &str, obj: &Map<String, Value>) -> Option<(String, i64)> {
    let segs = split_segments(prev);
    let empty_map = Value::Object(Map::new());
    let empty_list = Value::Array(vec![]);
    let rep = py::or(obj.get("replace"), Some(&empty_map))
        .unwrap_or(&empty_map)
        .as_object()?;
    let dele = py::or(obj.get("delete"), Some(&empty_list))
        .unwrap_or(&empty_list)
        .as_array()?;
    let ins = py::or(obj.get("insert"), Some(&empty_list))
        .unwrap_or(&empty_list)
        .as_array()?;
    // {int(x) for x in dele if str(x).isdigit()}
    let deleted: Vec<i64> = dele
        .iter()
        .map(str_of)
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|s| s.parse().ok())
        .collect();
    let inserts_after = |after: usize| -> Vec<String> {
        ins.iter()
            .filter_map(Value::as_object)
            .filter(|it| str_of(it.get("after").unwrap_or(&Value::Null)) == after.to_string())
            .filter_map(|it| {
                let t = str_of(py::or(it.get("text"), None).unwrap_or(&json!("")));
                let t = strip(&t).to_string();
                (!t.is_empty()).then_some(t)
            })
            .collect()
    };
    let kind_of = |t: &str| {
        if is_tags(t) {
            SegKind::Tags
        } else {
            SegKind::Prose
        }
    };
    let mut n = 0;
    let mut new: Vec<Seg> = vec![];
    for (i, (s, kind)) in segs.iter().enumerate().map(|(i, x)| (i + 1, x)) {
        for t in inserts_after(i - 1) {
            new.push((t.clone(), kind_of(&t)));
            n += 1;
        }
        if deleted.contains(&(i as i64)) {
            n += 1;
            continue;
        }
        let mut s = s.clone();
        if let Some(Value::String(r)) = rep.get(&i.to_string()) {
            let r = strip(r);
            if !r.is_empty() && r != s {
                s = if *kind == SegKind::Tags {
                    r.trim_end_matches(',').to_string()
                } else {
                    r.to_string()
                };
                n += 1;
            }
        }
        new.push((s, *kind));
    }
    for t in inserts_after(segs.len()) {
        new.push((t.clone(), kind_of(&t)));
        n += 1;
    }
    Some((join_segments(&new), n))
}

static RATIO_SENT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)[^.]*\b(?:aspect ratio|\d{3,4}\s*[x×]\s*\d{3,4}|\d+\s*:\s*\d+\s+(?:ratio|format|frame|aspect))\b[^.]*\.\s*",
    )
});

/// 官方規則：比例、解析度不寫進提示詞（模型偶爾會補一句 The aspect ratio is 2:3.）
pub fn strip_ratio(p: &str) -> String {
    let s = squash(&RATIO_SENT.replace_all(p, ""));
    if s.is_empty() {
        p.to_string()
    } else {
        s
    }
}

// ================================================================ 純函式：解析輸出
static THINK: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<think>.*?</think>"));
static CAPTION_META: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:let me|double-check|constraints?|forbidden|instructions? say|my (?:sentence|caption|draft)|sentence \d|draft:|one sentence caption|wait,|check:)",
    )
});
static CAPTION_HEAD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:caption|description)\s*:\s*"));
static FENCE: LazyLock<Regex> = LazyLock::new(|| re(r"^```(?:json)?\s*|\s*```$"));
static TRAIL_COMMA: LazyLock<Regex> = LazyLock::new(|| re(r",\s*}"));
static TRUNCATED: LazyLock<Regex> = LazyLock::new(|| re(r#"(?s)"rewritten_prompt"\s*:\s*"(.*)"#));
static KEY_STR: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?s)"rewritten_prompt"\s*:\s*"((?:[^"\\]|\\.)*)""#));
static WH: LazyLock<Regex> = LazyLock::new(|| re(r#""wh_ratio"\s*:\s*"([^"]*)""#));
static RATIO_OK: LazyLock<Regex> = LazyLock::new(|| re(r"^\d+(\.\d+)?\s*:\s*\d+(\.\d+)?$"));
static RATIO_OK_TIGHT: LazyLock<Regex> = LazyLock::new(|| re(r"^\d+(\.\d+)?:\d+(\.\d+)?$"));
static NOCT_LEAD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(?:The image is|This is|A|An)\s+(?:a|an)?\s*(?:[\w-]+\s+){0,4}?(?:illustration|photograph|image|scene|portrait|picture|artwork)\s+of\s+",
    )
});

/// 通用模型的回覆：去掉 <think> 和 ``` 圍欄
pub fn clean_reply(raw: &str) -> String {
    let text = THINK.replace_all(raw, "");
    FENCE.replace_all(strip(&text), "").trim().to_string()
}

/// 寫說明時附上反推視窗的 tag：WD tagger 對裸露、性器官、體位很準，讀圖模型自己看常常漏掉或看錯
/// （2026-10-15 實測 48 張成人圖，35B 配新版 caption.txt 寫到 61%，附 tag 後見 HANDOFF）
pub fn caption_hint(tags: &[String]) -> String {
    let tags: Vec<String> = tags
        .iter()
        .map(|t| t.replace("\\(", "(").replace("\\)", ")").trim().to_string())
        .filter(|t| !t.is_empty())
        .take(80)
        .collect();
    if tags.is_empty() {
        return String::new();
    }
    format!(
        "An automatic tagger found these tags in the image. They are usually right about nudity, body parts and sex acts but can be wrong: check each against the image, describe every one you can confirm, and trust the image when they disagree.\nTags: {}",
        tags.join(", ")
    )
}

/// 雲端腦袋：OpenAI 相容的網址（不含 /v1）、金鑰（自己架的走 SSH 通道，沒有）、模型名稱
pub struct Remote {
    pub base: String,
    pub key: Option<String>,
    pub model: String,
}

/// 助手要的 llama-server 種類：adult_vision 加上指定的腦袋（model_files 看 @ 後面選模型，不看設定頁的 pe_general）
pub fn agent_kind(brain: &str) -> String {
    format!("adult_vision@{}", if brain == "35b" { "35b" } else { "9b" })
}

/// 圖檔 → OpenAI 格式的 image_url（JPEG data URL，長邊縮到 side）。給助手看結果用：JPEG 比 PNG 小很多，走雲端通道也快
pub fn image_url_jpeg(path: &Path, side: u32) -> Result<Value, String> {
    let im = image::open(path)
        .map_err(|e| format!("讀不到圖 {}：{e}", file_name(path)))?
        .to_rgb8();
    let (w, h) = (im.width(), im.height());
    let s = side as f64 / w.max(h) as f64;
    let im = if s < 1.0 {
        image::imageops::resize(&im, ((w as f64 * s) as u32).max(28), ((h as f64 * s) as u32).max(28), image::imageops::FilterType::Lanczos3)
    } else {
        im
    };
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
        .encode_image(&im)
        .map_err(|e| format!("圖片轉 JPEG 失敗：{e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(out.into_inner());
    Ok(json!({"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{b64}")}}))
}

/// 微調用的新想法：原本的想法（或上一版提示詞）＋調整指令。模型看得懂任何語言，會自己整合；衝突時以調整為準
pub fn merge_request(source: &str, instruction: &str) -> String {
    format!("{}\n\n調整：{}", strip(source), strip(instruction))
}

/// 解析出來的改寫結果
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub prompt: String,
    pub wh_ratio: String,
    pub ratio_follow: String,
    /// JSON 有小問題、已自動修補
    pub repaired: bool,
    /// 微調：改了幾段
    pub changed: Option<i64>,
}

impl Parsed {
    fn plain(prompt: String, repaired: bool) -> Parsed {
        Parsed {
            prompt,
            wh_ratio: String::new(),
            ratio_follow: String::new(),
            repaired,
            changed: None,
        }
    }

    /// 跟 Python 回傳的 dict 一樣的 JSON（比對用）
    pub fn to_value(&self) -> Value {
        let mut v = json!({"prompt": self.prompt, "wh_ratio": self.wh_ratio, "ratio_follow": self.ratio_follow, "repaired": self.repaired});
        if let Some(n) = self.changed {
            v["changed"] = json!(n);
        }
        v
    }
}

/// t.find("{")、t.rfind("}") 之間的 JSON 物件
fn braces(t: &str) -> (Option<usize>, Option<usize>) {
    (t.find('{'), t.rfind('}'))
}

/// 從模型輸出拿出提示詞與比例；完全救不回來回傳 None。
/// 中文摘要（zh）是純文字，直接回傳；微調（refine）的輸出是補丁，套到 prev 上，另外回傳改了幾段
pub fn parse_output(text: &str, task: &str, prev: &str) -> Option<Parsed> {
    let no_think = THINK.replace_all(text, "");
    let mut t = strip(&no_think).to_string();
    if let Some((_, after)) = t.split_once("</think>") {
        t = strip(after).to_string();
    }
    if task == "zh" {
        return (!t.is_empty()).then(|| Parsed::plain(t, false));
    }
    if task == "caption" {
        // 純文字；偶爾會加「Caption:」或整段包在引號裡
        let c = CAPTION_HEAD.replace(&t, "");
        let c = strip(strip(&c).trim_matches(['"', '“', '”']));
        // PE 模型偶爾把自己的檢查過程（Let me double-check… / Sentence 1…）一起寫出來：當成失敗，讓呼叫端換種子重寫
        if c.chars().count() <= 20 || CAPTION_META.is_match(c) {
            return None;
        }
        return Some(Parsed::plain(squash(c), false));
    }
    let t = FENCE.replace_all(strip(&t), "").to_string();
    let obj_of = |s: &str| {
        serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.as_object().cloned())
    };
    if task == "refine" {
        let obj = match braces(&t) {
            (Some(a), Some(b)) if b > a => obj_of(&t[a..=b]),
            _ => None,
        };
        if let Some(obj) = obj.filter(|o| !truthy(o.get("rewritten_prompt"))) {
            let (p, n) = apply_patch(prev, &obj)?;
            let ratio =
                str_of(py::or(obj.get("wh_ratio"), None).unwrap_or(&json!(""))).replace(' ', "");
            let ok = RATIO_OK_TIGHT.is_match(&ratio);
            return Some(Parsed {
                prompt: strip_ratio(&p),
                wh_ratio: if ok { ratio } else { String::new() },
                ratio_follow: String::new(),
                repaired: false,
                changed: Some(n),
            });
        }
    }
    let mut repaired = false;
    let mut obj: Option<Map<String, Value>> = None;
    match braces(&t) {
        (Some(a), Some(b)) if b > a => {
            let chunk = &t[a..=b];
            obj = obj_of(chunk);
            if obj.is_none() {
                // 常見壞法：字串裡有真的換行、結尾多逗號、單引號
                let fix = TRAIL_COMMA
                    .replace_all(&chunk.replace(['\r', '\n'], " "), "}")
                    .to_string();
                obj = obj_of(&fix);
                repaired = obj.is_some();
            }
        }
        (Some(a), _) => {
            // 被截斷：只有開頭沒有結尾
            if let Some(m) = TRUNCATED.captures(&t[a..]) {
                let p = m[1].trim_end_matches(['"', '}', ' ', '\n']);
                obj = Some(Map::from_iter([("rewritten_prompt".to_string(), json!(p))]));
                repaired = true;
            }
        }
        _ => {}
    }
    if obj.is_none() {
        if let Some(m) = KEY_STR.captures(&t) {
            let raw = &m[1];
            let p = serde_json::from_str::<String>(&format!("\"{raw}\""))
                .unwrap_or_else(|_| raw.to_string());
            let mut o = Map::from_iter([("rewritten_prompt".to_string(), json!(p))]);
            if let Some(r) = WH.captures(&t) {
                o.insert("wh_ratio".into(), json!(&r[1]));
            }
            obj = Some(o);
            repaired = true;
        }
    }
    let Some(obj) = obj else {
        // 沒有 JSON 但看起來是一整段描述（例如用了純文字版系統提示詞）：當成結果，標記為修補
        if t.chars().count() > 80 && !t.contains('{') {
            return Some(Parsed::plain(squash(&t), true));
        }
        return None;
    };
    let p = match py::or(py::or(obj.get("rewritten_prompt"), obj.get("prompt")), None) {
        Some(Value::String(p)) if !strip(p).is_empty() => p.clone(),
        _ => return None,
    };
    // 通用模型偶爾包兩層：rewritten_prompt 裡又是一整個 {"rewritten_prompt": ...}（2026-10-11 實測）
    let p = match strip(&p).starts_with('{').then(|| obj_of(strip(&p))).flatten() {
        Some(inner) => match inner.get("rewritten_prompt") {
            Some(Value::String(q)) if !strip(q).is_empty() => q.clone(),
            _ => p,
        },
        None => p,
    };
    let ratio = str_of(py::or(obj.get("wh_ratio"), None).unwrap_or(&json!("")));
    let ratio = strip(&ratio);
    let ratio = if !ratio.is_empty() && !RATIO_OK.is_match(ratio) {
        ""
    } else {
        ratio
    };
    Some(Parsed {
        prompt: strip_ratio(&squash(&p)),
        wh_ratio: ratio.replace(' ', ""),
        ratio_follow: str_of(py::or(obj.get("ratio_follow"), None).unwrap_or(&json!(""))),
        repaired,
        changed: None,
    })
}

/// Noct Q Anime：提示詞一定要以 An anime illustration of 開頭（沒寫 anime 會畫成照片）
/// 從模型輸出裡挖出第一個 JSON 物件（去掉思考段落和 ``` 圍欄）
pub fn json_obj(text: &str) -> Option<Map<String, Value>> {
    let t = THINK.replace_all(text, "");
    let (a, b) = (t.find('{')?, t.rfind('}')?);
    if b <= a {
        return None;
    }
    let s = TRAIL_COMMA.replace_all(&t[a..=b], "}");
    serde_json::from_str::<Value>(&s).ok()?.as_object().cloned()
}

/// Anima 提示詞的「tag 段」到第一個英文句子為止（tag 全小寫、字數少）
pub(crate) fn anima_split(p: &str) -> (Vec<String>, String) {
    // 括號裡的逗號不切（"heterochromia (if applicable, or …)" 要當成同一個 tag）
    let mut parts: Vec<&str> = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in p.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                parts.push(&p[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&p[start..]);
    let mut tags = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let t = part.trim();
        let head = t.split('(').next().unwrap_or(t);
        let sentence = head.split_whitespace().count() >= 6
            || t.chars().next().is_some_and(|c| c.is_uppercase());
        if sentence {
            return (tags, parts[i..].join(",").trim().to_string());
        }
        if !t.is_empty() {
            tags.push(t.to_string());
        }
    }
    (tags, String::new())
}

/// 圖的實際長寬比換成最接近的官方比例（讀圖模型看到的是縮過的圖，自己報的比例常常不對）
pub fn image_ratio(png: &[u8]) -> Option<String> {
    const RATIOS: [(u32, u32); 10] = [
        (1, 1), (3, 2), (2, 3), (4, 3), (3, 4), (16, 9), (9, 16), (2, 1), (1, 2), (21, 9),
    ];
    let im = image::load_from_memory(png).ok()?;
    let r = (im.width() as f64 / im.height().max(1) as f64).ln();
    RATIOS
        .iter()
        .min_by(|a, b| {
            let d = |&(w, h): &(u32, u32)| ((w as f64 / h as f64).ln() - r).abs();
            d(a).total_cmp(&d(b))
        })
        .map(|(w, h)| format!("{w}:{h}"))
}

/// Anima 改寫有沒有寫英文句子（模型偶爾只給 tag）
pub fn anima_has_sentences(p: &str) -> bool {
    !anima_split(p).1.is_empty()
}

/// 改寫模型寫 Anima tag 偶爾自相矛盾（2026-10-11 實測：2girls 配 solo、nipples 卻標 sensitive、
/// cowboy shot 跟 full body 並存、on back 跟 on stomach 並存），提示詞擋不完，這裡照 Danbooru 慣例收尾
pub fn fix_anima_tags(p: &str) -> String {
    const RATINGS: [&str; 4] = ["safe", "sensitive", "nsfw", "explicit"];
    const EXPLICIT: [&str; 12] = [
        "sex", "vaginal", "anal", "penis", "pussy", "vagina", "penetration", "fellatio",
        "oral", "paizuri", "cum", "handjob",
    ];
    const NSFW: [&str; 6] = ["nipples", "nude", "completely nude", "naked", "topless", "bottomless"];
    const FRAMING: [&str; 5] = ["close-up", "portrait", "upper body", "cowboy shot", "full body"];
    const ANGLE: [&str; 5] = ["from front", "from side", "from behind", "from above", "from below"];
    const LYING: [&str; 2] = ["on back", "on stomach"];
    let (tags, rest) = anima_split(p);
    let has = |set: &[&str]| tags.iter().any(|t| set.contains(&t.to_lowercase().as_str()));
    let need = if has(&EXPLICIT) {
        3
    } else if has(&NSFW) {
        2
    } else {
        0
    };
    let group = tags.iter().any(|t| {
        let t = t.to_lowercase();
        ["2girls", "3girls", "4girls", "multiple girls", "2boys", "multiple boys"]
                .contains(&t.as_str())
    }) || (has(&["1girl"]) && has(&["1boy"]));
    let several_girls = has(&["2girls", "3girls", "4girls", "multiple girls"]);
    let mut seen: Vec<String> = Vec::new();
    let (mut framing, mut angle, mut lying, mut rated) = (false, false, false, false);
    let mut out: Vec<String> = Vec::new();
    for t in &tags {
        // 品質 tag 固定 score_7（模型偶爾自己改成 score_8）
        let t = &if t.starts_with("score_") {
            "score_7".to_string()
        } else {
            t.clone()
        };
        let k = t.to_lowercase();
        if seen.contains(&k) {
            continue;
        }
        // Danbooru tag 是英文；模型偶爾把使用者的中文詞原樣塞進來（"跪姿"）
        if !k.is_ascii() && !k.chars().any(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        // 括號裡寫成一句話的是模型的自言自語（"heterochromia (if applicable, or distinct eye colors)"），
        // 角色名的作品標註（"ganyu (genshin impact)"）只有一兩個字，保留
        if let Some((_, inner)) = k.split_once('(') {
            if inner.split_whitespace().count() >= 3 {
                continue;
            }
        }
        seen.push(k.clone());
        if (k == "solo" && group) || (k == "1girl" && several_girls) {
            continue;
        }
        if let Some(r) = RATINGS.iter().position(|r| *r == k) {
            if rated {
                continue;
            }
            rated = true;
            out.push(RATINGS[r.max(need)].to_string());
            continue;
        }
        // 鏡頭、角度、躺姿各只留第一個
        let mut dup = false;
        for (set, flag) in [
            (&FRAMING[..], &mut framing),
            (&ANGLE[..], &mut angle),
            (&LYING[..], &mut lying),
        ] {
            if set.contains(&k.as_str()) {
                dup |= *flag;
                *flag = true;
            }
        }
        if !dup {
            out.push(t.clone());
        }
    }
    let mut s = out.join(", ");
    if !rest.is_empty() {
        s.push_str(", ");
        s.push_str(&rest);
    }
    s
}

pub fn fix_noct_anime(p: &str) -> String {
    if p.to_lowercase()
        .starts_with(&NOCT_ANIME_HEAD.to_lowercase())
    {
        return p.to_string();
    }
    let rest = match NOCT_LEAD.find(p) {
        Some(m) => p[m.end()..].to_string(),
        None => {
            let mut cs = p.chars();
            match cs.next() {
                Some(c) => c.to_lowercase().chain(cs).collect(),
                None => String::new(),
            }
        }
    };
    format!("{NOCT_ANIME_HEAD} {rest}")
}

pub fn result_note(res: &Parsed) -> &'static str {
    if res.changed == Some(0) {
        return "模型這次沒有改動任何句子，可以換個說法再調整一次。";
    }
    if res.repaired {
        "輸出的 JSON 有小問題，已自動修補。"
    } else {
        ""
    }
}

/// 改寫類型＋生成頁的模型家族 → 要用哪份寫法的外掛 skill（agent::GUIDES 的名稱）；文生圖版不知道家族就不接
pub fn skill_guide(task: &str, family: &str) -> Option<&'static str> {
    match (task, family) {
        ("anima", _) => Some("anima"),
        ("t2i", "moss") => Some("moss"),
        ("t2i", "qwen21") => Some("qwen"),
        ("i2i", _) => Some("qwen_edit"),
        _ => None,
    }
}

/// 在一則訊息後面接一段字（內容是字串，或附圖時的 [圖…, {"type":"text"}] 陣列）
fn append_text(msg: &mut Value, t: &str) {
    match &mut msg["content"] {
        Value::String(u) => {
            u.push_str("\n\n");
            u.push_str(t);
        }
        Value::Array(parts) => match parts.iter().rposition(|p| p["type"] == "text") {
            Some(i) => {
                let old = parts[i]["text"].as_str().unwrap_or("").to_string();
                parts[i]["text"] = json!(format!("{old}\n\n{t}"));
            }
            None => parts.push(json!({"type": "text", "text": t})),
        },
        _ => {}
    }
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// json.dumps(..., sort_keys=True)：物件的鍵照字碼排序（巢狀的也是）
fn sort_keys(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut ks: Vec<&String> = o.keys().collect();
            ks.sort();
            Value::Object(
                ks.into_iter()
                    .map(|k| (k.clone(), sort_keys(&o[k])))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

// ================================================================ Core：檔案位置、llama-server、快取（pe_core.py）
pub struct Core {
    pub dir: PathBuf,
    pub prompts: PathBuf,
    pub models: PathBuf,
    /// 正式服務逐檔搜尋所有 LLM 來源；with_dirs 的隔離測試只搜尋指定位置。
    model_dirs: Vec<PathBuf>,
    search_external: bool,
    pub bin: PathBuf,
    state: PathBuf,
    lock: PathBuf,
    log: PathBuf,
    cache: PathBuf,
    cache_lock: Mutex<()>,
    /// 使用者自訂的系統提示詞（data\pe_prompts\<編號>.txt）；有檔就用它，沒有就用節點 prompts 裡的預設
    pub overrides: Option<PathBuf>,
    /// 程式附帶的 prompts（resources\extras\anima_prompt_enhancer\prompts），節點裡缺檔時用
    pub bundled: Option<PathBuf>,
    /// 程式的 resources 資料夾：缺讀圖用的 mmproj 時，從安裝清單（models_full.json）查下載網址
    pub res: Option<PathBuf>,
    /// 通用模型（成人／體位用）選哪個：auto＝有 35B-A3B 就用它，35b，9b（設定頁的 pe_general）
    general: Mutex<String>,
}

/// llama-server 的狀態檔內容（沒在跑就是空的）
pub type State = Map<String, Value>;

impl Core {
    /// node_dir：ComfyUI\custom_nodes\anima_prompt_enhancer。
    /// 模型放在 ComfyUI\models\LLM（本機是 junction 到 D:\AI\ComfyUI_models\LLM）；可用環境變數 ANIMA_PE_MODELS 改位置
    pub fn new(node_dir: &Path) -> Core {
        let models = std::env::var_os("ANIMA_PE_MODELS")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| node_dir.join("..").join("..").join("models").join("LLM"));
        let mut core = Core::with_dirs(node_dir, &node_dir.join("prompts"), &models);
        core.search_external = std::env::var_os("ANIMA_PE_MODELS").filter(|v| !v.is_empty()).is_none();
        core
    }

    pub fn with_dirs(node_dir: &Path, prompts: &Path, models: &Path) -> Core {
        Core {
            dir: node_dir.to_path_buf(),
            prompts: prompts.to_path_buf(),
            models: models.to_path_buf(),
            model_dirs: vec![],
            search_external: false,
            bin: node_dir.join("bin").join("llama-server.exe"),
            state: node_dir.join("server_state.json"),
            lock: node_dir.join("server.lock"),
            log: node_dir.join("llama-server.log"),
            cache: node_dir.join("cache.sqlite"),
            cache_lock: Mutex::new(()),
            overrides: None,
            bundled: None,
            res: None,
            general: Mutex::new("auto".into()),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{HOST}:{PORT}")
    }

    // ---------------------------------------------------------------- 模型檔
    fn model_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.models.clone()];
        roots.extend(self.model_dirs.iter().cloned());
        // 設定頁新加入的 LLM 路徑立即生效，不必重開程式。
        if self.search_external { roots.extend(crate::model_dirs::roots("LLM")); }
        roots.dedup();
        roots
    }

    /// 只列語言模型檔；不讀圖庫，不追子資料夾連結（避免循環）。
    fn model_candidates(&self) -> Vec<PathBuf> {
        self.llm_files(&|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf")))
    }

    /// LLM 資料夾＋外部模型資料夾（往下 4 層）裡 keep 說要的檔
    fn llm_files(&self, keep: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
        fn collect(dir: &Path, depth: usize, keep: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() && depth < 4 {
                    collect(&e.path(), depth + 1, keep, out);
                } else if e.path().is_file() && keep(&e.path()) {
                    out.push(e.path());
                }
            }
        }
        let mut out = vec![];
        for root in self.model_roots() { collect(&root, 0, keep, &mut out); }
        out
    }

    /// 外掛寫法挑段用的意思座標模型（檔名有 embedding 的 gguf，例如 Qwen3-Embedding-0.6B-Q8_0.gguf；embed.rs 用）
    pub fn embedding_model(&self) -> Option<PathBuf> {
        let mut v: Vec<PathBuf> = self.model_candidates().into_iter().filter(|p| is_embedding(&file_name(p))).collect();
        v.sort();
        v.into_iter().next()
    }

    /// (模型路徑, mmproj 路徑)；找不到就是錯誤
    /// _quant：以前挑官方 PE 的量化版本用，PE 拿掉之後沒作用了（留著免得動到每個呼叫端）
    pub fn model_files(
        &self,
        kind: &str,
        _quant: Option<&str>,
    ) -> Result<(PathBuf, Option<PathBuf>), PeError> {
        let dir = self.models.display();
        // 助手：adult_vision@9b／adult_vision@35b 指定腦袋
        let (kind, brain) = match kind.split_once('@') {
            Some((k, b)) => (k, Some(b)),
            None => (kind, None),
        };
        let kind = general_kind(kind);
        if kind == "adult" || kind == "adult_vision" {
            // 通用版去審查模型：Qwen3.6-35B-A3B heretic 或 Qwen3.5-9B abliterated。
            // adult 只寫文字、不載 mmproj；adult_vision（導演台看角色圖）同一個模型加上它的 mmproj
            let (mmprojs, files) = self.general_files();
            let pick = |want: &dyn Fn(&str) -> bool| files.iter().find(|p| want(&file_name(p).to_lowercase())).cloned();
            let pref = self.general.lock().map(|g| g.clone()).unwrap_or_default();
            // 2026-10-29 使用者：「讓別人自己決定要用什麼模型」：設定頁可以直接指定 LLM 資料夾裡的某個檔。
            // 那個檔不見了（搬走、刪掉）就照自動挑，不讓改寫整個壞掉
            let chosen = (!matches!(pref.as_str(), "" | "auto" | "9b" | "35b"))
                .then(|| files.iter().find(|p| file_name(p).eq_ignore_ascii_case(&pref)).cloned())
                .flatten();
            let big = |p: &PathBuf| is_big_brain(&file_name(p));
            let moe = chosen.clone().filter(big).or_else(|| pick(&|n| n.contains("qwen3.6") && is_moe(n)));
            // 9B：2026-10-29 擂台贏的 DavidAU Defiant Fable 有就優先，再來 Huihui abliterated，再來任何一顆 Qwen3.5（別人做的去審查版）
            let q9 = chosen.clone().filter(|p| !big(p))
                .or_else(|| pick(&|n| n.contains("qwen3.5") && n.contains("defiant-fable")))
                .or_else(|| pick(&|n| n.contains("qwen3.5") && n.contains("abliterated")))
                .or_else(|| pick(&|n| n.contains("qwen3.5") && !is_big_brain(n)));
            let found = match (brain, &chosen) {
                // 助手指定 9b／35b 腦袋：照那一類挑（指定的檔剛好是那一類就用它）
                (Some(b), _) => if b == "9b" { q9.or(moe) } else { moe.or(q9) },
                (None, Some(c)) => Some(c.clone()),
                (None, None) => if pref == "9b" { q9.or(moe) } else { moe.or(q9) },
            };
            let model = found.ok_or_else(|| {
                PeError(format!(
                    "找不到 AI 改寫用的通用模型（Qwen3.5-9B-The-Defiant-Fable*.gguf 或 Qwen3.6-35B-A3B*.gguf，放在 {dir}）。到「安裝與修復」勾「AI 改寫、生圖助手」會下載 9B；以前的官方 PE 改寫模型已經不再支援"
                ))
            })?;
            if kind == "adult" {
                return Ok((model, None));
            }
            return match pair_mmproj(&model, &mmprojs) {
                Some(m) => Ok((model, Some(m))),
                None => err(self.mmproj_missing_msg(&model, &mmproj_family(&model))),
            };
        }
        err(format!("不認得的模型種類：{kind}"))
    }

    /// 升級換模型要做的事：站找得到的（LLM 資料夾＋外部模型資料夾）官方 PE、舊 Huihui 9B，有沒有新 9B、有沒有用過改寫
    /// 要刪的連下載到一半的暫存檔（<檔名>.verify、.verify.part）一起算；有沒有新 9B 只看下載完的 .gguf
    pub fn retire_plan(&self) -> RetirePlan {
        let all = self.llm_files(&|p| is_official_pe(&file_name(p)) || is_old_huihui(&file_name(p)));
        let names = |f: fn(&str) -> bool| all.iter().filter(|p| f(&file_name(p))).cloned().collect::<Vec<_>>();
        let pe = names(is_official_pe);
        let had_llm = !pe.is_empty() || !self.general_files().1.is_empty();
        let has_fable = self.model_candidates().iter().any(|p| is_fable(&file_name(p)));
        RetirePlan { has_fable, old: names(is_old_huihui), pe, had_llm }
    }

    /// 站找得到這個檔名的語言模型嗎（不分大小寫；LLM 資料夾＋外部模型資料夾）
    pub fn has_file(&self, name: &str) -> bool {
        self.model_candidates().iter().any(|p| file_name(p).eq_ignore_ascii_case(name))
    }

    /// 缺讀圖用的 mmproj：講清楚去哪下載、放哪裡、檔名怎麼取。安裝清單裡有這個系列的（9B）就給網址，
    /// 網頁看到 MMPROJ_BUTTON 這幾個字會附上一鍵下載的按鈕（/api/pe/mmproj）
    fn mmproj_missing_msg(&self, model: &Path, fam: &str) -> String {
        // 模型資料夾是 custom_nodes\anima_prompt_enhancer\..\..\models\LLM 這種寫法：把 .. 消掉，給人看的路徑才看得懂
        let mut tidy = PathBuf::new();
        for c in self.models.components() {
            match c {
                std::path::Component::ParentDir if tidy.file_name().is_some() => {
                    tidy.pop();
                }
                std::path::Component::CurDir => {}
                c => tidy.push(c),
            }
        }
        let dir = tidy.display().to_string().replace('/', "\\");
        let m = file_name(model);
        match self.res.as_deref().and_then(|r| crate::setup::mmproj_source(r, fam)) {
            Some((name, size, url, _)) => format!(
                "找不到 {m} 讀圖用的 mmproj 檔（生圖助手、導演台要看圖就得有它）。按下面的「{MMPROJ_BUTTON}」會自動下載（約 {} MB）。\
                 也可以自己下載：{url} （大陸連不上就把網址裡的 huggingface.co 換成 hf-mirror.com），下載好放進 {dir} ，檔名保持「{name}」",
                size / 1_000_000
            ),
            None => format!(
                "找不到 {m} 讀圖用的 mmproj 檔（生圖助手、導演台要看圖就得有它）。請到下載這個模型的 Hugging Face 頁面，\
                 在同一個 repo 裡找檔名有 mmproj 的檔（通常叫 xxx.mmproj-f16.gguf），放進 {dir} ；檔名要同時有「mmproj」和「{fam}」"
            ),
        }
    }

    /// 「下載讀圖檔」要抓什麼：Ok(None)＝已經有了；Ok(Some(（存到哪, 網址, 大小, sha256）))；Err＝清單裡沒有來源或根本沒有模型
    pub fn mmproj_download(&self, brain: Option<&str>) -> Result<Option<MmprojGet>, PeError> {
        let kind = match brain {
            Some(b) => format!("adult_vision@{b}"),
            None => "adult_vision".into(),
        };
        let model = match self.model_files(&kind, None) {
            Ok(_) => return Ok(None),
            Err(_) => self.model_files(&kind.replace("adult_vision", "adult"), None)?.0,
        };
        // 安裝清單只有 Qwen3.5／3.6 的看圖檔；使用者自己指定的別家模型沒有來源，照 mmproj_missing_msg 教他去哪找
        let fam = mmproj_family(&model);
        match self.res.as_deref().filter(|_| fam.starts_with("qwen3.")).and_then(|r| crate::setup::mmproj_source(r, &fam)) {
            Some((name, size, url, sha)) => Ok(Some((model.parent().unwrap_or(&self.models).join(name), url, size, sha))),
            None => err(self.mmproj_missing_msg(&model, &fam)),
        }
    }

    /// (看圖檔, 能對話的通用模型)：LLM 資料夾裡的 gguf，扣掉挑段用的 embedding 和官方 PE（pe_*，已經不支援），照路徑排序
    fn general_files(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let (mmprojs, mut files): (Vec<PathBuf>, Vec<PathBuf>) = self
            .model_candidates()
            .into_iter()
            .partition(|p| file_name(p).to_lowercase().contains("mmproj"));
        files.retain(|p| {
            let n = file_name(p);
            !is_embedding(&n) && !n.to_lowercase().starts_with("pe_")
        });
        files.sort();
        (mmprojs, files)
    }

    /// 設定頁「通用模型」選單可以指定的檔：名字、大小、配不配得到看圖檔、是不是 Qwen 系列（別家不保證寫得出生圖站要的 JSON）
    pub fn general_choices(&self) -> Vec<Value> {
        let (mmprojs, files) = self.general_files();
        files
            .iter()
            .map(|p| {
                let n = file_name(p);
                json!({"name": n, "size": std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
                       "vision": pair_mmproj(p, &mmprojs).is_some(), "qwen": n.to_lowercase().contains("qwen3"), "big": is_big_brain(&n)})
            })
            .collect()
    }

    /// 設定頁「通用模型」：auto／35b／9b
    pub fn set_general(&self, pref: &str) {
        if let Ok(mut g) = self.general.lock() {
            *g = pref.to_string();
        }
    }

    pub fn available(&self) -> Value {
        let name = |k: &str| {
            self.model_files(k, None)
                .map(|(p, _)| json!(file_name(&p)))
                .unwrap_or(Value::Null)
        };
        json!({"adult": name("adult"), "vision": name("adult_vision"), "server": self.bin.exists()})
    }

    pub fn system_prompt(&self, task: &str) -> Result<String, PeError> {
        if let Some(t) = self.override_text(task) {
            return Ok(t);
        }
        self.default_prompt(task)
    }

    /// 節點 prompts 資料夾裡的預設系統提示詞（ComfyUI 節點也用同一份，網頁上改的不會動到它）
    pub fn default_prompt(&self, task: &str) -> Result<String, PeError> {
        let (file, _) =
            task_info(task).ok_or_else(|| PeError(format!("未知的改寫類型：{task}")))?;
        // 程式附帶的那份優先，沒有才讀節點資料夾。2026-10-29 盤點：安裝程式不覆蓋已經有的節點，
        // 已安裝的人一直讀 10-11 的舊 anima.txt／t2i_official.txt，之後改的規則（體位、paizuri…）都沒生效。
        // 使用者自己改的走 override_text（data\pe_prompts），不靠改節點檔
        let p = self.bundled.as_ref().map(|d| d.join(file)).filter(|b| b.is_file()).unwrap_or_else(|| self.prompts.join(file));
        read_prompt(&p).map_err(|e| PeError(format!("讀不到系統提示詞 {}：{e}", p.display())))
    }

    /// 在用的預設提示詞跟 ComfyUI 節點資料夾那份不一樣時回傳它（快取鍵要帶它的雜湊，不能跟節點的舊結果共用）
    pub fn default_differs_from_node(&self, task: &str) -> Option<String> {
        let (file, _) = task_info(task)?;
        let used = self.default_prompt(task).ok()?;
        (read_prompt(&self.prompts.join(file)).ok().as_deref() != Some(used.as_str())).then_some(used)
    }

    /// 使用者自訂的版本（沒有就是 None）
    pub fn override_text(&self, id: &str) -> Option<String> {
        let p = self.overrides.as_ref()?.join(format!("{id}.txt"));
        std::fs::read_to_string(p)
            .ok()
            .map(|s| strip(&s).replace("\r\n", "\n"))
            .filter(|s| !s.is_empty())
    }

    // ---------------------------------------------------------------- 請求
    /// 組出 chat messages。images：PNG 位元組（只有讀圖的任務用）
    #[allow(clippy::too_many_arguments)]
    pub fn build_messages(
        &self,
        task: &str,
        idea: &str,
        images: &[Vec<u8>],
        prev: &str,
        instruction: &str,
        noct_anime: bool,
        ratio: &str,
    ) -> Result<Value, PeError> {
        let sys_p = self.system_prompt(task)?;
        let content = match task {
            "refine" => {
                let segs: Vec<String> = split_segments(prev)
                    .iter()
                    .enumerate()
                    .map(|(i, (s, _))| format!("[{}] {s}", i + 1))
                    .collect();
                let ratio = if ratio.is_empty() { "unknown" } else { ratio };
                json!(format!(
                    "SEGMENTS:\n{}\n\nCURRENT RATIO: {ratio}\n\nCHANGE REQUEST:\n{}",
                    segs.join("\n"),
                    strip(instruction)
                ))
            }
            "zh" => json!(strip(idea)),
            "caption" | "caption_qwen" => {
                let mut parts: Vec<Value> = images.iter().take(1).map(|b| image_part(b)).collect();
                let idea = strip(idea);
                let ask = match images.first().and_then(|b| image_ratio(b)) {
                    // 模型看到的是縮過的圖，方向常常說錯：直接告訴它
                    Some(r) if task == "caption_qwen" && idea.is_empty() => {
                        let o = match r.as_str() {
                            "1:1" => "square",
                            "2:3" | "3:4" | "9:16" | "1:2" => "vertical",
                            _ => "wide",
                        };
                        format!("Write the description for this image. It is a {o} image, aspect ratio {r}.")
                    }
                    _ if idea.is_empty() => CAPTION_ASK.to_string(),
                    _ => idea.to_string(),
                };
                // 反推視窗送來的 tag 提示（caption_hint）放在 instruction（寫說明用不到它）
                let ask = if instruction.is_empty() { ask } else { format!("{ask}\n\n{}", strip(instruction)) };
                parts.push(json!({"type": "text", "text": ask}));
                Value::Array(parts)
            }
            "i2i" => {
                let mut parts = vec![];
                if images.len() > 1 {
                    for (i, b) in images.iter().enumerate() {
                        parts.push(json!({"type": "text", "text": format!("<image{}>", i + 1)}));
                        parts.push(image_part(b));
                    }
                } else {
                    parts.extend(images.iter().map(|b| image_part(b)));
                }
                let mut text = strip(idea).to_string();
                if !prev.is_empty() && !instruction.is_empty() {
                    text = format!(
                        "{text}\n\nA previous rewrite of this instruction was:\n{}\nRewrite it again with only this adjustment, keeping everything else the same: {}",
                        strip(prev),
                        strip(instruction)
                    );
                }
                parts.push(json!({"type": "text", "text": text}));
                Value::Array(parts)
            }
            _ => {
                let mut text = strip(idea).to_string();
                if noct_anime {
                    text.push_str(&format!(
                        "\n\n(The image is an anime illustration. The description must begin with the exact words \"{NOCT_ANIME_HEAD}\".)"
                    ));
                }
                json!(text)
            }
        };
        Ok(json!([{"role": "system", "content": sys_p}, {"role": "user", "content": content}]))
    }

    // ---------------------------------------------------------------- 快取
    /// 輸入＋任務＋參數＋seed＋系統提示詞內容＋模型檔名 → 雜湊（跟 Python 算出一樣的值）。圖片用內容雜湊
    pub fn cache_key(&self, parts: &[(&str, Value)], images: Option<&[Vec<u8>]>) -> String {
        let mut norm = Map::new();
        for (k, v) in parts {
            norm.insert(k.to_string(), v.clone());
        }
        if let Some(imgs) = images {
            norm.insert(
                "images".into(),
                json!(imgs.iter().map(|b| sha256_hex(b)).collect::<Vec<_>>()),
            );
        }
        if let Some(task) = norm
            .get("task")
            .and_then(Value::as_str)
            .filter(|t| task_info(t).is_some())
        {
            // 讀不到系統提示詞時 Python 會出錯；這裡當成空字串，接下來組請求時還是會回報錯誤
            let sys_p = self.system_prompt(task).unwrap_or_default();
            norm.insert(
                "_sys".into(),
                json!(sha256_hex(sys_p.as_bytes())[..16].to_string()),
            );
        }
        sha256_hex(dumps(&sort_keys(&Value::Object(norm))).as_bytes())
    }

    fn cache_con(&self) -> rusqlite::Result<rusqlite::Connection> {
        let con = rusqlite::Connection::open(&self.cache)?;
        con.busy_timeout(Duration::from_secs(10))?;
        con.execute(
            "CREATE TABLE IF NOT EXISTS cache (key TEXT PRIMARY KEY, value TEXT, created REAL)",
            [],
        )?;
        Ok(con)
    }

    pub fn clear_cache(&self) -> Result<u64, String> {
        let _guard = self.cache_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.cache.exists() { return Ok(0); }
        let size = || [self.cache.clone(), PathBuf::from(format!("{}-wal", self.cache.display())), PathBuf::from(format!("{}-shm", self.cache.display()))]
            .iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum::<u64>();
        let before = size();
        let con = self.cache_con().map_err(|e| format!("讀取改寫快取失敗：{e}"))?;
        con.execute("DELETE FROM cache", []).map_err(|e| format!("清理改寫快取失敗：{e}"))?;
        con.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| format!("壓縮改寫快取失敗：{e}"))?;
        drop(con);
        Ok(before.saturating_sub(size()))
    }

    pub fn cache_get(&self, key: &str) -> Option<Map<String, Value>> {
        let _g = self.cache_lock.lock().unwrap_or_else(|e| e.into_inner());
        let con = self.cache_con().ok()?;
        let text: String = con
            .query_row("SELECT value FROM cache WHERE key=?", [key], |r| r.get(0))
            .ok()?;
        serde_json::from_str::<Value>(&text)
            .ok()?
            .as_object()
            .cloned()
    }

    pub fn cache_put(&self, key: &str, value: &Value) {
        let _g = self.cache_lock.lock().unwrap_or_else(|e| e.into_inner());
        let r = self.cache_con().and_then(|con| {
            con.execute(
                "INSERT OR REPLACE INTO cache VALUES(?,?,?)",
                rusqlite::params![key, dumps(value), now()],
            )
        });
        if let Err(e) = r {
            eprintln!("AI 改寫快取寫入失敗：{e}");
        }
    }

    // ---------------------------------------------------------------- 程序與狀態
    pub fn read_state(&self) -> State {
        let Ok(text) = std::fs::read_to_string(&self.state) else {
            return State::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(s)) if s.get("pid").and_then(py::int).is_some_and(win::pid_alive) => s,
            _ => State::new(),
        }
    }

    fn write_state(&self, s: &State) {
        let tmp = self.state.with_extension("json.tmp");
        let r = std::fs::write(&tmp, dumps(&Value::Object(s.clone())))
            .and_then(|_| std::fs::rename(&tmp, &self.state));
        if let Err(e) = r {
            self.log_line(&format!("寫狀態檔失敗：{e}"));
        }
    }

    /// 記錄最後使用時間（閒置自動關閉用）
    pub fn touch(&self) {
        let mut s = self.read_state();
        if !s.is_empty() {
            s.insert("last_used".into(), json!(now()));
            self.write_state(&s);
        }
    }

    pub fn set_busy(&self, n: i64) {
        let mut s = self.read_state();
        if !s.is_empty() {
            s.insert("busy".into(), json!(n.max(0)));
            s.insert("last_used".into(), json!(now()));
            self.write_state(&s);
        }
    }

    pub fn health(&self, timeout: Duration) -> bool {
        health(timeout)
    }

    fn log_line(&self, msg: &str) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.log) {
            let _ = writeln!(
                f,
                "[anima-pe {}] {msg}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            );
        }
    }

    /// log 最後 n 行（llama-server 的輸出也在裡面）
    pub fn log_tail(&self, n: usize) -> String {
        let Ok(mut f) = File::open(&self.log) else {
            return String::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)));
        let mut buf = vec![];
        if f.read_to_end(&mut buf).is_err() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&buf);
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        lines[lines.len().saturating_sub(n)..].concat()
    }

    /// 關掉 llama-server（不管是誰開的）。回傳是否真的有關到東西
    pub fn stop(&self, reason: &str) -> Result<bool, PeError> {
        let _g = win::FileLock::acquire(&self.lock, Duration::from_secs(180))?;
        Ok(self.stop_locked(reason))
    }

    fn stop_locked(&self, reason: &str) -> bool {
        let s = self.read_state();
        crate::diagnostics::event("llm_stop_requested", json!({"reason":reason,"state":s}));
        let mut pids: Vec<i64> = s.get("pid").and_then(py::int).into_iter().collect();
        if let Some(orphan) = win::port_owner(PORT).filter(|o| !pids.contains(o)) {
            pids.push(orphan);
        }
        for &pid in &pids {
            if pid != 0 {
                win::kill(pid);
            }
        }
        let _ = std::fs::remove_file(&self.state);
        if !pids.is_empty() {
            self.log_line(&format!("stop pid={} {reason}", py::repr(&json!(pids))));
        }
        !pids.is_empty()
    }

    /// 確保 llama-server 跑著對的模型（kind＝t2i／i2i）和對的 GPU 層數。已經在跑就直接用；不對就重開。
    /// before_start(ngl, 第幾次)：真的要啟動前呼叫（生圖站用來叫 ComfyUI 釋放顯存、等它放完）。
    /// 第一次因為顯存不夠失敗，會用「第 1 次」再叫一次（等久一點）重試一次。回傳 (是否新啟動, 載入秒數)
    pub fn ensure(
        &self,
        kind: &str,
        ngl: i64,
        before_start: impl Fn(i64, u32),
        timeout: Duration,
    ) -> Result<(bool, f64), PeError> {
        let (model, mmproj) = self.model_files(kind, None)?;
        let model_name = file_name(&model);
        crate::diagnostics::snapshot("llm_model_requested", &json!({"kind":kind,"gpu_layers_requested":ngl,"model":model,"mmproj":mmproj,"loaded_confirmed":false}));
        let want = json!({"kind": kind, "ngl": ngl, "model": model_name});
        let _g = win::FileLock::acquire(&self.lock, Duration::from_secs(180))?;
        let mut s = self.read_state();
        let same = !s.is_empty()
            && want
                .as_object()
                .into_iter()
                .flatten()
                .all(|(k, v)| s.get(k) == Some(v));
        if same && health(Duration::from_millis(1500)) {
            s.insert("last_used".into(), json!(now()));
            self.write_state(&s);
            crate::diagnostics::event("llm_loaded_confirmed", json!({"reused":true,"state":s}));
            return Ok((false, 0.0));
        }
        let was = if s.is_empty() {
            "沒在跑".to_string()
        } else {
            py::repr(&Value::Object(s.clone()))
        };
        self.stop_locked(&format!("重開：要 {}，原本 {was}", py::repr(&want)));
        before_start(ngl, 0);
        let mut args: Vec<String> = vec![
            "-m".into(),
            model.to_string_lossy().into(),
            "--host".into(),
            HOST.into(),
            "--port".into(),
            PORT.to_string(),
            "-c".into(),
            CTX.to_string(),
            "-ngl".into(),
            ngl.to_string(),
            "--jinja".into(),
            "--no-webui".into(),
            "-np".into(),
            "1".into(),
            "--reasoning-format".into(),
            "deepseek".into(),
        ];
        if let Some(mm) = &mmproj {
            args.extend(["--mmproj".into(), mm.to_string_lossy().into()]);
            if ngl == 0 {
                args.push("--no-mmproj-offload".into());
            }
        }
        if ngl == 0 {
            args.extend(["--device".into(), "none".into()]); // 真正的 CPU 模式：連提示詞處理都不碰顯卡
        } else if is_moe(&model_name) {
            // MoE（35B-A3B）：專家權重放記憶體、其餘放顯卡。GPU 模式後 30 層的專家留在 CPU（3070 實測 43 token／秒、顯存 7.6GB），
            // 部分模式全部 40 層專家都放 CPU（顯存約 3GB）。-ngl 一律 99（層數交給 -ncmoe 決定）
            let i = args.iter().position(|a| a == "-ngl").expect("-ngl") + 1;
            args[i] = "99".into();
            let cpu_layers = if ngl >= 99 { MOE_CPU_LAYERS } else { 99 };
            args.extend(
                [
                    "-ncmoe",
                    &cpu_layers.to_string(),
                    "-ctk",
                    "q8_0",
                    "-ctv",
                    "q8_0",
                    "-fa",
                    "on",
                ]
                .map(String::from),
            );
        }
        let shown: Vec<String> = std::iter::once(file_name(&self.bin))
            .chain(args.iter().map(|a| {
                if Path::new(a).is_absolute() {
                    file_name(Path::new(a))
                } else {
                    a.clone()
                }
            }))
            .collect();
        if !self.bin.exists() {
            self.log_line(&format!("start {}", shown.join(" ")));
            return err(format!(
                "找不到 llama-server.exe（{}）：到「安裝與修復」勾「AI 改寫、生圖助手」安裝",
                self.bin.display()
            ));
        }
        // 顯存不夠（通常是 ComfyUI 還沒放完）：再叫一次釋放、等久一點，重試一次
        let (pid, load) = match self.launch(&args, &shown, kind, timeout) {
            Err(e) if ngl > 0 && vram_short(&self.log_tail(200)) => {
                self.log_line("顯存不夠，再叫 ComfyUI 釋放一次、等久一點重試");
                crate::diagnostics::event("llm_vram_retry", json!({"kind":kind,"model":model_name,"first_error":e.0}));
                before_start(ngl, 1);
                self.launch(&args, &shown, kind, timeout).map_err(|e| {
                    if vram_short(&self.log_tail(200)) {
                        PeError(format!(
                            "顯示卡記憶體不夠，載不下 {model_name}（已經叫 ComfyUI 釋放顯存、等過兩次）。\
                             可以到「設定 → AI 改寫提示詞 → 執行方式」改成「部分放顯卡」或「CPU」，或關掉其他吃顯卡的程式再試。\n{}",
                            e.0
                        ))
                    } else {
                        e
                    }
                })?
            }
            r => r?,
        };
        let mut st = want.as_object().cloned().unwrap_or_default();
        st.insert("pid".into(), json!(pid));
        st.insert("started".into(), json!(now()));
        st.insert("last_used".into(), json!(now()));
        st.insert("busy".into(), json!(0));
        st.insert("load_seconds".into(), json!(py::round(load, 2)));
        self.write_state(&st);
        self.log_line(&format!("ready pid={pid} load={load:.1}s"));
        crate::diagnostics::event("llm_loaded_confirmed", json!({"pid":pid,"model":model_name,"kind":kind,"load_seconds":load}));
        Ok((true, load))
    }

    /// 開一次 llama-server、等到 /health 通。回傳 (pid, 載入秒數)；中途結束或逾時是錯誤（log 最後幾行附在訊息裡）
    fn launch(&self, args: &[String], shown: &[String], kind: &str, timeout: Duration) -> Result<(i64, f64), PeError> {
        self.log_line(&format!("start {}", shown.join(" ")));
        crate::diagnostics::snapshot("llm_launch_attempt", &json!({"binary":self.bin,"arguments":args,"kind":kind,"loaded_confirmed":false}));
        let logf = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|e| PeError(format!("開不了 log 檔：{e}")))?;
        let logf2 = logf
            .try_clone()
            .map_err(|e| PeError(format!("開不了 log 檔：{e}")))?;
        let t0 = Instant::now();
        let mut cmd = std::process::Command::new(&self.bin);
        cmd.args(args)
            .stdout(logf)
            .stderr(logf2)
            .stdin(std::process::Stdio::null())
            .current_dir(self.bin.parent().unwrap_or(&self.dir));
        win::no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| PeError(format!("llama-server 啟動失敗：{e}")))?;
        let pid = child.id() as i64;
        crate::diagnostics::event("llm_process_started", json!({"pid":pid}));
        if !win::no_throttle(pid) {
            self.log_line("關閉節能限速失敗");
        }
        loop {
            if let Ok(Some(code)) = child.try_wait() {
                crate::diagnostics::event("llm_process_exited", json!({"pid":pid,"exit_code":code.code(),"loaded_confirmed":false}));
                crate::diagnostics::snapshot("llm_startup_error_log", &json!({"scope":"last 64 KiB of shared llama log; may include earlier attempts","text":self.log_tail(2000)}));
                let code = code
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into());
                return err(format!(
                    "llama-server 啟動失敗（結束碼 {code}）。log 最後幾行：\n{}",
                    self.log_tail(15)
                ));
            }
            if health(Duration::from_secs(1)) {
                break;
            }
            if t0.elapsed() > timeout {
                win::kill(pid);
                crate::diagnostics::snapshot("llm_startup_timeout_log", &json!({"scope":"last 64 KiB of shared llama log","text":self.log_tail(2000)}));
                return err(format!(
                    "llama-server {} 秒內沒有載入完成，已關閉。log 最後幾行：\n{}",
                    timeout.as_secs(),
                    self.log_tail(15)
                ));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok((pid, t0.elapsed().as_secs_f64()))
    }
}

/// 跨程序檔案鎖（測試用；平常由 Core::ensure／stop 使用）
pub use win::FileLock;

/// 檔名看起來是 MoE（Qwen3.6-35B-A3B 這類，只有約 3B 參數在算）
pub fn is_moe(name: &str) -> bool {
    name.to_lowercase().contains("a3b")
}

/// 算「35B 那一類」的大腦（助手選 35B 時用）：MoE，或檔名寫了 30B 以上
fn is_big_brain(name: &str) -> bool {
    let n = name.to_lowercase();
    is_moe(&n) || n.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w.strip_suffix('b').and_then(|d| d.parse::<u32>().ok()).is_some_and(|b| b >= 30))
}

/// 看圖檔要跟模型同一個系列：Qwen 是 qwen3.6／qwen3.5，別家用檔名第一個像名字的字（gemma、llama、mistral…）
fn mmproj_family(model: &Path) -> String {
    let n = file_name(model).to_lowercase();
    for f in ["qwen3.6", "qwen3.5"] {
        if n.contains(f) {
            return f.to_string();
        }
    }
    n.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| w.len() >= 3 && w.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
        .unwrap_or("mmproj")
        .to_string()
}

/// 模型配哪個看圖檔：同系列裡挑檔名跟模型最像的（同系列有好幾個不同人做的看圖檔時，免得照字母排序配到別人的）
fn pair_mmproj(model: &Path, mmprojs: &[PathBuf]) -> Option<PathBuf> {
    let fam = mmproj_family(model);
    let words = name_words(&file_name(model));
    let mut mm: Vec<&PathBuf> = mmprojs.iter().filter(|p| file_name(p).to_lowercase().contains(&fam)).collect();
    mm.sort_by_key(|p| (std::cmp::Reverse(name_words(&file_name(p)).intersection(&words).count()), (*p).clone()));
    mm.first().map(|p| (*p).clone())
}

/// 讀系統提示詞檔：換行統一成 \n（跟 Python 文字模式讀檔一樣）：git 在 Windows checkout 可能把 .txt 換成 CRLF
fn read_prompt(p: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(p).map(|s| strip(&s).replace("\r\n", "\n"))
}

/// 檔名拆成小寫的字（配看圖檔用）：「Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf」→ huihui、qwen3、5、9b、abliterated…
fn name_words(name: &str) -> std::collections::HashSet<String> {
    name.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty() && *w != "gguf" && *w != "mmproj").map(str::to_string).collect()
}

/// 意思座標模型（不是拿來對話的 LLM）
pub fn is_embedding(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("embedding") && !n.contains("mmproj")
}

/// llama-server 這次啟動（log 裡最後一個「] start 」之後）是不是顯存不夠才失敗的。
/// Vulkan（A 卡、Intel）是 ErrorOutOfDeviceMemory／unable to allocate，CUDA 是 cudaMalloc failed: out of memory
pub fn vram_short(log: &str) -> bool {
    let last = log.rfind("] start ").map_or(log, |i| &log[i..]).to_lowercase();
    ["erroroutofdevicememory", "out of memory", "unable to allocate", "failed to allocate"]
        .iter()
        .any(|k| last.contains(k))
}

/// 載入這顆模型大概要多少顯存（等 ComfyUI 讓位時「夠了就走」用；估多估少都只影響等多久）
pub fn vram_need(model_bytes: u64, mmproj_bytes: u64, moe: bool, ngl: i64) -> u64 {
    const G: u64 = 1 << 30;
    match (moe, ngl >= 99) {
        (true, true) => 8 * G,
        (true, false) => 3 * G + G / 2,
        (false, _) => model_bytes + mmproj_bytes + G + G / 2,
    }
}

/// POST /free 之後等顯存真的放出來。ComfyUI 的 /free 只是設旗標就回 200，卸載在它的背景執行緒做，
/// 所以要一直看剩下的顯存：夠了（enough）、一段時間都沒再增加（stable：放完了，再等也沒用）、到上限（timeout）就走
pub struct VramWait {
    need: u64,
    max: Duration,
    settle: Duration,
    /// (秒, 剩下的顯存)
    seen: Vec<(f64, u64)>,
}

impl VramWait {
    pub fn new(need: u64, max: Duration, settle: Duration) -> VramWait {
        VramWait { need, max, settle, seen: vec![] }
    }

    pub fn push(&mut self, t: f64, free: u64) -> Option<&'static str> {
        self.seen.push((t, free));
        if free >= self.need {
            return Some("enough");
        }
        if t >= self.max.as_secs_f64() {
            return Some("timeout");
        }
        // 最後一次「明顯增加」（多 64MB 以上）之後過了 settle 秒都沒再增加
        const STEP: u64 = 64 << 20;
        let mut since = self.seen[0].0;
        for w in self.seen.windows(2) {
            if w[1].1 > w[0].1 + STEP {
                since = w[1].0;
            }
        }
        (t - since >= self.settle.as_secs_f64()).then_some("stable")
    }
}

/// 助手選了 35B，實際找到的卻是別顆（機器上沒有 35B 就會用 9B）
pub fn brain_fell_back(brain: &str, model: &str) -> bool {
    brain == "35b" && !is_moe(model)
}

/// MoE（35B-A3B）照模型卡「不思考」的建議加 presence_penalty 1.5：不加的話偶爾會一直重複同樣的句子寫到上限
/// （2026-10-10 實測 16 次：不加最長 1097 token，加了 204～450）
pub fn tune_for(model: &str, mut p: Value) -> Value {
    if is_moe(model) {
        p["presence_penalty"] = json!(1.5);
    }
    p
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// GET /health 回 200＝模型載入完成（載入中是 503）。用最陽春的 HTTP，不需要 async
fn health(timeout: Duration) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr: SocketAddr = ([127, 0, 0, 1], PORT).into();
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    if s.write_all(
        format!("GET /health HTTP/1.1\r\nHost: {HOST}:{PORT}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 64];
    let mut n = 0;
    while n < 12 {
        match s.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let line = String::from_utf8_lossy(&buf[..n]);
    line.split_whitespace().nth(1) == Some("200")
}

// ================================================================ Windows：程序、檔案鎖
#[cfg(windows)]
pub(crate) mod win {
    use super::PeError;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, ProcessPowerThrottling, SetPriorityClass,
        SetProcessInformation, ABOVE_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    };

    const STILL_ACTIVE: u32 = 259;

    pub fn no_window(cmd: &mut std::process::Command) {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    pub fn pid_alive(pid: i64) -> bool {
        let Ok(pid) = u32::try_from(pid) else {
            return false;
        };
        if pid == 0 {
            return false;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE
        }
    }

    /// 關掉 Windows 對這個程序的節能限速（EcoQoS／Power Throttling）。
    /// 從背景程序（沒有視窗的伺服器、ComfyUI）啟動的 llama-server 會被當成背景工作降速，實測 57 → 26 token／秒
    pub fn no_throttle(pid: i64) -> bool {
        let Ok(pid) = u32::try_from(pid) else {
            return false;
        };
        unsafe {
            let h = OpenProcess(
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if h.is_null() {
                return false;
            }
            let set = |mask: u32| {
                // ControlMask＝這幾項由我們控制，StateMask=0＝不限速
                let st = PROCESS_POWER_THROTTLING_STATE {
                    Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                    ControlMask: mask,
                    StateMask: 0,
                };
                SetProcessInformation(
                    h,
                    ProcessPowerThrottling,
                    &st as *const _ as *const _,
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                ) != 0
            };
            // Windows 10 不認得 IGNORE_TIMER_RESOLUTION（錯誤 87，參數不正確），整個設定都不生效：失敗就只關執行速度限制。
            // 舊版 pe_core.py 只試第一種，在 Windows 10 上其實一直沒關成功（2026-10-09 實測 GetLastError=87）
            let ok = set(PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION)
                || set(PROCESS_POWER_THROTTLING_EXECUTION_SPEED);
            SetPriorityClass(h, ABOVE_NORMAL_PRIORITY_CLASS); // 背景執行時也不被搶走 CPU
            CloseHandle(h);
            ok
        }
    }

    fn run(prog: &str, args: &[&str]) -> Option<String> {
        let mut c = std::process::Command::new(prog);
        c.args(args).creation_flags(CREATE_NO_WINDOW);
        c.output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    }

    /// port 被誰占用（沒有狀態檔的孤兒程序）。只回傳 llama-server.exe 的 pid
    pub fn port_owner(port: u16) -> Option<i64> {
        let out = run("netstat", &["-ano", "-p", "TCP"])?;
        for line in out.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5
                && parts[1].ends_with(&format!(":{port}"))
                && parts[3] == "LISTENING"
            {
                let pid: i64 = parts[4].parse().ok()?;
                let img = run(
                    "tasklist",
                    &["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"],
                )?;
                return img.to_lowercase().contains("llama-server").then_some(pid);
            }
        }
        None
    }

    pub fn kill(pid: i64) {
        let _ = run("taskkill", &["/PID", &pid.to_string(), "/T", "/F"]);
        for _ in 0..50 {
            if !pid_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 跨程序互斥（ComfyUI 的 Python 和生圖站各自是不同程序）：鎖檔案第 0 個位元組，跟 msvcrt.locking 同一個範圍
    pub struct FileLock(std::fs::File);

    impl FileLock {
        pub fn acquire(path: &Path, timeout: Duration) -> Result<FileLock, PeError> {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(path)
                .map_err(|e| PeError(format!("開不了 {}：{e}", path.display())))?;
            let t0 = Instant::now();
            loop {
                let mut ov = unsafe { std::mem::zeroed() };
                let ok = unsafe {
                    LockFileEx(
                        f.as_raw_handle() as HANDLE,
                        LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                        0,
                        1,
                        0,
                        &mut ov,
                    )
                };
                if ok != 0 {
                    return Ok(FileLock(f));
                }
                if t0.elapsed() > timeout {
                    return Err(PeError(
                        "等 llama-server 啟動／關閉逾時（另一邊卡住了？）".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    impl Drop for FileLock {
        fn drop(&mut self) {
            let mut ov = unsafe { std::mem::zeroed() };
            unsafe { UnlockFileEx(self.0.as_raw_handle() as HANDLE, 0, 1, 0, &mut ov) };
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod win {
    use super::PeError;
    use std::path::Path;
    use std::time::Duration;
    pub fn no_window(_cmd: &mut std::process::Command) {}
    pub fn pid_alive(_pid: i64) -> bool {
        false
    }
    pub fn no_throttle(_pid: i64) -> bool {
        true
    }
    pub fn port_owner(_port: u16) -> Option<i64> {
        None
    }
    pub fn kill(_pid: i64) {}
    pub struct FileLock;
    impl FileLock {
        pub fn acquire(_path: &Path, _timeout: Duration) -> Result<FileLock, PeError> {
            Ok(FileLock)
        }
    }
}

// ================================================================ Service：網站這一側（舊站 pe.py）
// 顯存協調：GPU 模式啟動前先等生圖任務跑完，再叫 ComfyUI /free；按下生成前先關掉 llama-server。
// 串流（SSE）給網頁、可中途取消、一次只跑一個改寫（其他排隊）；閒置 N 秒自動關閉。

pub const TASKS: [&str; 7] = [
    "t2i",
    "i2i",
    "anima",
    "refine",
    "caption",
    "caption_qwen",
    "adult",
];

pub fn modes() -> Value {
    json!({"gpu": "GPU（最快）", "partial": "部分放顯卡", "cpu": "CPU（不占顯存，很慢）"})
}

/// 改寫中斷的原因
#[derive(Debug)]
pub enum Fail {
    /// 網頁關掉或按了取消
    Cancelled,
    Error(String),
}

impl From<PeError> for Fail {
    fn from(e: PeError) -> Fail {
        Fail::Error(e.0)
    }
}

/// 送 SSE 事件給網頁；網頁斷線就算取消
struct Emitter {
    tx: mpsc::Sender<Bytes>,
    cancel: CancellationToken,
}

impl Emitter {
    async fn emit(&self, ev: &str, data: Value) -> Result<(), Fail> {
        if matches!(ev, "status" | "error" | "cancelled" | "result") {
            crate::diagnostics::event("llm_sse", json!({"event":ev,"data":data}));
            if ev == "error" { crate::diagnostics::failure(&data.to_string()); }
            if ev == "cancelled" { if let Some(run)=crate::diagnostics::current() {run.cancel();} }
        }
        let msg = format!("event: {ev}\ndata: {}\n\n", dumps(&data));
        if self.tx.send(Bytes::from(msg)).await.is_err() {
            self.cancel.cancel(); // 網頁關掉或按了取消（fetch 被中斷）
            return Err(Fail::Cancelled);
        }
        Ok(())
    }

    fn check(&self) -> Result<(), Fail> {
        if self.cancel.is_cancelled() {
            Err(Fail::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// 排隊人數（離開時自動減一）
struct Waiting<'a>(&'a AtomicI64);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Service {
    core: Option<Arc<Core>>,
    error: Option<String>,
    input_dir: PathBuf,
    comfy: Arc<Comfy>,
    settings_path: PathBuf,
    hub: Hub,
    /// 一次只跑一個改寫
    lock: Arc<tokio::sync::Mutex<()>>,
    waiting: AtomicI64,
    /// 請求編號 → 取消用
    cancels: Mutex<HashMap<String, CancellationToken>>,
    /// 上一次改寫的摘要（秒數等），狀態列用
    last: Mutex<Value>,
    /// 升級換模型（拿掉官方 PE、換新 9B，server\api_pe.rs retire_old_models）做到哪：{phase, msg, pct}；沒在做＝null
    retire: Mutex<Value>,
    http: reqwest::Client,
    /// 姿勢大彙整（成人改寫用）
    pub poses: Arc<crate::poses::Poses>,
    /// 外掛寫法挑段用的意思座標（CPU 的 Qwen3-Embedding，另一顆 llama-server）
    pub embed: Arc<crate::embed::Embedder>,
}

impl Service {
    pub fn new(
        node_dir: &Path,
        input_dir: &Path,
        comfy: Arc<Comfy>,
        settings_path: &Path,
        hub: Hub,
        res: &Path,
    ) -> Arc<Service> {
        let (core, error) = if node_dir.join("pe_core.py").exists() {
            let mut c = Core::new(node_dir);
            c.overrides = settings_path.parent().map(|d| d.join("pe_prompts"));
            c.bundled = Some(res.join("extras").join("anima_prompt_enhancer").join("prompts"));
            c.res = Some(res.to_path_buf());
            (Some(Arc::new(c)), None)
        } else {
            (
                None,
                Some(format!(
                    "找不到 {}\\pe_core.py（ComfyUI 自訂節點 anima_prompt_enhancer）",
                    node_dir.display()
                )),
            )
        };
        let http = reqwest::Client::builder()
            .no_proxy() // llama-server 在本機
            .read_timeout(Duration::from_secs(900))
            .build()
            .expect("建立 HTTP client");
        Arc::new(Service {
            error,
            input_dir: input_dir.to_path_buf(),
            comfy,
            settings_path: settings_path.to_path_buf(),
            hub,
            lock: Arc::new(tokio::sync::Mutex::new(())),
            waiting: AtomicI64::new(0),
            cancels: Mutex::new(HashMap::new()),
            last: Mutex::new(Value::Null),
            retire: Mutex::new(Value::Null),
            http,
            poses: Arc::new(crate::poses::Poses::load(res)),
            embed: crate::embed::Embedder::new(core.clone()),
            core,
        })
    }

    // ---------------------------------------------------------------- 自訂系統提示詞（設定頁）
    /// 網頁上列出來可以改的系統提示詞：(編號, 名稱, 說明, 一定要保留的佔位符)
    const EDITABLE: [(&'static str, &'static str, &'static str, &'static [&'static str]); 23] = [
        ("t2i", "文生圖改寫（MOSS-Image）", "想法 → Qwen-Image 官方格式的英文長段落（MOSS 也吃這種寫法）。模型要回 JSON：{\"rewritten_prompt\": ..., \"wh_ratio\": ...}，改格式會讀不出來。", &[]),
        ("i2i", "改圖改寫（會讀參考圖）", "改圖、局部重繪、多圖參考用（Qwen 2.1）。一樣要回 JSON（rewritten_prompt、wh_ratio）。", &[]),
        ("anima", "Anima 改寫", "寫成 Danbooru tag＋英文句子。一樣要回 JSON（rewritten_prompt）。", &[]),
        ("zh", "中文對照", "把英文提示詞寫成繁體中文摘要。", &[]),
        ("caption", "看圖寫說明", "看一張圖寫英文說明（含畫風）。", &[]),
        ("caption_qwen", "看圖寫英文段落（MOSS 用）", "看一張圖寫 Qwen-Image 官方格式的英文長段落（反推用 MOSS 時）。要回 JSON：{\"rewritten_prompt\": ..., \"wh_ratio\": ...}。", &[]),
        ("adult_pose", "成人（體位）：對到姿勢時", "通用版模型只寫開頭句和人物／場景／光線，姿勢由程式插入。四個大括號佔位符會換成方向、畫風、性行為名稱、鏡頭，一定要留著；模型要回 JSON：{\"opening\": ..., \"details\": ...}。", &["{orientation}", "{style}", "{act}", "{camera}"]),
        ("adult", "成人（體位）：沒對到姿勢時", "整段交給通用版改寫，直接輸出提示詞文字。", &[]),
        ("adult_pose_anima", "成人（體位）Anima：對到姿勢時", "Anima 用。品質、評級、人數、姿勢 tag 和姿勢原文由程式加，模型只寫外觀／場景 tag 和幾句細節。{act} 會換成姿勢名稱，要留著；模型要回 JSON：{\"tags\": ..., \"details\": ...}。小孩樣的 tag 一律會被程式拿掉。", &["{act}"]),
        ("adult_anima", "成人（體位）Anima：沒對到姿勢時", "Anima 用。整段交給通用版寫成 tag＋英文句子，直接輸出提示詞文字。小孩樣的 tag 一律會被程式拿掉。", &[]),
        ("adult_pick", "成人（體位）：讓 AI 挑姿勢", "程式會在後面自動接上姿勢目錄（一行一筆 id｜中文｜英文）；模型要回 JSON：{\"id\": \"姿勢編號或 none\"}。", &[]),
        ("agent", "生圖助手：工作說明", "助手的角色、每輪要回的三種 JSON（generate／finish／reply）和規則。{model_guide} 換成下面那份寫法、{sizes} 換成尺寸、{loras} 換成 LoRA 清單，一定要留著；JSON 格式改了程式會讀不出來。改完下一輪就生效。", &["{model_guide}", "{sizes}", "{loras}"]),
        ("agent_prompt", "生圖助手：只寫詞模式的工作說明", "助手切到「只寫詞」時用：不畫圖，只回提示詞卡。{target} 換成寫法的用途、{guide} 換成那份寫法、{sizes} 換成尺寸，一定要留著；模型要回 JSON：{\"action\":\"prompt\", …} 或 {\"action\":\"reply\", …}。", &["{target}", "{guide}", "{sizes}"]),
        ("agent_qwen21", "生圖助手：Qwen 2.1 文生圖寫法", "生圖模型是 Qwen 2.1 時，填進工作說明的 {model_guide}；只寫詞模式寫 Qwen 文生圖也用它。", &[]),
        ("agent_qwen_edit", "生圖助手：Qwen 2.1 改圖寫法", "改一張圖（改圖工具、Qwen 附一張圖畫）用。助手第一次要寫改圖指令時會先讀它。", &[]),
        ("agent_qwen_multiref", "生圖助手：Qwen 2.1 多圖參考寫法", "兩張以上參考圖（<image1>、<image2>…）用。", &[]),
        ("agent_moss", "生圖助手：MOSS-Image 提示詞寫法", "生圖模型是 MOSS-Image 時，填進工作說明的 {model_guide}。", &[]),
        ("agent_anima", "生圖助手：Anima 提示詞寫法", "生圖模型是 Anima 時，填進工作說明的 {model_guide}。", &[]),
        ("agent_other", "生圖助手：其他模型提示詞寫法", "其他生圖模型（SDXL 系）用的 {model_guide}。", &[]),
        ("agent_h3_base", "H3 影片台：AI 幫我寫（文生、圖生、首尾幀）", "H3 影片台按「AI 幫我寫」時用（文生影片、圖生影片、首尾幀、接續）。照 MiniMax H3 官方寫法輸出一段英文提示詞，不要加說明。", &[]),
        ("agent_h3_ref", "H3 影片台：AI 幫我寫（多素材參考）", "H3 影片台按「AI 幫我寫」時用（多素材參考、關鍵幀、換角色、特效、換畫風、動作控制）。素材依序叫 <Picture N>、<Video N>、<Audio N>。", &[]),
        ("agent_h3_board", "H3 影片台：AI 幫我寫滿分鏡（素材板）", "H3 影片台素材板按「AI 幫我寫滿分鏡」時用。照素材板的用途寫，回 JSON 分鏡（每一鏡從第幾秒、鏡頭、畫面、台詞、聲音），網頁再組成官方格式。", &[]),
        ("agent_h3_sheet", "影片台：拍攝單 AI 潤飾", "影片台拍攝單按「AI 潤飾」時用。把程式組好的那段提示詞改得更好演：部位＋位置、補看得到的結果，時間戳、<d> 台詞、<Picture N> 一字不改；要回兩個 ```text 區塊（提示詞、另一種語言的對照）。", &[]),
    ];

    /// 自訂系統提示詞的字數上限（prompt_save 和設定檔匯入共用）
    pub const PROMPT_MAX_CHARS: usize = 60_000;

    /// 設定檔匯出入用：可以自訂的系統提示詞 (編號, 一定要保留的佔位符)
    pub fn prompt_rules() -> Vec<(&'static str, &'static [&'static str])> {
        Self::EDITABLE.iter().map(|(id, _, _, keep)| (*id, *keep)).collect()
    }

    fn default_system(&self, core: &Core, id: &str) -> String {
        if let Some(t) = crate::agent::default_text(id) {
            return t.into();
        }
        match id {
            "adult_pose" => crate::poses::SYSTEM_POSE.into(),
            "adult" => crate::poses::SYSTEM_ADULT.into(),
            "adult_pose_anima" => crate::poses::SYSTEM_POSE_ANIMA.into(),
            "adult_anima" => crate::poses::SYSTEM_ADULT_ANIMA.into(),
            "adult_pick" => crate::poses::SYSTEM_PICK.trim_end().into(),
            _ => core.default_prompt(id).unwrap_or_default(),
        }
    }

    pub fn prompts_list(&self) -> Result<Value, String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| self.error.clone().unwrap_or_default())?;
        Ok(json!(Self::EDITABLE
            .iter()
            .map(|(id, name, desc, keep)| {
                let custom = core.override_text(id);
                let default = self.default_system(core, id);
                json!({"id": id, "name": name, "desc": desc, "keep": keep, "custom": custom.is_some(),
                       "text": custom.unwrap_or_else(|| default.clone()), "default": default})
            })
            .collect::<Vec<_>>()))
    }

    /// 生圖助手的說明（agent／agent_qwen21／agent_moss／agent_anima／agent_other）和 H3 影片台的寫法（agent_h3_base／agent_h3_ref）：設定頁改過就用改過的，沒有就用預設
    pub fn agent_text(&self, id: &str) -> String {
        self.core
            .as_ref()
            .and_then(|c| c.override_text(id))
            .unwrap_or_else(|| crate::agent::default_text(id).unwrap_or_default().to_string())
    }

    /// 生圖助手本機這顆腦袋實際會載入的模型檔名（找不到模型就是 None，之後載入時會報錯）
    pub fn agent_model(&self, brain: &str) -> Option<String> {
        let core = self.core.as_ref()?;
        core.model_files(&agent_kind(brain), None).ok().map(|(m, _)| file_name(&m))
    }

    /// 「下載讀圖檔」按鈕：見 Core::mmproj_download。brain＝9b／35b（不給就照設定頁的通用模型）
    pub fn clear_cache(&self) -> Result<u64, String> {
        self.core.as_ref().map(|c| c.clear_cache()).unwrap_or_else(|| Err("AI 改寫尚未就緒，暫時無法清理改寫快取".into()))
    }

    pub fn mmproj_download(&self, brain: Option<&str>) -> Result<Option<MmprojGet>, String> {
        let Some(core) = &self.core else {
            return Err(self.error.clone().unwrap_or_else(|| "AI 改寫沒裝好".into()));
        };
        core.set_general(self.opt("pe_general").as_str().unwrap_or("auto"));
        core.mmproj_download(brain).map_err(|e| e.0)
    }

    /// 儲存自訂版本；text 空白（或跟預設一樣）＝還原預設（刪掉自訂檔）
    pub fn prompt_save(&self, id: &str, text: &str) -> Result<Value, String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| self.error.clone().unwrap_or_default())?;
        let (_, name, _, keep) = Self::EDITABLE
            .iter()
            .find(|e| e.0 == id)
            .ok_or_else(|| format!("沒有這個提示詞：{id}"))?;
        let dir = core.overrides.as_ref().ok_or("沒有資料資料夾")?;
        let path = dir.join(format!("{id}.txt"));
        let text = strip(text);
        if text.is_empty() || text == strip(&self.default_system(core, id)) {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| format!("還原預設失敗：{e}"))?;
            }
            return Ok(json!({"custom": false}));
        }
        if text.chars().count() > Self::PROMPT_MAX_CHARS {
            return Err("太長了（上限六萬字）".into());
        }
        let missing: Vec<&str> = keep.iter().copied().filter(|k| !text.contains(k)).collect();
        if !missing.is_empty() {
            return Err(format!("「{name}」一定要留著 {}，程式要靠它們填入內容", missing.join("、")));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("建立 {} 失敗：{e}", dir.display()))?;
        std::fs::write(&path, text).map_err(|e| format!("儲存失敗：{e}"))?;
        Ok(json!({"custom": true}))
    }

    /// 背景：閒置自動關閉
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { me.idle_loop().await });
        self.embed.start();
    }

    // ---------------------------------------------------------------- 設定與狀態
    fn opt(&self, k: &str) -> Value {
        let d = settings::pe_defaults()
            .get(k)
            .cloned()
            .unwrap_or(Value::Null);
        match settings::load(&self.settings_path).get(k) {
            None | Some(Value::Null) => d,
            Some(v) => v.clone(),
        }
    }

    fn ngl(&self) -> i64 {
        match self.opt("pe_mode").as_str() {
            Some("cpu") => 0,
            Some("partial") => py::int(&self.opt("pe_partial_layers")).unwrap_or(16),
            _ => 99,
        }
    }

    fn locked(&self) -> bool {
        self.lock.try_lock().is_err()
    }

    pub fn status(&self) -> Value {
        let Some(core) = &self.core else {
            return json!({"ok": false, "error": self.error});
        };
        core.set_general(self.opt("pe_general").as_str().unwrap_or("auto"));
        let st = core.read_state();
        let idle = py::int(&self.opt("pe_idle")).unwrap_or(120);
        let busy = truthy(st.get("busy"));
        let left = if !st.is_empty() && !busy {
            let last = st.get("last_used").and_then(py::float).unwrap_or_else(now);
            json!(((idle as f64 - (now() - last)).trunc() as i64).max(0))
        } else {
            Value::Null
        };
        let g = |k: &str| st.get(k).cloned().unwrap_or(Value::Null);
        json!({"ok": true, "available": core.available(), "general_models": core.general_choices(), "running": !st.is_empty(), "kind": g("kind"), "ngl": g("ngl"),
               "load_seconds": g("load_seconds"), "busy": busy || self.locked(), "waiting": self.waiting.load(Ordering::SeqCst),
               "idle_left": left, "mode": self.opt("pe_mode"), "modes": modes(),
               "last": self.last.lock().unwrap_or_else(|e| e.into_inner()).clone(),
               "retire": self.retire.lock().unwrap_or_else(|e| e.into_inner()).clone()})
    }

    /// 檔案位置那一層（升級換模型要看 LLM 資料夾）；找不到 AI 改寫節點＝None
    pub fn core(&self) -> Option<Arc<Core>> {
        self.core.clone()
    }

    /// 升級換模型的進度（見 retire 欄位），順便推給網頁
    pub fn set_retire(&self, v: Value) {
        *self.retire.lock().unwrap_or_else(|e| e.into_inner()) = v;
        self.push_status();
    }

    fn push_status(&self) {
        let mut m = Map::from_iter([("type".to_string(), json!("pe"))]);
        if let Value::Object(s) = self.status() {
            m.extend(s);
        }
        self.hub.send(Value::Object(m));
    }

    async fn idle_loop(self: Arc<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if self.locked() {
                continue;
            }
            let c = core.clone();
            let st = run_blocking(move || c.read_state()).await;
            let idle = py::int(&self.opt("pe_idle")).unwrap_or(120) as f64;
            let last = st.get("last_used").and_then(py::float).unwrap_or(0.0);
            if !st.is_empty() && !truthy(st.get("busy")) && now() - last > idle {
                let c = core.clone();
                match run_blocking(move || c.stop("閒置逾時")).await {
                    Ok(_) => eprintln!("llama-server 閒置 {idle} 秒，已關閉"),
                    Err(e) => eprintln!("閒置檢查失敗：{e}"),
                }
                self.push_status();
            }
        }
    }

    pub async fn stop(&self, reason: &str) -> Result<bool, PeError> {
        let Some(core) = self.core.clone() else {
            return Ok(false);
        };
        let r = {
            let _g = self.lock.lock().await;
            let reason = reason.to_string();
            run_blocking(move || core.stop(&reason)).await
        };
        self.push_status();
        r
    }

    /// 按下生成前呼叫：GPU／部分模式的 llama-server 占著顯存，ComfyUI 會爆或變很慢，先關掉（正在改寫就等它寫完）
    pub async fn release_for_generation(&self) -> bool {
        let Some(core) = self.core.clone() else {
            return false;
        };
        let c = core.clone();
        let st = run_blocking(move || c.read_state()).await;
        if st.is_empty() || st.get("ngl").and_then(py::int).unwrap_or(0) == 0 {
            return false; // 沒在跑；CPU 模式不占顯存
        }
        {
            let _g = self.lock.lock().await;
            if let Err(e) = run_blocking(move || core.stop("生圖前讓出顯存")).await {
                eprintln!("生圖前關閉 llama-server 失敗：{e}");
            }
        }
        self.push_status();
        true
    }

    pub fn cancel(&self, rid: &str) -> bool {
        match self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(rid)
        {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    // ---------------------------------------------------------------- 圖片
    /// 參考圖縮到看得清楚就好（張數越多越小），轉成 PNG
    fn load_images(input_dir: &Path, names: &[String]) -> Result<Vec<Vec<u8>>, PeError> {
        let n = names.len();
        let budget = if n <= 1 {
            1024.0 * 1024.0
        } else if n <= 3 {
            640.0 * 640.0
        } else {
            448.0 * 448.0
        };
        names
            .iter()
            .map(|name| {
                let base = file_name(Path::new(name));
                let p = input_dir.join(&base);
                let im = image::open(&p)
                    .map_err(|e| PeError(format!("讀不到參考圖 {base}：{e}")))?
                    .to_rgb8();
                let (w, h) = (im.width() as f64, im.height() as f64);
                let s = (budget / (w * h)).sqrt();
                let im = if s < 1.0 {
                    let (nw, nh) = (((w * s) as u32).max(28), ((h * s) as u32).max(28));
                    image::imageops::resize(&im, nw, nh, image::imageops::FilterType::Lanczos3)
                } else {
                    im
                };
                let mut out = std::io::Cursor::new(Vec::new());
                im.write_to(&mut out, image::ImageFormat::Png)
                    .map_err(|e| PeError(format!("參考圖轉 PNG 失敗：{e}")))?;
                Ok(out.into_inner())
            })
            .collect()
    }

    // ---------------------------------------------------------------- 串流到 llama-server
    /// 送出請求、邊收邊轉給網頁。zh＝中文摘要：內容改送 zh_delta、思考不送。回傳 (內容, 思考, timings)
    async fn chat(
        &self,
        core: &Core,
        body: &Value,
        em: &Emitter,
        zh: bool,
    ) -> Result<(String, String, Value), Fail> {
        self.chat_at(&core.url(), None, body, em, zh).await
    }

    /// 同 chat，送到指定的 OpenAI 相容網址（雲端助手：SSH 通道的 llama-server，或使用者填的網址＋金鑰）
    async fn chat_at(
        &self,
        base: &str,
        key: Option<&str>,
        body: &Value,
        em: &Emitter,
        zh: bool,
    ) -> Result<(String, String, Value), Fail> {
        if let Some(run)=crate::diagnostics::current() {
            crate::diagnostics::remember_secret(&run.store,key);
            run.snapshot("llm_request",body);
        }
        let mut req = self.http.post(format!("{base}/v1/chat/completions")).json(body);
        if let Some(k) = key.filter(|k| !k.is_empty()) {
            req = req.bearer_auth(k);
        }
        let r = req
            .send()
            .await
            .map_err(|e| Fail::Error(format!("連不到 llama-server：{e}")))?;
        let status = r.status();
        if status.as_u16() >= 400 {
            let text = r.text().await.unwrap_or_default();
            return Err(Fail::Error(format!(
                "llama-server 回應 {}：{}",
                status.as_u16(),
                text.chars().take(300).collect::<String>()
            )));
        }
        let (mut content, mut reasoning, mut timings) = (String::new(), String::new(), json!({}));
        let mut stream = r.bytes_stream();
        let mut buf: Vec<u8> = vec![];
        let mut finished = false;
        // 上次檢查鬼打牆時的長度：每多 256 個位元組看一次
        let mut seen = (0, 0);
        while !finished {
            let chunk = tokio::select! {
                _ = em.cancel.cancelled() => return Err(Fail::Cancelled),
                c = stream.next() => c,
            };
            let last = match chunk {
                Some(Ok(b)) => {
                    buf.extend_from_slice(&b);
                    false
                }
                Some(Err(e)) => return Err(Fail::Error(format!("llama-server 連線中斷：{e}"))),
                None => true,
            };
            // 一行一行處理；最後一段沒有換行也要處理
            let mut lines: Vec<Vec<u8>> = vec![];
            while let Some(i) = buf.iter().position(|&c| c == b'\n') {
                lines.push(buf.drain(..=i).collect());
            }
            if last && !buf.is_empty() {
                lines.push(std::mem::take(&mut buf));
            }
            for raw in lines {
                em.check()?;
                match sse_delta(strip(&String::from_utf8_lossy(&raw))) {
                    Some(Delta::Content(t)) => {
                        content.push_str(&t);
                        em.emit(if zh { "zh_delta" } else { "content" }, json!({"text": t}))
                            .await?;
                    }
                    Some(Delta::Reasoning(t)) => {
                        reasoning.push_str(&t);
                        if !zh {
                            em.emit("reasoning", json!({"text": t})).await?;
                        }
                    }
                    Some(Delta::Timings(v)) => timings = v,
                    Some(Delta::Done) => {
                        finished = true;
                        break;
                    }
                    None => {}
                }
            }
            finished |= last;
            if !finished && (content.len() >= seen.0 + 256 || reasoning.len() >= seen.1 + 256) {
                seen = (content.len(), reasoning.len());
                if looping(&content) || looping(&reasoning) {
                    // 斷線 llama-server 就停；寫到一半的交給呼叫的人照常解析（第一份通常是完整的，壞了就照原本的規則重試）
                    eprintln!("模型一直重複同一段，提早停下（內容 {} 字、思考 {} 字）", content.chars().count(), reasoning.chars().count());
                    em.emit("status", json!({"phase": "loop", "msg": "模型開始一直重複同一段，先停下來"})).await?;
                    break;
                }
            }
        }
        crate::diagnostics::snapshot("llm_response", &json!({"content":content,"timings":timings,"visible_reasoning_chars":reasoning.chars().count()}));
        Ok((content, reasoning, timings))
    }

    /// 排隊拿改寫鎖；排隊時每秒送一次心跳，網頁關掉或取消就放棄
    async fn acquire(&self, em: &Emitter) -> Result<OwnedMutexGuard<()>, Fail> {
        if self.locked() {
            em.emit(
                "status",
                json!({"phase": "queued", "msg": "前面還有改寫在跑，排隊中…"}),
            )
            .await?;
        }
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let _w = Waiting(&self.waiting);
        loop {
            em.check()?;
            match tokio::time::timeout(Duration::from_secs(1), self.lock.clone().lock_owned()).await
            {
                Ok(g) => return Ok(g),
                Err(_) => em.emit("ping", json!({})).await?,
            }
        }
    }

    /// 確保 llama-server 跑著對的模型；GPU 模式先等生圖任務跑完（8GB 放不下兩邊）。回傳載入秒數
    async fn ensure(
        &self,
        core: &Arc<Core>,
        kind: &str,
        ngl: i64,
        em: &Emitter,
    ) -> Result<f64, Fail> {
        if ngl > 0 {
            let mut notified = false;
            loop {
                let l = self.comfy.live();
                if l.running.is_none() && l.queue_remaining == 0 {
                    break;
                }
                if !notified {
                    em.emit("status", json!({"phase": "wait_gen", "msg": "生圖任務還在跑，等它跑完再改寫（8GB 顯卡放不下兩邊）…"})).await?;
                    notified = true;
                }
                em.check()?;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        let c = core.clone();
        let (k2, same) = (kind.to_string(), ngl);
        let ready = run_blocking(move || {
            let st = c.read_state();
            st.get("kind").and_then(Value::as_str) == Some(&k2)
                && st.get("ngl").and_then(py::int) == Some(same)
                && c.health(Duration::from_millis(1500))
        })
        .await;
        if !ready {
            let how = if ngl == 0 {
                "CPU".to_string()
            } else if ngl >= 99 {
                "GPU".to_string()
            } else {
                format!("GPU {ngl} 層")
            };
            em.emit(
                "status",
                json!({"phase": "loading", "msg": format!("載入改寫模型（{kind}，{how}）…")}),
            )
            .await?;
        }
        // 這顆模型大概要多少顯存：ComfyUI 放出這麼多就不用再等
        let need = {
            let (c, k) = (core.clone(), kind.to_string());
            run_blocking(move || {
                c.model_files(&k, None).map_or(0, |(m, mm)| {
                    let size = |p: &Path| std::fs::metadata(p).map_or(0, |x| x.len());
                    vram_need(size(&m), mm.as_deref().map_or(0, size), is_moe(&file_name(&m)), ngl)
                })
            })
            .await
        };
        let (c, kind, comfy, rt, log) = (
            core.clone(),
            kind.to_string(),
            self.comfy.clone(),
            tokio::runtime::Handle::current(),
            core.clone(),
        );
        // 在 llama-server 啟動前（背景執行緒）叫 ComfyUI 卸載模型、釋放顯存。/free 只是設旗標就回 200，
        // 真的卸載在 ComfyUI 的背景執行緒，所以要看 /system_stats 等它放完（2026-10-26 A 卡朋友固定等 1 秒會爆顯存）
        let free_comfy = move |ngl: i64, attempt: u32| {
            if ngl == 0 {
                return;
            }
            let body = json!({"unload_models": true, "free_memory": true});
            match rt.block_on(tokio::time::timeout(
                Duration::from_secs(15),
                comfy.post("/free", &body),
            )) {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return eprintln!("ComfyUI /free 失敗：{e}"),
                Err(_) => return eprintln!("ComfyUI /free 失敗：逾時"),
            }
            let (max, settle) = if attempt == 0 { (20, 4) } else { (45, 8) };
            let mut w = VramWait::new(need, Duration::from_secs(max), Duration::from_secs(settle));
            let t0 = Instant::now();
            let why = loop {
                let stats = rt.block_on(tokio::time::timeout(Duration::from_secs(3), comfy.get("/system_stats")));
                // 別的程式也能用的顯存＝vram_free 扣掉 PyTorch 自己快取著的那份
                let free = stats.ok().and_then(Result::ok).and_then(|s| {
                    let d = s["devices"].get(0)?;
                    Some(d["vram_free"].as_u64()?.saturating_sub(d["torch_vram_free"].as_u64().unwrap_or(0)))
                });
                let Some(free) = free else {
                    std::thread::sleep(Duration::from_secs(if attempt == 0 { 1 } else { 3 }));
                    break "no_stats";
                };
                if let Some(why) = w.push(t0.elapsed().as_secs_f64(), free) {
                    log.log_line(&format!("等 ComfyUI 讓出顯存：{why}，剩 {:.1}GB／要約 {:.1}GB，{:.1} 秒", free as f64 / 1e9, need as f64 / 1e9, t0.elapsed().as_secs_f64()));
                    break why;
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            crate::diagnostics::event("comfy_vram_wait", json!({"attempt": attempt, "result": why, "need": need, "seconds": t0.elapsed().as_secs_f64()}));
        };
        let (started, load) =
            run_blocking(move || c.ensure(&kind, ngl, free_comfy, Duration::from_secs(180)))
                .await?;
        if started {
            self.push_status();
        }
        Ok(load)
    }

    /// （持有改寫鎖時呼叫）送出請求；JSON 壞掉就換種子重試一次。回傳 (原始輸出, 思考, timings, 解析結果)
    #[allow(clippy::too_many_arguments)]
    async fn generate(
        &self,
        core: &Core,
        msgs: &Value,
        task: &str,
        prev: &str,
        temperature: f64,
        seed: i64,
        think: bool,
        kind: &str,
        request: &str,
        em: &Emitter,
    ) -> Result<(String, String, Value, Option<Parsed>), Fail> {
        let (mut raw, mut reasoning, mut timings, mut parsed) =
            (String::new(), String::new(), json!({}), None);
        // 通用模型（Anima、Qwen 反推走這條）是 MoE 時要加 presence_penalty，跟聊天一樣
        let model_name = if kind == "adult" || kind == "adult_vision" {
            core.model_files(kind, None)
                .map(|f| file_name(&f.0))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut msgs = msgs.clone();
        core.set_busy(1);
        let r: Result<(), Fail> = async {
            for attempt in 0..2 {
                if attempt > 0 {
                    em.emit(
                        "status",
                        json!({"phase": "retry", "msg": "輸出格式壞掉，換個種子重試一次…"}),
                    )
                    .await?;
                    em.emit("reset", json!({})).await?;
                }
                em.emit("status", json!({"phase": "generating", "msg": "改寫中…"}))
                    .await?;
                (raw, reasoning, timings) = self
                    .chat(
                        core,
                        &tune_for(
                            &model_name,
                            with_think_budget(
                                payload(
                                    &msgs,
                                    temperature,
                                    seed + attempt,
                                    think,
                                    Some(official_max_tokens(kind)),
                                    true,
                                ),
                                think.then_some(PE_THINK_BUDGET),
                            ),
                        ),
                        em,
                        false,
                    )
                    .await?;
                parsed = parse_output(&raw, task, prev);
                if let Some(p) = &parsed {
                    let missing = missing_visible_text(&p.prompt, request);
                    if !missing.is_empty() {
                        if let Some(a) = msgs.as_array_mut() {
                            a.push(json!({"role":"assistant","content":raw}));
                            a.push(json!({"role":"user","content":format!("Your rewritten prompt omitted these exact strings to display in the image: {}. Include every string verbatim, in quotes, at its requested position. Return the complete corrected JSON.", dumps(&json!(missing)))}));
                        }
                        parsed = None;
                        continue;
                    }
                }
                // Anima 只給 tag、沒寫英文句子：換種子再寫一次，第二次還是沒有就用這份
                if task == "anima"
                    && attempt == 0
                    && parsed.as_ref().is_some_and(|p| !anima_has_sentences(&p.prompt))
                {
                    eprintln!("Anima 改寫沒有英文句子，換種子重寫");
                    continue;
                }
                if parsed.is_some() {
                    break;
                }
                eprintln!(
                    "改寫輸出無法解析：{:?}",
                    raw.chars().take(300).collect::<String>()
                );
            }
            Ok(())
        }
        .await;
        core.set_busy(0);
        r?;
        Ok((raw, reasoning, timings, parsed))
    }

    /// 一次改寫。回傳結果（給網頁的 result 事件）
    async fn rewrite(&self, core: &Arc<Core>, body: &Value, em: &Emitter) -> Result<Value, Fail> {
        let s = |k: &str| match body.get(k) {
            Some(Value::String(v)) => strip(v).to_string(),
            Some(v) if truthy(Some(v)) => str_of(v),
            _ => String::new(),
        };
        let task = match body.get("task") {
            None => "t2i".to_string(),
            Some(v) => str_of(v),
        };
        if !TASKS.contains(&task.as_str()) {
            return Err(Fail::Error(format!("未知的改寫類型：{task}")));
        }
        let (mut idea, mut prev, mut instruction) = (s("idea"), s("prev"), s("instruction"));
        let last_version = prev.clone();
        // 微調＝「原本的想法＋調整指令」合成一句新想法，照原本的類型（base）重新改寫一次。
        // 改寫模型（Qwen-Image 2.1 PE）只學過「簡短想法 → 完整提示詞」和「改圖指令 → 精確指令」兩種工作；
        // 舊版叫它回逐段補丁，2026-10-09 實測會寫出「換成一字不改的原文、刪除欄位寫文字」這種補丁，
        // 新舊段落並存。官方範例和社群工具也都沒有多輪修改，所以新版不再用補丁（apply_patch 只留著跟 pe_core.py 對照）
        let mut refined: Option<String> = None;
        let task = if task == "refine" {
            let source = if idea.is_empty() {
                prev.clone()
            } else {
                idea.clone()
            };
            if source.is_empty() || instruction.is_empty() {
                return Err(Fail::Error(
                    "微調需要原本的想法（或上一版）和調整指令".into(),
                ));
            }
            idea = merge_request(&source, &instruction);
            (prev, instruction) = (String::new(), String::new());
            refined = Some(idea.clone());
            match body.get("base").map(str_of).as_deref() {
                Some(b @ ("t2i" | "anima" | "i2i" | "adult")) => b.to_string(),
                _ => "t2i".to_string(),
            }
        } else {
            task
        };
        let captioning = task == "caption" || task == "caption_qwen";
        if captioning {
            let tags: Vec<String> = body.get("tags").and_then(Value::as_array).into_iter().flatten().map(str_of).collect();
            (prev, instruction) = (String::new(), caption_hint(&tags));
        }
        if !captioning && idea.is_empty() {
            return Err(Fail::Error("請先輸入想法".into()));
        }
        if task == "adult" {
            return self.adult(core, body, &idea, refined, em).await;
        }
        let mut names: Vec<String> = body
            .get("images")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|n| truthy(Some(n)))
            .map(str_of)
            .take(10)
            .collect();
        if task == "i2i" && names.is_empty() {
            return Err(Fail::Error("圖生圖類的改寫需要參考圖".into()));
        }
        if captioning {
            if names.is_empty() {
                return Err(Fail::Error("寫說明需要一張圖".into()));
            }
            names.truncate(1);
        }
        let images = if names.is_empty() {
            vec![]
        } else {
            let dir = self.input_dir.clone();
            run_blocking(move || Service::load_images(&dir, &names)).await?
        };
        let temperature = match body.get("temperature") {
            None => py::float(&self.opt("pe_temperature")).unwrap_or(1.0),
            Some(v) => py::float(v).ok_or_else(|| Fail::Error("temperature 要是數字".into()))?,
        };
        let seed = match py::or(body.get("seed"), None) {
            None => 0,
            Some(v) => py::int(v).ok_or_else(|| Fail::Error("seed 要是整數".into()))?,
        };
        let think = truthy(Some(
            &body
                .get("think")
                .cloned()
                .unwrap_or_else(|| self.opt("pe_think")),
        ));
        let noct = truthy(body.get("noct_anime"));
        let ratio = py::or(body.get("ratio"), None)
            .map(str_of)
            .unwrap_or_default(); // 不去空白（跟舊版的快取鍵一致）
        let want_zh = truthy(Some(
            &body.get("zh").cloned().unwrap_or_else(|| self.opt("pe_zh")),
        ));
        let ngl = self.ngl();
        // 用哪顆模型：一律通用模型 9B／35B（2026-10-27 預設改通用、2026-10-30 官方 PE 整個拿掉，見 rewrite_kind）
        core.set_general(self.opt("pe_general").as_str().unwrap_or("auto"));
        let kind = rewrite_kind(&task, captioning, !images.is_empty()).to_string();
        let model_name = file_name(&core.model_files(&kind, None)?.0);
        let mut parts = vec![
            ("task", json!(task)),
            ("idea", json!(idea)),
            ("prev", json!(prev)),
            ("instruction", json!(instruction)),
            ("ratio", json!(ratio)),
            ("temperature", json!(py::round(temperature, 3))),
            ("seed", json!(seed)),
            ("think", json!(think)),
            ("noct_anime", json!(noct)),
            ("model", json!(model_name)),
        ];
        // 自訂了系統提示詞：快取要分開（沒自訂時鍵跟舊版 Python 一樣，兩邊共用快取）
        if let Some(t) = core.override_text(&task).or_else(|| core.default_differs_from_node(&task)) {
            parts.push(("system", json!(sha256_hex(t.as_bytes()))));
        }
        // Anima 版、文生圖版也照助手的做法查體位庫（網頁選「自動」永遠不會走「成人（體位）」；2026-10-28 實測 9B 不認得乳交，
        // 五種思考設定都寫成 nipple clamp、chest milking，畫出夾鍊子、兩根）：附 Term 那行，寫完照體位庫補正
        let pose_note = matches!(task.as_str(), "anima" | "t2i").then(|| self.poses.term_note(&idea)).flatten();
        if let Some(n) = &pose_note {
            parts.push(("pose", n["id"].clone()));
        }
        // 外掛寫法（2026-10-29）：這個家族的寫法接了使用者的 skill，照想法挑段接在使用者訊息後面（快取鍵帶挑到的內容）。
        // 裝了 Qwen3-Embedding 就照意思挑，沒有就照關鍵字（embed.rs）
        let skill = skill_guide(&task, &s("family"))
            .zip(self.settings_path.parent())
            .and_then(|(g, data)| crate::skills::for_guide(data, &self.settings_path, g));
        let skill_note = match &skill {
            Some(sk) => Some(self.embed.pick_block(sk, &format!("{idea} {instruction}"), crate::skills::PICK_WRITE).await),
            None => None,
        }
        .filter(|t| !t.is_empty());
        if let Some(t) = &skill_note {
            parts.push(("skill", json!(sha256_hex(t.as_bytes()))));
        }
        let key = core.cache_key(&parts, Some(&images));
        let use_cache = truthy(Some(&body.get("cache").cloned().unwrap_or(json!(true))));
        let t0 = Instant::now();
        let mut result = match use_cache.then(|| core.cache_get(&key)).flatten()
            .filter(|hit| missing_visible_text(hit.get("prompt").and_then(Value::as_str).unwrap_or(""), &idea).is_empty()) {
            Some(mut hit) => {
                // 舊快取可能是校正規則加入前存的（重複套用結果不變）
                if task == "anima" {
                    if let Some(p) = hit.get("prompt").and_then(Value::as_str) {
                        let fixed = fix_anima_tags(p);
                        hit.insert("prompt".into(), json!(fixed));
                    }
                }
                hit.insert("cached".into(), json!(true));
                hit.insert("seconds".into(), json!(0.0));
                hit.insert("load_seconds".into(), json!(0.0));
                em.emit("status", json!({"phase": "cached", "msg": "同樣的輸入改寫過了，直接用上次的結果（快取）"})).await?;
                hit
            }
            None => {
                let mut msgs =
                    core.build_messages(&task, &idea, &images, &prev, &instruction, noct, &ratio)?;
                if let (Some(t), Some(m)) = (&skill_note, msgs.get_mut(1)) {
                    append_text(m, t);
                }
                if let (Some(n), Some(Value::String(u))) = (&pose_note, msgs.get_mut(1).map(|m| &mut m["content"])) {
                    u.push_str("\n\n");
                    u.push_str(&crate::poses::Poses::term_line(n));
                }
                let (load, (raw, reasoning, timings, parsed)) = {
                    let _g = self.acquire(em).await?;
                    let load = self.ensure(core, &kind, ngl, em).await?;
                    (
                        load,
                        self.generate(
                            core,
                            &msgs,
                            &task,
                            &prev,
                            temperature,
                            seed,
                            think,
                            &kind,
                            &idea,
                            em,
                        )
                        .await?,
                    )
                };
                let tps = match timings.get("predicted_per_second").and_then(py::float) {
                    Some(x) if x != 0.0 => json!(py::round(x, 1)),
                    _ => json!(0), // Python：round(0, 1) 是整數 0
                };
                let mut r = Map::new();
                r.insert("reasoning".into(), json!(reasoning));
                r.insert(
                    "tokens".into(),
                    timings.get("predicted_n").cloned().unwrap_or(Value::Null),
                );
                r.insert("tps".into(), tps);
                r.insert("cached".into(), json!(false));
                r.insert("load_seconds".into(), json!(py::round(load, 2)));
                r.insert(
                    "seconds".into(),
                    json!(py::round(t0.elapsed().as_secs_f64(), 2)),
                );
                r.insert("model_kind".into(), json!(kind));
                match parsed {
                    None => {
                        // 讀不出來就退回原文：微調退回上一版
                        let back = if refined.is_some() && !last_version.is_empty() {
                            &last_version
                        } else {
                            &idea
                        };
                        let n = raw.chars().count();
                        r.insert("prompt".into(), json!(back));
                        r.insert("wh_ratio".into(), json!(""));
                        r.insert("ratio_follow".into(), json!(""));
                        r.insert("fallback".into(), json!(true));
                        r.insert("note".into(), json!("模型輸出的格式壞掉，重試後還是讀不出來，先退回原文。可以按「重新改寫」再試一次。"));
                        r.insert(
                            "raw".into(),
                            json!(raw.chars().skip(n.saturating_sub(1500)).collect::<String>()),
                        );
                    }
                    Some(p) => {
                        let pose = pose_note.as_ref().and_then(|n| n["id"].as_str()).and_then(|id| self.poses.get(id));
                        let (posed, pose_msg) = crate::agent::polish(pose, task == "anima", &p.prompt, &idea);
                        let prompt = if noct {
                            fix_noct_anime(&posed)
                        } else if task == "anima" {
                            fix_anima_tags(&posed)
                        } else {
                            posed
                        };
                        r.insert("prompt".into(), json!(prompt));
                        let ratio = if task == "caption_qwen" {
                            images.first().and_then(|b| image_ratio(b)).unwrap_or(p.wh_ratio.clone())
                        } else {
                            p.wh_ratio.clone()
                        };
                        r.insert("wh_ratio".into(), json!(ratio));
                        r.insert("ratio_follow".into(), json!(p.ratio_follow));
                        r.insert("fallback".into(), json!(false));
                        // 體位庫補正寫在說明裡，使用者看得到改了什麼
                        let note = [result_note(&p).to_string(), pose_msg.unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("；");
                        r.insert("note".into(), json!(note));
                        let keep: Map<String, Value> = [
                            "prompt",
                            "wh_ratio",
                            "ratio_follow",
                            "reasoning",
                            "fallback",
                            "note",
                            "tokens",
                        ]
                        .iter()
                        .map(|k| (k.to_string(), r[*k].clone()))
                        .collect();
                        core.cache_put(&key, &Value::Object(keep));
                    }
                }
                r
            }
        };
        if let Some(i) = &refined {
            result.insert("idea".into(), json!(i)); // 合成後的想法：下一次微調從這裡接著改
        }
        if want_zh && !truthy(result.get("fallback")) {
            let prompt = result.get("prompt").map(str_of).unwrap_or_default();
            let (zh, secs) = self.zh(core, &prompt, em).await?;
            result.insert("zh".into(), json!(zh));
            result.insert("zh_seconds".into(), json!(secs));
        }
        let mut last: Map<String, Value> = [
            "seconds",
            "load_seconds",
            "tokens",
            "tps",
            "cached",
            "model_kind",
        ]
        .iter()
        .map(|k| {
            (
                k.to_string(),
                result.get(*k).cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
        last.insert("at".into(), json!(now()));
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Value::Object(last);
        if truthy(Some(&self.opt("pe_close_after"))) && !self.locked() {
            let c = core.clone();
            if let Err(e) = run_blocking(move || c.stop("設定：改寫完立刻關閉")).await {
                eprintln!("改寫完關閉 llama-server 失敗：{e}");
            }
        } else {
            core.touch();
        }
        self.push_status();
        Ok(Value::Object(result))
    }

    /// 成人（體位）改寫：姿勢（選單 → 名稱／口語比對 → 模型從目錄挑）＋通用版去審查模型寫開頭和細節，
    /// 姿勢的幾何描述由程式原封不動插在中間（poses.rs 開頭有實測說明）。對不到姿勢就整段交給通用版改寫。
    async fn adult(
        &self,
        core: &Arc<Core>,
        body: &Value,
        idea: &str,
        refined: Option<String>,
        em: &Emitter,
    ) -> Result<Value, Fail> {
        let poses = self.poses.clone();
        let temperature = body.get("temperature").and_then(py::float).unwrap_or(0.7);
        let seed = body.get("seed").and_then(py::int).unwrap_or(1);
        let use_control = truthy(body.get("pose_control"));
        let anime = truthy(body.get("noct_anime"))
            || [
                "動漫",
                "anime",
                "插畫",
                "二次元",
                "卡通",
                "illustration",
                "manga",
            ]
            .iter()
            .any(|k| idea.to_lowercase().contains(k));
        let want = body.get("pose").map(str_of).unwrap_or_default();
        // 目前的生圖模型是 Anima：寫成 Danbooru tag＋英文句子（姿勢 tag 由程式加）
        let anima = body.get("target").map(str_of).as_deref() == Some("anima");
        core.set_general(self.opt("pe_general").as_str().unwrap_or("auto"));
        let model_name = file_name(&core.model_files("adult", None)?.0);
        // 系統提示詞：設定頁可以自訂（data\pe_prompts\adult_*.txt），沒有就用 poses.rs 的預設
        let sys_pose = if anima {
            core.override_text("adult_pose_anima")
                .unwrap_or_else(|| crate::poses::SYSTEM_POSE_ANIMA.into())
        } else {
            core.override_text("adult_pose")
                .unwrap_or_else(|| crate::poses::SYSTEM_POSE.into())
        };
        let sys_adult = if anima {
            core.override_text("adult_anima")
                .unwrap_or_else(|| crate::poses::SYSTEM_ADULT_ANIMA.into())
        } else {
            core.override_text("adult")
                .unwrap_or_else(|| crate::poses::SYSTEM_ADULT.into())
        };
        let sys_pick = core
            .override_text("adult_pick")
            .unwrap_or_else(|| crate::poses::SYSTEM_PICK.into());
        let want_zh = truthy(Some(
            &body.get("zh").cloned().unwrap_or_else(|| self.opt("pe_zh")),
        ));
        let mut parts = vec![
            ("task", json!("adult")),
            ("idea", json!(idea)),
            ("pose", json!(want)),
            ("pose_control", json!(use_control)),
            ("anime", json!(anime)),
            ("temperature", json!(py::round(temperature, 3))),
            ("seed", json!(seed)),
            ("model", json!(model_name)),
            ("poses", json!(poses.list.len())),
            (
                "system",
                json!(sha256_hex(
                    format!(
                        "{sys_pose}
{sys_adult}
{sys_pick}"
                    )
                    .as_bytes()
                )),
            ),
        ];
        if anima {
            parts.push(("target", json!("anima")));
        }
        let key = core.cache_key(&parts, None);
        let t0 = Instant::now();
        let ngl = self.ngl();
        let use_cache = truthy(Some(&body.get("cache").cloned().unwrap_or(json!(true))));
        let mut result = if let Some(mut hit) = use_cache.then(|| core.cache_get(&key)).flatten() {
            hit.insert("cached".into(), json!(true));
            hit.insert("seconds".into(), json!(0.0));
            hit.insert("load_seconds".into(), json!(0.0));
            em.emit(
                "status",
                json!({"phase": "cached", "msg": "同樣的輸入改寫過了，直接用上次的結果（快取）"}),
            )
            .await?;
            hit
        } else {
            let _g = self.acquire(em).await?;
            let load = self.ensure(core, "adult", ngl, em).await?;
            core.set_busy(1);
            let r: Result<Map<String, Value>, Fail> = async {
                // 1. 姿勢：選單指定 → 想法裡的名稱／口語 → 模型從目錄挑
                let mut how = String::new();
                let mut pose = match want.as_str() {
                    "" | "auto" => None,
                    "none" => {
                        how = "不用姿勢".into();
                        None
                    }
                    id => poses.get(id).inspect(|_| how = "選單".into()),
                };
                if pose.is_none() && how.is_empty() {
                    if let Some((p, name)) = poses.find(idea) {
                        how = format!("想法裡的「{name}」");
                        pose = Some(p);
                    }
                }
                if pose.is_none() && how.is_empty() && !poses.list.is_empty() && crate::poses::looks_sexual(idea) {
                    em.emit("status", json!({"phase": "pick", "msg": "從姿勢大彙整挑姿勢…"})).await?;
                    let msgs = json!([
                        {"role": "system", "content": format!("{sys_pick}{}", poses.catalog())},
                        {"role": "user", "content": idea},
                    ]);
                    let (raw, _, _) = self
                        .chat(core, &payload(&msgs, 0.2, seed, false, Some(200), true), em, false)
                        .await?;
                    let id = json_obj(&raw)
                        .and_then(|o| o.get("id").map(str_of))
                        .unwrap_or_default();
                    if let Some(p) = poses.get(id.trim()) {
                        how = "模型挑選".into();
                        pose = Some(p);
                    }
                }
                em.emit("reset", json!({})).await?;
                em.emit("status", json!({"phase": "generating", "msg": "改寫中…"})).await?;
                let style = if anime { "anime illustration" } else { "realistic photograph" };
                let mut r = Map::new();
                let (prompt, ratio, timings, fallback) = match pose {
                    Some(p) => {
                        let (camera, orient) = p.camera(idea, use_control);
                        let act = if anima { p.english.first().cloned().unwrap_or_else(|| p.zh.clone()) } else { p.act_phrase() };
                        let sys = sys_pose
                            .replace("{orientation}", &orient)
                            .replace("{style}", style)
                            .replace("{act}", &act)
                            .replace("{camera}", &camera);
                        let msgs = json!([{"role": "system", "content": sys}, {"role": "user", "content": idea}]);
                        let mut out = None;
                        let mut timings = json!({});
                        for attempt in 0..2 {
                            let (raw, _, t) = self
                                .chat(core, &tune_for(&model_name, payload(&msgs, temperature, seed + attempt, false, Some(1500), true)), em, false)
                                .await?;
                            timings = t;
                            out = json_obj(&raw).and_then(|o| {
                                let de = o.get("details").map(str_of).unwrap_or_default();
                                if anima {
                                    // 模型有時把 tags 寫成陣列：接成逗號分隔
                                    let tags = match o.get("tags") {
                                        Some(Value::Array(a)) => a.iter().map(str_of).collect::<Vec<_>>().join(", "),
                                        v => v.map(str_of).unwrap_or_default(),
                                    };
                                    // 沒有逗號＝tag 黏成一串分不開：第一次重寫，第二次就只用程式加的 tag
                                    let tags = if tags.contains(',') { tags } else if attempt == 0 { return None } else { String::new() };
                                    return Some(p.assemble_anima(&tags, &de, idea));
                                }
                                let op = o.get("opening").map(str_of).filter(|s| !s.trim().is_empty())?;
                                Some(p.assemble(&op, &de))
                            });
                            if out.is_some() {
                                break;
                            }
                            em.emit("reset", json!({})).await?;
                        }
                        r.insert("pose".into(), json!({"id": p.id, "zh": p.zh, "how": how,
                            "control": p.control.as_ref().filter(|_| use_control).map(|c| json!({"depth": c.depth, "pose": c.pose}))}));
                        match out {
                            Some(prompt) => (prompt, crate::poses::wh_ratio(&orient).to_string(), timings, false),
                            None => (idea.to_string(), String::new(), timings, true),
                        }
                    }
                    None => {
                        let msgs = json!([
                            {"role": "system", "content": sys_adult},
                            {"role": "user", "content": idea},
                        ]);
                        let (raw, _, t) = self
                            .chat(core, &tune_for(&model_name, payload(&msgs, temperature, seed, false, Some(1500), true)), em, false)
                            .await?;
                        let text = THINK.replace_all(&raw, "");
                        let text = FENCE.replace_all(strip(&text), "");
                        let text = if anima { crate::poses::safe_anima(text.trim(), idea) } else { crate::poses::adultify(text.trim()) };
                        r.insert("pose".into(), json!({"id": null, "how": if how.is_empty() { "沒對到姿勢".to_string() } else { how }}));
                        let empty = text.is_empty();
                        (if empty { idea.to_string() } else { text }, String::new(), t, empty)
                    }
                };
                let tps = match timings.get("predicted_per_second").and_then(py::float) {
                    Some(x) if x != 0.0 => json!(py::round(x, 1)),
                    _ => json!(0),
                };
                r.insert("prompt".into(), json!(prompt));
                r.insert("wh_ratio".into(), json!(ratio));
                r.insert("ratio_follow".into(), json!(""));
                r.insert("fallback".into(), json!(fallback));
                r.insert("note".into(), json!(if fallback { "模型輸出的格式壞掉，先退回原文。可以按「重新改寫」再試一次。" } else { "" }));
                r.insert("reasoning".into(), json!(""));
                r.insert("tokens".into(), timings.get("predicted_n").cloned().unwrap_or(Value::Null));
                r.insert("tps".into(), tps);
                Ok(r)
            }
            .await;
            core.set_busy(0);
            let mut r = r?;
            if !truthy(r.get("fallback")) {
                core.cache_put(&key, &Value::Object(r.clone()));
            }
            r.insert("cached".into(), json!(false));
            r.insert("load_seconds".into(), json!(py::round(load, 2)));
            r.insert(
                "seconds".into(),
                json!(py::round(t0.elapsed().as_secs_f64(), 2)),
            );
            r
        };
        result.insert("model_kind".into(), json!("adult"));
        if let Some(i) = &refined {
            result.insert("idea".into(), json!(i));
        }
        if want_zh && !truthy(result.get("fallback")) {
            let prompt = result.get("prompt").map(str_of).unwrap_or_default();
            let (zh, secs) = self.zh(core, &prompt, em).await?;
            result.insert("zh".into(), json!(zh));
            result.insert("zh_seconds".into(), json!(secs));
        }
        let mut last: Map<String, Value> = [
            "seconds",
            "load_seconds",
            "tokens",
            "tps",
            "cached",
            "model_kind",
        ]
        .iter()
        .map(|k| {
            (
                k.to_string(),
                result.get(*k).cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
        last.insert("at".into(), json!(now()));
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Value::Object(last);
        if truthy(Some(&self.opt("pe_close_after"))) && !self.locked() {
            let c = core.clone();
            if let Err(e) = run_blocking(move || c.stop("設定：改寫完立刻關閉")).await {
                eprintln!("改寫完關閉 llama-server 失敗：{e}");
            }
        } else {
            core.touch();
        }
        self.push_status();
        Ok(Value::Object(result))
    }

    /// 英文結果 → 繁體中文摘要（另一次請求，用目前已載入的模型）
    async fn zh(
        &self,
        core: &Arc<Core>,
        prompt: &str,
        em: &Emitter,
    ) -> Result<(String, f64), Fail> {
        let mut parts = vec![("task", json!("zh")), ("idea", json!(prompt))];
        if let Some(t) = core.override_text("zh") {
            parts.push(("system", json!(sha256_hex(t.as_bytes()))));
        }
        let key = core.cache_key(&parts, None);
        if let Some(hit) = core.cache_get(&key) {
            return Ok((hit.get("prompt").map(str_of).unwrap_or_default(), 0.0));
        }
        let t0 = Instant::now();
        em.emit("status", json!({"phase": "zh", "msg": "產生中文對照…"}))
            .await?;
        let raw = {
            let _g = self.acquire(em).await?;
            let st = core.read_state();
            let ngl = self.ngl();
            let kind = match st.get("kind") {
                Some(k) if !st.is_empty() && st.get("ngl").and_then(py::int) == Some(ngl) => {
                    general_kind(&str_of(k)).to_string()
                }
                _ => "adult".to_string(),
            };
            self.ensure(core, &kind, ngl, em).await?;
            let msgs = core.build_messages("zh", prompt, &[], "", "", false, "")?;
            core.set_busy(1);
            let r = self
                .chat(
                    core,
                    &payload(&msgs, 0.7, 0, false, Some(1024), true),
                    em,
                    true,
                )
                .await;
            core.set_busy(0);
            r?.0
        };
        let zh = parse_output(&raw, "zh", "")
            .map(|p| p.prompt)
            .unwrap_or_default();
        if !zh.is_empty() {
            core.cache_put(&key, &json!({"prompt": zh}));
        }
        Ok((zh, py::round(t0.elapsed().as_secs_f64(), 2)))
    }

    /// 導演台用：交給通用版去審查模型（35B／9B，有 mmproj 可以看圖）一次對話，回傳去掉思考和 ``` 的文字。
    /// images＝ComfyUI input 裡的檔名。進度（載入中、寫了幾個字）用 hub 送 {"type":"director_llm", rid, msg}；
    /// 網頁按取消走 /api/pe/cancel（同一個 rid）。
    pub async fn general_chat(
        self: &Arc<Self>,
        rid: &str,
        system: &str,
        user: &str,
        images: &[String],
        max_tokens: i64,
        temperature: f64,
    ) -> Result<String, String> {
        self.general_chat_as(rid, system, user, images, max_tokens, temperature, None).await
    }

    /// 同上，但可以指定通用模型：None＝照設定頁的 pe_general（auto 會優先 35B），
    /// "35b"／"9b" 強制那一顆（漫畫台排分鏡預設就用 35B）。
    #[allow(clippy::too_many_arguments)]
    pub async fn general_chat_as(
        self: &Arc<Self>,
        rid: &str,
        system: &str,
        user: &str,
        images: &[String],
        max_tokens: i64,
        temperature: f64,
        model: Option<&str>,
    ) -> Result<String, String> {
        let core = self
            .core
            .clone()
            .ok_or_else(|| self.error.clone().unwrap_or_default())?;
        let c = core.clone();
        let wanted = model.map(str::to_owned);
        self.hub_chat(&core, rid, "director_llm", true, |em| async move {
            let core = c;
            let imgs = if images.is_empty() {
                vec![]
            } else {
                let (dir, names) = (self.input_dir.clone(), images.to_vec());
                run_blocking(move || Service::load_images(&dir, &names)).await?
            };
            let choice = wanted.unwrap_or_else(|| self.opt("pe_general").as_str().unwrap_or("auto").to_string());
            core.set_general(&choice);
            // 有圖要掛 mmproj（adult_vision）；只寫文字時，已經開著的 adult_vision 也能用，省得重新載入
            let c = core.clone();
            let running_kind = run_blocking(move || c.read_state().get("kind").and_then(Value::as_str).map(str::to_owned)).await;
            let kind = if !imgs.is_empty() || running_kind.as_deref() == Some("adult_vision") { "adult_vision" } else { "adult" };
            let model_name = file_name(&core.model_files(kind, None)?.0);
            let content = if imgs.is_empty() {
                json!(user)
            } else {
                let mut parts: Vec<Value> = imgs.iter().map(|b| image_part(b)).collect();
                parts.push(json!({"type": "text", "text": user}));
                Value::Array(parts)
            };
            let msgs = json!([{"role": "system", "content": system}, {"role": "user", "content": content}]);
            crate::diagnostics::snapshot("general_chat_request",&json!({"rid":rid,"messages":msgs,"max_tokens":max_tokens,"temperature":temperature,"kind":kind,"model":model_name}));
            let _g = self.acquire(&em).await?;
            self.ensure(&core, kind, self.ngl(), &em).await?;
            em.emit("status", json!({"phase": "generating", "msg": "撰寫中…"})).await?;
            core.set_busy(1);
            let seed = (now() * 1000.0) as i64;
            let out = self
                .chat(&core, &tune_for(&model_name, payload(&msgs, temperature, seed, false, Some(max_tokens), true)), &em, false)
                .await;
            core.set_busy(0);
            let (raw, _, _) = out?;
            Ok(clean_reply(&raw))
        })
        .await
    }

    /// 生圖助手用：一次多輪對話。messages 已經組好（圖片是 data URL，見 image_url_jpeg）。
    /// brain＝9b／35b：本機用哪一顆（不看設定頁的 pe_general）；remote＝雲端 llama-server 網址（SSH 通道），有給就不碰本機的。
    /// think＝思考預算（None＝不想、-1＝不限，見 with_think_budget）。
    /// 進度用 hub 送 {"type":"agent_llm", rid, msg}；取消走 cancel(rid)。回傳 (內容, 思考)
    #[allow(clippy::too_many_arguments)]
    pub async fn agent_chat(
        self: &Arc<Self>,
        rid: &str,
        messages: &Value,
        max_tokens: i64,
        temperature: f64,
        think: Option<i64>,
        brain: &str,
        remote: Option<&Remote>,
    ) -> Result<(String, String), String> {
        crate::diagnostics::snapshot("assistant_request", &json!({"rid":rid,"messages":messages,"max_tokens":max_tokens,"temperature":temperature,"think":think,"brain":brain,"remote":remote.is_some()}));
        let core = self
            .core
            .clone()
            .ok_or_else(|| self.error.clone().unwrap_or_default())?;
        let (c, local) = (core.clone(), remote.is_none());
        self.hub_chat(&core, rid, "agent_llm", local, |em| async move {
            let core = c;
            let seed = (now() * 1000.0) as i64;
            let body = |model: &str| tune_for(model, ban_soft_words(with_think_budget(payload(messages, temperature, seed, think.is_some(), Some(max_tokens), true), think)));
            let busy = if think.is_some() { "思考中…" } else { "撰寫中…" };
            let out = match remote {
                Some(r) => {
                    em.emit("status", json!({"phase": "generating", "msg": busy})).await?;
                    // 自己架的雲端一律是 35B-A3B（MoE，模型名稱 remote-a3b）；網址模式用使用者填的名稱
                    let mut b = body(&r.model);
                    b["model"] = json!(r.model);
                    if r.key.is_some() || r.model != "remote-a3b" {
                        // 別家伺服器（vLLM、OpenRouter…）不一定認得 llama.cpp 專用的欄位
                        if let Some(m) = b.as_object_mut() {
                            m.remove("timings_per_token");
                            m.remove("repeat_penalty");
                            m.remove("thinking_budget_tokens");
                            m.remove("logit_bias"); // OpenAI 那種要 token id，字串版只有 llama.cpp 認得
                        }
                    }
                    self.chat_at(&r.base, r.key.as_deref(), &b, &em, false).await
                }
                None => {
                    let kind = agent_kind(brain);
                    let model_name = file_name(&core.model_files(&kind, None)?.0);
                    let _g = self.acquire(&em).await?;
                    self.ensure(&core, &kind, self.ngl(), &em).await?;
                    em.emit("status", json!({"phase": "generating", "msg": busy})).await?;
                    core.set_busy(1);
                    let out = self.chat(&core, &body(&model_name), &em, false).await;
                    core.set_busy(0);
                    out
                }
            };
            let (raw, reasoning, _) = out?;
            Ok((clean_reply(&raw), reasoning))
        })
        .await
    }

    /// 背景對話共用：登記取消、把 SSE 事件轉成 hub 訊息 {"type": event, rid, msg}（狀態原文照送，思考／內容只報字數，一秒最多一次），
    /// 跑完收尾（local＝用了本機 llama-server：重設閒置計時，或照設定立刻關閉）
    async fn hub_chat<T, Fut>(
        &self,
        core: &Arc<Core>,
        rid: &str,
        event: &'static str,
        local: bool,
        work: impl FnOnce(Emitter) -> Fut,
    ) -> Result<T, String>
    where
        Fut: std::future::Future<Output = Result<T, Fail>>,
    {
        let cancel = CancellationToken::new();
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(rid.to_string(), cancel.clone());
        let (tx, mut rx) = mpsc::channel::<Bytes>(64);
        let (hub, rid2) = (self.hub.clone(), rid.to_string());
        let drain = tokio::spawn(async move {
            let (mut chars, mut thought, mut last) = (0usize, 0usize, Instant::now());
            while let Some(b) = rx.recv().await {
                let text = String::from_utf8_lossy(&b);
                let mut ev = "";
                for line in text.lines() {
                    if let Some(e) = line.strip_prefix("event: ") {
                        ev = match e {
                            "status" => "status",
                            "content" => "content",
                            "reasoning" => "reasoning",
                            _ => "",
                        };
                    } else if let Some(d) = line.strip_prefix("data: ") {
                        let v: Value = serde_json::from_str(d).unwrap_or(Value::Null);
                        let n = v.get("text").and_then(Value::as_str).map(|t| t.chars().count()).unwrap_or(0);
                        let msg = match ev {
                            "status" => {
                                hub.send(json!({"type": event, "rid": rid2, "msg": v.get("msg").cloned().unwrap_or(Value::Null)}));
                                continue;
                            }
                            "content" => {
                                chars += n;
                                format!("撰寫中…（{chars} 字）")
                            }
                            "reasoning" => {
                                thought += n;
                                format!("思考中…（{thought} 字）")
                            }
                            _ => continue,
                        };
                        if last.elapsed() > Duration::from_secs(1) {
                            last = Instant::now();
                            hub.send(json!({"type": event, "rid": rid2, "msg": msg}));
                        }
                    }
                }
            }
        });
        let r = work(Emitter { tx, cancel }).await;
        let _ = drain.await;
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(rid);
        if local {
            if truthy(Some(&self.opt("pe_close_after"))) && !self.locked() {
                let c = core.clone();
                if let Err(e) = run_blocking(move || c.stop("設定：改寫完立刻關閉")).await {
                    eprintln!("改寫完關閉 llama-server 失敗：{e}");
                }
            } else {
                core.touch();
            }
            self.push_status();
        }
        match r {
            Ok(t) => Ok(t),
            Err(Fail::Cancelled) => Err("已取消".into()),
            Err(Fail::Error(e)) => Err(e),
        }
    }

    /// POST /api/pe/rewrite：SSE 串流 status／reasoning／content／zh_delta／result／error／cancelled 事件
    pub fn rewrite_sse(self: &Arc<Self>, body: Value) -> axum::response::Response {
        use axum::http::header;
        use axum::response::IntoResponse;
        let rid = str_of(py::or(body.get("rid"), None).unwrap_or(&json!(now())));
        let cancel = CancellationToken::new();
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(rid.clone(), cancel.clone());
        let (tx, rx) = mpsc::channel::<Bytes>(64);
        let me = self.clone();
        crate::diagnostics::spawn(async move {
            let em = Emitter { tx, cancel };
            let r = match &me.core {
                None => Err(Fail::Error(me.error.clone().unwrap_or_default())),
                Some(core) => me.rewrite(core, &body, &em).await,
            };
            let r = match r {
                Ok(res) => em.emit("result", res).await,
                Err(e) => Err(e),
            };
            match r {
                Ok(()) => {}
                Err(Fail::Cancelled) => {
                    eprintln!("改寫已取消（{rid}）");
                    let _ = em.emit("cancelled", json!({})).await;
                }
                Err(Fail::Error(msg)) => {
                    eprintln!("改寫失敗：{msg}");
                    let _ = em.emit("error", json!({"message": msg})).await;
                }
            }
            me.cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&rid);
            if let Some(core) = &me.core {
                core.set_busy(0);
            }
        });
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
        });
        (
            [
                (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
                (header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            axum::body::Body::from_stream(stream),
        )
            .into_response()
    }
}

/// 在背景執行緒跑會卡住的工作（檔案鎖、等 llama-server 載入、關程序）
async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    crate::diagnostics::blocking(f).await
}

#[cfg(test)]
mod tests {
    /// 2026-10-26 朋友（AMD 9070 16GB）回報的 log：只認這次啟動之後的顯存不足，舊的那次不算
    #[test]
    fn vram_short_reads_only_the_latest_start() {
        let amd = "[anima-pe 2026-10-26 10:00:00] start llama-server.exe -m Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf\n\
            ggml_vulkan: Device memory allocation of size 1053204992 failed.\n\
            ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory\n\
            0.03.835.605 E llama_model_load: error loading model: unable to allocate Vulkan0 buffer\n";
        assert!(vram_short(amd));
        assert!(vram_short("[anima-pe x] start a\nggml_backend_cuda_buffer_type_alloc_buffer: allocating 5000.00 MiB on device 0: cudaMalloc failed: out of memory\n"));
        // 上一次爆、這一次是別的錯：不算
        let later = format!("{amd}[anima-pe 2026-10-26 10:01:00] start llama-server.exe -m x.gguf\nerror: invalid argument: -ncmoe\n");
        assert!(!vram_short(&later));
        assert!(!vram_short("[anima-pe x] start a\nerror loading model: file not found\n"));
    }

    /// 顯存要多少：一般模型＝檔案＋mmproj＋1.5GB；MoE 照實測（GPU 約 7.6GB、部分約 3GB）
    #[test]
    fn vram_need_by_model_kind() {
        const G: u64 = 1 << 30;
        assert_eq!(vram_need(5 * G, G, false, 99), 7 * G + G / 2);
        assert_eq!(vram_need(20 * G, G, true, 99), 8 * G);
        assert_eq!(vram_need(20 * G, G, true, 16), 3 * G + G / 2);
    }

    /// /free 之後等顯存：夠了就走、到上限就走、一陣子都沒再增加（ComfyUI 放完了）也走；不能一開始還沒動就走
    #[test]
    fn vram_wait_stops_when_enough_stable_or_timeout() {
        const G: u64 = 1 << 30;
        let mut w = VramWait::new(8 * G, Duration::from_secs(20), Duration::from_secs(4));
        assert_eq!(w.push(0.0, 2 * G), None);
        assert_eq!(w.push(1.0, 2 * G), None, "ComfyUI 可能還沒開始卸載");
        assert_eq!(w.push(2.0, 5 * G), None);
        assert_eq!(w.push(2.5, 9 * G), Some("enough"));

        let mut w = VramWait::new(8 * G, Duration::from_secs(20), Duration::from_secs(4));
        for t in [0.0, 1.0, 2.0] {
            assert_eq!(w.push(t, 3 * G), None);
        }
        assert_eq!(w.push(3.0, 6 * G), None);
        assert_eq!(w.push(5.0, 6 * G), None, "增加後還沒穩定滿 4 秒");
        assert_eq!(w.push(7.5, 6 * G), Some("stable"));

        let mut w = VramWait::new(8 * G, Duration::from_secs(3), Duration::from_secs(10));
        let mut t = 0.0;
        let mut got = None;
        while got.is_none() && t < 10.0 {
            got = w.push(t, t as u64 * G / 4);
            t += 0.5;
        }
        assert_eq!(got, Some("timeout"));
    }

    /// 助手選 35B、機器上只有 9B：要講出來，不能默默換掉
    #[test]
    fn brain_fallback_is_reported() {
        assert!(brain_fell_back("35b", "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf"));
        assert!(!brain_fell_back("35b", "Qwen3.6-35B-A3B-heretic.Q4_K_M.gguf"));
        assert!(!brain_fell_back("9b", "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf"));
        assert!(!brain_fell_back("cloud", "whatever.gguf"));
    }

    #[test]
    fn clearing_rewrite_cache_compacts_and_next_request_can_cache_again() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../待刪除").join(format!("pe-cache-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&base).unwrap();
        let core = Core::with_dirs(&base, &base, &base);
        core.cache_put("fixture", &json!({"prompt":"x".repeat(100_000)}));
        assert!(core.cache_get("fixture").is_some());
        assert!(core.clear_cache().unwrap() > 0);
        assert!(core.cache_get("fixture").is_none());
        core.cache_put("again", &json!({"prompt":"works"}));
        assert_eq!(core.cache_get("again").unwrap()["prompt"], "works");
    }
    #[test]
    fn bundled_prompts_beat_the_old_node_copy() {
        // 已安裝的節點資料夾留著舊版 anima.txt：程式附帶的新版優先，快取鍵要分開
        let base = std::env::temp_dir().join(format!("anima_pe_bundled_{}", std::process::id()));
        let (node, res) = (base.join("node"), base.join("res"));
        std::fs::create_dir_all(&node).unwrap();
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(node.join("anima.txt"), "OLD rules\r\n").unwrap();
        std::fs::write(res.join("anima.txt"), "NEW rules\n").unwrap();
        std::fs::write(node.join("zh_summary.txt"), "same\n").unwrap();
        std::fs::write(res.join("zh_summary.txt"), "same\r\n").unwrap();
        let mut core = Core::with_dirs(&node, &node, &base);
        assert_eq!(core.default_prompt("anima").unwrap(), "OLD rules");
        assert_eq!(core.default_differs_from_node("anima"), None);
        core.bundled = Some(res.clone());
        assert_eq!(core.default_prompt("anima").unwrap(), "NEW rules");
        assert_eq!(core.default_differs_from_node("anima").as_deref(), Some("NEW rules"));
        assert_eq!(core.default_differs_from_node("zh"), None, "內容一樣（只差換行）就跟節點共用快取");
        // 程式沒附帶的檔照舊讀節點
        std::fs::write(node.join("refine.txt"), "node only\n").unwrap();
        assert_eq!(core.default_prompt("refine").unwrap(), "node only");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn embedding_model_is_found_but_never_used_as_the_brain() {
        let base = std::env::temp_dir().join(format!("anima_pe_embed_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let core = Core::with_dirs(&base, &base, &base);
        std::fs::write(base.join("Qwen3-Embedding-0.6B-Q8_0.gguf"), b"test").unwrap();
        assert_eq!(core.embedding_model().map(|p| file_name(&p)).as_deref(), Some("Qwen3-Embedding-0.6B-Q8_0.gguf"));
        assert!(core.model_files("adult", None).is_err(), "只有意思座標模型時不能拿它當腦袋");
        std::fs::write(base.join("Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf"), b"test").unwrap();
        assert_eq!(file_name(&core.model_files("adult", None).unwrap().0), "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf");
        assert!(is_embedding("Qwen3-Embedding-0.6B-Q8_0.gguf") && !is_embedding("mmproj-embedding.gguf") && !is_embedding("Qwen3.5-9B.gguf"));
        // 兩組 Qwen3.5 看圖檔：配跟模型檔名最像的（照字母排會配到 Aria 那個）
        std::fs::write(base.join("Aria-Qwen3.5-9B-mmproj-F16.gguf"), b"test").unwrap();
        std::fs::write(base.join("Huihui-Qwen3.5-9B-abliterated.mmproj-f16.gguf"), b"test").unwrap();
        let (_, mm) = core.model_files("adult_vision", None).unwrap();
        assert_eq!(file_name(&mm.unwrap()), "Huihui-Qwen3.5-9B-abliterated.mmproj-f16.gguf");
        // 擂台贏的 Defiant Fable 放進來：它優先，看圖檔也配它自己的
        std::fs::write(base.join("Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-Q4_K_M.gguf"), b"test").unwrap();
        std::fs::write(base.join("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf"), b"test").unwrap();
        let (m, mm) = core.model_files("adult_vision", None).unwrap();
        assert_eq!(file_name(&m), "Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-Q4_K_M.gguf");
        assert_eq!(file_name(&mm.unwrap()), "Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf");
        std::fs::remove_dir_all(&base).ok();
    }

    /// 2026-10-29 使用者：「讓別人自己決定要用什麼模型，但同時又要確保通用性」
    #[test]
    fn users_pick_their_own_general_model() {
        let base = std::env::temp_dir().join(format!("anima_pe_pick_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let core = Core::with_dirs(&base, &base, &base);
        let put = |n: &str| std::fs::write(base.join(n), b"test").unwrap();
        // 別人做的 Qwen3.5 9B（不是 Fable、也不是 Huihui）自動模式也認得
        put("Qwen3.5-9B-heretic-v2-Q4_K_M.gguf");
        put("mmproj-Qwen3.5-9b-heretic-v2-F16.gguf");
        assert_eq!(file_name(&core.model_files("adult", None).unwrap().0), "Qwen3.5-9B-heretic-v2-Q4_K_M.gguf");
        // 官方 PE、挑段模型不會出現在選單上
        put("pe_t2i_heretic-Q4_K_M.gguf");
        put("Qwen3-Embedding-0.6B-Q8_0.gguf");
        put("gemma-3-12b-it-abliterated-Q4_K_M.gguf");
        put("Qwen3.6-35B-A3B-heretic.gguf");
        let names: Vec<String> = core.general_choices().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["Qwen3.5-9B-heretic-v2-Q4_K_M.gguf", "Qwen3.6-35B-A3B-heretic.gguf", "gemma-3-12b-it-abliterated-Q4_K_M.gguf"]);
        let gemma = core.general_choices().into_iter().find(|c| c["name"] == "gemma-3-12b-it-abliterated-Q4_K_M.gguf").unwrap();
        assert!(gemma["qwen"] == false && gemma["vision"] == false && gemma["big"] == false);
        // 自動：有 35B 用 35B
        assert_eq!(file_name(&core.model_files("adult", None).unwrap().0), "Qwen3.6-35B-A3B-heretic.gguf");
        // 指定別家的檔：照用；看圖檔配同系列的（gemma），沒有就講清楚要放什麼，不給 Qwen 的下載
        core.set_general("gemma-3-12b-it-abliterated-Q4_K_M.gguf");
        assert_eq!(file_name(&core.model_files("adult", None).unwrap().0), "gemma-3-12b-it-abliterated-Q4_K_M.gguf");
        let e = core.model_files("adult_vision", None).unwrap_err().0;
        assert!(e.contains("gemma") && e.contains("mmproj"), "{e}");
        put("mmproj-gemma-3-12b-it-f16.gguf");
        assert_eq!(file_name(&core.model_files("adult_vision", None).unwrap().1.unwrap()), "mmproj-gemma-3-12b-it-f16.gguf");
        // 助手指定 35B 腦袋：指定的檔不是那一類，就用那一類的；指定 9B：指定的檔算 9B 那類，用它
        assert_eq!(file_name(&core.model_files("adult@35b", None).unwrap().0), "Qwen3.6-35B-A3B-heretic.gguf");
        assert_eq!(file_name(&core.model_files("adult@9b", None).unwrap().0), "gemma-3-12b-it-abliterated-Q4_K_M.gguf");
        // 指定的檔不見了：照自動挑，不會整個壞掉
        core.set_general("deleted-model.gguf");
        assert_eq!(file_name(&core.model_files("adult", None).unwrap().0), "Qwen3.6-35B-A3B-heretic.gguf");
        assert!(is_big_brain("Llama-3.3-70B-Q4.gguf") && is_big_brain("Qwen3-30B-A3B.gguf") && !is_big_brain("Qwen3.5-9B-Q4.gguf"));
        std::fs::remove_dir_all(&base).ok();
    }

    /// 只剩官方 PE 的舊安裝：當成沒有改寫模型（講清楚去下載通用模型），PE 檔永遠不會被載入；
    /// 通用模型放在外部模型資料夾的子資料夾也找得到；舊的 kind 名稱（server_state 可能留著 t2i／i2i）照通用模型處理
    #[test]
    fn general_models_search_every_llm_root_and_never_pick_pe() {
        let base = std::env::temp_dir().join(format!("anima_pe_roots_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let own = base.join("own");
        let extra = base.join("extra").join("LLM9B");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(own.join("pe_t2i_heretic-Q4_K_M.gguf"), b"test").unwrap();
        std::fs::write(own.join("pe_i2i_heretic-Q4_K_M.gguf"), b"test").unwrap();
        std::fs::write(own.join("pe_i2i_heretic.mmproj-bf16.gguf"), b"test").unwrap();
        let mut c = Core::with_dirs(&base, &base, &own);
        let e = c.model_files("adult", None).unwrap_err().0;
        assert!(e.contains("安裝與修復") && e.contains("AI 改寫、生圖助手"), "{e}");
        assert!(c.model_files("t2i", None).is_err() && c.model_files("i2i", None).is_err());
        let fable = extra.join("Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-IQ3_M.gguf");
        std::fs::write(&fable, b"test").unwrap();
        std::fs::write(extra.join("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf"), b"test").unwrap();
        c.model_dirs.push(base.join("extra"));
        assert_eq!(c.model_files("adult", None).unwrap().0, fable);
        assert_eq!(c.model_files("t2i", None).unwrap(), (fable.clone(), None));
        let (m, mm) = c.model_files("i2i", None).unwrap();
        assert_eq!(m, fable);
        assert_eq!(mm.unwrap(), extra.join("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf"));
        assert!(c.available().get("t2i").is_none() && c.available()["adult"].is_string());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// 2026-10-30 使用者：「把PE版的改寫模型在下一個版本直接砍掉…讓下一個更新的人，自動把舊模型真刪除，換新的上去。」
    /// 要刪的：站找得到的官方 PE（不分大小寫、各種量化、看圖檔）；換好新 9B 才刪的：程式以前發的 Huihui 9B（模型＋看圖檔）。
    /// 使用者自己放的別家模型、35B 不動
    #[test]
    fn retire_plan_finds_pe_and_old_huihui() {
        let base = std::env::temp_dir().join(format!("anima_pe_retire_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let own = base.join("own");
        let extra = base.join("extra").join("sub");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        let put = |d: &Path, n: &str| std::fs::write(d.join(n), b"test").unwrap();
        let names = |v: &[PathBuf]| v.iter().map(|p| file_name(p)).collect::<Vec<_>>();
        let mut c = Core::with_dirs(&base, &base, &own);
        let p = c.retire_plan();
        assert!(p.pe.is_empty() && p.old.is_empty() && !p.has_fable && !p.had_llm);
        put(&own, "pe_t2i_heretic-Q4_K_M.gguf");
        put(&own, "pe_i2i_heretic.mmproj-bf16.gguf");
        put(&extra, "PE_I2I_HERETIC-Q6_K.gguf");
        put(&own, "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf");
        put(&own, "Huihui-Qwen3.5-9B-abliterated.mmproj-f16.gguf");
        put(&own, "Qwen3.6-35B-A3B-heretic.gguf");
        put(&own, "Qwen3.5-9B.mmproj-bf16.gguf");
        // 下載到一半的暫存檔（api_models 的 <檔名>.verify／.verify.part）也一起刪，不留幾 GB 的殘檔
        put(&own, "pe_t2i_heretic-Q4_K_M.gguf.verify");
        put(&extra, "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf.verify.part");
        c.model_dirs.push(base.join("extra"));
        let mut p = c.retire_plan();
        p.pe.sort_by_key(|x| file_name(x).to_lowercase());
        p.old.sort_by_key(|x| file_name(x).to_lowercase());
        assert_eq!(
            names(&p.pe),
            ["PE_I2I_HERETIC-Q6_K.gguf", "pe_i2i_heretic.mmproj-bf16.gguf", "pe_t2i_heretic-Q4_K_M.gguf", "pe_t2i_heretic-Q4_K_M.gguf.verify"]
        );
        assert_eq!(
            names(&p.old),
            [
                "Huihui-Qwen3.5-9B-abliterated.mmproj-f16.gguf",
                "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf",
                "Huihui-Qwen3.5-9B-abliterated.Q4_K_M.gguf.verify.part"
            ]
        );
        assert!(!p.has_fable && p.had_llm);
        // 新 9B 下載到一半不算有
        put(&own, "Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-IQ3_M.gguf.verify.part");
        assert!(!c.retire_plan().has_fable);
        put(&own, "Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-IQ3_M.gguf");
        assert!(c.retire_plan().has_fable);
        // 只有 PE、沒有任何通用模型：也算用過改寫（要換上新的）
        let only = base.join("only");
        std::fs::create_dir_all(&only).unwrap();
        put(&only, "pe_t2i_heretic-Q4_K_M.gguf");
        let p = Core::with_dirs(&base, &base, &only).retire_plan();
        assert!(p.had_llm && !p.has_fable && p.old.is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// 提示詞檔是 CRLF（git 在 Windows checkout）也讀成 \n，跟 Python 版一致（GitHub CI 抓到的）
    #[test]
    fn default_prompt_normalizes_crlf() {
        let base = std::env::temp_dir().join(format!("anima_pe_crlf_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let (file, _) = task_info("caption").unwrap();
        std::fs::write(base.join(file), "line one\r\n\r\nline two\r\n").unwrap();
        let c = Core::with_dirs(&base, &base, &base);
        assert_eq!(c.default_prompt("caption").unwrap(), "line one\n\nline two");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 9B 缺讀圖用的 mmproj：訊息要有下載網址、整理過的資料夾（沒有 ..）、按鈕名字；有了就不算缺；35B 沒有來源就不給按鈕
    #[test]
    fn mmproj_missing_tells_where() {
        let base = std::env::temp_dir().join("anima_mmproj_test");
        let _ = std::fs::remove_dir_all(&base);
        let llm = base.join("models").join("LLM");
        std::fs::create_dir_all(&llm).unwrap();
        std::fs::create_dir_all(base.join("node").join("x")).unwrap();
        std::fs::write(llm.join("Qwen3.5-9B-The-Defiant-Fable-Uncnr-Heretic-NEO-MAX-Q4_K_M.gguf"), b"").unwrap();
        let models = base.join("node").join("x").join("..").join("..").join("models").join("LLM");
        let mut c = Core::with_dirs(&base.join("node"), &base.join("node"), &models);
        c.res = Some(crate::paths::default_res());
        let e = c.model_files("adult_vision@9b", None).unwrap_err().0;
        assert!(e.contains(MMPROJ_BUTTON) && e.contains("huggingface.co/DavidAU/Qwen3.5-9B-The-Defiant-Fable-"), "{e}");
        assert!(e.contains("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf") && !e.contains(".."), "{e}");
        let (dest, url, size, sha) = c.mmproj_download(Some("9b")).unwrap().unwrap();
        assert_eq!(dest, models.join("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf"));
        assert!(url.ends_with("/mmproj-F16.gguf") && size > 500_000_000 && sha.is_some());
        // 放好以後就不缺了
        std::fs::write(llm.join("Qwen3.5-9B-Defiant-Fable-mmproj-F16.gguf"), b"").unwrap();
        assert!(c.model_files("adult_vision@9b", None).unwrap().1.is_some());
        assert!(c.mmproj_download(Some("9b")).unwrap().is_none());
        // 2026-10-29 起 35B 是安裝精靈的選配包：缺看圖檔時也給下載按鈕，存到 35B 主檔旁邊
        std::fs::write(llm.join("Qwen3.6-35B-A3B-heretic.gguf"), b"").unwrap();
        let e = c.model_files("adult_vision@35b", None).unwrap_err().0;
        assert!(e.contains(MMPROJ_BUTTON) && e.contains("mmproj-Qwen3.6-35B"), "{e}");
        let (dest, url, _, sha) = c.mmproj_download(Some("35b")).unwrap().unwrap();
        assert!(dest.ends_with("mmproj-Qwen3.6-35B-A3B-uncensored-heretic-Native-MTP-Preserved-APEX-F16.gguf"), "{dest:?}");
        assert!(url.contains("SC117/") && sha.is_some(), "{url}");
        let _ = std::fs::remove_dir_all(&base);
    }


    #[test]
    fn nested_json_unwrapped() {
        let raw = r#"{"rewritten_prompt": "{\"rewritten_prompt\": \"The image is a photo.\", \"wh_ratio\": \"2:3\"}", "wh_ratio": "2:3"}"#;
        assert_eq!(parse_output(raw, "t2i", "").unwrap().prompt, "The image is a photo.");
    }

    #[test]
    fn anima_tags_cleanup() {
        // 2026-10-11 實測的幾種矛盾
        let p = "masterpiece, best quality, score_7, sensitive, 2girls, solo, 1girl, kissing, nipples, nipples, cowboy shot, full body, from side, from front, Two women kiss on a sofa.";
        assert_eq!(
            fix_anima_tags(p),
            "masterpiece, best quality, score_7, nsfw, 2girls, kissing, nipples, cowboy shot, from side, Two women kiss on a sofa."
        );
        let p = "masterpiece, score_7, nsfw, 1girl, 1boy, hetero, solo, sex, missionary, on back, on stomach, She lies on her back, sweating.";
        assert_eq!(
            fix_anima_tags(p),
            "masterpiece, score_7, explicit, 1girl, 1boy, hetero, sex, missionary, on back, She lies on her back, sweating."
        );
        // 沒問題的保持原樣（solo 只有一人時保留）
        let p = "masterpiece, best quality, score_7, safe, 1girl, solo, long hair, smile, upper body, She smiles, and the wind blows.";
        assert_eq!(fix_anima_tags(p), p);
        assert!(anima_has_sentences(p));
        let p = "masterpiece, score_8, sensitive, 3girls, heterochromia (if applicable, or distinct eye colors), ganyu (genshin impact), 跪姿, bikini, Three girls splash water.";
        assert_eq!(
            fix_anima_tags(p),
            "masterpiece, score_7, sensitive, 3girls, ganyu (genshin impact), bikini, Three girls splash water."
        );
        assert!(!anima_has_sentences("masterpiece, score_7, safe, 1girl, solo, beach, blue sky"));
    }
    use super::*;

    #[test]
    fn sentences_split_only_before_capitals() {
        assert_eq!(split_sentences("A b. C d. e f."), vec!["A b.", "C d. e f."]);
        assert_eq!(split_sentences(""), vec![""]);
    }

    #[test]
    fn caption_hint_unescapes_and_empty() {
        assert_eq!(caption_hint(&[]), "");
        let h = caption_hint(&["nipples".into(), " ".into(), r"hu tao \(genshin impact\)".into()]);
        assert!(h.ends_with("Tags: nipples, hu tao (genshin impact)"), "{h}");
    }

    #[test]
    fn payload_seed_wraps_like_python() {
        // Python：-3 % 2**31 == 2147483645
        assert_eq!(
            payload(&json!([]), 1.0, -3, false, None, true)["seed"],
            json!(2147483645i64)
        );
    }

    /// 2026-10-30 官方 PE 拿掉了：文字一律通用模型，有圖或看圖寫說明一律通用讀圖模型，沒有退路
    #[test]
    fn rewrite_always_uses_the_general_model() {
        assert_eq!(rewrite_kind("t2i", false, false), "adult");
        assert_eq!(rewrite_kind("zh", false, false), "adult");
        assert_eq!(rewrite_kind("anima", false, false), "adult");
        assert_eq!(rewrite_kind("i2i", false, false), "adult_vision");
        assert_eq!(rewrite_kind("t2i", false, true), "adult_vision");
        assert_eq!(rewrite_kind("caption", true, true), "adult_vision");
    }

    #[test]
    fn payload_sets_thinking_budget() {
        let b = with_think_budget(payload(&json!([]), 0.7, 1, true, Some(2000), true), Some(256));
        assert_eq!((b["thinking_budget_tokens"].clone(), b["chat_template_kwargs"]["enable_thinking"].clone()), (json!(256), json!(true)));
        let unlimited = with_think_budget(payload(&json!([]), 0.7, 1, true, Some(2000), true), Some(-1));
        assert!(unlimited.get("thinking_budget_tokens").is_none());
        let off = with_think_budget(payload(&json!([]), 0.7, 1, false, Some(2000), true), None);
        assert!(off.get("thinking_budget_tokens").is_none() && off["chat_template_kwargs"]["enable_thinking"] == json!(false));
    }

    #[test]
    fn skill_rules_go_after_the_users_words() {
        assert_eq!((skill_guide("t2i", "moss"), skill_guide("t2i", "qwen21"), skill_guide("t2i", ""), skill_guide("anima", "")), (Some("moss"), Some("qwen"), None, Some("anima")));
        let mut m = json!({"role": "user", "content": "海邊"});
        append_text(&mut m, "RULES");
        assert_eq!(m["content"], "海邊\n\nRULES");
        let mut m = json!({"role": "user", "content": [{"type": "image_url"}, {"type": "text", "text": "改背景"}]});
        append_text(&mut m, "RULES");
        assert_eq!(m["content"][1]["text"], "改背景\n\nRULES");
        let mut m = json!({"role": "user", "content": [{"type": "image_url"}]});
        append_text(&mut m, "RULES");
        assert_eq!(m["content"][1], json!({"type": "text", "text": "RULES"}));
    }

    #[test]
    fn looping_catches_a_model_copying_itself() {
        // 2026-10-28 擂台 9B 把整份提示詞一直重抄（節錄同樣的句型）
        let block = "1girl, 1boy, hetero, long hair, red hair, large breasts, kneeling, on knees, upright torso, hands on breasts, holding breasts, thighs, bed, center, left pillow, upper right, downlooking, sunlight, warm colors, ";
        let start = "{\"action\":\"generate\",\"prompt\":\"masterpiece, best quality, ";
        assert!(!looping(&format!("{start}{block}")));
        assert!(!looping(&format!("{start}{block}{block}")), "重複一次還不算");
        assert!(looping(&format!("{start}{block}{block}{block}")));
        // 短句一直循環、中文（多位元組）也抓得到，切點不會落在字中間
        assert!(looping(&"好的，我再想一下。".repeat(40)));
        assert!(looping(&format!("{}{}", "思考：", "等等，讓我重新確認構圖。".repeat(30))));
        // 正常的長提示詞：用詞會重複，但不會整段一樣
        let normal = (1..=40).map(|i| format!("detail number {i} of the scene, with its own lighting and color note")).collect::<Vec<_>>().join(", ");
        assert!(!looping(&normal));
        assert!(!looping(""));
    }

    /// 假的 llama-server 一直串流同一段：生圖站要自己斷線，不能等它寫到 max_tokens
    #[tokio::test]
    async fn chat_hangs_up_on_a_looping_model() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut req = vec![0u8; 65536];
            let _ = s.read(&mut req).await;
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n").await.unwrap();
            let line = format!("data: {}\n\n", json!({"choices": [{"delta": {"content": "1girl, 1boy, kneeling, paizuri, red hair, large breasts, bed, sunlight, "}}]}));
            let mut sent = 0;
            for _ in 0..3000 {
                if s.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                sent += 1;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            sent
        });
        let dir = std::env::temp_dir().join(format!("anima_pe_loop_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = crate::hub::Hub::new();
        let db = Arc::new(crate::db::Db::open(&dir.join("t.db")).unwrap());
        let svc = Service::new(&dir, &dir, crate::comfy::Comfy::new("http://127.0.0.1:1", db, hub.clone()), &dir.join("settings.json"), hub, &dir);
        let (tx, mut rx) = mpsc::channel(8192);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let em = Emitter { tx, cancel: CancellationToken::new() };
        let Ok((content, _, _)) = svc.chat_at(&base, None, &json!({}), &em, false).await else { panic!("chat_at 失敗") };
        assert!(looping(&content), "停下來的時候已經在重複");
        assert!(content.len() < 2000, "最多多看一輪就要停：收了 {} 位元組", content.len());
        let sent = server.await.unwrap();
        assert!(sent < 3000, "斷線後假伺服器就寫不進去：寫了 {sent} 段");
    }

    #[test]
    fn soft_words_are_banned_as_single_tokens() {
        let b = ban_soft_words(payload(&json!([]), 0.7, 1, false, Some(2000), true));
        let l = b["logit_bias"].as_array().unwrap();
        assert!(l.contains(&json!([" intimate", false])));
        // 每個都要前面帶空白（Qwen 分詞器裡帶空白的才是單一 token），也不能擋到 Anima 的評級 tag sensitive
        assert!(l.iter().all(|x| x[0].as_str().unwrap().starts_with(' ') && x[1] == json!(false)));
        assert!(!l.iter().any(|x| x[0].as_str().unwrap().contains("sensitive")));
    }

    #[test]
    fn visible_text_keeps_exact_strings_without_enforcing_style_quotes() {
        let ask = "黑板上面清楚寫「今日特調」，下面寫「拿鐵 120」";
        assert_eq!(missing_visible_text("Chalk lettering reads \"今日特調\".", ask), vec!["拿鐵 120"]);
        assert!(missing_visible_text("Chalk lettering reads \"今日特調\" and \"拿鐵 120\".", ask).is_empty());
        assert_eq!(missing_visible_text("Today's special, latte 120", ask), vec!["今日特調", "拿鐵 120"]);
        assert!(missing_visible_text("A red dress.", "穿「紅色洋裝」的成年女性").is_empty());
        assert_eq!(missing_visible_text("Autumn books", "海報大標題「秋日書市」，下面「10月27日」"), vec!["秋日書市", "10月27日"]);
        assert_eq!(missing_visible_text("A sign", "The sign reads \"OPEN\""), vec!["OPEN"]);
    }
}

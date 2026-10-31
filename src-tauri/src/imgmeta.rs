//! 圖裡的生成參數（對照舊版 server.py 的 read_params／import_existing），2026-10-29 擴充成「讀圖片參數」：
//! - 生圖站寫的 anima_params
//! - ComfyUI 存的節點圖（"prompt"）：用匯入工作流同一套找法（image_wf::graph_params）推回提示詞、取樣、尺寸、模型、LoRA
//! - WebUI（A1111、Forge、C 站下載的圖）的 parameters 文字；NovelAI、SwarmUI、Fooocus 這類 JSON
//! - 圖檔：PNG 的文字區塊；WebP、JPEG 的 EXIF（ComfyUI 寫在 Make／Model，WebUI 寫在 UserComment）
use crate::db::Db;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Cursor, Seek};
use std::path::Path;
use std::sync::LazyLock;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
/// EXIF 的 UserComment：WebUI 存 JPEG／WebP 時，參數寫在這裡
const USER_COMMENT: u16 = 0x9286;
/// IFD0 裡指向 EXIF 子目錄的欄位（UserComment 在子目錄裡）
const EXIF_IFD: u16 = 0x8769;

/// PNG 的文字區塊（tEXt／zTXt／iTXt，只讀影像資料之前的部分；ComfyUI 和 PIL 都寫在前面）和尺寸
pub fn png_text<R: BufRead + Seek>(r: R) -> Option<(HashMap<String, String>, (u32, u32))> {
    let mut dec = png::Decoder::new(r);
    dec.set_ignore_text_chunk(false);
    let reader = dec.read_info().ok()?;
    let info = reader.info();
    let mut out = HashMap::new();
    for t in &info.uncompressed_latin1_text {
        out.insert(t.keyword.clone(), t.text.clone());
    }
    for t in &info.compressed_latin1_text {
        if let Ok(s) = t.get_text() {
            out.insert(t.keyword.clone(), s);
        }
    }
    for t in &info.utf8_text {
        if let Ok(s) = t.get_text() {
            out.insert(t.keyword.clone(), s);
        }
    }
    Some((out, (info.width, info.height)))
}

/// 帶得了生成資料的圖片格式："png"、"webp"、"jpeg"；別的（或不是圖）是 None
pub fn image_kind(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(PNG_MAGIC) {
        Some("png")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else {
        None
    }
}

/// 圖裡的文字資料和尺寸。PNG：文字區塊；WebP、JPEG：EXIF——ComfyUI 寫成 "prompt:…"、"workflow:…" 的拆成 prompt、workflow 兩格，
/// UserComment（WebUI 的參數）放進 parameters（跟 PNG 一樣的名字）。尺寸讀不出來是 (0, 0)；不是這三種圖、PNG 壞掉是 None
pub fn meta_text(b: &[u8]) -> Option<(HashMap<String, String>, (u32, u32))> {
    let exif = match image_kind(b)? {
        "png" => return png_text(Cursor::new(b)),
        "webp" => webp_chunk(b, b"EXIF"),
        _ => jpeg_exif(b),
    };
    let mut out = HashMap::new();
    for (tag, t) in exif.map(exif_fields).unwrap_or_default() {
        if tag == USER_COMMENT {
            if !t.trim().is_empty() {
                out.insert("parameters".to_string(), t);
            }
        } else if let Some((k @ ("prompt" | "workflow"), v)) = t.split_once(':') {
            out.insert(k.to_string(), v.to_string());
        }
    }
    let size = image::ImageReader::new(Cursor::new(b))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((0, 0));
    Some((out, size))
}

/// WebP（RIFF）裡第一個 fourcc 區塊的內容
fn webp_chunk<'a>(b: &'a [u8], fourcc: &[u8; 4]) -> Option<&'a [u8]> {
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let size = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().ok()?) as usize;
        let start = pos + 8;
        if &b[pos..pos + 4] == fourcc {
            return b.get(start..start.saturating_add(size).min(b.len()));
        }
        // 區塊長度是奇數時後面補一個位元組
        pos = start.saturating_add(size).saturating_add(size & 1);
    }
    None
}

/// JPEG 的 APP1 Exif 區段（"Exif\0\0" 後面那段 TIFF）。看到影像資料（SOS）就停
fn jpeg_exif(b: &[u8]) -> Option<&[u8]> {
    let mut pos = 2;
    while pos + 4 <= b.len() && b[pos] == 0xFF {
        let marker = b[pos + 1];
        if marker == 0xFF {
            pos += 1; // 填充用的 0xFF
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
        if len < 2 {
            break;
        }
        let seg = b.get(pos + 4..(pos + 2 + len).min(b.len()))?;
        if marker == 0xE1 {
            if let Some(t) = seg.strip_prefix(b"Exif\0\0") {
                return Some(t);
            }
        }
        pos += 2 + len;
    }
    None
}

/// TIFF 目錄（IFD0 一路接下去，加上 EXIF 子目錄）裡的文字欄位：(標籤, 文字)。ASCII 型別照字面，UserComment 照它自己標的編碼解。
/// 區塊前面可能有 "Exif\0\0"，也可能沒有
pub(crate) fn exif_fields(data: &[u8]) -> Vec<(u16, String)> {
    let tiff = data.strip_prefix(b"Exif\0\0").unwrap_or(data);
    let le = match tiff.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return vec![],
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let mut out = vec![];
    if u16_at(2) != Some(42) {
        return out;
    }
    let mut todo: Vec<usize> = u32_at(4).map(|o| o as usize).into_iter().collect();
    let mut seen: HashSet<usize> = HashSet::new();
    // 目錄最多看幾個，免得壞檔繞圈
    while let Some(off) = todo.pop() {
        if off == 0 || seen.len() >= 8 || !seen.insert(off) {
            continue;
        }
        let Some(n) = u16_at(off) else { continue };
        for i in 0..n as usize {
            let e = off + 2 + i * 12;
            let (Some(tag), Some(ty), Some(count)) = (u16_at(e), u16_at(e + 2), u32_at(e + 4)) else {
                break;
            };
            if tag == EXIF_IFD {
                todo.extend(u32_at(e + 8).map(|p| p as usize));
                continue;
            }
            if !(ty == 2 || (ty == 7 && tag == USER_COMMENT)) {
                continue;
            }
            let count = count as usize;
            // 4 個位元組放得下就直接放在欄位裡，否則欄位裡放的是位移
            let raw = if count <= 4 {
                tiff.get(e + 8..e + 8 + count)
            } else {
                u32_at(e + 8).and_then(|p| tiff.get(p as usize..(p as usize).checked_add(count)?))
            };
            let Some(raw) = raw else { continue };
            let text = if ty == 2 { String::from_utf8_lossy(raw).trim_end_matches('\0').to_string() } else { user_comment(raw, le) };
            out.push((tag, text));
        }
        todo.extend(u32_at(off + 2 + n as usize * 12).map(|o| o as usize));
    }
    out
}

/// UserComment：前 8 個位元組標編碼（"ASCII"、"UNICODE"＝UTF-16、全 0＝沒說）；沒標的整段當 UTF-8。
/// WebUI（piexif）寫 UTF-16 大端序，別的軟體可能照 TIFF 的位元組順序：看第一個字的 0 落在哪邊
fn user_comment(raw: &[u8], le: bool) -> String {
    let head = raw.get(..8).unwrap_or(&[]);
    let labeled = head.starts_with(b"UNICODE") || head.starts_with(b"ASCII") || head.starts_with(b"JIS") || head == [0u8; 8];
    let body = if labeled { &raw[8..] } else { raw };
    let s = if head.starts_with(b"UNICODE") {
        let be = match body {
            [0, b, ..] if *b != 0 => true,
            [a, 0, ..] if *a != 0 => false,
            _ => !le,
        };
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| if be { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) })
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(body).into_owned()
    };
    s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
}

// ---------------------------------------------------------------- 讀參數
/// 圖裡讀到的東西
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// 生成頁的參數（跟 anima_params 同一種格式）；只有讀不出參數的工作流時是 None
    pub params: Option<Value>,
    /// "anima"、"comfyui"、"webui"（A1111／Forge／C 站）、"novelai"、"other"（SwarmUI、Fooocus 這類 JSON）
    pub source: &'static str,
    /// 圖裡有 ComfyUI 工作流（可以匯入成工作流）
    pub workflow: bool,
    /// 給使用者看的提醒：對不到的取樣器之類
    pub notes: Vec<String>,
}

impl Found {
    pub fn to_json(&self) -> Value {
        json!({"params": self.params, "source": self.source, "workflow": self.workflow, "notes": self.notes})
    }
}

/// 給使用者看的提醒（叫 note：tools\i18n_extract.py 靠函式名稱把字串抽去翻英文）
fn note(s: &str) -> String {
    s.to_string()
}

pub fn read_params_file(path: &Path) -> Option<Value> {
    let f = std::fs::File::open(path).ok()?;
    let (text, size) = png_text(std::io::BufReader::new(f))?;
    found_from_text(&text, size)?.params
}

pub fn read_params_bytes(b: &[u8]) -> Option<Value> {
    read_found(b)?.params
}

/// 「讀圖片參數」：PNG、WebP、JPEG 都看；什麼都沒讀到是 None
pub fn read_found(b: &[u8]) -> Option<Found> {
    let (text, size) = meta_text(b)?;
    found_from_text(&text, size)
}

/// /api/image/meta 的回應：Found::to_json，什麼都沒讀到時 params、source 是 null，notes 說明原因
pub fn read_found_json(b: &[u8]) -> Value {
    read_found(b).map(|f| f.to_json()).unwrap_or_else(|| {
        let why = note("這張圖裡沒有生成參數：被網站、聊天軟體重新壓縮過，或截圖、修圖軟體另存的圖，資料通常已經掉了");
        json!({"params": null, "source": null, "workflow": false, "notes": [why]})
    })
}

/// 優先順序：生圖站自己的 → ComfyUI 節點圖（讀得到提示詞的話）→ WebUI 文字／JSON → NovelAI 的 Comment → 讀不到提示詞的節點圖
fn found_from_text(text: &HashMap<String, String>, size: (u32, u32)) -> Option<Found> {
    let parsed = |k: &str| text.get(k).and_then(|t| serde_json::from_str::<Value>(t).ok());
    let workflow = parsed("prompt").is_some_and(|v| v.is_object()) || parsed("workflow").is_some_and(|v| v.is_object());
    let found = |params: Value, source: &'static str, notes: Vec<String>| Some(Found { params: Some(params), source, workflow, notes });
    if let Some(p) = parsed("anima_params").filter(Value::is_object) {
        return found(p, "anima", vec![]);
    }
    let graph = parsed("prompt").and_then(|g| crate::image_wf::graph_params(&g, size));
    let has_prompt = |p: &Value| p["prompt"].as_str().is_some_and(|s| !s.trim().is_empty());
    if let Some(g) = graph.clone().filter(has_prompt) {
        return found(g, "comfyui", vec![]);
    }
    let mut notes = vec![];
    if let Some(t) = text.get("parameters").map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if t.starts_with('{') {
            if let Some(p) = serde_json::from_str::<Value>(t).ok().and_then(|v| json_params(&v, &mut notes)) {
                return found(p, "other", notes);
            }
        } else {
            return found(webui_params(t, &mut notes), "webui", notes);
        }
    }
    if let Some(p) = parsed("Comment").and_then(|v| json_params(&v, &mut notes)) {
        let nai = text.get("Software").is_some_and(|s| s.contains("NovelAI"));
        return found(p, if nai { "novelai" } else { "other" }, notes);
    }
    if let Some(g) = graph {
        return found(g, "comfyui", vec![note("圖裡的工作流找不到提示詞（可能是自訂節點產生的文字），其他參數照讀")]);
    }
    workflow.then(|| Found {
        params: None,
        source: "comfyui",
        workflow,
        notes: vec![note("這張圖只有一般「儲存」格式的工作流，讀不出參數：可以直接匯入成工作流")],
    })
}

// ---------------------------------------------------------------- WebUI（A1111、Forge、C 站）
/// 參數那一行的「名稱: 值」（照 A1111 的 re_param；值有逗號時用雙引號包起來）
static RE_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\s*(\w[\w \-/]+):\s*("(?:\\.|[^\\"])+"|[^,]*)(?:,|$)"#).expect("re_param"));
/// 提示詞裡的 <lora:名稱:強度>（舊的 LyCORIS 外掛寫 lyco）
static RE_LORA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(?:lora|lyco):([^:>]+)(?::([^:>]*))?[^>]*>").expect("re_lora"));
/// 生圖站不支援、直接拿掉的 <hypernet:…>
static RE_HYPERNET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<hypernet:[^>]*>").expect("re_hypernet"));

/// A1111 格式：提示詞（可能好幾行）、「Negative prompt: …」、最後一行「Steps: 20, Sampler: …, CFG scale: 7, Seed: …, Size: 832x1216, Model: …」。
/// 最後一行湊不到 3 個參數就當它也是提示詞（跟 A1111 一樣）。C 站會在最後一行加 Civitai resources: [JSON]，先整段拿出來（裡面的逗號會打亂參數）
fn webui_params(text: &str, notes: &mut Vec<String>) -> Value {
    let text = text.replace("\r\n", "\n");
    let mut lines: Vec<&str> = text.trim().split('\n').collect();
    let last_raw = lines.pop().unwrap_or("");
    let mut last = last_raw.to_string();
    let mut resources = take_json_field(&mut last, "Civitai resources");
    take_json_field(&mut last, "Civitai metadata");
    let mut kv: HashMap<String, String> = RE_PARAM
        .captures_iter(&last)
        .map(|c| {
            let v = c[2].trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .map(|x| x.replace("\\\"", "\"").replace("\\n", "\n"))
                .unwrap_or_else(|| v.to_string());
            (c[1].trim().to_string(), v)
        })
        .collect();
    if kv.len() < 3 {
        lines.push(last_raw);
        kv.clear();
        resources = None;
    }
    let (mut prompt, mut negative, mut neg) = (String::new(), String::new(), false);
    for line in lines {
        let mut line = line.trim();
        if let Some(rest) = line.strip_prefix("Negative prompt:") {
            neg = true;
            line = rest.trim();
        }
        let buf = if neg { &mut negative } else { &mut prompt };
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
    }
    // LoRA：從提示詞拿出來放進 loras（名稱是 WebUI 的檔名、沒有副檔名，網頁再對到這台電腦的檔）
    let mut loras: Vec<Value> = vec![];
    for c in RE_LORA.captures_iter(&prompt) {
        let strength = c.get(2).and_then(|m| m.as_str().trim().parse::<f64>().ok()).unwrap_or(1.0);
        loras.push(json!({"name": c[1].trim(), "strength": strength}));
    }
    if RE_HYPERNET.is_match(&prompt) {
        notes.push(note("提示詞裡的 <hypernet:…>（超網路）生圖站不支援，已經拿掉"));
    }
    let prompt = tidy_prompt(&RE_HYPERNET.replace_all(&RE_LORA.replace_all(&prompt, ""), ""));
    // 沒寫 <lora:…> 的 C 站圖：LoRA 改看 Civitai resources
    if loras.is_empty() {
        for r in resources.iter().flatten().filter(|r| r["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("lora"))) {
            if let Some(name) = r["modelName"].as_str() {
                loras.push(json!({"name": name, "strength": r["weight"].as_f64().unwrap_or(1.0)}));
            }
        }
    }
    let num = |k: &str| kv.get(k).and_then(|v| v.trim().parse::<f64>().ok());
    let mut p = json!({"mode": "txt2img", "prompt": prompt, "negative": tidy_prompt(&negative), "loras": loras});
    if let Some(v) = num("Steps") {
        p["steps"] = json!(v as i64);
    }
    if let Some(v) = num("CFG scale") {
        p["cfg"] = json!(v);
    }
    if let Some(v) = kv.get("Seed").and_then(|v| v.trim().parse::<i64>().ok()) {
        p["seed"] = json!(v);
    }
    if let Some((w, h)) = kv.get("Size").and_then(|s| s.split_once('x')) {
        if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
            p["width"] = json!(w);
            p["height"] = json!(h);
        }
    }
    let model = kv.get("Model").cloned().or_else(|| {
        resources
            .iter()
            .flatten()
            .find(|r| r["type"].as_str() == Some("checkpoint"))
            .and_then(|r| r["modelName"].as_str().map(str::to_string))
    });
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        p["model"] = json!(m.trim());
    }
    if let Some(s) = kv.get("Sampler") {
        let (sampler, scheduler) = webui_sampler(s, kv.get("Schedule type").map(String::as_str));
        match sampler {
            Some(x) => p["sampler"] = json!(x),
            None => notes.push(format!("取樣器「{s}」對不到 ComfyUI 的，維持生成頁目前的")),
        }
        if let Some(x) = scheduler {
            p["scheduler"] = json!(x);
        }
    }
    if let (Some(scale), Some(d)) = (num("Hires upscale"), num("Denoising strength")) {
        p["hires"] = json!({"enabled": true, "scale": scale, "denoise": d});
    }
    p
}

/// 把「名稱: [JSON]」或「名稱: {JSON}」從參數行拿出來（括號配對，字串裡的括號不算），回傳解析後的 JSON（物件包成一筆）
fn take_json_field(line: &mut String, key: &str) -> Option<Vec<Value>> {
    let at = line.find(&format!("{key}:"))?;
    let open = at + line[at..].find(['[', '{'])?;
    let (mut depth, mut in_str, mut esc, mut end) = (0i32, false, false, None);
    for (i, ch) in line[open..].char_indices() {
        if in_str {
            match ch {
                _ if esc => esc = false,
                '\\' => esc = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '[' | '{' => depth += 1,
            ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let v: Value = serde_json::from_str(&line[open..end]).ok()?;
    let rest = &line[end..];
    let tail = rest.trim_start().strip_prefix(',').unwrap_or(rest).to_string();
    line.replace_range(at.., &tail);
    Some(match v {
        Value::Array(a) => a,
        other => vec![other],
    })
}

/// 拿掉 LoRA 標記後留下的空逗號、多的空白
fn tidy_prompt(s: &str) -> String {
    s.lines()
        .map(|l| l.split(',').map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// WebUI 的取樣器（舊版連排程一起寫，例如「DPM++ 2M Karras」；新版排程另外寫在 Schedule type）→ ComfyUI 的 (sampler, scheduler)。
/// 已經是 ComfyUI 寫法的（Image Saver 這類節點寫的「euler_ancestral」）照收
fn webui_sampler(name: &str, schedule: Option<&str>) -> (Option<String>, Option<&'static str>) {
    // SGM Uniform 要排在 Uniform 前面（後綴比對）
    const SCHEDULES: [(&str, Option<&str>); 13] = [
        ("Automatic", None),
        ("SGM Uniform", Some("sgm_uniform")),
        ("Uniform", Some("normal")),
        ("Normal", Some("normal")),
        ("Karras", Some("karras")),
        ("Exponential", Some("exponential")),
        ("Simple", Some("simple")),
        ("DDIM", Some("ddim_uniform")),
        ("Beta", Some("beta")),
        ("KL Optimal", Some("kl_optimal")),
        ("Polyexponential", None),
        ("Align Your Steps", None),
        ("Turbo", None),
    ];
    const SAMPLERS: [(&str, &str); 22] = [
        ("Euler a", "euler_ancestral"),
        ("Euler", "euler"),
        ("Euler CFG++", "euler_cfg_pp"),
        ("Euler a CFG++", "euler_ancestral_cfg_pp"),
        ("LMS", "lms"),
        ("Heun", "heun"),
        ("DPM2", "dpm_2"),
        ("DPM2 a", "dpm_2_ancestral"),
        ("DPM++ 2S a", "dpmpp_2s_ancestral"),
        ("DPM++ 2M", "dpmpp_2m"),
        ("DPM++ SDE", "dpmpp_sde"),
        ("DPM++ 2M SDE", "dpmpp_2m_sde"),
        ("DPM++ 2M SDE Heun", "dpmpp_2m_sde"),
        ("DPM++ 3M SDE", "dpmpp_3m_sde"),
        ("DPM fast", "dpm_fast"),
        ("DPM adaptive", "dpm_adaptive"),
        ("LCM", "lcm"),
        ("DDIM", "ddim"),
        ("DDPM", "ddpm"),
        ("UniPC", "uni_pc"),
        ("DEIS", "deis"),
        ("IPNDM", "ipndm"),
    ];
    let name = name.trim();
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return (Some(name.to_string()), None);
    }
    // 舊寫法：名稱後面接排程（只有拿掉排程後真的是取樣器名稱才算）
    let split = SCHEDULES.iter().skip(1).find_map(|(s, v)| {
        let base = name.strip_suffix(s)?.strip_suffix(' ')?;
        SAMPLERS.iter().any(|(k, _)| *k == base).then_some((base, *v))
    });
    let (base, suffix) = split.unwrap_or((name, None));
    let sampler = SAMPLERS.iter().find(|(k, _)| *k == base).map(|(_, v)| v.to_string());
    let from_type = schedule.and_then(|t| SCHEDULES.iter().find(|(s, _)| *s == t.trim()).and_then(|(_, v)| *v));
    // 新版沒寫排程（Automatic）時，WebUI 用每個取樣器自己的預設
    let default = match sampler.as_deref() {
        Some("dpmpp_2m" | "dpmpp_sde" | "dpm_2" | "dpm_2_ancestral") => Some("karras"),
        Some("dpmpp_2m_sde" | "dpmpp_3m_sde") => Some("exponential"),
        Some(_) => Some("normal"),
        None => None,
    };
    (sampler, suffix.or(from_type).or(default))
}

// ---------------------------------------------------------------- NovelAI、SwarmUI、Fooocus（JSON）
/// JSON 格式的參數：NovelAI 的 Comment（prompt、uc、scale、sampler＝k_euler…）、SwarmUI 的 sui_image_params、Fooocus 的 parameters。
/// 沒有 prompt 欄位就不算
fn json_params(v: &Value, notes: &mut Vec<String>) -> Option<Value> {
    let v = v.get("sui_image_params").unwrap_or(v);
    let s = |keys: &[&str]| keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str)).map(str::to_string);
    let n = |keys: &[&str]| {
        keys.iter().find_map(|k| match v.get(*k)? {
            Value::Number(x) => x.as_f64(),
            Value::String(x) => x.trim().parse::<f64>().ok(),
            _ => None,
        })
    };
    let prompt = s(&["prompt"])?;
    let negative = s(&["uc", "negative_prompt", "negativeprompt"]).unwrap_or_default();
    let mut p = json!({"mode": "txt2img", "prompt": prompt.trim(), "negative": negative.trim(), "loras": []});
    if let Some(x) = n(&["steps"]) {
        p["steps"] = json!(x as i64);
    }
    if let Some(x) = n(&["scale", "cfgscale", "cfg_scale", "guidance_scale", "cfg"]) {
        p["cfg"] = json!(x);
    }
    if let Some(x) = n(&["seed"]) {
        p["seed"] = json!(x as i64);
    }
    let mut size = (n(&["width"]), n(&["height"]));
    // Fooocus：resolution 是 "(896, 1152)"
    if let (None, Some(r)) = (size.0, s(&["resolution"])) {
        let nums: Vec<f64> = r.split(|c: char| !c.is_ascii_digit()).filter_map(|x| x.parse().ok()).collect();
        if let [w, h] = &nums[..] {
            size = (Some(*w), Some(*h));
        }
    }
    if let (Some(w), Some(h)) = size {
        p["width"] = json!(w as u32);
        p["height"] = json!(h as u32);
    }
    if let Some(m) = s(&["model", "base_model"]).filter(|m| !m.trim().is_empty()) {
        p["model"] = json!(m.trim());
    }
    if let Some(raw) = s(&["sampler"]) {
        // NovelAI 的 k_ 前綴、ddim_v3
        let x = raw.trim().strip_prefix("k_").unwrap_or(raw.trim());
        let x = if x == "ddim_v3" { "ddim" } else { x };
        match webui_sampler(x, None).0 {
            Some(name) => p["sampler"] = json!(name),
            None => notes.push(format!("取樣器「{raw}」對不到 ComfyUI 的，維持生成頁目前的")),
        }
    }
    let sched = s(&["scheduler", "noise_schedule"]).map(|x| x.trim().to_lowercase());
    match sched.as_deref() {
        Some("native") => p["scheduler"] = json!("normal"),
        Some(x @ ("normal" | "karras" | "exponential" | "simple" | "sgm_uniform" | "beta" | "ddim_uniform" | "kl_optimal" | "linear_quadratic")) => {
            p["scheduler"] = json!(x)
        }
        _ => {}
    }
    Some(p)
}

/// 把 ComfyUI output 資料夾裡、還沒進圖庫的 PNG 匯入（例如直接在 ComfyUI 介面產生的圖）。回傳匯入張數。
pub fn import_existing(db: &Db, output: &Path) -> usize {
    let known = db.active_files().unwrap_or_default();
    let mut n = 0;
    let mut stack = vec![output.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        let sub = dir
            .strip_prefix(output)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let mut subdirs = vec![];
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            if path.is_dir() {
                if !name.starts_with('_') {
                    subdirs.push(path);
                }
                continue;
            }
            if !name.to_lowercase().ends_with(".png")
                || known.contains(&(sub.clone(), name.clone()))
            {
                continue;
            }
            let Ok((w, h)) = image::image_dimensions(&path) else {
                continue;
            };
            let p = read_params_file(&path).unwrap_or_else(|| json!({}));
            let g = |k: &str, d: Value| p.get(k).cloned().unwrap_or(d);
            let info = json!({
                "mode": g("mode", json!("imported")), "prompt": g("prompt_resolved", g("prompt", json!(""))),
                "negative": g("negative", json!("")), "seed": g("seed", Value::Null), "width": w, "height": h,
            });
            let Ok(iid) = db.add_image(&name, &sub, "output", &info, &p, None) else {
                continue;
            };
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                let secs = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let _ = db.run(
                    "UPDATE images SET created=? WHERE id=?",
                    rusqlite::params![secs, iid],
                );
            }
            n += 1;
        }
        // os.walk 是由上而下、同一層照順序：反過來推進堆疊，先處理排前面的資料夾
        stack.extend(subdirs.into_iter().rev());
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_with(texts: &[(&str, &str)]) -> Vec<u8> {
        let mut out = vec![];
        {
            let mut enc = png::Encoder::new(&mut out, 3, 2);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            for (k, v) in texts {
                enc.add_itxt_chunk(k.to_string(), v.to_string()).expect("文字區塊");
            }
            let mut w = enc.write_header().expect("標頭");
            w.write_image_data(&[0; 6]).expect("像素");
            w.finish().expect("寫完");
        }
        out
    }

    /// 大端序（piexif 的寫法）TIFF：IFD0 放 ascii 欄位，再指向 EXIF 子目錄，子目錄放 UserComment
    fn tiff_with(ascii: &[(u16, &str)], comment: Option<&[u8]>) -> Vec<u8> {
        let n0 = ascii.len() + usize::from(comment.is_some());
        let ifd0_end = 8 + 2 + n0 * 12 + 4;
        let sub_end = ifd0_end + if comment.is_some() { 2 + 12 + 4 } else { 0 };
        let mut data: Vec<u8> = vec![];
        let mut t = b"MM".to_vec();
        t.extend(42u16.to_be_bytes());
        t.extend(8u32.to_be_bytes());
        t.extend((n0 as u16).to_be_bytes());
        for (tag, s) in ascii {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            t.extend(tag.to_be_bytes());
            t.extend(2u16.to_be_bytes());
            t.extend((b.len() as u32).to_be_bytes());
            t.extend(((sub_end + data.len()) as u32).to_be_bytes());
            data.extend(b);
        }
        if comment.is_some() {
            t.extend(EXIF_IFD.to_be_bytes());
            t.extend(4u16.to_be_bytes());
            t.extend(1u32.to_be_bytes());
            t.extend((ifd0_end as u32).to_be_bytes());
        }
        t.extend(0u32.to_be_bytes());
        if let Some(c) = comment {
            t.extend(1u16.to_be_bytes());
            t.extend(USER_COMMENT.to_be_bytes());
            t.extend(7u16.to_be_bytes());
            t.extend((c.len() as u32).to_be_bytes());
            t.extend(((sub_end + data.len()) as u32).to_be_bytes());
            t.extend(0u32.to_be_bytes());
            data.extend(c);
        }
        t.extend(data);
        t
    }

    fn unicode_be(s: &str) -> Vec<u8> {
        let mut out = b"UNICODE\0".to_vec();
        out.extend(s.encode_utf16().flat_map(u16::to_be_bytes));
        out
    }

    /// 真的 8×4 JPEG，SOI 後面插一段 APP1 Exif
    fn jpeg_with(tiff: &[u8]) -> Vec<u8> {
        let mut img = vec![];
        image::codecs::jpeg::JpegEncoder::new(&mut img).encode(&[128u8; 8 * 4 * 3], 8, 4, image::ExtendedColorType::Rgb8).expect("jpeg");
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend(tiff);
        let mut out = img[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(((seg.len() + 2) as u16).to_be_bytes());
        out.extend(seg);
        out.extend(&img[2..]);
        out
    }

    /// WebP 外殼（沒有影像資料，尺寸讀不到是 0×0）
    fn webp_with(tiff: &[u8]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        body.extend(b"EXIF");
        body.extend((tiff.len() as u32).to_le_bytes());
        body.extend(tiff);
        if tiff.len() % 2 == 1 {
            body.push(0);
        }
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    const A1111: &str = "masterpiece, best quality, 1girl, <lora:detail_tweaker:0.6>, silver hair,\n<lora:mystyle:1.2> smile\nNegative prompt: lowres, bad hands\nworst quality\nSteps: 28, Sampler: DPM++ 2M Karras, CFG scale: 6.5, Seed: 1234567890, Size: 832x1216, Model hash: abc123, Model: animagineXL_v31, Denoising strength: 0.4, Hires upscale: 1.5, Lora hashes: \"detail_tweaker: 1a2b, mystyle: 3c4d\", Version: v1.10.1";

    #[test]
    fn webui_text() {
        let mut notes = vec![];
        let p = webui_params(A1111, &mut notes);
        // 拿掉 LoRA 標記後，空掉的逗號（包括行尾的）一起清掉
        assert_eq!(p["prompt"], "masterpiece, best quality, 1girl, silver hair\nsmile");
        assert_eq!(p["negative"], "lowres, bad hands\nworst quality");
        assert_eq!(p["steps"], 28);
        assert_eq!(p["cfg"], 6.5);
        assert_eq!(p["seed"], 1234567890i64);
        assert_eq!((p["width"].as_u64(), p["height"].as_u64()), (Some(832), Some(1216)));
        assert_eq!((p["sampler"].as_str(), p["scheduler"].as_str()), (Some("dpmpp_2m"), Some("karras")));
        assert_eq!(p["model"], "animagineXL_v31");
        assert_eq!(p["loras"], json!([{"name": "detail_tweaker", "strength": 0.6}, {"name": "mystyle", "strength": 1.2}]));
        assert_eq!(p["hires"], json!({"enabled": true, "scale": 1.5, "denoise": 0.4}));
        assert!(notes.is_empty());
    }

    #[test]
    fn webui_civitai_resources_and_plain_prompt() {
        let t = "1girl, solo\nSteps: 20, Sampler: Euler a, CFG scale: 7, Seed: 5, Size: 512x768, Civitai resources: [{\"type\":\"checkpoint\",\"modelVersionId\":1,\"modelName\":\"Pony, Diffusion\",\"modelVersionName\":\"v6\"},{\"type\":\"lora\",\"weight\":0.8,\"modelName\":\"Cute Style\"}], Civitai metadata: {}";
        let p = webui_params(t, &mut vec![]);
        assert_eq!(p["prompt"], "1girl, solo");
        assert_eq!(p["seed"], 5);
        assert_eq!(p["model"], "Pony, Diffusion");
        assert_eq!(p["loras"], json!([{"name": "Cute Style", "strength": 0.8}]));
        assert_eq!((p["sampler"].as_str(), p["scheduler"].as_str()), (Some("euler_ancestral"), Some("normal")));
        // 最後一行湊不到參數：整段都是提示詞
        let p = webui_params("1girl\nNegative prompt: bad\nmore negative", &mut vec![]);
        assert_eq!((p["prompt"].as_str(), p["negative"].as_str()), (Some("1girl"), Some("bad\nmore negative")));
        assert!(p.get("steps").is_none());
    }

    #[test]
    fn samplers() {
        assert_eq!(webui_sampler("DPM++ 2M SDE", Some("Karras")), (Some("dpmpp_2m_sde".into()), Some("karras")));
        assert_eq!(webui_sampler("DPM++ 2M SDE", Some("Automatic")), (Some("dpmpp_2m_sde".into()), Some("exponential")));
        assert_eq!(webui_sampler("DPM++ 2M SGM Uniform", None), (Some("dpmpp_2m".into()), Some("sgm_uniform")));
        assert_eq!(webui_sampler("DDIM", None), (Some("ddim".into()), Some("normal")));
        assert_eq!(webui_sampler("euler_ancestral", None), (Some("euler_ancestral".into()), None));
        assert_eq!(webui_sampler("Restart", None), (None, None));
        let mut notes = vec![];
        let p = webui_params("x\nSteps: 20, Sampler: Restart, CFG scale: 7", &mut notes);
        assert!(p.get("sampler").is_none());
        assert!(notes[0].contains("Restart"));
    }

    #[test]
    fn jpeg_and_webp_exif() {
        // WebUI 存的 JPEG：參數在 EXIF 子目錄的 UserComment（UTF-16 大端序）
        let jpg = jpeg_with(&tiff_with(&[], Some(&unicode_be("女孩, smile\nSteps: 9, Sampler: Euler, CFG scale: 2, Seed: 7"))));
        assert_eq!(image_kind(&jpg), Some("jpeg"));
        let (text, size) = meta_text(&jpg).unwrap();
        assert_eq!(size, (8, 4));
        assert!(text["parameters"].starts_with("女孩, smile"));
        let f = read_found(&jpg).unwrap();
        assert_eq!((f.source, f.workflow), ("webui", false));
        assert_eq!(f.params.as_ref().unwrap()["steps"], 9);
        // ComfyUI 存的 WebP：Make／Model 是 "workflow:…"、"prompt:…"
        let graph = json!({"9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0]}}});
        let webp = webp_with(&tiff_with(&[(0x010F, "workflow:{\"nodes\":[]}"), (0x0110, &format!("prompt:{graph}"))], None));
        let (text, size) = meta_text(&webp).unwrap();
        assert_eq!(size, (0, 0));
        assert_eq!(text["workflow"], "{\"nodes\":[]}");
        assert_eq!(serde_json::from_str::<Value>(&text["prompt"]).unwrap(), graph);
        // 沒有取樣節點：讀不到參數，但有工作流可以匯入
        let f = read_found(&webp).unwrap();
        assert_eq!((f.params.is_none(), f.workflow), (true, true));
        // 標了 ASCII 的 UserComment、沒標編碼的、小端序 UTF-16
        assert_eq!(user_comment(b"ASCII\0\0\0hello", false), "hello");
        assert_eq!(user_comment(b"plain text", false), "plain text");
        let le: Vec<u8> = b"UNICODE\0".iter().copied().chain("ab".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(user_comment(&le, true), "ab");
        assert!(meta_text(b"GIF89a").is_none());
    }

    /// Qwen 這類：SamplerCustomAdvanced＋CFGGuider＋BasicScheduler＋KSamplerSelect＋RandomNoise，
    /// 提示詞在 Primitive 文字節點，負面是 ConditioningZeroOut，尺寸接在 Primitive Int
    fn qwen_graph() -> Value {
        json!({
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "qwen_image_2.1_turbo.safetensors", "weight_dtype": "default"}},
            "2": {"class_type": "LoraLoaderModelOnly", "inputs": {"model": ["1", 0], "lora_name": "qwen/style.safetensors", "strength_model": 0.7}},
            "3": {"class_type": "CLIPLoader", "inputs": {"clip_name": "qwen_vl.safetensors", "type": "qwen_image"}},
            "4": {"class_type": "PrimitiveStringMultiline", "inputs": {"value": "a girl reading in a sunflower field"}},
            "5": {"class_type": "TextEncodeQwenImage21", "inputs": {"clip": ["3", 0], "prompt": ["4", 0]}},
            "6": {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["5", 0]}},
            "7": {"class_type": "CFGGuider", "inputs": {"model": ["2", 0], "positive": ["5", 0], "negative": ["6", 0], "cfg": 1.5}},
            "8": {"class_type": "KSamplerSelect", "inputs": {"sampler_name": "res_multistep"}},
            "9": {"class_type": "BasicScheduler", "inputs": {"model": ["2", 0], "scheduler": "simple", "steps": 8, "denoise": 1.0}},
            "10": {"class_type": "RandomNoise", "inputs": {"noise_seed": 424242}},
            "11": {"class_type": "PrimitiveInt", "inputs": {"value": 1328}},
            "12": {"class_type": "EmptySD3LatentImage", "inputs": {"width": ["11", 0], "height": 1024, "batch_size": 1}},
            "13": {"class_type": "SamplerCustomAdvanced", "inputs": {"noise": ["10", 0], "guider": ["7", 0], "sampler": ["8", 0], "sigmas": ["9", 0], "latent_image": ["12", 0]}},
            "14": {"class_type": "VAEDecode", "inputs": {"samples": ["13", 0], "vae": ["15", 0]}},
            "15": {"class_type": "VAELoader", "inputs": {"vae_name": "qwen_vae.safetensors"}},
            "16": {"class_type": "SaveImage", "inputs": {"images": ["14", 0], "filename_prefix": "ComfyUI"}}
        })
    }

    #[test]
    fn comfy_graph_new_samplers() {
        let p = crate::image_wf::graph_params(&qwen_graph(), (999, 999)).unwrap();
        assert_eq!(p["prompt"], "a girl reading in a sunflower field");
        assert_eq!(p["negative"], "");
        assert_eq!(p["seed"], 424242);
        assert_eq!(p["steps"], 8);
        assert_eq!(p["cfg"], 1.5);
        assert_eq!((p["sampler"].as_str(), p["scheduler"].as_str()), (Some("res_multistep"), Some("simple")));
        assert_eq!((p["width"].as_u64(), p["height"].as_u64()), (Some(1328), Some(1024)));
        assert_eq!(p["model"], "qwen_image_2.1_turbo.safetensors");
        assert_eq!(p["loras"], json!([{"name": "qwen/style.safetensors", "strength": 0.7}]));
        // 整張 PNG 走一次：來源 comfyui、有工作流
        let png = png_with(&[("prompt", &qwen_graph().to_string())]);
        let f = read_found(&png).unwrap();
        assert_eq!((f.source, f.workflow), ("comfyui", true));
        assert_eq!(f.params.unwrap()["seed"], 424242);
    }

    #[test]
    fn comfy_graph_hires_uses_first_pass() {
        let g = json!({
            "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sdxl.safetensors"}},
            "2": {"class_type": "LoraLoader", "inputs": {"model": ["1", 0], "clip": ["1", 1], "lora_name": "anima-turbo-lora-v0.2.safetensors", "strength_model": 1.0, "strength_clip": 1.0}},
            "3": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["2", 1], "text": "1girl, beach"}},
            "4": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["2", 1], "text": "lowres"}},
            "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 832, "height": 1216, "batch_size": 1}},
            "6": {"class_type": "KSampler", "inputs": {"model": ["2", 0], "positive": ["3", 0], "negative": ["4", 0], "latent_image": ["5", 0],
                  "seed": 11, "steps": 30, "cfg": 4.0, "sampler_name": "euler_ancestral", "scheduler": "normal", "denoise": 1.0}},
            "7": {"class_type": "LatentUpscaleBy", "inputs": {"samples": ["6", 0], "upscale_method": "nearest-exact", "scale_by": 1.5}},
            "8": {"class_type": "KSampler", "inputs": {"model": ["2", 0], "positive": ["3", 0], "negative": ["4", 0], "latent_image": ["7", 0],
                  "seed": 99, "steps": 12, "cfg": 4.0, "sampler_name": "dpmpp_2m", "scheduler": "karras", "denoise": 0.45}},
            "9": {"class_type": "VAEDecode", "inputs": {"samples": ["8", 0], "vae": ["1", 2]}},
            "10": {"class_type": "SaveImage", "inputs": {"images": ["9", 0]}}
        });
        let p = crate::image_wf::graph_params(&g, (1248, 1824)).unwrap();
        assert_eq!((p["seed"].as_i64(), p["steps"].as_i64(), p["sampler"].as_str()), (Some(11), Some(30), Some("euler_ancestral")));
        assert_eq!((p["prompt"].as_str(), p["negative"].as_str()), (Some("1girl, beach"), Some("lowres")));
        assert_eq!((p["width"].as_u64(), p["height"].as_u64()), (Some(832), Some(1216)));
        assert_eq!(p["turbo_lora"], "anima-turbo-lora-v0.2.safetensors");
        assert_eq!(p["loras"], json!([]));
        assert_eq!(p["model"], "sdxl.safetensors");
    }

    #[test]
    fn priority_and_other_formats() {
        // 生圖站自己的參數最優先
        let png = png_with(&[("anima_params", "{\"prompt\": \"mine\"}"), ("prompt", &qwen_graph().to_string()), ("parameters", A1111)]);
        let f = read_found(&png).unwrap();
        assert_eq!((f.source, f.params.unwrap()["prompt"].as_str()), ("anima", Some("mine")));
        // WebUI 的 PNG
        let f = read_found(&png_with(&[("parameters", A1111)])).unwrap();
        assert_eq!((f.source, f.workflow), ("webui", false));
        // NovelAI
        let nai = r#"{"prompt": "1girl, cat ears", "uc": "lowres", "steps": 28, "scale": 5.0, "seed": 77, "width": 832, "height": 1216, "sampler": "k_euler_ancestral", "noise_schedule": "karras"}"#;
        let f = read_found(&png_with(&[("Software", "NovelAI"), ("Comment", nai)])).unwrap();
        let p = f.params.unwrap();
        assert_eq!(f.source, "novelai");
        assert_eq!((p["sampler"].as_str(), p["scheduler"].as_str(), p["negative"].as_str()), (Some("euler_ancestral"), Some("karras"), Some("lowres")));
        // Fooocus（parameters 是 JSON）
        let fo = r#"{"prompt": "a cat", "negative_prompt": "", "steps": 30, "guidance_scale": 4, "seed": "123", "resolution": "(896, 1152)", "sampler": "dpmpp_2m_sde_gpu", "scheduler": "karras", "base_model": "juggernautXL.safetensors"}"#;
        let p = read_found(&png_with(&[("parameters", fo)])).unwrap().params.unwrap();
        assert_eq!((p["width"].as_u64(), p["height"].as_u64(), p["seed"].as_i64()), (Some(896), Some(1152), Some(123)));
        assert_eq!(p["model"], "juggernautXL.safetensors");
        // 只有 UI 格式工作流：讀不出參數，但可以匯入
        let f = read_found(&png_with(&[("workflow", "{\"nodes\": []}")])).unwrap();
        assert_eq!((f.params, f.source, f.workflow), (None, "comfyui", true));
        // 什麼都沒有
        assert!(read_found(&png_with(&[])).is_none());
        assert!(read_params_bytes(b"not an image").is_none());
    }
}

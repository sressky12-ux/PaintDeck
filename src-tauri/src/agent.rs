//! 生圖助手（2026-10-13）：使用者用中文說想要什麼 → 通用模型（9B／35B，或雲端 35B）寫提示詞、挑 LoRA →
//! 生圖 → 看圖逐項打勾 → 有缺就只改有問題的地方重畫 → 挑最好的交卷。畫出來的圖全部照一般生成的路進圖庫。
//!
//! 這裡放不碰網路的部分：工作說明、組對話、讀回模型的 JSON 並檢查（寫中文退回、提示詞沒改退回、沒打勾退回）、
//! LoRA 目錄（認識每個 LoRA 是做什麼的）、對話存檔。執行在 server\api_agent.rs。
//!
//! 模型每次只回一個 JSON 物件（不用 OpenAI 的 tool_calls：串流、取消、進度都沿用 pe.rs 現成的路，9B 也比較守規矩）：
//!   {"action":"generate", check, prompt, width, height, loras, changes, say, refs?, denoise?}
//!   {"action":"finish", check, best, message}
//!   {"action":"reply", message}
//!
//! 存檔：data\agent\sessions\<id>.json（設定＋事件清單；事件同時是畫面上的對話和下一次給模型的上下文）
use crate::director::{json_in, tx, valid_id, write_atomic};
use crate::families::{ANIMA, MOSS, QWEN21};
use crate::py::{now, truthy};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// 一次給模型看的「助手動作」最多幾個；更早的濃縮成一段摘要（越後面不會越慢）
pub const MAX_ACTS: usize = 10;
/// 一次最多掛幾個 LoRA（助手自己挑的＋使用者固定的）
pub const MAX_LORAS: usize = 3;
/// 一個請求同一輪最多退回幾次（模型一直寫壞就停下來報錯）
pub const MAX_REJECTS: usize = 3;
/// 使用者一次最多附幾張圖；給模型看的使用者圖片最多幾張（最新的那幾張，舊的只留文字）
pub const MAX_USER_IMAGES: usize = 4;
pub const SHOW_USER_IMAGES: usize = 4;

// ================================================================ 工作說明
// 預設文字在 resources\agent\*.txt（編進程式）；設定頁「自訂系統提示詞」可以改，存在 data\pe_prompts\agent*.txt，下一輪就生效。
// system.txt 裡的 {model_guide}／{sizes}／{loras} 由程式填入，一定要留著。
const DEFAULT_SYSTEM: &str = include_str!("../../resources/agent/system.txt");
const GUIDE_QWEN: &str = include_str!("../../resources/agent/guide_qwen21.txt");
const GUIDE_MOSS: &str = include_str!("../../resources/agent/guide_moss.txt");
const GUIDE_ANIMA: &str = include_str!("../../resources/agent/guide_anima.txt");
const GUIDE_OTHER: &str = include_str!("../../resources/agent/guide_other.txt");
/// 寫法庫（2026-10-27）：使用者原話「把 每個模型的官方寫提示詞skill都分類整理教一教給他。讓他真的會寫。嚴謹分類。」
const GUIDE_QWEN_EDIT: &str = include_str!("../../resources/agent/guide_qwen_edit.txt");
const GUIDE_QWEN_MULTI: &str = include_str!("../../resources/agent/guide_qwen_multiref.txt");
/// 「只寫詞」模式的工作說明：不碰繪圖模型，只回提示詞卡（{target}／{guide}／{sizes} 由程式填）
const PROMPT_SYSTEM: &str = include_str!("../../resources/agent/prompt_mode.txt");
/// H3 影片台「AI 幫我寫」：T2VA／I2VA／FL2VA 用 base，Ref2VA（多素材、換角色、特效…）用 ref
const H3_BASE: &str = include_str!("../../resources/agent/h3_base.txt");
const H3_REF: &str = include_str!("../../resources/agent/h3_ref.txt");
const H3_BOARD: &str = include_str!("../../resources/agent/h3_board.txt");
/// 影片台拍攝單「AI 潤飾」（2026-10-24）
const H3_SHEET: &str = include_str!("../../resources/agent/h3_sheet.txt");

/// 設定頁可以改的十二份：(編號, 預設文字)（2026-10-30 LTX 退場，拿掉 agent_ltx）
pub const EDITABLE_TEXTS: [(&str, &str); 12] = [
    ("agent", DEFAULT_SYSTEM),
    ("agent_prompt", PROMPT_SYSTEM),
    ("agent_qwen21", GUIDE_QWEN),
    ("agent_qwen_edit", GUIDE_QWEN_EDIT),
    ("agent_qwen_multiref", GUIDE_QWEN_MULTI),
    ("agent_moss", GUIDE_MOSS),
    ("agent_anima", GUIDE_ANIMA),
    ("agent_other", GUIDE_OTHER),
    ("agent_h3_base", H3_BASE),
    ("agent_h3_ref", H3_REF),
    ("agent_h3_board", H3_BOARD),
    ("agent_h3_sheet", H3_SHEET),
];

/// 預設文字（去掉檔尾換行）
pub fn default_text(id: &str) -> Option<&'static str> {
    EDITABLE_TEXTS.iter().find(|(i, _)| *i == id).map(|(_, t)| t.trim())
}

/// 這個家族用哪一份寫法說明
pub fn guide_id(family: &str) -> &'static str {
    match family {
        QWEN21 => "agent_qwen21",
        MOSS => "agent_moss",
        ANIMA => "agent_anima",
        _ => "agent_other",
    }
}

/// 寫法庫：(名稱, 設定頁編號, 中文名, 什麼時候用)。名稱給模型用（guide 工具、只寫詞模式的目標）
pub const GUIDES: &[(&str, &str, &str, &str)] = &[
    ("anima", "agent_anima", "Anima", "Anima: Danbooru tags plus English sentences (text to image and image to image)"),
    ("qwen", "agent_qwen21", "Qwen 2.1 文生圖", "Qwen-Image 2.1 text to image: one long observer paragraph"),
    ("qwen_edit", "agent_qwen_edit", "Qwen 2.1 改圖", "Qwen-Image 2.1 editing one picture (the edit tool, or generate with one ref)"),
    ("qwen_multiref", "agent_qwen_multiref", "Qwen 2.1 多圖參考", "Qwen-Image 2.1 with two or more reference pictures named <image1>, <image2>…"),
    ("moss", "agent_moss", "MOSS-Image", "MOSS-Image: detailed prose, text in the picture, comic pages"),
    ("other", "agent_other", "SDXL 系", "SDXL-family models (Illustrious, NoobAI, Pony)"),
    ("h3_video", "agent_h3_base", "H3 影片", "MiniMax H3 video with sound (the video tool and the H3 video page): text or image to video"),
];

/// 寫法名稱 → 設定頁編號（認不得就 None）
pub fn guide_text_id(name: &str) -> Option<&'static str> {
    let n = name.trim().to_lowercase().replace(['-', ' '], "_");
    GUIDES.iter().find(|g| g.0 == n || g.1 == n).map(|g| g.1)
}

/// 寫法名稱 → 中文名
pub fn guide_zh(name: &str) -> &'static str {
    GUIDES.iter().find(|g| g.0 == name).map(|g| g.2).unwrap_or("寫法")
}

/// 寫法目錄（一行一份，給模型看）
pub fn guides_list() -> String {
    GUIDES.iter().map(|g| format!("{} = {}", g.0, g.3)).collect::<Vec<_>>().join("; ")
}

/// 家族的文生圖寫法名稱
pub fn family_guide(family: &str) -> &'static str {
    match family {
        QWEN21 => "qwen",
        MOSS => "moss",
        ANIMA => "anima",
        _ => "other",
    }
}

/// 明確指定寫法時，尺寸與校正也跟著目標家族，不沿用生成頁的另一個模型。
pub fn prompt_family<'a>(target: &str, fallback: &'a str) -> &'a str {
    match target {
        "anima" => ANIMA,
        "qwen" | "qwen_edit" | "qwen_multiref" => QWEN21,
        "moss" => MOSS,
        "other" => "other",
        _ => fallback,
    }
}

static H3_ASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bh3\b|minimax").unwrap());
static VIDEO_ASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)影片|視頻|视频|動起來|动起来|動畫|运镜|運鏡|\bvideo\b|\bclip\b|\banimate\b").unwrap());

/// 「只寫詞」自動挑寫法：講到影片 → H3 影片（2026-10-30 起內建影片就是 H3）；Qwen 附一張圖 → 改圖、兩張以上 → 多圖參考；其他照家族
pub fn pick_guide(family: &str, images: usize, ask: &str) -> &'static str {
    if VIDEO_ASK.is_match(ask) || H3_ASK.is_match(ask) {
        return "h3_video";
    }
    match (family, images) {
        (QWEN21, 1) => "qwen_edit",
        (QWEN21, n) if n >= 2 => "qwen_multiref",
        _ => family_guide(family),
    }
}

/// 動手做模式：這個動作要照哪份寫法寫（改圖、影片、Qwen 用附圖畫）
pub fn needed_guide(action: &Map<String, Value>, family: &str, refs_mode: &str) -> Option<&'static str> {
    match tx(action.get("action").unwrap_or(&Value::Null)).as_str() {
        "call" => match tx(action.get("tool").unwrap_or(&Value::Null)).as_str() {
            "edit" => Some("qwen_edit"),
            "video" => Some("h3_video"),
            _ => None,
        },
        "generate" if family == QWEN21 && refs_mode == "multi" => match action.get("refs").and_then(Value::as_array).map(Vec::len).unwrap_or(0) {
            0 => None,
            1 => Some("qwen_edit"),
            _ => Some("qwen_multiref"),
        },
        _ => None,
    }
}

/// 這段對話讀過這份寫法沒（guide 工具成功過）
pub fn guide_read(events: &[Value], name: &str) -> bool {
    events.iter().any(|e| ev_kind(e) == "tool" && e["tool"] == "guide" && e["ok"] != false && e["args"]["name"] == name)
}

/// 寫提示詞說明裡的片語：出現在提示詞裡就是抄了說明（小寫、空白已正規化）
const GUIDE_ECHO: &[&str] = &[
    "subject and look first",
    "then pose and expression",
    "camera angle and shot size, then",
    "then style or medium",
    "one paragraph of natural english",
    "then the people count",
    "real danbooru tags in lowercase",
    "real danbooru spellings only",
    "subject and look,",
    "work in this order",
    "positional phrases",
    "the medium noun",
    "what to describe, in this order",
    "fit the action to the length",
    "the governing principle",
    "about 20 to 35",
    "every tag once",
];

const SIZES_QWEN: &str = "portrait 832x1248 or 1024x1536, square 1024x1024, landscape 1248x832 or 1536x1024";
/// Anima、MOSS 用這組（MOSS 官方：總像素 1M～2M、16 的倍數）
const SIZES_ANIMA: &str = "portrait 832x1216, square 1024x1024, landscape 1216x832";

/// 這個家族的預設尺寸（模型沒寫或寫壞時用）：直圖
pub fn default_size(family: &str) -> (i64, i64) {
    if family == QWEN21 {
        (832, 1248)
    } else {
        (832, 1216)
    }
}

/// 尺寸修正：32 的倍數、每邊 512～1664、總像素最多約 1.6M（Qwen 1024x1536）
pub fn snap_size(w: i64, h: i64) -> (i64, i64) {
    let r = |v: i64| ((v.clamp(512, 1664) + 16) / 32 * 32).clamp(512, 1664);
    let (mut w, mut h) = (r(w), r(h));
    let max = 1024 * 1536 + 1;
    if w * h > max {
        let s = (max as f64 / (w * h) as f64).sqrt();
        w = ((w as f64 * s) as i64 / 32 * 32).max(512);
        h = ((h as f64 * s) as i64 / 32 * 32).max(512);
    }
    (w, h)
}

// ================================================================ LoRA 目錄
/// 助手看得到的一個 LoRA
#[derive(Debug, Clone, PartialEq)]
pub struct LoraInfo {
    /// ComfyUI 的名稱（qwen21\snowglow_qwen21_2000.safetensors）
    pub name: String,
    pub title: String,
    pub triggers: Vec<String>,
    /// 做什麼用的（英文一句；沒有就是空的）
    pub desc: String,
    pub strength: f64,
    pub min: f64,
    pub max: f64,
    pub fav: bool,
}

impl LoraInfo {
    /// 從 /api/state 那種 LoRA 物件（name＋lora_meta 欄位）讀出來
    pub fn from_meta(name: &str, m: &Value) -> LoraInfo {
        let num = |k: &str, d: f64| m.get(k).and_then(crate::py::float).unwrap_or(d);
        let strength = num("strength", 1.0);
        let (lo, hi) = (num("min", 0.0), num("max", 0.0));
        let (min, max) = if hi > lo { (lo, hi) } else { ((strength - 0.6).max(0.1), (strength + 0.6).min(2.0)) };
        let ag = &m["agent"];
        let desc = tx(&ag["desc"]);
        LoraInfo {
            name: name.to_string(),
            title: tx(&m["title"]),
            triggers: m["triggers"].as_array().into_iter().flatten().map(tx).filter(|t| !t.trim().is_empty()).collect(),
            desc: if desc.trim().is_empty() { tx(&m["note"]).lines().next().unwrap_or("").to_string() } else { desc },
            strength,
            min,
            max,
            fav: truthy(m.get("fav")),
        }
    }

    /// 檔名（不含資料夾和副檔名）
    pub fn stem(&self) -> String {
        let base = self.name.rsplit(['/', '\\']).next().unwrap_or(&self.name);
        base.strip_suffix(".safetensors").unwrap_or(base).to_string()
    }

    /// 助手可以自己挑嗎：調年齡的 LoRA（年齡滑桿之類）只有使用者在生成頁親手掛才用，助手不會自己加
    pub fn agent_may_pick(&self) -> bool {
        static AGE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)(^|[^a-z])(age|aging|de-?aging|teens?|loli|shota|younger)([^a-z]|$)|年齡|年紀|蘿莉|正太|年龄|年纪|萝莉").unwrap()); // 大陸的 LoRA 標題多是簡體
        ![self.name.as_str(), &self.title, &self.desc].iter().any(|t| AGE.is_match(t))
    }
}

/// 名字對到目錄裡的 LoRA：完整名稱 → 檔名（不分大小寫、可省副檔名）→ 標題
pub fn find_lora<'a>(list: &'a [LoraInfo], name: &str) -> Option<&'a LoraInfo> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    let n2 = n.replace('/', "\\");
    let stem = n2.rsplit('\\').next().unwrap_or(&n2).trim_end_matches(".safetensors").to_string();
    list.iter()
        .find(|l| l.name.to_lowercase() == n2)
        .or_else(|| list.iter().find(|l| l.stem().to_lowercase() == stem))
        .or_else(|| list.iter().find(|l| !l.title.is_empty() && l.title.to_lowercase() == n))
}

/// 工作說明裡的 LoRA 段落。fixed＝使用者在生成頁固定掛的（一律套用，不給它選）
pub fn loras_text(list: &[LoraInfo], fixed: &[LoraInfo], allow: bool) -> String {
    let mut out = vec![];
    if !fixed.is_empty() {
        out.push(format!(
            "Always on (chosen by the user, already applied, do not list them in \"loras\"): {}.",
            fixed.iter().map(|l| if l.triggers.is_empty() { l.stem() } else { format!("{} (trigger words: {})", l.stem(), l.triggers.join(", ")) }).collect::<Vec<_>>().join("; ")
        ));
    }
    let free: Vec<&LoraInfo> = list.iter().filter(|l| l.agent_may_pick() && !fixed.iter().any(|f| f.name == l.name)).collect();
    if !allow || free.is_empty() {
        out.push("You cannot add LoRAs: always write \"loras\":[].".into());
        return out.join("\n");
    }
    out.push(format!(
        "Use one only when its description clearly matches what the user wants (at most {} in total). Put its trigger words in the prompt. Start at its default strength; if its effect is too weak raise it by 0.2, if it breaks the picture lower it. If none fits, write \"loras\":[].",
        MAX_LORAS.saturating_sub(fixed.len()).max(1)
    ));
    for l in free.iter().take(60) {
        let mut line = format!("- {}", l.name);
        if !l.title.is_empty() && l.title != l.stem() {
            line += &format!(" | {}", l.title);
        }
        line += &format!(" | {}", if l.desc.trim().is_empty() { "(unknown purpose: guess from the name, use only if the name clearly matches)".to_string() } else { l.desc.trim().to_string() });
        if !l.triggers.is_empty() {
            line += &format!(" | trigger words: {}", l.triggers.join(", "));
        }
        line += &format!(" | strength {} ({}-{})", fmt_num(l.strength), fmt_num(l.min), fmt_num(l.max));
        out.push(line);
    }
    out.join("\n")
}

fn fmt_num(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 整份工作說明：system＝工作說明、guide＝這個家族的寫法（都可能是使用者改過的）。工具段落填進 {tools}，沒有就接在最後
pub fn system_prompt(system: &str, guide: &str, family: &str, loras: &str) -> String {
    let sizes = if family == QWEN21 { SIZES_QWEN } else { SIZES_ANIMA };
    crate::agent_tools::with_tools(&system.replace("{model_guide}", guide).replace("{sizes}", sizes).replace("{loras}", loras))
}

/// 只寫詞模式的工作說明：{target}＝寫法的名稱和用途、{guide}＝那份寫法全文、{sizes}＝尺寸
pub fn prompt_system(system: &str, guide: &str, guide_name: &str, family: &str) -> String {
    let sizes = if family == QWEN21 { SIZES_QWEN } else { SIZES_ANIMA };
    let target = GUIDES.iter().find(|g| g.0 == guide_name).map(|g| g.3).unwrap_or("the image model");
    system.replace("{target}", target).replace("{guide}", guide).replace("{sizes}", sizes)
}

/// 只寫詞模式：讀回 {"action":"prompt",…} 或 {"action":"reply",…}；不合格回傳要退回的英文說明。
/// guide_name 決定檢查：Qwen 改圖／多圖可以寫中文，Anima 修 tag，圖片才有尺寸
pub fn parse_prompt_action(raw: &str, family: &str, guide_name: &str) -> Result<Map<String, Value>, String> {
    let Some(m) = json_in(raw) else {
        return Err("Your reply was not a JSON object. Reply with exactly ONE JSON object, nothing else.".into());
    };
    let action = tx(m.get("action").unwrap_or(&Value::Null)).trim().to_lowercase();
    let is_reply = action == "reply" || (action.is_empty() && !m.contains_key("prompt"));
    if is_reply {
        let msg = tx(m.get("message").or(m.get("say")).or(m.get("note")).unwrap_or(&Value::Null)).trim().to_string();
        if msg.is_empty() {
            return Err("\"message\" is empty. Write your reply to the user in Traditional Chinese.".into());
        }
        return Ok(Map::from_iter([("action".into(), json!("reply")), ("message".into(), json!(msg))]));
    }
    if !matches!(action.as_str(), "prompt" | "generate" | "") {
        return Err(format!("Unknown action \"{action}\". Use prompt or reply."));
    }
    let mut prompt = tx(m.get("prompt").unwrap_or(&Value::Null)).trim().to_string();
    if prompt.chars().count() < 15 {
        return Err("\"prompt\" is missing or too short. Write the complete prompt.".into());
    }
    let edit = matches!(guide_name, "qwen_edit" | "qwen_multiref");
    if !edit && has_cjk_outside_quotes(&prompt) {
        return Err("The prompt must be written in English (only text that must appear inside the picture may stay in quotes). Reply again.".into());
    }
    let low = norm(&prompt);
    if let Some(p) = GUIDE_ECHO.iter().find(|p| low.contains(*p)) {
        return Err(format!("Your prompt copied the writing instructions (\"{p}\"). Follow them instead: describe the actual picture."));
    }
    let video = guide_name.ends_with("_video");
    if !video {
        prompt = crate::director::safe_text(&prompt, guide_name == "anima");
    }
    match guide_name {
        "anima" => prompt = crate::pe::fix_anima_tags(&prompt),
        "qwen" | "moss" => prompt = crate::pe::strip_ratio(&prompt),
        _ => {}
    }
    let mut out = Map::from_iter([
        ("action".into(), json!("prompt")),
        ("prompt".into(), json!(prompt)),
        ("negative".into(), json!(tx(m.get("negative").or(m.get("negative_prompt")).unwrap_or(&Value::Null)).trim())),
        ("note".into(), json!(tx(m.get("note").or(m.get("message")).or(m.get("say")).unwrap_or(&Value::Null)).trim())),
        ("guide".into(), json!(guide_name)),
    ]);
    if !video && !edit {
        let (dw, dh) = default_size(family);
        let w = m.get("width").and_then(int_of).unwrap_or(dw);
        let h = m.get("height").and_then(int_of).unwrap_or(dh);
        let (w, h) = snap_size(w, h);
        out.insert("width".into(), json!(w));
        out.insert("height".into(), json!(h));
    }
    Ok(out)
}

/// 使用者附圖時的說明（依能不能把圖餵給生圖模型分三種）。工作說明裡有 {images} 就填在那裡，沒有（舊的自訂說明）就接在最後
/// refs_mode：multi＝Qwen 2.1 本機（多圖參考／改圖）、one＝Anima／MOSS 本機（圖生圖重繪一張）、""＝只能看（雲端或其他模型）
pub fn images_text(refs_mode: &str) -> String {
    let head = "The user may attach images to a message. They are shown to you as \"User image N\" (N stays the same for the whole chat). \
Look at them closely before writing anything: they show what the user means (a character, an outfit, a pose, a place, a style) or a picture the user wants changed. \
\"check\" is only about the images YOU generated (judged against the request and the user's images); before your first round it stays [].";
    let how = match refs_mode {
        "multi" => "When the user wants THEIR picture changed or reused (\"this image\", \"same person\", \"change the hair\", \"put her in…\", \"combine these\"), you must use refs. \
To build on user images, add \"refs\":[N,...] to generate (user image numbers, in the order you want them). With refs the image model edits or combines those images: \
one ref = redraw that image following the prompt (change the clothes, the background, the pose, the style…); \
two or more = combine them, and the prompt must name each one as <image1>, <image2>… in the order of refs (for example \"the woman from <image1> wearing the dress from <image2>, standing on a beach\"). \
Describe in the prompt what to keep and what to change. Leave refs out to draw from text only; when a user image is only a loose idea or style reference, describe it in words instead.",
        "one" => "When the user wants THEIR picture changed or redrawn (\"this image\", \"same person, same pose\", \"change the hair / background\"), you must use refs: \
describe the new version in the prompt (the new hair, the new background…) and drop what is being replaced. To redraw a user image, add \"refs\":[N] (exactly one image) and \"denoise\" to generate: 0.3 = touch-ups only; 0.5 = same picture with small changes (expression, small details); \
0.7 = keeps the look and pose, can change the background and lighting; 0.85 = keeps the pose and composition but can change hair color and clothes; 0.95 = keeps only the rough composition. \
Changing the background needs at least 0.7; changing a hair or clothing color needs at least 0.85; turning the person into a zombie or monster, or adding heavy wounds and gore, needs 0.9. \
Remove from the prompt the words for what is being replaced (new background on a beach → no cafe, window or coffee machine words). \
If your result kept too much of the original (old color, old background), raise denoise by 0.1 instead of only repeating words. \
The prompt still describes the whole picture you want. Leave refs out to draw from text only; when the user wants a similar character or style, describe it in the prompt instead. Redrawing keeps the angle and pose: for a turnaround, a back or side view or other angles of the user's character, call the character_sheet tool with \"ref\":N instead. \
To change only the pose, the arms or the hands while keeping everything else, redrawing does not work either (low denoise keeps the old pose, high denoise turns the picture into another person): call the edit tool with \"ref\":N and an instruction that names only that change.",
        _ => return format!("{head}\nYou can look at user images but cannot feed them to the image model: describe in the prompt everything you need from them."),
    };
    // 2026-10-29 使用者轉來回報：要「原圖不變只改手臂」，助手卻先畫一張新的 CG 人再在上面改，新畫風之後洗不掉
    let keep = "When the user wants THEIR picture kept and only something changed, every try starts from the user's picture (refs, or the edit tool with \"ref\":N). \
Never draw a new picture from text first and then fix or edit it toward the user's picture: its new face and style stay for good. \
If a try changed the person or the style, go back to the user's picture with a clearer instruction instead of working on that result. \
Edit your own result (edit with \"id\") only when it already looks like the user's picture and needs one more change.";
    format!("{head}\n{how}\n{keep}")
}

/// 工作說明＋使用者附圖的說明
pub fn with_images(system: &str, refs_mode: &str) -> String {
    let t = images_text(refs_mode);
    if system.contains("{images}") {
        system.replace("{images}", &t)
    } else {
        format!("{system}\n\nUSER IMAGES\n{t}")
    }
}

/// 這個請求（最後一句不是插話的使用者訊息起）附的圖的編號
pub fn request_user_images(events: &[Value]) -> Vec<i64> {
    let start = events.iter().rposition(|e| ev_kind(e) == "user" && !truthy(e.get("mid"))).unwrap_or(0);
    user_images(&events[start..]).iter().filter_map(|i| i["n"].as_i64()).collect()
}

/// 整個對話裡使用者附過的圖（照順序；每張有 n＝第幾張，從 1 開始）
pub fn user_images(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|e| ev_kind(e) == "user")
        .flat_map(|e| e["images"].as_array().cloned().unwrap_or_default())
        .filter(|i| i["n"].as_i64().is_some() && !tx(&i["name"]).is_empty())
        .collect()
}

// ================================================================ 對話 → 給模型的 messages
/// 事件種類（存檔裡的 kind）：
///   user   {text, mid?}                     使用者說的話（mid＝助手跑到一半插話）
///   act    {action:{...}, round?, secs?}     助手的一個動作（檢查過、正規化過的 JSON）
///   result {round, ids, secs, errors, tool?} 一輪生成的結果（圖庫編號）；tool＝工具做出來的（look＝只是拿來看）
///   tool   {tool, ok, text, zh}             工具的結果（text 給模型、zh 給使用者）
///   confirm {call:{tool,args}, why, state}  要使用者准的工具（state：wait／yes／no）
///   note / error {text}                     只顯示在畫面上（自動補觸發詞、出錯）
pub fn ev_kind(e: &Value) -> &str {
    e["kind"].as_str().unwrap_or("")
}

/// 這個請求用掉幾輪：最後一句「不是插話」的使用者訊息之後的 generate 次數
pub fn rounds_used(events: &[Value]) -> i64 {
    let start = events
        .iter()
        .rposition(|e| ev_kind(e) == "user" && !truthy(e.get("mid")))
        .map(|i| i + 1)
        .unwrap_or(0);
    events[start..]
        .iter()
        .filter(|e| ev_kind(e) == "act" && e["action"]["action"] == "generate")
        .count() as i64
}

/// 使用者最後一句話（插話不算）對到體位庫的體位（事件的 pose，見 Poses::term_note）：照那個體位補正助手寫的提示詞。
/// Anima 補人數、sex、體位 tag，拿掉打架的；MOSS、Qwen 2.1 沒提到體位就接上驗證過的擺法。Anima 另外補使用者講了、模型漏掉的外觀／鏡頭詞。
/// 只看最後一句：下一句講別的（換背景、改穿衣服）就不再硬塞。回傳（提示詞, 給使用者看的一句說明）
pub fn pose_fix(poses: &crate::poses::Poses, events: &[Value], family: &str, prompt: &str) -> (String, Option<String>) {
    let last = events.iter().rev().find(|e| ev_kind(e) == "user" && !truthy(e.get("mid")));
    let pose = last.and_then(|e| e["pose"]["id"].as_str()).and_then(|id| poses.get(id));
    let text = last.map(|e| tx(&e["text"])).unwrap_or_default();
    polish(pose, family == ANIMA, prompt, &text)
}

/// 照使用者這句話補正一份提示詞：對到體位就照體位庫（[`fix_by_pose`]）；Anima 再補 [`crate::poses::asked_tags`]（雙馬尾、黑絲、第一人稱…）。
/// 助手畫圖前（pose_fix）和 AI 改寫的 Anima 版／文生圖版共用。回傳（新提示詞, 給使用者看的說明）
pub fn polish(pose: Option<&crate::poses::Pose>, anima: bool, prompt: &str, text: &str) -> (String, Option<String>) {
    let (s, pose_msg) = match pose {
        Some(p) => fix_by_pose(p, anima, prompt, text),
        None => (prompt.to_string(), None),
    };
    if !anima {
        return (s, pose_msg);
    }
    let (s, added) = crate::poses::insert_tags(&s, &crate::poses::asked_tags(text));
    let asked_msg = (!added.is_empty()).then(|| format!("照你講的補上 {}", added.join(", ")));
    let msg = [pose_msg, asked_msg].into_iter().flatten().collect::<Vec<_>>().join("；");
    (s, (!msg.is_empty()).then_some(msg))
}

/// 照體位庫補正：Anima 補 tag／拿掉打架的 tag，句子型（MOSS、Qwen）沒提到體位就接上擺法。
/// text＝使用者的原話：講了床以外的地點（[`crate::poses::names_place`]）就不寫床、不硬塞體位附帶的場景 tag
pub fn fix_by_pose(p: &crate::poses::Pose, anima: bool, prompt: &str, text: &str) -> (String, Option<String>) {
    let place = crate::poses::names_place(text);
    if !anima {
        return match p.fix_text_at(prompt, place) {
            Some(s) => (s, Some(format!("提示詞沒寫到「{}」，接上體位庫驗證過的擺法", p.zh))),
            None => (prompt.to_string(), None),
        };
    }
    let (s, added, removed) = p.fix_anima_at(prompt, place);
    if added.is_empty() && removed.is_empty() {
        return (s, None);
    }
    let mut msg = format!("照體位庫「{}」補正提示詞", p.zh);
    if !added.is_empty() {
        msg.push_str(&format!("：補上 {}", added.join(", ")));
    }
    if !removed.is_empty() {
        msg.push_str(&format!("；拿掉 {}", removed.join(", ")));
    }
    (s, Some(msg))
}

/// 這個請求叫了幾次工具
pub fn calls_used(events: &[Value]) -> i64 {
    let start = events
        .iter()
        .rposition(|e| ev_kind(e) == "user" && !truthy(e.get("mid")))
        .map(|i| i + 1)
        .unwrap_or(0);
    events[start..]
        .iter()
        .filter(|e| ev_kind(e) == "act" && e["action"]["action"] == "call")
        .count() as i64
}

/// 這個請求使用者說的話（最後一句不是插話的，加上之後的插話）
pub fn request_text(events: &[Value]) -> String {
    let start = events.iter().rposition(|e| ev_kind(e) == "user" && !truthy(e.get("mid"))).unwrap_or(0);
    events[start..].iter().filter(|e| ev_kind(e) == "user").map(|e| tx(&e["text"])).collect::<Vec<_>>().join("\n")
}

/// 這個請求還剩幾輪。auto 關掉＝只畫一輪；用完了但使用者在最後一輪之後插話，多給一輪照他的話改
pub fn rounds_left(events: &[Value], rounds: i64, auto: bool) -> i64 {
    let budget = if auto { rounds } else { 1 };
    let left = budget - rounds_used(events);
    if left > 0 {
        return left;
    }
    let last_act = events.iter().rposition(|e| matches!(ev_kind(e), "act" | "result"));
    let mid_after = events
        .iter()
        .enumerate()
        .any(|(i, e)| ev_kind(e) == "user" && truthy(e.get("mid")) && last_act.is_none_or(|a| i > a));
    if mid_after {
        1
    } else {
        0
    }
}

/// 這個請求裡畫過的所有圖（交卷只能從這些挑）
pub fn request_images(events: &[Value]) -> Vec<i64> {
    let start = events
        .iter()
        .rposition(|e| ev_kind(e) == "user" && !truthy(e.get("mid")))
        .unwrap_or(0);
    // look 工具的結果只是拿來看的圖（2026-10-29 盤點：算進來會讓「只看圖」的請求交卷 best:[] 被退回）
    events[start..]
        .iter()
        .filter(|e| ev_kind(e) == "result" && e["tool"] != "look")
        .flat_map(|e| e["ids"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_i64())
        .collect()
}

/// 上一個 generate 動作（比對「提示詞有沒有改」）
pub fn last_generate(events: &[Value]) -> Option<&Value> {
    events
        .iter()
        .rev()
        .find(|e| ev_kind(e) == "act" && e["action"]["action"] == "generate")
        .map(|e| &e["action"])
}

/// 最後一件事是不是一輪結果（是的話這次要給它看圖）
pub fn latest_result(events: &[Value]) -> Option<&Value> {
    events.iter().rev().find(|e| matches!(ev_kind(e), "act" | "result")).filter(|e| ev_kind(e) == "result")
}

fn short(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        t + "…"
    } else {
        t
    }
}

/// 組 messages。look＝這次要給它看的圖（最新一輪：(圖庫編號, image_url 物件)）；舊的圖只留文字。
pub fn build_messages(system: &str, events: &[Value], look: &[(i64, Value)], rounds_left: i64, count: i64) -> Value {
    build_messages_with(system, events, look, &[], rounds_left, count)
}

/// 同上，再加使用者附的圖：uimgs＝(第幾張, image_url 物件)，只有在這裡面的才真的給它看，其他留一行文字
pub fn build_messages_with(system: &str, events: &[Value], look: &[(i64, Value)], uimgs: &[(i64, Value)], rounds_left: i64, count: i64) -> Value {
    build_messages_ending(system, events, look, uimgs, rounds_left, count, None)
}

/// 只寫詞模式：沒有輪數、不看自己的圖，最後一句換成「給提示詞」
pub fn build_prompt_messages(system: &str, events: &[Value], uimgs: &[(i64, Value)]) -> Value {
    build_messages_ending(system, events, &[], uimgs, 0, 0, Some("Reply with ONE JSON object: the prompt action (or reply only if you truly cannot start)."))
}

fn build_messages_ending(system: &str, events: &[Value], look: &[(i64, Value)], uimgs: &[(i64, Value)], rounds_left: i64, count: i64, ending: Option<&str>) -> Value {
    let mut msgs = vec![json!({"role": "system", "content": system})];
    let mut buf: Vec<Value> = vec![];
    let text = |t: String| json!({"type": "text", "text": t});
    // 太長：更早的事件濃縮成一段
    let acts: Vec<usize> = events.iter().enumerate().filter(|(_, e)| ev_kind(e) == "act").map(|(i, _)| i).collect();
    let cut = if acts.len() > MAX_ACTS { acts[acts.len() - MAX_ACTS] } else { 0 };
    if cut > 0 {
        let asks: Vec<String> = events[..cut].iter().filter(|e| ev_kind(e) == "user").map(|e| short(&tx(&e["text"]), 200)).collect();
        let prompts: Vec<String> = events[..cut]
            .iter()
            .filter(|e| ev_kind(e) == "act" && matches!(e["action"]["action"].as_str(), Some("generate" | "prompt")))
            .map(|e| short(&tx(&e["action"]["prompt"]), 160))
            .collect();
        buf.push(text(format!(
            "Earlier in this chat (summary). The user asked: {}. Your earlier prompts, oldest first: {}",
            asks.join(" / "),
            if prompts.is_empty() { "(none)".into() } else { prompts.join(" || ") }
        )));
    }
    let latest_round = latest_result(events).and_then(|e| e["round"].as_i64());
    let flush = |buf: &mut Vec<Value>, msgs: &mut Vec<Value>| {
        if buf.is_empty() {
            buf.push(json!({"type": "text", "text": "(continue)"}));
        }
        let only_text = buf.iter().all(|p| p["type"] == "text");
        let content = if only_text {
            json!(buf.iter().map(|p| tx(&p["text"])).collect::<Vec<_>>().join("\n\n"))
        } else {
            json!(buf.clone())
        };
        msgs.push(json!({"role": "user", "content": content}));
        buf.clear();
    };
    for e in &events[cut..] {
        match ev_kind(e) {
            "user" => {
                let t = tx(&e["text"]);
                buf.push(text(if truthy(e.get("mid")) {
                    format!("The user added while you were working: {t}")
                } else {
                    format!("The user says: {t}")
                }));
                if let Some(n) = e.get("pose").filter(|n| n.is_object()) {
                    buf.push(text(crate::poses::Poses::term_line(n)));
                }
                for im in e["images"].as_array().into_iter().flatten() {
                    let Some(n) = im["n"].as_i64() else { continue };
                    match uimgs.iter().find(|(k, _)| *k == n) {
                        Some((_, part)) => {
                            buf.push(text(format!("User image {n}:")));
                            buf.push(part.clone());
                        }
                        None => buf.push(text(format!("(User image {n} was attached here; it is no longer shown.)"))),
                    }
                }
            }
            "act" => {
                flush(&mut buf, &mut msgs);
                msgs.push(json!({"role": "assistant", "content": serde_json::to_string(&e["action"]).unwrap_or_default()}));
            }
            "result" => {
                let round = e["round"].as_i64().unwrap_or(0);
                let ids: Vec<i64> = e["ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
                let errs: Vec<String> = e["errors"].as_array().into_iter().flatten().map(tx).filter(|s| !s.is_empty()).collect();
                let err = if errs.is_empty() { String::new() } else { format!(" Errors: {}", errs.join("; ")) };
                let seen = e["tool"] == "look";
                if Some(round) == latest_round && !look.is_empty() && seen {
                    buf.push(text(format!("The images you asked to look at ({} images):", look.len())));
                    for (id, part) in look {
                        buf.push(text(format!("Image id {id}:")));
                        buf.push(part.clone());
                    }
                } else if seen {
                    buf.push(text(format!("You looked at images {ids:?} (no longer shown).")));
                } else if Some(round) == latest_round && !look.is_empty() {
                    buf.push(text(format!("Round {round} results ({} images).{err} Look carefully at each image:", look.len())));
                    for (id, part) in look {
                        buf.push(text(format!("Image id {id}:")));
                        buf.push(part.clone());
                    }
                } else if ids.is_empty() {
                    buf.push(text(format!("Round {round}: no image was made.{err}")));
                } else {
                    buf.push(text(format!("Round {round} results: image ids {ids:?} (no longer shown; rely on your check notes).{err}")));
                }
            }
            "tool" => buf.push(text(format!("Tool {} {}: {}", tx(&e["tool"]), if e["ok"] == false { "failed" } else { "result" }, tx(&e["text"])))),
            _ => {}
        }
    }
    // 剛看完圖：只給「重畫或交卷」兩條路（9B 看到 reply 就會停下來問人）；用 look 看的圖不是它畫的，照請求繼續
    let looked = latest_result(events).is_some_and(|e| e["tool"] == "look");
    buf.push(text(match (rounds_left > 0, look.is_empty()) {
        _ if ending.is_some() => ending.unwrap_or_default().to_string(),
        (_, false) if looked => "Use what you see to carry on with the user's request. ONE JSON object.".to_string(),
        (true, true) => format!("Rounds left for this request: {rounds_left} ({count} images per round). Reply with ONE JSON object."),
        (true, false) => format!(
            "Rounds left for this request: {rounds_left} ({count} images per round). Fill check from what you see, then generate a fix or finish. ONE JSON object."
        ),
        (false, false) => "No rounds are left for this request: fill check from what you see and finish. ONE JSON object.".to_string(),
        (false, true) => "No rounds are left for this request: reply with the finish action (or reply). ONE JSON object.".to_string(),
    }));
    flush(&mut buf, &mut msgs);
    Value::Array(msgs)
}

// ================================================================ 讀回模型的動作並檢查
/// 檢查時要知道的事
pub struct Check<'a> {
    pub family: &'a str,
    pub rounds_left: i64,
    /// 上一輪的 generate（比對提示詞有沒有改）
    pub last: Option<&'a Value>,
    /// 這次有沒有給它看圖（有就一定要逐項打勾）
    pub saw_images: bool,
    /// 這個請求畫過的圖（交卷只能挑這些）
    pub images: &'a [i64],
    pub catalog: &'a [LoraInfo],
    pub fixed: &'a [LoraInfo],
    pub allow_loras: bool,
    /// 已經提醒過「打了 no 還有輪數卻交卷」：第二次就放行
    pub nudged: bool,
    /// 使用者附過的圖的編號（refs 只能挑這些）
    pub user_images: &'a [i64],
    /// 這個請求附的圖（附了卻沒用 refs：提醒一次）
    pub request_user_images: &'a [i64],
    /// 能不能把使用者的圖餵給生圖模型：multi／one／""（見 images_text）
    pub refs_mode: &'a str,
    /// 這個請求使用者說的話（判斷「要新角度」之類）
    pub ask: &'a str,
    /// 這個請求還能叫幾次工具（agent_tools::MAX_CALLS）
    pub calls_left: i64,
}

static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""[^"]*"|“[^”]*”|「[^」]*」|『[^』]*』"#).unwrap());
static CJK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{Han}\p{Hiragana}\p{Katakana}\p{Hangul}]").unwrap());
static IMAGE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<image(\d+)>").unwrap());
// 9B 會把 JSON 欄位寫進提示詞裡（"…, refs:[1], denoise:0.75, 832x1216"）
static FIELD_IN_PROMPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)"?\b(refs?|denoise)"?\s*[:=]\s*(\[[^\]]*\]|[0-9]+(?:\.[0-9]+)?)"#).unwrap());
// 單獨一項的尺寸：832x1216、1216、832（三位數 200 以上才算，年齡的 20 不動），還有抄進來的 loras:[]
static SIZE_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:\d{3,4}\s*[x×]\s*\d{3,4}|[2-9]\d{2}|\d{4}|loras?\s*[:=].*)$").unwrap());
// 要角色的新角度：三視圖、轉身、背面、側面、多視角…
static NEW_VIEW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)三視圖|三视图|四視圖|四视图|多視角|多视角|轉身|转身|背面|背影|側面|侧面|設定圖|设定图|turn-?around|back view|side view|character sheet|other angles?").unwrap());
// 要換姿勢、手臂、手勢（圖生圖做不到：低重繪留著舊姿勢，高重繪變成另一個人）
static POSE_CHANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)姿勢|姿势|姿態|姿态|動作|动作|手臂|胳膊|手勢|手势|舉起|举起|抬起|抬手|舉手|举手|叉腰|插腰|\bpose\b|\barms?\b|\braise").unwrap());
// 說自己「不用 refs」：英文 no／without，中文「不帶 refs」（2026-10-29 實測 Fable 寫「用文字重繪，不帶 refs」被當成說要用、連擋三次）
static NO_REFS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\bno|without|not use|don't use|不帶|不带|不用|沒有|没有|不使用|不加|無|无)\s*(?:the\s*)?refs").unwrap());
/// 要換姿勢時圖生圖最多准到多少重繪（再高就是畫成另一個人，改叫 edit）
const POSE_REDRAW_MAX: f64 = 0.5;
// 「姿勢不變」那種反過來的說法
static KEEP_POSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:姿勢|姿势|動作|动作|pose)\s*(?:不變|不变|不要變|不要变|保持|一樣|一样)|(?:同樣|同样|一樣的|一样的|保持)\s*(?:的)?\s*(?:姿勢|姿势|動作|动作)|same pose|keep (?:the )?pose").unwrap());
static NO_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)[:：]\s*(?:no\b|not\b|missing|wrong|✗|❌|否|沒有|不對)").unwrap());

/// 引號外面有中日韓文字（要畫進圖裡的字可以放引號裡）
pub fn has_cjk_outside_quotes(s: &str) -> bool {
    CJK.is_match(&QUOTED.replace_all(s, ""))
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// check 的一項：字串照用；9B 有時寫成 {"element":"…","check":"no - …"}（或把這個物件包成字串），攤平成「element: no - …」
fn check_item(v: &Value) -> String {
    let obj = match v {
        Value::Object(o) => Some(o.clone()),
        Value::String(s) if s.trim_start().starts_with('{') => serde_json::from_str::<Map<String, Value>>(s.trim()).ok(),
        _ => None,
    };
    let Some(o) = obj else { return tx(v) };
    let word = |v: &Value| match v {
        Value::Bool(b) => (if *b { "yes" } else { "no" }).to_string(),
        v => tx(v).trim().to_string(),
    };
    let pick = |keys: &[&str]| keys.iter().find_map(|k| o.get(*k)).map(word);
    match (pick(&["element", "item", "name", "requirement"]), pick(&["check", "status", "result", "ok", "answer", "done"])) {
        (Some(l), Some(r)) => format!("{l}: {r}"),
        _ => o.values().map(word).collect::<Vec<_>>().join(": "),
    }
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(check_item).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => s.lines().map(|l| l.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => vec![],
    }
}

/// refs 的一項：數字、"1"、"User image 1"、"<image1>"、{"n":1} 都算
fn ref_num(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => int_of(v).or_else(|| {
            let d: String = s.chars().rev().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
            d.chars().rev().collect::<String>().parse().ok()
        }),
        Value::Object(o) => ["n", "image", "id", "index", "number"].iter().find_map(|k| o.get(*k).and_then(ref_num)),
        _ => int_of(v),
    }
}

fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().trim_start_matches('#').parse().ok(),
        _ => None,
    }
}

/// 9B 常把 check 寫成不合法的 JSON：["hair":null, …] 或 [{"element":"hair": "yes"}, …]。
/// 找出 "check":[…] 那段，每一項取出字串（去掉 element／value 這類鍵名）接成「hair: yes」，換成合法陣列再讀一次
fn repair_check(raw: &str) -> Option<Map<String, Value>> {
    let key = raw.find("\"check\"")?;
    let open = key + raw[key..].find('[')?;
    let b = raw.as_bytes();
    let (mut depth, mut in_str, mut esc, mut close) = (0i32, false, false, None);
    let mut cuts = vec![open + 1];
    for (i, &ch) in b.iter().enumerate().skip(open) {
        if in_str {
            match ch {
                _ if esc => esc = false,
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            b'"' => in_str = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            b',' if depth == 1 => cuts.push(i + 1),
            _ => {}
        }
    }
    let close = close?;
    cuts.push(close + 1);
    static LIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""((?:[^"\\]|\\.)*)"|\btrue\b|\bfalse\b"#).unwrap());
    const KEYS: &[&str] = &["element", "item", "name", "requirement", "check", "status", "result", "ok", "answer", "done", "value", "note", "why"];
    let items: Vec<String> = cuts
        .windows(2)
        .map(|w| {
            let part = &raw[w[0]..w[1] - 1];
            LIT.captures_iter(part)
                .map(|c| match c.get(1) {
                    Some(m) => serde_json::from_str::<String>(&format!("\"{}\"", m.as_str())).unwrap_or_else(|_| m.as_str().to_string()),
                    None => (if &c[0] == "true" { "yes" } else { "no" }).to_string(),
                })
                .filter(|t| !KEYS.contains(&t.trim().to_lowercase().as_str()))
                .collect::<Vec<_>>()
                .join(": ")
        })
        .filter(|t| !t.trim().is_empty())
        .collect();
    let fixed = format!("{}{}{}", &raw[..open], json!(items), &raw[close + 1..]);
    json_in(&fixed)
}

/// 35B 會把 prompt 的 tag 寫完先關掉字串，英文句子另外當成沒有鍵名的字串放在後面：
/// "prompt": "masterpiece, …, from side", "She lies on her back…", "width": 832（2026-10-28 擂台交配按壓連 4 次這樣，內容是對的）。
/// 把這種落單的字串接回 prompt
fn repair_prompt_tail(raw: &str) -> Option<Map<String, Value>> {
    static TAIL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"("prompt"\s*:\s*"(?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*([,}])"#).unwrap()
    });
    let mut fixed = raw.to_string();
    while let Some(c) = TAIL.captures(&fixed) {
        let joined = format!("{}, {}\"{}", &c[1], &c[2], &c[3]);
        fixed.replace_range(c.get(0)?.range(), &joined);
    }
    (fixed != raw).then(|| json_in(&fixed)).flatten()
}

/// 9B 會把 refs／denoise 寫進提示詞字串裡、而且沒把字串收好（"prompt":"…, refs:[1],"width":832）：整個 JSON 壞掉。
/// 把那一段拆回成欄位（2026-10-29 真模型實測 heretic-v2 連三次都這樣）
fn repair_field_leak(raw: &str) -> Option<Map<String, Value>> {
    static LEAK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"("prompt"\s*:\s*"(?:[^"\\]|\\.)*?)\s*,?\s*\b(refs?|denoise)\s*[:=]\s*(\[[^\]]*\]|[0-9]+(?:\.[0-9]+)?)\s*,\s*""#).unwrap()
    });
    let fixed = LEAK.replace(raw, r#"${1}","${2}":${3},""#).to_string();
    (fixed != raw).then(|| json_in(&fixed)).flatten()
}

/// 模型輸出 → 正規化的動作；不合格回傳要退回給它的英文說明
pub fn parse_action(raw: &str, c: &Check) -> Result<Map<String, Value>, String> {
    let Some(m) = json_in(raw).or_else(|| repair_check(raw)).or_else(|| repair_prompt_tail(raw)).or_else(|| repair_field_leak(raw)) else {
        return Err("Your reply was not a JSON object. Reply with exactly ONE JSON object, nothing else.".into());
    };
    let mut action = tx(m.get("action").unwrap_or(&Value::Null)).trim().to_lowercase();
    // 9B 會把工具名直接寫在 action（{"action":"upscale","id":5}），或只寫 tool
    if crate::agent_tools::find(&action).is_some() || (action.is_empty() && m.contains_key("tool")) {
        return parse_call(&m, if action.is_empty() { None } else { Some(action.as_str()) }, c);
    }
    if action.is_empty() {
        action = if m.contains_key("prompt") {
            "generate"
        } else if m.contains_key("best") {
            "finish"
        } else {
            "reply"
        }
        .into();
    }
    let check = str_list(m.get("check"));
    match action.as_str() {
        "generate" => parse_generate(&m, check, c),
        "call" | "tool" | "use" => parse_call(&m, None, c),
        "finish" => {
            let mut best: Vec<i64> = m.get("best").and_then(Value::as_array).into_iter().flatten().filter_map(int_of).collect();
            if best.is_empty() {
                if let Some(b) = m.get("best").and_then(int_of) {
                    best.push(b);
                }
            }
            best.retain(|b| c.images.contains(b));
            best.dedup();
            best.truncate(3);
            if best.is_empty() && !c.images.is_empty() {
                return Err(format!("\"best\" must list 1 to 3 ids of images you were shown in this request: {:?}.", c.images));
            }
            if c.saw_images && check.is_empty() {
                return Err("Fill \"check\" first: every element the user asked for, each as \"element: yes\" or \"element: no - what is wrong\", judged on what you actually see.".into());
            }
            let missing: Vec<&String> = check.iter().filter(|i| NO_ITEM.is_match(i)).collect();
            if !missing.is_empty() && c.rounds_left > 0 && !c.nudged {
                return Err(format!(
                    "You marked these as not done: {}. {} round(s) remain. Fix them with a generate action; only if they really cannot be fixed, finish again and say so in the message.",
                    missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; "),
                    c.rounds_left
                ));
            }
            let msg = tx(m.get("message").unwrap_or(&Value::Null)).trim().to_string();
            Ok(Map::from_iter([
                ("action".into(), json!("finish")),
                ("check".into(), json!(check)),
                ("best".into(), json!(best)),
                ("message".into(), json!(if msg.is_empty() { "完成了。".to_string() } else { msg })),
            ]))
        }
        "reply" => {
            // 看完圖停下來問「你喜歡哪張」：退回一次，請它自己交卷或再修；真的非問不可，第二次放行
            if c.saw_images && !c.nudged {
                return Err(format!(
                    "You were shown the images of this request. Do not stop to ask the user which one they like: finish with \"check\" and \"best\", or generate to fix what is wrong ({} round(s) remain). Reply only if you really cannot go on without the user's answer.",
                    c.rounds_left
                ));
            }
            let msg = tx(m.get("message").or(m.get("say")).unwrap_or(&Value::Null)).trim().to_string();
            if msg.is_empty() {
                return Err("\"message\" is empty. Write your reply to the user in Traditional Chinese.".into());
            }
            Ok(Map::from_iter([("action".into(), json!("reply")), ("message".into(), json!(msg))]))
        }
        other => Err(format!("Unknown action \"{other}\". Use generate, call, finish or reply.")),
    }
}

/// {"action":"call","tool":…,"args":{…},"say":…}；args 寫在最外層也收（9B 常這樣）。named＝工具名寫在 action 的情況
fn parse_call(m: &Map<String, Value>, named: Option<&str>, c: &Check) -> Result<Map<String, Value>, String> {
    if c.calls_left <= 0 {
        return Err(format!("You already used {} tool calls for this request. Finish (or reply) now.", crate::agent_tools::MAX_CALLS));
    }
    let tool = named.map(str::to_string).unwrap_or_else(|| tx(m.get("tool").or(m.get("name")).unwrap_or(&Value::Null)).trim().to_string());
    if tool.is_empty() {
        return Err("A call needs \"tool\": {\"action\":\"call\",\"tool\":\"name\",\"args\":{...}}.".into());
    }
    let args = match m.get("args").or(m.get("arguments")).or(m.get("params")) {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
        Some(v) => v.clone(),
        None => Value::Object(m.iter().filter(|(k, _)| !["action", "tool", "name", "say", "check"].contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()),
    };
    let (args, level, why) = crate::agent_tools::check_call(&tool, &args)?;
    let name = crate::agent_tools::find(&tool).map(|t| t.name).unwrap_or_default();
    // 「id」是圖庫編號、「ref」才是使用者附的圖。這個請求還沒畫過圖、id 又剛好是使用者圖片的號碼（1～4）：
    // 一定是把附圖寫成 id，照送會改到圖庫第 N 張（2026-10-29 真模型實測 9B 寫 {"id":1}）
    if let Some(n) = args.get("id").and_then(Value::as_i64).filter(|n| c.images.is_empty() && c.user_images.contains(n)) {
        return Err(format!("\"id\" is a gallery image id, not the user's picture. For user image {n} write \"ref\":{n} instead of \"id\"."));
    }
    let mut out = Map::from_iter([
        ("action".into(), json!("call")),
        ("tool".into(), json!(name)),
        ("args".into(), args),
        ("say".into(), json!(tx(m.get("say").unwrap_or(&Value::Null)).trim())),
    ]);
    if level == crate::agent_tools::Level::Confirm {
        out.insert("confirm".into(), json!(why));
    }
    Ok(out)
}

/// 提示詞裡混進的 refs／denoise 撈出來當欄位，連同單獨一項的「832x1216」從提示詞拿掉
fn lift_fields(prompt: &str) -> (String, Map<String, Value>) {
    let mut got = Map::new();
    for cap in FIELD_IN_PROMPT.captures_iter(prompt) {
        let key = if cap[1].eq_ignore_ascii_case("denoise") { "denoise" } else { "refs" };
        let v = &cap[2];
        let val = if v.starts_with('[') {
            json!(v.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|x| x.trim().parse::<i64>().ok()).collect::<Vec<_>>())
        } else if key == "refs" {
            json!([v.parse::<i64>().unwrap_or(0)])
        } else {
            json!(v.parse::<f64>().unwrap_or(0.0))
        };
        got.entry(key).or_insert(val);
    }
    if got.is_empty() && !prompt.split(',').any(|i| SIZE_ITEM.is_match(i.trim())) {
        return (prompt.to_string(), got);
    }
    let cleaned = FIELD_IN_PROMPT.replace_all(prompt, "");
    let items: Vec<&str> = cleaned.split(',').map(str::trim).filter(|i| !i.is_empty() && !SIZE_ITEM.is_match(i)).collect();
    (items.join(", "), got)
}

fn parse_generate(m: &Map<String, Value>, check: Vec<String>, c: &Check) -> Result<Map<String, Value>, String> {
    if c.rounds_left <= 0 {
        return Err("No rounds are left. Reply with the finish action now, choosing the best images you were shown.".into());
    }
    let (prompt, lifted) = lift_fields(tx(m.get("prompt").unwrap_or(&Value::Null)).trim());
    let mut merged = m.clone();
    for (k, v) in lifted {
        merged.entry(k).or_insert(v);
    }
    let m = &merged;
    if prompt.chars().count() < 15 {
        return Err("\"prompt\" is missing or too short. Write the complete prompt.".into());
    }
    // Qwen 用附圖改圖／合成：官方寫法是「使用者寫中文就用中文下指令」
    let qwen_refs = c.family == QWEN21 && c.refs_mode == "multi" && ["refs", "ref", "references", "reference_images", "user_images", "images"].iter().any(|k| m.get(*k).is_some_and(|v| ref_num(v).is_some() || v.as_array().is_some_and(|a| !a.is_empty())));
    if !qwen_refs && has_cjk_outside_quotes(&prompt) {
        return Err("The prompt must be written in English: translate and expand the request into English (only text that must appear inside the picture may stay in quotes). Reply again.".into());
    }
    // 9B 偶爾把「怎麼寫提示詞」的說明整句抄進提示詞
    let low = norm(&prompt);
    if let Some(p) = GUIDE_ECHO.iter().find(|p| low.contains(*p)) {
        return Err(format!("Your prompt copied the writing instructions (\"{p}\"). Follow them instead: describe the actual picture."));
    }
    if c.saw_images && check.is_empty() {
        return Err("Fill \"check\" first: every element the user asked for, each as \"element: yes\" or \"element: no - what is wrong\", judged on what you actually see. Then write the new prompt.".into());
    }
    // 還沒看過自己畫的圖就填 check（9B 會把使用者附的圖拿來打勾）：清空
    let check = if c.saw_images { check } else { vec![] };
    // 自己打勾全部 yes 還要重畫＝為了變化而重畫：退回一次請它交卷；它改口標出 no 或堅持第二次就放行
    if c.saw_images && !c.nudged && !check.iter().any(|i| NO_ITEM.is_match(i)) {
        return Err("Everything in your check is yes. Do not regenerate just for variety: finish with \"best\". If something is actually wrong, mark it \"element: no - what is wrong\" in check and generate again.".into());
    }
    let (dw, dh) = default_size(c.family);
    let w = m.get("width").and_then(int_of).unwrap_or(dw);
    let h = m.get("height").and_then(int_of).unwrap_or(dh);
    let (w, h) = snap_size(w, h);
    // LoRA：名字要對得到目錄；強度夾在範圍裡；跟使用者固定的加起來最多 MAX_LORAS 個
    let mut loras: Vec<Value> = vec![];
    if c.allow_loras {
        let mut unknown = vec![];
        for l in m.get("loras").and_then(Value::as_array).into_iter().flatten() {
            let (name, st) = match l {
                Value::String(s) => (s.clone(), None),
                Value::Object(o) => (tx(o.get("name").unwrap_or(&Value::Null)), o.get("strength").and_then(crate::py::float)),
                _ => continue,
            };
            if name.trim().is_empty() {
                continue;
            }
            match find_lora(c.catalog, &name) {
                Some(info) if c.fixed.iter().any(|f| f.name == info.name) => {}
                // 沒列給它看的（調年齡的）當成不存在
                Some(info) if !info.agent_may_pick() => unknown.push(name),
                Some(info) => {
                    if loras.iter().any(|x| x["name"] == json!(info.name)) {
                        continue;
                    }
                    let s = st.unwrap_or(info.strength).clamp(info.min.min(info.strength), info.max.max(info.strength));
                    loras.push(json!({"name": info.name, "strength": (s * 100.0).round() / 100.0}));
                }
                None => unknown.push(name),
            }
        }
        if !unknown.is_empty() {
            return Err(format!("Unknown LoRA name(s): {}. Use exact names from the LoRA list, or \"loras\":[].", unknown.join(", ")));
        }
        loras.truncate(MAX_LORAS.saturating_sub(c.fixed.len()));
    }
    // 使用者的圖：refs＝要餵給生圖模型的圖（使用者圖片編號）。9B 會寫成 "User image 1"、"<image1>"、{"n":1} 或換個鍵名
    let raw_refs = ["refs", "ref", "references", "reference_images", "user_images", "images"].iter().find_map(|k| m.get(*k));
    let mut refs: Vec<i64> = match raw_refs {
        Some(Value::Array(a)) => a.iter().filter_map(ref_num).collect(),
        Some(v) => ref_num(v).into_iter().collect(),
        None => vec![],
    };
    refs.dedup();
    if refs.is_empty() && raw_refs.is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)) && v.as_array().is_none_or(|a| !a.is_empty())) {
        return Err("\"refs\" must list user image numbers, like \"refs\":[1].".into());
    }
    let mut denoise = Value::Null;
    let example = |n: i64| if c.refs_mode == "one" { format!("\"refs\":[{n}],\"denoise\":0.85") } else { format!("\"refs\":[{n}]") };
    // 提示詞點名了 <image1> 卻沒有 refs：生圖模型根本拿不到那張圖（只會當成字）。
    // Qwen 多圖參考時意思很清楚（<imageK>＝這個請求附的第 K 張），直接補上；補不了才退回
    let max_tag = IMAGE_TAG.captures_iter(&prompt).filter_map(|t| t[1].parse::<usize>().ok()).max().unwrap_or(0);
    if refs.is_empty() && c.refs_mode == "multi" && max_tag >= 1 && max_tag <= c.request_user_images.len() {
        refs = c.request_user_images[..max_tag].to_vec();
    }
    if refs.is_empty() && IMAGE_TAG.is_match(&prompt) {
        return Err(if c.refs_mode.is_empty() || c.user_images.is_empty() {
            "Remove the <imageN> tags from the prompt: this image model cannot take images here, so describe what you need from them in words.".to_string()
        } else {
            format!("Your prompt names <image1>, but \"refs\" is empty, so the image model never gets that picture. Add the field, for example {}.", example(*c.request_user_images.first().or(c.user_images.last()).unwrap_or(&1)))
        });
    }
    if refs.is_empty() && c.refs_mode == "one" && !c.request_user_images.is_empty() && !c.nudged && POSE_CHANGE.is_match(c.ask) && !KEEP_POSE.is_match(c.ask) {
        return Err(format!(
            "The user attached image(s) {:?} to this request, but your JSON has no \"refs\". To change the pose, arms or hands of THEIR picture and keep everything else, \
edit it: {{\"action\":\"call\",\"tool\":\"edit\",\"args\":{{\"ref\":{},\"instruction\":\"the one change in plain words\"}}}}. If you only take ideas from it, send the same JSON again without refs.",
            c.request_user_images,
            c.request_user_images[0]
        ));
    }
    if refs.is_empty() && !c.refs_mode.is_empty() && !c.request_user_images.is_empty() && !c.nudged {
        return Err(format!(
            "The user attached image(s) {:?} to this request, but your JSON has no \"refs\". If they want that picture changed, redrawn or combined (same person, pose or layout), \
put the field in the JSON itself, for example {}. If you only take ideas from it, send the same JSON again without refs.",
            c.request_user_images,
            example(c.request_user_images[0])
        ));
    }
    // 上一輪還用著使用者的圖，這輪拿掉 refs＝改成只照文字畫一張新的（另一個人、另一種畫風），之後怎麼修都洗不回去。
    // 「附了圖沒用」的提醒只在第一輪；2026-10-29 使用者轉來回報：助手先畫一張結構差不多的 CG 人，再在它自己的圖上改姿勢
    let last_refs: Vec<i64> = c.last.and_then(|l| l["refs"].as_array()).map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
    // 新請求也算：「同一張，頭髮再短一點」不會再附圖，拿掉 refs 一樣會畫成別人（真的要新圖，照原樣再送一次就放行）
    if refs.is_empty() && !c.refs_mode.is_empty() && !last_refs.is_empty() && !c.nudged {
        return Err(format!(
            "Your previous round built on user image(s) {last_refs:?}, but this JSON has no \"refs\", so the image model would draw a new picture from text only (another face and style that later rounds cannot remove). \
Keep the user's picture as the base, for example {}. If this really should be a new picture from text, send the same JSON again.",
            example(last_refs[0])
        ));
    }
    // 嘴上說用了 refs（say／changes），JSON 卻沒有：9B 常這樣，提醒到它補上或改口
    let said = format!("{} {}", tx(m.get("say").unwrap_or(&Value::Null)), tx(m.get("changes").unwrap_or(&Value::Null))).to_lowercase();
    if refs.is_empty() && !c.refs_mode.is_empty() && !c.user_images.is_empty() && said.contains("refs") && !NO_REFS.is_match(&said) {
        return Err(format!(
            "You wrote that you use refs, but the JSON has no \"refs\" field. Add it as its own field, for example {}, or stop mentioning refs.",
            example(*c.request_user_images.first().or(c.user_images.last()).unwrap_or(&1))
        ));
    }
    if !refs.is_empty() {
        if c.refs_mode.is_empty() {
            return Err("This image model cannot take user images as input here. Remove \"refs\" and describe what you need from the images in the prompt.".into());
        }
        if let Some(bad) = refs.iter().find(|r| !c.user_images.contains(r)) {
            return Err(format!("\"refs\" has {bad}, but the user images are {:?}. Use those numbers, or leave refs out.", c.user_images));
        }
        if c.refs_mode == "one" {
            if refs.len() > 1 {
                return Err("This image model can redraw only ONE user image at a time: \"refs\":[N] with a single number.".into());
            }
            // 圖生圖只是在原圖上重畫，角度和姿勢都會留著：三視圖、背面、側面要用 character_sheet（2026-10-26 朋友回報「三視圖畫出原圖」）
            if NEW_VIEW.is_match(c.ask) {
                return Err(format!(
                    "Redrawing with refs keeps the original angle and pose, so it cannot make a turnaround or new views of the character. Use the tool instead: {{\"action\":\"call\",\"tool\":\"character_sheet\",\"args\":{{\"ref\":{},\"kind\":\"turnaround\"}}}}",
                    refs[0]
                ));
            }
            let d = m.get("denoise").and_then(crate::py::float).unwrap_or(0.7).clamp(0.2, 0.95);
            // 換姿勢、手臂：圖生圖低重繪留著舊姿勢、高重繪變成另一個人（結構差不多的別人），改用 Qwen 2.1 照指令改圖。
            // 不靠「提醒過一次」放行：那個旗標常被前一條提醒用掉（2026-10-29 真模型實測就是這樣漏的）；只修小地方（0.5 以下）照放
            if d > POSE_REDRAW_MAX && POSE_CHANGE.is_match(c.ask) && !KEEP_POSE.is_match(c.ask) {
                return Err(format!(
                    "Redrawing cannot change only the pose: low denoise keeps the old pose and high denoise turns the picture into another person. \
Edit the user's picture instead: {{\"action\":\"call\",\"tool\":\"edit\",\"args\":{{\"ref\":{},\"instruction\":\"the one change in plain words, keep the person, clothes, style and background unchanged\"}}}}. \
If the pose should stay as it is and only small details change, use a denoise of {POSE_REDRAW_MAX} or lower.",
                    refs[0]
                ));
            }
            denoise = json!((d * 100.0).round() / 100.0);
        } else {
            refs.truncate(MAX_USER_IMAGES);
            let tags: Vec<i64> = IMAGE_TAG.captures_iter(&prompt).filter_map(|c| c[1].parse().ok()).collect();
            if let Some(t) = tags.iter().find(|t| **t < 1 || **t as usize > refs.len()) {
                return Err(format!("The prompt names <image{t}>, but refs has only {} image(s). <imageK> means the K-th entry of refs.", refs.len()));
            }
            if refs.len() >= 2 && tags.is_empty() {
                return Err("With two or more refs, name each one in the prompt as <image1>, <image2>… (in the order of refs) and say what to take from it.".into());
            }
        }
    }
    // 只比這個請求裡的上一輪：新請求「再來一張」照抄上一份提示詞是對的（換種子）
    if let Some(last) = c.last.filter(|_| !c.images.is_empty()) {
        if norm(&tx(&last["prompt"])) == norm(&prompt) && last["loras"] == json!(loras) && last.get("refs").cloned().unwrap_or(json!([])) == json!(refs)
            && last.get("denoise").cloned().unwrap_or(Value::Null) == denoise
        {
            return Err("Your prompt and LoRAs are identical to the previous round, so the result would not improve. Change the words that fix the problems you found (or finish).".into());
        }
    }
    Ok(Map::from_iter([
        ("action".into(), json!("generate")),
        ("check".into(), json!(check)),
        ("prompt".into(), json!(prompt)),
        ("width".into(), json!(w)),
        ("height".into(), json!(h)),
        ("loras".into(), json!(loras)),
        ("changes".into(), json!(tx(m.get("changes").unwrap_or(&Value::Null)).trim())),
        ("say".into(), json!(tx(m.get("say").unwrap_or(&Value::Null)).trim())),
        ("refs".into(), json!(refs)),
        ("denoise".into(), denoise),
    ]))
}

/// 掛了 LoRA 卻沒寫觸發詞：補在提示詞最前面（Anima 補在品質／評級 tag 後面）。回傳 (新提示詞, 補了哪些)
pub fn add_triggers(prompt: &str, loras: &[&LoraInfo], family: &str) -> (String, Vec<String>) {
    let low = prompt.to_lowercase();
    let add: Vec<String> = loras
        .iter()
        .filter(|l| !l.triggers.is_empty() && !l.triggers.iter().any(|t| low.contains(&t.to_lowercase())))
        .flat_map(|l| l.triggers.iter().take(2).cloned())
        .collect();
    if add.is_empty() {
        return (prompt.to_string(), add);
    }
    let joined = add.join(", ");
    if family == ANIMA {
        // masterpiece, best quality, score_7, <rating>, 後面
        let parts: Vec<&str> = prompt.splitn(5, ',').collect();
        if parts.len() == 5 && ["safe", "sensitive", "nsfw", "explicit"].contains(&parts[3].trim()) {
            return (format!("{},{},{},{}, {joined},{}", parts[0], parts[1], parts[2], parts[3], parts[4]), add);
        }
    }
    (format!("{joined}, {prompt}"), add)
}

// ================================================================ 認識 LoRA（看檔頭、看備註，必要時試畫比較）
pub const SYSTEM_LORA_META: &str = r#"You catalogue LoRA add-ons of an image model so an assistant can later decide when to use each one. From the clues (file name, title, trigger words, the owner's note, Civitai tags, the most frequent tags of the training captions) tell what this LoRA most likely does.
Reply with ONE JSON object and nothing else:
{"kind":"style|character|clothing|concept|pose|detail|lighting|other","desc":"one English sentence: what it changes and when to use it","desc_zh":"一句繁體中文（台灣用語）","sure":true}
Write "sure":false when the clues are too thin to tell."#;

pub const SYSTEM_LORA_PROBE: &str = r#"You catalogue LoRA add-ons of an image model so an assistant can later decide when to use each one. You see two images made with the same seed: image A WITHOUT the LoRA and image B WITH it (plus its trigger words). Compare them and describe what the LoRA changes: art style, colors, line work, rendering, face, body, clothing, accessories, background, lighting, or a specific character. Use the clues (file name, trigger words) too.
Reply with ONE JSON object and nothing else:
{"kind":"style|character|clothing|concept|pose|detail|lighting|other","desc":"one English sentence: what it changes and when to use it","desc_zh":"一句繁體中文（台灣用語）","sure":true}
Write "sure":false when A and B look almost the same."#;

/// 試畫用的提示詞（中性：一個成年人站著、簡單背景，風格／角色／衣服類的 LoRA 都看得出差別）
pub fn probe_prompt(family: &str) -> &'static str {
    if family == ANIMA {
        "masterpiece, best quality, score_7, safe, 1girl, solo, mature female, brown hair, medium hair, brown eyes, white shirt, blue jeans, standing, full body, looking at viewer, smile, simple background, white background. A young woman stands and smiles at the viewer."
    } else {
        "A 25-year-old woman with shoulder-length brown hair stands in a softly lit room and smiles at the viewer, full body, wearing a plain white t-shirt and blue jeans, simple light gray background, soft natural daylight."
    }
}

/// safetensors 檔頭的 __metadata__（不讀權重）
pub fn header_meta(path: &Path) -> Map<String, Value> {
    use std::io::Read;
    let read = || -> std::io::Result<Map<String, Value>> {
        let mut f = std::fs::File::open(path)?;
        let mut len = [0u8; 8];
        f.read_exact(&mut len)?;
        let n = u64::from_le_bytes(len);
        if n > 200 << 20 {
            return Ok(Map::new());
        }
        let mut buf = vec![0u8; n as usize];
        f.read_exact(&mut buf)?;
        let v: Value = serde_json::from_slice(&buf).map_err(std::io::Error::other)?;
        Ok(v.get("__metadata__").and_then(Value::as_object).cloned().unwrap_or_default())
    };
    read().unwrap_or_default()
}

/// 檔頭裡有用的線索（文字）：名稱、標題、說明、觸發詞、訓練標註最常見的 tag
pub fn header_clues(meta: &Map<String, Value>) -> Vec<String> {
    let mut out = vec![];
    let g = |k: &str| meta.get(k).map(tx).map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "None" && s != "null");
    for (k, label) in [
        ("modelspec.title", "title"),
        ("ss_output_name", "output name"),
        ("name", "name"),
        ("modelspec.description", "description"),
        ("modelspec.trigger_phrase", "trigger phrase"),
        ("trigger_word", "trigger word"),
        ("trigger_words", "trigger words"),
        ("modelspec.tags", "tags"),
    ] {
        if let Some(v) = g(k) {
            out.push(format!("{label}: {}", short(&v, 300)));
        }
    }
    // ss_tag_frequency：{"資料夾": {"tag": 次數}}（本身是 JSON 字串）
    if let Some(freq) = g("ss_tag_frequency").and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        let mut tags: Vec<(String, i64)> = vec![];
        for (_, d) in freq.as_object().into_iter().flatten() {
            for (t, n) in d.as_object().into_iter().flatten() {
                let n = n.as_i64().unwrap_or(0);
                match tags.iter_mut().find(|x| x.0 == *t) {
                    Some(x) => x.1 += n,
                    None => tags.push((t.trim().to_string(), n)),
                }
            }
        }
        tags.sort_by_key(|t| std::cmp::Reverse(t.1));
        if !tags.is_empty() {
            out.push(format!("most frequent training tags: {}", tags.iter().take(30).map(|t| t.0.as_str()).collect::<Vec<_>>().join(", ")));
        }
    }
    out
}

/// 給「看線索寫說明」的使用者訊息
pub fn lora_clues_text(info: &LoraInfo, note: &str, civitai_tags: &[String], header: &[String]) -> String {
    let mut s = format!("file: {}\n", info.name);
    if !info.title.is_empty() {
        s += &format!("title: {}\n", info.title);
    }
    if !info.triggers.is_empty() {
        s += &format!("trigger words: {}\n", info.triggers.join(", "));
    }
    if !note.trim().is_empty() {
        s += &format!("owner's note: {}\n", short(note.trim(), 400));
    }
    if !civitai_tags.is_empty() {
        s += &format!("Civitai tags: {}\n", civitai_tags.join(", "));
    }
    for h in header {
        s += &format!("{h}\n");
    }
    s
}

/// 線索夠不夠多（不用試畫就能猜）：有備註、Civitai 標籤、訓練 tag 統計、檔頭說明其中一種
pub fn clues_enough(note: &str, civitai_tags: &[String], header: &[String]) -> bool {
    !note.trim().is_empty()
        || !civitai_tags.is_empty()
        || header.iter().any(|h| h.starts_with("most frequent training tags") || h.starts_with("description"))
}

/// 模型寫的 LoRA 說明 → 存進 lora_meta 的 agent 欄位
pub fn parse_lora_desc(raw: &str, src: &str) -> Option<Value> {
    let m = json_in(raw)?;
    let desc = tx(m.get("desc")?).trim().to_string();
    if desc.is_empty() {
        return None;
    }
    let kinds = ["style", "character", "clothing", "concept", "pose", "detail", "lighting", "other"];
    let kind = tx(m.get("kind").unwrap_or(&Value::Null)).trim().to_lowercase();
    Some(json!({
        "desc": desc, "desc_zh": tx(m.get("desc_zh").unwrap_or(&Value::Null)).trim(),
        "kind": if kinds.contains(&kind.as_str()) { kind } else { "other".into() },
        "sure": m.get("sure").map(|v| truthy(Some(v))).unwrap_or(true),
        "src": src, "t": now(),
    }))
}

// ================================================================ 存檔
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(data: &Path) -> Store {
        Store { dir: data.join("agent") }
    }
    fn file(&self, id: &str) -> PathBuf {
        self.dir.join("sessions").join(format!("{id}.json"))
    }
    /// 清單：新的在前
    pub fn sessions(&self) -> Vec<Value> {
        let mut out: Vec<Value> = std::fs::read_dir(self.dir.join("sessions"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
            .map(|s| {
                let ev = s["events"].as_array().cloned().unwrap_or_default();
                let images = ev.iter().filter(|e| ev_kind(e) == "result").map(|e| e["ids"].as_array().map(|a| a.len()).unwrap_or(0)).sum::<usize>();
                let cover = ev.iter().rev().find_map(|e| e["action"]["best"].as_array().and_then(|b| b.first().cloned()));
                json!({"id": s["id"], "title": s["title"], "updated": s["updated"], "images": images, "cover": cover, "brain": s["settings"]["brain"]})
            })
            .collect();
        out.sort_by(|a, b| b["updated"].as_f64().unwrap_or(0.0).total_cmp(&a["updated"].as_f64().unwrap_or(0.0)));
        out
    }
    pub fn session(&self, id: &str) -> Option<Value> {
        if !valid_id(id) {
            return None;
        }
        serde_json::from_slice(&std::fs::read(self.file(id)).ok()?).ok()
    }
    pub fn save(&self, s: &mut Value) -> std::io::Result<()> {
        let id = tx(&s["id"]);
        if !valid_id(&id) {
            return Err(std::io::Error::other("對話編號不對"));
        }
        s["updated"] = json!(now());
        write_atomic(&self.file(&id), s)
    }
    /// 不真的刪：搬到 待刪除\助手\
    pub fn trash(&self, id: &str, trash: &Path) -> std::io::Result<()> {
        if !valid_id(id) {
            return Err(std::io::Error::other("對話編號不對"));
        }
        let to = trash.join("助手");
        std::fs::create_dir_all(&to)?;
        std::fs::rename(self.file(id), to.join(format!("{id}_{}.json", chrono::Local::now().format("%Y%m%d_%H%M%S"))))
    }
}

/// 對話設定的預設值與檢查：brain 9b／35b／cloud，rounds 1～5，count 1～4，auto（關＝畫一輪就交卷），think，loras（讓它自己挑）
/// 思考檔位：先做了五段（off/low/mid/high/max），使用者看過實測後改成三段
/// （2026-10-27：「off(關閉思考)/Low(現在的一般)/high(現在的多想)」）。不限預算的那段拿掉：9B 會想到沒字可答
pub const THINK_LEVELS: [&str; 3] = ["off", "low", "high"];

/// 設定裡的思考檔位；舊存檔是 true／false（true 以前是不限，改當 high），五段時期的 mid→low、max→high
pub fn think_level(v: &Value) -> &'static str {
    match v {
        Value::String(s) => match s.as_str() {
            "mid" => "low",
            "max" => "high",
            s => THINK_LEVELS.iter().find(|l| **l == s).copied().unwrap_or("off"),
        },
        Value::Bool(true) => "high",
        _ => "off",
    }
}

/// 檔位 → 思考預算（tokens）：None＝不想（交給 pe::with_think_budget）
pub fn think_budget(level: &str) -> Option<i64> {
    match level {
        "low" => Some(1024),
        "high" => Some(3072),
        _ => None,
    }
}

/// 這個檔位一次最多寫幾個 token（思考＋回答）
pub fn think_max_tokens(level: &str) -> i64 {
    think_budget(level).map_or(2048, |n| n + 2048)
}

pub fn clean_settings(v: &Value) -> Value {
    let brain = v["brain"].as_str().filter(|b| matches!(*b, "9b" | "35b" | "cloud")).unwrap_or("9b");
    let int = |k: &str, d: i64, lo: i64, hi: i64| v.get(k).and_then(crate::py::int).unwrap_or(d).clamp(lo, hi);
    let flag = |k: &str, d: bool| v.get(k).map(|x| truthy(Some(x))).unwrap_or(d);
    let think = match v.get("think") {
        Some(t) => think_level(t),
        None if brain == "cloud" => "high",
        None => "off",
    };
    // mode：tools＝動手做（畫圖、用工具）、prompt＝只寫詞（不碰繪圖模型）；target：auto 或寫法名稱
    let mode = if v["mode"] == "prompt" { "prompt" } else { "tools" };
    let target = v["target"].as_str().filter(|t| GUIDES.iter().any(|g| g.0 == *t)).unwrap_or("auto");
    json!({
        "brain": brain, "rounds": int("rounds", 3, 1, 5), "count": int("count", 2, 1, 4),
        "auto": flag("auto", true), "think": think, "loras": flag("loras", true), "show": flag("show", true),
        "mode": mode, "target": target,
        "base": if v["base"].is_object() { v["base"].clone() } else { Value::Null },
    })
}

pub fn blank_session(settings: &Value) -> Value {
    json!({"id": crate::director::new_id(), "title": "新對話", "created": now(), "updated": now(),
           "settings": clean_settings(settings), "events": []})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pose_fix_uses_the_latest_request_and_explains_itself() {
        let poses = crate::poses::Poses::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("resources"));
        let pz = poses.get("paizuri").unwrap();
        // Anima：補 tag、拿掉打架的，說明列出改了什麼（2026-10-28 AI 改寫實測 9B 寫成 nipple clamp、missionary）
        let (s, msg) = fix_by_pose(pz, true, "explicit, 1girl, 1boy, hetero, missionary, nipple clamp, She squeezes him.", "幫我畫乳交");
        assert!(s.contains("paizuri") && !s.contains("missionary"), "{s}");
        let msg = msg.unwrap();
        assert!(msg.starts_with("照體位庫「") && msg.contains("補上") && msg.contains("拿掉 missionary"), "{msg}");
        // 已經寫對：不動、不囉嗦
        assert_eq!(fix_by_pose(pz, true, "explicit, 1girl, 1boy, hetero, paizuri, penis", "乳交"), ("explicit, 1girl, 1boy, hetero, paizuri, penis".to_string(), None));
        // 句子型：沒提到體位才接擺法
        assert!(fix_by_pose(pz, false, "A woman on a bed.", "乳交").1.unwrap().contains("接上體位庫"));
        // 助手：看最後一個（不是途中補充的）使用者訊息的體位
        let ev = |pose: &str, mid: bool| json!({"kind": "user", "text": "x", "mid": mid, "pose": {"id": pose}});
        let events = vec![ev("doggy_style", false), ev("paizuri", false), ev("doggy_style", true)];
        let (s, _) = pose_fix(&poses, &events, ANIMA, "explicit, 1girl, 1boy, hetero, doggystyle");
        assert!(s.contains("paizuri"), "{s}");
    }

    fn lora(name: &str, trig: &[&str]) -> LoraInfo {
        LoraInfo {
            name: name.into(),
            title: String::new(),
            triggers: trig.iter().map(|s| s.to_string()).collect(),
            desc: "glow style".into(),
            strength: 1.0,
            min: 0.4,
            max: 1.6,
            fav: false,
        }
    }

    fn check<'a>(cat: &'a [LoraInfo], last: Option<&'a Value>, imgs: &'a [i64], saw: bool, left: i64) -> Check<'a> {
        Check { family: QWEN21, rounds_left: left, last, saw_images: saw, images: imgs, catalog: cat, fixed: &[], allow_loras: true, nudged: false, user_images: &[1, 2], request_user_images: &[], refs_mode: "multi", ask: "", calls_left: 12 }
    }

    #[test]
    fn sizes_snap() {
        assert_eq!(snap_size(832, 1248), (832, 1248));
        assert_eq!(snap_size(1000, 1000), (992, 992));
        assert_eq!(snap_size(100, 5000), (512, 1664));
        let (w, h) = snap_size(1664, 1664);
        assert!(w * h <= 1024 * 1536 + 1 && w % 32 == 0 && h % 32 == 0);
    }

    #[test]
    fn cjk_only_outside_quotes() {
        assert!(has_cjk_outside_quotes("一個女孩站在雪地"));
        assert!(!has_cjk_outside_quotes("A shop sign that reads \"咖啡\" above the door"));
        assert!(!has_cjk_outside_quotes("A banner with 「新年快樂」 on it"));
        assert!(!has_cjk_outside_quotes("A girl, — long hair"));
    }

    #[test]
    fn generate_rules() {
        let cat = vec![lora("qwen21\\snowglow_qwen21_2000.safetensors", &["snowglow"])];
        let imgs = [5, 6];
        // 中文提示詞退回
        let r = parse_action(r#"{"action":"generate","prompt":"一個銀髮女大學生站在下雪的校園裡回頭看","width":832,"height":1248}"#, &check(&cat, None, &[], false, 3));
        assert!(r.unwrap_err().contains("English"));
        // 把寫法說明抄進提示詞（2026-10-13 9B 實測抄過）退回
        let echo = r#"{"action":"generate","prompt":"A 25-year-old woman in a cafe by the window, warm lighting, cinematic feel, subject and look first, then pose and expression, outfit, setting"}"#;
        assert!(parse_action(echo, &check(&cat, None, &[], false, 3)).unwrap_err().contains("copied the writing"));
        // 看過圖沒打勾退回
        let p = r#"{"action":"generate","check":[],"prompt":"A 20-year-old woman with silver hair in a snowy campus at night","loras":[{"name":"snowglow_qwen21_2000","strength":3}]}"#;
        assert!(parse_action(p, &check(&cat, None, &imgs, true, 2)).unwrap_err().contains("check"));
        // 全部 yes 還重畫：退回一次；有 no 或第二次放行
        let ok_all = r#"{"action":"generate","check":["silver hair: yes","snow: yes"],"prompt":"A 20-year-old woman with long silver hair in a snowy campus at night"}"#;
        assert!(parse_action(ok_all, &check(&cat, None, &imgs, true, 2)).unwrap_err().starts_with("Everything in your check"));
        let one_no = ok_all.replace("snow: yes", "snow: no - it is raining");
        assert!(parse_action(&one_no, &check(&cat, None, &imgs, true, 2)).is_ok());
        let mut c = check(&cat, None, &imgs, true, 2);
        c.nudged = true;
        assert!(parse_action(ok_all, &c).is_ok());
        // LoRA 名稱模糊對到、強度夾住、尺寸補預設
        let a = parse_action(p, &check(&cat, None, &[], false, 3)).unwrap();
        assert_eq!(a["loras"], json!([{"name": "qwen21\\snowglow_qwen21_2000.safetensors", "strength": 1.6}]));
        assert_eq!((a["width"].clone(), a["height"].clone()), (json!(832), json!(1248)));
        // 提示詞沒改退回（同一個請求裡的上一輪才比；新請求「再來一張」照抄是對的）
        let last = Value::Object(a.clone());
        let r = parse_action(p, &check(&cat, Some(&last), &[5], false, 2));
        assert!(r.unwrap_err().contains("identical"));
        assert!(parse_action(p, &check(&cat, Some(&last), &[], false, 2)).is_ok());
        // 不認得的 LoRA 退回
        let r = parse_action(r#"{"action":"generate","prompt":"A woman standing in the rain at night","loras":[{"name":"nope"}]}"#, &check(&cat, None, &[], false, 3));
        assert!(r.unwrap_err().contains("Unknown LoRA"));
        // 沒有輪數了不能再畫
        let r = parse_action(r#"{"action":"generate","prompt":"A woman standing in the rain at night"}"#, &check(&cat, None, &[], false, 0));
        assert!(r.unwrap_err().contains("finish"));
    }

    #[test]
    fn finish_rules() {
        let imgs = [231, 232];
        // 只能挑看過的圖
        let r = parse_action(r#"{"action":"finish","check":["silver hair: yes"],"best":[999],"message":"好了"}"#, &check(&[], None, &imgs, true, 1));
        assert!(r.is_err());
        // 打了 no、還有輪數：提醒一次
        let raw = r#"```json
{"action":"finish","check":["three books: no - there are four","left hand milk tea: yes"],"best":["231"],"message":"選 231"}
```"#;
        assert!(parse_action(raw, &check(&[], None, &imgs, true, 1)).unwrap_err().contains("three books"));
        let mut c = check(&[], None, &imgs, true, 1);
        c.nudged = true;
        let a = parse_action(raw, &c).unwrap();
        assert_eq!(a["best"], json!([231]));
        // 沒有輪數了：打了 no 也放行
        assert!(parse_action(raw, &check(&[], None, &imgs, true, 0)).is_ok());
        // reply
        let a = parse_action(r#"{"action":"reply","message":"要直的還是橫的？"}"#, &check(&[], None, &[], false, 3)).unwrap();
        assert_eq!(a["action"], json!("reply"));
        assert!(parse_action("我覺得很好", &check(&[], None, &[], false, 3)).is_err());
        // 9B 把 check 寫成物件（或包成字串的物件）：攤平後一樣抓得到 no
        let obj = r#"{"action":"finish","check":[{"element":"three books","check":"yes"},"{\"element\":\"milk tea in left hand\",\"check\":\"no - right hand\"}",{"item":"rain","ok":true}],"best":[231]}"#;
        let e = parse_action(obj, &check(&[], None, &imgs, true, 1)).unwrap_err();
        assert!(e.contains("milk tea in left hand: no - right hand") && !e.contains("three books"), "{e}");
        let mut c = check(&[], None, &imgs, true, 1);
        c.nudged = true;
        assert_eq!(parse_action(obj, &c).unwrap()["check"], json!(["three books: yes", "milk tea in left hand: no - right hand", "rain: yes"]));
        // 看完圖就問「你喜歡哪張」：退回一次，第二次放行
        let ask = r#"{"action":"reply","message":"你比較喜歡哪一張？"}"#;
        assert!(parse_action(ask, &check(&[], None, &imgs, true, 2)).unwrap_err().starts_with("You were shown"));
        let mut c = check(&[], None, &imgs, true, 2);
        c.nudged = true;
        assert!(parse_action(ask, &c).is_ok());
    }

    #[test]
    fn triggers_added() {
        let l = lora("x.safetensors", &["jgdxstyle"]);
        let (p, add) = add_triggers("A knight in spiked armor", &[&l], QWEN21);
        assert_eq!(p, "jgdxstyle, A knight in spiked armor");
        assert_eq!(add, vec!["jgdxstyle"]);
        let (p, add) = add_triggers("JGDXstyle, a knight", &[&l], QWEN21);
        assert!(add.is_empty() && p == "JGDXstyle, a knight");
        let (p, _) = add_triggers("masterpiece, best quality, score_7, safe, 1girl, solo", &[&l], ANIMA);
        assert_eq!(p, "masterpiece, best quality, score_7, safe, jgdxstyle, 1girl, solo");
    }

    #[test]
    fn rounds_and_messages() {
        let ev = vec![
            json!({"kind": "user", "text": "畫雪夜"}),
            json!({"kind": "act", "action": {"action": "generate", "prompt": "p1", "loras": []}, "round": 1}),
            json!({"kind": "result", "round": 1, "ids": [1, 2]}),
            json!({"kind": "user", "text": "頭髮長一點", "mid": true}),
            json!({"kind": "act", "action": {"action": "generate", "prompt": "p2", "loras": []}, "round": 2}),
            json!({"kind": "result", "round": 2, "ids": [3, 4]}),
        ];
        assert_eq!(rounds_used(&ev), 2);
        assert_eq!(rounds_left(&ev, 3, true), 1);
        assert_eq!(rounds_left(&ev, 2, true), 0);
        assert_eq!(rounds_left(&ev, 3, false), 0);
        let mut ev3 = ev.clone();
        ev3.push(json!({"kind": "user", "text": "再亮一點", "mid": true}));
        assert_eq!(rounds_left(&ev3, 2, true), 1);
        assert_eq!(request_images(&ev), vec![1, 2, 3, 4]);
        assert_eq!(last_generate(&ev).unwrap()["prompt"], json!("p2"));
        let img = json!({"type": "image_url", "image_url": {"url": "data:x"}});
        let m = build_messages("SYS", &ev, &[(3, img.clone()), (4, img)], 1, 2);
        let a = m.as_array().unwrap();
        let roles: Vec<&str> = a.iter().map(|x| x["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["system", "user", "assistant", "user", "assistant", "user"]);
        // 舊的一輪只有文字、插話跟結果放在同一則；最新一輪附圖
        assert!(a[3]["content"].as_str().unwrap().contains("no longer shown"));
        assert!(a[3]["content"].as_str().unwrap().contains("頭髮長一點"));
        let last = a[5]["content"].as_array().unwrap();
        assert_eq!(last.iter().filter(|p| p["type"] == "image_url").count(), 2);
        assert!(last.last().unwrap()["text"].as_str().unwrap().contains("Rounds left for this request: 1"));
        // 新的請求：輪數重算
        let mut ev2 = ev.clone();
        ev2.push(json!({"kind": "act", "action": {"action": "finish", "best": [3]}}));
        ev2.push(json!({"kind": "user", "text": "再來一張側臉"}));
        assert_eq!(rounds_used(&ev2), 0);
        assert!(request_images(&ev2).is_empty());
        // look 拿來看的圖不算這個請求畫的
        let mut ev4 = ev2.clone();
        ev4.push(json!({"kind": "result", "round": 1, "ids": [9], "tool": "look"}));
        assert!(request_images(&ev4).is_empty());
        assert!(latest_result(&ev2).is_none());
    }

    #[test]
    fn pose_term_reaches_model() {
        let ev = vec![json!({"kind": "user", "text": "幫我畫個乳交圖。", "pose": {"said": "乳交", "zh": "乳交", "en": "paizuri / titjob", "geometry": "Her breasts are pressed together around his penis."}})];
        let m = build_messages("SYS", &ev, &[], 2, 2);
        let u = m[1]["content"].as_str().unwrap();
        assert!(u.contains("「乳交」") && u.contains("paizuri / titjob") && u.contains("around his penis"), "{u}");
    }

    #[test]
    fn long_chats_are_summarised() {
        let mut ev = vec![json!({"kind": "user", "text": "第一個要求"})];
        for i in 0..14 {
            ev.push(json!({"kind": "act", "action": {"action": "generate", "prompt": format!("prompt number {i}"), "loras": []}, "round": i + 1}));
            ev.push(json!({"kind": "result", "round": i + 1, "ids": [i]}));
        }
        let m = build_messages("SYS", &ev, &[], 1, 2);
        let a = m.as_array().unwrap();
        assert_eq!(a.iter().filter(|x| x["role"] == "assistant").count(), MAX_ACTS);
        let first = a[1]["content"].as_str().unwrap();
        assert!(first.contains("第一個要求") && first.contains("prompt number 0"));
    }

    #[test]
    fn lora_catalog_and_meta() {
        let m = json!({"triggers": ["snowglow"], "strength": 1.1, "agent": {"desc": "Soft snowy glow style."}, "fav": true});
        let l = LoraInfo::from_meta("qwen21\\snowglow.safetensors", &m);
        assert_eq!(l.stem(), "snowglow");
        assert!(l.fav && (l.min - 0.5).abs() < 1e-9 && (l.max - 1.7).abs() < 1e-9);
        let one = std::slice::from_ref(&l);
        let t = loras_text(one, &[], true);
        assert!(t.contains("qwen21\\snowglow.safetensors | Soft snowy glow style. | trigger words: snowglow | strength 1.1 (0.5-1.7)"));
        assert!(loras_text(one, &[], false).contains("cannot add"));
        assert!(loras_text(one, one, true).contains("Always on"));
        let mut meta = Map::new();
        meta.insert("ss_tag_frequency".into(), json!("{\"10_x\": {\"1girl\": 30, \"silver hair\": 12}, \"5_y\": {\"silver hair\": 20}}"));
        meta.insert("ss_output_name".into(), json!("snowglow"));
        let c = header_clues(&meta);
        assert!(c.contains(&"output name: snowglow".to_string()));
        assert!(c.contains(&"most frequent training tags: silver hair, 1girl".to_string()));
        assert!(clues_enough("", &[], &c));
        assert!(!clues_enough("", &[], &["output name: x".into()]));
        let d = parse_lora_desc(r#"{"kind":"Style","desc":"Adds a glowing snow look.","desc_zh":"雪光","sure":false}"#, "probe").unwrap();
        assert_eq!(d["kind"], json!("style"));
        assert_eq!(d["sure"], json!(false));
        assert!(parse_lora_desc("沒有", "meta").is_none());
        // 調年齡的 LoRA：不列給助手、它寫了也當不存在；檔名裡的 image、stage 不算
        let age = LoraInfo::from_meta("qwen21\\PornMaster_QI2.1_Age_Slider_V1.safetensors", &json!({}));
        assert!(!age.agent_may_pick());
        assert!(LoraInfo::from_meta("qwen21\\fwaun-style_qwen_image_2_1_v50.safetensors", &json!({})).agent_may_pick());
        assert!(LoraInfo::from_meta("x\\backstage_light.safetensors", &json!({})).agent_may_pick());
        assert!(!LoraInfo::from_meta("x\\style.safetensors", &json!({"note": "讓角色看起來年紀小一點"})).agent_may_pick());
        let t = loras_text(&[age.clone(), LoraInfo::from_meta("qwen21\\glow.safetensors", &json!({}))], &[], true);
        assert!(!t.contains("Age_Slider") && t.contains("glow"));
        let p = r#"{"action":"generate","prompt":"A 25-year-old woman reading in a cafe by the window","loras":[{"name":"PornMaster_QI2.1_Age_Slider_V1","strength":-2}]}"#;
        let cat = [age];
        let c = Check { family: QWEN21, rounds_left: 3, last: None, saw_images: false, images: &[], catalog: &cat, fixed: &[], allow_loras: true, nudged: false, user_images: &[], request_user_images: &[], refs_mode: "", ask: "", calls_left: 12 };
        assert!(parse_action(p, &c).unwrap_err().contains("Unknown LoRA"));
    }

    #[test]
    fn h3_writer_texts_editable() {
        // H3 影片台「AI 幫我寫」的兩份說明：編進來了、設定頁列得到
        assert!(default_text("agent_h3_base").is_some_and(|t| t.contains("MiniMax H3")));
        assert!(default_text("agent_h3_ref").is_some_and(|t| t.contains("Ref2VA")));
        let rules = crate::pe::Service::prompt_rules();
        assert!(default_text("agent_h3_board").is_some_and(|t| t.contains("JSON")));
        for id in ["agent_h3_base", "agent_h3_ref", "agent_h3_board", "agent_h3_sheet"] {
            assert!(rules.iter().any(|(i, _)| *i == id), "{id}");
        }
    }

    #[test]
    fn prompt_texts_from_files() {
        // resources\agent 的四份都編進來了、工作說明留著三個佔位符、填完不剩大括號
        for (id, _) in EDITABLE_TEXTS {
            assert!(default_text(id).is_some_and(|t| t.len() > 40), "{id}");
        }
        let sys = default_text("agent").unwrap();
        assert!(sys.starts_with("You are a commissioned artist") && !sys.ends_with('\n'));
        for k in ["{model_guide}", "{sizes}", "{loras}", "{tools}"] {
            assert!(sys.contains(k), "{k}");
        }
        let full = system_prompt(sys, default_text(guide_id(ANIMA)).unwrap(), ANIMA, "(none)");
        assert!(full.contains("masterpiece, best quality") && full.contains("832x1216") && !full.contains("{model_guide}"));
        assert!(full.contains("- character_sheet ") && !full.contains("{tools}") && full.matches("Besides drawing").count() == 1, "工具段落填在 {{tools}}，不重複接");
        assert_eq!(guide_id(QWEN21), "agent_qwen21");
        assert_eq!(guide_id(MOSS), "agent_moss");
        assert!(default_text("agent_moss").unwrap().contains("MOSS"));
        assert_eq!(default_size(MOSS), (832, 1216));
        assert!(system_prompt("{sizes}", "", MOSS, "").contains("832x1216") && system_prompt("{sizes}", "", QWEN21, "").contains("832x1248"));
        // 使用者改過的說明照用
        assert!(system_prompt("Be brief. {model_guide}", "GUIDE", QWEN21, "").starts_with("Be brief. GUIDE"));
    }

    #[test]
    fn settings_clamped() {
        let s = clean_settings(&json!({"brain": "cloud", "rounds": 9, "count": 0}));
        assert_eq!(s["rounds"], json!(5));
        assert_eq!(s["count"], json!(1));
        assert_eq!(s["think"], json!("high"));
        assert_eq!(s["show"], json!(true));
        assert_eq!(clean_settings(&json!({"brain": "x"}))["brain"], json!("9b"));
        let b = blank_session(&json!({}));
        assert!(valid_id(&tx(&b["id"])));
    }

    #[test]
    fn think_levels_map_to_budgets() {
        let lv = |t: Value| clean_settings(&json!({"think": t}))["think"].clone();
        assert_eq!((lv(json!(true)), lv(json!(false)), lv(json!("hyper"))), (json!("high"), json!("off"), json!("off")));
        // 五段時期存下來的
        assert_eq!((lv(json!("low")), lv(json!("mid")), lv(json!("high")), lv(json!("max"))), (json!("low"), json!("low"), json!("high"), json!("high")));
        assert_eq!(clean_settings(&json!({}))["think"], json!("off"));
        assert_eq!(clean_settings(&json!({"brain": "cloud"}))["think"], json!("high"));
        assert_eq!((think_budget("off"), think_budget("low"), think_budget("high")), (None, Some(1024), Some(3072)));
        assert_eq!((think_max_tokens("off"), think_max_tokens("low"), think_max_tokens("high")), (2048, 3072, 5120));
    }

    #[test]
    fn parse_action_accepts_call() {
        let cat: Vec<LoraInfo> = vec![];
        let mut c = check(&cat, None, &[], false, 3);
        let a = parse_action(r#"{"action":"call","tool":"upscale","args":{"id":5},"say":"放大"}"#, &c).unwrap();
        assert_eq!((a["tool"].clone(), a["args"]["id"].clone(), a["args"]["scale"].clone()), (json!("upscale"), json!(5), json!(2.0)));
        // 工具名寫在 action、參數寫在外層
        let a = parse_action(r#"{"action":"remove_bg","id":7}"#, &c).unwrap();
        assert_eq!((a["action"].clone(), a["tool"].clone(), a["args"]["id"].clone()), (json!("call"), json!("remove_bg"), json!(7)));
        let a = parse_action(r#"{"action":"call","tool":"trash","args":{"ids":[1]}}"#, &c).unwrap();
        assert!(a["confirm"].as_str().unwrap().contains("回收區"));
        assert!(parse_action(r#"{"action":"call","tool":"api","args":{"method":"POST","path":"/api/trash/empty"}}"#, &c).unwrap_err().starts_with("Not allowed"));
        c.calls_left = 0;
        assert!(parse_action(r#"{"action":"call","tool":"gallery","args":{}}"#, &c).unwrap_err().contains("tool calls"));
    }

    #[test]
    fn parse_action_takes_back_sentences_written_outside_the_prompt() {
        let cat: Vec<LoraInfo> = vec![];
        let c = Check {
            family: ANIMA, rounds_left: 3, last: None, saw_images: false, images: &[], catalog: &cat, fixed: &[],
            allow_loras: true, nudged: false, user_images: &[], request_user_images: &[], refs_mode: "", ask: "", calls_left: 12,
        };
        // 2026-10-28 擂台 35B「交配按壓」原樣（節錄）
        let raw = "{\n  \"action\": \"generate\",\n  \"check\": [],\n  \"prompt\": \"masterpiece, best quality, score_7, nsfw, 1girl, 1boy, hetero, mating press position, on back, from side\",\n  \"She lies on her back on the bed with her knees folded tightly against her chest. He kneels above her.\",\n  \"width\": 832,\n  \"height\": 1248,\n  \"loras\": [],\n  \"changes\": \"\",\n  \"say\": \"生成交配按壓姿勢\"\n}";
        let a = parse_action(raw, &c).unwrap();
        let p = a["prompt"].as_str().unwrap();
        assert!(p.starts_with("masterpiece") && p.ends_with("He kneels above her."), "{p}");
        assert_eq!(a["width"], json!(832));
        // 正常的 JSON 不受影響
        let ok = r#"{"action":"generate","prompt":"masterpiece, 1girl. She smiles.","say":"好"}"#;
        assert_eq!(parse_action(ok, &c).unwrap()["prompt"], json!("masterpiece, 1girl. She smiles."));
    }

    #[test]
    fn img2img_cannot_turn_the_character() {
        let cat: Vec<LoraInfo> = vec![];
        let ck = |ask: &'static str| Check {
            family: ANIMA, rounds_left: 3, last: None, saw_images: false, images: &[], catalog: &cat, fixed: &[],
            allow_loras: true, nudged: false, user_images: &[1], request_user_images: &[1], refs_mode: "one", ask, calls_left: 12,
        };
        let g = r#"{"action":"generate","prompt":"masterpiece, best quality, 1girl, turnaround, front view, side view, back view","refs":[1],"denoise":0.8}"#;
        let e = parse_action(g, &ck("幫我畫這個角色的三視圖")).unwrap_err();
        assert!(e.contains("character_sheet") && e.contains("\"ref\":1"), "{e}");
        assert!(parse_action(g, &ck("頭髮改成紅色")).is_ok());
    }

    #[test]
    fn keeps_building_on_the_users_picture() {
        // 2026-10-29 回報：要「原圖不變只改手臂」，助手先畫一張新的 CG 人再改
        let cat: Vec<LoraInfo> = vec![];
        let ck = |ask: &'static str, last: Option<&'static Value>, images: &'static [i64], nudged: bool| Check {
            family: ANIMA, rounds_left: 3, last, saw_images: false, images, catalog: &cat, fixed: &[],
            allow_loras: true, nudged, user_images: &[1], request_user_images: &[], refs_mode: "one", ask, calls_left: 12,
        };
        // 換姿勢：圖生圖做不到，導去 edit ref；說了姿勢不變、或提醒過一次就放行
        let g = r#"{"action":"generate","prompt":"masterpiece, best quality, 1girl, raising her right arm","refs":[1],"denoise":0.9}"#;
        let e = parse_action(g, &ck("保持原圖不變，把她的手臂換個姿勢", None, &[], false)).unwrap_err();
        assert!(e.starts_with("Redrawing cannot change only the pose") && e.contains("\"tool\":\"edit\"") && e.contains("\"ref\":1"), "{e}");
        assert!(parse_action(g, &ck("姿勢不變，頭髮改成紅色", None, &[], false)).is_ok());
        // 提醒過別的事（nudged）也照擋；只修小地方（重繪 0.5 以下）放行
        assert!(parse_action(g, &ck("把手臂換個姿勢", None, &[], true)).is_err());
        let small = r#"{"action":"generate","prompt":"masterpiece, best quality, 1girl, no tattoo on arm","refs":[1],"denoise":0.4}"#;
        assert!(parse_action(small, &ck("把手臂上的刺青去掉", None, &[], false)).is_ok());
        // 改圖工具把使用者的圖寫成 id：退回叫它用 ref；這個請求畫過圖之後的 id 照收
        let edit_id = r#"{"action":"call","tool":"edit","args":{"id":1,"instruction":"右手比 V"}}"#;
        let e = parse_action(edit_id, &ck("把手臂換個姿勢", None, &[], false)).unwrap_err();
        assert!(e.contains("\"ref\":1"), "{e}");
        assert!(parse_action(edit_id, &ck("把手臂換個姿勢", None, &[7], false)).is_ok());
        assert!(parse_action(r#"{"action":"call","tool":"edit","args":{"ref":1,"instruction":"右手比 V"}}"#, &ck("把手臂換個姿勢", None, &[], false)).is_ok());
        // 中文說「不帶 refs」不算說要用
        let no = r#"{"action":"generate","prompt":"masterpiece, best quality, 1girl, red hair","say":"用文字重繪，不帶 refs。"}"#;
        assert!(parse_action(no, &ck("頭髮改成紅色", None, &[], false)).is_ok());
        assert!(parse_action(&no.replace("不帶 refs", "用 refs"), &ck("頭髮改成紅色", None, &[], false)).unwrap_err().starts_with("You wrote that you use refs"));
        // refs 寫進提示詞字串又沒收好：拆回欄位
        let leak = r#"{"action":"generate","check":[],"prompt":"masterpiece, best quality, 1girl, red hair, from front, refs:[1],"width":832,"height":1216,"loras":[],"denoise":0.85,"say":"改紅髮"}"#;
        let a = parse_action(leak, &ck("頭髮改成紅色", None, &[], false)).unwrap();
        assert_eq!((a["refs"].clone(), a["denoise"].clone()), (json!([1]), json!(0.85)));
        assert!(!tx(&a["prompt"]).contains("refs"), "{a:?}");
        // 上一輪用著使用者的圖，這輪拿掉 refs：提醒一次
        static LAST: LazyLock<Value> = LazyLock::new(|| json!({"action": "generate", "prompt": "1girl", "refs": [1], "denoise": 0.5}));
        let plain = r#"{"action":"generate","prompt":"masterpiece, best quality, 1girl, red hair","refs":[]}"#;
        let e = parse_action(plain, &ck("頭髮改成紅色", Some(&LAST), &[7], false)).unwrap_err();
        assert!(e.starts_with("Your previous round built on user image(s) [1]") && e.contains("\"refs\":[1]"), "{e}");
        assert!(parse_action(plain, &ck("頭髮改成紅色", Some(&LAST), &[7], true)).is_ok());
        // 上一輪是別的請求（「同一張，頭髮再短一點」不會再附圖）也提醒一次；真的要新圖就照原樣再送
        assert!(parse_action(plain, &ck("同一張，頭髮再短一點", Some(&LAST), &[], false)).is_err());
        assert!(parse_action(plain, &ck("畫一隻貓", Some(&LAST), &[], true)).is_ok());
        // 說明裡有「從使用者的圖開始」和換姿勢改用 edit
        let t = images_text("one");
        assert!(t.contains("every try starts from the user's picture") && t.contains("call the edit tool with \"ref\":N"), "{t}");
        assert!(images_text("multi").contains("every try starts from the user's picture"));
        assert!(!images_text("").contains("every try"));
    }

    #[test]
    fn tool_events_reach_the_model() {
        let ev = vec![
            json!({"kind": "user", "text": "放大最後一張"}),
            json!({"kind": "act", "action": {"action": "call", "tool": "gallery", "args": {}}}),
            json!({"kind": "tool", "tool": "gallery", "ok": true, "text": "[{\"id\":9}]"}),
            json!({"kind": "act", "action": {"action": "call", "tool": "look", "args": {"ids": [9]}}}),
            json!({"kind": "tool", "tool": "look", "ok": true, "text": "shown below"}),
            json!({"kind": "result", "round": 1, "ids": [9], "tool": "look"}),
        ];
        assert_eq!(calls_used(&ev), 2);
        assert_eq!(rounds_used(&ev), 0);
        let look = vec![(9, json!({"type": "image_url", "image_url": {"url": "data:x"}}))];
        let m = build_messages_with("SYS", &ev, &look, &[], 3, 2);
        let all = serde_json::to_string(&m).unwrap();
        assert!(all.contains("Tool gallery result: [{\\\"id\\\":9}]"), "{all}");
        assert!(all.contains("The images you asked to look at") && all.contains("carry on with the user's request"));
        assert_eq!(request_text(&ev), "放大最後一張");
    }

    #[test]
    fn user_images_refs() {
        let cat: Vec<LoraInfo> = vec![];
        let ck = |mode: &'static str| Check {
            family: QWEN21, rounds_left: 3, last: None, saw_images: false, images: &[], catalog: &cat, fixed: &[],
            allow_loras: true, nudged: false, user_images: &[1, 2], request_user_images: &[], refs_mode: mode, ask: "", calls_left: 12,
        };
        let gen = |extra: &str| format!(r#"{{"action":"generate","prompt":"The woman from <image1> wearing the dress from <image2> on a beach","width":832,"height":1248{extra}}}"#);
        // Qwen：兩張合成，提示詞用 <image1>／<image2>
        let a = parse_action(&gen(r#","refs":[1,2]"#), &ck("multi")).unwrap();
        assert_eq!(a["refs"], json!([1, 2]));
        assert_eq!(a["denoise"], Value::Null);
        // 編號不存在、標記超過 refs 數量、兩張卻沒寫標記
        assert!(parse_action(&gen(r#","refs":[3]"#), &ck("multi")).unwrap_err().contains("user images are [1, 2]"));
        assert!(parse_action(&gen(r#","refs":[1]"#), &ck("multi")).unwrap_err().contains("<image2>"));
        let plain = r#"{"action":"generate","prompt":"A woman in a red dress standing on a beach at sunset","refs":[1,2]}"#;
        assert!(parse_action(plain, &ck("multi")).unwrap_err().contains("name each one"));
        // Anima：只能一張，denoise 夾在範圍裡；雲端不能用
        let one = r#"{"action":"generate","prompt":"A woman in a red dress standing on a beach at sunset","refs":2,"denoise":1.4}"#;
        let a = parse_action(one, &ck("one")).unwrap();
        assert_eq!((a["refs"].clone(), a["denoise"].clone()), (json!([2]), json!(0.95)));
        assert!(parse_action(plain, &ck("one")).unwrap_err().contains("ONE user image"));
        assert!(parse_action(plain, &ck("")).unwrap_err().contains("cannot take user images"));
        // 沒有 refs＝照舊
        let no_refs = r#"{"action":"generate","prompt":"A woman in a red dress standing on a beach at sunset"}"#;
        assert_eq!(parse_action(no_refs, &ck("")).unwrap()["refs"], json!([]));

        // 對話裡的圖：編號接著數、只有 uimgs 裡的才真的給它看
        let ev = vec![
            json!({"kind": "user", "text": "照這兩張", "images": [{"n": 1, "name": "a.png"}, {"n": 2, "name": "b.png"}]}),
            json!({"kind": "user", "text": "", "mid": true, "images": [{"n": 3, "name": "c.png"}]}),
        ];
        assert_eq!(user_images(&ev).len(), 3);
        let img = json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,AA"}});
        let m = build_messages_with("SYS", &ev, &[], &[(2, img.clone()), (3, img)], 1, 2);
        let s = m.to_string();
        assert!(s.contains("(User image 1 was attached here") && s.contains("User image 2:") && s.contains("User image 3:"));
        assert_eq!(s.matches("data:image").count(), 2);
        // 工作說明：有 {images} 填進去，沒有就接在最後
        assert!(with_images("A {images} B", "one").contains("\"denoise\""));
        // 這個請求附了圖卻沒用 refs：提醒一次，第二次（nudged）放行
        let plain_one = r#"{"action":"generate","prompt":"A woman with long blonde hair on a beach at sunset"}"#;
        // 提示詞有 <image1> 卻沒 refs：這個請求有附圖就照 <imageK> 補上，沒有才退回（nudged 也一樣）
        let mut c4 = ck("multi");
        c4.nudged = true;
        assert!(parse_action(r#"{"action":"generate","prompt":"the woman from <image1> wearing a red apron"}"#, &c4).unwrap_err().starts_with("Your prompt names <image1>"));
        c4.request_user_images = &[1, 2];
        let a = parse_action(r#"{"action":"generate","prompt":"the woman from <image1> next to the knight from <image2> in the rain"}"#, &c4).unwrap();
        assert_eq!(a["refs"], json!([1, 2]));
        // check 寫成不合法的 JSON 也救得回來
        let bad = r#"{"action":"generate","check":["20":null,"long hair":null],"prompt":"masterpiece, best quality, 1girl, long hair, rain"}"#;
        assert_eq!(parse_action(bad, &ck("")).unwrap()["prompt"], json!("masterpiece, best quality, 1girl, long hair, rain"));
        let bad2 = r#"{"action":"finish","check":[{"element":"1girl": "yes"}, {"element":"cloudy eyes": "no - still brown"}],"best":[5]}"#;
        assert_eq!(repair_check(bad2).unwrap()["check"], json!(["1girl: yes", "cloudy eyes: no - still brown"]));
        assert!(parse_action(r#"{"action":"generate","prompt":"the woman from <image1> wearing a red apron"}"#, &ck("")).unwrap_err().starts_with("Remove the <imageN>"));
        // 說用了 refs 卻沒寫：nudged 也要退回
        let mut c3 = ck("one");
        c3.nudged = true;
        assert!(parse_action(r#"{"action":"generate","prompt":"1girl, long golden hair, sunset beach","say":"用 refs 重畫"}"#, &c3).unwrap_err().starts_with("You wrote that you use refs"));
        assert!(parse_action(r#"{"action":"generate","prompt":"1girl, long golden hair, sunset beach","say":"這次不用 refs，without refs"}"#, &c3).is_ok());
        // refs／denoise 寫進提示詞裡：撈出來，提示詞拿掉這些（連同 832x1216）
        let a = parse_action(r#"{"action":"generate","prompt":"1girl, golden long hair, sunset beach, refs:[1], denoise:0.75, 832x1216, portrait"}"#, &ck("one")).unwrap();
        assert_eq!((a["refs"].clone(), a["denoise"].clone()), (json!([1]), json!(0.75)));
        assert_eq!(a["prompt"], json!("1girl, golden long hair, sunset beach, portrait"));
        assert_eq!(lift_fields("A tall woman, height 170cm").0, "A tall woman, height 170cm");
        assert_eq!(lift_fields("1girl, 1216, 832, loras:[], masterpiece, 20").0, "1girl, masterpiece, 20");
        // refs 寫成字串、物件、別的鍵名也收
        for (raw, want) in [
            (r#""refs":["User image 2"]"#, vec![2]),
            (r#""refs":["<image1>"]"#, vec![1]),
            (r#""ref":1"#, vec![1]),
            (r#""images":[{"n":2}]"#, vec![2]),
        ] {
            let a = parse_action(&format!(r#"{{"action":"generate","prompt":"Redraw her with long blonde hair on a beach at sunset",{raw}}}"#), &ck("one")).unwrap();
            assert_eq!(a["refs"], json!(want), "{raw}");
        }
        assert!(parse_action(r#"{"action":"generate","prompt":"Redraw her with long blonde hair on a beach at sunset","refs":["the barista"]}"#, &ck("one")).is_err());
        let mut c1 = ck("one");
        c1.request_user_images = &[1];
        assert!(parse_action(plain_one, &c1).unwrap_err().starts_with("The user attached image(s) [1]"));
        c1.nudged = true;
        assert!(parse_action(plain_one, &c1).is_ok());
        assert_eq!(request_user_images(&ev), vec![1, 2, 3]);
        // 第一輪寫了 check 也清空
        let a = parse_action(r#"{"action":"generate","check":["hair: yes"],"prompt":"A woman with long blonde hair on a beach at sunset"}"#, &ck("")).unwrap();
        assert_eq!(a["check"], json!([]));
        // 只調高重繪強度也算有改
        let prev = parse_action(r#"{"action":"generate","prompt":"1girl, long golden hair, sunset beach","refs":[1],"denoise":0.75}"#, &ck("one")).unwrap();
        let prev_v = Value::Object(prev);
        let mut c2 = ck("one");
        c2.last = Some(&prev_v);
        c2.images = &[5];
        assert!(parse_action(r#"{"action":"generate","prompt":"1girl, long golden hair, sunset beach","refs":[1],"denoise":0.85}"#, &c2).is_ok());
        assert!(parse_action(r#"{"action":"generate","prompt":"1girl, long golden hair, sunset beach","refs":[1],"denoise":0.75}"#, &c2).is_err());
        assert!(with_images("A", "").ends_with("cannot feed them to the image model: describe in the prompt everything you need from them."));
    }

    #[test]
    fn guides_are_picked_by_model_images_and_video() {
        assert_eq!(pick_guide(ANIMA, 0, "畫一個女孩"), "anima");
        assert_eq!(pick_guide(QWEN21, 0, "畫一個女孩"), "qwen");
        assert_eq!(pick_guide(QWEN21, 1, "把背景換成海邊"), "qwen_edit");
        assert_eq!(pick_guide(QWEN21, 3, "三個人合照"), "qwen_multiref");
        assert_eq!(pick_guide(ANIMA, 1, "照這張重畫"), "anima", "Anima 圖生圖照一般寫法");
        assert_eq!(pick_guide(MOSS, 0, "寫一段影片：她轉頭微笑"), "h3_video", "LTX 退場：影片一律照 H3 寫");
        assert_eq!(pick_guide(MOSS, 0, "幫我寫 H3 的提示詞"), "h3_video");
        assert_eq!(pick_guide("other", 0, "1girl"), "other");
        // 每一份寫法都有預設文字
        for g in GUIDES {
            assert!(default_text(g.1).is_some_and(|t| t.len() > 200), "{} 沒有寫法", g.0);
            assert_eq!(guide_text_id(g.0), Some(g.1));
        }
        assert_eq!(guide_text_id("Qwen-Edit"), Some("agent_qwen_edit"));
        assert_eq!(guide_text_id("nope"), None);
    }

    #[test]
    fn edits_and_videos_wait_for_their_guide() {
        let call = |tool: &str| json!({"action": "call", "tool": tool, "args": {}}).as_object().cloned().unwrap();
        assert_eq!(needed_guide(&call("edit"), ANIMA, "one"), Some("qwen_edit"));
        assert_eq!(needed_guide(&call("video"), ANIMA, "one"), Some("h3_video"));
        assert_eq!(guide_text_id("ltx_video"), None);
        assert_eq!(needed_guide(&call("upscale"), ANIMA, "one"), None);
        let gen = |refs: Value| json!({"action": "generate", "prompt": "x", "refs": refs}).as_object().cloned().unwrap();
        assert_eq!(needed_guide(&gen(json!([])), QWEN21, "multi"), None);
        assert_eq!(needed_guide(&gen(json!([1])), QWEN21, "multi"), Some("qwen_edit"));
        assert_eq!(needed_guide(&gen(json!([1, 2])), QWEN21, "multi"), Some("qwen_multiref"));
        assert_eq!(needed_guide(&gen(json!([1])), ANIMA, "one"), None);
        let ev = vec![json!({"kind": "tool", "tool": "guide", "ok": true, "args": {"name": "qwen_edit"}})];
        assert!(guide_read(&ev, "qwen_edit"));
        assert!(!guide_read(&ev, "h3_video"));
        let failed = vec![json!({"kind": "tool", "tool": "guide", "ok": false, "args": {"name": "qwen_edit"}})];
        assert!(!guide_read(&failed, "qwen_edit"));
    }

    #[test]
    fn qwen_refs_may_be_written_in_chinese() {
        let c = check(&[], None, &[], false, 3);
        assert!(parse_action(r#"{"action":"generate","prompt":"把她的白T恤換成黑色皮夾克，其他部分保持不變","refs":[1]}"#, &c).is_ok());
        assert!(parse_action(r#"{"action":"generate","prompt":"一個女孩坐在海邊的木棧道上看夕陽"}"#, &c).unwrap_err().contains("English"));
    }

    #[test]
    fn prompt_mode_reads_cards() {
        let a = parse_prompt_action(r#"{"action":"prompt","prompt":"The image is a vertical anime-style illustration of a girl at a desk.","negative":"","width":800,"height":1250,"note":"沒指定背景，我放在傍晚的教室"}"#, QWEN21, "qwen").unwrap();
        assert_eq!((a["action"].clone(), a["guide"].clone(), a["width"].clone(), a["height"].clone()), (json!("prompt"), json!("qwen"), json!(800), json!(1248)));
        assert_eq!(a["note"], "沒指定背景，我放在傍晚的教室");
        // 改圖：中文指令可以、沒有尺寸
        let e = parse_prompt_action(r#"{"prompt":"把背景換成夜晚的海邊，人物的臉、髮型、衣服和姿勢保持不變。"}"#, QWEN21, "qwen_edit").unwrap();
        assert!(e.get("width").is_none());
        // 文生圖寫中文、抄寫法、太短：退回
        assert!(parse_prompt_action(r#"{"action":"prompt","prompt":"一個女孩坐在書桌前，桌上疊著三本書"}"#, QWEN21, "qwen").unwrap_err().contains("English"));
        assert!(parse_prompt_action(r#"{"action":"prompt","prompt":"Work in this order: the opening sentence first, then the rest."}"#, QWEN21, "qwen").unwrap_err().contains("copied"));
        assert!(parse_prompt_action(r#"{"action":"prompt","prompt":"a cat"}"#, QWEN21, "qwen").is_err());
        // Anima 修 tag
        let an = parse_prompt_action(r#"{"action":"prompt","prompt":"masterpiece, best quality, score_7, 1girl, solo, long hair, holding cup, She smiles."}"#, ANIMA, "anima").unwrap();
        assert!(tx(&an["prompt"]).starts_with("masterpiece, best quality, score_7"), "{}", an["prompt"]);
        // 只回話
        let r = parse_prompt_action(r#"{"action":"reply","message":"你想要直的還是橫的？"}"#, QWEN21, "qwen").unwrap();
        assert_eq!(r["action"], "reply");
        assert!(parse_prompt_action(r#"{"action":"generate_image"}"#, QWEN21, "qwen").is_err());
        let sys = prompt_system(default_text("agent_prompt").unwrap(), "GUIDE TEXT", "qwen_edit", QWEN21);
        assert!(sys.contains("GUIDE TEXT") && sys.contains("editing one picture") && sys.contains("832x1248"));
        assert!(!sys.contains("{guide}") && !sys.contains("{target}") && !sys.contains("{sizes}"));
    }

    #[test]
    fn settings_keep_mode_and_target() {
        let s = clean_settings(&json!({"mode": "prompt", "target": "qwen_edit"}));
        assert_eq!((s["mode"].clone(), s["target"].clone()), (json!("prompt"), json!("qwen_edit")));
        let s = clean_settings(&json!({"mode": "weird", "target": "../x"}));
        assert_eq!((s["mode"].clone(), s["target"].clone()), (json!("tools"), json!("auto")));
    }

    #[test]
    fn explicit_prompt_target_uses_its_own_family() {
        assert_eq!(prompt_family("qwen", ANIMA), QWEN21);
        assert_eq!(prompt_family("qwen_edit", ANIMA), QWEN21);
        assert_eq!(prompt_family("anima", QWEN21), ANIMA);
        assert_eq!(prompt_family("auto", MOSS), MOSS);
        let family = prompt_family("qwen", ANIMA);
        let system = prompt_system(default_text("agent_prompt").unwrap(), "GUIDE", "qwen", family);
        assert!(system.contains("832x1248"));
        let a = parse_prompt_action(r#"{"prompt":"An orange cat sleeping on a wooden chair."}"#, family, "qwen").unwrap();
        assert_eq!((a["width"].as_i64(), a["height"].as_i64()), (Some(832), Some(1248)));
    }
}

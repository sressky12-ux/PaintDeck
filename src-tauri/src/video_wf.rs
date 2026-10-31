//! 影片模型（2026-10-20）：內建的 MiniMax H3（2026-10-30 起；h3.rs，之前是 LTX-2.3），或使用者匯入的 ComfyUI 影片工作流。
//! 選哪個存在 settings.json 的 video_engine（內建＝BUILTIN，其他＝工作流的代號）。
//!
//! 自訂工作流是在 ComfyUI 用「匯出（API）」存的 JSON，放在 data\video_workflows\<代號>.json。
//! 生圖站只改下面這些地方，其他照原樣送給 ComfyUI：
//! - 提示詞：文字欄位裡的 {{prompt}} 換成提示詞。整個工作流都沒寫的話，從取樣器的 positive 往回找，
//!   只找到一個文字框就整格換掉；找到好幾個就要使用者自己標
//! - 首格圖：標題含 {{image}} 的 Load Image；沒標的話用唯一的那個（沒有 Load Image＝只能文生影片）
//! - 種子：所有 seed／noise_seed 數字欄位
//! - 長寬：同時有 width、height、length 的節點（空白影片潛空間、WanImageToVideo 這類）照首格圖的比例調，像素數維持原本的
//! - 長度：讀得到每秒幾格（Create Video 的 fps 這類）才調：上面那些節點的 length 和 LTX 聲音的 frames_number 一起改
//! - 存檔：Save Video／Save WEBM／Video Combine 的檔名前綴改成 AnimaWeb/video；另外存第一格（縮圖封面）、
//!   最後一格（導演台接下一段用），跟內建流程一樣放 AnimaWeb/_poster
//!
//! 對應設定（2026-10-23，存檔裡的 "map"；設定頁「對應與參數…」改）：自動判斷不對、或判斷不出來的，使用者自己指定——
//! - prompt：放提示詞的 [節點, 欄位] 們（文字含 {{prompt}} 就只換那幾個字，不然整格換）；沒寫＝自動
//! - image：首格圖的 Load Image 節點；""＝沒有（只能文生影片）；沒寫＝自動
//! - values：{"節點.欄位": 值} 固定改掉工作流裡的值（跟生圖工作流的參數面板一樣，image_wf::apply_overrides）
//! - pins：[{key: "節點.欄位", label}] 釘到「動起來」視窗，每次生成可以調（送 params.wf_overrides）
//!
//! 有好幾個 Load Image、好幾個提示詞框的工作流（群組裡流通的 H3 大流程）也能先匯入，再到這裡指定
use crate::graphs::{self, GraphError};
use crate::py::{now, truthy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// 內建影片的代號（settings.json 的 video_engine 預設值）：MiniMax H3（h3.rs、server\api_h3.rs）。
/// 2026-10-30 LTX-2.3 退場（使用者：「把那個ltx2.3 狠狠的遺棄了…我們恭迎minimax h3」）
pub const BUILTIN: &str = "h3";
pub const BUILTIN_NAME: &str = "MiniMax H3（內建）";
/// 同一個值的舊名字，其他檔照用
pub const H3: &str = BUILTIN;
/// 以前內建 LTX 的代號：舊設定、導演台專案、拍攝紀錄裡還會出現，讀到一律當 H3
pub const LEGACY_LTX: &str = "ltx23";

/// 舊代號換成現在的（ltx23 → h3），其他原樣
pub fn normalize_engine(id: &str) -> &str {
    if id == LEGACY_LTX { BUILTIN } else { id }
}

/// 不用匯入就有的影片模型（H3；舊的 ltx23 也算，會被當成 H3）
pub fn is_builtin(id: &str) -> bool {
    normalize_engine(id) == BUILTIN
}
const PROMPT: &str = "{{prompt}}";
const IMAGE: &str = "{{image}}";
/// 存影片的節點：(class_type, 畫面從哪個輸入來, 每秒幾格的欄位)
const OUTPUTS: &[(&str, &str, &str)] = &[
    ("SaveVideo", "video", ""),
    ("VHS_VideoCombine", "images", "frame_rate"),
    ("SaveWEBM", "images", "fps"),
    ("SaveAnimatedWEBP", "images", "fps"),
    ("SaveAnimatedPNG", "images", "fps"),
];
/// 加進工作流的節點編號前綴（封面、最後一格）
const ADDED: &str = "anima_web_";

type R<T> = Result<T, String>;

/// 使用者的對應設定（存檔的 "map"，見檔頭）
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Mapping {
    pub prompt: Option<Vec<(String, String)>>,
    pub image: Option<String>,
    pub values: Map<String, Value>,
    pub pins: Vec<(String, String)>,
    pub media_slots: Vec<MediaSlot>,
    media_slots_error: Option<String>,
}

/// A named, literal ComfyUI file input. The role describes intent, not a new graph connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaSlot {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub role: String,
    pub node: String,
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
}

const MAX_MEDIA_SLOTS: usize = 24;
const MEDIA_ROLES: &[&str] = &[
    "first",
    "last",
    "character",
    "scene",
    "motion",
    "voice",
    "reference",
];

pub fn media_extension(kind: &str, ext: &str) -> bool {
    match kind {
        "image" => matches!(
            ext,
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "avif"
        ),
        "video" => matches!(ext, "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi"),
        "audio" => matches!(ext, "wav" | "mp3" | "flac" | "ogg" | "opus" | "m4a" | "aac"),
        _ => false,
    }
}

/// Only already uploaded ComfyUI input basenames cross the generation boundary.
pub fn media_basename(name: &str, kind: &str) -> R<()> {
    if name.is_empty()
        || name.len() > 240
        || name.trim() != name
        || name.starts_with('.')
        || name.contains("..")
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return Err("素材要是已上傳的檔名，不能填路徑或網址".into());
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !media_extension(kind, &ext) {
        return Err(format!("素材檔案類型不符：{name}（需要 {kind}）"));
    }
    Ok(())
}

fn media_input_path(input_dir: &Path, name: &str, kind: &str) -> R<PathBuf> {
    media_basename(name, kind)?;
    let root = input_dir
        .canonicalize()
        .map_err(|_| "找不到 ComfyUI input 資料夾".to_string())?;
    let path = input_dir
        .join(name)
        .canonicalize()
        .map_err(|_| format!("找不到已上傳素材：{name}，請重新選檔上傳"))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(format!("素材不在 ComfyUI input 資料夾：{name}"));
    }
    Ok(path)
}

/// Recognize only literal file inputs; a link, model filename or prompt is never a media slot.
fn media_input_kind(n: &Value, key: &str, value: &Value) -> Option<&'static str> {
    let value = value.as_str()?;
    let key = key.to_ascii_lowercase();
    let cls = class(Some(n)).to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "image"
            | "mask"
            | "image_path"
            | "image_file"
            | "first_frame"
            | "last_frame"
            | "reference_image"
    ) || key.starts_with("image_")
    {
        return Some("image");
    }
    if matches!(
        key.as_str(),
        "video" | "video_path" | "video_file" | "video_filename"
    ) {
        return Some("video");
    }
    if matches!(
        key.as_str(),
        "audio" | "audio_path" | "audio_file" | "audio_filename"
    ) {
        return Some("audio");
    }
    if matches!(key.as_str(), "file" | "filename" | "path" | "file_path") && cls.contains("load") {
        if cls.contains("video") {
            return Some("video");
        }
        if cls.contains("audio") {
            return Some("audio");
        }
        if cls.contains("image") {
            return Some("image");
        }
    }
    let ext = Path::new(value).extension()?.to_str()?.to_ascii_lowercase();
    ["image", "video", "audio"]
        .into_iter()
        .find(|kind| media_extension(kind, &ext))
}

/// 釘到「動起來」的欄位最多幾個、名字最長幾個字
const MAX_PINS: usize = 30;
const MAX_LABEL: usize = 30;

impl Mapping {
    pub fn from_value(v: &Value) -> Mapping {
        let pair = |x: &Value| {
            Some((
                x.get(0)?.as_str()?.to_string(),
                x.get(1)?.as_str()?.to_string(),
            ))
        };
        let media = match v.get("media_slots") {
            None => Ok(vec![]),
            Some(value) => serde_json::from_value::<Vec<MediaSlot>>(value.clone())
                .map_err(|_| "素材槽格式不對：要填名稱、類型、用途、節點與欄位".to_string()),
        };
        Mapping {
            prompt: v["prompt"]
                .as_array()
                .map(|a| a.iter().filter_map(pair).collect()),
            image: v["image"].as_str().map(str::to_string),
            values: v["values"].as_object().cloned().unwrap_or_default(),
            pins: v["pins"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some((
                        p["key"].as_str()?.to_string(),
                        p["label"].as_str().unwrap_or("").trim().to_string(),
                    ))
                })
                .collect(),
            media_slots: media.as_ref().cloned().unwrap_or_default(),
            media_slots_error: media.err(),
        }
    }

    pub fn to_value(&self) -> Value {
        let mut v = json!({"values": self.values, "pins": self.pins.iter().map(|(k, l)| json!({"key": k, "label": l})).collect::<Vec<_>>()});
        if let Some(p) = &self.prompt {
            v["prompt"] = json!(p.iter().map(|(n, k)| json!([n, k])).collect::<Vec<_>>());
        }
        if let Some(i) = &self.image {
            v["image"] = json!(i);
        }
        if !self.media_slots.is_empty() {
            v["media_slots"] = json!(self.media_slots);
        }
        v
    }

    /// 指到的欄位都要存在、不是連線（改值要型別對得上）
    fn check(&self, m: &Map<String, Value>) -> R<()> {
        if let Some(error) = &self.media_slots_error {
            return Err(error.clone());
        }
        let literal = |key: &str| -> R<(String, String)> {
            let (id, k) = key
                .split_once('.')
                .ok_or_else(|| format!("欄位寫法不對：{key}（要「節點編號.欄位」）"))?;
            match input(m, id, k) {
                None => Err(format!("工作流裡沒有 {key} 這個欄位（工作流換過了？）")),
                Some(v) if is_link(v) || v.is_array() || v.is_object() => Err(format!(
                    "{key} 是節點連線，不能改：選上游放實際值的那個欄位"
                )),
                Some(_) => Ok((id.to_string(), k.to_string())),
            }
        };
        for (id, k) in self.prompt.iter().flatten() {
            if !input(m, id, k).is_some_and(Value::is_string) {
                return Err(format!("{id}.{k} 不是文字欄位，不能放提示詞"));
            }
        }
        if let Some(id) = self.image.as_deref().filter(|i| !i.is_empty()) {
            if !input(m, id, "image").is_some_and(Value::is_string) {
                return Err(format!("#{id} 不是 Load Image，不能放首格圖"));
            }
        }
        for key in self.values.keys() {
            literal(key)?;
        }
        if self.pins.len() > MAX_PINS {
            return Err(format!("釘到「動起來」的欄位最多 {MAX_PINS} 個"));
        }
        for (key, l) in &self.pins {
            literal(key)?;
            if l.chars().count() > MAX_LABEL {
                return Err(format!("欄位名稱最多 {MAX_LABEL} 個字：{l}"));
            }
        }
        if self.media_slots.len() > MAX_MEDIA_SLOTS {
            return Err(format!("素材槽最多 {MAX_MEDIA_SLOTS} 個"));
        }
        if self.media_slots.iter().filter(|s| s.role == "first").count() > 1 {
            return Err("一個工作流只能指定一個首幀素材槽".into());
        }
        let mut keys = std::collections::HashSet::new();
        let mut targets = std::collections::HashSet::new();
        for slot in &self.media_slots {
            if slot.key.is_empty()
                || slot.key.len() > 64
                || !slot
                    .key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                || !keys.insert(slot.key.clone())
            {
                return Err(format!(
                    "素材槽代號不能重複，只能用英數、-、_：{}",
                    slot.key
                ));
            }
            if slot.label.trim().is_empty()
                || slot.label.chars().count() > 60
                || slot
                    .who
                    .as_ref()
                    .is_some_and(|s| s.chars().count() > 100 || s.chars().any(char::is_control))
                || !MEDIA_ROLES.contains(&slot.role.as_str())
                || (slot.role == "first" && slot.kind != "image")
            {
                return Err(format!("素材槽名稱、用途或角色不正確：{}", slot.key));
            }
            let key = format!("{}.{}", slot.node, slot.input);
            literal(&key)?;
            if !targets.insert(key.clone()) {
                return Err(format!("同一個素材欄位只能對應一次：{key}"));
            }
            if self.values.contains_key(&key)
                || self.pins.iter().any(|(k, _)| k == &key)
                || self
                    .prompt
                    .iter()
                    .flatten()
                    .any(|(n, k)| *n == slot.node && *k == slot.input)
            {
                return Err(format!("{key} 已作為素材槽，不能同時對應提示詞或一般參數"));
            }
            if media_input_kind(
                &m[&slot.node],
                &slot.input,
                input(m, &slot.node, &slot.input).unwrap(),
            ) != Some(slot.kind.as_str())
            {
                return Err(format!("{key} 不是 {} 的檔名欄位", slot.kind));
            }
        }
        Ok(())
    }
}

/// 生圖站在這個工作流裡會改的地方
#[derive(Debug, Default, PartialEq)]
pub struct Analysis {
    /// 放提示詞的 (節點, 欄位)
    pub prompt: Vec<(String, String)>,
    /// true＝只換掉 {{prompt}} 那幾個字；false＝整格換掉
    pub placeholder: bool,
    /// 首格圖的 Load Image；None＝只能文生影片
    pub image: Option<String>,
    pub seeds: Vec<(String, String)>,
    /// 有 width、height、length 的節點
    pub sizes: Vec<String>,
    /// 有 frames_number 的節點（LTX 的聲音潛空間）
    pub frames_number: Vec<String>,
    pub fps: Option<f64>,
    /// 存影片的節點，第一個拿來做封面
    pub outputs: Vec<String>,
    /// MiniMax H3 的工作流（有 MiniMaxH3*／EmptyMiniMaxH3* 節點）：長度要 17k+5 格、最長 15 秒
    pub h3: bool,
}

pub(crate) fn nodes(graph: &Value) -> R<&Map<String, Value>> {
    let Some(m) = graph.as_object() else {
        return Err("看不懂這個檔：要是 ComfyUI「匯出（API）」存的工作流 JSON".into());
    };
    if m.contains_key("nodes") && m.contains_key("links") {
        return Err("這是 ComfyUI 一般存檔的格式：請在 ComfyUI 用「匯出（API）」（Export (API)）另存一份再匯入".into());
    }
    if m.is_empty()
        || !m
            .values()
            .all(|n| n["class_type"].is_string() && n["inputs"].is_object())
    {
        return Err("看不懂這個檔：要是 ComfyUI「匯出（API）」存的工作流 JSON".into());
    }
    Ok(m)
}

/// ["節點", 輸出序號]
pub(crate) fn is_link(v: &Value) -> bool {
    matches!(v.as_array().map(Vec::as_slice), Some([Value::String(_), n]) if n.is_number())
}

pub(crate) fn class(n: Option<&Value>) -> &str {
    n.and_then(|n| n["class_type"].as_str()).unwrap_or("")
}

pub(crate) fn title(n: Option<&Value>) -> &str {
    n.and_then(|n| n["_meta"]["title"].as_str()).unwrap_or("")
}

/// 給人看的節點名稱：標題，沒有就用 class_type
pub(crate) fn label(n: Option<&Value>) -> &str {
    Some(title(n))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| class(n))
}

pub(crate) fn input<'a>(m: &'a Map<String, Value>, id: &str, k: &str) -> Option<&'a Value> {
    m.get(id)?.get("inputs")?.get(k)
}

pub(crate) fn set(m: &mut Map<String, Value>, id: &str, k: &str, v: Value) {
    if let Some(inp) = m
        .get_mut(id)
        .and_then(|n| n.get_mut("inputs"))
        .and_then(Value::as_object_mut)
    {
        inp.insert(k.into(), v);
    }
}

/// 節點編號照數字順序（"9" 在 "10" 前面；子圖的 "75:12" 排後面）
pub(crate) fn sorted_ids(m: &Map<String, Value>) -> Vec<String> {
    let mut ids: Vec<String> = m.keys().cloned().collect();
    ids.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
    ids
}

/// 一個提示詞要換的欄位們：(節點, 欄位)。同一個節點可以有好幾格（SDXL 的 text_g＋text_l、Flux 的 clip_l＋t5xxl…），每一格都換
pub(crate) type Slots = Vec<(String, String)>;

/// 只有一格文字的節點（CLIPTextEncode 的 text、各種編碼器的 prompt）
const TEXT_KEYS: [&str; 2] = ["text", "prompt"];
/// 有好幾格文字的節點，有哪格換哪格：CLIPTextEncodeSDXL（text_g、text_l）、CLIPTextEncodeFlux（clip_l、t5xxl）、
/// CLIPTextEncodeSD3（clip_l、clip_g、t5xxl）、HunyuanDiT（bert、mt5xl）
const MULTI_TEXT_KEYS: [&str; 7] = [
    "text_g", "text_l", "clip_l", "clip_g", "t5xxl", "bert", "mt5xl",
];
/// 文字框的欄位接到別的節點時，那個「放字串的節點」把字串放在哪個欄位（Primitive String、StringConstant、Text Multiline、CR Text…）
const STRING_KEYS: [&str; 5] = ["value", "string", "text", "prompt", "String"];

/// 文字欄位接到別的節點（Primitive、String、Text Multiline…）：順著連線找到真正放字串的節點欄位；上游沒有放字串的欄位就 None
pub(crate) fn text_source(m: &Map<String, Value>, link: &Value) -> Option<(String, String)> {
    let from = link.get(0)?.as_str()?;
    let inp = m.get(from)?.get("inputs")?.as_object()?;
    STRING_KEYS
        .iter()
        .find(|k| inp.get(**k).is_some_and(Value::is_string))
        .map(|k| (from.to_string(), (*k).to_string()))
}

/// 這個節點自己放的提示詞欄位（沒有就空）：text／prompt；SDXL、Flux、SD3 的多格欄位；Efficiency Eff. Loader 的 positive／negative 字串欄位。
/// 欄位是連線的話，換成連線上游放字串的那個欄位（text_source）
fn text_here(m: &Map<String, Value>, id: &str, inp: &Map<String, Value>, neg: bool) -> Slots {
    let is_str = |k: &str| inp.get(k).is_some_and(Value::is_string);
    if let Some(k) = TEXT_KEYS.into_iter().find(|k| is_str(k)) {
        return vec![(id.to_string(), k.to_string())];
    }
    let mut out: Slots = vec![];
    for k in MULTI_TEXT_KEYS {
        let slot = match inp.get(k) {
            Some(Value::String(_)) => Some((id.to_string(), k.to_string())),
            Some(v) if is_link(v) => text_source(m, v),
            _ => None,
        };
        if let Some(s) = slot.filter(|s| !out.contains(s)) {
            out.push(s);
        }
    }
    if !out.is_empty() {
        return out;
    }
    if let Some(s) = TEXT_KEYS.into_iter().find_map(|k| {
        inp.get(k)
            .filter(|v| is_link(v))
            .and_then(|v| text_source(m, v))
    }) {
        return vec![s];
    }
    let own = if neg { "negative" } else { "positive" };
    if is_str(own) {
        return vec![(id.to_string(), own.to_string())];
    }
    vec![]
}

/// 從某個節點往回找文字框：先走 positive（找負面時先走 negative），再走 conditioning 開頭的輸入。
/// neg＝找的是負面提示詞（Eff. Loader 這類把 positive、negative 都放在同一個節點的，要看是哪一邊）
pub(crate) fn walk_text(
    m: &Map<String, Value>,
    id: &str,
    depth: usize,
    neg: bool,
) -> Option<Slots> {
    let inp = m.get(id)?.get("inputs")?.as_object()?;
    let here = text_here(m, id, inp, neg);
    if !here.is_empty() {
        return Some(here);
    }
    if depth >= 12 {
        return None;
    }
    let link = |k: &str| inp.get(k).filter(|v| is_link(v));
    let next = neg
        .then(|| link("negative"))
        .flatten()
        .or_else(|| link("positive"))
        .or_else(|| {
            inp.iter()
                .filter(|(k, v)| k.starts_with("conditioning") && is_link(v))
                .map(|(_, v)| v)
                .next()
        })?;
    walk_text(m, next[0].as_str()?, depth + 1, neg)
}

/// 一組文字欄位，和用到它的節點
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cand {
    pub slots: Slots,
    /// 從哪些節點的 positive／negative（或 Guider 的 conditioning）連線找到它
    pub owners: Vec<String>,
}

/// 每個有 positive（neg＝negative）連線的節點，往回找到的文字欄位；同一組欄位合併成一筆。
/// guiders＝Flux 這類取樣節點沒有 positive 輸入，要從 BasicGuider 之類 Guider 節點的 conditioning 找
pub(crate) fn text_candidates(
    m: &Map<String, Value>,
    ids: &[String],
    neg: bool,
    guiders: bool,
) -> Vec<Cand> {
    let key = if neg { "negative" } else { "positive" };
    let mut out: Vec<Cand> = vec![];
    for id in ids {
        let inp = &m[id]["inputs"];
        let link = inp.get(key).filter(|v| is_link(v)).or_else(|| {
            let guider = guiders && !neg && class(m.get(id)).contains("Guider");
            guider
                .then(|| inp.get("conditioning").filter(|v| is_link(v)))
                .flatten()
        });
        let Some(slots) = link
            .and_then(|l| l[0].as_str())
            .and_then(|from| walk_text(m, from, 0, neg))
        else {
            continue;
        };
        match out.iter_mut().find(|c| c.slots == slots) {
            Some(c) => c.owners.push(id.clone()),
            None => out.push(Cand {
                slots,
                owners: vec![id.clone()],
            }),
        }
    }
    out
}

pub(crate) fn find_prompt(
    m: &Map<String, Value>,
    ids: &[String],
) -> R<(Vec<(String, String)>, bool)> {
    let mut hits = vec![];
    for id in ids {
        for (k, v) in m[id]["inputs"].as_object().into_iter().flatten() {
            if v.as_str().is_some_and(|s| s.contains(PROMPT)) {
                hits.push((id.clone(), k.clone()));
            }
        }
    }
    if !hits.is_empty() {
        return Ok((hits, true));
    }
    let mut found = text_candidates(m, ids, false, false);
    match found.len() {
        1 => Ok((found.remove(0).slots, false)),
        0 => Err(format!("找不到要放提示詞的地方：到「設定 → 影片模型 → 對應與參數…」指定（或在 ComfyUI 正面提示詞的文字框裡寫上 {PROMPT}）")),
        _ => Err(format!("有好幾個正面提示詞的文字框，不知道要換哪個：到「設定 → 影片模型 → 對應與參數…」指定（或在那個文字框裡寫上 {PROMPT}）")),
    }
}

/// 看懂工作流：哪裡放提示詞、首格圖、種子、長寬、長度，存在哪裡。看不懂就回傳原因（給使用者看的）
pub fn analyze(graph: &Value) -> R<Analysis> {
    analyze_with(graph, &Mapping::default())
}

/// 首格圖的自動判斷：標了 {{image}} 的 Load Image → 唯一的 Load Image → 沒有（只能文生影片）
fn auto_image(m: &Map<String, Value>, ids: &[String]) -> R<Option<String>> {
    let loads: Vec<&String> = ids
        .iter()
        .filter(|id| class(m.get(*id)) == "LoadImage")
        .collect();
    let marked: Vec<&String> = loads
        .iter()
        .copied()
        .filter(|id| title(m.get(*id)).contains(IMAGE))
        .collect();
    match (marked.len(), loads.len()) {
        (1, _) => Ok(Some(marked[0].clone())),
        (0, 0) => Ok(None),
        (0, 1) => Ok(Some(loads[0].clone())),
        (0, _) => Err(format!(
            "有好幾個 Load Image，不知道哪個是首格圖：到「設定 → 影片模型 → 對應與參數…」選一個（或在 ComfyUI 把那個節點的標題改成 {IMAGE}）"
        )),
        _ => Err(format!("有好幾個標題寫了 {IMAGE} 的 Load Image：只能標一個")),
    }
}

/// 存影片的節點（匯入時至少要有一個）
fn has_output(m: &Map<String, Value>) -> bool {
    m.values()
        .any(|n| OUTPUTS.iter().any(|(c, ..)| *c == class(Some(n))))
}

/// 照使用者的對應設定看懂工作流：指定了提示詞、首格圖的就用指定的，其他照自動判斷
pub fn analyze_with(graph: &Value, map: &Mapping) -> R<Analysis> {
    let m = nodes(graph)?;
    map.check(m)?;
    let ids = sorted_ids(m);
    let mut a = Analysis::default();
    for id in &ids {
        let n = m.get(id);
        let Some((cls, src, fps_key)) = OUTPUTS.iter().find(|(c, ..)| *c == class(n)) else {
            continue;
        };
        let Some(link) = input(m, id, src).filter(|v| is_link(v)) else {
            return Err(format!("存影片的節點「{}」沒有接上畫面", label(n)));
        };
        a.outputs.push(id.clone());
        if a.fps.is_some() {
            continue;
        }
        // Save Video 的每秒格數在接過來的 Create Video 上
        let from = link[0].as_str().unwrap_or("");
        let fps = if *cls != "SaveVideo" {
            input(m, id, fps_key).and_then(Value::as_f64)
        } else if class(m.get(from)) == "CreateVideo" {
            input(m, from, "fps").and_then(Value::as_f64)
        } else {
            None
        };
        a.fps = fps.filter(|f| *f > 0.0);
    }
    if a.outputs.is_empty() {
        return Err(
            "找不到存影片的節點：工作流的最後要有 Save Video、Save WEBM 或 Video Combine（VHS）"
                .into(),
        );
    }
    (a.prompt, a.placeholder) = match map.prompt.as_ref().filter(|p| !p.is_empty()) {
        Some(p) => (
            p.clone(),
            p.iter().all(|(id, k)| {
                input(m, id, k)
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.contains(PROMPT))
            }),
        ),
        None => find_prompt(m, &ids)?,
    };

    a.image = match map.image.as_deref() {
        Some("") => None,
        Some(id) => Some(id.to_string()),
        None => match map
            .media_slots
            .iter()
            .find(|s| s.kind == "image" && s.role == "first")
        {
            Some(slot) => (slot.input == "image").then(|| slot.node.clone()),
            None => auto_image(m, &ids)?,
        },
    };
    for slot in &map.media_slots {
        if slot.role == "first" && a.image.as_deref().is_some_and(|id| id != slot.node.as_str() || slot.input != "image") {
            return Err("首幀素材槽與舊的首格圖對應不同：請將「首格圖放哪」改回自動，或對應同一個欄位".into());
        }
        if a.image.as_deref() == Some(slot.node.as_str())
            && slot.input == "image"
            && slot.role != "first"
        {
            return Err(format!(
                "素材槽「{}」與首格圖用了同一個欄位：請將用途設為首幀，或改選其他節點",
                slot.label
            ));
        }
    }

    let num = |id: &str, k: &str| input(m, id, k).is_some_and(Value::is_number);
    a.h3 = ids.iter().any(|id| {
        let c = class(m.get(id));
        c.starts_with("MiniMaxH3") || c.starts_with("EmptyMiniMaxH3")
    });
    for id in &ids {
        for k in ["seed", "noise_seed"] {
            if num(id, k) {
                a.seeds.push((id.clone(), k.to_string()));
            }
        }
        if num(id, "width") && num(id, "height") && num(id, "length") {
            a.sizes.push(id.clone());
        }
        if num(id, "frames_number") {
            a.frames_number.push(id.clone());
        }
    }
    Ok(a)
}

impl Analysis {
    /// 秒數能不能調：要知道每秒幾格，也要有放長度的節點
    pub fn seconds(&self) -> bool {
        self.fps.is_some() && !self.sizes.is_empty()
    }

    /// 給設定頁看的摘要（文字由網頁組）
    pub fn summary(&self, graph: &Value) -> Value {
        let m = graph.as_object();
        let node = |id: &str| {
            let n = m.and_then(|m| m.get(id));
            json!({"node": id, "class": class(n), "title": label(n)})
        };
        json!({
            "prompt": self.prompt.iter().map(|(id, _)| node(id)).collect::<Vec<_>>(),
            "placeholder": self.placeholder,
            "image": self.image.as_deref().map(node),
            "seeds": self.seeds.len(),
            "sizes": self.sizes.len(),
            "fps": self.fps,
            "seconds": self.seconds(),
            "outputs": self.outputs.iter().map(|id| node(id)).collect::<Vec<_>>(),
        })
    }
}

/// 自訂工作流的節點圖。wf 是存檔的內容（{"name", "graph"}）；回傳 (nodes, info)，跟 graphs::build 一樣
pub fn build(
    p: &Value,
    wf: &Value,
    input_dir: &Path,
    seed_offset: i64,
) -> Result<(Map<String, Value>, Value), GraphError> {
    let graph = &wf["graph"];
    let map = Mapping::from_value(&wf["map"]);
    let a = analyze_with(graph, &map).map_err(GraphError)?;
    let mut g = nodes(graph).map_err(GraphError)?.clone();
    let prompt = p["prompt"].as_str().unwrap_or("").trim().to_string();
    if prompt.is_empty() {
        return Err(GraphError("影片要寫提示詞：描述畫面裡的動作和聲音".into()));
    }
    if truthy(p.get("video_preview")) {
        return Err(GraphError(
            "自訂的影片工作流沒有預覽模式：把「先出預覽」取消勾選".into(),
        ));
    }
    let seed = p["seed"].as_i64().unwrap_or(0) + seed_offset;
    let supplied = match p.get("media_slots") {
        None => Map::new(),
        Some(Value::Object(values)) => values.clone(),
        Some(_) => {
            return Err(GraphError(
                "media_slots 要是素材槽代號對應已上傳檔名的物件".into(),
            ))
        }
    };
    for (key, value) in &supplied {
        let slot = map
            .media_slots
            .iter()
            .find(|s| s.key == *key)
            .ok_or_else(|| GraphError(format!("這個工作流沒有素材槽：{key}")))?;
        let name = value
            .as_str()
            .ok_or_else(|| GraphError(format!("素材槽「{}」要填已上傳的檔名", slot.label)))?;
        media_input_path(input_dir, name, &slot.kind).map_err(GraphError)?;
    }
    if let Some(slot) = p.get("wf_overrides").and_then(Value::as_object).and_then(|ov| {
        map.media_slots.iter().find(|s| ov.contains_key(&format!("{}.{}", s.node, s.input)))
    }) {
        return Err(GraphError(format!("「{}」是素材槽，請透過選檔指定，不能同時當一般參數覆寫", slot.label)));
    }

    for (id, k) in &a.prompt {
        let v = if a.placeholder {
            json!(input(&g, id, k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .replace(PROMPT, &prompt))
        } else {
            json!(prompt)
        };
        set(&mut g, id, k, v);
    }
    let named_first = map.media_slots.iter().find(|s| s.role == "first" && s.kind == "image");
    let explicit_first = named_first.and_then(|s| supplied.get(&s.key)).and_then(Value::as_str);
    let init = explicit_first.or_else(|| {
        p.get("init_image")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    });
    let size = match (init, &a.image, named_first) {
        (Some(name), Some(id), _) => {
            set(&mut g, id, "image", json!(name));
            Some(graphs::image_size(&input_dir.join(name))?)
        }
        (Some(name), None, Some(slot)) => {
            media_input_path(input_dir, name, "image").map_err(GraphError)?;
            set(&mut g, &slot.node, &slot.input, json!(name));
            Some(graphs::image_size(&input_dir.join(name))?)
        }
        (Some(_), None, None) => return Err(GraphError(
            "這個影片工作流沒有 Load Image，只能文生影片：到「設定 → 影片模型」換一個能圖生影片的"
                .into(),
        )),
        (None, Some(_), _) => {
            return Err(GraphError(
                "這個影片工作流要一張首格圖：從圖片按「動起來」".into(),
            ))
        }
        (None, None, _) => None,
    };
    for (id, k) in &a.seeds {
        set(&mut g, id, k, json!(seed));
    }
    // 對應設定裡固定改的值，再來是「動起來」這次調的（釘選的欄位）
    crate::image_wf::apply_overrides(&mut g, &Value::Object(map.values.clone()));
    if let Some(ov) = p.get("wf_overrides").filter(|v| v.is_object()) {
        crate::image_wf::apply_overrides(&mut g, ov);
    }
    for slot in &map.media_slots {
        if let Some(name) = supplied.get(&slot.key) {
            set(&mut g, &slot.node, &slot.input, name.clone());
        }
    }

    // 長寬照首格圖的比例（像素數不變，對齊 32）；回報的尺寸取最大的那個節點
    let mut wh = (0, 0);
    for id in &a.sizes {
        let get = |k: &str| input(&g, id, k).and_then(Value::as_i64).unwrap_or(0);
        let (mut w, mut h) = (get("width"), get("height"));
        if let Some((ow, oh)) = size.filter(|_| w > 0 && h > 0) {
            (w, h) = graphs::fit_pixels(ow, oh, (w * h) as f64, 32);
            set(&mut g, id, "width", json!(w));
            set(&mut g, id, "height", json!(h));
        }
        if w * h > wh.0 * wh.1 {
            wh = (w, h);
        }
    }
    if wh.0 == 0 {
        wh = size.unwrap_or((0, 0));
    }

    // 長度：8n+1 格（LTX 要 8n+1，Wan、Hunyuan 要 4n+1，兩種都符合）；MiniMax H3 要 17k+5、最長 15 秒
    let mut frames = a
        .sizes
        .first()
        .and_then(|id| input(&g, id, "length"))
        .and_then(Value::as_i64);
    if let (true, Some(fps), Some(secs)) = (
        a.seconds(),
        a.fps,
        p.get("video_seconds").and_then(Value::as_f64),
    ) {
        let n = if a.h3 {
            crate::h3::frames(secs * fps / crate::h3::FPS as f64)
        } else {
            (secs.clamp(1.0, 10.0) * fps) as i64 / 8 * 8 + 1
        };
        for id in &a.sizes {
            set(&mut g, id, "length", json!(n));
        }
        for id in &a.frames_number {
            set(&mut g, id, "frames_number", json!(n));
        }
        frames = Some(n);
    }

    for id in &a.outputs {
        set(&mut g, id, "filename_prefix", json!("AnimaWeb/video"));
        if class(g.get(id)) == "VHS_VideoCombine" {
            set(&mut g, id, "save_output", json!(true));
        }
    }
    if let Some(poster) = p.get("poster").and_then(Value::as_str) {
        let out = &a.outputs[0];
        let video = class(g.get(out)) == "SaveVideo";
        let src = input(&g, out, if video { "video" } else { "images" })
            .cloned()
            .unwrap_or(Value::Null);
        let images = if video {
            g.insert(
                format!("{ADDED}frames"),
                json!({"class_type": "GetVideoComponents", "inputs": {"video": src}}),
            );
            json!([format!("{ADDED}frames"), 0])
        } else {
            src
        };
        let mut save = |name: &str, index: i64, prefix: String| {
            g.insert(
                format!("{ADDED}{name}"),
                json!({"class_type": "ImageFromBatch", "inputs": {"image": images, "batch_index": index, "length": 1}}),
            );
            g.insert(
                format!("{ADDED}{name}_save"),
                json!({"class_type": "SaveImage", "inputs": {"images": [format!("{ADDED}{name}"), 0], "filename_prefix": prefix}}),
            );
        };
        save("first", 0, format!("AnimaWeb/_poster/{poster}"));
        // 導演台：最後一格當下一段的第一格（-1＝最後一格）
        if truthy(p.get("save_last")) {
            save("last", -1, format!("AnimaWeb/_poster/{poster}-last"));
        }
    }
    let info = json!({"prompt": prompt, "negative": "", "seed": seed, "width": wh.0, "height": wh.1, "mode": "video",
                      "family": "custom", "workflow": wf["name"], "frames": frames, "fps": a.fps, "preview": false,
                      "media_slots": map.media_slots.iter().map(|slot| { let mut v = json!(slot); v["file"] = input(&g, &slot.node, &slot.input).cloned().unwrap_or(Value::Null); v }).collect::<Vec<_>>()});
    Ok((g, info))
}

// ---------------------------------------------------------------- 存檔：data\video_workflows\<代號>.json
pub fn dir(data: &Path) -> PathBuf {
    data.join("video_workflows")
}

/// 名字 → 代號（檔名）：只留文字、數字、空白、-、_
pub fn id_of(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    let s = s.trim().to_string();
    // Windows 不能當檔名的 CON、NUL、COM1…
    let up = s.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&up.as_str())
        || (up.len() == 4
            && (up.starts_with("COM") || up.starts_with("LPT"))
            && up.as_bytes()[3].is_ascii_digit());
    if s.is_empty() {
        "workflow".into()
    } else if s == BUILTIN || s == LEGACY_LTX || reserved {
        format!("{s}_wf")
    } else {
        s
    }
}

fn path_of(dir: &Path, id: &str) -> R<PathBuf> {
    if id.is_empty() || id_of(id) != id {
        return Err("影片工作流的代號不對".into());
    }
    Ok(dir.join(format!("{id}.json")))
}

/// 讀存檔（{"name", "added", "graph"}）
pub fn load(dir: &Path, id: &str) -> R<Value> {
    let path = path_of(dir, id)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("找不到影片工作流「{id}」：到「設定 → 影片模型」重新選一個"))?;
    serde_json::from_str(&text)
        .map_err(|_| format!("影片工作流「{id}」的檔案壞了：到「設定 → 影片模型」重新匯入"))
}

fn entry(id: &str, wf: &Value) -> Value {
    let map = Mapping::from_value(&wf["map"]);
    let media = media_slots_of(wf, &map);
    let mut e = json!({"id": id, "name": wf["name"], "added": wf["added"], "pins": pins_of(wf, &map), "media_slots": media, "mapped": !wf["map"].is_null()});
    match analyze_with(&wf["graph"], &map) {
        Ok(a) => {
            e["info"] = a.summary(&wf["graph"]);
            e["info"]["media_slots"] = json!(media_slots_of(wf, &map));
            if let Some(slot) = map.media_slots.iter().find(|s| s.kind == "image" && s.role == "first") {
                e["info"]["image"] = json!({"node": slot.node, "input": slot.input, "class": wf["graph"][&slot.node]["class_type"], "title": slot.label});
            }
        }
        Err(msg) => e["error"] = json!(msg),
    }
    e
}

fn media_slots_of(wf: &Value, map: &Mapping) -> Vec<Value> {
    map.media_slots
        .iter()
        .map(|slot| {
            let mut v = json!(slot);
            v["value"] = wf["graph"][&slot.node]["inputs"][&slot.input].clone();
            v
        })
        .collect()
}

/// 釘到「動起來」的欄位：[{key, label, value（對應設定改過的值，沒改就是工作流原本的）}]；欄位不見了就略過
fn pins_of(wf: &Value, map: &Mapping) -> Vec<Value> {
    let Ok(m) = nodes(&wf["graph"]) else {
        return vec![];
    };
    map.pins
        .iter()
        .filter_map(|(key, l)| {
            let (id, k) = key.split_once('.')?;
            let v = map.values.get(key).cloned().or_else(|| input(m, id, k).cloned())?;
            Some(json!({"key": key, "label": if l.is_empty() { k } else { l.as_str() }, "value": v, "node": id, "input": k, "title": label(m.get(id))}))
        })
        .collect()
}

/// 對應設定的編輯畫面要的：目前的設定、自動判斷的結果、可以選的文字欄位和 Load Image、全部欄位（image_wf::field_rows 的格式）
pub fn detail(dir: &Path, id: &str, object_info: &Value) -> R<Value> {
    let wf = load(dir, id)?;
    let graph = &wf["graph"];
    let m = nodes(graph)?;
    let map = Mapping::from_value(&wf["map"]);
    let ids = sorted_ids(m);
    let used = analyze_with(graph, &map);
    let node = |id: &str| json!({"node": id, "class": class(m.get(id)), "title": label(m.get(id))});
    let auto_prompt = find_prompt(m, &ids).map(|(p, _)| {
        p.iter()
            .map(|(n, k)| format!("{n}.{k}"))
            .collect::<Vec<_>>()
    });
    let auto_img = auto_image(m, &ids);
    let loads: Vec<Value> = ids
        .iter()
        .filter(|id| input(m, id, "image").is_some_and(Value::is_string))
        .map(|id| node(id))
        .collect();
    // 生圖站自己會填的欄位（照現在的設定）：不給在參數裡改
    let mut auto_set = std::collections::HashSet::new();
    if let Ok(a) = &used {
        for (id, k) in a.prompt.iter().chain(&a.seeds) {
            auto_set.insert((id.clone(), k.clone()));
        }
        if let Some(i) = &a.image {
            auto_set.insert((i.clone(), "image".to_string()));
        }
        for id in &a.outputs {
            auto_set.insert((id.clone(), "filename_prefix".to_string()));
        }
    }
    for slot in &map.media_slots {
        auto_set.insert((slot.node.clone(), slot.input.clone()));
    }
    let common = |_: &str, _: &str| false;
    let fields = crate::image_wf::field_rows(m, &auto_set, &[], &common, object_info);
    let media_inputs: Vec<Value> = ids.iter().flat_map(|id| {
        m[id]["inputs"].as_object().into_iter().flatten().filter_map(move |(key, value)| {
            let kind = media_input_kind(&m[id], key, value)?;
            Some(json!({"key": format!("{id}.{key}"), "node": id, "input": key, "class": class(m.get(id)), "title": label(m.get(id)), "kind": kind, "value": value}))
        })
    }).collect();
    // 放得了提示詞的：文字型的欄位（下拉選單、Load Image 的圖、存檔名、看起來像檔名的不算）
    let texts: Vec<Value> = fields
        .iter()
        .filter(|f| f["type"] == "STRING" && !matches!(f["input"].as_str(), Some("image" | "filename_prefix")))
        .filter(|f| !media_inputs.iter().any(|slot| slot["key"] == f["key"]))
        .filter(|f| !OUTPUTS.iter().any(|(c, ..)| f["class"] == *c))
        .filter(|f| !f["value"].as_str().is_some_and(|s| s.ends_with(".safetensors") || s.ends_with(".gguf") || s.ends_with(".ckpt")))
        .map(|f| {
            let preview: String = f["value"].as_str().unwrap_or("").chars().take(80).collect();
            json!({"key": f["key"], "node": f["node"], "input": f["input"], "title": f["title"], "class": f["class"], "preview": preview})
        })
        .collect();
    let slots = |a: &Analysis| json!({"prompt": a.prompt.iter().map(|(n, k)| format!("{n}.{k}")).collect::<Vec<_>>(), "image": a.image});
    Ok(json!({
        "id": id, "name": wf["name"], "map": map.to_value(), "mapped": !wf["map"].is_null(),
        "auto": {"prompt": auto_prompt.as_ref().ok(), "prompt_error": auto_prompt.as_ref().err(),
                 "image": auto_img.as_ref().ok().cloned().flatten(), "image_error": auto_img.as_ref().err()},
        "used": used.as_ref().map(slots).unwrap_or(Value::Null), "error": used.err(),
        "texts": texts, "loads": loads, "fields": fields, "media_inputs": media_inputs,
    }))
}

/// 存對應設定（null＝清掉、回到全部自動）：要指得到存在的欄位；存完照這個設定看不懂的話也存（畫面會顯示原因），回 list() 那樣的一筆
pub fn save_map(dir: &Path, id: &str, map: &Value) -> R<Value> {
    let mut wf = load(dir, id)?;
    let m = nodes(&wf["graph"])?;
    if map.is_null() {
        wf.as_object_mut().map(|o| o.remove("map"));
    } else {
        let mp = Mapping::from_value(map);
        mp.check(m)?;
        wf["map"] = mp.to_value();
    }
    let text = serde_json::to_string_pretty(&wf).map_err(|e| e.to_string())?;
    std::fs::write(path_of(dir, id)?, text).map_err(|e| format!("存檔失敗：{e}"))?;
    Ok(entry(id, &wf))
}

/// 匯入的工作流（照名字排）；每個附上 info（看懂的摘要）或 error（看不懂的原因）
pub fn list(dir: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension()?.to_str()? != "json" || id_of(&id) != id {
                return None;
            }
            let wf: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(entry(&id, &wf))
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

/// 匯入：格式對、有存影片的節點就存（提示詞、首格圖判斷不出來的，之後在「對應與參數…」指定）；同名的覆蓋。
/// 覆蓋時舊的對應設定在新工作流還指得到就留著（重新匯出同一個工作流不用重設）。回傳跟 list() 一樣的一筆
pub fn save(dir: &Path, name: &str, graph: &Value) -> R<Value> {
    let m = nodes(graph)?;
    if !has_output(m) {
        return Err(
            "找不到存影片的節點：工作流的最後要有 Save Video、Save WEBM 或 Video Combine（VHS）"
                .into(),
        );
    }
    let name: String = name.trim().chars().take(60).collect();
    let id = id_of(&name);
    let mut wf = json!({"name": name, "added": now(), "graph": graph});
    if let Ok(old) = load(dir, &id) {
        if !old["map"].is_null() && Mapping::from_value(&old["map"]).check(m).is_ok() {
            wf["map"] = old["map"].clone();
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("建立資料夾失敗：{e}"))?;
    let text = serde_json::to_string_pretty(&wf).map_err(|e| e.to_string())?;
    std::fs::write(path_of(dir, &id)?, text).map_err(|e| format!("存檔失敗：{e}"))?;
    Ok(entry(&id, &wf))
}

/// 刪掉匯入的工作流（使用者電腦上原本的 JSON 不受影響）
pub fn remove(dir: &Path, id: &str) -> R<()> {
    let path = path_of(dir, id)?;
    if !path.is_file() {
        return Err(format!("找不到影片工作流「{id}」"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("刪除失敗：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 像 ComfyUI 範本「Wan 2.2 圖生影片」匯出（API）的樣子（只留會用到的欄位）
    fn wan() -> Value {
        json!({
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["38", 0]}, "_meta": {"title": "CLIP Text Encode (Positive Prompt)"}},
            "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["38", 0]}},
            "38": {"class_type": "CLIPLoader", "inputs": {"clip_name": "umt5.safetensors", "type": "wan"}},
            "52": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
            "50": {"class_type": "WanImageToVideo", "inputs": {"positive": ["6", 0], "negative": ["7", 0], "start_image": ["52", 0],
                   "width": 832, "height": 480, "length": 81, "batch_size": 1}},
            "3": {"class_type": "KSampler", "inputs": {"seed": 123, "positive": ["50", 0], "negative": ["50", 1], "latent_image": ["50", 2]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0]}},
            "57": {"class_type": "CreateVideo", "inputs": {"images": ["8", 0], "fps": 16}},
            "58": {"class_type": "SaveVideo", "inputs": {"video": ["57", 0], "filename_prefix": "video/ComfyUI", "format": "auto", "codec": "auto"}},
        })
    }

    /// 放一張 600×900 的直圖 tall.png（測試平行跑，只寫一次）
    fn input_dir() -> PathBuf {
        static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let d = std::env::temp_dir().join("anima_video_wf_test");
            std::fs::create_dir_all(&d).unwrap();
            image::RgbImage::new(600, 900)
                .save(d.join("tall.png"))
                .unwrap();
            d
        })
        .clone()
    }

    /// 群組裡流通的大流程那樣：好幾個 Load Image、兩個正面提示詞框、一個 LoRA（自動判斷不出來）
    fn busy() -> Value {
        let mut g = wan();
        let m = g.as_object_mut().unwrap();
        m.insert(
            "53".into(),
            json!({"class_type": "LoadImage", "inputs": {"image": "ref.png"}}),
        );
        m.insert("9".into(), json!({"class_type": "CLIPTextEncode", "inputs": {"text": "style words", "clip": ["38", 0]}}));
        m.insert("60".into(), json!({"class_type": "LoraLoaderModelOnly", "inputs": {"model": ["61", 0], "lora_name": "Combat.safetensors", "strength_model": 0.65}}));
        m.insert("51".into(), json!({"class_type": "ConditioningCombine", "inputs": {"conditioning_1": ["6", 0], "conditioning_2": ["9", 0]}}));
        m["50"]["inputs"]["positive"] = json!(["51", 0]);
        m["3"]["inputs"]["steps"] = json!(8);
        g
    }

    fn media_workflow() -> Value {
        let mut graph = busy();
        graph["70"] = json!({"class_type": "VHS_LoadVideo", "inputs": {"video": "motion.mp4"}});
        graph["71"] = json!({"class_type": "LoadAudio", "inputs": {"audio": "voice.wav"}});
        let map = json!({"prompt": [["6", "text"]], "image": "52", "media_slots": [
            {"key":"opening", "label":"首幀", "kind":"image", "role":"first", "node":"52", "input":"image"},
            {"key":"hero", "label":"角色定稿", "kind":"image", "role":"character", "node":"53", "input":"image", "who":"主角"},
            {"key":"motion", "label":"動作", "kind":"video", "role":"motion", "node":"70", "input":"video"},
            {"key":"voice", "label":"聲音", "kind":"audio", "role":"voice", "node":"71", "input":"audio", "who":"主角"}
        ]});
        json!({"name":"media test", "graph":graph, "map":map})
    }

    #[test]
    fn media_slots_bind_independent_files_and_preserve_links() {
        let wf = media_workflow();
        let d = input_dir();
        std::fs::write(d.join("motion.mp4"), b"video fixture").unwrap();
        std::fs::write(d.join("voice.wav"), b"audio fixture").unwrap();
        image::RgbImage::new(900, 600)
            .save(d.join("media_wide.png"))
            .unwrap();
        let params = json!({"prompt":"wave", "init_image":"tall.png", "media_slots": {
            "opening":"media_wide.png", "hero":"tall.png", "motion":"motion.mp4", "voice":"voice.wav"
        }});
        let (out, info) = build(&params, &wf, &d, 0).unwrap();
        assert_eq!(out["52"]["inputs"]["image"], "media_wide.png");
        assert_eq!(out["53"]["inputs"]["image"], "tall.png");
        assert_eq!(out["70"]["inputs"]["video"], "motion.mp4");
        assert_eq!(out["71"]["inputs"]["audio"], "voice.wav");
        assert_eq!(out["50"]["inputs"]["start_image"], json!(["52", 0]));
        assert!(
            info["width"].as_i64().unwrap() > info["height"].as_i64().unwrap(),
            "explicit first frame owns the output ratio"
        );
        assert_eq!(info["media_slots"][1]["who"], "主角");
        assert_eq!(info["media_slots"][1]["file"], "tall.png");
        // Leaving named slots out preserves the legacy init_image and the untouched workflow files.
        let (legacy, _) = build(
            &json!({"prompt":"wave", "init_image":"tall.png"}),
            &wf,
            &d,
            0,
        )
        .unwrap();
        assert_eq!(legacy["52"]["inputs"]["image"], "tall.png");
        assert_eq!(legacy["53"]["inputs"]["image"], "ref.png");
    }

    #[test]
    fn media_slots_reject_paths_unknown_slots_wrong_types_and_override_collisions() {
        let wf = media_workflow();
        for supplied in [
            json!({"hero":"../tall.png"}),
            json!({"hero":"C:\\secret.png"}),
            json!({"hero":"https://example.com/a.png"}),
            json!({"hero":"voice.wav"}),
            json!({"hero":3}),
            json!({"unknown":"tall.png"}),
            json!(["tall.png"]),
            json!({"hero":"missing-file.png"}),
        ] {
            let params = json!({"prompt":"wave", "init_image":"tall.png", "media_slots":supplied});
            assert!(build(&params, &wf, &input_dir(), 0).is_err(), "{params}");
        }
        let params = json!({"prompt":"wave", "init_image":"tall.png", "wf_overrides":{"53.image":"tall.png"}});
        assert!(build(&params, &wf, &input_dir(), 0)
            .unwrap_err()
            .0
            .contains("素材槽"));
        assert!(media_basename("reference.webm", "video").is_ok());
        assert!(media_basename("reference.webm", "audio").is_err());
    }

    #[test]
    fn media_mapping_rejects_duplicate_targets_links_and_non_media_literals() {
        let wf = media_workflow();
        for change in 0..6 {
            let mut map = wf["map"].clone();
            match change {
                0 => map["media_slots"][1]["key"] = json!("opening"),
                1 => {
                    map["media_slots"][1]["node"] = json!("52");
                }
                2 => {
                    map["media_slots"][1]["node"] = json!("50");
                    map["media_slots"][1]["input"] = json!("start_image");
                }
                3 => {
                    map["media_slots"][1]["node"] = json!("6");
                    map["media_slots"][1]["input"] = json!("text");
                }
                4 => map["values"] = json!({"53.image":"ref.png"}),
                _ => map["media_slots"] = json!([{"key":"broken"}]),
            }
            assert!(
                analyze_with(&wf["graph"], & Mapping::from_value(&map)).is_err(),
                "{map}"
            );
        }
        let mut automatic = wf["map"].clone();
        automatic.as_object_mut().unwrap().remove("image");
        assert_eq!(
            analyze_with(&wf["graph"], &Mapping::from_value(&automatic))
                .unwrap()
                .image
                .as_deref(),
            Some("52")
        );
    }

    #[test]
    fn named_first_frame_can_target_a_custom_literal_input() {
        let mut wf = media_workflow();
        wf["graph"]["72"] = json!({"class_type":"CustomImageLoader", "inputs":{"first_frame":"original.png"}});
        wf["map"].as_object_mut().unwrap().remove("image");
        wf["map"]["media_slots"][0]["node"] = json!("72");
        wf["map"]["media_slots"][0]["input"] = json!("first_frame");
        let (graph, _) = build(&json!({"prompt":"wave", "init_image":"tall.png"}), &wf, &input_dir(), 0).unwrap();
        assert_eq!(graph["72"]["inputs"]["first_frame"], "tall.png");
        assert_eq!(graph["52"]["inputs"]["image"], "example.png", "a named first frame does not overwrite another image loader");
        wf["map"]["image"] = json!("52");
        assert!(analyze_with(&wf["graph"], &Mapping::from_value(&wf["map"])).is_err());
    }

    #[test]
    fn media_slots_detail_and_storage_keep_role_identity_and_literal_candidates() {
        let wf = media_workflow();
        let d = std::env::temp_dir().join(format!("anima_media_slots_{}", rand::random::<u64>()));
        save(&d, "media", &wf["graph"]).unwrap();
        let entry = save_map(&d, "media", &wf["map"]).unwrap();
        assert_eq!(entry["media_slots"][1]["who"], "主角");
        assert_eq!(entry["info"]["media_slots"][2]["kind"], "video");
        let detail = detail(&d, "media", &Value::Null).unwrap();
        let candidates = detail["media_inputs"].as_array().unwrap();
        assert!(candidates
            .iter()
            .any(|c| c["key"] == "71.audio" && c["kind"] == "audio"));
        assert!(candidates
            .iter()
            .any(|c| c["key"] == "70.video" && c["kind"] == "video"));
        assert!(!candidates
            .iter()
            .any(|c| c["key"] == "50.start_image" || c["key"] == "60.lora_name"));
        assert!(!detail["texts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["key"] == "71.audio"));
        assert_eq!(Mapping::from_value(&wf["map"]).to_value(), detail["map"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn mapping_fixes_ambiguous_workflow() {
        // 自動判斷：兩個 Load Image → 不知道首格圖
        assert!(analyze(&busy()).unwrap_err().contains("對應與參數"));
        let map = Mapping::from_value(&json!({"prompt": [["6", "text"]], "image": "52",
            "values": {"60.strength_model": 0.4, "3.steps": "12"}, "pins": [{"key": "60.strength_model", "label": "武打 LoRA"}]}));
        let a = analyze_with(&busy(), &map).unwrap();
        assert_eq!((a.prompt.clone(), a.image.as_deref()), (vec![("6".to_string(), "text".to_string())], Some("52")));
        // 存成 JSON 再讀回來一樣
        assert_eq!(Mapping::from_value(&map.to_value()), map);
        // 指到連線、不存在的欄位、不是 Load Image、不是文字：擋
        for bad in [json!({"values": {"50.positive": 1}}), json!({"values": {"99.x": 1}}), json!({"image": "6"}),
                    json!({"prompt": [["3", "seed"]]}), json!({"pins": [{"key": "nodot"}]}),
        ] {
            assert!(analyze_with(&busy(), &Mapping::from_value(&bad)).is_err(), "{bad}");
        }
        // 「沒有首格圖」＝只能文生影片
        assert!(analyze_with(&busy(), &Mapping::from_value(&json!({"prompt": [["6", "text"]], "image": ""}))).unwrap().image.is_none());

        // 生成：固定改的值、這次「動起來」調的值都套上（後者優先；數字字串轉成數字）
        let wf = json!({"name": "大流程", "graph": busy(), "map": map.to_value()});
        let p = json!({"prompt": "she waves", "init_image": "tall.png", "seed": 1, "wf_overrides": {"60.strength_model": 0.9}});
        let (g, _) = build(&p, &wf, &input_dir(), 0).unwrap();
        assert_eq!(g["6"]["inputs"]["text"], "she waves");
        assert_eq!(g["9"]["inputs"]["text"], "style words", "沒指定的提示詞框不動");
        assert_eq!(g["52"]["inputs"]["image"], "tall.png");
        assert_eq!(g["53"]["inputs"]["image"], "ref.png");
        assert_eq!(g["60"]["inputs"]["strength_model"], 0.9);
        assert_eq!(g["3"]["inputs"]["steps"], 12);
    }

    #[test]
    fn mapping_store_and_reimport() {
        let d = std::env::temp_dir().join("anima_video_wf_map");
        let _ = std::fs::remove_dir_all(&d);
        // 判斷不出來的也能匯入（列表顯示原因）
        let e = save(&d, "大流程", &busy()).unwrap();
        assert!(e["error"].as_str().unwrap().contains("Load Image"));
        let map = json!({"prompt": [["6", "text"]], "image": "52", "pins": [{"key": "60.strength_model", "label": "武打 LoRA"}]});
        let e = save_map(&d, "大流程", &map).unwrap();
        assert!(e["error"].is_null() && e["mapped"] == true);
        assert_eq!(e["pins"][0]["label"], "武打 LoRA");
        assert_eq!(e["pins"][0]["value"], 0.65);
        // 編輯畫面：文字欄位、Load Image、全部欄位（提示詞、首格圖、種子標成自動）
        let det = detail(&d, "大流程", &Value::Null).unwrap();
        assert_eq!(det["loads"].as_array().unwrap().len(), 2);
        assert!(det["auto"]["image_error"].is_string() && det["error"].is_null());
        assert_eq!(det["auto"]["prompt"], json!(["6.text"]));
        // 文字欄位：提示詞框才列（LoRA 檔名、Load Image 的圖、存檔名不列）
        let texts: Vec<&str> = det["texts"].as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap()).collect();
        assert!(texts.contains(&"6.text") && texts.contains(&"9.text") && !texts.iter().any(|k| k.starts_with("60.") || k.starts_with("52.")), "{texts:?}");
        let f = det["fields"].as_array().unwrap();
        let get = |k: &str| f.iter().find(|x| x["key"] == k).cloned().unwrap();
        assert_eq!((get("6.text")["auto"].as_bool(), get("60.strength_model")["auto"].as_bool()), (Some(true), Some(false)));
        // 重新匯入同一個工作流：對應設定留著；新工作流指不到了就清掉
        assert_eq!(save(&d, "大流程", &busy()).unwrap()["mapped"], true);
        assert_eq!(save(&d, "大流程", &wan()).unwrap()["mapped"], false);
        // 清掉＝回到全部自動
        save(&d, "大流程", &busy()).unwrap();
        save_map(&d, "大流程", &map).unwrap();
        assert_eq!(save_map(&d, "大流程", &Value::Null).unwrap()["mapped"], false);
        // 沒有存影片的節點：不收
        assert!(save(&d, "沒輸出", &json!({"1": {"class_type": "LoadImage", "inputs": {"image": "a.png"}}})).unwrap_err().contains("存影片"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn analyze_wan_without_marks() {
        let a = analyze(&wan()).unwrap();
        assert_eq!(a.prompt, vec![("6".to_string(), "text".to_string())]);
        assert!(!a.placeholder);
        assert_eq!(a.image.as_deref(), Some("52"));
        assert_eq!(a.seeds, vec![("3".to_string(), "seed".to_string())]);
        assert_eq!(a.sizes, vec!["50".to_string()]);
        assert_eq!(a.fps, Some(16.0));
        assert_eq!(a.outputs, vec!["58".to_string()]);
        assert!(a.seconds());
    }

    #[test]
    fn build_wan_image_to_video() {
        let wf = json!({"name": "Wan 測試", "graph": wan()});
        let p = json!({"prompt": "she waves", "init_image": "tall.png", "seed": 42, "video_seconds": 5,
                       "poster": "abc", "save_last": true});
        let (g, info) = build(&p, &wf, &input_dir(), 1).unwrap();
        assert_eq!(g["6"]["inputs"]["text"], "she waves");
        assert_eq!(g["7"]["inputs"]["text"], "blurry", "負面不動");
        assert_eq!(g["52"]["inputs"]["image"], "tall.png");
        assert_eq!(g["3"]["inputs"]["seed"], 43);
        // 600×900 的直圖：像素數維持 832×480 左右，比例變直的
        let (w, h) = (g["50"]["inputs"]["width"].as_i64().unwrap(), g["50"]["inputs"]["height"].as_i64().unwrap(),
        );
        assert!(h > w && w % 32 == 0 && h % 32 == 0, "{w}×{h}");
        assert!(((w * h) as f64 / (832.0 * 480.0) - 1.0).abs() < 0.1, "{w}×{h}");
        assert_eq!(g["50"]["inputs"]["length"], 81, "5 秒 × 16 格 → 8n+1");
        assert_eq!(g["58"]["inputs"]["filename_prefix"], "AnimaWeb/video");
        // 封面和最後一格從 Save Video 接的影片拆出來
        assert_eq!(g["anima_web_frames"]["class_type"], "GetVideoComponents");
        assert_eq!(g["anima_web_frames"]["inputs"]["video"], json!(["57", 0]));
        assert_eq!(g["anima_web_first_save"]["inputs"]["filename_prefix"], "AnimaWeb/_poster/abc");
        assert_eq!(g["anima_web_last"]["inputs"]["batch_index"], -1);
        assert_eq!(g["anima_web_last_save"]["inputs"]["filename_prefix"], "AnimaWeb/_poster/abc-last");
        assert_eq!(info["width"], w);
        assert_eq!(info["frames"], 81);
        assert_eq!(info["fps"], 16.0);
        assert_eq!(info["workflow"], "Wan 測試");
        assert_eq!(info["seed"], 43);
    }

    #[test]
    fn placeholder_and_marked_image() {
        let mut g = wan();
        g["6"]["inputs"]["text"] = json!("{{prompt}}, masterpiece");
        g["53"] = json!({"class_type": "LoadImage", "inputs": {"image": "ref.png"}});
        assert!(analyze(&g).unwrap_err().contains("好幾個 Load Image"));
        g["52"]["_meta"] = json!({"title": "首格 {{image}}"});
        let a = analyze(&g).unwrap();
        assert!(a.placeholder);
        assert_eq!(a.image.as_deref(), Some("52"));
        let wf = json!({"name": "x", "graph": g});
        let p = json!({"prompt": "she waves", "init_image": "tall.png", "seed": 1});
        let (out, info) = build(&p, &wf, &input_dir(), 0).unwrap();
        assert_eq!(out["6"]["inputs"]["text"], "she waves, masterpiece");
        assert_eq!(out["53"]["inputs"]["image"], "ref.png");
        assert_eq!(out["50"]["inputs"]["length"], 81, "沒送秒數：照工作流");
        assert!(out.get("anima_web_first").is_none(), "沒有 poster 就不存封面");
        assert_eq!(info["frames"], 81);
    }

    #[test]
    fn ambiguous_prompt_needs_mark() {
        let mut g = wan();
        g["9"] = json!({"class_type": "CLIPTextEncode", "inputs": {"text": "other", "clip": ["38", 0]}});
        g["10"] = json!({"class_type": "KSampler", "inputs": {"seed": 5, "positive": ["9", 0], "negative": ["7", 0], "latent_image": ["3", 0]}});
        assert!(analyze(&g).unwrap_err().contains("好幾個正面提示詞"));
        g["9"]["inputs"]["text"] = json!("{{prompt}}");
        let a = analyze(&g).unwrap();
        assert_eq!(a.prompt, vec![("9".to_string(), "text".to_string())]);
        assert_eq!(a.seeds.len(), 2);
    }

    #[test]
    fn rejects_ui_format_and_missing_output() {
        assert!(analyze(&json!({"nodes": [], "links": []})).unwrap_err().contains("匯出（API）"));
        assert!(analyze(&json!([1, 2])).is_err());
        let mut g = wan();
        g.as_object_mut().unwrap().remove("58");
        assert!(analyze(&g).unwrap_err().contains("找不到存影片的節點"));
    }

    #[test]
    fn vhs_output_and_text_to_video() {
        let g = json!({
            "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "x", "clip": ["9", 0]}},
            "2": {"class_type": "EmptyHunyuanLatentVideo", "inputs": {"width": 848, "height": 480, "length": 73, "batch_size": 1}},
            "3": {"class_type": "SamplerCustomAdvanced", "inputs": {"guider": ["4", 0], "latent_image": ["2", 0]}},
            "4": {"class_type": "CFGGuider", "inputs": {"positive": ["5", 0], "negative": ["1", 0]}},
            "5": {"class_type": "FluxGuidance", "inputs": {"conditioning": ["1", 0], "guidance": 6}},
            "6": {"class_type": "RandomNoise", "inputs": {"noise_seed": 7}},
            "8": {"class_type": "VHS_VideoCombine", "inputs": {"images": ["3", 0], "frame_rate": 24, "filename_prefix": "x", "save_output": false}},
        });
        let a = analyze(&g).unwrap();
        assert_eq!(a.prompt, vec![("1".to_string(), "text".to_string())], "positive → FluxGuidance → conditioning → 文字框");
        assert_eq!(a.image, None);
        let wf = json!({"name": "hy", "graph": g});
        let p = json!({"prompt": "rain", "seed": 9, "video_seconds": 3, "poster": "pp"});
        let (out, info) = build(&p, &wf, &input_dir(), 0).unwrap();
        assert_eq!(out["6"]["inputs"]["noise_seed"], 9);
        assert_eq!(out["2"]["inputs"]["width"], 848, "文生影片不改長寬");
        assert_eq!(out["2"]["inputs"]["length"], 73, "3 秒 × 24 格 → 72 → 73");
        assert_eq!(out["8"]["inputs"]["save_output"], true);
        assert_eq!(out["anima_web_first"]["inputs"]["image"], json!(["3", 0]));
        assert!(out.get("anima_web_last").is_none());
        assert_eq!(info["width"], 848);
        // 要圖的請求丟給文生影片的工作流：講清楚
        let p = json!({"prompt": "rain", "seed": 9, "init_image": "tall.png"});
        assert!(build(&p, &wf, &input_dir(), 0).unwrap_err().0.contains("只能文生影片"));
        let p = json!({"prompt": "rain", "seed": 9, "video_preview": true});
        assert!(build(&p, &wf, &input_dir(), 0).unwrap_err().0.contains("預覽"));
    }

    #[test]
    fn h3_grid() {
        let wf = json!({"name": "h3", "graph": {
            "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}", "clip": ["9", 0]}},
            "2": {"class_type": "MiniMaxH3ImageToVideo", "inputs": {"clip": ["9", 0], "vae": ["8", 0], "prompt": "{{prompt}}", "width": 864, "height": 480, "length": 124}},
            "3": {"class_type": "CreateVideo", "inputs": {"images": ["4", 0], "fps": 24.0}},
            "5": {"class_type": "SaveVideo", "inputs": {"video": ["3", 0], "filename_prefix": "x", "format": "auto", "format.codec": "auto"}}
        }});
        let a = analyze(&wf["graph"]).unwrap();
        assert!(a.h3 && a.seconds());
        for (secs, n) in [(10.0, 243), (15.0, 362), (20.0, 362), (5.0, 124)] {
            let (g, info) = build(&json!({"prompt": "hi", "video_seconds": secs}), &wf, &input_dir(), 0,
            ).unwrap();
            assert_eq!(g["2"]["inputs"]["length"], n, "{secs} 秒");
            assert_eq!(info["frames"], n);
        }
    }

    /// 2026-10-30 LTX 退場：內建影片就是 H3；設定、導演台、拍攝紀錄裡留著的 ltx23 一律讀成 h3
    #[test]
    fn legacy_engine_maps_to_h3() {
        assert_eq!(BUILTIN, "h3");
        assert_eq!(normalize_engine("ltx23"), "h3");
        assert_eq!(normalize_engine("h3"), "h3");
        assert_eq!(normalize_engine("wan-a"), "wan-a");
    }

    #[test]
    fn ids_and_storage() {
        assert_eq!(id_of("  Wan 2.2 / 圖生影片 "), "Wan 2_2 _ 圖生影片");
        assert_eq!(id_of("ltx23"), "ltx23_wf");
        // h3 是 H3 影片台的代號：匯入名為 h3 的工作流不能跟它撞（審查第 13 點）
        assert_eq!(id_of("h3"), "h3_wf");
        assert_eq!(id_of("con"), "con_wf");
        assert_eq!(id_of("..."), "___");
        assert!(path_of(Path::new("x"), "../a").is_err());
        let d = std::env::temp_dir().join("anima_video_wf_store");
        let _ = std::fs::remove_dir_all(&d);
        let e = save(&d, "Wan 2.2", &wan()).unwrap();
        assert_eq!(e["id"], "Wan 2_2");
        assert_eq!(e["info"]["seconds"], true);
        assert_eq!(list(&d).len(), 1);
        assert_eq!(load(&d, "Wan 2_2").unwrap()["graph"], wan());
        assert!(save(&d, "壞的", &json!({"nodes": [], "links": []})).is_err());
        remove(&d, "Wan 2_2").unwrap();
        assert!(list(&d).is_empty());
        assert!(load(&d, "Wan 2_2").unwrap_err().contains("找不到"));
    }
}

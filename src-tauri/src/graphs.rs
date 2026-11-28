//! 把生成參數組成 ComfyUI API 節點圖（POST /prompt 的 "prompt" 欄位）。逐行對照舊版 anima_ui/app/graphs.py：
//! 同樣的參數必須產生完全一樣的節點圖（tests/golden_graphs.rs 拿 Python 版的輸出比對）。
//!
//! 五種模式：txt2img、img2img、inpaint、multiref（Qwen 2.1 限定）、upscale。
//! 兩個家族：anima（CLIPTextEncode＋qwen_image_vae，可用 LLLite、Turbo LoRA）、
//!           qwen21（TextEncodeQwenImage21＋2.1 專用 VAE；圖生圖把輸入圖當參考圖）、
//!           moss（MOSS-Image：moss_image 文字編碼器＋FLUX.2 VAE、AuraFlow 位移、跳第 22 層）、
//!           krea2（Krea 2：Qwen3-VL-4B 文字編碼器＋qwen_image_vae；照 ComfyUI 0.43 內建範本「Text to Image (Krea-2 Turbo)」）。
//! Qwen 2.1 在 2.0.19 拿掉、2.1.3 裝回（使用者要求），跟 MOSS 並存：選哪個模型就走哪一套。
//!
//! 跟 Python 對齊的細節：
//! - Python 的 round() 遇到 .5 取偶數 → f64::round_ties_even
//! - `{a|b}` 用 pyrandom（跟 random.Random(seed) 一致）
//! - 每個欄位的整數／小數型別照舊版（ComfyUI 驗證會看型別，golden test 也會比）
use crate::py::truthy;
use crate::pyrandom::PyRandom;
use serde_json::{json, Map, Value};
use std::path::Path;

pub const CLIP: &str = "qwen_3_06b_base.safetensors";
pub const VAE: &str = "qwen_image_vae.safetensors";
pub const TURBO_LORA: &str = "anima-turbo-lora-v0.2.safetensors";
pub const QWEN21_TE: &str = "qwen3vl_8b_w4a8.safetensors";
pub const QWEN21_VAE: &str = "qwen_image_2.1_vae_bf16.safetensors";
pub const QWEN21_PIXELS: f64 = 1024.0 * 1024.0;
pub const DEPTH_MODEL: &str = "depth_anything_3_mono_large.safetensors";
/// 抽骨架（OpenPose／DWPose 格式，ComfyUI 原生 SDPose；Comfy-Org/SDPose，放 models\checkpoints）
pub const SDPOSE_CKPT: &str = "sdpose_wholebody_fp16.safetensors";
/// MOSS-Image（Comfy-Org/MOSS-Image 的 ComfyUI 版檔案）：Qwen3.5-9B 文字編碼器（w4a8）＋FLUX.2 VAE。
/// 官方建議：Euler、sgm_uniform、50 步、CFG 3、shift 5，負面那一路跳過第 22 層（SkipLayerGuidanceDiTSimple）。
/// ComfyUI 0.38 原本沒有 MOSS：D 槽與安裝精靈都補了官方 commit e087d8f 的 9 個檔（resources\extras\comfy_moss）
pub const MOSS_TE: &str = "qwen3.5_9b_w4a8_gptq.safetensors";
/// MOSS 省記憶體的 4B 文字編碼器（beycanai/Qwen-LM，Apache-2.0）＋把 4B 特徵換到 9B 空間的投影轉接器（models\moss_encoder_adapters\），
/// 由繪匣附帶的 ComfyUI 節點 AnimaMOSS4BCLIPLoader 載入（resources\extras\anima_moss_nodes）。設定 moss_te＝"4b" 才用
pub const MOSS_TE_4B: &str = "Qwen3.5-4B_int8_convrot.safetensors";
pub const MOSS_4B_ADAPTER: &str = "moss_qwen35_4b_to_9b_1600_train.safetensors";
pub const MOSS_VAE: &str = "flux2-vae.safetensors";
/// Krea 2（Comfy-Org/Krea-2 的 ComfyUI 版檔案，ComfyUI 0.43 內建支援）：Qwen3-VL-4B 文字編碼器（CLIPLoader type krea2）＋
/// 跟 Anima 同一顆 qwen_image_vae。官方 Turbo 範本：8 步、CFG 1、euler＋simple、負面＝ConditioningZeroOut（shift 1.15 是模型預設，不用另外接）
pub const MMIMG_MAX_REFS: usize = 16;
pub const MMIMG_DEFAULT_MODEL: &str = "minimax_h3_image_pruned_w4a8.safetensors";
pub const MMIMG_VAE: &str = "minimax_h3_image_vae_fp16.safetensors";
pub const KREA2_TE: &str = "qwen3vl_4b_int8_convrot.safetensors";

/// 依模型家族產生第 n 張參考圖的提示詞標記（Qwen 2.1 用 `<imageN>`，MiniMax H3 Image 用 `<Picture N>`）
pub fn ref_tag(family: &str, n: usize) -> String {
    if family == crate::families::MMIMG {
        format!("<Picture {n}>")
    } else {
        format!("<image{n}>")
    }
}

/// 把外部或使用者提示詞裡的 `<imageN>` 以及獨立的 `图N`/`圖N` 轉成 MiniMax H3 Image 的 `<Picture N>`。
/// 帶詞界保護，避免誤傷「地圖2」「截圖1」「原圖1」「四視圖1」等複合名詞。
pub fn adapt_mmimage_refs(text: &str) -> String {
    use std::sync::OnceLock;
    static IMG_RE: OnceLock<regex::Regex> = OnceLock::new();
    static ZH_RE: OnceLock<regex::Regex> = OnceLock::new();
    let img_re = IMG_RE.get_or_init(|| regex::Regex::new(r"(?i)<image\s*(\d+)>").unwrap());
    let zh_re = ZH_RE.get_or_init(|| regex::Regex::new(r"[图圖]\s*(\d+)").unwrap());
    let step1 = img_re.replace_all(text, "<Picture $1>");
    const BAD_PREFIXES: &[&str] = &[
        "地", "截", "构", "構", "原", "视", "視", "缩", "縮", "插", "底", "草", "拼", "彩", "套",
        "设计", "設計", "设定", "設定", "参考", "參考", "背景", "流程", "示意", "平面", "建筑", "建築",
        "工程", "统计", "統計", "曲线", "曲線", "折线", "折線", "柱状", "柱狀", "圆饼", "圓餅", "线", "線",
    ];
    let mut out = String::with_capacity(step1.len());
    let mut last = 0usize;
    for cap in zh_re.captures_iter(&step1) {
        let m = cap.get(0).unwrap();
        let before = &step1[..m.start()];
        let is_compound = BAD_PREFIXES.iter().any(|p| before.ends_with(p));
        out.push_str(&step1[last..m.start()]);
        if is_compound {
            out.push_str(m.as_str());
        } else {
            out.push_str(&format!("<Picture {}>", &cap[1]));
        }
        last = m.end();
    }
    out.push_str(&step1[last..]);
    out
}

/// 單圖編輯給 MiniMax 時，若指令完全沒提到 `<Picture ` 則補上指向 `<Picture 1>` 的前綴
fn ensure_mm_picture1(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() || t.contains("<Picture ") {
        t.to_string()
    } else {
        format!("Edit <Picture 1>. {t}")
    }
}
/// SDXL 系（Illustrious、NoobAI、Pony）的預設負面詞：NoobAI-XL 1.1 官方說明的負面詞，拿掉 nsfw 和防獸人的那幾個
pub const SDXL_NEG: &str = "worst quality, old, early, low quality, lowres, signature, username, logo, bad hands, mutated hands";

/// SDXL 的加速版（Lightning、Hyper、Turbo、DMD2、LCM）：檔名看得出來就用 8 步、低 CFG
pub fn is_fast_sdxl(model: &str) -> bool {
    let m = model.to_lowercase();
    ["lightning", "hyper", "turbo", "dmd", "lcm"].iter().any(|k| m.contains(k))
}
const MOSS_SHIFT: f64 = 5.0;
const MOSS_SKIP_LAYERS: &str = "22";
const TILED_ABOVE: i64 = 2_400_000;
const TURBO_T: [f64; 6] = [1.0, 0.9375, 0.875, 0.75, 0.5, 0.25];
/// 官方 Qwen-Image-2.1-Turbo 的 8 步排程（Hugging Face model_index.json 的 sample_sigmas，不隨尺寸位移）
const OFFICIAL_TURBO_SIGMAS: [f64; 8] = [
    1.0, 0.978453, 0.95418, 0.926626, 0.89508, 0.845148, 0.704534, 0.414568,
];

pub fn is_turbo_lora(name: &str) -> bool {
    name.to_lowercase().contains("turbo")
}

/// 官方 Turbo：完整模型（qwen_image_2.1_turbo_*），或原版／微調模型掛官方 Turbo LoRA（qwen_image_2.1_turbo_lora_*）
pub fn is_official_qwen21_turbo(p: &Value) -> bool {
    let official = |n: &str| {
        let n = n.to_lowercase().replace('-', "_");
        n.contains("qwen_image_2.1_turbo")
    };
    if official(s(p.get("model"), "")) {
        return true;
    }
    match p.get("loras") {
        Some(Value::Array(ls)) => ls.iter().any(|l| {
            l.get("enabled").map(|v| truthy(Some(v))).unwrap_or(true)
                && f(l.get("strength"), 1.0) != 0.0
                && official(s(l.get("name"), ""))
        }),
        _ => false,
    }
}

/// 官方 Turbo 的排程；改圖（denoise < 1）從 denoise 開始、只留比它小的
pub fn official_turbo_sigmas(denoise: f64) -> String {
    let mut ts: Vec<f64> = if denoise >= 0.999 {
        OFFICIAL_TURBO_SIGMAS.to_vec()
    } else {
        std::iter::once(denoise)
            .chain(
                OFFICIAL_TURBO_SIGMAS
                    .iter()
                    .copied()
                    .filter(|t| *t < denoise),
            )
            .collect()
    };
    ts.push(0.0);
    ts.iter()
        .map(|t| format!("{t:.6}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn lllite(kind: &str) -> &'static str {
    match kind {
        "lineart" => "anima-lllite-lineart-1.safetensors",
        "scribble" => "anima-lllite-scribble-1.safetensors",
        "depth" => "anima-lllite-depth-1.safetensors",
        "pose" => "anima-lllite-pose-1.safetensors",
        "inpaint" => "anima-lllite-inpainting-v2.safetensors",
        _ => "anima-lllite-any-test-like-v2.safetensors",
    }
}

#[derive(Debug)]
pub struct GraphError(pub String);
impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
type R<T> = Result<T, GraphError>;
fn err<T>(m: impl Into<String>) -> R<T> {
    Err(GraphError(m.into()))
}

// ---------------------------------------------------------------- Python 行為的小工具（真值判斷在 py.rs）
/// Python float(v)
fn f(v: Option<&Value>, default: f64) -> f64 {
    match v {
        None => default,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        Some(Value::Bool(b)) => *b as i64 as f64,
        _ => default,
    }
}
/// Python int(v)（小數無條件捨去）
fn i(v: Option<&Value>, default: i64) -> i64 {
    match v {
        None => default,
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().map(|x| x.trunc() as i64).unwrap_or(default)),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        Some(Value::Bool(b)) => *b as i64,
        _ => default,
    }
}
fn s<'a>(v: Option<&'a Value>, default: &'a str) -> &'a str {
    v.and_then(|x| x.as_str()).unwrap_or(default)
}
/// p.get(k, default) 原樣傳下去（例如 sampler_name 不轉型）
fn raw(p: &Value, k: &str, default: Value) -> Value {
    p.get(k).cloned().unwrap_or(default)
}
/// MOSS 照指令改圖（2026-10-30）：編輯 LoRA 訓練時的參考圖版面，對應 comfy_moss 補丁 TextEncodeMOSSImageEdit 的 edit_format
const MOSS_EDIT_FORMATS: [&str; 3] = ["ai-toolkit", "ai-toolkit (kv_cache off)", "Moss-Image-Edit"];

/// p.moss_edit：沒開＝None；"auto"＝照掛的 LoRA 猜（chinoll 的 Moss-Image-Edit 用自己的版面，Civitai 上 ai-toolkit 訓練的用 ai-toolkit）
fn moss_edit_format(p: &Value) -> R<Option<String>> {
    let v = s(p.get("moss_edit"), "");
    if v.is_empty() || v == "off" || p.get("moss_edit") == Some(&Value::Bool(false)) {
        return Ok(None);
    }
    if v != "auto" {
        return if MOSS_EDIT_FORMATS.contains(&v) { Ok(Some(v.to_string())) } else { err(format!("不認得的 MOSS 編輯版面：{v}")) };
    }
    let chinoll = p["loras"].as_array().into_iter().flatten().filter(|l| l["enabled"] != false).any(|l| {
        let n = l["name"].as_str().unwrap_or("").to_lowercase();
        n.contains("chinoll") || n.contains("moss-image-edit") || n.contains("moss_image_edit")
    });
    Ok(Some(if chinoll { "Moss-Image-Edit" } else { "ai-toolkit" }.to_string()))
}

/// Python: `p.get(k) or default`
fn or_f(p: &Value, k: &str, default: f64) -> f64 {
    if truthy(p.get(k)) {
        f(p.get(k), default)
    } else {
        default
    }
}

pub fn snap(v: f64, step: i64, lo: i64) -> i64 {
    lo.max(((v / step as f64).round_ties_even() as i64) * step)
}

/// 保持比例，把 w×h 縮放到大約 pixels 個像素，並對齊 step（Qwen 2.1 要 32）
pub fn fit_pixels(w: i64, h: i64, pixels: f64, step: i64) -> (i64, i64) {
    let sc = (pixels / (w * h) as f64).powf(0.5);
    (
        snap(w as f64 * sc, step, 256),
        snap(h as f64 * sc, step, 256),
    )
}

/// {紅|藍|綠} → 隨機挑一個；可巢狀，每張圖各抽一次
pub fn expand_wildcards(text: &str, rng: &mut PyRandom) -> String {
    let re = regex::Regex::new(r"\{([^{}]*\|[^{}]*)\}").unwrap();
    let mut t = text.to_string();
    while let Some(c) = re.captures(&t) {
        let m = c.get(0).unwrap();
        let opts: Vec<&str> = c.get(1).unwrap().as_str().split('|').collect();
        let pick = rng.choice(&opts).trim().to_string();
        t = format!("{}{}{}", &t[..m.start()], pick, &t[m.end()..]);
    }
    t
}

/// 版面框最多幾個
pub const LAYOUT_MAX: usize = 16;

/// MOSS-Image 版面框（生成頁、導演台拉的框）。params.layout：
/// `[{"type": "character"|"object"|"text", "desc": "外觀描述", "text": "要畫的字（type=text 才用）", "box": [x0, y0, x1, y1]}]`，box 是 0～1、左上角原點。
/// 組成官方的排版 JSON（design\MOSS官方資料\moss_layout_skill.md）：畫面中心當原點的像素、yxyx、左上 floor 右下 ceil，
/// 跟 ComfyUI 的 MOSSImageLayoutPrompt 節點算法一樣，壓成一行當正面提示詞。只有 MOSS 吃這個格式。
pub struct Layout {
    caption: String,
    meta: Vec<String>,
    canvas: Option<&'static str>,
    palette: Vec<String>,
    items: Vec<LayoutItem>,
}

struct LayoutItem {
    kind: &'static str,
    desc: String,
    text: String,
    tags: Vec<String>,
    palette: Vec<String>,
    bbox: [f64; 4],
}

fn layout_strings(value: Option<&Value>, rng: &mut PyRandom) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|item| expand_wildcards(item, rng).trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

impl Layout {
    /// 沒有能用的框就 None（照一般提示詞畫）。描述裡的 {紅|藍} 在這裡抽（組 JSON 之前，免得 | 和大括號弄壞 JSON）
    pub fn from_params(p: &Value, caption: &str, rng: &mut PyRandom) -> Option<Layout> {
        let mut items = vec![];
        for b in p.get("layout")?.as_array()?.iter().take(LAYOUT_MAX) {
            let kind = match s(b.get("type"), "object") {
                "character" => "character",
                "text" => "text",
                _ => "object",
            };
            let Some(v) = b.get("box").and_then(Value::as_array).filter(|a| a.len() == 4) else {
                continue;
            };
            let v: Vec<f64> = v.iter().map(|x| x.as_f64().unwrap_or(0.0).clamp(0.0, 1.0)).collect();
            let (x0, x1, y0, y1) = (v[0].min(v[2]), v[0].max(v[2]), v[1].min(v[3]), v[1].max(v[3]));
            let desc = expand_wildcards(s(b.get("desc"), ""), rng).trim().to_string();
            let text = if kind == "text" { s(b.get("text"), "").to_string() } else { String::new() };
            if x1 - x0 < 0.01 || y1 - y0 < 0.01 || (desc.is_empty() && text.trim().is_empty()) {
                continue;
            }
            items.push(LayoutItem {
                kind,
                desc,
                text,
                tags: layout_strings(b.get("tags"), rng),
                palette: layout_strings(b.get("palette"), rng),
                bbox: [x0, y0, x1, y1],
            });
        }
        if items.is_empty() {
            return None;
        }
        let primary = p.get("layout_summary").and_then(Value::as_object);
        let fallback = p.get("summary").and_then(Value::as_object);
        let summary_field = |key| {
            primary.and_then(|m| m.get(key)).or_else(|| fallback.and_then(|m| m.get(key)))
        };
        let canvas = match summary_field("canvas").and_then(Value::as_str).map(str::trim) {
            Some("single") => Some("single"),
            Some("multi_panel") => Some("multi_panel"),
            Some("collage") => Some("collage"),
            _ => None,
        };
        Some(Layout {
            caption: caption.trim().to_string(),
            meta: layout_strings(summary_field("meta"), rng),
            canvas,
            palette: layout_strings(summary_field("palette"), rng),
            items,
        })
    }

    /// w×h 畫面的排版 JSON（寬高不寫進去，生圖時用同樣的寬高）
    pub fn prompt(&self, w: i64, h: i64) -> String {
        // 0～1 → 像素邊（先四捨五入到小數 9 位再 floor／ceil，跟節點一樣）→ 減掉中心
        let edge = |v: f64, n: i64, up: bool| {
            let x = (v * n as f64 * 1e9).round() / 1e9;
            (if up { x.ceil() } else { x.floor() }) as i64 - n / 2
        };
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                let [x0, y0, x1, y1] = item.bbox;
                let mut m = Map::new();
                m.insert("label".into(), json!(item.kind));
                // 官方規則 caption 不能空：只寫了字的文字框補一句
                m.insert("caption".into(), json!(if item.desc.is_empty() { "clear, legible lettering" } else { item.desc.as_str() }));
                if item.kind == "text" {
                    m.insert("text".into(), json!(item.text));
                }
                if !item.tags.is_empty() {
                    m.insert("tags".into(), json!(item.tags));
                }
                if !item.palette.is_empty() {
                    m.insert("palette".into(), json!(item.palette));
                }
                m.insert("bbox".into(), json!([edge(y0, h, false), edge(x0, w, false), edge(y1, h, true), edge(x1, w, true)]));
                Value::Object(m)
            })
            .collect();
        let caption = if self.caption.is_empty() {
            self.items.iter().map(|it| it.desc.as_str()).filter(|d| !d.is_empty()).collect::<Vec<_>>().join(", ")
        } else {
            self.caption.clone()
        };
        let mut summary = Map::new();
        if !self.meta.is_empty() {
            summary.insert("meta".into(), json!(self.meta));
        }
        if let Some(canvas) = self.canvas {
            summary.insert("canvas".into(), json!(canvas));
        }
        if !self.palette.is_empty() {
            summary.insert("palette".into(), json!(self.palette));
        }
        summary.insert("caption".into(), json!(caption));
        json!({"summary": summary, "items": items, "bbox_coord_space": "abs_centralized", "bbox_format": "yxyx"}).to_string()
    }
}

/// Qwen 2.1 Turbo（Noct Q Turbo 內建 Viggle v0.2.1）的 6 步排程，依尺寸位移
pub fn turbo_sigmas(w: i64, h: i64, denoise: f64) -> String {
    let mu = 0.5 + 0.4 * ((w as f64 / 16.0) * (h as f64 / 16.0) - 256.0) / 7936.0;
    let ts: Vec<f64> = if denoise >= 0.999 {
        TURBO_T.to_vec()
    } else {
        std::iter::once(denoise)
            .chain(TURBO_T.iter().copied().filter(|t| *t < denoise))
            .collect()
    };
    let e = mu.exp();
    let mut parts: Vec<String> = ts
        .iter()
        .map(|t| format!("{:.6}", e / (e + 1.0 / t - 1.0)))
        .collect();
    parts.push("0.000000".into());
    parts.join(", ")
}

// ---------------------------------------------------------------- 節點圖
pub type Ref = Value; // ["節點編號", 輸出序號]

pub struct Graph {
    pub family: String,
    pub nodes: Map<String, Value>,
    n: usize,
    clip: Option<Ref>,
    vae: Option<Ref>,
    /// MOSS 用 4B 文字編碼器（params.moss_te＝"4b"）
    moss_4b: bool,
    /// MiniMax H3 Image 指定文字編碼器（params.mm_te，空白用 crate::h3::TE）
    mm_te: Option<String>,
}

impl Graph {
    pub fn new(family: &str) -> Self {
        Graph {
            family: family.into(),
            nodes: Map::new(),
            n: 0,
            clip: None,
            vae: None,
            moss_4b: false,
            mm_te: None,
        }
    }
    fn q(&self) -> bool {
        self.family == "qwen21"
    }
    /// 一張圖裡換家族（一鍵脫衣：先 Qwen 再 Anima）：文字編碼器、VAE 要重新載入對應家族的
    fn switch_family(&mut self, fam: &str) {
        self.family = fam.into();
        self.clip = None;
        self.vae = None;
    }
    fn moss(&self) -> bool {
        self.family == "moss"
    }
    fn mmimage(&self) -> bool {
        self.family == crate::families::MMIMG
    }
    fn krea2(&self) -> bool {
        self.family == "krea2"
    }
    /// SDXL：整包 checkpoint，文字編碼器和 VAE 從 CheckpointLoaderSimple 拿（model_chain 設好 clip／vae）
    fn sdxl(&self) -> bool {
        self.family == "sdxl"
    }
    /// 一般 CLIPTextEncode 家族的負面條件。Krea 2 在 CFG ≤ 1（Turbo）時跟官方範本一樣把正面條件歸零當負面，省掉一次 4B 編碼
    fn neg_text(&mut self, pos: &Ref, negative: &str, cfg: f64) -> Ref {
        if self.krea2() && cfg <= 1.0 {
            return self.add("ConditioningZeroOut", json!({"conditioning": pos}));
        }
        self.text(negative)
    }
    /// 空白 latent：MOSS 用 FLUX.2 的 latent 空間（128 通道），其他家族用一般的
    fn empty_latent(&mut self, w: i64, h: i64) -> Ref {
        let cls = if self.moss() { "EmptyFlux2LatentImage" } else { "EmptyLatentImage" };
        self.add(cls, json!({"width": w, "height": h, "batch_size": 1}))
    }
    pub fn add(&mut self, cls: &str, inputs: Value) -> Ref {
        self.n += 1;
        let id = self.n.to_string();
        self.nodes
            .insert(id.clone(), json!({"class_type": cls, "inputs": inputs}));
        json!([id, 0])
    }
    fn slot(r: &Ref, i: i64) -> Ref {
        json!([r[0].clone(), i])
    }
    fn clip(&mut self) -> Ref {
        if self.clip.is_none() {
            let c = if self.q() {
                self.add(
                    "CLIPLoader",
                    json!({"clip_name": QWEN21_TE, "type": "qwen_image", "device": "default"}),
                )
            } else if self.moss() && self.moss_4b {
                self.add("AnimaMOSS4BCLIPLoader", json!({"clip_name": MOSS_TE_4B, "adapter_name": MOSS_4B_ADAPTER}))
            } else if self.moss() {
                self.add(
                    "CLIPLoader",
                    json!({"clip_name": MOSS_TE, "type": "moss_image", "device": "default"}),
                )
            } else if self.krea2() {
                self.add(
                    "CLIPLoader",
                    json!({"clip_name": KREA2_TE, "type": "krea2", "device": "default"}),
                )
            } else if self.mmimage() {
                let te = self.mm_te.as_deref().filter(|s| !s.is_empty()).unwrap_or(crate::h3::TE);
                self.add(
                    "CLIPLoader",
                    json!({"clip_name": te, "type": "minimax", "device": "default"}),
                )
            } else {
                self.add(
                    "CLIPLoader",
                    json!({"clip_name": CLIP, "type": "stable_diffusion", "device": "default"}),
                )
            };
            self.clip = Some(c);
        }
        self.clip.clone().unwrap()
    }
    fn vae(&mut self) -> Ref {
        if self.vae.is_none() {
            let name = if self.q() {
                QWEN21_VAE
            } else if self.moss() {
                MOSS_VAE
            } else if self.mmimage() {
                MMIMG_VAE
            } else {
                VAE
            };
            self.vae = Some(self.add("VAELoader", json!({"vae_name": name})));
        }
        self.vae.clone().unwrap()
    }
    fn text(&mut self, t: &str) -> Ref {
        let clip = self.clip();
        self.add("CLIPTextEncode", json!({"text": t, "clip": clip}))
    }
    /// Qwen 2.1：正負提示詞一起編碼，參考圖同時給視覺編碼器與 VAE。回傳 (正向, 負向, 空 latent)
    fn qtext(&mut self, prompt: &str, negative: &str, images: &[Ref]) -> (Ref, Ref, Ref) {
        // 順序跟 Python 一樣：先準備 kw（有圖才建 VAE），呼叫 add 時才求 clip()，所以 VAE 節點編號在 CLIP 前面
        let mut kw = Map::new();
        for (k, im) in images.iter().enumerate() {
            kw.insert(format!("images.image_{}", k + 1), im.clone());
        }
        if !images.is_empty() {
            let v = self.vae();
            kw.insert("vae".into(), v);
        }
        let clip = self.clip();
        kw.insert("clip".into(), clip);
        kw.insert("prompt".into(), json!(prompt));
        kw.insert("negative_prompt".into(), json!(negative));
        kw.insert("resolution".into(), json!(0));
        let r = self.add("TextEncodeQwenImage21", Value::Object(kw));
        (r.clone(), Self::slot(&r, 1), Self::slot(&r, 2))
    }
    fn encode(&mut self, image: &Ref, w: i64, h: i64) -> Ref {
        let vae = self.vae();
        if w * h > TILED_ABOVE {
            return self.add("VAEEncodeTiled", json!({"pixels": image, "vae": vae, "tile_size": 512, "overlap": 64, "temporal_size": 64, "temporal_overlap": 8}));
        }
        self.add("VAEEncode", json!({"pixels": image, "vae": vae}))
    }
    fn decode(&mut self, latent: &Ref, w: i64, h: i64) -> Ref {
        let vae = self.vae();
        if w * h > TILED_ABOVE {
            return self.add("VAEDecodeTiled", json!({"samples": latent, "vae": vae, "tile_size": 512, "overlap": 64, "temporal_size": 64, "temporal_overlap": 8}));
        }
        self.add("VAEDecode", json!({"samples": latent, "vae": vae}))
    }
    fn scale(&mut self, image: &Ref, w: i64, h: i64, crop: &str, method: &str) -> Ref {
        self.add("ImageScale", json!({"image": image, "upscale_method": method, "width": w, "height": h, "crop": crop}))
    }
    fn mask_scale(&mut self, mask: &Ref, w: i64, h: i64) -> Ref {
        let img = self.add("MaskToImage", json!({"mask": mask}));
        let img = self.scale(&img, w, h, "disabled", "bilinear");
        self.add("ImageToMask", json!({"image": img, "channel": "red"}))
    }
    fn mask_blur(&mut self, mask: &Ref, radius: i64) -> Ref {
        if radius <= 0 {
            return mask.clone();
        }
        let img = self.add("MaskToImage", json!({"mask": mask}));
        let img = self.add("ImageBlur", json!({"image": img, "blur_radius": radius, "sigma": f64::max(0.5, radius as f64 / 2.0)}));
        self.add("ImageToMask", json!({"image": img, "channel": "red"}))
    }
}

/// LLLite：(patch 檔名, 控制圖, 遮罩, 強度, 開始, 結束)
type Lllite = (&'static str, Ref, Option<Ref>, f64, f64, f64);

fn model_chain(g: &mut Graph, p: &Value, lll: Vec<Lllite>) -> Ref {
    let model_name = s(p.get("model"), "");
    if g.sdxl() {
        return sdxl_chain(g, p);
    }
    let mut m = g.add(
        "UNETLoader",
        json!({"unet_name": p.get("model"), "weight_dtype": "default"}),
    );
    for (patch, img, mask, st, a, b) in lll {
        let pl = g.add("ModelPatchLoader", json!({"name": patch}));
        let mut kw = json!({"model": m, "model_patch": pl, "image": img, "strength": st, "start_percent": a, "end_percent": b});
        if let Some(mk) = mask {
            kw["mask"] = mk;
        }
        m = g.add("AnimaLLLiteApply", kw);
    }
    let tl = p.get("turbo_lora");
    if truthy(tl) && !model_name.to_lowercase().contains("turbo") && g.family == "anima" {
        let name = tl.and_then(|v| v.as_str()).unwrap_or(TURBO_LORA);
        m = g.add(
            "LoraLoaderModelOnly",
            json!({"model": m, "lora_name": name, "strength_model": 1.0}),
        );
    }
    if let Some(Value::Array(ls)) = p.get("loras") {
        for l in ls {
            let enabled = l.get("enabled").map(|v| truthy(Some(v))).unwrap_or(true);
            let st = f(l.get("strength"), 1.0);
            if enabled && st != 0.0 {
                m = g.add(
                    "LoraLoaderModelOnly",
                    json!({"model": m, "lora_name": l.get("name"), "strength_model": st}),
                );
            }
        }
    }
    if g.moss() {
        if let Some(a) = moss_control_adapter(p) {
            m = g.add("LoraLoaderModelOnly", json!({"model": m, "lora_name": a, "strength_model": 1.0}));
        }
    }
    // 姿勢參考圖（Qwen 2.1）：3D 人偶的深度圖＋骨架，各接一個 Fun ControlNet。
    // 2026-10-10 實測：強度各 0.55、只管前 80% 步數，第一人稱和身體貼合明顯變好（報告\Qwen-Image-2.1-Turbo資料_20261010.md）
    if g.family == "qwen21" {
        if let Some(pc) = p.get("pose_control").filter(|v| v.is_object()) {
            let st = f(pc.get("strength"), 0.55);
            let mut patch: Option<Ref> = None;
            for k in ["depth", "pose"] {
                let Some(file) = pc.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()) else {
                    continue;
                };
                let pl = patch
                    .get_or_insert_with(|| {
                        g.add("ModelPatchLoader", json!({"name": POSE_CONTROLNET}))
                    })
                    .clone();
                let img = g.add(
                    "LoadImage",
                    json!({"image": format!("{POSE_INPUT}/{file}")}),
                );
                let vae = g.vae();
                m = g.add(
                    "ZImageFunControlnet",
                    json!({"model": m, "model_patch": pl, "vae": vae, "strength": st, "image": img, "end_percent": 0.8}),
                );
            }
        }
    }
    if g.moss() {
        // 原生 MOSS 的時間步倍率是 1：不能用 ModelSamplingSD3（它固定乘 1000，2026-10-17 實測整張變雜訊），AuraFlow 這個是倍率 1
        m = g.add("ModelSamplingAuraFlow", json!({"model": m, "shift": MOSS_SHIFT}));
        // 官方：負面那一路跳過第 22 層（CFG 1 時沒有負面那一路，不用接）
        if f(p.get("cfg"), 0.0) > 1.0 {
            m = g.add(
                "SkipLayerGuidanceDiTSimple",
                json!({"model": m, "double_layers": "", "single_layers": MOSS_SKIP_LAYERS, "start_percent": 0.0, "end_percent": 1.0}),
            );
        }
    }
    m
}

/// SDXL：CheckpointLoaderSimple 一次給 MODEL／CLIP／VAE；LoRA 用 LoraLoader（連文字編碼器一起套，跟 A1111 一樣）。
/// clip skip 不用另外接：ComfyUI 的 SDXL 兩顆 CLIP 本來就取倒數第二層（等於 A1111 的 clip skip 2）
fn sdxl_chain(g: &mut Graph, p: &Value) -> Ref {
    let ck = g.add("CheckpointLoaderSimple", json!({"ckpt_name": p.get("model")}));
    let (mut m, mut clip) = (ck.clone(), Graph::slot(&ck, 1));
    g.vae = Some(Graph::slot(&ck, 2));
    if let Some(Value::Array(ls)) = p.get("loras") {
        for l in ls {
            let enabled = l.get("enabled").map(|v| truthy(Some(v))).unwrap_or(true);
            let st = f(l.get("strength"), 1.0);
            if enabled && st != 0.0 {
                let lo = g.add(
                    "LoraLoader",
                    json!({"model": m, "clip": clip, "lora_name": l.get("name"), "strength_model": st, "strength_clip": st}),
                );
                (m, clip) = (lo.clone(), Graph::slot(&lo, 1));
            }
        }
    }
    g.clip = Some(clip);
    m
}

// ---------------------------------------------------------------- 換家族改圖（兩段：Qwen 2.1 改 → Anima 精修 → 差異遮罩貼回）
// 「一鍵脫衣」用這套引擎，改圖指令和精修 tag 是外部內容補丁（feature_strip），這裡只有中性的 graph 機制。
pub const STRIP_QWEN: &str = "qwen_image_2.1_turbo_int8_convrot.safetensors";
pub const STRIP_REFINE: &str = "anima-turbo-v1.1.safetensors";
/// 輸出最多幾像素（原圖比這小就照原尺寸）
const STRIP_MAX_OUT: i64 = 2048 * 2048;

/// 一鍵脫衣、四視圖編輯的 Qwen 那段：生成頁選的是 Qwen 就照它（模型、步數、CFG、LoRA），不然用官方 Turbo、不掛 LoRA
fn edit_qwen_params(p: &Value, fam: &str) -> Value {
    let mut qp = if fam == "qwen21" {
        p.clone()
    } else {
        json!({"model": STRIP_QWEN, "cfg": 1, "sampler": "euler", "scheduler": "simple", "steps": 8, "loras": []})
    };
    qp["pose_control"] = Value::Null;
    qp
}

// ---------------------------------------------------------------- 四視圖編輯
// 來源：使用者給的 ComfyUI 工作流「Qwen21_四视图编辑_文字与服装参考_统一版」的保持要求（原文簡體，照抄）。
// 2026-10-18 實測（E:\AI\anima_ui\data\lora_train\jgdx_v2\showcase\agentimg\fourview_*.py）：官方 Turbo、參考圖約 1280×1280，
// 點名衣物改色（「红色西装外套」）、圖2 換衣、全裸都成功，五個視角同步改；只寫「上衣」兩個種子都沒改到。
const SHEET_KEEP: &str = "【保持要求】\n以图1为待编辑原图，仅执行上面的修改要求。保持图1的人物身份、脸部、发型、身体比例、肩宽、腰胯比例、四肢长度、体型、站姿、表情、视角和四视图排版一致。\n四个视图呈现同一角色、同一套服装，服装的颜色、材质、剪裁与装饰在各视角对应，侧面和背面结构合理延续正面。局部特写与全身视图同步修改。\n未要求修改的内容保持原样，尽量延续图1的画风、光照与背景。不改变画面中的视图数量，不添加额外人物。";
const SHEET_REF: &str = "【有图2时】\n图2仅提供本次指定的服装或材质参考；人物身份、身材、姿势与构图仍以图1为准。若图2包含模特，不采用该模特的脸、发型、身材或姿势。衣服适配图1原有身体比例，宽松衣物可自然改变服装外轮廓。";

/// 一鍵脫衣、四視圖編輯、差分是否改用 MiniMax H3 Image（engine == "mmimage"，或 "auto" 且生成頁選的是 mmimage）
fn edit_uses_mmimage(p: &Value, sub_key: &str, fam: &str) -> bool {
    let eng = p.get(sub_key).and_then(|v| v.get("engine")).and_then(Value::as_str).unwrap_or("auto");
    eng == crate::families::MMIMG || (eng == "auto" && fam == crate::families::MMIMG)
}

/// 一鍵脫衣、四視圖編輯、差分的 MiniMax H3 Image 那段：生成頁選的是 mmimage 就照它，不然用解析到的 mm_edit_model，強制關 mm_pe
fn edit_mmimage_params(p: &Value, fam: &str) -> Value {
    let mut mp = if fam == crate::families::MMIMG {
        p.clone()
    } else {
        let model = s(p.get("mm_edit_model"), MMIMG_DEFAULT_MODEL);
        json!({"model": model, "cfg": 1, "sampler": "euler", "scheduler": "simple", "steps": 6, "loras": []})
    };
    mp["mm_pe"] = json!("");
    mp["pose_control"] = Value::Null;
    mp
}

/// 四視圖編輯給 Qwen 的完整指令：本次修改要求＋保持要求（＋有圖2時的說明）
pub fn sheet_prompt(req: &str, has_ref: bool) -> String {
    sheet_prompt_for(req, has_ref, "qwen21")
}

/// 四視圖編輯完整指令（依家族決定圖1／圖2或 `<Picture 1>`／`<Picture 2>`）
pub fn sheet_prompt_for(req: &str, has_ref: bool, family: &str) -> String {
    let mut s = format!("【本次修改要求】\n{}\n\n{SHEET_KEEP}", req.trim());
    if has_ref {
        s.push_str("\n\n");
        s.push_str(SHEET_REF);
    }
    if family == crate::families::MMIMG {
        adapt_mmimage_refs(&s)
    } else {
        s
    }
}

// ---------------------------------------------------------------- 差分、擴圖、修臉、去背
/// 差分（表情、動作）：一張角色圖、一個修改要求，Qwen 2.1 改圖重繪 1.0；保持要求照四視圖那份的寫法（簡體）
const DIFF_KEEP: &str = "【保持要求】\n以图1为待编辑原图，仅执行上面的修改要求。保持图1的人物身份、脸部特征、发型、发色、瞳色、服装、体型、画风、光照、镜头和背景一致；未要求修改的部分保持原样。不添加额外人物，不改变画面比例。";

/// 差分給 Qwen 的完整指令
pub fn diff_prompt(req: &str) -> String {
    diff_prompt_for(req, "qwen21")
}

/// 差分完整指令（依家族決定圖1或 `<Picture 1>`）
pub fn diff_prompt_for(req: &str, family: &str) -> String {
    let s = format!("【本次修改要求】\n{}\n\n{DIFF_KEEP}", req.trim());
    if family == crate::families::MMIMG {
        adapt_mmimage_refs(&s)
    } else {
        s
    }
}

/// 擴圖時給 Qwen 2.1 的前綴（只進節點圖，不進圖庫記的提示詞）
const OUTPAINT_HINT: &str = "Extend the scene naturally into the empty gray borders so the whole frame is one seamless picture, with no gray areas or frames left.";
/// 擴圖時給 MiniMax H3 Image 的前綴（餵未補灰邊的原圖當 `<Picture 1>`，2026-11-28 探針 p2a 實測優於餵灰邊圖）
const MMIMG_OUTPAINT_HINT: &str = "Extend <Picture 1> seamlessly outward into the surrounding canvas so the entire frame forms one continuous, natural scene matching its lighting and style.";

/// 修臉／修手用的偵測模型（ComfyUI 0.38 原生 SAM3_Detect；Comfy-Org/sam3.1，放 models\checkpoints）
pub const SAM3_CKPT: &str = "sam3.1_multiplex_fp16.safetensors";
/// SAM3 找每顆頭的詞：後面的「:8」＝最多 8 個（ComfyUI 的 SAM3 詞不加 :N 只回分數最高的 1 個，2026-11-17 查 sam3_clip._parse_prompts）
pub const HEAD_PROMPT: &str = "head:8";
/// SAM3 每個詞最多找幾個（修臉修手、預演台找人、漫畫找頭）
pub const DETECT_MAX: u32 = 8;
/// 去背模型（ComfyUI 0.38 原生 RemoveBackground；Comfy-Org/BiRefNet，放 models\background_removal）
pub const BG_MODEL: &str = "birefnet.safetensors";

/// SAM3 偵測：每個詞（face、hand…）各找一次，每個物件一張黑白遮罩（原圖尺寸、分數高的在前），
/// 存成 temp 的圖（PreviewImage，不進 output、不會被收進圖庫）
pub fn detect_graph(image: &str, words: &[&str], threshold: f64) -> Map<String, Value> {
    let mut g = Graph::new("anima");
    let ck = g.add("CheckpointLoaderSimple", json!({"ckpt_name": SAM3_CKPT}));
    let li = g.add("LoadImage", json!({"image": image}));
    for w in words {
        // 沒寫「:N」的詞 SAM3 只回分數最高的 1 個（ComfyUI sam3_clip._parse_prompts），一律補上限
        let w = if w.contains(':') { w.to_string() } else { format!("{w}:{DETECT_MAX}") };
        let cond = g.add("CLIPTextEncode", json!({"text": w, "clip": Graph::slot(&ck, 1)}));
        let det = g.add(
            "SAM3_Detect",
            json!({"model": ck, "image": li, "conditioning": cond, "threshold": threshold.clamp(0.05, 0.95), "refine_iterations": 2, "individual_masks": true}),
        );
        let mi = g.add("MaskToImage", json!({"mask": det}));
        g.add("PreviewImage", json!({"images": mi}));
    }
    g.nodes
}

/// 一張黑白遮罩圖的白色外框（亮度 > 127）；用在 SAM3 讀回來的遮罩
pub fn bbox_of(img: &image::GrayImage) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[0] > 127 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x0 < x1).then_some((x0, y0, x1, y1))
}

/// Qwen 2.1 Fun ControlNet（姿勢參考圖用）與參考圖在 ComfyUI input 底下的資料夾（MOSS 也用同一個資料夾的深度圖）
pub const POSE_CONTROLNET: &str = "qwen_image_2.1_fun_controlnet_union_int8_convrot.safetensors";
pub const POSE_INPUT: &str = "anima_pose";
/// MOSS 色彩／深度控制的轉接器（darask0/MOSS-Image-color-depth-control，Apache-2.0）：一般 LoRA 載入器掛在模型上，
/// 圖由 ComfyUI 0.43 內建的 MOSSImageControl 送進條件。精靈清單放在 loras\moss\
pub const MOSS_CONTROL_LORA: &str = "moss\\moss-color-depth-control.safetensors";

/// MOSS 色彩／深度控制要掛的轉接器（moss_control 開著，或姿勢參考圖帶了 adapter）：沒有＝None
fn moss_control_adapter(p: &Value) -> Option<String> {
    let mc = p.get("moss_control").filter(|c| truthy(Some(c)) && truthy(c.get("enabled")) && truthy(c.get("image")));
    let pose = p.get("pose_control").and_then(|pc| pc.get("adapter")).and_then(Value::as_str).filter(|a| !a.is_empty());
    match (mc, pose) {
        (Some(c), _) => Some(c.get("adapter").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or(MOSS_CONTROL_LORA).to_string()),
        (None, Some(a)) => Some(a.to_string()),
        _ => None,
    }
}

/// MOSS 色彩／深度控制：參考圖 → 深度圖（DA3，近＝白，跟 Anima 深度控制同一套）＋色塊圖（模糊後減成幾種顏色：顏色只當區塊編號）→
/// MOSSImageControl 包住正負條件。官方建議：真實照片的圖 end 1.0；3D 人偶這類合成圖 0.5（前一半步數定構圖，後面讓模型畫表面）
fn moss_control(g: &mut Graph, p: &Value, pos: Ref, neg: Ref, w: i64, h: i64) -> (Ref, Ref) {
    let empty = json!({});
    let pose_depth = p.get("pose_control").filter(|pc| pc.get("adapter").and_then(Value::as_str).is_some_and(|a| !a.is_empty()))
        .and_then(|pc| pc.get("depth")).and_then(Value::as_str).filter(|x| !x.is_empty());
    let mc = p.get("moss_control").filter(|c| truthy(Some(c)) && truthy(c.get("enabled")) && truthy(c.get("image")));
    let (mut kw, depth_img, color_img) = match (mc, pose_depth) {
        (Some(c), _) => {
            let want = |k: &str| c.get(k).map(|v| truthy(Some(v))).unwrap_or(true);
            let depth = want("depth").then(|| control_image(g, &json!({"image": c["image"], "preprocess": "depth"}), w, h));
            let color = want("color").then(|| {
                let li = g.add("LoadImage", json!({"image": c["image"]}));
                let src = g.scale(&li, w, h, "center", "lanczos");
                let bl = g.add("ImageBlur", json!({"image": src, "blur_radius": 15, "sigma": 6.0}));
                g.add("ImageQuantize", json!({"image": bl, "colors": i(c.get("colors"), 8).clamp(2, 32), "dither": "none"}))
            });
            (json!({"strength": f(c.get("strength"), 1.0), "start_percent": 0.0, "end_percent": f(c.get("end"), 1.0)}), depth, color)
        }
        (None, Some(file)) => {
            let li = g.add("LoadImage", json!({"image": format!("{POSE_INPUT}/{file}")}));
            let pc = p.get("pose_control").unwrap_or(&empty);
            let depth = Some(g.scale(&li, w, h, "center", "lanczos"));
            (json!({"strength": f(pc.get("strength"), 1.0), "start_percent": 0.0, "end_percent": f(pc.get("end"), 0.5)}), depth, None)
        }
        (None, None) => return (pos, neg),
    };
    let vae = g.vae();
    kw["positive"] = pos;
    kw["negative"] = neg;
    kw["vae"] = vae;
    if let Some(d) = depth_img {
        kw["depth_map"] = d;
    }
    if let Some(c) = color_img {
        kw["color_map"] = c;
    }
    let ctl = g.add("MOSSImageControl", kw);
    (Graph::slot(&ctl, 0), Graph::slot(&ctl, 1))
}

#[allow(clippy::too_many_arguments)]
fn sample(
    g: &mut Graph,
    model: &Ref,
    pos: &Ref,
    neg: &Ref,
    latent: &Ref,
    p: &Value,
    seed: i64,
    denoise: f64,
    steps: Option<&Value>,
    size: (i64, i64),
) -> Ref {
    let model_name = s(p.get("model"), "").to_lowercase();
    let official = g.family == "qwen21" && is_official_qwen21_turbo(p);
    if official || (g.family == "qwen21" && model_name.contains("turbo")) {
        // 蒸餾模型對排程很敏感：官方 Turbo 用官方的 8 步，Noct Q Turbo（Viggle）用作者的 ManualSigmas
        let guider = g.add(
            "CFGGuider",
            json!({"model": model, "positive": pos, "negative": neg, "cfg": f(p.get("cfg"), 0.0)}),
        );
        let noise = g.add("RandomNoise", json!({"noise_seed": seed}));
        let sampler = g.add(
            "KSamplerSelect",
            json!({"sampler_name": raw(p, "sampler", json!("euler"))}),
        );
        let sigmas = g.add(
            "ManualSigmas",
            json!({"sigmas": if official { official_turbo_sigmas(denoise) } else { turbo_sigmas(size.0, size.1, denoise) }}),
        );
        return g.add("SamplerCustomAdvanced", json!({"noise": noise, "guider": guider, "sampler": sampler, "sigmas": sigmas, "latent_image": latent}));
    }
    let st = if truthy(steps) {
        i(steps, 0)
    } else {
        i(p.get("steps"), 0)
    };
    g.add("KSampler", json!({
        "model": model, "positive": pos, "negative": neg, "latent_image": latent, "seed": seed, "steps": st,
        "cfg": f(p.get("cfg"), 0.0), "sampler_name": raw(p, "sampler", json!("euler")),
        "scheduler": raw(p, "scheduler", json!("simple")), "denoise": denoise,
    }))
}

fn control_image(g: &mut Graph, c: &Value, w: i64, h: i64) -> Ref {
    let mut img = g.add("LoadImage", json!({"image": c.get("image")}));
    match s(c.get("preprocess"), "none") {
        "canny" => {
            img = g.add("Canny", json!({"image": img, "low_threshold": f(c.get("low"), 0.17), "high_threshold": f(c.get("high"), 0.45)}));
        }
        "depth" => {
            let dm = g.add(
                "LoadDA3Model",
                json!({"model_name": DEPTH_MODEL, "weight_dtype": "default"}),
            );
            let geo = g.add("DA3Inference", json!({"da3_model": dm, "image": img, "resolution": 504, "resize_method": "upper_bound_resize", "mode": "mono"}));
            img = g.add("DA3Render", json!({"da3_geometry": geo, "output": "depth", "output.normalization": "v2_style", "output.apply_sky_clip": false}));
        }
        "pose" => {
            // 照 ComfyUI 官方藍圖「Image to Pose Map (SDPose Multi-Person)」：先框人、每個框各抽一次骨架，再畫成 DWPose 那種彩色骨架圖。
            // 藍圖的框人用 RT-DETR，這裡改用修臉已經在用的 SAM3（少下載一個模型）；一個人都沒框到時 SDPose 自己改抽整張圖。
            // 不畫腳尖：Anima 的 pose LLLite 是拿 easy_dwpose 的圖訓練的，只有身體、手、臉。
            let sam = g.add("CheckpointLoaderSimple", json!({"ckpt_name": SAM3_CKPT}));
            let who = g.add("CLIPTextEncode", json!({"text": "person", "clip": Graph::slot(&sam, 1)}));
            let det = g.add("SAM3_Detect", json!({"model": sam, "image": img, "conditioning": who, "threshold": 0.4, "refine_iterations": 0, "individual_masks": true}));
            let sp = g.add("CheckpointLoaderSimple", json!({"ckpt_name": SDPOSE_CKPT}));
            let kp = g.add("SDPoseKeypointExtractor", json!({"model": sp, "vae": Graph::slot(&sp, 2), "image": img, "batch_size": 1, "bboxes": Graph::slot(&det, 1)}));
            img = g.add("SDPoseDrawKeypoints", json!({"keypoints": kp, "draw_body": true, "draw_hands": true, "draw_face": true, "draw_feet": false,
                                                       "stick_width": 4, "face_point_size": 2, "score_threshold": 0.5, "draw_head": true}));
        }
        _ => {}
    }
    if truthy(c.get("invert")) {
        img = g.add("ImageInvert", json!({"image": img}));
    }
    g.scale(&img, w, h, "center", "lanczos")
}

fn control_list(g: &mut Graph, p: &Value, w: i64, h: i64) -> Vec<Lllite> {
    if g.family != "anima" {
        return vec![];
    }
    // 構圖控制：一張（物件）或疊好幾張（陣列，例如深度＋骨架），依序接在模型上
    let items = match p.get("control") {
        Some(Value::Array(a)) => a.clone(),
        Some(c) => vec![c.clone()],
        None => vec![],
    };
    let mut out = vec![];
    for c in items.iter().filter(|c| truthy(Some(c)) && truthy(c.get("enabled")) && truthy(c.get("image"))) {
        let img = control_image(g, c, w, h);
        out.push((
            lllite(s(c.get("type"), "any")),
            img,
            None,
            f(c.get("strength"), 1.0),
            f(c.get("start"), 0.0),
            f(c.get("end"), 1.0),
        ));
    }
    // 體位庫的姿勢參考圖（3D 人偶的深度圖＋骨架）：深度 LLLite 鎖住兩個人的輪廓和前後，姿勢 LLLite 分出哪隻是左手右腳。
    // 2026-10-28 實測：只掛骨架太弱（作者也說 pose 那顆比較弱），深度圖才拉得動雙人體位；Anima 熟的體位（背後式）不掛反而最好
    if let Some(pc) = p.get("pose_control").filter(|v| v.is_object()) {
        for (k, st, end) in [("depth", f(pc.get("depth_strength"), 0.8), f(pc.get("depth_end"), 0.6)),
                             ("pose", f(pc.get("pose_strength"), 1.0), f(pc.get("pose_end"), 1.0))] {
            let Some(file) = pc.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()) else {
                continue;
            };
            let li = g.add("LoadImage", json!({"image": format!("{POSE_INPUT}/{file}")}));
            let img = g.scale(&li, w, h, "center", "lanczos");
            out.push((lllite(k), img, None, st, 0.0, end));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn hires(
    g: &mut Graph,
    image: Ref,
    p: &Value,
    model: &Ref,
    pos: &Ref,
    neg: &Ref,
    seed: i64,
    w: i64,
    h: i64,
    lay: Option<&Layout>,
    negative: &str,
) -> (Ref, i64, i64) {
    let empty = json!({});
    let hf = p.get("hires").filter(|v| truthy(Some(v))).unwrap_or(&empty);
    if !truthy(hf.get("enabled")) {
        return (image, w, h);
    }
    let st = if g.family == "qwen21" { 32 } else { 16 };
    let sc = f(hf.get("scale"), 1.5);
    let (ww, hh) = (snap(w as f64 * sc, st, 256), snap(h as f64 * sc, st, 256));
    // 版面框的座標是像素：放大後要照新尺寸重算一次。
    // 重算出來的是全新的正面條件：開著 MOSS 色彩／深度控制時要照新尺寸重新包一次，
    // 不然第二段變成「正面沒控制、負面還帶著第一段的控制」（2026-11-13 審查）
    let (pos, neg) = &match lay {
        Some(l) => {
            let t = g.text(&l.prompt(ww, hh));
            if moss_control_adapter(p).is_some() {
                let n = g.neg_text(&t, negative, f(p.get("cfg"), 0.0));
                moss_control(g, p, t, n, ww, hh)
            } else {
                (t, neg.clone())
            }
        }
        None => (pos.clone(), neg.clone()),
    };
    let up = g.add(
        "UpscaleModelLoader",
        json!({"model_name": raw(hf, "upscaler", json!("4x-AnimeSharp.safetensors"))}),
    );
    let img = g.add(
        "ImageUpscaleWithModel",
        json!({"upscale_model": up, "image": image}),
    );
    let img = g.scale(&img, ww, hh, "disabled", "lanczos");
    let lat = g.encode(&img, ww, hh);
    let smp = sample(
        g,
        model,
        pos,
        neg,
        &lat,
        p,
        seed + 1,
        f(hf.get("denoise"), 0.4),
        hf.get("steps"),
        (ww, hh),
    );
    (g.decode(&smp, ww, hh), ww, hh)
}

pub(crate) fn image_size(path: &Path) -> R<(i64, i64)> {
    image::image_dimensions(path)
        .map(|(w, h)| (w as i64, h as i64))
        .map_err(|_| {
            GraphError(format!(
                "找不到輸入圖片：{}",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            ))
        })
}

/// 遮罩的白色區域外框 (x0, y0, x1, y1)，跟 PIL getbbox 一樣右下角不含；沒塗東西回傳 None
fn mask_bbox(path: &Path) -> R<Option<(i64, i64, i64, i64)>> {
    let img = image::open(path).map_err(|_| GraphError("讀不到遮罩圖".into()))?;
    let rgb = img.to_rgb8();
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, -1i64, -1i64);
    for (x, y, px) in rgb.enumerate_pixels() {
        // PIL convert("L")：L = (R*19595 + G*38470 + B*7471 + 0x8000) >> 16
        let l = (px[0] as u32 * 19595 + px[1] as u32 * 38470 + px[2] as u32 * 7471 + 0x8000) >> 16;
        if l > 127 {
            let (x, y) = (x as i64, y as i64);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    Ok(if x1 < 0 {
        None
    } else {
        Some((x0, y0, x1 + 1, y1 + 1))
    })
}

/// MiniMax H3 Image 取樣：denoise == 1.0 走標準 6 步 KSampler；denoise < 1.0 改走 KSamplerAdvanced 精準切在 6 步 PDD 網格上
#[allow(clippy::too_many_arguments)]
fn mmimage_sample(g: &mut Graph, model: &Ref, positive: &Ref, negative: &Ref, latent: &Ref, p: &Value, seed: i64, denoise: f64) -> Ref {
    let steps = i(p.get("steps"), 6).max(1);
    let cfg = f(p.get("cfg"), 1.0);
    let sampler = s(p.get("sampler"), "euler");
    let scheduler = s(p.get("scheduler"), "simple");
    if denoise >= 0.999 {
        g.add("KSampler", json!({
            "model": model, "positive": positive, "negative": negative,
            "latent_image": latent, "seed": seed, "steps": steps, "cfg": cfg,
            "sampler_name": sampler, "scheduler": scheduler, "denoise": 1.0,
        }))
    } else {
        let start_at = (((1.0 - denoise.clamp(0.05, 1.0)) * steps as f64).round() as i64).clamp(0, steps - 1);
        g.add("KSamplerAdvanced", json!({
            "add_noise": "enable", "noise_seed": seed, "steps": steps, "cfg": cfg,
            "sampler_name": sampler, "scheduler": scheduler,
            "start_at_step": start_at, "end_at_step": steps,
            "return_with_leftover_noise": "disable",
            "model": model, "positive": positive, "negative": negative, "latent_image": latent,
        }))
    }
}

/// MiniMax H3 Image 解碼：照 vae_tiled 決定用 VAEDecodeTiled 或 VAEDecode
fn mmimage_decode(g: &mut Graph, samples: &Ref, p: &Value) -> Ref {
    let vae = g.vae();
    let tile = i(p.get("vae_tiled"), 0);
    if tile > 0 {
        g.add("VAEDecodeTiled", json!({"samples": samples, "vae": vae, "tile_size": tile, "overlap": 64, "temporal_size": 64, "temporal_overlap": 8}))
    } else {
        g.add("VAEDecode", json!({"samples": samples, "vae": vae}))
    }
}

/// MiniMax H3 Image 共用條件編碼：回傳 (positive, negative, empty_latent)
#[allow(clippy::too_many_arguments)]
fn mmimage_cond(g: &mut Graph, p: &Value, prompt: &str, images: &[Ref], width: Value, height: Value, allow_pe: bool) -> (Ref, Ref, Ref) {
    let clip = g.clip();
    let vae = g.vae();
    let mut cond = json!({"clip": clip, "vae": vae, "width": width, "height": height, "prompt": prompt});
    for (index, img) in images.iter().enumerate() {
        cond[format!("images.image_{}", index + 1)] = img.clone();
    }
    let edit = !images.is_empty();
    if allow_pe {
        if let Some(pe) = p["mm_pe"].as_str().filter(|s| !s.is_empty()) {
            let (system, instruction) = if edit {
                (include_str!("../../resources/share/mmimage_pe_edit.txt"), include_str!("../../resources/share/mmimage_instruction_edit.txt"))
            } else {
                (include_str!("../../resources/share/mmimage_pe_t2i.txt"), include_str!("../../resources/share/mmimage_instruction_t2i.txt"))
            };
            let pe_clip = g.add("CLIPLoader", json!({"clip_name": pe, "type": "minimax", "device": "default"}));
            let mut inputs = json!({"clip": pe_clip, "system_prompt": system, "prompt": instruction.replace("{input text}", prompt),
                "max_length": if edit { 8196 } else { 8192 }, "sampling_mode": "on", "sampling_mode.temperature": 1.0,
                "sampling_mode.top_k": 20, "sampling_mode.top_p": 0.95, "sampling_mode.min_p": 0.0,
                "sampling_mode.repetition_penalty": 1.0, "sampling_mode.presence_penalty": 0.0, "sampling_mode.seed": 0,
                "thinking": true, "use_default_template": true, "mtp": "auto"});
            for (index, img) in images.iter().enumerate() {
                inputs[format!("images.image{}", index + 1)] = img.clone();
            }
            let generated = g.add("TextGenerateV2", inputs);
            cond["prompt"] = g.add("JsonExtractString", json!({"json_string": generated, "key": "expanded_caption"}));
        }
    }
    let positive = g.add("MiniMaxH3ImageConditioning", cond);
    let negative = g.add("ConditioningZeroOut", json!({"conditioning": positive}));
    let latent = Graph::slot(&positive, 1);
    (positive, negative, latent)
}

/// ComfyUI v0.46.2 的 image_minimax_h3_image_t2i／edit 共用同一組條件與取樣節點（涵蓋 txt2img、img2img、multiref）。
fn mmimage_graph(g: &mut Graph, p: &Value, input_dir: &Path, mode: &str, prompt: &str, seed: i64) -> R<(Ref, i64, i64)> {
    let model = model_chain(g, p, vec![]);
    let inp = |name: &Value| input_dir.join(name.as_str().unwrap_or(""));
    match mode {
        "txt2img" => {
            let (w, h) = (snap(f(p.get("width"), 2048.0), 32, 32), snap(f(p.get("height"), 2048.0), 32, 32));
            let (pos, neg, lat) = mmimage_cond(g, p, prompt, &[], json!(w), json!(h), true);
            let samples = mmimage_sample(g, &model, &pos, &neg, &lat, p, seed, 1.0);
            let image = mmimage_decode(g, &samples, p);
            Ok((image, w, h))
        }
        "img2img" => {
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let max_px = or_f(p, "mm_max_pixels", or_f(p, "pixels", 1280.0 * 1280.0));
            let (w, h) = if s(p.get("size_mode"), "auto") == "auto" {
                fit_pixels(ow, oh, max_px, 32)
            } else {
                (snap(f(p.get("width"), 2048.0), 32, 256), snap(f(p.get("height"), 2048.0), 32, 256))
            };
            let li = g.add("LoadImage", json!({"image": init}));
            let src = g.scale(&li, w, h, "center", "lanczos");
            let denoise = f(p.get("denoise"), 1.0).clamp(0.05, 1.0);
            let mp = adapt_mmimage_refs(prompt);
            let (pos, neg, empty_lat) = mmimage_cond(g, p, &mp, std::slice::from_ref(&src), json!(w), json!(h), true);
            let lat = if denoise >= 0.999 { empty_lat } else { g.encode(&src, w, h) };
            let samples = mmimage_sample(g, &model, &pos, &neg, &lat, p, seed, denoise);
            let image = mmimage_decode(g, &samples, p);
            Ok((image, w, h))
        }
        "multiref" => {
            let refs = p["refs"].as_array().map(Vec::as_slice).unwrap_or_default();
            if refs.is_empty() || refs.len() > MMIMG_MAX_REFS {
                return err(format!("MiniMax H3 Image 多圖參考需要 1～{MMIMG_MAX_REFS} 張圖，目前有 {} 張", refs.len()));
            }
            let max_px = p.get("mm_max_pixels").and_then(Value::as_f64).filter(|v| *v > 0.0);
            let auto = s(p.get("size_mode"), "auto") == "auto";
            let (mut w, mut h) = (snap(f(p.get("width"), 2048.0), 32, 32), snap(f(p.get("height"), 2048.0), 32, 32));
            let (mut cond_w, mut cond_h) = (json!(w), json!(h));
            let mut images = vec![];
            for (index, r) in refs.iter().enumerate() {
                let Some(name) = r["image"].as_str().filter(|s| !s.is_empty()) else {
                    return err(format!("第 {} 張參考圖沒有檔名", index + 1));
                };
                let mut img = g.add("LoadImage", json!({"image": name}));
                if index == 0 {
                    if let Some(cap_px) = max_px {
                        let (rw, rh) = image_size(&inp(&r["image"]))
                            .or_else(|_| {
                                let (rw, rh) = (i(r.get("width"), 0), i(r.get("height"), 0));
                                if rw > 0 && rh > 0 { Ok((rw, rh)) } else { err("無法得知參考圖尺寸") }
                            })?;
                        if auto {
                            let (tw, th) = fit_pixels(rw, rh, cap_px, 32);
                            (w, h) = (tw, th);
                            (cond_w, cond_h) = (json!(w), json!(h));
                        }
                        img = g.scale(&img, w, h, "center", "lanczos");
                    } else {
                        // 手動多圖參考：沿用官方 4MP 對齊 32 節點鏈（並從硬碟 fallback 讀真實尺寸，避免 DB 記 0×0）
                        img = g.add("ResizeImageMaskNode", json!({"input": img, "resize_type": "scale total pixels", "resize_type.megapixels": 4, "scale_method": "area"}));
                        img = g.add("ResizeImageMaskNode", json!({"input": img, "resize_type": "scale to multiple", "resize_type.multiple": 32, "scale_method": "area"}));
                        if auto {
                            let size = g.add("GetImageSize", json!({"image": img}));
                            cond_w = size.clone();
                            cond_h = Graph::slot(&size, 1);
                            let (rw, rh) = (f(r.get("width"), 0.0), f(r.get("height"), 0.0));
                            let (rw, rh) = if rw > 0.0 && rh > 0.0 {
                                (rw, rh)
                            } else if let Ok((iw, ih)) = image_size(&inp(&r["image"])) {
                                (iw as f64, ih as f64)
                            } else {
                                (0.0, 0.0)
                            };
                            if rw > 0.0 && rh > 0.0 {
                                let scale = (4194304.0 / (rw * rh)).sqrt();
                                (w, h) = ((rw * scale).round() as i64 / 32 * 32, (rh * scale).round() as i64 / 32 * 32);
                            } else {
                                (w, h) = (0, 0);
                            }
                        }
                    }
                }
                images.push(img);
            }
            let mp = adapt_mmimage_refs(prompt);
            let (pos, neg, lat) = mmimage_cond(g, p, &mp, &images, cond_w, cond_h, true);
            let samples = mmimage_sample(g, &model, &pos, &neg, &lat, p, seed, 1.0);
            let image = mmimage_decode(g, &samples, p);
            Ok((image, w, h))
        }
        _ => err(format!("MiniMax H3 Image 不支援模式：{mode}")),
    }
}

pub fn build(p: &Value, input_dir: &Path, seed_offset: i64) -> R<(Map<String, Value>, Value)> {
    // 內建影片 2026-10-30 起是 MiniMax H3（h3::build，經 server\api_h3.rs），LTX 的節點圖拿掉了
    if s(p.get("mode"), "") == "video" {
        return err("內建影片改用 MiniMax H3，請從「動起來」或導演台送出");
    }
    let seed = i(p.get("seed"), 0) + seed_offset;
    let mut rng = PyRandom::new(seed);
    let mut prompt = expand_wildcards(s(p.get("prompt"), ""), &mut rng);
    let negative = expand_wildcards(s(p.get("negative"), ""), &mut rng);
    let mode = s(p.get("mode"), "txt2img").to_string();
    let mut fam = p
        .get("family")
        .and_then(|v| v.as_str())
        .filter(|x| !x.is_empty())
        .unwrap_or("anima")
        .to_string();
    let q = fam == "qwen21";
    let mm = fam == crate::families::MMIMG;
    let step = if q || mm { 32 } else { 16 };
    let mut g = Graph::new(&fam);
    g.moss_4b = s(p.get("moss_te"), "") == "4b";
    g.mm_te = p["mm_te"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let inp = |name: &Value| input_dir.join(name.as_str().unwrap_or(""));
    let empty = json!({});

    let (img, w, h);
    match mode.as_str() {
        "txt2img" if g.mmimage() => (img, w, h) = mmimage_graph(&mut g, p, input_dir, "txt2img", &prompt, seed)?,
        "txt2img" => {
            let (w0, h0) = (
                snap(f(p.get("width"), 0.0), step, 256),
                snap(f(p.get("height"), 0.0), step, 256),
            );
            let cl = control_list(&mut g, p, w0, h0);
            let model = model_chain(&mut g, p, cl);
            let lay = if g.moss() { Layout::from_params(p, &prompt, &mut rng) } else { None };
            let (pos, neg) = if q {
                let (a, b, _) = g.qtext(&prompt, &negative, &[]);
                (a, b)
            } else {
                let t = lay.as_ref().map_or_else(|| prompt.clone(), |l| l.prompt(w0, h0));
                let pos = g.text(&t);
                let neg = g.neg_text(&pos, &negative, f(p.get("cfg"), 0.0));
                (pos, neg)
            };
            let (pos, neg) = if g.moss() { moss_control(&mut g, p, pos, neg, w0, h0) } else { (pos, neg) };
            // 姿勢參考圖（MOSS）：MOSS 沒有 ControlNet，改成從 3D 人偶的深度圖出發、重繪 0.95（Qwen 2.1 走 model_chain 裡的 Fun ControlNet）。
            // 2026-10-17 實測（E:\AI\moss_test\exp_replace.py p1／p2）：0.95 是寫實照片、姿勢和鏡頭跟著深度圖；0.85 會留下深度圖的平塗和黑底
            let pose_depth = p
                .get("pose_control")
                .and_then(|pc| pc.get("depth"))
                .and_then(Value::as_str)
                .filter(|x| g.moss() && !x.is_empty())
                // 有色彩／深度控制轉接器：深度圖走 MOSSImageControl（上面），從空白畫
                .filter(|_| !p["pose_control"].get("adapter").and_then(Value::as_str).is_some_and(|a| !a.is_empty()));
            let (lat, denoise) = match pose_depth {
                Some(file) => {
                    let li = g.add("LoadImage", json!({"image": format!("{POSE_INPUT}/{file}")}));
                    let src = g.scale(&li, w0, h0, "center", "lanczos");
                    (g.encode(&src, w0, h0), f(p["pose_control"].get("denoise"), 0.95))
                }
                None => (g.empty_latent(w0, h0), 1.0),
            };
            let smp = sample(
                &mut g,
                &model,
                &pos,
                &neg,
                &lat,
                p,
                seed,
                denoise,
                None,
                (w0, h0),
            );
            let im = g.decode(&smp, w0, h0);
            (img, w, h) = hires(&mut g, im, p, &model, &pos, &neg, seed, w0, h0, lay.as_ref(), &negative);
        }
        "img2img" if g.mmimage() => (img, w, h) = mmimage_graph(&mut g, p, input_dir, "img2img", &prompt, seed)?,
        "img2img" => {
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let edit = if g.moss() { moss_edit_format(p)? } else { None };
            let (w0, h0) = if s(p.get("size_mode"), "auto") == "auto" {
                // MOSS 照指令改圖：參考圖約 0.5MP（edit_pixels 可改）。參考圖的鍵值每層都存、正負各一份，
                // 1MP 時約 4GB，8GB 顯卡會開始搬記憶體、一步一分鐘多（2026-10-30 實測 768×1344）；0.5MP 一步約 4 秒
                let px = if edit.is_some() { or_f(p, "edit_pixels", 512.0 * 1024.0) } else { or_f(p, "pixels", if q { QWEN21_PIXELS } else { 1024.0 * 1536.0 }) };
                fit_pixels(ow, oh, px, step)
            } else {
                (
                    snap(f(p.get("width"), 0.0), step, 256),
                    snap(f(p.get("height"), 0.0), step, 256),
                )
            };
            let li = g.add("LoadImage", json!({"image": init}));
            let src = g.scale(&li, w0, h0, "center", "lanczos");
            let cl = control_list(&mut g, p, w0, h0);
            let model = model_chain(&mut g, p, cl);
            // 照指令改圖從空白畫，參考圖只走條件：重繪固定 1
            let denoise = if edit.is_some() { 1.0 } else { f(p.get("denoise"), if q { 1.0 } else { 0.6 }) };
            let lay = if g.moss() && edit.is_none() { Layout::from_params(p, &prompt, &mut rng) } else { None };
            let (pos, neg, lat);
            if let Some(fmt) = &edit {
                let (clip, vae) = (g.clip(), g.vae());
                pos = g.add("TextEncodeMOSSImageEdit", json!({"clip": clip, "prompt": prompt, "vae": vae, "edit_format": fmt, "images.image_0": src, "image_strength": 1.0}));
                neg = g.add("TextEncodeMOSSImageEdit", json!({"clip": clip, "prompt": negative, "vae": vae, "edit_format": fmt, "images.image_0": src, "image_strength": 1.0}));
                lat = g.empty_latent(w0, h0);
            } else if q {
                let (a, b, e) = g.qtext(&prompt, &negative, std::slice::from_ref(&src));
                pos = a;
                neg = b;
                // 1.0：照參考圖整張重畫（官方改圖流程）；低於 1：從原圖像素出發
                lat = if denoise >= 0.999 {
                    e
                } else {
                    g.encode(&src, w0, h0)
                };
            } else {
                pos = g.text(&lay.as_ref().map_or_else(|| prompt.clone(), |l| l.prompt(w0, h0)));
                neg = g.neg_text(&pos, &negative, f(p.get("cfg"), 0.0));
                lat = g.encode(&src, w0, h0);
            }
            let smp = sample(
                &mut g,
                &model,
                &pos,
                &neg,
                &lat,
                p,
                seed,
                denoise.min(1.0),
                None,
                (w0, h0),
            );
            let im = g.decode(&smp, w0, h0);
            (img, w, h) = hires(&mut g, im, p, &model, &pos, &neg, seed, w0, h0, lay.as_ref(), &negative);
        }
        "inpaint" | "outpaint" => {
            // 擴圖＝局部重繪的特例：ImagePadForOutpaint 把畫布往外補灰、外圈當遮罩（往原圖內羽化），整張重畫遮罩那圈
            let outpaint = mode == "outpaint";
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let mask_name = p.get("mask").cloned().unwrap_or(Value::Null);
            let (mut ow, mut oh) = image_size(&inp(&init))?;
            let ip = p
                .get("inpaint")
                .filter(|v| truthy(Some(v)))
                .unwrap_or(&empty);
            let (orig, full_mask, orig_unpadded);
            if outpaint {
                let op = p.get("outpaint").filter(|v| v.is_object()).unwrap_or(&empty);
                // 節點要 8 的倍數：往上取
                let side = |k: &str| (i(op.get(k), 0).clamp(0, 2048) + 7) / 8 * 8;
                let (l, t, r, b) = (side("left"), side("top"), side("right"), side("bottom"));
                if l + t + r + b == 0 {
                    return err("擴圖：至少要選一邊往外擴");
                }
                let li = g.add("LoadImage", json!({"image": init}));
                let feather = i(op.get("feather"), 40).clamp(0, 400);
                let pad = g.add(
                    "ImagePadForOutpaint",
                    json!({"image": li, "left": l, "top": t, "right": r, "bottom": b, "feathering": feather}),
                );
                orig_unpadded = Some(li);
                orig = pad.clone();
                full_mask = Graph::slot(&pad, 1);
                (ow, oh) = (ow + l + r, oh + t + b);
                if ow * oh > 4096 * 4096 {
                    return err("擴圖後超過 4096×4096，請把往外擴的像素改小");
                }
            } else {
                orig_unpadded = None;
                orig = g.add("LoadImage", json!({"image": init}));
                full_mask = g.add(
                    "LoadImageMask",
                    json!({"image": mask_name, "channel": "red"}),
                );
            }
            let (rx, ry, rw, rh, region, rmask);
            if !outpaint && s(ip.get("area"), "") == "masked" {
                let Some(bb) = mask_bbox(&inp(&mask_name))? else {
                    return err("遮罩是空的，請先在圖上塗出要重繪的區域");
                };
                let pad = i(ip.get("padding"), 64).clamp(0, 1024);
                let (x0, y0) = ((bb.0 - pad).max(0), (bb.1 - pad).max(0));
                let (x1, y1) = (ow.min(bb.2 + pad), oh.min(bb.3 + pad));
                (rx, ry, rw, rh) = (x0, y0, x1 - x0, y1 - y0);
                region = g.add(
                    "ImageCrop",
                    json!({"image": orig, "width": rw, "height": rh, "x": rx, "y": ry}),
                );
                let mi = g.add("MaskToImage", json!({"mask": full_mask}));
                let crop = g.add(
                    "ImageCrop",
                    json!({"image": mi, "width": rw, "height": rh, "x": rx, "y": ry}),
                );
                rmask = g.add("ImageToMask", json!({"channel": "red", "image": crop}));
            } else {
                (rx, ry, rw, rh) = (0, 0, ow, oh);
                region = orig.clone();
                rmask = full_mask.clone();
            }
            let (w0, h0) = fit_pixels(rw, rh, f(ip.get("pixels"), 1024.0 * 1024.0).clamp(256.0 * 256.0, 4096.0 * 4096.0), step);
            let work = g.scale(&region, w0, h0, "disabled", "lanczos");
            let mut wmask = g.mask_scale(&rmask, w0, h0);
            let grow = i(ip.get("grow"), 8);
            if grow != 0 {
                let expand = 1i64.max((grow as f64 * w0 as f64 / rw as f64).trunc() as i64);
                wmask = g.add(
                    "GrowMask",
                    json!({"mask": wmask, "expand": expand, "tapered_corners": true}),
                );
            }
            let mut lll = vec![];
            if s(ip.get("method"), "lllite") == "lllite" && g.family == "anima" {
                lll.push((
                    lllite("inpaint"),
                    work.clone(),
                    Some(wmask.clone()),
                    f(ip.get("strength"), 1.0),
                    0.0,
                    1.0,
                ));
            }
            let model = model_chain(&mut g, p, lll);
            let out = if g.mmimage() {
                // MiniMax H3 Image：擴圖餵未補灰邊的原圖當 <Picture 1>（探針 p2a 實測接縫最自然），重繪則餵裁切區 work
                let ref_img = if outpaint { orig_unpadded.unwrap_or_else(|| work.clone()) } else { work.clone() };
                let mp = if outpaint {
                    let tail = adapt_mmimage_refs(&prompt);
                    if tail.trim().is_empty() { MMIMG_OUTPAINT_HINT.to_string() } else { format!("{MMIMG_OUTPAINT_HINT} {tail}") }
                } else {
                    ensure_mm_picture1(&adapt_mmimage_refs(&prompt))
                };
                let (pos, neg, _) = mmimage_cond(&mut g, p, &mp, std::slice::from_ref(&ref_img), json!(w0), json!(h0), false);
                let enc = g.encode(&work, w0, h0);
                let lat = g.add("SetLatentNoiseMask", json!({"samples": enc, "mask": wmask}));
                let smp = mmimage_sample(&mut g, &model, &pos, &neg, &lat, p, seed, f(p.get("denoise"), 1.0));
                mmimage_decode(&mut g, &smp, p)
            } else {
                // Qwen 2.1：整塊區域當參考圖，重繪處才接得上周圍（擴圖時它會照參考圖把灰邊畫出來：補一句叫它延伸場景）
                let (pos, neg) = if q {
                    let qp = if outpaint { format!("{OUTPAINT_HINT} {prompt}") } else { prompt.clone() };
                    let (a, b, _) = g.qtext(&qp, &negative, std::slice::from_ref(&work));
                    (a, b)
                } else {
                    let pos = g.text(&prompt);
                    let neg = g.neg_text(&pos, &negative, f(p.get("cfg"), 0.0));
                    (pos, neg)
                };
                let enc = g.encode(&work, w0, h0);
                let lat = g.add("SetLatentNoiseMask", json!({"samples": enc, "mask": wmask}));
                let smp = sample(
                    &mut g,
                    &model,
                    &pos,
                    &neg,
                    &lat,
                    p,
                    seed,
                    f(p.get("denoise"), 1.0),
                    None,
                    (w0, h0),
                );
                g.decode(&smp, w0, h0)
            };
            // 縮回原尺寸後貼回原圖：遮罩外的像素完全保留，邊緣用模糊遮罩過渡
            let out = g.scale(&out, rw, rh, "disabled", "lanczos");
            let cmask = if grow != 0 {
                g.add(
                    "GrowMask",
                    json!({"mask": rmask, "expand": grow, "tapered_corners": true}),
                )
            } else {
                rmask.clone()
            };
            let cmask = g.mask_blur(&cmask, i(ip.get("blur"), 12));
            img = g.add("ImageCompositeMasked", json!({"destination": orig, "source": out, "x": rx, "y": ry, "resize_source": false, "mask": cmask}));
            (w, h) = (ow, oh);
        }
        "multiref" if g.mmimage() => (img, w, h) = mmimage_graph(&mut g, p, input_dir, "multiref", &prompt, seed)?,
        "multiref" => {
            // 多圖參考（Qwen 2.1 最多 10 張；MiniMax H3 Image 走上方分支最多 16 張）
            if !q {
                return err("多圖參考支援 Qwen 2.1 與 MiniMax H3 Image 模型，請先切換模型");
            }
            let refs: Vec<&Value> = match p.get("refs") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter(|r| truthy(r.get("image")))
                    .take(10)
                    .collect(),
                _ => vec![],
            };
            if refs.is_empty() {
                return err("請先加入至少一張參考圖");
            }
            let auto = s(p.get("size_mode"), "auto") == "auto";
            let (rw0, rh0) = image_size(&inp(&refs[0]["image"]))?;
            let (w0, h0) = if auto {
                fit_pixels(rw0, rh0, or_f(p, "pixels", QWEN21_PIXELS), step)
            } else {
                (
                    snap(f(p.get("width"), 0.0), step, 256),
                    snap(f(p.get("height"), 0.0), step, 256),
                )
            };
            let mut imgs = vec![];
            for (k, r) in refs.iter().enumerate() {
                let (tw, th) = if k == 0 && auto {
                    (w0, h0)
                } else {
                    let (iw, ih) = image_size(&inp(&r["image"]))?;
                    fit_pixels(iw, ih, or_f(p, "ref_pixels", 512.0 * 1024.0), step)
                };
                let li = g.add("LoadImage", json!({"image": r["image"]}));
                imgs.push(g.scale(&li, tw, th, "center", "lanczos"));
            }
            let model = model_chain(&mut g, p, vec![]);
            let (pos, neg, mut lat) = g.qtext(&prompt, &negative, &imgs);
            if !auto {
                lat = g.add(
                    "EmptyLatentImage",
                    json!({"width": w0, "height": h0, "batch_size": 1}),
                );
            }
            let smp = sample(
                &mut g,
                &model,
                &pos,
                &neg,
                &lat,
                p,
                seed,
                1.0,
                None,
                (w0, h0),
            );
            img = g.decode(&smp, w0, h0);
            (w, h) = (w0, h0);
        }
        "headswap" => {
            // 漫畫修角色只換頭（2026-11-16）：SAM3 找頭 → AnimaHeadCrop 裁頭放大 → Qwen 多圖參考（<image1>＝頭部裁切）
            // 只在頭部遮罩裡重畫（SetLatentNoiseMask）→ AnimaHeadStitch 對齊顏色貼回原圖；節點在 resources\extras\anima_comic_nodes
            if !q {
                return err("多圖參考只支援 Qwen 2.1 模型（Noct Q），請先在「模型」換成 Qwen 2.1");
            }
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            // refs 空的＝只改頭部（補表情），<image1> 一張圖
            let refs: Vec<&Value> = p.get("refs").and_then(Value::as_array).into_iter().flatten().filter(|r| truthy(r.get("image"))).take(9).collect();
            let hd = p.get("head").filter(|v| v.is_object()).unwrap_or(&empty);
            let pixels = f(hd.get("pixels"), QWEN21_PIXELS).clamp(256.0 * 256.0, 2048.0 * 2048.0);
            let src = g.add("LoadImage", json!({"image": init}));
            // head.mask＝已經挑好的那顆頭（input 裡的黑白遮罩，比對參考圖挑的）；沒給才在這裡跑 SAM3、照 index 挑
            let (det, index) = match hd.get("mask").and_then(Value::as_str).filter(|m| !m.is_empty()) {
                Some(m) => (g.add("LoadImageMask", json!({"image": m, "channel": "red"})), 0),
                None => {
                    let sam = g.add("CheckpointLoaderSimple", json!({"ckpt_name": SAM3_CKPT}));
                    let word = g.add("CLIPTextEncode", json!({"text": s(hd.get("word"), HEAD_PROMPT), "clip": Graph::slot(&sam, 1)}));
                    let det = g.add(
                        "SAM3_Detect",
                        json!({"model": sam, "image": src, "conditioning": word, "threshold": f(hd.get("threshold"), 0.35).clamp(0.05, 0.95),
                               "refine_iterations": 2, "individual_masks": true}),
                    );
                    (det, i(hd.get("index"), 0).clamp(0, 16))
                }
            };
            let crop = g.add(
                "AnimaHeadCrop",
                json!({"image": src, "masks": det, "index": index, "context": f(hd.get("context"), 0.6).clamp(0.0, 3.0),
                       "pixels": pixels as i64, "grow": i(hd.get("grow"), 16).clamp(0, 128), "blur": i(hd.get("blur"), 12).clamp(0, 128)}),
            );
            let mut imgs = vec![crop.clone()];
            for r in &refs {
                let (iw, ih) = image_size(&inp(&r["image"]))?;
                let (tw, th) = fit_pixels(iw, ih, or_f(p, "ref_pixels", 512.0 * 1024.0), step);
                let li = g.add("LoadImage", json!({"image": r["image"]}));
                imgs.push(g.scale(&li, tw, th, "center", "lanczos"));
            }
            let model = model_chain(&mut g, p, vec![]);
            let (pos, neg, _) = g.qtext(&prompt, &negative, &imgs);
            // 裁切的實際尺寸要到 ComfyUI 裡才知道（約 pixels 個像素），編碼／解碼照 pixels 判斷要不要分塊
            let side = pixels.sqrt() as i64;
            let enc = g.encode(&crop, side, side);
            let lat = g.add("SetLatentNoiseMask", json!({"samples": enc, "mask": Graph::slot(&crop, 1)}));
            let smp = sample(&mut g, &model, &pos, &neg, &lat, p, seed, 1.0, None, (side, side));
            let fixed = g.decode(&smp, side, side);
            img = g.add(
                "AnimaHeadStitch",
                json!({"image": src, "fixed": fixed, "mask": Graph::slot(&crop, 1), "stitch": Graph::slot(&crop, 2), "color_match": true}),
            );
            (w, h) = (ow, oh);
        }
        "strip" => {
            // 一鍵脫衣：改圖引擎（Qwen 2.1 或 MiniMax H3 Image）→（可選）Anima 低重繪精修 →（可選）只把改到的地方貼回
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let st = p.get("strip").filter(|v| v.is_object()).unwrap_or(&empty);
            let use_mm = edit_uses_mmimage(p, "strip", &fam);
            let (w0, h0) = fit_pixels(ow, oh, QWEN21_PIXELS, 32);
            let li = g.add("LoadImage", json!({"image": init}));
            // 拉伸、不裁切：最後再拉回原尺寸，構圖一點都不少
            let src = g.scale(&li, w0, h0, "disabled", "lanczos");
            let raw_prompt = s(st.get("qwen_prompt"), "").to_string();
            let (qimg, qprompt, edit_fam) = if use_mm {
                let mp = edit_mmimage_params(p, &fam);
                g.switch_family(crate::families::MMIMG);
                let sp = ensure_mm_picture1(&adapt_mmimage_refs(&raw_prompt));
                let mmodel = model_chain(&mut g, &mp, vec![]);
                let (mpos, mneg, mlat) = mmimage_cond(&mut g, &mp, &sp, std::slice::from_ref(&src), json!(w0), json!(h0), false);
                let ms = mmimage_sample(&mut g, &mmodel, &mpos, &mneg, &mlat, &mp, seed, 1.0);
                (mmimage_decode(&mut g, &ms, &mp), sp, crate::families::MMIMG)
            } else {
                // 改圖指令和精修 tag 由內容補丁算好塞進 params（server::api_gen::prepare），graph 本身不含露骨文字
                // 生成頁選的是 Qwen 模型就用它（連同步數、CFG、LoRA）；不然用官方 Turbo，LoRA 留給 Anima 精修那段
                let qp = edit_qwen_params(p, &fam);
                g.switch_family("qwen21");
                let qmodel = model_chain(&mut g, &qp, vec![]);
                let (qpos, qneg, qlat) = g.qtext(&raw_prompt, "", std::slice::from_ref(&src));
                let qs = sample(&mut g, &qmodel, &qpos, &qneg, &qlat, &qp, seed, 1.0, None, (w0, h0));
                (g.decode(&qs, w0, h0), raw_prompt, "qwen21")
            };
            let refine = st.get("refine").map(|v| truthy(Some(v))).unwrap_or(true);
            let (out, ow2, oh2) = if !refine {
                (qimg, w0, h0)
            } else {
                // 精修：Anima Turbo 8 步、CFG 1；生成頁是 Anima 時，它掛的 LoRA（畫風）一起用
                let rmodel = s(st.get("refine_model"), STRIP_REFINE).to_string();
                let turbo = rmodel.to_lowercase().contains("turbo");
                let ap = json!({
                    "model": rmodel, "turbo_lora": !turbo, "steps": if turbo { 8 } else { 10 }, "cfg": 1,
                    "sampler": "euler", "scheduler": "simple",
                    "loras": if fam == "anima" { p.get("loras").cloned().unwrap_or(json!([])) } else { json!([]) },
                });
                g.switch_family("anima");
                let (aw, ah) = fit_pixels(w0, h0, 1024.0 * 1536.0, 16);
                let qup = g.scale(&qimg, aw, ah, "disabled", "lanczos");
                let amodel = model_chain(&mut g, &ap, vec![]);
                // 不用生成頁的提示詞框：那裡常留著上一張的服裝 tag，精修會把衣服畫回去
                let tags = s(st.get("refine_tags"), "");
                let apos = g.text(tags);
                let aneg = g.text("");
                let alat = g.encode(&qup, aw, ah);
                let den = f(st.get("refine_denoise"), 0.35).clamp(0.1, 0.8);
                let asmp = sample(&mut g, &amodel, &apos, &aneg, &alat, &ap, seed, den, None, (aw, ah));
                let rimg = g.decode(&asmp, aw, ah);
                let merged = if st.get("merge").map(|v| truthy(Some(v))).unwrap_or(true) {
                    // 改到的地方＝原圖和 Qwen 結果任一色版差超過門檻；先縮一點去掉線條錯位的雜點，再撐大、縮回補洞，最後柔化邊緣
                    let diff = g.add("ImageBlend", json!({"image1": src, "image2": qimg, "blend_factor": 1.0, "blend_mode": "difference"}));
                    let th = f(st.get("threshold"), 0.15);
                    let mut m: Option<Ref> = None;
                    for ch in ["red", "green", "blue"] {
                        let c = g.add("ImageToMask", json!({"image": diff, "channel": ch}));
                        let t = g.add("ThresholdMask", json!({"mask": c, "value": th}));
                        m = Some(match m {
                            None => t,
                            Some(prev) => g.add("MaskComposite", json!({"destination": prev, "source": t, "x": 0, "y": 0, "operation": "or"})),
                        });
                    }
                    let mut m = m.unwrap();
                    for e in [-2, 20, -14] {
                        m = g.add("GrowMask", json!({"mask": m, "expand": e, "tapered_corners": true}));
                    }
                    let m = g.mask_blur(&m, 6);
                    let m = g.mask_scale(&m, aw, ah);
                    g.add("ImageCompositeMasked", json!({"destination": qup, "source": rimg, "x": 0, "y": 0, "resize_source": false, "mask": m}))
                } else {
                    rimg
                };
                (merged, aw, ah)
            };
            // 輸出回到原圖尺寸（2026-10-29 實測 832×1216 的原圖出來變 1024×1536，長寬比差 2.5%）；太大的原圖照比例縮到 STRIP_MAX_OUT
            let (tw, th) = if ow * oh <= STRIP_MAX_OUT { (ow, oh) } else { fit_pixels(ow, oh, STRIP_MAX_OUT as f64, 8) };
            img = if (ow2, oh2) != (tw, th) { g.scale(&out, tw, th, "disabled", "lanczos") } else { out };
            (w, h) = (tw, th);
            prompt = qprompt;
            fam = edit_fam.into();
        }
        "sheet" => {
            // 四視圖編輯：圖1＝角色設定稿（構圖、尺寸照它），圖2＝可選的服裝參考；支援 Qwen 2.1 與 MiniMax H3 Image
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let sh = p.get("sheet").filter(|v| v.is_object()).unwrap_or(&empty);
            let req = s(sh.get("req"), "").trim().to_string();
            if req.is_empty() {
                return err("請先寫「修改要求」（或從常用要求挑一個）");
            }
            let use_mm = edit_uses_mmimage(p, "sheet", &fam);
            let (w0, h0) = fit_pixels(ow, oh, 1280.0 * 1280.0, 32);
            let li = g.add("LoadImage", json!({"image": init}));
            let mut imgs = vec![g.scale(&li, w0, h0, "center", "lanczos")];
            let rf = sh.get("ref").filter(|v| truthy(Some(v))).cloned();
            if let Some(rf) = &rf {
                let (rw, rh) = image_size(&inp(rf))?;
                let (tw, th) = fit_pixels(rw, rh, 1024.0 * 1024.0, 32);
                let ri = g.add("LoadImage", json!({"image": rf}));
                imgs.push(g.scale(&ri, tw, th, "center", "lanczos"));
            }
            if use_mm {
                let mp = edit_mmimage_params(p, &fam);
                g.switch_family(crate::families::MMIMG);
                let sp = sheet_prompt_for(&req, rf.is_some(), crate::families::MMIMG);
                let model = model_chain(&mut g, &mp, vec![]);
                let (pos, neg, lat) = mmimage_cond(&mut g, &mp, &sp, &imgs, json!(w0), json!(h0), false);
                let smp = mmimage_sample(&mut g, &model, &pos, &neg, &lat, &mp, seed, 1.0);
                img = mmimage_decode(&mut g, &smp, &mp);
                (w, h) = (w0, h0);
                prompt = sp;
                fam = crate::families::MMIMG.into();
            } else {
                let qp = edit_qwen_params(p, &fam);
                g.switch_family("qwen21");
                let sp = sheet_prompt(&req, rf.is_some());
                let model = model_chain(&mut g, &qp, vec![]);
                let (pos, neg, lat) = g.qtext(&sp, "", &imgs);
                let smp = sample(&mut g, &model, &pos, &neg, &lat, &qp, seed, 1.0, None, (w0, h0));
                img = g.decode(&smp, w0, h0);
                (w, h) = (w0, h0);
                prompt = sp;
                fam = "qwen21".into();
            }
        }
        "diff" => {
            // 差分：一張角色圖改表情／動作，支援 Qwen 2.1 與 MiniMax H3 Image
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let req = p.get("diff").map(|d| s(d.get("req"), "")).unwrap_or("").trim().to_string();
            if req.is_empty() {
                return err("差分：請先選或寫一個要改的表情／動作");
            }
            let use_mm = edit_uses_mmimage(p, "diff", &fam);
            let (w0, h0) = fit_pixels(ow, oh, QWEN21_PIXELS, 32);
            let li = g.add("LoadImage", json!({"image": init}));
            let src = g.scale(&li, w0, h0, "center", "lanczos");
            if use_mm {
                let mp = edit_mmimage_params(p, &fam);
                g.switch_family(crate::families::MMIMG);
                let dp = diff_prompt_for(&req, crate::families::MMIMG);
                let model = model_chain(&mut g, &mp, vec![]);
                let (pos, neg, lat) = mmimage_cond(&mut g, &mp, &dp, std::slice::from_ref(&src), json!(w0), json!(h0), false);
                let smp = mmimage_sample(&mut g, &model, &pos, &neg, &lat, &mp, seed, 1.0);
                img = mmimage_decode(&mut g, &smp, &mp);
                (w, h) = (w0, h0);
                prompt = dp;
                fam = crate::families::MMIMG.into();
            } else {
                let qp = edit_qwen_params(p, &fam);
                g.switch_family("qwen21");
                let dp = diff_prompt(&req);
                let model = model_chain(&mut g, &qp, vec![]);
                let (pos, neg, lat) = g.qtext(&dp, "", std::slice::from_ref(&src));
                let smp = sample(&mut g, &model, &pos, &neg, &lat, &qp, seed, 1.0, None, (w0, h0));
                img = g.decode(&smp, w0, h0);
                (w, h) = (w0, h0);
                prompt = dp;
                fam = "qwen21".into();
            }
        }
        "detail" => {
            // 修臉／修手：/api/detail 先用 SAM3 找出每個物件的遮罩（存在 input），這裡每個遮罩各裁一塊、放大重畫、貼回；
            // 一段接一段（下一段的底圖是上一段貼好的），共用同一組模型，不用 LLLite（低重繪不需要）
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let dt = p.get("detail").filter(|v| v.is_object()).unwrap_or(&empty);
            let masks: Vec<Value> = dt
                .get("masks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|m| truthy(Some(m)))
                .take(8)
                .cloned()
                .collect();
            if masks.is_empty() {
                return err("修細節：沒有偵測到要修的地方");
            }
            let den = f(p.get("denoise"), 0.4).clamp(0.05, 1.0);
            let grow = i(dt.get("grow"), 8).clamp(0, 64);
            let blur = i(dt.get("blur"), 8).clamp(0, 64);
            let pixels = f(dt.get("pixels"), 1024.0 * 1024.0).clamp(256.0 * 256.0, 1536.0 * 1536.0);
            let extra = s(dt.get("extra"), "").trim();
            let text = if extra.is_empty() { prompt.clone() } else { format!("{prompt}, {extra}") };
            let model = model_chain(&mut g, p, vec![]);
            let cfg = f(p.get("cfg"), 0.0);
            let shared = (!q && !mm).then(|| {
                let pos = g.text(&text);
                (pos.clone(), g.neg_text(&pos, &negative, cfg))
            });
            let mut cur = g.add("LoadImage", json!({"image": init}));
            for (k, m) in masks.iter().enumerate() {
                let Some(bb) = mask_bbox(&inp(m))? else {
                    continue;
                };
                // 外框四周多留一點（物件越大留越多），重畫時才看得到周圍、接得上
                let pad = 24.max((bb.2 - bb.0).max(bb.3 - bb.1) / 3);
                let (x0, y0) = ((bb.0 - pad).max(0), (bb.1 - pad).max(0));
                let (x1, y1) = (ow.min(bb.2 + pad), oh.min(bb.3 + pad));
                let (rw, rh) = (x1 - x0, y1 - y0);
                let region = g.add("ImageCrop", json!({"image": cur, "width": rw, "height": rh, "x": x0, "y": y0}));
                let lm = g.add("LoadImageMask", json!({"image": m, "channel": "red"}));
                let mi = g.add("MaskToImage", json!({"mask": lm}));
                let crop = g.add("ImageCrop", json!({"image": mi, "width": rw, "height": rh, "x": x0, "y": y0}));
                let rmask = g.add("ImageToMask", json!({"channel": "red", "image": crop}));
                let (w0, h0) = fit_pixels(rw, rh, pixels, step);
                let work = g.scale(&region, w0, h0, "disabled", "lanczos");
                let mut wmask = g.mask_scale(&rmask, w0, h0);
                if grow > 0 {
                    let expand = 1i64.max((grow as f64 * w0 as f64 / rw as f64).trunc() as i64);
                    wmask = g.add("GrowMask", json!({"mask": wmask, "expand": expand, "tapered_corners": true}));
                }
                let out = if mm {
                    let mp = ensure_mm_picture1(&adapt_mmimage_refs(&text));
                    let (pos, neg, _) = mmimage_cond(&mut g, p, &mp, std::slice::from_ref(&work), json!(w0), json!(h0), false);
                    let enc = g.encode(&work, w0, h0);
                    let lat = g.add("SetLatentNoiseMask", json!({"samples": enc, "mask": wmask}));
                    let smp = mmimage_sample(&mut g, &model, &pos, &neg, &lat, p, seed + k as i64, den);
                    mmimage_decode(&mut g, &smp, p)
                } else {
                    let (pos, neg) = match &shared {
                        Some(x) => x.clone(),
                        None => {
                            let (a, b, _) = g.qtext(&text, &negative, std::slice::from_ref(&work));
                            (a, b)
                        }
                    };
                    let enc = g.encode(&work, w0, h0);
                    let lat = g.add("SetLatentNoiseMask", json!({"samples": enc, "mask": wmask}));
                    let smp = sample(&mut g, &model, &pos, &neg, &lat, p, seed + k as i64, den, None, (w0, h0));
                    g.decode(&smp, w0, h0)
                };
                let out = g.scale(&out, rw, rh, "disabled", "lanczos");
                let cmask = if grow > 0 {
                    g.add("GrowMask", json!({"mask": rmask, "expand": grow, "tapered_corners": true}))
                } else {
                    rmask.clone()
                };
                let cmask = g.mask_blur(&cmask, blur);
                cur = g.add(
                    "ImageCompositeMasked",
                    json!({"destination": cur, "source": out, "x": x0, "y": y0, "resize_source": false, "mask": cmask}),
                );
            }
            img = cur;
            (w, h) = (ow, oh);
        }
        "rembg" => {
            // 一鍵去背：BiRefNet 出前景遮罩 → 當透明度接回原圖（官方藍圖 Remove Background (BiRefNet) 的接法），存成透明 PNG
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let li = g.add("LoadImage", json!({"image": init}));
            let bm = g.add("LoadBackgroundRemovalModel", json!({"bg_removal_name": s(p.get("bg_model"), BG_MODEL)}));
            let m = g.add("RemoveBackground", json!({"bg_removal_model": bm, "image": li}));
            let inv = g.add("InvertMask", json!({"mask": m}));
            img = g.add("JoinImageWithAlpha", json!({"image": li, "alpha": inv}));
            (w, h) = (ow, oh);
        }
        "upscale" => {
            let init = p.get("init_image").cloned().unwrap_or(Value::Null);
            let (ow, oh) = image_size(&inp(&init))?;
            let up = p
                .get("upscale")
                .filter(|v| truthy(Some(v)))
                .unwrap_or(&empty);
            let sc = f(up.get("scale"), 2.0);
            let st = if q || mm { 32 } else { 8 };
            let (w0, h0) = (snap(ow as f64 * sc, st, 64), snap(oh as f64 * sc, st, 64));
            let loader = g.add(
                "UpscaleModelLoader",
                json!({"model_name": raw(up, "upscaler", json!("4x-AnimeSharp.safetensors"))}),
            );
            let li = g.add("LoadImage", json!({"image": init}));
            let im = g.add(
                "ImageUpscaleWithModel",
                json!({"upscale_model": loader, "image": li}),
            );
            let mut im = g.scale(&im, w0, h0, "disabled", "lanczos");
            if truthy(up.get("refine")) {
                let model = model_chain(&mut g, p, vec![]);
                if mm {
                    let mp = ensure_mm_picture1(&adapt_mmimage_refs(&prompt));
                    let (pos, neg, _) = mmimage_cond(&mut g, p, &mp, std::slice::from_ref(&im), json!(w0), json!(h0), false);
                    let lat = g.encode(&im, w0, h0);
                    let smp = mmimage_sample(&mut g, &model, &pos, &neg, &lat, p, seed, f(up.get("denoise"), 0.3));
                    im = mmimage_decode(&mut g, &smp, p);
                } else {
                    let (pos, neg) = if q {
                        let (a, b, _) = g.qtext(&prompt, &negative, &[]);
                        (a, b)
                    } else {
                        let pos = g.text(&prompt);
                        let neg = g.neg_text(&pos, &negative, f(p.get("cfg"), 0.0));
                        (pos, neg)
                    };
                    let lat = g.encode(&im, w0, h0);
                    let smp = sample(
                        &mut g,
                        &model,
                        &pos,
                        &neg,
                        &lat,
                        p,
                        seed,
                        f(up.get("denoise"), 0.3),
                        None,
                        (w0, h0),
                    );
                    im = g.decode(&smp, w0, h0);
                }
            }
            (img, w, h) = (im, w0, h0);
        }
        other => return err(format!("未知的模式：{other}")),
    }
    g.add(
        "SaveImage",
        json!({"images": img, "filename_prefix": format!("AnimaWeb/{mode}")}),
    );
    let info = json!({"prompt": prompt, "negative": negative, "seed": seed, "width": w, "height": h, "mode": mode, "family": fam});
    Ok((g.nodes, info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moss_txt2img_graph() {
        let dir = std::env::temp_dir();
        let p = json!({"mode": "txt2img", "family": "moss", "model": "moss_image_sft_int8_convrot.safetensors", "prompt": "a cat", "negative": "",
                       "seed": 1, "width": 1024, "height": 1024, "steps": 50, "cfg": 3, "sampler": "euler", "scheduler": "sgm_uniform"});
        let (n, info) = build(&p, &dir, 0).unwrap();
        let by = |c: &str| n.values().find(|v| v["class_type"] == c).cloned();
        let clip = by("CLIPLoader").unwrap();
        assert_eq!(clip["inputs"]["type"], "moss_image");
        assert_eq!(clip["inputs"]["clip_name"], MOSS_TE);
        assert_eq!(by("VAELoader").unwrap()["inputs"]["vae_name"], MOSS_VAE);
        assert!(by("EmptyFlux2LatentImage").is_some() && by("EmptyLatentImage").is_none());
        assert_eq!(by("ModelSamplingAuraFlow").unwrap()["inputs"]["shift"], 5.0);
        assert!(by("ModelSamplingSD3").is_none());
        assert_eq!(by("SkipLayerGuidanceDiTSimple").unwrap()["inputs"]["single_layers"], "22");
        let ks = by("KSampler").unwrap();
        assert_eq!(ks["inputs"]["scheduler"], "sgm_uniform");
        assert_eq!(info["family"], "moss");
        // CFG 1：沒有負面那一路，不接跳層
        let mut p1 = p.clone();
        p1["cfg"] = json!(1);
        let (n1, _) = build(&p1, &dir, 0).unwrap();
        assert!(!n1.values().any(|v| v["class_type"] == "SkipLayerGuidanceDiTSimple"));
        assert!(n1.values().any(|v| v["class_type"] == "ModelSamplingAuraFlow"));
    }

    fn mm_case(count: usize) -> Value {
        let refs: Vec<_> = (1..=count).map(|i| json!({"image": format!("{i}.png")})).collect();
        json!({"family":"mmimage", "model":"main.safetensors", "prompt":"change the scene",
               "mode": if count == 0 { "txt2img" } else { "multiref" }, "refs":refs, "width":2048, "height":2304})
    }

    #[test]
    fn mmimage_txt2img_graph() {
        let (n, _) = build(&mm_case(0), Path::new("."), 0).unwrap();
        let input = |c| by_class(&n, c)[0]["inputs"].clone();
        assert_eq!(input("UNETLoader")["unet_name"], "main.safetensors");
        assert_eq!(input("VAELoader")["vae_name"], "minimax_h3_image_vae_fp16.safetensors");
        assert_eq!(input("CLIPLoader")["type"], "minimax");
        assert_eq!(input("CLIPLoader")["clip_name"], crate::h3::TE);
        let c = input("MiniMaxH3ImageConditioning");
        assert_eq!((&c["width"], &c["height"]), (&json!(2048), &json!(2304)));
        assert!(c.get("images.image_1").is_none());
        let k = input("KSampler");
        assert_eq!((&k["steps"], &k["cfg"], &k["denoise"]), (&json!(6), &json!(1.0), &json!(1.0)));
        assert_eq!((&k["sampler_name"], &k["scheduler"]), (&json!("euler"), &json!("simple")));
        assert_eq!(n[k["negative"][0].as_str().unwrap()]["class_type"], "ConditioningZeroOut");
        assert_eq!(input("ConditioningZeroOut")["conditioning"], k["positive"]);
    }

    #[test]
    fn mmimage_multiref_16() {
        let (n, _) = build(&mm_case(16), Path::new("."), 0).unwrap();
        let c = &by_class(&n, "MiniMaxH3ImageConditioning")[0]["inputs"];
        for i in 1..=16 {
            let mut node = &n[c[format!("images.image_{i}")][0].as_str().unwrap()];
            if i == 1 {
                for (k, v) in [("resize_type.multiple", 32), ("resize_type.megapixels", 4)] {
                    assert_eq!(node["class_type"], "ResizeImageMaskNode");
                    assert_eq!(node["inputs"][k], v);
                    node = &n[node["inputs"]["input"][0].as_str().unwrap()];
                }
            }
            assert_eq!(node["inputs"]["image"], format!("{i}.png"));
        }
    }

    #[test]
    fn mmimage_refs_over_limit() {
        assert!(build(&mm_case(17), Path::new("."), 0).unwrap_err().0.contains("16"));
    }

    #[test]
    fn mmimage_pe_on() {
        for count in [0, 3] {
            let mut p = mm_case(count);
            p["mm_pe"] = json!("pe.safetensors");
            let (n, _) = build(&p, Path::new("."), 0).unwrap();
            let c = &by_class(&n, "MiniMaxH3ImageConditioning")[0]["inputs"];
            let extract = &n[c["prompt"][0].as_str().unwrap()];
            assert_eq!(extract["class_type"], "JsonExtractString");
            assert_eq!(extract["inputs"]["key"], "expanded_caption");
            let gen = &n[extract["inputs"]["json_string"][0].as_str().unwrap()];
            assert_eq!(gen["class_type"], "TextGenerateV2");
            assert_eq!(n[gen["inputs"]["clip"][0].as_str().unwrap()]["inputs"]["clip_name"], "pe.safetensors");
            for i in 1..=count {
                assert_eq!(gen["inputs"][format!("images.image{i}")], c[format!("images.image_{i}")]);
            }
        }
    }

    #[test]
    fn mmimage_tiled_decode() {
        let mut p = mm_case(0);
        let (plain, _) = build(&p, Path::new("."), 0).unwrap();
        assert_eq!(by_class(&plain, "VAEDecode").len(), 1);
        p["vae_tiled"] = json!(512);
        let (n, _) = build(&p, Path::new("."), 0).unwrap();
        assert_eq!(by_class(&n, "VAEDecodeTiled")[0]["inputs"]["tile_size"], 512);
    }

    #[test]
    fn krea2_txt2img_graph() {
        // 照 ComfyUI 內建範本 Text to Image (Krea-2 Turbo)：CLIPLoader type krea2、qwen_image_vae、負面＝ConditioningZeroOut、KSampler 8 步 CFG 1
        let dir = std::env::temp_dir();
        let p = json!({"mode": "txt2img", "family": "krea2", "model": "krea2_turbo_int8_convrot.safetensors", "prompt": "a cat", "negative": "blurry",
                       "seed": 1, "width": 1024, "height": 1024, "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple",
                       "loras": [{"name": "krea2_darkbrush.safetensors", "strength": 0.8}]});
        let (n, info) = build(&p, &dir, 0).unwrap();
        let by = |n: &Map<String, Value>, c: &str| n.values().filter(|v| v["class_type"] == c).cloned().collect::<Vec<_>>();
        let clip = &by(&n, "CLIPLoader")[0];
        assert_eq!((clip["inputs"]["type"].clone(), clip["inputs"]["clip_name"].clone()), (json!("krea2"), json!(KREA2_TE)));
        assert_eq!(by(&n, "VAELoader")[0]["inputs"]["vae_name"], VAE);
        assert_eq!(by(&n, "EmptyLatentImage").len(), 1);
        let zero = &by(&n, "ConditioningZeroOut");
        assert_eq!(zero.len(), 1);
        assert_eq!(by(&n, "CLIPTextEncode").len(), 1, "CFG 1 不用編碼負面詞");
        let ks = &by(&n, "KSampler")[0]["inputs"];
        assert_eq!((ks["steps"].clone(), ks["cfg"].clone(), ks["scheduler"].clone()), (json!(8), json!(1.0), json!("simple")));
        assert_eq!(n[ks["negative"][0].as_str().unwrap()]["class_type"], "ConditioningZeroOut");
        assert_eq!(by(&n, "LoraLoaderModelOnly")[0]["inputs"]["lora_name"], "krea2_darkbrush.safetensors");
        assert!(by(&n, "ModelSamplingAuraFlow").is_empty() && by(&n, "AnimaLLLiteApply").is_empty());
        assert_eq!(info["family"], "krea2");
        // Raw（CFG > 1）：負面詞照常編碼
        let mut raw = p.clone();
        raw["cfg"] = json!(4);
        let (n2, _) = build(&raw, &dir, 0).unwrap();
        assert!(by(&n2, "ConditioningZeroOut").is_empty());
        assert_eq!(by(&n2, "CLIPTextEncode").len(), 2);
    }

    #[test]
    fn sdxl_checkpoint_graphs() {
        // SDXL：CheckpointLoaderSimple 一顆給 MODEL／CLIP／VAE；LoRA 用 LoraLoader 連 CLIP 一起套；負面詞照常編碼
        let dir = play_dir("sdxl", 640, 480, &[(100, 100, 300, 300)]);
        let p = json!({"mode": "txt2img", "family": "sdxl", "model": "NoobAI-XL-v1.1.safetensors", "prompt": "1girl", "negative": SDXL_NEG,
                       "seed": 1, "width": 832, "height": 1216, "steps": 28, "cfg": 5, "sampler": "euler_ancestral", "scheduler": "normal",
                       "turbo_lora": "anima-turbo-lora-v0.2.safetensors",
                       "loras": [{"name": "a.safetensors", "strength": 0.8}, {"name": "off.safetensors", "enabled": false}, {"name": "b.safetensors", "strength": 0.5}],
                       "hires": {"enabled": true, "scale": 1.5, "denoise": 0.4}});
        let (n, info) = build(&p, &dir, 0).unwrap();
        for c in ["UNETLoader", "CLIPLoader", "VAELoader", "LoraLoaderModelOnly", "AnimaLLLiteApply"] {
            assert!(by_class(&n, c).is_empty(), "SDXL 不該有 {c}");
        }
        let ck = by_class(&n, "CheckpointLoaderSimple");
        assert_eq!(ck.len(), 1);
        assert_eq!(ck[0]["inputs"]["ckpt_name"], "NoobAI-XL-v1.1.safetensors");
        let ck_id = n.iter().find(|(_, v)| v["class_type"] == "CheckpointLoaderSimple").unwrap().0.clone();
        let lo = by_class(&n, "LoraLoader");
        assert_eq!(lo.len(), 2, "停用的 LoRA 不接");
        assert_eq!((lo[0]["inputs"]["model"].clone(), lo[0]["inputs"]["clip"].clone()), (json!([ck_id, 0]), json!([ck_id, 1])));
        assert_eq!((lo[1]["inputs"]["lora_name"].clone(), lo[1]["inputs"]["strength_clip"].clone()), (json!("b.safetensors"), json!(0.5)));
        let last = n.iter().find(|(_, v)| v["inputs"]["lora_name"] == "b.safetensors").unwrap().0.clone();
        let te = by_class(&n, "CLIPTextEncode");
        assert_eq!(te.len(), 2, "CFG 5：正負都要編碼");
        assert!(te.iter().all(|x| x["inputs"]["clip"] == json!([last, 1])), "文字編碼器要接最後一顆 LoRA 的 CLIP");
        assert!(te.iter().any(|x| x["inputs"]["text"] == SDXL_NEG));
        let ks = by_class(&n, "KSampler");
        assert_eq!(ks.len(), 2, "高解析修復第二段");
        assert!(ks.iter().all(|k| k["inputs"]["model"] == json!([last, 0])));
        assert_eq!((ks[0]["inputs"]["sampler_name"].clone(), ks[0]["inputs"]["cfg"].clone()), (json!("euler_ancestral"), json!(5.0)));
        assert!(by_class(&n, "VAEDecode").iter().chain(by_class(&n, "VAEEncode").iter()).all(|x| x["inputs"]["vae"] == json!([ck_id, 2])));
        assert_eq!(by_class(&n, "EmptyLatentImage").len(), 1);
        assert_eq!((info["family"].clone(), info["width"].clone()), (json!("sdxl"), json!(1248)));
        // 圖生圖、局部重繪、修細節、放大精修：都只有一顆 checkpoint，VAE 也從它拿
        let cases = [
            json!({"mode": "img2img", "init_image": "in.png", "denoise": 0.6}),
            json!({"mode": "inpaint", "init_image": "in.png", "mask": "m0.png", "inpaint": {"area": "masked", "method": "lllite"}}),
            json!({"mode": "detail", "init_image": "in.png", "detail": {"masks": ["m0.png"]}}),
            json!({"mode": "upscale", "init_image": "in.png", "upscale": {"scale": 2, "refine": true}}),
        ];
        for c in cases {
            let mut q = p.clone();
            q["hires"] = json!({});
            for (k, v) in c.as_object().unwrap() {
                q[k] = v.clone();
            }
            let (n, _) = build(&q, &dir, 0).unwrap_or_else(|e| panic!("{c}: {e}"));
            assert_eq!(by_class(&n, "CheckpointLoaderSimple").len(), 1, "{c}");
            for cl in ["UNETLoader", "CLIPLoader", "VAELoader", "AnimaLLLiteApply"] {
                assert!(by_class(&n, cl).is_empty(), "{c} 不該有 {cl}");
            }
            let ck_id = n.iter().find(|(_, v)| v["class_type"] == "CheckpointLoaderSimple").unwrap().0.clone();
            assert!(by_class(&n, "VAEEncode").iter().all(|x| x["inputs"]["vae"] == json!([ck_id, 2])), "{c}");
        }
        assert!(is_fast_sdxl("sdxl_lightning_8step.safetensors") && is_fast_sdxl("noobai_DMD2.safetensors") && !is_fast_sdxl("NoobAI-XL-v1.1.safetensors"));
    }

    #[test]
    fn moss_layout_prompt() {
        // 官方 layout.md 的例子：1536×1024 的杯子 [176,224,800,864]（xyxy 像素）→ [-288,-592,352,32]
        let p = json!({"layout": [
            {"type": "object", "desc": "a {white|white} ceramic cup", "box": [176.0 / 1536.0, 224.0 / 1024.0, 800.0 / 1536.0, 864.0 / 1024.0]},
            {"type": "text", "desc": "", "text": "把日常，过成诗。", "box": [0.6, 0.25, 0.9, 0.45]},
            {"type": "character", "desc": "", "box": [0.1, 0.1, 0.5, 0.5]},
            {"type": "character", "desc": "too small", "box": [0.1, 0.1, 0.105, 0.5]}]});
        let mut rng = PyRandom::new(1);
        let l = Layout::from_params(&p, " warm studio photo ", &mut rng).unwrap();
        let v: Value = serde_json::from_str(&l.prompt(1536, 1024)).unwrap();
        assert_eq!(v["summary"]["caption"], "warm studio photo");
        assert_eq!(v["bbox_coord_space"], "abs_centralized");
        assert_eq!(v["bbox_format"], "yxyx");
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "沒描述的框、太小的框不要");
        assert_eq!(items[0], json!({"label": "object", "caption": "a white ceramic cup", "bbox": [-288, -592, 352, 32]}));
        assert_eq!(items[1]["label"], "text");
        assert_eq!(items[1]["text"], "把日常，过成诗。");
        assert_eq!(items[1]["caption"], "clear, legible lettering");
        // 一行、鍵的順序照官方
        let s = l.prompt(1536, 1024);
        assert!(!s.contains('\n') && s.starts_with("{\"summary\""));
        assert_eq!(s, concat!(
            r#"{"summary":{"caption":"warm studio photo"},"items":[{"label":"object","caption":"a white ceramic cup","bbox":[-288,-592,352,32]},"#,
            r#"{"label":"text","caption":"clear, legible lettering","text":"把日常，过成诗。","bbox":[-256,153,-51,615]}],"#,
            r#""bbox_coord_space":"abs_centralized","bbox_format":"yxyx"}"#,
        ));

        // 接進節點圖：MOSS 才包成 JSON；圖庫記的 prompt 還是使用者寫的那句；高解析修復照放大後的尺寸重算
        let dir = std::env::temp_dir();
        let mut q = json!({"mode": "txt2img", "family": "moss", "model": "m.safetensors", "prompt": "a cat {red|red} sofa", "negative": "",
                           "seed": 1, "width": 1024, "height": 1024, "steps": 25, "cfg": 3, "sampler": "euler", "scheduler": "sgm_uniform",
                           "layout": [{"type": "character", "desc": "a cat", "box": [0.25, 0.25, 0.75, 0.75]}],
                           "hires": {"enabled": true, "scale": 1.5, "denoise": 0.4}});
        let texts = |n: &Map<String, Value>| -> Vec<String> {
            n.values().filter(|v| v["class_type"] == "CLIPTextEncode").map(|v| v["inputs"]["text"].as_str().unwrap().to_string()).collect()
        };
        let (n, info) = build(&q, &dir, 0).unwrap();
        assert_eq!(info["prompt"], "a cat red sofa");
        let t = texts(&n);
        let lays: Vec<Value> = t.iter().filter_map(|x| serde_json::from_str::<Value>(x).ok()).collect();
        assert_eq!(lays.len(), 2);
        assert_eq!(lays[0]["summary"]["caption"], "a cat red sofa");
        assert_eq!(lays[0]["items"][0]["bbox"], json!([-256, -256, 256, 256]));
        assert_eq!(lays[1]["items"][0]["bbox"], json!([-384, -384, 384, 384]));
        let mut with_metadata = q.clone();
        with_metadata["layout_summary"] = json!({"meta": ["illustration"], "canvas": "single", "palette": ["red"]});
        with_metadata["layout"][0]["tags"] = json!(["{tabby|calico}"]);
        with_metadata["layout"][0]["palette"] = json!(["brown"]);
        let (n, _) = build(&with_metadata, &dir, 0).unwrap();
        let lays: Vec<Value> = texts(&n).iter().filter_map(|text| serde_json::from_str(text).ok()).collect();
        assert_eq!(lays.len(), 2);
        for (lay, radius) in lays.iter().zip([256, 384]) {
            assert_eq!(lay["summary"], json!({"meta": ["illustration"], "canvas": "single", "palette": ["red"], "caption": "a cat red sofa"}));
            assert_eq!(lay["items"][0]["palette"], json!(["brown"]));
            assert_eq!(lay["items"][0]["bbox"], json!([-radius, -radius, radius, radius]));
        }
        let tags = &lays[0]["items"][0]["tags"];
        assert!(tags == &json!(["tabby"]) || tags == &json!(["calico"]));
        assert_eq!(tags, &lays[1]["items"][0]["tags"]);
        // 版面框＋色彩／深度控制＋高解析修復：兩段的正負條件都要來自同一顆控制節點，第二段照新尺寸重新包一次
        let mut qc = q.clone();
        qc["moss_control"] = json!({"enabled": true, "image": "ctl.png", "color": false});
        let (n, _) = build(&qc, &dir, 0).unwrap();
        let ks: Vec<&Value> = n.values().filter(|v| v["class_type"] == "KSampler").collect();
        assert_eq!(ks.len(), 2);
        let src = |k: &Value, side: &str| k["inputs"][side][0].as_str().unwrap().to_string();
        for k in &ks {
            assert_eq!(src(k, "positive"), src(k, "negative"), "正負條件要來自同一顆控制節點");
            assert_eq!(n[&src(k, "positive")]["class_type"], "MOSSImageControl");
        }
        assert_ne!(src(ks[0], "positive"), src(ks[1], "positive"), "第二段重新包一次");
        q["family"] = json!("anima");
        let (n, _) = build(&q, &dir, 0).unwrap();
        assert!(texts(&n).iter().all(|x| !x.starts_with('{')), "Anima 不吃排版 JSON");
        q["family"] = json!("moss");
        q["layout"] = json!([]);
        let (n, _) = build(&q, &dir, 0).unwrap();
        assert!(texts(&n).contains(&"a cat red sofa".to_string()));
    }

    #[test]
    fn moss_layout_metadata_key_order_and_wildcards() {
        let p = json!({
            "layout_summary": {"meta": [" {poster|poster} ", " ", 5], "canvas": " multi_panel ", "palette": [" {red|red} "]},
            "layout": [{"type": "text", "desc": " serif ", "text": " Title ", "box": [0, 0, 1, 1],
                        "tags": [" {headline|headline} ", " ", false], "palette": [" {ivory|ivory} "]}],
        });
        let layout = Layout::from_params(&p, " cover ", &mut PyRandom::new(1)).unwrap();
        assert_eq!(layout.prompt(256, 128), concat!(
            r#"{"summary":{"meta":["poster"],"canvas":"multi_panel","palette":["red"],"caption":"cover"},"#,
            r#""items":[{"label":"text","caption":"serif","text":" Title ","tags":["headline"],"palette":["ivory"],"bbox":[-64,-128,64,128]}],"#,
            r#""bbox_coord_space":"abs_centralized","bbox_format":"yxyx"}"#,
        ));
    }

    #[test]
    fn moss_layout_summary_fallback() {
        let mut p = json!({
            "summary": {"meta": [" poster "], "canvas": " collage ", "palette": [" blue "], "caption": "ignored"},
            "layout": [{"desc": "cup", "box": [0, 0, 1, 1]}],
        });
        let prompt = |params: &Value| -> Value {
            let layout = Layout::from_params(params, "scene", &mut PyRandom::new(1)).unwrap();
            serde_json::from_str(&layout.prompt(256, 128)).unwrap()
        };
        let expected = json!({"meta": ["poster"], "canvas": "collage", "palette": ["blue"], "caption": "scene"});
        assert_eq!(prompt(&p)["summary"], expected);
        p["layout_summary"] = json!("ignored");
        assert_eq!(prompt(&p)["summary"], expected);
        p["layout_summary"] = json!({"meta": ["comic"], "canvas": "single"});
        assert_eq!(prompt(&p)["summary"], json!({"meta": ["comic"], "canvas": "single", "palette": ["blue"], "caption": "scene"}));
        p["summary"] = json!("ignored");
        assert_eq!(prompt(&p)["summary"], json!({"meta": ["comic"], "canvas": "single", "caption": "scene"}));
    }

    #[test]
    fn moss_layout_omits_invalid_and_empty_metadata() {
        for canvas in [json!("triptych"), json!("  "), json!(7), Value::Null] {
            let p = json!({
                "layout_summary": {"meta": [], "canvas": canvas, "palette": ["  ", null]},
                "summary": {"canvas": "single", "meta": ["fallback"], "palette": ["fallback"]},
                "layout": [{"desc": "cup", "text": "ignored", "box": [0, 0, 1, 1], "tags": [" "], "palette": []}],
            });
            let layout = Layout::from_params(&p, "", &mut PyRandom::new(1)).unwrap();
            let prompt: Value = serde_json::from_str(&layout.prompt(256, 128)).unwrap();
            assert_eq!(prompt["summary"], json!({"caption": "cup"}));
            assert_eq!(prompt["items"], json!([{"label": "object", "caption": "cup", "bbox": [-64, -128, 64, 128]}]));
        }
    }

    #[test]
    fn moss_edit_uses_reference_conditioning() {
        // 2026-10-30 MOSS 照指令改圖（編輯 LoRA，ComfyUI PR #17296／#17320 移植進 comfy_moss 補丁）：參考圖進 TextEncodeMOSSImageEdit、從空白畫、重繪 1
        let dir = std::env::temp_dir();
        let img = dir.join("aw_test_moss_edit.png");
        image::RgbImage::new(768, 1344).save(&img).unwrap();
        let mut p = json!({"mode": "img2img", "family": "moss", "model": "moss_image_sft_int8_convrot.safetensors", "prompt": "make the blazer red", "negative": "",
                       "seed": 1, "steps": 20, "cfg": 3, "sampler": "euler", "scheduler": "sgm_uniform", "init_image": "aw_test_moss_edit.png",
                       "size_mode": "auto", "denoise": 0.6, "moss_edit": "auto",
                       "loras": [{"name": "moss/moss_edit_chinoll.safetensors", "strength": 1.0, "enabled": true}]});
        let (n, info) = build(&p, &dir, 0).unwrap();
        let enc: Vec<&Value> = n.values().filter(|v| v["class_type"] == "TextEncodeMOSSImageEdit").collect();
        assert_eq!(enc.len(), 2, "正、負各一個");
        assert!(enc.iter().all(|e| e["inputs"]["edit_format"] == "Moss-Image-Edit" && e["inputs"]["images.image_0"].is_array() && e["inputs"]["vae"].is_array()));
        assert!(!n.values().any(|v| v["class_type"] == "CLIPTextEncode" || v["class_type"] == "VAEEncode"));
        assert!(n.values().any(|v| v["class_type"] == "EmptyFlux2LatentImage"));
        assert_eq!(n.values().find(|v| v["class_type"] == "KSampler").unwrap()["inputs"]["denoise"], 1.0);
        let (w, h) = (info["width"].as_i64().unwrap(), info["height"].as_i64().unwrap());
        assert!(w % 16 == 0 && h % 16 == 0 && w * h <= 512 * 1024 + 16 * 1024 && h > w, "8GB 顯卡：參考圖約 0.5MP，{w}x{h}");
        // 別的編輯 LoRA（Civitai 的 ai-toolkit 訓練）用 ai-toolkit 版面；也可以直接指定
        p["loras"] = json!([{"name": "moss/anime2real_edit.safetensors", "strength": 1.0, "enabled": true}]);
        let (n, _) = build(&p, &dir, 0).unwrap();
        assert!(n.values().any(|v| v["class_type"] == "TextEncodeMOSSImageEdit" && v["inputs"]["edit_format"] == "ai-toolkit"));
        p["moss_edit"] = json!("Moss-Image-Edit");
        let (n, _) = build(&p, &dir, 0).unwrap();
        assert!(n.values().any(|v| v["class_type"] == "TextEncodeMOSSImageEdit" && v["inputs"]["edit_format"] == "Moss-Image-Edit"));
        p["moss_edit"] = json!("nope");
        assert!(build(&p, &dir, 0).is_err());
        let _ = std::fs::remove_file(img);
    }

    #[test]
    fn moss_img2img_and_no_multiref() {
        let dir = std::env::temp_dir();
        let img = dir.join("aw_test_moss_i2i.png");
        image::RgbImage::new(832, 1216).save(&img).unwrap();
        let p = json!({"mode": "img2img", "family": "moss", "model": "moss_image_sft_int8_convrot.safetensors", "prompt": "a cat", "negative": "",
                       "seed": 1, "steps": 25, "cfg": 3, "sampler": "euler", "scheduler": "sgm_uniform", "init_image": "aw_test_moss_i2i.png",
                       "size_mode": "auto", "pixels": 1048576, "denoise": 0.85});
        let (n, info) = build(&p, &dir, 0).unwrap();
        let ks = n.values().find(|v| v["class_type"] == "KSampler").unwrap();
        assert_eq!(ks["inputs"]["denoise"], 0.85);
        assert!(n.values().any(|v| v["class_type"] == "VAEEncode"));
        assert!(!n.values().any(|v| v["class_type"] == "EmptyFlux2LatentImage"));
        assert_eq!(info["width"].as_i64().unwrap() % 16, 0);
        // 姿勢參考圖：MOSS 從深度圖出發重繪 0.95；Anima 改接 LLLite（深度＋骨架），還是從空白開始畫
        let pose = json!({"mode": "txt2img", "family": "moss", "model": "m.safetensors", "prompt": "a", "negative": "", "seed": 1, "steps": 25, "cfg": 3,
                          "width": 832, "height": 1216, "pose_control": {"depth": "cowgirl_depth.png", "pose": "cowgirl_pose.png"}});
        let (n, _) = build(&pose, &dir, 0).unwrap();
        assert!(n.values().any(|v| v["class_type"] == "LoadImage" && v["inputs"]["image"] == "anima_pose/cowgirl_depth.png"));
        assert_eq!(n.values().find(|v| v["class_type"] == "KSampler").unwrap()["inputs"]["denoise"], 0.95);
        let mut a = pose.clone();
        a["family"] = json!("anima");
        let (n, _) = build(&a, &dir, 0).unwrap();
        assert_eq!(n.values().filter(|v| v["class_type"] == "AnimaLLLiteApply").count(), 2);
        assert_eq!(n.values().find(|v| v["class_type"] == "KSampler").unwrap()["inputs"]["denoise"], 1.0);
        let mut m = p.clone();
        m["mode"] = json!("multiref");
        assert!(build(&m, &dir, 0).unwrap_err().0.contains("支援 Qwen 2.1"));
        let _ = std::fs::remove_file(img);
    }

    #[test]
    fn adapt_mmimage_refs_preserves_compounds() {
        assert_eq!(adapt_mmimage_refs("把图1的衣服换成<image2>的款式"), "把<Picture 1>的衣服换成<Picture 2>的款式");
        assert_eq!(adapt_mmimage_refs("保留地圖2、截圖1、原圖1、四視圖1，但把圖1改成圖2"), "保留地圖2、截圖1、原圖1、四視圖1，但把<Picture 1>改成<Picture 2>");
    }

    #[test]
    fn mmimage_all_edit_modes_and_rembg_upscale() {
        let dir = play_dir("mm_edits", 768, 512, &[(100, 100, 300, 300)]);
        let base = json!({"family": "mmimage", "model": MMIMG_DEFAULT_MODEL, "prompt": "edit <image1>", "negative": "",
                          "seed": 7, "steps": 6, "cfg": 1, "sampler": "euler", "scheduler": "simple", "init_image": "in.png"});
        // img2img (denoise=1.0 → KSampler 完整 6 步參考改圖；denoise<1.0 → KSamplerAdvanced PDD 切步)
        let mut p = base.clone();
        p["mode"] = json!("img2img");
        p["denoise"] = json!(1.0);
        let (n, info) = build(&p, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "KSampler").len(), 1);
        assert!(by_class(&n, "VAEEncode").is_empty());
        assert_eq!(by_class(&n, "MiniMaxH3ImageConditioning")[0]["inputs"]["prompt"], "edit <Picture 1>");
        assert_eq!(info["width"].as_i64().unwrap() % 32, 0);
        p["denoise"] = json!(0.5);
        let (n, _) = build(&p, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "KSamplerAdvanced").len(), 1);
        assert_eq!(by_class(&n, "KSamplerAdvanced")[0]["inputs"]["start_at_step"], 3);
        assert_eq!(by_class(&n, "VAEEncode").len(), 1);
        // inpaint & outpaint & detail
        let mut inp = base.clone();
        inp["mode"] = json!("inpaint");
        inp["mask"] = json!("m0.png");
        let (n, _) = build(&inp, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "SetLatentNoiseMask").len(), 1);
        assert_eq!(by_class(&n, "MiniMaxH3ImageConditioning").len(), 1);
        let mut outp = base.clone();
        outp["mode"] = json!("outpaint");
        outp["outpaint"] = json!({"left": 128, "right": 128, "top": 0, "bottom": 0, "feather": 40});
        let (n, _) = build(&outp, &dir, 0).unwrap();
        let cond = &by_class(&n, "MiniMaxH3ImageConditioning")[0]["inputs"];
        // 餵未補灰邊原圖（LoadImage）進 MiniMaxH3ImageConditioning
        assert_eq!(n[cond["images.image_1"][0].as_str().unwrap()]["class_type"], "LoadImage");
        let mut det = base.clone();
        det["mode"] = json!("detail");
        det["denoise"] = json!(0.4);
        det["detail"] = json!({"masks": ["m0.png"]});
        let (n, _) = build(&det, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "KSamplerAdvanced").len(), 1);
        assert_eq!(by_class(&n, "SetLatentNoiseMask").len(), 1);
        // rembg & upscale (refine=false / true) 不再被攔截
        let mut rb = base.clone();
        rb["mode"] = json!("rembg");
        let (n, _) = build(&rb, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "RemoveBackground").len(), 1);
        let mut up = base.clone();
        up["mode"] = json!("upscale");
        up["upscale"] = json!({"scale": 2, "refine": false});
        let (n, _) = build(&up, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "ImageUpscaleWithModel").len(), 1);
        assert!(by_class(&n, "KSamplerAdvanced").is_empty());
        up["upscale"] = json!({"scale": 2, "refine": true, "denoise": 0.3});
        let (n, _) = build(&up, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "KSamplerAdvanced").len(), 1);
        // strip / sheet / diff 指定 engine: "mmimage"
        let mut st = base.clone();
        st["family"] = json!("anima");
        st["model"] = json!("anima-turbo-v1.1.safetensors");
        st["mode"] = json!("strip");
        st["strip"] = json!({"engine": "mmimage", "refine": false, "qwen_prompt": "edit image"});
        let (n, info) = build(&st, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "MiniMaxH3ImageConditioning").len(), 1);
        assert_eq!(info["family"], "mmimage");
        let mut sh = st.clone();
        sh["mode"] = json!("sheet");
        sh["sheet"] = json!({"engine": "mmimage", "req": "把图1的上衣改黑"});
        let (n, info) = build(&sh, &dir, 0).unwrap();
        assert!(by_class(&n, "MiniMaxH3ImageConditioning")[0]["inputs"]["prompt"].as_str().unwrap().contains("<Picture 1>"));
        assert_eq!(info["family"], "mmimage");
        let mut df = st.clone();
        df["mode"] = json!("diff");
        df["diff"] = json!({"engine": "mmimage", "req": "微笑"});
        let (n, info) = build(&df, &dir, 0).unwrap();
        assert!(by_class(&n, "MiniMaxH3ImageConditioning")[0]["inputs"]["prompt"].as_str().unwrap().contains("<Picture 1>"));
        assert_eq!(info["family"], "mmimage");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_graph() {
        let dir = std::env::temp_dir();
        let img = dir.join("aw_test_strip.png");
        image::RgbImage::new(1024, 1536).save(&img).unwrap();
        // 改圖指令和精修 tag 由內容補丁算好塞進 params（prepare）；graph 只負責把它們放進節點
        let p = json!({"mode": "strip", "family": "anima", "model": "anima-turbo-v1.1.safetensors", "prompt": "", "negative": "", "seed": 5,
                       "init_image": "aw_test_strip.png", "loras": [{"name": "style.safetensors", "strength": 0.8}],
                       "strip": {"qwen_prompt": "改圖指令 A", "refine_tags": "refine tags A"}});
        let (n, info) = build(&p, &dir, 0).unwrap();
        let all = |c: &str| n.values().filter(|v| v["class_type"] == c).cloned().collect::<Vec<_>>();
        // 兩段：Qwen 官方 Turbo 改圖＋Anima 精修，各自的文字編碼器
        let unets: Vec<_> = all("UNETLoader").iter().map(|v| v["inputs"]["unet_name"].as_str().unwrap().to_string()).collect();
        assert_eq!(unets, vec![STRIP_QWEN.to_string(), STRIP_REFINE.to_string()]);
        assert_eq!(all("CLIPLoader").len(), 2);
        // params 的 qwen_prompt 進 Qwen 文字編碼，refine_tags 進 Anima 文字編碼
        let te = &all("TextEncodeQwenImage21")[0]["inputs"];
        assert_eq!(te["prompt"], "改圖指令 A");
        assert!(te.get("images.image_1").is_some());
        assert!(n.values().any(|v| v["inputs"]["text"] == "refine tags A"));
        // Anima 生成頁掛的 LoRA 只套在精修那段
        assert_eq!(all("LoraLoaderModelOnly").len(), 1);
        assert_eq!(all("KSampler")[0]["inputs"]["denoise"], 0.35);
        assert_eq!(all("ImageBlend")[0]["inputs"]["blend_mode"], "difference");
        assert_eq!(all("ImageCompositeMasked").len(), 1);
        assert_eq!(all("SaveImage")[0]["inputs"]["filename_prefix"], "AnimaWeb/strip");
        assert_eq!(info["mode"], "strip");
        assert_eq!(info["family"], "qwen21");
        // 精修那段約 150 萬像素：1024×1536 的原圖會回到原尺寸
        assert_eq!((info["width"].as_i64().unwrap(), info["height"].as_i64().unwrap()), (1024, 1536));
        // 不精修：只有 Qwen 那段，沒有貼回
        let mut p2 = p.clone();
        p2["strip"] = json!({"refine": false, "qwen_prompt": "只改這段"});
        let (n2, info2) = build(&p2, &dir, 0).unwrap();
        assert!(!n2.values().any(|v| v["class_type"] == "KSampler" || v["class_type"] == "ImageBlend"));
        assert!(n2.values().any(|v| v["class_type"] == "TextEncodeQwenImage21" && v["inputs"]["prompt"] == "只改這段"));
        assert_eq!((info2["width"].as_i64().unwrap(), info2["height"].as_i64().unwrap()), (1024, 1536));
        // 832×1216 的原圖：中間拉伸不裁切，最後拉回原尺寸（以前出來是 1024×1536）
        image::RgbImage::new(832, 1216).save(&img).unwrap();
        let (n4, info4) = build(&p, &dir, 0).unwrap();
        assert_eq!((info4["width"].as_i64().unwrap(), info4["height"].as_i64().unwrap()), (832, 1216));
        let scales: Vec<_> = n4.values().filter(|v| v["class_type"] == "ImageScale").map(|v| (v["inputs"]["width"].clone(), v["inputs"]["crop"].clone())).collect();
        assert!(scales.contains(&(json!(832), json!("disabled"))), "{scales:?}");
        assert!(scales.iter().all(|(_, c)| c == "disabled"), "{scales:?}");
        // 精修但不貼回
        let mut p3 = p.clone();
        p3["strip"] = json!({"merge": false, "refine_denoise": 0.4, "qwen_prompt": "x", "refine_tags": "y"});
        let (n3, _) = build(&p3, &dir, 0).unwrap();
        assert!(!n3.values().any(|v| v["class_type"] == "ImageCompositeMasked"));
        let _ = std::fs::remove_file(img);
    }

    #[test]
    fn sheet_graph() {
        let dir = std::env::temp_dir();
        let sheet = dir.join("aw_test_sheet.png");
        let cloth = dir.join("aw_test_sheet_cloth.png");
        image::RgbImage::new(1536, 1024).save(&sheet).unwrap();
        image::RgbImage::new(832, 1216).save(&cloth).unwrap();
        let p = json!({"mode": "sheet", "family": "anima", "model": "anima-turbo-v1.1.safetensors", "prompt": "", "negative": "", "seed": 3,
                       "init_image": "aw_test_sheet.png", "loras": [{"name": "style.safetensors", "strength": 0.8}],
                       "sheet": {"req": "把上衣改成黑色", "ref": null}});
        let (n, info) = build(&p, &dir, 0).unwrap();
        let all = |n: &Map<String, Value>, c: &str| n.values().filter(|v| v["class_type"] == c).cloned().collect::<Vec<_>>();
        assert_eq!(all(&n, "UNETLoader")[0]["inputs"]["unet_name"], STRIP_QWEN);
        assert!(all(&n, "LoraLoaderModelOnly").is_empty());  // Anima 的 LoRA 不套到 Qwen
        let te = &all(&n, "TextEncodeQwenImage21")[0]["inputs"];
        let pr = te["prompt"].as_str().unwrap();
        assert!(pr.starts_with("【本次修改要求】\n把上衣改成黑色") && pr.contains("四视图排版") && !pr.contains("【有图2时】"));
        assert!(te.get("images.image_1").is_some() && te.get("images.image_2").is_none());
        assert_eq!(all(&n, "ManualSigmas").len(), 1);  // 官方 Turbo 8 步
        assert_eq!(info["mode"], "sheet");
        assert_eq!(info["family"], "qwen21");
        let (w, h) = (info["width"].as_i64().unwrap(), info["height"].as_i64().unwrap());
        assert!(w % 32 == 0 && h % 32 == 0 && w > h, "{w}x{h}");
        // 有服裝參考：第二張圖、加「有图2时」那段
        let mut p2 = p.clone();
        p2["sheet"]["ref"] = json!("aw_test_sheet_cloth.png");
        let (n2, _) = build(&p2, &dir, 0).unwrap();
        let te2 = &all(&n2, "TextEncodeQwenImage21")[0]["inputs"];
        assert!(te2.get("images.image_2").is_some() && te2["prompt"].as_str().unwrap().contains("【有图2时】"));
        // 沒寫要求要擋
        let mut p3 = p.clone();
        p3["sheet"]["req"] = json!("  ");
        assert!(build(&p3, &dir, 0).unwrap_err().0.contains("修改要求"));
        let _ = std::fs::remove_file(sheet);
        let _ = std::fs::remove_file(cloth);
    }

    /// 測試用的輸入資料夾：一張 w×h 的圖＋（可選）幾張遮罩（白色矩形）
    fn play_dir(tag: &str, w: u32, h: u32, rects: &[(u32, u32, u32, u32)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aw_test_{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(w, h).save(dir.join("in.png")).unwrap();
        for (k, r) in rects.iter().enumerate() {
            let mut m = image::GrayImage::new(w, h);
            for y in r.1..r.3 {
                for x in r.0..r.2 {
                    m.put_pixel(x, y, image::Luma([255]));
                }
            }
            m.save(dir.join(format!("m{k}.png"))).unwrap();
        }
        dir
    }

    fn by_class(n: &Map<String, Value>, c: &str) -> Vec<Value> {
        n.values().filter(|v| v["class_type"] == c).cloned().collect()
    }

    #[test]
    fn headswap_locks_everything_but_the_head() {
        let dir = play_dir("headswap", 400, 300, &[]);
        image::RgbImage::new(512, 512).save(dir.join("ref.png")).unwrap();
        let p = json!({"mode": "headswap", "family": "qwen21", "model": STRIP_QWEN, "prompt": "head_swap: …", "negative": "", "seed": 1,
                       "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple", "init_image": "in.png", "refs": [{"image": "ref.png"}],
                       "loras": [{"name": "qwen21\\bfs_head_v1.1_qwen_2.1.safetensors", "strength": 1.0}],
                       "head": {"threshold": 0.35, "index": 0, "context": 0.6, "pixels": 1048576}});
        let (n, _) = build(&p, &dir, 0).unwrap();
        let id_of = |c: &str| n.iter().find(|(_, v)| v["class_type"] == c).map(|(k, _)| k.clone()).unwrap_or_else(|| panic!("沒有 {c}"));
        let src = n.iter().find(|(_, v)| v["class_type"] == "LoadImage" && v["inputs"]["image"] == "in.png").map(|(k, _)| k.clone()).unwrap();
        let det = &n[&id_of("SAM3_Detect")]["inputs"];
        assert_eq!((det["image"][0].as_str(), det["individual_masks"].as_bool()), (Some(src.as_str()), Some(true)));
        // SAM3 的詞不加「:N」只回 1 個（ComfyUI sam3_clip._parse_prompts）：要每顆頭都抓
        let word = &n[det["conditioning"][0].as_str().unwrap()]["inputs"]["text"];
        assert_eq!(word, HEAD_PROMPT);
        assert!(HEAD_PROMPT.ends_with(":8"));
        let crop_id = id_of("AnimaHeadCrop");
        let crop = &n[&crop_id]["inputs"];
        assert_eq!((crop["masks"][0].as_str(), crop["image"][0].as_str()), (Some(id_of("SAM3_Detect").as_str()), Some(src.as_str())));
        assert_eq!((crop["context"].as_f64(), crop["pixels"].as_i64()), (Some(0.6), Some(1048576)));
        // 只讓頭重畫：裁切 → VAEEncode → SetLatentNoiseMask（遮罩＝AnimaHeadCrop 第 1 個輸出）→ 取樣
        let nm = &n[&id_of("SetLatentNoiseMask")]["inputs"];
        assert_eq!(nm["mask"], json!([crop_id.clone(), 1]));
        assert_eq!(n[nm["samples"][0].as_str().unwrap()]["class_type"], "VAEEncode");
        let nm_id = id_of("SetLatentNoiseMask");
        assert!(n.values().any(|v| v["inputs"]["latent_image"][0] == nm_id.as_str()), "取樣要吃 SetLatentNoiseMask");
        // <image1>＝頭部裁切
        let te = &by_class(&n, "TextEncodeQwenImage21")[0]["inputs"];
        assert_eq!(te["images.image_1"], json!([crop_id.clone(), 0]));
        assert!(te["images.image_2"].is_array());
        // 貼回原圖
        let st = &n[&id_of("AnimaHeadStitch")]["inputs"];
        assert_eq!((st["image"][0].as_str(), st["stitch"].clone(), st["mask"].clone()), (Some(src.as_str()), json!([crop_id.clone(), 2]), json!([crop_id, 1])));
        assert!(by_class(&n, "LoraLoaderModelOnly").iter().any(|l| l["inputs"]["lora_name"] == "qwen21\\bfs_head_v1.1_qwen_2.1.safetensors"));
        // 指定了哪顆頭（比對挑好的遮罩檔）：不再跑 SAM3，遮罩直接進 AnimaHeadCrop
        let mut picked = p.clone();
        picked["head"]["mask"] = json!("m.png");
        let (n2, _) = build(&picked, &dir, 0).unwrap();
        assert!(by_class(&n2, "SAM3_Detect").is_empty());
        let crop2 = &by_class(&n2, "AnimaHeadCrop")[0]["inputs"];
        let lm = &n2[crop2["masks"][0].as_str().unwrap()];
        assert_eq!((lm["class_type"].as_str(), lm["inputs"]["image"].as_str(), crop2["index"].as_i64()), (Some("LoadImageMask"), Some("m.png"), Some(0)));
        // 沒參考圖：單張改頭部（補表情用），只有 <image1>
        let mut no = p.clone();
        no["refs"] = json!([]);
        let (n, _) = build(&no, &dir, 0).unwrap();
        let te = &by_class(&n, "TextEncodeQwenImage21")[0]["inputs"];
        assert!(te["images.image_1"].is_array() && te.get("images.image_2").is_none());
        assert_eq!(by_class(&n, "SetLatentNoiseMask").len(), 1);
        // 不是 Qwen：擋
        let mut moss = p.clone();
        moss["family"] = json!("moss");
        moss["model"] = json!("moss_image_sft_int8_convrot.safetensors");
        assert!(build(&moss, &dir, 0).unwrap_err().0.contains("只支援 Qwen 2.1"));
    }

    #[test]
    fn pose_control_graph() {
        let dir = std::env::temp_dir();
        let p = json!({"mode": "txt2img", "family": "anima", "model": "anima-turbo-v1.1.safetensors", "prompt": "1girl", "negative": "",
                       "seed": 1, "width": 832, "height": 1216, "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple",
                       "control": {"enabled": true, "type": "pose", "preprocess": "none", "image": "skel.png", "strength": 1.0, "end": 0.8}});
        let (n, _) = build(&p, &dir, 0).unwrap();
        let ll = &by_class(&n, "AnimaLLLiteApply")[0]["inputs"];
        let patch = &n[ll["model_patch"][0].as_str().unwrap()];
        assert_eq!(patch["inputs"]["name"], "anima-lllite-pose-1.safetensors");
        assert_eq!(ll["end_percent"], 0.8);
        // 自己畫好的骨架圖：直接用，不抽
        assert!(by_class(&n, "SDPoseKeypointExtractor").is_empty());
        // 從照片抽骨架：SAM3 框人 → SDPose 抽點 → 畫成骨架圖，再接 LLLite
        let mut q = p.clone();
        q["control"]["preprocess"] = json!("pose");
        let (n, _) = build(&q, &dir, 0).unwrap();
        let ex = &by_class(&n, "SDPoseKeypointExtractor")[0]["inputs"];
        assert_eq!(n[ex["model"][0].as_str().unwrap()]["inputs"]["ckpt_name"], SDPOSE_CKPT);
        assert_eq!(ex["vae"][1], 2);
        let det = &n[ex["bboxes"][0].as_str().unwrap()];
        assert_eq!((det["class_type"].as_str(), ex["bboxes"][1].as_i64()), (Some("SAM3_Detect"), Some(1)));
        let draw = by_class(&n, "SDPoseDrawKeypoints");
        assert_eq!(draw.len(), 1);
        // 控制圖縮成畫面大小後才進 LLLite
        let ll = &by_class(&n, "AnimaLLLiteApply")[0]["inputs"];
        let scaled = &n[ll["image"][0].as_str().unwrap()];
        assert_ne!(scaled["class_type"], "SDPoseDrawKeypoints");
        // 疊兩張（陣列）：深度＋骨架各一個 LLLite，照順序接
        let mut two = p.clone();
        two["control"] = json!([{"enabled": true, "type": "depth", "preprocess": "none", "image": "d.png", "strength": 0.8, "end": 0.6},
                                {"enabled": true, "type": "pose", "preprocess": "none", "image": "skel.png"}]);
        let (n, _) = build(&two, &dir, 0).unwrap();
        let names: Vec<String> = by_class(&n, "ModelPatchLoader").iter().map(|v| v["inputs"]["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"anima-lllite-depth-1.safetensors".into()) && names.contains(&"anima-lllite-pose-1.safetensors".into()));
        assert_eq!(by_class(&n, "AnimaLLLiteApply").len(), 2);
        // 體位庫的姿勢參考圖：Anima 也吃（深度 0.8／前 60%，骨架 1.0）
        let mut pc = p.clone();
        pc["control"] = Value::Null;
        pc["pose_control"] = json!({"depth": "cowgirl_depth.png", "pose": "cowgirl_pose.png"});
        let (n, _) = build(&pc, &dir, 0).unwrap();
        let ll = by_class(&n, "AnimaLLLiteApply");
        assert_eq!(ll.len(), 2);
        let depth = ll.iter().find(|v| n[v["inputs"]["model_patch"][0].as_str().unwrap()]["inputs"]["name"] == "anima-lllite-depth-1.safetensors").unwrap();
        assert_eq!((depth["inputs"]["strength"].as_f64(), depth["inputs"]["end_percent"].as_f64()), (Some(0.8), Some(0.6)));
        assert!(by_class(&n, "LoadImage").iter().any(|v| v["inputs"]["image"] == format!("{POSE_INPUT}/cowgirl_pose.png")));
        // Qwen 2.1／MOSS 沒有 LLLite：控制整段不進圖
        let mut m = q.clone();
        m["family"] = json!("moss");
        m["model"] = json!("moss_image_sft_int8_convrot.safetensors");
        let (n, _) = build(&m, &dir, 0).unwrap();
        assert!(by_class(&n, "SDPoseKeypointExtractor").is_empty() && by_class(&n, "AnimaLLLiteApply").is_empty());
    }

    /// MOSS 色彩／深度控制（2026-10-31，ComfyUI 0.43 內建 MOSSImageControl＋darask0 的轉接器 LoRA）：
    /// 一張參考圖 → DA3 深度圖＋模糊後減色的色塊圖 → 條件；轉接器用一般 LoRA 載入器掛在模型上
    #[test]
    fn moss_control_maps_and_adapter() {
        let dir = std::env::temp_dir();
        let p = json!({"mode": "txt2img", "family": "moss", "model": "moss_image_sft_int8_convrot.safetensors", "prompt": "a girl", "negative": "",
                       "seed": 1, "width": 768, "height": 1024, "steps": 20, "cfg": 3, "sampler": "euler", "scheduler": "sgm_uniform",
                       "moss_control": {"enabled": true, "image": "ref.png", "strength": 0.8, "end": 0.6}});
        let (n, _) = build(&p, &dir, 0).unwrap();
        let ctl = &by_class(&n, "MOSSImageControl")[0]["inputs"];
        assert_eq!((ctl["strength"].as_f64(), ctl["end_percent"].as_f64()), (Some(0.8), Some(0.6)));
        assert!(ctl.get("depth_map").is_some() && ctl.get("color_map").is_some());
        assert!(!by_class(&n, "DA3Inference").is_empty() && !by_class(&n, "ImageQuantize").is_empty());
        assert!(by_class(&n, "LoraLoaderModelOnly").iter().any(|v| v["inputs"]["lora_name"] == MOSS_CONTROL_LORA));
        // 取樣器吃的是控制後的條件
        let ks = &by_class(&n, "KSampler")[0]["inputs"];
        assert_eq!(n[ks["positive"][0].as_str().unwrap()]["class_type"], "MOSSImageControl");
        // 只要深度
        let mut d = p.clone();
        d["moss_control"]["color"] = json!(false);
        let (n, _) = build(&d, &dir, 0).unwrap();
        let ctl = &by_class(&n, "MOSSImageControl")[0]["inputs"];
        assert!(ctl.get("color_map").is_none() && ctl.get("depth_map").is_some());
        // 姿勢參考圖＋有轉接器：深度圖直接當控制、前一半步數，從空白畫（不再用重繪 0.95 的老辦法）
        let mut pc = p.clone();
        pc["moss_control"] = Value::Null;
        pc["pose_control"] = json!({"depth": "cowgirl_depth.png", "pose": "cowgirl_pose.png", "adapter": MOSS_CONTROL_LORA});
        let (n, _) = build(&pc, &dir, 0).unwrap();
        let ctl = &by_class(&n, "MOSSImageControl")[0]["inputs"];
        assert_eq!(ctl["end_percent"].as_f64(), Some(0.5));
        assert!(ctl.get("color_map").is_none());
        assert_eq!(by_class(&n, "KSampler")[0]["inputs"]["denoise"], 1.0);
        assert!(!by_class(&n, "EmptyFlux2LatentImage").is_empty());
        // 沒轉接器：照舊從深度圖重繪 0.95
        pc["pose_control"]["adapter"] = Value::Null;
        let (n, _) = build(&pc, &dir, 0).unwrap();
        assert!(by_class(&n, "MOSSImageControl").is_empty());
        assert_eq!(by_class(&n, "KSampler")[0]["inputs"]["denoise"], 0.95);
        // 別的家族不吃
        let mut a = p.clone();
        a["family"] = json!("anima");
        let (n, _) = build(&a, &dir, 0).unwrap();
        assert!(by_class(&n, "MOSSImageControl").is_empty());
    }

    /// MOSS 4B 文字編碼器（2026-10-31）：設定選 4B → 繪匣附帶的 AnimaMOSS4BCLIPLoader（4B＋投影轉接器）取代 9B 的 CLIPLoader
    #[test]
    fn moss_4b_encoder_uses_projection_loader() {
        let dir = std::env::temp_dir();
        let mut p = json!({"mode": "txt2img", "family": "moss", "model": "moss_image_sft_int8_convrot.safetensors", "prompt": "a cat", "negative": "",
                           "seed": 1, "width": 768, "height": 1024, "steps": 20, "cfg": 3, "sampler": "euler", "scheduler": "sgm_uniform", "moss_te": "4b"});
        let (n, _) = build(&p, &dir, 0).unwrap();
        let ld = &by_class(&n, "AnimaMOSS4BCLIPLoader")[0]["inputs"];
        assert_eq!((ld["clip_name"].as_str(), ld["adapter_name"].as_str()), (Some(MOSS_TE_4B), Some(MOSS_4B_ADAPTER)));
        assert!(by_class(&n, "CLIPLoader").is_empty());
        // 預設 9B 照舊；別的家族不理這個設定
        p["moss_te"] = json!("9b");
        let (n, _) = build(&p, &dir, 0).unwrap();
        assert!(by_class(&n, "AnimaMOSS4BCLIPLoader").is_empty());
        p["moss_te"] = json!("4b");
        p["family"] = json!("anima");
        let (n, _) = build(&p, &dir, 0).unwrap();
        assert!(by_class(&n, "AnimaMOSS4BCLIPLoader").is_empty());
    }

    #[test]
    fn outpaint_graph() {
        let dir = play_dir("outpaint", 640, 480, &[]);
        let base = json!({"mode": "outpaint", "family": "anima", "model": "anima-turbo-v1.1.safetensors", "prompt": "a beach", "negative": "",
                          "seed": 3, "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple", "init_image": "in.png",
                          "outpaint": {"left": 100, "right": 0, "top": 0, "bottom": 64}, "inpaint": {"method": "lllite"}});
        let (n, info) = build(&base, &dir, 0).unwrap();
        let pad = &by_class(&n, "ImagePadForOutpaint")[0]["inputs"];
        // 100 → 104（8 的倍數往上取）
        assert_eq!((pad["left"].as_i64(), pad["bottom"].as_i64(), pad["feathering"].as_i64()), (Some(104), Some(64), Some(40)));
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(744), Some(544)));
        assert!(by_class(&n, "LoadImageMask").is_empty(), "擴圖不用遮罩檔");
        assert!(by_class(&n, "ImageCrop").is_empty(), "擴圖整張重畫");
        assert_eq!(by_class(&n, "KSampler")[0]["inputs"]["denoise"], 1.0);
        assert_eq!(info["prompt"], "a beach");
        // 遮罩接 ImagePadForOutpaint 的第 2 個輸出
        let comp = &by_class(&n, "ImageCompositeMasked")[0]["inputs"];
        assert_eq!(comp["destination"][1], 0);
        // 沒選邊：報錯
        let mut none = base.clone();
        none["outpaint"] = json!({});
        assert!(build(&none, &dir, 0).is_err());
        // Qwen 2.1：節點圖補延伸場景的句子，圖庫記的提示詞不變
        let mut q = base.clone();
        q["family"] = json!("qwen21");
        let (n, info) = build(&q, &dir, 0).unwrap();
        let te = by_class(&n, "TextEncodeQwenImage21");
        assert!(te[0]["inputs"]["prompt"].as_str().unwrap().starts_with(OUTPAINT_HINT));
        assert_eq!(info["prompt"], "a beach");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn detail_graph() {
        let dir = play_dir("detail", 1024, 1024, &[(400, 200, 500, 320), (100, 800, 140, 860), (0, 0, 0, 0)]);
        let p = json!({"mode": "detail", "family": "anima", "model": "anima-turbo-v1.1.safetensors", "prompt": "1girl", "negative": "",
                       "seed": 9, "steps": 8, "cfg": 1, "sampler": "euler", "scheduler": "simple", "init_image": "in.png",
                       "detail": {"masks": ["m0.png", "m1.png", "m2.png"], "extra": "detailed face"}});
        let (n, info) = build(&p, &dir, 0).unwrap();
        // 空遮罩那張跳過：兩段
        let ks = by_class(&n, "KSampler");
        assert_eq!(ks.len(), 2);
        assert_eq!(ks[0]["inputs"]["denoise"], 0.4);
        assert_ne!(ks[0]["inputs"]["seed"], ks[1]["inputs"]["seed"]);
        assert_eq!(by_class(&n, "UNETLoader").len(), 1, "共用一組模型");
        assert_eq!(by_class(&n, "CLIPTextEncode")[0]["inputs"]["text"], "1girl, detailed face");
        // 第一塊：外框 100×120，四周留 max(24, 120/3=40)
        let crop = &by_class(&n, "ImageCrop")[0]["inputs"];
        assert_eq!((crop["x"].as_i64(), crop["y"].as_i64(), crop["width"].as_i64(), crop["height"].as_i64()), (Some(360), Some(160), Some(180), Some(200)));
        // 第二段貼在第一段的結果上
        let comps = by_class(&n, "ImageCompositeMasked");
        assert_eq!(comps.len(), 2);
        let last = by_class(&n, "SaveImage")[0]["inputs"]["images"][0].as_str().unwrap().to_string();
        assert_eq!(n[&last]["class_type"], "ImageCompositeMasked");
        let prev = n[&last]["inputs"]["destination"][0].as_str().unwrap().to_string();
        assert_eq!(n[&prev]["class_type"], "ImageCompositeMasked");
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(1024), Some(1024)));
        let mut empty = p.clone();
        empty["detail"]["masks"] = json!([]);
        assert!(build(&empty, &dir, 0).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rembg_diff_detect_graphs() {
        let dir = play_dir("rembg", 512, 768, &[]);
        let p = json!({"mode": "rembg", "family": "anima", "model": "x.safetensors", "prompt": "", "negative": "", "seed": 1, "init_image": "in.png"});
        let (n, info) = build(&p, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "LoadBackgroundRemovalModel")[0]["inputs"]["bg_removal_name"], BG_MODEL);
        assert_eq!(by_class(&n, "JoinImageWithAlpha").len(), 1);
        assert!(by_class(&n, "UNETLoader").is_empty() && by_class(&n, "KSampler").is_empty());
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(512), Some(768)));

        let d = json!({"mode": "diff", "family": "anima", "model": "anima-turbo-v1.1.safetensors", "prompt": "", "negative": "", "seed": 1,
                       "init_image": "in.png", "diff": {"req": "把表情改成开心的大笑"}});
        let (n, info) = build(&d, &dir, 0).unwrap();
        assert_eq!(by_class(&n, "UNETLoader")[0]["inputs"]["unet_name"], STRIP_QWEN);
        let te = by_class(&n, "TextEncodeQwenImage21");
        assert!(te[0]["inputs"]["prompt"].as_str().unwrap().contains("开心的大笑") && te[0]["inputs"]["prompt"].as_str().unwrap().contains("保持要求"));
        assert_eq!(info["family"], "qwen21");
        let mut e = d.clone();
        e["diff"] = json!({"req": "  "});
        assert!(build(&e, &dir, 0).is_err());

        let g = detect_graph("in.png", &["face", "hand"], 0.5);
        let det = g.values().find(|v| v["class_type"] == "SAM3_Detect").unwrap();
        assert_eq!(det["inputs"]["individual_masks"], true);
        assert_eq!(g.values().filter(|v| v["class_type"] == "PreviewImage").count(), 2);
        assert_eq!(g.values().filter(|v| v["class_type"] == "CheckpointLoaderSimple").count(), 1);
        // SAM3 的詞沒寫「:N」只回 1 個（ComfyUI sam3_clip._parse_prompts）→ 一張圖好幾張臉、兩隻手都要找到
        let words: Vec<&str> = g.values().filter(|v| v["class_type"] == "CLIPTextEncode").filter_map(|v| v["inputs"]["text"].as_str()).collect();
        assert!(words.contains(&"face:8") && words.contains(&"hand:8"), "{words:?}");
        let g = detect_graph("in.png", &["head:3"], 0.5);
        assert!(g.values().any(|v| v["inputs"]["text"] == "head:3"), "自己寫了上限就照用");
        assert!(!g.values().any(|v| v["class_type"] == "SaveImage"), "偵測結果不能進 output");
        let mut m = image::GrayImage::new(50, 40);
        assert_eq!(bbox_of(&m), None);
        m.put_pixel(10, 5, image::Luma([255]));
        m.put_pixel(20, 30, image::Luma([200]));
        assert_eq!(bbox_of(&m), Some((10, 5, 21, 31)));
        let _ = std::fs::remove_dir_all(dir);
    }
}

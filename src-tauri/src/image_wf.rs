//! 生圖工作流組（2026-10-21）：使用者在 ComfyUI 做好的生圖工作流，匯入後出現在生成頁模型選單（「⚙ 名字」，值是 wf:<代號>），
//! 也可以拿來「取代內建功能」（settings 的 feature_flows）。生成頁的提示詞、負面、種子、寬高、輸入圖、遮罩、LoRA 都套進去，其他照工作流原樣。
//!
//! 存在 data\image_workflows\<代號>.json：
//! `{"name", "added", "graphs": {"<模式>": API 格式的節點圖}, "options": {"page_sampler": bool}, "family": "sdxl", "source": "import"|"builtin"}`
//! 一個名字（組）底下可以放好幾個模式：txt2img、img2img、inpaint、upscale、multiref。
//! 舊格式 `{"name", "added", "graph"}` 讀的時候當成 graphs.txt2img（不用搬檔，下次存檔才變新格式）。
//! 代號規則跟影片工作流一樣（video_wf::id_of）。
//!
//! 生圖站只改這些地方（優先看標記，沒標記才自動找；規則寫在 design\萬用工作流與匯入匯出.md）：
//! - 提示詞：文字欄位裡的 {{prompt}}；都沒寫就從取樣器的 positive 往回找唯一的文字框，整格換掉（放大模式不需要提示詞）
//! - 負面提示詞：{{negative}}；沒寫就從 negative 往回找唯一的文字框（找不到或有好幾個就不動）
//! - 輸入圖：標題含 {{image}} 的 Load Image；沒標且只有一個 Load Image 就是它
//! - 遮罩：標題含 {{mask}} 的 Load Image／Load Image (as Mask)；沒標時，輸入圖那個 Load Image 的 MASK 輸出有被接去用也行
//! - 參考圖：標題 {{ref1}}、{{ref2}}…
//! - 種子：所有 seed／noise_seed 數字欄位（每張 +1）
//! - 寬高：同時有數字 width、height、batch_size 的節點（文生圖照生成頁；batch_size 一律改 1，張數由生成頁決定）
//! - 主模型：標題含 {{model}} 的載入節點；沒標就從取樣器的 model 輸入一路往上游找到第一個載入節點
//! - 取樣參數：標題含 {{sampler}} 的節點，或「用生成頁的步數、CFG…」打開時所有 KSampler 類（有數字 steps 和 cfg）的節點
//! - LoRA：生成頁選的 LoRA 接在主模型載入節點後面（LoraLoader 鏈）；工作流原本的 LoRA 節點保留
//! - 存檔：Save Image 的檔名前綴改 AnimaWeb/<模式>（圖庫才收得到）
//! - 參數面板（params.wf_overrides）：`{"<節點編號>.<欄位>": 值}` 最後套上，比上面的自動槽位優先
//!
//! 生成頁「拿去 ComfyUI 改」匯出的內建流程，Save Image 的標題帶 {{mode:<模式>}}：匯入時模式選「自動判斷」就照它（mode_mark）；
//! 還有 {{family:<家族>}}：找不到主模型檔（放大沒有主模型）時拿它當家族（family_mark）
use crate::graphs::{self, GraphError};
use crate::py::{self, now, truthy};
use crate::pyrandom::PyRandom;
use crate::video_wf::{class, input, is_link, label, nodes, set, sorted_ids, text_candidates, title, walk_text, Cand, Slots};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const NEGATIVE: &str = "{{negative}}";
const IMAGE: &str = "{{image}}";
const MASK: &str = "{{mask}}";
const MODEL: &str = "{{model}}";
const SAMPLER: &str = "{{sampler}}";
const PROMPT: &str = "{{prompt}}";
/// 匯出內建流程時還沒放圖用的佔位圖（ComfyUI 的 input 裡）
const EXPORT_PLACEHOLDER: &str = "anima_export_placeholder.png";
/// 生成頁模型選單的值：wf:<代號>
pub const PREFIX: &str = "wf:";
/// 工作流組支援的模式（順序＝顯示順序）
pub const MODES: [&str; 5] = ["txt2img", "img2img", "inpaint", "upscale", "multiref"];
/// 主模型載入節點上放檔名的欄位（model_path：Nunchaku 這類）
const MODEL_KEYS: [&str; 4] = ["ckpt_name", "unet_name", "model_name", "model_path"];
/// 沿著 model 往上找主模型時要跳過的節點：class 名含這些字的（AnimateDiff 的 Loader 有 model_name，但放的是動態模型，不是主模型）
const NOT_MAIN_LOADERS: [&str; 4] = ["animatediff", "motion", "adapter", "upscale"];
/// 第 1 個輸出是 CLIP 的標準 Checkpoint 載入節點：LoRA 的 CLIP 才接得上去（其他載入節點的輸出順序不一定一樣，只接 model）
const CLIP_LOADERS: [&str; 7] = [
    "CheckpointLoaderSimple",
    "CheckpointLoader",
    "unCLIPCheckpointLoader",
    "CheckpointLoaderNF4",
    "CheckpointLoader|pysssss",
    "Checkpoint Loader (Simple)",
    "CheckpointLoaderSimpleShared //Inspire",
];
/// SaveImage 以外常見的存圖節點（WAS 的 Image Save、Image Saver 系列…）：出圖時換成 SaveImage，圖庫才收得到。
/// 清單以外 class 名含 Save、又有 images 輸入的也算（但不含動畫、影片、Latent、音訊、3D 這些）
const OTHER_SAVERS: [&str; 9] = [
    "Image Save",
    "Image Saver",
    "Image Saver Simple",
    "ImageSaver",
    "ImageSaverSimple",
    "SaveImageExtended",
    "Save Image Extended",
    "SaveImageWithMetaData",
    "easy imageSave",
];
/// 只預覽、不存檔的輸出節點：工作流裡沒有其他存圖節點時才算輸出
const PREVIEWERS: [&str; 2] = ["PreviewImage", "SaveImageWebsocket"];
const NOT_IMAGE_SAVERS: [&str; 9] = ["anim", "video", "webm", "latent", "audio", "mesh", "glb", "3d", "gif"];
/// 取樣參數面板上算「常用」的欄位
const SAMPLER_FIELDS: [&str; 6] = ["steps", "cfg", "sampler_name", "scheduler", "denoise", "guidance"];
const IMAGE_LOADERS: [&str; 2] = ["LoadImage", "LoadImageMask"];
const SAMPLER_CLASSES: [&str; 4] = ["KSampler", "KSamplerAdvanced", "SamplerCustom", "SamplerCustomAdvanced"];

type R<T> = Result<T, String>;
type Nodes = Map<String, Value>;

pub fn mode_name(mode: &str) -> &'static str {
    match mode {
        "txt2img" => "文生圖",
        "img2img" => "圖生圖",
        "inpaint" => "局部重繪",
        "upscale" => "放大",
        "multiref" => "多圖參考",
        _ => "未知的模式",
    }
}

// ---------------------------------------------------------------- 看懂工作流
/// 主模型的載入節點
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ModelSlot {
    pub node: String,
    /// ckpt_name／unet_name／model_name
    pub key: String,
    /// 這個載入節點也輸出 CLIP（Checkpoint 類：第 1 個輸出）
    pub clip: bool,
    /// 標題寫了 {{model}}
    pub marked: bool,
}

/// 取樣節點（KSampler 類）：有哪些欄位可以套生成頁的值
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SamplerSlot {
    pub node: String,
    pub marked: bool,
    pub steps: bool,
    pub cfg: bool,
    pub sampler_name: bool,
    pub scheduler: bool,
    pub denoise: bool,
}

impl SamplerSlot {
    fn field_names(&self) -> Vec<&'static str> {
        [
            (self.steps, "steps"),
            (self.cfg, "cfg"),
            (self.sampler_name, "sampler_name"),
            (self.scheduler, "scheduler"),
            (self.denoise, "denoise"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, k)| k)
        .collect()
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Analysis {
    pub prompt: Vec<(String, String)>,
    pub placeholder: bool,
    pub negative: Vec<(String, String)>,
    pub negative_placeholder: bool,
    pub seeds: Vec<(String, String)>,
    pub sizes: Vec<String>,
    pub outputs: Vec<String>,
    /// 工作流裡 Load Image（含 Load Image (as Mask)）的數量
    pub load_images: usize,
    /// 沒有任何標記的 Load Image 數量
    pub unmarked: usize,
    /// 輸入圖的 Load Image
    pub image: Option<String>,
    /// 標了 {{mask}} 的節點
    pub mask: Option<String>,
    /// 輸入圖那個 Load Image 的 MASK 輸出（第 1 個輸出）有被接去用
    pub image_mask_used: bool,
    /// (編號, 節點)：{{ref1}}、{{ref2}}…
    pub refs: Vec<(usize, String)>,
    pub model: Option<ModelSlot>,
    pub samplers: Vec<SamplerSlot>,
    /// 所有有數字 denoise 的節點（取樣節點都沒有 denoise 時，圖生圖的重繪幅度改這些：Flux 的 BasicScheduler 這類）
    pub denoise_nodes: Vec<String>,
    /// 工作流原本就有的 LoRA 節點
    pub loras: Vec<String>,
    /// 分析時發現的、要讓使用者知道的事（白話；摘要裡的 warnings）
    pub warnings: Vec<String>,
    /// 寬高、batch_size 如果接的是 Primitive 這類節點：(用到它的節點, 欄位) → 真正放數字的 (節點, 欄位)
    pub redirects: BTreeMap<(String, String), (String, String)>,
    /// 取樣參數分散在別的節點的（SamplerCustom 系：BasicScheduler 的步數、KSamplerSelect 的取樣器、CFGGuider 的 CFG）
    pub chain: Vec<SamplerSlot>,
    /// 圖生圖、局部重繪：輸入圖那條路上第一個取樣節點（或它接的 Scheduler）的重繪幅度所在節點
    pub denoise_path: Vec<String>,
}

/// 某個節點的所有上游（輸入連線一路往上，不含自己）
fn ancestors(m: &Nodes, start: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut todo = vec![start.to_string()];
    while let Some(id) = todo.pop() {
        let Some(inp) = m.get(&id).and_then(|n| n["inputs"].as_object()) else { continue };
        for from in inp.values().filter(|v| is_link(v)).filter_map(|v| v[0].as_str()) {
            if seen.insert(from.to_string()) {
                todo.push(from.to_string());
            }
        }
    }
    seen
}

/// 「主要取樣器」：連到輸出那條路上、在潛空間裡取樣的取樣節點（有 latent_image 連線；base、refiner、hires 第二段都算）。
/// FaceDetailer 這類修圖節點是對圖片取樣、不算；路上只剩它們時，取步數最多的
fn main_samplers(m: &Nodes, ids: &[String], outputs: &[String]) -> Vec<String> {
    let mut on_path: HashSet<String> = HashSet::new();
    for o in outputs {
        on_path.extend(ancestors(m, o));
    }
    let all: Vec<&String> = ids.iter().filter(|id| on_path.contains(*id) && is_sampler_node(m, id)).collect();
    let latent: Vec<String> = all.iter().filter(|id| input(m, id, "latent_image").is_some_and(is_link)).map(|id| id.to_string()).collect();
    if !latent.is_empty() {
        return latent;
    }
    let steps = |id: &str| input(m, id, "steps").and_then(Value::as_f64).unwrap_or(0.0);
    let best = all.iter().map(|id| steps(id)).fold(0.0, f64::max);
    all.into_iter().filter(|id| steps(id) >= best).map(|id| id.to_string()).collect()
}

/// 取樣節點的 positive（neg＝negative）往回找到的文字欄位：沒有 positive 輸入的（Flux 的 SamplerCustomAdvanced）看它接的 Guider
fn sampler_text(m: &Nodes, id: &str, neg: bool) -> Option<Slots> {
    let inp = m.get(id)?.get("inputs")?;
    let via = |inp: &Value, k: &str| inp.get(k).filter(|v| is_link(v)).and_then(|l| l[0].as_str().map(str::to_string));
    let key = if neg { "negative" } else { "positive" };
    if let Some(from) = via(inp, key) {
        return walk_text(m, &from, 0, neg);
    }
    let guider = m.get(&via(inp, "guider")?)?.get("inputs")?;
    let keys: &[&str] = if neg { &["negative"] } else { &["positive", "conditioning"] };
    let from = keys.iter().find_map(|k| via(guider, k))?;
    walk_text(m, &from, 0, neg)
}

fn cand_names(m: &Nodes, c: &Cand) -> String {
    c.owners.iter().map(|id| format!("「{}」（#{id}）", label(m.get(id.as_str())))).collect::<Vec<_>>().join("、")
}

/// 正面提示詞放哪：標了 {{prompt}} 的欄位；沒標就從取樣器往回找文字框。
/// 找到好幾組時：只換主要取樣器（main_samplers）用到的；其他取樣器（例如 FaceDetailer 自己的提示詞框）文字跟主要的不同就維持原樣並提醒。
/// 回傳 (欄位, 是不是 {{prompt}} 標記, 提醒)
fn find_prompt_slots(m: &Nodes, ids: &[String], main: &[String], optional: bool) -> R<(Slots, bool, Vec<String>)> {
    let mut hits = vec![];
    for id in ids {
        for (k, v) in m[id]["inputs"].as_object().into_iter().flatten() {
            if v.as_str().is_some_and(|s| s.contains(PROMPT)) {
                hits.push((id.clone(), k.clone()));
            }
        }
    }
    if !hits.is_empty() {
        return Ok((hits, true, vec![]));
    }
    let mut cands = text_candidates(m, ids, false, true);
    match cands.len() {
        0 if optional => return Ok((vec![], false, vec![])),
        0 => return Err(format!("找不到要放提示詞的地方：在 ComfyUI 正面提示詞的文字框裡寫上 {PROMPT}，再匯出（API）一次")),
        1 => return Ok((cands.remove(0).slots, false, vec![])),
        _ => {}
    }
    let mut slots: Slots = vec![];
    for t in main.iter().filter_map(|s| sampler_text(m, s, false)) {
        for x in t {
            if !slots.contains(&x) {
                slots.push(x);
            }
        }
    }
    if slots.is_empty() {
        let names: Vec<String> = cands.iter().map(|c| cand_names(m, c)).collect();
        return Err(format!(
            "有好幾個正面提示詞的文字框，不知道要換哪個（{}）：在要換的那個文字框裡寫上 {PROMPT}，再匯出（API）一次",
            names.join("；")
        ));
    }
    let main_texts: Vec<&str> = slots.iter().filter_map(|(id, k)| txt(m, id, k)).map(str::trim).collect();
    let mut warns: Vec<String> = vec![];
    let mut extra: Slots = vec![];
    for c in cands.iter().filter(|c| !c.slots.iter().any(|x| slots.contains(x))) {
        let same = c.slots.iter().all(|(id, k)| txt(m, id, k).is_some_and(|t| main_texts.contains(&t.trim())));
        if same {
            extra.extend(c.slots.iter().cloned()); // 文字一樣：一起換
            continue;
        }
        let name = label(m.get(c.owners[0].as_str())).to_string();
        let w = format!("{name} 的提示詞維持工作流原本的（生成頁的提示詞只換到主要的取樣節點）");
        if !warns.contains(&w) {
            warns.push(w);
        }
    }
    slots.extend(extra);
    Ok((slots, false, warns))
}

/// 負面提示詞放哪：{{negative}}；沒標就從 negative 往回找。好幾組時只換主要取樣器的；判斷不出來就不動
fn find_negative(m: &Nodes, ids: &[String], prompt: &[(String, String)], main: &[String]) -> (Slots, bool) {
    let mut hits = vec![];
    for id in ids {
        for (k, v) in m[id]["inputs"].as_object().into_iter().flatten() {
            if v.as_str().is_some_and(|s| s.contains(NEGATIVE)) {
                hits.push((id.clone(), k.clone()));
            }
        }
    }
    if !hits.is_empty() {
        return (hits, true);
    }
    let mut cands: Vec<Cand> = text_candidates(m, ids, true, false);
    cands.retain(|c| !c.slots.iter().any(|x| prompt.contains(x)));
    match cands.len() {
        1 => (cands.remove(0).slots, false),
        0 => (vec![], false),
        _ => {
            let mut slots: Slots = vec![];
            for t in main.iter().filter_map(|s| sampler_text(m, s, true)) {
                for x in t.into_iter().filter(|x| !prompt.contains(x)) {
                    if !slots.contains(&x) {
                        slots.push(x);
                    }
                }
            }
            (slots, false)
        }
    }
}

/// 數字欄位接到別的節點（PrimitiveInt、Int、Seed…）：順著連線找到真正放數字的節點欄位；上游沒有就 None
fn num_source(m: &Nodes, link: &Value) -> Option<(String, String)> {
    let from = link.get(0)?.as_str()?;
    let inp = m.get(from)?.get("inputs")?.as_object()?;
    ["value", "int", "seed", "number", "Number", "noise_seed"]
        .iter()
        .find(|k| inp.get(**k).is_some_and(Value::is_number))
        .map(|k| (from.to_string(), (*k).to_string()))
}

/// 輸出節點的層級：0＝SaveImage、1＝其他存圖節點、2＝只預覽的。不是輸出就 None。
/// 有接圖的輸出裡取最前面那一層（有 SaveImage 就只算 SaveImage，同時有 Preview Image 也不會變成兩張）
fn output_tier(m: &Nodes, id: &str) -> Option<usize> {
    let c = class(m.get(id));
    if c == "SaveImage" {
        return Some(0);
    }
    if PREVIEWERS.contains(&c) {
        return Some(2);
    }
    let low = c.to_lowercase();
    let guess = low.contains("save") && !NOT_IMAGE_SAVERS.iter().any(|w| low.contains(w)) && input(m, id, "images").is_some();
    (OTHER_SAVERS.contains(&c) || guess).then_some(1)
}

fn txt<'a>(m: &'a Nodes, id: &str, k: &str) -> Option<&'a str> {
    input(m, id, k).and_then(Value::as_str)
}

fn is_num(m: &Nodes, id: &str, k: &str) -> bool {
    input(m, id, k).is_some_and(Value::is_number)
}

/// 標題裡的 {{ref3}} → [3]
fn ref_marks(t: &str) -> Vec<usize> {
    let mut out = vec![];
    let mut rest = t;
    while let Some(i) = rest.find("{{ref") {
        let tail = &rest[i + 5..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && tail[digits.len()..].starts_with("}}") {
            if let Some(n) = digits.parse::<usize>().ok().filter(|n| (1..=99).contains(n)) {
                out.push(n);
            }
        }
        rest = tail;
    }
    out
}

fn one(mut found: Vec<String>, tag: &str) -> R<Option<String>> {
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(format!("有好幾個標題寫了 {tag} 的 Load Image：只能標一個")),
    }
}

fn is_sampler_node(m: &Nodes, id: &str) -> bool {
    SAMPLER_CLASSES.contains(&class(m.get(id)))
        || (is_num(m, id, "steps") && is_num(m, id, "cfg"))
        || title(m.get(id)).contains(SAMPLER)
}

fn model_slot(m: &Nodes, id: &str, key: &str, marked: bool) -> ModelSlot {
    // 只有標準的 Checkpoint 載入節點，第 1 個輸出才一定是 CLIP（Eff. Loader、Pipe 類、ImageOnly 的輸出順序不一樣）
    let clip = key == "ckpt_name" && CLIP_LOADERS.contains(&class(m.get(id)));
    ModelSlot { node: id.to_string(), key: key.to_string(), clip, marked }
}

/// 從取樣節點沿著 model（或 guider 再 model）往上游走，找到第一個有模型檔欄位的載入節點
fn trace_loader(m: &Nodes, start: &str) -> Option<(String, String)> {
    let mut cur = start.to_string();
    for step in 0..64 {
        let skip = NOT_MAIN_LOADERS.iter().any(|w| class(m.get(&cur)).to_lowercase().contains(w));
        if step > 0 && !skip {
            if let Some(k) = MODEL_KEYS.iter().find(|k| txt(m, &cur, k).is_some()) {
                return Some((cur, k.to_string()));
            }
        }
        let inp = m.get(&cur)?.get("inputs")?.as_object()?;
        let next = ["model", "guider"].iter().find_map(|k| inp.get(*k).filter(|v| is_link(v)))?;
        cur = next[0].as_str()?.to_string();
    }
    None
}

fn find_model(m: &Nodes, ids: &[String]) -> R<Option<ModelSlot>> {
    let marked: Vec<&String> = ids.iter().filter(|id| title(m.get(id.as_str())).contains(MODEL)).collect();
    if marked.len() > 1 {
        return Err(format!("有好幾個標題寫了 {MODEL} 的節點：只能標一個"));
    }
    if let Some(id) = marked.first() {
        let Some(key) = MODEL_KEYS.iter().find(|k| txt(m, id, k).is_some()) else {
            return Err(format!(
                "標了 {MODEL} 的節點「{}」沒有放模型檔名的欄位（{}）",
                label(m.get(id.as_str())),
                MODEL_KEYS.join("、")
            ));
        };
        return Ok(Some(model_slot(m, id, key, true)));
    }
    // 沒標：每個取樣節點往上游找，多數決（hires fix 共用同一個模型，refiner 的另一個模型不會搶過去）；同票取編號小的
    let mut votes: Vec<((String, String), usize)> = vec![];
    for id in ids.iter().filter(|id| is_sampler_node(m, id)) {
        if let Some(found) = trace_loader(m, id) {
            match votes.iter_mut().find(|(k, _)| *k == found) {
                Some((_, n)) => *n += 1,
                None => votes.push((found, 1)),
            }
        }
    }
    let mut best: Option<&((String, String), usize)> = None;
    for v in &votes {
        if best.is_none_or(|b| v.1 > b.1) {
            best = Some(v);
        }
    }
    Ok(best.map(|((id, key), _)| model_slot(m, id, key, false)))
}

/// 取樣參數分散在別的節點的（SamplerCustom／SamplerCustomAdvanced 接的 BasicScheduler、KSamplerSelect、CFGGuider）：
/// 「用生成頁的步數、CFG…」要改的就是它們。已經算在取樣節點（有步數和 CFG）裡的不重複算
fn chain_slots(m: &Nodes, ids: &[String], samplers: &[SamplerSlot]) -> Vec<SamplerSlot> {
    let mut hit: Vec<String> = vec![];
    for id in ids {
        for k in ["sigmas", "sampler", "guider"] {
            if let Some(from) = input(m, id, k).filter(|v| is_link(v)).and_then(|l| l[0].as_str()) {
                if !hit.iter().any(|h| h == from) {
                    hit.push(from.to_string());
                }
            }
        }
        if class(m.get(id)) == "SamplerCustom" && is_num(m, id, "cfg") && !hit.contains(id) {
            hit.push(id.clone());
        }
    }
    hit.into_iter()
        .filter(|id| m.contains_key(id) && !samplers.iter().any(|s| s.node == *id))
        .map(|id| SamplerSlot {
            marked: false,
            steps: is_num(m, &id, "steps"),
            cfg: is_num(m, &id, "cfg"),
            sampler_name: txt(m, &id, "sampler_name").is_some(),
            scheduler: txt(m, &id, "scheduler").is_some(),
            denoise: is_num(m, &id, "denoise"),
            node: id,
        })
        .filter(|s| !s.field_names().iter().all(|f| *f == "denoise"))
        .collect()
}

/// 圖生圖、局部重繪的重繪幅度要改哪裡：從輸入圖往下游走（輸入圖→VAE Encode→latent→取樣），
/// 取「最前面」的取樣節點（它上游沒有別的取樣節點）。hires 第二段、FaceDetailer 在它後面，不算。
/// 取樣節點自己沒有 denoise（SamplerCustomAdvanced）就看它的 sigmas 接的 BasicScheduler。判斷不出來回傳空（建圖時退回改全部）
fn denoise_path(m: &Nodes, ids: &[String], image: &str) -> Vec<String> {
    let mut next: HashMap<String, Vec<String>> = HashMap::new();
    for id in ids {
        for from in m[id]["inputs"].as_object().into_iter().flatten().filter(|(_, v)| is_link(v)).filter_map(|(_, v)| v[0].as_str()) {
            next.entry(from.to_string()).or_default().push(id.clone());
        }
    }
    let mut reach: HashSet<String> = HashSet::new();
    let mut todo = vec![image.to_string()];
    while let Some(x) = todo.pop() {
        for y in next.get(&x).into_iter().flatten() {
            if reach.insert(y.clone()) {
                todo.push(y.clone());
            }
        }
    }
    let samplers: Vec<&String> = ids.iter().filter(|id| reach.contains(*id) && is_sampler_node(m, id)).collect();
    let mut out: Vec<String> = vec![];
    for s in &samplers {
        let up = ancestors(m, s);
        if samplers.iter().any(|o| o != s && up.contains(*o)) {
            continue;
        }
        let own = is_num(m, s, "denoise").then(|| s.to_string());
        let via = input(m, s, "sigmas").filter(|v| is_link(v)).and_then(|l| l[0].as_str()).filter(|n| is_num(m, n, "denoise")).map(str::to_string);
        if let Some(t) = own.or(via).filter(|t| !out.contains(t)) {
            out.push(t);
        }
    }
    out
}

/// 看懂工作流的結構（不管模式需要什麼）。prompt_optional＝找不到提示詞的文字框也不算錯（放大模式）；
/// mode 只用來決定要不要提醒「寬高改不到」（只有文生圖用得到寬高）
fn analyze_raw(graph: &Value, mode: &str, prompt_optional: bool) -> R<Analysis> {
    let m = nodes(graph)?;
    let ids = sorted_ids(m);
    let mut a = Analysis::default();
    // 輸出：沒接圖的閒置輸出忽略，至少要有一個接了圖的
    let mut tiers: [Vec<String>; 3] = Default::default();
    let mut idle: Vec<&String> = vec![];
    for id in &ids {
        let Some(t) = output_tier(m, id) else { continue };
        if input(m, id, "images").is_some_and(is_link) {
            tiers[t].push(id.clone());
        } else {
            idle.push(id);
        }
    }
    a.outputs = tiers.into_iter().find(|t| !t.is_empty()).unwrap_or_default();
    if a.outputs.is_empty() {
        return Err(match idle.first() {
            Some(id) => format!("存圖的節點「{}」沒有接上圖片：把它接到最後出圖的那條線上，再匯出（API）一次", label(m.get(id.as_str()))),
            None => "找不到存圖的節點：工作流的最後要有 Save Image（Preview Image、WAS 的 Image Save 這類存圖或預覽節點也行）".into(),
        });
    }
    let main = main_samplers(m, &ids, &a.outputs);
    match find_prompt_slots(m, &ids, &main, prompt_optional) {
        Ok((slots, placeholder, warns)) => {
            (a.prompt, a.placeholder) = (slots, placeholder);
            a.warnings.extend(warns);
        }
        Err(_) if prompt_optional => {}
        Err(e) => return Err(e),
    }
    (a.negative, a.negative_placeholder) = find_negative(m, &ids, &a.prompt, &main);

    // 輸入圖、遮罩、參考圖：看 Load Image 的標題
    let loaders: Vec<&String> = ids.iter().filter(|id| IMAGE_LOADERS.contains(&class(m.get(id.as_str())))).collect();
    a.load_images = loaders.len();
    let tagged = |tag: &str| -> Vec<String> {
        loaders.iter().filter(|id| title(m.get(id.as_str())).contains(tag)).map(|id| id.to_string()).collect()
    };
    a.image = one(tagged(IMAGE), IMAGE)?;
    a.mask = one(tagged(MASK), MASK)?;
    for id in &loaders {
        for n in ref_marks(title(m.get(id.as_str()))) {
            if a.refs.iter().any(|(k, _)| *k == n) {
                return Err(format!("有好幾個標題寫了 {{{{ref{n}}}}} 的 Load Image：每個編號只能標一個"));
            }
            a.refs.push((n, id.to_string()));
        }
    }
    a.refs.sort();
    let plain: Vec<&&String> = loaders
        .iter()
        .filter(|id| {
            let t = title(m.get(id.as_str()));
            class(m.get(id.as_str())) == "LoadImage" && !t.contains(IMAGE) && !t.contains(MASK) && ref_marks(t).is_empty()
        })
        .collect();
    a.unmarked = plain.len();
    if a.image.is_none() && plain.len() == 1 {
        a.image = Some(plain[0].to_string());
    }
    if let Some(img) = a.image.as_deref().filter(|id| class(m.get(*id)) == "LoadImage") {
        a.image_mask_used = m.values().any(|n| {
            n["inputs"]
                .as_object()
                .is_some_and(|i| i.values().any(|v| is_link(v) && v[0].as_str() == Some(img) && v[1].as_i64() == Some(1)))
        });
    }

    let mut stuck: Vec<String> = vec![]; // 種子接在改不到的節點上
    let mut stuck_size: Vec<String> = vec![];
    for id in &ids {
        for k in ["seed", "noise_seed"] {
            match input(m, id, k) {
                Some(v) if v.is_number() => a.seeds.push((id.clone(), k.to_string())),
                // 種子接在 Primitive／Int 這類節點上：改那個節點的數字
                Some(v) if is_link(v) => match num_source(m, v) {
                    Some(src) if !a.seeds.contains(&src) => a.seeds.push(src),
                    Some(_) => {}
                    None => {
                        if let Some(from) = v[0].as_str().filter(|f| m.contains_key(*f)) {
                            stuck.push(format!("「{}」（#{from}）", label(m.get(from))));
                        }
                    }
                },
                _ => {}
            }
        }
        // 寬高：width、height、batch_size 都是數字，或接在 Primitive 這類節點上（找得到放數字的欄位）
        let field = |k: &str| -> Option<Option<(String, String)>> {
            let v = input(m, id, k)?;
            if v.is_number() {
                Some(Some((id.clone(), k.to_string())))
            } else if is_link(v) {
                Some(num_source(m, v))
            } else {
                None
            }
        };
        if let (Some(w), Some(h), Some(b)) = (field("width"), field("height"), field("batch_size")) {
            if let (Some(w), Some(h)) = (w, h) {
                a.sizes.push(id.clone());
                // batch_size 是找不到數字欄位的連線：建圖時直接把它改成 1
                for (k, src) in [("width", Some(w)), ("height", Some(h)), ("batch_size", b)] {
                    if let Some(src) = src.filter(|s| *s != (id.clone(), k.to_string())) {
                        a.redirects.insert((id.clone(), k.to_string()), src);
                    }
                }
            } else if mode == "txt2img" {
                for k in ["width", "height"] {
                    if let Some(from) = input(m, id, k).filter(|v| is_link(v)).and_then(|v| v[0].as_str()).filter(|f| m.contains_key(*f)) {
                        stuck_size.push(format!("「{}」（#{from}）", label(m.get(from))));
                    }
                }
            }
        }
        if is_num(m, id, "denoise") {
            a.denoise_nodes.push(id.clone());
        }
        if class(m.get(id)).to_lowercase().contains("lora") {
            a.loras.push(id.clone());
        }
        let marked = title(m.get(id)).contains(SAMPLER);
        let slot = SamplerSlot {
            node: id.clone(),
            marked,
            steps: is_num(m, id, "steps"),
            cfg: is_num(m, id, "cfg"),
            sampler_name: txt(m, id, "sampler_name").is_some(),
            scheduler: txt(m, id, "scheduler").is_some(),
            denoise: is_num(m, id, "denoise"),
        };
        if marked {
            if slot.field_names().is_empty() {
                return Err(format!(
                    "標了 {SAMPLER} 的節點「{}」沒有步數、CFG、取樣器、排程、重繪幅度這些欄位可以套",
                    label(m.get(id))
                ));
            }
            a.samplers.push(slot);
        } else if slot.steps && slot.cfg {
            a.samplers.push(slot);
        }
    }
    a.model = find_model(m, &ids)?;
    a.chain = chain_slots(m, &ids, &a.samplers);
    if let Some(img) = a.image.as_deref() {
        a.denoise_path = denoise_path(m, &ids, img);
    }

    stuck.sort();
    stuck.dedup();
    if !stuck.is_empty() {
        a.warnings.push(format!("種子接在 {} 節點，生圖站改不到，每次會出同一張：改接 Primitive Int 或 Seed 節點，或直接把種子填在取樣節點上，再匯出（API）一次", stuck.join("、")));
    }
    stuck_size.sort();
    stuck_size.dedup();
    if !stuck_size.is_empty() {
        a.warnings.push(format!("寬高接在 {} 節點，生圖站改不到，文生圖會照工作流原本的尺寸：改接 Primitive Int 節點，或直接把寬高填在 Empty Latent 節點上，再匯出（API）一次", stuck_size.join("、")));
    }
    if let Some(s) = a.model.as_ref().filter(|s| !s.clip && s.key == "ckpt_name") {
        a.warnings.push(format!(
            "主模型的載入節點「{}」不是標準的 Load Checkpoint，生成頁選的 LoRA 只接在模型上（文字編碼那邊不會套到 LoRA），效果可能比在 ComfyUI 裡弱",
            label(m.get(s.node.as_str()))
        ));
    }
    Ok(a)
}

/// 每個模式需要什麼：沒有就說清楚怎麼標
fn validate(a: &Analysis, mode: &str) -> R<()> {
    let need_image = |what: &str| -> R<()> {
        if a.image.is_some() {
            return Ok(());
        }
        Err(match (a.load_images, a.unmarked) {
            (0, _) => format!("{what}需要一張輸入圖：在 ComfyUI 的工作流裡加一個 Load Image（載入圖像）接到要處理的地方，再匯出（API）一次"),
            (_, 0) => format!("{what}需要一張輸入圖：把放輸入圖的 Load Image 節點標題改成 {IMAGE}（現在的 Load Image 都標成別的用途了），再匯出（API）一次"),
            _ => format!("有好幾個 Load Image，不知道哪個是輸入圖：把放輸入圖的那個節點標題改成 {IMAGE}，再匯出（API）一次"),
        })
    };
    match mode {
        "img2img" | "upscale" => need_image(mode_name(mode))?,
        "inpaint" => {
            need_image("局部重繪")?;
            if a.mask.is_none() && !a.image_mask_used {
                return Err(format!(
                    "局部重繪需要遮罩：把放遮罩的 Load Image（或 Load Image (as Mask)）節點標題改成 {MASK}；\
                     或把輸入圖那個 Load Image 的 MASK 輸出接去用（例如接 Set Latent Noise Mask），再匯出（API）一次"
                ));
            }
        }
        "multiref" if !a.refs.iter().any(|(n, _)| *n == 1) => {
            return Err("多圖參考需要參考圖：把放第一張參考圖的 Load Image 節點標題改成 {{ref1}}（第二張 {{ref2}}，以此類推），再匯出（API）一次".into());
        }
        _ => {}
    }
    Ok(())
}

/// 看懂某個模式的工作流：哪裡放提示詞、輸入圖、遮罩、種子、寬高、模型、取樣參數。看不懂就回傳原因（給使用者看的）
pub fn analyze(graph: &Value, mode: &str) -> R<Analysis> {
    if !MODES.contains(&mode) {
        return Err(format!("工作流組只支援這幾種模式：{}", MODES.map(mode_name).join("、")));
    }
    let a = analyze_raw(graph, mode, mode == "upscale")?;
    validate(&a, mode)?;
    Ok(a)
}

impl Analysis {
    /// 「用生成頁的步數、CFG、取樣器…」有沒有地方可以套：取樣節點（KSampler 類），或 SamplerCustom 系接的 Scheduler、KSamplerSelect、CFGGuider
    pub fn page_sampler_ok(&self) -> bool {
        !self.samplers.is_empty() || !self.chain.is_empty()
    }

    /// 要套生成頁取樣參數／重繪幅度的節點：有人標了 {{sampler}} 就只算標的，沒有就全部
    pub fn active_samplers(&self) -> Vec<&SamplerSlot> {
        let marked: Vec<&SamplerSlot> = self.samplers.iter().filter(|s| s.marked).collect();
        if marked.is_empty() {
            self.samplers.iter().collect()
        } else {
            marked
        }
    }

    /// 給設定頁／生成頁看的摘要
    pub fn summary(&self, graph: &Value) -> Value {
        let m = graph.as_object();
        let node = |id: &str| {
            let n = m.and_then(|m| m.get(id));
            json!({"node": id, "class": class(n), "title": label(n)})
        };
        let model = self.model.as_ref().map(|s| {
            let n = m.and_then(|m| m.get(&s.node));
            json!({"node": s.node, "class": class(n), "title": label(n), "key": s.key, "clip": s.clip, "marked": s.marked,
                   "value": n.map(|n| n["inputs"][&s.key].clone())})
        });
        json!({
            "prompt": self.prompt.iter().map(|(id, _)| node(id)).collect::<Vec<_>>(),
            "placeholder": self.placeholder,
            "negative": self.negative.iter().map(|(id, _)| node(id)).collect::<Vec<_>>(),
            "negative_placeholder": self.negative_placeholder,
            "seeds": self.seeds.len(),
            "sizes": self.sizes.len(),
            "outputs": self.outputs.iter().map(|id| node(id)).collect::<Vec<_>>(),
            "load_images": self.load_images,
            "image": self.image.as_deref().map(node),
            "mask": self.mask.as_deref().map(node),
            "mask_from_image": self.mask.is_none() && self.image_mask_used,
            "refs": self.refs.iter().map(|(n, id)| {
                let mut v = node(id);
                v["n"] = json!(n);
                v
            }).collect::<Vec<_>>(),
            "model": model,
            "samplers": self.samplers.iter().map(|s| {
                let mut v = node(&s.node);
                v["marked"] = json!(s.marked);
                v["fields"] = json!(s.field_names());
                v
            }).collect::<Vec<_>>(),
            "loras": self.loras.iter().map(|id| node(id)).collect::<Vec<_>>(),
            "warnings": self.warnings,
            "page_sampler_ok": self.page_sampler_ok(),
        })
    }
}

// ---------------------------------------------------------------- 從節點圖讀回參數（imgmeta：ComfyUI 生成的圖）
/// 某個節點自己加上全部上游，近的在前（同一層照輸入欄位的順序）
fn upstream(m: &Nodes, start: &str) -> Vec<String> {
    let mut out = vec![start.to_string()];
    let mut i = 0;
    while i < out.len() {
        let id = out[i].clone();
        i += 1;
        let Some(inp) = m.get(&id).and_then(|n| n["inputs"].as_object()) else { continue };
        for from in inp.values().filter(|v| is_link(v)).filter_map(|v| v[0].as_str()) {
            if m.contains_key(from) && !out.iter().any(|o| o == from) {
                out.push(from.to_string());
            }
        }
    }
    out
}

/// ComfyUI 存在圖裡的節點圖（API 格式）→ 生成頁的參數：提示詞、負面、種子、步數、CFG、取樣器、排程、尺寸、主模型、LoRA。
/// 跟匯入工作流同一套找法：從「最前面的主要取樣器」往上游找（hires 第二段、refiner 在它後面，不算），
/// 所以 SamplerCustomAdvanced＋CFGGuider＋BasicScheduler、Qwen／Flux 的文字編碼節點、Primitive 文字都讀得到。
/// 找不到取樣節點是 None；找不到提示詞時 prompt 是空字串。size＝圖本身的尺寸（工作流裡找不到 Empty Latent 時用）
pub fn graph_params(graph: &Value, size: (u32, u32)) -> Option<Value> {
    let m = nodes(graph).ok()?;
    let ids = sorted_ids(m);
    let mut tiers: [Vec<String>; 3] = Default::default();
    for id in &ids {
        if let Some(t) = output_tier(m, id).filter(|_| input(m, id, "images").is_some_and(is_link)) {
            tiers[t].push(id.clone());
        }
    }
    let outputs = tiers.into_iter().find(|t| !t.is_empty()).unwrap_or_default();
    let mut samplers = main_samplers(m, &ids, &outputs);
    if samplers.is_empty() {
        samplers = ids.iter().filter(|id| is_sampler_node(m, id)).cloned().collect();
    }
    let first = samplers
        .iter()
        .find(|s| {
            let up = ancestors(m, s);
            !samplers.iter().any(|o| o != *s && up.contains(o))
        })
        .or(samplers.first())?
        .clone();
    let up = upstream(m, &first);
    // 數字：直接填的，或接在 Primitive／Int 這類節點上的
    let at = |id: &str, k: &str| -> Option<Value> {
        let v = input(m, id, k)?;
        if v.is_number() {
            return Some(v.clone());
        }
        let (n, f) = num_source(m, v)?;
        input(m, &n, &f).cloned()
    };
    let num = |keys: &[&str]| up.iter().find_map(|id| keys.iter().find_map(|k| at(id, k)));
    let word = |k: &str| up.iter().find_map(|id| txt(m, id, k).map(str::to_string));
    let pos = sampler_text(m, &first, false).unwrap_or_default();
    // 負面接的是 ConditioningZeroOut（Qwen、Flux 常見）時會一路走回正面的文字框：那種不算負面
    let neg: Slots = sampler_text(m, &first, true).unwrap_or_default().into_iter().filter(|s| !pos.contains(s)).collect();
    let text = |slots: &Slots| slots.iter().find_map(|(id, k)| txt(m, id, k).filter(|t| !t.trim().is_empty())).unwrap_or("").to_string();
    let (w, h) = up
        .iter()
        .find_map(|id| {
            input(m, id, "batch_size")?;
            Some((at(id, "width")?.as_u64()?, at(id, "height")?.as_u64()?))
        })
        .unwrap_or((size.0 as u64, size.1 as u64));
    // LoRA：主要取樣器上游的 LoRA 節點（照節點編號排）；rgthree 的 Power Lora Loader 每格是 {"on", "lora", "strength"}
    let in_up: HashSet<&String> = up.iter().collect();
    let mut loras: Vec<Value> = vec![];
    for id in ids.iter().filter(|id| in_up.contains(id)) {
        if let Some(name) = txt(m, id, "lora_name").filter(|n| *n != "None") {
            let s = ["strength_model", "strength", "lora_strength"].iter().find_map(|k| input(m, id, k).and_then(Value::as_f64));
            loras.push(json!({"name": name, "strength": s.unwrap_or(1.0)}));
        }
        for v in m[id]["inputs"].as_object().into_iter().flatten().map(|(_, v)| v) {
            if let Some(name) = v.get("lora").and_then(Value::as_str).filter(|n| *n != "None" && v["on"] != json!(false)) {
                loras.push(json!({"name": name, "strength": v["strength"].as_f64().unwrap_or(1.0)}));
            }
        }
    }
    let turbo = |l: &Value| l["name"].as_str().is_some_and(graphs::is_turbo_lora);
    let mut p = json!({
        "mode": "txt2img", "prompt": text(&pos), "negative": text(&neg), "width": w, "height": h,
        "loras": loras.iter().filter(|l| !turbo(l)).cloned().collect::<Vec<_>>(),
        "turbo_lora": loras.iter().find(|l| turbo(l)).map(|l| l["name"].clone()).unwrap_or(json!(false)),
    });
    if let Some(v) = num(&["seed", "noise_seed"]).filter(|v| v.is_i64() || v.is_u64()) {
        p["seed"] = v;
    }
    for (k, keys) in [("steps", ["steps"]), ("cfg", ["cfg"])] {
        if let Some(v) = num(&keys) {
            p[k] = v;
        }
    }
    for (k, field) in [("sampler", "sampler_name"), ("scheduler", "scheduler")] {
        if let Some(v) = word(field) {
            p[k] = json!(v);
        }
    }
    let model = trace_loader(m, &first).or_else(|| find_model(m, &ids).ok().flatten().map(|s| (s.node, s.key)));
    if let Some(name) = model.and_then(|(id, k)| txt(m, &id, &k).map(str::to_string)).filter(|n| !n.is_empty()) {
        p["model"] = json!(name);
    }
    Some(p)
}

// ---------------------------------------------------------------- 組成節點圖
fn int_of(v: Option<&Value>) -> Option<i64> {
    v.filter(|v| !v.is_null()).and_then(py::int)
}

fn float_of(v: Option<&Value>) -> Option<f64> {
    v.filter(|v| !v.is_null() && v.as_str() != Some("")).and_then(py::float)
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// 輸入圖在 ComfyUI input 資料夾裡的檔：沒有就用「找不到輸入圖片：」開頭的錯誤（api_gen 會補解法）
fn input_file(input_dir: &Path, name: &str) -> Result<PathBuf, GraphError> {
    let path = input_dir.join(name);
    if name.is_empty() || !path.is_file() {
        return Err(GraphError(format!("找不到輸入圖片：{name}")));
    }
    Ok(path)
}

fn fnv(parts: &[&[u8]]) -> u32 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in parts {
        for b in *p {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    (h ^ (h >> 32)) as u32
}

/// 局部重繪：把遮罩寫成 ComfyUI 讀得懂的 RGBA PNG（Load Image 的 MASK 輸出＝1 − alpha/255，所以重繪的地方 alpha=0）。
/// 回傳 (輸入圖那張, 遮罩那張)：輸入圖的 RGB 是原圖，遮罩那張的 RGB 是遮罩本身（紅色色版＝遮罩，Load Image (as Mask) 讀紅色或 alpha 都對）。
/// 檔名帶遮罩內容的雜湊：同一張原圖排兩個不同遮罩的任務時，後一個不會把前一個還沒跑到的檔蓋掉
fn write_masks(input_dir: &Path, init: &str, mask: &str, want_image: bool, want_mask: bool) -> Result<(String, String), GraphError> {
    let bad = |what: &str, e: image::ImageError| GraphError(format!("讀不到{what}：{e}"));
    let init_path = input_file(input_dir, init)?;
    let mask_path = input_file(input_dir, mask)?;
    let base = image::open(&init_path).map_err(|e| bad("輸入圖", e))?.to_rgb8();
    let (w, h) = base.dimensions();
    let mut m = image::open(&mask_path).map_err(|e| bad("遮罩", e))?.to_rgba8();
    if m.dimensions() != (w, h) {
        m = image::imageops::resize(&m, w, h, image::imageops::FilterType::Triangle);
    }
    let red: Vec<u8> = m.pixels().map(|p| p[0]).collect();
    if !red.iter().any(|r| *r > 127) {
        return Err(GraphError("遮罩是空的，請先在圖上塗出要重繪的區域".into()));
    }
    let len = init_path.metadata().map(|x| x.len()).unwrap_or(0);
    let hash = fnv(&[&red, &w.to_le_bytes(), &h.to_le_bytes(), &len.to_le_bytes()]);
    let stem: String = Path::new(init)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into())
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    let (mut name_image, mut name_mask) = (init.to_string(), mask.to_string());
    let save = |name: &str, pixel: &dyn Fn(usize) -> [u8; 4]| -> Result<(), GraphError> {
        let path = input_dir.join(name);
        if path.is_file() {
            return Ok(()); // 同一份內容已經寫過（同一批的第二張、重跑）
        }
        let mut buf = Vec::with_capacity(red.len() * 4);
        for i in 0..red.len() {
            buf.extend_from_slice(&pixel(i));
        }
        let img = image::RgbaImage::from_raw(w, h, buf).ok_or_else(|| GraphError("組遮罩圖失敗".into()))?;
        // 先寫暫存檔再換名：ComfyUI 不會讀到寫一半的檔
        let tmp = input_dir.join(format!("{name}.tmp"));
        img.save_with_format(&tmp, image::ImageFormat::Png)
            .and_then(|_| std::fs::rename(&tmp, &path).map_err(image::ImageError::IoError))
            .map_err(|e| GraphError(format!("寫遮罩圖失敗：{e}")))
    };
    if want_image {
        name_image = format!("{stem}_{hash:08x}_wfmask.png");
        let px = base.as_raw();
        save(&name_image, &|i| [px[i * 3], px[i * 3 + 1], px[i * 3 + 2], 255 - red[i]])?;
    }
    if want_mask {
        name_mask = format!("{stem}_{hash:08x}_wfmaskonly.png");
        save(&name_mask, &|i| [red[i], red[i], red[i], 255 - red[i]])?;
    }
    Ok((name_image, name_mask))
}

/// 把所有指向 (from, slot) 的連線改接到 to
fn rewire(g: &mut Nodes, from: &str, slot: i64, to: &Value) {
    for n in g.values_mut() {
        let Some(inp) = n.get_mut("inputs").and_then(Value::as_object_mut) else { continue };
        for v in inp.values_mut() {
            if is_link(v) && v[0].as_str() == Some(from) && v[1].as_i64() == Some(slot) {
                *v = to.clone();
            }
        }
    }
}

/// 生成頁選的 LoRA 接在主模型載入節點後面：Checkpoint 類（也輸出 CLIP）用 LoraLoader，其他用 LoraLoaderModelOnly。
/// 原本接在載入節點後面的每一個節點（含工作流自己的 LoRA 節點）都改接到鏈的尾端
fn inject_loras(g: &mut Nodes, slot: &ModelSlot, loras: &[(String, f64, f64)]) {
    if loras.is_empty() {
        return;
    }
    let mut ids: Vec<String> = vec![];
    for _ in loras {
        let mut k = ids.len() + 1;
        while g.contains_key(&format!("anima_lora_{k}")) || ids.contains(&format!("anima_lora_{k}")) {
            k += 1;
        }
        ids.push(format!("anima_lora_{k}"));
    }
    let last = ids.last().cloned().unwrap_or_default();
    rewire(g, &slot.node, 0, &json!([last, 0]));
    if slot.clip {
        rewire(g, &slot.node, 1, &json!([last, 1]));
    }
    let (mut model, mut clip) = (json!([slot.node, 0]), json!([slot.node, 1]));
    for (id, (name, strength, strength_clip)) in ids.iter().zip(loras) {
        let node = if slot.clip {
            json!({"class_type": "LoraLoader", "inputs": {"model": model, "clip": clip, "lora_name": name,
                   "strength_model": strength, "strength_clip": strength_clip}})
        } else {
            json!({"class_type": "LoraLoaderModelOnly", "inputs": {"model": model, "lora_name": name, "strength_model": strength}})
        };
        g.insert(id.clone(), node);
        model = json!([id, 0]);
        clip = json!([id, 1]);
    }
}

/// 參數面板改的值：只改已經存在、不是連線的欄位；不認得的鍵略過。數字欄位收到數字字串就轉成數字
pub(crate) fn apply_overrides(g: &mut Nodes, ov: &Value) {
    for (key, v) in ov.as_object().into_iter().flatten() {
        let Some((id, k)) = key.split_once('.') else { continue };
        // 存圖前綴固定，免得圖庫收不到
        if class(g.get(id)) == "SaveImage" && k == "filename_prefix" {
            continue;
        }
        let Some(old) = input(g, id, k).filter(|o| !is_link(o) && !o.is_array() && !o.is_object()) else { continue };
        let new = match (old, v) {
            (_, Value::Null | Value::Array(_) | Value::Object(_)) => continue,
            (Value::Number(n), Value::String(s)) => match s.trim().parse::<f64>() {
                Ok(f) if n.is_i64() && f.fract() == 0.0 => json!(f as i64),
                Ok(f) => json!(f),
                Err(_) => continue,
            },
            (Value::Bool(_), Value::String(s)) => json!(matches!(s.trim().to_lowercase().as_str(), "true" | "1" | "yes")),
            _ => v.clone(),
        };
        set(g, id, k, new);
    }
}

/// 工作流原本的文字後面接上生成頁的（prompt_mode＝append）：中間用「, 」連接，原本結尾的逗號不重複
fn append_text(old: &str, text: &str) -> String {
    let old = old.trim_end().trim_end_matches(',').trim_end();
    if old.is_empty() {
        text.to_string()
    } else {
        format!("{old}, {text}")
    }
}

fn fill(g: &mut Nodes, slots: &[(String, String)], placeholder: bool, append: bool, mark: &str, text: &str) {
    for (id, k) in slots {
        let v = if placeholder {
            json!(input(g, id, k).and_then(Value::as_str).unwrap_or("").replace(mark, text))
        } else if append {
            json!(append_text(input(g, id, k).and_then(Value::as_str).unwrap_or(""), text))
        } else {
            json!(text)
        };
        set(g, id, k, v);
    }
}

/// 寬高要對齊幾：Flux、SD3 家族（工作流記的家族、主模型檔名、節點類型看得出來）一律 16，其他 8
fn size_align(wf: &Value, g: &Nodes) -> i64 {
    let fam = wf["family"].as_str().unwrap_or("");
    let by_family = fam == crate::families::FLUX || fam == crate::families::SD3;
    let by_node = g.values().any(|n| {
        let c = n["class_type"].as_str().unwrap_or("");
        c == "EmptySD3LatentImage" || c.contains("Flux") || c.contains("SD3")
    });
    let by_file = g.values().filter_map(|n| MODEL_KEYS.iter().find_map(|k| n["inputs"][k].as_str())).any(|f| {
        let f = f.to_lowercase();
        f.contains("flux") || f.contains("sd3")
    });
    if by_family || by_node || by_file {
        16
    } else {
        8
    }
}

/// 某個模式的節點圖，回傳 (nodes, info)，跟 graphs::build 一樣。
/// set＝存檔的工作流組（舊格式也行）；p＝生成頁的參數。
/// 內建功能被取代時 p.wf_model_from_page＝true：主模型換成生成頁選的
pub fn build(p: &Value, set_: &Value, mode: &str, input_dir: &Path, seed_offset: i64) -> Result<(Nodes, Value), GraphError> {
    let wf = normalize(set_.clone());
    let name = wf["name"].as_str().unwrap_or("").to_string();
    let Some(graph) = wf["graphs"].get(mode) else {
        return Err(GraphError(format!("工作流「{name}」沒有{}的工作流：到「設定 → 模型與工作流」補一個", mode_name(mode))));
    };
    let a = analyze(graph, mode).map_err(GraphError)?;
    let mut g = nodes(graph).map_err(GraphError)?.clone();

    // 種子、提示詞（{a|b} 跟內建流程一樣：先抽提示詞再抽負面，同一種子抽到一樣的）
    let seed = int_of(p.get("seed")).unwrap_or(0) + seed_offset;
    let mut rng = PyRandom::new(seed);
    let prompt = graphs::expand_wildcards(p["prompt"].as_str().unwrap_or(""), &mut rng);
    let negative = graphs::expand_wildcards(p["negative"].as_str().unwrap_or(""), &mut rng);
    if prompt.trim().is_empty() && mode != "upscale" {
        return Err(GraphError("要寫提示詞".into()));
    }
    // 沒標 {{prompt}} 時：整格換掉（預設），或保留工作流原本的文字、把生成頁的接在後面
    let append = wf["options"]["prompt_mode"].as_str() == Some("append");
    if !a.prompt.is_empty() && (!prompt.trim().is_empty() || a.placeholder) {
        fill(&mut g, &a.prompt, a.placeholder, append, PROMPT, &prompt);
    }
    // 負面：生成頁空白時，有 {{negative}} 的清掉標記、整格的照工作流原本寫的
    if a.negative_placeholder || !negative.trim().is_empty() {
        fill(&mut g, &a.negative, a.negative_placeholder, append, NEGATIVE, &negative);
    }
    for (id, k) in &a.seeds {
        set(&mut g, id, k, json!(seed));
    }

    // 寬高（只有文生圖照生成頁，Flux／SD3 對齊 16、其他對齊 8）；batch_size 一律 1。欄位接在 Primitive 節點上的，改那個節點
    let align = size_align(&wf, &g);
    let want = |k: &str| int_of(p.get(k)).filter(|v| (64..=8192).contains(v)).map(|v| v / align * align);
    let mut wh = (0, 0);
    for id in &a.sizes {
        let real = |k: &str| a.redirects.get(&(id.clone(), k.to_string())).cloned().unwrap_or_else(|| (id.clone(), k.to_string()));
        let ((wn, wk), (hn, hk), (bn, bk)) = (real("width"), real("height"), real("batch_size"));
        if let (true, Some(w), Some(h)) = (mode == "txt2img", want("width"), want("height")) {
            set(&mut g, &wn, &wk, json!(w));
            set(&mut g, &hn, &hk, json!(h));
        }
        set(&mut g, &bn, &bk, json!(1));
        let get = |n: &str, k: &str| input(&g, n, k).and_then(Value::as_i64).unwrap_or(0);
        let (w, h) = (get(&wn, &wk), get(&hn, &hk));
        if w * h > wh.0 * wh.1 {
            wh = (w, h);
        }
    }

    // 輸入圖、遮罩、參考圖
    let init = str_of(p.get("init_image"));
    let missing_init = || GraphError(format!("{}要一張輸入圖：先放一張圖片", mode_name(mode)));
    let mut input_size = None;
    match mode {
        "img2img" | "upscale" | "inpaint" => {
            let (init, image_id) = (init.ok_or_else(missing_init)?, a.image.as_deref().unwrap_or_default());
            let path = input_file(input_dir, init)?;
            input_size = image::image_dimensions(&path).ok();
            if mode == "inpaint" {
                let mask = str_of(p.get("mask")).ok_or_else(|| GraphError("局部重繪要有遮罩：先在圖上塗出要重繪的區域".into()))?;
                let (img_name, mask_name) = write_masks(input_dir, init, mask, a.image_mask_used, a.mask.is_some())?;
                set(&mut g, image_id, "image", json!(img_name));
                if let Some(mid) = &a.mask {
                    set(&mut g, mid, "image", json!(mask_name));
                }
            } else {
                set(&mut g, image_id, "image", json!(init));
            }
        }
        "multiref" => {
            let refs: Vec<&str> = p
                .get("refs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| str_of(r.get("image")))
                .take(10)
                .collect();
            if refs.is_empty() {
                return Err(GraphError("請先加入至少一張參考圖".into()));
            }
            for (n, id) in &a.refs {
                if let Some(name) = refs.get(n - 1) {
                    let path = input_file(input_dir, name)?;
                    if *n == 1 {
                        input_size = image::image_dimensions(&path).ok();
                    }
                    set(&mut g, id, "image", json!(name));
                }
            }
        }
        _ => {}
    }
    if wh.0 == 0 {
        wh = input_size.map_or((0, 0), |(w, h)| (i64::from(w), i64::from(h)));
    }

    // 重繪幅度（圖生圖、局部重繪）
    if matches!(mode, "img2img" | "inpaint") {
        if let Some(d) = float_of(p.get("denoise")).map(|d| d.clamp(0.0, 1.0)) {
            let active = a.active_samplers();
            let marked = a.samplers.iter().any(|s| s.marked);
            let mut targets: Vec<&str> = if !marked && !a.denoise_path.is_empty() {
                // 沒標 {{sampler}}：只改輸入圖那條路上第一個取樣節點，hires 第二段、FaceDetailer 保留原值
                a.denoise_path.iter().map(String::as_str).collect()
            } else {
                active.iter().filter(|s| s.denoise).map(|s| s.node.as_str()).collect()
            };
            if targets.is_empty() {
                targets = a.denoise_nodes.iter().map(String::as_str).collect();
            }
            for id in targets {
                set(&mut g, id, "denoise", json!(d));
            }
        }
    }

    // 取樣參數：「用生成頁的步數、CFG…」打開，或節點標了 {{sampler}}
    let page_sampler = truthy(wf["options"].get("page_sampler"));
    let (steps, cfg) = (int_of(p.get("steps")), float_of(p.get("cfg")));
    let (sampler, scheduler) = (str_of(p.get("sampler")), str_of(p.get("scheduler")));
    for s in a.active_samplers().into_iter().filter(|s| page_sampler || s.marked) {
        if let (true, Some(v)) = (s.steps, steps) {
            set(&mut g, &s.node, "steps", json!(v));
        }
        if let (true, Some(v)) = (s.cfg, cfg) {
            set(&mut g, &s.node, "cfg", json!(v));
        }
        if let (true, Some(v)) = (s.sampler_name, sampler) {
            set(&mut g, &s.node, "sampler_name", json!(v));
        }
        if let (true, Some(v)) = (s.scheduler, scheduler) {
            set(&mut g, &s.node, "scheduler", json!(v));
        }
    }
    // SamplerCustom 系：步數、排程在 BasicScheduler，取樣器在 KSamplerSelect，CFG 在 CFGGuider
    if page_sampler && !a.samplers.iter().any(|s| s.marked) {
        for s in &a.chain {
            if let (true, Some(v)) = (s.steps, steps) {
                set(&mut g, &s.node, "steps", json!(v));
            }
            if let (true, Some(v)) = (s.cfg, cfg) {
                set(&mut g, &s.node, "cfg", json!(v));
            }
            if let (true, Some(v)) = (s.sampler_name, sampler) {
                set(&mut g, &s.node, "sampler_name", json!(v));
            }
            if let (true, Some(v)) = (s.scheduler, scheduler) {
                set(&mut g, &s.node, "scheduler", json!(v));
            }
        }
    }

    // 主模型：取代內建功能時用生成頁選的
    let from_page = truthy(p.get("wf_model_from_page"));
    if from_page {
        let Some(m) = &a.model else {
            return Err(GraphError(format!(
                "取代內建功能的工作流「{name}」找不到主模型的載入節點：把載入模型的節點標題改成 {MODEL}，再匯出（API）一次"
            )));
        };
        if let Some(model) = str_of(p.get("model")).filter(|m| !m.starts_with(PREFIX)) {
            set(&mut g, &m.node, &m.key, json!(model));
        }
    }

    // LoRA
    let loras: Vec<(String, f64, f64)> = p
        .get("loras")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|l| l.get("enabled").is_none_or(|v| truthy(Some(v))))
        .filter_map(|l| {
            let name = str_of(l.get("name"))?.to_string();
            let strength = float_of(l.get("strength")).unwrap_or(1.0);
            let clip = float_of(l.get("strength_clip")).unwrap_or(strength);
            (strength != 0.0).then_some((name, strength, clip))
        })
        .collect();
    if !loras.is_empty() {
        let Some(m) = &a.model else {
            return Err(GraphError(format!(
                "工作流「{name}」找不到主模型的載入節點，LoRA 接不上去：把載入模型的節點標題改成 {MODEL}，再匯出（API）一次"
            )));
        };
        inject_loras(&mut g, m, &loras);
    }

    // 存圖：Preview Image、WAS 的 Image Save 這類換成 SaveImage（images 照接），檔名前綴改 AnimaWeb/<模式>，圖庫才收得到
    for id in &a.outputs {
        if class(g.get(id)) != "SaveImage" {
            let images = input(&g, id, "images").cloned().unwrap_or(Value::Null);
            let mut save = json!({"class_type": "SaveImage", "inputs": {"images": images, "filename_prefix": ""}});
            if let Some(meta) = g.get(id).and_then(|n| n.get("_meta")) {
                save["_meta"] = meta.clone();
            }
            g.insert(id.clone(), save);
        }
        set(&mut g, id, "filename_prefix", json!(format!("AnimaWeb/{mode}")));
    }
    // 參數面板改的值最後套（比上面的自動槽位優先）
    if let Some(ov) = p.get("wf_overrides") {
        apply_overrides(&mut g, ov);
    }
    let family = if from_page { str_of(p.get("family")).unwrap_or("custom") } else { "custom" };
    let info = json!({"prompt": prompt, "negative": negative, "seed": seed, "width": wh.0, "height": wh.1, "mode": mode,
                      "family": family, "workflow": name});
    Ok((g, info))
}

// ---------------------------------------------------------------- 參數欄位清單（參數面板）
fn input_spec<'a>(oi: &'a Value, cls: &str, key: &str) -> Option<&'a Value> {
    let i = oi.get(cls)?.get("input")?;
    ["required", "optional"].iter().find_map(|g| i.get(*g)?.get(key))
}

/// 每個非連線的欄位一筆：節點、欄位、目前值、型別（INT／FLOAT／STRING／BOOLEAN／COMBO）、範圍／選項（讀 ComfyUI 的 /object_info）。
/// group：common＝取樣參數、主模型檔、LoRA；advanced＝其他。auto：生圖站自己會填（提示詞、種子、寬高、輸入圖、存檔前綴），畫面上可以藏起來。
/// missing：下拉選項裡沒有目前的值（別人的模型檔名在這台電腦沒有）；node_missing：這台 ComfyUI 沒有這種節點（缺自訂節點）
pub fn fields(graph: &Value, mode: &str, object_info: &Value) -> Vec<Value> {
    let Ok(m) = nodes(graph) else { return vec![] };
    let a = analyze_raw(graph, mode, true).unwrap_or_default();
    let mut auto: HashSet<(String, String)> = HashSet::new();
    for (id, k) in a.prompt.iter().chain(&a.negative).chain(&a.seeds) {
        auto.insert((id.clone(), k.clone()));
    }
    for id in &a.sizes {
        let real = |k: &str| a.redirects.get(&(id.clone(), k.to_string())).cloned().unwrap_or_else(|| (id.clone(), k.to_string()));
        auto.insert(real("batch_size"));
        if mode == "txt2img" {
            auto.insert(real("width"));
            auto.insert(real("height"));
        }
    }
    for id in &a.outputs {
        auto.insert((id.clone(), "filename_prefix".into()));
    }
    let slots: Vec<&String> = match mode {
        "img2img" | "upscale" => a.image.iter().collect(),
        "inpaint" => a.image.iter().chain(&a.mask).collect(),
        "multiref" => a.refs.iter().map(|(_, id)| id).collect(),
        _ => vec![],
    };
    for id in slots {
        auto.insert((id.clone(), "image".into()));
    }
    // 出圖時會換成 SaveImage 的輸出節點（Image Save、Preview Image…）：它們原本的欄位用不到
    let skip: Vec<String> = a.outputs.iter().filter(|id| class(m.get(*id)) != "SaveImage").cloned().collect();
    let common = |id: &str, k: &str| {
        let model_slot = a.model.as_ref().is_some_and(|s| s.node == id && s.key == k);
        let lora = a.loras.iter().any(|l| l == id) && matches!(k, "lora_name" | "strength_model" | "strength_clip");
        SAMPLER_FIELDS.contains(&k) || model_slot || lora
    };
    field_rows(m, &auto, &skip, &common, object_info)
}

/// 參數面板的欄位列（生圖工作流、影片工作流共用）：每個非連線的欄位一筆。
/// auto＝生圖站自己會填的欄位；skip＝整個節點不列；common＝算「常用」的欄位
pub(crate) fn field_rows(
    m: &Nodes,
    auto: &HashSet<(String, String)>,
    skip: &[String],
    common: &dyn Fn(&str, &str) -> bool,
    object_info: &Value,
) -> Vec<Value> {
    let info_known = object_info.as_object().is_some_and(|o| !o.is_empty());
    let mut out = vec![];
    for id in sorted_ids(m) {
        let cls = class(m.get(&id));
        if skip.contains(&id) {
            continue;
        }
        let known = !info_known || object_info.get(cls).is_some();
        for (k, v) in m[&id]["inputs"].as_object().into_iter().flatten() {
            if is_link(v) || v.is_array() || v.is_object() || v.is_null() {
                continue;
            }
            let spec = input_spec(object_info, cls, k);
            let head = spec.and_then(|s| s.get(0));
            let opts = spec.and_then(|s| s.get(1));
            // 型別：舊寫法 [[選項…], {…}]、新寫法 ["COMBO", {options}]、["INT", {min, max}]；ComfyUI 沒有這個節點就看值猜
            let mut options: Option<Vec<Value>> = None;
            let ty = match head {
                Some(Value::Array(list)) => {
                    options = Some(list.clone());
                    "COMBO"
                }
                Some(Value::String(t)) if t == "COMBO" => {
                    options = Some(opts.and_then(|o| o.get("options")).and_then(Value::as_array).cloned().unwrap_or_default());
                    "COMBO"
                }
                Some(Value::String(t)) if matches!(t.as_str(), "INT" | "FLOAT" | "STRING" | "BOOLEAN") => match t.as_str() {
                    "INT" => "INT",
                    "FLOAT" => "FLOAT",
                    "STRING" => "STRING",
                    _ => "BOOLEAN",
                },
                _ => match v {
                    Value::Bool(_) => "BOOLEAN",
                    Value::Number(n) if n.is_i64() || n.is_u64() => "INT",
                    Value::Number(_) => "FLOAT",
                    _ => "STRING",
                },
            };
            let is_auto = auto.contains(&(id.clone(), k.clone()));
            let common = !is_auto && common(&id, k);
            let mut f = json!({
                "key": format!("{id}.{k}"), "node": id, "class": cls, "title": label(m.get(&id)), "input": k,
                "value": v, "type": ty, "group": if common { "common" } else { "advanced" }, "auto": is_auto,
                "missing": false, "node_missing": !known,
            });
            if let Some(list) = &options {
                f["options"] = json!(list);
                f["missing"] = json!(!list.is_empty() && v.is_string() && !list.contains(v));
            }
            if let Some(o) = opts.and_then(Value::as_object) {
                for key in ["min", "max", "step", "default", "multiline"] {
                    if let Some(x) = o.get(key) {
                        f[key] = x.clone();
                    }
                }
            }
            out.push(f);
        }
    }
    out
}

// ---------------------------------------------------------------- 內建功能匯出：加標記
fn set_title(n: &mut Value, mark: &str) {
    if !n["_meta"].is_object() {
        n["_meta"] = json!({});
    }
    let old = n["_meta"]["title"].as_str().unwrap_or("").to_string();
    if old.contains(mark) {
        return;
    }
    n["_meta"]["title"] = json!(if old.is_empty() { mark.to_string() } else { format!("{old} {mark}") });
}

/// graphs::build 組出來的內建流程 → 加上槽位標記的 API 格式工作流（匯出給使用者在 ComfyUI 改，改好再匯入取代）。
/// 比對用 p.prompt_resolved／p.negative_resolved（{a|b} 已展開的），沒有就用 p.prompt／p.negative：
/// 文字欄位換成 {{prompt}}／{{negative}}；Load Image 標 {{image}}、{{mask}}、{{refN}}；載入主模型的標 {{model}}；
/// 步數跟生成頁一樣的取樣節點標 {{sampler}}（原本的 _meta 保留）
pub fn mark_for_export(graph: &Nodes, p: &Value) -> Value {
    let mut g = graph.clone();
    let text_of = |resolved: &str, plain: &str| {
        str_of(p.get(resolved)).or_else(|| str_of(p.get(plain))).map(str::to_string)
    };
    let (prompt, negative) = (text_of("prompt_resolved", "prompt"), text_of("negative_resolved", "negative"));
    let init = str_of(p.get("init_image"));
    let mask = str_of(p.get("mask"));
    let model = str_of(p.get("model"));
    let steps = int_of(p.get("steps"));
    let refs: Vec<&str> = p
        .get("refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| str_of(r.get("image")))
        .take(10)
        .collect();
    let (mut init_done, mut mask_done) = (false, false);
    let mut ref_done = vec![false; refs.len()];
    for id in sorted_ids(&g) {
        let cls = class(g.get(&id)).to_string();
        let keys: Vec<String> = g[&id]["inputs"].as_object().map(|i| i.keys().cloned().collect()).unwrap_or_default();
        let mut marks: Vec<String> = vec![];
        for k in keys {
            let Some(s) = g[&id]["inputs"][&k].as_str().map(str::to_string) else { continue };
            let mark = if matches!(k.as_str(), "text" | "prompt" | "negative_prompt") && !s.is_empty() {
                if prompt.as_deref() == Some(s.as_str()) {
                    Some("{{prompt}}")
                } else if negative.as_deref() == Some(s.as_str()) {
                    Some(NEGATIVE)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(mark) = mark {
                set(&mut g, &id, &k, json!(mark));
            }
            if MODEL_KEYS.contains(&k.as_str()) && k != "model_name" && model == Some(s.as_str()) {
                marks.push(MODEL.into());
            }
            if k == "image" && cls == "LoadImage" {
                if init == Some(s.as_str()) && !init_done {
                    init_done = true;
                    marks.push(IMAGE.into());
                } else if let Some(i) = (0..refs.len()).find(|i| !ref_done[*i] && refs[*i] == s) {
                    ref_done[i] = true;
                    marks.push(format!("{{{{ref{}}}}}", i + 1));
                }
            }
            if k == "image" && cls == "LoadImageMask" && mask == Some(s.as_str()) && !mask_done {
                mask_done = true;
                marks.push(MASK.into());
            }
        }
        if let (Some(want), true, true) = (steps, is_num(&g, &id, "steps"), is_num(&g, &id, "cfg")) {
            if input(&g, &id, "steps").and_then(Value::as_i64) == Some(want) {
                marks.push(SAMPLER.into());
            }
        }
        if let Some(n) = g.get_mut(&id) {
            for mark in marks {
                set_title(n, &mark);
            }
        }
    }
    // 記下是哪個模式、哪個家族匯出的（匯入時自動判斷模式；放大這類沒有主模型的流程靠家族標記才知道要取代誰）
    let mode = str_of(p.get("mode")).unwrap_or("txt2img");
    if MODES.contains(&mode) {
        if let Some(id) = sorted_ids(&g).into_iter().find(|id| class(g.get(id)) == "SaveImage") {
            if let Some(n) = g.get_mut(&id) {
                set_title(n, &format!("{{{{mode:{mode}}}}}"));
                if let Some(f) = str_of(p.get("family")).filter(|f| family_ok(f)) {
                    set_title(n, &format!("{{{{family:{f}}}}}"));
                }
            }
        }
    }
    Value::Object(g)
}

/// 家族代號：英數和底線（跟 feature_flows 的鍵一樣），other／custom 不算
fn family_ok(f: &str) -> bool {
    !f.is_empty() && f != crate::families::OTHER && f != "custom" && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 匯出時寫在 Save Image 標題的 {{mode:<模式>}}（mark_for_export）；照節點編號找第一個
pub fn mode_mark(graph: &Value) -> Option<&'static str> {
    let g = graph.as_object()?;
    sorted_ids(g).into_iter().find_map(|id| {
        let t = title(g.get(&id));
        MODES.into_iter().find(|m| t.contains(&format!("{{{{mode:{m}}}}}")))
    })
}

/// 同上的 {{family:<家族>}}
pub fn family_mark(graph: &Value) -> Option<String> {
    let g = graph.as_object()?;
    sorted_ids(g).into_iter().find_map(|id| {
        let t = title(g.get(&id));
        let rest = &t[t.find("{{family:")? + "{{family:".len()..];
        let f = &rest[..rest.find("}}")?];
        family_ok(f).then(|| f.to_string())
    })
}

/// 匯出內建流程時還沒放圖：用佔位圖代替（放在 ComfyUI 的 input，檔名固定、已經有就不重寫），
/// 使用者到 ComfyUI 再換成要的圖。回傳補了哪些（"image"、"mask"、"ref"），給畫面提示
pub fn export_placeholders(p: &mut Map<String, Value>, input: &Path) -> R<Vec<&'static str>> {
    let mode = str_of(p.get("mode")).unwrap_or("txt2img").to_string();
    let mut filled = vec![];
    // 先寫暫存檔再改名：寫到一半中斷也不會留下壞檔（檔名固定，有檔就不重寫）
    let write = |f: &Path, img: image::GrayImage| -> Result<(), String> {
        std::fs::create_dir_all(input).map_err(|e| format!("建不了 ComfyUI 的 input 資料夾：{e}"))?;
        let tmp = f.with_extension("png.part");
        img.save_with_format(&tmp, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, f).map_err(|e| e.to_string())
    };
    let gray = |name: &str| -> R<String> {
        let f = input.join(name);
        if !f.is_file() {
            write(&f, image::GrayImage::from_pixel(1024, 1024, image::Luma([128]))).map_err(|e| format!("寫不了佔位圖：{e}"))?;
        }
        Ok(name.to_string())
    };
    if matches!(mode.as_str(), "img2img" | "inpaint" | "upscale") && str_of(p.get("init_image")).is_none() {
        p.insert("init_image".into(), json!(gray(EXPORT_PLACEHOLDER)?));
        filled.push("image");
    }
    if mode == "inpaint" && str_of(p.get("mask")).is_none() {
        // 遮罩跟輸入圖一樣大：中間一塊（寬高各一半）是白的
        let init = str_of(p.get("init_image")).unwrap_or(EXPORT_PLACEHOLDER).to_string();
        let f = input.join(&init);
        if !f.is_file() {
            // 跟 api_gen 的 build_error 一樣的說法
            return Err(format!("找不到輸入圖片：{init}。解法：重新放一次圖片（上傳暫存可能被清掉了）。"));
        }
        let (w, h) = image::image_dimensions(&f).map_err(|e| format!("讀不到輸入圖的大小：{e}"))?;
        let name = format!("anima_export_mask_{w}x{h}.png");
        let f = input.join(&name);
        if !f.is_file() {
            // 灰階（一個像素一個位元組）：ComfyUI 的 Load Image (as Mask) 會轉成 RGBA，紅色通道就是這個值
            let (x0, x1, y0, y1) = (w / 4, w - w / 4, h / 4, h - h / 4);
            let m = image::GrayImage::from_fn(w, h, |x, y| image::Luma([if (x0..x1).contains(&x) && (y0..y1).contains(&y) { 255 } else { 0 }]));
            write(&f, m).map_err(|e| format!("寫不了佔位遮罩：{e}"))?;
        }
        p.insert("mask".into(), json!(name));
        filled.push("mask");
    }
    let has_ref = p.get("refs").and_then(Value::as_array).is_some_and(|a| a.iter().any(|r| str_of(r.get("image")).is_some()));
    if mode == "multiref" && !has_ref {
        p.insert("refs".into(), json!([{"image": gray(EXPORT_PLACEHOLDER)?}]));
        filled.push("ref");
    }
    Ok(filled)
}

// ---------------------------------------------------------------- 取代內建功能（settings 的 feature_flows）
/// feature_flows 的鍵：「<家族>.<模式>」，例如 anima.inpaint
pub fn flow_key_ok(key: &str) -> bool {
    match key.split_once('.') {
        Some((fam, mode)) => !fam.is_empty() && fam.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && MODES.contains(&mode),
        None => false,
    }
}

/// 這個家族的這個模式有沒有被指定取代：回傳工作流組代號。builtin＝請求硬要用內建的
pub fn pick_flow<'a>(flows: &'a Value, fam: &str, mode: &str, builtin: bool) -> Option<&'a str> {
    if builtin || !MODES.contains(&mode) {
        return None;
    }
    flows.get(format!("{fam}.{mode}")).and_then(Value::as_str).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- 存檔：data\image_workflows\<代號>.json
pub fn dir(data: &Path) -> PathBuf {
    data.join("image_workflows")
}

fn path_of(dir: &Path, id: &str) -> R<PathBuf> {
    if id.is_empty() || crate::video_wf::id_of(id) != id {
        return Err("生圖工作流的代號不對".into());
    }
    Ok(dir.join(format!("{id}.json")))
}

/// 舊格式 {"name", "added", "graph"} → 新格式（只在記憶體裡轉）；補齊 options、family、source
pub fn normalize(mut wf: Value) -> Value {
    let Some(o) = wf.as_object_mut() else { return json!({"name": "", "graphs": {}, "options": {"page_sampler": false}, "family": "", "source": "import"}) };
    if !o.get("graphs").is_some_and(Value::is_object) {
        let mut graphs = Map::new();
        if let Some(g) = o.remove("graph") {
            graphs.insert("txt2img".into(), g);
        }
        o.insert("graphs".into(), Value::Object(graphs));
    }
    o.remove("graph");
    if !o.get("options").is_some_and(Value::is_object) {
        o.insert("options".into(), json!({}));
    }
    if !o["options"].get("page_sampler").is_some_and(Value::is_boolean) {
        o["options"]["page_sampler"] = json!(false);
    }
    if !o.get("family").is_some_and(Value::is_string) {
        o.insert("family".into(), json!(""));
    }
    if !matches!(o.get("source").and_then(Value::as_str), Some("import" | "builtin")) {
        o.insert("source".into(), json!("import"));
    }
    wf
}

/// 是不是生圖站自己匯出的工作流組檔（{"name", "graphs": {"<模式>": 節點圖}}），不是 ComfyUI 的工作流
pub fn is_set_file(v: &Value) -> bool {
    v.get("graphs").and_then(Value::as_object).is_some_and(|g| !g.is_empty() && g.keys().all(|k| MODES.contains(&k.as_str())))
}

pub fn load(dir: &Path, id: &str) -> R<Value> {
    let path = path_of(dir, id)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("找不到生圖工作流「{id}」：到「設定 → 模型與工作流」重新匯入"))?;
    serde_json::from_str(&text)
        .map(normalize)
        .map_err(|_| format!("生圖工作流「{id}」的檔案壞了：到「設定 → 模型與工作流」重新匯入"))
}

/// 這個工作流組有哪些模式的圖（照 MODES 的順序，不管看不看得懂）
pub fn modes_of(wf: &Value) -> Vec<&'static str> {
    MODES.into_iter().filter(|m| wf["graphs"].get(*m).is_some_and(Value::is_object)).collect()
}

pub fn has_mode(wf: &Value, mode: &str) -> bool {
    wf["graphs"].get(mode).is_some_and(Value::is_object)
}

/// 某個模式的圖用的主模型檔名（看不懂這張圖、找不到主模型載入節點就是 None）
pub fn graph_model_file(graph: &Value, mode: &str) -> Option<String> {
    let slot = analyze(graph, mode).ok()?.model?;
    txt(nodes(graph).ok()?, &slot.node, &slot.key).map(str::to_string)
}

/// 工作流組的主模型檔名：依序找每個模式的圖，第一個看得懂、找得到主模型載入節點的
pub fn model_file(wf: &Value) -> Option<String> {
    modes_of(wf).into_iter().find_map(|mode| graph_model_file(&wf["graphs"][mode], mode))
}

fn entry(id: &str, wf: &Value) -> Value {
    let wf = normalize(wf.clone());
    let (mut modes, mut summary, mut errors) = (vec![], Map::new(), Map::new());
    for mode in modes_of(&wf) {
        let g = &wf["graphs"][mode];
        match analyze(g, mode) {
            Ok(a) => {
                modes.push(mode);
                summary.insert(mode.to_string(), a.summary(g));
            }
            Err(e) => {
                errors.insert(mode.to_string(), json!(e));
            }
        }
    }
    let mut e = json!({
        "id": id, "name": wf["name"], "added": wf["added"], "family": wf["family"], "source": wf["source"],
        "options": wf["options"], "modes": modes, "model_file": model_file(&wf),
    });
    // 舊版網頁讀的 info／error：第一個看得懂的模式的摘要
    match modes.first() {
        Some(first) => e["info"] = summary[*first].clone(),
        None => {
            e["error"] = json!(errors.values().next().and_then(Value::as_str).unwrap_or("這個工作流組裡沒有任何工作流"));
        }
    }
    // 有任何一個模式可以套就算可以（一個都看不懂時不灰掉）
    e["page_sampler_ok"] = json!(summary.is_empty() || summary.values().any(|s| s["page_sampler_ok"] == json!(true)));
    e["summary"] = Value::Object(summary);
    e["errors"] = Value::Object(errors);
    e
}

pub fn list(dir: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension()?.to_str()? != "json" || crate::video_wf::id_of(&id) != id {
                return None;
            }
            let wf: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(entry(&id, &wf))
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

fn write_set(dir: &Path, id: &str, wf: &Value) -> R<()> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建立資料夾失敗：{e}"))?;
    let text = serde_json::to_string_pretty(wf).map_err(|e| e.to_string())?;
    let path = path_of(dir, id)?;
    // 先寫暫存檔再換名，寫到一半當機也不會弄壞原本的
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("存檔失敗：{e}"))
}

/// 存工作流組時一起寫的資料
#[derive(Debug, Default, Clone)]
pub struct Meta {
    /// 模型家族（sdxl、flux…）；None＝不動
    pub family: Option<String>,
    /// "import"｜"builtin"；None＝新組用 import、舊組不動
    pub source: Option<String>,
}

/// 把某個模式的圖存進工作流組：看得懂才存。set_id＝存進這個既有的組；沒給就用名字（同名的組已經存在就加進去／換掉那個模式，其他模式保留）
pub fn save(dir: &Path, name: &str, set_id: Option<&str>, mode: &str, graph: &Value, meta: &Meta) -> R<Value> {
    analyze(graph, mode)?;
    let name: String = name.trim().chars().take(60).collect();
    let id = match set_id {
        Some(id) => id.to_string(),
        None if name.is_empty() => return Err("幫這個工作流取個名字".into()),
        None => crate::video_wf::id_of(&name),
    };
    let path = path_of(dir, &id)?;
    let old = match std::fs::read_to_string(&path) {
        Ok(t) => match serde_json::from_str::<Value>(&t) {
            Ok(v) => Some(normalize(v)),
            Err(_) if set_id.is_some() => return Err(format!("生圖工作流「{id}」的檔案壞了：刪掉再重新匯入")),
            Err(_) => None,
        },
        Err(_) if set_id.is_some() => return Err(format!("找不到生圖工作流「{id}」")),
        Err(_) => None,
    };
    let mut wf = old.unwrap_or_else(|| normalize(json!({"name": name, "added": now()})));
    if set_id.is_none() && !name.is_empty() {
        wf["name"] = json!(name);
    }
    wf["graphs"][mode] = graph.clone();
    if let Some(f) = &meta.family {
        wf["family"] = json!(f);
    }
    if let Some(s) = meta.source.as_deref().filter(|s| matches!(*s, "import" | "builtin")) {
        wf["source"] = json!(s);
    }
    write_set(dir, &id, &wf)?;
    Ok(entry(&id, &wf))
}

/// 「用生成頁的步數、CFG…」開關
pub fn set_options(dir: &Path, id: &str, page_sampler: bool) -> R<Value> {
    set_options_all(dir, id, Some(page_sampler), None)
}

/// 工作流組的選項：page_sampler（用生成頁的步數、CFG…）、prompt_mode（replace＝整格換掉，append＝保留原本的文字、生成頁的接在後面）；None＝不動
pub fn set_options_all(dir: &Path, id: &str, page_sampler: Option<bool>, prompt_mode: Option<&str>) -> R<Value> {
    if let Some(m) = prompt_mode.filter(|m| !matches!(*m, "replace" | "append")) {
        return Err(format!("提示詞模式只能是 replace 或 append（送來的是 {m}）"));
    }
    let mut wf = load(dir, id)?;
    if let Some(on) = page_sampler {
        wf["options"]["page_sampler"] = json!(on);
    }
    if let Some(m) = prompt_mode {
        wf["options"]["prompt_mode"] = json!(m);
    }
    write_set(dir, id, &wf)?;
    Ok(entry(id, &wf))
}

/// 刪某個模式（最後一個模式刪掉就整組刪掉）；mode＝None 整組刪掉。回傳整組是不是都刪了
pub fn remove(dir: &Path, id: &str, mode: Option<&str>) -> R<bool> {
    let path = path_of(dir, id)?;
    if !path.is_file() {
        return Err(format!("找不到生圖工作流「{id}」"));
    }
    if let Some(mode) = mode {
        let mut wf = load(dir, id)?;
        if !has_mode(&wf, mode) {
            return Err(format!("工作流「{id}」沒有{}的工作流", mode_name(mode)));
        }
        if let Some(g) = wf["graphs"].as_object_mut() {
            g.remove(mode);
        }
        if !modes_of(&wf).is_empty() {
            write_set(dir, id, &wf)?;
            return Ok(false);
        }
    }
    std::fs::remove_file(&path).map_err(|e| format!("刪除失敗：{e}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 像 ComfyUI 預設文生圖工作流匯出（API）的樣子
    fn sdxl() -> Value {
        json!({
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sdxl.safetensors"}},
            "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 4}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}},
            "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["4", 1]}},
            "3": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 1,
                   "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "ComfyUI"}}
        })
    }

    /// 圖生圖：Load Image → VAE Encode 取代 Empty Latent
    fn sdxl_img2img() -> Value {
        let mut g = sdxl();
        let o = g.as_object_mut().unwrap();
        o.remove("5");
        o.insert("10".into(), json!({"class_type": "LoadImage", "inputs": {"image": "example.png"}}));
        o.insert("11".into(), json!({"class_type": "VAEEncode", "inputs": {"pixels": ["10", 0], "vae": ["4", 2]}}));
        g["3"]["inputs"]["latent_image"] = json!(["11", 0]);
        g["3"]["inputs"]["denoise"] = json!(0.5);
        g
    }

    /// 局部重繪：輸入圖的 MASK 輸出接 Set Latent Noise Mask
    fn sdxl_inpaint() -> Value {
        let mut g = sdxl_img2img();
        g["12"] = json!({"class_type": "SetLatentNoiseMask", "inputs": {"samples": ["11", 0], "mask": ["10", 1]}});
        g["3"]["inputs"]["latent_image"] = json!(["12", 0]);
        g
    }

    fn wf(g: Value) -> Value {
        json!({"name": "SDXL", "graphs": {"txt2img": g}})
    }

    /// 測試用的 input 資料夾：放一張 8×6 的原圖 base.png 和遮罩 mask.png（左半邊白＝要重繪）
    fn input_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anima_image_wf_in_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        image::RgbImage::from_fn(8, 6, |x, y| image::Rgb([10 + x as u8, 20 + y as u8, 30])).save(d.join("base.png")).unwrap();
        image::RgbImage::from_fn(8, 6, |x, _| if x < 4 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 0]) })
            .save(d.join("mask.png"))
            .unwrap();
        d
    }

    #[test]
    fn default_txt2img() {
        let g = sdxl();
        let a = analyze(&g, "txt2img").unwrap();
        assert_eq!(a.prompt, vec![("6".into(), "text".into())]);
        assert_eq!(a.negative, vec![("7".into(), "text".into())]);
        assert_eq!(a.sizes, vec!["5".to_string()]);
        assert_eq!(a.seeds, vec![("3".to_string(), "seed".to_string())]);
        // 主模型：從取樣器的 model 往上游找到 Checkpoint 載入節點，它也輸出 CLIP
        assert_eq!(a.model, Some(ModelSlot { node: "4".into(), key: "ckpt_name".into(), clip: true, marked: false }));
        assert_eq!(a.samplers.len(), 1);
        assert!(a.samplers[0].steps && a.samplers[0].cfg && a.samplers[0].sampler_name && a.samplers[0].scheduler && a.samplers[0].denoise);
        assert!(a.loras.is_empty() && a.image.is_none());
        let wf = wf(g);
        let (n, info) = build(&json!({"prompt": "a dog", "negative": "ugly", "seed": 10, "width": 832, "height": 1217}), &wf, "txt2img", Path::new("."), 2).unwrap();
        assert_eq!(n["6"]["inputs"]["text"], "a dog");
        assert_eq!(n["7"]["inputs"]["text"], "ugly");
        assert_eq!(n["3"]["inputs"]["seed"], 12);
        assert_eq!(n["3"]["inputs"]["steps"], 20); // 步數照工作流
        assert_eq!(n["5"]["inputs"]["width"], 832);
        assert_eq!(n["5"]["inputs"]["height"], 1216); // 對齊 8
        assert_eq!(n["5"]["inputs"]["batch_size"], 1);
        assert_eq!(n["9"]["inputs"]["filename_prefix"], "AnimaWeb/txt2img");
        assert_eq!(info["family"], "custom");
        assert_eq!(info["workflow"], "SDXL");
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(832), Some(1216)));
        // 負面空白：整格的照工作流原本寫的
        let (n, _) = build(&json!({"prompt": "x", "seed": 0}), &wf, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["7"]["inputs"]["text"], "blurry");
        assert!(build(&json!({"prompt": " ", "seed": 0}), &wf, "txt2img", Path::new("."), 0).is_err());
        // 沒有這個模式的圖：講清楚缺哪個
        let e = build(&json!({"prompt": "x", "seed": 0}), &wf, "inpaint", Path::new("."), 0).unwrap_err().0;
        assert!(e.contains("局部重繪"), "{e}");
    }

    #[test]
    fn wildcards_match_builtin_draw() {
        // 同一個種子，抽到的跟內建流程一樣：先提示詞、再負面，同一個亂數
        let mut rng = PyRandom::new(5);
        let want_p = graphs::expand_wildcards("{red|blue|green} hair", &mut rng);
        let want_n = graphs::expand_wildcards("{a|b|c|d}", &mut rng);
        let (n, info) = build(&json!({"prompt": "{red|blue|green} hair", "negative": "{a|b|c|d}", "seed": 5}), &wf(sdxl()), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["6"]["inputs"]["text"], want_p.as_str());
        assert_eq!(n["7"]["inputs"]["text"], want_n.as_str());
        assert_eq!(info["prompt"], want_p.as_str());
    }

    #[test]
    fn placeholders_and_errors() {
        let mut g = sdxl();
        g["6"]["inputs"]["text"] = json!("masterpiece, {{prompt}}");
        g["7"]["inputs"]["text"] = json!("{{negative}}, lowres");
        let a = analyze(&g, "txt2img").unwrap();
        assert!(a.placeholder && a.negative_placeholder);
        let (n, _) = build(&json!({"prompt": "girl", "negative": "", "seed": 0}), &wf(g), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["6"]["inputs"]["text"], "masterpiece, girl");
        assert_eq!(n["7"]["inputs"]["text"], ", lowres");
        let mut no_save = sdxl();
        no_save.as_object_mut().unwrap().remove("9");
        assert!(analyze(&no_save, "txt2img").unwrap_err().contains("Save Image"));
        assert!(analyze(&json!({"nodes": [], "links": []}), "txt2img").unwrap_err().contains("匯出（API）"));
        assert!(analyze(&sdxl(), "video").unwrap_err().contains("文生圖"));
    }

    #[test]
    fn marked_slots_win_over_auto() {
        let mut g = sdxl_inpaint();
        g["13"] = json!({"class_type": "LoadImageMask", "inputs": {"image": "m.png", "channel": "red"}, "_meta": {"title": "遮罩 {{mask}}"}});
        g["14"] = json!({"class_type": "LoadImage", "inputs": {"image": "ref.png"}, "_meta": {"title": "{{ref2}}"}});
        g["15"] = json!({"class_type": "LoadImage", "inputs": {"image": "ref.png"}, "_meta": {"title": "{{ref1}} 第一張"}});
        g["10"]["_meta"] = json!({"title": "{{image}}"});
        // 兩個 Checkpoint：標了 {{model}} 的算，不管取樣器接哪個
        g["20"] = json!({"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "other.safetensors"}, "_meta": {"title": "{{model}}"}});
        // 兩個取樣器，只有標的那個算
        g["21"] = json!({"class_type": "KSampler", "inputs": {"seed": 2, "steps": 8, "cfg": 2, "sampler_name": "euler", "scheduler": "simple", "denoise": 1,
                   "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["12", 0]}, "_meta": {"title": "細修 {{sampler}}"}});
        let a = analyze(&g, "inpaint").unwrap();
        assert_eq!(a.image.as_deref(), Some("10"));
        assert_eq!(a.mask.as_deref(), Some("13"));
        assert!(a.image_mask_used);
        assert_eq!(a.refs, vec![(1, "15".to_string()), (2, "14".to_string())]);
        assert_eq!(a.model, Some(ModelSlot { node: "20".into(), key: "ckpt_name".into(), clip: true, marked: true }));
        assert_eq!(a.samplers.len(), 2);
        assert_eq!(a.active_samplers().len(), 1);
        assert_eq!(a.active_samplers()[0].node, "21");
        assert_eq!(a.seeds.len(), 2);
        // 同一個標記標兩個節點、標了卻沒有欄位：錯
        g["16"] = json!({"class_type": "LoadImage", "inputs": {"image": "x.png"}, "_meta": {"title": "{{image}}"}});
        assert!(analyze(&g, "inpaint").unwrap_err().contains("只能標一個"));
        g.as_object_mut().unwrap().remove("16");
        g["17"] = json!({"class_type": "LoadImage", "inputs": {"image": "x.png"}, "_meta": {"title": "{{ref1}}"}});
        assert!(analyze(&g, "inpaint").unwrap_err().contains("ref1"));
        g.as_object_mut().unwrap().remove("17");
        g["22"] = json!({"class_type": "VAELoader", "inputs": {"vae_name": "x.safetensors"}, "_meta": {"title": "{{sampler}}"}});
        assert!(analyze(&g, "inpaint").unwrap_err().contains("{{sampler}}"));
    }

    #[test]
    fn validation_per_mode() {
        // 文生圖不需要輸入圖；圖生圖、放大要
        assert!(analyze(&sdxl(), "txt2img").is_ok());
        assert!(analyze(&sdxl(), "img2img").unwrap_err().contains("Load Image"));
        assert!(analyze(&sdxl(), "upscale").unwrap_err().contains("輸入圖"));
        assert!(analyze(&sdxl_img2img(), "img2img").is_ok());
        // 局部重繪：沒標 {{mask}}、MASK 輸出也沒接去用 → 講清楚怎麼標
        let e = analyze(&sdxl_img2img(), "inpaint").unwrap_err();
        assert!(e.contains("{{mask}}") && e.contains("MASK"), "{e}");
        assert!(analyze(&sdxl_inpaint(), "inpaint").is_ok());
        // 兩個沒標的 Load Image：不猜
        let mut two = sdxl_img2img();
        two["13"] = json!({"class_type": "LoadImage", "inputs": {"image": "b.png"}});
        let e = analyze(&two, "img2img").unwrap_err();
        assert!(e.contains("好幾個 Load Image") && e.contains("{{image}}"), "{e}");
        two["13"]["_meta"] = json!({"title": "{{image}}"});
        assert_eq!(analyze(&two, "img2img").unwrap().image.as_deref(), Some("13"));
        // 多圖參考一定要 {{ref1}}
        assert!(analyze(&sdxl_img2img(), "multiref").unwrap_err().contains("{{ref1}}"));
        // 放大不需要提示詞：沒有文字框也能存
        let upscale = json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "a.png"}},
            "2": {"class_type": "UpscaleModelLoader", "inputs": {"model_name": "4x.pth"}},
            "3": {"class_type": "ImageUpscaleWithModel", "inputs": {"upscale_model": ["2", 0], "image": ["1", 0]}},
            "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "x"}}
        });
        let a = analyze(&upscale, "upscale").unwrap();
        assert!(a.prompt.is_empty() && a.model.is_none() && a.image.as_deref() == Some("1"));
        assert!(analyze(&upscale, "txt2img").unwrap_err().contains("提示詞"));
        // UpscaleModelLoader 的 model_name 不是主模型（不在 model 連線上）
        let d = input_dir("upscale");
        let (n, _) = build(&json!({"seed": 1, "init_image": "base.png"}), &json!({"name": "up", "graphs": {"upscale": upscale}}), "upscale", &d, 0).unwrap();
        assert_eq!(n["1"]["inputs"]["image"], "base.png");
        assert_eq!(n["4"]["inputs"]["filename_prefix"], "AnimaWeb/upscale");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn img2img_denoise_and_page_sampler() {
        let d = input_dir("i2i");
        let mut set_ = json!({"name": "i2i", "graphs": {"img2img": sdxl_img2img()}});
        let p = json!({"prompt": "x", "seed": 3, "init_image": "base.png", "denoise": 0.7, "steps": 30, "cfg": 5.5, "sampler": "dpmpp_2m", "scheduler": "karras", "width": 512, "height": 512});
        let (n, info) = build(&p, &set_, "img2img", &d, 0).unwrap();
        assert_eq!(n["10"]["inputs"]["image"], "base.png");
        assert_eq!(n["3"]["inputs"]["denoise"], 0.7);
        // 沒開「用生成頁的取樣參數」：步數、CFG、取樣器照工作流
        assert_eq!(n["3"]["inputs"]["steps"], 20);
        assert_eq!(n["3"]["inputs"]["cfg"], 7);
        assert_eq!(n["3"]["inputs"]["sampler_name"], "euler");
        assert_eq!(n["9"]["inputs"]["filename_prefix"], "AnimaWeb/img2img");
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(8), Some(6)), "圖生圖回報輸入圖的尺寸");
        set_["options"] = json!({"page_sampler": true});
        let (n, _) = build(&p, &set_, "img2img", &d, 0).unwrap();
        assert_eq!(n["3"]["inputs"]["steps"], 30);
        assert_eq!(n["3"]["inputs"]["cfg"], 5.5);
        assert_eq!(n["3"]["inputs"]["sampler_name"], "dpmpp_2m");
        assert_eq!(n["3"]["inputs"]["scheduler"], "karras");
        // 缺輸入圖：要講清楚；檔案不在：用 api_gen 認得的「找不到輸入圖片：」開頭
        let e = build(&json!({"prompt": "x", "seed": 3}), &set_, "img2img", &d, 0).unwrap_err().0;
        assert!(e.contains("輸入圖"), "{e}");
        let e = build(&json!({"prompt": "x", "init_image": "nope.png"}), &set_, "img2img", &d, 0).unwrap_err().0;
        assert!(e.starts_with("找不到輸入圖片：nope.png"), "{e}");
        // 標了 {{sampler}} 的節點：沒開開關也套；只套標的
        let mut g = sdxl_img2img();
        g["3"]["_meta"] = json!({"title": "{{sampler}}"});
        g["21"] = json!({"class_type": "KSampler", "inputs": {"seed": 2, "steps": 8, "cfg": 2, "sampler_name": "euler", "scheduler": "simple", "denoise": 0.3,
                   "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["11", 0]}});
        let (n, _) = build(&p, &json!({"name": "m", "graphs": {"img2img": g}}), "img2img", &d, 0).unwrap();
        assert_eq!(n["3"]["inputs"]["steps"], 30);
        assert_eq!(n["3"]["inputs"]["denoise"], 0.7);
        assert_eq!(n["21"]["inputs"]["steps"], 8);
        assert_eq!(n["21"]["inputs"]["denoise"], 0.3, "沒標的取樣器不動");
        assert_eq!(n["21"]["inputs"]["seed"], 3, "但種子每個都改");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn denoise_falls_back_to_scheduler_nodes() {
        // Flux 這類：取樣用 SamplerCustomAdvanced，重繪幅度在 BasicScheduler
        let g = json!({
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "flux.safetensors", "weight_dtype": "default"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "x", "clip": ["9", 0]}},
            "3": {"class_type": "BasicGuider", "inputs": {"model": ["1", 0], "conditioning": ["2", 0]}},
            "4": {"class_type": "BasicScheduler", "inputs": {"model": ["1", 0], "scheduler": "simple", "steps": 20, "denoise": 1.0}},
            "5": {"class_type": "RandomNoise", "inputs": {"noise_seed": 1}},
            "6": {"class_type": "KSamplerSelect", "inputs": {"sampler_name": "euler"}},
            "7": {"class_type": "SamplerCustomAdvanced", "inputs": {"noise": ["5", 0], "guider": ["3", 0], "sampler": ["6", 0], "sigmas": ["4", 0], "latent_image": ["10", 0]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["9", 0]}},
            "9": {"class_type": "VAELoader", "inputs": {"vae_name": "ae.safetensors"}},
            "10": {"class_type": "VAEEncode", "inputs": {"pixels": ["11", 0], "vae": ["9", 0]}},
            "11": {"class_type": "LoadImage", "inputs": {"image": "a.png"}},
            "12": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "x"}}
        });
        let a = analyze(&g, "img2img").unwrap();
        // guider 那條線：取樣器 → BasicGuider → UNETLoader，不是 Checkpoint，沒有 CLIP
        assert_eq!(a.model, Some(ModelSlot { node: "1".into(), key: "unet_name".into(), clip: false, marked: false }));
        assert!(a.samplers.is_empty());
        assert_eq!(a.denoise_nodes, vec!["4".to_string()]);
        let d = input_dir("flux");
        let (n, _) = build(&json!({"prompt": "y", "seed": 2, "init_image": "base.png", "denoise": 0.45}), &json!({"name": "f", "graphs": {"img2img": g}}), "img2img", &d, 0).unwrap();
        assert_eq!(n["4"]["inputs"]["denoise"], 0.45);
        assert_eq!(n["5"]["inputs"]["noise_seed"], 2);
        assert_eq!(n["2"]["inputs"]["text"], "y");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inpaint_writes_alpha_masks() {
        let d = input_dir("inpaint");
        let set_ = json!({"name": "inp", "graphs": {"inpaint": sdxl_inpaint()}});
        let p = json!({"prompt": "x", "seed": 1, "init_image": "base.png", "mask": "mask.png", "denoise": 0.8});
        let (n, _) = build(&p, &set_, "inpaint", &d, 0).unwrap();
        // 輸入圖的 MASK 輸出有被用：換成「RGB＝原圖、alpha＝255−遮罩紅色」的 PNG
        let name = n["10"]["inputs"]["image"].as_str().unwrap().to_string();
        assert!(name.starts_with("base_") && name.ends_with("_wfmask.png"), "{name}");
        let img = image::open(d.join(&name)).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (8, 6));
        assert_eq!(img.get_pixel(1, 2).0, [11, 22, 30, 0], "遮罩白色的地方 alpha=0（ComfyUI 的 MASK=1）");
        assert_eq!(img.get_pixel(6, 2).0, [16, 22, 30, 255], "遮罩黑色的地方 alpha=255，RGB 還是原圖");
        assert_eq!(n["3"]["inputs"]["denoise"], 0.8);
        // 同一份內容再組一次：同一個檔名、不重寫
        let (n2, _) = build(&p, &set_, "inpaint", &d, 1).unwrap();
        assert_eq!(n2["10"]["inputs"]["image"].as_str().unwrap(), name);
        // 另外標了 {{mask}} 的 Load Image (as Mask)：RGB＝遮罩、alpha＝255−遮罩
        let mut g = sdxl_inpaint();
        g["13"] = json!({"class_type": "LoadImageMask", "inputs": {"image": "old.png", "channel": "red"}, "_meta": {"title": "{{mask}}"}});
        g["12"]["inputs"]["mask"] = json!(["13", 0]);
        let a = analyze(&g, "inpaint").unwrap();
        assert!(!a.image_mask_used && a.mask.as_deref() == Some("13"));
        let (n, _) = build(&p, &json!({"name": "m", "graphs": {"inpaint": g}}), "inpaint", &d, 0).unwrap();
        assert_eq!(n["10"]["inputs"]["image"], "base.png", "輸入圖的 MASK 沒人用：原圖直接給");
        let name = n["13"]["inputs"]["image"].as_str().unwrap().to_string();
        assert!(name.ends_with("_wfmaskonly.png"), "{name}");
        let img = image::open(d.join(&name)).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(1, 2).0, [255, 255, 255, 0]);
        assert_eq!(img.get_pixel(6, 2).0, [0, 0, 0, 255]);
        // 沒遮罩、遮罩是空的、檔案不在
        assert!(build(&json!({"prompt": "x", "init_image": "base.png"}), &set_, "inpaint", &d, 0).unwrap_err().0.contains("遮罩"));
        image::RgbImage::new(8, 6).save(d.join("empty.png")).unwrap();
        let e = build(&json!({"prompt": "x", "init_image": "base.png", "mask": "empty.png"}), &set_, "inpaint", &d, 0).unwrap_err().0;
        assert!(e.contains("遮罩是空的"), "{e}");
        let e = build(&json!({"prompt": "x", "init_image": "base.png", "mask": "gone.png"}), &set_, "inpaint", &d, 0).unwrap_err().0;
        assert!(e.starts_with("找不到輸入圖片：gone.png"), "{e}");
        // 遮罩尺寸跟原圖不一樣：縮成原圖大小
        image::RgbImage::from_fn(16, 12, |_, _| image::Rgb([255, 255, 255])).save(d.join("big.png")).unwrap();
        let (n, _) = build(&json!({"prompt": "x", "init_image": "base.png", "mask": "big.png"}), &set_, "inpaint", &d, 0).unwrap();
        let img = image::open(d.join(n["10"]["inputs"]["image"].as_str().unwrap())).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (8, 6));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn multiref_images() {
        let d = input_dir("ref");
        std::fs::copy(d.join("base.png"), d.join("r2.png")).unwrap();
        let mut g = sdxl();
        g["10"] = json!({"class_type": "LoadImage", "inputs": {"image": "a.png"}, "_meta": {"title": "{{ref1}}"}});
        g["11"] = json!({"class_type": "LoadImage", "inputs": {"image": "b.png"}, "_meta": {"title": "{{ref2}}"}});
        g["12"] = json!({"class_type": "LoadImage", "inputs": {"image": "c.png"}, "_meta": {"title": "{{ref3}}"}});
        let set_ = json!({"name": "mr", "graphs": {"multiref": g}});
        let p = json!({"prompt": "x", "seed": 1, "refs": [{"image": "base.png", "role": "body"}, {"image": ""}, {"image": "r2.png"}]});
        let (n, _) = build(&p, &set_, "multiref", &d, 0).unwrap();
        // 空的參考圖略過，跟內建一樣照順序數
        assert_eq!(n["10"]["inputs"]["image"], "base.png");
        assert_eq!(n["11"]["inputs"]["image"], "r2.png");
        assert_eq!(n["12"]["inputs"]["image"], "c.png", "生成頁沒給第三張：照工作流原本的");
        assert!(build(&json!({"prompt": "x", "refs": []}), &set_, "multiref", &d, 0).unwrap_err().0.contains("參考圖"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lora_injection_checkpoint() {
        let mut g = sdxl();
        // 工作流自己有一個 LoRA 節點，接在載入節點後面
        g["20"] = json!({"class_type": "LoraLoader", "inputs": {"model": ["4", 0], "clip": ["4", 1], "lora_name": "mine.safetensors", "strength_model": 0.8, "strength_clip": 0.8}});
        g["3"]["inputs"]["model"] = json!(["20", 0]);
        g["6"]["inputs"]["clip"] = json!(["20", 1]);
        let p = json!({"prompt": "x", "seed": 1, "loras": [
            {"name": "a.safetensors", "strength": 0.6},
            {"name": "b.safetensors", "strength": 1.2, "strength_clip": 0.5},
            {"name": "off.safetensors", "strength": 1, "enabled": false},
            {"name": "zero.safetensors", "strength": 0}]});
        let (n, _) = build(&p, &wf(g), "txt2img", Path::new("."), 0).unwrap();
        // 載入節點 → a → b → 原本的 LoRA 節點 → 取樣器／文字框
        assert_eq!(n["anima_lora_1"]["class_type"], "LoraLoader");
        assert_eq!(n["anima_lora_1"]["inputs"]["model"], json!(["4", 0]));
        assert_eq!(n["anima_lora_1"]["inputs"]["clip"], json!(["4", 1]));
        assert_eq!(n["anima_lora_1"]["inputs"]["strength_model"], 0.6);
        assert_eq!(n["anima_lora_1"]["inputs"]["strength_clip"], 0.6);
        assert_eq!(n["anima_lora_2"]["inputs"]["model"], json!(["anima_lora_1", 0]));
        assert_eq!(n["anima_lora_2"]["inputs"]["clip"], json!(["anima_lora_1", 1]));
        assert_eq!(n["anima_lora_2"]["inputs"]["strength_clip"], 0.5);
        assert_eq!(n["20"]["inputs"]["model"], json!(["anima_lora_2", 0]));
        assert_eq!(n["20"]["inputs"]["clip"], json!(["anima_lora_2", 1]));
        assert_eq!(n["20"]["inputs"]["lora_name"], "mine.safetensors", "原本的 LoRA 節點保留");
        assert!(n.get("anima_lora_3").is_none());
        // VAE 是載入節點的第 2 個輸出，不動
        assert_eq!(n["8"]["inputs"]["vae"], json!(["4", 2]));
        // 沒有原本的 LoRA：取樣器和文字框直接改接到鏈尾
        let (n, _) = build(&p, &wf(sdxl()), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["3"]["inputs"]["model"], json!(["anima_lora_2", 0]));
        assert_eq!(n["6"]["inputs"]["clip"], json!(["anima_lora_2", 1]));
        assert_eq!(n["7"]["inputs"]["clip"], json!(["anima_lora_2", 1]));
        // 節點編號撞名就往後挑
        let mut g = sdxl();
        g["anima_lora_1"] = json!({"class_type": "LoraLoader", "inputs": {"model": ["4", 0], "clip": ["4", 1], "lora_name": "z", "strength_model": 1, "strength_clip": 1}});
        let (n, _) = build(&json!({"prompt": "x", "loras": [{"name": "a", "strength": 1}]}), &wf(g), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["anima_lora_2"]["inputs"]["lora_name"], "a");
        assert_eq!(n["anima_lora_1"]["inputs"]["lora_name"], "z");
    }

    #[test]
    fn lora_injection_model_only() {
        // UNETLoader + 獨立的 CLIPLoader（Flux／Anima 這種）：只接 MODEL
        let g = json!({
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "u.safetensors", "weight_dtype": "default"}},
            "2": {"class_type": "CLIPLoader", "inputs": {"clip_name": "c.safetensors", "type": "stable_diffusion"}},
            "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "x", "clip": ["2", 0]}},
            "4": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 4, "model": ["1", 0], "positive": ["3", 0], "negative": ["3", 0], "latent_image": ["5", 0]}},
            "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 512, "height": 512, "batch_size": 1}},
            "6": {"class_type": "SaveImage", "inputs": {"images": ["4", 0], "filename_prefix": "x"}}
        });
        let (n, _) = build(&json!({"prompt": "y", "loras": [{"name": "a", "strength": 0.7}, {"name": "b", "strength": 1}]}), &wf(g), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["anima_lora_1"]["class_type"], "LoraLoaderModelOnly");
        assert_eq!(n["anima_lora_1"]["inputs"], json!({"model": ["1", 0], "lora_name": "a", "strength_model": 0.7}));
        assert_eq!(n["anima_lora_2"]["inputs"]["model"], json!(["anima_lora_1", 0]));
        assert_eq!(n["4"]["inputs"]["model"], json!(["anima_lora_2", 0]));
        assert_eq!(n["3"]["inputs"]["clip"], json!(["2", 0]), "獨立的 CLIPLoader 不動");
    }

    #[test]
    fn lora_without_loader_is_an_error() {
        let g = json!({
            "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "x", "clip": ["9", 0]}},
            "2": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 4, "model": ["8", 0], "positive": ["1", 0], "negative": ["1", 0], "latent_image": ["5", 0]}},
            "5": {"class_type": "EmptyLatentImage", "inputs": {"width": 512, "height": 512, "batch_size": 1}},
            "6": {"class_type": "SaveImage", "inputs": {"images": ["2", 0], "filename_prefix": "x"}}
        });
        let w = wf(g);
        assert!(build(&json!({"prompt": "y"}), &w, "txt2img", Path::new("."), 0).is_ok());
        let e = build(&json!({"prompt": "y", "loras": [{"name": "a", "strength": 1}]}), &w, "txt2img", Path::new("."), 0).unwrap_err().0;
        assert!(e.contains("{{model}}"), "{e}");
        let e = build(&json!({"prompt": "y", "wf_model_from_page": true, "model": "m"}), &w, "txt2img", Path::new("."), 0).unwrap_err().0;
        assert!(e.contains("{{model}}"), "{e}");
    }

    #[test]
    fn model_from_page_and_overrides() {
        let mut set_ = wf(sdxl());
        set_["graphs"]["txt2img"]["4"]["_meta"] = json!({"title": "{{model}}"});
        let p = json!({"prompt": "x", "seed": 1, "model": "mine.safetensors", "family": "sdxl", "wf_model_from_page": true});
        let (n, info) = build(&p, &set_, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["4"]["inputs"]["ckpt_name"], "mine.safetensors");
        assert_eq!(info["family"], "sdxl", "取代內建功能時回報原本的家族");
        // 沒開 wf_model_from_page：模型照工作流
        let (n, info) = build(&json!({"prompt": "x", "model": "wf:abc"}), &set_, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["4"]["inputs"]["ckpt_name"], "sdxl.safetensors");
        assert_eq!(info["family"], "custom");
        // 參數面板：最後套上；連線、不存在的鍵、存圖前綴不理；數字字串轉成數字
        let p = json!({"prompt": "x", "seed": 7, "steps": 99, "wf_overrides": {
            "3.steps": 33, "3.cfg": "4.5", "3.seed": 1234, "3.model": 5, "3.sampler_name": "dpmpp_2m", "99.steps": 1, "3.nope": 1,
            "9.filename_prefix": "hack", "bad key": 1, "3.scheduler": null, "4.ckpt_name": "other.safetensors"}});
        let (n, _) = build(&p, &set_, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["3"]["inputs"]["steps"], 33);
        assert_eq!(n["3"]["inputs"]["cfg"], 4.5);
        assert_eq!(n["3"]["inputs"]["seed"], 1234, "面板的值比自動槽位優先");
        assert_eq!(n["3"]["inputs"]["model"], json!(["4", 0]), "連線不能改");
        assert_eq!(n["3"]["inputs"]["sampler_name"], "dpmpp_2m");
        assert_eq!(n["3"]["inputs"]["scheduler"], "normal");
        assert!(n["3"]["inputs"].get("nope").is_none() && n.get("99").is_none());
        assert_eq!(n["9"]["inputs"]["filename_prefix"], "AnimaWeb/txt2img");
        assert_eq!(n["4"]["inputs"]["ckpt_name"], "other.safetensors");
    }

    #[test]
    fn fields_from_object_info() {
        let mut g = sdxl();
        g["4"]["inputs"]["ckpt_name"] = json!("someone_elses.safetensors");
        g["20"] = json!({"class_type": "LoraLoader", "inputs": {"model": ["4", 0], "clip": ["4", 1], "lora_name": "mine.safetensors", "strength_model": 0.8, "strength_clip": 0.8}});
        g["21"] = json!({"class_type": "Mystery", "inputs": {"flag": true, "note": "hi", "n": 3, "f": 0.5}});
        let oi = json!({
            "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["sdxl.safetensors", "other.safetensors"]]}}},
            "KSampler": {"input": {"required": {
                "seed": ["INT", {"default": 0, "min": 0, "max": 18446744073709551615u64}],
                "steps": ["INT", {"default": 20, "min": 1, "max": 10000}],
                "cfg": ["FLOAT", {"default": 8.0, "min": 0.0, "max": 100.0, "step": 0.1, "round": 0.01}],
                "sampler_name": ["COMBO", {"options": ["euler", "dpmpp_2m"]}],
                "scheduler": [["normal", "karras"]],
                "denoise": ["FLOAT", {"default": 1.0, "min": 0.0, "max": 1.0, "step": 0.01}],
                "model": ["MODEL"], "positive": ["CONDITIONING"]}}},
            "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true, "dynamicPrompts": true}], "clip": ["CLIP"]}}},
            "EmptyLatentImage": {"input": {"required": {"width": ["INT", {"min": 16, "max": 16384, "step": 8}], "height": ["INT", {}], "batch_size": ["INT", {}]}}},
            "LoraLoader": {"input": {"required": {"lora_name": [["mine.safetensors"]], "strength_model": ["FLOAT", {"min": -100, "max": 100}], "strength_clip": ["FLOAT", {}]}}},
            "SaveImage": {"input": {"required": {"filename_prefix": ["STRING", {}]}}},
            "VAEDecode": {"input": {"required": {}}}
        });
        let fs = fields(&g, "txt2img", &oi);
        let f = |key: &str| fs.iter().find(|f| f["key"] == key).unwrap_or_else(|| panic!("沒有欄位 {key}"));
        // 連線不列
        assert!(fs.iter().all(|f| f["input"] != "model" && f["input"] != "clip" && f["input"] != "samples"));
        // 取樣參數：常用，型別範圍從 object_info 來
        let steps = f("3.steps");
        assert_eq!((steps["type"].as_str(), steps["group"].as_str(), steps["auto"].as_bool()), (Some("INT"), Some("common"), Some(false)));
        assert_eq!((steps["min"].as_i64(), steps["max"].as_i64(), steps["value"].as_i64()), (Some(1), Some(10000), Some(20)));
        assert_eq!(f("3.cfg")["type"], "FLOAT");
        assert_eq!(f("3.cfg")["step"], 0.1);
        // 新舊兩種下拉寫法
        assert_eq!(f("3.sampler_name")["type"], "COMBO");
        assert_eq!(f("3.sampler_name")["options"], json!(["euler", "dpmpp_2m"]));
        assert_eq!(f("3.scheduler")["options"], json!(["normal", "karras"]));
        // 主模型檔：常用；選項裡沒有 → missing
        let ckpt = f("4.ckpt_name");
        assert_eq!((ckpt["group"].as_str(), ckpt["missing"].as_bool(), ckpt["type"].as_str()), (Some("common"), Some(true), Some("COMBO")));
        assert_eq!(f("3.sampler_name")["missing"], false);
        // LoRA 節點：檔名和強度常用
        assert_eq!(f("20.lora_name")["group"], "common");
        assert_eq!(f("20.strength_model")["group"], "common");
        assert_eq!(f("20.strength_clip")["type"], "FLOAT");
        // 生圖站自己填的：auto
        assert_eq!(f("6.text")["auto"], true);
        assert_eq!(f("7.text")["auto"], true);
        assert_eq!(f("6.text")["multiline"], true);
        assert_eq!(f("3.seed")["auto"], true);
        assert_eq!(f("5.width")["auto"], true);
        assert_eq!(f("5.batch_size")["auto"], true);
        assert_eq!(f("9.filename_prefix")["auto"], true);
        assert_eq!(f("3.denoise")["group"], "common");
        // 圖生圖時寬高不是自動填的
        let fs2 = fields(&g, "img2img", &oi);
        assert_eq!(fs2.iter().find(|f| f["key"] == "5.width").unwrap()["auto"], false);
        // 這台 ComfyUI 沒有的節點：看值猜型別、標 node_missing
        assert_eq!(f("21.flag")["type"], "BOOLEAN");
        assert_eq!(f("21.note")["type"], "STRING");
        assert_eq!(f("21.n")["type"], "INT");
        assert_eq!(f("21.f")["type"], "FLOAT");
        assert_eq!(f("21.flag")["node_missing"], true);
        assert_eq!(f("3.steps")["node_missing"], false);
        assert_eq!(f("21.flag")["group"], "advanced");
        // 沒拿到 object_info（空物件）：全部看值猜，不亂標缺節點
        let blind = fields(&g, "txt2img", &json!({}));
        assert!(blind.iter().all(|f| f["node_missing"] == false));
        assert_eq!(blind.iter().find(|f| f["key"] == "3.steps").unwrap()["type"], "INT");
    }

    #[test]
    fn mark_built_in_graph_for_export() {
        // 內建 Anima 文生圖：graphs::build 組出來的
        let dir = std::env::temp_dir();
        let p = json!({"mode": "txt2img", "family": "anima", "model": "anima-test.safetensors", "prompt": "1girl, {red|blue} hair", "negative": "bad hands",
                       "seed": 4, "width": 1024, "height": 1024, "steps": 25, "cfg": 4.5, "sampler": "euler", "scheduler": "simple"});
        let (nodes_, info) = graphs::build(&p, &dir, 0).unwrap();
        let mut pp = p.clone();
        pp["prompt_resolved"] = info["prompt"].clone();
        pp["negative_resolved"] = info["negative"].clone();
        let marked = mark_for_export(&nodes_, &pp);
        let m = marked.as_object().unwrap();
        let texts: Vec<&str> = m.values().filter(|n| n["class_type"] == "CLIPTextEncode").filter_map(|n| n["inputs"]["text"].as_str()).collect();
        assert!(texts.contains(&"{{prompt}}") && texts.contains(&"{{negative}}"), "{texts:?}");
        let unet = m.values().find(|n| n["class_type"] == "UNETLoader").unwrap();
        assert_eq!(unet["_meta"]["title"], "{{model}}");
        let ks = m.values().find(|n| n["class_type"] == "KSampler").unwrap();
        assert_eq!(ks["_meta"]["title"], "{{sampler}}");
        // 記下模式：匯入時自動判斷
        let save = m.values().find(|n| n["class_type"] == "SaveImage").unwrap();
        assert_eq!(save["_meta"]["title"], "{{mode:txt2img}} {{family:anima}}");
        assert_eq!(mode_mark(&marked), Some("txt2img"));
        assert_eq!(family_mark(&marked).as_deref(), Some("anima"));
        // 原本的圖沒被動到
        assert!(nodes_.values().all(|n| n.get("_meta").is_none()));
        // 匯回去：看得懂，提示詞用標記，模型和取樣器找得到
        let a = analyze(&marked, "txt2img").unwrap();
        assert!(a.placeholder && a.negative_placeholder);
        assert_eq!(a.model.as_ref().map(|s| s.marked), Some(true));
        assert!(a.samplers.iter().any(|s| s.marked));
        // 再建一次，同種子同參數 → 跟內建一樣的節點圖（取代內建功能）
        let set_ = json!({"name": "builtin", "graphs": {"txt2img": marked}, "source": "builtin"});
        let p2 = json!({"prompt": "1girl, {red|blue} hair", "negative": "bad hands", "seed": 4, "width": 1024, "height": 1024, "model": "anima-test.safetensors",
                        "wf_model_from_page": true, "family": "anima", "steps": 25, "cfg": 4.5, "sampler": "euler", "scheduler": "simple"});
        let (rebuilt, info2) = build(&p2, &set_, "txt2img", &dir, 0).unwrap();
        assert_eq!(info2["prompt"], info["prompt"]);
        for (id, n) in &nodes_ {
            assert_eq!(rebuilt[id]["class_type"], n["class_type"], "{id}");
            assert_eq!(rebuilt[id]["inputs"], n["inputs"], "節點 {id}");
        }
        assert_eq!(rebuilt.len(), nodes_.len());
    }

    #[test]
    fn mark_inpaint_and_refs_for_export() {
        let d = input_dir("mark");
        let p = json!({"mode": "inpaint", "family": "anima", "model": "anima-test.safetensors", "prompt": "fix", "negative": "", "seed": 1,
                       "init_image": "base.png", "mask": "mask.png", "denoise": 1.0, "steps": 20, "cfg": 4, "sampler": "euler", "scheduler": "simple"});
        let (n, _) = graphs::build(&p, &d, 0).unwrap();
        let marked = mark_for_export(&n, &p);
        let titles: Vec<(String, String)> = marked
            .as_object()
            .unwrap()
            .values()
            .filter_map(|n| Some((n["class_type"].as_str()?.to_string(), n["_meta"]["title"].as_str()?.to_string())))
            .collect();
        assert!(titles.contains(&("LoadImage".into(), "{{image}}".into())), "{titles:?}");
        assert!(titles.contains(&("LoadImageMask".into(), "{{mask}}".into())), "{titles:?}");
        // 匯回去：遮罩、輸入圖都找得到，模式認得出來
        let a = analyze(&marked, "inpaint").unwrap();
        assert!(a.image.is_some() && a.mask.is_some());
        assert_eq!(mode_mark(&marked), Some("inpaint"));
        // 多圖參考的 {{refN}}：直接用 Load Image 節點測
        let mut g = Nodes::new();
        g.insert("1".into(), json!({"class_type": "LoadImage", "inputs": {"image": "a.png"}, "_meta": {"title": "我的圖"}}));
        g.insert("2".into(), json!({"class_type": "LoadImage", "inputs": {"image": "b.png"}}));
        let p = json!({"refs": [{"image": "a.png"}, {"image": "b.png"}]});
        let out = mark_for_export(&g, &p);
        assert_eq!(out["1"]["_meta"]["title"], "我的圖 {{ref1}}", "原本的標題保留");
        assert_eq!(out["2"]["_meta"]["title"], "{{ref2}}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mode_mark_needs_a_known_mode() {
        let g = |t: &str| json!({"1": {"class_type": "SaveImage", "inputs": {}, "_meta": {"title": t}}});
        assert_eq!(mode_mark(&g("存檔 {{mode:upscale}}")), Some("upscale"));
        assert_eq!(mode_mark(&g("{{mode:video}}")), None);
        assert_eq!(mode_mark(&g("Save Image")), None);
        assert_eq!(mode_mark(&json!({})), None);
        assert_eq!(mode_mark(&json!("不是節點圖")), None);
    }

    #[test]
    fn family_mark_reads_known_codes_only() {
        let g = |t: &str| json!({"1": {"class_type": "SaveImage", "inputs": {}, "_meta": {"title": t}}});
        assert_eq!(family_mark(&g("{{mode:upscale}} {{family:anima}}")).as_deref(), Some("anima"));
        assert_eq!(family_mark(&g("存檔 {{family:qwen21}}")).as_deref(), Some("qwen21"));
        // other、custom、怪字元、沒收尾都不算
        assert_eq!(family_mark(&g("{{family:other}}")), None);
        assert_eq!(family_mark(&g("{{family:custom}}")), None);
        assert_eq!(family_mark(&g("{{family:a b}}")), None);
        assert_eq!(family_mark(&g("{{family:}}")), None);
        assert_eq!(family_mark(&g("{{family:anima")), None);
        assert_eq!(family_mark(&g("Save Image")), None);
        assert_eq!(family_mark(&json!("不是節點圖")), None);
        // 匯出時：family 是 other／custom 就不寫
        let mut n = Nodes::new();
        n.insert("9".into(), json!({"class_type": "SaveImage", "inputs": {"images": ["8", 0]}}));
        for (fam, want) in [("anima", "{{mode:upscale}} {{family:anima}}"), ("other", "{{mode:upscale}}"), ("custom", "{{mode:upscale}}")] {
            let out = mark_for_export(&n, &json!({"mode": "upscale", "family": fam}));
            assert_eq!(out["9"]["_meta"]["title"], want, "{fam}");
        }
    }

    /// 還沒放圖就匯出：補佔位圖（檔名固定、已經有就不重寫），組得出圖、標記照樣找得到
    #[test]
    fn export_fills_missing_images() {
        let d = input_dir("holder");
        let base = |mode: &str| {
            json!({"mode": mode, "family": "anima", "model": "anima-test.safetensors", "prompt": "x", "negative": "", "seed": 1, "denoise": 0.6,
                   "steps": 20, "cfg": 4, "sampler": "euler", "scheduler": "simple", "width": 1024, "height": 1024})
            .as_object()
            .unwrap()
            .clone()
        };
        // 文生圖不用圖
        let mut p = base("txt2img");
        assert!(export_placeholders(&mut p, &d).unwrap().is_empty());
        assert!(p.get("init_image").is_none());
        // 圖生圖：灰色 1024×1024
        let mut p = base("img2img");
        assert_eq!(export_placeholders(&mut p, &d).unwrap(), vec!["image"]);
        assert_eq!(p["init_image"], EXPORT_PLACEHOLDER);
        assert_eq!(image::image_dimensions(d.join(EXPORT_PLACEHOLDER)).unwrap(), (1024, 1024));
        // 局部重繪、有輸入圖沒遮罩：遮罩跟輸入圖一樣大，中間是白的
        let mut p = base("inpaint");
        p.insert("init_image".into(), json!("base.png"));
        assert_eq!(export_placeholders(&mut p, &d).unwrap(), vec!["mask"]);
        assert_eq!(p["init_image"], "base.png");
        assert_eq!(p["mask"], "anima_export_mask_8x6.png");
        let m = image::open(d.join("anima_export_mask_8x6.png")).unwrap().to_rgb8();
        assert_eq!(m.get_pixel(4, 3).0, [255, 255, 255]);
        assert_eq!(m.get_pixel(0, 0).0, [0, 0, 0]);
        // 暫存檔都改名完了
        assert!(std::fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part")));
        // 局部重繪、什麼都沒有：兩個都補，組得出圖，{{image}}、{{mask}} 標得上
        let mut p = base("inpaint");
        assert_eq!(export_placeholders(&mut p, &d).unwrap(), vec!["image", "mask"]);
        assert_eq!(p["mask"], "anima_export_mask_1024x1024.png");
        let pv = Value::Object(p);
        let (n, _) = graphs::build(&pv, &d, 0).unwrap();
        let marked = mark_for_export(&n, &pv);
        let a = analyze(&marked, "inpaint").unwrap();
        assert!(a.image.is_some() && a.mask.is_some());
        // 多圖參考：補一張參考圖；已經有參考圖就不動
        let mut p = base("multiref");
        assert_eq!(export_placeholders(&mut p, &d).unwrap(), vec!["ref"]);
        assert_eq!(p["refs"], json!([{"image": EXPORT_PLACEHOLDER}]));
        let mut p = base("multiref");
        p.insert("refs".into(), json!([{"image": "base.png"}]));
        assert!(export_placeholders(&mut p, &d).unwrap().is_empty());
        // 放大：已經有輸入圖就不動
        let mut p = base("upscale");
        p.insert("init_image".into(), json!("base.png"));
        assert!(export_placeholders(&mut p, &d).unwrap().is_empty());
        // 輸入圖的檔不見了：講清楚怎麼辦，不寫遮罩
        let mut p = base("inpaint");
        p.insert("init_image".into(), json!("gone.png"));
        assert!(export_placeholders(&mut p, &d).unwrap_err().starts_with("找不到輸入圖片：gone.png"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn old_format_is_read_as_txt2img() {
        let d = std::env::temp_dir().join(format!("anima_image_wf_old_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("舊的.json"), serde_json::to_string(&json!({"name": "舊的", "added": 1.5, "graph": sdxl()})).unwrap()).unwrap();
        let loaded = load(&d, "舊的").unwrap();
        assert_eq!(modes_of(&loaded), vec!["txt2img"]);
        assert_eq!(loaded["graphs"]["txt2img"], sdxl());
        assert_eq!(loaded["options"]["page_sampler"], false);
        assert_eq!(loaded["source"], "import");
        assert!(loaded.get("graph").is_none());
        let l = list(&d);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0]["modes"], json!(["txt2img"]));
        assert_eq!(l[0]["info"]["placeholder"], false);
        assert_eq!(l[0]["model_file"], "sdxl.safetensors");
        assert!(l[0].get("error").is_none());
        // 檔案沒被改
        assert!(std::fs::read_to_string(d.join("舊的.json")).unwrap().contains("\"graph\""));
        // 舊格式的組可以直接拿來生圖
        let (n, _) = build(&json!({"prompt": "x", "seed": 0}), &loaded, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["6"]["inputs"]["text"], "x");
        // 加一個模式進去：舊的 txt2img 保留，檔案變新格式
        save(&d, "", Some("舊的"), "img2img", &sdxl_img2img(), &Meta::default()).unwrap();
        let again = load(&d, "舊的").unwrap();
        assert_eq!(modes_of(&again), vec!["txt2img", "img2img"]);
        assert!(!std::fs::read_to_string(d.join("舊的.json")).unwrap().contains("\"graph\":"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn store_sets_and_modes() {
        let d = std::env::temp_dir().join(format!("anima_image_wf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let meta = Meta { family: Some("sdxl".into()), source: None };
        let e = save(&d, "我的 SDXL", None, "txt2img", &sdxl(), &meta).unwrap();
        assert_eq!(e["id"], "我的 SDXL");
        assert_eq!(e["family"], "sdxl");
        assert_eq!(e["modes"], json!(["txt2img"]));
        assert_eq!(list(&d).len(), 1);
        assert!(load(&d, "我的 SDXL").is_ok());
        assert!(load(&d, "../x").is_err());
        // 同名再存另一個模式：加進去，不是覆蓋
        let e = save(&d, "我的 SDXL", None, "img2img", &sdxl_img2img(), &Meta::default()).unwrap();
        assert_eq!(e["modes"], json!(["txt2img", "img2img"]));
        assert_eq!(e["family"], "sdxl", "沒給家族：不動");
        assert!(e["summary"]["img2img"]["image"].is_object());
        // 指定既有的組；組不存在要報錯
        save(&d, "ignored", Some("我的 SDXL"), "inpaint", &sdxl_inpaint(), &Meta { family: None, source: Some("builtin".into()) }).unwrap();
        let w = load(&d, "我的 SDXL").unwrap();
        assert_eq!(w["name"], "我的 SDXL", "指定組：名字不變");
        assert_eq!(w["source"], "builtin");
        assert_eq!(modes_of(&w), vec!["txt2img", "img2img", "inpaint"]);
        assert!(save(&d, "x", Some("不存在"), "txt2img", &sdxl(), &Meta::default()).unwrap_err().contains("找不到"));
        // 看不懂的圖不存、不弄壞原本的
        let e = save(&d, "我的 SDXL", None, "upscale", &sdxl(), &Meta::default()).unwrap_err();
        assert!(e.contains("輸入圖"), "{e}");
        assert!(save(&d, "  ", None, "txt2img", &sdxl(), &Meta::default()).unwrap_err().contains("名字"));
        assert_eq!(modes_of(&load(&d, "我的 SDXL").unwrap()).len(), 3);
        // 選項
        let e = set_options(&d, "我的 SDXL", true).unwrap();
        assert_eq!(e["options"]["page_sampler"], true);
        assert_eq!(load(&d, "我的 SDXL").unwrap()["options"]["page_sampler"], true);
        // 刪一個模式、刪到最後一個整組刪掉、整組刪
        assert_eq!(remove(&d, "我的 SDXL", Some("inpaint")), Ok(false));
        assert!(remove(&d, "我的 SDXL", Some("inpaint")).unwrap_err().contains("沒有"));
        assert_eq!(modes_of(&load(&d, "我的 SDXL").unwrap()), vec!["txt2img", "img2img"]);
        assert_eq!(remove(&d, "我的 SDXL", Some("txt2img")), Ok(false));
        assert_eq!(remove(&d, "我的 SDXL", Some("img2img")), Ok(true));
        assert!(list(&d).is_empty());
        save(&d, "再來", None, "txt2img", &sdxl(), &Meta::default()).unwrap();
        assert_eq!(remove(&d, "再來", None), Ok(true));
        assert!(remove(&d, "再來", None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn feature_flow_selection() {
        let flows = json!({"anima.inpaint": "我的重繪", "qwen21.upscale": "", "sdxl.txt2img": "x"});
        assert_eq!(pick_flow(&flows, "anima", "inpaint", false), Some("我的重繪"));
        // 請求硬要用內建的、沒指定、指定的是空字串、模式不是工作流組支援的
        assert_eq!(pick_flow(&flows, "anima", "inpaint", true), None);
        assert_eq!(pick_flow(&flows, "anima", "txt2img", false), None);
        assert_eq!(pick_flow(&flows, "qwen21", "upscale", false), None);
        assert_eq!(pick_flow(&flows, "moss", "inpaint", false), None);
        assert_eq!(pick_flow(&json!({"anima.video": "a", "anima.strip": "a"}), "anima", "video", false), None);
        assert_eq!(pick_flow(&json!({"anima.strip": "a"}), "anima", "strip", false), None);
        assert_eq!(pick_flow(&json!({}), "anima", "inpaint", false), None);
        assert_eq!(pick_flow(&Value::Null, "anima", "inpaint", false), None);
        // 鍵的格式
        assert!(flow_key_ok("anima.inpaint") && flow_key_ok("sdxl.multiref"));
        assert!(!flow_key_ok("anima") && !flow_key_ok(".inpaint") && !flow_key_ok("anima.video") && !flow_key_ok("a b.txt2img") && !flow_key_ok("anima.inpaint.x"));
    }

    // ---------------------------------------------------------------- 老手的工作流（2026-10-22 放寬）
    /// SDXL 的雙框編碼節點＋只有 Preview Image：以前「找不到存圖的節點」「找不到提示詞」整個不收
    #[test]
    fn sdxl_dual_text_and_preview_only() {
        let mut g = sdxl();
        g["6"] = json!({"class_type": "CLIPTextEncodeSDXL", "inputs": {"text_g": "a cat", "text_l": "a cat", "width": 1024, "height": 1024,
                         "crop_w": 0, "crop_h": 0, "target_width": 1024, "target_height": 1024, "clip": ["4", 1]}});
        g["9"] = json!({"class_type": "PreviewImage", "inputs": {"images": ["8", 0]}});
        // 旁邊多一個沒接圖的 Save Image：忽略，不報錯
        g["20"] = json!({"class_type": "SaveImage", "inputs": {"filename_prefix": "x"}});
        let a = analyze(&g, "txt2img").unwrap();
        assert_eq!(a.prompt, vec![("6".into(), "text_g".into()), ("6".into(), "text_l".into())]);
        assert_eq!(a.outputs, vec!["9".to_string()]);
        let (n, _) = build(&json!({"prompt": "a dog", "seed": 1, "width": 1024, "height": 1024}), &wf(g), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!((n["6"]["inputs"]["text_g"].as_str(), n["6"]["inputs"]["text_l"].as_str()), (Some("a dog"), Some("a dog")));
        assert_eq!(n["9"]["class_type"], "SaveImage", "預覽節點換成存圖，圖庫才收得到");
        assert_eq!(n["9"]["inputs"]["images"], json!(["8", 0]));
        assert_eq!(n["9"]["inputs"]["filename_prefix"], "AnimaWeb/txt2img");
        // WAS 的 Image Save 也算；同時有 SaveImage 時只算 SaveImage（不會變成兩張）
        let mut g2 = sdxl();
        g2["21"] = json!({"class_type": "Image Save", "inputs": {"images": ["8", 0], "output_path": "x"}});
        g2["22"] = json!({"class_type": "PreviewImage", "inputs": {"images": ["8", 0]}});
        assert_eq!(analyze(&g2, "txt2img").unwrap().outputs, vec!["9".to_string()]);
        g2.as_object_mut().unwrap().remove("9");
        assert_eq!(analyze(&g2, "txt2img").unwrap().outputs, vec!["21".to_string()]);
        // 影片、Latent 的存檔節點不算圖片輸出
        let mut g3 = sdxl();
        g3["9"] = json!({"class_type": "SaveAnimatedWEBP", "inputs": {"images": ["8", 0]}});
        assert!(analyze(&g3, "txt2img").unwrap_err().contains("找不到存圖的節點"));
    }

    /// Flux：CLIPTextEncodeFlux（clip_l＋t5xxl）、Guider＋SamplerCustomAdvanced；「用生成頁的步數…」改 BasicScheduler／KSamplerSelect；寬高對齊 16
    #[test]
    fn flux_text_chain_sampler_and_align() {
        let g = json!({
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "flux1-dev.safetensors", "weight_dtype": "default"}},
            "2": {"class_type": "CLIPTextEncodeFlux", "inputs": {"clip_l": "a cat", "t5xxl": "a cat", "guidance": 3.5, "clip": ["12", 0]}},
            "3": {"class_type": "BasicGuider", "inputs": {"model": ["1", 0], "conditioning": ["2", 0]}},
            "4": {"class_type": "BasicScheduler", "inputs": {"model": ["1", 0], "scheduler": "simple", "steps": 20, "denoise": 1.0}},
            "5": {"class_type": "RandomNoise", "inputs": {"noise_seed": 1}},
            "6": {"class_type": "KSamplerSelect", "inputs": {"sampler_name": "euler"}},
            "7": {"class_type": "SamplerCustomAdvanced", "inputs": {"noise": ["5", 0], "guider": ["3", 0], "sampler": ["6", 0], "sigmas": ["4", 0], "latent_image": ["10", 0]}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["11", 0]}},
            "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "flux"}},
            "10": {"class_type": "EmptySD3LatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 1}},
            "11": {"class_type": "VAELoader", "inputs": {"vae_name": "ae.safetensors"}},
            "12": {"class_type": "DualCLIPLoader", "inputs": {"clip_name1": "clip_l.safetensors", "clip_name2": "t5xxl.safetensors", "type": "flux"}}
        });
        let a = analyze(&g, "txt2img").unwrap();
        assert_eq!(a.prompt, vec![("2".into(), "clip_l".into()), ("2".into(), "t5xxl".into())]);
        assert!(a.page_sampler_ok(), "BasicScheduler、KSamplerSelect 可以套生成頁的步數、取樣器");
        let mut set_ = wf(g);
        set_["options"] = json!({"page_sampler": true});
        let p = json!({"prompt": "a dog", "seed": 9, "width": 1000, "height": 1000, "steps": 28, "sampler": "dpmpp_2m", "scheduler": "beta"});
        let (n, _) = build(&p, &set_, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!((n["2"]["inputs"]["clip_l"].as_str(), n["2"]["inputs"]["t5xxl"].as_str()), (Some("a dog"), Some("a dog")));
        assert_eq!(n["5"]["inputs"]["noise_seed"], 9);
        assert_eq!(n["4"]["inputs"]["steps"], 28);
        assert_eq!(n["4"]["inputs"]["scheduler"], "beta");
        assert_eq!(n["6"]["inputs"]["sampler_name"], "dpmpp_2m");
        assert_eq!(n["10"]["inputs"]["width"], 992, "Flux 對齊 16");
        // SDXL 照舊對齊 8
        let (n, _) = build(&json!({"prompt": "x", "seed": 1, "width": 1000, "height": 1000}), &wf(sdxl()), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["5"]["inputs"]["width"], 1000);
    }

    /// 種子、寬高、提示詞接在 Primitive 這類節點上：改那個節點（以前種子改不到，每次同一張）
    #[test]
    fn primitive_seed_size_and_text() {
        let mut g = sdxl();
        g["30"] = json!({"class_type": "PrimitiveInt", "inputs": {"value": 5}});
        g["31"] = json!({"class_type": "PrimitiveStringMultiline", "inputs": {"value": "a cat"}});
        g["32"] = json!({"class_type": "PrimitiveInt", "inputs": {"value": 768}});
        g["3"]["inputs"]["seed"] = json!(["30", 0]);
        g["6"]["inputs"]["text"] = json!(["31", 0]);
        g["5"]["inputs"]["width"] = json!(["32", 0]);
        let a = analyze(&g, "txt2img").unwrap();
        assert_eq!(a.prompt, vec![("31".into(), "value".into())]);
        assert_eq!(a.seeds, vec![("30".into(), "value".into())]);
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        let (n, info) = build(&json!({"prompt": "a dog", "seed": 42, "width": 640, "height": 512}), &wf(g.clone()), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["31"]["inputs"]["value"], "a dog");
        assert_eq!(n["30"]["inputs"]["value"], 42);
        assert_eq!(n["32"]["inputs"]["value"], 640);
        assert_eq!(n["5"]["inputs"]["width"], json!(["32", 0]), "連線保留");
        assert_eq!(n["5"]["inputs"]["height"], 512);
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(640), Some(512)));
        // 接到看不懂的節點：收，但提醒
        g["30"] = json!({"class_type": "SomeCustomSeed", "inputs": {"mode": "random"}});
        let a = analyze(&g, "txt2img").unwrap();
        assert!(a.warnings.iter().any(|w| w.contains("種子") && w.contains("#30")), "{:?}", a.warnings);
    }

    /// SDXL base＋refiner 各一個文字框（文字一樣）＋FaceDetailer 自己的提示詞：base、refiner 一起換，FaceDetailer 保留並提醒
    #[test]
    fn refiner_and_face_detailer_prompts() {
        let mut g = sdxl();
        g["13"] = json!({"class_type": "CLIPTextEncode", "inputs": {"text": "a cat", "clip": ["4", 1]}});
        g["14"] = json!({"class_type": "KSamplerAdvanced", "inputs": {"noise_seed": 1, "steps": 25, "cfg": 7, "sampler_name": "euler", "scheduler": "normal",
                         "add_noise": "disable", "start_at_step": 20, "end_at_step": 25, "return_with_leftover_noise": "disable",
                         "model": ["4", 0], "positive": ["13", 0], "negative": ["7", 0], "latent_image": ["3", 0]}});
        g["8"]["inputs"]["samples"] = json!(["14", 0]);
        g["15"] = json!({"class_type": "CLIPTextEncode", "inputs": {"text": "detailed face", "clip": ["4", 1]}});
        g["16"] = json!({"class_type": "FaceDetailer", "inputs": {"image": ["8", 0], "model": ["4", 0], "positive": ["15", 0], "negative": ["7", 0],
                         "seed": 1, "steps": 20, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 0.5}});
        g["9"]["inputs"]["images"] = json!(["16", 0]);
        let a = analyze(&g, "txt2img").unwrap();
        assert_eq!(a.prompt, vec![("6".into(), "text".into()), ("13".into(), "text".into())]);
        assert!(a.warnings.iter().any(|w| w.contains("FaceDetailer")), "{:?}", a.warnings);
        assert_eq!(a.negative, vec![("7".into(), "text".into())]);
        let (n, _) = build(&json!({"prompt": "a dog", "negative": "ugly", "seed": 1}), &wf(g), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!((n["6"]["inputs"]["text"].as_str(), n["13"]["inputs"]["text"].as_str()), (Some("a dog"), Some("a dog")));
        assert_eq!(n["15"]["inputs"]["text"], "detailed face");
    }

    /// 圖生圖有 hires 第二段：重繪幅度只改輸入圖那條路上的第一個取樣節點
    #[test]
    fn img2img_denoise_only_first_sampler() {
        let d = input_dir("hires");
        let mut g = sdxl_img2img();
        g["40"] = json!({"class_type": "LatentUpscaleBy", "inputs": {"samples": ["3", 0], "upscale_method": "nearest-exact", "scale_by": 1.5}});
        g["41"] = json!({"class_type": "KSampler", "inputs": {"seed": 1, "steps": 15, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 0.45,
                         "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["40", 0]}});
        g["8"]["inputs"]["samples"] = json!(["41", 0]);
        let p = json!({"prompt": "x", "seed": 3, "init_image": "base.png", "denoise": 0.7});
        let (n, _) = build(&p, &json!({"name": "h", "graphs": {"img2img": g}}), "img2img", &d, 0).unwrap();
        assert_eq!(n["3"]["inputs"]["denoise"], 0.7);
        assert_eq!(n["41"]["inputs"]["denoise"], 0.45, "hires 第二段保留原值");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// prompt_mode＝append：保留工作流原本的文字，生成頁的接在後面
    #[test]
    fn prompt_mode_append() {
        let mut set_ = wf(sdxl());
        set_["options"] = json!({"prompt_mode": "append"});
        let (n, _) = build(&json!({"prompt": "a dog", "negative": "ugly", "seed": 1}), &set_, "txt2img", Path::new("."), 0).unwrap();
        assert_eq!(n["6"]["inputs"]["text"], "a cat, a dog");
        assert_eq!(n["7"]["inputs"]["text"], "blurry, ugly");
        assert_eq!(append_text("masterpiece, ", "x"), "masterpiece, x");
        assert_eq!(append_text("", "x"), "x");
        // 選項存檔：只收 replace／append
        let dir = std::env::temp_dir().join(format!("anima_image_wf_pm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e = save(&dir, "pm", None, "txt2img", &sdxl(), &Meta::default()).unwrap();
        let id = e["id"].as_str().unwrap().to_string();
        assert!(set_options_all(&dir, &id, None, Some("weird")).is_err());
        set_options_all(&dir, &id, None, Some("append")).unwrap();
        assert_eq!(load(&dir, &id).unwrap()["options"]["prompt_mode"], "append");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Efficiency 的 Eff. Loader：提示詞放在載入節點的 positive／negative 字串欄位；第 1 個輸出不是 CLIP，LoRA 只接模型並提醒
    #[test]
    fn efficient_loader() {
        let g = json!({
            "1": {"class_type": "Efficient Loader", "inputs": {"ckpt_name": "sdxl.safetensors", "vae_name": "Baked VAE", "clip_skip": -2,
                  "positive": "a cat", "negative": "blurry", "empty_latent_width": 1024, "empty_latent_height": 1024, "batch_size": 1}},
            "2": {"class_type": "KSampler", "inputs": {"seed": 1, "steps": 20, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 1,
                  "model": ["1", 0], "positive": ["1", 1], "negative": ["1", 2], "latent_image": ["1", 3]}},
            "3": {"class_type": "VAEDecode", "inputs": {"samples": ["2", 0], "vae": ["1", 4]}},
            "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "x"}}
        });
        let a = analyze(&g, "txt2img").unwrap();
        assert_eq!(a.prompt, vec![("1".into(), "positive".into())]);
        assert_eq!(a.negative, vec![("1".into(), "negative".into())]);
        let m = a.model.clone().unwrap();
        assert!(m.node == "1" && !m.clip, "{m:?}");
        assert!(a.warnings.iter().any(|w| w.contains("LoRA")), "{:?}", a.warnings);
        let (n, _) = build(&json!({"prompt": "a dog", "negative": "ugly", "seed": 1}), &wf(g), "txt2img", Path::new("."), 0).unwrap();
        assert_eq!((n["1"]["inputs"]["positive"].as_str(), n["1"]["inputs"]["negative"].as_str()), (Some("a dog"), Some("ugly")));
    }
}

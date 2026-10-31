//! 工作流匯入（2026-10-21，設計見 design\萬用工作流與匯入匯出.md 第四節）：
//! ComfyUI 老手手上的工作流什麼格式都有，這裡統一轉成 ComfyUI 的「API 格式」（image_wf／video_wf 看得懂的那種：
//! 物件，值有 class_type 和 inputs，連線是 ["節點編號", 輸出序號]）。
//!
//! 看得懂的格式（來源記在 Imported::source）：
//! - "api"：ComfyUI「匯出（API）」存的 JSON（也收 {"prompt": {…}} 這種包一層的），原樣收下
//! - "ui"：一般「儲存」的工作流 JSON（有 nodes、links），照前端 graphToPrompt 的做法轉換，要靠 /object_info 把
//!   widgets_values 對回欄位名稱
//! - "png"／"webp"／"jpeg"：ComfyUI 生成的圖，讀裡面的 "prompt"（API 格式，優先）或 "workflow"（UI 格式，轉換）
//!
//! UI 格式的轉換規則：
//! - 停用（mode 2，Mute）的節點拿掉；下游因此少了必要連線的節點一路連鎖拿掉，並報警告
//! - 跳過（mode 4，Bypass）的節點拿掉，下游改接到它「同型別」的輸入（優先同序號）；找不到就拿掉那條連線並報警告
//! - Reroute、KJNodes 的 SetNode／GetNode 直通；PrimitiveNode（沒在 object_info 裡的 Primitive* 也算）把值直接填進下游的欄位
//! - Note、MarkdownNote、群組、沒接線又不認得的節點（rgthree 的 Label 之類）忽略
//! - 子圖（definitions.subgraphs）、群組節點（workflow>…）沒辦法轉：請使用者在 ComfyUI 用「匯出（API）」
//! - object_info 裡找不到的節點（沒裝的自訂節點）全部列出來，不要匯入後才在生成時炸掉
//! - 節點標題寫進 _meta.title（跟 ComfyUI 匯出（API）一樣；標題沒改過就用節點的顯示名稱），工作流裡的 {{image}} 這類標記就寫在標題上
use crate::imgmeta;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

type R<T> = Result<T, String>;

/// seed 後面那個「生成後」控制欄位（fixed／increment／decrement／randomize）會佔一格 widgets_values
const CONTROL: [&str; 4] = ["fixed", "increment", "decrement", "randomize"];
/// 載入圖片節點最後那個上傳按鈕也會佔一格 widgets_values
const UPLOAD: [&str; 4] = ["image", "upload", "video", "audio"];
/// 前端才有的「值節點」：沒有對應的後端節點時，值直接填進下游欄位（有後端節點的版本照一般節點轉）
const PRIMITIVES: [&str; 6] = [
    "PrimitiveNode",
    "PrimitiveInt",
    "PrimitiveFloat",
    "PrimitiveString",
    "PrimitiveStringMultiline",
    "PrimitiveBoolean",
];

/// 注意：server\api_wf.rs 靠訊息裡有「/object_info」判斷「要帶 object_info 再轉一次」，改字的時候這幾個字要留著
/// 存圖、預覽、存影片的節點（object_info 沒標 output_node 時的備用名單）：自訂節點缺了但接不到這些，就不影響出圖
const OUTPUT_CLASSES: [&str; 8] = ["SaveImage", "PreviewImage", "Image Save", "Image Saver", "SaveVideo", "VHS_VideoCombine", "SaveAnimatedWEBP", "SaveWEBM"];
const NEED_OBJECT_INFO: &str ="讀不到 ComfyUI 的節點清單（/object_info）：一般「儲存」的工作流要靠它才轉得了，請先啟動 ComfyUI 再匯入";
const NO_WORKFLOW: &str = "這張圖裡沒有 ComfyUI 工作流（要用 ComfyUI 自己生成的原圖；被網站或聊天軟體重新壓縮過的圖會掉資料）";
const SUBGRAPH: &str = "這個工作流用了子圖（Subgraph）或群組節點，生圖站沒辦法直接轉換：請在 ComfyUI 打開它，用「匯出（API）」（Export (API)）另存一份，再匯入那個檔案";

/// 轉換結果
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    /// API 格式的節點圖
    pub graph: Value,
    /// 給使用者看的提醒（拿掉了哪些節點、哪些連線斷了…），沒有就是空的
    pub warnings: Vec<String>,
    /// 原本是什麼格式："api"、"ui"、"png"、"webp"、"jpeg"
    pub source: &'static str,
}

/// 把使用者給的檔案（JSON、PNG、WebP）轉成 API 格式。
/// `file_name` 只用在錯誤訊息；`object_info` 是 ComfyUI GET /object_info 的完整回應，只有轉換 UI 格式時才用得到
/// （API 格式、帶 prompt 的圖都不需要，可以傳 Value::Null）。
pub fn to_api(bytes: &[u8], file_name: &str, object_info: &Value) -> R<Imported> {
    if bytes.is_empty() {
        return Err("這個檔案是空的".into());
    }
    if let Some(kind) = imgmeta::image_kind(bytes) {
        let (text, _) = imgmeta::meta_text(bytes).ok_or("這張圖讀不出來：檔案可能壞了")?;
        return from_embedded(text.get("prompt").map(String::as_str), text.get("workflow").map(String::as_str), object_info, kind);
    }
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let v: Value = serde_json::from_slice(body).map_err(|_| unsupported(file_name))?;
    match classify(&v) {
        Kind::Api(g) => Ok(Imported { graph: g.clone(), warnings: vec![], source: "api" }),
        Kind::Ui => {
            let (graph, warnings) = ui_to_api(&v, object_info)?;
            Ok(Imported { graph, warnings, source: "ui" })
        }
        Kind::Unknown => Err(unsupported(file_name)),
    }
}

fn unsupported(file_name: &str) -> String {
    let name = file_name.trim();
    let who = if name.is_empty() { "這個檔案".to_string() } else { format!("「{}」", short(name)) };
    format!("看不懂{who}：可以匯入 ComfyUI「匯出（API）」存的 JSON、一般「儲存」的工作流 JSON，或 ComfyUI 生成、帶著工作流的 PNG／WebP 圖")
}

/// 太長的標題、檔名在提醒裡截短
fn short(s: &str) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(24).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

// ---------------------------------------------------------------- 判斷格式
enum Kind<'a> {
    Api(&'a Value),
    Ui,
    Unknown,
}

fn classify(v: &Value) -> Kind<'_> {
    if is_api(v) {
        return Kind::Api(v);
    }
    if v.get("nodes").is_some_and(Value::is_array) {
        return Kind::Ui;
    }
    match v.get("prompt") {
        Some(p) if is_api(p) => Kind::Api(p),
        _ => Kind::Unknown,
    }
}

/// API 格式：不是空的，而且每個值都有 class_type 和 inputs（跟 video_wf::nodes 的標準一樣）
fn is_api(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|m| !m.is_empty() && m.values().all(|n| n["class_type"].is_string() && n["inputs"].is_object()))
}

// ---------------------------------------------------------------- 圖裡的工作流（PNG、WebP）
/// 圖裡的 prompt（API 格式）優先；沒有或壞掉才用 workflow（UI 格式，轉換）
fn from_embedded(prompt: Option<&str>, workflow: Option<&str>, oi: &Value, source: &'static str) -> R<Imported> {
    let mut warnings = vec![];
    if let Some(p) = prompt {
        match serde_json::from_str::<Value>(p).ok().filter(is_api) {
            Some(graph) => return Ok(Imported { graph, warnings, source }),
            None => warnings.push("圖裡的 prompt 讀不出來，改用 workflow".to_string()),
        }
    }
    let Some(w) = workflow else {
        return Err(NO_WORKFLOW.into());
    };
    let v: Value = serde_json::from_str(w).map_err(|_| "圖裡的 workflow 讀不出來：檔案可能壞了".to_string())?;
    match classify(&v) {
        Kind::Api(g) => Ok(Imported { graph: g.clone(), warnings, source }),
        Kind::Ui => {
            let (graph, more) = ui_to_api(&v, oi)?;
            warnings.extend(more);
            Ok(Imported { graph, warnings, source })
        }
        Kind::Unknown => Err("圖裡的 workflow 看不懂".into()),
    }
}

// ---------------------------------------------------------------- UI 格式 → API 格式
/// 節點定義裡的一個輸入：object_info[類別].input.required／optional 的一項
struct Spec<'a> {
    name: &'a str,
    /// 型別：字串（"INT"、"MODEL"…）或選項清單
    kind: &'a Value,
    opts: Option<&'a Map<String, Value>>,
    required: bool,
}

impl<'a> Spec<'a> {
    fn type_name(&self) -> &'a str {
        self.kind.as_str().unwrap_or("")
    }

    fn opt(&self, k: &str) -> Option<&'a Value> {
        self.opts.and_then(|o| o.get(k))
    }

    /// 在節點上畫成欄位（吃一格 widgets_values）：數字、文字、布林、下拉選單；forceInput 的是連線孔
    fn is_widget(&self) -> bool {
        if self.opt("forceInput").and_then(Value::as_bool) == Some(true) {
            return false;
        }
        self.kind.is_array() || matches!(self.type_name(), "INT" | "FLOAT" | "STRING" | "BOOLEAN" | "COMBO")
    }

    /// 欄位後面跟著一格「生成後怎麼變」的控制值（seed 這類）
    fn has_control(&self) -> bool {
        let flag = self.opt("control_after_generate").is_some_and(|v| v.as_bool().unwrap_or_else(|| v.is_string()));
        flag || (self.type_name() == "INT" && matches!(self.name, "seed" | "noise_seed"))
    }

    /// 有上傳按鈕（Load Image 這類）
    fn wants_upload(&self) -> bool {
        self.opts.is_some_and(|o| o.iter().any(|(k, v)| k.ends_with("_upload") && v.as_bool() == Some(true)))
    }

    /// 一般的連線孔（MODEL、LATENT…）；新版的動態輸入（COMFY_AUTOGROW_V3 這類）不算
    fn is_plain_socket(&self) -> bool {
        let t = self.type_name();
        !self.is_widget() && !t.is_empty() && t != "*" && !t.starts_with("COMFY_")
    }
}

/// 節點的輸入定義，順序照前端：required 在前、optional 在後，各自優先照 input_order
fn specs(info: &Value) -> Vec<Spec<'_>> {
    let mut out = vec![];
    for (group, required) in [("required", true), ("optional", false)] {
        let Some(map) = info["input"][group].as_object() else {
            continue;
        };
        let mut names: Vec<&str> =
            info["input_order"][group].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|k| map.contains_key(*k)).collect();
        for k in map.keys() {
            if !names.contains(&k.as_str()) {
                names.push(k);
            }
        }
        for name in names {
            let arr = &map[name];
            out.push(Spec { name, kind: &arr[0], opts: arr.get(1).and_then(Value::as_object), required });
        }
    }
    out
}

/// 動態欄位的自訂節點（rgthree Power Lora Loader 這類）轉換後設定可能掉了：請使用者改用「匯出（API）」或 PNG
fn dynamic_widgets_warning(label: &str) -> String {
    format!(
        "「{label}」節點的設定可能沒轉過來（它的欄位是動態的，生圖站沒辦法一一對上）。解法：在 ComfyUI 用「匯出（API）」（Export (API)）存一份再匯入，或用 ComfyUI 生成的 PNG 匯入"
    )
}

/// 把 widgets_values 對回欄位名稱。陣列照 object_info 的欄位順序一格一格吃；物件（某些自訂節點）照名稱拿。
/// custom：不是 ComfyUI 內建的節點（object_info 的 python_module 在 custom_nodes 底下）：欄位值對不上時警告得比較重
fn widget_values(specs: &[Spec], wv: &Value, label: &str, custom: bool, warnings: &mut Vec<String>) -> Map<String, Value> {
    let mut out = Map::new();
    match wv {
        Value::Array(vals) => {
            let mut i = 0;
            let mut take = |s: &Spec, i: &mut usize| {
                let v = vals.get(*i);
                if v.is_some() {
                    *i += 1;
                }
                if let Some(v) = v.filter(|v| !v.is_null()) {
                    out.insert(s.name.to_string(), v.clone());
                }
                if v.is_some() && s.has_control() && vals.get(*i).and_then(Value::as_str).is_some_and(|c| CONTROL.contains(&c)) {
                    *i += 1;
                }
            };
            for s in specs.iter().filter(|s| s.required && s.is_widget()) {
                take(s, &mut i);
            }
            // 前端把上傳按鈕補在 required 的最後面
            if specs.iter().any(|s| s.required && s.wants_upload()) && vals.get(i).and_then(Value::as_str).is_some_and(|u| UPLOAD.contains(&u)) {
                i += 1;
            }
            for s in specs.iter().filter(|s| !s.required && s.is_widget()) {
                take(s, &mut i);
            }
            if i < vals.len() {
                // 內建節點的欄位值裡不會有物件：有的話一定是動態欄位（Power Lora Loader 的每一列 LoRA 是一個物件）
                if custom || vals[i..].iter().any(Value::is_object) {
                    warnings.push(dynamic_widgets_warning(label));
                } else {
                    warnings.push(format!(
                        "節點「{label}」的欄位值比 ComfyUI 的節點定義多了 {} 個（可能是節點版本不同），請確認它的設定",
                        vals.len() - i
                    ));
                }
            }
        }
        Value::Object(obj) => {
            for s in specs.iter().filter(|s| s.is_widget()) {
                if let Some(v) = obj.get(s.name).filter(|v| !v.is_null()) {
                    out.insert(s.name.to_string(), v.clone());
                }
            }
            // 自訂節點的物件裡有對不上任何欄位的值：那些設定掉了
            if custom && obj.iter().any(|(k, v)| !v.is_null() && !specs.iter().any(|s| s.is_widget() && s.name == k)) {
                warnings.push(dynamic_widgets_warning(label));
            }
        }
        _ => {}
    }
    // 工作流裡沒存到的必要欄位用預設值（前端的欄位本來就有預設）
    for s in specs.iter().filter(|s| s.required && s.is_widget()) {
        if let (false, Some(d)) = (out.contains_key(s.name), s.opt("default")) {
            out.insert(s.name.to_string(), d.clone());
        }
    }
    out
}

/// 一條連線追到頭的結果
enum Src {
    /// 真正的節點輸出
    Node(String, usize),
    /// PrimitiveNode 這類：值直接填進下游
    Value(Value),
    /// 斷了（接到停用、跳過、不存在的節點），附原因（寫在提醒裡）
    Gone(String),
}

struct Ui<'a> {
    oi: &'a Map<String, Value>,
    nodes: HashMap<String, &'a Value>,
    /// 連線編號 → (來源節點, 來源輸出序號)
    links: HashMap<i64, (String, usize)>,
}

fn id_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn link_id(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// links 裡一條連線：[編號, 來源節點, 來源序號, 目標節點, 目標序號, 型別]，新版也有物件寫法
fn parse_links(v: Option<&Value>) -> HashMap<i64, (String, usize)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let (id, origin, slot) = match l {
                Value::Array(a) => (link_id(a.first()?), id_str(a.get(1)?)?, a.get(2)?.as_u64()?),
                _ => (link_id(&l["id"]), id_str(&l["origin_id"])?, l["origin_slot"].as_u64()?),
            };
            Some((id?, (origin, slot as usize)))
        })
        .collect()
}

fn mode(n: &Value) -> i64 {
    n["mode"].as_i64().unwrap_or(0)
}

fn node_type(n: &Value) -> &str {
    n["type"].as_str().unwrap_or("")
}

/// 給人看的名稱：使用者改過的標題，沒有就用類別名稱
fn label(n: &Value) -> &str {
    Some(n["title"].as_str().unwrap_or("")).filter(|t| !t.is_empty()).unwrap_or_else(|| node_type(n))
}

fn same_type(a: &str, b: &str) -> bool {
    a == b || a == "*" || b == "*"
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
}

impl<'a> Ui<'a> {
    /// 前端才有、不會送給 ComfyUI 的節點（object_info 裡沒有它們）
    fn is_virtual(&self, ty: &str) -> bool {
        if self.oi.contains_key(ty) {
            return false;
        }
        matches!(ty, "Note" | "MarkdownNote" | "Reroute" | "SetNode" | "GetNode") || ty.starts_with("Reroute (") || PRIMITIVES.contains(&ty)
    }

    /// 節點第 idx 個輸入接到哪裡（追到頭）；那個輸入沒接線就是 None
    fn upstream(&self, n: &Value, idx: usize, depth: usize) -> Option<Src> {
        let lid = link_id(&n["inputs"].get(idx)?["link"])?;
        let (origin, slot) = self.links.get(&lid)?;
        Some(self.resolve(origin, *slot, depth + 1))
    }

    /// 某節點的某個輸出實際是誰：略過停用、跳過、直通、值節點
    fn resolve(&self, id: &str, slot: usize, depth: usize) -> Src {
        if depth > 100 {
            return Src::Gone("連線繞成一圈".into());
        }
        let Some(&n) = self.nodes.get(id) else {
            return Src::Gone("連線的來源節點不存在".into());
        };
        let name = short(label(n));
        match mode(n) {
            2 => return Src::Gone(format!("接到已停用（Mute）的節點「{name}」")),
            4 => return self.bypass(n, slot, depth),
            _ => {}
        }
        let ty = node_type(n);
        if !self.is_virtual(ty) {
            return Src::Node(id.to_string(), slot);
        }
        match ty {
            "Note" | "MarkdownNote" => Src::Gone(format!("接到沒有輸出的節點「{name}」")),
            "GetNode" => {
                // KJNodes：用名稱找到配對的 SetNode，接它的輸入
                let key = n["widgets_values"].get(0).and_then(Value::as_str);
                let setter = self
                    .nodes
                    .values()
                    .find(|s| key.is_some() && node_type(s) == "SetNode" && mode(s) != 2 && s["widgets_values"].get(0).and_then(Value::as_str) == key);
                match setter {
                    Some(s) => self.upstream(s, 0, depth).unwrap_or_else(|| Src::Gone(format!("接到沒有輸入的節點「{name}」"))),
                    None => Src::Gone(format!("找不到跟「{name}」配對的 Set 節點")),
                }
            }
            "Reroute" | "SetNode" => self.upstream(n, 0, depth).unwrap_or_else(|| Src::Gone(format!("接到沒有輸入的節點「{name}」"))),
            _ if ty.starts_with("Reroute (") => self.upstream(n, 0, depth).unwrap_or_else(|| Src::Gone(format!("接到沒有輸入的節點「{name}」"))),
            _ => match n["widgets_values"].get(0) {
                Some(v) if !v.is_null() => Src::Value(v.clone()),
                _ => Src::Gone(format!("值節點「{name}」沒有值")),
            },
        }
    }

    /// 跳過（Bypass）：下游改接到它同型別的輸入，優先同序號，再來是第一個同型別的
    fn bypass(&self, n: &Value, slot: usize, depth: usize) -> Src {
        let name = short(label(n));
        let out_ty = n["outputs"].get(slot).and_then(|o| o["type"].as_str()).unwrap_or("*");
        let inputs = n["inputs"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        // 帶 widget 的是欄位轉成的連線孔，不是資料的入口
        let fits = |i: &Value| i.get("widget").is_none() && same_type(i["type"].as_str().unwrap_or("*"), out_ty);
        let pick = inputs.iter().enumerate().find(|&(k, i)| k == slot && fits(i)).or_else(|| inputs.iter().enumerate().find(|&(_, i)| fits(i)));
        match pick {
            Some((k, _)) => self
                .upstream(n, k, depth)
                .unwrap_or_else(|| Src::Gone(format!("接到被跳過（Bypass）的節點「{name}」，但它對應的輸入沒有接線"))),
            None => Src::Gone(format!("接到被跳過（Bypass）的節點「{name}」，但它沒有跟輸出同型別（{out_ty}）的輸入可以接")),
        }
    }

    /// 存圖、預覽、存影片這類「終點」節點（沒被停用或跳過）：object_info 標了 output_node 的，或 class 是常見的存圖節點
    fn is_output(&self, id: &str) -> bool {
        let Some(&n) = self.nodes.get(id) else {
            return false;
        };
        if !matches!(mode(n), 0 | 1 | 3) {
            return false;
        }
        let ty = node_type(n);
        match self.oi.get(ty) {
            Some(info) => info["output_node"].as_bool().unwrap_or(false) || OUTPUT_CLASSES.contains(&ty),
            None => false,
        }
    }

    /// 這個節點的輸出有沒有（直接或繞幾個節點之後）通到終點節點。被停用的節點當作斷開
    fn reaches_output(&self, start: &str) -> bool {
        let mut fwd: HashMap<&str, Vec<&str>> = HashMap::new();
        for (id, n) in &self.nodes {
            if mode(n) == 2 {
                continue;
            }
            for inp in n["inputs"].as_array().into_iter().flatten() {
                if let Some((origin, _)) = link_id(&inp["link"]).and_then(|l| self.links.get(&l)) {
                    fwd.entry(origin.as_str()).or_default().push(id.as_str());
                }
            }
        }
        let (mut seen, mut stack) = (HashSet::new(), vec![start]);
        while let Some(id) = stack.pop() {
            for &next in fwd.get(id).into_iter().flatten() {
                if self.is_output(next) {
                    return true;
                }
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        false
    }

    /// 一個節點的 inputs：欄位值（widgets_values 對回名稱）加上連線；連線蓋過同名的欄位值。
    /// 第二個回傳值是斷掉又沒有欄位值可退回的輸入（名稱, 原因）
    fn node_inputs(&self, n: &Value, info: &Value, warnings: &mut Vec<String>) -> (Map<String, Value>, Vec<(String, String)>) {
        let specs = specs(info);
        let custom = info["python_module"].as_str().is_some_and(|m| m.starts_with("custom_nodes"));
        let widgets = widget_values(&specs, &n["widgets_values"], &short(label(n)), custom, warnings);
        let mut inputs = Map::new();
        for s in specs.iter().filter(|s| s.is_widget()) {
            if let Some(v) = widgets.get(s.name) {
                inputs.insert(s.name.to_string(), v.clone());
            }
        }
        let mut gone = vec![];
        for inp in n["inputs"].as_array().into_iter().flatten() {
            let (Some(name), Some(lid)) = (inp["name"].as_str(), link_id(&inp["link"])) else {
                continue;
            };
            let src = match self.links.get(&lid) {
                Some((origin, slot)) => self.resolve(origin, *slot, 0),
                None => Src::Gone("連線的來源節點不存在".into()),
            };
            match src {
                Src::Node(id, slot) => {
                    inputs.insert(name.to_string(), json!([id, slot]));
                }
                Src::Value(v) => {
                    inputs.insert(name.to_string(), v);
                }
                // 欄位轉成連線孔又接到斷掉的地方：退回欄位本來的值（前端也是這樣）
                Src::Gone(why) => {
                    if !inputs.contains_key(name) {
                        gone.push((name.to_string(), why));
                    }
                }
            }
        }
        (inputs, gone)
    }
}

fn is_link(v: &Value) -> bool {
    matches!(v.as_array().map(Vec::as_slice), Some([Value::String(_), n]) if n.is_number())
}

fn ui_to_api(wf: &Value, oi: &Value) -> R<(Value, Vec<String>)> {
    let Some(oi) = oi.as_object().filter(|m| !m.is_empty()) else {
        return Err(NEED_OBJECT_INFO.into());
    };
    let mut ui = Ui { oi, nodes: HashMap::new(), links: parse_links(wf.get("links")) };
    let mut order = vec![];
    for n in wf["nodes"].as_array().into_iter().flatten() {
        let Some(id) = id_str(&n["id"]) else {
            continue;
        };
        if ui.nodes.insert(id.clone(), n).is_none() {
            order.push(id);
        }
    }

    // 子圖、群組節點：節點裡的東西不在 nodes 裡，沒辦法轉
    let subgraphs: HashSet<&str> = wf["definitions"]["subgraphs"].as_array().into_iter().flatten().filter_map(|s| s["id"].as_str()).collect();
    for id in &order {
        let Some(&n) = ui.nodes.get(id) else {
            continue;
        };
        let ty = node_type(n);
        if mode(n) != 2 && (subgraphs.contains(ty) || ty.starts_with("workflow>") || is_uuid(ty)) {
            return Err(SUBGRAPH.into());
        }
    }

    // 哪些節點要送給 ComfyUI；缺的自訂節點一次列完
    let mut warnings = vec![];
    let (mut muted, mut bypassed, mut emit, mut missing, mut dropped) = (vec![], vec![], vec![], vec![], vec![]);
    for id in &order {
        let Some(&n) = ui.nodes.get(id) else {
            continue;
        };
        let ty = node_type(n);
        if ty.is_empty() {
            continue;
        }
        if ui.is_virtual(ty) {
            continue;
        }
        match mode(n) {
            2 => {
                muted.push(short(label(n)));
                continue;
            }
            4 => {
                bypassed.push(short(label(n)));
                continue;
            }
            _ => {}
        }
        if ui.oi.contains_key(ty) {
            emit.push(id.clone());
            continue;
        }
        // 不認得、又完全沒接線：多半是前端的裝飾節點（Label、Fast Groups Muter…），忽略就好
        let wired = n["inputs"].as_array().into_iter().flatten().any(|i| !i["link"].is_null())
            || n["outputs"].as_array().into_iter().flatten().any(|o| o["links"].as_array().is_some_and(|l| !l.is_empty()));
        if wired {
            // 沒裝的自訂節點：下游通得到存圖節點才擋；只是旁支（預覽、偵測用的）就拿掉，下面的連鎖清理會處理接到它的輸入
            if ui.reaches_output(id) {
                if !missing.contains(&ty) {
                    missing.push(ty);
                }
            } else {
                let name = short(label(n));
                if !dropped.contains(&name) {
                    dropped.push(name);
                }
            }
        } else {
            warnings.push(format!("略過了沒有接線、也不認得的節點「{}」", short(label(n))));
        }
    }
    if !missing.is_empty() {
        return Err(format!("缺少自訂節點：{}（在 ComfyUI 裝好、重開 ComfyUI 再匯入）", missing.join("、")));
    }
    if !dropped.is_empty() {
        warnings.insert(
            0,
            format!("略過了 {} 個沒安裝、又沒有接到存圖或預覽的自訂節點（不影響出圖）：{}", dropped.len(), dropped.join("、")),
        );
    }
    if !muted.is_empty() {
        warnings.insert(0, format!("略過了 {} 個停用（Mute）的節點：{}", muted.len(), muted.join("、")));
    }
    if !bypassed.is_empty() {
        warnings.insert(0, format!("略過了 {} 個跳過（Bypass）的節點（下游改接到它的上游）：{}", bypassed.len(), bypassed.join("、")));
    }

    // 轉節點
    let mut graph: Map<String, Value> = Map::new();
    let mut broken: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for id in &emit {
        let Some(&n) = ui.nodes.get(id) else {
            continue;
        };
        let ty = node_type(n);
        let Some(info) = ui.oi.get(ty) else {
            continue;
        };
        let (inputs, gone) = ui.node_inputs(n, info, &mut warnings);
        let title = Some(label_title(n)).filter(|t| !t.is_empty()).or_else(|| info["display_name"].as_str().filter(|d| !d.is_empty())).unwrap_or(ty);
        graph.insert(id.clone(), json!({"inputs": inputs, "class_type": ty, "_meta": {"title": title}}));
        if !gone.is_empty() {
            broken.insert(id.clone(), gone);
        }
    }

    // 少了必要連線的節點拿掉，連鎖往下游；選填的連線只拿掉那一條
    let mut alive: HashSet<String> = graph.keys().cloned().collect();
    let required = |ty: &str, name: &str| ui.oi.get(ty).is_some_and(|info| specs(info).iter().any(|s| s.required && s.name == name));
    loop {
        let mut changed = false;
        for id in &emit {
            if !alive.contains(id) {
                continue;
            }
            let Some(inputs) = graph.get_mut(id).and_then(|n| n.get_mut("inputs")).and_then(Value::as_object_mut) else {
                continue;
            };
            let dead: Vec<String> =
                inputs.iter().filter(|(_, v)| is_link(v)).filter(|(_, v)| !alive.contains(v[0].as_str().unwrap_or(""))).map(|(k, _)| k.clone()).collect();
            for k in dead {
                let up = inputs.remove(&k).and_then(|v| v[0].as_str().map(str::to_string)).unwrap_or_default();
                let up_name = ui.nodes.get(&up).map_or(up.clone(), |u| short(label(u)));
                broken.entry(id.clone()).or_default().push((k, format!("上游的節點「{up_name}」已被移除")));
            }
            let ty = ui.nodes.get(id).map_or("", |n| node_type(n));
            if broken.get(id).is_some_and(|b| b.iter().any(|(k, _)| required(ty, k))) {
                alive.remove(id);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for id in &emit {
        let Some(&n) = ui.nodes.get(id) else {
            continue;
        };
        let (ty, name) = (node_type(n), short(label(n)));
        for (k, why) in broken.get(id).into_iter().flatten() {
            if !alive.contains(id) && required(ty, k) {
                warnings.push(format!("節點「{name}」的「{k}」{why}，缺了它跑不起來，節點已一起移除"));
            } else if alive.contains(id) {
                warnings.push(format!("節點「{name}」的選填輸入「{k}」{why}，這條連線已移除"));
            }
        }
        // 原本的工作流就沒接好的必要輸入：提醒一聲（生成時 ComfyUI 會報錯）
        if let (true, Some(inputs)) = (alive.contains(id), graph.get(id).and_then(|g| g["inputs"].as_object())) {
            for s in ui.oi.get(ty).map(specs).unwrap_or_default().iter().filter(|s| s.required && s.is_plain_socket()) {
                if !inputs.contains_key(s.name) {
                    warnings.push(format!("節點「{name}」的必要輸入「{}」沒有接線，生成時 ComfyUI 會報錯", s.name));
                }
            }
        }
    }

    // 編號照數字順序放
    graph.retain(|k, _| alive.contains(k));
    let mut ids: Vec<String> = graph.keys().cloned().collect();
    ids.sort_by_key(|k| (k.parse::<u64>().map_or((1u8, 0u64), |n| (0, n)), k.clone()));
    let sorted: Map<String, Value> = ids.into_iter().filter_map(|k| graph.remove(&k).map(|v| (k, v))).collect();
    if sorted.is_empty() {
        return Err("轉換後一個節點都不剩：工作流裡的節點都被停用或跳過了".into());
    }
    let mut seen = HashSet::new();
    warnings.retain(|w| seen.insert(w.clone()));
    Ok((Value::Object(sorted), warnings))
}

/// 使用者自己改的節點標題；沒改就是空字串
fn label_title(n: &Value) -> &str {
    n["title"].as_str().unwrap_or("")
}

// ---------------------------------------------------------------- 匯入失敗時：讓使用者自己指定節點
// 匯入的分析（image_wf／video_wf 的 analyze）找不到提示詞、輸入圖這些地方時，api_wf 把節點圖裡「可以選的節點」（candidates）
// 連同缺的是哪一種（need）一起回給網頁；使用者選好，網頁帶著 marks 再送一次，這裡把標記寫進節點圖，後面的分析就照標記走。

/// 標記只能是這幾種：提示詞和負面詞寫在文字欄位裡，其他寫在節點標題上
const TEXT_MARKS: [&str; 2] = ["{{prompt}}", "{{negative}}"];
const TITLE_MARKS: [&str; 4] = ["{{image}}", "{{mask}}", "{{model}}", "{{sampler}}"];
/// 放得了 {{image}}、{{mask}}、{{refN}} 的節點
const LOADERS: [&str; 2] = ["LoadImage", "LoadImageMask"];
/// 「這個節點放的是主模型檔名」的欄位（跟 image_wf 一樣）
const MODEL_KEYS: [&str; 4] = ["ckpt_name", "unet_name", "model_name", "model_path"];
const SAMPLER_FIELDS: [&str; 6] = ["steps", "cfg", "sampler_name", "scheduler", "denoise", "guidance"];
/// 不是提示詞的字串欄位（模型檔名、取樣器名稱、存檔前綴…）：候選清單裡不列
const NOT_TEXT_KEYS: [&str; 12] = [
    "scheduler", "filename_prefix", "image", "weight_dtype", "type", "device", "mode", "method", "format", "control_after_generate", "upload", "channel",
];
const MODEL_EXTS: [&str; 9] = [".safetensors", ".ckpt", ".pt", ".pth", ".gguf", ".bin", ".sft", ".onnx", ".pkl"];
/// 候選清單最多列幾個有字串欄位的節點（大工作流裡的雜項節點很多）
const MAX_TEXT_CANDIDATES: usize = 60;

/// {{ref3}} 這種
fn is_ref_mark(m: &str) -> bool {
    m.strip_prefix("{{ref").and_then(|r| r.strip_suffix("}}")).is_some_and(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn gnode<'a>(g: &'a Value, id: &str) -> Option<&'a Value> {
    g.get(id).filter(|n| n.is_object())
}

fn gclass(n: &Value) -> &str {
    n["class_type"].as_str().unwrap_or("")
}

fn gtitle(n: &Value) -> &str {
    n["_meta"]["title"].as_str().unwrap_or("")
}

/// 給人看的節點名稱：標題，沒有就用 class_type
fn glabel(n: &Value) -> String {
    short(Some(gtitle(n)).filter(|t| !t.is_empty()).unwrap_or_else(|| gclass(n)))
}

/// 標題後面接上標記（原本的標題保留；已經有就不重複）
fn add_title_mark(n: &mut Value, mark: &str) {
    if !n["_meta"].is_object() {
        n["_meta"] = json!({});
    }
    let old = n["_meta"]["title"].as_str().unwrap_or("").to_string();
    if old.contains(mark) {
        return;
    }
    n["_meta"]["title"] = json!(if old.is_empty() { mark.to_string() } else { format!("{old} {mark}") });
}

/// 把標題裡的標記拿掉（同一個標記只能放在一個節點上，使用者重新指定時舊的要清掉）
fn strip_title_mark(n: &mut Value, mark: &str) {
    let old = gtitle(n).to_string();
    if !old.contains(mark) {
        return;
    }
    let new = old.replace(mark, "");
    n["_meta"]["title"] = json!(new.split_whitespace().collect::<Vec<_>>().join(" "));
}

/// 這個欄位能不能當提示詞的文字框：字串（不是連線）、不是模型檔名之類
fn is_text_field(k: &str, v: &Value) -> bool {
    let Some(s) = v.as_str() else {
        return false;
    };
    let low = s.to_lowercase();
    !(NOT_TEXT_KEYS.contains(&k) || k.ends_with("_name") || MODEL_EXTS.iter().any(|e| low.ends_with(e)))
}

fn text_fields(n: &Value) -> Vec<&str> {
    n["inputs"].as_object().into_iter().flatten().filter(|(k, v)| is_text_field(k, v)).map(|(k, _)| k.as_str()).collect()
}

/// 把使用者指定的標記寫進節點圖。marks 是物件：鍵「節點編號」或「節點編號.欄位名」，值是一個標記或標記的陣列。
/// - 節點標題類（{{image}}、{{mask}}、{{refN}}、{{model}}、{{sampler}}）：鍵寫節點編號，標記加在標題後面；
///   {{image}}、{{mask}}、{{model}}、{{refN}} 同一個只能放一個節點，其他節點上的同一個標記會拿掉
/// - 文字類（{{prompt}}、{{negative}}）：鍵寫「節點編號.欄位名」，那個欄位的值整個換成標記；只寫節點編號時，節點只有一個文字欄位就用它
///
/// 回傳寫了什麼（一行一項，給使用者看）。標記不認得、節點或欄位不存在就回錯誤原因（什麼都沒改）
pub fn apply_marks(graph: &mut Value, marks: &Value) -> R<Vec<String>> {
    let Some(marks) = marks.as_object().filter(|m| !m.is_empty()) else {
        return Ok(vec![]);
    };
    if !graph.is_object() {
        return Err("看不懂這個工作流，沒辦法標記".into());
    }
    // 先全部檢查過，免得改一半
    let mut plan: Vec<(String, Option<String>, String)> = vec![];
    for (key, v) in marks {
        let (id, field) = match key.split_once('.') {
            Some((i, f)) => (i.trim(), Some(f.trim().to_string()).filter(|f| !f.is_empty())),
            None => (key.trim(), None),
        };
        let list: Vec<&str> = match v {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => return Err(format!("節點 #{id} 的標記格式不對")),
        };
        let Some(n) = gnode(graph, id) else {
            return Err(format!("工作流裡找不到節點 #{id}"));
        };
        for m in list.into_iter().map(str::trim) {
            let is_title = TITLE_MARKS.contains(&m) || is_ref_mark(m);
            if !is_title && !TEXT_MARKS.contains(&m) {
                return Err(format!("不認得的標記 {m}：只能是 {{{{prompt}}}}、{{{{negative}}}}、{{{{image}}}}、{{{{mask}}}}、{{{{refN}}}}、{{{{model}}}}、{{{{sampler}}}}"));
            }
            if is_title {
                if (m == "{{image}}" || m == "{{mask}}" || is_ref_mark(m)) && !LOADERS.contains(&gclass(n)) {
                    return Err(format!("節點 #{id}「{}」不是 Load Image，不能標成 {m}", glabel(n)));
                }
                plan.push((id.to_string(), None, m.to_string()));
                continue;
            }
            // 文字欄位
            let field = match field.clone() {
                Some(f) => f,
                None => {
                    let fields = text_fields(n);
                    match fields.as_slice() {
                        [one] => (*one).to_string(),
                        [] => return Err(format!("節點 #{id}「{}」沒有可以放提示詞的文字欄位", glabel(n))),
                        many => match ["text", "prompt"].into_iter().find(|k| many.contains(k)) {
                            Some(k) => k.to_string(),
                            None => return Err(format!("節點 #{id}「{}」有好幾個文字欄位（{}）：要指定是哪個，寫成「{id}.欄位名」", glabel(n), many.join("、"))),
                        },
                    }
                }
            };
            if !n["inputs"].get(&field).is_some_and(Value::is_string) {
                return Err(format!("節點 #{id}「{}」的「{field}」不是文字欄位（可能接了別的節點），沒辦法放 {m}", glabel(n)));
            }
            plan.push((id.to_string(), Some(field), m.to_string()));
        }
    }
    let mut notes = vec![];
    for (id, field, mark) in plan {
        // 名字在改之前取（標題標記會接在標題後面）
        let name = glabel(&graph[&id]);
        match field {
            Some(f) => {
                graph[&id]["inputs"][&f] = json!(mark);
                notes.push(format!("節點 #{id}「{name}」的「{f}」當成 {mark}"));
            }
            None => {
                // 單一位置的標記：先把別的節點上同一個標記拿掉
                if mark != "{{sampler}}" {
                    let others: Vec<String> = graph.as_object().into_iter().flatten().filter(|(k, _)| **k != id).map(|(k, _)| k.clone()).collect();
                    for o in others {
                        strip_title_mark(&mut graph[&o], &mark);
                    }
                }
                add_title_mark(&mut graph[&id], &mark);
                notes.push(format!("節點 #{id}「{name}」當成 {mark}"));
            }
        }
    }
    Ok(notes)
}

/// 分析失敗的原因 → 缺的是哪一種（給網頁決定要問使用者什麼）：prompt（正面提示詞）｜image（輸入圖）｜mask（遮罩）｜ref1（參考圖）｜model（主模型）｜sampler（取樣節點）。
/// 靠錯誤訊息裡的關鍵字認（image_wf／video_wf 的訊息都會講到要標哪個標記）；認不出來是 None
pub fn need_of(err: &str) -> Option<&'static str> {
    // 順序有差：遮罩、參考圖的訊息裡也會講到 Load Image
    [
        ("{{ref", "ref1"),
        ("多圖參考", "ref1"),
        ("遮罩", "mask"),
        ("{{mask}}", "mask"),
        ("Load Image", "image"),
        ("輸入圖", "image"),
        ("首格圖", "image"),
        ("{{model}}", "model"),
        ("{{sampler}}", "sampler"),
        ("提示詞", "prompt"),
    ]
    .into_iter()
    .find(|(k, _)| err.contains(k))
    .map(|(_, n)| n)
}

/// 節點圖裡使用者可以選的節點：{"texts": [{node, class, title, field, text}], "images": [{node, class, title, image}],
/// "models": [{node, class, title, field, value}], "samplers": [{node, class, title}]}。節點編號照數字順序
pub fn candidates(graph: &Value) -> Value {
    let Some(m) = graph.as_object() else {
        return json!({"texts": [], "images": [], "models": [], "samplers": []});
    };
    let mut ids: Vec<&String> = m.keys().collect();
    ids.sort_by_key(|k| (k.parse::<u64>().map_or((1u8, 0u64), |n| (0, n)), (*k).clone()));
    let (mut texts, mut images, mut models, mut samplers) = (vec![], vec![], vec![], vec![]);
    let base = |id: &str, n: &Value| json!({"node": id, "class": gclass(n), "title": gtitle(n)});
    for id in ids {
        let n = &m[id];
        if !n["inputs"].is_object() {
            continue;
        }
        let cls = gclass(n);
        if LOADERS.contains(&cls) {
            let mut e = base(id, n);
            e["image"] = n["inputs"]["image"].clone();
            images.push(e);
        }
        if let Some(k) = MODEL_KEYS.iter().find(|k| n["inputs"][**k].is_string()) {
            let mut e = base(id, n);
            e["field"] = json!(k);
            e["value"] = n["inputs"][*k].clone();
            models.push(e);
        }
        if SAMPLER_FIELDS.iter().any(|k| !n["inputs"][*k].is_null() && !is_link(&n["inputs"][*k])) && (n["inputs"]["steps"].is_number() || cls.contains("Sampler")) {
            samplers.push(base(id, n));
        }
        for k in text_fields(n) {
            // 有字的才列；空的只列叫 text／prompt 的（空的負面文字框也是常見的選擇）
            let s = n["inputs"][k].as_str().unwrap_or("");
            if s.is_empty() && !matches!(k, "text" | "prompt") {
                continue;
            }
            let mut e = base(id, n);
            e["field"] = json!(k);
            e["text"] = json!(s.chars().take(60).collect::<String>());
            texts.push(e);
        }
    }
    // 文字編碼節點排前面（真正的提示詞多半在那裡），其餘照編號
    texts.sort_by_key(|e| !e["class"].as_str().is_some_and(|c| c.contains("TextEncode")));
    texts.truncate(MAX_TEXT_CANDIDATES);
    json!({"texts": texts, "images": images, "models": models, "samplers": samplers})
}

/// 沒有生圖站的 {{mode:…}} 標記時，看節點圖猜是哪個模式 → (模式, 理由)。猜不出來是 None（照文生圖）
pub fn guess_mode(graph: &Value) -> Option<(&'static str, String)> {
    let m = graph.as_object()?;
    let cls = |n: &Value| n["class_type"].as_str().unwrap_or("").to_string();
    if let Some(c) = m.values().map(cls).find(|c| matches!(c.as_str(), "VAEEncodeForInpaint" | "InpaintModelConditioning" | "SetLatentNoiseMask")) {
        return Some(("inpaint", format!("有「{c}」這種局部重繪用的節點")));
    }
    let has_sampler = m.values().any(|n| n["inputs"]["steps"].is_number() || matches!(n["class_type"].as_str(), Some("SamplerCustomAdvanced" | "SamplerCustom")));
    if !has_sampler && m.values().any(|n| n["class_type"] == "ImageUpscaleWithModel") {
        return Some(("upscale", "有「放大模型」的節點、沒有取樣器".to_string()));
    }
    // Load Image 接到 VAE Encode（中間可以隔幾個縮放、裁切的節點）。只沿著圖片那條路往上找：不走 vae 這種欄位，
    // 碰到 VAE Decode 就停（那是生成出來的圖，例如高解析修復；再往上會摸到 ControlNet 的 Load Image）
    for (id, n) in m {
        if !matches!(n["class_type"].as_str(), Some("VAEEncode" | "VAEEncodeTiled")) {
            continue;
        }
        let (mut seen, mut stack) = (HashSet::from([id.as_str()]), vec![id.as_str()]);
        while let Some(cur) = stack.pop() {
            for (k, v) in m.get(cur).and_then(|n| n["inputs"].as_object()).into_iter().flatten() {
                let Some(from) = v.as_array().filter(|_| is_link(v) && k != "vae").and_then(|a| a[0].as_str()) else {
                    continue;
                };
                let fc = m.get(from).map_or("", |f| f["class_type"].as_str().unwrap_or(""));
                if fc == "LoadImage" {
                    return Some(("img2img", "有 Load Image 接到 VAE Encode".to_string()));
                }
                if !fc.starts_with("VAEDecode") && seen.insert(from) {
                    stack.push(from);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const OI: &str = include_str!("../tests/fixtures/object_info_trimmed.json");
    const DEFAULT_UI: &str = include_str!("../tests/fixtures/default_txt2img_ui.json");
    const DEFAULT_API: &str = include_str!("../tests/fixtures/default_txt2img_api.json");

    fn oi() -> Value {
        serde_json::from_str(OI).expect("object_info 範本")
    }

    fn json_of(s: &str) -> Value {
        serde_json::from_str(s).expect("範本 JSON")
    }

    fn convert(wf: &Value) -> R<Imported> {
        to_api(wf.to_string().as_bytes(), "wf.json", &oi())
    }

    /// 低解析度的全灰 PNG，附上文字區塊
    fn png_with(texts: &[(&str, &str)]) -> Vec<u8> {
        let mut out = vec![];
        {
            let mut enc = png::Encoder::new(&mut out, 1, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            for (k, v) in texts {
                enc.add_text_chunk(k.to_string(), v.to_string()).expect("文字區塊");
            }
            let mut w = enc.write_header().expect("標頭");
            w.write_image_data(&[0]).expect("像素");
            w.finish().expect("寫完");
        }
        out
    }

    /// 組一個帶 EXIF 的 WebP 外殼（沒有真的影像資料，讀 metadata 用不到）
    fn webp_with(entries: &[(u16, String)], le: bool, exif_prefix: bool) -> Vec<u8> {
        let u16b = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let u32b = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut tiff = if le { b"II".to_vec() } else { b"MM".to_vec() };
        tiff.extend(u16b(42));
        tiff.extend(u32b(8));
        tiff.extend(u16b(entries.len() as u16));
        let mut data_off = 8 + 2 + entries.len() as u32 * 12 + 4;
        let mut data = vec![];
        for (tag, text) in entries {
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(0);
            tiff.extend(u16b(*tag));
            tiff.extend(u16b(2));
            tiff.extend(u32b(bytes.len() as u32));
            tiff.extend(u32b(data_off));
            data_off += bytes.len() as u32;
            data.extend(bytes);
        }
        tiff.extend(u32b(0));
        tiff.extend(data);
        let mut exif = if exif_prefix { b"Exif\0\0".to_vec() } else { vec![] };
        exif.extend(tiff);
        let mut body = b"WEBP".to_vec();
        body.extend(b"VP8X");
        body.extend(10u32.to_le_bytes());
        body.extend([0u8; 10]);
        body.extend(b"EXIF");
        body.extend((exif.len() as u32).to_le_bytes());
        let pad = exif.len() % 2 == 1;
        body.extend(exif);
        if pad {
            body.push(0);
        }
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    // ---- 小積木：用程式組 UI 格式的工作流
    fn node(id: u64, ty: &str, mode: i64, inputs: Value, outputs: Value, wv: Value) -> Value {
        json!({"id": id, "type": ty, "mode": mode, "inputs": inputs, "outputs": outputs, "widgets_values": wv})
    }

    /// UNET → LoRA（mode 由參數決定）→ KSampler → VAEDecode → SaveImage 的最小文生圖
    fn lora_chain(lora_mode: i64) -> Value {
        json!({
            "nodes": [
                node(1, "UNETLoader", 0, json!([]), json!([{"name": "MODEL", "type": "MODEL", "links": [1]}]), json!(["a.safetensors", "default"])),
                node(2, "LoraLoaderModelOnly", lora_mode, json!([{"name": "model", "type": "MODEL", "link": 1}]),
                     json!([{"name": "MODEL", "type": "MODEL", "links": [2]}]), json!(["x.safetensors", 0.6])),
                node(3, "CLIPLoader", 0, json!([]), json!([{"name": "CLIP", "type": "CLIP", "links": [3, 4]}]), json!(["c.safetensors", "stable_diffusion", "default"])),
                node(4, "CLIPTextEncode", 0, json!([{"name": "clip", "type": "CLIP", "link": 3}]),
                     json!([{"name": "CONDITIONING", "type": "CONDITIONING", "links": [5]}]), json!(["1girl"])),
                node(5, "CLIPTextEncode", 0, json!([{"name": "clip", "type": "CLIP", "link": 4}]),
                     json!([{"name": "CONDITIONING", "type": "CONDITIONING", "links": [6]}]), json!(["bad"])),
                node(6, "EmptyLatentImage", 0, json!([]), json!([{"name": "LATENT", "type": "LATENT", "links": [7]}]), json!([832, 1216, 1])),
                node(7, "KSampler", 0,
                     json!([{"name": "model", "type": "MODEL", "link": 2}, {"name": "positive", "type": "CONDITIONING", "link": 5},
                            {"name": "negative", "type": "CONDITIONING", "link": 6}, {"name": "latent_image", "type": "LATENT", "link": 7}]),
                     json!([{"name": "LATENT", "type": "LATENT", "links": [8]}]), json!([111, "randomize", 20, 7, "euler", "normal", 1])),
                node(8, "VAELoader", 0, json!([]), json!([{"name": "VAE", "type": "VAE", "links": [9]}]), json!(["v.safetensors"])),
                node(9, "VAEDecode", 0, json!([{"name": "samples", "type": "LATENT", "link": 8}, {"name": "vae", "type": "VAE", "link": 9}]),
                     json!([{"name": "IMAGE", "type": "IMAGE", "links": [10]}]), json!([])),
                node(10, "SaveImage", 0, json!([{"name": "images", "type": "IMAGE", "link": 10}]), json!([]), json!(["out"])),
            ],
            "links": [
                [1, 1, 0, 2, 0, "MODEL"], [2, 2, 0, 7, 0, "MODEL"], [3, 3, 0, 4, 0, "CLIP"], [4, 3, 0, 5, 0, "CLIP"],
                [5, 4, 0, 7, 1, "CONDITIONING"], [6, 5, 0, 7, 2, "CONDITIONING"], [7, 6, 0, 7, 3, "LATENT"],
                [8, 7, 0, 9, 0, "LATENT"], [9, 8, 0, 9, 1, "VAE"], [10, 9, 0, 10, 0, "IMAGE"]
            ],
            "groups": [], "version": 0.4
        })
    }

    #[test]
    fn api_passthrough() {
        let api = json_of(DEFAULT_API);
        let r = to_api(DEFAULT_API.as_bytes(), "x.json", &Value::Null).unwrap();
        assert_eq!(r.source, "api");
        assert_eq!(r.graph, api);
        assert!(r.warnings.is_empty());
        // 包一層 {"prompt": …}、檔頭有 BOM 也收
        let wrapped = json!({"prompt": api, "extra_data": {}}).to_string();
        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend(wrapped.as_bytes());
        assert_eq!(to_api(&bom, "x.json", &Value::Null).unwrap().graph, api);
        // 空的、不是工作流的 JSON
        assert!(to_api(b"{}", "x.json", &Value::Null).is_err());
        assert!(to_api(b"", "x.json", &Value::Null).unwrap_err().contains("空"));
        let e = to_api(b"[1,2]", "notes.json", &Value::Null).unwrap_err();
        assert!(e.contains("notes.json") && e.contains("匯出（API）"), "{e}");
        assert!(to_api(b"hello", "a.txt", &Value::Null).unwrap_err().contains("PNG"));
    }

    #[test]
    fn default_ui_equals_api_export() {
        let r = to_api(DEFAULT_UI.as_bytes(), "default.json", &oi()).unwrap();
        assert_eq!(r.source, "ui");
        assert_eq!(r.graph, json_of(DEFAULT_API));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        // 沒有 object_info 就轉不了
        assert!(to_api(DEFAULT_UI.as_bytes(), "d.json", &Value::Null).unwrap_err().contains("/object_info"));
        assert!(to_api(DEFAULT_UI.as_bytes(), "d.json", &json!({})).is_err());
    }

    #[test]
    fn seed_control_value_is_skipped() {
        let r = convert(&lora_chain(0)).unwrap();
        let ks = &r.graph["7"]["inputs"];
        assert_eq!(ks["seed"], 111);
        assert_eq!(ks["steps"], 20);
        assert_eq!(ks["cfg"], 7);
        assert_eq!(ks["sampler_name"], "euler");
        assert_eq!(ks["scheduler"], "normal");
        assert_eq!(ks["denoise"], 1);
        assert_eq!(ks["model"], json!(["2", 0]));
        // 舊工作流沒有控制值那一格也要對
        let mut wf = lora_chain(0);
        wf["nodes"][6]["widgets_values"] = json!([111, 20, 7, "euler", "normal", 1]);
        let ks = &convert(&wf).unwrap().graph["7"]["inputs"];
        assert_eq!((ks["seed"].clone(), ks["steps"].clone(), ks["scheduler"].clone()), (json!(111), json!(20), json!("normal")));
        // 自訂節點的 noise_seed（spec 標了 control_after_generate）
        let wf = json!({"nodes": [node(1, "RandomNoise", 0, json!([]), json!([{"name": "NOISE", "type": "NOISE", "links": []}]), json!([12345, "increment"]))],
                        "links": []});
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["1"]["inputs"], json!({"noise_seed": 12345}));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn load_image_upload_value_is_skipped() {
        let outs = json!([{"name": "IMAGE", "type": "IMAGE", "links": []}, {"name": "MASK", "type": "MASK", "links": []}]);
        let wf = json!({"nodes": [
            node(1, "LoadImage", 0, json!([]), outs, json!(["src.png", "image"])),
            node(2, "LoadImageMask", 0, json!([]), json!([{"name": "MASK", "type": "MASK", "links": []}]), json!(["mask.png", "alpha", "image"])),
            // 手寫的、沒有上傳按鈕那一格
            node(3, "LoadImageMask", 0, json!([]), json!([{"name": "MASK", "type": "MASK", "links": []}]), json!(["m2.png", "red"])),
        ], "links": []});
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["1"]["inputs"], json!({"image": "src.png"}));
        assert_eq!(r.graph["2"]["inputs"], json!({"image": "mask.png", "channel": "alpha"}));
        assert_eq!(r.graph["3"]["inputs"], json!({"image": "m2.png", "channel": "red"}));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(crate::video_wf::class(r.graph.get("1")), "LoadImage");
    }

    #[test]
    fn reroute_is_followed() {
        let mut wf = lora_chain(0);
        // UNET → Reroute(11) → LoRA；Reroute 再串一個 Reroute(12)
        wf["nodes"][0]["outputs"][0]["links"] = json!([11]);
        wf["nodes"].as_array_mut().unwrap().push(node(
            11, "Reroute", 0, json!([{"name": "", "type": "*", "link": 11}]), json!([{"name": "", "type": "MODEL", "links": [12]}]), json!([]),
        ));
        wf["nodes"].as_array_mut().unwrap().push(node(
            12, "Reroute", 0, json!([{"name": "", "type": "*", "link": 12}]), json!([{"name": "", "type": "MODEL", "links": [1]}]), json!([]),
        ));
        wf["links"][0] = json!([1, 12, 0, 2, 0, "MODEL"]);
        wf["links"].as_array_mut().unwrap().push(json!([11, 1, 0, 11, 0, "MODEL"]));
        wf["links"].as_array_mut().unwrap().push(json!([12, 11, 0, 12, 0, "MODEL"]));
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["2"]["inputs"]["model"], json!(["1", 0]));
        assert!(r.graph.get("11").is_none() && r.graph.get("12").is_none());
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn bypass_reconnects_by_type() {
        let r = convert(&lora_chain(4)).unwrap();
        assert!(r.graph.get("2").is_none());
        assert_eq!(r.graph["7"]["inputs"]["model"], json!(["1", 0]));
        assert!(r.warnings.iter().any(|w| w.contains("Bypass")), "{:?}", r.warnings);
        assert_eq!(r.graph.as_object().map(Map::len), Some(9));

        // 雙輸入的 LoRA（model, clip）：兩條輸出各自接回同型別的輸入，序號相同
        let mut wf = lora_chain(4);
        wf["nodes"][1] = node(
            2, "LoraLoader", 4,
            json!([{"name": "model", "type": "MODEL", "link": 1}, {"name": "clip", "type": "CLIP", "link": 11}]),
            json!([{"name": "MODEL", "type": "MODEL", "links": [2]}, {"name": "CLIP", "type": "CLIP", "links": [3, 4]}]),
            json!(["x.safetensors", 1, 1]),
        );
        wf["nodes"].as_array_mut().unwrap().push(node(
            11, "CLIPLoader", 0, json!([]), json!([{"name": "CLIP", "type": "CLIP", "links": [11]}]), json!(["c2.safetensors", "stable_diffusion", "default"]),
        ));
        wf["links"][2] = json!([3, 2, 1, 4, 0, "CLIP"]);
        wf["links"][3] = json!([4, 2, 1, 5, 0, "CLIP"]);
        wf["links"].as_array_mut().unwrap().push(json!([11, 11, 0, 2, 1, "CLIP"]));
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["7"]["inputs"]["model"], json!(["1", 0]));
        assert_eq!(r.graph["4"]["inputs"]["clip"], json!(["11", 0]));
        assert_eq!(r.graph["5"]["inputs"]["clip"], json!(["11", 0]));
    }

    #[test]
    fn bypass_without_matching_input_drops_dependents() {
        // 空白潛空間（沒有輸入）被跳過：KSampler 少了必要的 latent_image，連同下游一起拿掉
        let r = convert(&{
            let mut wf = lora_chain(0);
            wf["nodes"][5]["mode"] = json!(4);
            wf
        })
        .unwrap();
        for id in ["6", "7", "9", "10"] {
            assert!(r.graph.get(id).is_none(), "{id} 應該被拿掉");
        }
        assert!(r.graph.get("1").is_some() && r.graph.get("4").is_some());
        assert!(r.warnings.iter().any(|w| w.contains("沒有跟輸出同型別") && w.contains("latent_image")), "{:?}", r.warnings);
    }

    #[test]
    fn muted_node_cascades() {
        let r = convert(&lora_chain(2)).unwrap();
        // LoRA 停用：KSampler 少了 model，VAEDecode 少了 samples，SaveImage 少了 images，一路拿掉
        for id in ["2", "7", "9", "10"] {
            assert!(r.graph.get(id).is_none(), "{id} 應該被拿掉");
        }
        assert_eq!(r.graph.as_object().map(Map::len), Some(6));
        assert!(r.warnings.iter().any(|w| w.contains("停用（Mute）")), "{:?}", r.warnings);
        assert!(r.warnings.iter().any(|w| w.contains("已停用（Mute）的節點") && w.contains("model")), "{:?}", r.warnings);
        assert!(r.warnings.iter().any(|w| w.contains("上游的節點") && w.contains("samples")), "{:?}", r.warnings);
        // 沒有任何節點留下來才算失敗
        let only = json!({"nodes": [node(1, "VAELoader", 2, json!([]), json!([{"name": "VAE", "type": "VAE", "links": []}]), json!(["v.safetensors"]))], "links": []});
        assert!(convert(&only).is_err());
    }

    #[test]
    fn muted_optional_input_only_removes_the_link() {
        let wf = json!({"nodes": [
            node(1, "CLIPLoader", 0, json!([]), json!([{"name": "CLIP", "type": "CLIP", "links": [1]}]), json!(["c.safetensors", "qwen_image", "default"])),
            node(2, "VAELoader", 2, json!([]), json!([{"name": "VAE", "type": "VAE", "links": [2]}]), json!(["v.safetensors"])),
            node(3, "TextEncodeQwenImage21", 0,
                 json!([{"name": "clip", "type": "CLIP", "link": 1}, {"name": "vae", "type": "VAE", "link": 2}]),
                 json!([{"name": "positive", "type": "CONDITIONING", "links": []}]), json!(["p", "n", 1024])),
        ], "links": [[1, 1, 0, 3, 0, "CLIP"], [2, 2, 0, 3, 1, "VAE"]]});
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["3"]["inputs"], json!({"prompt": "p", "negative_prompt": "n", "resolution": 1024, "clip": ["1", 0]}));
        assert!(r.warnings.iter().any(|w| w.contains("選填輸入「vae」")), "{:?}", r.warnings);
    }

    #[test]
    fn primitive_node_fills_widget() {
        let mut wf = lora_chain(0);
        wf["nodes"][6]["inputs"].as_array_mut().unwrap().push(json!({"name": "seed", "type": "INT", "widget": {"name": "seed"}, "link": 12}));
        wf["nodes"].as_array_mut().unwrap().push(node(
            12, "PrimitiveNode", 0, json!([]), json!([{"name": "INT", "type": "INT", "links": [12], "widget": {"name": "seed"}}]), json!([424242, "fixed"]),
        ));
        wf["links"].as_array_mut().unwrap().push(json!([12, 12, 0, 7, 4, "INT"]));
        let r = convert(&wf).unwrap();
        let ks = &r.graph["7"]["inputs"];
        assert_eq!(ks["seed"], 424242);
        assert_eq!(ks["steps"], 20);
        assert!(r.graph.get("12").is_none());
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn set_get_nodes_pass_through() {
        let mut wf = lora_chain(0);
        // UNET → SetNode("model")，GetNode("model") → LoRA
        wf["nodes"][0]["outputs"][0]["links"] = json!([11]);
        wf["nodes"].as_array_mut().unwrap().push(node(11, "SetNode", 0, json!([{"name": "MODEL", "type": "MODEL", "link": 11}]), json!([]), json!(["model"])));
        wf["nodes"].as_array_mut().unwrap().push(node(12, "GetNode", 0, json!([]), json!([{"name": "MODEL", "type": "MODEL", "links": [1]}]), json!(["model"])));
        wf["links"][0] = json!([1, 12, 0, 2, 0, "MODEL"]);
        wf["links"].as_array_mut().unwrap().push(json!([11, 1, 0, 11, 0, "MODEL"]));
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["2"]["inputs"]["model"], json!(["1", 0]));
    }

    #[test]
    fn widget_linked_to_node_keeps_link_and_consumes_value() {
        // 寬高欄位轉成連線孔：連線蓋過欄位值，但欄位值那一格照樣要吃掉，後面的 batch_size 才對得上
        let wf = json!({"nodes": [
            node(1, "AnimaRatioToSize", 0,
                 json!([{"name": "wh_ratio", "type": "STRING", "link": null}, {"name": "megapixels", "type": "FLOAT", "widget": {"name": "megapixels"}, "link": null}]),
                 json!([{"name": "width", "type": "INT", "links": [1]}, {"name": "height", "type": "INT", "links": [2]}]), json!([1, 832, 1248])),
            node(2, "EmptyLatentImage", 0,
                 json!([{"name": "width", "type": "INT", "widget": {"name": "width"}, "link": 1}, {"name": "height", "type": "INT", "widget": {"name": "height"}, "link": 2},
                        {"name": "batch_size", "type": "INT", "widget": {"name": "batch_size"}, "link": null}]),
                 json!([{"name": "LATENT", "type": "LATENT", "links": []}]), json!([1024, 1024, 3])),
        ], "links": [[1, 1, 0, 2, 0, "INT"], [2, 1, 1, 2, 1, "INT"]]});
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["2"]["inputs"], json!({"width": ["1", 0], "height": ["1", 1], "batch_size": 3}));
        // wh_ratio 是必須連線的文字，沒接線要提醒
        assert!(r.warnings.iter().any(|w| w.contains("wh_ratio")), "{:?}", r.warnings);
    }

    #[test]
    fn widgets_values_as_object() {
        let wf = json!({"nodes": [node(1, "EmptyLatentImage", 0, json!([]), json!([{"name": "LATENT", "type": "LATENT", "links": []}]),
                                       json!({"width": 640, "height": 960, "batch_size": 2, "preview": {"x": 1}}))], "links": []});
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph["1"]["inputs"], json!({"width": 640, "height": 960, "batch_size": 2}));
    }

    #[test]
    fn titles_become_meta() {
        let mut wf = lora_chain(0);
        wf["nodes"][3]["title"] = json!("正面提示詞 {{prompt}}");
        wf["nodes"][4]["title"] = json!("");
        let r = convert(&wf).unwrap();
        assert_eq!(crate::video_wf::label(r.graph.get("4")), "正面提示詞 {{prompt}}");
        assert_eq!(r.graph["5"]["_meta"]["title"], "CLIP Text Encode (Prompt)");
        assert_eq!(r.graph["1"]["_meta"]["title"], "Load Diffusion Model");
        // object_info 的 display_name 是 null（新版 V3 節點）時用類別名稱
        assert_eq!(convert(&json!({"nodes": [node(1, "RandomNoise", 0, json!([]), json!([]), json!([1, "fixed"]))], "links": []})).unwrap().graph["1"]["_meta"]["title"], "RandomNoise");
    }

    #[test]
    fn note_group_and_decorations_are_ignored() {
        let mut wf = lora_chain(0);
        let nodes = wf["nodes"].as_array_mut().unwrap();
        nodes.push(node(20, "Note", 0, json!([]), json!([]), json!(["說明"])));
        nodes.push(node(21, "MarkdownNote", 0, json!([]), json!([]), json!(["# 說明"])));
        nodes.push(node(22, "Fast Groups Muter (rgthree)", 0, json!([]), json!([{"name": "OPT_CONNECTION", "type": "*", "links": null}]), json!([])));
        wf["groups"] = json!([{"title": "分組", "bounding": [0, 0, 100, 100]}]);
        wf["extra"] = json!({"ds": {"scale": 1}});
        let r = convert(&wf).unwrap();
        assert_eq!(r.graph.as_object().map(Map::len), Some(10));
        assert!(r.warnings.iter().any(|w| w.contains("Fast Groups Muter")), "{:?}", r.warnings);
        assert!(r.warnings.iter().all(|w| !w.contains("說明")), "{:?}", r.warnings);
    }

    #[test]
    fn missing_custom_nodes_are_listed() {
        // 缺的自訂節點在出圖的路上（VAEDecode → FaceDetailer → ImpactSimpleDetectorSEGS → SaveImage）：擋下來、列出名字
        let mut wf = lora_chain(0);
        {
            let nodes = wf["nodes"].as_array_mut().unwrap();
            nodes.push(node(30, "FaceDetailer", 0, json!([{"name": "image", "type": "IMAGE", "link": 10}]),
                            json!([{"name": "IMAGE", "type": "IMAGE", "links": [31]}]), json!([])));
            nodes.push(node(31, "ImpactSimpleDetectorSEGS", 0, json!([{"name": "image", "type": "IMAGE", "link": 31}]),
                            json!([{"name": "IMAGE", "type": "IMAGE", "links": [32]}]), json!([])));
            // 被停用的缺節點不用管；重複的只列一次
            nodes.push(node(32, "FaceDetailer", 2, json!([]), json!([]), json!([])));
            nodes[9]["inputs"][0]["link"] = json!(32); // SaveImage 改接最後那個
        }
        let links = wf["links"].as_array_mut().unwrap();
        links[9] = json!([10, 9, 0, 30, 0, "IMAGE"]);
        links.push(json!([31, 30, 0, 31, 0, "IMAGE"]));
        links.push(json!([32, 31, 0, 10, 0, "IMAGE"]));
        let e = convert(&wf).unwrap_err();
        assert!(e.starts_with("缺少自訂節點：") && e.ends_with("（在 ComfyUI 裝好、重開 ComfyUI 再匯入）"), "{e}");
        assert!(e.matches("FaceDetailer").count() == 1 && e.contains("ImpactSimpleDetectorSEGS"), "{e}");

        // 缺的節點沒有接到存圖或預覽（旁邊多出來的）：拿掉、提醒，照樣匯入
        let mut wf = lora_chain(0);
        let nodes = wf["nodes"].as_array_mut().unwrap();
        nodes.push(node(30, "ImpactSimpleDetectorSEGS", 0, json!([{"name": "image", "type": "IMAGE", "link": 30}]), json!([]), json!([])));
        nodes.push(node(31, "FaceDetailer", 0, json!([]), json!([{"name": "IMAGE", "type": "IMAGE", "links": [31]}]), json!([])));
        wf["links"].as_array_mut().unwrap().push(json!([30, 9, 0, 30, 0, "IMAGE"]));
        wf["links"].as_array_mut().unwrap().push(json!([31, 31, 0, 30, 0, "IMAGE"]));
        let r = convert(&wf).unwrap();
        assert!(r.warnings.iter().any(|w| w.contains("沒有接到存圖或預覽") && w.contains("FaceDetailer")), "{:?}", r.warnings);
        assert_eq!(r.graph.as_object().map(Map::len), Some(10));

        // 只有被停用的缺節點：可以匯入
        let mut wf = lora_chain(0);
        wf["nodes"].as_array_mut().unwrap().push(node(32, "FaceDetailer", 2, json!([]), json!([]), json!([])));
        assert!(convert(&wf).is_ok());
    }

    #[test]
    fn subgraph_asks_for_api_export() {
        let uuid = "6e0f2a3c-1b4d-4c5e-9f70-1234567890ab";
        let mut wf = lora_chain(0);
        wf["definitions"] = json!({"subgraphs": [{"id": uuid, "name": "我的子圖", "nodes": [], "links": []}]});
        // 定義了但沒用到：不擋
        assert!(convert(&wf).is_ok());
        wf["nodes"].as_array_mut().unwrap().push(node(40, uuid, 0, json!([]), json!([]), json!([])));
        let e = convert(&wf).unwrap_err();
        assert!(e.contains("子圖") && e.contains("匯出（API）"), "{e}");
        // 只靠 UUID 長相也認得（定義被拿掉的殘缺檔）
        let mut wf = lora_chain(0);
        wf["nodes"].as_array_mut().unwrap().push(node(40, uuid, 0, json!([]), json!([]), json!([])));
        assert!(convert(&wf).unwrap_err().contains("匯出（API）"));
        // 群組節點
        let mut wf = lora_chain(0);
        wf["nodes"].as_array_mut().unwrap().push(node(41, "workflow>我的群組", 0, json!([]), json!([]), json!([])));
        assert!(convert(&wf).unwrap_err().contains("群組節點"));
    }

    #[test]
    fn png_with_prompt_chunk() {
        let png = png_with(&[("prompt", DEFAULT_API)]);
        let r = to_api(&png, "ComfyUI_00001_.png", &Value::Null).unwrap();
        assert_eq!(r.source, "png");
        assert_eq!(r.graph, json_of(DEFAULT_API));
        // 兩個都有：優先 prompt
        let mut api = json_of(DEFAULT_API);
        api["3"]["inputs"]["seed"] = json!(7);
        let png = png_with(&[("workflow", DEFAULT_UI), ("prompt", &api.to_string())]);
        let r = to_api(&png, "a.png", &oi()).unwrap();
        assert_eq!((r.source, r.graph["3"]["inputs"]["seed"].clone()), ("png", json!(7)));
    }

    #[test]
    fn png_with_only_workflow() {
        let png = png_with(&[("workflow", DEFAULT_UI)]);
        let r = to_api(&png, "a.png", &oi()).unwrap();
        assert_eq!(r.source, "png");
        assert_eq!(r.graph, json_of(DEFAULT_API));
        // prompt 壞了：退到 workflow 並提醒
        let png = png_with(&[("prompt", "{broken"), ("workflow", DEFAULT_UI)]);
        let r = to_api(&png, "a.png", &oi()).unwrap();
        assert_eq!(r.graph, json_of(DEFAULT_API));
        assert!(r.warnings.iter().any(|w| w.contains("prompt")), "{:?}", r.warnings);
        // workflow 是 UI 格式但沒有 object_info
        assert!(to_api(&png_with(&[("workflow", DEFAULT_UI)]), "a.png", &Value::Null).is_err());
    }

    #[test]
    fn png_without_comfy_metadata() {
        assert_eq!(to_api(&png_with(&[]), "a.png", &oi()).unwrap_err(), NO_WORKFLOW);
        assert!(to_api(&png_with(&[("parameters", "1girl\nSteps: 20")]), "a.png", &oi()).unwrap_err().starts_with("這張圖裡沒有 ComfyUI 工作流"));
        assert!(to_api(&png_with(&[("workflow", "[1]")]), "a.png", &oi()).unwrap_err().contains("看不懂"));
        // 檔頭是 PNG 但後面壞了
        assert!(to_api(b"\x89PNG\r\n\x1a\n", "a.png", &oi()).is_err());
    }

    #[test]
    fn webp_with_exif_metadata() {
        for (le, prefix) in [(true, false), (true, true), (false, false)] {
            // ComfyUI：Model(0x0110)＝prompt:…，Make(0x010f)＝workflow:…
            let entries = vec![(0x010f, format!("workflow:{DEFAULT_UI}")), (0x0110, format!("prompt:{}", DEFAULT_API.replace('\n', "")))];
            let r = to_api(&webp_with(&entries, le, prefix), "a.webp", &oi()).unwrap();
            assert_eq!((r.source, r.graph), ("webp", json_of(DEFAULT_API)), "le={le} prefix={prefix}");
        }
        // 只有 workflow：轉換
        let only = webp_with(&[(0x010f, format!("workflow:{DEFAULT_UI}"))], true, false);
        assert_eq!(to_api(&only, "a.webp", &oi()).unwrap().graph, json_of(DEFAULT_API));
        // 短字串（4 個位元組以內直接放在欄位裡）、沒有 ComfyUI 的欄位
        let none = webp_with(&[(0x010f, "abc".into()), (0x0110, "Canon".into())], true, false);
        assert_eq!(to_api(&none, "a.webp", &oi()).unwrap_err(), NO_WORKFLOW);
        // 沒有 EXIF 區塊、截斷的檔
        let mut bare = b"RIFF\x04\0\0\0WEBP".to_vec();
        assert_eq!(to_api(&bare, "a.webp", &oi()).unwrap_err(), NO_WORKFLOW);
        bare.extend(b"EXIF\xff\xff\xff\x7fII*\0");
        assert_eq!(to_api(&bare, "a.webp", &oi()).unwrap_err(), NO_WORKFLOW);
    }

    // ---- 隨附的工作流
    const BUNDLED: [(&str, &str); 3] = [
        ("Anima_turbo11_boost", include_str!("../../resources/extras/workflows/Anima_turbo11_boost.json")),
        ("Anima_inpaint_uncensor", include_str!("../../resources/extras/workflows/Anima_inpaint_uncensor.json")),
        ("Anima_AI改寫_QwenTurbo", include_str!("../../resources/extras/workflows/Anima_AI改寫_QwenTurbo.json")),
    ];

    fn bundled(name: &str) -> Imported {
        let text = BUNDLED.iter().find(|(n, _)| *n == name).map(|(_, t)| *t).expect("隨附工作流");
        to_api(text.as_bytes(), name, &oi()).unwrap_or_else(|e| panic!("{name} 轉換失敗：{e}"))
    }

    #[test]
    fn bundled_turbo_boost() {
        let r = bundled("Anima_turbo11_boost");
        assert_eq!(r.source, "ui");
        // Turbo LoRA（節點 2）被跳過：Highres Boost 直接接 UNET
        assert!(r.graph.get("2").is_none() && r.graph.get("12").is_none());
        assert_eq!(r.graph["3"]["inputs"]["model"], json!(["1", 0]));
        assert_eq!(r.graph["3"]["inputs"]["strength_model"], 0.6);
        assert_eq!(r.graph["9"]["inputs"]["model"], json!(["3", 0]));
        let ks = &r.graph["9"]["inputs"];
        assert_eq!((ks["seed"].clone(), ks["steps"].clone()), (json!(777001), json!(8)));
        assert_eq!(ks["cfg"].as_f64(), Some(1.0));
        assert_eq!(ks["sampler_name"], "euler");
        assert_eq!(ks["scheduler"], "simple");
        assert_eq!(r.graph["8"]["inputs"], json!({"width": 1536, "height": 1024, "batch_size": 1}));
        assert_eq!(crate::video_wf::label(r.graph.get("6")), "正面提示詞");
        assert_eq!(r.graph["11"]["inputs"]["filename_prefix"], "Anima");
        assert!(r.warnings.iter().any(|w| w.contains("Bypass")), "{:?}", r.warnings);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    }

    #[test]
    fn bundled_inpaint() {
        let r = bundled("Anima_inpaint_uncensor");
        assert_eq!(r.graph["6"]["inputs"], json!({"image": "inpaint_src.png"}));
        assert_eq!(r.graph["7"]["inputs"], json!({"image": "inpaint_mask.png", "channel": "red"}));
        assert_eq!(r.graph["10"]["inputs"]["mask"], json!(["7", 0]));
        assert_eq!(r.graph["10"]["inputs"]["strength"].as_f64(), Some(1.0));
        assert_eq!(r.graph["13"]["inputs"]["seed"], 0);
        assert_eq!(r.graph["13"]["inputs"]["steps"], 8);
        assert_eq!(r.graph["13"]["inputs"]["latent_image"], json!(["12", 0]));
        assert!(r.graph.get("999").is_none());
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn bundled_ai_rewrite() {
        let r = bundled("Anima_AI改寫_QwenTurbo");
        let pe = &r.graph["1"]["inputs"];
        assert_eq!((pe["task"].clone(), pe["seed"].clone(), pe["gpu_layers"].clone()), (json!("t2i"), json!(7), json!(99)));
        assert_eq!(pe["unload_after"], true);
        assert_eq!(r.graph["2"]["inputs"]["text"], json!(["1", 0]));
        // 欄位轉成連線孔、接著別的節點
        let qwen = &r.graph["7"]["inputs"];
        assert_eq!(qwen["prompt"], json!(["2", 0]));
        assert_eq!(qwen["clip"], json!(["5", 0]));
        assert_eq!(qwen["resolution"], 1024);
        assert!(qwen["negative_prompt"].as_str().is_some_and(|s| s.starts_with("artifacts")));
        assert_eq!(r.graph["8"]["inputs"], json!({"width": ["3", 0], "height": ["3", 1], "batch_size": 1}));
        assert_eq!(r.graph["9"]["inputs"]["noise_seed"], 12345);
        assert_eq!(r.graph["12"]["inputs"]["sigmas"], "1.000000, 0.967691, 0.933233, 0.856945, 0.666308, 0.399612, 0.000000");
        assert!(r.graph.get("16").is_none());
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    fn default_api() -> Value {
        serde_json::from_str(DEFAULT_API).unwrap()
    }

    #[test]
    fn marks_written_into_graph() {
        let mut g = default_api();
        g["10"] = json!({"class_type": "LoadImage", "inputs": {"image": "a.png", "upload": "image"}, "_meta": {"title": "Load Image {{image}}"}});
        g["11"] = json!({"class_type": "LoadImage", "inputs": {"image": "b.png", "upload": "image"}, "_meta": {"title": "參考"}});
        let notes = apply_marks(&mut g, &json!({"6": "{{prompt}}", "7.text": "{{negative}}", "11": ["{{image}}"], "3": "{{sampler}}"})).unwrap();
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert_eq!(g["6"]["inputs"]["text"], "{{prompt}}");
        assert_eq!(g["7"]["inputs"]["text"], "{{negative}}");
        assert_eq!(g["11"]["_meta"]["title"], "參考 {{image}}");
        assert_eq!(g["10"]["_meta"]["title"], "Load Image"); // 同一個標記只能一個節點：舊的拿掉
        assert_eq!(g["3"]["_meta"]["title"], "KSampler {{sampler}}");
        // 再標一次不會重複
        apply_marks(&mut g, &json!({"3": "{{sampler}}"})).unwrap();
        assert_eq!(g["3"]["_meta"]["title"], "KSampler {{sampler}}");
        // 錯的：回錯誤原因、什麼都不改（有一個錯就整批不改）
        let before = g.clone();
        for bad in [
            json!({"6": "{{nope}}"}),
            json!({"99": "{{prompt}}"}),
            json!({"6": "{{image}}"}),
            json!({"3": "{{prompt}}"}),
            json!({"6.clip": "{{prompt}}"}),
            json!({"6": 5}),
            json!({"7": "{{negative}}", "4": "{{mask}}"}),
        ] {
            assert!(apply_marks(&mut g, &bad).is_err(), "{bad}");
            assert_eq!(g, before, "{bad}");
        }
        assert!(apply_marks(&mut g, &json!({})).unwrap().is_empty());
        assert!(apply_marks(&mut g, &Value::Null).unwrap().is_empty());
        // 好幾個文字欄位：有 text／prompt 就用它，不然要指定欄位
        let mut g2 = json!({"1": {"class_type": "CLIPTextEncodeSDXL", "inputs": {"text_g": "a", "text_l": "b", "width": 1024}}});
        let e = apply_marks(&mut g2, &json!({"1": "{{prompt}}"})).unwrap_err();
        assert!(e.contains("text_g") && e.contains("1.欄位名"), "{e}");
        apply_marks(&mut g2, &json!({"1.text_l": "{{prompt}}"})).unwrap();
        assert_eq!(g2["1"]["inputs"], json!({"text_g": "a", "text_l": "{{prompt}}", "width": 1024}));
    }

    #[test]
    fn need_from_analysis_errors() {
        let g = default_api();
        // 拿真的分析錯誤來認（訊息改了這裡會抓到）
        let e = crate::image_wf::analyze(&g, "img2img").unwrap_err();
        assert_eq!(need_of(&e), Some("image"), "{e}");
        let e = crate::image_wf::analyze(&g, "multiref").unwrap_err();
        assert_eq!(need_of(&e), Some("ref1"), "{e}");
        let mut g2 = g.clone();
        g2["10"] = json!({"class_type": "LoadImage", "inputs": {"image": "a.png"}, "_meta": {"title": "Load Image {{image}}"}});
        g2["11"] = json!({"class_type": "VAEEncode", "inputs": {"pixels": ["10", 0], "vae": ["4", 2]}});
        g2["3"]["inputs"]["latent_image"] = json!(["11", 0]);
        let e = crate::image_wf::analyze(&g2, "inpaint").unwrap_err();
        assert_eq!(need_of(&e), Some("mask"), "{e}");
        assert_eq!(need_of("找不到要放提示詞的地方：在 ComfyUI 正面提示詞的文字框裡寫上 {{prompt}}，再匯出（API）一次"), Some("prompt"));
        assert_eq!(need_of("有好幾個正面提示詞的文字框，不知道要換哪個（#6、#12）"), Some("prompt"));
        assert_eq!(need_of("幫這個工作流取個名字"), None);
    }

    #[test]
    fn candidates_for_picking() {
        let mut g = default_api();
        g["10"] = json!({"class_type": "LoadImage", "inputs": {"image": "a.png", "upload": "image"}, "_meta": {"title": "Load Image"}});
        g["12"] = json!({"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "x"}});
        let c = candidates(&g);
        let ids = |k: &str| c[k].as_array().unwrap().iter().map(|e| e["node"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ids("texts"), ["6", "7"]);
        assert_eq!((c["texts"][0]["field"].clone(), c["texts"][1]["text"].clone()), (json!("text"), json!("text, watermark")));
        assert_eq!(ids("images"), ["10"]);
        assert_eq!(c["images"][0]["image"], "a.png");
        assert_eq!(ids("models"), ["4"]);
        assert_eq!(c["models"][0]["value"], "v1-5-pruned-emaonly-fp16.safetensors");
        assert_eq!(ids("samplers"), ["3"]);
        assert_eq!(candidates(&json!([])), json!({"texts": [], "images": [], "models": [], "samplers": []}));
    }

    #[test]
    fn mode_guessed_from_nodes() {
        let g = default_api();
        assert_eq!(guess_mode(&g), None);
        // Load Image → 縮放 → VAE Encode → KSampler：圖生圖
        let mut i2i = g.clone();
        i2i["10"] = json!({"class_type": "LoadImage", "inputs": {"image": "a.png"}});
        i2i["11"] = json!({"class_type": "ImageScale", "inputs": {"image": ["10", 0], "upscale_method": "nearest-exact", "width": 512, "height": 512, "crop": "disabled"}});
        i2i["12"] = json!({"class_type": "VAEEncode", "inputs": {"pixels": ["11", 0], "vae": ["4", 2]}});
        i2i["3"]["inputs"]["latent_image"] = json!(["12", 0]);
        assert_eq!(guess_mode(&i2i).map(|m| m.0), Some("img2img"));
        // 局部重繪用的節點
        let mut inp = i2i.clone();
        inp["13"] = json!({"class_type": "SetLatentNoiseMask", "inputs": {"samples": ["12", 0], "mask": ["10", 1]}});
        assert_eq!(guess_mode(&inp).map(|m| m.0), Some("inpaint"));
        // 只有放大模型
        let up = json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "a.png"}},
            "2": {"class_type": "UpscaleModelLoader", "inputs": {"model_name": "4x.pth"}},
            "3": {"class_type": "ImageUpscaleWithModel", "inputs": {"upscale_model": ["2", 0], "image": ["1", 0]}},
            "4": {"class_type": "SaveImage", "inputs": {"images": ["3", 0], "filename_prefix": "x"}}
        });
        assert_eq!(guess_mode(&up).map(|m| m.0), Some("upscale"));
        // 文生圖＋ControlNet（Load Image 接 ControlNet）＋高解析修復（VAE Decode → 放大 → VAE Encode）：還是文生圖
        let mut hires = g.clone();
        hires["20"] = json!({"class_type": "LoadImage", "inputs": {"image": "pose.png"}});
        hires["21"] = json!({"class_type": "ControlNetLoader", "inputs": {"control_net_name": "cn.safetensors"}});
        hires["22"] = json!({"class_type": "ControlNetApply", "inputs": {"conditioning": ["6", 0], "control_net": ["21", 0], "image": ["20", 0], "strength": 1.0}});
        hires["3"]["inputs"]["positive"] = json!(["22", 0]);
        hires["23"] = json!({"class_type": "ImageScaleBy", "inputs": {"image": ["8", 0], "upscale_method": "nearest-exact", "scale_by": 1.5}});
        hires["24"] = json!({"class_type": "VAEEncode", "inputs": {"pixels": ["23", 0], "vae": ["4", 2]}});
        hires["25"] = json!({"class_type": "KSampler", "inputs": {"model": ["4", 0], "positive": ["22", 0], "negative": ["7", 0], "latent_image": ["24", 0],
                                                         "seed": 1, "steps": 10, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 0.4}});
        assert_eq!(guess_mode(&hires), None);
    }

    /// 手動用：WF_IMPORT_OBJECT_INFO 指到一份完整的 /object_info JSON，三個隨附工作流都要轉得過
    #[test]
    #[ignore]
    fn bundled_with_full_object_info() {
        let path = std::env::var("WF_IMPORT_OBJECT_INFO").expect("要設 WF_IMPORT_OBJECT_INFO");
        let full: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("讀 object_info")).expect("解析 object_info");
        for (name, text) in BUNDLED {
            let r = to_api(text.as_bytes(), name, &full).unwrap_or_else(|e| panic!("{name} 轉換失敗：{e}"));
            println!("{name}: {} 個節點，提醒 {:?}", r.graph.as_object().map_or(0, Map::len), r.warnings);
        }
    }
}

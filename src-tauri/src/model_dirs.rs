//! 外部模型資料夾（2026-10-20）：電腦裡本來就有的模型「原地使用」，不搬、不複製、不重新下載。
//!
//! - 清單存在 <資料>\data\model_dirs.json：[{kind, path, source, added}]。kind 是 ComfyUI 的模型分類
//!   （diffusion_models、loras、vae…；LLM＝AI 改寫用的語言模型，生圖站自己讀）。
//! - 生圖站會把清單寫成 ComfyUI\anima_model_paths.yaml（extra_model_paths 的格式），啟動 ComfyUI 時用
//!   --extra-model-paths-config 帶進去（launcher.rs）；使用者自己的 extra_model_paths.yaml 不動。改了要重開 ComfyUI 才生效。
//! - 生圖站自己找檔（判斷模型家族、列 LoRA、安裝精靈算缺哪些模型）也會一起看這些資料夾：roots(kind)。
//! - 掃描：把每顆本機硬碟整個走一遍，找兩種東西：
//!   1. 安裝精靈清單（models_basic／full.json）裡的檔：檔名和大小都對才算，子資料夾也要對（loras/qwen21/x 要在 qwen21 底下）
//!   2. 模型庫：別的 ComfyUI 的 models\<分類>、A1111／Forge 的 models\Stable-diffusion、Lora…，還有別人寫的 extra_model_paths.yaml
//!
//!   掃描只讀資料夾清單和檔案大小，不打開模型檔（除了判斷家族時讀檔頭）。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

/// ComfyUI 的模型分類（yaml 的鍵）＋給人看的名字。順序就是設定頁顯示的順序
pub const KINDS: &[(&str, &str)] = &[
    ("diffusion_models", "主模型（UNET／DiT）"),
    ("checkpoints", "Checkpoint（整包模型）"),
    ("loras", "LoRA"),
    ("vae", "VAE"),
    ("text_encoders", "文字編碼器"),
    ("upscale_models", "放大模型"),
    ("model_patches", "模型補丁（構圖控制 LLLite 等）"),
    ("controlnet", "ControlNet"),
    ("clip_vision", "CLIP Vision"),
    ("embeddings", "Embedding"),
    ("geometry_estimation", "深度估計"),
    ("background_removal", "去背模型"),
    ("LLM", "AI 改寫用的語言模型（GGUF）"),
];

/// 算模型檔的副檔名
pub const EXTS: &[&str] = &["safetensors", "sft", "ckpt", "pt", "pth", "bin", "gguf", "onnx"];

pub fn is_model_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_lowercase().as_str()))
}

/// 舊名換成現在的名字（ComfyUI 的 map_legacy）；不認得的回 None
pub fn canonical(kind: &str) -> Option<&'static str> {
    let k = match kind.to_lowercase().as_str() {
        "unet" => "diffusion_models",
        "clip" => "text_encoders",
        "llm" => "LLM",
        k => return KINDS.iter().find(|(n, _)| n.to_lowercase() == k).map(|(n, _)| *n),
    };
    Some(k)
}

pub fn kind_label(kind: &str) -> &'static str {
    KINDS.iter().find(|(k, _)| *k == kind).map(|(_, l)| *l).unwrap_or("其他")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dir {
    pub kind: String,
    pub path: PathBuf,
    /// scan＝掃描找到的、manual＝自己加的
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub added: String,
}

static EXTRA: RwLock<Vec<Dir>> = RwLock::new(Vec::new());

pub fn file(data: &Path) -> PathBuf {
    data.join("model_dirs.json")
}

pub fn yaml_path(comfyui: &Path) -> PathBuf {
    comfyui.join("anima_model_paths.yaml")
}

/// 讀清單（壞掉或沒有＝空的）
pub fn load(data: &Path) -> Vec<Dir> {
    std::fs::read(file(data))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| serde_json::from_value(v["dirs"].clone()).ok())
        .unwrap_or_default()
}

/// 開站時讀一次，之後生圖站自己找檔都看 roots()
pub fn init(data: &Path) {
    *EXTRA.write().unwrap_or_else(|e| e.into_inner()) = load(data);
}

pub fn current() -> Vec<Dir> {
    EXTRA.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 清單實際生效的樣子：每個 checkpoints 資料夾同時也算主模型（diffusion_models）資料夾，不看資料夾叫什麼名字
/// （寫進 yaml、生圖站自己找檔都用這份）。
/// 原因：A1111／Forge／SwarmUI 的 Stable-diffusion 資料夾（ComfyUI 官方範例 yaml 也把它填在 checkpoints）裡，
/// Anima、MOSS 這類只有主模型的單檔跟 SDXL（Illustrious）整包模型混著放；ComfyUI 的 UNETLoader 只看 diffusion_models，
/// 生成頁選單列的就是 UNETLoader 的清單，只登記成 checkpoints 的話那些主模型永遠不會出現。
/// 整包模型（SDXL…）跟著出現在主模型清單也沒關係：生成頁只列內建畫得出來的家族（認家族看檔頭，有快取）
pub fn effective(dirs: &[Dir]) -> Vec<Dir> {
    effective_with(dirs, None)
}

/// effective，加上使用者自己 ComfyUI 的 extra_model_paths.yaml 裡填成 checkpoints 的資料夾（那份 yaml 我們不改，
/// 所以在自己寫的 yaml 裡把它們也列成主模型）。comfyui＝目前使用的 ComfyUI 資料夾；只看 yaml 檔，不看開著的 ComfyUI 回報的，結果才不會一直變
pub fn effective_with(dirs: &[Dir], comfyui: Option<&Path>) -> Vec<Dir> {
    let mut out = dirs.to_vec();
    let mut extra: Vec<Dir> = dirs.iter().filter(|d| d.kind == "checkpoints").cloned().collect();
    if let Some(c) = comfyui {
        let own = c.join("models").join("checkpoints");
        for y in comfy_extra(c).into_iter().filter(|y| y.kind == "checkpoints" && y.path.is_dir() && !same(&y.path, &own)) {
            extra.push(Dir { kind: y.kind, path: y.path, source: "yaml".into(), added: String::new() });
        }
    }
    for d in extra {
        if !out.iter().any(|o| o.kind == "diffusion_models" && same(&o.path, &d.path)) {
            out.push(Dir { kind: "diffusion_models".into(), ..d });
        }
    }
    out
}

/// 清單裡的資料夾，開著的 ComfyUI 還沒讀到（加了資料夾沒重開 ComfyUI；ComfyUI 不是生圖站開的所以沒帶到 yaml）。
/// 判斷方式：ComfyUI 回報的資料夾（refresh_live，每分鐘更新）裡找不到。ComfyUI 沒回報過、或它根本不認得那個分類
/// （舊版 ComfyUI 沒有的新分類、生圖站自己用的 LLM）就不算，免得永遠跳不必要的提醒
pub fn pending_restart() -> Vec<Dir> {
    let live = LIVE.read().unwrap_or_else(|e| e.into_inner()).clone();
    if live.is_empty() {
        return vec![];
    }
    let comfy = COMFY.read().unwrap_or_else(|e| e.into_inner()).clone();
    pending_with(&effective_with(&current(), comfy.as_deref()), &live)
}

fn pending_with(dirs: &[Dir], live: &[(String, PathBuf)]) -> Vec<Dir> {
    dirs.iter()
        .filter(|d| d.path.is_dir() && live.iter().any(|(k, _)| *k == d.kind) && !live.iter().any(|(k, p)| *k == d.kind && same(p, &d.path)))
        .cloned()
        .collect()
}

/// 某個分類的外部資料夾（unet＝diffusion_models、clip＝text_encoders）：
/// 生圖站自己的「外部模型資料夾」清單在前，後面接使用者自己 ComfyUI 的 extra_model_paths.yaml（comfy_extra）、
/// 開著的 ComfyUI 回報的資料夾（live）。重複的、生圖站自己 ComfyUI 的 models\<分類> 不再列一次
pub fn roots(kind: &str) -> Vec<PathBuf> {
    let comfy = COMFY.read().unwrap_or_else(|e| e.into_inner()).clone();
    roots_with(kind, comfy.as_deref())
}

/// roots 的本體：comfyui＝目前使用的 ComfyUI 資料夾（沒有就只看生圖站自己的清單）
pub fn roots_with(kind: &str, comfyui: Option<&Path>) -> Vec<PathBuf> {
    let k = canonical(kind).unwrap_or(kind);
    let mut v: Vec<PathBuf> = effective_with(&current(), comfyui).into_iter().filter(|d| d.kind == k).map(|d| d.path).collect();
    if let Some(c) = comfyui {
        // 整包模型資料夾也算主模型資料夾（見 effective）：開著的 ComfyUI 回報的 checkpoints 資料夾也一起找檔、判斷家族
        let also = if k == "diffusion_models" { comfy_extra_roots(c, "checkpoints") } else { vec![] };
        for p in comfy_extra_roots(c, k).into_iter().chain(also) {
            if !v.iter().any(|x| same(x, &p)) {
                v.push(p);
            }
        }
    }
    v
}

// ---------------------------------------------------------------- 沿用使用者自己 ComfyUI 的 extra_model_paths.yaml
/// yaml 裡的一個資料夾：分類、完整路徑、那一段有沒有寫 is_default
#[derive(Debug, Clone, PartialEq)]
pub struct YamlDir {
    pub kind: String,
    pub path: PathBuf,
    pub default: bool,
}

/// 目前使用的 ComfyUI 資料夾（main.py 那層；開站時 init_comfy 記下）
static COMFY: RwLock<Option<PathBuf>> = RwLock::new(None);
/// yaml 檔的解析結果快取：路徑 → (修改時間, 大小, 內容)
type YamlCache = HashMap<PathBuf, (std::time::SystemTime, u64, Vec<YamlDir>)>;
static YAML_CACHE: Mutex<Option<YamlCache>> = Mutex::new(None);
/// 開著的 ComfyUI 的 /internal/folder_paths 回報的資料夾（refresh_live 更新）
static LIVE: RwLock<Vec<(String, PathBuf)>> = RwLock::new(Vec::new());

/// 開站時記下目前使用的 ComfyUI 資料夾，roots() 才知道要去讀哪個 extra_model_paths.yaml
pub fn init_comfy(comfyui: &Path) {
    *COMFY.write().unwrap_or_else(|e| e.into_inner()) = Some(comfyui.to_path_buf());
}

/// ComfyUI 會讀的 yaml：資料夾裡的 extra_model_paths.yaml、使用者在「額外啟動參數」自己指的 --extra-model-paths-config、
/// 官方桌面版的 %APPDATA%\ComfyUI\extra_models_config.yaml。生圖站自己寫的 anima_model_paths.yaml 不算（那份就是外部模型資料夾清單）
pub fn comfy_yaml_files(comfyui: &Path) -> Vec<PathBuf> {
    let mut v = vec![comfyui.join("extra_model_paths.yaml")];
    if let Ok(args) = crate::gpu::extra_args(&crate::gpu::prefs().extra_args) {
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let val = match a.strip_prefix("--extra-model-paths-config=") {
                Some(x) => Some(x.to_string()),
                None if a == "--extra-model-paths-config" => it.next(),
                None => None,
            };
            if let Some(x) = val.map(|x| PathBuf::from(x.trim_matches(['"', '\'']))).filter(|x| x.is_absolute()) {
                v.push(x);
            }
        }
    }
    if !comfyui.join("main.py").is_file() {
        if let Some(a) = std::env::var_os("APPDATA") {
            v.push(PathBuf::from(a).join("ComfyUI").join("extra_models_config.yaml"));
        }
    }
    let ours = yaml_path(comfyui);
    let mut out: Vec<PathBuf> = vec![];
    for f in v {
        let mine = f.file_name() == ours.file_name();
        if !mine && f.is_file() && !out.iter().any(|o| same(o, &f)) {
            out.push(f);
        }
    }
    out
}

/// 讀一份 yaml（依修改時間、大小快取，沒改就不重讀）
fn read_yaml_cached(f: &Path) -> Vec<YamlDir> {
    let Ok(m) = std::fs::metadata(f) else { return vec![] };
    let key = (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len());
    if let Some((t, n, v)) = YAML_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|c| c.get(f)) {
        if (*t, *n) == key {
            return v.clone();
        }
    }
    let text = std::fs::read(f).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let v = parse_yaml_dirs(&text, f.parent().unwrap_or(Path::new(".")));
    YAML_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(f.to_path_buf(), (key.0, key.1, v.clone()));
    v
}

/// ComfyUI 的 extra_model_paths.yaml 裡所有資料夾（不管在不在）。排序照 ComfyUI：is_default 的段排前面（後寫的在先），其他照檔案順序
pub fn comfy_extra(comfyui: &Path) -> Vec<YamlDir> {
    let mut defaults: Vec<YamlDir> = vec![];
    let mut rest: Vec<YamlDir> = vec![];
    for f in comfy_yaml_files(comfyui) {
        for d in read_yaml_cached(&f) {
            if d.default {
                defaults.insert(0, d);
            } else {
                rest.push(d);
            }
        }
    }
    defaults.extend(rest);
    defaults
}

/// 是不是 ComfyUI 自己的 models\<分類>（含舊名 unet、clip）：這些本來就會讀，不算外部資料夾。
/// 2026-11-10 以前這條規則抄了三份，comfy_extra_view 那份漏了舊名，設定頁會把內建資料夾列成「沿用的」
fn own_dir(comfyui: &Path, kind: &str, p: &Path) -> bool {
    same(p, &comfyui.join("models").join(kind))
        || (kind == "diffusion_models" && same(p, &comfyui.join("models").join("unet")))
        || (kind == "text_encoders" && same(p, &comfyui.join("models").join("clip")))
}

/// yaml 和開著的 ComfyUI 回報的、給某個分類的資料夾：存在、不是 ComfyUI 自己的 models\<分類>、不重複
fn comfy_extra_roots(comfyui: &Path, kind: &str) -> Vec<PathBuf> {
    let own = |p: &Path| own_dir(comfyui, kind, p);
    let live: Vec<PathBuf> = LIVE.read().unwrap_or_else(|e| e.into_inner()).iter().filter(|(k, _)| k == kind).map(|(_, p)| p.clone()).collect();
    let mut out: Vec<PathBuf> = vec![];
    for p in comfy_extra(comfyui).into_iter().filter(|d| d.kind == kind).map(|d| d.path).chain(live) {
        if p.is_dir() && !own(&p) && !out.iter().any(|o| same(o, &p)) {
            out.push(p);
        }
    }
    out
}

/// 給設定頁看：沿用了哪些 yaml、裡面有幾個（存在的）資料夾
pub fn comfy_extra_view(comfyui: &Path) -> Value {
    let files = comfy_yaml_files(comfyui);
    let mut dirs: Vec<Value> = vec![];
    let mut seen: Vec<(String, PathBuf)> = vec![];
    for d in comfy_extra(comfyui) {
        let own = own_dir(comfyui, &d.kind, &d.path);
        if own || !d.path.is_dir() || seen.iter().any(|(k, p)| *k == d.kind && same(p, &d.path)) {
            continue;
        }
        dirs.push(json!({"kind": d.kind, "label": kind_label(&d.kind), "path": d.path.to_string_lossy()}));
        seen.push((d.kind, d.path));
    }
    json!({"files": files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>(), "count": dirs.len(), "dirs": dirs})
}

/// /internal/folder_paths 的回應 → (分類, 資料夾)。每一項是 [[路徑…], [副檔名…]]（也收單純的路徑陣列）
pub fn parse_folder_paths(v: &Value) -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    for (name, val) in v.as_object().into_iter().flatten() {
        let Some(kind) = canonical(name) else { continue };
        let list = match val.get(0) {
            Some(Value::Array(a)) => a.as_slice(),
            _ => val.as_array().map(Vec::as_slice).unwrap_or(&[]),
        };
        for p in list.iter().filter_map(Value::as_str) {
            out.push((kind.to_string(), PathBuf::from(p)));
        }
    }
    out
}

/// 問開著的 ComfyUI（base＝http://127.0.0.1:8188）它看得到哪些模型資料夾，記下來給 roots() 用。
/// 連不上就維持上次的結果（ComfyUI 重開的空檔不要讓選單忽然少東西）；回傳記到幾個資料夾。會連本機網路，要在背景執行緒呼叫
pub fn refresh_live(base: &str) -> Option<usize> {
    let client = reqwest::blocking::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(3)).build().ok()?;
    let v: Value = client.get(format!("{}/internal/folder_paths", base.trim_end_matches('/'))).send().ok()?.error_for_status().ok()?.json().ok()?;
    let list = parse_folder_paths(&v);
    let n = list.len();
    *LIVE.write().unwrap_or_else(|e| e.into_inner()) = list;
    Some(n)
}

/// 同一個資料夾（不分大小寫、斜線方向、結尾斜線）。字面不同時再看真正的位置：
/// models 常用 junction 指到別顆硬碟（D:\ComfyUI\models\loras → D:\ComfyUI_models\loras），掃描找到的是後者
pub fn same(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    n(a) == n(b)
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => n(&x) == n(&y),
            _ => false,
        }
}

/// 檢查、去掉重複、存檔、寫 ComfyUI 的 yaml，生效給生圖站自己用。comfyui＝ComfyUI 資料夾（裡面有 main.py、models）
pub fn save(data: &Path, comfyui: &Path, dirs: Vec<Dir>) -> Result<Vec<Dir>, String> {
    save_checked(data, comfyui, dirs).map(|(out, _)| out)
}

/// save，另外回警告（給使用者看的白話）：清單裡有資料夾找不到時不讓整批存檔失敗。
/// 本來就在清單裡、現在找不到的（外接硬碟沒插、網路磁碟沒連上）照樣留著，接回來就又能用；新加的找不到就略過
pub fn save_checked(data: &Path, comfyui: &Path, dirs: Vec<Dir>) -> Result<(Vec<Dir>, Vec<String>), String> {
    let mut out: Vec<Dir> = vec![];
    let mut warnings: Vec<String> = vec![];
    let before = current();
    for mut d in dirs {
        let kind = canonical(&d.kind).ok_or_else(|| format!("不認得的模型分類：{}", d.kind))?;
        d.kind = kind.to_string();
        if !d.path.is_absolute() {
            return Err(format!("資料夾要寫完整路徑（例如 D:\\models\\loras）：{}", d.path.display()));
        }
        if !d.path.is_dir() {
            if before.iter().any(|b| b.kind == d.kind && b.path == d.path) {
                warnings.push(format!("找不到資料夾 {}（外接硬碟沒插或網路磁碟沒連上？）：先留在清單裡，接回來就能用", d.path.display()));
            } else {
                warnings.push(format!("找不到資料夾 {}，這個先略過、沒有加進清單：確認路徑打對、硬碟有接上之後再加一次", d.path.display()));
                continue;
            }
        }
        // 斜線統一（貼上的路徑常常是 / 和 \ 混著），結尾的斜線拿掉（磁碟根目錄 D:\ 保留）
        if cfg!(windows) {
            let s = d.path.to_string_lossy().replace('/', "\\");
            d.path = PathBuf::from(if s.len() > 3 { s.trim_end_matches('\\') } else { &s });
        }
        // 生圖站自己的 ComfyUI 本來就會讀
        if same(&d.path, &comfyui.join("models").join(kind)) {
            continue;
        }
        if out.iter().any(|o| o.kind == d.kind && same(&o.path, &d.path)) {
            continue;
        }
        if d.added.is_empty() {
            d.added = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        }
        out.push(d);
    }
    std::fs::create_dir_all(data).map_err(|e| format!("建立 {} 失敗：{e}", data.display()))?;
    let body = serde_json::to_vec_pretty(&json!({"dirs": out})).map_err(|e| e.to_string())?;
    let tmp = file(data).with_extension("json.tmp");
    std::fs::write(&tmp, body)
        .and_then(|_| std::fs::rename(&tmp, file(data)))
        .map_err(|e| format!("存 {} 失敗：{e}", file(data).display()))?;
    write_yaml(comfyui, &out)?;
    *EXTRA.write().unwrap_or_else(|e| e.into_inner()) = out.clone();
    Ok((out, warnings))
}

/// ComfyUI 的 extra_model_paths 格式。沒有外部資料夾就把檔刪掉
pub fn yaml_text(dirs: &[Dir], comfyui: Option<&Path>) -> String {
    yaml_render(&effective_with(dirs, comfyui))
}

fn yaml_render(dirs: &[Dir]) -> String {
    let mut by: Vec<(&str, Vec<String>)> = vec![];
    for d in dirs {
        let p = d.path.to_string_lossy().replace('\\', "/");
        match by.iter_mut().find(|(k, _)| *k == d.kind) {
            Some((_, v)) => v.push(p),
            None => by.push((d.kind.as_str(), vec![p])),
        }
    }
    let mut s = String::from("# 繪匣 PaintDeck產生的檔（設定 → 模型與工作流 → 外部模型資料夾），不要手改；改了會被蓋掉。\n");
    s.push_str("# 啟動 ComfyUI 時用 --extra-model-paths-config 帶進去。你自己的 extra_model_paths.yaml 照樣有效。\n");
    s.push_str("anima_station:\n");
    for (k, v) in by {
        s.push_str(&format!("    {k}: |\n"));
        for p in v {
            s.push_str(&format!("        {p}\n"));
        }
    }
    s
}

fn write_yaml(comfyui: &Path, dirs: &[Dir]) -> Result<(), String> {
    let y = yaml_path(comfyui);
    let eff = effective_with(dirs, Some(comfyui));
    if eff.is_empty() {
        if y.exists() {
            std::fs::remove_file(&y).map_err(|e| format!("刪掉 {} 失敗：{e}", y.display()))?;
        }
        return Ok(());
    }
    if !comfyui.is_dir() {
        return Ok(()); // 精靈還沒裝 ComfyUI：裝好後下一次存清單、或開站時（ensure_yaml）會寫
    }
    std::fs::write(&y, yaml_render(&eff)).map_err(|e| format!("寫 {} 失敗：{e}", y.display()))
}

/// 開站時對一次：yaml 跟清單對不上（ComfyUI 後來才裝好、被刪掉、升版後規則變了）就重寫。
/// 要在啟動 ComfyUI 之前做，ComfyUI 才讀得到新的
pub fn ensure_yaml(comfyui: &Path) {
    let dirs = current();
    let eff = effective_with(&dirs, Some(comfyui));
    if !eff.is_empty() && std::fs::read_to_string(yaml_path(comfyui)).ok().as_deref() != Some(yaml_render(&eff).as_str()) {
        if let Err(e) = write_yaml(comfyui, &dirs) {
            eprintln!("{e}");
        }
    }
}

/// 某個模型（分類＋子路徑，例如 loras、qwen21/x.safetensors）在不在外部資料夾裡、大小對不對
pub fn find(dirs: &[Dir], kind: &str, sub: &str, size: Option<u64>) -> Option<PathBuf> {
    let k = canonical(kind)?;
    dirs.iter()
        .filter(|d| d.kind == k)
        .map(|d| sub.split('/').fold(d.path.clone(), |p, s| p.join(s)))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && size.is_none_or(|n| m.len() == n)))
}

/// 某一類模型的所有檔（生圖站自己的 ComfyUI models\<分類>、外部資料夾清單、ComfyUI 的 yaml 資料夾）：(路徑, 大小)。
/// 給 find_renamed 找「改過名字的同一個模型」用；要找很多個檔時先建一次，不要每個檔都走一遍
pub fn size_index(models: &Path, extra: &[Dir], kind: &str) -> Vec<(PathBuf, u64)> {
    let Some(k) = canonical(kind) else { return vec![] };
    let mut roots: Vec<PathBuf> = vec![models.join(k)];
    match k {
        "diffusion_models" => roots.push(models.join("unet")),
        "text_encoders" => roots.push(models.join("clip")),
        _ => {}
    }
    roots.extend(extra.iter().filter(|d| d.kind == k).map(|d| d.path.clone()));
    roots.extend(roots_with(k, models.parent()));
    let mut seen: Vec<PathBuf> = vec![];
    let mut out = vec![];
    for r in roots {
        if seen.iter().any(|s| same(s, &r)) {
            continue;
        }
        for p in crate::setup::files_under(&r).into_iter().filter(|p| is_model_file(p)) {
            if let Ok(m) = std::fs::metadata(&p) {
                out.push((p, m.len()));
            }
        }
        seen.push(r);
    }
    out
}

/// 清單裡的 rel（例如 loras/qwen21/x.safetensors）檔名不同、大小完全一樣的檔＝多半是使用者改過名字的同一個模型。
/// 回傳實際的檔案路徑（要用的話得照這個檔名，不是清單裡的名字）；檔名一樣的不算（那種 find 就找得到）
pub fn find_renamed(index: &[(PathBuf, u64)], rel: &str, size: u64) -> Option<PathBuf> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    index
        .iter()
        .find(|(p, n)| *n == size && size > 0 && p.file_name().and_then(|f| f.to_str()).is_some_and(|f| !f.eq_ignore_ascii_case(name)))
        .map(|(p, _)| p.clone())
}

// ---------------------------------------------------------------- 讀別人寫的 extra_model_paths.yaml
/// ComfyUI 讀路徑時會展開 ~ 和環境變數（%NAME%、$NAME、${NAME}）
fn expand_vars(s: &str) -> String {
    let home = || std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    let mut s = s.to_string();
    if s == "~" || s.starts_with("~/") || s.starts_with("~\\") {
        s = format!("{}{}", home(), &s[1..]);
    }
    let mut out = String::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let (open, close) = match cs[i] {
            '%' => ('%', Some('%')),
            '$' if cs.get(i + 1) == Some(&'{') => ('{', Some('}')),
            '$' => ('$', None),
            c => {
                out.push(c);
                i += 1;
                continue;
            }
        };
        let start = if open == '{' { i + 2 } else { i + 1 };
        let mut j = start;
        while j < cs.len() && (cs[j].is_alphanumeric() || cs[j] == '_') {
            j += 1;
        }
        let name: String = cs[start..j].iter().collect();
        let closed = close.is_none_or(|c| cs.get(j) == Some(&c));
        match std::env::var(&name).ok().filter(|_| !name.is_empty() && closed) {
            Some(v) => {
                out.push_str(&v);
                i = j + usize::from(close.is_some());
            }
            None => {
                out.push(cs[i]);
                i += 1;
            }
        }
    }
    out
}

/// 一段（yaml 最上層的一個名字）讀到的東西
#[derive(Default)]
struct Section {
    base: Option<PathBuf>,
    default: bool,
    items: Vec<(String, String)>,
}

fn flush_yaml(sec: &mut Section, yaml_dir: &Path, out: &mut Vec<YamlDir>) {
    let sec = std::mem::take(sec);
    for (k, v) in sec.items {
        let Some(kind) = canonical(&k) else { continue };
        let p = PathBuf::from(expand_vars(v.trim().trim_matches(['"', '\''])));
        // ComfyUI：有 base_path 就接在它後面（絕對路徑接上去還是自己），沒有的話相對路徑算在 yaml 所在的資料夾
        let full = match &sec.base {
            Some(b) => b.join(&p),
            None if !p.is_absolute() => yaml_dir.join(&p),
            None => p,
        };
        out.push(YamlDir { kind: kind.to_string(), path: full, default: sec.default });
    }
}

/// 只認 ComfyUI 範例那種寫法：最上層一段一段，段裡 base_path、is_default、`分類: 路徑` 或 `分類: |` 接好幾行路徑。
/// 不管資料夾在不在（要的人自己再篩）
pub fn parse_yaml_dirs(text: &str, yaml_dir: &Path) -> Vec<YamlDir> {
    let mut out = vec![];
    let mut sec = Section::default();
    let mut block: Option<String> = None; // 正在讀 `分類: |` 的那幾行
    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').split(" #").next().unwrap_or("").trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            flush_yaml(&mut sec, yaml_dir, &mut out);
            block = None;
            continue;
        }
        let t = line.trim();
        if let Some(k) = &block {
            if !t.contains(": ") && !t.ends_with(':') && !t.ends_with('|') {
                sec.items.push((k.clone(), t.to_string()));
                continue;
            }
            block = None;
        }
        let Some((k, v)) = t.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k == "base_path" {
            let b = PathBuf::from(expand_vars(v.trim_matches(['"', '\''])));
            sec.base = Some(if b.is_absolute() { b } else { yaml_dir.join(b) });
        } else if k == "is_default" {
            sec.default = matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1");
        } else if v == "|" || v == "|-" || v == ">" || v == ">-" {
            block = Some(k.to_string());
        } else if !v.is_empty() {
            sec.items.push((k.to_string(), v.to_string()));
        }
    }
    flush_yaml(&mut sec, yaml_dir, &mut out);
    out
}

/// parse_yaml_dirs，只留存在的資料夾、不帶 is_default（掃描用）
pub fn parse_extra_yaml(text: &str, yaml_dir: &Path) -> Vec<(String, PathBuf)> {
    parse_yaml_dirs(text, yaml_dir).into_iter().filter(|d| d.path.is_dir()).map(|d| (d.kind, d.path)).collect()
}

// ---------------------------------------------------------------- 掃描
/// 安裝精靈清單裡的一個檔（models_*.json 的 path、size）
#[derive(Clone)]
pub struct Want {
    pub rel: String,
    pub size: u64,
}

/// 一個模型庫（某個分類的資料夾）
#[derive(Debug, Clone, Serialize)]
pub struct Library {
    pub kind: String,
    pub path: PathBuf,
    /// 怎麼認出來的：comfyui＝別的 ComfyUI 的 models\<分類>、a1111、yaml＝別人寫的 extra_model_paths.yaml
    pub how: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Found {
    /// 清單裡的 path（loras/qwen21/x.safetensors）
    pub rel: String,
    pub file: PathBuf,
    pub kind: String,
    /// 要加進清單的資料夾（檔案路徑去掉分類底下的子路徑）
    pub root: PathBuf,
    pub size: u64,
}

#[derive(Default, Serialize, Clone)]
pub struct ScanState {
    pub running: bool,
    pub finished: bool,
    pub cancelled: bool,
    pub dirs: u64,
    pub files: u64,
    pub current: String,
    pub found: Vec<Found>,
    pub libraries: Vec<Library>,
    /// 檔名對、大小或子資料夾不對（給使用者看，不會自動用）
    pub near: Vec<Value>,
    pub seconds: f64,
}

static SCAN: Mutex<Option<ScanState>> = Mutex::new(None);
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn scan_state() -> ScanState {
    SCAN.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub fn scan_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// A1111／Forge／reForge／SwarmUI 的 models 底下的資料夾名稱 → 分類。名稱比對時不分大小寫、底線和減號視為一樣
/// （A1111 叫 Stable-diffusion、SwarmUI 叫 Stable-Diffusion，Forge 另外有 text_encoder）
const A1111: &[(&str, &str)] = &[
    ("stable-diffusion", "checkpoints"),
    ("lora", "loras"),
    ("lycoris", "loras"),
    ("locon", "loras"),
    ("vae", "vae"),
    ("esrgan", "upscale_models"),
    ("realesrgan", "upscale_models"),
    ("swinir", "upscale_models"),
    ("scunet", "upscale_models"),
    ("ldsr", "upscale_models"),
    ("text-encoder", "text_encoders"),
    ("controlnet", "controlnet"),
];

/// 資料夾名稱 → A1111 系的分類（Forge、reForge、SwarmUI 一樣）
fn a1111_kind(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase().replace('_', "-");
    A1111.iter().find(|(k, _)| *k == n).map(|(_, k)| *k)
}

/// 這個資料夾是不是某個模型庫：父資料夾叫 models（SwarmUI 叫 Models），自己的名字是 ComfyUI 的分類或 A1111 系的資料夾名
fn library_kind(dir: &Path) -> Option<(&'static str, &'static str)> {
    let name = dir.file_name()?.to_str()?.to_lowercase();
    let parent = dir.parent()?.file_name()?.to_str()?.to_lowercase();
    if parent != "models" {
        return None;
    }
    if let Some(k) = canonical(&name) {
        return Some((k, "comfyui"));
    }
    a1111_kind(&name).map(|k| (k, "a1111"))
}

/// 掃描時不用進去的資料夾（除了 setup::skip_dir 那些）
fn skip_more(name: &str) -> bool {
    let n = name.to_lowercase();
    matches!(
        n.as_str(),
        "appdata" | "python_embeded" | "site-packages" | "__pycache__" | "custom_nodes" | "output" | "input" | "temp" | "windowsapps" | "node_modules" | "incremental"
    ) || n.starts_with("anima_web")
}

/// 開始背景掃描（已經在掃就不重開）。want＝安裝精靈清單；known＝已經在用的資料夾（生圖站的 ComfyUI models、外部清單），結果會標出來
pub fn scan_start(want: Vec<Want>, extra_roots: Vec<PathBuf>) -> bool {
    {
        let mut g = SCAN.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|s| s.running) {
            return false;
        }
        *g = Some(ScanState { running: true, ..Default::default() });
    }
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let mut roots = crate::setup::fixed_drives();
        for r in extra_roots {
            if !roots.iter().any(|x| r.starts_with(x)) {
                roots.push(r);
            }
        }
        let r = scan_roots(&roots, &want);
        let mut g = SCAN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut() {
            s.running = false;
            s.finished = true;
            s.cancelled = CANCEL.load(Ordering::SeqCst);
            s.current.clear();
            s.found = r.0;
            s.libraries = r.1;
            s.near = r.2;
            s.seconds = (t.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        }
    });
    true
}

fn update(f: impl FnOnce(&mut ScanState)) {
    if let Some(s) = SCAN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        f(s)
    }
}

/// 走一遍：回傳 (清單裡找到的, 模型庫, 差一點的)
pub fn scan_roots(roots: &[PathBuf], want: &[Want]) -> (Vec<Found>, Vec<Library>, Vec<Value>) {
    // 檔名（小寫）→ 清單裡的檔
    let mut by_name: HashMap<String, Vec<&Want>> = HashMap::new();
    for w in want {
        if let Some(n) = w.rel.rsplit('/').next() {
            by_name.entry(n.to_lowercase()).or_default().push(w);
        }
    }
    let mut found: Vec<Found> = vec![];
    let mut libs: Vec<Library> = vec![];
    let mut near: Vec<Value> = vec![];
    let mut yamls: Vec<PathBuf> = vec![];
    // (資料夾, 屬於哪個模型庫)
    let mut stack: Vec<(PathBuf, Option<usize>)> = roots.iter().map(|r| (r.clone(), None)).collect();
    let (mut ndirs, mut nfiles) = (0u64, 0u64);
    while let Some((dir, lib)) = stack.pop() {
        if CANCEL.load(Ordering::SeqCst) {
            break;
        }
        ndirs += 1;
        if ndirs % 200 == 0 {
            let (d, f, c) = (ndirs, nfiles, dir.to_string_lossy().to_string());
            update(|s| {
                s.dirs = d;
                s.files = f;
                s.current = c;
            });
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            // 連結到別處的 models 或 models\<分類>（很多人把模型放別顆硬碟再用 junction／symlink 連過來）：
            // 不往下走，只把它記成模型庫（檔案另外算）。Windows 的連結 is_dir() 是 false，要先看 is_symlink
            if ft.is_symlink() && lib.is_none() && p.is_dir() && !crate::setup::skip_dir(&name) {
                let kinds: Vec<(PathBuf, &str, &str)> = match library_kind(&p) {
                    Some((k, how)) => vec![(p.clone(), k, how)],
                    None if name.eq_ignore_ascii_case("models") => std::fs::read_dir(&p)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|c| c.path())
                        .filter(|c| c.is_dir())
                        .filter_map(|c| library_kind(&c).map(|(k, how)| (c, k, how)))
                        .collect(),
                    None => vec![],
                };
                for (path, kind, how) in kinds {
                    if libs.iter().any(|l| l.kind == kind && same(&l.path, &path)) {
                        continue;
                    }
                    let (files, bytes) = count_models(&path);
                    libs.push(Library { kind: kind.into(), path, how: how.into(), files, bytes });
                }
                continue;
            }
            if ft.is_dir() {
                // 連結（junction、捷徑）不跟進去：同一份檔會被算好幾次，也可能繞圈
                if ft.is_symlink() || crate::setup::skip_dir(&name) || skip_more(&name) {
                    continue;
                }
                let mut l = lib;
                if lib.is_none() {
                    if let Some((kind, how)) = library_kind(&p) {
                        libs.push(Library { kind: kind.into(), path: p.clone(), how: how.into(), files: 0, bytes: 0 });
                        l = Some(libs.len() - 1);
                    }
                }
                stack.push((p, l));
                continue;
            }
            if name.eq_ignore_ascii_case("extra_model_paths.yaml") {
                yamls.push(p.clone());
                continue;
            }
            if !is_model_file(&p) {
                continue;
            }
            nfiles += 1;
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            if let Some(i) = lib {
                libs[i].files += 1;
                libs[i].bytes += size;
            }
            let Some(ws) = by_name.get(&name.to_lowercase()) else { continue };
            for w in ws {
                let mut parts: Vec<&str> = w.rel.split('/').collect();
                let kind = parts.remove(0);
                let ok_size = size == w.size;
                // 子資料夾要對得上：x.safetensors 在 …\qwen21\x.safetensors
                let mut root = p.clone();
                let mut ok_sub = true;
                for part in parts.iter().rev() {
                    let matches = root.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(part));
                    if !matches {
                        ok_sub = false;
                        break;
                    }
                    root = root.parent().map(Path::to_path_buf).unwrap_or_default();
                }
                if ok_size && ok_sub {
                    if !found.iter().any(|f| f.rel == w.rel && f.file == p) {
                        found.push(Found { rel: w.rel.clone(), file: p.clone(), kind: canonical(kind).unwrap_or(kind).into(), root, size });
                    }
                } else if near.len() < 50 {
                    near.push(json!({"rel": w.rel, "file": p.to_string_lossy(), "size": size, "want_size": w.size,
                                      "why": if ok_size { "subdir" } else { "size" }}));
                }
            }
        }
    }
    // 別人寫的 extra_model_paths.yaml：裡面的資料夾也算模型庫（檔案數另外算）
    for y in yamls {
        let Ok(text) = std::fs::read_to_string(&y) else { continue };
        for (kind, path) in parse_extra_yaml(&text, y.parent().unwrap_or(Path::new("."))) {
            if libs.iter().any(|l| l.kind == kind && same(&l.path, &path)) {
                continue;
            }
            let (files, bytes) = count_models(&path);
            libs.push(Library { kind, path, how: "yaml".into(), files, bytes });
        }
    }
    libs.retain(|l| l.files > 0);
    libs.sort_by(|a, b| (a.path.to_string_lossy().to_lowercase(), &a.kind).cmp(&(b.path.to_string_lossy().to_lowercase(), &b.kind)));
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    update(|s| {
        s.dirs = ndirs;
        s.files = nfiles;
    });
    (found, libs, near)
}

/// 給網頁看的掃描結果：每一項標出是不是已經在用（生圖站的 ComfyUI 本來就會讀，或已經在外部清單裡）。
/// suggest＝建議加進清單的資料夾（找到的清單模型所在的資料夾＋沒在用的模型庫），網頁預設全勾
pub fn scan_view(comfyui: &Path, dirs: &[Dir]) -> Value {
    let s = scan_state();
    let used = |kind: &str, path: &Path| own_dir(comfyui, kind, path) || dirs.iter().any(|d| d.kind == kind && same(&d.path, path));
    let mut suggest: Vec<Value> = vec![];
    let mut push = |kind: &str, path: &Path, why: Value| {
        if used(kind, path) || suggest.iter().any(|x| x["kind"] == kind && same(Path::new(x["path"].as_str().unwrap_or("")), path)) {
            return;
        }
        suggest.push(json!({"kind": kind, "label": kind_label(kind), "path": path.to_string_lossy(), "why": why}));
    };
    for f in &s.found {
        push(&f.kind, &f.root, json!({"type": "manifest"}));
    }
    for l in &s.libraries {
        push(&l.kind, &l.path, json!({"type": l.how, "files": l.files, "bytes": l.bytes}));
    }
    // 同一個資料夾在清單裡找到幾個檔
    for x in suggest.iter_mut() {
        let p = PathBuf::from(x["path"].as_str().unwrap_or(""));
        let n = s.found.iter().filter(|f| f.kind == x["kind"].as_str().unwrap_or("") && same(&f.root, &p)).count();
        x["manifest_files"] = json!(n);
    }
    let found: Vec<Value> = s
        .found
        .iter()
        .map(|f| json!({"rel": f.rel, "file": f.file.to_string_lossy(), "kind": f.kind, "size": f.size, "used": used(&f.kind, &f.root)}))
        .collect();
    json!({"running": s.running, "finished": s.finished, "cancelled": s.cancelled, "dirs": s.dirs, "files": s.files,
           "current": s.current, "seconds": s.seconds, "found": found, "suggest": suggest, "near": s.near})
}

/// 「加一個資料夾」：kind 指定了就照用；auto＝自己看
/// - 是 ComfyUI、A1111／Forge 的資料夾（或它們的 models）：底下每個有模型檔的分類資料夾各加一筆
/// - 資料夾名字就是分類（loras、Lora、vae…）：照名字
/// - 都不是：抽看裡面最多 20 個模型檔的權重，多數決
pub fn expand(path: &Path, kind: &str) -> Result<Vec<Dir>, String> {
    if !path.is_absolute() {
        return Err(format!("要寫完整路徑（例如 D:\\models\\loras）：{}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("找不到資料夾 {}", path.display()));
    }
    let one = |k: &str| Dir { kind: k.into(), path: path.to_path_buf(), source: "manual".into(), added: String::new() };
    if kind != "auto" {
        let k = canonical(kind).ok_or_else(|| format!("不認得的模型分類：{kind}"))?;
        return Ok(vec![one(k)]);
    }
    let models_root = if path.join("models").is_dir() {
        Some(path.join("models"))
    } else if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("models")) {
        Some(path.to_path_buf())
    } else {
        None
    };
    if let Some(m) = models_root {
        let mut out: Vec<Dir> = vec![];
        for e in std::fs::read_dir(&m).map_err(|e| format!("讀不到 {}：{e}", m.display()))?.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some((k, _)) = library_kind(&p) else { continue };
            if count_models(&p).0 > 0 && !out.iter().any(|d| d.kind == k && same(&d.path, &p)) {
                out.push(Dir { kind: k.into(), path: p, source: "manual".into(), added: String::new() });
            }
        }
        // A1111／Forge 的 embeddings 放在 models 外面（網頁介面資料夾底下）
        let emb = path.join("embeddings");
        if m != path && emb.is_dir() && count_models(&emb).0 > 0 {
            out.push(Dir { kind: "embeddings".into(), path: emb, source: "manual".into(), added: String::new() });
        }
        if out.is_empty() {
            return Err(format!("{} 底下的分類資料夾裡都沒有模型檔", m.display()));
        }
        return Ok(out);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    if let Some(k) = canonical(&name).or_else(|| a1111_kind(&name)) {
        return Ok(vec![one(k)]);
    }
    let mut votes: HashMap<&str, usize> = HashMap::new();
    for f in crate::setup::files_under(path).iter().filter(|p| is_model_file(p)).take(20) {
        if let (Some(k), ..) = classify_file(f) {
            *votes.entry(k).or_default() += 1;
        }
    }
    match votes.into_iter().max_by_key(|(_, n)| *n) {
        Some((k, _)) => Ok(vec![one(k)]),
        None => Err(format!("看不出 {} 放的是哪一類模型：請自己選分類", path.display())),
    }
}

/// 資料夾（含子資料夾）裡有幾個模型檔、多大
pub fn count_models(dir: &Path) -> (u64, u64) {
    crate::setup::files_under(dir)
        .iter()
        .filter(|p| is_model_file(p))
        .fold((0, 0), |(n, b), p| (n + 1, b + std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)))
}

// ---------------------------------------------------------------- 判斷模型檔是哪一類
/// safetensors 檔頭的權重名稱（不載入權重）；不是 safetensors 或讀不到＝空的
pub fn tensor_keys(path: &Path) -> Vec<String> {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return vec![] };
    let mut len = [0u8; 8];
    if f.read_exact(&mut len).is_err() {
        return vec![];
    }
    let n = u64::from_le_bytes(len);
    if n == 0 || n > 200 << 20 {
        return vec![];
    }
    let mut buf = vec![0u8; n as usize];
    if f.read_exact(&mut buf).is_err() {
        return vec![];
    }
    serde_json::from_slice::<Value>(&buf)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().filter(|k| *k != "__metadata__").cloned().collect()))
        .unwrap_or_default()
}

/// 依權重名稱猜分類（猜不出來回 None，讓使用者自己選）
pub fn guess_kind_from_keys(keys: &[String]) -> Option<&'static str> {
    if keys.is_empty() {
        return None;
    }
    let any = |f: &dyn Fn(&str) -> bool| keys.iter().any(|k| f(k));
    let frac = |f: &dyn Fn(&str) -> bool| keys.iter().filter(|k| f(k)).count() as f64 / keys.len() as f64;
    if any(&|k| k.contains("lora_") || k.contains(".lora_A") || k.contains(".lora_B") || k.contains("lora_down") || k.contains("lora_up")
        || k.contains("hada_w") || k.contains("lokr_w"))
    {
        return Some("loras");
    }
    if any(&|k| k.starts_with("lllite_")) {
        return Some("model_patches");
    }
    if any(&|k| k.starts_with("model.diffusion_model.")) && any(&|k| k.starts_with("first_stage_model.") || k.starts_with("cond_stage_model.") || k.starts_with("conditioner.")) {
        return Some("checkpoints");
    }
    if any(&|k| k.starts_with("control_model.") || k.contains("controlnet_") || k.starts_with("input_hint_block")) {
        return Some("controlnet");
    }
    // 影片模型（Wan、LTX）：家族是 other，但一樣是主模型；LTX 的整包檔（連 VAE 一起）放 checkpoints
    if crate::families::is_video_keys(keys) {
        return Some(if any(&|k| k.starts_with("vae.") || k.starts_with("audio_vae.")) { "checkpoints" } else { "diffusion_models" });
    }
    if crate::families::detect_keys(keys) != crate::families::OTHER
        || any(&|k| k.contains("double_blocks.") || k.contains("single_blocks.") || k.contains("joint_blocks.") || k.starts_with("model.diffusion_model."))
    {
        return Some("diffusion_models");
    }
    if frac(&|k| k.starts_with("encoder.") || k.starts_with("decoder.") || k.starts_with("quant_conv") || k.starts_with("post_quant_conv")) > 0.9 {
        return Some("vae");
    }
    if any(&|k| k.starts_with("model.layers.") || k.starts_with("encoder.block.") || k.starts_with("text_model.encoder.")
        || k.starts_with("transformer.resblocks.") || k.starts_with("language_model.") || k.starts_with("shared.weight"))
    {
        return Some("text_encoders");
    }
    if any(&|k| k.starts_with("body.") || k.starts_with("model.0.") || k.starts_with("conv_first.")) {
        return Some("upscale_models");
    }
    None
}

/// 猜一個檔是哪一類：(分類, 家族, 說明)。家族只有主模型、LoRA 才看
pub fn classify_file(path: &Path) -> (Option<&'static str>, &'static str, String) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "safetensors" | "sft" => {
            let keys = tensor_keys(path);
            if keys.is_empty() {
                return (None, crate::families::OTHER, "讀不到 safetensors 檔頭（檔案可能壞了或還沒下載完）".into());
            }
            let kind = guess_kind_from_keys(&keys);
            // 整包模型（SDXL、SD1.5…）也看家族；detect 會再參考檔頭的 metadata（kohya、modelspec）
            let fam = if matches!(kind, Some("diffusion_models" | "loras" | "checkpoints")) {
                crate::families::detect(path)
            } else {
                crate::families::OTHER
            };
            let why = match kind {
                Some(k) => format!("看權重名稱判斷是「{}」", kind_label(k)),
                None => "看不出是哪一類，請自己選".into(),
            };
            (kind, fam, why)
        }
        "gguf" => {
            // 語言模型的 GGUF 會寫 general.architecture＝qwen／llama…；生圖的 GGUF（city96）是 flux、wan…
            let head = read_head(path, 256 * 1024);
            let llm = ["qwen", "llama", "gemma", "mistral", "phi", "deepseek", "glm"].iter().any(|a| head.contains(a));
            // 影片模型（H3、Wan、LTX）再看張量名稱：architecture 沒寫在清單裡的（MiniMax H3 的量化版）以前會「看不出來」、下載中心報失敗
            let img = ["flux", "wan", "hunyuan", "sd3", "ltxv", "cosmos", "hidream", "lumina", "chroma"].iter().any(|a| head.contains(a))
                || crate::families::is_video_file(path);
            if name.contains("mmproj") || (llm && !img) {
                (Some("LLM"), crate::families::OTHER, "GGUF 語言模型（AI 改寫用）".into())
            } else if img {
                (Some("diffusion_models"), crate::families::OTHER, "GGUF 生圖模型（要裝 ComfyUI-GGUF 節點才讀得到）".into())
            } else {
                (None, crate::families::OTHER, "看不出這個 GGUF 是語言模型還是生圖模型，請自己選".into())
            }
        }
        "pth" | "pt" if ["esrgan", "x4", "x2", "upscal", "ultrasharp", "swinir", "dat"].iter().any(|s| name.contains(s)) => {
            (Some("upscale_models"), crate::families::OTHER, "檔名看起來是放大模型".into())
        }
        _ => (None, crate::families::OTHER, "看不出是哪一類，請自己選".into()),
    }
}

fn read_head(path: &Path, n: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    let got = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    String::from_utf8_lossy(&buf[..got]).to_lowercase()
}

// ---------------------------------------------------------------- 模型總覽
/// 某分類底下所有模型檔（生圖站的 ComfyUI models＋外部資料夾）：[{name, size, root, external}]
pub fn list_kind(comfyui: &Path, kind: &str) -> Vec<Value> {
    let mut dirs: Vec<(PathBuf, bool)> = vec![(comfyui.join("models").join(kind), false)];
    match kind {
        "diffusion_models" => dirs.push((comfyui.join("models").join("unet"), false)),
        "text_encoders" => dirs.push((comfyui.join("models").join("clip"), false)),
        _ => {}
    }
    dirs.extend(roots(kind).into_iter().map(|p| (p, true)));
    let mut out: Vec<Value> = vec![];
    for (r, external) in dirs {
        for p in crate::setup::files_under(&r) {
            if !is_model_file(&p) {
                continue;
            }
            let name = p.strip_prefix(&r).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            if out.iter().any(|o| o["name"] == name.as_str()) {
                continue; // ComfyUI 也是先找到的那個算數
            }
            out.push(json!({"name": name, "size": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                            "root": r.to_string_lossy(), "external": external}));
        }
    }
    out.sort_by(|a, b| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anima_model_dirs_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn kinds_and_legacy_names() {
        assert_eq!(canonical("unet"), Some("diffusion_models"));
        assert_eq!(canonical("CLIP"), Some("text_encoders"));
        assert_eq!(canonical("llm"), Some("LLM"));
        assert_eq!(canonical("Loras"), Some("loras"));
        assert_eq!(canonical("nope"), None);
    }

    #[test]
    fn yaml_round_trip_and_foreign_yaml() {
        let dirs = vec![
            Dir { kind: "loras".into(), path: "D:\\x\\loras".into(), source: "".into(), added: "".into() },
            Dir { kind: "loras".into(), path: "E:\\更多 LoRA".into(), source: "".into(), added: "".into() },
            Dir { kind: "vae".into(), path: "D:\\x\\vae".into(), source: "".into(), added: "".into() },
        ];
        let y = yaml_text(&dirs, None);
        assert!(y.contains("    loras: |\n        D:/x/loras\n        E:/更多 LoRA\n"));
        assert!(y.contains("    vae: |\n        D:/x/vae\n"));
        // 別人寫的 yaml：base_path＋相對路徑、單行、區塊、註解、unet 舊名
        let d = tmp("yaml");
        for s in ["models/checkpoints", "models/unet", "loraA", "loraB"] {
            std::fs::create_dir_all(d.join(s)).unwrap();
        }
        let text = format!(
            "# comment\ncomfyui:\n    base_path: {}\n    is_default: true\n    checkpoints: models/checkpoints/\n    unet: models/unet  # 舊名\n    vae: models/vae\n\nother:\n    loras: |\n        {}\n        {}\n",
            d.display(),
            d.join("loraA").display(),
            d.join("loraB").display()
        );
        let got = parse_extra_yaml(&text, &d);
        let kinds: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["checkpoints", "diffusion_models", "loras", "loras"]); // vae 資料夾不存在：不算
        assert!(got[1].1.ends_with("models/unet"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_checkpoints_dir_also_counts_as_diffusion_models() {
        let sd = |kind: &str, p: &str| Dir { kind: kind.into(), path: p.into(), source: "".into(), added: "".into() };
        // 不管資料夾叫什麼（Forge 的 Stable-diffusion、使用者自己取的名字）：整包模型資料夾一律也算主模型資料夾
        let dirs = vec![
            sd("checkpoints", "D:\\Forge\\models\\Stable-diffusion"),
            sd("checkpoints", "D:\\my_models\\big"),
            sd("loras", "D:\\Forge\\models\\Lora"),
        ];
        let eff = effective(&dirs);
        assert_eq!(eff.iter().filter(|d| d.kind == "diffusion_models").count(), 2);
        let y = yaml_text(&dirs, None);
        assert!(y.contains("    diffusion_models: |\n        D:/Forge/models/Stable-diffusion\n        D:/my_models/big\n"), "{y}");
        assert!(y.contains("    checkpoints: |\n        D:/Forge/models/Stable-diffusion\n        D:/my_models/big\n"), "{y}");
        // 使用者自己也登記過同一個主模型資料夾（斜線、大小寫、結尾斜線不同）：不重複
        let mut again = dirs.clone();
        again.push(sd("diffusion_models", "D:/Forge/models/stable-diffusion/"));
        assert_eq!(effective(&again).iter().filter(|d| d.kind == "diffusion_models").count(), 2);
        // 沒有 checkpoints 資料夾就不多出東西（連 yaml 都不用寫）
        assert_eq!(effective(&[sd("loras", "D:\\l")]).len(), 1);
    }

    #[test]
    fn users_own_yaml_checkpoints_dir_is_listed_as_diffusion_models_too() {
        // ComfyUI 官方範例 yaml 把 A1111 的 Stable-diffusion 填成 checkpoints：我們不改那份，在自己的 yaml 裡補一份主模型
        let c = tmp("own_yaml_comfy");
        let sdd = tmp("own_yaml_sd");
        std::fs::create_dir_all(c.join("models").join("checkpoints")).unwrap();
        std::fs::write(c.join("extra_model_paths.yaml"), format!("a111:\n    checkpoints: |\n        {}\n        {}\n", sdd.display(), c.join("models").join("checkpoints").display())).unwrap();
        let eff = effective_with(&[], Some(&c));
        let paths: Vec<&Path> = eff.iter().filter(|d| d.kind == "diffusion_models").map(|d| d.path.as_path()).collect();
        // （這台電腦的官方桌面版 yaml 也可能補進別的資料夾，所以不數總數）
        assert!(paths.iter().any(|p| same(p, &sdd)), "{eff:?}");
        assert!(!paths.iter().any(|p| same(p, &c.join("models").join("checkpoints"))), "{eff:?}");
        assert!(yaml_text(&[], Some(&c)).contains(&sdd.to_string_lossy().replace('\\', "/")));
        // ComfyUI 自己的 models\checkpoints 不用補（它本來就會讀）；沒給 ComfyUI 資料夾就不讀 yaml
        assert!(effective_with(&[], None).is_empty());
        let _ = std::fs::remove_dir_all(&c);
        let _ = std::fs::remove_dir_all(&sdd);
    }

    #[test]
    fn pending_restart_only_counts_what_comfyui_knows_about() {
        let a = tmp("pending_a");
        let b = tmp("pending_b");
        let sd = |kind: &str, p: &Path| Dir { kind: kind.into(), path: p.to_path_buf(), source: "".into(), added: "".into() };
        let dirs = vec![sd("diffusion_models", &a), sd("loras", &b), sd("model_patches", &b), sd("LLM", &b)];
        // ComfyUI 回報的資料夾裡有 a、沒有 b：只有 b 的 loras 算「還沒讀到」
        let live = vec![("diffusion_models".to_string(), a.clone()), ("loras".to_string(), PathBuf::from("D:\\別的"))];
        let p = pending_with(&dirs, &live);
        assert_eq!(p.iter().map(|d| d.kind.as_str()).collect::<Vec<_>>(), ["loras"]);
        // ComfyUI 讀到了（路徑寫法不同也算同一個）：沒有待辦
        let live = vec![("diffusion_models".to_string(), a.clone()), ("loras".to_string(), PathBuf::from(b.to_string_lossy().replace('\\', "/").to_uppercase()))];
        assert!(pending_with(&dirs[..2], &live).is_empty());
        // 資料夾不見了（外接硬碟沒插）不算
        assert!(pending_with(&[sd("loras", Path::new("Z:\\不存在"))], &live).is_empty());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn scan_finds_manifest_files_and_libraries() {
        let d = tmp("scan");
        let put = |rel: &str, n: usize| {
            let p = rel.split('/').fold(d.clone(), |p, s| p.join(s));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; n]).unwrap();
        };
        put("old_comfy/models/loras/qwen21/fast.safetensors", 10); // 子資料夾對、大小對
        put("old_comfy/models/loras/a.safetensors", 7); // 大小不對
        put("stash/qwen21/fast.safetensors", 10); // 也對（子資料夾名稱一樣）
        put("stash/b.safetensors", 3); // 子資料夾不對
        put("webui/models/Lora/mine.safetensors", 5); // A1111 模型庫
        put("old_comfy/models/vae/notes.txt", 5); // 不是模型檔
        let want = vec![
            Want { rel: "loras/qwen21/fast.safetensors".into(), size: 10 },
            Want { rel: "loras/a.safetensors".into(), size: 8 },
            Want { rel: "loras/qwen21/b.safetensors".into(), size: 3 },
        ];
        let (found, libs, near) = scan_roots(std::slice::from_ref(&d), &want);
        let roots: Vec<String> = found.iter().map(|f| f.root.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(roots.iter().any(|r| r.ends_with("old_comfy/models/loras")));
        assert!(roots.iter().any(|r| r.ends_with("/stash")));
        assert!(found.iter().all(|f| f.kind == "loras"));
        assert_eq!(near.len(), 2); // a（大小）、b（子資料夾）
        let lk: Vec<(&str, &str)> = libs.iter().map(|l| (l.kind.as_str(), l.how.as_str())).collect();
        assert!(lk.contains(&("loras", "comfyui")));
        assert!(lk.contains(&("loras", "a1111")));
        assert!(!lk.iter().any(|(k, _)| *k == "vae")); // 沒有模型檔的模型庫不列
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 掃描：models 整個是 junction、models\loras 是 junction，都要記成模型庫（掃描不跟進連結）
    #[cfg(windows)]
    #[test]
    fn scan_finds_linked_libraries() {
        let d = tmp("linked");
        let mklink = |link: &Path, target: &Path| {
            std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        // a：一般的 ComfyUI（git clone），models\checkpoints 是真的資料夾、models\loras 連到別處
        std::fs::create_dir_all(d.join("a").join("models").join("checkpoints")).unwrap();
        std::fs::write(d.join("a").join("models").join("checkpoints").join("c.safetensors"), b"x").unwrap();
        std::fs::create_dir_all(d.join("store").join("my_loras")).unwrap();
        std::fs::write(d.join("store").join("my_loras").join("l.safetensors"), b"xy").unwrap();
        // b：整個 models 連到 store\b_models
        std::fs::create_dir_all(d.join("store").join("b_models").join("vae")).unwrap();
        std::fs::write(d.join("store").join("b_models").join("vae").join("v.safetensors"), b"xyz").unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        if !mklink(&d.join("a").join("models").join("loras"), &d.join("store").join("my_loras"))
            || !mklink(&d.join("b").join("models"), &d.join("store").join("b_models"))
        {
            return; // 不能建 junction 的環境就不測
        }
        let (_, libs, _) = scan_roots(std::slice::from_ref(&d), &[]);
        let has = |kind: &str, p: &Path, files: u64| libs.iter().any(|l| l.kind == kind && same(&l.path, p) && l.files == files);
        assert!(has("checkpoints", &d.join("a").join("models").join("checkpoints"), 1));
        assert!(has("loras", &d.join("a").join("models").join("loras"), 1));
        assert!(has("vae", &d.join("b").join("models").join("vae"), 1));
        assert_eq!(libs.len(), 3, "{libs:?}"); // store 底下的真資料夾父資料夾不叫 models，不另外算
        let _ = std::fs::remove_dir_all(&d);
    }

    /// models 是 junction（使用者的 D:\...\ComfyUI\models → D:\AI\ComfyUI_models）：指到同一處的外部資料夾不能再加一次
    #[cfg(windows)]
    #[test]
    fn junction_is_same_folder() {
        let d = tmp("junction");
        let (real, comfyui) = (d.join("ComfyUI_models"), d.join("ComfyUI"));
        std::fs::create_dir_all(real.join("loras")).unwrap();
        std::fs::create_dir_all(&comfyui).unwrap();
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(comfyui.join("models"))
            .arg(&real)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return; // 不能建 junction 的環境就不測
        }
        assert!(same(&real.join("loras"), &comfyui.join("models").join("loras")));
        assert!(!same(&real.join("loras"), &comfyui.join("models")));
        let out = save(&d.join("data"), &comfyui, vec![Dir { kind: "loras".into(), path: real.join("loras"), source: "".into(), added: "".into() }]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn save_writes_yaml_and_roots() {
        let d = tmp("save");
        let (data, comfyui) = (d.join("data"), d.join("ComfyUI"));
        std::fs::create_dir_all(comfyui.join("models").join("loras")).unwrap();
        std::fs::create_dir_all(d.join("ext")).unwrap();
        assert!(save(&data, &comfyui, vec![Dir { kind: "loras".into(), path: "relative".into(), source: "".into(), added: "".into() }]).is_err());
        let out = save(
            &data,
            &comfyui,
            vec![
                Dir { kind: "unet".into(), path: d.join("ext"), source: "manual".into(), added: "".into() },
                Dir { kind: "diffusion_models".into(), path: d.join("ext"), source: "scan".into(), added: "".into() }, // 重複
                Dir { kind: "loras".into(), path: comfyui.join("models").join("loras"), source: "".into(), added: "".into() }, // 本來就會讀
            ],
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "diffusion_models");
        assert_eq!(load(&data), out);
        assert!(std::fs::read_to_string(yaml_path(&comfyui)).unwrap().contains("diffusion_models: |"));
        assert_eq!(roots("unet"), vec![d.join("ext")]);
        save(&data, &comfyui, vec![]).unwrap();
        assert!(!yaml_path(&comfyui).exists());
        assert!(roots("diffusion_models").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn guess_kinds() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(guess_kind_from_keys(&s(&["lora_unet_blocks_0.lora_down.weight"])), Some("loras"));
        assert_eq!(guess_kind_from_keys(&s(&["model.diffusion_model.x", "first_stage_model.y"])), Some("checkpoints"));
        assert_eq!(guess_kind_from_keys(&s(&["double_blocks.0.img_attn.qkv.weight"])), Some("diffusion_models"));
        assert_eq!(guess_kind_from_keys(&s(&["encoder.conv_in.weight", "decoder.conv_out.weight"])), Some("vae"));
        assert_eq!(guess_kind_from_keys(&s(&["model.layers.0.mlp.up_proj.weight"])), Some("text_encoders"));
        assert_eq!(guess_kind_from_keys(&s(&["lllite_unet_x"])), Some("model_patches"));
        assert_eq!(guess_kind_from_keys(&s(&["something.else"])), None);
    }
}

//! MiniMax H3 影片（2026-10-22）：組 ComfyUI API 節點圖，給「H3 影片」分頁用。設計在 design\H3影片台設計.md。
//! 節點名、輸入名照 ComfyUI comfy_extras\nodes_minimax_h3.py；Autogrow 輸入在 API 格式寫成 `ref_images.ref_image_0`。
//!
//! 模式（玩法牆的每個玩法背後是其中一個）：
//! - t2v 文生、i2v 首幀、fl2v 首尾幀、continue 接續上一支（MiniMaxH3ImageToVideo）
//! - ref2v 多素材參考、keys 多關鍵幀（MiniMaxH3ReferenceToVideo＋AddGuide）
//! - swap 換角色、vfx 特效改片、style 換畫風（原影片接 AddGuide 第 0 格＋對應 LoRA，社群 Alissonerdx 的做法）
//! - control 動作控制（Fun ControlNet Union：參考影片抽骨架／深度／線稿）
//!
//! 所有素材檔名都是 ComfyUI input 裡的檔名（雲端也上傳成同一個名字）。
use crate::graphs::{self, GraphError};
use crate::py::truthy;
use serde_json::{json, Map, Value};
use std::path::Path;

/// 10Eros-Max beta5 TURBO-hybrid int8（無審核、Turbo 已烤進去、8 步；TenStrip/10Eros-Max）
pub const MODEL: &str = "10Eros_Max_h3_TURBO-hybrid_beta5_int8.safetensors";
/// 同一顆的 w4a8 量化版（14GB）：3070 8GB 實測 512×768 5 秒約 200 秒，比 Q3 GGUF 快 1.8 倍、顯存一樣（2026-10-30）
pub const MODEL_W4A8: &str = "10Eros_Max_h3_TURBO-hybrid_beta5_w4a8_14gb_optimized.safetensors";

/// 給小顯存卡的版本（GGUF、w4a8）：沒指定 ff_chunks 就自動切塊
pub fn low_vram_model(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(".gguf") || l.contains("w4a8")
}
pub const TE: &str = "qwen3vl_32b_minimax_h3_nvfp4_awq.safetensors";
pub const VAE: &str = "minimax_h3_video_vae_int8_convrot.safetensors";
pub const AVAE: &str = "minimax_h3_audio_vae_fp32.safetensors";
/// 二采用的 3D 潛空間放大（HF LBH-123-AI/Minimax_h3_latent_Upscaler 的 3d_conv_v1；YZ金魚工作流用的是它的舊檔名），放 models\latent_upscale_models
pub const UPSCALER: &str = "minimax_h3_latent_upscaler_3d_conv_v1_fp16.safetensors";
/// 官方 ref2va（社群的換角色、特效、換畫風、動態桌布 LoRA 都是在它上面練的），沒有 Turbo：自動加 DMD 8 步 Turbo LoRA
pub const OFFICIAL_REF2VA: &str = "minimax_h3_ref2va_pruned_int8_convrot.safetensors";
pub const TURBO_REF2VA: &str = "minimax_h3_dmd_ref2va_8step_turbo_pruned.safetensors";
pub const FPS: i64 = 24;
/// 舊玩法＋素材板（board：任何素材組合，見 design\H3素材板_設計.md）
pub const MODES: &[&str] = &["t2v", "i2v", "fl2v", "ref2v", "keys", "continue", "swap", "vfx", "style", "live", "control", "board"];
pub const MAX_IMAGES: usize = 9;
pub const MAX_VIDEOS: usize = 3;
pub const MAX_AUDIOS: usize = 3;
pub const MAX_REFS: usize = 12;
/// 接續上一支：取最後幾格當第 0 格的引導（17k+5：22＝約 0.9 秒）
pub const CONTINUE_FRAMES: i64 = 22;
/// 二采：放大倍數、多跑幾步
const UPSCALE: f64 = 1.5;
const REFINE_STEPS: i64 = 4;

/// 社群 LoRA（HF Alissonerdx/Minimax-H3-ComfyUI）：(模式, 檔名, 觸發詞)
pub const MODE_LORAS: &[(&str, &str, &str)] = &[
    ("swap", "minimax_h3_character_swap_v1.1_ref2va_fro.safetensors", "body_swap:"),
    ("vfx", "minimax_h3_vfx_edit_v1.0_r128.safetensors", "vfx_edit:"),
    ("style", "minimax_h3_style_transfer_v1.0_r64.safetensors", "style_transfer:"),
    ("live", "minimax_h3_live_wallpaper_v1.0_ref2va_r64.safetensors", "live_wallpaper:"),
];
/// Fun ControlNet Union（Comfy-Org/MiniMax-H3 model_patches）
pub const CONTROL_PATCH: &str = "minimax_h3_fun_controlnet_union_pruned_int8_convrot.safetensors";

type R<T> = Result<T, GraphError>;
fn err<T>(m: impl Into<String>) -> R<T> {
    Err(GraphError(m.into()))
}

fn s<'a>(p: &'a Value, k: &str) -> &'a str {
    p.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("")
}
fn num(p: &Value, k: &str) -> Option<f64> {
    match p.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
}
fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect()
}

/// 秒數 → 格數：H3 只吃 17k＋5（5、10、15 秒＝124、243、362），夾在 1～15 秒
pub fn frames(secs: f64) -> i64 {
    let secs = if secs.is_finite() { secs.clamp(1.0, 15.0) } else { 5.0 };
    let n = ((secs * FPS as f64).round() as i64).max(5);
    n + (5 - n % 17).rem_euclid(17)
}

/// 畫質 → 像素數（跟 ComfyUI ResolutionSelector 一樣用 1024×1024 當 1MP：high 0.98＝16:9 的 1344×768）
pub fn quality_pixels(q: &str) -> f64 {
    let mp = match q {
        "fast" => 0.4,
        "high" => 0.98,
        _ => 0.6,
    };
    mp * 1_048_576.0
}

fn aspect_wh(a: &str) -> (i64, i64) {
    match a {
        "9:16" => (9, 16),
        "1:1" => (1, 1),
        "4:3" => (4, 3),
        "3:4" => (3, 4),
        "21:9" => (21, 9),
        _ => (16, 9),
    }
}

/// 畫布大小：有首幀（或原影片）就照它的比例，不然照比例選項；對齊 32
pub fn canvas(aspect: &str, quality: &str, from: Option<(i64, i64)>) -> (i64, i64) {
    let (w, h) = from.filter(|(w, h)| *w > 0 && *h > 0).unwrap_or_else(|| aspect_wh(aspect));
    graphs::fit_pixels(w, h, quality_pixels(quality), 32)
}

/// 提示詞裡提到、卻沒有對應素材的標籤（<Picture N>／<Video N>／<Audio N>）。
/// 編號照 ComfyUI：Picture＝圖的順序；Audio 先算配了原聲的影片（照影片順序）再算單獨的聲音
pub fn missing_refs(prompt: &str, n_img: usize, n_vid: usize, n_aud: usize) -> Vec<String> {
    let re = regex::Regex::new(r"<(Picture|Video|Audio) (\d+)>").unwrap();
    let mut out: Vec<String> = vec![];
    for c in re.captures_iter(prompt) {
        let n: usize = c[2].parse().unwrap_or(0);
        let have = match &c[1] {
            "Picture" => n_img,
            "Video" => n_vid,
            _ => n_aud,
        };
        let tag = c[0].to_string();
        if (n == 0 || n > have) && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

struct G {
    nodes: Map<String, Value>,
    n: usize,
}

impl G {
    fn add(&mut self, cls: &str, inputs: Value) -> Value {
        self.n += 1;
        let id = self.n.to_string();
        self.nodes.insert(id.clone(), json!({"class_type": cls, "inputs": inputs}));
        json!([id, 0])
    }
    fn out(r: &Value, i: i64) -> Value {
        json!([r[0].clone(), i])
    }
}

/// 參考素材（多素材、多關鍵幀、換角色…）
#[derive(Default)]
struct Refs {
    images: Vec<String>,
    videos: Vec<String>,
    video_audio: Vec<bool>,
    audios: Vec<String>,
}

impl Refs {
    fn from(p: &Value) -> Refs {
        let r = &p["refs"];
        let videos = strs(&r["videos"]);
        let flags: Vec<bool> = r["video_audio"].as_array().map(|a| a.iter().map(|v| truthy(Some(v))).collect()).unwrap_or_default();
        Refs {
            images: strs(&r["images"]),
            video_audio: (0..videos.len()).map(|i| flags.get(i).copied().unwrap_or(false)).collect(),
            videos,
            audios: strs(&r["audios"]),
        }
    }
    /// <Audio N> 的總數：配了原聲的影片＋單獨的聲音
    fn audio_count(&self) -> usize {
        self.video_audio.iter().filter(|b| **b).count() + self.audios.len()
    }
    fn check(&self) -> R<()> {
        if self.images.len() > MAX_IMAGES {
            return err(format!("參考圖最多 {MAX_IMAGES} 張（現在 {} 張）", self.images.len()));
        }
        if self.videos.len() > MAX_VIDEOS {
            return err(format!("參考影片最多 {MAX_VIDEOS} 段（現在 {} 段）", self.videos.len()));
        }
        if self.audios.len() > MAX_AUDIOS {
            return err(format!("參考聲音最多 {MAX_AUDIOS} 段（現在 {} 段）", self.audios.len()));
        }
        let total = self.images.len() + self.videos.len() + self.audios.len();
        if total > MAX_REFS {
            return err(format!("參考素材合計最多 {MAX_REFS} 個（現在 {total} 個）"));
        }
        if !self.audios.is_empty() && self.images.is_empty() && self.videos.is_empty() {
            return err("參考聲音不能單獨用：至少再放一張圖或一段影片");
        }
        Ok(())
    }
}

/// 一支片要用到的素材：舊玩法照模式填、素材板（board）照 items 填；build 照它組節點圖
#[derive(Default)]
struct Plan {
    refs: Refs,
    /// 走參考路（MiniMaxH3ReferenceToVideo）；不然走關鍵幀路（MiniMaxH3ImageToVideo）
    uses_refs: bool,
    /// 關鍵幀路的首幀、尾幀
    first: String,
    last: String,
    /// 參考路釘在某一格的圖（圖也放進 refs.images，提示詞才提得到）：(檔名, 秒；-1＝最後一格)
    pins: Vec<(String, f64)>,
    /// 原封不動釘進去的聲音：(檔名, 秒)
    audio_pins: Vec<(String, f64)>,
    /// 接續：上一支的尾段當開頭
    prev: String,
    continue_audio: bool,
    /// 整段改造：原影片整段當引導
    source: String,
    keep_audio: bool,
    /// 骨架控制的影片
    control: String,
    /// 社群 LoRA（swap／vfx／style／live）
    lora_mode: String,
    /// 參考圖照原尺寸（動態桌布）
    max_size: bool,
}

/// 素材板一格：{kind: image|video|audio, name, use, secs?, audio?, lora?}
fn board_plan(p: &Value) -> R<Plan> {
    let mut pl = Plan { lora_mode: s(p, "lora_mode").to_string(), ..Default::default() };
    // 圖照素材板順序：(檔名, 釘在第幾秒)
    let mut images: Vec<(String, Option<f64>)> = vec![];
    let mut special = 0;
    for it in p["items"].as_array().into_iter().flatten() {
        let name = s(it, "name").to_string();
        if name.is_empty() {
            continue;
        }
        let audio = truthy(it.get("audio"));
        match (s(it, "kind"), s(it, "use")) {
            ("image", "ref") => images.push((name, None)),
            ("image", "anchor") => images.push((name, Some(num(it, "secs").unwrap_or(0.0)))),
            ("video", "ref") => {
                pl.refs.videos.push(name);
                pl.refs.video_audio.push(audio);
            }
            ("video", "continue") => {
                (pl.prev, pl.continue_audio, special) = (name, audio, special + 1);
            }
            ("video", "guide") => {
                (pl.source, pl.keep_audio, special) = (name, audio, special + 1);
                let l = s(it, "lora");
                if !l.is_empty() {
                    pl.lora_mode = l.to_string();
                }
            }
            ("video", "control") => {
                (pl.control, special) = (name, special + 1);
            }
            ("audio", "ref") => pl.refs.audios.push(name),
            ("audio", "anchor") => pl.audio_pins.push((name, num(it, "secs").unwrap_or(0.0))),
            (k, u) => return err(format!("素材板看不懂這一格：{k}／{u}")),
        }
    }
    if special > 1 {
        return err("「接著拍」「整段改造」「骨架控制」的影片合起來只能放一段");
    }
    if !pl.lora_mode.is_empty() && !MODE_LORAS.iter().any(|(m, ..)| *m == pl.lora_mode) {
        return err(format!("沒有這個改造方式：{}", pl.lora_mode));
    }
    if pl.lora_mode == "live" {
        pl.max_size = true;
    }
    let pinned = |t: f64| images.iter().filter(|(_, x)| *x == Some(t)).count();
    if pinned(0.0) > 1 || pinned(-1.0) > 1 {
        return err("同一個時間點只能釘一張圖");
    }
    if images.iter().filter(|(_, t)| t.is_some()).count() > 6 {
        return err("釘在時間點的圖最多 6 張");
    }
    if pl.audio_pins.len() > MAX_AUDIOS {
        return err(format!("原封不動放進去的聲音最多 {MAX_AUDIOS} 段"));
    }
    if !pl.prev.is_empty() && pinned(0.0) > 0 {
        return err("「接著上一支拍」的開頭已經是上一支的畫面：第 0 秒不能再釘圖");
    }
    // 有參考素材、或釘在首尾以外的圖 → 參考路：所有圖照順序當參考，釘的再用 AddGuide 釘住
    let middle = images.iter().any(|(_, t)| t.is_some_and(|t| t != 0.0 && t != -1.0));
    let any_ref = images.iter().any(|(_, t)| t.is_none()) || !pl.refs.videos.is_empty() || !pl.refs.audios.is_empty();
    pl.uses_refs = any_ref || middle || !pl.lora_mode.is_empty() || !pl.control.is_empty();
    if pl.uses_refs {
        for (n, t) in images {
            if let Some(t) = t {
                pl.pins.push((n.clone(), t));
            }
            pl.refs.images.push(n);
        }
    } else {
        for (n, t) in images {
            if t == Some(0.0) {
                pl.first = n;
            } else {
                pl.last = n;
            }
        }
    }
    match pl.lora_mode.as_str() {
        "swap" if pl.refs.images.is_empty() => return err("換角色要放一張角色圖"),
        "swap" | "vfx" | "style" if pl.source.is_empty() => return err("換角色、加特效、換畫風要放一段原影片（用途選「整段改造」）"),
        "live" if pl.refs.images.is_empty() => return err("動態桌布要放 1～3 張圖"),
        "live" if pl.refs.images.len() > 3 => return err("動態桌布最多 3 張圖"),
        _ => {}
    }
    Ok(pl)
}

/// 舊玩法（一個模式一組固定的格子）→ Plan
fn legacy_plan(p: &Value, mode: &str, length: i64) -> R<Plan> {
    let mut pl = Plan { refs: Refs::from(p), ..Default::default() };
    let (first, last, source, prev) = (s(p, "first"), s(p, "last"), s(p, "source"), s(p, "prev"));
    let keys: Vec<(String, f64)> = p["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| {
            let img = k["image"].as_str().map(str::trim).filter(|x| !x.is_empty())?;
            Some((img.to_string(), num(k, "secs").unwrap_or(0.0)))
        })
        .collect();
    match mode {
        "i2v" | "fl2v" if first.is_empty() => return err("這個玩法要一張首幀圖"),
        "fl2v" if last.is_empty() => return err("首尾幀要再放一張尾幀圖"),
        "continue" if prev.is_empty() => return err("接續要放上一支影片"),
        "keys" if keys.len() < 2 => return err("多關鍵幀至少要兩張圖，每張填它在第幾秒"),
        "swap" | "vfx" | "style" | "control" if source.is_empty() => return err("這個玩法要放一段原影片"),
        "swap" if pl.refs.images.is_empty() => return err("換角色要放一張角色圖（會變成 <Picture 1>）"),
        "live" if pl.refs.images.is_empty() => return err("動態桌布要放 1～3 張圖"),
        "live" if pl.refs.images.len() > 3 => return err("動態桌布最多 3 張圖"),
        _ => {}
    }
    if mode == "keys" {
        let max_t = length as f64 / FPS as f64;
        // 用跟 AddGuide 一樣的「四捨五入成第幾格」來檢查
        if let Some((_, t)) = keys.iter().find(|(_, t)| *t < 0.0 || (t * FPS as f64).round() as i64 >= length) {
            return err(format!("關鍵幀的秒數 {t} 超出影片長度（0～{max_t:.1} 秒）"));
        }
        // 關鍵幀也要放進 ref_images，提示詞才提得到它（官方 Multiframe 範本的坑）；
        // 同一張圖用兩次也照放：網頁的 <Picture N> 是一格一號
        pl.refs.images.extend(keys.iter().map(|(img, _)| img.clone()));
        pl.pins = keys;
    }
    pl.uses_refs = matches!(mode, "ref2v" | "keys" | "swap" | "vfx" | "style" | "live" | "control");
    pl.first = first.to_string();
    pl.last = if mode == "fl2v" { last.to_string() } else { String::new() };
    if mode == "continue" {
        pl.prev = prev.to_string();
        pl.continue_audio = truthy(p.get("continue_audio"));
    }
    match mode {
        "swap" | "vfx" | "style" => {
            pl.source = source.to_string();
            pl.keep_audio = truthy(p.get("keep_audio"));
        }
        "control" => pl.control = source.to_string(),
        _ => {}
    }
    if MODE_LORAS.iter().any(|(m, ..)| *m == mode) {
        pl.lora_mode = mode.to_string();
    }
    pl.max_size = mode == "live" || s(p, "ref_image_size") == "max";
    Ok(pl)
}

/// 秒 → AddGuide 的第幾格（-1＝最後一格）；超出影片長度就擋
fn pin_frame(t: f64, length: i64) -> R<i64> {
    if t == -1.0 {
        return Ok(-1);
    }
    let idx = (t * FPS as f64).round() as i64;
    if t < 0.0 || idx >= length {
        return err(format!("釘的時間 {t} 秒超出影片長度（0～{:.1} 秒）", length as f64 / FPS as f64));
    }
    Ok(idx)
}

/// 組好的節點圖（nodes, info），跟 graphs::build 一樣。p 的欄位見 design\H3影片台_實作計畫.md Task 1；素材板見 design\H3素材板_設計.md
pub fn build(p: &Value, input_dir: &Path, seed_offset: i64) -> R<(Map<String, Value>, Value)> {
    let mode = match s(p, "h3_mode") {
        "" => "t2v",
        m => m,
    };
    if !MODES.contains(&mode) {
        return err(format!("沒有這個玩法：{mode}"));
    }
    let mut prompt = s(p, "prompt").to_string();
    if prompt.is_empty() {
        return err("要寫提示詞：描述畫面、動作和聲音（選了玩法會自動帶範本）");
    }
    let secs = num(p, "seconds").unwrap_or(5.0);
    let length = frames(secs);
    let pl = if mode == "board" { board_plan(p)? } else { legacy_plan(p, mode, length)? };
    let lora_trigger = MODE_LORAS.iter().find(|(m, ..)| *m == pl.lora_mode);
    if let Some((_, _, trig)) = lora_trigger {
        if !prompt.starts_with(trig) {
            prompt = format!("{trig} {prompt}");
        }
    }
    let two_pass = truthy(p.get("two_pass"));
    let guided = !pl.first.is_empty() || !pl.last.is_empty() || !pl.pins.is_empty() || !pl.audio_pins.is_empty()
        || !pl.prev.is_empty() || !pl.source.is_empty() || !pl.control.is_empty() || !pl.lora_mode.is_empty();
    if two_pass && (if mode == "board" { guided } else { !matches!(mode, "t2v" | "ref2v") }) {
        return err("這個玩法還不能用高畫質二采（目前只有文生影片、多素材參考可以）：把「高畫質二采」取消勾選");
    }
    let selflift = truthy(p.get("selflift"));
    if selflift && two_pass {
        return err("「SelfLift 兩段取樣」跟「高畫質二采」只能選一個");
    }
    let seed = num(p, "seed").map(|x| x as i64).unwrap_or(0) + seed_offset;
    let quality = s(p, "quality");
    let refmods = crate::refmod::names_of(p).or_else(err)?;
    let refs = &pl.refs;
    if pl.uses_refs {
        refs.check()?;
        let miss = missing_refs(&prompt, refs.images.len(), refs.videos.len(), refs.audio_count());
        if !miss.is_empty() {
            return err(format!("提示詞提到 {}，素材格子裡卻沒有：補上素材或改提示詞", miss.join("、")));
        }
    }
    let pin_idx: Vec<(String, i64)> = pl.pins.iter().map(|(n, t)| Ok((n.clone(), pin_frame(*t, length)?))).collect::<R<_>>()?;
    let audio_idx: Vec<(String, i64)> = pl.audio_pins.iter().map(|(n, t)| Ok((n.clone(), pin_frame(*t, length)?))).collect::<R<_>>()?;

    // 畫布：首幀 → 第一張釘圖（第 0 秒的優先）→ 原影片尺寸（網頁量好帶 src_wh）→ 比例選項
    let pin0 = pl.pins.iter().find(|(_, t)| *t == 0.0).or(pl.pins.first()).map(|(n, _)| n.as_str());
    let from = if !pl.first.is_empty() {
        Some(graphs::image_size(&input_dir.join(&pl.first))?)
    } else if let Some(n) = pin0 {
        Some(graphs::image_size(&input_dir.join(n))?)
    } else if !pl.last.is_empty() {
        Some(graphs::image_size(&input_dir.join(&pl.last))?)
    } else {
        p["src_wh"].as_array().and_then(|a| Some((a.first()?.as_i64()?, a.get(1)?.as_i64()?)))
    };
    let (w, h, final_wh) = if two_pass {
        let (fw, fh) = from.unwrap_or_else(|| aspect_wh(s(p, "aspect")));
        let (w, h) = graphs::fit_pixels(fw, fh, quality_pixels(quality) / (UPSCALE * UPSCALE), 32);
        // 成品＝一采放大 1.5 倍（放大節點 align 32）
        let up = |v: i64| ((v as f64 * UPSCALE / 32.0).round() as i64) * 32;
        (w, h, (up(w), up(h)))
    } else {
        let (w, h) = canvas(s(p, "aspect"), quality, from);
        (w, h, (w, h))
    };

    let mut g = G { nodes: Map::new(), n: 0 };
    let model_name = match s(p, "model") {
        "" => MODEL,
        m => m,
    };
    // GGUF 量化版（顯存小的卡）走 ComfyUI-GGUF 的載入節點
    let mut model = if model_name.to_lowercase().ends_with(".gguf") {
        g.add("UnetLoaderGGUF", json!({"unet_name": model_name}))
    } else {
        g.add("UNETLoader", json!({"unet_name": model_name, "weight_dtype": "default"}))
    };
    let mut loras: Vec<(String, f64)> = p["loras"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let n = l["name"].as_str().map(str::trim).filter(|x| !x.is_empty())?;
            Some((n.to_string(), num(l, "strength").unwrap_or(1.0)))
        })
        .collect();
    let has = |loras: &[(String, f64)], f: &str| loras.iter().any(|(n, _)| basename(n) == f);
    if let Some((_, file, _)) = lora_trigger {
        if !has(&loras, file) {
            loras.insert(0, (file.to_string(), 1.0));
        }
    }
    // 官方 ref2va 沒有 Turbo：8 步以內自動補 DMD Turbo LoRA（10Eros 已經烤進去，不用）
    let steps = num(p, "steps").map(|x| x as i64).filter(|x| (1..=100).contains(x)).unwrap_or(8);
    if selflift && steps < 4 {
        return err("SelfLift 兩段取樣至少要 4 步（前一半低解析度、後一半原解析度）");
    }
    if basename(model_name) == OFFICIAL_REF2VA && steps <= 10 && !truthy(p.get("no_turbo")) && !has(&loras, TURBO_REF2VA) {
        loras.insert(0, (TURBO_REF2VA.to_string(), 1.0));
    }
    for (name, st) in &loras {
        model = g.add("LoraLoaderModelOnly", json!({"model": model, "lora_name": name, "strength_model": st}));
    }
    // 加速（照 YZ金鱼工作流的順序）：INT8 注意力＋稀疏注意力（sol-attn），都是 ComfyUI 內建節點；
    // token 少於 min_tokens（低解析度、短片）時稀疏注意力不會動作
    if truthy(p.get("speedup")) {
        model = g.add("ModelAttentionBackend", json!({"model": model, "attention": "comfy kitchen attention"}));
        model = g.add(
            "BlockSparseAttention",
            json!({"model": model, "selection": "sol-attn", "selection.tau": 1.3, "start_percent": 0.2, "end_percent": 1.0, "dense_blocks": "",
                   "min_tokens": 12288, "extra_tokens": 256, "sink_conditioning": "exact_kv_and_rows", "verbose": false}),
        );
    }
    if let (Some(v), a) = (num(p, "shift_video"), num(p, "shift_audio")) {
        model = g.add("MiniMaxH3SigmaShift", json!({"model": model, "shift_video": v, "shift_audio": a.unwrap_or(3.0)}));
    }
    // 顯存小的卡：feedforward 切塊算，峰值顯存變低、結果一樣（MiniMaxChunkFeedForward，節點在 ComfyUI-KJNodes，不是內建）。
    // GGUF、w4a8 本來就是給小顯存用的：沒指定就切 8（3070 8GB 實測過）；ff_chunks 1＝不切
    let chunks = num(p, "ff_chunks").map(|x| x as i64).or(low_vram_model(model_name).then_some(8));
    if let Some(c) = chunks.filter(|c| (2..=64).contains(c)) {
        model = g.add("MiniMaxChunkFeedForward", json!({"model": model, "chunks": c, "seq_threshold": 4096}));
    }
    let clip = g.add("CLIPLoader", json!({"clip_name": TE, "type": "minimax", "device": "default"}));
    let vae = g.add("VAELoader", json!({"vae_name": VAE}));
    let avae = g.add("VAELoader", json!({"vae_name": AVAE}));

    if !pl.control.is_empty() {
        let patch = g.add("ModelPatchLoader", json!({"name": CONTROL_PATCH}));
        let src = g.add("LoadVideo", json!({"file": pl.control}));
        let comp = g.add("GetVideoComponents", json!({"video": src}));
        // 截到要做的長度：前處理（SDPose）不用白跑整段
        let cut = g.add("ImageFromBatch", json!({"image": G::out(&comp, 0), "batch_index": 0, "length": length}));
        let ctl = control_video(&mut g, p, cut);
        let strength = num(p, "control_strength").unwrap_or(1.0);
        model = g.add(
            "MiniMaxH3FunControlNetApply",
            json!({"model": model, "model_patch": patch, "vae": vae, "strength": strength, "start_percent": 0.0, "end_percent": 1.0, "control_video": ctl}),
        );
    }

    // 條件＋空白潛空間
    let (mut positive, latent) = if pl.uses_refs {
        let size = if pl.max_size { "max" } else { "match" };
        let mut inputs = json!({"clip": clip, "vae": vae, "audio_vae": avae, "prompt": prompt, "width": w, "height": h, "length": length, "ref_image_size": size});
        for (i, img) in refs.images.iter().enumerate() {
            let li = g.add("LoadImage", json!({"image": img}));
            inputs[format!("ref_images.ref_image_{i}")] = li;
        }
        for (i, v) in refs.videos.iter().enumerate() {
            let lv = g.add("LoadVideo", json!({"file": v}));
            let comp = g.add("GetVideoComponents", json!({"video": lv}));
            inputs[format!("ref_videos.ref_video_{i}")] = G::out(&comp, 0);
            if refs.video_audio[i] {
                inputs[format!("ref_video_audios.ref_video_audio_{i}")] = G::out(&comp, 1);
            }
        }
        for (i, a) in refs.audios.iter().enumerate() {
            let la = g.add("LoadAudio", json!({"audio": a}));
            inputs[format!("ref_audios.ref_audio_{i}")] = la;
        }
        let c = g.add("MiniMaxH3ReferenceToVideo", inputs);
        (G::out(&c, 0), G::out(&c, 1))
    } else {
        let mut inputs = json!({"clip": clip, "vae": vae, "prompt": prompt, "width": w, "height": h, "length": length});
        if !pl.first.is_empty() {
            inputs["first_frame"] = g.add("LoadImage", json!({"image": pl.first}));
        }
        if !pl.last.is_empty() {
            inputs["last_frame"] = g.add("LoadImage", json!({"image": pl.last}));
        }
        let c = g.add("MiniMaxH3ImageToVideo", inputs);
        (G::out(&c, 0), G::out(&c, 1))
    };

    // AddGuide：釘住的圖、聲音、接續、原影片
    let guide = |g: &mut G, positive: Value, image: Option<Value>, audio: Option<Value>, idx: i64| {
        let mut inputs = json!({"positive": positive, "latent": latent, "vae": vae, "audio_vae": avae, "frame_idx": idx});
        if let Some(im) = image {
            inputs["image"] = im;
        }
        if let Some(a) = audio {
            inputs["audio"] = a;
        }
        g.add("MiniMaxH3AddGuide", inputs)
    };
    for (img, idx) in &pin_idx {
        let li = g.add("LoadImage", json!({"image": img}));
        positive = guide(&mut g, positive, Some(li), None, *idx);
    }
    for (a, idx) in &audio_idx {
        let la = g.add("LoadAudio", json!({"audio": a}));
        positive = guide(&mut g, positive, None, Some(la), *idx);
    }
    if !pl.prev.is_empty() {
        let lv = g.add("LoadVideo", json!({"file": pl.prev}));
        let comp = g.add("GetVideoComponents", json!({"video": lv}));
        let tail = g.add("ImageFromBatch", json!({"image": G::out(&comp, 0), "batch_index": -CONTINUE_FRAMES, "length": CONTINUE_FRAMES}));
        // 聲音只取上一段最後 CONTINUE_FRAMES 格那一截，對齊 tail；整條貼上去會從 0 秒重播上一段（連念錯的台詞一起傳下去）
        let secs = CONTINUE_FRAMES as f64 / FPS as f64;
        let audio = pl.continue_audio.then(|| g.add("TrimAudioDuration", json!({"audio": G::out(&comp, 1), "start_index": -secs, "duration": secs})));
        positive = guide(&mut g, positive, Some(tail), audio, 0);
    }
    if !pl.source.is_empty() {
        let lv = g.add("LoadVideo", json!({"file": pl.source}));
        let comp = g.add("GetVideoComponents", json!({"video": lv}));
        // 原影片當引導：先截到要做的格數（AddGuide 的引導比影片長會直接報錯）；聲音保留原片的（聲音 AddGuide 自己會截）
        let cut = g.add("ImageFromBatch", json!({"image": G::out(&comp, 0), "batch_index": 0, "length": length}));
        let audio = pl.keep_audio.then(|| G::out(&comp, 1));
        positive = guide(&mut g, positive, Some(cut), audio, 0);
    }

    // SelfLift 只接受 Euler
    let sampler = match s(p, "sampler") {
        _ if selflift => "euler",
        "" => "res_multistep",
        x => x,
    };
    let scheduler = match s(p, "scheduler") {
        "" => "simple",
        x => x,
    };
    // RefMod（角色卡正面特寫做的，refmod.rs）：接在所有 AddGuide 之後、guider 之前，一般取樣、二采、SelfLift 都吃同一份條件
    if !refmods.is_empty() {
        let mods = g.add("MiniMaxH3RefModsLoader", crate::refmod::loader_inputs(&refmods));
        positive = G::out(&g.add("MiniMaxH3RefModApply", crate::refmod::apply_inputs(positive, G::out(&mods, 0))), 0);
    }
    let noise = g.add("RandomNoise", json!({"noise_seed": seed}));
    let guider = g.add("BasicGuider", json!({"model": model, "conditioning": positive}));
    let ks = g.add("KSamplerSelect", json!({"sampler_name": sampler}));
    let total = if two_pass { steps + REFINE_STEPS } else { steps };
    let sched = g.add("BasicScheduler", json!({"model": model, "scheduler": scheduler, "steps": total, "denoise": 1.0}));
    let samples = if two_pass {
        let split = g.add("SplitSigmas", json!({"sigmas": sched, "step": steps}));
        let a = g.add("SamplerCustomAdvanced", json!({"noise": noise, "guider": guider, "sampler": ks, "sigmas": G::out(&split, 0), "latent_image": latent}));
        let sep = g.add("LTXVSeparateAVLatent", json!({"av_latent": a}));
        let up = g.add(
            "MinimaxH3LatentUpscaler3D",
            // 這顆節點不同版本的欄位名不一樣（YZ 存的是 mode.scale／enable_chunking，新版 README 是 scale／enable_temporal_chunking）：
            // 兩種都填，ComfyUI 會略過不認得的欄位
            json!({"latent": G::out(&sep, 0), "model_name": UPSCALER, "mode": "scale by multiplier", "mode.scale": UPSCALE, "scale": UPSCALE, "align": 32,
                   "device": "cuda", "precision": "fp16", "enable_chunking": false, "enable_temporal_chunking": true, "force_unload": false}),
        );
        let cat = g.add("LTXVConcatAVLatent", json!({"video_latent": up, "audio_latent": G::out(&sep, 1)}));
        g.add("SamplerCustomAdvanced", json!({"noise": noise, "guider": guider, "sampler": ks, "sigmas": G::out(&split, 1), "latent_image": cat}))
    } else if selflift {
        // SelfLiftH3Sampler（rogen84）：前半步數在 0.6 倍解析度算，放大後後半步數在原解析度收尾；值照 YZ金鱼工作流
        let neg = g.add("ConditioningZeroOut", json!({"conditioning": positive}));
        let sl = g.add(
            "SelfLiftH3Sampler",
            json!({"model": model, "positive": positive, "negative": neg, "vae": vae, "latent_image": latent, "sampler": ks, "sigmas": sched,
                   "seed": seed, "cfg": 1.0, "transition_step": (steps / 2).max(1), "lowres_scale": 0.6, "rho": 0.0, "w_min": 0.5, "w_max": 1.0,
                   "upscaler_model": UPSCALER, "highres_tiling": false}),
        );
        G::out(&sl, 0)
    } else {
        g.add("SamplerCustomAdvanced", json!({"noise": noise, "guider": guider, "sampler": ks, "sigmas": sched, "latent_image": latent}))
    };
    let images = g.add("VAEDecode", json!({"samples": samples, "vae": vae}));
    let audio = g.add("VAEDecodeAudio", json!({"samples": samples, "vae": avae}));
    let video = g.add("CreateVideo", json!({"images": images, "fps": FPS as f64, "audio": audio}));
    g.add("SaveVideo", json!({"video": video, "filename_prefix": "AnimaWeb/video", "format": "auto", "format.codec": "auto"}));
    if let Some(poster) = p.get("poster").and_then(Value::as_str) {
        let one = g.add("ImageFromBatch", json!({"image": images, "batch_index": 0, "length": 1}));
        g.add("SaveImage", json!({"images": one, "filename_prefix": format!("AnimaWeb/_poster/{poster}")}));
        if truthy(p.get("save_last")) {
            let last = g.add("ImageFromBatch", json!({"image": images, "batch_index": -1, "length": 1}));
            g.add("SaveImage", json!({"images": last, "filename_prefix": format!("AnimaWeb/_poster/{poster}-last")}));
        }
    }
        let (fw, fh) = final_wh;
    let info = json!({"prompt": prompt, "negative": "", "seed": seed, "width": fw, "height": fh, "mode": "video", "family": "h3",
                      "h3_mode": mode, "frames": length, "fps": FPS, "preview": false, "two_pass": two_pass, "selflift": selflift, "model": model_name});
    Ok((g.nodes, info))
}

/// 動作控制的前處理：pose（SDPose＋RT-DETR，照官方 video_minimax_h3_fun_controlnet_union 範本）、canny（核心 Canny 節點）、
/// none（直接用，例如已經是骨架或深度影片）。欄位名照 comfy_extras 的 nodes_sdpose.py、nodes_rtdetr.py、nodes_canny.py
fn control_video(g: &mut G, p: &Value, frames: Value) -> Value {
    match s(p, "control_type") {
        "canny" => g.add("Canny", json!({"image": frames, "low_threshold": 0.4, "high_threshold": 0.8})),
        "none" => frames,
        _ => {
            let pose = g.add("CheckpointLoaderSimple", json!({"ckpt_name": "sdpose_wholebody_fp16.safetensors"}));
            let det = g.add("UNETLoader", json!({"unet_name": "rt_detr_v4-x-hgnet_fp16.safetensors", "weight_dtype": "default"}));
            let boxes = g.add("RTDETR_detect", json!({"model": det, "image": frames, "threshold": 0.5, "class_name": "person", "max_detections": 2}));
            let kp = g.add(
                "SDPoseKeypointExtractor",
                json!({"model": G::out(&pose, 0), "vae": G::out(&pose, 2), "image": frames, "batch_size": 16, "bboxes": boxes}),
            );
            g.add(
                "SDPoseDrawKeypoints",
                json!({"keypoints": kp, "draw_body": true, "draw_hands": true, "draw_face": true, "draw_feet": true, "stick_width": 4,
                       "face_point_size": 2, "score_threshold": 0.5, "draw_head": true}),
            )
        }
    }
}

fn basename(n: &str) -> &str {
    n.rsplit(['/', '\\']).next().unwrap_or(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_class<'a>(n: &'a Map<String, Value>, c: &str) -> Vec<&'a Value> {
        n.values().filter(|v| v["class_type"] == c).collect()
    }
    /// 測試圖只準備一次：測試是平行跑的，各自「沒有就寫」會讀到別人寫到一半的檔
    fn dir() -> std::path::PathBuf {
        static D: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        D.get_or_init(|| {
            let d = std::env::temp_dir().join(format!("anima_h3_test_{}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            for (name, w, h) in [("a.png", 1344, 768), ("b.png", 1344, 768), ("tall.png", 1000, 1500)] {
                image::RgbImage::new(w, h).save(d.join(name)).unwrap();
            }
            d
        })
        .clone()
    }
    fn node<'a>(n: &'a Map<String, Value>, r: &Value) -> &'a Value {
        &n[r[0].as_str().unwrap()]
    }

    #[test]
    fn frames_grid() {
        assert_eq!(frames(5.0), 124);
        assert_eq!(frames(10.0), 243);
        assert_eq!(frames(15.0), 362);
        assert_eq!(frames(20.0), 362);
        assert_eq!(frames(0.1), 39);
        assert_eq!(frames(f64::NAN), 124);
        for s in 1..=15 {
            assert_eq!(frames(s as f64) % 17, 5, "{s} 秒");
        }
    }

    #[test]
    fn canvas_sizes() {
        assert_eq!(canvas("16:9", "high", None), (1344, 768));
        assert_eq!(canvas("16:9", "fast", None), (864, 480));
        assert_eq!(canvas("16:9", "std", None), (1056, 608));
        let (w, h) = canvas("16:9", "std", Some((1000, 1500)));
        assert!(h > w && ((w as f64 / h as f64) - 0.667).abs() < 0.03, "{w}x{h}");
        assert_eq!(w % 32 + h % 32, 0);
        let (w, h) = canvas("9:16", "high", None);
        assert_eq!((w, h), (768, 1344));
    }

    #[test]
    fn fl2v_graph() {
        let p = json!({"h3_mode": "fl2v", "prompt": "x", "first": "a.png", "last": "b.png", "seconds": 10, "quality": "high", "poster": "p1", "save_last": true});
        let (n, info) = build(&p, &dir(), 0).unwrap();
        let c = by_class(&n, "MiniMaxH3ImageToVideo");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["inputs"]["length"], 243);
        assert_eq!((c[0]["inputs"]["width"].as_i64(), c[0]["inputs"]["height"].as_i64()), (Some(1344), Some(768)));
        assert_eq!(node(&n, &c[0]["inputs"]["first_frame"])["inputs"]["image"], "a.png");
        assert_eq!(node(&n, &c[0]["inputs"]["last_frame"])["inputs"]["image"], "b.png");
        assert_eq!(by_class(&n, "SaveVideo")[0]["inputs"]["filename_prefix"], "AnimaWeb/video");
        assert_eq!(by_class(&n, "SaveImage").len(), 2);
        assert_eq!(by_class(&n, "UNETLoader")[0]["inputs"]["unet_name"], MODEL);
        let (g, _) = build(&json!({"prompt": "x", "model": "10Eros-Max-Turbo-Beta5-Q3_K_M.gguf"}), &dir(), 0).unwrap();
        assert!(by_class(&g, "UNETLoader").is_empty());
        assert_eq!(by_class(&g, "UnetLoaderGGUF")[0]["inputs"]["unet_name"], "10Eros-Max-Turbo-Beta5-Q3_K_M.gguf");
        assert_eq!(by_class(&g, "MiniMaxChunkFeedForward")[0]["inputs"]["chunks"], 8, "GGUF 沒指定就切 8");
        let (g, _) = build(&json!({"prompt": "x", "model": "a.gguf", "ff_chunks": 1}), &dir(), 0).unwrap();
        assert!(by_class(&g, "MiniMaxChunkFeedForward").is_empty(), "指定 1＝不切");
        let (g, _) = build(&json!({"prompt": "x", "ff_chunks": 8}), &dir(), 0).unwrap();
        assert_eq!(by_class(&g, "MiniMaxChunkFeedForward")[0]["inputs"]["chunks"], 8);
        // w4a8 也是小顯存版：沒指定就切 8；預設 int8 不切
        let (g, _) = build(&json!({"prompt": "x", "model": MODEL_W4A8}), &dir(), 0).unwrap();
        assert_eq!(by_class(&g, "UNETLoader")[0]["inputs"]["unet_name"], MODEL_W4A8);
        assert_eq!(by_class(&g, "MiniMaxChunkFeedForward")[0]["inputs"]["chunks"], 8, "w4a8 沒指定就切 8");
        assert!(by_class(&n, "MiniMaxChunkFeedForward").is_empty(), "int8 預設不切");
        assert_eq!(info["h3_mode"], "fl2v");
        assert_eq!(info["family"], "h3");
        assert_eq!(info["frames"], 243);
        // 少尾幀 → 擋下
        assert!(build(&json!({"h3_mode": "fl2v", "prompt": "x", "first": "a.png"}), &dir(), 0).is_err());
        // t2v 不接圖
        let (n, _) = build(&json!({"prompt": "x"}), &dir(), 0).unwrap();
        assert!(by_class(&n, "MiniMaxH3ImageToVideo")[0]["inputs"].get("first_frame").is_none());
        assert!(by_class(&n, "LoadImage").is_empty());
    }

    #[test]
    fn refmods_feed_the_guider_after_add_guide() {
        let p = json!({"h3_mode": "ref2v", "prompt": "<Picture 1> waves", "refs": {"images": ["a.png"]}, "pins": [{"image": "b.png", "t": 0}],
                       "refmods": ["paintdeck/pd_char_7", "paintdeck/pd_char_9"]});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        let guider = by_class(&n, "BasicGuider")[0];
        let apply = node(&n, &guider["inputs"]["conditioning"]);
        assert_eq!(apply["class_type"], "MiniMaxH3RefModApply");
        let loader = node(&n, &apply["inputs"]["mods"]);
        assert_eq!((loader["inputs"]["mod_1"].as_str(), loader["inputs"]["mod_2"].as_str(), loader["inputs"]["mod_3"].as_str()),
                   (Some("paintdeck/pd_char_7"), Some("paintdeck/pd_char_9"), Some("(none)")));
        let before = node(&n, &apply["inputs"]["conditioning"])["class_type"].as_str().unwrap();
        assert!(matches!(before, "MiniMaxH3AddGuide" | "MiniMaxH3ReferenceToVideo"), "{before}");
        // SelfLift 也吃同一份
        let mut q = p.clone();
        q["selflift"] = json!(true);
        let (n, _) = build(&q, &dir(), 0).unwrap();
        let sl = &by_class(&n, "SelfLiftH3Sampler")[0]["inputs"];
        assert_eq!(node(&n, &sl["positive"])["class_type"], "MiniMaxH3RefModApply");
        // 沒有 RefMod：圖跟以前一樣
        let (n, _) = build(&json!({"prompt": "x"}), &dir(), 0).unwrap();
        assert!(by_class(&n, "MiniMaxH3RefModApply").is_empty() && by_class(&n, "MiniMaxH3RefModsLoader").is_empty());
        assert!(build(&json!({"prompt": "x", "refmods": ["../evil"]}), &dir(), 0).is_err());
    }

    #[test]
    fn ref2v_graph() {
        let p = json!({"h3_mode": "ref2v", "prompt": "<Picture 1> and <Picture 2> dance to <Audio 2>, moving like <Video 1>",
                       "refs": {"images": ["a.png", "b.png"], "videos": ["v.mp4"], "video_audio": [true], "audios": ["s.wav"]}});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        let c = by_class(&n, "MiniMaxH3ReferenceToVideo");
        assert_eq!(c.len(), 1);
        let i = &c[0]["inputs"];
        for k in ["ref_images.ref_image_0", "ref_images.ref_image_1", "ref_videos.ref_video_0", "ref_video_audios.ref_video_audio_0", "ref_audios.ref_audio_0"] {
            assert!(i.get(k).is_some(), "少了 {k}");
        }
        assert_eq!(i["ref_videos.ref_video_0"][1], 0);
        assert_eq!(i["ref_video_audios.ref_video_audio_0"][1], 1);
        assert_eq!(i["ref_image_size"], "match");
        assert!(i.get("ref_images.ref_image_2").is_none());
    }

    #[test]
    fn keys_graph() {
        let p = json!({"h3_mode": "keys", "prompt": "<Picture 1> then <Picture 3>", "seconds": 10,
                       "keys": [{"image": "a.png", "secs": 0}, {"image": "b.png", "secs": 3}, {"image": "tall.png", "secs": 5}]});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        let g = by_class(&n, "MiniMaxH3AddGuide");
        let mut idx: Vec<i64> = g.iter().map(|x| x["inputs"]["frame_idx"].as_i64().unwrap()).collect();
        idx.sort();
        assert_eq!(idx, vec![0, 72, 120]);
        let r = &by_class(&n, "MiniMaxH3ReferenceToVideo")[0]["inputs"];
        assert!(r.get("ref_images.ref_image_2").is_some(), "關鍵幀也要進 ref_images");
        // 串接：guider 接最後一個 AddGuide
        let guider = by_class(&n, "BasicGuider")[0];
        assert_eq!(node(&n, &guider["inputs"]["conditioning"])["class_type"], "MiniMaxH3AddGuide");
        // 秒數超出影片長度
        let bad = json!({"h3_mode": "keys", "prompt": "x", "seconds": 5, "keys": [{"image": "a.png", "secs": 0}, {"image": "b.png", "secs": 9}]});
        assert!(build(&bad, &dir(), 0).is_err());
        // 四捨五入後剛好等於總格數也算超出（審查第 10 點：10 秒＝243 格，10.11 秒 → 第 243 格越界）
        let edge = json!({"h3_mode": "keys", "prompt": "x", "seconds": 10, "keys": [{"image": "a.png", "secs": 0}, {"image": "b.png", "secs": 10.11}]});
        assert!(build(&edge, &dir(), 0).is_err());
        // 同一張圖用兩次：不去重，跟網頁的 <Picture N> 編號一致（審查第 9 點：A→B→A 網頁顯示 <Picture 3>）
        let aba = json!({"h3_mode": "keys", "prompt": "<Picture 1> <Picture 3>", "seconds": 10,
                         "keys": [{"image": "a.png", "secs": 0}, {"image": "b.png", "secs": 3}, {"image": "a.png", "secs": 6}]});
        let (n, _) = build(&aba, &dir(), 0).expect("A→B→A 要能送出");
        assert!(by_class(&n, "MiniMaxH3ReferenceToVideo")[0]["inputs"].get("ref_images.ref_image_2").is_some());
    }

    #[test]
    fn source_cut_to_length() {
        // 原影片比要做的影片長：先截到同樣格數再當引導（AddGuide 引導比影片長會直接報錯；動作控制的前處理也不用白跑整段）
        for mode in ["swap", "vfx", "style"] {
            let p = json!({"h3_mode": mode, "prompt": "<Picture 1> walks", "source": "src.mp4", "refs": {"images": ["a.png"]}, "seconds": 5});
            let (n, info) = build(&p, &dir(), 0).unwrap();
            let g = by_class(&n, "MiniMaxH3AddGuide");
            let cut = node(&n, &g[0]["inputs"]["image"]);
            assert_eq!(cut["class_type"], "ImageFromBatch", "{mode}");
            assert_eq!((cut["inputs"]["batch_index"].as_i64(), cut["inputs"]["length"].as_i64()), (Some(0), info["frames"].as_i64()), "{mode}");
            assert_eq!(node(&n, &cut["inputs"]["image"])["class_type"], "GetVideoComponents");
        }
        let p = json!({"h3_mode": "control", "prompt": "a dancer", "source": "dance.mp4", "control_type": "pose", "seconds": 5});
        let (n, info) = build(&p, &dir(), 0).unwrap();
        let cut = node(&n, &by_class(&n, "RTDETR_detect")[0]["inputs"]["image"]);
        assert_eq!((cut["class_type"].as_str(), cut["inputs"]["length"].as_i64()), (Some("ImageFromBatch"), info["frames"].as_i64()));
    }

    #[test]
    fn continue_graph() {
        let p = json!({"h3_mode": "continue", "prompt": "x", "prev": "last.mp4", "continue_audio": true});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        assert_eq!(by_class(&n, "LoadVideo")[0]["inputs"]["file"], "last.mp4");
        let tail = by_class(&n, "ImageFromBatch").into_iter().find(|x| x["inputs"]["length"] == CONTINUE_FRAMES).unwrap();
        assert_eq!(tail["inputs"]["batch_index"], -CONTINUE_FRAMES);
        let g = by_class(&n, "MiniMaxH3AddGuide");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["inputs"]["frame_idx"], 0);
        // 聲音只取上一段最後 22 格那一截，不能整條從 0 秒貼上去
        let trim = node(&n, &g[0]["inputs"]["audio"]);
        assert_eq!(trim["class_type"], "TrimAudioDuration");
        let secs = CONTINUE_FRAMES as f64 / FPS as f64;
        assert_eq!(trim["inputs"]["start_index"].as_f64(), Some(-secs));
        assert_eq!(trim["inputs"]["duration"].as_f64(), Some(secs));
        assert_eq!(trim["inputs"]["audio"][1], 1);
    }

    #[test]
    fn two_pass_graph() {
        let p = json!({"h3_mode": "ref2v", "prompt": "<Picture 1>", "refs": {"images": ["a.png"]}, "two_pass": true, "quality": "high", "steps": 8});
        let (n, info) = build(&p, &dir(), 0).unwrap();
        assert_eq!(by_class(&n, "SplitSigmas")[0]["inputs"]["step"], 8);
        assert_eq!(by_class(&n, "BasicScheduler")[0]["inputs"]["steps"], 12);
        assert_eq!(by_class(&n, "SamplerCustomAdvanced").len(), 2);
        assert_eq!(by_class(&n, "MinimaxH3LatentUpscaler3D")[0]["inputs"]["mode.scale"], 1.5);
        // 一采在低解析度：放大 1.5 倍＝最後的尺寸
        let r = &by_class(&n, "MiniMaxH3ReferenceToVideo")[0]["inputs"];
        assert_eq!((r["width"].as_i64(), r["height"].as_i64()), (Some(896), Some(512)));
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(1344), Some(768)));
        let bad = json!({"h3_mode": "i2v", "prompt": "x", "first": "a.png", "two_pass": true});
        assert!(build(&bad, &dir(), 0).is_err());
        // 記錄的尺寸＝一采×1.5（雲端實測：標準畫質 16:9 一采 704×384，成品 1056×576，舊的記成 1056×608）
        let std = json!({"h3_mode": "t2v", "prompt": "x", "two_pass": true, "quality": "std", "aspect": "16:9"});
        let (n, info) = build(&std, &dir(), 0).unwrap();
        let c = &by_class(&n, "MiniMaxH3ImageToVideo")[0]["inputs"];
        assert_eq!((c["width"].as_i64(), c["height"].as_i64()), (Some(704), Some(384)));
        assert_eq!((info["width"].as_i64(), info["height"].as_i64()), (Some(1056), Some(576)));
    }

    fn board(items: Value) -> Value {
        json!({"h3_mode": "board", "prompt": "x", "seconds": 5, "items": items})
    }

    #[test]
    fn board_keyframe_path() {
        // 只有首尾幀：走 ImageToVideo（官方 FL2VA），沒有參考
        let p = board(json!([{"kind": "image", "name": "b.png", "use": "anchor", "secs": -1}, {"kind": "image", "name": "tall.png", "use": "anchor", "secs": 0}]));
        let (n, info) = build(&p, &dir(), 0).unwrap();
        let c = &by_class(&n, "MiniMaxH3ImageToVideo")[0]["inputs"];
        assert_eq!(node(&n, &c["first_frame"])["inputs"]["image"], "tall.png");
        assert_eq!(node(&n, &c["last_frame"])["inputs"]["image"], "b.png");
        assert!(by_class(&n, "MiniMaxH3ReferenceToVideo").is_empty() && by_class(&n, "MiniMaxH3AddGuide").is_empty());
        assert!(info["height"].as_i64() > info["width"].as_i64(), "畫布照首幀（直的）");
        // 什麼都沒放＝文生影片
        let (n, _) = build(&board(json!([])), &dir(), 0).unwrap();
        assert!(by_class(&n, "MiniMaxH3ImageToVideo")[0]["inputs"].get("first_frame").is_none());
    }

    #[test]
    fn board_everything_together() {
        // 參考圖＋首幀＋中間釘圖＋參考影片（含原聲）＋音色＋釘住的聲音＋接續：全部疊在同一支
        let p = board(json!([
            {"kind": "image", "name": "a.png", "use": "ref", "role": "character"},
            {"kind": "image", "name": "tall.png", "use": "anchor", "secs": 2.5},
            {"kind": "video", "name": "move.mp4", "use": "ref", "audio": true},
            {"kind": "audio", "name": "voice.wav", "use": "ref"},
            {"kind": "audio", "name": "song.wav", "use": "anchor", "secs": 1},
            {"kind": "video", "name": "last.mp4", "use": "continue", "audio": false},
        ]));
        let (n, _) = build(&p, &dir(), 0).unwrap();
        let r = &by_class(&n, "MiniMaxH3ReferenceToVideo")[0]["inputs"];
        // 圖照素材板順序：<Picture 1>＝a.png、<Picture 2>＝釘的 tall.png
        assert_eq!(node(&n, &r["ref_images.ref_image_0"])["inputs"]["image"], "a.png");
        assert_eq!(node(&n, &r["ref_images.ref_image_1"])["inputs"]["image"], "tall.png");
        assert_eq!(r["ref_video_audios.ref_video_audio_0"][1], 1);
        assert_eq!(node(&n, &r["ref_audios.ref_audio_0"])["inputs"]["audio"], "voice.wav");
        let g = by_class(&n, "MiniMaxH3AddGuide");
        assert_eq!(g.len(), 3, "釘圖、釘聲音、接續各一個");
        let idx: Vec<i64> = g.iter().map(|x| x["inputs"]["frame_idx"].as_i64().unwrap()).collect();
        assert!(idx.contains(&60) && idx.contains(&24) && idx.contains(&0), "{idx:?}");
        let la = by_class(&n, "LoadAudio").into_iter().find(|x| x["inputs"]["audio"] == "song.wav").unwrap();
        assert!(g.iter().any(|x| x["inputs"].get("audio").is_some_and(|a| node(&n, a) == la) && x["inputs"]["frame_idx"] == 24));
        // 提示詞提到不存在的素材照樣擋
        let mut bad = p.clone();
        bad["prompt"] = json!("<Picture 3>");
        assert!(build(&bad, &dir(), 0).unwrap_err().0.contains("<Picture 3>"));
    }

    #[test]
    fn board_guide_and_rules() {
        let swap = board(json!([{"kind": "image", "name": "a.png", "use": "ref"}, {"kind": "video", "name": "src.mp4", "use": "guide", "lora": "swap", "audio": true}]));
        let (n, info) = build(&swap, &dir(), 0).unwrap();
        assert!(info["prompt"].as_str().unwrap().starts_with("body_swap:"));
        assert_eq!(by_class(&n, "LoraLoaderModelOnly")[0]["inputs"]["lora_name"], MODE_LORAS[0].1);
        let g = by_class(&n, "MiniMaxH3AddGuide");
        assert_eq!((g.len(), g[0]["inputs"]["frame_idx"].as_i64()), (1, Some(0)));
        // 最後一幀釘在參考路：frame_idx -1
        let last = board(json!([{"kind": "image", "name": "a.png", "use": "ref"}, {"kind": "image", "name": "b.png", "use": "anchor", "secs": -1}]));
        let (n, _) = build(&last, &dir(), 0).unwrap();
        assert_eq!(by_class(&n, "MiniMaxH3AddGuide")[0]["inputs"]["frame_idx"], -1);
        // 骨架控制
        let ctl = board(json!([{"kind": "video", "name": "dance.mp4", "use": "control"}]));
        let (n, _) = build(&ctl, &dir(), 0).unwrap();
        assert_eq!(by_class(&n, "MiniMaxH3FunControlNetApply").len(), 1);
        // 擋下的組合
        let e = |items: Value| build(&board(items), &dir(), 0).unwrap_err().0;
        assert!(e(json!([{"kind": "video", "name": "a.mp4", "use": "continue"}, {"kind": "video", "name": "b.mp4", "use": "guide"}])).contains("只能放一段"));
        assert!(e(json!([{"kind": "video", "name": "a.mp4", "use": "continue"}, {"kind": "image", "name": "a.png", "use": "anchor", "secs": 0}])).contains("第 0 秒"));
        assert!(e(json!([{"kind": "image", "name": "a.png", "use": "anchor", "secs": 0}, {"kind": "image", "name": "b.png", "use": "anchor", "secs": 0}])).contains("同一個時間點"));
        assert!(e(json!([{"kind": "video", "name": "src.mp4", "use": "guide", "lora": "swap"}])).contains("角色圖"));
        assert!(e(json!([{"kind": "image", "name": "a.png", "use": "anchor", "secs": 30}])).contains("超出"));
        assert!(e(json!([{"kind": "audio", "name": "a.wav", "use": "ref"}])).contains("不能單獨"));
        assert!(e(json!([{"kind": "image", "name": "a.png", "use": "fly"}])).contains("看不懂"));
        // 二采只能用在純文生／純參考
        let mut two = board(json!([{"kind": "image", "name": "a.png", "use": "ref"}]));
        two["two_pass"] = json!(true);
        assert!(build(&two, &dir(), 0).is_ok());
        let mut two = last.clone();
        two["two_pass"] = json!(true);
        assert!(build(&two, &dir(), 0).is_err());
    }

    #[test]
    fn selflift_and_speedup_graph() {
        let p = json!({"h3_mode": "i2v", "prompt": "x", "first": "a.png", "selflift": true, "speedup": true, "steps": 12, "sampler": "res_multistep",
                       "loras": [{"name": "a.safetensors", "strength": 0.5}], "shift_video": 5});
        let (n, info) = build(&p, &dir(), 0).unwrap();
        assert_eq!(info["selflift"], true);
        let sl = &by_class(&n, "SelfLiftH3Sampler")[0]["inputs"];
        assert_eq!(sl["transition_step"], 6);
        assert_eq!(sl["upscaler_model"], UPSCALER);
        assert_eq!(node(&n, &sl["negative"])["class_type"], "ConditioningZeroOut");
        assert_eq!(node(&n, &sl["sampler"])["inputs"]["sampler_name"], "euler");
        assert_eq!(node(&n, &sl["sigmas"])["inputs"]["steps"], 12);
        // 成品從 SelfLift 出來，沒有 SamplerCustomAdvanced
        assert_eq!(node(&n, &by_class(&n, "VAEDecode")[0]["inputs"]["samples"])["class_type"], "SelfLiftH3Sampler");
        assert!(by_class(&n, "SamplerCustomAdvanced").is_empty());
        // 加速節點在 LoRA 後、SigmaShift 前
        let shift = by_class(&n, "MiniMaxH3SigmaShift")[0];
        let bsa = node(&n, &shift["inputs"]["model"]);
        assert_eq!(bsa["class_type"], "BlockSparseAttention");
        assert_eq!(bsa["inputs"]["selection"], "sol-attn");
        let mab = node(&n, &bsa["inputs"]["model"]);
        assert_eq!(mab["class_type"], "ModelAttentionBackend");
        assert_eq!(node(&n, &mab["inputs"]["model"])["class_type"], "LoraLoaderModelOnly");
        // 不能跟二采一起、步數太少擋下
        let both = json!({"prompt": "x", "selflift": true, "two_pass": true});
        assert!(build(&both, &dir(), 0).is_err());
        assert!(build(&json!({"prompt": "x", "selflift": true, "steps": 3}), &dir(), 0).is_err());
        // 沒開就沒有這些節點
        let (n, _) = build(&json!({"prompt": "x"}), &dir(), 0).unwrap();
        assert!(by_class(&n, "SelfLiftH3Sampler").is_empty() && by_class(&n, "BlockSparseAttention").is_empty());
    }

    #[test]
    fn missing_refs_checks() {
        assert_eq!(missing_refs("<Picture 1> <Picture 3>", 2, 0, 0), vec!["<Picture 3>"]);
        assert!(missing_refs("<Audio 1> sings", 0, 1, 1).is_empty());
        assert_eq!(missing_refs("<Video 2> <Video 2>", 0, 1, 0), vec!["<Video 2>"]);
        assert_eq!(missing_refs("<Picture 0>", 3, 0, 0), vec!["<Picture 0>"]);
        // 送出時擋下
        let p = json!({"h3_mode": "ref2v", "prompt": "<Picture 3>", "refs": {"images": ["a.png", "b.png"]}});
        assert!(build(&p, &dir(), 0).unwrap_err().0.contains("<Picture 3>"));
    }

    #[test]
    fn limits() {
        let imgs: Vec<String> = (0..10).map(|i| format!("{i}.png")).collect();
        assert!(build(&json!({"h3_mode": "ref2v", "prompt": "x", "refs": {"images": imgs}}), &dir(), 0).is_err());
        let imgs: Vec<String> = (0..9).map(|i| format!("{i}.png")).collect();
        let p = json!({"h3_mode": "ref2v", "prompt": "x", "refs": {"images": imgs, "videos": ["a", "b", "c"], "audios": ["d"]}});
        assert!(build(&p, &dir(), 0).unwrap_err().0.contains("12"));
        let p = json!({"h3_mode": "ref2v", "prompt": "x", "refs": {"audios": ["d.wav"]}});
        assert!(build(&p, &dir(), 0).is_err(), "聲音不能單獨用");
        assert!(build(&json!({"h3_mode": "nope", "prompt": "x"}), &dir(), 0).is_err());
        assert!(build(&json!({"h3_mode": "t2v", "prompt": " "}), &dir(), 0).is_err());
    }

    #[test]
    fn loras_chain() {
        let p = json!({"prompt": "x", "loras": [{"name": "a.safetensors", "strength": 0.5}, {"name": "b.safetensors", "strength": 1}]});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        let l = by_class(&n, "LoraLoaderModelOnly");
        assert_eq!(l.len(), 2);
        let guider = by_class(&n, "BasicGuider")[0];
        let last = node(&n, &guider["inputs"]["model"]);
        assert_eq!(last["inputs"]["lora_name"], "b.safetensors");
        assert_eq!(node(&n, &last["inputs"]["model"])["inputs"]["lora_name"], "a.safetensors");
        assert_eq!(node(&n, &l[0]["inputs"]["model"])["class_type"], "UNETLoader");
    }

    #[test]
    fn swap_graph() {
        let p = json!({"h3_mode": "swap", "prompt": "<Picture 1> walks", "source": "src.mp4", "refs": {"images": ["a.png"]}, "src_wh": [720, 1280]});
        let (n, info) = build(&p, &dir(), 0).unwrap();
        assert!(info["prompt"].as_str().unwrap().starts_with("body_swap:"));
        assert_eq!(by_class(&n, "LoraLoaderModelOnly")[0]["inputs"]["lora_name"], MODE_LORAS[0].1);
        let g = by_class(&n, "MiniMaxH3AddGuide");
        assert_eq!(g[0]["inputs"]["frame_idx"], 0);
        assert!(info["height"].as_i64() > info["width"].as_i64(), "比例跟原影片走");
        assert!(build(&json!({"h3_mode": "swap", "prompt": "x", "source": "src.mp4"}), &dir(), 0).is_err());
    }

    #[test]
    fn live_and_official_turbo() {
        let p = json!({"h3_mode": "live", "prompt": "Animate <Picture 1> with a locked-off camera", "refs": {"images": ["a.png"]}, "model": OFFICIAL_REF2VA});
        let (n, info) = build(&p, &dir(), 0).unwrap();
        assert!(info["prompt"].as_str().unwrap().starts_with("live_wallpaper:"));
        assert_eq!(by_class(&n, "MiniMaxH3ReferenceToVideo")[0]["inputs"]["ref_image_size"], "max");
        let names: Vec<&str> = by_class(&n, "LoraLoaderModelOnly").iter().map(|l| l["inputs"]["lora_name"].as_str().unwrap()).collect();
        assert!(names.contains(&TURBO_REF2VA) && names.contains(&MODE_LORAS[3].1), "{names:?}");
        // 10Eros 不補 Turbo
        let p = json!({"h3_mode": "live", "prompt": "x", "refs": {"images": ["a.png"]}});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        assert!(!by_class(&n, "LoraLoaderModelOnly").iter().any(|l| l["inputs"]["lora_name"] == TURBO_REF2VA));
        assert!(build(&json!({"h3_mode": "live", "prompt": "x"}), &dir(), 0).is_err());
    }

    #[test]
    fn control_graph() {
        let p = json!({"h3_mode": "control", "prompt": "a dancer", "source": "dance.mp4", "control_type": "pose"});
        let (n, _) = build(&p, &dir(), 0).unwrap();
        let c = by_class(&n, "MiniMaxH3FunControlNetApply");
        assert_eq!(c.len(), 1);
        assert_eq!(node(&n, &c[0]["inputs"]["control_video"])["class_type"], "SDPoseDrawKeypoints");
        let guider = by_class(&n, "BasicGuider")[0];
        assert_eq!(node(&n, &guider["inputs"]["model"])["class_type"], "MiniMaxH3FunControlNetApply");
        assert_eq!(by_class(&n, "ModelPatchLoader")[0]["inputs"]["name"], CONTROL_PATCH);
    }
}

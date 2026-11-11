//! 訓練台（煉 LoRA）的純邏輯：專案檔、家族表、參數計算、訓練圖整理、描述處理、ai-toolkit 設定檔、C 站請求內容。
//! HTTP 與背景工作在 server\api_train.rs。設計：design\訓練台_設計.md。
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const KINDS: [&str; 3] = ["style", "character", "concept"];
/// 訓練圖縮到大約這個像素數（1024×1024）
pub const TARGET_PX: u64 = 1_048_576;
/// 原圖超過這個像素數不收（解碼會吃掉幾百 MB 記憶體）
pub const MAX_SRC_PX: u64 = 50_000_000;
/// 原圖小於這個像素數算「偏小」
const SMALL_PX: u64 = 500_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Caption {
    /// Danbooru tag（Anima、SDXL）
    Tags,
    /// 英文句子（Qwen、MOSS、Krea 2、H3）
    Sentence,
}

/// 一個可以煉 LoRA 的模型家族。數字出處：ai-toolkit 介面的預設值（extensions_built_in\diffusion_models\ui.tsx，
/// commit 9ab33b1）＋使用者自己煉過的紀錄（設計稿第 5 節）。
pub struct Fam {
    pub id: &'static str,
    pub label: &'static str,
    pub caption: Caption,
    /// ai-toolkit 的 model.arch
    pub arch: &'static str,
    /// 訓練用的底模（Hugging Face repo；SDXL 會換成使用者自己的 checkpoint）
    pub base_hint: &'static str,
    /// 給人看的一句話：為什麼用這顆底模
    pub base_note: &'static str,
    pub rank: u32,
    pub lr: f64,
    pub res: [u32; 2],
    /// 本機至少要多少顯存才讓選
    pub min_vram_gb: f64,
    /// C 站訓練 API 的 ecosystem
    pub civitai: Option<&'static str>,
    /// 魔塔訓練頁的底模代號
    pub modelscope: Option<&'static str>,
    /// 煉好的 LoRA 放 loras 底下哪個資料夾（跟下載中心 civitai::base_subdir 同一套）
    pub lora_sub: &'static str,
    pub video: bool,
    /// (qtype, qtype_te)：這個家族預設要量化才塞得進顯卡
    pub quant: Option<(&'static str, &'static str)>,
    pub timestep: Option<&'static str>,
    /// Turbo 模型用的 training adapter
    pub adapter: Option<&'static str>,
    pub sample_cfg: f64,
    pub sample_steps: u32,
    /// model.model_kwargs 底下的幾行（已經是 YAML）
    pub model_kwargs: &'static [&'static str],
    /// network.network_kwargs.ignore_if_contains
    pub ignore: &'static [&'static str],
    /// 底模在 Hugging Face 要先按同意才能下載的話，放那一頁的網址
    pub gate: Option<&'static str>,
    /// 24GB 級雲端顯卡上每一步大概幾秒（估價用，粗估）
    pub sec_per_step: f64,
}

pub const FAMS: &[Fam] = &[
    Fam {
        id: "anima",
        label: "Anima（動漫）",
        caption: Caption::Tags,
        arch: "anima",
        base_hint: "circlestone-labs/Anima-Base-v1.0-Diffusers",
        base_note: "在 Anima Base 上煉，Turbo 上一樣能用",
        rank: 32,
        lr: 1e-4,
        res: [768, 1024],
        min_vram_gb: 10.0,
        civitai: Some("anima"),
        modelscope: None,
        lora_sub: "",
        video: false,
        quant: None,
        timestep: Some("weighted"),
        adapter: None,
        sample_cfg: 4.0,
        sample_steps: 25,
        model_kwargs: &[],
        ignore: &[],
        gate: None,
        sec_per_step: 1.2,
    },
    Fam {
        id: "sdxl",
        label: "SDXL 系（Illustrious、NoobAI、Pony…）",
        caption: Caption::Tags,
        arch: "sdxl",
        base_hint: "stabilityai/stable-diffusion-xl-base-1.0",
        base_note: "用你自己的 SDXL 整包模型當底模，煉出來最合那一系",
        rank: 16,
        lr: 1e-4,
        res: [768, 1024],
        min_vram_gb: 8.0,
        civitai: Some("sdxl"),
        modelscope: None,
        lora_sub: "sdxl",
        video: false,
        quant: None,
        timestep: None,
        adapter: None,
        sample_cfg: 6.0,
        sample_steps: 20,
        model_kwargs: &[],
        ignore: &[],
        gate: None,
        sec_per_step: 0.8,
    },
    Fam {
        id: "qwen21",
        label: "Qwen 2.1（寫實／通用）",
        caption: Caption::Sentence,
        arch: "qwen_image_2",
        base_hint: "Comfy-Org/Qwen-Image-2.1/diffusion_models/qwen_image_2.1_turbo_int8_convrot.safetensors",
        base_note: "Turbo 版＋training adapter，煉出來直接配 8 步用",
        rank: 32,
        lr: 1e-4,
        res: [768, 1024],
        min_vram_gb: 16.0,
        civitai: Some("qwen21"),
        modelscope: Some("QWEN_IMAGE_2_1"),
        lora_sub: "qwen21",
        video: false,
        quant: Some(("convrot8", "convrot8")),
        timestep: Some("shift"),
        adapter: Some("ostris/qwen_image_2_turbo_training_adapter/qwen_image_2_turbo_training_adapter_v1.safetensors"),
        sample_cfg: 1.0,
        sample_steps: 8,
        model_kwargs: &["turbo: true", "rgba: false"],
        ignore: &[],
        gate: None,
        sec_per_step: 3.5,
    },
    Fam {
        id: "moss",
        label: "MOSS-Image",
        caption: Caption::Sentence,
        arch: "moss_image",
        base_hint: "OpenMOSS-Team/MOSS-Image-Base",
        base_note: "在 MOSS Base 上煉，SFT 和 Base 都能用",
        rank: 32,
        lr: 1e-4,
        res: [768, 1024],
        min_vram_gb: 16.0,
        civitai: Some("mossImage"),
        modelscope: Some("MOSS_IMAGE_SFT"),
        lora_sub: "moss",
        video: false,
        quant: Some(("convrot8", "convrot8")),
        timestep: Some("shift"),
        adapter: None,
        sample_cfg: 3.0,
        sample_steps: 30,
        model_kwargs: &["uncond_skip_layers: [ 22 ]"],
        ignore: &["layers.22."],
        gate: None,
        sec_per_step: 3.0,
    },
    Fam {
        id: "krea2",
        label: "Krea 2",
        caption: Caption::Sentence,
        arch: "krea2",
        base_hint: "krea/Krea-2-Raw",
        base_note: "官方建議在 Raw 上煉、套到 Turbo 用",
        rank: 32,
        lr: 1e-4,
        res: [768, 1024],
        min_vram_gb: 16.0,
        civitai: Some("krea2"),
        modelscope: Some("KREA_2"),
        lora_sub: "krea2",
        video: false,
        quant: Some(("qfloat8", "qfloat8")),
        timestep: Some("linear"),
        adapter: None,
        sample_cfg: 4.0,
        sample_steps: 25,
        model_kwargs: &[],
        ignore: &[],
        gate: Some("https://huggingface.co/krea/Krea-2-Raw"),
        sec_per_step: 3.0,
    },
    Fam {
        id: "h3",
        label: "MiniMax H3（影片）",
        caption: Caption::Sentence,
        arch: "minimax_h3",
        base_hint: "Comfy-Org/MiniMax-H3",
        base_note: "配 H3 的 training adapter 煉，訓練素材是影片（會帶聲音）",
        rank: 16,
        lr: 1e-4,
        res: [512, 768],
        min_vram_gb: 24.0,
        civitai: Some("minimaxh3"),
        modelscope: None,
        lora_sub: "other\\minimax_h3_aab83455",
        video: true,
        quant: Some(("convrot8", "nvfp4")),
        timestep: Some("shift"),
        adapter: Some("ostris/minimax_h3_training_adapter/minimax_h3_training_adapter_v3.safetensors"),
        sample_cfg: 1.0,
        sample_steps: 28,
        model_kwargs: &[],
        ignore: &["adaln_proj"],
        gate: None,
        sec_per_step: 8.0,
    },
];

pub fn fam(id: &str) -> Option<&'static Fam> {
    FAMS.iter().find(|f| f.id == id)
}

/// 專案代號：t ＋ 12 位小寫十六進位（同時擋掉路徑穿越）
pub fn id_ok(id: &str) -> bool {
    id.len() == 13 && id.starts_with('t') && id[1..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn new_id() -> String {
    format!("t{:012x}", rand::random_range(0..(1u64 << 48)))
}

/// 觸發詞只收英數、底線、連字號，2～32 字（逗號、引號、換行會弄壞描述檔和設定檔）
pub fn trigger_ok(s: &str) -> bool {
    (2..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 自動給一個不容易撞到常用字的觸發詞：名字裡的英數＋兩個子音＋種類字尾
pub fn suggest_trigger(name: &str, kind: &str, salt: u32) -> String {
    const CONS: &[u8] = b"bcdfghjkmnpqrtvwxz";
    let mut base: String = name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).take(12).collect();
    if base.len() < 2 {
        base = "lora".into();
    }
    let n = CONS.len() as u32;
    let tail = match kind {
        "character" => "chr",
        "concept" => "cpt",
        _ => "style",
    };
    format!("{base}{}{}{tail}", CONS[(salt % n) as usize] as char, CONS[((salt / n) % n) as usize] as char)
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "character" => "角色",
        "concept" => "概念／服裝／物件",
        _ => "畫風",
    }
}

pub fn new_project(name: &str, kind: &str, family: &str, trigger: &str) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err("專案名字要 1～60 個字".into());
    }
    if !KINDS.contains(&kind) {
        return Err("煉什麼只能選畫風、角色、概念".into());
    }
    let f = fam(family).ok_or("沒有這個模型家族")?;
    if !trigger_ok(trigger) {
        return Err("觸發詞只能用英文、數字、底線、連字號，2～32 個字".into());
    }
    let now = chrono::Utc::now().timestamp();
    Ok(json!({
        "id": new_id(),
        "name": name,
        "kind": kind,
        "family": f.id,
        "media": if f.video { "video" } else { "image" },
        "trigger": trigger,
        "created": now,
        "updated": now,
        "images": [],
        "level": "std",
        "params": null,
        "samples": [],
        "run": null,
        "adopted": null,
    }))
}

pub fn dir(data: &Path) -> PathBuf {
    data.join("train")
}

pub fn load(dir: &Path, id: &str) -> Result<Value, String> {
    if !id_ok(id) {
        return Err("專案代號不對".into());
    }
    let raw = std::fs::read(dir.join(id).join("project.json")).map_err(|_| "找不到這個訓練專案".to_string())?;
    serde_json::from_slice(&raw).map_err(|e| format!("專案檔壞了：{e}"))
}

pub fn save(dir: &Path, p: &Value) -> Result<(), String> {
    let id = p.get("id").and_then(Value::as_str).filter(|i| id_ok(i)).ok_or("專案代號不對")?;
    crate::director::write_atomic(&dir.join(id).join("project.json"), p).map_err(|e| format!("專案存不進去：{e}"))
}

/// 全部專案，新的在前
pub fn list(dir: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| load(dir, &e.file_name().to_string_lossy()).ok())
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.get("created").and_then(Value::as_i64).unwrap_or(0)));
    out
}

// ================================================================ 參數

/// 照「每張圖看幾次」算步數。回 {steps, epochs, rank, alpha, lr, optimizer, resolution, batch, caption_dropout, flip, why}
pub fn auto_params(fam: &Fam, kind: &str, images: usize, level: &str) -> Value {
    let per = match kind {
        "character" => 30.0,
        "concept" => 25.0,
        _ => 20.0,
    };
    let (mult, level_name) = match level {
        "light" => (0.6, "輕"),
        "full" => (1.5, "充分"),
        _ => (1.0, "標準"),
    };
    let (lo, hi) = if fam.video { (1000.0, 8000.0) } else { (500.0, 6000.0) };
    let raw = images as f64 * per * mult;
    let steps = ((raw / 50.0).round() * 50.0).clamp(lo, hi) as u64;
    let epochs = if steps <= 1500 {
        5
    } else if steps <= 3000 {
        8
    } else {
        10
    };
    let unit = if fam.video { "段" } else { "張" };
    let mut why = format!("{images} {unit} × 每{unit}約看 {} 次", per * mult);
    if (raw - steps as f64).abs() < 50.0 {
        why.push_str(&format!(" ≈ {steps} 步"));
    } else if raw < lo {
        why.push_str(&format!(" 只有 {raw:.0} 步，太少會學不起來，拉到下限 {steps} 步"));
    } else {
        why.push_str(&format!(" 有 {raw:.0} 步，再多只會背圖，壓到上限 {steps} 步"));
    }
    why.push_str(&format!("（{level_name}）；每 {} 步存一版，共 {epochs} 版", steps / epochs));
    json!({
        "steps": steps,
        "epochs": epochs,
        "rank": fam.rank,
        "alpha": fam.rank,
        "lr": fam.lr,
        "optimizer": "adamw8bit",
        "resolution": fam.res,
        "batch": 1,
        "caption_dropout": 0.05,
        "flip": kind != "character" && !fam.video,
        "why": why,
    })
}

// ================================================================ 訓練圖

/// 不放大、邊長對齊 64、面積不超過 target
pub fn fit_size(w: u32, h: u32, target_px: u64) -> (u32, u32) {
    let (wf, hf) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let s = (target_px as f64 / (wf * hf)).sqrt().min(1.0);
    let al = |v: f64| (((v * s + 1e-6) / 64.0).floor() as u32 * 64).max(64);
    (al(wf), al(hf))
}

/// 差異雜湊：縮成 9×8 灰階，每格跟右邊那格比
pub fn dhash(img: &image::DynamicImage) -> u64 {
    let g = img.resize_exact(9, 8, image::imageops::FilterType::Triangle).to_luma8();
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h = (h << 1) | u64::from(g.get_pixel(x, y).0[0] > g.get_pixel(x + 1, y).0[0]);
        }
    }
    h
}

/// 幾乎一樣的圖（雜湊差 max_bits 個位元以內），回 (前面那張, 後面那張)
pub fn near_dupes(hashes: &[(String, u64)], max_bits: u32) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, (a, ha)) in hashes.iter().enumerate() {
        for (b, hb) in &hashes[i + 1..] {
            if (ha ^ hb).count_ones() <= max_bits {
                out.push((a.clone(), b.clone()));
            }
        }
    }
    out
}

/// 把一張原圖整理進專案資料夾：透明底鋪白、縮到約 100 萬像素（置中裁到 64 的倍數）、存成 NNNN.png。原圖不動。
pub fn import_image(src: &Path, dst_dir: &Path, index: usize) -> Result<Value, String> {
    let open = || image::ImageReader::open(src).and_then(image::ImageReader::with_guessed_format).map_err(|e| format!("打不開：{e}"));
    let (sw, sh) = open()?.into_dimensions().map_err(|_| "這個檔不是看得懂的圖片".to_string())?;
    if sw == 0 || sh == 0 {
        return Err("這個檔不是看得懂的圖片".into());
    }
    if u64::from(sw) * u64::from(sh) > MAX_SRC_PX {
        return Err(format!("圖太大（{sw}×{sh}，超過 5000 萬像素），請先縮小再放進來"));
    }
    let rgba = open()?.decode().map_err(|_| "這張圖解不開（檔案可能壞了）".to_string())?.to_rgba8();
    let mut rgb = image::RgbImage::new(sw, sh);
    for (o, p) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = u32::from(p.0[3]);
        for c in 0..3 {
            o.0[c] = ((u32::from(p.0[c]) * a + 255 * (255 - a) + 127) / 255) as u8;
        }
    }
    let (tw, th) = fit_size(sw, sh, TARGET_PX);
    let scale = (f64::from(tw) / f64::from(sw)).max(f64::from(th) / f64::from(sh));
    let (rw, rh) = (((f64::from(sw) * scale).ceil() as u32).max(tw), ((f64::from(sh) * scale).ceil() as u32).max(th));
    let img = image::DynamicImage::ImageRgb8(rgb);
    let img = if (rw, rh) == (sw, sh) { img } else { img.resize_exact(rw, rh, image::imageops::FilterType::CatmullRom) };
    let img = img.crop_imm((rw - tw) / 2, (rh - th) / 2, tw, th);
    std::fs::create_dir_all(dst_dir).map_err(|e| format!("建不了資料夾：{e}"))?;
    let file = format!("{index:04}.png");
    img.save(dst_dir.join(&file)).map_err(|e| format!("存不進去：{e}"))?;
    Ok(json!({
        "file": file,
        "w": tw,
        "h": th,
        "src_w": sw,
        "src_h": sh,
        "small": u64::from(sw) * u64::from(sh) < SMALL_PX,
        "hash": format!("{:016x}", dhash(&img)),
        "src_name": src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    }))
}

fn on(item: &Value) -> bool {
    item.get("off").and_then(Value::as_bool) != Some(true)
}

fn bucket(w: u64, h: u64) -> &'static str {
    const B: [(&str, f64); 7] = [("1:1", 1.0), ("4:3", 4.0 / 3.0), ("3:4", 0.75), ("3:2", 1.5), ("2:3", 2.0 / 3.0), ("16:9", 16.0 / 9.0), ("9:16", 9.0 / 16.0)];
    let r = (w.max(1) as f64 / h.max(1) as f64).ln();
    B.iter().min_by(|a, b| (a.1.ln() - r).abs().total_cmp(&(b.1.ln() - r).abs())).map_or("1:1", |b| b.0)
}

/// 訓練圖的體檢：張數夠不夠、有沒有偏小的、有沒有幾乎一樣的、長寬比分布。勾掉的（off）不算。
pub fn dataset_report(items: &[Value], kind: &str) -> Value {
    let live: Vec<&Value> = items.iter().filter(|i| on(i)).collect();
    let small = live.iter().filter(|i| i.get("small").and_then(Value::as_bool) == Some(true)).count();
    let hashes: Vec<(String, u64)> = live
        .iter()
        .filter_map(|i| Some((i.get("file")?.as_str()?.to_string(), u64::from_str_radix(i.get("hash")?.as_str()?, 16).ok()?)))
        .collect();
    let dupes = near_dupes(&hashes, 6);
    let mut buckets = Map::new();
    for i in &live {
        let b = bucket(i.get("w").and_then(Value::as_u64).unwrap_or(1), i.get("h").and_then(Value::as_u64).unwrap_or(1));
        let n = buckets.get(b).and_then(Value::as_u64).unwrap_or(0);
        buckets.insert(b.into(), json!(n + 1));
    }
    let (lo, hi) = match kind {
        "character" => (15, 60),
        "concept" => (20, 100),
        _ => (30, 200),
    };
    let (n, k) = (live.len(), kind_label(kind));
    let mut warnings = Vec::new();
    if n == 0 {
        warnings.push("還沒有訓練圖".to_string());
    } else if n < lo {
        warnings.push(format!("只有 {n} 張，太少：煉{k}建議 {lo}～{hi} 張，太少容易只會背這幾張圖"));
    } else if n > hi {
        warnings.push(format!("有 {n} 張，偏多：煉{k}建議 {lo}～{hi} 張，挑品質好、變化多的就夠"));
    }
    if small > 0 {
        warnings.push(format!("有 {small} 張圖偏小（不到 50 萬像素），煉出來容易糊，建議換大一點的或勾掉"));
    }
    if !dupes.is_empty() {
        warnings.push(format!("有 {} 組幾乎一樣的圖，重複的圖會被學得特別重，建議各留一張", dupes.len()));
    }
    json!({
        "count": n,
        "small": small,
        "dupes": dupes.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
        "buckets": buckets,
        "suggest": [lo, hi],
        "warnings": warnings,
    })
}

// ================================================================ 描述

fn norm(tag: &str) -> String {
    tag.trim().to_lowercase().replace('_', " ").replace('\\', "")
}

/// 觸發詞放最前面、只出現一次
pub fn with_trigger(caption: &str, trigger: &str, tags: bool) -> String {
    let c = caption.trim();
    if c.is_empty() {
        return trigger.to_string();
    }
    if tags {
        let rest: Vec<&str> = c.split(',').map(str::trim).filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case(trigger)).collect();
        return std::iter::once(trigger).chain(rest).collect::<Vec<_>>().join(", ");
    }
    let head = c.split(',').next().unwrap_or("").trim();
    if head.eq_ignore_ascii_case(trigger) {
        c.to_string()
    } else {
        format!("{trigger}, {c}")
    }
}

const APPEARANCE: &[&str] = &[
    "ahoge", "twintails", "ponytail", "side ponytail", "braid", "twin braids", "single braid", "hair bun", "double bun", "bangs",
    "blunt bangs", "sidelocks", "hair between eyes", "hair over one eye", "freckles", "mole", "mole under eye", "mole under mouth",
    "fang", "fangs", "heterochromia", "dark-skinned female", "dark-skinned male", "tan", "halo", "antenna hair", "drill hair",
    "hime cut", "bob cut", "facial mark", "scar on face", "eyepatch", "thick eyebrows", "beard", "mustache",
];

fn appearance(t: &str) -> bool {
    [" hair", " eyes", " ears", " tail", " skin", " horns", " horn", " pupils", " sclera", " wings"].iter().any(|s| t.ends_with(s)) || APPEARANCE.contains(&t)
}

/// 角色每張都有的固定外觀（出現比例 ≥ min_ratio）：建議從描述拿掉，讓觸發詞把它吃進去
pub fn fixed_traits(captions: &[Vec<String>], min_ratio: f64) -> Vec<(String, f64)> {
    if captions.is_empty() {
        return Vec::new();
    }
    let mut n: HashMap<String, usize> = HashMap::new();
    for c in captions {
        let mut seen: Vec<String> = c.iter().map(|t| norm(t)).filter(|t| appearance(t)).collect();
        seen.sort();
        seen.dedup();
        for t in seen {
            *n.entry(t).or_default() += 1;
        }
    }
    let total = captions.len() as f64;
    let mut out: Vec<(String, f64)> = n.into_iter().map(|(t, k)| (t, k as f64 / total)).filter(|(_, r)| *r >= min_ratio).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

pub fn drop_tags(caption: &str, drop: &[String]) -> String {
    let d: Vec<String> = drop.iter().map(|t| norm(t)).collect();
    caption.split(',').map(str::trim).filter(|t| !t.is_empty() && !d.contains(&norm(t))).collect::<Vec<_>>().join(", ")
}

const QUALITY: &[&str] = &[
    "masterpiece", "best quality", "high quality", "good quality", "normal quality", "low quality", "worst quality", "amazing quality",
    "great quality", "very aesthetic", "aesthetic", "very awa", "newest", "recent", "mid", "early", "old", "official art",
];

/// 畫風／畫師／品質／年份類：煉畫風時不要寫進描述（寫了，這些詞就會把畫風分走）
pub fn style_noise(tag: &str) -> bool {
    let t = norm(tag);
    QUALITY.contains(&t.as_str())
        || crate::tagclean::NOISE.contains(&t.as_str())
        || crate::tagclean::REAL.contains(&t.as_str())
        || t.starts_with("score ")
        || t.starts_with("year ")
        || t.starts_with('@')
        || t.starts_with("artist:")
        || t.ends_with("(medium)")
        || t.ends_with("(style)")
}

/// 訓練中途出樣張用的 4 句：觸發詞＋資料集裡多半沒有的題材，用來看是不是只會背圖
pub fn sample_prompts(fam: &Fam, kind: &str, trigger: &str) -> Vec<String> {
    let rest: [&str; 4] = match (fam.caption, kind) {
        (Caption::Tags, "character") => [
            "solo, upper body, smile, looking at viewer, white background",
            "solo, full body, standing, school uniform, classroom",
            "solo, swimsuit, beach, from side, blue sky",
            "solo, sitting, cafe, holding cup, night",
        ],
        (Caption::Tags, "concept") => [
            "1girl, solo, standing, white background",
            "1boy, solo, street, day, full body",
            "1girl, sitting, indoors, from above",
            "2girls, outdoors, night, city lights",
        ],
        (Caption::Tags, _) => [
            "1girl, astronaut, floating, holding cup, space, full body",
            "1boy, old man, wizard, riding bicycle, street, full body",
            "no humans, cat, chef hat, kitchen, pizza",
            "scenery, night, rain, street lamp, city",
        ],
        (Caption::Sentence, "character") => [
            "portrait, smiling at the camera, plain white background",
            "standing in a classroom wearing a school uniform, full body",
            "walking on a beach in summer clothes, seen from the side",
            "sitting in a cafe at night holding a cup of coffee",
        ],
        (Caption::Sentence, "concept") => [
            "a young woman standing against a plain white background",
            "a man walking down a sunny street, full body",
            "a woman sitting indoors, seen from above",
            "two friends outdoors at night with city lights behind them",
        ],
        (Caption::Sentence, _) => [
            "a woman astronaut floating with a cup of tea, full body, white background",
            "a grumpy old wizard riding a bicycle, full body, white background",
            "a cat chef tossing pizza dough in a busy kitchen",
            "a couple dancing in the rain under a street lamp",
        ],
    };
    rest.iter().map(|r| format!("{trigger}, {r}")).collect()
}

// ================================================================ ai-toolkit 設定檔

/// YAML 雙引號字串（跳脫 \ 和 "，換行壓成空白）
fn yq(s: &str) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    format!("\"{}\"", flat.replace('\\', "\\\\").replace('"', "\\\""))
}

fn pnum(p: &Value, key: &str, default: f64) -> f64 {
    p.get("params").and_then(|x| x.get(key)).and_then(Value::as_f64).unwrap_or(default)
}

/// LoRA 的檔名＝觸發詞（只有英數、底線、連字號，放哪個系統都安全）
pub fn lora_name(p: &Value) -> &str {
    p.get("trigger").and_then(Value::as_str).filter(|t| trigger_ok(t)).unwrap_or("lora")
}

/// 產生 ai-toolkit 的訓練設定（YAML）。骨架照使用者煉 Qwen 2.1 用過的 jgdxstyle_qwen21.yaml。
pub fn toolkit_config(p: &Value, fam: &Fam, data_dir: &str, out_dir: &str, model: &str, vram_gb: f64) -> String {
    let steps = (pnum(p, "steps", 1000.0) as u64).max(1);
    let epochs = (pnum(p, "epochs", 5.0) as u64).max(1);
    let every = (steps / epochs).max(1);
    let rank = pnum(p, "rank", f64::from(fam.rank)) as u64;
    let alpha = pnum(p, "alpha", rank as f64) as u64;
    let res: Vec<String> = p
        .get("params")
        .and_then(|x| x.get("resolution"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).map(|v| v.to_string()).collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| fam.res.iter().map(u32::to_string).collect());
    let optimizer = p.get("params").and_then(|x| x.get("optimizer")).and_then(Value::as_str).unwrap_or("adamw8bit");
    let flip = p.get("params").and_then(|x| x.get("flip")).and_then(Value::as_bool).unwrap_or(false) && !fam.video;
    let low_vram = vram_gb < fam.min_vram_gb * 1.5;
    let sched = if fam.id == "sdxl" { "ddpm" } else { "flowmatch" };
    let side = if fam.video { 768 } else { 1024 };

    let mut y: Vec<String> = Vec::new();
    let mut add = |indent: usize, s: String| y.push(format!("{}{s}", " ".repeat(indent)));
    add(0, "---".into());
    add(0, "job: extension".into());
    add(0, "config:".into());
    add(2, format!("name: {}", yq(lora_name(p))));
    add(2, "process:".into());
    add(4, "- type: \"sd_trainer\"".into());
    add(6, format!("training_folder: {}", yq(out_dir)));
    add(6, "performance_log_every: 50".into());
    add(6, "device: cuda:0".into());
    add(6, "network:".into());
    add(8, "type: \"lora\"".into());
    add(8, format!("linear: {rank}"));
    add(8, format!("linear_alpha: {alpha}"));
    if !fam.ignore.is_empty() {
        add(8, "network_kwargs:".into());
        add(10, "ignore_if_contains:".into());
        for i in fam.ignore {
            add(12, format!("- {}", yq(i)));
        }
    }
    add(6, "save:".into());
    add(8, "dtype: float16".into());
    add(8, format!("save_every: {every}"));
    add(8, format!("max_step_saves_to_keep: {epochs}"));
    add(6, "datasets:".into());
    add(8, format!("- folder_path: {}", yq(data_dir)));
    add(10, "caption_ext: \"txt\"".into());
    add(10, format!("caption_dropout_rate: {}", pnum(p, "caption_dropout", 0.05)));
    add(10, "shuffle_tokens: false".into());
    add(10, "cache_latents_to_disk: true".into());
    add(10, format!("resolution: [ {} ]", res.join(", ")));
    if flip {
        add(10, "flip_x: true".into());
    }
    if fam.video {
        add(10, "num_frames: 39".into());
        add(10, "fps: 24".into());
        add(10, "auto_frame_count: true".into());
        add(10, "do_audio: true".into());
        add(10, "do_i2v: false".into());
    }
    add(6, "train:".into());
    add(8, format!("batch_size: {}", (pnum(p, "batch", 1.0) as u64).max(1)));
    add(8, format!("steps: {steps}"));
    add(8, "gradient_accumulation: 1".into());
    add(8, "train_unet: true".into());
    add(8, "train_text_encoder: false".into());
    add(8, "gradient_checkpointing: true".into());
    add(8, format!("noise_scheduler: {}", yq(sched)));
    if let Some(t) = fam.timestep {
        add(8, format!("timestep_type: {}", yq(t)));
    }
    add(8, format!("optimizer: {}", yq(optimizer)));
    add(8, format!("lr: {}", pnum(p, "lr", fam.lr)));
    add(8, "dtype: bf16".into());
    // 還沒開始煉的那張樣張不用畫：8GB 顯卡上光這一張就要等十分鐘，而且它只是底模本來的樣子
    add(8, "skip_first_sample: true".into());
    if fam.quant.is_some() {
        add(8, "cache_text_embeddings: true".into());
    }
    if fam.video {
        add(8, "audio_loss_multiplier: 1.0".into());
    }
    add(6, "model:".into());
    add(8, format!("name_or_path: {}", yq(model)));
    add(8, format!("arch: {}", yq(fam.arch)));
    if let Some(a) = fam.adapter {
        add(8, format!("assistant_lora_path: {}", yq(a)));
    }
    if !fam.model_kwargs.is_empty() {
        add(8, "model_kwargs:".into());
        for k in fam.model_kwargs {
            add(10, (*k).to_string());
        }
    }
    if let Some((q, qte)) = fam.quant {
        add(8, "quantize: true".into());
        add(8, format!("qtype: {}", yq(q)));
        add(8, "quantize_te: true".into());
        add(8, format!("qtype_te: {}", yq(qte)));
    }
    add(8, format!("low_vram: {low_vram}"));
    add(6, "sample:".into());
    add(8, format!("sampler: {}", yq(sched)));
    add(8, format!("sample_every: {every}"));
    add(8, format!("width: {side}"));
    add(8, format!("height: {side}"));
    add(8, "prompts:".into());
    for s in p.get("samples").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).filter(|s| !s.trim().is_empty()) {
        add(10, format!("- {}", yq(s.trim())));
    }
    add(8, "neg: \"\"".into());
    add(8, "seed: 42".into());
    add(8, "walk_seed: false".into());
    add(8, format!("guidance_scale: {}", fam.sample_cfg));
    add(8, format!("sample_steps: {}", fam.sample_steps));
    if fam.video {
        add(8, "num_frames: 49".into());
        add(8, "fps: 24".into());
    }
    add(0, "meta:".into());
    add(2, "name: \"[name]\"".into());
    add(2, "version: '1.0'".into());
    y.join("\n") + "\n"
}

// ================================================================ 打標

fn quality_noise(tag: &str) -> bool {
    let t = norm(tag);
    QUALITY.contains(&t.as_str()) || t.starts_with("score ") || t.starts_with("year ")
}

/// 反推結果 → 訓練用的 tag 描述（不含觸發詞）。general／character＝[(tag, 分數)]，分數高到低。
/// 畫風：內容照實寫、畫風／畫師／品質類拿掉；角色：角色名不寫（交給觸發詞）。括號還原成沒有反斜線的。
pub fn tags_caption(general: &[(String, f64)], character: &[(String, f64)], kind: &str) -> String {
    let style = kind == "style";
    let (order, notes) = crate::tagclean::clean(general, style);
    let dropped: Vec<&str> = notes.iter().map(|(t, _)| t.as_str()).collect();
    let mut out: Vec<String> = order
        .iter()
        .filter(|t| !dropped.contains(&t.as_str()))
        .filter(|t| if style { !style_noise(t) } else { !quality_noise(t) })
        .map(|t| t.replace('\\', ""))
        .collect();
    if kind != "character" {
        out.extend(character.iter().map(|(t, _)| t.replace('\\', "")));
    }
    out.join(", ")
}

/// 句子家族看圖寫描述時給語言模型的指示
pub fn caption_system(kind: &str, video: bool) -> String {
    let mut s = String::from(
        "You write training captions for fine-tuning an image model. Look at the image and describe it in one plain English paragraph of one to three sentences: \
         who or what is in it, clothing, pose and action, expression, setting, lighting and framing. Be literal and specific. \
         Output only the caption, with no preamble, no quotes and no line breaks.",
    );
    match kind {
        "character" => s.push_str(
            " The main character will be named by a trigger word, so refer to them only as \"the character\" and do NOT describe their permanent features \
             (hair colour or hairstyle, eye colour, face, skin, body type). Do describe what changes between images: clothing, pose, expression, background.",
        ),
        "style" => s.push_str(" Do NOT mention the art style, medium, rendering technique, artist or image quality; describe only the content."),
        _ => s.push_str(" Describe everything normally, including the featured object, outfit or concept."),
    }
    if video {
        s.push_str(" This frame comes from a short video clip: also describe the motion and camera movement that the clip most plausibly shows.");
    }
    s
}

// ================================================================ 訓練產物

/// ai-toolkit 釘住的版本（2026-11-09）
pub const TOOLKIT_COMMIT: &str = "9ab33b1104d71bc4c6753761e40466daaadc4a78";

/// 小幫手回報的檔案清單 → 這顆 LoRA 的各版，照步數排：[(第幾版, 檔案相對路徑)]。
/// ai-toolkit 中途存 `<名字>_000000250.safetensors`，最後一版是 `<名字>.safetensors`。
pub fn epoch_names(files: &[String], name: &str) -> Vec<(u32, String)> {
    let mut found: Vec<(u64, &String)> = files
        .iter()
        .filter_map(|f| {
            let base = f.rsplit('/').next()?.strip_suffix(".safetensors")?;
            if base == name {
                return Some((u64::MAX, f));
            }
            let step = base.strip_prefix(name)?.strip_prefix('_')?;
            (step.len() == 9 && step.bytes().all(|b| b.is_ascii_digit())).then(|| step.parse().ok()).flatten().map(|n| (n, f))
        })
        .collect();
    found.sort();
    found.into_iter().enumerate().map(|(i, (_, f))| (i as u32 + 1, f.clone())).collect()
}

/// 樣張檔名 `<時間>__<9 位步數>_<第幾句>.jpg` → (步數, 第幾句)
pub fn sample_step(file: &str) -> Option<(u64, u32)> {
    let base = file.rsplit('/').next()?;
    let (stem, _ext) = base.rsplit_once('.')?;
    let (_, rest) = stem.split_once("__")?;
    let (step, idx) = rest.split_once('_')?;
    Some((step.parse().ok()?, idx.parse().ok()?))
}

/// 雲端訓練粗估：(小時, 美金)。步數 × 這個家族每步大概幾秒，加開機裝環境約 15 分鐘
pub fn estimate(fam: &Fam, steps: u64, usd_per_hour: f64) -> (f64, f64) {
    let hours = (steps as f64 * fam.sec_per_step + 900.0) / 3600.0;
    (hours, hours * usd_per_hour)
}

/// 雲端訓練機的開機指令（bash）。口令和小幫手程式從環境變數 TRAIN_TOKEN、TRAIN_AGENT_B64 進來，不寫在指令裡。
pub fn boot_script(port: u16) -> String {
    [
        "set -e".to_string(),
        "mkdir -p /workspace && cd /workspace".to_string(),
        "if [ ! -f ai-toolkit/run.py ]; then".to_string(),
        format!("  curl -fsSL -o aitk.zip https://codeload.github.com/ostris/ai-toolkit/zip/{TOOLKIT_COMMIT}"),
        "  python3 -c \"import zipfile; zipfile.ZipFile('aitk.zip').extractall('.')\"".to_string(),
        format!("  rm -rf ai-toolkit && mv ai-toolkit-{TOOLKIT_COMMIT} ai-toolkit && rm -f aitk.zip"),
        "fi".to_string(),
        "cd /workspace/ai-toolkit && python3 -m manager install".to_string(),
        "echo \"$TRAIN_AGENT_B64\" | base64 -d > /workspace/agent.py".to_string(),
        "PY=/workspace/ai-toolkit/.venv/bin/python; [ -x \"$PY\" ] || PY=/workspace/ai-toolkit/venv/bin/python".to_string(),
        format!("exec \"$PY\" /workspace/agent.py --host 0.0.0.0 --port {port} --token \"$TRAIN_TOKEN\" --work /workspace/train_work --toolkit /workspace/ai-toolkit --python \"$PY\""),
    ]
    .join("\n")
}

// ================================================================ C 站

/// C 站 orchestration API 的訓練請求。items＝[(blob 的 AIR, 描述)]。
/// 欄位照舊站 tools\civitai_train.py 的 body()（實際送成功過的那一份），沒驗證過的欄位不亂加。
pub fn civitai_body(p: &Value, fam: &Fam, items: &[(String, String)]) -> Value {
    json!({
        "tags": ["paintdeck", lora_name(p)],
        "steps": [{"$type": "training", "input": {
            "engine": "ai-toolkit",
            "ecosystem": fam.civitai.unwrap_or(fam.id),
            "steps": pnum(p, "steps", 1000.0) as u64,
            "epochs": pnum(p, "epochs", 5.0) as u64,
            "trainingData": {"type": "blobs", "items": items.iter().map(|(air, caption)| json!({"air": air, "caption": caption})).collect::<Vec<_>>()},
        }}],
    })
}

/// 專案檔寫著「還在跑」，但繪匣這邊已經沒有在追（繪匣重開過）：
/// 本機的訓練跟著小幫手一起沒了＝interrupted；雲端訓練機、C 站任務還在外面跑＝detached（按開始可以接回去）。
/// 只改給網頁看的那一份，不存檔。
pub fn fix_stale(p: &mut Value, tracking: bool) {
    if tracking || !matches!(p["run"]["state"].as_str(), Some("starting" | "running")) {
        return;
    }
    let remote = &p["run"]["remote"];
    let alive = match p["run"]["target"].as_str() {
        Some("runpod") => remote["pod"].as_str().is_some_and(|x| !x.is_empty()),
        Some("civitai") => remote["workflow"].as_str().is_some_and(|x| !x.is_empty()),
        _ => false,
    };
    p["run"]["state"] = json!(if alive { "detached" } else { "interrupted" });
}

// ================================================================ 驗收

/// 對照表要畫的格子：每一版 × 每個強度，全部同一個種子。[(第幾版, 強度, 種子)]
pub fn compare_jobs(epochs: &[u32], strengths: &[f64], seed: i64) -> Vec<(u32, f64, i64)> {
    epochs.iter().flat_map(|&e| strengths.iter().map(move |&s| (e, s, seed))).collect()
}

/// 對照表送出前擋一下：生圖流程會把「家族跟模型不一樣」的 LoRA 默默略過，那樣整張表每格都一樣、看不出差別。
/// model_fam＝生成頁選的模型看檔頭的家族、lora_fam＝這顆 LoRA 被認成的家族（families 的代號）、
/// workflow_set＝生成頁選的是匯入的工作流組（那條路不照家族篩）。回傳要給使用者看的原因；None＝可以送
pub fn compare_block(fam: &Fam, model_fam: &str, lora_fam: &str, workflow_set: bool) -> Option<String> {
    if fam.video {
        return Some(format!("{} 是影片 LoRA，沒有生圖對照表：到「H3 影片」掛上去試", fam.label));
    }
    if workflow_set {
        return None;
    }
    if model_fam != fam.id {
        return Some(format!("生成頁現在選的模型不是 {} 家族，LoRA 掛不上去（每格會長得一樣）：先到生成頁換成 {} 的模型再出對照表", fam.label, fam.label));
    }
    if lora_fam != fam.id {
        return Some(format!("這顆 LoRA 檔沒被認成 {} 家族，生圖時會被略過：到 LoRA 卡匣把它的底模改成 {} 再試", fam.label, fam.label));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("anima_train_{name}_{}", new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pattern(w: u32, h: u32) -> image::DynamicImage {
        // 有結構的圖：斜向色塊＋漸層，縮小後 dhash 幾乎不變
        let im = image::RgbImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let block = if ((fx * 4.0) as u32 + (fy * 3.0) as u32).is_multiple_of(2) { 200.0 } else { 40.0 };
            let v = (block * 0.7 + fx * 60.0 + fy * 20.0) as u8;
            image::Rgb([v, (fy * 255.0) as u8, 255 - v])
        });
        image::DynamicImage::ImageRgb8(im)
    }

    #[test]
    fn ids_and_triggers() {
        assert!(id_ok("t0123456789ab"));
        assert!(!id_ok("../x"));
        assert!(!id_ok("t0123456789aB"));
        assert!(!id_ok("x0123456789ab"));
        assert!(id_ok(&new_id()));
        assert!(trigger_ok("jgdxstyle"));
        assert!(trigger_ok("my_char-01"));
        assert!(!trigger_ok("a,b"));
        assert!(!trigger_ok("x"));
        assert!(!trigger_ok("has space"));
        assert!(!trigger_ok(&"a".repeat(33)));
        let t = suggest_trigger("小克 Xiaoke!", "style", 7);
        assert!(trigger_ok(&t), "{t}");
        assert!(t.starts_with("xiaoke") && t.ends_with("style"), "{t}");
        assert!(trigger_ok(&suggest_trigger("小克", "character", 0)));
    }

    #[test]
    fn families_complete() {
        let mut seen = std::collections::HashSet::new();
        for f in FAMS {
            assert!(seen.insert(f.id), "重複的家族 {}", f.id);
            assert_eq!(fam(f.id).map(|x| x.id), Some(f.id));
            assert!(f.rank > 0 && f.lr > 0.0 && f.min_vram_gb > 0.0);
        }
        let tags: Vec<&str> = FAMS.iter().filter(|f| f.caption == Caption::Tags).map(|f| f.id).collect();
        assert_eq!(tags, ["anima", "sdxl"]);
        assert!(fam("h3").unwrap().video);
        assert!(FAMS.iter().filter(|f| f.video).count() == 1);
        for (id, base) in [("anima", "Anima"), ("qwen21", "Qwen 2.1"), ("moss", "MOSS Image"), ("krea2", "Krea 2"), ("sdxl", "SDXL 1.0"), ("h3", "MiniMax H3")] {
            assert_eq!(fam(id).unwrap().lora_sub, crate::civitai::base_subdir(base), "{id}");
        }
        assert!(fam("nope").is_none());
    }

    #[test]
    fn project_roundtrip() {
        let d = tmp("proj");
        let mut a = new_project("我的畫風", "style", "anima", "jgdxstyle").unwrap();
        a["created"] = json!(100);
        save(&d, &a).unwrap();
        let mut b = new_project("角色", "character", "sdxl", "xkchr").unwrap();
        b["created"] = json!(200);
        save(&d, &b).unwrap();
        assert_eq!(load(&d, a["id"].as_str().unwrap()).unwrap(), a);
        let l = list(&d);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0]["id"], b["id"]);
        assert!(load(&d, "../x").is_err());
        assert!(new_project("x", "style", "anima", "a,b").is_err());
        assert!(new_project("x", "weird", "anima", "abc").is_err());
        assert!(new_project("x", "style", "nope", "abc").is_err());
        assert!(new_project("  ", "style", "anima", "abc").is_err());
        assert_eq!(new_project("v", "style", "h3", "abcd").unwrap()["media"], "video");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn auto_params_match_history() {
        let a = fam("anima").unwrap();
        let p = auto_params(a, "style", 62, "std");
        assert_eq!(p["steps"], 1250);
        assert_eq!(p["epochs"], 5);
        assert_eq!(p["flip"], true);
        assert_eq!(p["rank"], a.rank);
        assert_eq!(p["alpha"], a.rank);
        assert!(p["why"].as_str().unwrap().contains("62"));
        let p = auto_params(a, "style", 232, "std");
        assert_eq!(p["steps"], 4650);
        assert_eq!(p["epochs"], 10);
        let p = auto_params(a, "character", 20, "std");
        assert_eq!(p["steps"], 600);
        assert_eq!(p["flip"], false);
        assert_eq!(auto_params(a, "style", 5, "std")["steps"], 500);
        assert_eq!(auto_params(a, "style", 1000, "full")["steps"], 6000);
        assert_eq!(auto_params(a, "style", 100, "light")["steps"], 1200);
        assert_eq!(auto_params(a, "style", 100, "std")["epochs"], 8);
        let h = fam("h3").unwrap();
        assert_eq!(auto_params(h, "style", 5, "std")["steps"], 1000);
        assert_eq!(auto_params(h, "style", 1000, "full")["steps"], 8000);
        assert_eq!(auto_params(h, "style", 30, "std")["flip"], false);
    }

    #[test]
    fn fit_size_rules() {
        assert_eq!(fit_size(2048, 2048, TARGET_PX), (1024, 1024));
        assert_eq!(fit_size(800, 600, TARGET_PX), (768, 576));
        let (w, h) = fit_size(4000, 1000, TARGET_PX);
        assert!(u64::from(w) * u64::from(h) <= TARGET_PX && w % 64 == 0 && h % 64 == 0 && w > h, "{w}x{h}");
        assert_eq!(fit_size(30, 20, TARGET_PX), (64, 64));
    }

    #[test]
    fn dupes_found() {
        let big = pattern(512, 384);
        let small = big.resize_exact(256, 192, image::imageops::FilterType::Triangle);
        let other = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(256, 256, |x, _| image::Rgb([255 - x as u8, 0, 0])));
        let grad = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(256, 256, |x, _| image::Rgb([x as u8, 0, 0])));
        let hs = vec![
            ("a".to_string(), dhash(&big)),
            ("b".to_string(), dhash(&small)),
            ("c".to_string(), dhash(&other)),
            ("d".to_string(), dhash(&grad)),
        ];
        assert_eq!(near_dupes(&hs, 6), vec![("a".to_string(), "b".to_string())]);
    }

    #[test]
    fn import_handles_bad_files() {
        let d = tmp("imp");
        let out = d.join("images");
        let txt = d.join("fake.png");
        std::fs::write(&txt, "not an image").unwrap();
        assert!(import_image(&txt, &out, 1).is_err());
        let empty = d.join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        assert!(import_image(&empty, &out, 1).is_err());
        let zh = d.join("中文 檔名 (1).png");
        pattern(300, 200).save(&zh).unwrap();
        let r = import_image(&zh, &out, 1).unwrap();
        assert_eq!(r["file"], "0001.png");
        assert_eq!((r["src_w"].as_u64(), r["src_h"].as_u64()), (Some(300), Some(200)));
        assert_eq!((r["w"].as_u64(), r["h"].as_u64()), (Some(256), Some(192)));
        assert_eq!(r["small"], true);
        assert_eq!(r["src_name"], "中文 檔名 (1).png");
        assert_eq!(r["hash"].as_str().unwrap().len(), 16);
        let saved = image::open(out.join("0001.png")).unwrap();
        assert_eq!((saved.width(), saved.height()), (256, 192));
        // 透明底要鋪白，不能變黑
        let alpha = d.join("alpha.png");
        image::RgbaImage::from_pixel(128, 128, image::Rgba([0, 0, 0, 0])).save(&alpha).unwrap();
        let r = import_image(&alpha, &out, 2).unwrap();
        let px = image::open(out.join(r["file"].as_str().unwrap())).unwrap().to_rgb8();
        assert_eq!(px.get_pixel(10, 10).0, [255, 255, 255]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn report_warns() {
        let items: Vec<Value> = (0..5)
            .map(|i| json!({"file": format!("{i:04}.png"), "w": 1024, "h": 1024, "small": i == 0, "hash": format!("{:016x}", (i as u64) * 0x0101_0101_0101_0101 * 37)}))
            .collect();
        let r = dataset_report(&items, "character");
        assert_eq!(r["count"], 5);
        assert_eq!(r["small"], 1);
        assert_eq!(r["buckets"]["1:1"], 5);
        let w: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(w.iter().any(|x| x.contains("太少")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("偏小")), "{w:?}");
        // 勾掉的不算
        let mut items2 = items.clone();
        items2[0]["off"] = json!(true);
        let r = dataset_report(&items2, "character");
        assert_eq!(r["count"], 4);
        assert_eq!(r["small"], 0);
        // 重複的要列出來
        let mut items3 = items.clone();
        items3[1]["hash"] = items3[2]["hash"].clone();
        let r = dataset_report(&items3, "character");
        assert_eq!(r["dupes"], json!([["0001.png", "0002.png"]]));
    }

    #[test]
    fn trigger_goes_first_once() {
        assert_eq!(with_trigger("1girl, solo, jgdxstyle, smile", "jgdxstyle", true), "jgdxstyle, 1girl, solo, smile");
        assert_eq!(with_trigger("1girl, solo", "jgdxstyle", true), "jgdxstyle, 1girl, solo");
        assert_eq!(with_trigger("", "jgdxstyle", true), "jgdxstyle");
        assert_eq!(with_trigger("A woman walks.", "xkchr", false), "xkchr, A woman walks.");
        assert_eq!(with_trigger("xkchr, A woman walks.", "xkchr", false), "xkchr, A woman walks.");
        assert_eq!(with_trigger("  ", "xkchr", false), "xkchr");
    }

    #[test]
    fn fixed_traits_only_appearance() {
        let caps: Vec<Vec<String>> = (0..10)
            .map(|i| {
                let mut v = vec!["1girl".to_string()];
                if i < 9 {
                    v.push("blue_hair".into());
                }
                if i < 3 {
                    v.push("smile".into());
                }
                if i < 5 {
                    v.push("red eyes".into());
                }
                v
            })
            .collect();
        let r = fixed_traits(&caps, 0.8);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].0, "blue hair");
        assert!((r[0].1 - 0.9).abs() < 1e-9);
        assert!(fixed_traits(&[], 0.8).is_empty());
    }

    #[test]
    fn drop_keeps_others() {
        assert_eq!(drop_tags("xk, 1girl, blue hair, smile, red_eyes", &["Blue_Hair".to_string(), "red eyes".to_string()]), "xk, 1girl, smile");
        assert_eq!(drop_tags("a, b", &[]), "a, b");
    }

    #[test]
    fn style_noise_words() {
        for t in ["masterpiece", "best quality", "score_9", "year 2024", "newest", "@someone", "watercolor (medium)", "signature", "anime coloring", "absurdres"] {
            assert!(style_noise(t), "{t}");
        }
        for t in ["1girl", "blue hair", "smile", "outdoors"] {
            assert!(!style_noise(t), "{t}");
        }
    }

    #[test]
    fn samples_start_with_trigger() {
        for f in FAMS {
            for kind in KINDS {
                let s = sample_prompts(f, kind, "xktrig");
                assert_eq!(s.len(), 4, "{} {kind}", f.id);
                assert!(s.iter().all(|x| x.starts_with("xktrig, ")), "{s:?}");
                let uniq: std::collections::HashSet<&String> = s.iter().collect();
                assert_eq!(uniq.len(), 4);
            }
        }
        // tag 家族給 tag（沒有句點），句子家族給句子
        assert!(!sample_prompts(fam("anima").unwrap(), "style", "x1").iter().any(|s| s.contains('.')));
        assert!(sample_prompts(fam("qwen21").unwrap(), "style", "x1").iter().all(|s| s.contains(' ')));
    }

    fn proj(family: &str, images: usize) -> Value {
        let mut p = new_project("測試", "style", family, "jgdxstyle").unwrap();
        let f = fam(family).unwrap();
        p["params"] = auto_params(f, "style", images, "std");
        p["samples"] = json!(sample_prompts(f, "style", "jgdxstyle"));
        p
    }

    #[test]
    fn config_has_everything() {
        let s = fam("sdxl").unwrap();
        let y = toolkit_config(&proj("sdxl", 62), s, "D:\\x\\data", "D:\\x\\out", "D:\\m\\sdxl.safetensors", 24.0);
        for want in [
            "job: extension",
            "type: \"sd_trainer\"",
            "name: \"jgdxstyle\"",
            "arch: \"sdxl\"",
            "steps: 1250",
            "save_every: 250",
            "sample_every: 250",
            "max_step_saves_to_keep: 5",
            "folder_path: \"D:\\\\x\\\\data\"",
            "training_folder: \"D:\\\\x\\\\out\"",
            "name_or_path: \"D:\\\\m\\\\sdxl.safetensors\"",
            "noise_scheduler: \"ddpm\"",
            "caption_ext: \"txt\"",
            "caption_dropout_rate: 0.05",
            "lr: 0.0001",
            "optimizer: \"adamw8bit\"",
            "flip_x: true",
            "skip_first_sample: true",
            "- \"jgdxstyle, ",
        ] {
            assert!(y.contains(want), "少了 {want}\n{y}");
        }
        assert!(!y.contains("low_vram: true"), "{y}");
        assert!(!y.contains("quantize: true"), "SDXL 不量化\n{y}");
        assert!(!y.contains('\t'));

        let q = fam("qwen21").unwrap();
        let y = toolkit_config(&proj("qwen21", 100), q, "/w/data", "/w/out", q.base_hint, 24.0);
        for want in ["noise_scheduler: \"flowmatch\"", "linear: 32", "linear_alpha: 32", "arch: \"qwen_image_2\"", "assistant_lora_path: \"ostris/qwen_image_2_turbo_training_adapter", "quantize: true", "qtype: \"convrot8\"", "turbo: true", "timestep_type: \"shift\""] {
            assert!(y.contains(want), "少了 {want}\n{y}");
        }
        assert!(toolkit_config(&proj("sdxl", 62), s, "d", "o", "m", 8.0).contains("low_vram: true"));
        let h = fam("h3").unwrap();
        let y = toolkit_config(&proj("h3", 40), h, "/w/data", "/w/out", h.base_hint, 48.0);
        for want in ["arch: \"minimax_h3\"", "num_frames: 39", "do_audio: true", "fps: 24", "linear: 16"] {
            assert!(y.contains(want), "少了 {want}\n{y}");
        }
        assert!(!y.contains("flip_x: true"));
    }

    #[test]
    fn config_escapes_quotes() {
        let s = fam("sdxl").unwrap();
        let mut p = proj("sdxl", 62);
        p["samples"] = json!(["jgdxstyle, a sign that says \"hi\"\nsecond line"]);
        let y = toolkit_config(&p, s, "d", "o", "m", 24.0);
        assert!(y.contains("- \"jgdxstyle, a sign that says \\\"hi\\\" second line\""), "{y}");
    }

    fn scored(v: &[(&str, f64)]) -> Vec<(String, f64)> {
        v.iter().map(|(t, s)| (t.to_string(), *s)).collect()
    }

    #[test]
    fn tags_caption_by_kind() {
        let general = scored(&[("smile", 0.95), ("1girl", 0.9), ("blue hair", 0.9), ("signature", 0.8), ("anime coloring", 0.7), ("masterpiece", 0.6), ("outdoors", 0.5)]);
        let chars = scored(&[("hu tao \\(genshin impact\\)", 0.9)]);
        // 畫風：內容照實寫（角色名也留）、畫風／品質／雜訊拿掉、括號還原
        assert_eq!(tags_caption(&general, &chars, "style"), "1girl, smile, blue hair, outdoors, hu tao (genshin impact)");
        // 角色：角色名交給觸發詞，不寫
        assert_eq!(tags_caption(&general, &chars, "character"), "1girl, smile, blue hair, anime coloring, outdoors");
        assert_eq!(tags_caption(&[], &[], "style"), "");
    }

    #[test]
    fn caption_system_by_kind() {
        assert!(caption_system("character", false).contains("hair"));
        assert!(caption_system("style", false).contains("art style"));
        assert!(caption_system("style", true).contains("motion"));
        assert!(!caption_system("concept", false).contains("motion"));
    }

    #[test]
    fn epoch_names_sorted() {
        let files: Vec<String> = ["x/x.safetensors", "x/x_000000500.safetensors", "x/optimizer.pt", "x/x_000000250.safetensors", "x/samples/a.jpg", "y/y_000000100.safetensors", "x/xx_000000100.safetensors"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            epoch_names(&files, "x"),
            vec![(1, "x/x_000000250.safetensors".to_string()), (2, "x/x_000000500.safetensors".to_string()), (3, "x/x.safetensors".to_string())]
        );
        assert!(epoch_names(&files, "nope").is_empty());
        assert_eq!(sample_step("x/samples/1731200000__000000250_2.jpg"), Some((250, 2)));
        assert_eq!(sample_step("x/samples/readme.txt"), None);
    }

    #[test]
    fn estimate_is_rough_but_sane() {
        let q = fam("qwen21").unwrap();
        let (hours, usd) = estimate(q, 3000, 0.7);
        // 3000 步 × 3.5 秒 ＋ 開機裝環境 15 分 ≈ 3.17 小時
        assert!((hours - 3.1667).abs() < 0.01, "{hours}");
        assert!((usd - hours * 0.7).abs() < 1e-9);
        assert!(estimate(fam("sdxl").unwrap(), 500, 0.3).0 < 0.5);
    }

    #[test]
    fn civitai_body_matches_old_script() {
        let mut p = new_project("我的畫風", "style", "anima", "jgdxstyle").unwrap();
        p["params"] = json!({"steps": 1500, "epochs": 5});
        let items = vec![("urn:air:other:other:civitai-r2:civitai-prod-settled@a.png".to_string(), "jgdxstyle, 1girl".to_string())];
        let b = civitai_body(&p, fam("anima").unwrap(), &items);
        // 跟舊站 tools\civitai_train.py 的 body() 同一組輸入：只有 tags 的第一個從 anima-ui 換成 paintdeck
        assert_eq!(
            b,
            json!({"tags": ["paintdeck", "jgdxstyle"], "steps": [{"$type": "training", "input": {
                "engine": "ai-toolkit", "ecosystem": "anima", "steps": 1500, "epochs": 5,
                "trainingData": {"type": "blobs", "items": [{"air": "urn:air:other:other:civitai-r2:civitai-prod-settled@a.png", "caption": "jgdxstyle, 1girl"}]}}}]})
        );
        assert_eq!(civitai_body(&p, fam("moss").unwrap(), &items)["steps"][0]["input"]["ecosystem"], "mossImage");
    }

    #[test]
    fn compare_jobs_cross_product_same_seed() {
        let j = compare_jobs(&[2, 5], &[0.6, 1.0], 42);
        assert_eq!(j, vec![(2, 0.6, 42), (2, 1.0, 42), (5, 0.6, 42), (5, 1.0, 42)]);
        assert!(compare_jobs(&[], &[1.0], 1).is_empty());
    }

    #[test]
    fn compare_block_catches_silent_lora_drop() {
        let sdxl = fam("sdxl").unwrap();
        // 家族對得上：放行
        assert_eq!(compare_block(sdxl, "sdxl", "sdxl", false), None);
        // 生成頁選的是別家模型：LoRA 會被生圖流程略過，要先講
        let e = compare_block(sdxl, "anima", "sdxl", false).unwrap();
        assert!(e.contains("SDXL") && e.contains("生成頁"), "{e}");
        // 模型對、LoRA 檔頭卻被認成別家（或認不出來）：一樣會被略過
        let e = compare_block(sdxl, "sdxl", "other", false).unwrap();
        assert!(e.contains("LoRA"), "{e}");
        // 生成頁選的是匯入的工作流組：生圖流程不照家族篩，放行
        assert_eq!(compare_block(sdxl, "other", "other", true), None);
        // 影片家族沒有生圖對照表
        let e = compare_block(fam("h3").unwrap(), "other", "other", false).unwrap();
        assert!(e.contains("影片"), "{e}");
    }

    #[test]
    fn stale_runs_are_marked() {
        let mk = |target: &str, state: &str, remote: Value| json!({"run": {"target": target, "state": state, "remote": remote}});
        // 本機：繪匣重開後小幫手已經不在 → 中斷
        let mut p = mk("local", "running", Value::Null);
        fix_stale(&mut p, false);
        assert_eq!(p["run"]["state"], "interrupted");
        // 真的還在跑就不動
        let mut p = mk("local", "running", Value::Null);
        fix_stale(&mut p, true);
        assert_eq!(p["run"]["state"], "running");
        // 雲端／C 站：外面還在跑，標成可以接回去
        for (t, r) in [("runpod", json!({"pod": "abc"})), ("civitai", json!({"workflow": "w1"}))] {
            let mut p = mk(t, "starting", r);
            fix_stale(&mut p, false);
            assert_eq!(p["run"]["state"], "detached", "{t}");
        }
        // 沒留下遠端編號的就是中斷
        let mut p = mk("runpod", "starting", Value::Null);
        fix_stale(&mut p, false);
        assert_eq!(p["run"]["state"], "interrupted");
        // 已經結束的、沒跑過的不動
        let mut p = mk("local", "done", Value::Null);
        fix_stale(&mut p, false);
        assert_eq!(p["run"]["state"], "done");
        let mut p = json!({"run": null});
        fix_stale(&mut p, false);
        assert_eq!(p["run"], Value::Null);
    }

    #[test]
    fn boot_script_has_pinned_commit_and_port() {
        let s = boot_script(8000);
        assert!(s.contains(TOOLKIT_COMMIT) && s.contains("--port 8000") && s.contains("$TRAIN_TOKEN") && s.contains("$TRAIN_AGENT_B64") && s.contains("-m manager install"), "{s}");
        assert!(!s.contains('\r'));
    }
}

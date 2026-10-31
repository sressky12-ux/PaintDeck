//! 判斷模型與 LoRA 屬於哪個家族，只讀檔頭，不載入權重。
//! 檔名不可靠（社群微調常取 Noct Q、REDQW21 之類的名字），所以看權重名稱（再用 metadata 補）：
//!   Anima（Cosmos predict2）  blocks.N.*、lora_unet_blocks_N_*、llm_adapter.*
//!   Qwen-Image 2.1（MMDiT）   transformer_blocks.N.*、txt_in.text_norm
//!   MOSS-Image（5B DiT）      shared_block_modulation、layers.N.proj.ffn_up_gate（跟 ComfyUI model_detection 一樣；
//!                              它的文字端也有 .blocks.，所以要比 Anima 先判斷）
//!   SDXL（含 Pony、Illustrious、NoobAI、Animagine，架構都一樣）
//!                             conditioner.embedders.*、label_emb、add_embedding、UNet 的 transformer_blocks.1 以後、
//!                              input_blocks 只到 8、up_blocks.0.attentions；LoRA 有 lora_te2_ / lora_te1_
//!   SD 1.5（含 2.x）          cond_stage_model.*、input_blocks 到 11、down_blocks.3、down_blocks.0.attentions；LoRA 有 lora_te_text_model_
//!   Flux.1                    double_blocks.N.*、single_blocks.N.*（lora_unet_double_blocks_*、diffusion_model.double_blocks.*）、
//!                              diffusers 格式的 single_transformer_blocks.*
//!   SD3／3.5                  joint_blocks.N.*（lora_unet_joint_blocks_*）
//!
//! 判斷順序（Anima、Qwen、MOSS 的規則一字不改，只在前面插入新家族的「結構證據」）：
//!   MOSS → Qwen 的 txt_in.text_norm → Flux／SD3 → SDXL／SD1.5 → （有 UNet 命名卻分不出 SD1.5／SDXL：metadata，否則 other）
//!   → Qwen 的 transformer_blocks → Anima → metadata → other。
//!   SD 的 UNet 與 Flux／SD3 的 diffusers 格式也有 transformer_blocks，所以一定要比舊的 Qwen 規則先判斷。
//!   SD1.5 與 SDXL 之間，結構證據（區塊編號、transformer 深度）比 label_emb、文字編碼器名稱可靠。
//!   metadata（modelspec.architecture、kohya 的 ss_base_model_version…）只在權重名稱看不出家族時才用，
//!   而且只會得到 sdxl／sd15／flux／sd3，不會蓋掉 anima／qwen21／moss。
//!
//! 檔案：.safetensors、.sft 讀 safetensors 檔頭；.gguf 讀 GGUF 的張量名稱（只認新的四個家族，其餘一律 other）；
//! .ckpt／.pt 是 pickle，讀不了，一律 other。
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

pub const ANIMA: &str = "anima";
pub const QWEN21: &str = "qwen21";
pub const MOSS: &str = "moss";
pub const SD15: &str = "sd15";
pub const SDXL: &str = "sdxl";
pub const FLUX: &str = "flux";
pub const SD3: &str = "sd3";
pub const OTHER: &str = "other";
/// 顯示名稱（順序就是生成頁模型選單的順序：內建的三個在前面）
pub const FAMILY_NAMES: [(&str, &str); 7] = [
    (ANIMA, "Anima（動漫）"),
    (QWEN21, "Qwen 2.1（編輯）"),
    (MOSS, "MOSS-Image（文生圖繪圖）"),
    (SDXL, "SDXL（含 Pony、Illustrious、NoobAI）"),
    (SD15, "SD 1.5"),
    (FLUX, "Flux"),
    (SD3, "SD3"),
];
/// 生圖站內建流程（graphs.rs）真的會畫的家族；其他家族只能走匯入的工作流
pub const BUILTIN: [&str; 3] = [ANIMA, QWEN21, MOSS];

pub fn is_builtin(family: &str) -> bool {
    BUILTIN.contains(&family)
}

/// 家族代號 → 顯示名稱；不認得的回「其他」
pub fn label(family: &str) -> &'static str {
    FAMILY_NAMES
        .iter()
        .find(|(k, _)| *k == family)
        .map(|(_, l)| *l)
        .unwrap_or("其他")
}

/// 檔案判斷結果：家族＋（SDXL 才有的）子標籤
#[derive(Clone, Copy)]
struct Info {
    family: &'static str,
    sub: Option<&'static str>,
    /// LTX 影片模型／LoRA（family 是 other）
    ltx: bool,
}

const NO_INFO: Info = Info {
    family: OTHER,
    sub: None,
    ltx: false,
};

static CACHE: Mutex<Option<HashMap<(PathBuf, SystemTime), Info>>> = Mutex::new(None);

fn cache() -> MutexGuard<'static, Option<HashMap<(PathBuf, SystemTime), Info>>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- 讀檔頭
/// 檔頭裡有用的 metadata（全部轉小寫；沒有就是空字串）
#[derive(Default)]
struct Meta {
    /// modelspec.architecture，例如 stable-diffusion-xl-v1-base、flux-1-dev
    arch: String,
    /// modelspec.title
    title: String,
    /// modelspec.description
    desc: String,
    /// kohya 的 ss_base_model_version，例如 sdxl_base_v1-0、sd_v1
    base_ver: String,
    /// kohya 的 ss_sd_model_name（訓練時的底模檔名）
    base_name: String,
    /// kohya 的 ss_network_module，例如 networks.lora_flux
    net_module: String,
    /// kohya 的 ss_output_name
    out_name: String,
    /// ss_tag_frequency 裡有 score_9（Pony 系的標籤）
    score9: bool,
    /// GGUF 的 general.architecture
    gguf_arch: String,
    /// LTX 的 IC-LoRA（要配參考影片）：檔頭有 reference_downscale_factor
    ic_lora: bool,
}

#[derive(Default)]
struct Header {
    keys: Vec<String>,
    meta: Meta,
}

fn meta_from_obj(o: &serde_json::Map<String, Value>) -> Meta {
    let s = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .unwrap_or_default()
    };
    Meta {
        arch: s("modelspec.architecture"),
        title: s("modelspec.title"),
        desc: s("modelspec.description"),
        base_ver: s("ss_base_model_version"),
        base_name: s("ss_sd_model_name"),
        net_module: s("ss_network_module"),
        out_name: s("ss_output_name"),
        score9: o
            .get("ss_tag_frequency")
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains("score_9")),
        gguf_arch: String::new(),
        ic_lora: o.keys().any(|k| k.ends_with("reference_downscale_factor")),
    }
}

fn is_gguf(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().ends_with(".gguf")
}

fn is_safetensors(path: &Path) -> bool {
    let p = path.to_string_lossy().to_lowercase();
    p.ends_with(".safetensors") || p.ends_with(".sft")
}

fn read_header(path: &Path) -> io::Result<Header> {
    if is_gguf(path) {
        return gguf_header(path);
    }
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut len = [0u8; 8];
    f.read_exact(&mut len)?;
    let n = u64::from_le_bytes(len);
    if n == 0 || n > 200 << 20 || n > size.saturating_sub(8) {
        return Ok(Header::default()); // 檔頭異常大（或比檔案還大）：不是正常的 safetensors
    }
    let mut buf = vec![0u8; n as usize];
    f.read_exact(&mut buf)?;
    let v: Value = serde_json::from_slice(&buf).map_err(io::Error::other)?;
    let Some(o) = v.as_object() else {
        return Ok(Header::default());
    };
    Ok(Header {
        keys: o.keys().filter(|k| *k != "__metadata__").cloned().collect(),
        meta: o
            .get("__metadata__")
            .and_then(Value::as_object)
            .map(meta_from_obj)
            .unwrap_or_default(),
    })
}

// ---------------------------------------------------------------- GGUF（不加套件，自己讀檔頭）
// 格式：GGUF、版本 u32、張量數 u64、KV 數 u64、KV…、張量資訊…（名稱 string、維度數 u32、維度 u64×n、型別 u32、位移 u64）
fn bad_gguf() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "GGUF 格式不對")
}

fn rd_u32(r: &mut BufReader<File>) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn rd_u64(r: &mut BufReader<File>) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn skip(r: &mut BufReader<File>, n: u64) -> io::Result<()> {
    r.seek_relative(i64::try_from(n).map_err(|_| bad_gguf())?)
}

/// 讀一個字串；超過 cap 的跳過（回 None）
fn rd_str(r: &mut BufReader<File>, cap: u64) -> io::Result<Option<String>> {
    let n = rd_u64(r)?;
    if n > cap {
        skip(r, n)?;
        return Ok(None);
    }
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b)?;
    Ok(Some(String::from_utf8_lossy(&b).into_owned()))
}

fn fixed_size(ty: u32) -> Option<u64> {
    match ty {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

fn skip_value(r: &mut BufReader<File>, ty: u32, depth: u8) -> io::Result<()> {
    match ty {
        8 => {
            let n = rd_u64(r)?;
            skip(r, n)
        }
        9 => {
            if depth > 2 {
                return Err(bad_gguf());
            }
            let (et, n) = (rd_u32(r)?, rd_u64(r)?);
            match et {
                8 | 9 => {
                    if n > 10_000_000 {
                        return Err(bad_gguf());
                    }
                    for _ in 0..n {
                        skip_value(r, et, depth + 1)?;
                    }
                    Ok(())
                }
                _ => skip(
                    r,
                    n.checked_mul(fixed_size(et).ok_or_else(bad_gguf)?)
                        .ok_or_else(bad_gguf)?,
                ),
            }
        }
        _ => skip(r, fixed_size(ty).ok_or_else(bad_gguf)?),
    }
}

fn gguf_header(path: &Path) -> io::Result<Header> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Ok(Header::default());
    }
    if !(2..=3).contains(&rd_u32(&mut r)?) {
        return Ok(Header::default());
    }
    let (n_tensors, n_kv) = (rd_u64(&mut r)?, rd_u64(&mut r)?);
    if n_tensors > 1_000_000 || n_kv > 100_000 {
        return Ok(Header::default());
    }
    let mut h = Header::default();
    for _ in 0..n_kv {
        let key = rd_str(&mut r, 1 << 16)?.ok_or_else(bad_gguf)?;
        let ty = rd_u32(&mut r)?;
        if ty == 8 {
            let v = rd_str(&mut r, 1 << 16)?.unwrap_or_default().to_lowercase();
            match key.as_str() {
                "general.architecture" => h.meta.gguf_arch = v,
                "general.name" => h.meta.title = v,
                _ => {}
            }
        } else {
            skip_value(&mut r, ty, 0)?;
        }
    }
    for _ in 0..n_tensors {
        let name = rd_str(&mut r, 1 << 16)?.ok_or_else(bad_gguf)?;
        let dims = rd_u32(&mut r)?;
        if dims > 8 {
            return Err(bad_gguf());
        }
        skip(&mut r, 8 * u64::from(dims) + 4 + 8)?;
        h.keys.push(name);
    }
    Ok(h)
}

// ---------------------------------------------------------------- 分類
fn find(t: &[&str], pat: &[&str]) -> Option<usize> {
    if pat.is_empty() || t.len() < pat.len() {
        return None;
    }
    (0..=t.len() - pat.len()).find(|&i| t[i..i + pat.len()] == *pat)
}

fn has(t: &[&str], pat: &[&str]) -> bool {
    find(t, pat).is_some()
}

/// 第一次出現 pat 之後的 token
fn after<'a>(t: &'a [&'a str], pat: &[&str]) -> Option<&'a [&'a str]> {
    find(t, pat).map(|i| &t[i + pat.len()..])
}

fn num(r: &[&str], i: usize) -> Option<u32> {
    r.get(i)?.parse().ok()
}

fn bump(slot: &mut Option<u32>, n: u32) {
    *slot = Some(slot.map_or(n, |m| m.max(n)));
}

/// 掃一遍權重名稱得到的證據
#[derive(Default)]
struct Probe {
    // 舊規則（用原始權重名稱，一字不改）
    moss: bool,
    qwen_strong: bool,
    qwen_weak: bool,
    anima: bool,
    // Flux／SD3 一族
    dbl_max: Option<u32>,
    sgl_max: Option<u32>,
    flux_diff: bool,
    joint: bool,
    tb_max: Option<u32>,
    ctx_stream: bool,
    pos_embed: bool,
    hunyuan: bool,
    flux2: bool,
    // SD UNet 一族
    in_max: Option<u32>,
    out_max: Option<u32>,
    unet_ctx: bool,
    /// transformer_blocks 有第 2 層以上（只有 SDXL 才有）
    depth2: bool,
    /// 結構證據：編號、區塊數量不可能是另一邊
    sdxl_struct: bool,
    sd15_struct: bool,
    /// 軟證據：文字編碼器、條件嵌入（SD2 unCLIP、SDPose 之類的 SD2 改版也有 label_emb，所以不如結構可靠）
    sdxl_soft: bool,
    sd15_soft: bool,
    // 影片模型（Wan 2.x、LTX-Video／LTX-2）：架構名稱跟 Anima（.blocks.）、Qwen（transformer_blocks）撞名，要先認出來排除
    wan: bool,
    ltx: bool,
    /// transformer_blocks 底下有 attn1／attn2（LTX 的命名；Qwen-Image 是 attn、SD 的 UNet 才有 input_blocks 之類的結構）
    attn12: bool,
    /// Anima 自己才有的名稱（Cosmos predict2 的 adaln_modulation、llm_adapter）
    anima_strong: bool,
}

impl Probe {
    fn new(keys: &[String]) -> Probe {
        let mut p = Probe::default();
        for k in keys {
            p.legacy(k);
            p.structure(k);
        }
        p
    }

    fn legacy(&mut self, k: &str) {
        self.moss |= k.contains("shared_block_modulation") || k.contains(".proj.ffn_up_gate");
        self.qwen_strong |= k.contains("txt_in.text_norm");
        self.qwen_weak |= k.contains("transformer_blocks");
        self.anima_strong |= k.contains("llm_adapter") || k.contains("adaln_modulation");
        self.anima |= k.contains("llm_adapter")
            || k.contains("adaln_modulation")
            || k.contains(".blocks.")
            || k.starts_with("blocks.")
            || k.starts_with("lora_unet_blocks_");
    }

    /// 名稱切成小寫 token（. 和 _ 都當分隔），這樣 checkpoint 的 input_blocks.4.1、
    /// kohya LoRA 的 lora_unet_input_blocks_4_1、diffusers 的 unet.down_blocks.0 都用同一套規則看
    fn structure(&mut self, k: &str) {
        let low = k.to_ascii_lowercase();
        let t: Vec<&str> = low.split(['.', '_']).filter(|s| !s.is_empty()).collect();
        let t = &t[..];
        self.video_evidence(t);
        // MOSS 的 LoRA 也有 kohya 底線版（lora_unet_layers_0_proj_qkv）和只練注意力層的（沒有 ffn_up_gate）：認 layers.N.proj.<qkv|q|k|v|o|ffn>
        self.moss |= has(t, &["shared", "block", "modulation"])
            || after(t, &["layers"]).is_some_and(|r| num(r, 0).is_some() && r.get(1) == Some(&"proj") && r.get(2).is_some_and(|x| matches!(*x, "qkv" | "q" | "k" | "v" | "o" | "ffn")));
        // Flux／SD3／HunyuanVideo 一族
        if let Some(n) = after(t, &["double", "blocks"]).and_then(|r| num(r, 0)) {
            bump(&mut self.dbl_max, n);
        }
        if let Some(n) = after(t, &["single", "blocks"]).and_then(|r| num(r, 0)) {
            bump(&mut self.sgl_max, n);
        }
        self.flux_diff |= has(t, &["single", "transformer", "blocks"]);
        self.joint |= has(t, &["joint", "blocks"]);
        self.pos_embed |= has(t, &["pos", "embed"]);
        self.ctx_stream |= has(t, &["ff", "context"]) || has(t, &["norm1", "context"]);
        // HunyuanVideo 的 double／single_blocks 跟 Flux 同名，但 mod／mlp／modulation 的名字不同
        self.hunyuan |= has(t, &["individual", "token", "refiner"])
            || has(t, &["img", "mod", "linear"])
            || has(t, &["img", "mlp", "fc1"])
            || has(t, &["modulation", "linear"]);
        self.flux2 |= has(t, &["double", "stream", "modulation"]);
        let tb = find(t, &["transformer", "blocks"])
            .filter(|&i| i == 0 || t[i - 1] != "single")
            .and_then(|i| num(&t[i + 2..], 0));
        if let Some(n) = tb {
            bump(&mut self.tb_max, n);
        }
        // 文字編碼器與條件嵌入
        if has(t, &["label", "emb"])
            || has(t, &["add", "embedding"])
            || has(t, &["conditioner", "embedders"])
            || has(t, &["te2", "text", "model"])
            || has(t, &["text", "encoder", "2", "text", "model"])
        {
            self.sdxl_soft = true;
        }
        if has(t, &["cond", "stage", "model"]) || has(t, &["lora", "te", "text", "model"]) {
            self.sd15_soft = true;
        }
        // UNet：VAE（encoder／decoder 裡也有 up_blocks、down_blocks）與 ControlNet 的零卷積不算
        if t.iter()
            .any(|x| matches!(*x, "encoder" | "decoder" | "controlnet" | "first"))
        {
            return;
        }
        let mut unet = false;
        if let Some(r) = after(t, &["input", "blocks"]) {
            unet = true;
            if let Some(n) = num(r, 0) {
                bump(&mut self.in_max, n);
                // SD1.x 的 input_blocks.1、.2 有注意力（.1）、.9 以後只有 SD1.x 才有；SDXL 的 input_blocks 只到 8
                if n >= 9 || ((n == 1 || n == 2) && num(r, 1) == Some(1)) {
                    self.sd15_struct = true;
                }
            }
        }
        if let Some(r) = after(t, &["output", "blocks"]) {
            unet = true;
            if let Some(n) = num(r, 0) {
                bump(&mut self.out_max, n);
                if n >= 9 {
                    self.sd15_struct = true;
                }
                // SDXL 的 output_blocks.0、.1 有注意力（.1），SD1.x 的這兩個只有 ResBlock
                if n <= 1 && num(r, 1) == Some(1) {
                    self.sdxl_struct = true;
                }
            }
        }
        unet |= has(t, &["middle", "block"]) || has(t, &["mid", "block"]);
        // diffusers 命名：SD1.x 有 4 個 down／up block（down_blocks.0 就有注意力、up_blocks.0 沒有），SDXL 只有 3 個
        if let Some(r) = after(t, &["down", "blocks"]) {
            unet = true;
            if num(r, 0) == Some(3) || (num(r, 0) == Some(0) && r.get(1) == Some(&"attentions")) {
                self.sd15_struct = true;
            }
        }
        if let Some(r) = after(t, &["up", "blocks"]) {
            unet = true;
            let attn = r.get(1) == Some(&"attentions");
            match num(r, 0) {
                Some(3) => self.sd15_struct = true,
                Some(2) if attn => self.sd15_struct = true,
                Some(0) if attn => self.sdxl_struct = true,
                _ => {}
            }
        }
        // SD1.x／2.x 的 transformer_blocks 永遠只有 0 一層；SDXL 的有 2 層和 10 層
        if unet && tb.is_some_and(|n| n >= 1) {
            self.depth2 = true;
        }
        self.unet_ctx |= unet;
    }

    /// Wan、LTX 的權重名稱特徵（ComfyUI model_detection.py 也是靠這些認的）
    fn video_evidence(&mut self, t: &[&str]) {
        // Wan 2.x：head.modulation；注意力叫 self_attn.q／.k／.v／.o（Anima 是 q_proj），前饋叫 ffn.0／ffn.2（Anima 是 mlp.layer1）；
        // i2v 有 cross_attn.k_img。patch_embedding、time_projection、text_embedding 別的模型（CLIP Vision…）也有，不算證據
        let qkvo = |x: &&str| matches!(*x, "q" | "k" | "v" | "o");
        let attn_plain = |name: &str| {
            find(t, &[name, "attn"]).is_some_and(|i| t.get(i + 2).is_some_and(qkvo) && t.get(i + 3) != Some(&"proj"))
        };
        self.wan |= has(t, &["head", "modulation"])
            || has(t, &["k", "img"])
            || find(t, &["ffn"]).is_some_and(|i| matches!(t.get(i + 1), Some(&"0") | Some(&"2")))
            || attn_plain("self")
            || attn_plain("cross");
        // LTX-Video、LTX-2（含音訊）：adaln_single、scale_shift_table、patchify_proj、caption_projection、audio／video 互相注意
        self.ltx |= has(t, &["adaln", "single"])
            || has(t, &["scale", "shift", "table"])
            || has(t, &["patchify", "proj"])
            || has(t, &["caption", "projection"])
            || has(t, &["audio", "to", "video"])
            || has(t, &["video", "to", "audio"]);
        if find(t, &["transformer", "blocks"]).is_some() && (has(t, &["attn1"]) || has(t, &["attn2"])) {
            self.attn12 = true;
        }
    }

    /// 影片模型：不是 Anima 也不是 Qwen，也不是任何生圖家族
    fn is_video(&self) -> bool {
        (self.wan && !self.anima_strong) || self.ltx || (self.attn12 && !self.unet_ctx && !self.depth2 && self.dit_family().is_none())
    }

    /// 影片模型裡的 LTX（LTX-Video／LTX-2）：有 LTX 自己的名稱，或 transformer_blocks.N.attn1／attn2 又不是 Wan
    fn is_ltx(&self) -> bool {
        self.ltx || (self.is_video() && self.attn12 && !self.wan)
    }

    /// Flux／SD3（DiT）
    fn dit_family(&self) -> Option<&'static str> {
        if self.hunyuan || self.flux2 {
            return None;
        }
        if self.joint {
            return Some(SD3);
        }
        if self.dbl_max.is_some() || self.sgl_max.is_some() {
            // Flux.1 是 19 個 double（0..18）＋38 個 single（0..37）；比這多的是 HunyuanVideo、Flux.2 之類
            let ok = self.dbl_max.unwrap_or(0) <= 18 && self.sgl_max.unwrap_or(0) <= 37;
            return ok.then_some(FLUX);
        }
        if self.flux_diff {
            return Some(FLUX);
        }
        // diffusers 格式只有雙流區塊：Flux 與 SD3 都有 ff_context（Qwen-Image 沒有）。
        // SD3 有 pos_embed、而且至少 24 個區塊；Flux 只有 19 個
        if self.ctx_stream && self.tb_max.is_some() {
            let sd3 = self.pos_embed || self.tb_max.is_some_and(|n| n >= 19);
            return Some(if sd3 { SD3 } else { FLUX });
        }
        None
    }

    /// SDXL／SD1.5
    fn unet_family(&self) -> Option<&'static str> {
        // 結構證據優先（SD2 系的 UNet 加了 label_emb 還是 SD1.x 的結構：input_blocks 到 11）
        if self.sdxl_struct || self.depth2 {
            return Some(SDXL);
        }
        if self.sd15_struct {
            return Some(SD15);
        }
        if self.sdxl_soft {
            return Some(SDXL);
        }
        if self.sd15_soft {
            return Some(SD15);
        }
        // 沒有決定性證據、但用原始 UNet 命名（input_blocks_4_1…）而且沒有 SD1.x 才有的編號：
        // 會這樣命名 LoRA 的是 kohya 訓練 SDXL（SD1.x 的 kohya LoRA 用 down_blocks 命名）
        let weak = self.in_max.is_some_and(|n| n >= 4) || self.out_max.is_some_and(|n| n >= 3);
        weak.then_some(SDXL)
    }
}

/// metadata 判斷（權重名稱看不出來時才用）
fn meta_family(m: &Meta) -> Option<&'static str> {
    let a = m.arch.as_str();
    if a.contains("flux-2") || a.contains("flux.2") || a.contains("flux2") {
        return None; // Flux.2 的 LoRA 跟 Flux.1 不通用
    }
    if a.contains("stable-diffusion-xl") || a.contains("sdxl") {
        return Some(SDXL);
    }
    if a.contains("stable-diffusion-3") || a.contains("stable-diffusion-v3") || a.contains("sd3") {
        return Some(SD3);
    }
    if a.contains("stable-diffusion-v1") || a.contains("stable-diffusion-v2") {
        return Some(SD15);
    }
    if a.contains("flux") {
        return Some(FLUX);
    }
    let b = m.base_ver.as_str();
    if b.starts_with("sdxl") {
        return Some(SDXL);
    }
    if b.starts_with("sd_v") {
        return Some(SD15);
    }
    if b.contains("flux") {
        return Some(FLUX);
    }
    if b.contains("sd3") {
        return Some(SD3);
    }
    let n = m.net_module.as_str();
    if n.contains("flux") {
        return Some(FLUX);
    }
    if n.contains("sd3") {
        return Some(SD3);
    }
    match m.gguf_arch.as_str() {
        "flux" => Some(FLUX),
        "sdxl" => Some(SDXL),
        "sd1" | "sd2" => Some(SD15),
        "sd3" => Some(SD3),
        _ => None,
    }
}

fn classify_with(keys: &[String], meta: Option<&Meta>) -> &'static str {
    let p = Probe::new(keys);
    if p.moss {
        MOSS
    } else if p.qwen_strong {
        QWEN21
    } else if p.is_video() {
        OTHER
    } else if let Some(f) = p.dit_family().or_else(|| p.unet_family()) {
        f
    } else if p.unet_ctx {
        // UNet 的命名（input_blocks、down_blocks…）但看不出是 SD1.x 還是 SDXL：不是 Qwen 也不是 Anima，只能靠 metadata
        meta.and_then(meta_family).unwrap_or(OTHER)
    } else if p.qwen_weak {
        QWEN21
    } else if p.anima {
        ANIMA
    } else {
        meta.and_then(meta_family).unwrap_or(OTHER)
    }
}

/// 只看權重名稱
fn classify(keys: &[String]) -> &'static str {
    classify_with(keys, None)
}

/// SDXL 子標籤：從 modelspec.title、ss_sd_model_name（訓練底模檔名）、modelspec.description、ss_output_name 找品牌字；
/// 都沒有時，ss_tag_frequency 裡有 score_9 就是 Pony 系
fn sub_of(m: &Meta) -> Option<&'static str> {
    [&m.title, &m.base_name, &m.desc, &m.out_name]
        .into_iter()
        .find_map(|h| brand(h))
        .or(m.score9.then_some("Pony"))
}

fn brand(text: &str) -> Option<&'static str> {
    let c: String = text
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' ' | '.'))
        .collect();
    let c = c.replace("ponytail", "");
    if c.contains("noobai") {
        Some("NoobAI")
    } else if c.contains("illustrious") {
        Some("Illustrious")
    } else if c.contains("pony") || c.contains("pdxl") {
        Some("Pony")
    } else if c.contains("animagine") {
        Some("Animagine")
    } else {
        None
    }
}

fn info_of(path: &Path) -> Info {
    let Ok(h) = read_header(path) else {
        return NO_INFO;
    };
    let mut family = classify_with(&h.keys, Some(&h.meta));
    // GGUF 以前一律是 other：只開放新的四個家族，Anima／Qwen／MOSS 的 GGUF 維持 other
    if is_gguf(path) && !matches!(family, SD15 | SDXL | FLUX | SD3) {
        family = OTHER;
    }
    Info {
        family,
        sub: if family == SDXL {
            sub_of(&h.meta)
        } else {
            None
        },
        ltx: family == OTHER && !h.meta.ic_lora && is_ltx_keys(&h.keys),
    }
}

fn cached(path: &Path) -> Info {
    if !is_safetensors(path) && !is_gguf(path) {
        return NO_INFO;
    }
    let Ok(mtime) = path.metadata().and_then(|m| m.modified()) else {
        return NO_INFO;
    };
    let key = (path.to_path_buf(), mtime);
    if let Some(i) = cache().get_or_insert_with(HashMap::new).get(&key) {
        return *i;
    }
    let info = info_of(path); // 讀檔不要拿著鎖
    cache().get_or_insert_with(HashMap::new).insert(key, info);
    info
}

// ---------------------------------------------------------------- 對外
/// 已經讀好的權重名稱（model_dirs 判斷分類時順便判斷家族）。只看權重名稱，沒有 metadata 可以補；
/// 要讓 metadata 也參與，請改用 [`detect`]（讀檔頭、有快取）
pub fn detect_keys(keys: &[String]) -> &'static str {
    classify(keys)
}

/// 這些權重名稱是不是影片模型（Wan 2.x、LTX-Video／LTX-2）。生圖站不認它們（家族是 other），
/// 但 model_dirs 判斷分類時要知道它是「主模型」，不是看不出來
pub fn is_video_keys(keys: &[String]) -> bool {
    Probe::new(keys).is_video()
}

/// 這些權重名稱是不是 LTX（影片 LoRA 下拉只列這種）
pub fn is_ltx_keys(keys: &[String]) -> bool {
    Probe::new(keys).is_ltx()
}

/// 檔案屬於哪個家族：anima／qwen21／moss／sdxl／sd15／flux／sd3／other。
/// 看 .safetensors、.sft 的檔頭或 .gguf 的張量名稱，不載入權重；結果依（路徑、修改時間）快取
pub fn detect(path: &Path) -> &'static str {
    cached(path).family
}

/// SDXL 微調的子標籤："Pony"、"Illustrious"、"NoobAI"、"Animagine"。
/// 只在 [`detect`] 是 sdxl、而且檔頭 metadata 看得出來時才有（看不出來很正常，不要當必要條件）
pub fn sublabel(path: &Path) -> Option<&'static str> {
    cached(path).sub
}

/// ComfyUI 回傳的相對名稱（可能含子資料夾，用 / 或 \）→ 實際檔案路徑
pub fn resolve(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    let rel: PathBuf = name.split(['/', '\\']).collect();
    roots.iter().map(|r| r.join(&rel)).find(|p| p.exists())
}

/// 生圖前檢查：選了模型、卻在所有模型資料夾都找不到這個檔 → 給畫面看的說明（不要讓 family_of 猜成 other、用錯流程）
pub fn missing_model(roots: &[PathBuf], name: &str) -> Option<String> {
    (!name.is_empty() && resolve(roots, name).is_none()).then(|| {
        format!("找不到模型檔「{name}」：ComfyUI 的模型資料夾裡沒有這個檔。剛開站的話等幾秒再按一次；還是不行就到模型選單重選一次，或確認檔案還在")
    })
}

pub fn family_of(roots: &[PathBuf], name: &str) -> &'static str {
    resolve(roots, name).map(|p| detect(&p)).unwrap_or(OTHER)
}

/// Civitai 寫的底模（baseModel，例如 "Anima"、"MOSS Image"）對到內建家族；
/// "Qwen" 不寫 2.1 的是第一代 Qwen-Image，架構不同，不猜
pub fn from_base_model(base: &str) -> Option<&'static str> {
    let b = base.to_lowercase();
    if b.contains("anima") {
        Some(ANIMA)
    } else if b.contains("moss") {
        Some(MOSS)
    } else if b.contains("qwen") && b.contains("2.1") {
        Some(QWEN21)
    } else {
        None
    }
}

/// LoRA 的家族：先看檔頭；認不出來（other）才用 Civitai 資料（lora_meta）寫的底模補。
/// 2026-10-28：使用者回報新格式的 MOSS LoRA 被判成 other，選單選不到、生成時被略過
pub fn lora_family(roots: &[PathBuf], name: &str, meta: Option<&serde_json::Value>) -> &'static str {
    match family_of(roots, name) {
        OTHER => meta
            .and_then(|m| m.get("base_model"))
            .and_then(serde_json::Value::as_str)
            .and_then(from_base_model)
            .unwrap_or(OTHER),
        f => f,
    }
}

/// 這個名稱的檔案是不是 LTX 影片模型／LoRA（看檔頭、有快取）
pub fn is_ltx_of(roots: &[PathBuf], name: &str) -> bool {
    resolve(roots, name).is_some_and(|p| cached(&p).ltx)
}

/// 同 [`family_of`]，回子標籤
pub fn sublabel_of(roots: &[PathBuf], name: &str) -> Option<&'static str> {
    resolve(roots, name).and_then(|p| sublabel(&p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 2026-10-31（第一百九十一筆）：站剛開、還不知道模型資料夾時找不到 MOSS 模型檔 → 判成 other → 用 Anima 的文字編碼器跑 MOSS、
    /// 報「too many values to unpack」。找不到模型檔要講出來，不能猜成 other
    #[test]
    fn missing_model_is_reported_not_guessed() {
        let d = std::env::temp_dir().join(format!("anima_missing_model_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("moss")).unwrap();
        std::fs::write(d.join("moss").join("m.safetensors"), "x").unwrap();
        let roots = vec![d.clone()];
        assert!(missing_model(&roots, "moss\\m.safetensors").is_none());
        assert!(missing_model(&roots, "").is_none()); // 沒選模型：照舊交給後面
        let e = missing_model(&roots, "moss_image_sft_int8_convrot.safetensors").expect("要報找不到");
        assert!(e.contains("moss_image_sft_int8_convrot.safetensors") && e.contains("ComfyUI"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// 改版前的判斷（一字不改），拿來對照：Anima／Qwen／MOSS 的結果不能變
    fn legacy_classify(keys: &[String]) -> &'static str {
        if keys
            .iter()
            .any(|k| k.contains("shared_block_modulation") || k.contains(".proj.ffn_up_gate"))
        {
            MOSS
        } else if keys
            .iter()
            .any(|k| k.contains("txt_in.text_norm") || k.contains("transformer_blocks"))
        {
            QWEN21
        } else if keys.iter().any(|k| {
            k.contains("llm_adapter")
                || k.contains("adaln_modulation")
                || k.contains(".blocks.")
                || k.starts_with("blocks.")
                || k.starts_with("lora_unet_blocks_")
        }) {
            ANIMA
        } else {
            OTHER
        }
    }

    fn meta(pairs: &[(&str, &str)]) -> Meta {
        let o: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        meta_from_obj(&o)
    }

    // ------------------------------------------------ 舊家族：結果不能變
    #[test]
    fn classify_keys() {
        assert_eq!(
            classify(&s(&["transformer_blocks.0.attn.to_q.weight"])),
            QWEN21
        );
        assert_eq!(
            classify(&s(&[
                "lora_unet_blocks_3_self_attn_q_proj.lora_down.weight"
            ])),
            ANIMA
        );
        assert_eq!(classify(&s(&["net.llm_adapter.x"])), ANIMA);
        assert_eq!(
            classify(&s(&[
                "text_embedder.blocks.0.attn.weight",
                "shared_block_modulation.weight",
                "layers.0.proj.ffn_up_gate.weight"
            ])),
            MOSS
        );
        assert_eq!(
            classify(&s(&[
                "diffusion_model.layers.3.proj.ffn_up_gate.lora_A.weight"
            ])),
            MOSS
        );
        // 只有一個 input_blocks.0：看不出是 SD1.x 還是 SDXL，維持 other
        assert_eq!(
            classify(&s(&["model.diffusion_model.input_blocks.0"])),
            OTHER
        );
    }

    #[test]
    fn civitai_base_model_fills_unknown_lora_family() {
        assert_eq!(from_base_model("Anima"), Some(ANIMA));
        assert_eq!(from_base_model("MOSS Image"), Some(MOSS));
        assert_eq!(from_base_model("Qwen Image 2.1"), Some(QWEN21));
        // Qwen-Image 第一代跟 2.1 架構不同，不能猜成 2.1；影片模型維持 other
        assert_eq!(from_base_model("Qwen"), None);
        assert_eq!(from_base_model("LTXV 2.3"), None);
        // 檔案找不到（看不出家族）時才用 Civitai 的底模
        let roots = vec![std::env::temp_dir().join("no_such_lora_root_20261028")];
        assert_eq!(lora_family(&roots, "x.safetensors", Some(&json!({"base_model": "MOSS Image"}))), MOSS);
        assert_eq!(lora_family(&roots, "x.safetensors", Some(&json!({"base_model": "SDXL 1.0"}))), OTHER);
        assert_eq!(lora_family(&roots, "x.safetensors", None), OTHER);
    }

    #[test]
    fn moss_lora_other_formats() {
        // kohya 底線版、只練注意力層（沒有 ffn_up_gate）的 MOSS LoRA：以前認成 other，助手和選單都看不到
        assert_eq!(classify(&s(&["lora_unet_layers_0_proj_ffn_up_gate.lora_down.weight"])), MOSS);
        assert_eq!(classify(&s(&["lora_unet_layers_12_proj_qkv.lora_up.weight", "lora_unet_layers_12_proj_o.alpha"])), MOSS);
        assert_eq!(classify(&s(&["transformer.layers.4.proj.q.lora_A.weight", "transformer.layers.4.proj.v.lora_B.weight"])), MOSS);
        // 文字編碼器 LoRA（layers.N.self_attn.q_proj）不能被認成 MOSS
        assert_eq!(classify(&s(&["model.layers.0.self_attn.q_proj.lora_A.weight"])), OTHER);
    }

    #[test]
    fn old_families_match_legacy_on_corpus() {
        let corpus: Vec<(&str, Vec<String>)> = vec![
            (
                ANIMA,
                s(&[
                    "net.blocks.0.self_attn.q_proj.weight",
                    "net.x_embedder.proj.1.weight",
                    "net.llm_adapter.blocks.0.cross_attn.q_proj.weight",
                ]),
            ),
            (
                ANIMA,
                s(&[
                    "blocks.0.adaln_modulation_self_attn.1.weight",
                    "t_embedder.1.linear_1.weight",
                ]),
            ),
            (
                ANIMA,
                s(&[
                    "lora_unet_blocks_3_self_attn_q_proj.lora_down.weight",
                    "lora_unet_blocks_3_mlp_layer1.lora_up.weight",
                ]),
            ),
            (
                ANIMA,
                s(&["diffusion_model.net.blocks.12.cross_attn.k_proj.lora_A.weight"]),
            ),
            (
                ANIMA,
                s(&["lora_unet_llm_adapter_blocks_0_self_attn_q_proj.lora_down.weight"]),
            ),
            (
                QWEN21,
                s(&[
                    "transformer_blocks.0.attn.to_q.weight",
                    "txt_in.text_norm.weight",
                    "img_in.weight",
                ]),
            ),
            (
                QWEN21,
                s(&["diffusion_model.transformer_blocks.5.attn.add_q_proj.lora_A.weight"]),
            ),
            (
                QWEN21,
                s(&[
                    "lora_unet_transformer_blocks_3_attn_to_q.lora_down.weight",
                    "lora_unet_transformer_blocks_3_img_mlp_net_0_proj.lora_up.weight",
                ]),
            ),
            (
                QWEN21,
                s(&[
                    "transformer_blocks.2.img_mod.1.weight",
                    "transformer_blocks.2.txt_mlp.net.2.weight",
                    "txt_norm.weight",
                    "proj_out.weight",
                ]),
            ),
            (QWEN21, s(&["txt_in.text_norm.weight"])),
            (
                MOSS,
                s(&[
                    "shared_block_modulation.weight",
                    "layers.0.proj.ffn_up_gate.weight",
                ]),
            ),
            (
                MOSS,
                s(&[
                    "diffusion_model.layers.3.proj.ffn_up_gate.lora_A.weight",
                    "text_embedder.blocks.0.attn.weight",
                ]),
            ),
            (
                MOSS,
                s(&[
                    "layers.0.attn.q.weight",
                    "shared_block_modulation.weight",
                    "blocks.0.x",
                ]),
            ),
            (OTHER, s(&["conv_in.weight", "something.else"])),
            (OTHER, s(&[])),
        ];
        for (want, keys) in &corpus {
            assert_eq!(legacy_classify(keys), *want, "legacy {keys:?}");
            assert_eq!(classify(keys), *want, "new {keys:?}");
            if *want != OTHER {
                let sdxl_meta = meta(&[
                    ("modelspec.architecture", "stable-diffusion-xl-v1-base"),
                    ("ss_base_model_version", "sdxl_base_v1-0"),
                ]);
                assert_eq!(
                    classify_with(keys, Some(&sdxl_meta)),
                    *want,
                    "metadata 不能蓋掉 {keys:?}"
                );
            }
        }
    }

    // ------------------------------------------------ SD 1.5
    #[test]
    fn sd15_checkpoint_and_unet() {
        let ckpt = s(&[
            "model.diffusion_model.time_embed.0.weight",
            "model.diffusion_model.input_blocks.0.0.weight",
            "model.diffusion_model.input_blocks.1.1.transformer_blocks.0.attn1.to_q.weight",
            "model.diffusion_model.input_blocks.11.0.in_layers.0.weight",
            "model.diffusion_model.middle_block.1.transformer_blocks.0.attn2.to_k.weight",
            "model.diffusion_model.output_blocks.11.1.transformer_blocks.0.ff.net.2.weight",
            "cond_stage_model.transformer.text_model.embeddings.token_embedding.weight",
            "first_stage_model.encoder.conv_in.weight",
        ]);
        assert_eq!(classify(&ckpt), SD15);
        // UNet 單檔（沒有 model.diffusion_model. 前綴、沒有文字編碼器）
        assert_eq!(
            classify(&s(&[
                "input_blocks.1.1.transformer_blocks.0.attn1.to_q.weight",
                "input_blocks.0.0.weight"
            ])),
            SD15
        );
        assert_eq!(
            classify(&s(&["input_blocks.10.0.in_layers.0.weight"])),
            SD15
        );
        // SD2.x（OpenCLIP 文字端）算同一組
        assert_eq!(
            classify(&s(&[
                "cond_stage_model.model.transformer.resblocks.0.attn.in_proj_weight",
                "model.diffusion_model.input_blocks.1.1.proj_in.weight"
            ])),
            SD15
        );
        // SD2 系的改版（SD2 unCLIP、SDPose）多一個 label_emb：結構（input_blocks 到 11）比 label_emb 可靠，仍是 SD1.x 一組
        assert_eq!(
            classify(&s(&[
                "model.diffusion_model.label_emb.0.0.weight",
                "model.diffusion_model.input_blocks.11.0.in_layers.0.weight",
                "model.diffusion_model.input_blocks.1.1.transformer_blocks.0.attn1.to_q.weight"
            ])),
            SD15
        );
    }

    #[test]
    fn sd15_loras() {
        // kohya：diffusers 命名＋單一文字編碼器
        let kohya = s(&[
            "lora_unet_down_blocks_0_attentions_0_transformer_blocks_0_attn1_to_k.lora_down.weight",
            "lora_unet_down_blocks_0_attentions_0_transformer_blocks_0_attn1_to_k.lora_up.weight",
            "lora_unet_mid_block_attentions_0_transformer_blocks_0_attn2_to_v.lora_down.weight",
            "lora_unet_up_blocks_3_attentions_2_transformer_blocks_0_ff_net_2.lora_up.weight",
            "lora_te_text_model_encoder_layers_0_self_attn_q_proj.lora_down.weight",
        ]);
        assert_eq!(classify(&kohya), SD15);
        // 只有文字編碼器
        assert_eq!(
            classify(&s(&[
                "lora_te_text_model_encoder_layers_11_mlp_fc2.lora_up.weight"
            ])),
            SD15
        );
        // 只有 UNet、沒有文字編碼器，靠 down_blocks.3／up_blocks.2.attentions
        assert_eq!(
            classify(&s(&[
                "lora_unet_down_blocks_3_resnets_0_conv1.lora_down.weight"
            ])),
            SD15
        );
        assert_eq!(
            classify(&s(&[
                "lora_unet_up_blocks_2_attentions_1_proj_in.lora_down.weight"
            ])),
            SD15
        );
        // diffusers／PEFT
        assert_eq!(
            classify(&s(&[
                "unet.down_blocks.0.attentions.0.transformer_blocks.0.attn1.to_q.lora_A.weight",
                "text_encoder.text_model.encoder.layers.0.self_attn.q_proj.lora_A.weight"
            ])),
            SD15
        );
        // LyCORIS（LoKr、LoHa）
        assert_eq!(
            classify(&s(&[
                "lora_unet_down_blocks_0_attentions_0_transformer_blocks_0_attn1_to_k.lokr_w1",
                "lora_te_text_model_encoder_layers_0_mlp_fc1.hada_w1_a"
            ])),
            SD15
        );
    }

    // ------------------------------------------------ SDXL
    #[test]
    fn sdxl_checkpoint_and_unet() {
        let ckpt = s(&[
            "model.diffusion_model.time_embed.0.weight",
            "model.diffusion_model.label_emb.0.0.weight",
            "model.diffusion_model.input_blocks.4.1.transformer_blocks.1.attn1.to_q.weight",
            "model.diffusion_model.middle_block.1.transformer_blocks.9.ff.net.2.weight",
            "model.diffusion_model.output_blocks.8.0.in_layers.0.weight",
            "conditioner.embedders.0.transformer.text_model.embeddings.token_embedding.weight",
            "conditioner.embedders.1.model.transformer.resblocks.0.attn.in_proj_weight",
            "first_stage_model.decoder.conv_in.weight",
        ]);
        // 這種檔案有 transformer_blocks，舊規則會誤認成 Qwen
        assert_eq!(legacy_classify(&ckpt), QWEN21);
        assert_eq!(classify(&ckpt), SDXL);
        // Pony／Illustrious／NoobAI 都是這個架構，一樣認 SDXL；refiner 只有 embedders.0
        assert_eq!(
            classify(&s(&[
                "conditioner.embedders.0.model.transformer.resblocks.0.attn.in_proj_weight",
                "model.diffusion_model.input_blocks.1.0.in_layers.0.weight"
            ])),
            SDXL
        );
        // UNet 單檔
        assert_eq!(
            classify(&s(&[
                "label_emb.0.0.weight",
                "input_blocks.4.1.transformer_blocks.0.attn1.to_q.weight"
            ])),
            SDXL
        );
        // 只有深度證據：input_blocks.4 以後有 transformer_blocks.1
        assert_eq!(
            classify(&s(&[
                "input_blocks.5.1.transformer_blocks.1.attn1.to_q.weight"
            ])),
            SDXL
        );
        // diffusers 格式的 UNet
        assert_eq!(
            classify(&s(&[
                "add_embedding.linear_1.weight",
                "down_blocks.1.attentions.0.transformer_blocks.0.attn1.to_q.weight"
            ])),
            SDXL
        );
    }

    #[test]
    fn sdxl_loras() {
        let kohya = s(&[
            "lora_unet_input_blocks_4_1_transformer_blocks_0_attn1_to_k.lora_down.weight",
            "lora_unet_input_blocks_4_1_transformer_blocks_0_attn1_to_k.lora_up.weight",
            "lora_unet_middle_block_1_transformer_blocks_5_ff_net_2.lora_down.weight",
            "lora_te1_text_model_encoder_layers_0_self_attn_q_proj.lora_down.weight",
            "lora_te2_text_model_encoder_layers_0_self_attn_q_proj.lora_down.weight",
        ]);
        assert_eq!(classify(&kohya), SDXL);
        // 只訓練 UNet（kohya --network_train_unet_only），靠 transformer_blocks_1 以後
        assert_eq!(
            classify(&s(&[
                "lora_unet_input_blocks_7_1_transformer_blocks_3_attn2_to_q.lora_down.weight"
            ])),
            SDXL
        );
        // 只訓練文字編碼器
        assert_eq!(
            classify(&s(&[
                "lora_te2_text_model_encoder_layers_5_mlp_fc1.lora_up.weight"
            ])),
            SDXL
        );
        // LyCORIS／LoKr：output_blocks.0.1 只有 SDXL 有
        assert_eq!(
            classify(&s(&[
                "lora_unet_output_blocks_0_1_transformer_blocks_0_attn1_to_k.lokr_w1",
                "lora_unet_output_blocks_0_1_transformer_blocks_0_attn1_to_k.lokr_w2"
            ])),
            SDXL
        );
        // diffusers 命名的 SDXL LoRA：up_blocks.0 有注意力、深度 2 以上
        assert_eq!(classify(&s(&["lora_unet_up_blocks_0_attentions_0_transformer_blocks_0_attn1_to_k.lora_down.weight"])), SDXL);
        assert_eq!(classify(&s(&["lora_unet_down_blocks_1_attentions_0_transformer_blocks_1_attn1_to_k.lora_down.weight"])), SDXL);
        // PEFT／diffusers
        assert_eq!(
            classify(&s(&[
                "unet.up_blocks.0.attentions.0.transformer_blocks.0.attn1.to_q.lora_A.weight",
                "text_encoder_2.text_model.encoder.layers.0.self_attn.q_proj.lora_A.weight"
            ])),
            SDXL
        );
        // 原始 UNet 命名、沒有決定性證據：kohya 只會這樣訓練 SDXL
        assert_eq!(
            classify(&s(&[
                "lora_unet_input_blocks_4_1_transformer_blocks_0_attn1_to_q.lora_down.weight",
                "lora_unet_output_blocks_3_1_transformer_blocks_0_attn1_to_q.lora_down.weight"
            ])),
            SDXL
        );
        // 反例：舊規則誤認成 Qwen
        assert_eq!(legacy_classify(&kohya), QWEN21);
    }

    // ------------------------------------------------ Flux
    #[test]
    fn flux_models_and_loras() {
        let bfl = s(&[
            "img_in.weight",
            "time_in.in_layer.weight",
            "guidance_in.in_layer.weight",
            "double_blocks.0.img_attn.qkv.weight",
            "double_blocks.0.img_mod.lin.weight",
            "double_blocks.18.txt_mlp.2.weight",
            "single_blocks.0.linear1.weight",
            "single_blocks.0.modulation.lin.weight",
            "single_blocks.37.linear2.weight",
            "final_layer.adaLN_modulation.1.weight",
        ]);
        assert_eq!(classify(&bfl), FLUX);
        // checkpoint 版（fp8 整合包）
        let all_in_one: Vec<String> = bfl
            .iter()
            .map(|k| format!("model.diffusion_model.{k}"))
            .chain(s(&[
                "vae.decoder.conv_in.weight",
                "text_encoders.t5xxl.transformer.encoder.block.0.layer.0.SelfAttention.q.weight",
            ]))
            .collect();
        assert_eq!(classify(&all_in_one), FLUX);
        // schnell：沒有 guidance_in；Chroma：沒有 img_mod
        assert_eq!(
            classify(&s(&[
                "double_blocks.0.img_attn.qkv.weight",
                "single_blocks.0.linear1.weight"
            ])),
            FLUX
        );
        // LoRA：kohya、ComfyUI／BFL、XLabs
        assert_eq!(
            classify(&s(&[
                "lora_unet_double_blocks_0_img_attn_qkv.lora_down.weight",
                "lora_unet_single_blocks_7_linear1.lora_up.weight",
                "lora_te1_text_model_encoder_layers_0_mlp_fc1.lora_down.weight"
            ])),
            FLUX
        );
        assert_eq!(
            classify(&s(&[
                "diffusion_model.double_blocks.0.img_attn.qkv.lora_A.weight",
                "diffusion_model.single_blocks.3.linear2.lora_B.weight"
            ])),
            FLUX
        );
        assert_eq!(
            classify(&s(&["double_blocks.0.processor.qkv_lora1.down.weight"])),
            FLUX
        );
        assert_eq!(
            classify(&s(&["lora_unet_single_blocks_7_linear1.lokr_w1"])),
            FLUX
        );
        // diffusers／ai-toolkit：舊規則誤認成 Qwen
        let diffusers = s(&[
            "transformer.single_transformer_blocks.0.attn.to_k.lora_A.weight",
            "transformer.transformer_blocks.0.attn.to_q.lora_A.weight",
        ]);
        assert_eq!(legacy_classify(&diffusers), QWEN21);
        assert_eq!(classify(&diffusers), FLUX);
        assert_eq!(
            classify(&s(&[
                "lora_transformer_single_transformer_blocks_0_attn_to_k.lora_down.weight"
            ])),
            FLUX
        );
        // diffusers 只訓練雙流區塊：ff_context 是 Flux／SD3 才有，Qwen-Image 沒有
        assert_eq!(
            classify(&s(&[
                "transformer.transformer_blocks.3.ff_context.net.0.proj.lora_A.weight",
                "transformer.transformer_blocks.3.attn.to_q.lora_A.weight"
            ])),
            FLUX
        );
        // diffusers 完整模型
        assert_eq!(
            classify(&s(&[
                "x_embedder.weight",
                "context_embedder.weight",
                "transformer_blocks.0.norm1_context.linear.weight",
                "single_transformer_blocks.0.attn.to_q.weight"
            ])),
            FLUX
        );
    }

    #[test]
    fn flux_lookalikes_are_not_flux() {
        // HunyuanVideo：double_blocks／single_blocks 同名，但 mod／mlp／refiner 的名字不同，區塊也比較多
        let hy = s(&[
            "double_blocks.0.img_attn_qkv.weight",
            "double_blocks.0.img_mod.linear.weight",
            "txt_in.individual_token_refiner.blocks.0.self_attn_qkv.weight",
            "single_blocks.0.modulation.linear.weight",
        ]);
        assert_ne!(classify(&hy), FLUX);
        assert_eq!(classify(&hy), legacy_classify(&hy));
        assert_eq!(
            classify(&s(&[
                "lora_unet_double_blocks_0_img_attn_qkv.lora_down.weight",
                "lora_unet_double_blocks_19_img_attn_qkv.lora_down.weight"
            ])),
            OTHER
        );
        assert_eq!(
            classify(&s(&["lora_unet_single_blocks_39_linear1.lora_down.weight"])),
            OTHER
        );
        // Flux.2
        assert_eq!(
            classify(&s(&[
                "double_blocks.0.img_attn.qkv.weight",
                "double_stream_modulation_img.lin.weight",
                "single_stream_modulation.lin.weight"
            ])),
            OTHER
        );
        // HiDream 的名字是 double_stream_blocks
        assert_eq!(
            classify(&s(&["double_stream_blocks.0.block.attn1.to_q.weight"])),
            OTHER
        );
    }

    // ------------------------------------------------ SD3
    #[test]
    fn sd3_models_and_loras() {
        let m = s(&[
            "model.diffusion_model.joint_blocks.0.x_block.attn.qkv.weight",
            "model.diffusion_model.joint_blocks.0.context_block.attn.qkv.weight",
            "model.diffusion_model.x_embedder.proj.weight",
            "model.diffusion_model.context_embedder.weight",
            "model.diffusion_model.y_embedder.mlp.0.weight",
            "model.diffusion_model.final_layer.adaLN_modulation.1.weight",
            "text_encoders.clip_g.transformer.text_model.encoder.layers.0.self_attn.k_proj.weight",
            "first_stage_model.decoder.conv_in.weight",
        ]);
        assert_eq!(classify(&m), SD3);
        assert_eq!(
            classify(&s(&["joint_blocks.3.x_block.attn2.qkv.weight"])),
            SD3
        );
        assert_eq!(
            classify(&s(&[
                "lora_unet_joint_blocks_4_x_block_attn_qkv.lora_down.weight",
                "lora_te1_text_model_encoder_layers_0_mlp_fc1.lora_down.weight",
                "lora_te2_text_model_encoder_layers_0_mlp_fc1.lora_down.weight"
            ])),
            SD3
        );
        assert_eq!(
            classify(&s(&[
                "diffusion_model.joint_blocks.4.context_block.attn.proj.lora_A.weight"
            ])),
            SD3
        );
        // diffusers 格式：pos_embed 或區塊編號超過 Flux 的 19 個
        assert_eq!(
            classify(&s(&[
                "pos_embed.proj.weight",
                "context_embedder.weight",
                "transformer_blocks.0.ff_context.net.0.proj.weight"
            ])),
            SD3
        );
        assert_eq!(
            classify(&s(&[
                "transformer.transformer_blocks.21.attn.to_q.lora_A.weight",
                "transformer.transformer_blocks.21.ff_context.net.0.proj.lora_A.weight"
            ])),
            SD3
        );
    }

    #[test]
    fn unknown_things_stay_other() {
        // VAE：decoder.up_blocks.3／encoder.down_blocks.3 不是 SD1.x 的 UNet
        assert_eq!(
            classify(&s(&[
                "decoder.up_blocks.3.resnets.0.conv1.weight",
                "encoder.down_blocks.3.resnets.0.conv1.weight",
                "decoder.mid_block.attentions.0.group_norm.weight"
            ])),
            OTHER
        );
        // diffusers 的 ControlNet 零卷積
        assert_eq!(
            classify(&s(&[
                "controlnet_down_blocks.3.weight",
                "controlnet_cond_embedding.conv_in.weight"
            ])),
            OTHER
        );
        // 單獨一個 mid_block：看不出是 SD1.x 還是 SDXL
        assert_eq!(
            classify(&s(&[
                "lora_unet_mid_block_attentions_0_transformer_blocks_0_attn2_to_k.lora_down.weight"
            ])),
            OTHER
        );
        assert_eq!(classify(&s(&["conv_in.weight"])), OTHER);
        assert_eq!(classify(&[]), OTHER);
    }

    // ------------------------------------------------ metadata
    #[test]
    fn metadata_only_fills_in_when_weights_are_ambiguous() {
        let ambiguous = s(&[
            "lora_unet_mid_block_attentions_0_transformer_blocks_0_attn2_to_k.lora_down.weight",
        ]);
        assert_eq!(classify(&ambiguous), OTHER);
        let with = |pairs: &[(&str, &str)]| classify_with(&ambiguous, Some(&meta(pairs)));
        assert_eq!(
            with(&[("modelspec.architecture", "stable-diffusion-xl-v1-base/lora")]),
            SDXL
        );
        assert_eq!(
            with(&[("modelspec.architecture", "stable-diffusion-v1")]),
            SD15
        );
        assert_eq!(
            with(&[("modelspec.architecture", "stable-diffusion-v2-768-v")]),
            SD15
        );
        assert_eq!(with(&[("modelspec.architecture", "flux-1-dev")]), FLUX);
        assert_eq!(
            with(&[("modelspec.architecture", "flux-1-schnell/lora")]),
            FLUX
        );
        assert_eq!(
            with(&[("modelspec.architecture", "stable-diffusion-3-medium")]),
            SD3
        );
        assert_eq!(with(&[("modelspec.architecture", "flux-2-dev")]), OTHER);
        assert_eq!(with(&[("ss_base_model_version", "sdxl_base_v1-0")]), SDXL);
        assert_eq!(with(&[("ss_base_model_version", "sd_v1")]), SD15);
        assert_eq!(with(&[("ss_base_model_version", "sd_v2")]), SD15);
        assert_eq!(with(&[("ss_base_model_version", "flux1")]), FLUX);
        assert_eq!(with(&[("ss_network_module", "networks.lora_flux")]), FLUX);
        assert_eq!(with(&[("ss_network_module", "networks.lora_sd3")]), SD3);
        assert_eq!(with(&[("ss_base_model_version", "unknown")]), OTHER);
        assert_eq!(with(&[]), OTHER);
        // 權重名稱看得出來的，以權重為準（metadata 寫錯也不聽）
        let sdxl =
            s(&["lora_unet_input_blocks_4_1_transformer_blocks_1_attn1_to_k.lora_down.weight"]);
        assert_eq!(
            classify_with(&sdxl, Some(&meta(&[("ss_base_model_version", "sd_v1")]))),
            SDXL
        );
        // Anima／Qwen／MOSS 不會被 metadata 蓋掉
        let anima = s(&["lora_unet_blocks_3_self_attn_q_proj.lora_down.weight"]);
        assert_eq!(
            classify_with(
                &anima,
                Some(&meta(&[("ss_base_model_version", "sdxl_base_v1-0")]))
            ),
            ANIMA
        );
        let qwen = s(&["lora_unet_transformer_blocks_3_attn_to_q.lora_down.weight"]);
        assert_eq!(
            classify_with(
                &qwen,
                Some(&meta(&[("modelspec.architecture", "flux-1-dev")]))
            ),
            QWEN21
        );
    }

    #[test]
    fn sublabels_from_metadata() {
        let sub = |pairs: &[(&str, &str)]| sub_of(&meta(pairs));
        assert_eq!(
            sub(&[("modelspec.title", "waiIllustriousSDXL_v140")]),
            Some("Illustrious")
        );
        assert_eq!(
            sub(&[("ss_sd_model_name", "illustriousXL_v01.safetensors")]),
            Some("Illustrious")
        );
        assert_eq!(
            sub(&[(
                "ss_sd_model_name",
                "noobaiXLNAIXL_vPred10Version.safetensors"
            )]),
            Some("NoobAI")
        );
        assert_eq!(
            sub(&[("modelspec.title", "NoobAI-XL (Illustrious based)")]),
            Some("NoobAI")
        );
        assert_eq!(
            sub(&[(
                "ss_sd_model_name",
                "ponyDiffusionV6XL_v6StartWithThisOne.safetensors"
            )]),
            Some("Pony")
        );
        assert_eq!(
            sub(&[("modelspec.description", "A PDXL lora")]),
            Some("Pony")
        );
        assert_eq!(
            sub(&[("ss_sd_model_name", "animagine-xl-3.1.safetensors")]),
            Some("Animagine")
        );
        assert_eq!(sub(&[("modelspec.title", "Noob_AI XL")]), Some("NoobAI"));
        // 標題優先於底模檔名
        assert_eq!(
            sub(&[
                ("modelspec.title", "my Pony lora"),
                ("ss_sd_model_name", "illustriousXL_v01.safetensors")
            ]),
            Some("Pony")
        );
        // ponytail 不是 Pony
        assert_eq!(
            sub(&[("modelspec.description", "girl with a ponytail")]),
            None
        );
        // 都沒寫：看標籤統計有沒有 score_9
        assert_eq!(
            sub(&[(
                "ss_tag_frequency",
                "{\"img\": {\"score_9\": 100, \"1girl\": 90}}"
            )]),
            Some("Pony")
        );
        assert_eq!(
            sub(&[
                ("modelspec.title", "my lora"),
                ("ss_tag_frequency", "{\"img\": {\"1girl\": 90}}")
            ]),
            None
        );
        assert_eq!(sub(&[]), None);
    }

    // ------------------------------------------------ 讀檔（用暫存檔，不碰使用者的模型資料夾）
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str, bytes: &[u8]) -> Tmp {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "anima_families_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            Tmp(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            if let Some(d) = self.0.parent() {
                let _ = std::fs::remove_dir_all(d);
            }
        }
    }

    fn safetensors(keys: &[&str], meta: Option<Value>) -> Vec<u8> {
        let mut h = serde_json::Map::new();
        if let Some(m) = meta {
            h.insert("__metadata__".into(), m);
        }
        for k in keys {
            h.insert(
                k.to_string(),
                json!({"dtype": "F16", "shape": [0], "data_offsets": [0, 0]}),
            );
        }
        let body = serde_json::to_vec(&Value::Object(h)).unwrap();
        let mut out = (body.len() as u64).to_le_bytes().to_vec();
        out.extend(body);
        out
    }

    #[test]
    fn is_ltx_of_reads_file_headers() {
        let ltx = Tmp::new("ltx_move.safetensors", &safetensors(&["diffusion_model.transformer_blocks.4.attn1.to_q.lora_A.weight"], None));
        let h3 = Tmp::new("h3.safetensors", &safetensors(&["diffusion_model.blocks.0.attn.qkv_proj.lora_A.weight"], None));
        let root = |t: &Tmp| vec![t.path().parent().unwrap().to_path_buf()];
        assert!(is_ltx_of(&root(&ltx), "ltx_move.safetensors"));
        assert!(!is_ltx_of(&root(&h3), "h3.safetensors"));
        assert!(!is_ltx_of(&root(&ltx), "missing.safetensors"));
        // IC-LoRA（要配參考影片才有用，檔頭有 reference_downscale_factor）：不算一般的影片 LoRA
        let ic = Tmp::new("ic.safetensors", &safetensors(&["diffusion_model.transformer_blocks.0.attn1.to_k.lora_A.weight"],
            Some(json!({"model_version": "2.3.0", "reference_downscale_factor": "2"}))));
        assert!(!is_ltx_of(&root(&ic), "ic.safetensors"));
    }

    #[test]
    fn detect_reads_headers_and_metadata() {
        let sdxl_lora = Tmp::new(
            "x.safetensors",
            &safetensors(
                &[
                    "lora_unet_input_blocks_4_1_transformer_blocks_0_attn1_to_k.lora_down.weight",
                    "lora_te2_text_model_encoder_layers_0_mlp_fc1.lora_up.weight",
                ],
                Some(
                    json!({"ss_sd_model_name": "illustriousXL_v01.safetensors", "ss_base_model_version": "sdxl_base_v1-0"}),
                ),
            ),
        );
        assert_eq!(detect(sdxl_lora.path()), SDXL);
        assert_eq!(sublabel(sdxl_lora.path()), Some("Illustrious"));
        assert_eq!(detect(sdxl_lora.path()), SDXL); // 第二次走快取
                                                    // 副檔名大小寫、.sft
        let flux = Tmp::new(
            "flux1-dev.SFT",
            &safetensors(
                &[
                    "double_blocks.0.img_attn.qkv.weight",
                    "single_blocks.0.linear1.weight",
                ],
                None,
            ),
        );
        assert_eq!(detect(flux.path()), FLUX);
        assert_eq!(sublabel(flux.path()), None);
        // 只有 metadata 看得出來
        let by_meta = Tmp::new(
            "l.safetensors",
            &safetensors(
                &["weird.key.lora_down.weight"],
                Some(json!({"modelspec.architecture": "stable-diffusion-v1"})),
            ),
        );
        assert_eq!(detect(by_meta.path()), SD15);
        // 舊家族
        let anima = Tmp::new(
            "a.safetensors",
            &safetensors(
                &["net.blocks.0.self_attn.q_proj.weight"],
                Some(json!({"ss_base_model_version": "sdxl_base_v1-0"})),
            ),
        );
        assert_eq!(detect(anima.path()), ANIMA);
        // 非模型副檔名
        let part = Tmp::new(
            "x.safetensors.part",
            &safetensors(&["double_blocks.0.img_attn.qkv.weight"], None),
        );
        assert_eq!(detect(part.path()), OTHER);
        assert_eq!(detect(Path::new("Z:\\no\\such\\file.safetensors")), OTHER);
    }

    #[test]
    fn detect_survives_bad_headers() {
        // 檔頭長度宣稱 10GB（超過上限）
        let huge = Tmp::new(
            "huge.safetensors",
            &[&(10u64 << 30).to_le_bytes()[..], b"{}"].concat(),
        );
        assert_eq!(detect(huge.path()), OTHER);
        // 檔頭長度比整個檔案還大
        let lie = Tmp::new(
            "lie.safetensors",
            &[&1000u64.to_le_bytes()[..], b"{}"].concat(),
        );
        assert_eq!(detect(lie.path()), OTHER);
        // 不是 JSON、太短、空檔
        let junk = Tmp::new(
            "junk.safetensors",
            &[&4u64.to_le_bytes()[..], b"abcd"].concat(),
        );
        assert_eq!(detect(junk.path()), OTHER);
        assert_eq!(detect(Tmp::new("short.safetensors", b"abc").path()), OTHER);
        assert_eq!(detect(Tmp::new("empty.safetensors", b"").path()), OTHER);
    }

    fn gstr(out: &mut Vec<u8>, s: &str) {
        out.extend((s.len() as u64).to_le_bytes());
        out.extend(s.as_bytes());
    }

    /// 做一個最小的 GGUF v3：幾個 KV（含字串陣列、數字）＋ 張量資訊
    fn gguf(arch: &str, tensors: &[&str]) -> Vec<u8> {
        let mut o = b"GGUF".to_vec();
        o.extend(3u32.to_le_bytes());
        o.extend((tensors.len() as u64).to_le_bytes());
        o.extend(3u64.to_le_bytes());
        gstr(&mut o, "general.architecture");
        o.extend(8u32.to_le_bytes());
        gstr(&mut o, arch);
        gstr(&mut o, "general.alignment");
        o.extend(4u32.to_le_bytes());
        o.extend(32u32.to_le_bytes());
        gstr(&mut o, "tokenizer.tokens");
        o.extend(9u32.to_le_bytes());
        o.extend(8u32.to_le_bytes());
        o.extend(2u64.to_le_bytes());
        gstr(&mut o, "a");
        gstr(&mut o, "bb");
        for t in tensors {
            gstr(&mut o, t);
            o.extend(2u32.to_le_bytes());
            o.extend(64u64.to_le_bytes());
            o.extend(64u64.to_le_bytes());
            o.extend(8u32.to_le_bytes());
            o.extend(0u64.to_le_bytes());
        }
        o
    }

    #[test]
    fn gguf_detection() {
        let flux = Tmp::new(
            "flux1-dev-Q4_0.gguf",
            &gguf(
                "flux",
                &[
                    "double_blocks.0.img_attn.qkv.weight",
                    "single_blocks.0.linear1.weight",
                ],
            ),
        );
        assert_eq!(detect(flux.path()), FLUX);
        let sdxl = Tmp::new(
            "sdxl-Q8.gguf",
            &gguf(
                "sdxl",
                &[
                    "label_emb.0.0.weight",
                    "input_blocks.4.1.transformer_blocks.1.attn1.to_q.weight",
                ],
            ),
        );
        assert_eq!(detect(sdxl.path()), SDXL);
        // 張量名稱看不出來，靠 general.architecture
        let by_arch = Tmp::new("x.gguf", &gguf("sd3", &["some.tensor"]));
        assert_eq!(detect(by_arch.path()), SD3);
        // Anima、Qwen、MOSS 的 GGUF 跟以前一樣是 other
        assert_eq!(
            detect(
                Tmp::new(
                    "a.gguf",
                    &gguf("anima", &["net.blocks.0.self_attn.q_proj.weight"])
                )
                .path()
            ),
            OTHER
        );
        assert_eq!(
            detect(
                Tmp::new(
                    "q.gguf",
                    &gguf("qwen_image", &["transformer_blocks.0.attn.to_q.weight"])
                )
                .path()
            ),
            OTHER
        );
        // 語言模型、壞檔、被截斷的檔
        assert_eq!(
            detect(Tmp::new("llm.gguf", &gguf("qwen3", &["blk.0.attn_q.weight"])).path()),
            OTHER
        );
        assert_eq!(
            detect(Tmp::new("bad.gguf", b"NOTGGUF-and-some-more-bytes").path()),
            OTHER
        );
        let full = gguf("flux", &["double_blocks.0.img_attn.qkv.weight"]);
        assert_eq!(
            detect(Tmp::new("cut.gguf", &full[..full.len() / 2]).path()),
            OTHER
        );
    }

    #[test]
    fn display_names() {
        assert_eq!(label(SDXL), "SDXL（含 Pony、Illustrious、NoobAI）");
        assert_eq!(label(SD15), "SD 1.5");
        assert_eq!(label(OTHER), "其他");
        assert!(is_builtin(ANIMA) && is_builtin(QWEN21) && is_builtin(MOSS));
        assert!(!is_builtin(SDXL) && !is_builtin(FLUX) && !is_builtin(OTHER));
        // 內建三個維持在最前面（生成頁模型選單的順序）
        assert_eq!(
            FAMILY_NAMES
                .iter()
                .take(3)
                .map(|(k, _)| *k)
                .collect::<Vec<_>>(),
            BUILTIN
        );
        let ids: std::collections::HashSet<_> = FAMILY_NAMES.iter().map(|(k, _)| *k).collect();
        assert_eq!(ids.len(), FAMILY_NAMES.len());
    }

    /// 手動：掃一個資料夾裡所有模型檔，對照舊規則與新規則（只讀檔頭）。
    /// FAMILIES_SCAN_DIR=D:\AI\ComfyUI_models cargo test --lib families::tests::scan_real_files -- --ignored --nocapture
    #[test]
    #[ignore = "手動掃描真實模型資料夾，需要 FAMILIES_SCAN_DIR"]
    fn scan_real_files() {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if is_safetensors(&p) || is_gguf(&p) {
                    out.push(p);
                }
            }
        }
        let root =
            PathBuf::from(std::env::var("FAMILIES_SCAN_DIR").expect("要設 FAMILIES_SCAN_DIR"));
        let mut files = vec![];
        walk(&root, &mut files);
        files.sort();
        let mut changed = 0;
        for p in &files {
            let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
            let Ok(h) = read_header(p) else {
                println!("SCAN|{rel}|unreadable");
                continue;
            };
            let old = if is_gguf(p) {
                OTHER
            } else {
                legacy_classify(&h.keys)
            };
            let new = detect(p);
            let mark = if old != new {
                changed += 1;
                "CHANGED"
            } else {
                "same"
            };
            println!(
                "SCAN|{rel}|keys={}|old={old}|new={new}|sub={:?}|{mark}",
                h.keys.len(),
                sublabel(p)
            );
        }
        println!("SCAN|total={}|changed={changed}", files.len());
    }

    /// Wan 2.x、LTX-Video／LTX-2 的權重名稱跟 Anima（blocks.）、Qwen（transformer_blocks）撞名：要判成 other，不能被當成內建家族
    #[test]
    fn video_models_are_not_anima_or_qwen() {
        // Wan 2.1／2.2（ComfyUI model_detection.py：head.modulation、blocks.N.ffn.0、self_attn.q）
        let wan = s(&[
            "patch_embedding.weight",
            "text_embedding.0.weight",
            "time_embedding.0.weight",
            "time_projection.1.weight",
            "blocks.0.self_attn.q.weight",
            "blocks.0.self_attn.norm_q.weight",
            "blocks.0.cross_attn.k.weight",
            "blocks.0.ffn.0.weight",
            "blocks.0.ffn.2.weight",
            "blocks.0.modulation",
            "head.head.weight",
            "head.modulation",
        ]);
        assert_eq!(legacy_classify(&wan), ANIMA, "以前會被誤認成 Anima");
        assert_eq!(classify(&wan), OTHER);
        assert!(is_video_keys(&wan));
        // 帶前綴、image-to-video（cross_attn.k_img）
        assert_eq!(classify(&s(&["model.diffusion_model.blocks.3.cross_attn.k_img.weight", "model.diffusion_model.blocks.3.cross_attn.q.weight"])), OTHER);
        // Wan 的 LoRA（kohya／musubi／diffusers 命名）
        assert_eq!(classify(&s(&["diffusion_model.blocks.0.self_attn.q.lora_A.weight", "diffusion_model.blocks.0.self_attn.q.lora_B.weight"])), OTHER);
        assert_eq!(classify(&s(&["lora_unet_blocks_0_self_attn_q.lora_down.weight", "lora_unet_blocks_0_ffn_0.lora_up.weight"])), OTHER);
        // LTX-Video：transformer_blocks（舊規則會當成 Qwen）、adaln_single、scale_shift_table、patchify_proj
        let ltx = s(&[
            "model.diffusion_model.transformer_blocks.0.attn1.to_q.weight",
            "model.diffusion_model.transformer_blocks.0.attn2.to_k.weight",
            "model.diffusion_model.transformer_blocks.0.ff.net.0.proj.weight",
            "model.diffusion_model.transformer_blocks.0.scale_shift_table",
            "model.diffusion_model.adaln_single.linear.weight",
            "model.diffusion_model.patchify_proj.weight",
            "model.diffusion_model.caption_projection.linear_1.weight",
        ]);
        assert_eq!(legacy_classify(&ltx), QWEN21, "以前會被誤認成 Qwen");
        assert_eq!(classify(&ltx), OTHER);
        // LTX-2.3（10Eros）：只有 transformer_blocks.N.attn1＋audio 的名稱（真實檔頭 D:\AI\ComfyUI_models\checkpointsEros_v1.5_DMD_INT8_checkpoint.safetensors）
        assert_eq!(
            classify(&s(&[
                "model.diffusion_model.transformer_blocks.0.attn1.to_k.weight_scale",
                "model.diffusion_model.transformer_blocks.0.audio_attn1.to_k.weight_scale",
                "model.diffusion_model.transformer_blocks.0.audio_to_video_attn.to_q.weight_scale",
                "model.diffusion_model.audio_adaln_single.linear.weight",
                "audio_vae.decoder.conv_in.conv.weight",
            ])),
            OTHER
        );
        // LTX 的 LoRA：只有 transformer_blocks.N.attn1／attn2、沒有 UNet 的結構
        assert_eq!(classify(&s(&["diffusion_model.transformer_blocks.4.attn1.to_q.lora_A.weight", "diffusion_model.transformer_blocks.4.ff.net.2.lora_B.weight"])), OTHER);
    }

    /// 影片 LoRA 下拉只列 LTX 的：LTX 認得出來，Wan、MiniMax H3、生圖家族都不算
    #[test]
    fn ltx_keys_only_for_ltx() {
        assert!(is_ltx_keys(&s(&["diffusion_model.transformer_blocks.4.attn1.to_q.lora_A.weight", "diffusion_model.transformer_blocks.4.audio_attn1.to_k.lora_B.weight"])));
        assert!(is_ltx_keys(&s(&["model.diffusion_model.adaln_single.linear.weight", "model.diffusion_model.transformer_blocks.0.attn1.to_k.weight"])));
        // Wan：self_attn.q、ffn.0
        assert!(!is_ltx_keys(&s(&["diffusion_model.blocks.0.self_attn.q.lora_A.weight", "diffusion_model.blocks.0.ffn.0.lora_B.weight"])));
        // MiniMax H3：blocks.N.attn.qkv_proj（真實檔頭 minimax_h3_fl2v_turbo_8step_v1.0_comfyui_bf16）
        assert!(!is_ltx_keys(&s(&["diffusion_model.blocks.0.attn.qkv_proj.lora_A.weight", "diffusion_model.blocks.0.attn.out_proj.lora_B.weight"])));
        // Qwen-Image：transformer_blocks.N.attn（不是 attn1）
        assert!(!is_ltx_keys(&s(&["transformer_blocks.0.attn.to_q.weight", "transformer_blocks.0.img_mlp.net.0.proj.weight"])));
        // SDXL 的 UNet 也有 attn1／attn2，但有 input_blocks
        assert!(!is_ltx_keys(&s(&["lora_unet_input_blocks_4_1_transformer_blocks_0_attn1_to_q.lora_down.weight", "lora_unet_label_emb_0_0.lora_down.weight"])));
    }

    /// 影片模型的排除條件不能誤傷：Anima、Qwen、SDXL／SD1.5 的 UNet（也有 attn1／attn2）、SD3.5（transformer_blocks.N.attn2）照舊
    #[test]
    fn video_rules_leave_other_families_alone() {
        assert_eq!(classify(&s(&["net.blocks.0.self_attn.q_proj.weight", "net.blocks.0.adaln_modulation_self_attn.1.weight", "net.llm_adapter.blocks.0.cross_attn.q_proj.weight"])), ANIMA);
        assert_eq!(classify(&s(&["blocks.0.self_attn.q_proj.weight", "blocks.0.mlp.layer1.weight"])), ANIMA);
        assert_eq!(classify(&s(&["diffusion_model.net.blocks.12.cross_attn.k_proj.lora_A.weight"])), ANIMA);
        assert_eq!(classify(&s(&["transformer_blocks.0.attn.to_q.weight", "transformer_blocks.0.img_mlp.net.0.proj.weight", "transformer_blocks.0.attn.add_q_proj.weight"])), QWEN21);
        // SDXL／SD1.5：UNet 裡的 transformer_blocks.N.attn1
        assert_eq!(
            classify(&s(&["model.diffusion_model.input_blocks.4.1.transformer_blocks.1.attn1.to_q.weight", "model.diffusion_model.middle_block.1.transformer_blocks.9.attn2.to_k.weight"])),
            SDXL
        );
        assert_eq!(
            classify(&s(&["lora_unet_down_blocks_0_attentions_0_transformer_blocks_0_attn1_to_k.lora_down.weight", "lora_unet_down_blocks_3_resnets_0_conv1.lora_up.weight"])),
            SD15
        );
        // SD3.5：diffusers 命名的雙注意力區塊有 attn2、ff_context
        let sd35: Vec<String> = (0..24).flat_map(|i| [format!("transformer_blocks.{i}.attn2.to_q.weight"), format!("transformer_blocks.{i}.ff_context.net.0.proj.weight")]).collect();
        assert_eq!(classify(&sd35), SD3);
        assert!(!is_video_keys(&sd35));
        // CLIP Vision 之類也有 patch_embedding：不能算影片
        assert!(!is_video_keys(&s(&["vision_model.embeddings.patch_embedding.weight", "vision_model.encoder.layers.0.self_attn.q_proj.weight"])));
    }

    /// 真的影片模型檔頭（只讀檔頭）：ANIMA_TEST_VIDEO_FILE＝檔案路徑
    /// cargo test --lib real_video_header -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_video_header() {
        let Some(f) = std::env::var_os("ANIMA_TEST_VIDEO_FILE") else { return };
        let h = read_header(Path::new(&f)).expect("讀不到檔頭");
        println!("{} 個權重，家族＝{}，影片＝{}", h.keys.len(), classify_with(&h.keys, Some(&h.meta)), is_video_keys(&h.keys));
        assert_eq!(classify_with(&h.keys, Some(&h.meta)), OTHER);
    }
}

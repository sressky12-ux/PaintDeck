//! 反推提示詞：WD EVA02-Large Tagger v3（ONNX，CPU 執行）從圖片產生 Danbooru tag（對照舊版 tagger.py）。
//!
//! 模型：https://huggingface.co/SmilingWolf/wd-eva02-large-tagger-v3（data\models\tagger\model.onnx＋selected_tags.csv）
//! 輸入 448×448、BGR、0～255 浮點、NHWC；以白色補成正方形。
//!
//! 前處理逐位元照 Pillow 做（疊白底的整數算法、BICUBIC 縮圖的定點數算法），所以跟舊版餵給模型的數字一模一樣。
//! ONNX Runtime 用 load-dynamic 載入 onnxruntime.dll：環境變數 ORT_DYLIB_PATH，或放在程式旁邊／模型資料夾裡。
use crate::py;
use image::{Rgb, RgbImage, RgbaImage};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 護欄已拆：不再擋年齡類 tag
pub const SKIP: [&str; 0] = [];
const KAOMOJI: [&str; 19] = [
    "0_0", "(o)_(o)", "+_+", "+_-", "._.", "<o>_<o>", "<|>_<|>", "=_=", ">_<", "3_3", "6_9", ">_o",
    "@_@", "^_^", "o_o", "u_u", "x_x", "|_|", "||_||",
];

/// tag 名稱 → 提示詞寫法：底線換空白（顏文字除外）、括號跳脫
pub fn fmt_tag(tag: &str) -> String {
    let t = if KAOMOJI.contains(&tag) {
        tag.to_string()
    } else {
        tag.replace('_', " ")
    };
    t.replace('(', "\\(").replace(')', "\\)")
}

// ================================================================ 前處理（照 Pillow）
/// Image.alpha_composite（AlphaComposite.c 的整數算法），dst＝底、src＝疊上去的圖
fn alpha_composite(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    if src[3] == 0 {
        return dst;
    }
    const PB: u32 = 7;
    let shift_div255 = |a: u32| ((a >> 8) + a) >> 8;
    let (sa, da) = (src[3] as u32, dst[3] as u32);
    let blend = da * (255 - sa);
    let outa255 = sa * 255 + blend;
    let coef1 = sa * 255 * 255 * (1 << PB) / outa255;
    let coef2 = 255 * (1 << PB) - coef1;
    let ch = |i: usize| {
        (shift_div255(src[i] as u32 * coef1 + dst[i] as u32 * coef2 + (0x80 << PB)) >> PB) as u8
    };
    [ch(0), ch(1), ch(2), shift_div255(outa255 + 0x80) as u8]
}

fn bicubic(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

const PRECISION_BITS: u32 = 32 - 8 - 2;

/// Resample.c 的 precompute_coeffs＋normalize_coeffs_8bpc：每個輸出位置的 (起點, 個數, 定點係數)
fn coeffs(in_size: usize, out_size: usize) -> (usize, Vec<(usize, usize)>, Vec<i32>) {
    let (in0, in1) = (0.0f32, in_size as f32);
    let scale = (in1 - in0) as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = 2.0 * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut kk = vec![0i32; out_size * ksize];
    let mut k = vec![0f64; ksize];
    for xx in 0..out_size {
        let center = in0 as f64 + (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        let xmin = ((center - support + 0.5) as i64).max(0);
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) - xmin;
        let xmax = xmax.max(0) as usize;
        let mut ww = 0.0;
        for (x, kx) in k.iter_mut().enumerate().take(xmax) {
            let w = bicubic(((x as i64 + xmin) as f64 - center + 0.5) * ss);
            *kx = w;
            ww += w;
        }
        for (x, kx) in k.iter().enumerate().take(xmax) {
            let v = if ww != 0.0 { kx / ww } else { *kx };
            let f = v * (1u32 << PRECISION_BITS) as f64;
            kk[xx * ksize + x] = if v < 0.0 {
                (-0.5 + f) as i32
            } else {
                (0.5 + f) as i32
            };
        }
        bounds.push((xmin as usize, xmax));
    }
    (ksize, bounds, kk)
}

fn clip8(v: i64) -> u8 {
    if v >= (1i64 << PRECISION_BITS << 8) {
        255
    } else if v <= 0 {
        0
    } else {
        (v >> PRECISION_BITS) as u8
    }
}

/// Image.resize(size, Image.BICUBIC)（RGB、8 位元）：先橫向再縱向，定點數運算，結果跟 Pillow 逐位元一樣
pub fn pil_resize_bicubic(im: &RgbImage, w: u32, h: u32) -> RgbImage {
    let (iw, ih) = im.dimensions();
    if (iw, ih) == (w, h) {
        return im.clone(); // Pillow：大小一樣直接複製
    }
    let half = 1i64 << (PRECISION_BITS - 1);
    // 橫向
    let tmp = if iw != w {
        let (ks, bounds, kk) = coeffs(iw as usize, w as usize);
        let mut out = RgbImage::new(w, ih);
        for y in 0..ih {
            for (xx, &(xmin, n)) in bounds.iter().enumerate() {
                let k = &kk[xx * ks..xx * ks + n];
                let mut ss = [half; 3];
                for (x, &kx) in k.iter().enumerate() {
                    let p = im.get_pixel((xmin + x) as u32, y);
                    for c in 0..3 {
                        ss[c] += p[c] as i64 * kx as i64;
                    }
                }
                out.put_pixel(
                    xx as u32,
                    y,
                    Rgb([clip8(ss[0]), clip8(ss[1]), clip8(ss[2])]),
                );
            }
        }
        out
    } else {
        im.clone()
    };
    // 縱向
    if ih == h {
        return tmp;
    }
    let (ks, bounds, kk) = coeffs(ih as usize, h as usize);
    let mut out = RgbImage::new(w, h);
    for (yy, &(ymin, n)) in bounds.iter().enumerate() {
        let k = &kk[yy * ks..yy * ks + n];
        for x in 0..w {
            let mut ss = [half; 3];
            for (y, &ky) in k.iter().enumerate() {
                let p = tmp.get_pixel(x, (ymin + y) as u32);
                for c in 0..3 {
                    ss[c] += p[c] as i64 * ky as i64;
                }
            }
            out.put_pixel(
                x,
                yy as u32,
                Rgb([clip8(ss[0]), clip8(ss[1]), clip8(ss[2])]),
            );
        }
    }
    out
}

/// 舊版 Tagger.tag 的前處理：RGBA 疊白底 → 補成白色正方形（置中）→ BICUBIC 縮到 size
pub fn preprocess(rgba: &RgbaImage, size: u32) -> RgbImage {
    let (w, h) = rgba.dimensions();
    let side = w.max(h);
    let mut sq = RgbImage::from_pixel(side, side, Rgb([255, 255, 255]));
    let (ox, oy) = ((side - w) / 2, (side - h) / 2);
    for (x, y, p) in rgba.enumerate_pixels() {
        let c = alpha_composite([255, 255, 255, 255], p.0);
        sq.put_pixel(x + ox, y + oy, Rgb([c[0], c[1], c[2]]));
    }
    pil_resize_bicubic(&sq, size, size)
}

pub fn load_rgba(path: &Path) -> Result<RgbaImage, String> {
    image::open(path)
        .map(|im| im.to_rgba8())
        .map_err(|e| format!("讀不到圖片：{e}"))
}

// ================================================================ 模型
struct Loaded {
    sess: Session,
    input: String,
    names: Vec<String>,
    cats: Vec<i64>,
    size: u32,
}

pub struct Tagger {
    model_dir: PathBuf,
    /// 一次只跑一張（ONNX Runtime 自己會用多執行緒）
    inner: Mutex<Option<Loaded>>,
}

static ORT_READY: OnceLock<Result<(), String>> = OnceLock::new();

/// onnxruntime.dll 的位置：ORT_DYLIB_PATH → 程式旁邊 → 模型資料夾
fn find_dll(model_dir: &Path) -> Option<PathBuf> {
    let mut c = vec![];
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH").filter(|p| !p.is_empty()) {
        c.push(PathBuf::from(p));
    }
    // Linux 是 libonnxruntime.so（tauri.linux.conf.json 放在附帶檔案的資料夾：/usr/lib/<產品名>/）
    let lib = if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" };
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        c.push(dir.join(lib));
    }
    if let Some(dir) = crate::paths::linux_lib_dir() {
        c.push(dir.join(lib));
    }
    if cfg!(debug_assertions) {
        c.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("vendor").join("onnxruntime").join(lib));
    }
    c.push(model_dir.join(lib));
    c.into_iter().find(|p| p.is_file())
}

fn init_ort(model_dir: &Path) -> Result<(), String> {
    ORT_READY
        .get_or_init(|| {
            let dll = find_dll(model_dir)
                .ok_or(if cfg!(windows) {
                    "找不到 onnxruntime.dll（放在程式旁邊，或設環境變數 ORT_DYLIB_PATH）"
                } else {
                    "找不到 libonnxruntime.so（重新安裝繪匣，或設環境變數 ORT_DYLIB_PATH）"
                })?;
            ort::init_from(&dll)
                .map_err(|e| format!("載入 {} 失敗：{e}", dll.display()))?
                .with_name("anima-tagger")
                .commit();
            Ok(())
        })
        .clone()
}

impl Tagger {
    pub fn new(model_dir: &Path) -> Tagger {
        Tagger {
            model_dir: model_dir.to_path_buf(),
            inner: Mutex::new(None),
        }
    }

    pub fn available(&self) -> bool {
        self.model_dir.join("model.onnx").exists()
    }

    fn load(&self) -> Result<Loaded, String> {
        init_ort(&self.model_dir)?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(2)
            .max(2);
        let e = |e: ort::Error<_>| format!("載入反推模型失敗：{e}");
        let sess = Session::builder()
            .map_err(|e| format!("載入反推模型失敗：{e}"))?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(e)?
            .with_intra_threads(threads)
            .map_err(e)?
            .commit_from_file(self.model_dir.join("model.onnx"))
            .map_err(|e| format!("載入反推模型失敗：{e}"))?;
        let mut rd = csv::Reader::from_path(self.model_dir.join("selected_tags.csv"))
            .map_err(|e| format!("讀不到 selected_tags.csv：{e}"))?;
        let hdr = rd.headers().map_err(|e| e.to_string())?.clone();
        let col = |n: &str| {
            hdr.iter()
                .position(|h| h == n)
                .ok_or(format!("selected_tags.csv 少了 {n} 欄"))
        };
        let (ni, ci) = (col("name")?, col("category")?);
        let (mut names, mut cats) = (vec![], vec![]);
        for r in rd.records() {
            let r = r.map_err(|e| e.to_string())?;
            names.push(r[ni].to_string());
            cats.push(
                r[ci]
                    .trim()
                    .parse::<i64>()
                    .map_err(|e| format!("selected_tags.csv 分類欄格式不對：{e}"))?,
            );
        }
        let inp = &sess.inputs()[0];
        let size = inp
            .dtype()
            .tensor_shape()
            .and_then(|s| s.get(1).copied())
            .filter(|&d| d > 0)
            .unwrap_or(448) as u32;
        let input = inp.name().to_string();
        Ok(Loaded {
            sess,
            input,
            names,
            cats,
            size,
        })
    }

    /// 反推一張圖：{"rating", "character": [[tag, 分數]], "general": [[tag, 分數]]}，依分數由高到低
    pub fn tag(&self, path: &Path, general: f64, character: f64) -> Result<Value, String> {
        let rgba = load_rgba(path)?;
        let probs = self.probs(&rgba)?;
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.as_ref().ok_or("反推模型沒有載入")?;
        Ok(results(&probs, &m.names, &m.cats, general, character))
    }

    /// 模型原始輸出（每個 tag 的機率）
    pub fn probs(&self, rgba: &RgbaImage) -> Result<Vec<f32>, String> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(self.load()?);
        }
        let m = g.as_mut().expect("剛載入");
        let sq = preprocess(rgba, m.size);
        let s = m.size as usize;
        let mut x = Vec::with_capacity(s * s * 3);
        for p in sq.pixels() {
            x.extend([p[2] as f32, p[1] as f32, p[0] as f32]); // RGB → BGR
        }
        let t = Tensor::from_array(([1usize, s, s, 3], x)).map_err(|e| format!("反推失敗：{e}"))?;
        let out = m
            .sess
            .run(ort::inputs![m.input.as_str() => t])
            .map_err(|e| format!("反推失敗：{e}"))?;
        let (_, probs) = out[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("反推失敗：{e}"))?;
        Ok(probs.to_vec())
    }
}

/// 機率 → 結果（舊版 tag() 的後半）
pub fn results(
    probs: &[f32],
    names: &[String],
    cats: &[i64],
    general: f64,
    character: f64,
) -> Value {
    // 評級：分類 9 裡機率最高的（一樣高取第一個）
    let mut rating: Option<usize> = None;
    for (i, &c) in cats.iter().enumerate() {
        if c == 9 && rating.is_none_or(|r| probs[i] > probs[r]) {
            rating = Some(i);
        }
    }
    let mut order: Vec<usize> = (0..probs.len().min(names.len())).collect();
    order.sort_by(|&a, &b| probs[b].total_cmp(&probs[a]));
    let (mut ch, mut ge) = (vec![], vec![]);
    for i in order {
        let (p, c, n) = (probs[i] as f64, cats[i], names[i].as_str());
        if SKIP.contains(&n) {
            continue;
        }
        if c == 0 && p >= general {
            ge.push(json!([fmt_tag(n), py::round(p, 3)]));
        } else if c == 4 && p >= character {
            ch.push(json!([fmt_tag(n), py::round(p, 3)]));
        }
    }
    json!({"rating": rating.map(|r| names[r].clone()), "character": ch, "general": ge})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_on_white_matches_pillow() {
        // Pillow：Image.new("RGBA",(1,1),(255,255,255,255)) alpha_composite (200,100,0,128) → (227,177,127,255)
        assert_eq!(
            alpha_composite([255, 255, 255, 255], [200, 100, 0, 128]),
            [227, 177, 127, 255]
        );
        assert_eq!(
            alpha_composite([255, 255, 255, 255], [200, 100, 0, 255]),
            [200, 100, 0, 255]
        );
        assert_eq!(
            alpha_composite([255, 255, 255, 255], [9, 9, 9, 0]),
            [255, 255, 255, 255]
        );
    }
}

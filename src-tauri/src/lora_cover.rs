//! LoRA 庫卡片的封面（2026-11-07）。網友：「老大，这些预载的可以显示预览图吗，全黑有点不好找」；
//! 使用者：「你幫我把所有預覽lora都能有個預覽圖。優先用c站/魔塔的封面預覽。」
//!
//! 程式附的 LoRA 說明（resources\share\lora設定.json）只有預覽圖檔名，圖本身要用 Civitai 下載才會存進 <資料>\previews\；
//! 網盤包、安裝精靈下載的 LoRA 就整片黑。resources\share\lora_covers.json（tools\lora_covers.py 產生）記每個 LoRA 去哪抓封面，
//! 網頁打開 LoRA 庫時把缺圖的名字送給 POST /api/loras/covers，後端在背景照順序抓、存進 previews\、寫回 LoRA 資訊。
//! 這裡是純邏輯（找來源、挑圖、取檔名），網路在 server\api_misc.rs。
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const CIVITAI_API: &str = "https://civitai.com/api/v1";

/// 一個封面來源，照順序試：前面的抓不到才換下一個
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// 直接是圖片網址（清單裡記好的 C 站縮圖、魔塔封面、Hugging Face 的示範圖）。nsfw＝C 站的尺度等級，別的來源沒有
    Img { url: String, nsfw: Option<i64> },
    /// 清單裡的 C 站圖片網址失效（作者換圖）時：重新查這個版本，挑法同 civitai.rs 下載 LoRA
    CivitaiVersion(i64),
}

/// 清單：完整名稱 → 一筆，另有「檔名（小寫）→ 完整名稱」：LoRA 放在別的資料夾也認得（同 setup::merge_lora_notes）
pub struct Covers {
    by_name: Map<String, Value>,
    by_base: HashMap<String, String>,
}

fn base(k: &str) -> String {
    k.rsplit(['/', '\\']).next().unwrap_or(k).to_lowercase()
}

impl Covers {
    pub fn parse(v: &Value) -> Covers {
        let by_name = v["covers"].as_object().cloned().unwrap_or_default();
        let by_base = by_name.keys().map(|k| (base(k), k.clone())).collect();
        Covers { by_name, by_base }
    }

    pub fn entry(&self, name: &str) -> Option<&Value> {
        self.by_name.get(name).or_else(|| self.by_base.get(&base(name)).and_then(|k| self.by_name.get(k)))
    }

    /// 這個 LoRA 的封面來源（清單的 C 站圖 → C 站重查 → 魔塔 → Hugging Face）。meta＝資料庫裡的 LoRA 資訊：
    /// 不在清單裡、但用 Civitai 下載過（有 versionId，預覽圖不見了）的也重查得到
    pub fn sources(&self, name: &str, meta: Option<&Value>) -> Vec<Source> {
        let e = self.entry(name);
        let mut out = vec![];
        let img = |v: Option<&Value>, nsfw: bool| {
            let v = v?;
            let url = v["img"].as_str().filter(|u| u.starts_with("https://"))?;
            Some(Source::Img { url: url.to_string(), nsfw: if nsfw { v["nsfw"].as_i64().or(Some(1)) } else { None } })
        };
        if let Some(s) = img(e.map(|e| &e["civitai"]), true) {
            out.push(s);
        }
        let version = e.and_then(|e| e["civitai"]["version"].as_i64()).or_else(|| meta.and_then(|m| m["versionId"].as_i64()));
        if let Some(v) = version.filter(|v| *v > 0) {
            out.push(Source::CivitaiVersion(v));
        }
        for k in ["modelscope", "hf"] {
            if let Some(s) = img(e.map(|e| &e[k]), false) {
                out.push(s);
            }
        }
        out
    }

    /// 網頁先用來決定要不要模糊（設定「模糊成人預覽圖」）：清單裡 C 站那張圖的尺度等級，沒有就當 1（同下載中心 cover_nsfw）
    pub fn nsfw(&self, name: &str) -> i64 {
        self.entry(name).and_then(|e| e["civitai"]["nsfw"].as_i64()).unwrap_or(1)
    }

    /// 程式附的縮圖（2026-11-08，使用者：「看能不能直接把圖片先直接匯入進去。就不用讓他們抓了」）：
    /// (檔名, 那張圖的尺度等級)。檔案要真的在 res\share\lora_covers\ 裡才算（被刪了就照舊去網路抓）
    pub fn bundled(&self, res: &Path, name: &str) -> Option<(String, i64)> {
        let e = self.entry(name)?;
        let file = e["file"].as_str().filter(|f| bundled_name_ok(f))?;
        bundled_path(res, file)?;
        Some((file.to_string(), e["file_nsfw"].as_i64().unwrap_or(1)))
    }
}

/// 內建縮圖的檔名只會是「16 個小寫十六進位＋.webp」（tools\lora_covers.py 用網址的 SHA256 取名）：擋掉 ..、斜線
fn bundled_name_ok(f: &str) -> bool {
    f.strip_suffix(".webp").is_some_and(|h| h.len() == 16 && h.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)))
}

/// 內建縮圖的路徑（檔名不對或檔案不在＝None）
pub fn bundled_path(res: &Path, file: &str) -> Option<PathBuf> {
    let p = res.join("share").join("lora_covers").join(file);
    (bundled_name_ok(file) && p.is_file()).then_some(p)
}

/// 讀 res\share\lora_covers.json（讀過就留著；換了 res 資料夾才重讀）。檔案不見或壞掉＝空清單
pub fn load(res: &Path) -> Arc<Covers> {
    static CACHE: Mutex<Option<(PathBuf, Arc<Covers>)>> = Mutex::new(None);
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, c)) = g.as_ref() {
        if p == res {
            return c.clone();
        }
    }
    let v = std::fs::read(res.join("share").join("lora_covers.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    let c = Arc::new(Covers::parse(&v));
    *g = Some((res.to_path_buf(), c.clone()));
    c
}

/// C 站版本資訊 → (縮圖網址, 尺度等級)：只看圖片、尺度最低的那張，寬 450（同 civitai.rs 下載 LoRA、modelhub 的封面）
pub fn pick_civitai(v: &Value) -> Option<(String, i64)> {
    v["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i.get("type").and_then(Value::as_str).unwrap_or("image") == "image")
        .filter_map(|i| Some((i["url"].as_str()?, i["nsfwLevel"].as_i64().unwrap_or(1))))
        .min_by_key(|(_, n)| *n)
        .map(|(u, n)| (crate::civitai::img_small(u, 450), n))
}

/// 存進 previews\ 的檔名：LoRA 資訊裡本來就寫了預覽圖檔名（程式附的說明都有）就用它；沒有用「檔名.jpg」，
/// 已經被別的 LoRA 用掉（不同資料夾的同名檔）就加上名稱的短雜湊。副檔名照 civitai.rs 一律 .jpg（瀏覽器看內容，不看副檔名）
pub fn file_name(name: &str, meta: Option<&Value>, previews: &Path) -> String {
    if let Some(p) = meta.and_then(|m| m["preview"].as_str()).map(crate::server::basename).filter(|p| !p.is_empty()) {
        return p;
    }
    let stem = Path::new(&crate::server::basename(name)).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let stem = if stem.is_empty() { "lora".to_string() } else { stem };
    let plain = format!("{stem}.jpg");
    if !previews.join(&plain).exists() {
        return plain;
    }
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(name.as_bytes()).iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{stem}_{h}.jpg")
}

/// 抓回來的真的是圖（JPEG／PNG／WebP／GIF）：擋掉登入頁、錯誤頁
pub fn is_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"\x89PNG")
        || (b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP")
        || b.starts_with(b"GIF8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Covers {
        Covers::parse(&json!({"covers": {
            "moss\\Excellent-Loras\\风格\\a.safetensors": {
                "civitai": {"model": 1, "version": 11, "img": "https://image.civitai.com/x/width=450/a.jpeg", "nsfw": 2},
                "modelscope": {"id": "o/n", "img": "https://www.modelscope.cn/models/o/n/resolve/master/banner.jpg"}},
            "b.safetensors": {"hf": {"img": "https://huggingface.co/x/resolve/main/b.jpg"}},
            "bad.safetensors": {"civitai": {"img": "http://insecure/x.jpg"}}
        }}))
    }

    #[test]
    fn sources_follow_civitai_then_modelscope_then_hf() {
        let c = sample();
        let s = c.sources("moss\\Excellent-Loras\\风格\\a.safetensors", None);
        assert_eq!(s, vec![
            Source::Img { url: "https://image.civitai.com/x/width=450/a.jpeg".into(), nsfw: Some(2) },
            Source::CivitaiVersion(11),
            Source::Img { url: "https://www.modelscope.cn/models/o/n/resolve/master/banner.jpg".into(), nsfw: None },
        ]);
        // 放在別的資料夾（照合集 README 放）也認得
        assert_eq!(c.sources("loras2\\A.safetensors", None).len(), 3);
        assert_eq!(c.nsfw("x\\a.safetensors"), 2);
        assert_eq!(c.sources("b.safetensors", None), vec![Source::Img { url: "https://huggingface.co/x/resolve/main/b.jpg".into(), nsfw: None }]);
        // 只收 https
        assert!(c.sources("bad.safetensors", None).is_empty());
        // 不在清單裡、但下載過（有 versionId）的重查 C 站
        assert_eq!(c.sources("mine.safetensors", Some(&json!({"versionId": 99}))), vec![Source::CivitaiVersion(99)]);
        assert!(c.sources("mine.safetensors", Some(&json!({"note": "x"}))).is_empty());
        assert_eq!(c.nsfw("mine.safetensors"), 1);
    }

    #[test]
    fn picks_lowest_rated_still_image() {
        let v = json!({"images": [
            {"url": "https://image.civitai.com/x/original=true/1.jpeg", "nsfwLevel": 4},
            {"url": "https://image.civitai.com/x/original=true/2.mp4", "nsfwLevel": 1, "type": "video"},
            {"url": "https://image.civitai.com/x/width=1024/3.jpeg", "nsfwLevel": 2}]});
        assert_eq!(pick_civitai(&v), Some(("https://image.civitai.com/x/width=450/3.jpeg".into(), 2)));
        assert_eq!(pick_civitai(&json!({"images": []})), None);
    }

    #[test]
    fn file_names_reuse_meta_and_avoid_clashes() {
        let dir = std::env::temp_dir().join(format!("anima_lora_cover_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(file_name("sub\\x.safetensors", Some(&json!({"preview": "x.jpg"})), &dir), "x.jpg");
        assert_eq!(file_name("sub\\y.safetensors", None, &dir), "y.jpg");
        std::fs::write(dir.join("y.jpg"), b"x").unwrap();
        let other = file_name("other\\y.safetensors", None, &dir);
        assert!(other.starts_with("y_") && other.ends_with(".jpg") && other.len() == "y_12345678.jpg".len(), "{other}");
        assert_eq!(file_name("..\\..\\z.safetensors", Some(&json!({"preview": "..\\evil.jpg"})), &dir), "evil.jpg");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_real_images_pass() {
        assert!(is_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(is_image(b"\x89PNG\r\n"));
        assert!(is_image(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!is_image(b"<!DOCTYPE html>"));
    }

    #[test]
    fn bundled_list_parses() {
        let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("resources");
        let c = load(&res);
        assert!(c.entry("hinaAnimaBase_R_Tweaker_v4.safetensors").is_some(), "lora_covers.json 讀不到或缺了截圖裡的例子");
        assert!(!c.sources("Meion_anima_style_lokr_2-000034.safetensors", None).is_empty());
        // 截圖裡那幾張都有程式附的縮圖，檔案真的在
        for n in ["hinaAnimaBase_R_Tweaker_v4.safetensors", "Meion_anima_style_lokr_2-000034.safetensors", "anima-base-1-masterpiece-v51.safetensors",
                  "mixed_styles_anima-base1_v6.safetensors", "Anima_colorfix_v1_by_Volnovik.safetensors", "anima_context_detailer_base10.safetensors"] {
            let (f, _) = c.bundled(&res, n).unwrap_or_else(|| panic!("{n} 沒有內建縮圖"));
            assert!(is_image(&std::fs::read(bundled_path(&res, &f).unwrap()).unwrap()), "{f}");
        }
    }

    #[test]
    fn bundled_needs_safe_name_and_real_file() {
        let dir = std::env::temp_dir().join(format!("anima_lora_cover_res_{}", std::process::id()));
        let share = dir.join("share").join("lora_covers");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(share.join("0123456789abcdef.webp"), b"RIFF\0\0\0\0WEBPVP8 ").unwrap();
        let c = Covers::parse(&json!({"covers": {
            "a.safetensors": {"file": "0123456789abcdef.webp", "file_nsfw": 4},
            "gone.safetensors": {"file": "fedcba9876543210.webp"},
            "evil.safetensors": {"file": "..\\..\\x.webp"}
        }}));
        assert_eq!(c.bundled(&dir, "sub\\a.safetensors"), Some(("0123456789abcdef.webp".into(), 4)));
        assert_eq!(c.bundled(&dir, "gone.safetensors"), None);
        assert_eq!(c.bundled(&dir, "evil.safetensors"), None);
        assert!(bundled_path(&dir, "../0123456789abcdef.webp").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

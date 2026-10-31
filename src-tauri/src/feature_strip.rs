//! 「一鍵脫衣」的內容補丁（2026-10-29）：露骨提示詞和 UI 文案放外部檔 `<資料>\plugins\strip\pack.json`，
//! 公開版不帶這個檔，私人版在 resources 內建。graph 機制（換家族改圖＋差異遮罩）在 graphs.rs，是中性的；
//! 這裡只負責載入補丁、照 target／keep 查出對應文字。沒有補丁 → load 回 None，功能停用。
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const PACK: &str = "pack.json";

/// 使用者放補丁的位置
pub fn dir(data: &Path) -> PathBuf {
    data.join("plugins").join("strip")
}

pub struct StripPack {
    v: Value,
}

/// 找補丁：先使用者裝的 `<資料>\plugins\strip\pack.json`，沒有再找內建的 `<resources>\extras\strip\pack.json`（公開版沒有）
pub fn load(data: &Path, res: &Path) -> Option<StripPack> {
    for p in [dir(data).join(PACK), res.join("extras").join("strip").join(PACK)] {
        let Ok(b) = std::fs::read(&p) else { continue };
        if let Ok(v) = serde_json::from_slice::<Value>(&b) {
            if v.get("qwen").is_some() {
                return Some(StripPack { v });
            }
        }
    }
    None
}

impl StripPack {
    /// 給網頁的 UI 文案（按鈕、選項、說明）
    pub fn ui(&self) -> Value {
        self.v.get("ui").cloned().unwrap_or(Value::Null)
    }

    fn text(&self, group: &str, key: &str) -> String {
        self.v[group][key].as_str().unwrap_or("").to_string()
    }

    /// Qwen 改圖指令：照 `target_keep` 查，接上使用者的補充指令
    pub fn qwen_prompt(&self, target: &str, keep: &str, extra: &str) -> String {
        let mut s = self.text("qwen", &format!("{target}_{keep}"));
        let extra = extra.trim();
        if !s.is_empty() && !extra.is_empty() {
            s.push('\n');
            s.push_str(extra);
        }
        s
    }

    /// Anima 精修 tag：先照 `target_keep` 查；補丁沒有這組就只分 underwear 與否（舊補丁沒有 _shoes），接上使用者補充 tag
    pub fn refine_tags(&self, target: &str, keep: &str, extra: &str) -> String {
        let k = if keep == "underwear" { "underwear" } else { "all" };
        let mut s = self.text("refine", &format!("{target}_{keep}"));
        if s.is_empty() {
            s = self.text("refine", &format!("{target}_{k}"));
        }
        let extra = extra.trim();
        if !s.is_empty() && !extra.is_empty() {
            s.push_str(", ");
            s.push_str(extra);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pack(v: Value) -> StripPack {
        StripPack { v }
    }

    #[test]
    fn looks_up_by_target_and_keep_and_appends_extra() {
        let p = pack(json!({
            "qwen": {"female_all": "A", "male_underwear": "B"},
            "refine": {"female_all": "ra", "female_underwear": "ru"},
            "ui": {"button": "X"}
        }));
        assert_eq!(p.qwen_prompt("female", "all", ""), "A");
        assert_eq!(p.qwen_prompt("female", "all", "keep hat"), "A\nkeep hat");
        assert_eq!(p.qwen_prompt("male", "underwear", ""), "B");
        // 補丁沒有 _shoes 那組：keep=shoes 走 _all；有就用它
        assert_eq!(p.refine_tags("female", "shoes", ""), "ra");
        let p2 = pack(json!({"qwen": {}, "refine": {"female_all": "ra", "female_shoes": "rs"}}));
        assert_eq!(p2.refine_tags("female", "shoes", "x"), "rs, x");
        assert_eq!(p.refine_tags("female", "underwear", "miku"), "ru, miku");
        // 補丁缺這組 → 空字串，不會拼出只有 extra 的句子
        assert_eq!(p.qwen_prompt("male", "all", "x"), "");
        assert_eq!(p.ui()["button"], "X");
    }

    #[test]
    fn missing_pack_is_none() {
        let d = std::env::temp_dir().join(format!("anima_strip_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(load(&d, &d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}

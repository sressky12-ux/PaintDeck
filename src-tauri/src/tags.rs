//! 提示詞自動完成：Danbooru tag（依使用次數排序）＋中文對照（輸入中文也能找到英文 tag）。對照舊版 tags.py。
//!
//! 資料：
//!   data\tags\danbooru.csv   來自 a1111-sd-webui-tagcomplete（tag,category,count,aliases）
//!   data\tags\zh_map.json    由使用者提供的提示詞對照表轉出（中文 → 英文 tag）
//!   resources\tags\t2s_chars.json  繁→簡逐字對照（編進程式）：對照表是繁體，簡體輸入折成簡體再比（2026-10-21）
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

static CJK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[㐀-鿿]").unwrap());

/// 繁體字 → 簡體字逐字對照（resources\tags\t2s_chars.json，由 tools\make_t2s_chars.mjs 從 OpenCC 字典產生）。
/// 編進程式裡，不用另外找檔：使用者自己的 data\tags 換了 danbooru.csv／zh_map.json 也不影響，poses.rs 的姿勢比對共用同一份
const T2S_JSON: &str = include_str!("../../resources/tags/t2s_chars.json");

static T2S: LazyLock<HashMap<char, char>> = LazyLock::new(|| {
    let v: Value = serde_json::from_str(T2S_JSON).unwrap_or_default();
    v["map"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, s)| {
            let (mut k, mut s) = (k.chars(), s.as_str()?.chars());
            match (k.next(), k.next(), s.next(), s.next()) {
                (Some(a), None, Some(b), None) => Some((a, b)),
                _ => None,
            }
        })
        .collect()
});

/// 把繁體字逐字折成簡體：對照表（zh_map.json、poses.json）是繁體，使用者可能打簡體，兩邊都折過再比就找得到。
/// 簡體字和沒有簡體寫法的字原樣不動；一對多（髮／發 → 发、乾／幹 → 干）折在一起，所以折過的字串只拿來比對，不拿來顯示
pub fn fold_zh(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    s.chars().map(|c| *T2S.get(&c).unwrap_or(&c)).collect()
}

fn cat_name(c: i64) -> &'static str {
    match c {
        0 => "一般",
        1 => "畫師",
        3 => "作品",
        4 => "角色",
        5 => "後設",
        _ => "",
    }
}

/// Python int(s or 0)：空字串當 0，其他解析失敗也當 0（舊版會丟例外，資料檔裡沒有這種列）
fn int0(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

#[derive(Default)]
pub struct TagIndex {
    rows: Vec<(String, i64, i64, Vec<String>)>,
    zh: HashMap<String, String>,
    zh_rows: Vec<(String, String)>,
    zh_fold: Vec<String>, // zh_rows 每一列的中文折成簡體（fold_zh），一列對一列，簡體輸入用
    count: HashMap<String, i64>,
}

impl TagIndex {
    pub fn load(dir: &Path) -> TagIndex {
        let mut ix = TagIndex::default();
        if let Ok(mut rd) = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_path(dir.join("danbooru.csv"))
        {
            for r in rd.records().flatten() {
                let f = |i: usize| r.get(i).unwrap_or("");
                let aliases: Vec<String> = if r.len() > 3 && !f(3).is_empty() {
                    f(3).split(',').map(str::to_owned).collect()
                } else {
                    vec![]
                };
                if r.len() < 3 {
                    continue;
                }
                ix.rows
                    .push((f(0).to_string(), int0(f(1)), int0(f(2)), aliases));
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("zh_map.json")) {
            if let Ok(Value::Array(pairs)) = serde_json::from_str::<Value>(&text) {
                for p in pairs {
                    let (Some(zh), Some(en)) = (p[0].as_str(), p[1].as_str()) else {
                        continue;
                    };
                    let en = en.replace(' ', "_");
                    ix.zh
                        .entry(en.to_lowercase())
                        .or_insert_with(|| zh.to_string());
                    ix.zh_fold.push(fold_zh(zh));
                    ix.zh_rows.push((zh.to_string(), en));
                }
            }
        }
        ix.count = ix.rows.iter().map(|(t, _, c, _)| (t.clone(), *c)).collect();
        ix
    }

    /// 一個 tag 的中文對照（反推視窗用）；對照表是簡轉繁，「头发」會被轉成「頭發」，跟頭髮有關的改回「髮」。
    /// 對照表裡翻錯或沒有、反推又常出現的，用 ZH_FIX 蓋過去
    pub fn zh_of(&self, tag: &str) -> Option<String> {
        const ZH_FIX: &[(&str, &str)] = &[
            ("upskirt", "裙底視角"), ("pantyshot", "露出內褲"), ("lace-trimmed_legwear", "蕾絲邊襪"),
            ("painting_(object)", "畫作（物品）"), ("drawing_(object)", "畫（物品）"), ("portrait_(object)", "肖像畫（物品）"),
            ("red_lips", "紅唇"), ("realistic", "寫實"), ("nose", "鼻子（畫得明顯）"), ("lips", "嘴唇（畫得明顯）"),
            ("asian", "亞洲人"), ("on_couch", "在沙發上"), ("bent_over", "彎腰"), ("from_behind", "從背後看"),
            ("hand_on_own_hip", "單手叉腰"), ("hands_on_own_hips", "雙手叉腰"), ("upper_teeth_only", "只露上排牙齒"),
            ("puff_of_air", "噴氣（生氣或嘆氣）"), ("stuffed_rabbit", "兔子布偶"), ("shoulder_boards", "肩章"),
            ("art_tools_in_frame", "畫具入鏡"), ("pink_halo", "粉紅光環"), ("tarot_(medium)", "塔羅牌風格"),
            ("dress", "洋裝"), ("closed_mouth", "閉著嘴"), ("school_uniform", "制服"),
            ("grey_hair", "灰髮"), ("short_sleeves", "短袖"), ("sidelocks", "耳前側髮"),
            ("cowboy_shot", "七分身（大腿以上）"), ("pantyhose", "褲襪"), ("nude", "裸體"),
            ("open_clothes", "衣服敞開"), ("parted_lips", "微張的嘴唇"), ("censored", "打碼的"),
            ("collared_shirt", "有領襯衫"), ("japanese_clothes", "日式服裝"), ("medium_hair", "中長髮"),
            ("puffy_sleeves", "泡泡袖"), ("fate_(series)", "Fate（系列）"), ("fang", "虎牙"),
            ("blunt_bangs", "齊瀏海"), ("stomach", "肚子"), ("speech_bubble", "對話框"),
            ("sex", "性愛"), ("white_dress", "白色洋裝"), ("on_back", "仰躺"),
            ("blue_archive", "蔚藍檔案"), ("fate/grand_order", "Fate/Grand Order（FGO）"), ("orange_hair", "橘髮"),
            ("star_(symbol)", "星形"), ("black_dress", "黑色洋裝"), ("blurry", "模糊的"),
            ("from_side", "從側面看"), ("white_thighhighs", "白色大腿襪"), ("arm_up", "抬起一隻手臂"),
            ("clothes_lift", "掀起衣服"), ("leotard", "緊身連身衣"), ("black_pantyhose", "黑色褲襪"),
            ("bracelet", "手環"), ("arms_up", "舉起雙手"), ("two_side_up", "兩側綁髮"),
            ("orange_eyes", "橘眼"), ("covered_nipples", "遮住乳頭"), ("clothing_cutout", "服裝鏤空"),
            ("wrist_cuffs", "腕套"), ("bar_censor", "條狀打碼"), ("sleeveless_dress", "無袖洋裝"),
            ("torn_clothes", "破損的衣服"), ("short_shorts", "熱褲"), ("buttons", "鈕扣"),
            ("gradient_background", "漸層背景"), ("ascot", "領巾"), ("gradient_hair", "漸層頭髮"),
            ("one-piece_swimsuit", "連身泳衣"), ("tattoo", "刺青"), ("uncensored", "無碼"),
            ("hoodie", "連帽T"), ("profile", "側臉"), ("floating_hair", "飄浮的頭髮"),
            ("double_bun", "雙丸子頭"), ("dress_shirt", "襯衫"), ("bodysuit", "緊身連身衣"),
            ("blue_dress", "藍洋裝"), ("swept_bangs", "掃瀏海"), ("dutch_angle", "傾斜鏡頭"),
            ("bell", "鈴鐺"), ("open_shirt", "敞開的襯衫"), ("hair_intakes", "頭頂兩撮翹髮"),
            ("watermark", "浮水印"), ("one_side_up", "單側綁髮"), ("facial_mark", "臉部印記"),
            ("side-tie_bikini_bottom", "側綁帶比基尼泳褲"), ("black_socks", "黑色襪子"), ("sunglasses", "墨鏡"),
            ("bob_cut", "鮑伯頭"), ("no_shoes", "沒穿鞋"), ("frilled_dress", "花邊洋裝"),
            ("short_dress", "短洋裝"), ("red_dress", "紅洋裝"), ("fire_emblem", "聖火降魔錄"),
            ("drill_hair", "鑽頭捲髮"), ("thigh_boots", "過膝長靴"), ("flying_sweatdrops", "飛出的汗珠"),
            ("traditional_media", "傳統媒材"), ("arms_behind_back", "雙手背後"), ("stuffed_toy", "絨毛玩偶"),
            ("crossed_arms", "雙手交叉抱胸"), ("cleavage_cutout", "乳溝鏤空"), ("garter_straps", "吊襪帶"),
            ("animated", "動圖"), ("obi", "和服腰帶"), ("portrait", "肖像"),
            ("shirt_lift", "掀起上衣"), ("oral", "口交"), ("final_fantasy", "Final Fantasy（太空戰士）"),
            ("tank_top", "背心（細肩帶）"), ("scrunchie", "大腸髮圈"), ("crossed_legs", "翹二郎腿"),
            ("colored_inner_hair", "內層染髮"), ("partial_commentary", "部分註解"), ("backpack", "背包"),
            ("mob_cap", "軟布帽"), ("blazer", "西裝外套"), ("yaoi", "BL（耽美）"),
            ("spiked_hair", "尖刺髮"), ("single_hair_bun", "單丸子頭"), ("black_leotard", "黑色緊身連身衣"),
            ("juliet_sleeves", "茱麗葉袖"), ("light_blush", "淡淡臉紅"), ("slit_pupils", "豎瞳"),
            ("underwear_only", "只穿內衣褲"), ("hat_ribbon", "帽子緞帶"), ("grey_shirt", "灰色襯衫"),
            ("sisters", "姊妹"), ("crossover", "跨界合作"), ("breasts_out", "露出乳房"),
            ("polka_dot", "圓點"), ("fishnets", "網襪"), ("bandaid", "OK繃"),
            ("pink_dress", "粉色洋裝"), ("black_bra", "黑胸罩"), ("skin_fang", "露出的虎牙"),
            ("tan", "曬黑"), ("hat_bow", "帽子蝴蝶結"), ("crossed_bangs", "交叉瀏海"),
            ("letterboxed", "上下黑邊（寬螢幕）"), ("otoko_no_ko", "偽娘（男の娘）"), ("hair_bobbles", "髮圈"),
            ("cherry_blossoms", "櫻花"), ("armband", "臂章"), ("revealing_clothes", "暴露衣著"),
            ("surprised", "驚訝"), ("remilia_scarlet", "蕾米莉亞·斯卡蕾特"), ("flandre_scarlet", "芙蘭朵露·斯卡蕾特"),
            ("meme", "迷因"), ("anal", "肛交"), ("low_ponytail", "低馬尾"),
            ("danganronpa_(series)", "槍彈辯駁（系列）"), ("strapless_dress", "無肩帶洋裝"), ("bike_shorts", "單車褲"),
            ("pelvic_curtain", "腰間布簾"), ("holding_phone", "拿著手機"), ("white_pantyhose", "白色褲襪"),
            ("large_pectorals", "大胸肌"), ("waist_apron", "半身圍裙"), ("purple_dress", "紫洋裝"),
            ("gundam", "鋼彈"),
        ];
        // Anima 提示詞開頭常見的品質、分級、年代詞：對照表沒有，或照 Danbooru 原意翻錯（safe 會變成保險箱）
        const META: &[(&str, &str)] = &[
            ("masterpiece", "傑作（品質詞）"), ("best_quality", "最高品質"), ("high_quality", "高品質"),
            ("good_quality", "好品質"), ("normal_quality", "普通品質"), ("low_quality", "低品質"),
            ("worst_quality", "最差品質"), ("amazing_quality", "極佳品質"),
            ("very_aesthetic", "非常好看（美感詞）"), ("aesthetic", "好看（美感詞）"),
            ("displeasing", "難看（美感詞）"), ("very_displeasing", "非常難看（美感詞）"),
            ("safe", "分級：全年齡"), ("general", "分級：全年齡"), ("sensitive", "分級：輕微性感"),
            ("questionable", "分級：擦邊"), ("nsfw", "成人內容"), ("explicit", "分級：露骨成人"),
            ("newest", "年代：最新"), ("recent", "年代：近年"), ("mid", "年代：中期"),
            ("early", "年代：早期"), ("old", "年代：老圖"),
        ];
        let key = tag.trim().replace("\\(", "(").replace("\\)", ")").replace(' ', "_").to_lowercase();
        if key.len() == 4 && key.chars().all(|c| c.is_ascii_digit()) {
            return Some("年份".into());
        }
        if let Some((_, z)) = META.iter().find(|(k, _)| *k == key) {
            return Some(z.to_string());
        }
        // year_2025、score_9、score_7_up
        if let Some(y) = key.strip_prefix("year_").filter(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit())) {
            return Some(format!("{y} 年"));
        }
        if let Some(s) = key.strip_prefix("score_") {
            let (n, up) = s.strip_suffix("_up").map_or((s, false), |n| (n, true));
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                return Some(format!("評分 {n}{}", if up { " 以上" } else { "" }));
            }
        }
        if let Some((_, z)) = ZH_FIX.iter().find(|(k, _)| *k == key) {
            return Some(z.to_string());
        }
        let zh = self.zh.get(&key)?;
        Some(if key.contains("hair") { zh.replace('發', "髮") } else { zh.clone() })
    }

    /// 這個 tag 在 danbooru.csv 裡有沒有（空白、底線、大小寫都算同一個）；資產庫存 Anima tag 時檢查用
    pub fn has(&self, tag: &str) -> bool {
        let key = tag.trim().replace(' ', "_").to_lowercase();
        !key.is_empty() && self.count.contains_key(&key)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.zh_rows.is_empty()
    }

    fn cnt(&self, en: &str) -> i64 {
        *self
            .count
            .get(&en.replace(' ', "_").to_lowercase())
            .unwrap_or(&0)
    }

    pub fn search(&self, q: &str, limit: usize) -> Vec<Value> {
        let q = q.trim();
        if q.is_empty() {
            return vec![];
        }
        let mut out = vec![];
        if CJK.is_match(q) {
            // 對照表是繁體。輸入含繁體專用字（折成簡體會變）就照舊只比原文，結果跟以前完全一樣；
            // 輸入是簡體（折了不變）才多比一次折成簡體的對照表，這些命中排在原文命中後面（長发 → 長髮、后入 → 後入式）
            let simp = fold_zh(q) == q;
            let mut hits: Vec<(u8, &(String, String))> = vec![];
            for (i, row) in self.zh_rows.iter().enumerate() {
                let rank = if row.0.starts_with(q) {
                    0
                } else if row.0.contains(q) {
                    1
                } else if simp && self.zh_fold[i].starts_with(q) {
                    2
                } else if simp && self.zh_fold[i].contains(q) {
                    3
                } else {
                    continue;
                };
                hits.push((rank, row));
            }
            hits.sort_by_key(|(rank, (_, en))| (*rank, -self.cnt(en)));
            let mut seen = HashSet::new();
            for (_, (zh, en)) in hits {
                if !seen.insert(en.to_lowercase()) {
                    continue;
                }
                out.push(json!({"tag": en.replace('_', " "), "zh": zh, "count": self.cnt(en), "cat": "中文"}));
                if out.len() >= limit {
                    break;
                }
            }
            return out;
        }
        let key = q.to_lowercase().replace(' ', "_");
        let (mut pre, mut sub) = (vec![], vec![]);
        for (tag, cat, cnt, aliases) in &self.rows {
            if tag.starts_with(&key) {
                pre.push((tag, *cat, *cnt, None));
            } else if let Some(a) = aliases.iter().find(|a| a.starts_with(&key)) {
                pre.push((tag, *cat, *cnt, Some(a)));
            } else if key.chars().count() >= 3 && tag.contains(&key) {
                sub.push((tag, *cat, *cnt, None));
            }
        }
        pre.sort_by_key(|r| -r.2);
        sub.sort_by_key(|r| -r.2);
        for (tag, cat, cnt, alias) in pre.into_iter().chain(sub).take(limit) {
            out.push(json!({"tag": tag.replace('_', " "), "zh": self.zh.get(tag.as_str()).cloned().unwrap_or_default(),
                            "count": cnt, "cat": cat_name(cat), "alias": alias}));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> TagIndex {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("resources")
            .join("tags");
        TagIndex::load(&dir)
    }

    /// 改動前的中文搜尋（原文 contains、原文開頭的排前面、再依使用次數），拿來證明繁體輸入的結果沒變
    fn legacy_cjk(ix: &TagIndex, q: &str, limit: usize) -> Vec<(String, String)> {
        let mut hits: Vec<&(String, String)> =
            ix.zh_rows.iter().filter(|(zh, _)| zh.contains(q)).collect();
        hits.sort_by_key(|(zh, en)| (!zh.starts_with(q), -ix.cnt(en)));
        let mut seen = HashSet::new();
        hits.into_iter()
            .filter(|(_, en)| seen.insert(en.to_lowercase()))
            .take(limit)
            .map(|(zh, en)| (en.replace('_', " "), zh.clone()))
            .collect()
    }

    fn found(ix: &TagIndex, q: &str, limit: usize) -> Vec<(String, String)> {
        ix.search(q, limit)
            .iter()
            .map(|v| {
                (
                    v["tag"].as_str().unwrap().to_string(),
                    v["zh"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn has_matches_space_or_underscore() {
        let ix = index();
        assert!(ix.has("orange hair"));
        assert!(ix.has("orange_hair"));
        assert!(ix.has(" Orange Hair "));
        assert!(!ix.has("not a tag zz"));
        assert!(!ix.has(""));
    }

    #[test]
    fn fold_table_loads() {
        assert!(T2S.len() > 3000, "{}", T2S.len());
        assert_eq!(fold_zh("長髮"), "长发");
        assert_eq!(fold_zh("長發"), "长发"); // 髮、發折在一起
        assert_eq!(fold_zh("傳教士體位"), "传教士体位");
        assert_eq!(fold_zh("长发 long_hair"), "长发 long_hair"); // 簡體、英文原樣
    }

    #[test]
    fn simplified_finds_same_tags_as_traditional() {
        let ix = index();
        assert!(!ix.is_empty());
        let tr = found(&ix, "長髮", 12);
        assert_eq!(tr[0].0, "long hair");
        assert_eq!(found(&ix, "长发", 12), tr);
        for (simp, trad, tag) in [
            ("后入", "後入", "sex from behind"),
            ("蓝眼", "藍眼", "blue eyes"),
            ("内裤", "內褲", "panties"),
            ("头发", "頭髮", "hair between eyes"),
        ] {
            let s = found(&ix, simp, 40);
            assert!(s.iter().any(|(t, _)| t == tag), "{simp}: {s:?}");
            assert_eq!(
                s.iter().take(5).collect::<Vec<_>>(),
                found(&ix, trad, 40).iter().take(5).collect::<Vec<_>>(),
                "{simp}"
            );
        }
        // 英文查詢不受影響
        assert_eq!(ix.search("long_ha", 3)[0]["tag"], "long hair");
    }

    #[test]
    fn traditional_queries_unchanged() {
        let ix = index();
        // 含繁體專用字的查詢：結果跟改動前逐筆相同
        for q in [
            "長髮",
            "藍眼",
            "頭髮",
            "內褲",
            "後入式",
            "體位",
            "騎乘",
            "臉紅",
            "雙馬尾",
            "看向觀眾",
            "裡",
            "乾",
            "髮",
        ] {
            let old = legacy_cjk(&ix, q, 12);
            assert!(
                !old.is_empty(),
                "{q} 在對照表裡要有東西，不然這個測試什麼都沒證明"
            );
            assert_eq!(found(&ix, q, 12), old, "{q}");
        }
        // 繁簡同形的字（簡體輸入也可能是它）：改動前的命中照原順序排在最前面，只可能在後面多出簡體命中
        for q in ["笑", "面", "后", "里"] {
            let old = legacy_cjk(&ix, q, 12);
            let new = found(&ix, q, 12);
            assert_eq!(
                new[..old.len().min(new.len())],
                old[..old.len().min(new.len())],
                "{q}"
            );
        }
    }

    #[test]
    fn zh_of_meta_and_dictionary() {
        let ix = index();
        assert_eq!(ix.zh_of("masterpiece").as_deref(), Some("傑作（品質詞）"));
        assert_eq!(ix.zh_of("Best Quality").as_deref(), Some("最高品質"));
        assert_eq!(ix.zh_of("safe").as_deref(), Some("分級：全年齡"));
        assert_eq!(ix.zh_of("year 2025").as_deref(), Some("2025 年"));
        assert_eq!(ix.zh_of("score_7_up").as_deref(), Some("評分 7 以上"));
        assert_eq!(ix.zh_of("score_9").as_deref(), Some("評分 9"));
        assert!(ix.zh_of("score_x").is_none());
        // 對照表本來就有的照舊；空白、底線、跳脫括號都算同一個
        assert!(ix.zh_of("long hair").is_some_and(|z| z.contains('髮')));
        assert_eq!(ix.zh_of("1girl"), ix.zh_of("1girl "));
        assert!(ix.zh_of("a woman sitting on a bench at sunset").is_none());
    }

    #[test]
    fn zh_fix_taiwan_terms() {
        let ix = index();
        assert_eq!(ix.zh_of("orange_hair").as_deref(), Some("橘髮"));
        for (tag, zh) in [
            ("cowboy_shot", "七分身（大腿以上）"),
            ("oral", "口交"),
            ("large_pectorals", "大胸肌"),
            ("blue_archive", "蔚藍檔案"),
        ] {
            assert_eq!(ix.zh_of(tag).as_deref(), Some(zh), "{tag}");
        }
    }
}

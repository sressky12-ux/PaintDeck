//! 反推「自動清理」：把 WD tagger 的結果整理成好用的 Anima 提示詞，每個建議拿掉的 tag 都附原因（網頁上變灰、點一下可以加回）。
//!
//! 規則（照順序）：
//!   1. 雜訊：簽名、浮水印、網址這類一律拿掉（生圖時會畫出字）。
//!   2. 寫實／媒材／畫風類：只在掛著「有觸發詞的 LoRA」時拿掉（realistic、nose、lips…會跟畫風 LoRA 打架；
//!      沒掛 LoRA 時可能就是想要寫實，所以留著）。
//!   3. 衝突：同一組只能有一個的（髮色、眼睛顏色、髮長、胸部大小、構圖、姿勢）只留分數最高的；
//!      - 有 multicolored hair／heterochromia 這類「本來就有兩種」的 tag 時，髮色／眼色不限制；
//!      - 多人圖（2girls、multiple girls、1girl＋1boy…）不比髮色、眼色、髮長、胸部、姿勢（每個人可以不一樣）；
//!      - 第二名分數要低於第一名的某個倍數才拿掉，分數接近就兩個都留（寧可多留、不要刪錯）：
//!        髮色／眼色／髮長 0.6、人數／構圖 0.8、胸部／姿勢一律只留第一名（160 張 Danbooru 圖實測調出來的）；
//!      - 人數（1girl／2girls…）最先比，比完才判斷是不是多人圖；
//!      - 有包含關係的不算衝突（very long hair 包含 long hair → 交給第 4 條）。
//!   4. 重複：留下的 tag 已經隱含的上層 tag 拿掉（pink rose 已經代表 rose、flower）。
//!      隱含關係在 resources\tag_rules.json（tools\tag_clean_proto\build_tag_rules.py 從 Danbooru 抓的）。
//!
//! 排序：人數（1girl、solo…）放最前，其餘照分數；觸發詞由網頁插在品質詞後面。
//! 原型（Python，同一套規則）：tools\tag_clean_proto\tag_clean.py。
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const RULES: &str = include_str!("../../resources/tag_rules.json");

pub(crate) const NOISE: &[&str] = &[
    "signature", "artist name", "watermark", "web address", "dated", "twitter username", "patreon username",
    "pixiv username", "copyright notice", "artist logo", "company name", "character name", "sample watermark",
    "jpeg artifacts", "lowres", "highres", "absurdres", "commentary request",
    "english text", "japanese text", "chinese text", "korean text", "translated", "translation request", "copyright name",
];
pub(crate) const REAL: &[&str] = &[
    "realistic", "photorealistic", "photo (medium)", "3d", "nose", "lips", "asian", "traditional media",
    "painting (medium)", "oil painting (medium)", "watercolor (medium)", "acrylic paint (medium)", "photo background",
    "flat color", "outline", "thick outlines", "white outline", "black outline", "lineart", "cel shading",
    "anime coloring", "no lineart", "sketch", "faux traditional media", "impasto", "official style",
    "retro artstyle", "partially colored", "anime screencap", "game cg", "pixel art",
];
/// Danbooru 沒建、但意思上確實包含的關係（補在 tag_rules.json 上）
const EXTRA_IMPLIES: &[(&str, &[&str])] = &[
    ("lace-trimmed legwear", &["lace trim"]),
    ("lace trim", &["lace"]),
    ("strappy heels", &["high heels"]),
    ("red lips", &["lips"]),
];
const COLORS: &str = "aqua|black|blonde|blue|brown|green|grey|light blue|light brown|light green|light purple|orange|pink|purple|red|silver|white|yellow|dark blue|dark green";

struct Group {
    name: &'static str,
    rx: Regex,
    exempt: &'static [&'static str],
    /// 多人圖時不比（每個人可以不一樣）
    per_person: bool,
    /// 第二名分數低於第一名的幾倍才拿掉；None＝一律只留第一名
    ratio: Option<f64>,
}

struct Rules {
    implies: HashMap<String, Vec<String>>,
    count: Regex,
    multi: Regex,
    girl: Regex,
    boy: Regex,
    /// 人數（1girl／2girls…只能有一個）：先處理，才知道是不是多人圖
    counts: Vec<Group>,
    groups: Vec<Group>,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| {
        let mut implies: HashMap<String, Vec<String>> = HashMap::new();
        if let Ok(v) = serde_json::from_str::<Value>(RULES) {
            if let Some(m) = v.get("implies").and_then(Value::as_object) {
                for (k, list) in m {
                    let parents = list
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.replace('_', " ")).collect())
                        .unwrap_or_default();
                    implies.insert(k.replace('_', " "), parents);
                }
            }
        }
        for (k, ps) in EXTRA_IMPLIES {
            let e = implies.entry(k.to_string()).or_default();
            for p in *ps {
                if !e.iter().any(|x| x == p) {
                    e.push(p.to_string());
                }
            }
        }
        let rx = |s: &str| Regex::new(s).expect("tagclean regex");
        Rules {
            implies,
            count: rx(r"^(\d\+?(girl|boy|other)s?|solo|solo focus|multiple (girls|boys|others)|no humans)$"),
            multi: rx(r"^([2-6]\+?(girls|boys|others)|multiple (girls|boys|others))$"),
            girl: rx(r"^\d\+?girls?$"),
            boy: rx(r"^\d\+?boys?$"),
            counts: vec![
                Group { name: "女生人數", rx: rx(r"^\d\+?girls?$"), per_person: false, ratio: Some(0.8), exempt: &[] },
                Group { name: "男生人數", rx: rx(r"^\d\+?boys?$"), per_person: false, ratio: Some(0.8), exempt: &[] },
            ],
            groups: vec![
                Group { name: "髮色", rx: rx(&format!("^({COLORS}) hair$")), per_person: true, ratio: Some(0.6),
                        exempt: &["multicolored hair", "two-tone hair", "gradient hair", "streaked hair", "split-color hair", "colored inner hair"] },
                Group { name: "眼睛顏色", rx: rx(&format!("^({COLORS}) eyes$")), per_person: true, ratio: Some(0.6),
                        exempt: &["heterochromia", "multicolored eyes"] },
                Group { name: "髮長", rx: rx(r"^(short|medium|long|very long|absurdly long) hair$"), per_person: true, ratio: Some(0.6), exempt: &[] },
                Group { name: "胸部大小", rx: rx(r"^(flat chest|small breasts|medium breasts|large breasts|huge breasts|gigantic breasts)$"),
                        per_person: true, ratio: None, exempt: &[] },
                Group { name: "構圖", rx: rx(r"^(full body|cowboy shot|upper body|portrait|close-up|lower body|feet out of frame|head out of frame)$"),
                        per_person: false, ratio: Some(0.8), exempt: &[] },
                Group { name: "姿勢", rx: rx(r"^(standing|sitting|lying|kneeling|squatting|on back|on stomach|on side|all fours)$"),
                        per_person: true, ratio: None, exempt: &[] },
            ],
        }
    })
}

/// 網頁顯示用的 tag（括號跳脫）→ 規則用的純文字
fn plain(t: &str) -> String {
    t.replace("\\(", "(").replace("\\)", ")").trim().to_lowercase()
}

fn ancestors(tag: &str, imp: &HashMap<String, Vec<String>>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut todo: Vec<String> = imp.get(tag).cloned().unwrap_or_default();
    while let Some(t) = todo.pop() {
        if out.insert(t.clone()) {
            todo.extend(imp.get(&t).cloned().unwrap_or_default());
        }
    }
    out
}

/// 衝突組：同組只留分數最高的（見檔頭第 3 條）
fn conflicts(groups: &[Group], tags: &[(String, f64)], pl: &[String], notes: &mut HashMap<usize, String>, multi: bool,
             imp: &HashMap<String, Vec<String>>) {
    let have: HashSet<&str> = (0..tags.len()).filter(|i| !notes.contains_key(i)).map(|i| pl[i].as_str()).collect();
    // 被同組其他 tag 包含的（long hair 之於 very long hair）不參加比較，留給「重複」處理
    let implied: HashSet<String> = have.iter().flat_map(|t| ancestors(t, imp)).collect();
    for g in groups {
        if g.exempt.iter().any(|e| have.contains(e)) || (g.per_person && multi) {
            continue;
        }
        let members: Vec<usize> = (0..tags.len())
            .filter(|i| !notes.contains_key(i) && g.rx.is_match(&pl[*i]) && !implied.contains(&pl[*i]))
            .collect();
        let Some(&top) = members.first() else { continue };
        for &i in members.iter().skip(1) {
            if g.ratio.is_some_and(|k| tags[i].1 >= tags[top].1 * k) {
                continue; // 分數接近：可能兩個都對（雙色頭髮之類），留著
            }
            notes.insert(i, format!("{}衝突：留分數較高的「{}」", g.name, tags[top].0));
        }
    }
}

/// tags：(顯示用 tag, 分數)，已照分數由高到低。
/// 回傳 (order, notes)：order＝全部 tag 的建議順序（留下的在前、建議拿掉的在後），notes＝[(建議拿掉的 tag, 原因)]（照 order 的順序）。
pub fn clean(tags: &[(String, f64)], style_lora: bool) -> (Vec<String>, Vec<(String, String)>) {
    let r = rules();
    let pl: Vec<String> = tags.iter().map(|(t, _)| plain(t)).collect();
    let mut notes: HashMap<usize, String> = HashMap::new();

    for (i, p) in pl.iter().enumerate() {
        if NOISE.contains(&p.as_str()) {
            notes.insert(i, "雜訊（簽名、浮水印這類，生圖會畫出字）".into());
        } else if style_lora && (REAL.contains(&p.as_str()) || p.ends_with("(medium)") || p.ends_with("(style)")) {
            notes.insert(i, "寫實／畫風類，會跟畫風 LoRA 打架".into());
        }
    }

    conflicts(&r.counts, tags, &pl, &mut notes, false, &r.implies);
    let multi = (0..tags.len()).filter(|i| !notes.contains_key(i)).map(|i| pl[i].as_str()).any(|t| r.multi.is_match(t))
        || ((0..tags.len()).any(|i| !notes.contains_key(&i) && r.girl.is_match(&pl[i]))
            && (0..tags.len()).any(|i| !notes.contains_key(&i) && r.boy.is_match(&pl[i]))); // 1girl＋1boy（cat girl、tomboy 不算）
    conflicts(&r.groups, tags, &pl, &mut notes, multi, &r.implies);

    let kept: Vec<usize> = (0..tags.len()).filter(|i| !notes.contains_key(i)).collect();
    let by_plain: HashMap<&str, usize> = kept.iter().map(|&i| (pl[i].as_str(), i)).collect();
    for &i in &kept {
        for a in ancestors(&pl[i], &r.implies) {
            if let Some(&j) = by_plain.get(a.as_str()) {
                if j != i && !notes.contains_key(&j) {
                    notes.insert(j, format!("重複：「{}」已經包含", tags[i].0));
                }
            }
        }
    }

    let mut keep: Vec<usize> = (0..tags.len()).filter(|i| !notes.contains_key(i)).collect();
    keep.sort_by(|&a, &b| {
        let (ca, cb) = (r.count.is_match(&pl[a]), r.count.is_match(&pl[b]));
        cb.cmp(&ca).then(tags[b].1.partial_cmp(&tags[a].1).unwrap_or(std::cmp::Ordering::Equal))
    });
    let drop: Vec<usize> = (0..tags.len()).filter(|i| notes.contains_key(i)).collect();
    let order = keep.iter().chain(drop.iter()).map(|&i| tags[i].0.clone()).collect();
    let notes = drop.iter().map(|&i| (tags[i].0.clone(), notes[&i].clone())).collect();
    (order, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<(String, f64)> {
        list.iter().enumerate().map(|(i, t)| (t.to_string(), 1.0 - i as f64 / 100.0)).collect()
    }
    fn why<'a>(notes: &'a [(String, String)], t: &str) -> Option<&'a str> {
        notes.iter().find(|(x, _)| x == t).map(|(_, w)| w.as_str())
    }

    /// 使用者那張油畫風女僕原圖的真實反推結果（照分數順序）
    const MAID: &[&str] = &["maid", "1girl", "thighhighs", "vase", "high heels", "maid headdress", "white thighhighs",
        "painting \\(object\\)", "underwear", "ass", "panties", "couch", "indoors", "solo", "long hair", "dress", "apron",
        "pink rose", "wrist cuffs", "flower", "black footwear", "looking at viewer", "realistic", "rose", "black dress",
        "white panties", "puffy sleeves", "short sleeves", "lace trim", "pantyshot", "pink flower", "nose",
        "puffy short sleeves", "frills", "portrait \\(object\\)", "standing", "lace-trimmed legwear", "looking back",
        "full body", "carpet", "leaning forward", "picture frame", "maid apron", "strappy heels", "white apron", "lips",
        "brown hair", "short dress", "drawing \\(object\\)", "bent over", "table", "lace", "blonde hair", "upskirt",
        "red lips", "breasts", "from behind", "rug", "armchair", "fireplace", "brown eyes", "frilled apron", "asian",
        "white rose", "on couch"];

    #[test]
    fn maid_with_style_lora() {
        let (order, notes) = clean(&tags(MAID), true);
        assert_eq!(order.len(), MAID.len());
        assert_eq!(&order[..2], &["1girl".to_string(), "solo".to_string()]); // 人數放最前
        for t in ["realistic", "nose", "lips", "asian"] {
            assert!(why(&notes, t).unwrap().starts_with("寫實"), "{t}");
        }
        assert!(why(&notes, "blonde hair").is_none()); // 測試用的假分數差很小（0.99 倍）→ 可能是雙色，留著
        assert!(why(&notes, "on couch").is_none()); // 在沙發上是地點，不是姿勢
        for t in ["thighhighs", "underwear", "rose", "flower", "pink flower", "dress", "panties", "lace", "puffy sleeves",
                  "short sleeves", "lace trim", "apron", "high heels", "frills"] {
            assert!(why(&notes, t).unwrap().starts_with("重複"), "{t}");
        }
        for t in ["maid", "pink rose", "white rose", "white panties", "brown hair", "red lips", "frilled apron", "standing"] {
            assert!(why(&notes, t).is_none(), "{t} 不該被拿掉");
        }
        assert!(why(&notes, "couch").unwrap().starts_with("重複")); // on couch 已經包含 couch
        assert_eq!(notes.len(), 19);
    }

    #[test]
    fn maid_without_lora_keeps_realism() {
        let (_, notes) = clean(&tags(MAID), false);
        assert!(why(&notes, "realistic").is_none() && why(&notes, "nose").is_none() && why(&notes, "asian").is_none());
        assert!(why(&notes, "lips").unwrap().starts_with("重複")); // 沒掛 LoRA 時，lips 照樣被 red lips 包含
    }

    #[test]
    fn exemptions_and_noise() {
        let (_, notes) = clean(&tags(&["1girl", "blue hair", "red hair", "multicolored hair", "blue eyes", "green eyes", "heterochromia", "signature"]), true);
        assert!(why(&notes, "red hair").is_none() && why(&notes, "green eyes").is_none());
        assert!(why(&notes, "signature").unwrap().starts_with("雜訊"));
        let (_, notes) = clean(&tags(&["1girl", r"marker \(medium\)", r"1980s \(style\)", "english text"]), true);
        assert_eq!(notes.len(), 3, "{notes:?}");
        let sc = |l: &[(&str, f64)]| l.iter().map(|(t, p)| (t.to_string(), *p)).collect::<Vec<_>>();
        let (_, notes) = clean(&sc(&[("1girl", 0.99), ("blue eyes", 0.9), ("green eyes", 0.4)]), false);
        assert!(why(&notes, "green eyes").unwrap().starts_with("眼睛顏色衝突")); // 0.44 倍 → 拿掉
        let (_, notes) = clean(&sc(&[("1girl", 0.99), ("blue eyes", 0.9), ("green eyes", 0.7)]), false);
        assert!(why(&notes, "green eyes").is_none()); // 0.78 倍 → 留著
    }

    #[test]
    fn multi_person_and_implied_lengths() {
        let sc = |l: &[(&str, f64)]| l.iter().map(|(t, p)| (t.to_string(), *p)).collect::<Vec<_>>();
        let (_, notes) = clean(&sc(&[("2girls", 0.99), ("blonde hair", 0.9), ("black hair", 0.3), ("short hair", 0.8), ("long hair", 0.3)]), false);
        assert!(notes.is_empty(), "多人圖不比髮色髮長：{notes:?}");
        let (_, notes) = clean(&sc(&[("1girl", 0.99), ("long hair", 0.9), ("very long hair", 0.7)]), false);
        assert!(why(&notes, "very long hair").is_none());
        assert!(why(&notes, "long hair").unwrap().starts_with("重複"));
        // 反推誤報 2girls（實際 1boy＋1girl）：人數只留分數高的，而且不會因為假的 2girls 被當成多人
        let (_, notes) = clean(&sc(&[("1boy", 0.95), ("1girl", 0.9), ("2girls", 0.5), ("hetero", 0.6)]), false);
        assert!(why(&notes, "2girls").unwrap().starts_with("女生人數衝突"));
        let (_, notes) = clean(&sc(&[("1girl", 0.99), ("sitting", 0.9), ("on couch", 0.8), ("standing", 0.3)]), false);
        assert!(why(&notes, "on couch").is_none() && why(&notes, "standing").unwrap().starts_with("姿勢衝突"));
    }
}

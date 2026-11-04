//! 姿勢大彙整（resources\poses\poses.json）：成人改寫用的「姿勢小抄」。
//!
//! 2026-10-10 實測：改寫模型只看到「傳教士體位」這種名稱會亂擺（通用版 Qwen3.5 甚至把傳教士寫成女上），
//! 專用改寫模型（Pe_T2I_Heretic）則會把性行為洗白成穿衣服抱著。可行做法是：
//! 程式找出姿勢 → 通用版只寫開頭句和人物／場景／光線 → 姿勢的幾何描述（geometry_en）原封不動插在中間。
//! 四種體位 × 兩個種子 16／16 擺對（報告\Qwen-Image-2.1-Turbo資料_20261010.md）。
use crate::tags::fold_zh;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Pose {
    pub id: String,
    pub zh: String,
    pub category: String,
    pub people: String,
    pub acts: Vec<String>,
    pub names: Vec<String>, // aliases＋triggers＋中文名（比對用）
    pub english: Vec<String>,
    pub camera: String,
    pub orientation: String,
    pub geometry: String,
    pub geometry_zh: String,
    /// 一句話的擺法（只講身體，沒有床、畫面位置、鏡頭）。使用者 2026-10-29：「你直接稍微跟他講姿勢是怎樣的（稍微），他也能知道」
    pub brief: String,
    pub danbooru: Vec<String>, // 給 Anima 的姿勢 tag（2026-10-10 用 Danbooru API 驗證過都存在）
    pub control: Option<Control>,
}

#[derive(Debug, Clone)]
pub struct Control {
    pub depth: String,
    pub pose: String,
    pub camera: String,
    pub orientation: String,
}

#[derive(Debug, Default)]
pub struct Poses {
    pub list: Vec<Pose>,
    pub dir: PathBuf,
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 第一人稱的說法（想法裡出現就把鏡頭改成男方視角）
const POV: [&str; 6] = [
    "視角",
    "pov",
    "第一人稱",
    "主觀",
    "point of view",
    "point-of-view",
];

impl Poses {
    pub fn load(res: &Path) -> Poses {
        let dir = res.join("poses");
        let v: Value = std::fs::read(dir.join("poses.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(json!({}));
        let s = |e: &Value, k: &str| e[k].as_str().unwrap_or("").trim().to_string();
        let list = v["poses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let id = s(e, "id");
                let geometry = s(e, "geometry_en");
                if id.is_empty() || geometry.is_empty() {
                    return None;
                }
                let zh = s(e, "zh");
                let aliases = strs(&e["aliases"]);
                let mut names = aliases.clone();
                names.extend(strs(&e["triggers"]));
                // 「貝合わせ（日文）」「M字開脚（日文）」這種帶註解的別名，原樣永遠比對不到：多放一個去掉括號的
                let bare: Vec<String> = names.iter().filter(|n| n.contains('（')).map(|n| n.split('（').next().unwrap_or("").trim().to_string()).collect();
                names.extend(bare);
                names.push(zh.split('（').next().unwrap_or("").trim().to_string());
                names.retain(|n| n.chars().count() >= 2);
                let english = aliases
                    .iter()
                    .filter(|a| a.is_ascii() && !a.contains('_'))
                    .cloned()
                    .collect();
                let control = e.get("control").filter(|c| c.is_object()).map(|c| Control {
                    depth: s(c, "depth"),
                    pose: s(c, "pose"),
                    camera: s(c, "camera_en"),
                    orientation: s(c, "orientation"),
                });
                Some(Pose {
                    id,
                    zh,
                    category: s(e, "category"),
                    people: s(e, "people"),
                    acts: strs(&e["acts"]),
                    names,
                    english,
                    camera: s(e, "camera_en"),
                    orientation: s(e, "orientation"),
                    geometry,
                    geometry_zh: s(e, "geometry_zh"),
                    brief: s(e, "brief_en"),
                    danbooru: strs(&e["danbooru"]),
                    control,
                })
            })
            .collect();
        Poses { list, dir }
    }

    pub fn get(&self, id: &str) -> Option<&Pose> {
        self.list.iter().find(|p| p.id == id)
    }

    /// 在想法裡找姿勢名稱或口語說法，取最長的那個（「反向騎乘」優先於「騎乘」）。
    /// 名稱是繁體，想法可能是簡體：原文比不到的，再把雙方折成簡體比一次（传教士体位 → 傳教士體位）
    pub fn find(&self, idea: &str) -> Option<(&Pose, String)> {
        self.find_where(idea, |_| true)
    }

    /// 同 [`find`]，只算 ok(名稱) 為真的名稱。英文名要整個字對到（anal 不算 analog）；
    /// 一樣長時，提到兩個女生就挑女女的版本（69、舔陰、指交男女版和女女版同名）
    fn find_where(&self, idea: &str, ok: impl Fn(&str) -> bool) -> Option<(&Pose, String)> {
        let low = idea.to_lowercase();
        let low_s = fold_zh(&low);
        let ff = mentions(idea, &TWO_WOMEN);
        let mut best: Option<(&Pose, &String)> = None;
        for p in &self.list {
            for n in &p.names {
                let nl = n.to_lowercase();
                let hit = if nl.is_ascii() {
                    has_word(&low, &nl)
                } else {
                    low.contains(&nl) || low_s.contains(&fold_zh(&nl))
                };
                if !hit || !ok(&nl) {
                    continue;
                }
                let len = n.chars().count();
                // 一樣長時，不會撞日常用語的名稱優先：「側躺著從後面插入，湯匙式」要挑湯匙式，不是「從後面」的背後式
                let plain = |s: &str| !AMBIGUOUS.contains(&s.to_lowercase().as_str());
                let better = best.is_none_or(|(bp, b)| {
                    let bl = b.chars().count();
                    len > bl
                        || (len == bl && plain(n) && !plain(b))
                        || (len == bl && plain(n) == plain(b) && ff && p.people == "F+F" && bp.people != "F+F")
                });
                if better {
                    best = Some((p, n));
                }
            }
        }
        best.map(|(p, n)| (p, n.clone()))
    }

    /// 助手用：使用者講了體位的口語（乳交、磨鏡…），附上英文名和一種驗證過的擺法（實測 35B 把乳交猜成磨鏡）。
    /// 會撞到日常用語的名稱（蝴蝶、三明治、從後面…）要句子裡另有色色的字眼才算，不然普通的圖也會被塞一段性愛擺法
    pub fn term_note(&self, text: &str) -> Option<Value> {
        let sexy = looks_sexual(text) || mentions(text, &EXPLICIT);
        // 黑話（老漢推車、吹簫…）只要句子裡一男一女都在就算：「金髮女生被老漢推車」「女僕幫主人吹簫」沒有露骨字眼，但不會是推車、吹樂器
        let pair = mentions(text, &FEMALE) && mentions(text, &MALE);
        let (p, said) = self.find_where(text, |n| sexy || !AMBIGUOUS.contains(&n) || (pair && SLANG.contains(&n)))?;
        // id：助手畫圖前照這個體位補正提示詞（agent::pose_fix）。使用者講了床以外的地點，擺法裡的床拿掉
        let place = names_place(text);
        let slang = SLANG.iter().any(|s| said.contains(s));
        // brief：短版擺法（term_line 用；沒有短版就用長版）
        let brief = if p.brief.is_empty() { p.geometry_at(place) } else { p.brief.clone() };
        Some(json!({"id": p.id, "said": said, "zh": p.zh, "en": p.english.join(" / "), "geometry": p.geometry_at(place), "brief": brief, "place": place, "slang": slang}))
    }

    /// 附在使用者那句話後面的說明（助手、AI 改寫的 Anima 版／文生圖版共用）
    pub fn term_line(note: &Value) -> String {
        let s = |k: &str| note[k].as_str().unwrap_or("").to_string();
        // 2026-10-29 長短版擂台：短版不輸長版、輸出 token 少一半，正式說明用短版；舊格式的說明沒有 brief 就退回長版
        let how = if s("brief").is_empty() { s("geometry") } else { s("brief") };
        let mut line = format!(
            "Term: 「{}」 is the sex act {} ({}). Keep the act exactly; one arrangement that draws it correctly (change place, camera or clothes if the user asked): {}",
            s("said"),
            s("zh"),
            s("en"),
            how
        );
        // 2026-10-29 場景擂台：「被老漢推車」9B／MOSS 都多畫一個老頭；海邊、沙發還是被寫成床
        if note["slang"] == json!(true) {
            line.push_str(&format!(" 「{}」 is only a nickname for the act: draw nothing from its literal words.", s("said")));
        }
        if note["place"] == json!(true) {
            line.push_str(" Put them in the place the user asked for, not on a bed.");
        }
        line
    }

    /// 給模型挑姿勢用的目錄：一行一筆「id｜中文名｜英文名」
    pub fn catalog(&self) -> String {
        self.list
            .iter()
            .map(|p| {
                format!(
                    "{} | {} | {}",
                    p.id,
                    p.zh,
                    p.english.first().map(String::as_str).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 選單用
    pub fn menu(&self) -> Value {
        json!(self
            .list
            .iter()
            .map(
                |p| json!({"id": p.id, "zh": p.zh, "category": p.category, "people": p.people,
                            "control": p.control.is_some(), "desc": p.geometry_zh})
            )
            .collect::<Vec<_>>())
    }
}

/// 想法裡有性行為的字眼才讓模型從目錄挑（實測：「裸女泡澡」也被挑成自慰）
const SEXUAL: [&str; 34] = [
    "做愛",
    "性交",
    "性愛",
    "插入",
    "抽插",
    "口交",
    "舔",
    "幹",
    "上床",
    "高潮",
    "體位",
    "姿勢",
    "自慰",
    "手淫",
    "騎",
    "後入",
    "內射",
    "肛交",
    "乳交",
    "足交",
    "素股",
    "磨",
    "愛撫",
    "sex",
    "fuck",
    "blowjob",
    "fellatio",
    "cunnilingus",
    "penetrat",
    "intercourse",
    "masturbat",
    "handjob",
    "riding",
    "position",
];

/// 想法裡有沒有 keys 的其中一個字眼（keys 是繁體或英文）：原文比不到的，再把雙方折成簡體比一次，簡體輸入也認得。
/// 「幹」不折：乾、幹、干都會折成「干」，繁體的「乾淨」會被當成性字眼
fn mentions(idea: &str, keys: &[&str]) -> bool {
    let low = idea.to_lowercase();
    let low_s = fold_zh(&low);
    keys.iter()
        .any(|k| low.contains(k) || (*k != "幹" && low_s.contains(&fold_zh(k))))
}

pub fn looks_sexual(idea: &str) -> bool {
    mentions(idea, &SEXUAL)
}

/// 身體或成人字眼：讓 [`AMBIGUOUS`] 的名稱算數（cum、cock、dick 會撞到 document、peacock、Dickens，不放）
const EXPLICIT: [&str; 20] = [
    "雞雞", "陰莖", "肉棒", "屌", "奶子", "乳頭", "小穴", "陰部", "陰道", "屁眼", "肛門", "精液", "裸", "色色", "瑟瑟",
    "penis", "pussy", "nipple", "nude", "nsfw",
];

/// 姿勢庫裡會撞到日常用語的名稱（小寫）：只在句子看起來是色色的時候才算
const AMBIGUOUS: [&str; 77] = [
    "butterfly", "amazon", "亞馬遜", "lotus", "蓮花坐", "蓮花座", "anvil", "鐵砧", "shower", "sofa", "foot", "toy", "pov",
    "grind", "grinding", "straddling", "bent over", "stairway", "on stairs", "on desk", "樓梯式", "桌上式", "橋式", "拱腰橋式",
    "三明治", "sandwich", "dp", "69", "六九", "從後面", "坐在他身上", "坐上去", "她在上面", "on top of him", "剪刀式", "剪刀腳",
    "推車式", "打屁股", "spanking", "over the knee", "用頭髮包住", "用腳夾", "用腳磨蹭", "用胸夾住", "trample", "edging",
    "watch me", "spooning", "neck kissing", "ear licking", "body kiss", "seated caress",
    // 2026-10-28 補的口語，本身也是日常用語：吹樂器、佛像、麻將、便當、靠關係
    "吹簫", "吹喇叭", "觀音坐蓮", "自摸", "火車便當", "走後門", "老漢推車", "打飛機", "打手槍", "雙飛", "磨豆腐",
    // 2026-10-28 新條目的名稱：章魚觸手、警察手銬、摔角鎖技、小狗舔腳
    "觸手", "触手", "tentacles", "手銬", "拘束", "綁縛", "掐脖子", "掐脖", "choking", "hand on neck", "舔腳", "牆洞", "雙性", "full nelson",
];

/// [`AMBIGUOUS`] 裡的性愛黑話：日常意思（推車、佛像、吹樂器、便當）不會同時出現一男一女，有一男一女就當成體位
const SLANG: [&str; 10] = ["老漢推車", "觀音坐蓮", "吹簫", "吹喇叭", "火車便當", "打飛機", "打手槍", "走後門", "雙飛", "推車式"];
const FEMALE: [&str; 11] = ["女生", "女孩", "女人", "女僕", "少女", "女友", "女朋友", "老婆", "妹子", "她", "女主"];
const MALE: [&str; 10] = ["男生", "男人", "男友", "男朋友", "老公", "主人", "大叔", "老漢", "他", "男主"];

/// 提到兩個女生
const TWO_WOMEN: [&str; 11] = ["兩個女生", "兩個女人", "兩女", "女女", "百合", "女同", "蕾絲", "yuri", "lesbian", "2girls", "two women"];

/// 英文名整個字出現（前後不是英數字）
fn has_word(text: &str, word: &str) -> bool {
    let alnum = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    text.match_indices(word)
        .any(|(i, _)| !alnum(text[..i].chars().next_back()) && !alnum(text[i + word.len()..].chars().next()))
}

/// 模型偶爾會寫 girl／boy：一律換成 woman／man（只換整個字，大小寫照原文）
pub fn adultify(text: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b([Gg]irl|[Bb]oy)(s?)\b").unwrap());
    RE.replace_all(text, |c: &regex::Captures| {
        let w = match &c[1] {
            "girl" => "woman",
            "Girl" => "Woman",
            "boy" => "man",
            _ => "Man",
        };
        if c[2].is_empty() {
            w.to_string()
        } else {
            w.replace("man", "men") // women／men
        }
    })
    .into_owned()
}

/// 成人模式 Anima 的 tag 裡會讓人物看起來像小孩的字：一律拿掉（模型輸出、使用者自訂規則都過這一關）。
/// 整個詞比對：babydoll（睡衣）、childhood friend、kidney 不算。petite、flat chest、young 是成年人也會用的描述，不擋
fn childlike(tag: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(loli|lolicon|shota|shotacon|child|children|kids?|toddlers?|aged down|bab(y|ies)|elementary)\b").unwrap()
    });
    RE.is_match(&tag.replace('_', " "))
}

/// 模型會自己編畫師（@artgerm…）：想法裡沒寫到的 @畫師 一律拿掉
fn artist_ok(tag: &str, idea: &str) -> bool {
    match tag.strip_prefix('@') {
        Some(name) => idea.to_lowercase().contains(&name.trim().to_lowercase()),
        None => true,
    }
}

/// 沒對到姿勢時整段由模型寫：每一行像 tag 列的（逗號多）把小孩樣的 tag、想法裡沒有的畫師拿掉，底線換空格；
/// 句子（整行，或 tag 列裡超過 6 個字的那段）只做 girl／boy → woman／man
pub fn safe_anima(text: &str, idea: &str) -> String {
    text.lines()
        .map(|line| {
            if line.matches(',').count() >= 3 {
                static SCORE: std::sync::LazyLock<regex::Regex> =
                    std::sync::LazyLock::new(|| regex::Regex::new(r"^score_[1-9]$").unwrap());
                line.split(',')
                    // 夾在段落中間的 @畫師（"anime style @Yoruji"）也要檢查
                    .map(|t| {
                        t.split_whitespace()
                            .filter(|w| artist_ok(w, idea))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|t| !t.is_empty())
                    .filter_map(|t| {
                        if t.split_whitespace().count() > 6 {
                            Some(adultify(&t))
                        } else if childlike(&t) {
                            None
                        } else if t.starts_with("score_") {
                            // 模型會編 score_8_5 這種不存在的
                            SCORE.is_match(&t).then_some(t)
                        } else {
                            Some(t.replace('_', " "))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                adultify(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 逗號分隔的 tag：去掉小孩樣的、品質／評級／人數這類程式會自己加的，再去重複
pub fn clean_tags(tags: &str, have: &[String], idea: &str) -> Vec<String> {
    const OURS: [&str; 12] = [
        "masterpiece",
        "best quality",
        "score_",
        "safe",
        "sensitive",
        "nsfw",
        "explicit",
        "1girl",
        "1boy",
        "2girls",
        "2boys",
        "solo",
    ];
    let mut out: Vec<String> = Vec::new();
    for t in tags.split([',', '\n']) {
        let t = t
            .trim()
            .trim_matches(|c| c == '.' || c == '"')
            .replace('_', " ")
            .to_lowercase();
        // 「… position」這類姿勢名稱程式已經用正式 tag 加過了，模型自己寫的不要
        if t.is_empty()
            || t.len() > 60
            || t.ends_with("position")
            || !artist_ok(&t, idea)
            || childlike(&t)
            || OURS.iter().any(|k| t.starts_with(k))
        {
            continue;
        }
        if !have.contains(&t) && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

pub fn is_pov(idea: &str) -> bool {
    mentions(idea, &POV)
}

impl Pose {
    pub fn penetrative(&self) -> bool {
        self.acts.iter().any(|a| a == "vaginal" || a == "anal")
    }

    /// 開頭句要用的性行為名稱：程式填，不讓模型自己寫（實測模型會把騎乘寫成口交）
    pub fn act_phrase(&self) -> String {
        let name = self
            .english
            .iter()
            .find(|a| a.contains(' '))
            .or(self.english.first())
            .cloned()
            .unwrap_or_else(|| self.zh.clone())
            .to_lowercase();
        if self.penetrative() {
            let pos = if name.contains("position") {
                ""
            } else {
                " position"
            };
            format!("having sex in the {name}{pos}")
        } else {
            name
        }
    }

    /// (鏡頭, 方向)：開了姿勢參考圖以參考圖為準，其次是想法裡的第一人稱，最後是大彙整的建議
    pub fn camera(&self, idea: &str, use_control: bool) -> (String, String) {
        if let Some(c) = self.control.as_ref().filter(|_| use_control) {
            return (c.camera.clone(), c.orientation.clone());
        }
        if is_pov(idea) {
            return (
                "a point-of-view shot from the man's eyes, looking at her".into(),
                "vertical".into(),
            );
        }
        (self.camera.clone(), self.orientation.clone())
    }

    /// 插入類（一男一女）補一句：實測背後式常畫成男方手握陰莖、兩人中間有空隙
    pub fn contact_sentence(&self) -> String {
        if !self.penetrative() || self.people != "M+F" {
            return String::new();
        }
        let hole = if self.acts.iter().any(|a| a == "vaginal") {
            "vagina"
        } else {
            "anus"
        };
        format!(
            " There is no gap between their bodies: his hips are pressed flat against her body, his penis is hidden inside her {hole} \
             except for the very base, and both of his hands rest on her body, never holding his penis."
        )
    }

    /// 開頭句＋幾何描述（原文）＋接觸句＋細節
    pub fn assemble(&self, opening: &str, details: &str) -> String {
        let (opening, details) = (adultify(opening), adultify(details));
        format!(
            "{} {}{} {}",
            opening.trim(),
            self.geometry.trim(),
            self.contact_sentence(),
            details.trim()
        )
        .trim()
        .to_string()
    }

    /// Anima 的人數 tag（照 people 欄位）
    pub fn people_tags(&self) -> &'static [&'static str] {
        match self.people.as_str() {
            "F+F" => &["2girls", "yuri"],
            "solo F" => &["1girl", "solo"],
            "M+M" => &["2boys", "yaoi"],
            "F+M+M" | "M+F+M" => &["1girl", "2boys", "hetero"],
            "M+F+F" => &["2girls", "1boy", "hetero"],
            _ => &["1girl", "1boy", "hetero"],
        }
    }

    /// Anima 版：品質＋評級 → 人數 → 姿勢 tag（程式填）→ 模型寫的外觀／場景 tag，空一行接姿勢原文和細節句
    pub fn assemble_anima(&self, tags: &str, details: &str, idea: &str) -> String {
        let mut head: Vec<String> = ["masterpiece", "best quality", "score_7", "explicit"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        head.extend(self.people_tags().iter().map(|s| s.to_string()));
        if self.penetrative() {
            head.push("sex".into());
        }
        for t in &self.danbooru {
            if !head.contains(t) {
                head.push(t.clone());
            }
        }
        head.extend(clean_tags(tags, &head.clone(), idea));
        format!(
            "{}\n\n{}{} {}",
            head.join(", "),
            self.geometry.trim(),
            self.contact_sentence(),
            adultify(details).trim()
        )
        .trim()
        .to_string()
    }
}

/// 模型拿來代替性行為的委婉字：Anima 的 tag 裡出現就拿掉（助手那邊另外用 logit_bias 擋，見 pe::SOFT_WORDS）
const SOFT_TAGS: [&str; 7] = ["intimate", "intimacy", "sensual", "suggestive", "tasteful", "implied", "lovemaking"];
/// 「這是哪一個體位」的 tag：跟使用者要的體位不同就拿掉（9B 把乳交寫成 missionary）。fellatio、handjob 這種動作會跟體位一起出現，不放
const POSITION_TAGS: [&str; 15] = [
    "missionary", "cowgirl position", "reverse cowgirl position", "doggystyle", "prone bone", "mating press", "piledriver (sex)",
    "suspended congress", "spooning", "full nelson", "upright straddle", "reverse upright straddle", "amazon position", "69", "tribadism",
];
/// 體位附帶的場景 tag：使用者另外講了地點就不硬塞
const SETTING_TAGS: [&str; 10] = ["against wall", "pillow", "chair", "shower (place)", "showering", "soap", "on desk", "on table", "couch", "train interior"];
const GIRLS: [&str; 6] = ["1girl", "2girls", "3girls", "4girls", "5girls", "multiple girls"];
const BOYS: [&str; 6] = ["1boy", "2boys", "3boys", "4boys", "5boys", "multiple boys"];
/// 會露出男生那根的動作 tag：體位有男生又帶這些，就補 penis（擂台裡 35B 常漏，Anima 沒寫就畫得含糊）
const PENIS_ACTS: [&str; 14] = [
    "fellatio", "deepthroat", "irrumatio", "cooperative fellatio", "paizuri", "nursing handjob", "handjob", "footjob", "thigh sex",
    "hairjob", "mutual masturbation", "spitroast", "69", "glory hole",
];

impl Pose {
    /// 人數 tag：只有確定的組合才給（扶他、觸手、一女四男這種 people_tags 給不對，交給模型自己寫）
    fn count_tags(&self) -> Option<&'static [&'static str]> {
        matches!(self.people.as_str(), "M+F" | "F+M" | "F+F" | "solo F" | "M+M" | "F+M+M" | "M+F+M" | "M+F+F").then(|| self.people_tags())
    }

    /// 助手寫完的 Anima 提示詞照這個體位補正：缺的人數、sex、姿勢 tag 補在品質／評級後面；
    /// 跟體位打架的人數 tag、別的體位 tag、軟詞拿掉。回傳（新提示詞, 補了什麼, 拿掉什麼）。評級交給 pe::fix_anima_tags
    pub fn fix_anima(&self, prompt: &str) -> (String, Vec<String>, Vec<String>) {
        self.fix_anima_at(prompt, false)
    }

    /// place：使用者講了床以外的地點（[`names_place`]）。這時不硬塞體位附帶的場景 tag（against wall、shower…），
    /// 提示詞裡的床、床墊、枕頭拿掉（2026-10-29 場景擂台：森林站立後入被補上 against wall；海邊、沙發被寫成 mattress、pillow）
    pub fn fix_anima_at(&self, prompt: &str, place: bool) -> (String, Vec<String>, Vec<String>) {
        let (tags, rest) = crate::pe::anima_split(prompt);
        let counts: Vec<&str> = self.count_tags().map(|c| c.to_vec()).unwrap_or_default();
        let mut want: Vec<String> = counts.iter().map(|s| s.to_string()).collect();
        // 拳交的 acts 也有 vaginal，但不是插入式性交
        let fucking = self.penetrative() && self.acts.first().map(String::as_str) != Some("manual");
        if fucking {
            want.push("sex".into());
        }
        want.extend(self.danbooru.iter().filter(|t| !(place && SETTING_TAGS.contains(&t.as_str()))).cloned());
        let has_girls = counts.iter().any(|t| GIRLS.contains(t));
        let has_boys = counts.iter().any(|t| BOYS.contains(t));
        if has_boys && !want.iter().any(|t| t == "penis") && (fucking || self.danbooru.iter().any(|t| PENIS_ACTS.contains(&t.to_lowercase().as_str()))) {
            want.push("penis".into());
        }
        let low_want: Vec<String> = want.iter().map(|t| t.to_lowercase()).collect();
        let clash = |k: &str| {
            if SOFT_TAGS.contains(&k) || (POSITION_TAGS.contains(&k) && !low_want.iter().any(|w| w == k)) {
                return true;
            }
            // 乳交、口交、手交這種不插入的：sex、vaginal 會讓 Anima 另外畫一根插進去（2026-10-28 AI 改寫的乳交畫出兩根）
            if !fucking && ["sex", "vaginal", "anal"].contains(&k) && !low_want.iter().any(|w| w == k) {
                return true;
            }
            if place && ["bed", "mattress", "pillow"].iter().any(|b| k.contains(b)) {
                return true;
            }
            if counts.is_empty() {
                return false;
            }
            (GIRLS.contains(&k) && !low_want.iter().any(|w| w == k))
                || (BOYS.contains(&k) && !low_want.iter().any(|w| w == k))
                || (k == "yuri" && has_boys)
                || (k == "yaoi" && has_girls)
                || (k == "hetero" && !(has_girls && has_boys))
                || (k == "solo" && counts.len() > 1 && !counts.contains(&"solo"))
        };
        let (mut kept, mut removed) = (vec![], vec![]);
        for t in tags {
            if clash(&t.to_lowercase()) {
                removed.push(t);
            } else {
                kept.push(t);
            }
        }
        // 補在開頭的品質、評級 tag 後面
        let head = head_len(&kept);
        let have: Vec<String> = kept.iter().map(|t| t.to_lowercase()).collect();
        let added: Vec<String> = want.into_iter().filter(|w| !have.contains(&w.to_lowercase())).collect();
        for (i, t) in added.iter().enumerate() {
            kept.insert(head + i, t.clone());
        }
        let mut s = kept.join(", ");
        if !rest.is_empty() {
            s.push_str(", ");
            s.push_str(&rest);
        }
        (s, added, removed)
    }

    /// 句子型的模型（MOSS、Qwen 2.1）：提示詞完全沒提到這個體位，就把驗證過的擺法接在後面
    pub fn fix_text(&self, prompt: &str) -> Option<String> {
        self.fix_text_at(prompt, false)
    }

    /// place：使用者講了床以外的地點，接上去的擺法不帶床。模型已經照擺法寫過（句子大多對得上）就不再接一次
    /// （2026-10-29 MOSS 海邊題：模型自己抄了「on a white mattress」，程式又整段接一次，床寫了兩遍）
    pub fn fix_text_at(&self, prompt: &str, place: bool) -> Option<String> {
        let low = prompt.to_lowercase();
        let named = self.english.iter().chain(self.danbooru.iter()).any(|n| has_word(&low, &n.to_lowercase()));
        let geo = self.geometry_at(place);
        (!named && !geo.trim().is_empty() && !described(&low, &self.geometry)).then(|| format!("{}\n\n{}", prompt.trim_end(), geo.trim()))
    }

    /// 擺法：place＝true 時把床、床墊、枕頭換掉（身體怎麼擺不變）
    pub fn geometry_at(&self, place: bool) -> String {
        if place {
            off_bed(&self.geometry)
        } else {
            self.geometry.clone()
        }
    }
}

/// 使用者講的地點（不含床）：講了就不讓體位庫的「在床上」蓋掉它。只放中文和不會撞到別的字的英文（table→comfortable、park→sparkle 不放）
const PLACES: [&str; 64] = [
    "海邊", "沙灘", "海灘", "海裡", "泳池", "溫泉", "浴室", "浴缸", "淋浴", "廁所", "廚房", "客廳", "沙發", "椅子", "椅上", "桌上",
    "桌子", "講桌", "書桌", "辦公室", "教室", "會議室", "電車", "車上", "車裡", "車內", "公車", "森林", "樹林", "草地", "公園", "戶外",
    "野外", "山上", "街上", "巷子", "屋頂", "陽台", "樓梯", "更衣室", "神社", "舞台", "地板", "地上", "牆", "窗邊", "帳篷", "雪地",
    "河邊", "湖邊", "門口", "走廊", "電梯", "beach", "onsen", "hot spring", "office", "classroom", "forest", "sofa", "couch", "bathroom",
    "kitchen", "outdoors",
];
const BEDS: [&str; 4] = ["床", "bed", "被窩", "棉被"];

pub fn names_place(text: &str) -> bool {
    mentions(text, &PLACES) && !mentions(text, &BEDS)
}

/// 擺法裡的床換成地面、枕頭拿掉；扶著的牆改成「當地有的支撐物」（2026-10-29：森林裡站著後入，9B 照擺法寫了 palms against wall）
fn off_bed(g: &str) -> String {
    static PILLOW: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\s*(?:on|against|into) (?:the |a )?(?:left |right )?pillows?").unwrap());
    static BED: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:bed sheets?|sheets|mattress|bed)\b").unwrap());
    static WALL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:(?:the|a) )?walls?\b").unwrap());
    let g = PILLOW.replace_all(g, "");
    let g = BED.replace_all(&g, "ground").replace("headboard", "backrest");
    let mut first = true;
    WALL.replace_all(&g, |_: &regex::Captures| {
        if std::mem::take(&mut first) {
            "a wall or other sturdy support that fits the place (tree trunk, rock, pillar)".to_string()
        } else {
            "the support".to_string()
        }
    })
    .into_owned()
}

/// 模型已經把這段擺法寫進去了：擺法的四字片語有三成以上出現在提示詞裡
fn described(prompt_low: &str, geometry: &str) -> bool {
    let words: Vec<String> = geometry.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    let grams: Vec<String> = words.windows(4).map(|w| w.join(" ")).collect();
    let flat: String = prompt_low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    !grams.is_empty() && grams.iter().filter(|g| flat.contains(g.as_str())).count() * 10 >= grams.len() * 3
}

/// 使用者講了、模型常漏掉的外觀／鏡頭／光線／場景詞 → Danbooru tag（2026-10-28／29 擂台：雙馬尾、黑絲、第一人稱、仰拍一直漏）。
/// 只放意思單純、不會撞到日常句子的詞；長的寫前面（「雙馬尾」先於「馬尾」）
const ASKED: [(&str, &str); 52] = [
    ("第一人稱", "pov"), ("主觀視角", "pov"), ("pov", "pov"), ("仰拍", "from below"), ("仰視", "from below"), ("低角度", "from below"),
    ("從下往上", "from below"), ("俯拍", "from above"), ("俯視", "from above"), ("從上往下", "from above"), ("從側面", "from side"),
    ("側面拍", "from side"), ("逆光", "backlighting"), ("全身", "full body"), ("上半身", "upper body"), ("特寫", "close-up"),
    ("雙馬尾", "twintails"), ("單馬尾", "ponytail"), ("馬尾", "ponytail"), ("麻花辮", "braid"), ("黑長直", "black hair, long hair, straight hair"),
    ("金髮", "blonde hair"), ("銀髮", "grey hair"), ("白髮", "white hair"), ("紅髮", "red hair"), ("黑髮", "black hair"), ("粉髮", "pink hair"),
    ("藍髮", "blue hair"), ("黑絲", "black pantyhose"), ("白絲", "white pantyhose"), ("連褲襪", "pantyhose"), ("褲襪", "pantyhose"),
    ("過膝襪", "thighhighs"), ("吊帶襪", "garter straps"), ("眼鏡", "glasses"), ("女僕", "maid"), ("護士", "nurse"), ("兔女郎", "playboy bunny"),
    ("比基尼", "bikini"), ("泳裝", "swimsuit"), ("爆乳", "huge breasts"), ("巨乳", "large breasts"), ("貧乳", "small breasts"),
    ("夕陽", "sunset"), ("黃昏", "sunset"), ("月光", "moonlight"), ("霓虹", "neon lights"), ("溫泉", "onsen"), ("海邊", "beach"),
    ("沙灘", "beach"), ("教室", "classroom"), ("辦公室", "office"),
];

/// 使用者這句話講到的 [`ASKED`] tag（前面有「不、別、沒」的不算）
pub fn asked_tags(text: &str) -> Vec<String> {
    let low = text.to_lowercase();
    let mut taken: Vec<(usize, usize)> = vec![];
    let mut out: Vec<String> = vec![];
    for (zh, tag) in ASKED {
        for (i, _) in low.match_indices(zh) {
            let end = i + zh.len();
            if taken.iter().any(|&(a, b)| i < b && end > a) {
                continue;
            }
            taken.push((i, end));
            let before: String = low[..i].chars().rev().take(3).collect();
            if ['不', '別', '沒', '無', '非'].iter().any(|c| before.contains(*c)) {
                continue;
            }
            for t in tag.split(", ") {
                if !out.iter().any(|o| o == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// 一張圖只能有一個的鏡頭 tag（pe::fix_anima_tags 會留第一個）：使用者講了哪一個，其他的讓位
/// （2026-10-29 場景擂台：使用者說仰拍，35B 另外寫了描述姿勢的 from behind，結果仰拍被刪掉）
const ONE_OF: [&[&str]; 2] = [
    &["from front", "from side", "from behind", "from above", "from below"],
    &["close-up", "portrait", "upper body", "cowboy shot", "full body"],
];

/// Anima 提示詞裡補上 tag（品質、評級 tag 後面），已經有的不補；補的是鏡頭角度或取景就拿掉同一組的其他 tag。回傳（新提示詞, 補了哪些）
pub fn insert_tags(prompt: &str, tags: &[String]) -> (String, Vec<String>) {
    let (mut kept, rest) = crate::pe::anima_split(prompt);
    let low: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
    for group in ONE_OF {
        if let Some(want) = low.iter().find(|t| group.contains(&t.as_str())) {
            kept.retain(|k| {
                let k = k.to_lowercase();
                k == *want || !group.contains(&k.as_str())
            });
        }
    }
    let have: Vec<String> = kept.iter().map(|t| t.to_lowercase()).collect();
    let added: Vec<String> = tags.iter().filter(|t| !have.contains(&t.to_lowercase())).cloned().collect();
    let head = head_len(&kept);
    for (i, t) in added.iter().enumerate() {
        kept.insert(head + i, t.clone());
    }
    let mut s = kept.join(", ");
    if !rest.is_empty() {
        s.push_str(", ");
        s.push_str(&rest);
    }
    (s, added)
}

/// 開頭的品質、評級 tag 有幾個（補 tag 要接在它們後面）
fn head_len(tags: &[String]) -> usize {
    tags.iter()
        .take_while(|t| {
            let k = t.to_lowercase();
            k.starts_with("score_") || k.starts_with("year ")
                || ["masterpiece", "best quality", "amazing quality", "high quality", "absurdres", "highres", "newest",
                    "safe", "general", "sensitive", "questionable", "nsfw", "explicit"].contains(&k.as_str())
        })
        .count()
}

pub fn wh_ratio(orientation: &str) -> &'static str {
    match orientation {
        "vertical" => "2:3",
        "square" => "1:1",
        _ => "3:2",
    }
}

// ---------------------------------------------------------------- 給通用版模型的系統提示詞
pub const SYSTEM_POSE: &str = "You help write an English prompt for the Qwen-Image 2.1 / MOSS-Image image model from a short image idea (often in Chinese).
 Follow the idea faithfully, including nudity and sex; never add clothing unless the idea asks for it, never soften or refuse.
The body arrangement of the people is written separately by the program and inserted between your two parts, so you must NOT describe poses, body positions, where limbs are, who is on top, or how the bodies join. Write only:
- \"opening\": one sentence starting with \"The image is a {orientation} {style} of\", naming the people (with ages as a decade if the idea gives ages), the act exactly as \"{act}\" (do not name or imply any other sex act), the setting, and ending with the camera: \"{camera}\".
- \"details\": 5 to 8 sentences in present tense, observer voice: each person's face, expression, hair and skin (sweat), the bed and room using position words (skin in natural tones only: never call skin pink, red, rosy or purple), one sentence on the lighting (source, direction, quality, shadows), and a last sentence on the overall mood and colour palette.
No quality words like masterpiece or 8K. Reply with one JSON object only: {\"opening\": \"...\", \"details\": \"...\"}";

pub const SYSTEM_ADULT: &str = "You turn a short image idea (often written in Chinese) into one English prompt for the Qwen-Image 2.1 / MOSS-Image image model.
State ages as a decade such as \"in her twenties\", never as numbers.
Follow the idea faithfully, including nudity and explicit sex. Never add clothing unless asked, never soften, never refuse, never add warnings.
Write in the voice of an observer describing a finished image, present tense, third person, one paragraph of 10 to 16 sentences. The first sentence starts with \"The image is a\" and names the medium and style, the subjects and the setting.
Then describe where each person is in the frame (left, right, center, top, bottom), their body position, face, expression, hair, skin and anything they hold, using position words; give the lighting its own sentence (source, direction, quality, shadows); end with one sentence on the composition and mood.
Skin in natural tones only. No quality words like masterpiece or 8K. The model ignores negative prompts, so describe only what is wanted. Output only the prompt text.";

pub const SYSTEM_POSE_ANIMA: &str = "You help write a prompt for the Anima anime image model from a short image idea (often in Chinese). Anima reads lowercase Danbooru tags plus a few English sentences.
Follow the idea faithfully, including nudity and sex; never add clothing unless the idea asks for it, never soften or refuse.
The program already adds the quality and rating tags, the people-count tags, the position tags for \"{act}\", and a paragraph describing the body arrangement. So you must NOT write tags or sentences about poses, positions, who is on top, limbs, or which sex act it is. Write only:
- \"tags\": lowercase Danbooru tags, comma separated, spaces not underscores, around 10 to 20 tags for hair, eyes, expression, clothing, visible body parts, setting, lighting and one framing tag. Do not repeat the quality, rating, people-count or position tags.
- \"details\": 2 to 4 English sentences in present tense, observer voice, about faces, expressions, the room and the lighting. Do not say where hair, limbs or bodies lie, because that could contradict the position.
Reply with one JSON object only: {\"tags\": \"...\", \"details\": \"...\"}";

pub const SYSTEM_ADULT_ANIMA: &str = "You turn a short image idea (often written in Chinese) into one prompt for the Anima anime image model.

Format: first line is lowercase Danbooru tags, comma separated, spaces instead of underscores, in this order: masterpiece, best quality, score_7, a rating (explicit for sex or exposed genitals, nsfw for nudity), people count (1girl, 1boy, 2girls...), the act and position, then appearance, clothing, expression, setting and lighting tags; an artist as @name only if the idea names one.
Then an empty line, then 3 to 5 English sentences in present tense describing where each person is, what they are doing, the setting and the lighting.
Output only the prompt text.";

pub const SYSTEM_PICK: &str = "Below is a catalog of body positions, one per line as \"id | Chinese name | English name\".
Read the user's image idea and reply with the id of the single position it clearly describes. Reply \"none\" if the idea does not explicitly describe a sexual act between the people in it, if it only describes nudity, or if you are unsure. Never add an act the idea does not mention.
Reply with one JSON object only: {\"id\": \"...\"}

Catalog:
";

#[cfg(test)]
mod tests {
    use super::*;

    fn res() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("resources")
    }

    #[test]
    fn term_note_explains_chinese_slang() {
        let p = Poses::load(&res());
        let n = p.term_note("幫我畫個乳交圖。").unwrap();
        assert_eq!(n["said"], "乳交");
        assert!(n["en"].as_str().unwrap().contains("titjob"));
        assert!(n["geometry"].as_str().unwrap().contains("between them"));
        assert!(p.term_note("一隻橘貓睡在椅子上").is_none());
    }

    #[test]
    fn term_line_gives_the_short_arrangement() {
        let p = Poses::load(&res());
        let n = p.term_note("幫我畫個乳交圖。").unwrap();
        let line = Poses::term_line(&n);
        assert!(line.contains(n["brief"].as_str().unwrap()), "{line}");
        assert!(n["brief"] == n["geometry"] || !line.contains(n["geometry"].as_str().unwrap()), "{line}");
        // 舊格式的說明沒有 brief：退回長版
        let old = json!({"said": "乳交", "zh": "乳交", "en": "paizuri", "geometry": "Her breasts are pressed together."});
        assert!(Poses::term_line(&old).contains("pressed together"));
    }

    #[test]
    fn fix_anima_puts_the_act_back() {
        let p = Poses::load(&res());
        // 2026-10-28 擂台 huihui9b 把「乳交」寫成這樣（原文節錄）
        let pz = p.get("paizuri").unwrap();
        let (s, added, removed) = pz.fix_anima("masterpiece, best quality, score_7, sensitive, 1girl, solo, long hair, 1boy, hetero, sex, missionary, lying, on back, intimate, cock, pussy, Her breasts squeeze his cock.");
        assert!(s.starts_with("masterpiece, best quality, score_7, sensitive, paizuri, "), "{s}");
        assert!(added.contains(&"paizuri".to_string()));
        for t in ["solo", "missionary", "intimate"] {
            assert!(removed.contains(&t.to_string()), "{t} 該拿掉：{removed:?}");
        }
        assert!(s.ends_with("Her breasts squeeze his cock."), "句子要留著：{s}");
        // 女女：男生的人數 tag、hetero 拿掉
        let (_, _, removed) = p.get("tribadism_scissoring").unwrap().fix_anima("explicit, 2girls, 1boy, hetero, yuri, tribadism");
        assert!(removed.contains(&"1boy".to_string()) && removed.contains(&"hetero".to_string()));
        // 3P：1boy 換成 2boys，doggystyle 換成 spitroast
        let (s, added, removed) = p.get("spitroast_fmm").unwrap().fix_anima("explicit, 1girl, 1boy, hetero, doggystyle, fellatio");
        assert!(added.contains(&"2boys".to_string()) && added.contains(&"spitroast".to_string()), "{s}");
        assert!(removed.contains(&"1boy".to_string()) && removed.contains(&"doggystyle".to_string()));
        assert!(s.contains("fellatio"), "動作 tag 不拿：{s}");
        assert!(added.contains(&"penis".to_string()), "有男生的插入式要補 penis：{added:?}");
        // 乳交原本只寫 cock：補 penis；拳交不算插入式性交，不補 sex、penis
        let (_, added, _) = pz.fix_anima("explicit, 1girl, 1boy, hetero, paizuri, cock");
        assert_eq!(added, vec!["penis".to_string()]);
        // 乳交寫了 sex／vaginal：拿掉（不然多畫一根插進去）；拳交自己的 vaginal 留著
        let (_, _, removed) = pz.fix_anima("explicit, 1girl, 1boy, hetero, sex, vaginal, paizuri, penis");
        assert_eq!(removed, vec!["sex".to_string(), "vaginal".to_string()]);
        let (_, _, removed) = p.get("fisting").unwrap().fix_anima("explicit, 1girl, 1boy, hetero, fisting, vaginal");
        assert!(removed.is_empty(), "{removed:?}");
        let (_, added, _) = p.get("fisting").unwrap().fix_anima("explicit, 1girl, 1boy, hetero, fisting");
        assert!(!added.iter().any(|t| t == "sex" || t == "penis"), "{added:?}");
        // 顏面騎乘、前後夾攻要帶到舔與口交
        assert!(p.get("facesitting").unwrap().danbooru.iter().any(|t| t == "cunnilingus"));
        assert!(p.get("spitroast_fmm").unwrap().danbooru.iter().any(|t| t == "fellatio"));
        // 寫對的不動
        let (s, added, removed) = pz.fix_anima("explicit, 1girl, 1boy, hetero, paizuri, penis");
        assert!(added.is_empty() && removed.is_empty(), "{s}");
        // 句子型：沒提到體位才接擺法
        assert!(pz.fix_text("A woman lies on a bed with a man.").unwrap().contains(pz.geometry.trim()));
        assert!(pz.fix_text("She gives him paizuri on the bed.").is_none());
    }

    /// 2026-10-29 場景擂台：體位庫的「在床上」把使用者要的海邊、森林、沙發蓋掉
    #[test]
    fn named_places_keep_the_bed_out() {
        let p = Poses::load(&res());
        assert!(names_place("海邊夕陽下，金髮女生被老漢推車") && names_place("森林裡站著從後面幹"));
        assert!(!names_place("她躺在床上") && !names_place("沙發旁邊的床上正常位") && !names_place("黑長直女生被後入"));
        // 擺法：床換成地面、枕頭拿掉，身體怎麼擺不變
        let doggy = p.get("doggy_style").unwrap();
        let g = doggy.geometry_at(true);
        assert!(!g.to_lowercase().contains("bed") && !g.contains("mattress") && !g.contains("pillow"), "{g}");
        assert!(g.contains("hands and knees") && doggy.geometry_at(false) == doggy.geometry);
        let wg = p.get("standing_wall_rear").unwrap().geometry_at(true);
        assert!(wg.contains("tree trunk") && !wg.contains("the wall"), "{wg}");
        // 說明：講了地點就叫它放到那裡；黑話提醒只是外號
        let n = p.term_note("海邊夕陽下，金髮女生被老漢推車").unwrap();
        assert_eq!((n["place"].clone(), n["slang"].clone()), (json!(true), json!(true)));
        let line = Poses::term_line(&n);
        assert!(line.contains("only a nickname") && line.contains("not on a bed") && !line.contains("mattress"), "{line}");
        let n = p.term_note("黑長直女生被後入，趴在床上翹屁股").unwrap();
        assert_eq!(n["place"], json!(false));
        assert!(!Poses::term_line(&n).contains("nickname"));
        // Anima：森林不補 against wall，提示詞裡的床拿掉；沒講地點照舊
        let wall = p.get("standing_wall_rear").unwrap();
        let (s, added, removed) = wall.fix_anima_at("explicit, 1girl, 1boy, hetero, forest, bed, white bed sheets, standing sex", true);
        assert!(!added.iter().any(|t| t == "against wall") && !s.contains("against wall"), "{s}");
        assert_eq!(removed, vec!["bed".to_string(), "white bed sheets".to_string()]);
        assert!(wall.fix_anima("explicit, 1girl, 1boy, hetero, standing sex").1.contains(&"against wall".to_string()));
        // 句子型：模型已經照擺法寫過就不再接一次；接的話也不帶床
        let copied = format!("A photo on the beach at sunset. {}", doggy.geometry_at(true));
        assert!(doggy.fix_text_at(&copied, true).is_none());
        assert!(!doggy.fix_text_at("A woman and a man at sunset.", true).unwrap().contains("mattress"));
        // 「幫我吹簫」不用一男一女也算，對到第一人稱口交
        assert_eq!(p.term_note("第一人稱視角，辦公室裡女秘書跪著幫我吹簫").unwrap()["id"], json!("pov_fellatio"));
    }

    #[test]
    fn asked_words_become_tags() {
        assert_eq!(asked_tags("雙馬尾女生騎乘位"), vec!["twintails"], "雙馬尾不能再算一次馬尾");
        assert_eq!(asked_tags("黑絲女生用腳，第一人稱，仰拍、逆光"), vec!["pov", "from below", "backlighting", "black pantyhose"]);
        assert_eq!(asked_tags("黑長直女生"), vec!["black hair", "long hair", "straight hair"]);
        assert!(asked_tags("不要戴眼鏡，別穿黑絲").is_empty(), "前面有不、別的不算");
        assert!(asked_tags("一隻橘貓睡在椅子上").is_empty());
        assert_eq!(asked_tags("海邊夕陽下，從低角度仰拍，金髮女生被老漢推車"), vec!["from below", "blonde hair", "sunset", "beach"]);
        let (s, added) = insert_tags("masterpiece, best quality, score_7, explicit, 1girl, cowgirl position, She rides him.", &asked_tags("雙馬尾、第一人稱"));
        assert_eq!(added, vec!["pov".to_string(), "twintails".to_string()]);
        assert!(s.starts_with("masterpiece, best quality, score_7, explicit, pov, twintails, 1girl") && s.ends_with("She rides him."), "{s}");
        assert!(insert_tags("explicit, 1girl, twintails", &["twintails".to_string()]).1.is_empty());
        // 使用者講的鏡頭角度、取景優先，同一組的其他 tag 讓位
        let (s, added) = insert_tags("explicit, 1girl, from behind, beach, close-up", &asked_tags("海邊，仰拍，全身"));
        assert_eq!(added, vec!["from below".to_string(), "full body".to_string()], "beach 已經有了不補");
        assert!(!s.contains("from behind") && !s.contains("close-up") && s.contains("from below") && s.contains("full body"), "{s}");
    }

    #[test]
    fn term_note_reads_couple_slang() {
        // 2026-10-28 體位擂台的題目：黑話有一男一女就算、一樣長挑不撞日常用語的、補的口語
        let p = Poses::load(&res());
        for (t, id) in [
            ("畫一個金髮女生被老漢推車", "doggy_style"),
            ("女僕跪著幫主人吹簫", "kneeling_fellatio"),
            ("畫觀音坐蓮，女生坐在男生身上", "cowgirl"),
            ("側躺著從後面插入，湯匙式", "spooning"),
            ("把她按在牆上站著從後面幹", "standing_wall_rear"),
            ("一個女生被兩個男人前後夾攻", "spitroast_fmm"),
            ("女生躺著M字開腿自摸", "solo_f_supine_fingering"),
        ] {
            let n = p.term_note(t).unwrap_or_else(|| panic!("{t} 沒有註解"));
            assert_eq!(n["id"], id, "{t}");
        }
    }

    #[test]
    fn term_note_ignores_everyday_words() {
        let p = Poses::load(&res());
        for t in [
            "一隻蝴蝶停在花上，butterfly",
            "Amazon 包裹放在門口",
            "亞馬遜雨林的風景",
            "池塘裡的蓮花座",
            "早餐的三明治",
            "女孩在 shower 洗澡",
            "pov 視角走在街上",
            "a girl holding a toy",
            "analog clock on the wall",
            "1969年的老照片",
            "一個女生站在海邊，從後面拍她的背影",
            "小狗坐在他身上",
            "做瑜伽橋式",
            "媽媽打屁股",
            "觀音坐蓮的佛像",
            "古典美女在吹簫",
            "畫一個女生在吹簫",
            "畫一個老漢推車賣菜",
            "她坐在他身上撒嬌",
            "打麻將自摸",
            "火車便當的特寫",
            "章魚的觸手",
            "警察拿出手銬",
            "摔角選手使出 full nelson",
        ] {
            assert!(p.term_note(t).is_none(), "{t} -> {:?}", p.term_note(t));
        }
        // 真的在講體位就要認得
        for (t, id) in [("狗爬式", "doggy_style"), ("傳教士", "missionary_basic"), ("titjob", "paizuri"), ("裸體的她坐在他身上", "cowgirl"), ("anal sex", "missionary_anal")] {
            assert_eq!(p.term_note(t).map(|n| n["zh"].clone()), p.get(id).map(|x| json!(x.zh)), "{t}");
        }
    }

    #[test]
    fn two_women_pick_the_women_version() {
        let p = Poses::load(&res());
        assert_eq!(p.find("兩個女生69").unwrap().0.id, "sixty_nine_ff");
        assert_eq!(p.find("兩個女生互相舔陰").unwrap().0.id, "cunnilingus_ff");
        assert_eq!(p.find("百合 指交").unwrap().0.id, "fingering_ff");
        assert_eq!(p.find("69").unwrap().0.id, "side_69");
    }

    #[test]
    fn everyday_slang_is_known() {
        let p = Poses::load(&res());
        for (t, id) in [
            ("後入", "doggy_style"),
            ("后入", "doggy_style"),
            ("老漢推車", "doggy_style"),
            ("觀音坐蓮", "cowgirl"),
            ("观音坐莲", "cowgirl"),
            ("火車便當", "standing_carry"),
            ("女上男下", "cowgirl"),
            ("吹簫", "kneeling_fellatio"),
            ("含屌", "kneeling_fellatio"),
            ("舔雞雞", "kneeling_fellatio"),
            ("イラマチオ", "irrumatio"),
            ("抓著頭插", "irrumatio"),
            ("打手槍", "handjob"),
            ("打飛機", "handjob"),
            ("舔逼", "cunnilingus"),
            ("舔小穴", "cunnilingus"),
            ("奶炮", "paizuri"),
            ("用奶子夾", "paizuri"),
            ("腳交", "footjob"),
            ("肛交", "doggy_anal"),
            ("爆菊", "doggy_anal"),
            ("顏面騎乘", "facesitting"),
            ("手マン", "fingering"),
            ("磨豆腐", "tribadism_scissoring"),
            ("一男兩女", "mff_cowgirl_facesit"),
            ("雙飛", "mff_cowgirl_facesit"),
            ("兩男一女", "spitroast_fmm"),
            ("雙插", "double_penetration_mfm"),
            ("自摸", "solo_f_supine_fingering"),
            ("假陽具", "solo_f_kneeling_vibrator"),
            ("貝合わせ", "tribadism_scissoring"),
            ("M字開脚", "solo_f_chair_spread"),
            ("腳踩頭旁的打樁機", "piledriver_feet_floor"),
            ("standing piledriver", "piledriver_feet_floor"),
            ("打樁機", "piledriver"),
            ("抱腿後入", "full_nelson"),
            ("站著69", "standing_69"),
            ("群交", "gangbang_cowgirl"),
            ("綁起來做", "shibari_frogtie_missionary"),
            ("銬在床頭", "restrained_cuffed_missionary"),
            ("掐著脖子做", "choking_missionary"),
            ("吸腳趾", "foot_licking"),
            ("拳交", "fisting"),
            ("男男後入", "mm_anal_doggy"),
            ("男男正常位", "mm_anal_missionary"),
            ("男生自慰", "solo_m_masturbation"),
            ("女女假陽具後入", "ff_strapon_doggy"),
            ("女插男", "pegging"),
            ("glory hole", "glory_hole"),
            ("電車痴漢", "chikan_train"),
            ("被觸手", "tentacle_suspension"),
            ("扶他", "futa_on_female_doggy"),
            ("授乳手交", "nursing_handjob"),
            ("掰開小穴", "solo_f_spread_pussy"),
            ("翹臀掰開", "solo_f_ass_up_spread"),
        ] {
            assert_eq!(p.find(t).map(|x| x.0.id.as_str()), Some(id), "{t}");
        }
    }

    #[test]
    fn loads_and_finds_longest_name() {
        let p = Poses::load(&res());
        assert!(p.list.len() >= 100, "{}", p.list.len());
        assert_eq!(
            p.find("一對情侶在床上做愛，傳教士體位").unwrap().0.id,
            "missionary_basic"
        );
        assert_eq!(
            p.find("寫實照片，背後式（狗爬式），白天窗邊").unwrap().0.id,
            "doggy_style"
        );
        assert_eq!(
            p.find("二十出頭的女生在床上騎乘男生").unwrap().0.id,
            "cowgirl"
        );
        assert_eq!(p.find("反向騎乘").unwrap().0.id, "reverse_cowgirl");
        assert!(p.find("一隻貓在窗邊睡覺").is_none());
    }

    /// 改動前的 find（只比原文），拿來證明繁體輸入的結果沒變
    fn legacy_find<'a>(p: &'a Poses, idea: &str) -> Option<(&'a Pose, String)> {
        let low = idea.to_lowercase();
        let mut best: Option<(&Pose, &String)> = None;
        for pose in &p.list {
            for n in &pose.names {
                if low.contains(&n.to_lowercase())
                    && best.is_none_or(|(_, b)| n.chars().count() > b.chars().count())
                {
                    best = Some((pose, n));
                }
            }
        }
        best.map(|(pose, n)| (pose, n.clone()))
    }

    #[test]
    fn simplified_input_finds_poses() {
        let p = Poses::load(&res());
        // 姿勢名稱是繁體，簡體想法也要找得到同一個姿勢
        for (simp, trad, id) in [
            (
                "一对情侣在床上做爱，传教士体位",
                "一對情侶在床上做愛，傳教士體位",
                "missionary_basic",
            ),
            (
                "写实照片，背后式（狗爬式），白天窗边",
                "寫實照片，背後式（狗爬式），白天窗邊",
                "doggy_style",
            ),
            (
                "二十出头的女生在床上骑乘男生",
                "二十出頭的女生在床上騎乘男生",
                "cowgirl",
            ),
            ("反向骑乘", "反向騎乘", "reverse_cowgirl"),
        ] {
            let (s, t) = (p.find(simp).unwrap(), p.find(trad).unwrap());
            assert_eq!(s.0.id, id, "{simp}");
            assert_eq!((&s.0.id, &s.1), (&t.0.id, &t.1), "{simp}");
        }
        assert!(p.find("一只猫在窗边睡觉").is_none());
        // 每個姓名的簡體寫法都要找得到（找到的名稱不會比它短）
        for pose in &p.list {
            for n in &pose.names {
                let simp = fold_zh(n);
                let got = p
                    .find(&simp)
                    .unwrap_or_else(|| panic!("{} 的簡體寫法 {simp} 找不到", pose.id));
                assert!(
                    got.1.chars().count() >= n.chars().count(),
                    "{simp} → {}",
                    got.1
                );
            }
        }
    }

    #[test]
    fn traditional_input_unchanged() {
        let p = Poses::load(&res());
        // 繁體的名稱、句子：跟改動前逐筆相同
        let mut ideas: Vec<String> = p.list.iter().flat_map(|x| x.names.clone()).collect();
        ideas.extend(
            [
                "一對情侶在床上做愛，傳教士體位",
                "寫實照片，背後式（狗爬式），白天窗邊",
                "二十出頭的女生在床上騎乘男生",
                "反向騎乘",
                "一隻貓在窗邊睡覺",
                "乾淨的房間裡一個女生，皇后坐在椅子上",
            ]
            .map(String::from),
        );
        for idea in &ideas {
            let (old, new) = (legacy_find(&p, idea), p.find(idea));
            assert_eq!(
                old.map(|(x, n)| (x.id.clone(), n)),
                new.map(|(x, n)| (x.id.clone(), n)),
                "{idea}"
            );
        }
    }

    #[test]
    fn simplified_gates() {
        assert!(looks_sexual("一对情侣在床上做爱"));
        assert!(looks_sexual("她骑在他身上"));
        assert!(looks_sexual("一對情侶在床上做愛")); // 繁體照舊
        assert!(!looks_sexual("写实照片，女生全裸躺在浴缸里，泡泡"));
        // 乾、幹、干折成同一個字：繁體的「乾淨」、簡體的「干净」不能被當成性字眼
        assert!(!looks_sexual("乾淨的房間裡一個女生") && !looks_sexual("干净的房间里一个女生"));
        assert!(looks_sexual("幹她")); // 原文的「幹」照舊算
        assert!(is_pov("男生视角往上看") && is_pov("第一人称") && is_pov("男生視角往上看"));
        assert!(!is_pov("全身照"));
        let p = Poses::load(&res());
        assert_eq!(
            p.get("cowgirl").unwrap().camera("男生视角往上看", false).1,
            "vertical"
        );
    }

    #[test]
    fn act_and_camera() {
        let p = Poses::load(&res());
        let m = p.get("missionary_basic").unwrap();
        assert_eq!(m.act_phrase(), "having sex in the missionary position");
        let c = p.get("cowgirl").unwrap();
        assert_eq!(c.camera("男生視角往上看", false).1, "vertical");
        assert!(c.camera("", true).0.contains("point-of-view")); // 參考圖是第一人稱
        let s = m.assemble("Open.", "Details.");
        assert!(s.starts_with("Open. ") && s.ends_with(" Details.") && s.contains("no gap"));
        assert_eq!(wh_ratio("vertical"), "2:3");
    }

    #[test]
    fn anima_assembly() {
        let p = Poses::load(&res());
        assert!(
            p.list.iter().all(|x| !x.danbooru.is_empty()),
            "每個姿勢都要有 danbooru tag"
        );
        let m = p.get("missionary_basic").unwrap();
        let s = m.assemble_anima(
            "black hair, Long_Hair, loli, petite, 1girl, explicit, missionary, blush, @artgerm",
            "A girl smiles.",
            "傳教士",
        );
        let (tags, rest) = s.split_once("\n\n").unwrap();
        assert!(tags.starts_with("masterpiece, best quality, score_7, explicit, 1girl, 1boy, hetero, sex, missionary, vaginal, black hair, long hair, petite, blush"), "{tags}");
        assert!(
            !tags.contains("loli")
                && tags.contains("petite")
                && tags.matches("missionary").count() == 1
        );
        assert!(rest.contains("no gap") && rest.ends_with("A woman smiles."));
        assert_eq!(
            p.get("tribadism_scissoring").unwrap().people_tags(),
            ["2girls", "yuri"]
        );
        assert_eq!(safe_anima("masterpiece, score_7, 1girl, loli, short_silver_hair, @artgerm, @kyomu\nA girl lies down.", "@kyomu 畫風"), "masterpiece, score_7, 1girl, short silver hair, @kyomu\nA woman lies down.");
    }

    #[test]
    fn adult_words_and_gate() {
        assert_eq!(
            adultify("a red-haired girl and a boy; Girls, boys, girlfriend"),
            "a red-haired woman and a man; Women, men, girlfriend"
        );
        assert!(looks_sexual("一對情侶在床上做愛"));
        assert!(!looks_sexual("寫實照片，女生全裸躺在浴缸裡，泡泡"));
    }
}

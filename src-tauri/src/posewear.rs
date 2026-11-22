//! 生成頁「衣姿」抽屜（2026-11-21，設計：docs\superpowers\specs\2026-11-21-posewear-drawer-design.md）的純函式。
//! 素材是日本作者ギア 的四個 AI 參考圖庫（原站規矩：一張一張當生成參考可以、商用可以；禁止大量下載與轉載），
//! 中文、英文提示詞改自 posewear（MIT，github.com/atuizz/posewear；授權在 resources\posewear\LICENSE.txt）。
//! 這裡只做：解析原站首頁內嵌的 `const D=[…]`、翻譯、服裝描述（移植 posewear 的 costume_text.gen2）、姿勢分組、路徑檢查。
//! 抓網頁、抓圖、快取在 server\api_posewear.rs。
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

/// lib → 原站固定網址（只會從這四個地方抓東西）
pub const LIBS: [(&str, &str); 4] = [
    ("anime", "https://ai-costume-anime-gallery.pages.dev/"),
    ("costume", "https://ai-costume-gallery.pages.dev/"),
    ("pose", "https://ai-pose-gallery.pages.dev/"),
    ("depth", "https://ai-pose-depth-gallery.pages.dev/"),
];

pub fn base(lib: &str) -> Option<&'static str> {
    LIBS.iter().find(|(k, _)| *k == lib).map(|(_, b)| *b)
}

/// 原站首頁把目錄寫在 `const D=[…];` 那一行（跟 posewear 的 fetch-assets.py 同一個認法）
pub fn parse_inline_d(html: &str) -> Result<Vec<Value>, String> {
    let re = Regex::new(r"const D=(.*?);\r?\n").expect("regex");
    let raw = re.captures(html).and_then(|c| c.get(1)).ok_or("原站頁面改版了，衣姿抽屜讀不到目錄（找不到 const D）")?;
    let v: Value = serde_json::from_str(raw.as_str()).map_err(|e| format!("原站頁面改版了，衣姿抽屜讀不到目錄（{e}）"))?;
    let arr = v.as_array().ok_or("原站頁面改版了，衣姿抽屜讀不到目錄（D 不是陣列）")?;
    if arr.iter().any(|x| x.get("id").and_then(Value::as_str).is_none()) {
        return Err("原站頁面改版了，衣姿抽屜讀不到目錄（有項目沒有 id）".into());
    }
    Ok(arr.clone())
}

/// resources\posewear\text.json（tools\posewear_text.py＋posewear_text.mjs 產生）。服裝詞表是簡中，拼完句子再用 tw 轉繁。
pub struct Text {
    tr_pose: HashMap<String, (String, String)>,
    line: HashMap<String, (String, String)>,
    cat: HashMap<String, String>,
    ang: Vec<(String, String, String)>,
    cc: HashMap<String, (String, String)>,
    fix: HashMap<String, String>,
    /// 顏色要保留原本順序（標題去顏色的 regex 照這個順序比）
    col: Vec<(String, String)>,
    modf: HashMap<String, String>,
    n: HashMap<String, (String, String)>,
    solo: HashMap<String, (String, String)>,
    en_solo: HashMap<String, String>,
    skirt_len: HashMap<String, String>,
    tw: Vec<(String, String)>,
    tw_chars: HashMap<char, String>,
}

fn obj<'a>(v: &'a Value, k: &str) -> Result<&'a Map<String, Value>, String> {
    v.get(k).and_then(Value::as_object).ok_or_else(|| format!("text.json 缺 {k}"))
}
fn strs(m: &Map<String, Value>) -> HashMap<String, String> {
    m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()
}
fn pairs(m: &Map<String, Value>) -> HashMap<String, (String, String)> {
    m.iter()
        .filter_map(|(k, v)| Some((k.clone(), (v.get(0)?.as_str()?.to_string(), v.get(1)?.as_str()?.to_string()))))
        .collect()
}

impl Text {
    pub fn from_value(v: &Value) -> Result<Text, String> {
        let tw = v.get("tw").and_then(Value::as_array).ok_or("text.json 缺 tw")?;
        let mut tw: Vec<(String, String)> = tw
            .iter()
            .filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string())))
            .collect();
        tw.sort_by_key(|(a, _)| std::cmp::Reverse(a.chars().count()));
        Ok(Text {
            tr_pose: pairs(obj(v, "tr_pose")?),
            line: pairs(obj(v, "line")?),
            cat: strs(obj(v, "cat")?),
            ang: obj(v, "ang")?
                .iter()
                .filter_map(|(k, x)| Some((k.clone(), x.get(0)?.as_str()?.to_string(), x.get(1)?.as_str()?.to_string())))
                .collect(),
            cc: pairs(obj(v, "cc")?),
            fix: strs(obj(v, "fix")?),
            col: obj(v, "col")?.iter().filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_string()))).collect(),
            modf: strs(obj(v, "mod")?),
            n: pairs(obj(v, "n")?),
            solo: pairs(obj(v, "solo")?),
            en_solo: strs(obj(v, "en_solo")?),
            skirt_len: strs(obj(v, "skirt_len")?),
            tw,
            tw_chars: obj(v, "tw_chars")?
                .iter()
                .filter_map(|(k, x)| Some((k.chars().next()?, x.as_str()?.to_string())))
                .collect(),
        })
    }

    pub fn load(res: &Path) -> Result<Text, String> {
        let p = res.join("posewear").join("text.json");
        let s = std::fs::read_to_string(&p).map_err(|e| format!("讀不到 {}：{e}", p.display()))?;
        Text::from_value(&serde_json::from_str(&s).map_err(|e| format!("{} 壞了：{e}", p.display()))?)
    }

    /// 簡→繁：每個位置先比最長的片語，比不到就單字查表（跟 tools\posewear_text.mjs 的 toTw 同一個演算法）
    pub fn to_tw(&self, s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        'outer: while let Some(ch) = rest.chars().next() {
            for (a, b) in &self.tw {
                if rest.starts_with(a.as_str()) {
                    out.push_str(b);
                    rest = &rest[a.len()..];
                    continue 'outer;
                }
            }
            match self.tw_chars.get(&ch) {
                Some(t) => out.push_str(t),
                None => out.push(ch),
            }
            rest = &rest[ch.len_utf8()..];
        }
        out
    }

    fn col(&self, k: &str) -> Option<&str> {
        self.col.iter().find(|(a, _)| a == k).map(|(_, b)| b.as_str())
    }
    fn is_col(&self, k: &str) -> bool {
        self.col(k).is_some()
    }
}

/// zlib.crc32（IEEE），posewear 拿 id 的 CRC 挑句型；專案沒有 crc crate，自己算
pub fn crc32(b: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &x in b {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// posewear 的 parse 結果：(修飾詞, 英文詞, 中文, 位置)
type Piece = (Vec<String>, String, String, String);

fn parse(t: &Text, kw: &str) -> Vec<Piece> {
    let words: Vec<&str> = kw.split_whitespace().collect();
    let mut out: Vec<Piece> = vec![];
    let mut mods: Vec<String> = vec![];
    for (j, w) in words.iter().enumerate() {
        let nx = words.get(j + 1).copied().unwrap_or("");
        if t.solo.contains_key(*w) && !t.n.contains_key(*w) && t.is_col(nx) && !t.skirt_len.contains_key(*w) {
            let (zh, slot) = t.solo[*w].clone();
            out.push((std::mem::take(&mut mods), w.to_string(), zh, slot));
            continue;
        }
        if t.n.contains_key(*w) || (!t.is_col(w) && !t.modf.contains_key(*w)) {
            let (zh, slot) = t.n.get(*w).cloned().unwrap_or_else(|| (w.to_string(), "acc".into()));
            out.push((std::mem::take(&mut mods), w.to_string(), zh, slot));
        } else {
            mods.push(w.to_string());
        }
    }
    for m in mods {
        if let Some((zh, slot)) = t.solo.get(&m) {
            if slot == "outer" && out.iter().any(|o| o.3 == "outer") {
                continue;
            }
            out.push((vec![], m.clone(), zh.clone(), slot.clone()));
        } else if t.is_col(&m) {
            if let Some(last) = out.last_mut() {
                last.0.push(m);
            }
        }
    }
    out
}

fn zhp(t: &Text, mods: &[String], w: &str, zh: &str) -> String {
    let mods: Vec<String> = if t.solo.contains_key(w) && !t.n.contains_key(w) {
        mods.iter().filter(|m| t.is_col(m)).cloned().collect()
    } else {
        mods.to_vec()
    };
    let mut cs: Vec<&str> = mods.iter().map(String::as_str).filter(|m| t.is_col(m)).collect();
    if cs.len() > 1 && (cs[0] == "mint" || cs[0] == "olive") && cs[1] == "green" {
        cs.remove(1);
    }
    let mut sg = false;
    if cs.len() >= 2 && cs[0] == "silver" && cs[1] == "grey" {
        cs.drain(..2);
        sg = true;
    }
    let mut colors: Vec<String> = cs.iter().map(|m| t.col(m).unwrap_or_default().to_string()).collect();
    if sg {
        colors.insert(0, "银灰色".into());
    }
    let c = colors.join("拼");
    let len_ok = |m: &str| w == "skirt" || w == "dress" || !t.skirt_len.contains_key(m);
    let mut ms: Vec<&str> = mods.iter().map(String::as_str).filter(|m| t.modf.contains_key(*m) && len_ok(m)).collect();
    let mut zh = zh.to_string();
    if w == "skirt" || w == "dress" {
        let ln: Vec<&str> = ms.iter().copied().filter(|m| t.skirt_len.contains_key(*m)).collect();
        ms.retain(|m| !t.skirt_len.contains_key(*m));
        if let Some(l) = ln.first() {
            if w == "skirt" {
                zh = t.skirt_len[*l].clone();
            } else {
                let pre = match *l { "mini" => "短款", "midi" => "中长", _ => "长款" };
                zh = format!("{pre}{zh}");
            }
        }
    }
    let m: String = ms.iter().map(|x| t.modf[*x].as_str()).collect();
    format!("{c}{m}{zh}")
}

fn enp(t: &Text, mods: &[String], w: &str) -> String {
    if let Some(e) = t.en_solo.get(w).filter(|_| !t.n.contains_key(w)) {
        return mods.iter().map(String::as_str).chain([e.as_str()]).collect::<Vec<_>>().join(" ");
    }
    let base = match w {
        "pants" => "trousers",
        "tote" => "tote bag",
        "heels" => "high heels",
        "mini" => "miniskirt",
        _ => w,
    };
    mods.iter()
        .map(String::as_str)
        .filter(|m| w == "skirt" || w == "dress" || !t.skirt_len.contains_key(*m))
        .chain([base])
        .collect::<Vec<_>>()
        .join(" ")
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for x in v {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

/// 移植 posewear costume_text.gen2：→（中文描述, 英文提示詞, 標題）；中文、標題已轉繁
pub fn costume_text(t: &Text, kw: &str, cat_en: &str, id: &str) -> (String, String, String) {
    let items = parse(t, kw);
    // 位置照第一次出現的順序（Python dict）
    let mut s: Vec<(String, Vec<String>)> = vec![];
    for (mods, w, zh, slot) in &items {
        let mods: Vec<String> = mods
            .iter()
            .filter(|m| !t.modf.get(*m).is_some_and(|x| zh.contains(x.as_str())))
            .cloned()
            .collect();
        let v = zhp(t, &mods, w, zh);
        match s.iter_mut().find(|(k, _)| k == slot) {
            Some((_, xs)) => xs.push(v),
            None => s.push((slot.clone(), vec![v])),
        }
    }
    if let Some((_, b)) = s.iter_mut().find(|(k, _)| k == "bottom") {
        let sk = b.iter().find(|x| x.ends_with('裙')).cloned();
        let sh = b.iter().any(|x| x.ends_with("短裤"));
        if let (Some(sk), true, 2) = (sk, sh, b.len()) {
            let mut c: Vec<char> = sk.chars().collect();
            c.pop();
            *b = vec![format!("{}裙裤", c.into_iter().collect::<String>())];
        }
    }
    for (_, xs) in s.iter_mut() {
        *xs = dedup(std::mem::take(xs));
    }
    let get = |k: &str| s.iter().find(|(a, _)| a == k).map(|(_, v)| v);
    let has = |k: &str| get(k).is_some();
    let h = crc32(id.as_bytes());
    let pick = |opts: &[&str], n: u32| opts[((h >> n) as usize) % opts.len()].to_string();
    let j = |k: &str| {
        let v = get(k).unwrap();
        if v.len() < 3 { v.join("与") } else { format!("{}和{}", v[..v.len() - 1].join("、"), v[v.len() - 1]) }
    };
    let mut p: Vec<String> = vec![];
    if has("one") {
        let pre = if get("one").unwrap()[0].ends_with('裙') { pick(&["身穿", "一条", "穿"], 1) } else { "身穿".into() };
        p.push(pre + &j("one"));
    }
    if has("top") {
        let pre = if has("outer") { "内搭".into() } else if has("one") { "叠穿".into() } else { pick(&["上身是", "上身穿", "上衣选"], 3) };
        p.push(pre + &j("top"));
    }
    if has("outer") {
        let pre = if has("one") { "外披".into() } else { pick(&["外搭", "外面套", "外穿"], 5) };
        p.push(pre + &j("outer"));
    }
    if has("bottom") {
        p.push(pick(&["下身配", "下装是", "配"], 7) + &j("bottom"));
    }
    if has("legs") {
        p.push(pick(&["搭配", "配", "腿上是"], 9) + &j("legs"));
    }
    if has("shoes") {
        p.push(pick(&["脚踩", "穿", "鞋子选"], 11) + &j("shoes"));
    }
    if has("acc") {
        let a = j("acc");
        let opts = [format!("再加{a}"), format!("配{a}"), format!("{a}作点缀"), format!("点缀{a}")];
        p.push(opts[((h >> 13) as usize) % 4].clone());
    }
    let zh = format!("{}。", if p.is_empty() { kw.to_string() } else { p.join("，") });
    let first = |k: &str| get(k).and_then(|v| v.first().cloned());
    let mut main: Vec<String> = if has("one") {
        [first("one"), first("outer")].into_iter().flatten().collect()
    } else {
        [first("outer").or_else(|| first("top")), first("bottom")].into_iter().flatten().collect()
    };
    if main.is_empty() {
        main = s.iter().filter_map(|(_, v)| v.first().cloned()).take(2).collect();
    }
    let mut title = main.join(" + ");
    if title.chars().count() > 14 {
        let mut first_alts: Vec<String> = t.col.iter().map(|(_, v)| v.clone()).collect();
        first_alts.push("银灰色".into());
        first_alts.sort();
        first_alts.dedup();
        first_alts.sort_by_key(|x| std::cmp::Reverse(x.chars().count()));
        let rest: Vec<&str> = t.col.iter().map(|(_, v)| v.as_str()).collect();
        let re = Regex::new(&format!("^({})+(拼({}))*", first_alts.join("|"), rest.join("|"))).expect("regex");
        title = main
            .iter()
            .map(|x| {
                let y = re.replace(x, "").to_string();
                if y.is_empty() { x.clone() } else { y }
            })
            .collect::<Vec<_>>()
            .join(" + ");
    }
    let prefix = if cat_en.is_empty() { "outfit".to_string() } else { format!("{cat_en} outfit") };
    let tags = dedup(std::iter::once(prefix).chain(items.iter().map(|(m, w, _, _)| enp(t, m, w))).collect());
    (t.to_tw(&zh), tags.join(", "), t.to_tw(&title))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// 服裝目錄 → 抽屜要畫的格子 {id, cat, cat_en, kw, zh, en, title, thumb}
pub fn costumes(t: &Text, lib: &str, d: &[Value]) -> Vec<Value> {
    let b = base(lib).unwrap_or_default();
    d.iter()
        .map(|x| {
            let ja = s(x, "カテゴリ");
            let (cat, cat_en) = t.cc.get(ja).cloned().unwrap_or_else(|| (ja.to_string(), String::new()));
            let (zh, en, title) = costume_text(t, s(x, "キーワード"), &cat_en, s(x, "id"));
            let th = s(x, "サムネ");
            json!({"id": s(x, "id"), "cat": cat, "cat_en": cat_en, "kw": s(x, "キーワード"), "zh": zh, "en": en, "title": title,
                   "thumb": if safe_rel(th) { format!("{b}{th}") } else { String::new() }})
        })
        .collect()
}

/// 姿勢目錄（一張一筆）→ 一組一筆 {id, no, line, line_en, cat, ja, zh, en, angles:[{key, zh, en, item, thumb}]}；
/// 一組＝id 去掉最後一段角度。角度照 正面、上から、下から、横、斜め 排；缺角度的組照樣列。
pub fn poses(t: &Text, d: &[Value]) -> Vec<Value> {
    let b = base("pose").unwrap_or_default();
    let mut order: Vec<String> = vec![];
    let mut groups: HashMap<String, Vec<&Value>> = HashMap::new();
    for x in d {
        let id = s(x, "id");
        let g = id.rsplit_once('_').map(|(a, _)| a).unwrap_or(id).to_string();
        if !groups.contains_key(&g) {
            order.push(g.clone());
        }
        groups.entry(g).or_default().push(x);
    }
    let ang_idx = |ja: &str| t.ang.iter().position(|(k, _, _)| k == ja).unwrap_or(usize::MAX);
    order
        .into_iter()
        .map(|g| {
            let mut xs = groups.remove(&g).unwrap_or_default();
            xs.sort_by_key(|x| ang_idx(s(x, "アングル")));
            let f = xs[0];
            let ja = s(f, "ポーズ名");
            let (zh, en) = t.tr_pose.get(ja).cloned().unwrap_or_else(|| (ja.to_string(), String::new()));
            let (line, line_en) = t.line.get(s(f, "系統")).cloned().unwrap_or_else(|| (s(f, "系統").to_string(), String::new()));
            let cat = t.fix.get(&g).cloned().or_else(|| t.cat.get(s(f, "分類")).cloned()).unwrap_or_else(|| s(f, "分類").to_string());
            let angles: Vec<Value> = xs
                .iter()
                .map(|x| {
                    let a = s(x, "アングル");
                    let (azh, aen) = t.ang.iter().find(|(k, _, _)| k == a).map(|(_, z, e)| (z.clone(), e.clone())).unwrap_or((a.to_string(), String::new()));
                    let id = s(x, "id");
                    let th = s(x, "サムネ");
                    json!({"key": id.rsplit_once('_').map(|(_, k)| k).unwrap_or(""), "zh": azh, "en": aen, "item": id,
                           "thumb": if safe_rel(th) { format!("{b}{th}") } else { String::new() }})
                })
                .collect();
            json!({"id": g, "no": s(f, "ポーズ番号"), "line": line, "line_en": line_en, "cat": cat, "ja": ja, "zh": zh, "en": en, "angles": angles})
        })
        .collect()
}

/// 原站給的相對路徑才准拼網址：只准英數 _ . / -，不准 ..、開頭 /、空的
pub fn safe_rel(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains("..")
        && !p.contains("//")
        && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
}

pub fn find_item<'a>(d: &'a [Value], id: &str) -> Option<&'a Value> {
    d.iter().find(|x| s(x, "id") == id)
}

/// 拖進去時抓哪張：服裝用 画像（中尺寸 webp），沒有才用 原寸；姿勢線稿、深度圖用 原寸。路徑不安全就不給
pub fn image_rel(lib: &str, item: &Value) -> Option<String> {
    let keys: &[&str] = if lib == "anime" || lib == "costume" { &["画像", "原寸"] } else { &["原寸"] };
    keys.iter().map(|k| s(item, k)).find(|p| !p.is_empty()).filter(|p| safe_rel(p)).map(str::to_string)
}

/// ComfyUI input 裡的檔名：固定一張一個名字，同一張拖幾次都是同一個檔
pub fn input_name(lib: &str, id: &str) -> String {
    let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
    format!("pw_{lib}_{id}.png")
}

// ---------------------------------------------------------------- 衣姿整頁（2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-page-design.md）
// 整頁用的是 posewear 官網本身的資料檔（12 個分類），這裡負責：從官網首頁／app.js 找出資料檔網址、
// 把 ギア 四庫的圖片網址改回原站（官網放在作者自己轉存的 CDN）、檢查「送到繪匣」要抓的網址。

/// 官網資料檔的名稱（app.js 裡 '/assets/<名稱>.<雜湊>.json'）
pub const SPA_NAMES: [&str; 12] =
    ["poses", "costumes", "anime", "portraits", "depths", "expressions", "hair", "dynasties", "scenes", "lights", "props", "furniture"];
pub const SPA_SITE: &str = "https://posewear.pages.dev/";
/// 官網的圖片主機（app.js 的 AH）；資料檔裡的相對路徑都接在它後面
pub const PWIMG: &str = "https://pwimg.0vo.pro/";
/// 「送到繪匣」可以抓圖的主機（都是官網資料檔裡會出現的）
const URL_HOSTS: [&str; 8] = [
    "pwimg.0vo.pro",
    "posewear.pages.dev",
    "charadb-2fi.pages.dev",
    "ai-pose-gallery.pages.dev",
    "ai-costume-gallery.pages.dev",
    "ai-costume-anime-gallery.pages.dev",
    "ai-pose-depth-gallery.pages.dev",
    "ai-portrait-gallery.pages.dev",
];

/// 官網首頁 → 'assets/app.<雜湊>.js'
pub fn spa_app_js(index_html: &str) -> Option<String> {
    Regex::new(r"assets/app\.[0-9a-f]+\.js").ok()?.find(index_html).map(|m| m.as_str().to_string())
}

/// app.js → {名稱: 'assets/<名稱>.<雜湊>.json'}，只收 SPA_NAMES 裡的
pub fn spa_assets(app_js: &str) -> HashMap<String, String> {
    let re = Regex::new(r"/assets/([a-z]+)\.[0-9a-f]+\.json").expect("regex");
    re.captures_iter(app_js)
        .filter(|c| SPA_NAMES.contains(&&c[1]))
        .map(|c| (c[1].to_string(), c[0].trim_start_matches('/').to_string()))
        .collect()
}

fn set_url(o: &mut Map<String, Value>, k: &str, base: &str, rel: &str) {
    if o.contains_key(k) && safe_rel(rel) {
        o.insert(k.into(), Value::String(format!("{base}{rel}")));
    }
}

/// 官網的大圖（i、m）放在官網本站，縮圖（t）、原圖（o）才在 PWIMG（app.js 只幫 t、o 接 AH）。
/// 頁面搬到繪匣後相對路徑會指到繪匣自己，所以相對路徑的 i、m 一律接上官網網址
fn absolutize_big(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(absolutize_big),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(s) if (k == "i" || k == "m") && !s.starts_with("http") && safe_rel(s) => *s = format!("{SPA_SITE}{s}"),
                    _ => absolutize_big(x),
                }
            }
        }
        _ => {}
    }
}

/// ギア 四庫的圖片網址改回原站；cat 是對應的 ギア 目錄（pose／costume／anime），沒有就不改（照官網原樣走 pwimg）。
/// 其他分類只把大圖（i、m）接上官網網址（absolutize_big）
pub fn rewrite_spa(name: &str, v: &mut Value, cat: Option<&[Value]>) {
    rewrite_gear(name, v, cat);
    absolutize_big(v);
}

fn rewrite_gear(name: &str, v: &mut Value, cat: Option<&[Value]>) {
    match name {
        "depths" => {
            let b = base("depth").unwrap_or_default();
            for it in v.get_mut("items").and_then(Value::as_array_mut).into_iter().flatten() {
                for a in it.get_mut("angles").and_then(Value::as_array_mut).into_iter().flatten() {
                    for k in ["t", "i", "o"] {
                        if let Some(s) = a.get(k).and_then(Value::as_str).and_then(|s| s.strip_prefix("https://pwimg.0vo.pro/d/")) {
                            let u = format!("{b}{s}");
                            a[k] = Value::String(u);
                        }
                    }
                }
            }
        }
        "poses" => {
            let (Some(cat), Some(b)) = (cat, base("pose")) else { return };
            for it in v.as_array_mut().into_iter().flatten() {
                for a in it.get_mut("angles").and_then(Value::as_array_mut).into_iter().flatten() {
                    // 角度 id 從官網的路徑拿：p/t/<id>.webp
                    let id = a.get("t").and_then(Value::as_str).and_then(|t| t.rsplit('/').next()).and_then(|f| f.rsplit_once('.')).map(|(s, _)| s.to_string());
                    let Some(src) = id.and_then(|id| find_item(cat, &id)) else { continue };
                    let Some(o) = a.as_object_mut() else { continue };
                    set_url(o, "t", b, s(src, "サムネ"));
                    set_url(o, "i", b, s(src, "原寸"));
                    set_url(o, "o", b, s(src, "原寸"));
                }
            }
        }
        "costumes" | "anime" => {
            let lib = if name == "anime" { "anime" } else { "costume" };
            let (Some(cat), Some(b)) = (cat, base(lib)) else { return };
            for it in v.as_array_mut().into_iter().flatten() {
                let id = it.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                if it.get("ext").is_some() || id.starts_with("charadb-") {
                    continue;
                }
                let Some(src) = find_item(cat, &id) else { continue };
                let Some(o) = it.as_object_mut() else { continue };
                let img = s(src, "画像");
                let full = Some(s(src, "原寸")).filter(|x| !x.is_empty()).unwrap_or(img);
                set_url(o, "t", b, s(src, "サムネ"));
                set_url(o, "m", b, img);
                set_url(o, "i", b, img);
                set_url(o, "o", b, full);
            }
        }
        _ => {}
    }
}

/// 資料檔裡有沒有這個圖片網址：絕對網址逐字比對，相對路徑接在 PWIMG 後面比對
pub fn spa_has_url(v: &Value, url: &str) -> bool {
    url.starts_with("https://") && has_url(v, url)
}

fn has_url(v: &Value, url: &str) -> bool {
    match v {
        Value::String(s) => s == url || (!s.starts_with("http") && url.strip_prefix(PWIMG) == Some(s.as_str())),
        Value::Array(a) => a.iter().any(|x| has_url(x, url)),
        Value::Object(o) => o.values().any(|x| has_url(x, url)),
        _ => false,
    }
}

/// 只准 https、白名單主機、沒有帳密、沒有指定埠
pub fn url_host_ok(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && u.host_str().is_some_and(|h| URL_HOSTS.contains(&h))
        && !u.path().contains("..")
}

/// 用網址抓的圖在 ComfyUI input 的檔名：同一個網址固定同一個檔
pub fn url_input_name(url: &str) -> String {
    format!("pw_url_{:08x}.png", crc32(url.as_bytes()))
}

// ---------------------------------------------------------------- 衣姿：執行時跟官網同步前端（2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md）
// 從官網首頁找出正在用的 5 個前端檔，套上跟 tools\posewear_vendor.py 同義的替換規則（寫法照上線版）。
// 每條規則都要剛好命中規定的次數；有一條不對就回 Err（帶規則名稱），整套退回安裝檔內建版。

/// 會同步的前端檔（網址 /api/posewear/front/file/<名稱> 只收這幾個）
pub const FRONT_FILES: [&str; 5] = ["app.js", "studio.js", "extras.js", "zip.js", "style.css"];
/// 單檔上限 2MB
pub const FRONT_MAX: usize = 2 * 1024 * 1024;
/// 工作台「交出整套」：把 studio.js 的 `window.WB={` 換成這段（tools\posewear_vendor.py 讀同一個檔）
pub const BUNDLE_JS: &str = include_str!("../../resources/share/posewear_bundle.js.txt");

/// studio.js 的 loadImg：網址圖加 crossOrigin（tools\posewear_vendor.py 用同一組字串）
const LOADIMG_OLD: &str = "function loadImg(src){return new Promise((r,j)=>{const i=new Image();";
const LOADIMG_NEW: &str = "function loadImg(src){return new Promise((r,j)=>{const i=new Image();if(/^https?:/.test(src))i.crossOrigin='anonymous';";

/// 網址裡的檔名 → 白名單裡的名稱；其他一律 None
pub fn front_name(s: &str) -> Option<&'static str> {
    FRONT_FILES.iter().copied().find(|n| *n == s)
}

/// 同步只准抓官網本身（HTTPS、主機剛好是 posewear.pages.dev）；轉址也用這個檢查
pub fn front_url_ok(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str() == Some("posewear.pages.dev"))
}

/// 官網首頁 → {"app.js": "assets/app.<雜湊>.js", …, "style.css": "assets/style.<雜湊>.css"}；少一個就 Err
pub fn front_assets(index_html: &str) -> Result<HashMap<String, String>, String> {
    let re = Regex::new(r"/assets/(app|studio|extras|zip|style)\.[0-9a-f]+\.(js|css)").expect("regex");
    let mut m = HashMap::new();
    for c in re.captures_iter(index_html) {
        let name = format!("{}.{}", &c[1], &c[2]);
        if front_name(&name).is_some() {
            m.entry(name).or_insert_with(|| c[0].trim_start_matches('/').to_string());
        }
    }
    match FRONT_FILES.iter().find(|n| !m.contains_key(**n)) {
        Some(n) => Err(format!("首頁：找不到 {n}")),
        None => Ok(m),
    }
}

/// 剛好 n 處（ns 列出允許的次數）才換，否則 Err(規則名稱)
fn front_rep(text: &str, old: &str, new: &str, ok: &[usize], rule: &str) -> Result<String, String> {
    let n = text.matches(old).count();
    if !ok.contains(&n) {
        return Err(format!("{rule}（預期 {ok:?} 處，找到 {n} 處）"));
    }
    Ok(text.replace(old, new))
}

/// 套上這個檔的全部替換規則；任何一條次數不對就 Err，訊息帶規則名稱
pub fn front_patch(name: &str, text: &str) -> Result<String, String> {
    match name {
        "app.js" => {
            let mut t = text.to_string();
            for d in SPA_NAMES {
                let rule = format!("app.js：資料檔 {d}");
                let re = Regex::new(&format!(r"'/assets/{d}\.[0-9a-f]+\.json'")).expect("regex");
                let n = re.find_iter(&t).count();
                if n != 1 {
                    return Err(format!("{rule}（預期 1 處，找到 {n} 處）"));
                }
                t = re.replace(&t, format!("'/api/posewear/spa/{d}'").as_str()).into_owned();
            }
            let re = Regex::new(r"const AH='[^']*';").expect("regex");
            let n = re.find_iter(&t).count();
            if n != 1 {
                return Err(format!("app.js：圖片主機 AH（預期 1 處，找到 {n} 處）"));
            }
            t = re.replace(&t, format!("const AH='{PWIMG}';").as_str()).into_owned();
            t = front_rep(
                &t,
                "const orig=it=>it.ext?'':((AH||'/')+(it.angles?it.angles[it.ai].o:it.o));",
                "const orig=it=>{if(it.ext)return'';const o=it.angles?it.angles[it.ai].o:it.o;return o.startsWith('http')?o:(AH||'/')+o};",
                &[1],
                "app.js：原圖網址 orig",
            )?;
            // 上線版若沒有絕對路徑的 Service Worker 就不套
            front_rep(&t, "register('/sw.js')", "register('sw.js')", &[0, 1], "app.js：Service Worker")
        }
        "studio.js" => {
            let t = front_rep(text, "window.WB={", BUNDLE_JS.trim_end(), &[1], "studio.js：交出整套 WB.bundle")?;
            // 人像（posewear.pages.dev/r/…）在繪匣裡是跨網域：不設 crossOrigin，畫進 canvas 後 toBlob 會丟 SecurityError（「人像加载失败」）
            front_rep(&t, LOADIMG_OLD, LOADIMG_NEW, &[1], "studio.js：跨網域圖片 loadImg")
        }
        "extras.js" | "zip.js" | "style.css" => Ok(text.to_string()),
        _ => Err(format!("不認得的前端檔：{name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Text {
        let res = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("resources");
        Text::load(&res).unwrap()
    }

    fn spa_sample() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/posewear_spa/sample.json")).unwrap()
    }

    fn arr(v: &Value, k: &str) -> Vec<Value> {
        v[k].as_array().unwrap().clone()
    }

    #[test]
    fn spa_app_js_found() {
        let html = r#"<link rel="modulepreload" href="/assets/app.3c9fb95b16.js"><script src="/assets/zip.11d4d7b041.js">"#;
        assert_eq!(spa_app_js(html).as_deref(), Some("assets/app.3c9fb95b16.js"));
        assert_eq!(spa_app_js("assets/app.zz.js"), None);
    }

    #[test]
    fn spa_assets_twelve() {
        let js: String = SPA_NAMES.iter().map(|n| format!("fetch('/assets/{n}.0a1b2c.json');")).collect::<String>() + "'/assets/other.ff.json'";
        let m = spa_assets(&js);
        assert_eq!(m.len(), 12);
        assert_eq!(m["anime"], "assets/anime.0a1b2c.json");
        assert!(!m.contains_key("other"));
    }

    #[test]
    fn spa_asset_missing() {
        assert!(spa_assets("").is_empty());
        assert_eq!(spa_app_js("<html></html>"), None);
    }

    #[test]
    fn rewrite_poses_to_origin() {
        let s = spa_sample();
        let mut v = s["poses"].clone();
        rewrite_spa("poses", &mut v, Some(&arr(&s, "cat_pose")));
        let a = &v[0]["angles"][0];
        assert_eq!(a["t"], "https://ai-pose-gallery.pages.dev/thumbs/full_single_P001_front.jpg");
        assert_eq!(a["i"], "https://ai-pose-gallery.pages.dev/img/full_single_P001_front.png");
        assert_eq!(a["o"], "https://ai-pose-gallery.pages.dev/img/full_single_P001_front.png");
    }

    #[test]
    fn rewrite_costumes_to_origin() {
        let s = spa_sample();
        let mut v = s["costumes"].clone();
        rewrite_spa("costumes", &mut v, Some(&arr(&s, "cat_costume")));
        assert_eq!(v[0]["t"], "https://ai-costume-gallery.pages.dev/thumbs/SPR-0001.webp");
        assert_eq!(v[0]["m"], "https://ai-costume-gallery.pages.dev/img/SPR-0001.webp");
        assert_eq!(v[0]["i"], "https://ai-costume-gallery.pages.dev/img/SPR-0001.webp");
        assert_eq!(v[0]["o"], "https://ai-costume-gallery.pages.dev/png/SPR-0001.png");
    }

    #[test]
    fn rewrite_skips_charadb() {
        let s = spa_sample();
        let mut v = s["anime"].clone();
        rewrite_spa("anime", &mut v, Some(&arr(&s, "cat_anime")));
        assert_eq!(v[0]["t"], "https://ai-costume-anime-gallery.pages.dev/thumbs/SPR-0001.webp");
        assert_eq!(v[1], s["anime"][1], "CharaDB 的條目不動");
    }

    #[test]
    fn rewrite_depths_to_origin() {
        let s = spa_sample();
        let mut v = s["depths"].clone();
        rewrite_spa("depths", &mut v, None);
        let a = &v["items"][0]["angles"][0];
        assert_eq!(a["t"], "https://ai-pose-depth-gallery.pages.dev/thumbs/full_sexy_P001_front.jpg");
        assert_eq!(a["i"], "https://ai-pose-depth-gallery.pages.dev/img/full_sexy_P001_front.png");
    }

    #[test]
    fn rewrite_without_catalog_keeps_original() {
        let s = spa_sample();
        // 沒有 ギア 目錄：縮圖、原圖照官網原樣，只有大圖接上官網網址
        let mut v = s["costumes"].clone();
        rewrite_spa("costumes", &mut v, None);
        assert_eq!((v[0]["t"].as_str(), v[0]["o"].as_str()), (s["costumes"][0]["t"].as_str(), s["costumes"][0]["o"].as_str()));
        assert_eq!(v[0]["i"], "https://posewear.pages.dev/c/i/SPR-0001.webp");
        let mut v = s["anime"].clone();
        rewrite_spa("anime", &mut v, None);
        assert_eq!(v[1], s["anime"][1], "CharaDB 本來就是絕對網址");
    }

    #[test]
    fn rewrite_big_images_to_site() {
        let mut v = json!([{"angles": [{"t": "e/t/a.webp", "i": "e/i/a.webp", "o": "o/e/a.webp"}]}, {"m": "r/m/x.webp", "i": "r/m/x.webp", "t": "r/t/x.webp"},
                           {"i": "../x.webp"}, {"i": 3}]);
        rewrite_spa("expressions", &mut v, Some(&[]));
        assert_eq!(v[0]["angles"][0], json!({"t": "e/t/a.webp", "i": "https://posewear.pages.dev/e/i/a.webp", "o": "o/e/a.webp"}));
        assert_eq!(v[1]["m"], "https://posewear.pages.dev/r/m/x.webp");
        assert_eq!(v[1]["t"], "r/t/x.webp");
        assert_eq!(v[2]["i"], "../x.webp", "不安全的路徑不接");
        assert_eq!(v[3]["i"], 3);
        assert!(url_host_ok("https://posewear.pages.dev/e/i/a.webp"));
        assert!(spa_has_url(&v, "https://posewear.pages.dev/e/i/a.webp"));
    }

    #[test]
    fn spa_has_url_relative_and_absolute() {
        let v = json!([{"angles": [{"t": "e/t/a.webp"}]}, {"i": "https://charadb-2fi.pages.dev/img/H001.jpg"}]);
        assert!(spa_has_url(&v, "https://pwimg.0vo.pro/e/t/a.webp"));
        assert!(spa_has_url(&v, "https://charadb-2fi.pages.dev/img/H001.jpg"));
        assert!(!spa_has_url(&v, "https://pwimg.0vo.pro/e/t/b.webp"));
        assert!(!spa_has_url(&v, "e/t/a.webp"));
    }

    #[test]
    fn url_host_rules() {
        assert!(url_host_ok("https://pwimg.0vo.pro/x.webp"));
        assert!(url_host_ok("https://charadb-2fi.pages.dev/img/H001.jpg"));
        for bad in [
            "http://pwimg.0vo.pro/x",
            "https://evil.com/x",
            "https://u:p@pwimg.0vo.pro/x",
            "https://pwimg.0vo.pro:8080/x",
            "https://pwimg.0vo.pro.evil.com/x",
            "not a url",
        ] {
            assert!(!url_host_ok(bad), "{bad}");
        }
        assert_eq!(url_input_name("https://pwimg.0vo.pro/x.webp"), url_input_name("https://pwimg.0vo.pro/x.webp"));
        assert!(url_input_name("a").starts_with("pw_url_") && url_input_name("a").ends_with(".png"));
    }

    #[test]
    fn parse_ok_and_errors() {
        let d = parse_inline_d("<script>\nconst META={};\nconst D=[{\"id\":\"A\"},{\"id\":\"B\"}];\nconst X=1;\n").unwrap();
        assert_eq!(d.len(), 2);
        assert!(parse_inline_d("<html>沒有</html>").unwrap_err().contains("改版"));
        assert!(parse_inline_d("const D=[{oops];\n").unwrap_err().contains("改版"));
        assert!(parse_inline_d("const D={\"id\":1};\n").unwrap_err().contains("不是陣列"));
        assert!(parse_inline_d("const D=[{\"x\":1}];\n").unwrap_err().contains("沒有 id"));
    }

    #[test]
    fn crc32_matches_zlib() {
        assert_eq!(crc32(b"abc"), 0x3524_41C2);
        assert_eq!(crc32(b"SPR-0001"), 0xBF91_AB4A); // Python：zlib.crc32(b"SPR-0001")
    }

    #[test]
    fn costume_cases_match_python() {
        let t = text();
        let cases: Vec<Value> = serde_json::from_str(include_str!("../tests/fixtures/posewear_costume_cases.json")).unwrap();
        assert_eq!(cases.len(), 30);
        for c in &cases {
            let (zh, en, title) = costume_text(&t, s(c, "kw"), s(c, "cat_en"), s(c, "id"));
            assert_eq!(zh, s(c, "zh"), "{}", s(c, "id"));
            assert_eq!(en, s(c, "en"), "{}", s(c, "id"));
            assert_eq!(title, s(c, "title"), "{}", s(c, "id"));
        }
    }

    /// 全部 2727 套跟 Python 原版逐字比（資料不進 repo）：PW_ALL_CASES=<json> cargo test --lib posewear -- --ignored
    #[test]
    #[ignore]
    fn costume_all_cases_match_python() {
        let t = text();
        let p = std::env::var("PW_ALL_CASES").expect("PW_ALL_CASES");
        let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        let bad: Vec<String> = cases
            .iter()
            .filter_map(|c| {
                let got = costume_text(&t, s(c, "kw"), s(c, "cat_en"), s(c, "id"));
                (got != (s(c, "zh").into(), s(c, "en").into(), s(c, "title").into()))
                    .then(|| format!("{}：{:?} ≠ {} | {} | {}", s(c, "id"), got, s(c, "zh"), s(c, "en"), s(c, "title")))
            })
            .collect();
        assert!(bad.is_empty(), "{} 筆不同，前幾筆：\n{}", bad.len(), bad[..bad.len().min(5)].join("\n"));
    }

    #[test]
    fn poses_group_and_sort() {
        let t = text();
        let mk = |id: &str, ang: &str| json!({"id": id, "系統": "全身1人・標準系", "ポーズ番号": "P001", "分類": "立ち",
            "ポーズ名": "S字立ち・腰を横に突き出す", "アングル": ang, "サムネ": format!("thumbs/{id}.jpg"), "原寸": format!("img/{id}.png")});
        let d = vec![
            mk("std_P001_side", "横"), mk("std_P001_front", "正面"), mk("std_P001_diag", "斜め"),
            mk("std_P001_low", "下から"), mk("std_P001_high", "上から"),
            mk("pair60_P031_front", "正面"), mk("pair60_P031_side", "横"),
        ];
        let g = poses(&t, &d);
        assert_eq!(g.len(), 2);
        let keys: Vec<&str> = g[0]["angles"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["front", "high", "low", "side", "diag"]);
        assert_eq!(g[0]["zh"], "S形站姿·胯部側頂");
        assert_eq!(g[0]["en"], "S-curve standing pose, hip pushed out to the side");
        assert_eq!(g[0]["line_en"], "full body, solo");
        assert_eq!(g[0]["angles"][0]["thumb"], "https://ai-pose-gallery.pages.dev/thumbs/std_P001_front.jpg");
        assert_eq!(g[1]["angles"].as_array().unwrap().len(), 2);
        assert_eq!(g[1]["cat"], "坐位"); // build.py 的人工修正 FIX
    }

    #[test]
    fn safe_rel_rejects() {
        assert!(safe_rel("img/full_sexy_P001_front.png"));
        for bad in ["", "../a.png", "/a.png", "http://x/a.png", "a\\b.png", "img//a.png", "a b.png"] {
            assert!(!safe_rel(bad), "{bad}");
        }
    }

    #[test]
    fn image_rel_prefers_medium() {
        let c = json!({"id": "SPR-0001", "画像": "img/SPR-0001.webp", "原寸": "png/SPR-0001.png"});
        assert_eq!(image_rel("anime", &c).as_deref(), Some("img/SPR-0001.webp"));
        assert_eq!(image_rel("costume", &json!({"原寸": "png/a.png"})).as_deref(), Some("png/a.png"));
        assert_eq!(image_rel("depth", &json!({"原寸": "img/a.png", "サムネ": "thumbs/a.jpg"})).as_deref(), Some("img/a.png"));
        assert_eq!(image_rel("pose", &json!({"原寸": "../x.png"})), None);
        assert!(find_item(std::slice::from_ref(&c), "SPR-0001").is_some() && find_item(&[c], "nope").is_none());
    }

    #[test]
    fn input_name_sanitizes() {
        assert_eq!(input_name("depth", "full_sexy_P001_front"), "pw_depth_full_sexy_P001_front.png");
        assert_eq!(input_name("anime", "SPR-0001/../x"), "pw_anime_SPR-0001x.png");
    }
    // ---------------------------------------------------------------- 衣姿：執行時同步（2026-11-22）
    fn front_fixture(name: &str) -> String {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("posewear_front");
        let idx = std::fs::read_to_string(dir.join("index.html")).unwrap();
        if name == "index.html" {
            return idx;
        }
        let rel = front_assets(&idx).unwrap()[name].clone();
        std::fs::read_to_string(dir.join(rel.trim_start_matches("assets/"))).unwrap()
    }

    #[test]
    fn front_assets_finds_all_five() {
        let m = front_assets(&front_fixture("index.html")).unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(m["app.js"], "assets/app.d529c13529.js");
        assert_eq!(m["style.css"], "assets/style.54f8c037a9.css");
        for n in FRONT_FILES {
            assert!(m.contains_key(n), "{n}");
        }
        let e = front_assets(r#"<script src="/assets/app.d529c13529.js">"#).unwrap_err();
        assert!(e.contains("studio.js"), "{e}");
    }

    #[test]
    fn front_patch_fixture_all_ok() {
        for n in FRONT_FILES {
            let t = front_fixture(n);
            let out = front_patch(n, &t).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert!(out.len() <= FRONT_MAX + 4096);
            if n == "app.js" {
                for d in SPA_NAMES {
                    assert!(out.contains(&format!("'/api/posewear/spa/{d}'")), "{d}");
                }
                assert!(!out.contains("/assets/"));
                assert!(out.contains("const AH='https://pwimg.0vo.pro/';"));
                assert!(out.contains("return o.startsWith('http')?o:(AH||'/')+o"));
                assert!(out.contains("register('sw.js')"));
            }
            if n == "studio.js" {
                assert!(out.contains("window.WB={bundle:async()=>{"));
                assert_eq!(out.matches("window.WB={").count(), 1);
                // 人像在繪匣裡是跨網域，沒有 crossOrigin 會汙染 canvas、toBlob 丟 SecurityError（「人像加载失败」）
                assert!(out.contains("const i=new Image();if(/^https?:/.test(src))i.crossOrigin='anonymous';"));
            }
        }
    }

    #[test]
    fn front_patch_reports_rule_name() {
        let app = front_fixture("app.js").replace("const AH='https://pwimg.0vo.pro/';", "");
        let e = front_patch("app.js", &app).unwrap_err();
        assert!(e.contains("AH"), "{e}");
        let st = front_fixture("studio.js").replace("window.WB={", "window.WB = {");
        let e = front_patch("studio.js", &st).unwrap_err();
        assert!(e.contains("WB.bundle"), "{e}");
        let st = front_fixture("studio.js").replace("function loadImg(src)", "function loadImage(src)");
        assert!(front_patch("studio.js", &st).unwrap_err().contains("loadImg"));
        let app = front_fixture("app.js").replacen("'/assets/poses.2383bf449f.json'", "'x'", 1);
        assert!(front_patch("app.js", &app).unwrap_err().contains("poses"));
        // 上游 src 的寫法（AH 空字串、沒有 Service Worker 絕對路徑）也要能套
        let app = front_fixture("app.js").replace("const AH='https://pwimg.0vo.pro/';", "const AH='';").replace("register('/sw.js')", "register(x)");
        assert!(front_patch("app.js", &app).unwrap().contains("const AH='https://pwimg.0vo.pro/';"));
        assert!(front_patch("index.html", "x").is_err());
    }

    #[test]
    fn front_name_whitelist_and_url() {
        assert_eq!(front_name("app.js"), Some("app.js"));
        assert_eq!(front_name("style.css"), Some("style.css"));
        for bad in ["index.html", "../app.js", "app.tw.js", "sw.js", ""] {
            assert_eq!(front_name(bad), None, "{bad}");
        }
        assert!(front_url_ok("https://posewear.pages.dev/assets/app.1.js"));
        for bad in ["http://posewear.pages.dev/", "https://evil.example/", "https://posewear.pages.dev.evil.com/", "https://x.posewear.pages.dev/"] {
            assert!(!front_url_ok(bad), "{bad}");
        }
    }
}

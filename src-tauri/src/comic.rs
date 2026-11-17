//! 漫畫台（2026-10-16）：一整頁分格漫畫＝選版型、每格寫景別／內容／台詞／狀聲詞 → 組提示詞 → MOSS-Image 一次畫完整頁和字。
//! 2026-10-25 起可以叫本機 35B 把「這段劇情」拆成每一格（`script_messages`／`parse_script`；使用者指定用 35B），
//! 排好的東西一樣是普通欄位，可以再自己改。
//!
//! 2026-10-20 起只剩 MOSS-Image 一條路（使用者：「都只讓 MOSS 用就好」）。以前還有「Anima 畫整頁、每格留白框 → Qwen 2.1 多圖參考填字」，
//! 跟 Qwen 2.1 一次畫完兩條，都拿掉了（design\漫畫頁提示詞設計.md 有當時的五輪實測）。MOSS 2026-10-17 實測：四格、三句繁中台詞＋狀聲詞一次全對。
//! 這裡放不碰網路的部分：版型、組提示詞、作品存檔。執行在 server\api_comic.rs。
//! 作品存在 data\comic\projects\<id>.json（不動 anima.db）。舊作品檔裡的 flow／rating／tags／auto_fill 欄位留著，程式不再讀。
use crate::director::{new_id, tx, valid_id, write_atomic};
use crate::py::now;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 一種版型：(代號, 中文名, 英文版型句, [(英文格子位置, 中文格子位置)])
pub type Layout = (&'static str, &'static str, &'static str, &'static [(&'static str, &'static str)]);

/// 版型，閱讀順序右上先。
/// 版型句與格子位置的說法都實測過 Anima／Qwen 會照著分格（Anima 52 頁分對 51 頁，錯的是六格）；MOSS 照同一套句子也分對，現在只給 MOSS 用
pub const LAYOUTS: &[Layout] = &[
    ("L3", "三格：右邊直長主格＋左邊上下兩格",
     "three panels: one tall panel on the right that spans the full height of the page, and two stacked panels on the left",
     &[("tall right panel", "右邊直長那格"), ("top left panel", "左上那格"), ("bottom left panel", "左下那格")]),
    ("L4", "四格：上面兩小格＋中間大格＋右下斜切小格",
     "four panels: two small panels side by side at the top, one large panel in the middle, and a small slanted inset panel at the bottom right",
     &[("top right panel", "右上那格"), ("top left panel", "左上那格"), ("large middle panel", "中間大格"),
       ("small slanted inset panel at the bottom right", "右下斜切的小格")]),
    ("L4w", "四格：上面橫長＋中間兩格＋下面大格",
     "four panels: one wide panel across the top, two panels side by side in the middle, and one large panel across the bottom",
     &[("wide top panel", "最上面橫長那格"), ("middle right panel", "中間右邊那格"), ("middle left panel", "中間左邊那格"),
       ("large bottom panel", "最下面大格")]),
    ("L5", "五格：上面橫長＋下面兩排各兩格",
     "five panels: one wide panel across the top, then two rows of two panels side by side",
     &[("wide top panel", "最上面橫長那格"), ("middle right panel", "中間右邊那格"), ("middle left panel", "中間左邊那格"),
       ("bottom right panel", "右下那格"), ("bottom left panel", "左下那格")]),
    ("L6", "六格：三排各兩格",
     "six panels in three rows of two",
     &[("top right panel", "右上那格"), ("top left panel", "左上那格"), ("middle right panel", "中間右邊那格"),
       ("middle left panel", "中間左邊那格"), ("bottom right panel", "右下那格"), ("bottom left panel", "左下那格")]),
    ("K4", "四格漫畫：四格一樣大由上往下",
     "four horizontal panels of equal size stacked from top to bottom, like a 4koma strip",
     &[("first panel at the top", "第一格（最上面）"), ("second panel", "第二格"), ("third panel", "第三格"),
       ("fourth panel at the bottom", "第四格（最下面）")]),
];

/// 對話框位置：(英文, 中文)
pub const BUBBLES: &[(&str, &str)] = &[
    ("top left", "左上角"), ("top right", "右上角"), ("top", "上方"), ("left side", "左邊"), ("right side", "右邊"), ("bottom", "下方"),
];

/// 景別：(英文, 中文)。英文會接成「In the X panel, <景別> shows …」「The X panel shows <景別> of …」，兩種句子都要通順
pub const SHOTS: &[(&str, &str)] = &[
    ("a wide shot", "遠景（交代場景）"), ("a full-body view", "全身"), ("a cowboy shot", "七分身（大腿以上）"), ("a medium shot", "半身"),
    ("a close-up", "特寫"), ("an extreme close-up", "大特寫"), ("a low-angle view", "仰角（由下往上）"), ("a high-angle view", "俯角（由上往下）"),
    ("a first-person point-of-view shot", "第一人稱視角"), ("a side view", "側面"), ("a dynamic wide shot", "動態大場面"),
];

/// 頁面尺寸 832×1248（使用者說不要用高解析）
pub const MOSS_SIZE: (i64, i64) = (832, 1248);

pub fn layout(id: &str) -> Option<&'static Layout> {
    LAYOUTS.iter().find(|l| l.0 == id)
}

fn bubble(en: &str) -> (&'static str, &'static str) {
    BUBBLES.iter().copied().find(|b| b.0 == en).unwrap_or(BUBBLES[2])
}

// ================================================================ 版面（六種預設＋自己排的）
/// 自訂排版的一列：(高度權重, 每一格的寬度權重)。權重 1～4，數字只是比例
pub type Row = (u32, Vec<u32>);

/// 一頁的排版：(英文描述, [(英文位置, 中文位置)])。`layout` 是預設版型代號，或 "custom"（用 `custom.rows`）
pub fn layout_of(p: &Value) -> Result<(String, Vec<(String, String)>), String> {
    let id = tx(&p["layout"]);
    if id == "custom" {
        if is_free(&p["custom"]) {
            return Ok(free_layout(&p["custom"]));
        }
        return custom_layout(rows_of(p)?);
    }
    let l = layout(&id).ok_or_else(|| format!("沒有這個版型：{id}"))?;
    Ok((l.2.to_string(), l.3.iter().map(|(en, zh)| (en.to_string(), zh.to_string())).collect()))
}

/// 自訂排版的列：最多 6 列、每列 4 格、全部最多 8 格（MOSS 一頁畫得完的範圍）
pub fn rows_of(p: &Value) -> Result<Vec<Row>, String> {
    let list = p["custom"]["rows"].as_array().ok_or("自訂版面還沒有排任何格")?;
    let mut out: Vec<Row> = Vec::new();
    let mut total = 0usize;
    for r in list.iter().take(6) {
        let h = crate::py::int(&r["h"]).unwrap_or(1).clamp(1, 4) as u32;
        let cells: Vec<u32> = r["cells"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(crate::py::int)
            .map(|w| w.clamp(1, 4) as u32)
            .take(4)
            .collect();
        if cells.is_empty() {
            continue;
        }
        total += cells.len();
        out.push((h, cells));
        if total >= 8 {
            break;
        }
    }
    if out.is_empty() {
        return Err("自訂版面至少要有一格".into());
    }
    Ok(out)
}

/// 這一列在頁面上的位置：(給描述用的片語, 給格子名字用的短詞, 中文)
fn row_place(i: usize, n: usize) -> (&'static str, &'static str, &'static str) {
    match n {
        1 => ("", "", ""),
        2 => [("at the top", "top", "上"), ("at the bottom", "bottom", "下")][i],
        3 => [("at the top", "top", "上"), ("in the middle", "middle", "中"), ("at the bottom", "bottom", "下")][i],
        4 => [
            ("at the top", "top", "上"),
            ("in the upper middle", "upper middle", "上中"),
            ("in the lower middle", "lower middle", "下中"),
            ("at the bottom", "bottom", "下"),
        ][i],
        _ => ("", "", ""),
    }
}

/// 一列有幾格、寬度一不一樣
fn row_phrase(cells: &[u32]) -> String {
    let n = cells.len();
    let equal = cells.windows(2).all(|w| w[0] == w[1]);
    match n {
        1 => "one wide panel".to_string(),
        2 if equal => "two panels side by side".to_string(),
        2 => "two panels side by side, one of them wider".to_string(),
        3 if equal => "three panels side by side".to_string(),
        3 => "three panels side by side with different widths".to_string(),
        4 if equal => "four panels side by side".to_string(),
        _ => format!("{n} panels side by side"),
    }
}

/// 同一列裡這一格的位置（k＝由左數第幾格，0 開始）：(英文, 中文)
fn col_word(k: usize, m: usize) -> (String, String) {
    let (en, zh) = match m {
        1 => ("", ""),
        2 => [("left", "左"), ("right", "右")][k],
        3 => [("left", "左"), ("center", "中"), ("right", "右")][k],
        4 => [("leftmost", "最左"), ("center left", "左中"), ("center right", "右中"), ("rightmost", "最右")][k],
        _ => return (format!("column {}", k + 1), format!("第{}欄", k + 1)),
    };
    (en.to_string(), zh.to_string())
}

/// 自己排的版面 → (英文描述, 每一格的位置)。閱讀順序：上面先、同一列右邊先（日本漫畫右到左）
pub fn custom_layout(rows: Vec<Row>) -> Result<(String, Vec<(String, String)>), String> {
    let n = rows.len();
    let parts: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, (_, cells))| {
            let (place, _, _) = row_place(i, n);
            if place.is_empty() {
                row_phrase(cells)
            } else {
                format!("{} {place}", row_phrase(cells))
            }
        })
        .collect();
    let mut desc = if n == 1 { parts.join("") } else { format!("{n} rows: {}", parts.join(", ")) };
    // 列高不一樣：把比較高的列講出來，不然模型會平均分
    let hs: Vec<u32> = rows.iter().map(|r| r.0).collect();
    if hs.iter().max() != hs.iter().min() {
        let max = *hs.iter().max().unwrap_or(&1);
        let tall: Vec<String> = hs
            .iter()
            .enumerate()
            .filter(|(_, h)| **h == max)
            .map(|(i, _)| {
                let (place, word, _) = row_place(i, n);
                if place.is_empty() { format!("row {}", i + 1) } else { word.to_string() }
            })
            .collect();
        if !tall.is_empty() {
            desc.push_str(&format!(
                " (the {} {})",
                tall.join(" and "),
                if tall.len() > 1 { "rows are taller" } else { "row is taller" }
            ));
        }
    }
    let mut slots = Vec::with_capacity(rows.iter().map(|r| r.1.len()).sum());
    for (i, (_, cells)) in rows.iter().enumerate() {
        let m = cells.len();
        // 超過 4 列就沒有 top／middle／bottom 這種說法了，改用「第幾列」
        let (rword, rzh) = if n >= 5 {
            (format!("row {}", i + 1), format!("第{}列", i + 1))
        } else {
            let (_, word, zh) = row_place(i, n);
            (word.to_string(), zh.to_string())
        };
        // 同一列從右邊先讀
        for k in (0..m).rev() {
            let (cword, czh) = col_word(k, m);
            let en = if rword.is_empty() && cword.is_empty() {
                "panel".to_string()
            } else {
                let parts: Vec<String> = [rword.clone(), cword.clone()].into_iter().filter(|s| !s.is_empty()).collect();
                format!("{} panel", parts.join(" "))
            };
            let zh = match (rzh.as_str(), czh.as_str()) {
                ("", "") => "這一格".to_string(),
                ("", h) => format!("{h}邊那格"),
                ("中", "") => "中間那格".to_string(),
                ("中", h) => format!("中間{h}邊那格"),
                (v, "") => format!("{v}方那格"),
                (v, "中") => format!("{v}方中間那格"),
                (v, h) => format!("{h}{v}那格"),
            };
            slots.push((en, zh));
        }
    }
    Ok((desc, slots))
}

// ================================================================ 排版工作台（2026-10-27，custom.v = 2）
// 使用者：「他的[自訂]功能，能夠更加智能，可以自由堆疊，也更直覺，不要單純按鈕點一點」。
// 一頁＝一刀一刀切出來的樹（tree）＋疊在上面的小格（insets）。設計：docs\superpowers\specs\2026-10-27-comic-free-layout-design.md
// Node = {split:"row"|"col", at, slant, a, b} | {id, brk}；row＝橫的一刀（a 上 b 下），col＝直的一刀（a 左 b 右）。
// 閱讀順序（日本漫畫）：橫刀先讀上面、直刀先讀右邊；疊的小格接在它底下那格後面。網頁 ui\js\comiclayout.js 用同一套規則。

/// 一頁最多幾格（含疊的小格；MOSS 一頁畫得完的範圍）、最多疊幾個小格、樹最深幾層
pub const MAX_PANELS: usize = 8;
pub const MAX_INSETS: usize = 2;
const MAX_DEPTH: usize = 8;

/// 排版工作台的一格
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub id: String,
    /// [x0, y0, x1, y1]，0～1、左上角原點（頁面是直的 2:3）
    pub rect: [f64; 4],
    /// 有一邊是斜的刀
    pub slant: bool,
    /// 人物破格
    pub brk: bool,
    /// 疊在別格上面的小格
    pub inset: bool,
}

// 這一格哪幾邊是斜的刀（位元）
const TOP: u8 = 1;
const BOTTOM: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn clean_id(v: &Value) -> String {
    tx(&v["id"]).chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).take(16).collect()
}

/// custom 是不是排版工作台的格式（不是就是舊的 rows）
pub fn is_free(custom: &Value) -> bool {
    custom["v"].as_i64() == Some(2) || custom.get("tree").is_some_and(Value::is_object)
}

/// 整理樹：刀的位置夾在 0.15～0.85、斜度 ±0.2；格子 id 不重複；room＝還能再切幾刀（每切一刀多一格），切不下去的那塊就留一格
fn clean_tree(node: &Value, depth: usize, room: &mut usize, seen: &mut std::collections::HashSet<String>, next: &mut usize) -> Value {
    let split = tx(&node["split"]);
    if (split == "row" || split == "col") && depth < MAX_DEPTH && *room > 0 {
        *room -= 1;
        let at = node["at"].as_f64().unwrap_or(0.5).clamp(0.15, 0.85);
        let slant = node["slant"].as_f64().unwrap_or(0.0).clamp(-0.2, 0.2);
        let slant = if slant.abs() < 0.01 { 0.0 } else { slant };
        let a = clean_tree(&node["a"], depth + 1, room, seen, next);
        let b = clean_tree(&node["b"], depth + 1, room, seen, next);
        return json!({"split": split, "at": r3(at), "slant": r3(slant), "a": a, "b": b});
    }
    let mut id = clean_id(node);
    while id.is_empty() || seen.contains(&id) {
        *next += 1;
        id = format!("p{next}");
    }
    seen.insert(id.clone());
    json!({"id": id, "brk": node["brk"].as_bool() == Some(true)})
}

/// 疊的小格的方框：0～1、至少 8%×6%、最多 70%；讀不出來就 None
fn inset_rect(v: &Value) -> Option<[f64; 4]> {
    let g = |k: &str| v[k].as_f64().filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 1.0));
    let (x0, y0, x1, y1) = (g("x0")?, g("y0")?, g("x1")?, g("y1")?);
    let r = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    (w >= 0.08 && h >= 0.06 && w <= 0.7 && h <= 0.7).then_some(r)
}

/// 網頁、AI 給的 custom → 乾淨的 v2 格式（壞掉的地方修好、格數超過就少切幾刀；整個讀不出來就是一整格）
pub fn clean_free(custom: &Value) -> Value {
    let rects: Vec<(&Value, [f64; 4])> = custom["insets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| inset_rect(v).map(|r| (v, r)))
        .take(MAX_INSETS)
        .collect();
    let mut room = MAX_PANELS - rects.len() - 1;
    let mut seen = std::collections::HashSet::new();
    let mut next = 0usize;
    let tree = clean_tree(&custom["tree"], 0, &mut room, &mut seen, &mut next);
    let insets: Vec<Value> = rects
        .into_iter()
        .map(|(v, r)| {
            let mut id = clean_id(v);
            while id.is_empty() || seen.contains(&id) {
                next += 1;
                id = format!("p{next}");
            }
            seen.insert(id.clone());
            json!({"id": id, "x0": r3(r[0]), "y0": r3(r[1]), "x1": r3(r[2]), "y1": r3(r[3]), "brk": v["brk"].as_bool() == Some(true)})
        })
        .collect();
    json!({"v": 2, "tree": tree, "insets": insets})
}

fn walk(node: &Value, r: [f64; 4], edges: u8, out: &mut Vec<Cell>) {
    let split = tx(&node["split"]);
    if split == "row" || split == "col" {
        let at = node["at"].as_f64().unwrap_or(0.5);
        let cut = if node["slant"].as_f64().unwrap_or(0.0).abs() >= 0.01 { 1 } else { 0 };
        let [x0, y0, x1, y1] = r;
        if split == "row" {
            let y = y0 + (y1 - y0) * at;
            walk(&node["a"], [x0, y0, x1, y], (edges & !BOTTOM) | (cut * BOTTOM), out);
            walk(&node["b"], [x0, y, x1, y1], (edges & !TOP) | (cut * TOP), out);
        } else {
            let x = x0 + (x1 - x0) * at;
            // 直的一刀：右邊先讀
            walk(&node["b"], [x, y0, x1, y1], (edges & !LEFT) | (cut * LEFT), out);
            walk(&node["a"], [x0, y0, x, y1], (edges & !RIGHT) | (cut * RIGHT), out);
        }
        return;
    }
    out.push(Cell { id: tx(&node["id"]), rect: r, slant: edges != 0, brk: node["brk"].as_bool() == Some(true), inset: false });
}

/// 排版工作台的每一格，照閱讀順序（疊的小格接在它底下那格後面）
pub fn free_cells(custom: &Value) -> Vec<Cell> {
    let c = clean_free(custom);
    let mut out = vec![];
    walk(&c["tree"], [0.0, 0.0, 1.0, 1.0], 0, &mut out);
    for v in c["insets"].as_array().into_iter().flatten() {
        let Some(r) = inset_rect(v) else { continue };
        let (cx, cy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
        let host = out.iter().position(|k| !k.inset && cx >= k.rect[0] && cx <= k.rect[2] && cy >= k.rect[1] && cy <= k.rect[3]);
        let mut at = host.map_or(out.len(), |h| h + 1);
        while at < out.len() && out[at].inset {
            at += 1;
        }
        out.insert(at, Cell { id: tx(&v["id"]), rect: r, slant: false, brk: v["brk"].as_bool() == Some(true), inset: true });
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Vp {
    Top,
    Middle,
    Bottom,
    Full,
}
#[derive(Clone, Copy, PartialEq)]
enum Hp {
    Left,
    Center,
    Right,
    Full,
}

fn vpos(r: &[f64; 4]) -> Vp {
    let cy = (r[1] + r[3]) / 2.0;
    if r[3] - r[1] >= 0.9 {
        Vp::Full
    } else if cy < 0.36 {
        Vp::Top
    } else if cy > 0.64 {
        Vp::Bottom
    } else {
        Vp::Middle
    }
}

fn hpos(r: &[f64; 4]) -> Hp {
    let cx = (r[0] + r[2]) / 2.0;
    if r[2] - r[0] >= 0.9 {
        Hp::Full
    } else if cx < 0.4 {
        Hp::Left
    } else if cx > 0.6 {
        Hp::Right
    } else {
        Hp::Center
    }
}

/// tall／wide／large／small／""（疊的小格一律 small）
fn size_word(c: &Cell) -> &'static str {
    let (w, h) = (c.rect[2] - c.rect[0], c.rect[3] - c.rect[1]);
    if c.inset {
        "small"
    } else if h >= 0.9 && w < 0.75 {
        "tall"
    } else if w >= 0.9 && h < 0.45 {
        "wide"
    } else if w * h >= 0.3 {
        "large"
    } else if w * h <= 0.08 {
        "small"
    } else {
        ""
    }
}

/// 位置的英文：「top right」「middle」「right」…
fn place_en(v: Vp, h: Hp) -> String {
    match (v, h) {
        (Vp::Middle, Hp::Center) => "middle".into(),
        (Vp::Full, Hp::Center) => "center".into(),
        _ => {
            let vs = match v {
                Vp::Top => "top",
                Vp::Middle => "middle",
                Vp::Bottom => "bottom",
                Vp::Full => "",
            };
            let hs = match h {
                Hp::Left => "left",
                Hp::Right => "right",
                _ => "",
            };
            [vs, hs].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
        }
    }
}

fn cell_en(c: &Cell) -> String {
    let (v, h) = (vpos(&c.rect), hpos(&c.rect));
    let slant = if c.slant { "slanted " } else { "" };
    if c.inset {
        let p = place_en(v, h);
        return format!("small {slant}inset panel at the {}", if p.is_empty() { "center".into() } else { p });
    }
    if v == Vp::Full && h == Hp::Full {
        return "full-page panel".into();
    }
    let words: Vec<String> = [slant.trim().to_string(), size_word(c).to_string(), place_en(v, h), "panel".into()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    words.join(" ")
}

fn cell_zh(c: &Cell) -> String {
    let (v, h) = (vpos(&c.rect), hpos(&c.rect));
    if !c.inset && v == Vp::Full && h == Hp::Full {
        return "整頁那格".into();
    }
    let place = match (v, h) {
        (Vp::Full, Hp::Left) => "左邊",
        (Vp::Full, Hp::Right) => "右邊",
        (Vp::Full, _) => "中間",
        (Vp::Top, Hp::Full) => "最上面",
        (Vp::Bottom, Hp::Full) => "最下面",
        (Vp::Middle, Hp::Full) => "中間",
        (Vp::Top, Hp::Left) => "左上",
        (Vp::Top, Hp::Right) => "右上",
        (Vp::Bottom, Hp::Left) => "左下",
        (Vp::Bottom, Hp::Right) => "右下",
        (Vp::Middle, Hp::Left) => "中間左邊",
        (Vp::Middle, Hp::Right) => "中間右邊",
        (Vp::Top, Hp::Center) => "上方中間",
        (Vp::Bottom, Hp::Center) => "下方中間",
        (Vp::Middle, Hp::Center) => "中間",
    };
    let slant = if c.slant { "斜切的" } else { "" };
    if c.inset {
        return format!("{place}{slant}疊的小格");
    }
    match size_word(c) {
        "tall" => format!("{place}{slant}直長那格"),
        "wide" => format!("{place}{slant}橫長那格"),
        "large" => format!("{place}{slant}大格"),
        "small" => format!("{place}{slant}小格"),
        _ => format!("{place}{slant}那格"),
    }
}

/// 整頁描述裡的一格（說法跟預設版型實測過的一樣：「a tall panel on the right that spans the full height of the page」）
fn cell_phrase(c: &Cell, name: &str, host: Option<&str>) -> String {
    let (v, h) = (vpos(&c.rect), hpos(&c.rect));
    let slant = if c.slant { "slanted " } else { "" };
    if c.inset {
        return match host {
            Some(hn) => format!("a {name} overlapping the {hn}"),
            None => format!("a {name}"),
        };
    }
    match (v, h, size_word(c)) {
        (Vp::Full, Hp::Full, _) => "one full-page panel".into(),
        (Vp::Full, _, "tall") => {
            let side = match h {
                Hp::Left => "on the left",
                Hp::Right => "on the right",
                _ => "in the middle",
            };
            format!("a {slant}tall panel {side} that spans the full height of the page")
        }
        (_, Hp::Full, "wide") => {
            let at = match v {
                Vp::Top => "the top",
                Vp::Bottom => "the bottom",
                _ => "the middle of the page",
            };
            format!("a {slant}wide panel across {at}")
        }
        _ => format!("a {name}"),
    }
}

const NUM_EN: [&str; 9] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight"];
const ORD_EN: [&str; 9] = ["", "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth"];

/// 排版工作台的版面 → (英文描述, 每一格的 (英文名, 中文名))；同名的格加序數（英文）或「第幾格」（中文）
pub fn free_layout(custom: &Value) -> (String, Vec<(String, String)>) {
    let cells = free_cells(custom);
    let en: Vec<String> = cells.iter().map(cell_en).collect();
    let zh: Vec<String> = cells.iter().map(cell_zh).collect();
    let mut slots = Vec::with_capacity(cells.len());
    for i in 0..cells.len() {
        let same_en: Vec<usize> = (0..cells.len()).filter(|j| en[*j] == en[i]).collect();
        let e = if same_en.len() > 1 {
            let k = same_en.iter().position(|j| *j == i).unwrap_or(0) + 1;
            format!("{} {}", ORD_EN[k.min(8)], en[i])
        } else {
            en[i].clone()
        };
        let z = if zh.iter().filter(|s| **s == zh[i]).count() > 1 { format!("{}（第{}格）", zh[i], i + 1) } else { zh[i].clone() };
        slots.push((e, z));
    }
    let phrases: Vec<String> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            // 疊的小格：它底下那格是前面最近的一般格
            let host = c.inset.then(|| (0..i).rev().find(|j| !cells[*j].inset).map(|j| slots[j].0.as_str())).flatten();
            cell_phrase(c, &slots[i].0, host)
        })
        .collect();
    let n = cells.len();
    let desc = match n {
        1 => phrases[0].clone(),
        _ => {
            let (last, rest) = phrases.split_last().expect("n >= 2");
            format!("{} panels: {}, and {last}", NUM_EN[n.min(8)], rest.join(", "))
        }
    };
    (desc, slots)
}

/// 每一格要不要人物破格（照閱讀順序；不是排版工作台的版面就是空的）
pub fn breaks(p: &Value) -> Vec<bool> {
    if tx(&p["layout"]) == "custom" && is_free(&p["custom"]) {
        free_cells(&p["custom"]).iter().map(|c| c.brk).collect()
    } else {
        vec![]
    }
}

// ================================================================ 用說的排版（站內助手幫忙拉格子）
/// 給本機語言模型的系統提示：一句話（或這頁的劇情）→ 排版工作台的樹。回傳一定要是 JSON
pub const SYSTEM_LAYOUT: &str = r#"You design the panel layout of ONE vertical manga page (portrait, 2:3). Return JSON only. No prose, no code fences.

Describe the page as nested lists:
{"rows": [ ... ]}   parts stacked from TOP to BOTTOM
{"cols": [ ... ]}   parts side by side from LEFT to RIGHT
Each part is a panel {"id":"p1"} or another {"rows":[...]} / {"cols":[...]}.
Any part may have "size": a relative number (default 1). A bigger panel gets a bigger size.
Optional on a panel: "brk": true = the character bursts out of the panel border; "slanted": true = the cut next to it is slanted.
Small panels laid on top of a bigger panel go in "insets": [{"id":"p7","x0":0.62,"y0":0.72,"x1":0.95,"y1":0.95}] (0 to 1, origin top-left), at most 2.

Output: {"plan": "<one short line: what goes where>", "page": {"rows": [...]} or {"cols": [...]}, "insets": [...]}
Write "plan" first, then build "page" and "insets" to match the plan exactly.

Rules:
- Follow the request exactly. When it says how many panels go somewhere, list exactly that many there; count them before answering.
- The current layout is only a reference. When the request describes a layout, build that layout and ignore the current panel count.
- 1 to 8 panels in total, insets included. Use 4 to 6 when the request does not say.
- Panel ids are p1, p2, p3 ... and unique. At most 3 levels of nesting.
- The page reads right to left, top to bottom: the first story beat is the top right panel. Give the most important moment the biggest size.
- Add insets only when the request asks for a small panel laid on top of another (疊、小格、inset). Otherwise "insets" is [].
- Slanted (斜、斜切): "slanted": true on that panel. Breaking out of the frame (破格、衝出格子): "brk": true on that panel.

Examples (request, then the JSON):
左邊一格大特寫貫穿整頁，右邊切三格，右下疊一個小格
{"plan":"cols: left 1 tall | right 3 stacked; small inset at the bottom right","page":{"cols":[{"id":"p4","size":1.4},{"rows":[{"id":"p1"},{"id":"p2"},{"id":"p3"}],"size":2}]},"insets":[{"id":"p5","x0":0.7,"y0":0.8,"x1":0.97,"y1":0.97}]}
上面一格橫長，下面左右兩格，最後一格斜切而且人物破格
{"plan":"rows: top 1 wide / bottom 2 side by side, the left one is read last, slanted and breaking out","page":{"rows":[{"id":"p1"},{"cols":[{"id":"p3","brk":true,"slanted":true},{"id":"p2"}],"size":2}]},"insets":[]}
三排：上排兩格、中排一格大的、下排三格
{"plan":"rows: 2 / 1 big / 3","page":{"rows":[{"cols":[{"id":"p2"},{"id":"p1"}]},{"id":"p3","size":1.6},{"cols":[{"id":"p6"},{"id":"p5"},{"id":"p4"}]}]},"insets":[]}"#;

/// 用說的排版的訊息：(系統, 使用者)。ask＝使用者想怎麼排（中文可以；空白就照劇情自己排）
pub fn layout_messages(p: &Value, ask: &str) -> (String, String) {
    let story = field(p, "script");
    let now = layout_of(p).map(|(d, _)| d).unwrap_or_default();
    let or_free = |s: String| if s.trim().is_empty() { "(none)".to_string() } else { s };
    let ask = if ask.trim().is_empty() {
        "Design a layout that fits the story below.".to_string()
    } else {
        ask.trim().to_string()
    };
    let user = format!(
        "Request (may be written in Chinese): {ask}\n\nStory of this page (decides which moment gets the biggest panel): {}\n\nCurrent layout: {}\n\nReturn the JSON now.",
        or_free(story),
        or_free(now),
    );
    (SYSTEM_LAYOUT.to_string(), user)
}

/// 模型輸出 → 乾淨的 v2 版面；讀不出版面就 None（呼叫的人重試）。
/// 模型寫巢狀列表（`page: {rows|cols: [...]}`，小模型數格子比較準）；也收排版工作台的樹（`tree`）
pub fn parse_layout(text: &str) -> Option<Value> {
    use crate::director::json_in;
    let m = Value::Object(json_in(text)?);
    let obj = if m["custom"].is_object() { &m["custom"] } else { &m };
    if obj["tree"].is_object() {
        return Some(clean_free(obj));
    }
    let page = if obj["page"].is_object() { &obj["page"] } else { obj };
    if !(page["rows"].is_array() || page["cols"].is_array()) {
        return None;
    }
    let insets: Vec<Value> = obj["insets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let mut v = v.clone();
            if v["id"].is_null() {
                v["id"] = v["panel"].clone();
            }
            v
        })
        .collect();
    Some(clean_free(&json!({"tree": nest_tree(page, 0), "insets": insets})))
}

/// 巢狀列表的一塊 → 樹：rows＝由上到下（橫刀），cols＝由左到右（直刀）；size 是比例
fn nest_tree(n: &Value, depth: usize) -> Value {
    let (split, list) = match (n["rows"].as_array(), n["cols"].as_array()) {
        (Some(l), _) => ("row", l),
        (None, Some(l)) => ("col", l),
        _ => {
            let id = if n["id"].is_null() { &n["panel"] } else { &n["id"] };
            return json!({"id": tx(id), "brk": n["brk"].as_bool() == Some(true)});
        }
    };
    let items: Vec<&Value> = list.iter().filter(|v| v.is_object()).collect();
    match items.len() {
        0 => json!({"id": ""}),
        _ if depth >= MAX_DEPTH => nest_tree(items[0], depth + 1),
        _ => nest_chain(split, &items, depth, true),
    }
}

/// 一排 n 塊 → 一刀切下第一塊、剩下的再切；「slanted」的那塊旁邊那一刀斜（第一塊是它後面那刀）
fn nest_chain(split: &str, items: &[&Value], depth: usize, first: bool) -> Value {
    if items.len() == 1 {
        return nest_tree(items[0], depth + 1);
    }
    let size = |v: &Value| v["size"].as_f64().filter(|x| x.is_finite() && *x > 0.0).unwrap_or(1.0).min(20.0);
    let total: f64 = items.iter().map(|v| size(v)).sum();
    let at = (size(items[0]) / total).clamp(0.15, 0.85);
    let slanted = |v: &Value| v["slanted"].as_bool() == Some(true);
    let slant = if slanted(items[1]) || (first && slanted(items[0])) { 0.1 } else { 0.0 };
    json!({"split": split, "at": r3(at), "slant": slant,
           "a": nest_tree(items[0], depth + 1), "b": nest_chain(split, &items[1..], depth + 1, false)})
}

fn field(v: &Value, k: &str) -> String {
    tx(&v[k]).trim().to_string()
}

/// 句尾補句點（使用者寫的內容常常沒有）
fn sentence(s: &str) -> String {
    let s = s.trim().trim_end_matches([',', '，']);
    if s.ends_with(['.', '!', '?', '。', '！', '？']) {
        s.to_string()
    } else {
        format!("{s}.")
    }
}

/// 一格有沒有內容（景別或內容寫了任何一個）
fn has_scene(c: &Value) -> bool {
    !field(c, "what").is_empty()
}

// ================================================================ 角色卡（2026-11-16）
/// 角色卡最多幾張（一頁 MOSS 畫得清楚的人數；Qwen 修角色時每格最多也只帶這麼多人的參考圖）
pub const MAX_CHARS: usize = 4;

/// 這頁的角色卡：[{asset, name, text, trigger, refs:[資產檔名], loras:[…]}]。名字是空的不算
pub fn chars(p: &Value) -> Vec<Value> {
    p["chars"].as_array().into_iter().flatten().filter(|c| !field(c, "name").is_empty()).take(MAX_CHARS).cloned().collect()
}

/// 角色卡的名字（排分鏡的 who、修角色對格子都用它）
pub fn char_names(p: &Value) -> Vec<String> {
    chars(p).iter().map(|c| field(c, "name")).collect()
}

/// 一張角色卡 → 一句外貌：「Lin (lin_v1): A tall woman …」。觸發詞放名字後面，跟外貌一起出現 LoRA 比較吃得到
fn char_sentence(c: &Value) -> String {
    let (name, text, trig) = (field(c, "name"), field(c, "text"), field(c, "trigger"));
    let head = if trig.is_empty() { name } else { format!("{name} ({})", trig.trim_end_matches([',', '.'])) };
    if text.is_empty() {
        sentence(&head)
    } else {
        sentence(&format!("{head}: {}", text.trim_end_matches([',', '.'])))
    }
}

/// 整頁的角色外貌：角色卡一張一句（有名字，分格內容可以直接用名字指人），再接自由寫的外貌欄
pub fn cast_text(p: &Value) -> String {
    let mut out: Vec<String> = chars(p).iter().map(char_sentence).collect();
    let cast = field(p, "cast");
    if !cast.is_empty() {
        out.push(sentence(&cast));
    }
    out.join(" ")
}

/// 一格裡有誰（只留這頁角色卡上有的名字）
pub fn panel_who(c: &Value, names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for w in c["who"].as_array().into_iter().flatten() {
        let w = tx(w).trim().to_string();
        if let Some(n) = names.iter().find(|n| n.eq_ignore_ascii_case(&w)) {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
    }
    out
}

/// MOSS-Image 一次畫完整頁＋字。cast＝角色外貌（角色卡一張一句＋自由寫的外貌）
pub fn moss_page(p: &Value) -> Result<String, String> {
    let (desc, slots) = layout_of(p)?;
    let list = p["panels"].as_array().cloned().unwrap_or_default();
    let brk = breaks(p);
    let mut sent = vec![format!(
        "The image is a vertical full-color anime manga page divided into {desc}, separated by thin black borders with white gutters, read from right to left."
    )];
    let cast = cast_text(p);
    if !cast.is_empty() {
        sent.push(cast);
    }
    for (i, (slot, _)) in slots.iter().enumerate() {
        let c = list.get(i).unwrap_or(&Value::Null);
        if !has_scene(c) {
            continue;
        }
        let shot = field(c, "shot");
        let shot = if shot.is_empty() { "a medium shot".to_string() } else { shot };
        let mut s = format!("The {slot} shows {shot} of {}", field(c, "what").trim_end_matches(['.', ',']));
        let line = field(c, "line");
        if !line.is_empty() {
            s.push_str(&format!(
                "; a white oval speech bubble at the {} of the panel contains the vertical black Traditional Chinese text \"{}\"",
                bubble(&field(c, "bubble")).0,
                line.replace('"', "\u{201D}")
            ));
        }
        let sfx = field(c, "sfx");
        if !sfx.is_empty() {
            s.push_str(&format!(
                "; a large pink hand-lettered Japanese sound effect \"{}\" floats nearby with small pink hearts",
                sfx.replace('"', "\u{201D}")
            ));
        }
        if brk.get(i) == Some(&true) {
            s.push_str("; the character bursts out of the panel border, overlapping the gutter");
        }
        sent.push(sentence(&s));
    }
    let light = field(p, "light");
    if !light.is_empty() {
        sent.push(sentence(&light));
    }
    let mood = field(p, "mood");
    let mood = if mood.is_empty() { "lively".to_string() } else { mood };
    sent.push(format!("The page has a {mood} mood with saturated colors and clean anime line art."));
    let body = sent.join(" ");
    let trig = field(p, "trigger");
    Ok(if trig.is_empty() { body } else { format!("{}. {body}", trig.trim_end_matches(['.', ','])) })
}

/// 寫好的格數（版型範圍內、有內容的格；切到格數少的版型時，多出來的格會留著但不算）
pub fn filled_panels(p: &Value) -> usize {
    let Ok((_, slots)) = layout_of(p) else { return 0 };
    let list = p["panels"].as_array().cloned().unwrap_or_default();
    (0..slots.len()).filter(|i| list.get(*i).is_some_and(has_scene)).count()
}

// ================================================================ AI 排分鏡（35B）
/// 給 35B 的系統提示：把一頁的劇情拆成每一格。回傳一定要是 JSON，欄位跟手寫的一模一樣。
pub const SYSTEM_SCRIPT: &str = r#"You lay out ONE page of a manga for an AI image model (MOSS-Image) that draws the entire page, panels and lettering included, in a single image. You are given the panel layout, the fixed character descriptions, the lighting, the mood, and the story this page must tell. Split it into panels, in reading order, and write what each panel shows.

Return JSON only. No prose, no explanation, no code fences.

{"panels":[{"shot":"a medium shot","what":"...","who":[],"line":"...","bubble":"top","sfx":""}]}

Rules:
- Exactly {N} panel objects, in reading order. Use every panel; never merge, skip or add panels.
- "shot" must be one of: a wide shot, a full-body view, a cowboy shot, a medium shot, a close-up, an extreme close-up, a low-angle view, a high-angle view, a first-person point-of-view shot, a side view, a dynamic wide shot.
- "what" is ONE short English phrase describing who is where, what they are doing and how the picture is composed. Visible physical things only. Never repeat hair, clothing or body details already listed in the character descriptions. Never write text, signs, captions, speech or letters in this field. When named characters are given, call them by their exact names in this field.
- "who" lists the exact names of the named characters visible in the panel, [] when none of them appears.
- "line" is the spoken dialogue inside the speech bubble, in Traditional Chinese (Taiwan usage), 8 characters or fewer when the panel is small. Use "" when nobody speaks.
- "bubble" is where the bubble sits: top left, top right, top, left side, right side, or bottom.
- "sfx" is a short Japanese onomatopoeia drawn as a large hand-lettered sound effect, or "".
- Every panel is a different moment or a clearly different angle. Do not repeat the same composition.
- Keep the art style consistent across the page and match the requested lighting and mood.
- The page reads right to left, so the earliest story beats go in the rightmost/topmost panels."#;

/// 排分鏡的訊息：(系統, 使用者)。story＝使用者寫的這段劇情（中文可以）
pub fn script_messages(p: &Value, story: &str) -> (String, String) {
    let (desc, slots) = layout_of(p).unwrap_or_else(|_| {
        let l = &LAYOUTS[1];
        (l.2.to_string(), l.3.iter().map(|(en, zh)| (en.to_string(), zh.to_string())).collect())
    });
    let n = slots.len();
    let brk = breaks(p);
    let order: Vec<String> = slots
        .iter()
        .enumerate()
        .map(|(i, (en, zh))| {
            let tail = if brk.get(i) == Some(&true) { " (the character breaks out of the frame: give it the climax)" } else { "" };
            format!("{}. {} / {}{tail}", i + 1, en, zh)
        })
        .collect();
    let or_free = |s: String| if s.trim().is_empty() { "(free choice)".to_string() } else { s };
    let names = char_names(p);
    let named = if names.is_empty() { "(none)".to_string() } else { names.join(", ") };
    let user = format!(
        "Page layout: {desc}.\nPanel order (reading order, right to left):\n{}\n\nNamed characters: {named}\nFixed character descriptions: {}\nLighting: {}\nMood: {}\nStyle trigger words: {}\n\nStory for this page:\n{}\n\nExactly {n} panels. Return the JSON now.",
        order.join("\n"),
        or_free(cast_text(p)),
        or_free(field(p, "light")),
        or_free(field(p, "mood")),
        or_free(field(p, "trigger")),
        story.trim(),
    );
    (SYSTEM_SCRIPT.replace("{N}", &n.to_string()), user)
}

/// 模型吐的 shot 對回選單裡有的那幾個（對不到就半身）
fn snap_shot(s: &str) -> &'static str {
    let s = s.to_lowercase();
    let has = |k: &str| s.contains(k);
    if has("extreme close") || has("大特寫") {
        "an extreme close-up"
    } else if has("close") || has("特寫") {
        "a close-up"
    } else if has("cowboy") || has("七分") {
        "a cowboy shot"
    } else if has("full") || has("全身") {
        "a full-body view"
    } else if has("first-person") || has("point-of-view") || has("pov") || has("第一人稱") {
        "a first-person point-of-view shot"
    } else if has("low-angle") || has("low angle") || has("仰角") {
        "a low-angle view"
    } else if has("high-angle") || has("high angle") || has("俯角") {
        "a high-angle view"
    } else if has("dynamic") || has("動態") {
        "a dynamic wide shot"
    } else if has("side") || has("側面") {
        "a side view"
    } else if has("wide") || has("遠景") {
        "a wide shot"
    } else {
        "a medium shot"
    }
}

/// 模型吐的對話框位置對回選單裡有的那幾個
fn snap_bubble(s: &str) -> &'static str {
    let s = s.to_lowercase();
    let has = |k: &str| s.contains(k);
    if has("top left") || has("upper left") {
        "top left"
    } else if has("top right") || has("upper right") {
        "top right"
    } else if has("bottom") || has("下") {
        "bottom"
    } else if has("left") || has("左") {
        "left side"
    } else if has("right") || has("右") {
        "right side"
    } else {
        "top"
    }
}

fn blank_panel() -> Value {
    json!({"shot": "a medium shot", "what": "", "who": [], "line": "", "bubble": "top", "sfx": ""})
}

/// 模型輸出 → 剛好 n 格（多出來的不理、不夠的補空白）。一格的內容都沒有＝格式壞掉，回 None 讓呼叫的人重試。
pub fn parse_script(text: &str, n: usize) -> Option<Vec<Value>> {
    use crate::director::json_in;
    let m = json_in(text).or_else(|| {
        // 偶爾直接吐陣列：包一層再讀
        let i = text.find('[')?;
        json_in(&format!("{{\"panels\":{}}}", &text[i..]))
    })?;
    let arr = m.get("panels").and_then(Value::as_array)?;
    let get = |c: &Value, keys: &[&str]| -> String {
        keys.iter().find_map(|k| c.get(*k)).map(|v| tx(v).trim().to_string()).unwrap_or_default()
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let c = arr.get(i);
        let Some(c) = c else {
            out.push(blank_panel());
            continue;
        };
        let what = get(c, &["what", "content", "description", "scene", "action"]);
        // who：名字清單（偶爾吐成一個字串「Lin, Ken」）；對不對得上角色卡由呼叫的人篩（panel_who）
        let who: Vec<String> = match c.get("who").or_else(|| c.get("characters")) {
            Some(Value::Array(a)) => a.iter().map(|v| tx(v).trim().to_string()).filter(|s| !s.is_empty()).collect(),
            Some(v) => tx(v).split([',', '、', '，']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            None => vec![],
        };
        out.push(json!({
            "shot": snap_shot(&get(c, &["shot", "framing"])),
            "what": what,
            "who": who,
            "line": get(c, &["line", "dialogue", "speech"]),
            "bubble": snap_bubble(&get(c, &["bubble", "bubble_at", "position"])),
            "sfx": get(c, &["sfx", "sound", "onomatopoeia"]),
        }));
    }
    // 至少要有兩格真的有內容，不然就是模型沒照格式寫
    (out.iter().filter(|c| has_scene(c)).count() >= 2 || (n < 2 && out.first().is_some_and(has_scene))).then_some(out)
}

// ================================================================ 存檔（跟導演台同一套寫法）
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(data: &Path) -> Store {
        Store { dir: data.join("comic") }
    }
    fn file(&self, id: &str) -> PathBuf {
        self.dir.join("projects").join(format!("{id}.json"))
    }
    pub fn projects(&self) -> Vec<Value> {
        let mut out: Vec<Value> = std::fs::read_dir(self.dir.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
            .map(|p| json!({"id": p["id"], "title": p["title"], "updated": p["updated"], "layout": p["layout"]}))
            .collect();
        out.sort_by(|a, b| b["updated"].as_f64().unwrap_or(0.0).total_cmp(&a["updated"].as_f64().unwrap_or(0.0)));
        out
    }
    pub fn project(&self, id: &str) -> Option<Value> {
        if !valid_id(id) {
            return None;
        }
        serde_json::from_slice(&std::fs::read(self.file(id)).ok()?).ok()
    }
    pub fn save(&self, p: &mut Value) -> std::io::Result<()> {
        let id = tx(&p["id"]);
        if !valid_id(&id) {
            return Err(std::io::Error::other("作品編號不對"));
        }
        p["updated"] = json!(now());
        write_atomic(&self.file(&id), p)
    }
    /// 不真的刪：搬到 data\待刪除\漫畫台\
    pub fn trash(&self, id: &str, trash: &Path) -> std::io::Result<()> {
        if !valid_id(id) {
            return Err(std::io::Error::other("作品編號不對"));
        }
        let to = trash.join("漫畫台");
        std::fs::create_dir_all(&to)?;
        std::fs::rename(self.file(id), to.join(format!("{id}_{}.json", chrono::Local::now().format("%Y%m%d_%H%M%S"))))
    }
}

/// 新作品的預設值（四格）
pub fn blank(title: &str) -> Value {
    let panel = || json!({"shot": "a medium shot", "what": "", "who": [], "line": "", "bubble": "top", "sfx": ""});
    json!({
        "id": new_id(), "title": if title.trim().is_empty() { "未命名" } else { title.trim() }, "created": now(), "updated": now(),
        "layout": "L4", "trigger": "", "cast": "", "light": "", "mood": "", "script": "",
        "panels": [panel(), panel(), panel(), panel()], "chars": [], "loras": [], "image": null, "count": 4, "runs": [],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({"layout": "L3", "trigger": "jgdxstyle", "light": "Purple candlelight", "mood": "hot",
               "panels": [
                   {"shot": "a full-body view", "what": "the succubus riding the man", "line": "你的精氣，我全都要～", "bubble": "top left"},
                   {"shot": "a close-up", "what": "the man gritting his teeth.", "line": "等、等一下……！", "bubble": "top right"},
                   {"shot": "an extreme close-up", "what": "their joined hips", "line": "", "sfx": "ズチュッ"}]})
    }

    #[test]
    fn hidden_and_sceneless_panels_are_skipped() {
        // 版型 L3 只看前三格：第四格有內容也不算
        let mut p = json!({"layout": "L3", "panels": [{"what": ""}, {"what": ""}, {"what": ""}, {"what": "a girl", "line": "嗨"}]});
        assert_eq!(filled_panels(&p), 0);
        p["layout"] = json!("L4");
        assert_eq!(filled_panels(&p), 1);
        // 只有台詞、沒寫內容的格：不畫，台詞也不進提示詞
        p["panels"][0] = json!({"what": "", "line": "看不到我"});
        p["panels"][3]["line"] = json!("她說「好」");
        let s = moss_page(&p).unwrap();
        assert!(!s.contains("看不到我"));
        assert!(s.contains("The small slanted inset panel at the bottom right shows a medium shot of a girl;"));
        assert!(s.contains("contains the vertical black Traditional Chinese text \"她說「好」\""));
    }

    #[test]
    fn moss_page_and_missing_panels() {
        let mut p = sample();
        p["layout"] = json!("K4"); // 四格只給三格：第四格空白不寫
        p["cast"] = json!("A pink-haired succubus");
        let s = moss_page(&p).unwrap();
        assert!(s.starts_with("jgdxstyle. The image is a vertical full-color anime manga page divided into four horizontal panels"));
        assert!(s.contains("A pink-haired succubus. The first panel at the top shows a full-body view of the succubus riding the man; a white oval speech bubble at the top left of the panel contains the vertical black Traditional Chinese text \"你的精氣，我全都要～\"."));
        assert!(s.contains("Japanese sound effect \"ズチュッ\""));
        assert!(!s.contains("fourth panel"));
        assert!(s.ends_with("The page has a hot mood with saturated colors and clean anime line art."));
        assert!(moss_page(&json!({"layout": "L9"})).is_err());
    }

    #[test]
    fn every_layout_and_option_is_consistent() {
        for (id, zh, desc, slots) in LAYOUTS {
            assert!(!zh.is_empty() && desc.contains("panel"), "{id}");
            assert!((3..=6).contains(&slots.len()), "{id}");
        }
        assert_eq!(blank("x")["panels"].as_array().unwrap().len(), layout("L4").unwrap().3.len());
        assert!(valid_id(&tx(&blank("")["id"])));
    }

    // ---------------------------------------------------------------- 排版工作台（v2）
    fn free(tree: Value, insets: Value) -> Value {
        json!({"layout": "custom", "custom": {"v": 2, "tree": tree, "insets": insets}})
    }

    #[test]
    fn free_tall_left_and_three_right_read_right_to_left() {
        // 左邊一格貫穿整頁，右邊切三格
        let p = free(
            json!({"split": "col", "at": 0.42, "a": {"id": "big"},
                   "b": {"split": "row", "at": 0.33, "a": {"id": "r1"}, "b": {"split": "row", "at": 0.5, "a": {"id": "r2"}, "b": {"id": "r3"}}}}),
            json!([]),
        );
        let ids: Vec<String> = free_cells(&p["custom"]).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["r1", "r2", "r3", "big"], "右邊先讀、上面先讀");
        let (desc, slots) = layout_of(&p).unwrap();
        assert!(desc.starts_with("four panels: a top right panel, a middle right panel, a bottom right panel, and "), "{desc}");
        assert!(desc.ends_with("a tall panel on the left that spans the full height of the page"), "{desc}");
        assert_eq!(slots[3], ("tall left panel".to_string(), "左邊直長那格".to_string()));
        assert_eq!(slots[0], ("top right panel".to_string(), "右上那格".to_string()));
    }

    #[test]
    fn free_inset_sits_after_its_host_and_is_named() {
        let p = free(
            json!({"split": "row", "at": 0.3, "a": {"id": "top"}, "b": {"id": "main"}}),
            json!([{"id": "ins", "x0": 0.62, "y0": 0.72, "x1": 0.95, "y1": 0.95}]),
        );
        let ids: Vec<String> = free_cells(&p["custom"]).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["top", "main", "ins"]);
        let (desc, slots) = layout_of(&p).unwrap();
        assert_eq!(slots[0].0, "wide top panel");
        assert_eq!(slots[1].0, "large bottom panel");
        assert_eq!(slots[2], ("small inset panel at the bottom right".to_string(), "右下疊的小格".to_string()));
        assert_eq!(
            desc,
            "three panels: a wide panel across the top, a large bottom panel, and a small inset panel at the bottom right overlapping the large bottom panel"
        );
    }

    #[test]
    fn free_slant_and_break_reach_the_prompt() {
        let mut p = free(json!({"split": "row", "at": 0.5, "slant": 0.1, "a": {"id": "a"}, "b": {"id": "b", "brk": true}}), json!([]));
        p["panels"] = json!([{"shot": "a close-up", "what": "her face"}, {"shot": "a wide shot", "what": "she jumps"}]);
        let (_, slots) = layout_of(&p).unwrap();
        assert_eq!(slots[0].0, "slanted large top panel");
        assert_eq!(slots[1].1, "最下面斜切的大格");
        assert_eq!(breaks(&p), vec![false, true]);
        let s = moss_page(&p).unwrap();
        assert!(
            s.contains("The slanted large bottom panel shows a wide shot of she jumps; the character bursts out of the panel border"),
            "{s}"
        );
        let (_, user) = script_messages(&p, "她跳起來");
        assert!(user.contains("2. slanted large bottom panel / 最下面斜切的大格 (the character breaks out of the frame"), "{user}");
        // 只有被斜刀碰到的那幾格是斜的
        let q = free(
            json!({"split": "row", "at": 0.5, "a": {"split": "row", "at": 0.5, "slant": 0.1, "a": {"id": "t1"}, "b": {"id": "t2"}}, "b": {"id": "low"}}),
            json!([]),
        );
        let sl: Vec<bool> = free_cells(&q["custom"]).iter().map(|c| c.slant).collect();
        assert_eq!(sl, vec![true, true, false]);
    }

    #[test]
    fn free_duplicate_names_get_ordinals() {
        // 上下兩排各兩格，再把右上切成上下兩小格 → 兩個「top right」
        let p = free(
            json!({"split": "row", "at": 0.6, "a": {"split": "col", "at": 0.5, "a": {"id": "l"},
                   "b": {"split": "row", "at": 0.5, "a": {"id": "r1"}, "b": {"id": "r2"}}}, "b": {"id": "low"}}),
            json!([]),
        );
        let (_, slots) = layout_of(&p).unwrap();
        let en: Vec<&str> = slots.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(en.iter().collect::<std::collections::HashSet<_>>().len(), en.len(), "英文名不重複：{en:?}");
        let zh: Vec<&str> = slots.iter().map(|s| s.1.as_str()).collect();
        assert_eq!(zh.iter().collect::<std::collections::HashSet<_>>().len(), zh.len(), "中文名不重複：{zh:?}");
    }

    #[test]
    fn free_layout_is_cleaned_and_capped() {
        // 亂寫：位置超出範圍、id 重複、切太多刀、小格太小
        let mut deep = json!({"id": "x"});
        for _ in 0..12 {
            deep = json!({"split": "row", "at": 2.0, "a": {"id": "x"}, "b": deep});
        }
        let c = clean_free(&json!({"tree": deep, "insets": [{"x0": 0.1, "y0": 0.1, "x1": 0.12, "y1": 0.11}, {"x0": 0.5, "y0": 0.5, "x1": 0.8, "y1": 0.8}]}));
        let cells = free_cells(&c);
        assert!(cells.len() <= MAX_PANELS, "{}", cells.len());
        assert_eq!(cells.len(), MAX_PANELS, "切到滿：7 格＋1 個小格");
        assert_eq!(cells.iter().filter(|k| k.inset).count(), 1, "太小的小格丟掉");
        let mut ids: Vec<&str> = cells.iter().map(|k| k.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), cells.len(), "id 不重複");
        assert_eq!(c["tree"]["at"], 0.85, "位置夾在範圍內");
        // 整個讀不出來也至少一格
        assert_eq!(free_cells(&json!({"v": 2})).len(), 1);
        let (desc, slots) = layout_of(&free(json!(null), json!(null))).unwrap();
        assert_eq!((desc.as_str(), slots.len()), ("one full-page panel", 1));
        assert_eq!(slots[0], ("full-page panel".to_string(), "整頁那格".to_string()));
    }

    #[test]
    fn old_rows_still_work_and_parse_layout_reads_model_output() {
        let old = custom(json!([{"h": 1, "cells": [1, 1]}]));
        assert!(!is_free(&old["custom"]));
        assert_eq!(layout_of(&old).unwrap().1.len(), 2);
        assert!(breaks(&old).is_empty());
        let txt = "好的：\n{\"tree\":{\"split\":\"col\",\"at\":0.5,\"a\":{\"id\":\"p2\"},\"b\":{\"id\":\"p1\"}},\"insets\":[]}";
        let v = parse_layout(txt).unwrap();
        assert_eq!(v["v"], 2);
        let ids: Vec<String> = free_cells(&v).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["p1", "p2"]);
        let wrapped = parse_layout("{\"custom\":{\"tree\":{\"id\":\"solo\"}}}").unwrap();
        assert_eq!(free_cells(&wrapped)[0].id, "solo");
        assert!(parse_layout("沒有 JSON").is_none());
        assert!(parse_layout("{\"panels\":[]}").is_none());
        let (sys, user) = layout_messages(&json!({"layout": "L3", "script": "她衝過來"}), "左邊一格大特寫");
        assert!(sys.contains("{\"rows\": [ ... ]}"), "{sys}");
        assert!(user.contains("左邊一格大特寫") && user.contains("她衝過來") && user.contains("tall panel on the right"), "{user}");
    }

    #[test]
    fn parse_layout_reads_nested_rows_and_cols() {
        // 模型寫巢狀列表：左邊一格直長、右邊三格（第二格斜、第三格破格）、右下疊小格
        let txt = r#"{"plan":"x","page":{"cols":[{"id":"big","size":1.4},{"rows":[{"id":"r1"},{"id":"r2","slanted":true},{"id":"r3","brk":true}],"size":2}]},"insets":[{"panel":"s","x0":0.7,"y0":0.8,"x1":0.97,"y1":0.97}]}"#;
        let v = parse_layout(txt).unwrap();
        let cells = free_cells(&v);
        let ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["r1", "r2", "r3", "s", "big"], "右邊先讀，小格接在右下那格後面");
        assert_eq!(v["tree"]["split"], "col");
        assert_eq!(v["tree"]["at"], 0.412, "size 1.4：2");
        assert_eq!(v["tree"]["b"]["slant"], 0.1, "r2 前面那一刀斜");
        assert_eq!(v["tree"]["b"]["b"]["slant"], 0.0);
        assert!(cells[2].brk && !cells[0].brk);
        // 一排四格一樣大：每一刀都切在剩下那段的等分上
        let four = parse_layout(r#"{"page":{"rows":[{"id":"a"},{"id":"b"},{"id":"c"},{"id":"d"}]}}"#).unwrap();
        let ys: Vec<f64> = free_cells(&four).iter().map(|c| (c.rect[1] * 100.0).round()).collect();
        assert_eq!(ys, [0.0, 25.0, 50.0, 75.0]);
        // 怪東西也不會當掉：空的、只有一格、太深、超過 8 格
        assert_eq!(free_cells(&parse_layout(r#"{"page":{"rows":[]}}"#).unwrap()).len(), 1);
        assert_eq!(free_cells(&parse_layout(r#"{"rows":[{"id":"solo"}]}"#).unwrap())[0].id, "solo");
        let many: Vec<Value> = (0..12).map(|i| json!({"id": format!("x{i}")})).collect();
        assert_eq!(free_cells(&parse_layout(&json!({"page": {"cols": many}}).to_string()).unwrap()).len(), MAX_PANELS);
        let mut deep = json!({"id": "z"});
        for _ in 0..20 {
            deep = json!({"rows": [{"id": "y"}, deep]});
        }
        assert!(free_cells(&parse_layout(&json!({"page": deep}).to_string()).unwrap()).len() <= MAX_PANELS);
    }

    // ---------------------------------------------------------------- 自訂版面
    fn custom(rows: Value) -> Value {
        json!({"layout": "custom", "custom": {"rows": rows}})
    }

    #[test]
    fn custom_rows_are_described_and_read_right_to_left() {
        let p = custom(json!([
            {"h": 1, "cells": [1, 1]},
            {"h": 2, "cells": [1]},
            {"h": 1, "cells": [1, 1]}
        ]));
        let (desc, slots) = layout_of(&p).unwrap();
        assert!(desc.starts_with("3 rows: two panels side by side at the top"), "{desc}");
        assert!(desc.contains("one wide panel in the middle"), "{desc}");
        assert!(desc.contains("two panels side by side at the bottom"), "{desc}");
        assert!(desc.contains("middle row is taller"), "{desc}");
        let names: Vec<&str> = slots.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(
            names,
            ["top right panel", "top left panel", "middle panel", "bottom right panel", "bottom left panel"]
        );
        let zh: Vec<&str> = slots.iter().map(|s| s.1.as_str()).collect();
        assert_eq!(zh, ["右上那格", "左上那格", "中間那格", "右下那格", "左下那格"]);

        // 提示詞要照閱讀順序寫，而且每一格都進得去
        let mut page = p.clone();
        page["panels"] = json!([
            {"what": "a"}, {"what": "b"}, {"what": "c"}, {"what": "d"}, {"what": "e"}
        ]);
        let s = moss_page(&page).unwrap();
        assert!(s.contains("The image is a vertical full-color anime manga page divided into 3 rows:"));
        let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("缺 {needle}：{s}"));
        assert!(at("The top right panel shows") < at("The top left panel shows"));
        assert!(at("The top left panel shows") < at("The middle panel shows"));
        assert!(at("The middle panel shows") < at("The bottom right panel shows"));
        assert!(at("The bottom right panel shows") < at("The bottom left panel shows"));
    }

    #[test]
    fn custom_one_row_names_every_column() {
        let p = custom(json!([{"h": 1, "cells": [1, 1, 1, 1]}]));
        let (desc, slots) = layout_of(&p).unwrap();
        assert_eq!(desc, "four panels side by side");
        let names: Vec<&str> = slots.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(names, ["rightmost panel", "center right panel", "center left panel", "leftmost panel"]);
        // 只有一格：就叫 panel，中文「這一格」
        let (desc1, s1) = layout_of(&custom(json!([{"h": 1, "cells": [1]}]))).unwrap();
        assert_eq!(desc1, "one wide panel");
        assert_eq!((s1[0].0.as_str(), s1[0].1.as_str()), ("panel", "這一格"));
    }

    #[test]
    fn custom_unequal_widths_and_heights_are_mentioned() {
        let (desc, _) = layout_of(&custom(json!([{"h": 1, "cells": [1, 3]}]))).unwrap();
        assert!(desc.contains("one of them wider"), "{desc}");
        let (desc2, _) = layout_of(&custom(json!([{"h": 3, "cells": [1]}, {"h": 1, "cells": [1]}]))).unwrap();
        assert!(desc2.contains("top row is taller"), "{desc2}");
        // 五列以上改用「第幾列」
        let five = custom(json!([{"cells": [1]}, {"cells": [1]}, {"cells": [1]}, {"cells": [1]}, {"cells": [1]}]));
        assert_eq!(layout_of(&five).unwrap().1[0].0, "row 1 panel");
    }

    #[test]
    fn custom_layout_is_clamped_and_never_empty() {
        assert!(layout_of(&json!({"layout": "custom"})).is_err());
        assert!(layout_of(&custom(json!([]))).is_err());
        // 每列最多 4 格、全部最多 8 格；高度夾在 1～4
        let p = custom(json!([
            {"h": 9, "cells": [1, 1, 1, 1, 1, 1]},
            {"h": 0, "cells": [1, 1, 1, 1]},
            {"h": 1, "cells": [1]}
        ]));
        let (_, slots) = layout_of(&p).unwrap();
        assert_eq!(slots.len(), 8);
        assert_eq!(rows_of(&p).unwrap()[0].0, 4);
        assert_eq!(rows_of(&p).unwrap()[1].0, 1);
    }

    #[test]
    fn custom_layout_feeds_the_script_prompt() {
        let mut p = custom(json!([{"h": 1, "cells": [1, 1]}]));
        p["cast"] = json!("A knight");
        let (sys, user) = script_messages(&p, "她拔劍。");
        assert!(sys.contains("Exactly 2 panel objects"), "{sys}");
        assert!(user.contains("two panels side by side"), "{user}");
        assert!(user.contains("1. right panel / 右邊那格"), "{user}");
        assert!(user.contains("她拔劍。"), "{user}");
    }

    #[test]
    fn character_cards_feed_cast_names_and_who() {
        let mut p = sample();
        p["cast"] = json!("A black cat sleeps on the bed");
        p["chars"] = json!([
            {"name": "Lin", "text": "A tall woman with long silver hair and red eyes.", "trigger": "lin_v1,"},
            {"name": "", "text": "no name: ignored"},
            {"name": "Ken", "text": ""}]);
        let s = moss_page(&p).unwrap();
        assert!(s.contains("Lin (lin_v1): A tall woman with long silver hair and red eyes. Ken. A black cat sleeps on the bed."), "{s}");
        assert!(!s.contains("ignored"));
        assert_eq!(char_names(&p), vec!["Lin", "Ken"]);
        let (sys, user) = script_messages(&p, "她拔劍。");
        assert!(sys.contains("\"who\""), "{sys}");
        assert!(user.contains("Named characters: Lin, Ken"), "{user}");
        // who 只留角色卡上的名字（大小寫不拘、不重複）；模型吐成字串也讀得懂
        let out = parse_script(r#"{"panels":[{"what":"Lin waves","who":"lin, Bob"},{"what":"Ken nods","who":["Ken","Ken"]}]}"#, 2).unwrap();
        let names = char_names(&p);
        assert_eq!(panel_who(&out[0], &names), vec!["Lin"]);
        assert_eq!(panel_who(&out[1], &names), vec!["Ken"]);
        // 沒有角色卡：照舊
        let plain = moss_page(&sample()).unwrap();
        assert!(plain.contains("The tall right panel shows"), "{plain}");
    }
}

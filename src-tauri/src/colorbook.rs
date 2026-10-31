//! 整本上色（2026-10-29，設計見 design\整本上色_設計.md，研究在 小克工作區\測試區\manga_color\報告.md）：
//! 黑白漫畫整本丟進來 → 分彩頁／黑白／空白 → 35B 看彩頁寫角色配色表 → 每頁看黑白稿寫上色指令 → Qwen 2.1 改圖上色 → 原稿亮度回貼。
//! 這裡只有純邏輯（分類、排序、亮度回貼、提示詞、書的存檔），背景工作和 API 在 server\api_colorbook.rs。
//!
//! book.json：{id, title, created, seed, sheet, pages:[{n, file, orig, w, h, kind, status, prompt, prompt_by, out, error}]}
//!   kind：color（彩頁：不上色，拿來寫配色表）｜bw（要上色）｜blank（空白頁，跳過）
//!   status：todo｜done｜error；prompt_by：ai｜user｜plain（沒有看圖模型時的通用指令）
use image::{imageops::FilterType, Rgb, RgbImage};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// 讀得懂的圖（image crate 開了 png、jpeg、webp）
pub const IMAGE_EXT: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

pub fn is_image_name(name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// 檔名自然排序：「2.jpg」排在「10.jpg」前面（漫畫頁碼常常沒補零）
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(ch) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(ch);
                        it.next();
                    }
                    s
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m2, n2) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let o = m2.len().cmp(&n2.len()).then_with(|| m2.cmp(n2)).then_with(|| m.len().cmp(&n.len()));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// 彩頁／黑白／空白。縮到 256 寬算：平均彩度（HSV 的 S，0～255）超過 25 是彩頁（實測三本：彩頁 38～98、黑白 0～4），
/// 亮度幾乎沒有變化（標準差 < 4）是空白頁（全黑、全白的隔頁）
pub fn classify(img: &RgbImage) -> &'static str {
    let small = if img.width() > 256 { image::imageops::resize(img, 256, (img.height() * 256 / img.width()).max(1), FilterType::Triangle) } else { img.clone() };
    let n = (small.width() * small.height()).max(1) as f64;
    let (mut sat, mut sum, mut sq) = (0.0, 0.0, 0.0);
    for Rgb([r, g, b]) in small.pixels() {
        let (mx, mn) = (*r.max(g).max(b) as f64, *r.min(g).min(b) as f64);
        if mx > 0.0 {
            sat += (mx - mn) * 255.0 / mx;
        }
        let y = 0.299 * *r as f64 + 0.587 * *g as f64 + 0.114 * *b as f64;
        sum += y;
        sq += y * y;
    }
    let mean = sum / n;
    let sd = (sq / n - mean * mean).max(0.0).sqrt();
    if sd < 4.0 {
        "blank"
    } else if sat / n > 25.0 {
        "color"
    } else {
        "bw"
    }
}

/// 原稿亮度回貼：亮度（Y）用原稿、色彩（Cb、Cr）用上色結果放大到原稿尺寸 → 線條、字、網點都是原稿的，只多了顏色。
/// （JPEG 用的 BT.601 全範圍 YCbCr，跟 PIL 的 convert("YCbCr") 一樣）
pub fn keep_lines(orig: &RgbImage, colored: &RgbImage) -> RgbImage {
    let big = image::imageops::resize(colored, orig.width(), orig.height(), FilterType::Lanczos3);
    let mut out = RgbImage::new(orig.width(), orig.height());
    for (x, y, Rgb([r, g, b])) in orig.enumerate_pixels() {
        let yy = 0.299 * *r as f32 + 0.587 * *g as f32 + 0.114 * *b as f32;
        let Rgb([cr, cg, cbl]) = *big.get_pixel(x, y);
        let (cr, cg, cbl) = (cr as f32, cg as f32, cbl as f32);
        let cb = -0.168_736 * cr - 0.331_264 * cg + 0.5 * cbl;
        let crr = 0.5 * cr - 0.418_688 * cg - 0.081_312 * cbl;
        let px = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        out.put_pixel(x, y, Rgb([px(yy + 1.402 * crr), px(yy - 0.344_136 * cb - 0.714_136 * crr), px(yy + 1.772 * cb)]));
    }
    out
}

/// Qwen 上色的輸出像素數：原稿一半（大概 115 萬），夾在 80 萬～140 萬（8GB 顯卡一頁 15～30 秒）
pub fn qwen_pixels(w: u32, h: u32) -> u64 {
    (u64::from(w) * u64::from(h) / 2).clamp(800_000, 1_400_000)
}

// ---------------------------------------------------------------- 提示詞（2026-10-29 實測、10-30 改全中文，見研究報告）
// 全部中文，使用者才好改。Qwen 2.1 看中文指令跟英文一樣準（10-30 對照：認人最準的就是中文組）；
// 35B 用中文寫比較容易判斷錯，所以規則講得比英文版細：打光不是原色、配色表角色用特徵命名、裸體怎麼判斷。
/// 每頁指令開頭固定這句（沒有就補上）：Qwen 才會只上色、不重畫
pub const KEEP: &str = "為黑白漫畫頁 <image1> 上色。所有線條、分格框、對話框、所有文字和構圖都保持完全不變，只加上顏色。";

/// 看彩頁寫角色配色表（35B；9B 會把封面的衣服抄進來、兩次不一樣）
pub const SYS_SHEET: &str = r#"你要替一本黑白漫畫做「角色配色表」，給之後的上色用。
你會拿到這本書的彩頁（封面、彩色插圖）。每個不同的角色寫：黑白頁上認得出來的特徵，和原本的顏色。
規則：
- 角色名稱用看得出來的特徵取（例：「長捲髮的媽媽」「短髮齊瀏海的女兒」「金髮馬尾的同學」），不要只寫「女」「母」「A」。
- 忽略打光和特效：夜晚的色調、霓虹、紫色或粉紅色的光、發光或被催眠的眼睛。寫原本的顏色（例：被紫光照的棕髮要寫「暗赤褐色」）。
- 皮膚一定寫正常人的膚色（白皙、自然膚色、小麥色、深膚色）；彩頁皮膚看起來紫紅、粉紫都是打光，不能照抄。
- 眼睛寫正常的虹膜顏色；發光、粉紅、愛心、螺旋都是特效，改寫成看得出的原色，看不出來就寫「棕色」。
- 特徵：不靠顏色也看得出來的：性別、大人或少年少女、臉、髮型和長度、瀏海、眼鏡、配件。
- 制服：只寫在別頁也能用來認人的衣服（例：「白襯衫、紅領結、深藍裙」），沒有就寫「無」。絕對不要寫內衣、蕾絲、裸體或體液。
- 全部用繁體中文，一個角色一行，格式：角色名稱：特徵／頭髮：顏色／眼睛：顏色／皮膚：顏色／制服：…
只回答配色表本身。"#;

/// 看一頁黑白稿＋配色表，寫這頁的上色指令：一定用位置指人（「長髮的女人」這種說法換頁會認錯）
pub const SYS_PAGE: &str = r#"你要替圖像編輯模型寫一段指令，讓它替「一頁」黑白漫畫上色。
你會拿到這一頁和角色配色表。看這一頁，找出配色表裡的哪些角色出現了，每個人都用「在頁面上的位置」指（例：「左上那格」「從上面數來第二張頭像」「最下面那格右邊的女孩」），不要只用髮型長短指人。
配色表裡的角色照配色表的頭髮、眼睛、皮膚顏色；髮型、年紀、性別對不上的人（例如金髮的同學、路人、男生）不是配色表的角色，用自然的顏色，不確定是誰也用自然的顏色，不要硬套。
衣服：先看每一格的每個人。乳房、腹部或下身真的沒有衣服遮住，那個人在那一格才是裸的：寫「裸體，自然膚色」，不要給衣服顏色，就算配色表寫了制服也一樣。領口敞開、露出乳溝、露出手臂或腿都不算裸，衣服照樣上色。
只寫那一格那個人身上真的畫出來的衣服，給自然的顏色；那一格畫的就是同一套制服才用配色表的制服顏色。
敞開、拉起來或拉下來的衣服：只塗看得到的布料，並說露出來的胸口、肚子或腿是皮膚。
不要加衣服或改衣服，這一頁沒畫內衣就不要提內衣，不要加發光或打光效果；眼睛是正常的。
每一格都要給顏色，沒有臉的特寫（皮膚、手、身體部位：自然膚色）和只有物品或背景的格子也要。
也簡短說背景的顏色。開頭一定是：「為黑白漫畫頁 <image1> 上色。所有線條、分格框、對話框、所有文字和構圖都保持完全不變，只加上顏色。」
只回答指令本身，用繁體中文，一段，不要用列表或粗體。"#;

pub fn sheet_request(n: usize) -> String {
    format!("這本書的彩頁（{n} 張），請寫角色配色表。")
}

pub fn page_request(sheet: &str) -> String {
    if sheet.trim().is_empty() {
        "角色配色表：（沒有，每個人都用自然的顏色）".into()
    } else {
        format!("角色配色表：\n{}", sheet.trim())
    }
}

/// LLM 回的指令整理：拿掉 ``` 圍欄、列表符號和粗體、合成一段；沒有固定開頭（<image1>）就補上
pub fn fix_page_prompt(raw: &str) -> String {
    let t: String = raw
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim().replace("**", ""))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.contains("<image1>") {
        t
    } else {
        format!("{KEEP} {t}").trim().to_string()
    }
}

/// 沒有看圖模型（或使用者關掉 AI）時的通用指令：固定開頭＋自然配色＋配色表原文
pub fn plain_prompt(sheet: &str) -> String {
    let s = sheet.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        format!("{KEEP} 用自然的動畫配色。")
    } else {
        format!("{KEEP} 用自然的動畫配色。角色顏色：{s}")
    }
}

// ---------------------------------------------------------------- 存檔
pub fn root(data: &Path) -> PathBuf {
    data.join("colorbook")
}

/// 書的代號：cb_時間＋亂數，只收英數和底線（會拼進路徑和網址）
pub fn id_ok(id: &str) -> bool {
    id.starts_with("cb_") && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn book_dir(data: &Path, id: &str) -> PathBuf {
    root(data).join(id)
}

pub fn load(data: &Path, id: &str) -> Option<Value> {
    if !id_ok(id) {
        return None;
    }
    std::fs::read(book_dir(data, id).join("book.json")).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn save(data: &Path, book: &Value) -> std::io::Result<()> {
    let id = book["id"].as_str().unwrap_or("");
    crate::director::write_atomic(&book_dir(data, id).join("book.json"), book)
}

/// 書的清單（新的在前）：只回摘要
pub fn list(data: &Path) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(root(data))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| load(data, &e.file_name().to_string_lossy()))
        .map(|b| summary(&b))
        .collect();
    out.sort_by(|a, b| b["created"].as_f64().unwrap_or(0.0).total_cmp(&a["created"].as_f64().unwrap_or(0.0)));
    out
}

pub fn summary(b: &Value) -> Value {
    let pages = b["pages"].as_array().cloned().unwrap_or_default();
    let count = |k: &str, v: &str| pages.iter().filter(|p| p[k] == v).count();
    json!({"id": b["id"], "title": b["title"], "created": b["created"], "pages": pages.len(),
           "bw": count("kind", "bw"), "color": count("kind", "color"), "done": pages.iter().filter(|p| p["kind"] == "bw" && p["status"] == "done").count()})
}

/// 要上色的頁：指定的頁碼（1 開始），沒指定＝全部黑白頁；redo＝已經上好的也重上
pub fn targets(book: &Value, want: &[usize], redo: bool) -> Vec<usize> {
    book["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(i, p)| p["kind"] == "bw" && (want.is_empty() || want.contains(&(i + 1))) && (redo || !want.is_empty() || p["status"] != "done"))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["10.webp", "2.webp", "1.webp", "cover.jpg", "02b.png", "Page 3.png", "page 20.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1.webp", "2.webp", "02b.png", "10.webp", "cover.jpg", "Page 3.png", "page 20.png"]);
        assert!(is_image_name("a/B.JPG") && is_image_name("x.webp") && !is_image_name("79.gif") && !is_image_name("meta.json"));
    }

    #[test]
    fn classify_pages() {
        let white = RgbImage::from_pixel(300, 400, Rgb([255, 255, 255]));
        assert_eq!(classify(&white), "blank");
        let mut bw = RgbImage::from_pixel(300, 400, Rgb([250, 250, 250]));
        for x in 0..300 {
            for y in (0..400).step_by(7) {
                bw.put_pixel(x, y, Rgb([20, 20, 20]));
            }
        }
        assert_eq!(classify(&bw), "bw");
        let mut color = bw.clone();
        for (x, _, p) in color.enumerate_pixels_mut() {
            if x < 200 {
                *p = Rgb([200, 60, 90]);
            }
        }
        assert_eq!(classify(&color), "color");
    }

    #[test]
    fn keep_lines_uses_original_luma() {
        // 原稿：左半黑、右半白；上色結果（一半大小）：整張紅
        let mut orig = RgbImage::from_pixel(8, 4, Rgb([255, 255, 255]));
        for x in 0..4 {
            for y in 0..4 {
                orig.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let red = RgbImage::from_pixel(4, 2, Rgb([200, 40, 40]));
        let out = keep_lines(&orig, &red);
        assert_eq!(out.dimensions(), (8, 4));
        let Rgb([r, g, b]) = *out.get_pixel(0, 0);
        let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        assert!(luma < 40.0 && g < 10 && b < 10, "黑線還是暗的（帶一點紅）：{r},{g},{b}");
        let Rgb([r, g, b]) = *out.get_pixel(7, 3);
        assert!(r == 255 && g > 200 && b > 200, "白紙還是很亮：{r},{g},{b}");
        // 灰色原稿＋紅色：亮度不變，變成偏紅
        let grey = RgbImage::from_pixel(2, 2, Rgb([128, 128, 128]));
        let Rgb([r, g, b]) = *keep_lines(&grey, &RgbImage::from_pixel(2, 2, Rgb([200, 40, 40]))).get_pixel(0, 0);
        let y = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        assert!((y - 128.0).abs() < 2.0 && r > g + 60, "{r},{g},{b}");
    }

    #[test]
    fn prompts_and_targets() {
        assert!(fix_page_prompt("```\n最上面那格的女孩：深藍色頭髮。\n```").starts_with(KEEP));
        assert_eq!(fix_page_prompt(&format!("{KEEP}\n\n**第一格：**\n- 女孩：深藍色頭髮。")), format!("{KEEP} 第一格： 女孩：深藍色頭髮。"));
        let own = format!("{KEEP} 男生是黑髮。");
        assert_eq!(fix_page_prompt(&own), own);
        assert!(plain_prompt("").ends_with("用自然的動畫配色。"));
        assert!(plain_prompt("媽媽：暗赤褐\n女兒：深藍").contains("角色顏色：媽媽：暗赤褐 女兒：深藍"));
        assert!(page_request("").contains("沒有"));
        assert_eq!(qwen_pixels(1280, 1790), 1_145_600);
        assert_eq!(qwen_pixels(4000, 6000), 1_400_000);
        assert!(id_ok("cb_20261029_ab12") && !id_ok("cb_../x") && !id_ok("x_1"));
        let book = json!({"pages": [{"kind": "color"}, {"kind": "bw", "status": "done"}, {"kind": "bw", "status": "todo"}, {"kind": "blank"}, {"kind": "bw", "status": "error"}]});
        assert_eq!(targets(&book, &[], false), vec![2, 4]);
        assert_eq!(targets(&book, &[], true), vec![1, 2, 4]);
        assert_eq!(targets(&book, &[2], false), vec![1]); // 指定頁就算上好了也重上
        assert_eq!(targets(&book, &[1], false), Vec::<usize>::new()); // 彩頁不上色
    }
}

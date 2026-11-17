//! 漫畫修角色：這一格的每顆頭各是誰（2026-11-17）。
//! SAM3 抓出格子裡的每顆頭、也抓出角色卡參考圖裡的頭，兩邊各算「長相特徵」再配對：
//! ① 顏色分布（HSV：有彩度的照色相×彩度分格，灰白黑另外照亮度分格）→ 髮色、膚色、衣領顏色；
//! ② 反推 tag 裡跟長相有關的（髮色、髮型、瞳色、眼鏡、角、鬍子、光頭、疤…，表情不算）。
//! 分數＝顏色 60%＋tag 40%；每個角色挑分數最高、而且過門檻的那顆頭，一顆頭只給一個人；都不夠像就不換（寧可少修，不換錯人）。
use image::{GrayImage, RgbImage};
use serde_json::Value;

/// 一顆頭的長相特徵
#[derive(Debug, Clone, Default)]
pub struct HeadFeat {
    pub hist: Vec<f32>,
    pub tags: Vec<String>,
}

const HUE_BINS: usize = 12;
const SAT_BINS: usize = 3;
const GRAY_BINS: usize = 4;
pub const HIST_LEN: usize = HUE_BINS * SAT_BINS + GRAY_BINS;

/// 遮罩裡（>127）的顏色分布，加總為 1；遮罩空的回全 0
pub fn color_hist(img: &RgbImage, mask: &GrayImage) -> Vec<f32> {
    let mut h = vec![0f32; HIST_LEN];
    let (w, hh) = img.dimensions();
    let mut n = 0f32;
    for y in 0..hh.min(mask.height()) {
        for x in 0..w.min(mask.width()) {
            if mask.get_pixel(x, y).0[0] <= 127 {
                continue;
            }
            let [r, g, b] = img.get_pixel(x, y).0.map(|c| c as f32 / 255.0);
            let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
            let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
            let bin = if sat < 0.2 || mx < 0.12 {
                // 灰白黑：照亮度分（白髮、黑髮、光頭的亮皮膚分得開）
                HUE_BINS * SAT_BINS + ((mx * GRAY_BINS as f32) as usize).min(GRAY_BINS - 1)
            } else {
                let d = mx - mn;
                let hue = if mx == r {
                    ((g - b) / d).rem_euclid(6.0)
                } else if mx == g {
                    (b - r) / d + 2.0
                } else {
                    (r - g) / d + 4.0
                } / 6.0;
                let hb = ((hue * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
                let sb = (((sat - 0.2) / 0.8 * SAT_BINS as f32) as usize).min(SAT_BINS - 1);
                hb * SAT_BINS + sb
            };
            h[bin] += 1.0;
            n += 1.0;
        }
    }
    if n > 0.0 {
        h.iter_mut().for_each(|v| *v /= n);
    }
    h
}

/// 兩個分布重疊多少（0～1）
pub fn hist_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x.min(*y)).sum()
}

/// tag 裡跟「長相」有關的關鍵字（髮、眼、膚、配件、記號）；表情、動作、構圖不算
const LOOK_WORDS: &[&str] = &[
    "hair", "bangs", "braid", "twintails", "ponytail", "bun", "ahoge", "eyes", "skin", "glasses", "eyewear", "horn", "ears", "beard", "mustache",
    "bald", "scar", "tattoo", "freckles", "mole", "hat", "hood", "ribbon", "hairband", "bow", "halo", "fang", "earrings", "choker", "headband",
];
const NOT_LOOK: &[&str] = &["closed eyes", "one eye closed", "half-closed eyes", "wide-eyed", "eyes visible through hair", "looking at viewer", "hand in own hair", "hair over one eye"];

/// 反推結果 → 長相 tag（機率 ≥ min）
pub fn look_tags(tagged: &Value, min: f64) -> Vec<String> {
    let mut out: Vec<String> = tagged["general"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t[1].as_f64().unwrap_or(0.0) >= min)
        .map(|t| t[0].as_str().unwrap_or("").to_lowercase())
        .filter(|t| !NOT_LOOK.contains(&t.as_str()) && LOOK_WORDS.iter().any(|w| t.contains(w)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 長相 tag 的重疊（Jaccard）；兩邊都沒有 tag 回 0.5（不加分也不扣分）
pub fn tag_sim(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.5;
    }
    let inter = a.iter().filter(|t| b.contains(t)).count() as f32;
    let union = (a.len() + b.len()) as f32 - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// 一個角色（參考圖的頭）對一顆頭的分數 0～1
pub fn score(char_head: &HeadFeat, head: &HeadFeat) -> f32 {
    0.6 * hist_sim(&char_head.hist, &head.hist) + 0.4 * tag_sim(&char_head.tags, &head.tags)
}

/// 配對：分數由高到低，每個角色、每顆頭各用一次；低於 min 的不配。回傳每個角色配到第幾顆頭
pub fn assign(chars: &[HeadFeat], heads: &[HeadFeat], min: f32) -> Vec<Option<usize>> {
    let mut pairs: Vec<(f32, usize, usize)> = vec![];
    for (c, cf) in chars.iter().enumerate() {
        for (h, hf) in heads.iter().enumerate() {
            pairs.push((score(cf, hf), c, h));
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut out = vec![None; chars.len()];
    let mut used = vec![false; heads.len()];
    for (s, c, h) in pairs {
        if s < min || out[c].is_some() || used[h] {
            continue;
        }
        out[c] = Some(h);
        used[h] = true;
    }
    out
}

/// SAM3 常把同一顆頭拆成好幾塊（整顆頭＋一撮雙馬尾、一束瀏海）。
/// 由大到小留：一塊有一半以上落在已留下的頭裡，就算同一顆頭、丟掉。回傳由大到小
pub fn dedupe_masks(masks: Vec<GrayImage>) -> Vec<GrayImage> {
    let on = |m: &GrayImage, x: u32, y: u32| x < m.width() && y < m.height() && m.get_pixel(x, y).0[0] > 127;
    let mut sized: Vec<(usize, GrayImage)> = masks.into_iter().map(|m| (m.pixels().filter(|p| p.0[0] > 127).count(), m)).collect();
    sized.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut kept: Vec<GrayImage> = vec![];
    for (area, m) in sized {
        if area == 0 {
            continue;
        }
        let dup = kept.iter().any(|k| {
            let inside = m.enumerate_pixels().filter(|(x, y, p)| p.0[0] > 127 && on(k, *x, *y)).count();
            inside * 2 > area
        });
        if !dup {
            kept.push(m);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb};
    use serde_json::json;

    /// 畫一顆「頭」：上半是頭髮色、下半是皮膚色
    fn head(hair: [u8; 3], skin: [u8; 3]) -> (RgbImage, GrayImage) {
        let img = RgbImage::from_fn(40, 40, |_, y| Rgb(if y < 20 { hair } else { skin }));
        (img, GrayImage::from_pixel(40, 40, Luma([255])))
    }

    fn feat(hair: [u8; 3], skin: [u8; 3], tags: &[&str]) -> HeadFeat {
        let (i, m) = head(hair, skin);
        HeadFeat { hist: color_hist(&i, &m), tags: tags.iter().map(|s| s.to_string()).collect() }
    }

    fn rect_mask(x0: u32, y0: u32, x1: u32, y1: u32) -> GrayImage {
        GrayImage::from_fn(100, 100, |x, y| Luma([if (x0..x1).contains(&x) && (y0..y1).contains(&y) { 255 } else { 0 }]))
    }

    #[test]
    fn overlapping_masks_count_as_one_head() {
        // 大頭（含頭髮）＋它裡面的一撮頭髮＋另一顆頭 → 兩顆，大的在前
        let big = rect_mask(10, 10, 50, 50);
        let tuft = rect_mask(12, 12, 30, 25);
        let other = rect_mask(60, 60, 90, 90);
        let kept = dedupe_masks(vec![tuft, other.clone(), big.clone()]);
        assert_eq!(kept.len(), 2);
        assert_eq!((kept[0].get_pixel(20, 20).0[0], kept[1].get_pixel(70, 70).0[0]), (255, 255));
        // 只碰到一點點的兩顆頭都留
        let a = rect_mask(10, 10, 50, 50);
        let b = rect_mask(45, 10, 85, 50);
        assert_eq!(dedupe_masks(vec![a, b]).len(), 2);
    }

    #[test]
    fn histogram_tells_dark_haired_from_white_haired() {
        let kaito = feat([20, 20, 30], [110, 70, 45], &[]);
        let kaito2 = feat([25, 22, 28], [105, 68, 48], &[]);
        let old = feat([235, 235, 235], [240, 200, 170], &[]);
        assert!((kaito.hist.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        let same = hist_sim(&kaito.hist, &kaito2.hist);
        let diff = hist_sim(&kaito.hist, &old.hist);
        assert!(same > 0.9 && diff < 0.2, "same {same} diff {diff}");
        // 遮罩外的不算
        let (img, _) = head([20, 20, 30], [110, 70, 45]);
        let empty = GrayImage::new(40, 40);
        assert_eq!(color_hist(&img, &empty).iter().sum::<f32>(), 0.0);
    }

    #[test]
    fn look_tags_keep_appearance_drop_expression() {
        let t = json!({"general": [["black hair", 0.9], ["glasses", 0.8], ["dark skin", 0.7], ["smile", 0.9], ["closed eyes", 0.8],
                                   ["green eyes", 0.5], ["1boy", 0.99], ["white hair", 0.1]]});
        assert_eq!(look_tags(&t, 0.35), vec!["black hair", "dark skin", "glasses", "green eyes"]);
        let a: Vec<String> = vec!["black hair".into(), "glasses".into()];
        let b: Vec<String> = vec!["black hair".into(), "glasses".into(), "scar".into()];
        assert!((tag_sim(&a, &b) - 2.0 / 3.0).abs() < 1e-5);
        assert_eq!(tag_sim(&[], &[]), 0.5);
    }

    #[test]
    fn assign_picks_the_lookalike_not_the_biggest_and_refuses_strangers() {
        let kaito = feat([20, 20, 30], [110, 70, 45], &["black hair", "glasses", "dark skin"]);
        let mira = feat([250, 150, 190], [250, 225, 210], &["pink hair", "horns", "blue eyes"]);
        let old = feat([235, 235, 235], [240, 200, 170], &["white hair", "bald", "mustache"]);
        let kaito_small = feat([22, 21, 30], [108, 72, 46], &["black hair", "glasses"]);
        // 便利商店那格：店長（大）＋小 Kaito → Kaito 配到第 2 顆
        assert_eq!(assign(std::slice::from_ref(&kaito), &[old.clone(), kaito_small.clone()], 0.35), vec![Some(1)]);
        // 只抓到店長（Kaito 的頭被切掉）→ 不換
        assert_eq!(assign(std::slice::from_ref(&kaito), std::slice::from_ref(&old), 0.35), vec![None]);
        // 兩個主角同格：各配各的，順序不影響
        assert_eq!(assign(&[mira.clone(), kaito.clone()], &[kaito_small, old, mira.clone()], 0.35), vec![Some(2), Some(0)]);
        // 一顆頭不會給兩個人
        assert_eq!(assign(&[kaito.clone(), kaito], &[feat([20, 20, 30], [110, 70, 45], &["black hair", "glasses", "dark skin"])], 0.35), vec![Some(0), None]);
    }
}

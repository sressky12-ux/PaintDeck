//! 漫畫台「照資產修角色」（2026-11-16）：從 MOSS 畫好的整頁找出每一格，照閱讀順序（上面先、右邊先）排好；
//! 修好的格子再貼回原頁。不碰網路、不碰 ComfyUI，背景工作在 server\api_comic.rs 的 fix_page。
//!
//! 找格子的做法：從頁面四邊往內灌「白色」→ 灌得到的就是格線和頁邊（格子有黑框圍住，格子裡的白色灌不進去）；
//! 剩下不是格線的連在一起的區塊＝一格（含格子裡的白色、對話框）。太小的區塊（格線上的狀聲詞、雜點）不算。
//! 破格的角色會把兩格連成一塊、斜切小格會黏在大格上：這時候找到的格數跟版型對不上，呼叫的人改成整頁一次修。
use image::{GrayImage, Luma, RgbImage};
use std::collections::VecDeque;

/// 一格：外框（x, y, w, h）＋外框大小的遮罩（255＝這一格）
pub struct Panel {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub mask: GrayImage,
}

/// 格線的白：夠亮、幾乎沒有顏色（MOSS 的格線是純白，邊緣有反鋸齒的灰）
fn is_white(p: &image::Rgb<u8>) -> bool {
    let [r, g, b] = p.0;
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    mn >= 225 && mx - mn <= 30
}

/// 找格子，回傳照閱讀順序排好的格子。一格都找不到（整頁沒有格線）回空的
pub fn detect(img: &RgbImage) -> Vec<Panel> {
    let (w, h) = img.dimensions();
    if w < 16 || h < 16 {
        return vec![];
    }
    let (wu, hu) = (w as usize, h as usize);
    let idx = |x: usize, y: usize| y * wu + x;
    // ① 從四邊灌白色：灌到的是格線、頁邊
    let mut gutter = vec![false; wu * hu];
    let mut q: VecDeque<(usize, usize)> = VecDeque::new();
    let seed = |x: usize, y: usize, g: &mut Vec<bool>, q: &mut VecDeque<(usize, usize)>| {
        if !g[idx(x, y)] && is_white(img.get_pixel(x as u32, y as u32)) {
            g[idx(x, y)] = true;
            q.push_back((x, y));
        }
    };
    for x in 0..wu {
        seed(x, 0, &mut gutter, &mut q);
        seed(x, hu - 1, &mut gutter, &mut q);
    }
    for y in 0..hu {
        seed(0, y, &mut gutter, &mut q);
        seed(wu - 1, y, &mut gutter, &mut q);
    }
    while let Some((x, y)) = q.pop_front() {
        for (nx, ny) in neighbours(x, y, wu, hu) {
            seed(nx, ny, &mut gutter, &mut q);
        }
    }
    // ② 不是格線的地方分成一塊一塊（四連通）；破格的對話框把兩格連成一塊時，沿格線切開再分一次（最多 3 輪）
    let (mut label, mut boxes) = regions(&gutter, wu, hu);
    for _ in 0..3 {
        if !cut_bridges(&mut gutter, &label, &boxes, wu, hu) {
            break;
        }
        (label, boxes) = regions(&gutter, wu, hu);
    }
    // ③ 夠大的才算一格：面積至少整頁的 1%，寬高至少整頁的 4%（自訂版面很矮的一列約 4%；格線上的狀聲詞、雜點比這小）
    let page = (wu * hu) as f64;
    let keep: Vec<usize> = (0..boxes.len())
        .filter(|&i| {
            let (x0, y0, x1, y1, area) = boxes[i];
            area as f64 >= page * 0.01 && (x1 - x0 + 1) as f64 >= wu as f64 * 0.04 && (y1 - y0 + 1) as f64 >= hu as f64 * 0.04
        })
        .collect();
    let rects: Vec<(f64, f64, f64, f64)> = keep.iter().map(|&i| (boxes[i].0 as f64, boxes[i].1 as f64, boxes[i].2 as f64 + 1.0, boxes[i].3 as f64 + 1.0)).collect();
    let order = reading_order(&rects, (0..rects.len()).collect(), wu as f64 * 0.01, hu as f64 * 0.01);
    order
        .into_iter()
        .map(|k| {
            let i = keep[k];
            let (x0, y0, x1, y1, _) = boxes[i];
            let (pw, ph) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
            let id = i as u32 + 1;
            let mask = GrayImage::from_fn(pw, ph, |mx, my| {
                let (x, y) = (x0 + mx as usize, y0 + my as usize);
                Luma([if label[idx(x, y)] == id { 255 } else { 0 }])
            });
            Panel { x: x0 as u32, y: y0 as u32, w: pw, h: ph, mask }
        })
        .collect()
}

/// 一塊的外框與面積：(minx, miny, maxx, maxy, 面積)
type Region = (usize, usize, usize, usize, usize);

/// 不是格線的地方分塊（四連通）：回傳每個像素的塊編號（0＝格線）和每一塊的外框
fn regions(gutter: &[bool], wu: usize, hu: usize) -> (Vec<u32>, Vec<Region>) {
    let idx = |x: usize, y: usize| y * wu + x;
    let mut label = vec![0u32; wu * hu];
    let mut boxes: Vec<Region> = vec![];
    let mut q: VecDeque<(usize, usize)> = VecDeque::new();
    for sy in 0..hu {
        for sx in 0..wu {
            if gutter[idx(sx, sy)] || label[idx(sx, sy)] != 0 {
                continue;
            }
            let id = boxes.len() as u32 + 1;
            let mut b = (sx, sy, sx, sy, 0usize);
            label[idx(sx, sy)] = id;
            q.push_back((sx, sy));
            while let Some((x, y)) = q.pop_front() {
                b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y), b.4 + 1);
                for (nx, ny) in neighbours(x, y, wu, hu) {
                    let i = idx(nx, ny);
                    if !gutter[i] && label[i] == 0 {
                        label[i] = id;
                        q.push_back((nx, ny));
                    }
                }
            }
            boxes.push(b);
        }
    }
    (label, boxes)
}

/// 破格：一塊的外框裡有一整條（至少 2 像素厚）幾乎全是格線的橫帶或直帶（格線佔 60% 以上），上下（左右）兩邊都還有這一塊 →
/// 中間只是被對話框、角色的頭髮連起來的兩格。把那一條裡屬於這一塊的像素也當格線。有切到回 true
fn cut_bridges(gutter: &mut [bool], label: &[u32], boxes: &[Region], wu: usize, hu: usize) -> bool {
    let idx = |x: usize, y: usize| y * wu + x;
    let page = (wu * hu) as f64;
    let mut cuts: Vec<usize> = vec![];
    for (i, &(x0, y0, x1, y1, area)) in boxes.iter().enumerate() {
        if (area as f64) < page * 0.02 {
            continue;
        }
        let id = i as u32 + 1;
        // horiz＝橫帶：沿 y 掃每一列；不然沿 x 掃每一行
        for horiz in [true, false] {
            let (a0, a1, b0, b1) = if horiz { (y0, y1, x0, x1) } else { (x0, x1, y0, y1) };
            let at = |a: usize, b: usize| if horiz { idx(b, a) } else { idx(a, b) };
            let span = (b1 - b0 + 1) as f64;
            let gutterish: Vec<bool> = (a0..=a1).map(|a| (b0..=b1).filter(|&b| gutter[at(a, b)]).count() as f64 >= span * 0.6).collect();
            let mut k = 0;
            while k < gutterish.len() {
                if !gutterish[k] {
                    k += 1;
                    continue;
                }
                let s = k;
                while k < gutterish.len() && gutterish[k] {
                    k += 1;
                }
                // 帶子太薄、或貼著外框（一邊沒有這一塊）就不算
                let has = |r: std::ops::Range<usize>| r.into_iter().any(|a| (b0..=b1).any(|b| label[at(a0 + a, b)] == id));
                if k - s >= 2 && has(0..s) && has(k..gutterish.len()) {
                    for a in a0 + s..a0 + k {
                        cuts.extend((b0..=b1).map(|b| at(a, b)).filter(|&p| label[p] == id));
                    }
                }
            }
        }
    }
    for &p in &cuts {
        gutter[p] = true;
    }
    !cuts.is_empty()
}

fn neighbours(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = (usize, usize)> {
    let mut v = [(usize::MAX, 0); 4];
    if x > 0 {
        v[0] = (x - 1, y);
    }
    if x + 1 < w {
        v[1] = (x + 1, y);
    }
    if y > 0 {
        v[2] = (x, y - 1);
    }
    if y + 1 < h {
        v[3] = (x, y + 1);
    }
    v.into_iter().filter(|p| p.0 != usize::MAX)
}

/// 找一刀切得開的位置：照起點排好，前面幾塊的終點都不超過下一塊的起點（容許 tol 的重疊）→ 切在那裡
fn cut(ids: &[usize], start: impl Fn(usize) -> f64, end: impl Fn(usize) -> f64, tol: f64) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut s: Vec<usize> = ids.to_vec();
    s.sort_by(|&a, &b| start(a).total_cmp(&start(b)));
    let mut reach = f64::MIN;
    for k in 1..s.len() {
        reach = reach.max(end(s[k - 1]));
        if reach <= start(s[k]) + tol {
            return Some((s[..k].to_vec(), s[k..].to_vec()));
        }
    }
    None
}

/// 漫畫的閱讀順序（XY 切割）：先找橫的一刀（上面那塊先讀），切不開再找直的一刀（右邊那塊先讀），兩種都切不開（斜切）就照上緣、右緣排
fn reading_order(r: &[(f64, f64, f64, f64)], ids: Vec<usize>, tolx: f64, toly: f64) -> Vec<usize> {
    if ids.len() <= 1 {
        return ids;
    }
    if let Some((top, rest)) = cut(&ids, |i| r[i].1, |i| r[i].3, toly) {
        let mut out = reading_order(r, top, tolx, toly);
        out.extend(reading_order(r, rest, tolx, toly));
        return out;
    }
    if let Some((left, right)) = cut(&ids, |i| r[i].0, |i| r[i].2, tolx) {
        let mut out = reading_order(r, right, tolx, toly);
        out.extend(reading_order(r, left, tolx, toly));
        return out;
    }
    let mut s = ids;
    s.sort_by(|&a, &b| r[a].1.total_cmp(&r[b].1).then(r[b].2.total_cmp(&r[a].2)));
    s
}

/// 遮罩往內縮 shrink 像素再羽化；羽化的尾巴不准超過往內縮 2 像素的硬邊（原本的黑框一點都不重畫）
fn soft_mask(mask: &GrayImage, shrink: u32, feather: f32) -> GrayImage {
    let soft = erode(mask, shrink);
    let soft = if feather > 0.0 { image::imageops::blur(&soft, feather) } else { soft };
    let hard = erode(mask, 2);
    GrayImage::from_fn(mask.width(), mask.height(), |x, y| Luma([soft.get_pixel(x, y).0[0].min(hard.get_pixel(x, y).0[0])]))
}

/// 遮罩往內縮 r 像素（可分離的最小值濾波：先橫再直；圖外當 0）
fn erode(mask: &GrayImage, r: u32) -> GrayImage {
    let (w, h) = mask.dimensions();
    let r = r as i64;
    let pass = |src: &GrayImage, horiz: bool| {
        GrayImage::from_fn(w, h, |x, y| {
            let mut m = 255u8;
            for d in -r..=r {
                let (nx, ny) = if horiz { (x as i64 + d, y as i64) } else { (x as i64, y as i64 + d) };
                let v = if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 { 0 } else { src.get_pixel(nx as u32, ny as u32).0[0] };
                m = m.min(v);
            }
            Luma([m])
        })
    };
    pass(&pass(mask, true), false)
}

/// 修好的格子（任何尺寸）縮回這一格的大小，照遮罩貼回整頁；格線、黑框、格子外的東西一點都不動
pub fn paste(page: &mut RgbImage, p: &Panel, fixed: &RgbImage) {
    let fixed = image::imageops::resize(fixed, p.w, p.h, image::imageops::FilterType::Lanczos3);
    let m = soft_mask(&p.mask, 4, 1.5);
    for my in 0..p.h {
        for mx in 0..p.w {
            let a = m.get_pixel(mx, my).0[0] as u32;
            if a == 0 {
                continue;
            }
            let (x, y) = (p.x + mx, p.y + my);
            if x >= page.width() || y >= page.height() {
                continue;
            }
            let o = page.get_pixel(x, y).0;
            let n = fixed.get_pixel(mx, my).0;
            let mix = |i: usize| ((n[i] as u32 * a + o[i] as u32 * (255 - a) + 127) / 255) as u8;
            page.put_pixel(x, y, image::Rgb([mix(0), mix(1), mix(2)]));
        }
    }
}

/// 這一格的外框切下來（送 Qwen 用）
pub fn crop(page: &RgbImage, p: &Panel) -> RgbImage {
    image::imageops::crop_imm(page, p.x, p.y, p.w, p.h).to_image()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// 畫一頁：白底，每一格黑框（3px）＋灰色內容，格子中間放一塊白色（對話框）
    fn page(rects: &[(u32, u32, u32, u32)]) -> RgbImage {
        let mut img = RgbImage::from_pixel(400, 600, Rgb([255, 255, 255]));
        for &(x, y, w, h) in rects {
            for yy in y..y + h {
                for xx in x..x + w {
                    let edge = xx < x + 3 || yy < y + 3 || xx >= x + w - 3 || yy >= y + h - 3;
                    img.put_pixel(xx, yy, if edge { Rgb([0, 0, 0]) } else { Rgb([120, 140, 160]) });
                }
            }
            for yy in y + 10..y + 30 {
                for xx in x + 10..x + 40 {
                    img.put_pixel(xx, yy, Rgb([255, 255, 255]));
                }
            }
        }
        img
    }

    #[test]
    fn finds_panels_in_reading_order() {
        // L3：右邊直長＋左邊上下兩格 → 右、左上、左下
        let img = page(&[(210, 10, 180, 580), (10, 10, 185, 285), (10, 305, 185, 285)]);
        let p = detect(&img);
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].x, p[0].y), (210, 10), "右邊直長那格先");
        assert_eq!((p[1].x, p[1].y), (10, 10));
        assert_eq!((p[2].x, p[2].y), (10, 305));
        // 格子裡的白色對話框算在格子裡
        assert_eq!(p[0].mask.get_pixel(20, 15).0[0], 255);
        // 六格：三排各兩格，右邊先
        let six: Vec<(u32, u32, u32, u32)> = [10u32, 205, 400].iter().flat_map(|&y| [(205, y, 185, 185), (10, y, 185, 185)]).collect();
        let p = detect(&page(&six));
        assert_eq!(p.len(), 6);
        let at: Vec<(u32, u32)> = p.iter().map(|q| (q.x, q.y)).collect();
        assert_eq!(at, vec![(205, 10), (10, 10), (205, 205), (10, 205), (205, 400), (10, 400)]);
    }

    #[test]
    fn no_gutter_or_tiny_bits_are_not_panels() {
        // 整頁沒有格線（四邊灌不進白色）：整張算一大塊，呼叫的人會因為格數對不上改成整頁修
        let full = RgbImage::from_pixel(200, 300, Rgb([90, 90, 90]));
        assert_eq!(detect(&full).len(), 1);
        // 格線上的小字（狀聲詞）不算一格
        let mut img = page(&[(10, 10, 380, 280), (10, 310, 380, 280)]);
        for y in 295..300 {
            for x in 100..110 {
                img.put_pixel(x, y, Rgb([255, 0, 128]));
            }
        }
        assert_eq!(detect(&img).len(), 2);
    }

    #[test]
    fn bubble_crossing_the_gutter_does_not_merge_panels() {
        // 上下兩格，中間一個對話框（深色）跨過格線把兩格連起來 → 還是兩格
        let mut img = page(&[(10, 10, 380, 280), (10, 310, 380, 280)]);
        for y in 280..320 {
            for x in 150..200 {
                img.put_pixel(x, y, Rgb([30, 30, 30]));
            }
        }
        let p = detect(&img);
        assert_eq!(p.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>(), vec![(10, 10), (10, 310)]);
        // 左右兩格，跨過直的格線 → 還是兩格，右邊先
        let mut img = page(&[(10, 10, 185, 580), (205, 10, 185, 580)]);
        for y in 200..240 {
            for x in 185..215 {
                img.put_pixel(x, y, Rgb([30, 30, 30]));
            }
        }
        let p = detect(&img);
        assert_eq!(p.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>(), vec![(205, 10), (10, 10)]);
        // L 形黏在一起（Kaito 那頁：上面橫長格＋右下格被對話框連起來）→ 拆回兩格，左下格照舊
        let mut img = page(&[(10, 10, 380, 280), (10, 310, 185, 280), (205, 310, 185, 280)]);
        for y in 280..320 {
            for x in 300..340 {
                img.put_pixel(x, y, Rgb([30, 30, 30]));
            }
        }
        assert_eq!(detect(&img).len(), 3);
    }

    #[test]
    fn paste_keeps_borders_and_outside() {
        let mut img = page(&[(10, 10, 380, 580)]);
        let before = img.clone();
        let p = detect(&img).remove(0);
        let red = RgbImage::from_pixel(50, 80, Rgb([255, 0, 0]));
        paste(&mut img, &p, &red);
        assert_eq!(img.get_pixel(200, 300), &Rgb([255, 0, 0]), "格子裡面換掉");
        assert_eq!(img.get_pixel(11, 200), before.get_pixel(11, 200), "黑框不動");
        assert_eq!(img.get_pixel(5, 5), &Rgb([255, 255, 255]), "格線不動");
        assert_eq!(crop(&before, &p).dimensions(), (p.w, p.h));
    }
}

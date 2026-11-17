"""裁頭／貼回的運算（只用 torch，不 import comfy，方便單元測試）。

圖：[H, W, 3]、0～1；遮罩：[H, W]、0～1。"""
import math

import torch
import torch.nn.functional as F

MULTIPLE = 32


def pick_mask(masks, index):
    """SAM3 的多張遮罩照面積大到小排，取第 index 張；沒有（或全空）回 None。"""
    if masks is None or masks.ndim != 3 or masks.shape[0] == 0:
        return None
    areas = (masks > 0.5).flatten(1).sum(1)
    order = [int(i) for i in torch.argsort(areas, descending=True) if int(areas[i]) > 0]
    return masks[order[index]] if index < len(order) else None


def crop_box(mask, w, h, context):
    """遮罩外框往外擴 context × 長邊，盡量擴成正方形，再夾在圖內。None＝整張。"""
    if mask is None:
        return (0, 0, w, h)
    ys, xs = torch.nonzero(mask > 0.5, as_tuple=True)
    if len(xs) == 0:
        return (0, 0, w, h)
    bx0, bx1, by0, by1 = int(xs.min()), int(xs.max()) + 1, int(ys.min()), int(ys.max()) + 1
    side = max(bx1 - bx0, by1 - by0)
    size = min(int(round(side * (1 + 2 * context))), max(w, h))
    cx, cy = (bx0 + bx1) / 2, (by0 + by1) / 2

    def span(c, n, limit):
        n = min(n, limit)
        a = int(round(c - n / 2))
        a = max(0, min(a, limit - n))
        return a, a + n

    x0, x1 = span(cx, size, w)
    y0, y1 = span(cy, size, h)
    return (x0, y0, x1, y1)


def _resize(img, h, w):
    """[H, W, C] → [h, w, C]，bicubic＋antialias，夾在 0～1。"""
    t = img.permute(2, 0, 1)[None]
    out = F.interpolate(t, size=(h, w), mode="bicubic", align_corners=False, antialias=True)
    return out[0].permute(1, 2, 0).clamp(0, 1)


def _resize_mask(m, h, w):
    return F.interpolate(m[None, None], size=(h, w), mode="bilinear", align_corners=False, antialias=True)[0, 0].clamp(0, 1)


def feather(mask, grow, blur):
    """往外長 grow 像素，再高斯羽化 blur 像素。"""
    m = mask[None, None].float()
    if grow > 0:
        m = F.max_pool2d(m, kernel_size=2 * grow + 1, stride=1, padding=grow)
    if blur > 0:
        sigma = blur / 2
        r = max(1, int(math.ceil(sigma * 3)))
        x = torch.arange(-r, r + 1, dtype=m.dtype)
        k = torch.exp(-(x * x) / (2 * sigma * sigma))
        k = k / k.sum()
        m = F.pad(m, (r, r, r, r), mode="replicate")
        m = F.conv2d(m, k.view(1, 1, 1, -1))
        m = F.conv2d(m, k.view(1, 1, -1, 1))
    return m[0, 0].clamp(0, 1)


def do_crop(img, mask, context, pixels, grow, blur):
    """回傳 (放大後的裁切, 裁切的遮罩, stitch)。mask=None（沒找到頭）＝整張、遮罩全黑：什麼都不重畫，貼回跟原圖一樣。"""
    h, w = img.shape[:2]
    x0, y0, x1, y1 = crop_box(mask, w, h, context)
    rw, rh = x1 - x0, y1 - y0
    scale = math.sqrt(pixels / float(rw * rh))
    tw = max(MULTIPLE, int(round(rw * scale / MULTIPLE)) * MULTIPLE)
    th = max(MULTIPLE, int(round(rh * scale / MULTIPLE)) * MULTIPLE)
    crop = _resize(img[y0:y1, x0:x1], th, tw)
    if mask is None:
        cmask = torch.zeros(th, tw)
    else:
        cmask = feather((_resize_mask(mask[y0:y1, x0:x1].float(), th, tw) > 0.5).float(), grow, blur)
    return crop, cmask, {"box": (x0, y0, x1, y1), "size": (w, h)}


def do_stitch(img, fixed, cmask, stitch, color_match=True):
    """把 fixed 縮回裁切框、照遮罩貼回原圖。color_match：用遮罩外那一圈把整體偏色拉回來。"""
    x0, y0, x1, y1 = stitch["box"]
    rh, rw = y1 - y0, x1 - x0
    orig = img[y0:y1, x0:x1]
    f = _resize(fixed[..., :3], rh, rw)
    m = _resize_mask(cmask.float(), rh, rw)
    if color_match:
        ring = (m < 0.02) & (feather((m > 0.02).float(), max(2, min(rh, rw) // 16), 0) > 0.5)
        if int(ring.sum()) >= 32:
            shift = orig[ring].mean(0) - f[ring].mean(0)
            f = (f + shift).clamp(0, 1)
    m3 = m[..., None]
    out = img.clone()
    out[y0:y1, x0:x1] = orig * (1 - m3) + f * m3
    return out

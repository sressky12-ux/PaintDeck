"""headcrop 的單元測試：只要 torch（用 ComfyUI 的 python_embeded 跑）。
python -I resources\\extras\\anima_comic_nodes\\test_headcrop.py"""
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import torch  # noqa: E402

from headcrop import crop_box, do_crop, do_stitch, pick_mask  # noqa: E402


def blank_mask(h, w, x0, y0, x1, y1):
    m = torch.zeros(h, w)
    m[y0:y1, x0:x1] = 1.0
    return m


def test_pick_largest():
    small = blank_mask(50, 50, 0, 0, 10, 10)
    big = blank_mask(50, 50, 10, 10, 30, 30)
    assert torch.equal(pick_mask(torch.stack([small, big]), 0), big)
    assert torch.equal(pick_mask(torch.stack([small, big]), 1), small)
    assert pick_mask(torch.zeros(2, 50, 50), 0) is None
    assert pick_mask(torch.zeros(0, 50, 50), 0) is None
    assert pick_mask(torch.stack([small, big]), 5) is None


def test_box_clamped_at_edge():
    x0, y0, x1, y1 = crop_box(blank_mask(100, 200, 0, 0, 10, 10), 200, 100, 0.6)
    assert (x0, y0) == (0, 0)
    assert x1 <= 200 and y1 <= 100 and x1 > x0 and y1 > y0
    # 右下角也一樣夾住
    x0, y0, x1, y1 = crop_box(blank_mask(100, 200, 190, 90, 200, 100), 200, 100, 0.6)
    assert x1 == 200 and y1 == 100 and x0 >= 0 and y0 >= 0


def test_box_none_is_whole():
    assert crop_box(None, 200, 100, 0.6) == (0, 0, 200, 100)


def test_stitch_roundtrip_keeps_outside():
    torch.manual_seed(0)
    img = torch.rand(100, 100, 3)
    mask = blank_mask(100, 100, 40, 40, 60, 60)
    crop, cmask, st = do_crop(img, mask, context=0.6, pixels=256 * 256, grow=2, blur=2)
    # 跟真的一樣：遮罩外 Qwen 沒改（noise mask 鎖住），只有頭那塊變紅
    red = crop.clone()
    red[cmask > 0.99] = torch.tensor([1.0, 0.0, 0.0])
    out = do_stitch(img, red, cmask, st, color_match=True)
    assert out.shape == img.shape
    x0, y0, x1, y1 = st["box"]
    outside = torch.ones(100, 100, dtype=torch.bool)
    outside[y0:y1, x0:x1] = False
    assert torch.equal(out[outside], img[outside]), "裁切框外一個像素都不能動"
    # 框內、遮罩外（羽化範圍以外）也照原圖
    assert torch.allclose(out[y0, x0], img[y0, x0])
    # 遮罩中心換成新的（顏色對齊後不一定純紅，但要跟原圖不同、紅色偏多）
    c = out[50, 50]
    assert not torch.allclose(c, img[50, 50]) and c[0] >= c[1] and c[0] >= c[2]


def test_color_match_removes_overall_drift():
    torch.manual_seed(1)
    img = torch.rand(100, 100, 3) * 0.6 + 0.2
    mask = blank_mask(100, 100, 40, 40, 60, 60)
    crop, cmask, st = do_crop(img, mask, context=0.6, pixels=256 * 256, grow=2, blur=2)
    drift = (crop + 0.15).clamp(0, 1)  # Qwen 整張偏亮
    out = do_stitch(img, drift, cmask, st, color_match=True)
    assert abs(float(out[45:55, 45:55].mean() - img[45:55, 45:55].mean())) < 0.03, "偏亮要被拉回來"
    raw = do_stitch(img, drift, cmask, st, color_match=False)
    assert float(raw[45:55, 45:55].mean() - img[45:55, 45:55].mean()) > 0.1


def test_small_head_scaled_up():
    img = torch.rand(1000, 1000, 3)
    mask = blank_mask(1000, 1000, 500, 500, 530, 530)
    crop, cmask, st = do_crop(img, mask, context=0.6, pixels=1024 * 1024, grow=4, blur=4)
    h, w = crop.shape[:2]
    assert h >= 512 and w >= 512 and h % 32 == 0 and w % 32 == 0
    assert cmask.shape == (h, w)
    assert st["size"] == (1000, 1000)


def test_no_head_changes_nothing():
    # 一顆頭都沒找到：遮罩全黑 → 不重畫、貼回後跟原圖一模一樣（對話框和字不會被重抽）
    img = torch.rand(64, 96, 3)
    crop, cmask, st = do_crop(img, None, context=0.6, pixels=128 * 128, grow=0, blur=0)
    assert st["box"] == (0, 0, 96, 64)
    assert float(cmask.max()) == 0.0
    out = do_stitch(img, torch.zeros_like(crop), cmask, st, color_match=True)
    assert torch.equal(out, img)


if __name__ == "__main__":
    fails = 0
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print("ok  ", name)
            except Exception as e:  # noqa: BLE001
                fails += 1
                print("FAIL", name, repr(e))
    sys.exit(1 if fails else 0)

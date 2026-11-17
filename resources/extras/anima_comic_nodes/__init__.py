"""繪匣附帶的漫畫小節點（2026-11-16）：漫畫台「照資產修角色」只換頭用。

AnimaHeadCrop：SAM3 找到的頭部遮罩挑最大的（index 0），連周圍一起裁下來、放大到約 pixels 個像素，
  遠景小臉也有足夠的解析度重畫；輸出裁切、羽化過的遮罩（接 SetLatentNoiseMask，只讓頭重畫）、貼回用的位置。
  一顆頭都沒找到時輸出整張圖＋全黑遮罩（什麼都不重畫，對話框和字不會被重抽）。只處理 batch 的第一張。
AnimaHeadStitch：重畫好的裁切縮回原大小，用遮罩外那一圈把整體偏色拉回原圖，再照遮罩貼回；遮罩外的像素一個都不動。
運算在 headcrop.py（只用 torch，有單元測試 test_headcrop.py）。
"""
import torch

from .headcrop import do_crop, do_stitch, pick_mask


class AnimaHeadCrop:
    @classmethod
    def INPUT_TYPES(cls):
        return {
            "required": {
                "image": ("IMAGE",),
                "index": ("INT", {"default": 0, "min": 0, "max": 16, "tooltip": "第幾顆頭（照面積大到小，0＝最大）"}),
                "context": ("FLOAT", {"default": 0.6, "min": 0.0, "max": 3.0, "step": 0.05, "tooltip": "頭的外框四周多裁多少（長邊的倍數）"}),
                "pixels": ("INT", {"default": 1048576, "min": 65536, "max": 4194304, "step": 1024, "tooltip": "裁下來放大到的總像素"}),
                "grow": ("INT", {"default": 16, "min": 0, "max": 128, "tooltip": "遮罩往外長幾個像素（裁切後的尺寸）"}),
                "blur": ("INT", {"default": 12, "min": 0, "max": 128, "tooltip": "遮罩羽化幾個像素（裁切後的尺寸）"}),
            },
            "optional": {"masks": ("MASK", {"tooltip": "SAM3_Detect 的 masks（individual_masks 開）"})},
        }

    RETURN_TYPES = ("IMAGE", "MASK", "ANIMA_STITCH")
    RETURN_NAMES = ("crop", "mask", "stitch")
    FUNCTION = "run"
    CATEGORY = "image/anima"

    def run(self, image, index=0, context=0.6, pixels=1048576, grow=16, blur=12, masks=None):
        img = image[0, ..., :3].float().cpu()
        m = None
        if masks is not None:
            mk = masks.float().cpu()
            if mk.ndim == 2:
                mk = mk[None]
            if mk.shape[-2:] != img.shape[:2]:
                mk = torch.nn.functional.interpolate(mk[:, None], size=img.shape[:2], mode="nearest")[:, 0]
            m = pick_mask(mk, index)
        crop, cmask, stitch = do_crop(img, m, context, pixels, grow, blur)
        stitch["found"] = m is not None
        return (crop[None], cmask[None], stitch)


class AnimaHeadStitch:
    @classmethod
    def INPUT_TYPES(cls):
        return {
            "required": {
                "image": ("IMAGE", {"tooltip": "原圖（裁切前那張）"}),
                "fixed": ("IMAGE", {"tooltip": "重畫好的裁切"}),
                "mask": ("MASK", {"tooltip": "AnimaHeadCrop 的 mask"}),
                "stitch": ("ANIMA_STITCH",),
                "color_match": ("BOOLEAN", {"default": True, "tooltip": "用遮罩外那一圈把整體偏色拉回原圖"}),
            }
        }

    RETURN_TYPES = ("IMAGE",)
    FUNCTION = "run"
    CATEGORY = "image/anima"

    def run(self, image, fixed, mask, stitch, color_match=True):
        img = image[0, ..., :3].float().cpu()
        out = do_stitch(img, fixed[0].float().cpu(), mask[0].float().cpu(), stitch, color_match)
        return (out[None].to(image.dtype),)


NODE_CLASS_MAPPINGS = {"AnimaHeadCrop": AnimaHeadCrop, "AnimaHeadStitch": AnimaHeadStitch}
NODE_DISPLAY_NAME_MAPPINGS = {"AnimaHeadCrop": "Anima 裁頭（漫畫修角色）", "AnimaHeadStitch": "Anima 貼回頭（漫畫修角色）"}

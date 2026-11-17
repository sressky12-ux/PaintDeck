"""繪匣附帶的 MiniMax H3 小節點。

AnimaH3AddRefLatents：把參考圖用影片 VAE 編碼後放進 conditioning 的 minimax_refs，
也就是內建 MiniMaxH3ReferenceToVideo 對參考圖做的後半段（DiT 每一步取樣都看得到原圖）。
長片接續節點（Herrgotts H3 Infinite）的 qwen_reference 只把圖交給文字編碼器，接續段沒有首幀可看，
角色的衣服、髮色會一段一段跑掉；同一張圖再經過這顆節點就守得住（2026-11-13 本機三組對照）。

用法：同一批圖照同樣順序接 qwen_reference_N 和這裡的 image_N，提示詞用 <Picture N> 指它們。
前面不能有首幀／尾幀佔掉 <Picture> 編號（那種接法沒測過，繪匣不會這樣接）。
"""
import math

import comfy.utils
import node_helpers

CANVAS_MULTIPLE = 32
REF_IMAGE_SHORT_EDGE = 2048
MAX_IMAGES = 8


class AnimaH3AddRefLatents:
    @classmethod
    def INPUT_TYPES(cls):
        return {
            "required": {
                "positive": ("CONDITIONING",),
                "vae": ("VAE",),
                "latent": ("LATENT", {"tooltip": "這段的 AV latent，只用來讀畫面尺寸"}),
                "ref_image_size": (["match", "max"], {"default": "match"}),
            },
            "optional": {f"image_{i}": ("IMAGE",) for i in range(1, MAX_IMAGES + 1)},
        }

    RETURN_TYPES = ("CONDITIONING",)
    RETURN_NAMES = ("positive",)
    FUNCTION = "run"
    CATEGORY = "model/conditioning/minimax"

    def run(self, positive, vae, latent, ref_image_size="match", **images):
        samples = latent["samples"]
        if not samples.is_nested or len(samples.tensors) != 2 or samples.tensors[0].ndim != 5:
            raise ValueError("AnimaH3AddRefLatents 要接 MiniMax H3 的 AV latent")
        video = samples.tensors[0]
        height, width = video.shape[3] * 16, video.shape[4] * 16
        blocks = []
        for i in range(1, MAX_IMAGES + 1):
            img = images.get(f"image_{i}")
            if img is None:
                continue
            h, w = img.shape[1], img.shape[2]
            # 跟 MiniMaxH3ReferenceToVideo、Herrgott 的 qwen_reference 同一套縮放：文字編碼器和 DiT 看到同一張
            if ref_image_size == "match":
                scale = min(1.0, math.sqrt((width * height) / float(w * h)))
            else:
                scale = min(1.0, REF_IMAGE_SHORT_EDGE / float(min(w, h)))
            tw = max(CANVAS_MULTIPLE, round(w * scale / CANVAS_MULTIPLE) * CANVAS_MULTIPLE)
            th = max(CANVAS_MULTIPLE, round(h * scale / CANVAS_MULTIPLE) * CANVAS_MULTIPLE)
            resized = comfy.utils.common_upscale(img[:1, ..., :3].movedim(-1, 1), tw, th, "lanczos", "disabled").movedim(1, -1)
            blocks.append({"kind": "image", "latent_h": th // 16, "latent_w": tw // 16, "latent": vae.encode(resized)})
        if not blocks:
            return (positive,)
        refs = blocks + list(positive[0][1].get("minimax_refs", []) or [])
        return (node_helpers.conditioning_set_values(positive, {"minimax_refs": refs}),)


NODE_CLASS_MAPPINGS = {"AnimaH3AddRefLatents": AnimaH3AddRefLatents}
NODE_DISPLAY_NAME_MAPPINGS = {"AnimaH3AddRefLatents": "Anima H3 Add Ref Latents"}

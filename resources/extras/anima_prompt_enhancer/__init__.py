"""Anima Prompt Enhance：在 ComfyUI 工作流裡用通用模型（Qwen3.5 9B／Qwen3.6 35B GGUF）照 Qwen-Image 2.1 官方寫法改寫提示詞（官方 PE Heretic 2026-10-30 起拿掉）。

節點：
  Anima Prompt Enhance                     想法 → 官方格式的完整英文提示詞（＋建議比例、思考過程）
  Anima Prompt Edit (preview)              顯示改寫結果，可以手動修改後再往下傳
  Anima Ratio To Size                      wh_ratio（例如 3:2）→ 寬、高（對齊 32）

llama-server 的管理跟 Anima 生圖站共用 pe_core.py：同一個 port 8199、同一份狀態檔，不會兩邊同時開。
只用標準函式庫（subprocess、urllib）＋ ComfyUI 本身帶的 numpy／PIL／torch。
"""
import hashlib
import io
import os
import sys
import threading

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pe_core  # noqa: E402

WEB_DIRECTORY = "./web"


def _free_vram(ngl):
    """啟動 llama-server（GPU）前把 ComfyUI 已載入的模型全部卸載，不然 8GB 顯卡放不下。"""
    if not ngl:
        return
    import comfy.model_management as mm
    mm.unload_all_models()
    mm.soft_empty_cache()


def _to_png(img):
    """IMAGE 張量（[B,H,W,C]，0～1）的第一張 → PNG bytes，縮到約 100 萬像素以內。"""
    arr = (img[0].cpu().numpy() * 255.0).clip(0, 255).astype(np.uint8)
    im = Image.fromarray(arr[..., :3])
    s = (1024 * 1024 / float(im.width * im.height)) ** 0.5
    if s < 1:
        im = im.resize((max(28, int(im.width * s)), max(28, int(im.height * s))), Image.LANCZOS)
    b = io.BytesIO()
    im.save(b, "PNG")
    return b.getvalue()


def _progress(node_id, text):
    try:
        from server import PromptServer
        PromptServer.instance.send_progress_text(text, node_id)
    except Exception:  # noqa: BLE001  舊版 ComfyUI 沒有這個 API
        pass


class AnimaPromptEnhance:
    CATEGORY = "Anima/提示詞"
    FUNCTION = "run"
    RETURN_TYPES = ("STRING", "STRING", "STRING")
    RETURN_NAMES = ("prompt", "wh_ratio", "thinking")
    DESCRIPTION = ("用通用模型（Qwen3.5 9B／Qwen3.6 35B）把一句想法改寫成 Qwen-Image 2.1 官方格式的完整英文提示詞。"
                   "t2i＝文生圖；i2i＝改圖／多圖參考（要接圖）；anima＝Anima 動漫 tag 寫法。"
                   "填了「上一版提示詞」和「調整指令」就只改指令提到的部分。")

    @classmethod
    def INPUT_TYPES(cls):
        return {
            "required": {
                "idea": ("STRING", {"multiline": True, "default": "", "tooltip": "想法，中文或英文都可以"}),
                "task": (["t2i", "i2i", "anima"], {"default": "t2i",
                                                   "tooltip": "t2i：Qwen 2.1／Noct Q 文生圖；i2i：改圖、多圖參考（要接圖）；anima：Anima 動漫 tag＋句子"}),
                "temperature": ("FLOAT", {"default": 1.0, "min": 0.1, "max": 1.5, "step": 0.05,
                                          "tooltip": "官方範本用 1.0；調低會比較保守、每次結果比較像"}),
                "seed": ("INT", {"default": 0, "min": 0, "max": 2 ** 31 - 1, "control_after_generate": "fixed"}),
                "think": ("BOOLEAN", {"default": False, "tooltip": "先思考再寫（慢約 2 倍，結果在 thinking 輸出）"}),
                "gpu_layers": ("INT", {"default": 99, "min": 0, "max": 99,
                                       "tooltip": "99＝全放顯卡（最快，約 5.5GB）；0＝只用 CPU（不占顯存，很慢）"}),
                "unload_after": ("BOOLEAN", {"default": True,
                                             "tooltip": "改寫完關掉 llama-server，把顯存讓給後面的生圖（8GB 顯卡請保持開啟）"}),
                "noct_anime": ("BOOLEAN", {"default": False, "tooltip": "Noct Q Anime 用：開頭固定 An anime illustration of"}),
            },
            "optional": {
                "image1": ("IMAGE",), "image2": ("IMAGE",), "image3": ("IMAGE",), "image4": ("IMAGE",),
                "previous_prompt": ("STRING", {"multiline": True, "default": "", "tooltip": "微調用：上一版改寫結果"}),
                "instruction": ("STRING", {"multiline": True, "default": "", "tooltip": "微調用：還想怎麼改（只改提到的部分）"}),
            },
            "hidden": {"unique_id": "UNIQUE_ID"},
        }

    @classmethod
    def IS_CHANGED(cls, idea, task, temperature, seed, think, gpu_layers, unload_after, noct_anime,
                   image1=None, image2=None, image3=None, image4=None, previous_prompt="", instruction="", unique_id=None):
        # 輸入一樣就回傳一樣的雜湊：ComfyUI 不會重跑這個節點（gpu_layers、unload_after 不影響結果，不算進去）
        h = hashlib.sha256(repr((idea, task, round(temperature, 3), seed, think, noct_anime, previous_prompt, instruction)).encode())
        for im in (image1, image2, image3, image4):
            if im is not None:
                h.update(im.cpu().numpy().tobytes()[:1 << 20])
                h.update(str(tuple(im.shape)).encode())
        return h.hexdigest()

    def run(self, idea, task, temperature, seed, think, gpu_layers, unload_after, noct_anime,
            image1=None, image2=None, image3=None, image4=None, previous_prompt="", instruction="", unique_id=None):
        import comfy.model_management as mm
        images = [_to_png(im) for im in (image1, image2, image3, image4) if im is not None]
        refine = bool(previous_prompt.strip() and instruction.strip())
        if task == "i2i" and not images and not refine:
            raise ValueError("task 選 i2i 時至少要接一張圖（image1）")
        # 微調只改文字（逐句補丁），不用再讀一次圖
        t = "refine" if refine else task
        if refine:
            images = []
        cancel = threading.Event()
        buf = []

        def on_event(kind, text):
            if mm.processing_interrupted():
                cancel.set()
            if kind == "content":
                buf.append(text)
                if len(buf) % 20 == 0:
                    _progress(unique_id, "".join(buf)[-300:])

        _progress(unique_id, "載入改寫模型…" if not pe_core.read_state() else "改寫中…")
        try:
            r = pe_core.rewrite(t, idea=idea, images=images, prev=previous_prompt if refine else "",
                                instruction=instruction if refine else "", temperature=temperature, seed=seed, think=think,
                                ngl=gpu_layers, noct_anime=noct_anime, before_start=_free_vram, on_event=on_event, cancel=cancel)
        except pe_core.PEError as e:
            if cancel.is_set():
                mm.throw_exception_if_processing_interrupted()
            raise RuntimeError(f"AI 改寫失敗：{e}") from e
        finally:
            if unload_after:
                pe_core.stop("ComfyUI 節點：改寫完讓出顯存")
        src = "快取" if r.get("cached") else f"{r['seconds']:.1f} 秒（載入 {r['load_seconds']:.1f} 秒）"
        print(f"[Anima PE] {t}：{src}，{len(r['prompt'])} 字元，比例 {r.get('wh_ratio') or '-'}"
              + (f"；{r['note']}" if r.get("note") else ""))
        _progress(unique_id, f"完成：{src}" + (f"\n{r['note']}" if r.get("note") else ""))
        return (r["prompt"], r.get("wh_ratio") or "", r.get("reasoning") or "")


class AnimaPromptEdit:
    """顯示上游的提示詞；想手動改就改下面的 edited，並把 use_edited 打開。"""
    CATEGORY = "Anima/提示詞"
    FUNCTION = "run"
    RETURN_TYPES = ("STRING",)
    RETURN_NAMES = ("prompt",)
    OUTPUT_NODE = True
    DESCRIPTION = "預覽改寫結果。use_edited 關閉時，每次執行都會把最新結果填進 edited 讓你看；打開後改用你修改過的 edited。"

    @classmethod
    def INPUT_TYPES(cls):
        return {"required": {
            "text": ("STRING", {"forceInput": True}),
            "edited": ("STRING", {"multiline": True, "default": ""}),
            "use_edited": ("BOOLEAN", {"default": False, "label_on": "用我改過的", "label_off": "用 AI 的結果"}),
        }}

    def run(self, text, edited, use_edited):
        out = edited if use_edited and edited.strip() else text
        return {"ui": {"text": [text]}, "result": (out,)}


class AnimaRatioToSize:
    CATEGORY = "Anima/提示詞"
    FUNCTION = "run"
    RETURN_TYPES = ("INT", "INT")
    RETURN_NAMES = ("width", "height")
    DESCRIPTION = "把改寫給的 wh_ratio（例如 3:2）換成寬高，總像素約等於 megapixels，對齊 32（Qwen 2.1 的要求）。"

    @classmethod
    def INPUT_TYPES(cls):
        return {"required": {
            "wh_ratio": ("STRING", {"forceInput": True}),
            "megapixels": ("FLOAT", {"default": 1.0, "min": 0.25, "max": 2.5, "step": 0.05}),
            "fallback_width": ("INT", {"default": 832, "min": 256, "max": 2048, "step": 32}),
            "fallback_height": ("INT", {"default": 1248, "min": 256, "max": 2048, "step": 32}),
        }}

    def run(self, wh_ratio, megapixels, fallback_width, fallback_height):
        return ratio_to_size(wh_ratio, megapixels, fallback_width, fallback_height)


def ratio_to_size(ratio, megapixels=1.0, fw=832, fh=1248, step=32):
    try:
        a, b = (float(x) for x in str(ratio).split(":"))
        if a <= 0 or b <= 0:
            raise ValueError
    except (ValueError, TypeError):
        return (fw, fh)
    px = megapixels * 1024 * 1024
    w = (px * a / b) ** 0.5
    h = w * b / a
    return (max(256, int(round(w / step)) * step), max(256, int(round(h / step)) * step))


NODE_CLASS_MAPPINGS = {
    "AnimaPromptEnhance": AnimaPromptEnhance,
    "AnimaPromptEdit": AnimaPromptEdit,
    "AnimaRatioToSize": AnimaRatioToSize,
}
NODE_DISPLAY_NAME_MAPPINGS = {
    "AnimaPromptEnhance": "Anima Prompt Enhance",
    "AnimaPromptEdit": "Anima Prompt Edit (preview)",
    "AnimaRatioToSize": "Anima Ratio To Size",
}

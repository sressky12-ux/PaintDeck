"""繪匣附帶的 MOSS-Image 小節點（2026-10-31）。開站時由繪匣自動放進 ComfyUI\\custom_nodes\\（setup.rs sync_bundled）。

AnimaMOSS4BCLIPLoader：用 Qwen3.5-4B 當 MOSS-Image 的文字編碼器（原本是 9B），省記憶體。
4B 最後一層的特徵（2560 維）照投影轉接器線性換到 9B 的空間（4096 維）再交給 MOSS：
    y = ((x - mean_in) / std_in) @ W * std_out + mean_out
轉接器檔放 models\\moss_encoder_adapters\\（格式 potatoforge_moss_encoder_projection_v1，例如社群的
moss_qwen35_4b_to_9b_1600_train.safetensors）。畫面會跟 9B 不一樣，這是用品質換記憶體。"""
from pathlib import Path

import torch
from safetensors import safe_open

import comfy.sd
import folder_paths

FOLDER = "moss_encoder_adapters"
if FOLDER not in folder_paths.folder_names_and_paths:
    folder_paths.folder_names_and_paths[FOLDER] = ([], {".safetensors"})
folder_paths.add_model_folder_path(FOLDER, str(Path(folder_paths.models_dir) / FOLDER))

SHAPES = {"weight": (2560, 4096), "mean_in": (2560,), "std_in": (2560,), "mean_out": (4096,), "std_out": (4096,)}


def read_adapter(path):
    with safe_open(str(path), framework="pt", device="cpu") as f:
        if (f.metadata() or {}).get("format") != "potatoforge_moss_encoder_projection_v1":
            raise ValueError("這不是 MOSS 4B→9B 的投影轉接器（不能用 LoRA 或文字編碼器檔）")
        if set(f.keys()) != set(SHAPES):
            raise ValueError("轉接器的內容不完整")
        t = {k: f.get_tensor(k).float() for k in SHAPES}
    for k, shape in SHAPES.items():
        if tuple(t[k].shape) != shape or not torch.isfinite(t[k]).all():
            raise ValueError(f"轉接器的 {k} 形狀或數值不對")
    return t


def project(x, a):
    if x.shape[-1] != 2560:
        raise ValueError("要接 Qwen3.5-4B（每個 token 2560 維）")
    y = ((x.float().cpu() - a["mean_in"]) / a["std_in"]) @ a["weight"]
    # 轉接器是照 FP16 輸出擬合的：照樣四捨五入一次
    return (y * a["std_out"] + a["mean_out"]).half().float()


def wrap(clip, adapter):
    """CLIP 換成：MOSS 的提示詞模板 → 4B 最後一層 → 投影到 9B。clone 出來的也一樣"""
    from comfy.text_encoders.moss_image import MOSSImageTokenizer

    tok = MOSSImageTokenizer()
    name = clip.cond_stage_model.clip_name
    plain_encode, plain_clone = clip.encode_from_tokens, clip.clone

    def tokenize(text, return_word_ids=False, **kw):
        return {name: tok.tokenize_with_weights(text, return_word_ids=return_word_ids, **kw)["qwen35_9b"]}

    def encode_from_tokens(tokens, return_pooled=False, return_dict=False):
        out = plain_encode(tokens, return_pooled=return_pooled, return_dict=True)
        out["cond"] = project(out["cond"], adapter)
        if return_dict:
            return out
        return (out["cond"], out.get("pooled_output")) if return_pooled else out["cond"]

    clip.tokenize, clip.encode_from_tokens = tokenize, encode_from_tokens
    clip.clone = lambda *a, **k: wrap(plain_clone(*a, **k), adapter)
    return clip


class AnimaMOSS4BCLIPLoader:
    @classmethod
    def INPUT_TYPES(cls):
        return {"required": {
            "clip_name": (folder_paths.get_filename_list("text_encoders"),),
            "adapter_name": (folder_paths.get_filename_list(FOLDER),),
        }}

    RETURN_TYPES = ("CLIP",)
    FUNCTION = "load"
    CATEGORY = "advanced/loaders"
    DESCRIPTION = "MOSS-Image 用 Qwen3.5-4B 當文字編碼器（省記憶體）：4B 的特徵照轉接器換到 9B 的空間。接一般的 CLIP Text Encode。"

    def load(self, clip_name, adapter_name):
        path = folder_paths.get_full_path_or_raise("text_encoders", clip_name)
        clip = comfy.sd.load_clip(ckpt_paths=[path], embedding_directory=folder_paths.get_folder_paths("embeddings"),
                                  clip_type=comfy.sd.CLIPType.MOSS_IMAGE)
        if getattr(clip.cond_stage_model, "clip_name", None) != "qwen35_4b":
            raise ValueError("文字編碼器要選 Qwen3.5-4B（不是 Qwen3-VL-4B，也不是原本的 9B）")
        # MOSS 用最後一層（正規化後）的特徵；一般的 Qwen3.5 預設是倒數第二層
        inner = getattr(clip.cond_stage_model, clip.cond_stage_model.clip)
        inner.options_default = ("last", None, inner.options_default[2])
        inner.reset_clip_options()
        adapter = read_adapter(folder_paths.get_full_path_or_raise(FOLDER, adapter_name))
        return (wrap(clip, adapter),)


NODE_CLASS_MAPPINGS = {"AnimaMOSS4BCLIPLoader": AnimaMOSS4BCLIPLoader}
NODE_DISPLAY_NAME_MAPPINGS = {"AnimaMOSS4BCLIPLoader": "MOSS 4B 文字編碼器（繪匣）"}

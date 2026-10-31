import json
import math
import re

import torch
from typing_extensions import override

import comfy.text_encoders.qwen_vl
import comfy.utils
import node_helpers
from comfy_api.latest import ComfyExtension, io


class MOSSImageLayoutPrompt(io.ComfyNode):
    @classmethod
    def define_schema(cls):
        return io.Schema(
            node_id="MOSSImageLayoutPrompt",
            display_name="MOSS-Image Layout Prompt",
            category="model/conditioning/moss_image",
            description="Builds a MOSS-Image layout prompt from the prompt and the boxes from Create Bounding Boxes.",
            inputs=[
                io.String.Input("prompt", multiline=True, dynamic_prompts=True, tooltip="Description of the whole image."),
                io.Array.Input("element", tooltip="Prompt elements from the node Create Bounding Boxes. The description is the box's caption, text boxes also render their text."),
                io.Int.Input("width", default=1024, min=16, max=16384, step=16, tooltip="Width of the generated image; the boxes are placed in its pixels."),
                io.Int.Input("height", default=1024, min=16, max=16384, step=16, tooltip="Height of the generated image; the boxes are placed in its pixels."),
                io.String.Input("character_boxes", default="", tooltip="Numbers of the object boxes to label as characters instead of objects, as tagged in the Create Bounding Boxes preview, e.g. \"1, 3\"."),
            ],
            outputs=[io.String.Output(display_name="prompt")],
            is_experimental=True,
        )

    @classmethod
    def execute(cls, prompt, element, width, height, character_boxes="") -> io.NodeOutput:
        characters = {int(i) for i in re.findall(r"\d+", character_boxes)}
        items = []
        for i, el in enumerate(element if isinstance(element, list) else []):
            label = "text" if el.get("type") == "text" else ("character" if i + 1 in characters else "object")
            item = {"label": label, "caption": el.get("desc", "")}
            if item["label"] == "text":
                item["text"] = el.get("text", "")
            if el.get("color_palette"):
                item["palette"] = el["color_palette"]
            # 0-1000 [y0, x0, y1, x1] -> pixel edges enclosing the box, origin at the canvas centre
            y0, x0, y1, x1 = el["bbox"]
            item["bbox"] = [math.floor(round(y0 * height / 1000, 9)) - height // 2, math.floor(round(x0 * width / 1000, 9)) - width // 2,
                            math.ceil(round(y1 * height / 1000, 9)) - height // 2, math.ceil(round(x1 * width / 1000, 9)) - width // 2]
            items.append(item)
        if not items:
            return io.NodeOutput(prompt)
        layout = {"summary": {"caption": prompt.strip()}, "items": items, "bbox_coord_space": "abs_centralized", "bbox_format": "yxyx"}
        return io.NodeOutput(json.dumps(layout, ensure_ascii=False, separators=(",", ":")))


# edit LoRA layouts: the reference method and whether the text encoder also sees the references
# 繪匣（2026-10-30）：從 ComfyUI PR #17296／#17320 移植
EDIT_FORMATS = {
    "ai-toolkit": ("index_timestep_zero_isolated", True),
    "ai-toolkit (kv_cache off)": ("index_timestep_zero", True),
    "Moss-Image-Edit": ("index_timestep_zero_text_start", False),
}


class TextEncodeMOSSImageEdit(io.ComfyNode):
    @classmethod
    def define_schema(cls):
        return io.Schema(
            node_id="TextEncodeMOSSImageEdit",
            display_name="Text Encode MOSS-Image Edit",
            category="model/conditioning/moss_image",
            description="Encodes an edit instruction with reference images for a MOSS-Image edit LoRA, use it for the negative prompt too. The references are used at the size they come in, scale them first. Without a VAE, the images only go to the text encoder.",
            inputs=[
                io.Clip.Input("clip"),
                io.String.Input("prompt", multiline=True, dynamic_prompts=True),
                io.Vae.Input("vae", optional=True, tooltip="Encodes the images as reference latents for the edit LoRA."),
                io.Combo.Input("edit_format", options=list(EDIT_FORMATS), tooltip="The layout the edit LoRA was trained with: ai-toolkit's MOSS-Image Edit Training (its kv_cache option on or off) or Moss-Image-Edit. With kv_cache off the references also attend to the image being generated, so they are recomputed every step."),
                io.Autogrow.Input("images", template=io.Autogrow.TemplatePrefix(input=io.Image.Input("image"), prefix="image_", min=1)),
                io.Float.Input("image_strength", default=1.0, min=0.0, max=100.0, step=0.05, advanced=True, tooltip="Scales how much the model attends to the images the text encoder sees (ai-toolkit formats, or no VAE): 0 ignores them, 1 is unchanged, above 1 emphasizes them."),
            ],
            outputs=[io.Conditioning.Output()],
            is_experimental=True,
        )

    @classmethod
    def execute(cls, clip, prompt, edit_format, images, vae=None, image_strength=1.0) -> io.NodeOutput:
        method, vision = EDIT_FORMATS[edit_format]
        images = [images[name] for name in sorted(images) if images[name] is not None]
        vl = []
        if (vision or vae is None) and image_strength > 0:
            # the text encoder sees each reference downscaled to at most 384x384 pixels, after a "Picture N:" marker
            for image in images:
                scale = min(1.0, math.sqrt(384 * 384 / (image.shape[1] * image.shape[2])))
                vl.append(comfy.utils.common_upscale(image.movedim(-1, 1), round(image.shape[2] * scale), round(image.shape[1] * scale), "area", "disabled").movedim(1, -1))
            prompt = "".join("Picture {}: <|vision_start|><|image_pad|><|vision_end|>".format(i + 1) for i in range(len(vl))) + prompt
        tokens = clip.tokenize(prompt, images=vl)
        conditioning = clip.encode_from_tokens_scheduled(tokens)
        if len(vl) > 0 and image_strength != 1.0:
            # log(strength) on each image's vision tokens scales their share of attention, Qwen3.5 merges 2x2 patches of 16 px into one token
            weight = math.log(image_strength)
            bias = []
            for t in next(iter(tokens.values()))[0]:
                if isinstance(t[0], dict):
                    _, grid = comfy.text_encoders.qwen_vl.process_qwen2vl_images(t[0]["data"], patch_size=16)
                    bias += [weight] * (int(grid[0, 1] * grid[0, 2]) // 4)
                else:
                    bias.append(0.0)
            conditioning = node_helpers.conditioning_set_values(conditioning, {"context_attention_bias": torch.tensor([bias])})
        if vae is None:
            return io.NodeOutput(conditioning)
        refs = [vae.encode(image[:, :, :, :3]) for image in images]
        return io.NodeOutput(node_helpers.conditioning_set_values(conditioning, {"reference_latents": refs, "reference_latents_method": method}))


class MOSSImageExtension(ComfyExtension):
    @override
    async def get_node_list(self) -> list[type[io.ComfyNode]]:
        return [
            MOSSImageLayoutPrompt,
            TextEncodeMOSSImageEdit,
        ]


async def comfy_entrypoint() -> MOSSImageExtension:
    return MOSSImageExtension()

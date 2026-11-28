"""產生繪匣 H3 影片用的 Colab 筆記本（resources/cloud/paintdeck_h3_colab.ipynb）。

Colab 上裝 ComfyUI＋H3 節點＋模型，開一個只認口令的轉送（ComfyUI 本身沒有密碼），
再用 cloudflared（http2）開通道，印出網址和口令貼進繪匣「雲端設定 → Google Colab」。
節點的版本跟 src-tauri/src/server/api_cloud.rs 的 BOOT 同一組（UPSCALER／SELFLIFT／refmod），換版本兩邊一起改。
"""
import json
import sys

UPSCALER = ("https://github.com/LBH-123-AI/Comfyui_Minimax_h3_latent_Upscaler.git", "40316cf008b2fd8663263270669eb4da23f89d2c")
SELFLIFT = ("https://github.com/rogen84/SelfLiftH3Sampler.git", "cb772ed2a02266c9b6efefc149dae2d731d78ded")
REFMOD = ("https://github.com/Luisacaotica/ComfyUI-MiniMaxH3Mod", "517e8c419a8584fbf69fceb487bc39d83e6a6ec8", "ComfyUI-MiniMaxH3Mod")
PUBLIC_RAW = "https://raw.githubusercontent.com/sressky12-ux/PaintDeck/main/resources/extras"


def md(text):
    return {"cell_type": "markdown", "metadata": {}, "source": text.strip("\n").splitlines(keepends=True)}


def code(text):
    return {"cell_type": "code", "metadata": {}, "execution_count": None, "outputs": [], "source": text.strip("\n").splitlines(keepends=True)}


cells = [
    md("""
# 繪匣 PaintDeck：用 Colab 的顯卡跑 MiniMax H3 影片

1. 上方選單「執行階段 → 變更執行階段類型」選顯卡：**G4（96GB）跑滿血版最順**；A100 也可以；L4、T4 只能跑 int8。
2. 「執行階段 → 全部執行」，等第 4 格印出 **網址** 和 **口令**（第一次要下載模型，滿血版約 100GB，大約 10～20 分鐘）。
3. 繪匣「生圖助手 → 雲端設定 → Google Colab」貼上網址和口令，按「保存並測試連線」，之後 H3 影片台選「雲端」就會用這台。

注意：
- 這個分頁要一直開著；關掉、閒置太久或運算單元用完，Colab 會收回機器。
- 開著就在扣運算單元（G4 每小時約 9～10 個），用完記得「執行階段 → 中斷連線並刪除執行階段」。
"""),
    code("""
#@title 1. 選模型（看不懂就保持「自動」）
模型 = "自動"  #@param ["自動", "滿血 bf16（ref2va pruned，40GB）", "int8（ref2va pruned，21GB）", "10Eros Turbo int8（跟 RunPod 同一顆，8 步很快）"]
文字編碼器 = "自動"  #@param ["自動", "bf16（51GB）", "int8（27GB）", "nvfp4（16GB，要 G4 這類新卡）"]
進階包 = False  #@param {type:"boolean"}
import subprocess
out = subprocess.run(["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader,nounits"], capture_output=True, text=True).stdout.strip()
if not out:
    raise SystemExit("沒有顯卡：到「執行階段 → 變更執行階段類型」選 G4、A100 或 L4")
GPU, VRAM = out.split(",")[0].strip(), int(out.split(",")[1]) // 1024
BLACKWELL = any(k in GPU for k in ("RTX PRO 6000", "B200", "B100", "5090", "Blackwell"))
if 模型 == "自動":
    模型 = "滿血 bf16（ref2va pruned，40GB）" if VRAM >= 70 else "int8（ref2va pruned，21GB）"
if 文字編碼器 == "自動":
    文字編碼器 = "bf16（51GB）" if VRAM >= 70 else ("nvfp4（16GB，要 G4 這類新卡）" if BLACKWELL else "int8（27GB）")
print(f"顯卡：{GPU}（{VRAM}GB）")
print("模型：", 模型)
print("文字編碼器：", 文字編碼器)
"""),
    code(f"""
#@title 2. 安裝 ComfyUI 和 H3 節點（沿用 Colab 內建的 PyTorch）
import os
os.chdir("/content")
if not os.path.isdir("/content/ComfyUI"):
    !git clone -q --depth 1 https://github.com/comfyanonymous/ComfyUI.git /content/ComfyUI
!pip install -q -r /content/ComfyUI/requirements.txt
!pip install -q -U "huggingface_hub[hf_transfer]"
N = "/content/ComfyUI/custom_nodes"
def node(repo, commit, name):
    d = f"{{N}}/{{name}}"
    if not os.path.isdir(d):
        r = os.system(f"git clone -q {{repo}} {{d}} && git -C {{d}} checkout -q {{commit}}")
        print(("裝好" if r == 0 else "沒裝起來（少這個功能而已）") + "：" + name)
node("{UPSCALER[0]}", "{UPSCALER[1]}", "Comfyui_Minimax_h3_latent_Upscaler")
node("{SELFLIFT[0]}", "{SELFLIFT[1]}", "SelfLiftH3Sampler")
node("{REFMOD[0]}", "{REFMOD[1]}", "{REFMOD[2]}")
# 繪匣自己的長片節點（參考圖進影片模型本體）＋長片接續節點
os.makedirs(f"{{N}}/anima_h3_nodes", exist_ok=True)
!curl -fsSL -o {{N}}/anima_h3_nodes/__init__.py {PUBLIC_RAW}/anima_h3_nodes/__init__.py
if not os.path.isdir(f"{{N}}/HerrgottsH3Infinite"):
    !curl -fsSL -o /content/h3_infinite.tgz {PUBLIC_RAW}/h3_infinite.tar.gz && mkdir -p {{N}}/HerrgottsH3Infinite && tar xzf /content/h3_infinite.tgz --no-same-owner -C {{N}}/HerrgottsH3Infinite --strip-components=1
!python -c "import torch; print('torch', torch.__version__, '顯卡', torch.cuda.is_available())"
"""),
    code("""
#@title 3. 下載模型（從 Hugging Face，Colab 機房很快）
import os
os.environ["HF_HUB_ENABLE_HF_TRANSFER"] = "1"
from huggingface_hub import hf_hub_download
M = "/content/ComfyUI/models"
def get(repo, f, sub):
    p = hf_hub_download(repo, f, local_dir=f"/content/hf/{repo.replace('/', '__')}")
    os.makedirs(f"{M}/{sub}", exist_ok=True)
    dst = f"{M}/{sub}/{os.path.basename(f)}"
    if not os.path.exists(dst):
        os.symlink(p, dst)
    print("好了：", os.path.basename(f))
H3 = "Comfy-Org/MiniMax-H3"
if 模型.startswith("滿血"):
    get(H3, "diffusion_models/minimax_h3_ref2va_pruned_bf16.safetensors", "diffusion_models")
elif 模型.startswith("int8"):
    get(H3, "diffusion_models/minimax_h3_ref2va_pruned_int8_convrot.safetensors", "diffusion_models")
else:
    get("TenStrip/10Eros-Max", "10Eros_Max_h3_TURBO-hybrid_beta5_int8.safetensors", "diffusion_models")
te = {"bf16": "qwen3vl_32b_minimax_h3_bf16.safetensors", "int8": "qwen3vl_32b_minimax_h3_int8_convrot.safetensors", "nvfp4": "qwen3vl_32b_minimax_h3_nvfp4_awq.safetensors"}[文字編碼器.split("（")[0]]
get(H3, f"text_encoders/{te}", "text_encoders")
get(H3, "vae/minimax_h3_video_vae_int8_convrot.safetensors", "vae")
get(H3, "vae/minimax_h3_audio_vae_fp32.safetensors", "vae")
get("LBH-123-AI/Minimax_h3_latent_Upscaler", "minimax_h3_latent_upscaler_3d_conv_v1/minimax_h3_latent_upscaler_3d_conv_v1_fp16.safetensors", "latent_upscale_models")
# 官方 ref2va 沒有 Turbo：繪匣 8 步以內會自動掛這顆 DMD Turbo LoRA
get("drbaph/MiniMax-H3-Turbo-Lora-ComfyUI", "experimental/minimax_h3_dmd_ref2va_8step_turbo_pruned.safetensors", "loras")
if 進階包:
    for repo, f, sub in [
        ("drbaph/MiniMax-H3-Turbo-Lora-ComfyUI", "minimax_h3_turbo_v4_step600_ema_pruned_comfyui.safetensors", "loras"),
        ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_character_swap_v1.1_ref2va_fro.safetensors", "loras"),
        ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_vfx_edit_v1.0_r128.safetensors", "loras"),
        ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_style_transfer_v1.0_r64.safetensors", "loras"),
        ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_live_wallpaper_v1.0_ref2va_r64.safetensors", "loras"),
        ("Alissonerdx/Minimax-H3-ComfyUI", "loras/minimax_h3_lms_v1.0_r64.safetensors", "loras"),
        (H3, "model_patches/minimax_h3_fun_controlnet_union_pruned_int8_convrot.safetensors", "model_patches"),
    ]:
        get(repo, f, sub)
"""),
    code("""
#@title 4. 開 ComfyUI 和通道，印出網址與口令
import os, re, secrets, subprocess, time, urllib.request
if not os.path.isfile("/content/cloudflared"):
    urllib.request.urlretrieve("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64", "/content/cloudflared")
    os.chmod("/content/cloudflared", 0o755)
TOKEN = secrets.token_urlsafe(18)
# ComfyUI 沒有密碼：只聽本機，外面一律經過只認口令的轉送（繪匣送 Authorization: Bearer <口令>）
open("/content/gate.py", "w").write('''
import os, aiohttp
from aiohttp import web
TOKEN = os.environ["GATE_TOKEN"]
UP = "http://127.0.0.1:8188"
HOP = {"host", "authorization", "content-length", "transfer-encoding", "connection"}
async def handle(req):
    if req.headers.get("Authorization") != "Bearer " + TOKEN:
        return web.Response(status=401, text="token")
    async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None)) as s:
        async with s.request(req.method, UP + req.path_qs, headers={k: v for k, v in req.headers.items() if k.lower() not in HOP}, data=await req.read()) as r:
            resp = web.StreamResponse(status=r.status, headers={k: v for k, v in r.headers.items() if k.lower() not in HOP | {"content-encoding"}})
            await resp.prepare(req)
            async for chunk in r.content.iter_chunked(1 << 20):
                await resp.write(chunk)
            await resp.write_eof()
            return resp
app = web.Application(client_max_size=1 << 30)
app.router.add_route("*", "/{tail:.*}", handle)
web.run_app(app, host="127.0.0.1", port=8189, print=None)
''')
comfy = subprocess.Popen(["python", "/content/ComfyUI/main.py", "--listen", "127.0.0.1", "--port", "8188"], stdout=open("/content/comfy.log", "w"), stderr=subprocess.STDOUT)
gate = subprocess.Popen(["python", "/content/gate.py"], env=dict(os.environ, GATE_TOKEN=TOKEN), stdout=open("/content/gate.log", "w"), stderr=subprocess.STDOUT)
for _ in range(180):
    time.sleep(2)
    try:
        urllib.request.urlopen("http://127.0.0.1:8188/system_stats", timeout=3)
        break
    except Exception:
        if comfy.poll() is not None:
            print(open("/content/comfy.log").read()[-3000:])
            raise SystemExit("ComfyUI 沒開起來，上面是錯誤訊息")
# 預設的 QUIC 從 Colab 往外很慢（實測約 55KB/s），http2 實測 37.9MB/s
tunnel = subprocess.Popen(["/content/cloudflared", "tunnel", "--no-autoupdate", "--protocol", "http2", "--url", "http://127.0.0.1:8189"],
                          stdout=open("/content/tunnel.log", "w"), stderr=subprocess.STDOUT)
url = ""
for _ in range(60):
    time.sleep(1)
    m = re.search(r"https://[a-z0-9-]+\\.trycloudflare\\.com", open("/content/tunnel.log").read())
    if m:
        url = m.group(0)
        break
if not url:
    print(open("/content/tunnel.log").read())
    raise SystemExit("通道沒開起來，重跑這一格試試")
print("=" * 60)
print("網址：", url)
print("口令：", TOKEN)
print("=" * 60)
print("貼到繪匣「生圖助手 → 雲端設定 → Google Colab」，按保存並測試連線。")
"""),
    code("""
#@title 5. 看 ComfyUI 紀錄（讓這格一直跑，分頁不要關）
import time, subprocess
try:
    while True:
        out = subprocess.run(["tail", "-n", "2", "/content/comfy.log"], capture_output=True, text=True).stdout.strip()
        print(time.strftime("%H:%M:%S"), out.splitlines()[-1][:150] if out else "等繪匣送工作…", flush=True)
        time.sleep(60)
except KeyboardInterrupt:
    print("停止顯示（ComfyUI 照樣在跑）")
"""),
]

nb = {
    "nbformat": 4,
    "nbformat_minor": 0,
    "metadata": {"accelerator": "GPU", "colab": {"provenance": [], "gpuType": "G4"}, "kernelspec": {"name": "python3", "display_name": "Python 3"}, "language_info": {"name": "python"}},
    "cells": cells,
}
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as f:
    json.dump(nb, f, ensure_ascii=False, indent=1)
    f.write("\n")
print("ok", sys.argv[1])

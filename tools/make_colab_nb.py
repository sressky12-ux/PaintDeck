"""產生繪匣訓練用 Colab 筆記本（resources/train/paintdeck_colab.ipynb）。"""
import json
import sys

COMMIT = "9ab33b1104d71bc4c6753761e40466daaadc4a78"
AGENT_URL = "https://raw.githubusercontent.com/sressky12-ux/PaintDeck/main/resources/train/agent.py"


def md(text):
    return {"cell_type": "markdown", "metadata": {}, "source": text.strip("\n").splitlines(keepends=True)}


def code(text):
    return {"cell_type": "code", "metadata": {}, "execution_count": None, "outputs": [], "source": text.strip("\n").splitlines(keepends=True)}


cells = [
    md("""
# 繪匣 PaintDeck：用 Colab 的顯卡煉 LoRA

1. 上方選單「執行階段 → 變更執行階段類型」選 **T4 GPU**（或付費的 L4／A100）。
2. 按上方選單「執行階段 → 全部執行」，等最後一格印出 **網址** 和 **口令**（第一次大約 5～10 分鐘）。
3. 回到繪匣「訓練台 → 在哪煉 → 雲端顯卡 → 進階：自己架小幫手」，貼上網址和口令，按開始。

注意：
- 免費 T4 只有 16GB 顯存：SDXL、Anima 可以煉；Qwen、MOSS、Krea 很勉強；H3 不行。
- 這個分頁要一直開著。關掉、閒置太久或用滿時數，Colab 會收回機器，訓練會停在一半。
- 煉完記得「執行階段 → 中斷連線並刪除執行階段」。
"""),
    code("""
#@title 1. 檢查顯卡
!nvidia-smi --query-gpu=name,memory.total --format=csv,noheader || echo '沒有顯卡：到「執行階段 → 變更執行階段類型」選 T4 GPU'
"""),
    code(f"""
#@title 2. 安裝 ai-toolkit（沿用 Colab 內建的 PyTorch）
import os, subprocess, zipfile, urllib.request
COMMIT = "{COMMIT}"
os.chdir("/content")
if not os.path.isfile("/content/ai-toolkit/run.py"):
    urllib.request.urlretrieve(f"https://codeload.github.com/ostris/ai-toolkit/zip/{{COMMIT}}", "aitk.zip")
    zipfile.ZipFile("aitk.zip").extractall(".")
    os.rename(f"ai-toolkit-{{COMMIT}}", "ai-toolkit")
    os.remove("aitk.zip")
# Colab 是 Python 3.13：requirements 釘的 scipy==1.12.0 沒有現成安裝檔、建不起來，改用 Colab 內建的 scipy（2026-11-28 實測）
!cd /content/ai-toolkit && grep -v '^scipy' requirements.txt > requirements_colab.txt && pip install -q -r requirements_colab.txt
!python -c "import torch, diffusers; print('torch', torch.__version__, '顯卡', torch.cuda.is_available())"
"""),
    code(f"""
#@title 3. 開訓練小幫手和通道，印出網址與口令
import os, re, secrets, subprocess, time, urllib.request
os.makedirs("/content/train_work", exist_ok=True)
urllib.request.urlretrieve("{AGENT_URL}", "/content/agent.py")
if not os.path.isfile("/content/cloudflared"):
    urllib.request.urlretrieve("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64", "/content/cloudflared")
    os.chmod("/content/cloudflared", 0o755)
TOKEN = secrets.token_urlsafe(18)
agent = subprocess.Popen(["python3", "/content/agent.py", "--host", "127.0.0.1", "--port", "8000", "--token", TOKEN,
                          "--work", "/content/train_work", "--toolkit", "/content/ai-toolkit", "--python", "python3"],
                         stdout=open("/content/agent.log", "w"), stderr=subprocess.STDOUT)
# --protocol http2：預設的 QUIC 從 Colab 往外只有約 55KB/s，http2 實測 37.9MB/s（2026-11-28）
tunnel = subprocess.Popen(["/content/cloudflared", "tunnel", "--no-autoupdate", "--protocol", "http2", "--url", "http://127.0.0.1:8000"],
                          stdout=open("/content/tunnel.log", "w"), stderr=subprocess.STDOUT)
url = ""
for _ in range(60):
    time.sleep(1)
    m = re.search(r"https://[a-z0-9-]+\\.trycloudflare\\.com", open("/content/tunnel.log").read())
    if m:
        url = m.group(0)
        break
if agent.poll() is not None:
    print(open("/content/agent.log").read())
    raise SystemExit("訓練小幫手沒開起來，上面是錯誤訊息")
if not url:
    print(open("/content/tunnel.log").read())
    raise SystemExit("通道沒開起來，重跑這一格試試")
print("=" * 60)
print("網址：", url)
print("口令：", TOKEN)
print("=" * 60)
print("貼到繪匣「訓練台 → 雲端顯卡 → 進階：自己架小幫手」，然後按開始。")
"""),
    code("""
#@title 4. 看訓練紀錄（讓這格一直跑，分頁不要關）
import time, subprocess
try:
    while True:
        out = subprocess.run(["tail", "-n", "3", "/content/agent.log"], capture_output=True, text=True).stdout.strip()
        print(time.strftime("%H:%M:%S"), out.splitlines()[-1] if out else "等繪匣連進來…", flush=True)
        time.sleep(60)
except KeyboardInterrupt:
    print("停止顯示（小幫手照樣在跑）")
"""),
]

nb = {
    "nbformat": 4,
    "nbformat_minor": 0,
    "metadata": {"accelerator": "GPU", "colab": {"provenance": [], "gpuType": "T4"}, "kernelspec": {"name": "python3", "display_name": "Python 3"}, "language_info": {"name": "python"}},
    "cells": cells,
}
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as f:
    json.dump(nb, f, ensure_ascii=False, indent=1)
    f.write("\n")
print("ok", sys.argv[1])

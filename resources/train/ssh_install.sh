KIT="$ROOT/ai-toolkit"
# Keep an older installation available to its owner.
if [ -f "$KIT/run.py" ] && ! grep -Fxq "VERSION = \"$VERSION\"" "$KIT/version.py"; then
    KIT="$ROOT/ai-toolkit-$VERSION"
fi
if [ ! -f "$KIT/run.py" ]; then
    echo '下載訓練器中…'
    tmp=$(mktemp -d "$ROOT/install.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL --connect-timeout 30 --retry 5 -o "$tmp/aitk.zip" "https://codeload.github.com/ostris/ai-toolkit/zip/$COMMIT" || { echo '下載訓練器失敗，請檢查主機到 GitHub 的連線'; exit 1; }
    echo '解壓縮訓練器中…'
    python3 -m zipfile -e "$tmp/aitk.zip" "$tmp"
    mkdir -p "$KIT"
    cp -a "$tmp/ai-toolkit-$COMMIT/." "$KIT/"
    rm -rf "$tmp"
    trap - EXIT
fi
cd "$KIT"
check='import sys,torch,diffusers; sys.exit(0 if torch.cuda.is_available() else 1)'
PY=
echo '檢查機器上的 PyTorch 和顯卡…'
for p in "$(command -v python3)" /root/miniconda3/bin/python; do
    [ -x "$p" ] && "$p" -c 'import sys,torch; sys.exit(0 if (3,10) <= sys.version_info[:2] <= (3,12) and torch.cuda.is_available() else 1)' || continue
    echo '沿用機器上的 PyTorch，安裝訓練套件中…'
    vpy="$KIT/venv/bin/python"
    if "$p" -m venv --system-site-packages venv && "$vpy" -m pip install -r requirements.txt && "$vpy" -m pip install setuptools==69.5.1 && "$vpy" -c "$check"; then
        PY="$vpy"
        break
    fi
done
if [ -z "$PY" ]; then
    echo '準備完整訓練環境中…'
    sed -i "$CUDA_PATCH" manager/spec.py || true
    index=https://download.pytorch.org/whl/
    if [ "$mode" = cn ]; then index=https://mirrors.nju.edu.cn/pytorch/whl/; fi
    sed -i "s|^PYTORCH_INDEX = .*|PYTORCH_INDEX = \"$index\"|" manager/spec.py || true
    python3 -m manager install || echo '安裝沒跑完，檢查已有的環境…'
    for p in "$KIT/.venv/bin/python" "$KIT/venv/bin/python"; do
        if [ -x "$p" ] && "$p" -c "$check"; then PY="$p"; break; fi
    done
    [ -n "$PY" ] || { echo '訓練套件沒裝好，或 PyTorch 找不到顯卡'; exit 1; }
fi

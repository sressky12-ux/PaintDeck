#!/usr/bin/env bash
# Linux 版的 fetch_vendor.ps1：下載打包要用、但不放進版控的執行檔到 vendor/（tauri.linux.conf.json 的 bundle.resources 會用到）。
#   vendor/onnxruntime/libonnxruntime.so  ONNX Runtime 1.30.0 CPU 版（官方 GitHub Release 的 onnxruntime-linux-x64-1.30.0.tgz）：反推用。MIT（Microsoft）
# Linux 不用 7zr：Linux 的安裝精靈不裝 ComfyUI 的 Windows 免安裝版（.7z）。
# 壓縮檔和取出來的 .so 都比對 SHA-256（2026-11-07 實測）；對不上就停，不留下檔案。已經有、而且雜湊對的就跳過。
#
# 用法（在專案根目錄）：bash tools/fetch_vendor.sh [目的資料夾]
set -euo pipefail

DEST="${1:-$(cd "$(dirname "$0")/.." && pwd)/vendor}"
ORT_URL="https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-linux-x64-1.30.0.tgz"
ORT_TGZ_SHA="a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd"
ORT_INNER="onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so.1.30.0"
ORT_SHA="245a6f8c38127551057a1cd1ffd59f0a186a227ade4f3492dea2494eb565542e"
ORT_OUT="$DEST/onnxruntime/libonnxruntime.so"

sha() { sha256sum "$1" | cut -d' ' -f1; }

echo "libonnxruntime.so"
if [ -f "$ORT_OUT" ] && [ "$(sha "$ORT_OUT")" = "$ORT_SHA" ]; then
  echo "  已經有了，跳過"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl -sSLf --retry 3 -o "$work/ort.tgz" "$ORT_URL"
got="$(sha "$work/ort.tgz")"
if [ "$got" != "$ORT_TGZ_SHA" ]; then
  echo "SHA-256 不對：$ORT_URL" >&2
  echo "  應該是 $ORT_TGZ_SHA" >&2
  echo "  下載到 $got" >&2
  exit 1
fi
tar -xzf "$work/ort.tgz" -C "$work" "$ORT_INNER"
got="$(sha "$work/$ORT_INNER")"
if [ "$got" != "$ORT_SHA" ]; then
  echo "SHA-256 不對：$ORT_INNER（應該是 $ORT_SHA，取出來是 $got）" >&2
  exit 1
fi
mkdir -p "$(dirname "$ORT_OUT")"
mv "$work/$ORT_INNER" "$ORT_OUT"
echo "  好了：$ORT_OUT"

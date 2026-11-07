#!/usr/bin/env bash
# 打包 Linux 無頭版（純命令列伺服器，不連 GTK／WebKit）：
#   paintdeck-server-<版本>-linux-x64.tar.gz
#     anima-server              主程式（anima-server --help 看參數）
#     ui/、resources/           網頁、附帶的資料
#     libonnxruntime.so         反推用（tools/fetch_vendor.sh 先抓好）
#     paintdeck-server.service  systemd 使用者服務的範本
#     README-linux.txt          怎麼用
#     LICENSE、THIRD_PARTY_NOTICES.md
# 用法（在專案根目錄、Linux 上）：bash tools/linux_headless.sh [輸出資料夾，預設 src-tauri/target/release/headless]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/src-tauri/target/release/headless}"
VER="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/src-tauri/Cargo.toml" | head -1)"
NAME="paintdeck-server-$VER-linux-x64"

[ -f "$ROOT/vendor/onnxruntime/libonnxruntime.so" ] || bash "$ROOT/tools/fetch_vendor.sh"

(cd "$ROOT/src-tauri" && cargo build --release --locked --no-default-features --bin anima-server)

STAGE="$OUT/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$ROOT/src-tauri/target/release/anima-server" "$STAGE/"
cp -r "$ROOT/ui" "$STAGE/ui"
cp -r "$ROOT/resources" "$STAGE/resources"
cp "$ROOT/vendor/onnxruntime/libonnxruntime.so" "$STAGE/"
cp "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$STAGE/"

cat > "$STAGE/paintdeck-server.service" <<'EOF'
# systemd 使用者服務（開機自動開、不用登入桌面）：
#   1. 把 ExecStart 改成 anima-server 的完整路徑
#   2. mkdir -p ~/.config/systemd/user && cp paintdeck-server.service ~/.config/systemd/user/
#   3. systemctl --user daemon-reload && systemctl --user enable --now paintdeck-server
#   4. 沒登入也要跑：sudo loginctl enable-linger $USER
#   看 log：journalctl --user -u paintdeck-server -f
[Unit]
Description=PaintDeck (繪匣) headless server
After=network-online.target

[Service]
ExecStart=/home/YOUR_NAME/paintdeck-server/anima-server
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF

cat > "$STAGE/README-linux.txt" <<'EOF'
繪匣 PaintDeck 無頭版（Linux 純命令列，沒有視窗，用瀏覽器操作）

為什麼用這個：不用裝桌面環境、不開 WebKit 視窗，記憶體都留給 ComfyUI 和模型。
需要：x86_64 Linux（glibc 2.35 以上，Ubuntu 22.04／Debian 12 以後）、tar、curl。
     N 卡要裝好驅動（nvidia-smi 能用；新卡要 570 以上）。
     影音工作台要 ffmpeg：sudo apt install ffmpeg

1. 解開、執行
     tar xzf paintdeck-server-*.tar.gz
     cd paintdeck-server-*/
     ./anima-server
   第一次會進安裝精靈：可攜式 Python、ComfyUI、PyTorch、模型都由精靈下載（放在你選的資料夾，不用 root）。
   已經有 ComfyUI 的話，在精靈第二頁「自己指定」填它的資料夾（有 venv 的 git clone 也認得）。

2. 從別台電腦用（最安全：SSH 通道）
     ssh -N -L 8190:127.0.0.1:8190 使用者@這台伺服器
   再在自己電腦的瀏覽器開 http://127.0.0.1:8190/

   要在區網直接連（沒有密碼，只在信任的網路用）：
     ./anima-server --host 0.0.0.0 --allow-host 192.168.1.20
   然後開 http://192.168.1.20:8190/（--allow-host 填這台的 IP 或主機名稱，可以給好幾個）

3. 常用參數：./anima-server --help
     --port 8190      網頁的連接埠
     --config 檔案    設定檔，預設 ~/.config/local.anima.studio/config.json

4. 開機自動開：看 paintdeck-server.service 裡的說明（systemd 使用者服務）

5. 結束：Ctrl+C（或 systemctl --user stop paintdeck-server），自己開的 ComfyUI 會一起關掉。
   ComfyUI 的 log 在 <資料資料夾>/data/logs/comfyui.log。

沒有 App 內更新：到 https://github.com/sressky12-ux/PaintDeck/releases 下載新版，解開蓋掉舊的程式資料夾就好（資料在你選的資料夾，不會動）。
EOF

chmod +x "$STAGE/anima-server"
tar -C "$OUT" -czf "$OUT/$NAME.tar.gz" "$NAME"
echo "好了：$OUT/$NAME.tar.gz"
ls -la "$OUT/$NAME.tar.gz"

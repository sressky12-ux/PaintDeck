#!/usr/bin/env bash
# Linux 版冒煙測試（CI 用，不需要模型或 ComfyUI）：
#   1. anima-server（無視窗）用空的資料資料夾開起來，/api/state、首頁要回 200
#   2. AppImage 在 xvfb 裡開起來（第一次開＝安裝精靈），8190 要有回應
#   3. deb 解開看檔案有沒有放對（主程式、resources、ui、libonnxruntime.so）
#      套件名要合 Debian 規定、Depends 不重複、選單 .desktop 有中文名
#   4. 有免密碼 sudo（CI）就真的 apt 裝 deb，開裝好的 /usr/bin/anima-server，再移除
# 用法：bash tools/linux_smoke.sh <target/release 資料夾>
set -uo pipefail

REL="$(cd "${1:?要給 target/release 資料夾}" && pwd)"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
fail=0
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done; rm -rf "$work"; }
trap cleanup EXIT

wait_http() { # url 秒數
  local i
  for ((i = 0; i < $2; i++)); do
    code="$(curl -s -o /dev/null -w '%{http_code}' "$1" || true)"
    [ "$code" = 200 ] && return 0
    sleep 1
  done
  echo "  $1 等了 $2 秒還是 $code"
  return 1
}

echo "== 1. anima-server"
mkdir -p "$work/root" "$work/fakecomfy/models"
ANIMA_ROOT="$work/root" ANIMA_WEB="$ROOT/ui" ANIMA_RES="$ROOT/resources" COMFY_ROOT="$work/fakecomfy" \
  ANIMA_PORT=8191 COMFY_URL=http://127.0.0.1:9 "$REL/anima-server" >"$work/server.log" 2>&1 &
pids+=($!)
if wait_http http://127.0.0.1:8191/api/state 60 && wait_http http://127.0.0.1:8191/ 5; then
  curl -s http://127.0.0.1:8191/api/state | head -c 300; echo
  echo "  OK"
else
  fail=1; cat "$work/server.log"
fi

echo "== 1b. 無頭版壓縮檔（不能連 WebKit／GTK；沒設定檔＝網頁精靈）"
tgz="$(ls "$REL"/headless/paintdeck-server-*-linux-x64.tar.gz 2>/dev/null | head -1)"
if [ -z "$tgz" ]; then
  echo "  找不到無頭版壓縮檔"; fail=1
else
  mkdir -p "$work/hl" "$work/hlhome"
  tar -xzf "$tgz" -C "$work/hl"
  bin="$(ls "$work"/hl/*/anima-server)"
  if ldd "$bin" | grep -Ei "webkit|gtk|soup"; then
    echo "  無頭版連到了圖形函式庫"; fail=1
  else
    echo "  沒有連 GTK／WebKit"
  fi
  # 只帶最少的環境（像沒有桌面的伺服器）：沒有 DISPLAY、沒有 WAYLAND
  env -i PATH=/usr/bin:/bin HOME="$work/hlhome" "$bin" --port 8192 >"$work/hl.log" 2>&1 &
  pids+=($!)
  if wait_http http://127.0.0.1:8192/ 60 && curl -s http://127.0.0.1:8192/ | grep -q "安裝與修復"; then
    code="$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: 10.0.0.9:8192' http://127.0.0.1:8192/api/setup/status)"
    if [ "$code" = 403 ]; then echo "  精靈 OK，沒列在 --allow-host 的網址會擋（403）"; else echo "  外來網址沒擋：$code"; fail=1; fi
  else
    fail=1; cat "$work/hl.log"
  fi
fi

echo "== 2. AppImage（xvfb）"
app="$(ls "$REL"/bundle/appimage/*.AppImage 2>/dev/null | head -1)"
if [ -z "$app" ]; then
  echo "  找不到 AppImage"; fail=1
else
  chmod +x "$app"
  HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" XDG_DATA_HOME="$work/home/.local/share" \
    xvfb-run -a "$app" --appimage-extract-and-run >"$work/app.log" 2>&1 &
  pids+=($!)
  if wait_http http://127.0.0.1:8190/ 90; then
    echo "  OK"
  else
    fail=1; cat "$work/app.log"
  fi
fi

echo "== 3. deb 內容"
deb="$(ls "$REL"/bundle/deb/*.deb 2>/dev/null | head -1)"
if [ -z "$deb" ]; then
  echo "  找不到 deb"; fail=1
else
  dpkg-deb -c "$deb" > "$work/deb.txt"
  for want in usr/bin/anima-app usr/bin/anima-server /resources/poses/ /ui/index.html libonnxruntime.so; do
    if grep -q -- "$want" "$work/deb.txt"; then echo "  有 $want"; else echo "  缺 $want"; fail=1; fi
  done
  # Debian 的套件名只能是小寫 ASCII 字母數字和 + . -，開頭要是字母或數字（26.1108.1 是「繪匣-paint-deck」，dpkg 拒裝）
  pkg="$(dpkg-deb -f "$deb" Package)"
  if printf '%s' "$pkg" | grep -Eq '^[a-z0-9][a-z0-9+.-]+$'; then echo "  套件名 $pkg"; else echo "  套件名不合 Debian 規定：$pkg"; fail=1; fi
  deps="$(dpkg-deb -f "$deb" Depends)"
  echo "  Depends: $deps"
  for want in libwebkit2gtk-4.1-0 libgtk-3-0; do
    printf '%s' "$deps" | grep -q "$want" || { echo "  Depends 缺 $want"; fail=1; }
  done
  dup="$(printf '%s' "$deps" | tr ',' '\n' | sed 's/^ *//' | sort | uniq -d)"
  [ -z "$dup" ] || { echo "  Depends 重複：$dup"; fail=1; }
  dpkg-deb -x "$deb" "$work/debx"
  desk="$(ls "$work"/debx/usr/share/applications/*.desktop 2>/dev/null | head -1)"
  if [ -n "$desk" ] && grep -q '^Name\[zh_TW\]=繪匣 PaintDeck$' "$desk"; then
    echo "  選單：$(basename "$desk")（有中文名）"
    if command -v desktop-file-validate >/dev/null; then desktop-file-validate "$desk" || fail=1; fi
  else
    echo "  .desktop 不對：$desk"; fail=1
  fi

  echo "== 4. 真的用 apt 裝 deb，開裝好的 anima-server"
  if sudo -n true 2>/dev/null; then
    if sudo DEBIAN_FRONTEND=noninteractive apt-get install -y "$deb" >"$work/apt.log" 2>&1; then
      dpkg -s "$pkg" | grep -E '^(Status|Version):'
      env -i PATH=/usr/bin:/bin HOME="$work/debhome" /usr/bin/anima-server --port 8193 >"$work/deb-server.log" 2>&1 &
      pids+=($!)
      if wait_http http://127.0.0.1:8193/ 60 && curl -s http://127.0.0.1:8193/ | grep -q "安裝與修復"; then
        echo "  裝好的 anima-server 找得到網頁（精靈 OK）"
      else
        fail=1; cat "$work/deb-server.log"
      fi
      kill "${pids[-1]}" 2>/dev/null
      sudo apt-get remove -y "$pkg" >/dev/null 2>&1 && echo "  已移除"
    else
      echo "  apt 裝不起來"; cat "$work/apt.log"; fail=1
    fi
  else
    echo "  沒有免密碼 sudo，跳過"
  fi
fi

exit $fail

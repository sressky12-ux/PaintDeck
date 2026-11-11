#!/usr/bin/env python3
"""繪匣訓練小幫手：站在 ai-toolkit 旁邊的小 HTTP 服務，只用 Python 標準庫。

本機（繪匣在 127.0.0.1 開它）和雲端訓練機（開機腳本開它）用同一支、同一套協定：
  GET  /status          狀態、步數、loss、最後幾行 log、產出的檔案
  POST /dataset         body＝zip，解到 work/data（先清空）
  POST /start           {"config": "<yaml>", "name": "..."}，開始訓練
  POST /stop            停掉訓練（整棵行程樹）
  GET  /file?name=...   下載 work/out 底下的檔
每個請求都要帶 X-Train-Token 標頭。

用法：python agent.py --port 8000 --token <口令> --work <工作資料夾> --toolkit <ai-toolkit 資料夾> [--host 127.0.0.1] [--watchdog 秒]
測試可以用環境變數 TRAIN_CMD（JSON 陣列，{config} 會換成設定檔路徑）換掉真正的訓練指令。
"""
import argparse
import hmac
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

LOG_KEEP = 400
LOG_SEND = 40
MAX_ZIP = 20 * 1024 ** 3
PROGRESS = re.compile(r"(\d+)/(\d+)")
LOSS = re.compile(r"loss: ([0-9.eE+-]+)")
SAMPLE_EXT = (".jpg", ".jpeg", ".png", ".webp", ".mp4", ".webm", ".gif")


class State:
    def __init__(self, args):
        self.args = args
        self.work = os.path.abspath(args.work)
        self.data = os.path.join(self.work, "data")
        self.out = os.path.join(self.work, "out")
        self.lock = threading.Lock()
        self.proc = None
        self.state = "idle"
        self.step = 0
        self.total = 0
        self.want_total = 0
        self.loss = None
        self.log = []
        self.error = ""
        self.seen = time.monotonic()
        os.makedirs(self.data, exist_ok=True)
        os.makedirs(self.out, exist_ok=True)

    def running(self):
        return self.proc is not None and self.proc.poll() is None

    def files(self):
        found = []
        for root, _dirs, names in os.walk(self.out):
            for n in names:
                low = n.lower()
                rel = os.path.relpath(os.path.join(root, n), self.out).replace("\\", "/")
                # 點開頭的資料夾（ai-toolkit 的 .thumbs 縮圖）不是產物
                if any(part.startswith(".") for part in rel.split("/")):
                    continue
                if low.endswith(".safetensors"):
                    kind = "lora"
                elif low.endswith(SAMPLE_EXT) and "/samples/" in "/" + rel:
                    kind = "sample"
                else:
                    continue
                try:
                    st = os.stat(os.path.join(root, n))
                except OSError:
                    continue
                found.append({"name": rel, "size": st.st_size, "kind": kind, "mtime": int(st.st_mtime)})
        found.sort(key=lambda f: f["name"])
        return found

    def status(self):
        with self.lock:
            return {
                "state": self.state,
                "step": self.step,
                "total": self.total,
                "loss": self.loss,
                "log": self.log[-LOG_SEND:],
                "error": self.error,
                "files": self.files(),
                "data_dir": self.data,
                "out_dir": self.out,
                "toolkit": os.path.isfile(os.path.join(self.args.toolkit, "run.py")),
            }

    def feed(self, line):
        line = line.strip()
        if not line:
            return
        with self.lock:
            self.log.append(line[-500:])
            del self.log[:-LOG_KEEP]
            for m in PROGRESS.finditer(line):
                step, total = int(m.group(1)), int(m.group(2))
                # 設定檔有寫總步數就只認那一條進度條（前面還有「整理圖片 3/6」這種別的進度）
                if total > 0 and step <= total and (self.want_total == 0 or total == self.want_total):
                    self.step, self.total = step, total
            m = LOSS.search(line)
            if m:
                try:
                    self.loss = float(m.group(1))
                except ValueError:
                    pass

    def pump(self, proc):
        buf = b""
        while True:
            chunk = proc.stdout.read1(4096)
            if not chunk:
                break
            buf += chunk
            parts = re.split(rb"[\r\n]", buf)
            buf = parts.pop()
            for p in parts:
                self.feed(p.decode("utf-8", "replace"))
        self.feed(buf.decode("utf-8", "replace"))
        rc = proc.wait()
        with self.lock:
            if self.proc is not proc:
                return
            if self.state == "stopped":
                pass
            elif rc == 0:
                self.state = "done"
            else:
                self.state = "error"
                tail = " ｜ ".join(self.log[-3:])
                self.error = f"訓練程式結束了（代碼 {rc}）：{tail}"[:1500]

    def start(self, config, name):
        with self.lock:
            if self.running():
                return False
            cfg_path = os.path.join(self.work, "config.yaml")
            with open(cfg_path, "w", encoding="utf-8", newline="\n") as f:
                f.write(config)
            m = re.search(r"^\s*steps:\s*(\d+)\s*$", config, re.M)
            self.want_total = int(m.group(1)) if m else 0
            custom = os.environ.get("TRAIN_CMD")
            if custom:
                cmd = [c.replace("{config}", cfg_path) for c in json.loads(custom)]
                cwd = self.work
            else:
                cmd = [self.args.python or sys.executable, "run.py", cfg_path]
                cwd = self.args.toolkit
            env = dict(os.environ, PYTHONUNBUFFERED="1", PYTHONIOENCODING="utf-8", TRAIN_OUT=self.out, TRAIN_DATA=self.data, TRAIN_NAME=str(name or ""))
            env.setdefault("HF_HOME", os.path.join(os.path.dirname(self.work), "hf"))
            env.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")
            kw = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt" else {"start_new_session": True}
            self.proc = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, **kw)
            self.state, self.step, self.total, self.loss, self.error, self.log = "running", 0, self.want_total, None, "", []
            threading.Thread(target=self.pump, args=(self.proc,), daemon=True).start()
            return True

    def stop(self):
        with self.lock:
            proc = self.proc
            if proc is None or proc.poll() is not None:
                return
            self.state = "stopped"
        try:
            if os.name == "nt":
                subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True, check=False)
            else:
                os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            proc.kill()

    def unzip(self, path):
        """解到 work/data（先清空）。有 .. 或絕對路徑就整包不收。回傳檔案數。"""
        with zipfile.ZipFile(path) as z:
            names = [i for i in z.infolist() if not i.is_dir()]
            root = os.path.realpath(self.data)
            for i in names:
                n = i.filename.replace("\\", "/")
                if n.startswith("/") or re.match(r"^[A-Za-z]:", n) or ".." in n.split("/"):
                    raise ValueError("壓縮檔裡有不安全的路徑：" + i.filename)
                dest = os.path.realpath(os.path.join(root, n))
                if os.path.commonpath([root, dest]) != root:
                    raise ValueError("壓縮檔裡有不安全的路徑：" + i.filename)
            shutil.rmtree(self.data, ignore_errors=True)
            os.makedirs(self.data, exist_ok=True)
            for i in names:
                dest = os.path.join(root, i.filename.replace("\\", "/"))
                os.makedirs(os.path.dirname(dest), exist_ok=True)
                with z.open(i) as src, open(dest, "wb") as dst:
                    shutil.copyfileobj(src, dst)
            return len(names)


class Handler(BaseHTTPRequestHandler):
    server_version = "PaintDeckTrain/1"
    protocol_version = "HTTP/1.1"
    st = None

    def log_message(self, *_):
        pass

    def reply(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authed(self):
        got = self.headers.get("X-Train-Token") or ""
        if hmac.compare_digest(got.encode(), self.st.args.token.encode()):
            self.st.seen = time.monotonic()
            return True
        # 沒讀完的 body 要丟掉，不然同一條連線下一個請求會讀到垃圾
        self.close_connection = True
        self.reply(401, {"error": "口令不對"})
        return False

    def body(self, limit):
        n = int(self.headers.get("Content-Length") or 0)
        if n < 0 or n > limit:
            self.close_connection = True
            raise ValueError("內容太大")
        return n

    def do_GET(self):
        if not self.authed():
            return
        url = urlparse(self.path)
        if url.path == "/status":
            return self.reply(200, self.st.status())
        if url.path == "/file":
            name = (parse_qs(url.query).get("name") or [""])[0].replace("\\", "/")
            root = os.path.realpath(self.st.out)
            if not name or name.startswith("/") or re.match(r"^[A-Za-z]:", name) or ".." in name.split("/"):
                return self.reply(400, {"error": "檔名不對"})
            path = os.path.realpath(os.path.join(root, name))
            if os.path.commonpath([root, path]) != root:
                return self.reply(400, {"error": "檔名不對"})
            if not os.path.isfile(path):
                return self.reply(404, {"error": "沒有這個檔"})
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(os.path.getsize(path)))
            self.end_headers()
            with open(path, "rb") as f:
                shutil.copyfileobj(f, self.wfile, 1024 * 1024)
            return None
        return self.reply(404, {"error": "沒有這個路徑"})

    def do_POST(self):
        if not self.authed():
            return None
        url = urlparse(self.path)
        try:
            if url.path == "/dataset":
                if self.st.running():
                    self.close_connection = True
                    return self.reply(409, {"error": "訓練正在跑，不能換資料集"})
                n = self.body(MAX_ZIP)
                tmp = os.path.join(self.st.work, "dataset.zip.part")
                with open(tmp, "wb") as f:
                    left = n
                    while left > 0:
                        chunk = self.rfile.read(min(left, 1024 * 1024))
                        if not chunk:
                            raise ValueError("上傳中斷")
                        f.write(chunk)
                        left -= len(chunk)
                try:
                    count = self.st.unzip(tmp)
                except (zipfile.BadZipFile, ValueError) as e:
                    return self.reply(400, {"error": str(e) or "壓縮檔壞了"})
                finally:
                    try:
                        os.remove(tmp)
                    except OSError:
                        pass
                return self.reply(200, {"ok": True, "files": count})
            n = self.body(8 * 1024 * 1024)
            req = json.loads(self.rfile.read(n) or b"{}")
            if url.path == "/start":
                config = req.get("config")
                if not isinstance(config, str) or not config.strip():
                    return self.reply(400, {"error": "沒有設定檔"})
                if not self.st.start(config, req.get("name")):
                    return self.reply(409, {"error": "已經在跑"})
                return self.reply(200, {"ok": True})
            if url.path == "/stop":
                self.st.stop()
                return self.reply(200, {"ok": True})
        except (ValueError, json.JSONDecodeError) as e:
            return self.reply(400, {"error": str(e)})
        except OSError as e:
            return self.reply(500, {"error": str(e)})
        return self.reply(404, {"error": "沒有這個路徑"})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8000)
    ap.add_argument("--token", default=os.environ.get("TRAIN_TOKEN", ""))
    ap.add_argument("--work", required=True)
    ap.add_argument("--toolkit", required=True)
    ap.add_argument("--python", default="")
    ap.add_argument("--watchdog", type=int, default=0, help="這麼多秒沒有人帶口令來問，就把訓練停掉、自己結束（0＝不管）")
    args = ap.parse_args()
    if len(args.token) < 8:
        sys.exit("口令至少要 8 個字（--token 或環境變數 TRAIN_TOKEN）")
    Handler.st = State(args)
    srv = ThreadingHTTPServer((args.host, args.port), Handler)
    srv.daemon_threads = True
    print(f"TRAIN_AGENT_READY port={srv.server_address[1]}", flush=True)
    if args.watchdog > 0:
        # 繪匣被關掉或當掉時不會有人來收尾：沒人問就自己停，不要留一個訓練行程占著顯卡
        def watch():
            while time.monotonic() - Handler.st.seen < args.watchdog:
                time.sleep(0.5)
            Handler.st.stop()
            srv.shutdown()

        threading.Thread(target=watch, daemon=True).start()
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        Handler.st.stop()


if __name__ == "__main__":
    main()

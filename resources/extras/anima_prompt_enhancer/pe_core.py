"""Qwen-Image 2.1 提示詞改寫（通用模型 Qwen3.5 9B／Qwen3.6 35B；官方 PE Heretic 2026-10-30 起拿掉）：llama-server 管理、請求組裝、輸出解析、快取。

只用標準函式庫，ComfyUI 節點（__init__.py）和 Anima 生圖站（anima_ui/app/pe.py）共用這一份：
  - 同一個 port（127.0.0.1:8199），同一時間只會有一個 llama-server。
  - 狀態寫在 server_state.json（pid、哪個模型、GPU 層數、最後使用時間），誰啟動的都看得到、都能關。
  - 啟動／關閉用跨程序的檔案鎖（server.lock）保護，不會兩邊同時開。
  - 快取在 cache.sqlite：輸入＋任務＋參數＋seed 一樣就直接回傳上次的結果。
"""
import base64
import ctypes
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, "bin", "llama-server.exe")
PROMPTS = os.path.join(HERE, "prompts")
STATE = os.path.join(HERE, "server_state.json")
LOCK = os.path.join(HERE, "server.lock")
LOG = os.path.join(HERE, "llama-server.log")
CACHE = os.path.join(HERE, "cache.sqlite")
# 模型放在 ComfyUI\models\LLM（本機是 junction 到 D:\AI\ComfyUI_models\LLM）；可用環境變數 ANIMA_PE_MODELS 改位置
MODELS_DIR = os.environ.get("ANIMA_PE_MODELS") or os.path.normpath(os.path.join(HERE, "..", "..", "models", "LLM"))
HOST, PORT = "127.0.0.1", 8199
URL = f"http://{HOST}:{PORT}"
CTX = 16384

# 任務 → (系統提示詞檔, 用哪個模型)。改寫、微調、中文摘要都可以用目前已載入的模型，只有讀圖一定要 i2i
TASKS = {
    "t2i": ("t2i_official.txt", "t2i"),      # Qwen 2.1／Noct Q 文生圖（官方 JSON 版）
    "i2i": ("i2i_official.txt", "i2i"),      # 圖生圖、局部重繪、多圖參考（官方 JSON 版，要讀圖）
    "anima": ("anima.txt", "t2i"),           # Anima 動漫：tag＋英文句子（生圖站自己寫的）
    "refine": ("refine.txt", "t2i"),         # 微調：上一版＋調整指令，只改提到的部分
    "zh": ("zh_summary.txt", "t2i"),         # 英文結果 → 繁體中文摘要
    "caption": ("caption.txt", "i2i"),       # 看圖寫英文說明（含畫風），反推視窗和 LoRA 訓練圖用
}
CAPTION_ASK = "Write the caption for this image."
NOCT_ANIME_HEAD = "An anime illustration of"


class PEError(Exception):
    pass


# ---------------------------------------------------------------- 模型檔
def _words(name):
    """檔名拆成小寫的字（配看圖檔用，跟生圖站 pe.rs 的 name_words 一樣）"""
    return {w for w in re.split(r"[^a-z0-9]+", name.lower()) if w and w not in ("gguf", "mmproj")}


def _big(name):
    """30B 以上（35B 那類，跟生圖站 pe.rs 的 is_big_brain 一樣看檔名裡的 xxB）"""
    return any(w[:-1].isdigit() and int(w[:-1]) >= 30 for w in _words(name) if w.endswith("b"))


def _llm_files():
    """MODELS_DIR 底下（含子資料夾，最多 4 層）的 gguf，照路徑排序"""
    out = []
    for root, dirs, files in os.walk(MODELS_DIR):
        if os.path.relpath(root, MODELS_DIR).count(os.sep) >= 4:
            dirs[:] = []
        out += [os.path.join(root, f) for f in files if f.lower().endswith(".gguf")]
    return sorted(out)


def model_files(kind, quant=None):
    """回傳 (模型路徑, mmproj 路徑或 None)。找不到就丟 PEError。
    2026-10-30 起官方 PE（pe_*_heretic）拿掉了，照生圖站 pe.rs 自動挑的順序用通用模型：
    有 Qwen3.6 35B 先用它，沒有就 Qwen3.5 9B（Defiant Fable 優先，再來 abliterated、任何一顆 Qwen3.5）。
    kind＝i2i 要讀圖，配同系列的看圖檔（mmproj）。quant 是以前挑 PE 量化版用的，留著相容、沒作用"""
    chat, mmprojs = [], []
    for p in _llm_files():
        n = os.path.basename(p).lower()
        if "mmproj" in n:
            mmprojs.append(p)
        elif "embedding" not in n and not n.startswith("pe_"):
            chat.append((p, n))

    def pick(want):
        return next((p for p, n in chat if want(n)), None)
    model = (pick(lambda n: "qwen3.6" in n and _big(n))
             or pick(lambda n: "qwen3.5" in n and "defiant-fable" in n)
             or pick(lambda n: "qwen3.5" in n and "abliterated" in n)
             or pick(lambda n: "qwen3.5" in n and not _big(n)))
    if not model:
        raise PEError(f"找不到 AI 改寫用的通用模型（Qwen3.5-9B-The-Defiant-Fable*.gguf 或 Qwen3.6-35B-A3B*.gguf，放在 {MODELS_DIR}）。"
                      "到繪匣的「安裝與修復」勾「AI 改寫、生圖助手」會下載 9B；以前的官方 PE 改寫模型已經不再支援")
    if kind != "i2i":
        return model, None
    name = os.path.basename(model)
    fam = "qwen3.6" if "qwen3.6" in name.lower() else "qwen3.5"
    mm = sorted((p for p in mmprojs if fam in os.path.basename(p).lower()),
                key=lambda p: (-len(_words(os.path.basename(p)) & _words(name)), p))
    if not mm:
        raise PEError(f"找不到 {name} 讀圖用的 mmproj 檔（放在 {MODELS_DIR}，檔名要同時有「mmproj」和「{fam}」）")
    return model, mm[0]


def available():
    out = {}
    for k in ("t2i", "i2i"):
        try:
            p, mm = model_files(k)
            out[k] = os.path.basename(p)
        except PEError:
            out[k] = None
    out["server"] = os.path.exists(BIN)
    return out


def system_prompt(task):
    with open(os.path.join(PROMPTS, TASKS[task][0]), encoding="utf-8") as f:
        return f.read().strip()


# ---------------------------------------------------------------- 程序與狀態
def _pid_alive(pid):
    if not pid:
        return False
    k32 = ctypes.windll.kernel32
    h = k32.OpenProcess(0x1000, False, int(pid))  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return False
    code = ctypes.c_ulong()
    ok = k32.GetExitCodeProcess(h, ctypes.byref(code))
    k32.CloseHandle(h)
    return bool(ok) and code.value == 259  # STILL_ACTIVE


def read_state():
    try:
        with open(STATE, encoding="utf-8") as f:
            s = json.load(f)
    except (OSError, ValueError):
        return {}
    return s if _pid_alive(s.get("pid")) else {}


def _write_state(s):
    tmp = STATE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(s, f)
    os.replace(tmp, STATE)


def touch():
    """記錄最後使用時間（閒置自動關閉用）。"""
    s = read_state()
    if s:
        s["last_used"] = time.time()
        _write_state(s)


def set_busy(n):
    s = read_state()
    if s:
        s["busy"] = max(0, int(n))
        s["last_used"] = time.time()
        _write_state(s)


class _FileLock:
    """跨程序互斥（ComfyUI 和生圖站各自是不同的 Python 程序）。"""

    def __init__(self, path, timeout=180):
        self.path, self.timeout, self.f = path, timeout, None

    def __enter__(self):
        import msvcrt
        self.f = open(self.path, "a+")
        t0 = time.time()
        while True:
            try:
                self.f.seek(0)
                msvcrt.locking(self.f.fileno(), msvcrt.LK_NBLCK, 1)
                return self
            except OSError:
                if time.time() - t0 > self.timeout:
                    self.f.close()
                    raise PEError("等 llama-server 啟動／關閉逾時（另一邊卡住了？）")
                time.sleep(0.2)

    def __exit__(self, *a):
        import msvcrt
        try:
            self.f.seek(0)
            msvcrt.locking(self.f.fileno(), msvcrt.LK_UNLCK, 1)
        finally:
            self.f.close()


def health(timeout=1.5):
    try:
        with urllib.request.urlopen(URL + "/health", timeout=timeout) as r:
            return r.status == 200
    except Exception:
        return False


def _port_owner():
    """8199 被誰占用（沒有狀態檔的孤兒程序）。只回傳 llama-server.exe 的 pid。"""
    try:
        # 主控台輸出是系統的 OEM 字碼頁（繁中 cp950、簡中 GBK）；ComfyUI 用 UTF-8 模式開時 text=True 會照 UTF-8 解而出錯
        out = subprocess.run(["netstat", "-ano", "-p", "TCP"], capture_output=True, text=True, encoding="oem", errors="replace",
                             timeout=10, creationflags=0x08000000).stdout
    except Exception:
        return None
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 5 and parts[1].endswith(f":{PORT}") and parts[3] == "LISTENING":
            pid = int(parts[4])
            try:
                img = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"], capture_output=True,
                                     text=True, encoding="oem", errors="replace", timeout=10, creationflags=0x08000000).stdout
            except Exception:
                return None
            return pid if "llama-server" in img.lower() else None
    return None


def _no_throttle(pid):
    """關掉 Windows 對這個程序的節能限速（EcoQoS／Power Throttling）。
    從背景程序（沒有視窗的生圖站伺服器、ComfyUI）啟動的 llama-server 會被當成背景工作降速，實測 57 → 26 token／秒。"""
    class _State(ctypes.Structure):
        _fields_ = [("Version", ctypes.c_ulong), ("ControlMask", ctypes.c_ulong), ("StateMask", ctypes.c_ulong)]
    k32 = ctypes.windll.kernel32
    h = k32.OpenProcess(0x0200 | 0x1000, False, int(pid))  # PROCESS_SET_INFORMATION | QUERY_LIMITED
    if not h:
        return False
    try:
        ok = False
        # EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION 都由我們控制，State=0＝不限速。
        # Windows 10 不認得 IGNORE_TIMER_RESOLUTION（錯誤 87，整個設定不生效）：失敗就只關執行速度限制（2026-10-09 修正）
        for mask in (0x1 | 0x4, 0x1):
            st = _State(1, mask, 0)
            if k32.SetProcessInformation(h, 4, ctypes.byref(st), ctypes.sizeof(st)):  # ProcessPowerThrottling
                ok = True
                break
        k32.SetPriorityClass(h, 0x8000)  # ABOVE_NORMAL_PRIORITY_CLASS：生圖站背景執行時也不被搶走 CPU
        return bool(ok)
    finally:
        k32.CloseHandle(h)


def _kill(pid):
    subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True, creationflags=0x08000000)
    for _ in range(50):
        if not _pid_alive(pid):
            return
        time.sleep(0.1)


def stop(reason=""):
    """關掉 llama-server（不管是誰開的）。回傳是否真的有關到東西。"""
    with _FileLock(LOCK):
        return _stop_locked(reason)


def _stop_locked(reason=""):
    s = read_state()
    pids = [s.get("pid")] if s else []
    orphan = _port_owner()
    if orphan and orphan not in pids:
        pids.append(orphan)
    for pid in pids:
        if pid:
            _kill(pid)
    try:
        os.remove(STATE)
    except OSError:
        pass
    if pids:
        _log(f"stop pid={pids} {reason}")
    return bool(pids)


def _log(msg):
    try:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(f"[anima-pe {time.strftime('%Y-%m-%d %H:%M:%S')}] {msg}\n")
    except OSError:
        pass


def log_tail(n=15):
    try:
        with open(LOG, encoding="utf-8", errors="replace") as f:
            lines = f.readlines()[-n:]
        return "".join(lines)
    except OSError:
        return ""


def ensure(kind, ngl=99, before_start=None, timeout=180, quant=None):
    """確保 llama-server 跑著對的模型（kind＝t2i／i2i）和對的 GPU 層數。
    已經在跑就直接用；不對就重開。before_start()：真的要啟動前呼叫（生圖站用來叫 ComfyUI 釋放顯存）。
    回傳 dict：{"started": 是否新啟動, "load_seconds": 載入秒數}"""
    model, mmproj = model_files(kind, quant)
    # 狀態檔的 kind 寫成生圖站 pe.rs 用的名字（adult＝純文字、adult_vision＝加看圖檔），兩邊才能共用已經載好的模型
    want = {"kind": "adult_vision" if mmproj else "adult", "ngl": int(ngl), "model": os.path.basename(model)}
    with _FileLock(LOCK):
        s = read_state()
        if s and all(s.get(k) == v for k, v in want.items()) and health():
            s["last_used"] = time.time()
            _write_state(s)
            return {"started": False, "load_seconds": 0.0}
        _stop_locked(f"重開：要 {want}，原本 {s or '沒在跑'}")
        if before_start:
            before_start(int(ngl))
        args = [BIN, "-m", model, "--host", HOST, "--port", str(PORT), "-c", str(CTX), "-ngl", str(int(ngl)),
                "--jinja", "--no-webui", "-np", "1", "--reasoning-format", "deepseek"]
        if mmproj:
            args += ["--mmproj", mmproj]
            if int(ngl) == 0:
                args += ["--no-mmproj-offload"]
        env = dict(os.environ)
        if int(ngl) == 0:
            args += ["--device", "none"]  # 真正的 CPU 模式：連提示詞處理都不碰顯卡
        _log("start " + " ".join(os.path.basename(a) if os.path.isabs(a) else a for a in args))
        logf = open(LOG, "a", encoding="utf-8", errors="replace")
        t0 = time.time()
        proc = subprocess.Popen(args, stdout=logf, stderr=subprocess.STDOUT, cwd=os.path.dirname(BIN), env=env,
                                creationflags=0x08000000)  # CREATE_NO_WINDOW
        logf.close()
        try:
            _no_throttle(proc.pid)
        except Exception as e:  # noqa: BLE001
            _log(f"關閉節能限速失敗：{e}")
        while True:
            if proc.poll() is not None:
                raise PEError(f"llama-server 啟動失敗（結束碼 {proc.returncode}）。log 最後幾行：\n{log_tail()}")
            if health(timeout=1):
                break
            if time.time() - t0 > timeout:
                _kill(proc.pid)
                raise PEError(f"llama-server {timeout} 秒內沒有載入完成，已關閉。log 最後幾行：\n{log_tail()}")
            time.sleep(0.3)
        load = time.time() - t0
        _write_state({**want, "pid": proc.pid, "started": time.time(), "last_used": time.time(), "busy": 0,
                      "load_seconds": round(load, 2)})
        _log(f"ready pid={proc.pid} load={load:.1f}s")
        return {"started": True, "load_seconds": load}


# ---------------------------------------------------------------- 請求
def image_part(png_bytes, mime="image/png"):
    return {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{base64.b64encode(png_bytes).decode()}"}}


def build_messages(task, idea="", images=(), prev="", instruction="", noct_anime=False, ratio=""):
    """組出 chat messages。images：PNG bytes 的 list（只有 i2i 用）。"""
    sys_p = system_prompt(task)
    if task == "refine":
        segs = "\n".join(f"[{i + 1}] {s}" for i, (s, _) in enumerate(split_segments(prev)))
        content = f"SEGMENTS:\n{segs}\n\nCURRENT RATIO: {ratio or 'unknown'}\n\nCHANGE REQUEST:\n{instruction.strip()}"
    elif task == "zh":
        content = idea.strip()
    elif task == "caption":
        content = [image_part(b) for b in images[:1]] + [{"type": "text", "text": idea.strip() or CAPTION_ASK}]
    elif task == "i2i":
        parts = []
        if len(images) > 1:
            for i, b in enumerate(images):
                parts += [{"type": "text", "text": f"<image{i + 1}>"}, image_part(b)]
        else:
            parts += [image_part(b) for b in images]
        text = idea.strip()
        if prev and instruction:
            text = (f"{text}\n\nA previous rewrite of this instruction was:\n{prev.strip()}\n"
                    f"Rewrite it again with only this adjustment, keeping everything else the same: {instruction.strip()}")
        parts.append({"type": "text", "text": text})
        content = parts
    else:
        text = idea.strip()
        if noct_anime:
            text += f"\n\n(The image is an anime illustration. The description must begin with the exact words \"{NOCT_ANIME_HEAD}\".)"
        content = text
    return [{"role": "system", "content": sys_p}, {"role": "user", "content": content}]


THINK_BUDGET = 1024  # 開思考時最多想幾個 token（llama-server 的 thinking_budget_tokens；跟生圖站 pe.rs 的 PE_THINK_BUDGET 一樣）


def payload(messages, temperature=1.0, seed=0, think=False, max_tokens=None, stream=True):
    """取樣參數照 ComfyUI 官方 Qwen-Image 2.1 範本的 TextGenerate：temperature 1、top_k 20、top_p 0.95、重複懲罰 1.05。"""
    body = {
        "messages": messages, "stream": stream,
        "temperature": float(temperature), "top_k": 20, "top_p": 0.95, "min_p": 0.0, "repeat_penalty": 1.05,
        "seed": int(seed) % (2 ** 31), "max_tokens": int(max_tokens or (6144 if think else 2048)),
        "chat_template_kwargs": {"enable_thinking": bool(think)},
        "timings_per_token": False,
    }
    if think:
        body["thinking_budget_tokens"] = THINK_BUDGET
    return body


def sse_delta(line):
    """解析一行 SSE：回傳 ("reasoning"|"content"|"done"|None, 文字或 dict)。"""
    if not line.startswith("data:"):
        return None, None
    data = line[5:].strip()
    if data == "[DONE]":
        return "done", None
    try:
        j = json.loads(data)
    except ValueError:
        return None, None
    if j.get("timings") and not (j.get("choices") or [{}])[0].get("delta"):
        return "timings", j["timings"]
    ch = (j.get("choices") or [{}])[0]
    d = ch.get("delta") or {}
    if d.get("reasoning_content"):
        return "reasoning", d["reasoning_content"]
    if d.get("content"):
        return "content", d["content"]
    if j.get("timings"):
        return "timings", j["timings"]
    return None, None


def stream_chat(body, on_event=None, cancel=None, timeout=600):
    """同步串流（ComfyUI 節點用）。回傳 (content, reasoning, timings)。cancel：threading.Event。"""
    req = urllib.request.Request(URL + "/v1/chat/completions", json.dumps(body).encode("utf-8"),
                                 {"Content-Type": "application/json"})
    content, reasoning, timings = [], [], {}
    with urllib.request.urlopen(req, timeout=timeout) as r:
        for raw in r:
            if cancel is not None and cancel.is_set():
                raise PEError("已取消")
            kind, val = sse_delta(raw.decode("utf-8", "replace").strip())
            if kind == "content":
                content.append(val)
            elif kind == "reasoning":
                reasoning.append(val)
            elif kind == "timings":
                timings = val
            elif kind == "done":
                break
            if kind in ("content", "reasoning") and on_event:
                on_event(kind, val)
    return "".join(content), "".join(reasoning), timings


# ---------------------------------------------------------------- 微調：切句、套補丁
# 微調不讓模型整段重寫（實測 PE 會順手改掉沒提到的句子、又漏改該改的），改成：程式把提示詞切成編號段落，
# 模型只回「第幾段換成什麼、刪哪段、在哪段後面加什麼」，沒點到的段落原封不動。
_SENT = re.compile(r"(?<=[.!?。])\s+(?=[A-Z\"“<(（])")


def _is_tags(s):
    s = s.strip()
    return bool(s) and not s[0].isupper() and s.count(",") >= 2 and not s.endswith((".", "!", "?"))


def split_segments(text):
    """→ [(段落文字, "tags"|"prose")]。Anima 格式（tag 串＋英文句子）的 tag 串算一段，其餘一句一段。"""
    t = " ".join((text or "").split())
    out = []
    if t and not t[0].isupper():
        # tag 串和後面的英文句子之間可能是逗號，也可能是句點（"…, atmospheric depth. A single…"）
        m = re.search(r"[,.]\s+(?=[A-Z])", t)
        head = t[:m.start()] if m else t
        if _is_tags(head + ","):
            end = "." if m and t[m.start()] == "." else ""
            out.append((head.strip().rstrip(",") + end, "tags"))
            t = t[m.end():] if m else ""
    out += [(s.strip(), "prose") for s in _SENT.split(t) if s.strip()]
    return out


def join_segments(segs):
    out = ""
    for i, (s, kind) in enumerate(segs):
        if i:
            p, pk = segs[i - 1]
            out += ", " if pk == "tags" and not p.endswith((".", "!", "?")) else " "
        out += s
    return out


def apply_patch(prev, obj):
    """把模型回的補丁套到上一版。回傳 (新提示詞, 改了幾段)；補丁格式不對回傳 (None, 0)。"""
    segs = split_segments(prev)
    rep, dele, ins = obj.get("replace") or {}, obj.get("delete") or [], obj.get("insert") or []
    if not isinstance(rep, dict) or not isinstance(dele, list) or not isinstance(ins, list):
        return None, 0
    n = 0
    new = []
    for i, (s, kind) in enumerate(segs, 1):
        for it in ins:
            if isinstance(it, dict) and str(it.get("after")) == str(i - 1) and str(it.get("text") or "").strip():
                t = str(it["text"]).strip()
                new.append((t, "tags" if _is_tags(t) else "prose"))
                n += 1
        if i in {int(x) for x in dele if str(x).isdigit()}:
            n += 1
            continue
        r = rep.get(str(i), rep.get(i))
        if isinstance(r, str) and r.strip() and r.strip() != s:
            s = r.strip().rstrip(",") if kind == "tags" else r.strip()
            n += 1
        new.append((s, kind))
    for it in ins:
        if isinstance(it, dict) and str(it.get("after")) == str(len(segs)) and str(it.get("text") or "").strip():
            t = str(it["text"]).strip()
            new.append((t, "tags" if _is_tags(t) else "prose"))
            n += 1
    return join_segments(new), n


_RATIO_SENT = re.compile(r"[^.]*\b(?:aspect ratio|\d{3,4}\s*[x×]\s*\d{3,4}|\d+\s*:\s*\d+\s+(?:ratio|format|frame|aspect))\b[^.]*\.\s*", re.I)


def strip_ratio(p):
    """官方規則：比例、解析度不寫進提示詞（模型偶爾會補一句 The aspect ratio is 2:3.）。"""
    return " ".join(_RATIO_SENT.sub("", p).split()) or p


# ---------------------------------------------------------------- 解析輸出
_THINK = re.compile(r"<think>.*?</think>", re.S)
_CAPTION_META = re.compile(r"\b(?:let me|double-check|constraints?|forbidden|instructions? say|my (?:sentence|caption|draft)|"
                           r"sentence \d|draft:|one sentence caption|wait,|check:)", re.I)


def parse_output(text, task="t2i", prev=""):
    """從模型輸出拿出 {"prompt", "wh_ratio", "ratio_follow", "repaired"}；完全救不回來回傳 None。
    中文摘要（zh）是純文字，直接回傳；微調（refine）的輸出是補丁，套到 prev 上，另外回傳 "changed"（改了幾段）。"""
    t = _THINK.sub("", text or "").strip()
    if "</think>" in t:
        t = t.split("</think>", 1)[1].strip()
    if task == "zh":
        return {"prompt": t, "wh_ratio": "", "ratio_follow": "", "repaired": False} if t else None
    if task == "caption":  # 純文字；偶爾會加「Caption:」或整段包在引號裡
        t = re.sub(r"^(?:caption|description)\s*:\s*", "", t, flags=re.I).strip().strip('"“”').strip()
        # PE 模型偶爾把自己的檢查過程（Let me double-check… / Sentence 1…）一起寫出來：當成失敗，讓呼叫端換種子重寫
        if len(t) <= 20 or _CAPTION_META.search(t):
            return None
        return {"prompt": " ".join(t.split()), "wh_ratio": "", "ratio_follow": "", "repaired": False}
    t = re.sub(r"^```(?:json)?\s*|\s*```$", "", t.strip())
    if task == "refine":
        a, b = t.find("{"), t.rfind("}")
        try:
            obj = json.loads(t[a:b + 1]) if a >= 0 and b > a else None
        except ValueError:
            obj = None
        if isinstance(obj, dict) and not obj.get("rewritten_prompt"):
            p, n = apply_patch(prev, obj)
            if p is None:
                return None
            ratio = str(obj.get("wh_ratio") or "").replace(" ", "")
            return {"prompt": strip_ratio(p), "wh_ratio": ratio if re.fullmatch(r"\d+(\.\d+)?:\d+(\.\d+)?", ratio) else "",
                    "ratio_follow": "", "repaired": False, "changed": n}
    a, b = t.find("{"), t.rfind("}")
    repaired = False
    obj = None
    if a >= 0 and b > a:
        chunk = t[a:b + 1]
        try:
            obj = json.loads(chunk)
        except ValueError:
            # 常見壞法：字串裡有真的換行、結尾多逗號、單引號
            fix = re.sub(r",\s*}", "}", chunk.replace("\r", " ").replace("\n", " "))
            try:
                obj, repaired = json.loads(fix), True
            except ValueError:
                obj = None
    elif a >= 0:  # 被截斷：只有開頭沒有結尾
        m = re.search(r'"rewritten_prompt"\s*:\s*"(.*)', t[a:], re.S)
        if m:
            obj, repaired = {"rewritten_prompt": m.group(1).rstrip('"} \n')}, True
    if obj is None:
        m = re.search(r'"rewritten_prompt"\s*:\s*"((?:[^"\\]|\\.)*)"', t, re.S)
        if m:
            try:
                obj = {"rewritten_prompt": json.loads(f'"{m.group(1)}"')}
            except ValueError:
                obj = {"rewritten_prompt": m.group(1)}
            r = re.search(r'"wh_ratio"\s*:\s*"([^"]*)"', t)
            if r:
                obj["wh_ratio"] = r.group(1)
            repaired = True
    if obj is None:
        # 沒有 JSON 但看起來是一整段描述（例如用了純文字版系統提示詞）：當成結果，標記為修補
        if len(t) > 80 and "{" not in t:
            return {"prompt": " ".join(t.split()), "wh_ratio": "", "ratio_follow": "", "repaired": True}
        return None
    p = obj.get("rewritten_prompt") or obj.get("prompt") or ""
    if not isinstance(p, str) or not p.strip():
        return None
    ratio = str(obj.get("wh_ratio") or "").strip()
    if ratio and not re.fullmatch(r"\d+(\.\d+)?\s*:\s*\d+(\.\d+)?", ratio):
        ratio = ""
    return {"prompt": strip_ratio(" ".join(p.split())), "wh_ratio": ratio.replace(" ", ""),
            "ratio_follow": str(obj.get("ratio_follow") or ""), "repaired": repaired}


def fix_noct_anime(p):
    """Noct Q Anime：提示詞一定要以 An anime illustration of 開頭（沒寫 anime 會畫成照片）。"""
    if p.lower().startswith(NOCT_ANIME_HEAD.lower()):
        return p
    m = re.match(r"^(?:The image is|This is|A|An)\s+(?:a|an)?\s*(?:[\w-]+\s+){0,4}?(?:illustration|photograph|image|scene|portrait|picture|artwork)\s+of\s+",
                 p, re.I)
    rest = p[m.end():] if m else p[0].lower() + p[1:]
    return f"{NOCT_ANIME_HEAD} {rest}"


# ---------------------------------------------------------------- 快取
_cache_lock = threading.Lock()


def _cache_con():
    con = sqlite3.connect(CACHE, timeout=10)
    con.execute("CREATE TABLE IF NOT EXISTS cache (key TEXT PRIMARY KEY, value TEXT, created REAL)")
    return con


def cache_key(**parts):
    """輸入＋任務＋參數＋seed＋系統提示詞內容＋模型檔名 → 雜湊。圖片用內容雜湊。"""
    norm = {}
    for k, v in parts.items():
        if k == "images":
            v = [hashlib.sha256(b).hexdigest() for b in v or ()]
        norm[k] = v
    task = parts.get("task")
    if task in TASKS:
        norm["_sys"] = hashlib.sha256(system_prompt(task).encode()).hexdigest()[:16]
    return hashlib.sha256(json.dumps(norm, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def cache_get(key):
    with _cache_lock:
        con = _cache_con()
        try:
            r = con.execute("SELECT value FROM cache WHERE key=?", (key,)).fetchone()
        finally:
            con.close()
    return json.loads(r[0]) if r else None


def cache_put(key, value):
    with _cache_lock:
        con = _cache_con()
        try:
            con.execute("INSERT OR REPLACE INTO cache VALUES(?,?,?)", (key, json.dumps(value, ensure_ascii=False), time.time()))
            con.commit()
        finally:
            con.close()


# ---------------------------------------------------------------- 一次完整改寫（同步版，ComfyUI 節點用）
def rewrite(task, idea="", images=(), prev="", instruction="", temperature=1.0, seed=0, think=False, ngl=99,
            noct_anime=False, ratio="", before_start=None, on_event=None, cancel=None, use_cache=True, retries=1):
    """回傳 dict：prompt、wh_ratio、ratio_follow、reasoning、seconds、load_seconds、cached、fallback、note、tokens。"""
    key = cache_key(task=task, idea=idea, images=list(images), prev=prev, instruction=instruction, ratio=ratio,
                    temperature=round(float(temperature), 3), seed=int(seed), think=bool(think), noct_anime=bool(noct_anime),
                    model=os.path.basename(model_files("i2i" if images else TASKS[task][1])[0]))
    if use_cache:
        hit = cache_get(key)
        if hit:
            return {**hit, "cached": True, "seconds": 0.0, "load_seconds": 0.0}
    kind = "i2i" if (task == "i2i" or images) else TASKS[task][1]
    st = read_state()
    if task in ("zh", "refine") and st.get("kind") in ("adult", "adult_vision") and int(st.get("ngl", -1)) == int(ngl):
        kind = "i2i" if st["kind"] == "adult_vision" else "t2i"  # 純文字工作用目前已載入的模型，不為了它重開
    t0 = time.time()
    info = ensure(kind, ngl, before_start=before_start)
    msgs = build_messages(task, idea, images, prev, instruction, noct_anime, ratio)
    last_raw, reasoning, timings, res = "", "", {}, None
    set_busy(1)
    try:
        for attempt in range(retries + 1):
            body = payload(msgs, temperature, int(seed) + attempt, think)
            last_raw, reasoning, timings = stream_chat(body, on_event, cancel)
            res = parse_output(last_raw, task, prev)
            if res:
                break
            _log(f"輸出無法解析（第 {attempt + 1} 次）：{last_raw[:200]!r}")
    finally:
        set_busy(0)
    out = {"reasoning": reasoning, "seconds": round(time.time() - t0, 2), "load_seconds": round(info["load_seconds"], 2),
           "cached": False, "tokens": timings.get("predicted_n"), "tps": timings.get("predicted_per_second"), "model_kind": kind}
    if not res:
        return {**out, "prompt": idea if task != "refine" else prev, "wh_ratio": "", "ratio_follow": "", "fallback": True,
                "note": "模型輸出的格式壞掉、重試後還是讀不出來，先退回原文。", "raw": last_raw[-2000:]}
    p = res["prompt"]
    if noct_anime and task != "zh":
        p = fix_noct_anime(p)
    out.update(prompt=p, wh_ratio=res["wh_ratio"], ratio_follow=res["ratio_follow"], fallback=False, note=result_note(res))
    cache_put(key, {k: out[k] for k in ("prompt", "wh_ratio", "ratio_follow", "reasoning", "fallback", "note", "tokens")})
    return out


def result_note(res):
    if res.get("changed") == 0:
        return "模型這次沒有改動任何句子，可以換個說法再調整一次。"
    return "輸出的 JSON 有小問題，已自動修補。" if res["repaired"] else ""

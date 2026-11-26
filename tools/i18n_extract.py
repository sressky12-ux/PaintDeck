"""介面英文字典（2026-10-20）：抽出要翻的中文字串、合併翻好的批次、產生 ui\\i18n\\en.json。

  python -I tools/i18n_extract.py extract   抽字串 → tools\\i18n\\todo_NN.json（只含 en_src.json 還沒有的；每批約 250 筆）
  （把 todo_NN.json 交給翻譯，結果存成 tools\\i18n\\done_NN.json：{"原文": "English", ...}）
  python -I tools/i18n_extract.py build     done_*.json 併進 tools\\i18n\\en_src.json → 產生 ui\\i18n\\en.json（只留這次抽得到的）

來源：
  ui\\index.html  的文字與 title／placeholder／aria-label／data-label
  ui\\js\\*.js    的字串（"…"、'…'、`…${x}…`；含 HTML 的字串拆成標籤之間的文字）
  src-tauri\\src  回給網頁的訊息（bad(…)、err(…)、emit(…)、format!(…)、("key", "說明")… 的字串；format! 的 {} 變成 {0}{1}…）
  resources\\poses\\poses.json  姿勢選單的名稱、分類、說明；EXTRA＝觀景窗被切開的片段
  resources\\h3\\templates.json  H3 影片台的玩法：名稱、分類、說明、小技巧、素材格子、填空的標籤和提示（英文的提示詞骨架不用翻）
`${…}`、{} 這些變數一律變成 {0}、{1}…（en.json 的 patterns），執行時 i18n.js 用正規式比對。
只抽含中文的字串；給 AI 看的提示詞（Qwen 改圖指令等）會混進來，翻了也不會出現在畫面上，無害。
"""
import html
import json
import pathlib
import re
import sys
from html.parser import HTMLParser

ROOT = pathlib.Path(__file__).resolve().parent.parent
WORK = ROOT / "tools" / "i18n"
OUT = ROOT / "ui" / "i18n" / "en.json"
CJK = re.compile(r"[㐀-鿿豈-﫿]")
BATCH = 250
MAX_LEN = 600

entries = {}  # 原文 → {"ctx": 出處, "pattern": bool}


def add(text, ctx, pattern=False):
    t = text.strip()
    if not t or not CJK.search(t) or len(t) > MAX_LEN:
        return
    if pattern and not re.search(r"\{\d+\}", t):
        pattern = False
    # 整段只有變數加標點（例如 "{0}・{1}"）沒意義
    if pattern and not CJK.search(re.sub(r"\{\d+\}", "", t)):
        return
    if t not in entries:
        entries[t] = {"ctx": ctx[:160], "pattern": pattern}


def renumber(s):
    """把片段裡的 {n} 照出現順序重編成 {0}{1}…"""
    k = iter(range(1000))
    return re.sub(r"\{\d+\}", lambda m: "{%d}" % next(k), s)


def add_markup(text, ctx, pattern=False):
    """含 HTML 標籤的字串：只取標籤之間的文字（畫面上每段是一個 text node）"""
    if "<" in text and re.search(r"<[a-zA-Z/][^>]*>", text):
        for seg in re.split(r"<[^>]*>", text):
            seg = html.unescape(seg)
            add(renumber(seg) if pattern else seg, ctx, pattern)
    else:
        add(text, ctx, pattern)


# ---------------------------------------------------------------- HTML
class H(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.stack = []

    def handle_starttag(self, tag, attrs):
        if tag not in ("br", "img", "input", "meta", "link", "hr", "source"):
            self.stack.append(tag)
        for k, v in attrs:
            if k in ("title", "placeholder", "aria-label", "data-label") and v:
                add(v, f"index.html <{tag} {k}>")

    def handle_endtag(self, tag):
        while self.stack and self.stack.pop() != tag:
            pass

    def handle_data(self, data):
        if self.stack and self.stack[-1] in ("script", "style", "textarea", "code", "pre"):
            return
        add(data, f"index.html <{self.stack[-1] if self.stack else ''}>")


# ---------------------------------------------------------------- JS
ESC = {"n": "\n", "t": "\t", "r": "\r", "b": "\b", "f": "\f", "v": "\v", "0": "\0"}


def js_unescape(s):
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c != "\\" or i + 1 >= len(s):
            out.append(c)
            i += 1
            continue
        d = s[i + 1]
        if d == "u" and s[i + 2:i + 3] == "{":
            j = s.index("}", i)
            out.append(chr(int(s[i + 3:j], 16)))
            i = j + 1
        elif d == "u":
            out.append(chr(int(s[i + 2:i + 6], 16)))
            i += 6
        elif d == "x":
            out.append(chr(int(s[i + 2:i + 4], 16)))
            i += 4
        elif d == "\n":
            i += 2
        else:
            out.append(ESC.get(d, d))
            i += 2
    return "".join(out)


REGEX_PREV = set("(,=:[!&|?{};+-*%<>~^")
REGEX_WORDS = {"return", "typeof", "case", "of", "in", "delete", "void", "throw", "new", "else", "do"}


class JS:
    def __init__(self, src, name):
        self.s, self.n, self.name = src, len(src), name

    def ctx(self, i):
        line = self.s.count("\n", 0, i) + 1
        return f"{self.name}:{line}"

    def string(self, i, q):
        j, buf = i + 1, []
        while j < self.n:
            c = self.s[j]
            if c == "\\":
                buf.append(self.s[j:j + 2])
                j += 2
                continue
            if c == q or c == "\n":
                break
            buf.append(c)
            j += 1
        add_markup(js_unescape("".join(buf)), self.ctx(i))
        return j + 1

    def template(self, i):
        j, parts = i + 1, [""]
        start = i
        while j < self.n:
            c = self.s[j]
            if c == "\\":
                parts[-1] += js_unescape(self.s[j:j + 2])
                j += 2
                continue
            if c == "`":
                j += 1
                break
            if c == "$" and self.s[j + 1:j + 2] == "{":
                j = self.code(j + 2, True)
                parts.append(None)
                parts.append("")
                continue
            parts[-1] += c
            j += 1
        k = 0
        text = ""
        for p in parts:
            if p is None:
                text += "{%d}" % k
                k += 1
            else:
                text += p.replace("{", "｛").replace("}", "｝") if False else p
        add_markup(text, self.ctx(start), pattern=k > 0)
        return j

    def regex(self, i):
        j, cls = i + 1, False
        while j < self.n:
            c = self.s[j]
            if c == "\\":
                j += 2
                continue
            if c == "\n":
                break
            if c == "[":
                cls = True
            elif c == "]":
                cls = False
            elif c == "/" and not cls:
                j += 1
                break
            j += 1
        while j < self.n and self.s[j].isalpha():
            j += 1
        return j

    def code(self, i, in_expr=False):
        """掃程式碼；in_expr＝在 ${ } 裡，碰到對應的 } 就回傳它後面的位置"""
        depth, prev, word = 0, "", ""
        while i < self.n:
            c = self.s[i]
            nx = self.s[i + 1:i + 2]
            if c in " \t\r\n":
                i += 1
                continue
            if c == "/" and nx == "/":
                i = self.s.find("\n", i)
                i = self.n if i < 0 else i
                continue
            if c == "/" and nx == "*":
                i = self.s.find("*/", i + 2)
                i = self.n if i < 0 else i + 2
                continue
            if c in "\"'":
                i = self.string(i, c)
                prev, word = "a", ""
                continue
            if c == "`":
                i = self.template(i)
                prev, word = "a", ""
                continue
            if c == "/" and (prev == "" or prev in REGEX_PREV or word in REGEX_WORDS):
                i = self.regex(i)
                prev, word = "a", ""
                continue
            if c.isalnum() or c in "_$":
                j = i
                while j < self.n and (self.s[j].isalnum() or self.s[j] in "_$"):
                    j += 1
                word = self.s[i:j]
                prev = "a"
                i = j
                continue
            if in_expr:
                if c == "{":
                    depth += 1
                elif c == "}":
                    if depth == 0:
                        return i + 1
                    depth -= 1
            prev, word = c, ""
            i += 1
        return i


# ---------------------------------------------------------------- Rust
RS_MSG = re.compile(r"(bad|err|Err|sse_error|not_found|internal|emit|say|phase|problem|set_status|ComfyError|GraphError|ok_or|ok_or_else|map_err|note|toast|ApiError|reject|explain|friendly|format)\s*[(!]|\"(msg|error|text|note|message|label|title|hint|reason)\"\s*:")
# ("key", "中文說明") 這種對照表（例：maint.rs 恢復原廠的項目）
RS_PAIR = re.compile(r"\(\s*\"[^\"\n]{1,200}\"\s*(?:,\s*\"[^\"\n]{0,200}\"\s*)*,\s*$")  # 也收 ("tall right panel", "右邊直長那格")、("L3", "三格：…")；三欄以上的也收（setup.rs 的 PACKS：代號, 名字, 說明, …）
# 結構欄位 note: "…"（例：api_cloud.rs 的顯示卡清單）
RS_FIELD = re.compile(r"\b(note|label|title|hint|desc|text|message|msg|reason|error|tip)\s*:\s*$")


def rust_fmt(s):
    k = iter(range(1000))
    s = s.replace("{{", "\0L").replace("}}", "\0R")
    s = re.sub(r"\{[^{}]*\}", lambda m: "{%d}" % next(k), s)
    return s.replace("\0L", "{").replace("\0R", "}")


def rust(src, name):
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            i = src.find("\n", i)
            i = n if i < 0 else i
            continue
        if src.startswith("/*", i):
            i = src.find("*/", i + 2)
            i = n if i < 0 else i + 2
            continue
        if c == "r" and re.match(r'r#*"', src[i:i + 8]) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            m = re.match(r'r(#*)"', src[i:])
            end = '"' + m.group(1)
            j = src.find(end, i + len(m.group(0)))
            i = n if j < 0 else j + len(end)
            continue
        if c == "'":
            m = re.match(r"'(\\.|\\u\{[0-9a-fA-F]+\}|[^\\'])'", src[i:])
            i += len(m.group(0)) if m else 1
            continue
        if c == '"':
            j, buf = i + 1, []
            while j < n and src[j] != '"':
                if src[j] == "\\":
                    buf.append(src[j:j + 2])
                    j += 2
                    continue
                buf.append(src[j])
                j += 1
            raw = "".join(buf)
            before = src[max(0, i - 120):i]
            stmt = before[max(before.rfind(";"), before.rfind("{\n"), before.rfind("=>")) + 1:]
            if RS_MSG.search(stmt) or RS_MSG.search(before[-40:]) or RS_PAIR.search(before) or RS_FIELD.search(before):
                text = re.sub(r"\\\n\s*", "", raw)
                text = js_unescape(text)
                fmt = rust_fmt(text)
                add(fmt, f"{name}:{src.count(chr(10), 0, i) + 1}", pattern=fmt != text or "{0}" in fmt)
            i = j + 1
            continue
        i += 1


def extract_all():
    for page in ("index.html", "setup.html"):  # setup.html＝第一次啟動精靈／安裝與修復
        p = H()
        p.feed((ROOT / "ui" / page).read_text("utf-8"))
    for f in sorted((ROOT / "ui" / "js").glob("*.js")):
        JS(f.read_text("utf-8"), f"js/{f.name}").code(0)
    for f in sorted((ROOT / "src-tauri" / "src").rglob("*.rs")):
        rust(f.read_text("utf-8"), str(f.relative_to(ROOT / "src-tauri" / "src")).replace("\\", "/"))
    # 姿勢大彙整：選單上的名稱、分類、滑過去的說明（/api/poses）
    poses = json.loads((ROOT / "resources" / "poses" / "poses.json").read_text("utf-8"))
    for p in poses.get("poses", []):
        for k in ("zh", "category", "geometry_zh"):
            if isinstance(p.get(k), str):
                add(p[k], f"poses.json {p.get('id')}.{k}")
    # 拍攝單鏡位小抄：按鈕和提示說明也會顯示在介面。
    cheat = json.loads((ROOT / "resources" / "h3" / "sheet_rules.json").read_text("utf-8"))["cheat"]
    for group in cheat.values():
        for label, zh, _en in group:
            add(label, "sheet_rules.json cheat label")
            add(zh, "sheet_rules.json cheat tooltip")
    # H3 影片台的玩法（/api/h3/info 的 templates，js/h3.js 照原樣顯示）
    for t in json.loads((ROOT / "resources" / "h3" / "templates.json").read_text("utf-8")):
        tid = t.get("id")
        for k in ("name_zh", "group_zh", "desc_zh", "tips_zh"):
            v = t.get(k)
            for x in v if isinstance(v, list) else [v]:  # tips_zh 有的是一句、有的是好幾條（網頁一條一行）
                add(str(x or ""), f"h3 templates {tid}.{k}")
        add(str((t.get("media") or {}).get("notes_zh") or ""), f"h3 templates {tid}.notes_zh")
        for s in t.get("slots", []):
            add(str(s.get("role") or ""), f"h3 templates {tid}.slot")
        for i in t.get("inputs", []):
            for k in ("label_zh", "hint_zh"):
                add(str(i.get(k) or ""), f"h3 templates {tid}.{k}")
    # 觀景窗、狀態列的字會被 deck.js 切成「中文一段、英數一段」（VT323 字型沒有中文），切開的片段要能單獨查到
    for t in EXTRA:
        add(t, "deck.js 切開的片段")
    return entries


EXTRA = ["佇列", "待命", "改寫", "執行中", "待機", "無法使用", "連線中", "已連線", "顯存", "記憶體",
         "機器還開著：開關機和計費在那家網站，用完記得去關機", "這台主機已有別的訓練在跑", "訓練用的連接埠已被占用", "沒有讀到訓練小幫手的程序編號",
         "臉", "手", "臉或手"]  # 後三個：api_play.rs「這張圖找不到{臉／手}…」的變數


def load_src():
    p = WORK / "en_src.json"
    return json.loads(p.read_text("utf-8")) if p.exists() else {}


def main(cmd):
    WORK.mkdir(parents=True, exist_ok=True)
    ent = extract_all()
    src = load_src()
    if cmd == "extract":
        todo = [{"zh": k, "ctx": v["ctx"], "pattern": v["pattern"]} for k, v in ent.items() if k not in src]
        for old in WORK.glob("todo_*.json"):
            old.unlink()
        for b in range(0, len(todo), BATCH):
            (WORK / f"todo_{b // BATCH:02d}.json").write_text(json.dumps(todo[b:b + BATCH], ensure_ascii=False, indent=1), "utf-8")
        print(f"抽到 {len(ent)} 筆（句型 {sum(v['pattern'] for v in ent.values())}），還沒翻的 {len(todo)} 筆 → {(len(todo) + BATCH - 1) // BATCH} 批")
    elif cmd == "build":
        bad = []
        for f in sorted(WORK.glob("done_*.json")):
            for k, v in json.loads(f.read_text("utf-8")).items():
                # 空字串＝這段在英文裡故意不顯示（例：「跳到第 [ ] 頁」的「頁」）；只有空白的當作沒翻
                if not isinstance(v, str) or (v and not v.strip()):
                    continue
                # 變數 {n} 要原封不動
                if sorted(re.findall(r"\{\d+\}", k)) != sorted(re.findall(r"\{\d+\}", v)):
                    bad.append((f.name, k, v))
                    continue
                src[k] = v
        (WORK / "en_src.json").write_text(json.dumps(src, ensure_ascii=False, indent=1, sort_keys=True), "utf-8")
        exact, pats = {}, []
        for k, v in ent.items():
            if k in src:
                if v["pattern"]:
                    pats.append([k, src[k]])
                else:
                    exact[k] = src[k]
        # 「讀不到：」這種冒號結尾的前綴：i18n.js 照標點切段時會把冒號切掉，多放一份沒冒號的
        for k, v in list(exact.items()):
            if k[-1:] in "：:" and k[:-1].strip() and k[:-1] not in exact:
                exact[k[:-1]] = v.rstrip().rstrip(":").rstrip()
        # 長的句型先比（比較具體）
        pats.sort(key=lambda x: -len(re.sub(r"\{\d+\}", "", x[0])))
        OUT.parent.mkdir(parents=True, exist_ok=True)
        OUT.write_text(json.dumps({"exact": exact, "patterns": pats}, ensure_ascii=False, separators=(",", ":")), "utf-8")
        miss = [k for k in ent if k not in src]
        print(f"en.json：整句 {len(exact)}、句型 {len(pats)}；還沒翻 {len(miss)}；變數對不上被丟掉 {len(bad)}")
        for b in bad[:20]:
            print("  變數不符：", b)
    else:
        print(__doc__)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "")

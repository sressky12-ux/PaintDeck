"""Danbooru wiki 沒有漢字別名的角色：拿 wiki 的日文名去 Bangumi（bgm.tv）找中文名。2026-11-17

  python -I tools/bangumi_fetch.py <wiki_chars.json> <輸出 bangumi_chars.json> [tag 清單.txt]

每個 tag 取 other_names 裡前兩個含假名的日文名當關鍵字，搜 bgm.tv 角色；只收「Bangumi 的日文名」跟關鍵字完全一樣
（去掉空白、・）的那一個，拿它的簡體中文名、第二中文名、別名裡的中文。找不到記 null。每次間隔 0.7 秒、可以中斷再跑。
有給 tag 清單就只查清單裡的（清單外的 wiki 不管）。只帶通用 User-Agent，不送個人資料。
"""
import json
import pathlib
import re
import sys
import time
import urllib.request

UA = "PaintDeck-tagtool/1.0"
KANA = re.compile(r"[ぁ-ヺー-ヿ]")
HAN = re.compile(r"[㐀-鿿]")
norm = lambda s: re.sub(r"[\s・·=＝]", "", s or "")


def search(kw):
    req = urllib.request.Request(
        "https://api.bgm.tv/v0/search/characters?limit=5",
        data=json.dumps({"keyword": kw}).encode(),
        headers={"User-Agent": UA, "Content-Type": "application/json"},
    )
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r).get("data") or []
        except Exception as e:
            print("  重試", attempt + 1, kw, e, flush=True)
            time.sleep(4 * (attempt + 1))
    return []


def zh_names(c):
    out = []
    for item in c.get("infobox") or []:
        v = item.get("value")
        if item.get("key") == "简体中文名" and isinstance(v, str):
            out.append(v)
        elif item.get("key") == "别名" and isinstance(v, list):
            for x in v:
                if x.get("k") in ("第二中文名", "中文名", None) and isinstance(x.get("v"), str) and HAN.search(x["v"]) and not KANA.search(x["v"]):
                    out.append(x["v"])
    return out


def main():
    wiki = json.loads(pathlib.Path(sys.argv[1]).read_text("utf-8"))
    out_p = pathlib.Path(sys.argv[2])
    only = set(pathlib.Path(sys.argv[3]).read_text("utf-8").split()) if len(sys.argv) > 3 else None
    out = json.loads(out_p.read_text("utf-8")) if out_p.exists() else {}
    # wiki 已經有漢字別名（沒有假名、韓文）的不用查
    has_han = lambda w: any(HAN.search(n) and not KANA.search(n) and not re.search(r"[가-힯]", n) for n in w.get("other_names") or [])
    todo = [t for t, w in wiki.items() if (only is None or t in only) and t not in out and w and not has_han(w)
            and any(KANA.search(n) for n in w.get("other_names") or [])]
    print("要查", len(todo), flush=True)
    for i, t in enumerate(todo):
        kws = [n for n in wiki[t].get("other_names") or [] if KANA.search(n)][:2]
        hit = None
        for kw in kws:
            kw_clean = re.sub(r"\(.*?\)$", "", kw).strip()
            for c in search(kw_clean):
                if norm(c.get("name")) == norm(kw_clean):
                    hit = {"kw": kw_clean, "id": c.get("id"), "cands": zh_names(c)}
                    break
            time.sleep(0.7)
            if hit:
                break
        out[t] = hit
        if i % 100 == 0:
            out_p.write_text(json.dumps(out, ensure_ascii=False), "utf-8")
            print(f"  {i}/{len(todo)}", flush=True)
    out_p.write_text(json.dumps(out, ensure_ascii=False), "utf-8")
    print("完成", sum(1 for v in out.values() if v), "/", len(out))


if __name__ == "__main__":
    main()

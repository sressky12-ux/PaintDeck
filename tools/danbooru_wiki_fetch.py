"""抓 Danbooru wiki 的 other_names（各語言的別名，人工維護）和 body，給 tools\\weilin_tags.mjs 校對角色翻譯用。2026-11-17

  python -I tools/danbooru_wiki_fetch.py <tag 清單.txt> <輸出.json>

tag 清單一行一個（danbooru 原名，底線格式）。一次查 50 個，每次間隔 0.6 秒；輸出 {tag: {"other_names": [...], "body": "..."}}，
已經抓過的會跳過（可以中斷後再跑）。查不到 wiki 的 tag 記成 null。不送任何個人資料，只帶通用 User-Agent。
"""
import json
import pathlib
import sys
import time
import urllib.parse
import urllib.request

BATCH = 50
UA = "PaintDeck-tagtool/1.0"


def fetch(titles):
    q = [("search[title_array][]", t) for t in titles]
    q += [("only", "title,other_names,body,is_deleted"), ("limit", str(len(titles) + 10))]
    url = "https://danbooru.donmai.us/wiki_pages.json?" + urllib.parse.urlencode(q)
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=40) as r:
                return json.load(r)
        except Exception as e:  # 429／網路：等一下再試
            print("  重試", attempt + 1, e, flush=True)
            time.sleep(5 * (attempt + 1))
    raise SystemExit("一直抓不到，先停")


def main():
    tags = [t.strip() for t in pathlib.Path(sys.argv[1]).read_text("utf-8").splitlines() if t.strip()]
    out_p = pathlib.Path(sys.argv[2])
    out = json.loads(out_p.read_text("utf-8")) if out_p.exists() else {}
    todo = [t for t in dict.fromkeys(tags) if t not in out]
    print(f"共 {len(tags)}，要抓 {len(todo)}", flush=True)
    for i in range(0, len(todo), BATCH):
        chunk = todo[i:i + BATCH]
        got = {p["title"]: p for p in fetch(chunk) if not p.get("is_deleted")}
        for t in chunk:
            p = got.get(t)
            out[t] = {"other_names": p.get("other_names") or [], "body": (p.get("body") or "")[:1500]} if p else None
        if (i // BATCH) % 20 == 0:
            out_p.write_text(json.dumps(out, ensure_ascii=False), "utf-8")
            print(f"  {i + len(chunk)}/{len(todo)}", flush=True)
        time.sleep(0.6)
    out_p.write_text(json.dumps(out, ensure_ascii=False), "utf-8")
    print("完成", len(out))


if __name__ == "__main__":
    main()

r"""產生 resources\share\lora_covers.json：程式附帶說明（lora設定.json）的 LoRA 各自的封面圖來源（2026-11-07）。

  python -I -X utf8 tools/lora_covers.py              全部重查（約 8 分鐘）＋下載縮圖
  python -I -X utf8 tools/lora_covers.py --images     只照現有的 lora_covers.json 補下載縮圖（沒有的才抓）

內建縮圖（2026-11-08，使用者：「看能不能直接把圖片先直接匯入進去。就不用讓他們抓了」；網友的電腦連不上 C 站，卡在「正在抓封面…」）：
每筆照 civitai → modelscope → hf 的順序抓到第一張，縮成寬 COVER_W 的 WebP 存進 resources\share\lora_covers\<網址雜湊>.webp，
清單那筆記 "file"。後端看到 "file" 就直接用程式附的圖，完全不連網；縮圖不見了才照舊去抓。

為什麼要有：lora設定.json 裡的 "preview" 只是檔名，圖本身在使用者自己的 <資料>\previews\（用 Civitai 下載時才會存）。
網盤包、安裝精靈下載的 LoRA 沒有預覽圖，LoRA 庫整片黑（網友：「这些预载的可以显示预览图吗，全黑有点不好找」）。
開站後網頁要顯示卡片時，後端（server\api_misc.rs 的 lora_cover）照這份清單依序抓圖、存進 <資料>\previews\。

每個 LoRA 的來源，照這個順序找（使用者：「優先用c站/魔塔的封面預覽」）：
1. C 站：lora設定.json 有 versionId 就查那個版本；沒有就用安裝清單（models_full／models_basic.json）的 SHA256 查 by-hash；
   版本本身沒有圖就看同一個模型的其他版本。挑圖的規則跟 civitai.rs 下載 LoRA 存預覽圖一樣：只看圖片（不要影片）、尺度最低的那張。
2. 魔塔：能在魔塔找到同一個檔（搜尋結果的檔案清單裡有一樣的 SHA256）才算；
   從魔塔合集（gordonz/Excellent-Loras-for-MossImage）下載的，用合集的封面。
3. 其他：Hugging Face 的模型頁自己附的圖（下面 MANUAL）。
都找不到就不列，網頁顯示寫著名字的替代卡。
"""
import concurrent.futures as cf
import json
import pathlib
import re
import time
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
SHARE = ROOT / "resources" / "share"
OUT = SHARE / "lora_covers.json"
UA = {"User-Agent": "PaintDeck-lora-covers/1.0"}
CIVITAI = "https://civitai.com/api/v1"
MS_API = "https://modelscope.cn/api/v1"
MS_COLLECTION = "gordonz/Excellent-Loras-for-MossImage"
MS_COLLECTION_COVER = f"https://www.modelscope.cn/models/{MS_COLLECTION}/resolve/master/banner.jpg"
IMG_DIR = SHARE / "lora_covers"
COVER_W = 360  # 卡片最寬約 260px，留一點給高 DPI 螢幕
COVER_Q = 72
NOTE = ("LoRA 庫卡片的封面，由 tools/lora_covers.py 產生（別手改，改 MANUAL 再重跑）。file＝程式附的縮圖（resources/share/lora_covers/），"
        "有就直接用、不連網；沒有才照 civitai → modelscope → hf 的順序抓圖，存進 <資料>/previews/。")

# 自動找不到、要手動指定的（2026-11-07 查過）
MANUAL = {
    # 跟 C 站 3006599「Moss Image Turbo LoRas」的 lodestones 8 步 DMD 是同一個作者的同一個加速 LoRA（Hugging Face 的 lora_latest），檔案雜湊不同
    "moss\\moss_speed_dmd_lodestones.safetensors": {"civitai_version": 3420002},
    "moss\\Excellent-Loras\\加速LoRA\\lora_latest_step8_0.7.safetensors": {"civitai_version": 3420002},
    # 安裝清單裡放在 moss_encoder_adapters\（檔名不同），C 站是 3021928「MOSS-Image 4B Text Encoder Adapter」
    "moss\\Excellent-Loras\\4B文本编码适配器\\mossImage4BTextEncoder_1600Fit.safetensors": {"civitai_version": 3431398},
    # Hugging Face chinoll/Moss-Image-Edit 模型頁的示範圖
    "moss\\moss_edit_chinoll.safetensors": {"hf": "https://huggingface.co/chinoll/Moss-Image-Edit/resolve/main/assets/moss-image-edit-showcase.jpg"},
}


def get(url, method="GET", body=None, tries=3):
    data = json.dumps(body).encode() if body is not None else None
    headers = dict(UA, **({"Content-Type": "application/json"} if data else {}))
    last = None
    for i in range(tries):
        try:
            req = urllib.request.Request(url, data=data, headers=headers, method=method)
            with urllib.request.urlopen(req, timeout=40) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
            last = e
        except Exception as e:  # 網路忽斷忽續：重試
            last = e
        time.sleep(2 * (i + 1))
    raise RuntimeError(f"{url}：{last}")


def img_small(url, width=450):
    """同 civitai.rs 的 img_small：網址裡的 /original=true/ 或 /width=…/ 換成指定寬度"""
    return re.sub(r"/(original=true|width=\d+)[^/]*/", f"/width={width}/", url)


def pick(images):
    """同 civitai.rs 下載 LoRA 時挑預覽圖：只看圖片、尺度（nsfwLevel）最低的那張"""
    imgs = [i for i in images or [] if (i.get("type") or "image") == "image" and i.get("url")]
    return min(imgs, key=lambda i: i.get("nsfwLevel") or 1) if imgs else None


def civitai_cover(version_id=None, sha=None):
    v = get(f"{CIVITAI}/model-versions/{version_id}") if version_id else get(f"{CIVITAI}/model-versions/by-hash/{sha}") if sha else None
    if not v:
        return None
    img = pick(v.get("images"))
    if not img and v.get("modelId"):  # 這個版本沒附圖：看同一個模型的其他版本
        m = get(f"{CIVITAI}/models/{v['modelId']}") or {}
        img = pick([i for mv in m.get("modelVersions") or [] for i in mv.get("images") or []])
    if not img:
        return None
    return {"model": v.get("modelId"), "version": v.get("id"), "img": img_small(img["url"]), "nsfw": img.get("nsfwLevel") or 1}


def ms_thumb(u):
    """同 modelhub.rs 的 ms_thumb：魔塔圖片加縮圖參數"""
    if not u.startswith("https://resources.modelscope.cn/") or "?" in u:
        return u
    fmt = "" if u.lower().endswith(".gif") else "/format,webp"
    return f"{u}?x-oss-process=image/resize,w_500{fmt}/quality,Q_60"


def ms_search(q):
    body = {"PageSize": 10, "PageNumber": 1, "SortBy": "Default", "Target": "", "IsAigc": True, "ImgUrl": "", "Name": q,
            "SingleCriterion": [{"category": "aigc_type", "DateType": "string", "predicate": "equal", "StringValue": "LoRA"},
                                {"category": "vision_foundation", "DateType": "string", "predicate": "equal", "StringValue": "all"}],
            "Criterion": [], "IsStar": False}
    try:
        v = get(f"{MS_API}/dolphin/models", "PUT", body) or {}
    except RuntimeError:
        return []
    return ((v.get("Data") or {}).get("Model") or {}).get("Models") or []


_ms_files = {}


def ms_has(repo, sha):
    if repo not in _ms_files:
        try:
            v = get(f"{MS_API}/models/{repo}/repo/files?Revision=master&Recursive=true") or {}
        except RuntimeError:
            v = {}
        _ms_files[repo] = {(f.get("Sha256") or "").lower() for f in (v.get("Data") or {}).get("Files") or []}
    return sha.lower() in _ms_files[repo]


def ms_cover_of(m):
    muse = m.get("MuseInfo") or {}
    for c in [c for v in muse.get("versions") or [] for c in v.get("coverImages") or []] + (m.get("CoverImages") or []):
        u = c if isinstance(c, str) else c.get("url") or c.get("Url")
        if u:
            return ms_thumb(u)
    return None


def modelscope_cover(key, sha, title):
    """魔塔上有同一個檔（SHA256 一樣）才算"""
    if not sha:
        return None
    stem = re.split(r"[\\/]", key)[-1].rsplit(".", 1)[0]
    queries = [stem]
    if title:
        queries.append(" ".join(re.findall(r"[A-Za-z0-9]+", title)[:3]))
    for q in dict.fromkeys(q for q in queries if q.strip()):
        for m in ms_search(q):
            repo = f"{m.get('Path')}/{m.get('Name')}"
            cover = ms_cover_of(m)
            if cover and ms_has(repo, sha):
                return {"id": repo, "img": cover}
    return None


def load_sha():
    sha = {}
    for f in ("models_full.json", "models_basic.json"):
        p = SHARE / f
        if not p.exists():
            continue
        for it in json.loads(p.read_text(encoding="utf-8")):
            path = it.get("path") or ""
            if path.startswith("loras/") and it.get("sha256"):
                sha.setdefault(path[len("loras/"):].lower(), it["sha256"])
                sha.setdefault(path.rsplit("/", 1)[-1].lower(), it["sha256"])
    return sha


def from_collection(key):
    return key.lower().startswith("moss\\excellent-loras\\")


def one(key, meta, sha):
    rel = key.replace("\\", "/").lower()
    h = sha.get(rel) or sha.get(rel.rsplit("/", 1)[-1])
    man = MANUAL.get(key, {})
    out = {"title": meta.get("title") or re.split(r"[\\/]", key)[-1].rsplit(".", 1)[0]}
    c = civitai_cover(man.get("civitai_version") or meta.get("versionId"), None if man.get("civitai_version") or meta.get("versionId") else h)
    if not c and meta.get("versionId") and h:  # 版本被作者刪了：再用雜湊找一次
        c = civitai_cover(None, h)
    if c:
        out["civitai"] = c
    ms = modelscope_cover(key, h, meta.get("title"))
    if not ms and from_collection(key):
        ms = {"id": MS_COLLECTION, "img": MS_COLLECTION_COVER}
    if ms:
        out["modelscope"] = ms
    if man.get("hf"):
        out["hf"] = {"img": man["hf"]}
    return key, out


def fetch_bytes(url):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (PaintDeck-lora-covers)"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def thumb(data):
    """縮成寬 COVER_W 的 WebP（比較小的圖不放大）；動圖只取第一格"""
    import io
    from PIL import Image
    im = Image.open(io.BytesIO(data))
    im.seek(0)
    im = im.convert("RGB")
    if im.width > COVER_W:
        im = im.resize((COVER_W, max(1, round(im.height * COVER_W / im.width))), Image.LANCZOS)
    out = io.BytesIO()
    im.save(out, "WEBP", quality=COVER_Q, method=6)
    return out.getvalue()


def images():
    """照 lora_covers.json 把每筆的封面縮圖存進 resources\\share\\lora_covers\\，清單記 "file"；用不到的舊縮圖刪掉"""
    import hashlib
    doc = json.loads(OUT.read_text(encoding="utf-8"))
    doc["_說明"] = NOTE
    IMG_DIR.mkdir(exist_ok=True)
    got, failed = 0, []
    for key, e in doc["covers"].items():
        e.pop("file", None)
        for src in ("civitai", "modelscope", "hf"):
            url = (e.get(src) or {}).get("img")
            if not url:
                continue
            name = hashlib.sha256(url.encode()).hexdigest()[:16] + ".webp"
            path = IMG_DIR / name
            if not path.exists():
                try:
                    path.write_bytes(thumb(fetch_bytes(url)))
                except Exception as ex:  # 這個來源抓不到：換下一個
                    print(f"  {key}：{src} 抓不到（{ex}）")
                    continue
            e["file"] = name
            # 尺度等級跟著實際用的那張：不是 C 站的圖就沒有等級（當 1，同下載中心）
            e["file_nsfw"] = (e.get("civitai") or {}).get("nsfw", 1) if src == "civitai" else 1
            got += 1
            break
        else:
            failed.append(key)
    used = {e["file"] for e in doc["covers"].values() if "file" in e}
    for p in IMG_DIR.glob("*.webp"):
        if p.name not in used:
            p.unlink()
    OUT.write_text(json.dumps(doc, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    size = sum(p.stat().st_size for p in IMG_DIR.glob("*.webp"))
    print(f"內建縮圖：{got} 筆有圖（{len(used)} 個檔，共 {size / 1e6:.1f} MB），抓不到 {len(failed)}")
    for k in failed:
        print("  沒有縮圖：", k)


def main():
    if "--images" in __import__("sys").argv:
        return images()
    meta = json.loads((SHARE / "lora設定.json").read_text(encoding="utf-8"))["lora_meta"]
    sha = load_sha()
    with cf.ThreadPoolExecutor(4) as ex:
        res = list(ex.map(lambda k: one(k, meta[k], sha), meta))
    covers = {k: v for k, v in res if any(s in v for s in ("civitai", "modelscope", "hf"))}
    missing = [k for k, v in res if k not in covers]
    doc = {
        "_說明": NOTE,
        "generated": time.strftime("%Y-%m-%d"),
        "covers": covers,
    }
    OUT.write_text(json.dumps(doc, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    n = lambda s: sum(1 for v in covers.values() if s in v)
    print(f"{len(meta)} 個 LoRA：有封面 {len(covers)}（C 站 {n('civitai')}、魔塔 {n('modelscope')}、HF {n('hf')}），找不到 {len(missing)}")
    for k in missing:
        print("  找不到：", k)
    for k, v in covers.items():
        if "civitai" not in v:
            print("  沒有 C 站：", k, "→", ", ".join(s for s in ("modelscope", "hf") if s in v))
    images()


if __name__ == "__main__":
    main()

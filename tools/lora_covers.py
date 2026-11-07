r"""產生 resources\share\lora_covers.json：程式附帶說明（lora設定.json）的 LoRA 各自的封面圖來源（2026-11-07）。

  python -I -X utf8 tools/lora_covers.py

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


def main():
    meta = json.loads((SHARE / "lora設定.json").read_text(encoding="utf-8"))["lora_meta"]
    sha = load_sha()
    with cf.ThreadPoolExecutor(4) as ex:
        res = list(ex.map(lambda k: one(k, meta[k], sha), meta))
    covers = {k: v for k, v in res if any(s in v for s in ("civitai", "modelscope", "hf"))}
    missing = [k for k, v in res if k not in covers]
    doc = {
        "_說明": "LoRA 庫卡片的封面來源，由 tools/lora_covers.py 產生（別手改，改 MANUAL 再重跑）。後端照 civitai → modelscope → hf 的順序抓圖，存進 <資料>/previews/。",
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


if __name__ == "__main__":
    main()

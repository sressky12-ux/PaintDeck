"""把官方免安裝版的 ComfyUI 換到指定版本（繪匣安裝精靈用，2026-10-31）。
用免安裝版自己的 python_embeded 執行：python.exe -s comfy_upgrade.py <ComfyUI 資料夾> <標籤，例如 v0.43.0> <備份資料夾> [--req-only <寫到哪>]
- 用 pygit2（官方免安裝版附的，跟官方 update.py 一樣），不需要另外裝 git
- models、input、output、user、custom_nodes 不動（models 常是連到別的硬碟的 junction，官方的空白佔位檔不要寫進去）
- 要被換掉、內容又跟目標版本不一樣的檔（以前的 MOSS 補丁、自己改過的檔）先搬到備份資料夾
- 最後一行印 JSON：{"ok": true, "from": 舊版, "to": 新版, "backed": [...]} 或 {"ok": false, "error": ...}"""
import json, os, shutil, sys

KEEP = {"models", "input", "output", "user", "custom_nodes", "temp"}


def main():
    root, tag, backup = sys.argv[1], sys.argv[2], sys.argv[3]
    import pygit2

    repo = pygit2.Repository(root)
    old = None
    try:
        with open(os.path.join(root, "comfyui_version.py"), encoding="utf-8") as f:
            old = next((l.split('"')[1] for l in f if l.startswith("__version__")), None)
    except OSError:
        pass
    repo.remotes["origin"].fetch([f"+refs/tags/{tag}:refs/tags/{tag}"])
    commit = repo.revparse_single(f"refs/tags/{tag}").peel(pygit2.Commit)
    tree = commit.tree
    # --req-only <檔>：只把新版的 requirements.txt 寫出來、不動程式（先裝好套件、成功了才換程式，
    # 這樣沒網路、pip 失敗時 ComfyUI 還是舊版、照樣開得起來）
    if len(sys.argv) > 5 and sys.argv[4] == "--req-only":
        with open(sys.argv[5], "wb") as f:
            f.write(tree["requirements.txt"].data)
        print(json.dumps({"ok": True, "from": old, "to": tag.lstrip("v"), "requirements": sys.argv[5]}, ensure_ascii=False))
        return
    backed = []
    # 自己改過的檔（git 說改過）、沒被追蹤卻會被新版蓋掉的檔（以前的 MOSS 補丁）先備份；
    # 不自己比內容：Windows 上工作目錄是 CRLF、庫裡是 LF，一比全都不一樣
    def in_target(path):
        try:
            tree[path]
            return True
        except KeyError:
            return False

    changed = pygit2.GIT_STATUS_WT_MODIFIED | pygit2.GIT_STATUS_WT_NEW | pygit2.GIT_STATUS_INDEX_MODIFIED | pygit2.GIT_STATUS_WT_TYPECHANGE
    for path, flags in repo.status().items():
        if path.split("/")[0] in KEEP or not flags & changed or not in_target(path):
            continue
        disk = os.path.join(root, *path.split("/"))
        if os.path.isfile(disk):
            dst = os.path.join(backup, *path.split("/"))
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(disk, dst)
            backed.append(path)
    paths = [e.name for e in tree if e.name not in KEEP]
    repo.checkout_tree(tree, strategy=pygit2.GIT_CHECKOUT_FORCE, paths=paths)
    repo.set_head(commit.id)
    print(json.dumps({"ok": True, "from": old, "to": tag.lstrip("v"), "backed": backed}, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except Exception as e:  # 精靈只看最後一行
        print(json.dumps({"ok": False, "error": f"{type(e).__name__}: {e}"}, ensure_ascii=False))
        sys.exit(1)

"""衣姿抽屜（2026-11-21）翻譯資料第一段：從 posewear（MIT，github.com/atuizz/posewear）抽出簡中對照表，
再用它原版的 costume_text.gen2 算 30 筆服裝描述當測試期望值。輸出是簡中，第二段 tools\\posewear_text.mjs 轉繁體。

用法（posewear repo、原站首頁存檔資料夾都是自己 clone／下載的）：
  python -I -X utf8 tools\\posewear_text.py <posewear repo> <原站首頁 meta 資料夾> <輸出 json>
meta 資料夾要有 anime.html、costume.html（原站首頁原樣存檔），只拿裡面的 const D 關鍵字當測試輸入。
"""
import importlib.util
import json
import os
import re
import sys


def load_mod(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def build_maps(repo):
    """build.py 會連 PIL、素材資料夾一起跑，不能 import；只把開頭幾個對照表的那幾行抓出來 eval（純字面值）。"""
    src = open(os.path.join(repo, 'build.py'), encoding='utf-8').read()
    out = {}
    for name in ('LINE', 'CAT', 'ANG', 'CC', 'FIX'):
        m = re.search(r'^' + name + r'=(\{.*?\})\s*$', src, re.M)
        if not m:
            sys.exit(f'build.py 找不到 {name}')
        out[name] = eval(m.group(1), {'__builtins__': {}})
    return out


def inline_d(path):
    html = open(path, encoding='utf-8').read()
    m = re.search(r'const D=(.*?);\n', html)
    return json.loads(m.group(1))


def main():
    repo, meta, dst = sys.argv[1:4]
    ct = load_mod(os.path.join(repo, 'costume_text.py'), 'costume_text')
    maps = build_maps(repo)
    tr = {}
    for line in open(os.path.join(repo, 'tr_pose.txt'), encoding='utf-8').read().strip().split('\n'):
        ja, zh, en = line.split('|')
        tr[ja] = [zh, en]
    cases = []
    for lib in ('anime', 'costume'):
        d = inline_d(os.path.join(meta, lib + '.html'))
        # 每個分類挑 2 筆（前面幾筆＋關鍵字最長的），涵蓋不同句型
        by = {}
        for x in d:
            by.setdefault(x['カテゴリ'], []).append(x)
        picked = []
        for cat, xs in by.items():
            # 動畫、寫實開頭幾筆的關鍵字一樣，寫實改挑中間那筆和第二長的
            longest = sorted(xs, key=lambda x: -len(x['キーワード']))
            picked += [xs[0], longest[0]] if lib == 'anime' else [xs[len(xs) // 2], longest[1]]
        for x in picked[:15]:
            cat_en = maps['CC'][x['カテゴリ']][1]
            zh, en, ti = ct.gen2(x['キーワード'], cat_en, x['id'])
            cases.append({'kw': x['キーワード'], 'cat_en': cat_en, 'id': x['id'], 'zh': zh, 'en': en, 'title': ti})
    data = {
        'tr_pose': tr,
        'line': {k: list(v) for k, v in maps['LINE'].items()},
        'cat': maps['CAT'],
        'ang': {k: list(v) for k, v in maps['ANG'].items()},
        'cc': {k: list(v) for k, v in maps['CC'].items()},
        'fix': maps['FIX'],
        'col': ct.COL, 'mod': ct.MOD,
        'n': {k: list(v) for k, v in ct.N.items()},
        'solo': {k: list(v) for k, v in ct.SOLO.items()},
        'en_solo': ct.EN_SOLO, 'skirt_len': ct.SKIRT_LEN,
        'cases': cases,
    }
    json.dump(data, open(dst, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print('ok', len(tr), 'poses,', len(cases), 'cases ->', dst)


if __name__ == '__main__':
    main()

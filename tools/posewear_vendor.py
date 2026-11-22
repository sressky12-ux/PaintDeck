"""把 posewear（MIT，github.com/atuizz/posewear）官網前端原樣搬進繪匣 ui/posewear/（衣姿整頁）。
設計：docs/superpowers/specs/2026-11-22-posewear-page-design.md。上游改版時重跑一次就好：

    py tools/posewear_vendor.py <posewear repo 資料夾>

只做最少的替換，每一處都要「剛好命中」，次數不對就停下來報錯（上游改版了，要人看過再改這支）：
- 資料檔改走繪匣後端 /api/posewear/spa/<名稱>（後端從官網抓、快取 7 天、ギア 四庫的圖改回原站）
- 圖片主機 AH 用官網的 https://pwimg.0vo.pro/（跟官網一樣）
- 原圖下載遇到絕對網址直接用（ギア 四庫改回原站後是絕對網址）
- Service Worker 用相對路徑註冊（頁面在 /static/posewear/ 底下）
- 試穿工作台多一個「交出整套」WB.bundle（替換字串在 resources/share/posewear_bundle.js.txt，跟後端執行時同步的規則共用）
- 頁面只載簡繁切換 zh.js 和載入器 loader.js；loader 決定載官網同步版（後端 /api/posewear/front/…）還是這裡產生的內建版，
  最後再載送到繪匣的 bridge.js。zh.js、loader.js、bridge.js 是手寫的，這裡不產生也不覆蓋
- index.s.html＝簡體官網原樣；index.html＋*.tw.js＝繁體頁（tools/posewear_tw.mjs 產生，要有 node）
"""
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, 'ui', 'posewear')
NAMES = ['poses', 'costumes', 'anime', 'portraits', 'depths', 'expressions', 'hair', 'dynasties', 'scenes', 'lights',
         'props', 'furniture']
COPY = ['index.html', 'app.js', 'style.css', 'extras.js', 'studio.js', 'zip.js', 'sw.js', 'manifest.webmanifest',
        'favicon.svg', 'logo-mono.svg', 'apple-touch-icon.png', 'icon-192.png', 'icon-512.png', 'icon-maskable-512.png',
        'og.png']
HAND = {'zh.js', 'bridge.js', 'loader.js'}  # 手寫的，不動
BUNDLE = os.path.join(ROOT, 'resources', 'share', 'posewear_bundle.js.txt')


def sub(text, old, new, name, count=1):
    n = text.count(old)
    if n != count:
        sys.exit(f'{name}：預期 {count} 處，找到 {n} 處，上游可能改版了：{old[:90]}')
    print(f'  {name}：{old[:60]} → OK')
    return text.replace(old, new)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    repo = sys.argv[1]
    src = os.path.join(repo, 'src')
    if not os.path.isfile(os.path.join(src, 'app.js')):
        sys.exit(f'{src} 裡沒有 app.js，不是 posewear 的 repo？')
    commit = subprocess.run(['git', '-C', repo, 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True).stdout.strip() or 'unknown'
    rd = lambda f: open(os.path.join(src, f), encoding='utf-8').read()

    app = rd('app.js')
    for n in NAMES:
        app = sub(app, f"'{n}.json'", f"'/api/posewear/spa/{n}'", 'app.js')
    app = sub(app, "const AH='';", "const AH='https://pwimg.0vo.pro/';", 'app.js')
    app = sub(app, "const orig=it=>it.ext?'':((AH||'/')+(it.angles?it.angles[it.ai].o:it.o));",
              "const orig=it=>{if(it.ext)return'';const o=it.angles?it.angles[it.ai].o:it.o;return o.startsWith('http')?o:(AH||'/')+o};",
              'app.js')
    app = sub(app, "register('/sw.js')", "register('sw.js')", 'app.js')

    idx = rd('index.html')
    idx = sub(idx, 'href="poses.json"', 'href="/api/posewear/spa/poses"', 'index.html')
    idx = sub(idx, '<script src="app.js" defer></script>', '<script src="zh.js" defer></script><script src="loader.js" defer></script>', 'index.html')
    for f in ['zip.js', 'extras.js', 'studio.js']:  # 這幾支改由 loader.js 依序載入
        idx = sub(idx, f'<script src="{f}" defer></script>', '', 'index.html')

    studio = sub(rd('studio.js'), 'window.WB={', open(BUNDLE, encoding='utf-8').read().rstrip(), 'studio.js')
    # 人像在繪匣裡是跨網域：不設 crossOrigin，canvas 會被汙染、toBlob 丟錯（「人像加载失败」）。跟 posewear.rs 的 LOADIMG_* 一樣
    studio = sub(studio, "function loadImg(src){return new Promise((r,j)=>{const i=new Image();",
                 "function loadImg(src){return new Promise((r,j)=>{const i=new Image();if(/^https?:/.test(src))i.crossOrigin='anonymous';",
                 'studio.js')

    sw = sub(rd('sw.js'), "const V='posewear-v9'", f"const V='posewear-pd-{commit}'", 'sw.js')

    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        if f not in HAND:
            os.remove(os.path.join(OUT, f))
    for f in COPY:
        shutil.copyfile(os.path.join(src, f), os.path.join(OUT, f))
    os.remove(os.path.join(OUT, 'index.html'))  # 簡體原樣存成 index.s.html；index.html 是下面 node 產生的繁體頁
    for f, t in [('app.js', app), ('studio.js', studio), ('index.s.html', idx), ('sw.js', sw)]:
        open(os.path.join(OUT, f), 'w', encoding='utf-8', newline='').write(t)
    shutil.copyfile(os.path.join(repo, 'LICENSE'), os.path.join(OUT, 'LICENSE.txt'))
    r = subprocess.run(['node', os.path.join(ROOT, 'tools', 'posewear_tw.mjs')])
    if r.returncode:
        sys.exit('繁體頁產生失敗（tools/posewear_tw.mjs）')
    open(os.path.join(OUT, 'VENDOR.txt'), 'w', encoding='utf-8').write(
        f'posewear 前端（MIT，https://github.com/atuizz/posewear），commit {commit}。\n'
        '由 tools/posewear_vendor.py 產生，不要手改（zh.js、loader.js、bridge.js 除外）；上游改版時重跑那支腳本。\n')
    print(f'完成：{OUT}（上游 commit {commit}）')


if __name__ == '__main__':
    main()

// 生成頁左右兩欄（2026-11-23 使用者：「有蠻多東西找不到，而且我看左右兩側有留白，應該可以利用一下」；選了小克案）
// 左區：模型與輸入、尺寸與模式設定（原本藏在 ENGINE、尺寸膠囊）；右區：這張圖（成品動作，一直看得到）＋任務。
// 隨機頁只借提示詞卡，看不到左區：ENGINE、尺寸膠囊只在生成頁藏著時出現。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const read = (p) => fs.readFileSync(path.join(__dirname, '..', p), 'utf8');
const html = read('ui/index.html');
const main = read('ui/js/main.js');
const stage = read('ui/js/stage.js');
const dock = read('ui/js/dock.js');

// main.js 裡 stage 那個 mountDock 的 panels
const stagePanels = (() => {
  const at = main.indexOf('mountDock($("#page-gen")');
  const body = main.slice(at, main.indexOf('});', at));
  return [...body.matchAll(/\{ id: "(\w+)"([^}]*)\}/g)].map((m) => ({ id: m[1], rest: m[2] }));
})();

test('左區：source、params 從膠囊改放左區，也留膠囊給隨機頁', () => {
  for (const id of ['source', 'params']) {
    const p = stagePanels.find((x) => x.id === id);
    assert.ok(p, `沒有 ${id}`);
    assert.match(p.rest, /zone: "left"/);
    assert.match(p.rest, /chip: true/);
  }
  // 膠囊列剩下的
  for (const id of ['lora', 'sampler', 'extra', 'layout', 'pe']) assert.match(stagePanels.find((x) => x.id === id).rest, /zone: "cap"/);
});

test('ENGINE、尺寸膠囊只在生成頁藏著（隨機頁）時出現', () => {
  for (const id of ['source', 'params']) {
    const line = stage.split('\n').find((l) => l.trim().startsWith(`${id}:`));
    assert.match(line, /visible: offGen/, id);
  }
  assert.match(stage, /const offGen = \(\) => !\$\("#page-gen"\)\.classList\.contains\("on"\)/);
  // 換頁時要在 .page 的 on 換好之後重算膠囊
  assert.match(main, /pg\.classList\.toggle\("on", pg\.id === `page-\$\{p\}`\)\);\s*stage\.refreshChips\(\);/);
});

test('dock.js：zone 面板也能有膠囊、render 完叫 onRender', () => {
  assert.match(dock, /p\.zone === "cap" \|\| p\.chip/);
  assert.match(dock, /spec\.onRender\?\.\(\)/);
});

const monitor = html.match(/<section class="panel monitor[^]*?<\/section>/)[0];
const actions = monitor.slice(monitor.indexOf('id="viewActions"'));
const items = (s) => [...s.matchAll(/data-act="(\w+)"/g)].map(m => m[1]);

test('觀景窗底框固定放動作列，右區只留任務', () => {
  assert.ok(monitor.indexOf('id="viewActions"') > monitor.indexOf('id="viewer"'));
  assert.deepEqual(stagePanels.filter(p => p.rest.includes('zone: "right"')).map(p => p.id), ['tasks']);
});

test('底框六個動作照使用順序，最後接更多', () => {
  assert.deepEqual(items(actions.split('id="viewMore"')[0]), ['reuse', 'img2img', 'inpaint', 'upscale', 'fav', 'sheet']);
  assert.match(actions, /id="viewMore"[^>]*>更多 ▾</);
});

test('更多選單十三項照順序，移到回收區固定最後', () => {
  const menu = actions.split('id="viewMoreMenu"')[1];
  assert.match(menu, /role="menu"/);
  assert.deepEqual(items(menu), ['multiref', 'charsheet', 'play', 'strip', 'tagger', 'download', 'lib', 'asset',
    'studio', 'lorapreview', 'video', 'frames', 'delete']);
});

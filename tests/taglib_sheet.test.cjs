// node --test --test-isolation=none tests/taglib_sheet.test.cjs
// 固定舞台 A 案（2026-11-22 使用者：「這個詞庫收起來」）：生成頁的詞庫改成從提示詞卡上緣往上彈出的浮層，卡片不變高。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/taglib.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '').replace(/import\.meta\.url/g, '"file:///x/ui/js/taglib.js"');
  const context = vm.createContext({ console, store: { get: (_k, d) => d, set() {} }, el: () => null, toast() {} });
  vm.runInContext(source + '\nglobalThis.subject = { sheetRect, sheetH };', context, { filename: 'taglib.js' });
  const plain = (f) => (...a) => JSON.parse(JSON.stringify(f(...a)));
  return { sheetRect: plain(context.subject.sheetRect), sheetH: context.subject.sheetH };
}

// 2026-11-24 玩家：「詞庫面板太小，點分類很難點」→ 預設高度 min(60vh, 560)，上面空間不夠就往下蓋住提示詞卡上半，但整個留在畫面裡
test('sheetRect：上面空間夠，底邊貼在提示詞卡上緣、寬度一樣、預設 min(60vh, 560)', () => {
  const { sheetRect } = load();
  assert.deepEqual(sheetRect({ left: 100, top: 800, width: 800 }, 1000), { left: 100, width: 800, top: 234, height: 560 });
  assert.equal(sheetRect({ left: 0, top: 700, width: 500 }, 800).height, 480);
});

test('sheetRect：上面空間不夠就往下長，蓋住提示詞卡上半，但頂邊不低於 56、底邊離視窗底至少 8', () => {
  const { sheetRect } = load();
  const r = sheetRect({ left: 0, top: 300, width: 500 }, 900);
  assert.equal(r.top, 56);
  assert.equal(r.height, 540);
  const small = sheetRect({ left: 0, top: 120, width: 500 }, 400, 900);
  assert.ok(small.top >= 56 && small.top + small.height <= 400 - 8, JSON.stringify(small));
});

test('sheetRect：拖過的高度照用，夾在 [200, 視窗高 − 80]', () => {
  const { sheetRect } = load();
  assert.equal(sheetRect({ left: 0, top: 900, width: 500 }, 1000, 700).height, 700);
  assert.equal(sheetRect({ left: 0, top: 900, width: 500 }, 1000, 50).height, 200);
  assert.equal(sheetRect({ left: 0, top: 900, width: 500 }, 1000, 5000).height, 920);
});

test('sheet bounds and control minimum', () => {
  const { sheetRect } = load();
  const r = sheetRect({ left: 0, top: 1000, width: 450 }, 800, 200, 56, 370);
  assert.equal(r.top + r.height, 792);
  assert.equal(r.height, 370);
  assert.equal(sheetRect({ top: 1000 }, 300, 200, 56, 370).height, 236);
});

test('sheetH：0 或沒存過就是預設高度', () => {
  const { sheetH } = load();
  assert.equal(sheetH(0, 1000), 560);
  assert.equal(sheetH(undefined, 600), 360);
  assert.equal(sheetH(150, 1000), 200);
  assert.equal(sheetH(990, 1000), 920);
});

test('生成頁用浮層模式、浮層啟動時不自動打開', () => {
  const gen = fs.readFileSync(path.join(__dirname, '../ui/js/gen.js'), 'utf8');
  assert.match(gen, /taglib\.init\(\{[^\n]*sheet: \{ anchor: F\("promptBox"\) \}/);
  const src = fs.readFileSync(path.join(__dirname, '../ui/js/taglib.js'), 'utf8');
  assert.match(src, /if \(!sheet && store\.get\(prefix \+ "_open", false\)\) open\(true\)/);
});

test('浮層：上緣有拖拉把手，高度記在 store（taglib_sheet_h），按兩下回預設', () => {
  const src = fs.readFileSync(path.join(__dirname, '../ui/js/taglib.js'), 'utf8');
  assert.match(src, /prefix \+ "_sheet_h"/);
  assert.match(src, /setPointerCapture/);
  assert.match(src, /dblclick/);
  const css = fs.readFileSync(path.join(__dirname, '../ui/css/app.css'), 'utf8');
  assert.match(css, /\.taglib\.tl-sheet \{[^}]*overflow: hidden/);
  assert.match(css, /\.tl-sheet \.tl-list \{[^}]*flex: 1 1 0[^}]*min-height: min\(5rem, 30%\)[^}]*max-height: none/);
  assert.match(css, /\.tl-sheet \.tl-subs \{[^}]*max-height: 40%/);
});

// node --test --test-isolation=none tests/dock_layout.test.cjs
// 固定版面（2026-11-22 使用者：「可以改成固定介面，不要讓人拉來拉去。頂多收起展開」）：
// 面板一律在 spec 寫的那一區；存的只有收起狀態（dock_<名稱>_fixed），讀回來一定要能用（壞掉、舊版、未知面板都不能當掉）；
// 舊的自由版面存檔（浮動、藏邊）不讀；看圖區不能被擠掉。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/dock.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const store = { get: (_k, d) => d, set() {} };
  const context = vm.createContext({ console, store, el: () => null, $: () => null, $$: () => [] });
  vm.runInContext(source + '\nglobalThis.subject = { DOCK_V, FIXED_V, defaultLayout, normalizeFixed, fixedLayout, whereIs, clampSize, fitSizes, popPosition };', context, { filename: 'dock.js' });
  // vm 裡的物件原型不同，deepEqual 前先轉成本地的
  return Object.fromEntries(Object.entries(context.subject).map(([k, f]) => [k, typeof f === 'function' ? (...a) => JSON.parse(JSON.stringify(f(...a)) ?? 'null') : f]));
}

const SPEC = {
  panels: [
    { id: 'params', zone: 'left' }, { id: 'prompt', zone: 'left' }, { id: 'lora', zone: 'left' }, { id: 'extra', zone: 'left' },
    { id: 'recent', zone: 'right' },
  ],
  sizes: { top: 160, left: 350, right: 118, bottom: 220 },
};
const SPEC_CAP = {
  panels: [
    { id: 'source', zone: 'cap' }, { id: 'lora', zone: 'cap' },
    { id: 'compose', zone: 'bottom' }, { id: 'tasks', zone: 'right' },
  ],
  sizes: { top: 160, left: 350, right: 330, bottom: 260 },
};

test('defaultLayout：照每塊面板的預設區排好', () => {
  const { defaultLayout, DOCK_V } = load();
  const L = defaultLayout(SPEC);
  assert.equal(L.v, DOCK_V);
  assert.deepEqual(L.zones.left.ids, ['params', 'prompt', 'lora', 'extra']);
  assert.deepEqual(L.zones.right.ids, ['recent']);
  assert.deepEqual(L.zones.top.ids, []);
  assert.deepEqual(L.zones.bottom.ids, []);
  assert.equal(L.zones.left.size, 350);
  assert.equal(L.zones.top.size, 160);
  assert.deepEqual(L.float, {});
  assert.deepEqual(L.hide, {});
  assert.deepEqual(L.folded, []);
});

test('normalizeFixed：沒存檔、亂碼、版本不對都回全展開', () => {
  const { normalizeFixed, FIXED_V } = load();
  assert.equal(FIXED_V, 1);
  const open = { v: 1, collapsed: { top: false, left: false, right: false, bottom: false }, folded: [] };
  for (const s of [null, 'x', 42, { v: 2 }, { v: 1, collapsed: 'no' }]) assert.deepEqual(normalizeFixed(s, SPEC), open);
});

test('normalizeFixed：只收 true、未知面板從 folded 丟掉、重複只留一個', () => {
  const { normalizeFixed } = load();
  const r = normalizeFixed({ v: 1, collapsed: { right: true, left: 'yes', middle: true }, folded: ['lora', 'nope', 'lora', 'recent'] }, SPEC);
  assert.deepEqual(r, { v: 1, collapsed: { top: false, left: false, right: true, bottom: false }, folded: ['lora', 'recent'] });
});

test('normalizeFixed：舊的自由版面存檔（v:2，含浮動、藏邊）不讀，全展開', () => {
  const { normalizeFixed } = load();
  const old = { v: 2, zones: { left: { ids: [], collapsed: true } }, float: { lora: { x: 1, y: 1, w: 300, h: 200 } }, hide: { recent: { edge: 'left', at: 0.3, w: 300, h: 300 } } };
  assert.deepEqual(normalizeFixed(old, SPEC).collapsed, { top: false, left: false, right: false, bottom: false });
});

test('fixedLayout 忽略舊存檔：面板一律在預設區，沒有浮動、藏邊', () => {
  const { fixedLayout, normalizeFixed, defaultLayout } = load();
  const L = fixedLayout(SPEC, normalizeFixed({ v: 1, collapsed: { right: true }, folded: ['extra'] }, SPEC));
  const D = defaultLayout(SPEC);
  assert.deepEqual(L.zones.left.ids, D.zones.left.ids);
  assert.deepEqual(L.zones.right.ids, D.zones.right.ids);
  assert.equal(L.zones.right.collapsed, true);
  assert.equal(L.zones.left.collapsed, false);
  assert.deepEqual(L.folded, ['extra']);
  assert.deepEqual(L.float, {});
  assert.deepEqual(L.hide, {});
});

test('fixedLayout：膠囊面板留在 cap，zones 不收', () => {
  const { fixedLayout, normalizeFixed, whereIs } = load();
  const L = fixedLayout(SPEC_CAP, normalizeFixed(null, SPEC_CAP));
  assert.deepEqual(L.cap, ['source', 'lora']);
  assert.deepEqual(L.zones.left.ids, []);
  assert.deepEqual(L.zones.bottom.ids, ['compose']);
  assert.deepEqual(whereIs(L, 'lora'), { kind: 'cap' });
  assert.deepEqual(whereIs(L, 'tasks'), { kind: 'zone', zone: 'right' });
  assert.equal(whereIs(L, 'nope'), null);
});

test('膠囊：沒有膠囊面板的 spec 不長出 cap 鍵', () => {
  const { defaultLayout } = load();
  assert.equal('cap' in defaultLayout(SPEC), false);
});

test('clampSize：每區有最小值，最大不超過可用空間的一定比例', () => {
  const { clampSize } = load();
  assert.equal(clampSize('left', 100, 1600), 260);
  assert.equal(clampSize('left', 2000, 1600), 800);
  assert.equal(clampSize('right', 50, 1600), 100);
  assert.equal(clampSize('bottom', 50, 900), 100);
  assert.equal(clampSize('bottom', 900, 900), 450);
  assert.equal(clampSize('top', 900, 900), 450);
  assert.equal(clampSize('left', 400, 1600), 400);
});

test('fitSizes：視窗變小時各區按比例縮，看圖區至少留 420×320', () => {
  const { defaultLayout, fitSizes } = load();
  const L = defaultLayout(SPEC);
  L.zones.left.size = 800; L.zones.right.size = 400;
  const s = fitSizes(L, 1500, 900);
  assert.ok(s.left + s.right + 420 + 20 <= 1500, `左右加主區超出：${JSON.stringify(s)}`);
  assert.ok(Math.abs(s.left / s.right - 2) < 0.05, '按原本比例縮');
  assert.equal(fitSizes(defaultLayout(SPEC), 1600, 900).left, 350, '夠寬就不動');

  const C = defaultLayout(SPEC); C.zones.left.collapsed = true;
  assert.equal(fitSizes(C, 1600, 900).left, 28, '收起剩直條');
  assert.equal(fitSizes(C, 1600, 900).bottom, 0, '沒有面板的區不佔位置');

  const B = defaultLayout(SPEC); B.zones.bottom.ids = ['recent']; B.zones.top.ids = ['lora']; B.zones.right.ids = [];
  B.zones.bottom.size = 400; B.zones.top.size = 400;
  const t = fitSizes(B, 1600, 800);
  assert.ok(t.top + t.bottom + 320 + 20 <= 800, `上下太高：${JSON.stringify(t)}`);
  assert.equal(t.top, t.bottom, '上下按比例縮');
});

test('popPosition：放在膠囊上方、左對齊', () => {
  const { popPosition } = load();
  assert.deepEqual(popPosition({ left: 300, top: 800, width: 100 }, { w: 340, h: 400 }, { w: 1920, h: 910 }), { left: 300, top: 392, arrowX: 50 });
});
test('popPosition：右邊超出就往左推，箭頭還指著膠囊', () => {
  const { popPosition } = load();
  assert.deepEqual(popPosition({ left: 1800, top: 800, width: 100 }, { w: 340, h: 400 }, { w: 1920, h: 910 }), { left: 1572, top: 392, arrowX: 278 });
});
test('popPosition：上面放不下就貼頂、窄視窗也不超出', () => {
  const { popPosition } = load();
  assert.deepEqual(popPosition({ left: 900, top: 300, width: 80 }, { w: 340, h: 600 }, { w: 1100, h: 800 }), { left: 752, top: 8, arrowX: 188 });
});

test('dock.js 沒有拖拉的殘留：沒有 pointerdown 拖曳、右鍵選單、拉縫、圖釘', () => {
  const src = fs.readFileSync(path.join(__dirname, '../ui/js/dock.js'), 'utf8');
  for (const s of ['startDrag', 'contextmenu', 'startResize', 'dock-rz', 'dock-pin', 'cap-pin', 'floatPanel', 'hidePanel']) assert.equal(src.includes(s), false, s);
});

test('舊生成頁存檔的 imgacts 可略過，任務欄的收合狀態保留', () => {
  const { normalizeFixed, fixedLayout, FIXED_V } = load();
  const spec = { panels: [{ id: 'tasks', zone: 'right' }], sizes: {} };
  const saved = { v: FIXED_V, collapsed: { right: true }, folded: ['imgacts', 'tasks'] };
  const layout = fixedLayout(spec, normalizeFixed(saved, spec));
  assert.deepEqual(layout.folded, ['tasks']);
  assert.deepEqual(layout.zones.right.ids, ['tasks']);
  assert.equal(layout.zones.right.collapsed, true);
});

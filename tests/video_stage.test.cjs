// 中央舞台第二期：影片頁（短片、長片、拍攝單抽屜）的結構與膠囊摘要
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
const mainJs = fs.readFileSync(path.join(__dirname, '../ui/js/main.js'), 'utf8');

const count = (s, needle) => s.split(needle).length - 1;
/** 從 open（第一次出現）切到它之後第一個 close */
function slice(open, close) {
  const a = html.indexOf(open);
  assert.ok(a >= 0, `找不到 ${open}`);
  const b = html.indexOf(close, a);
  assert.ok(b > a, `${open} 之後找不到 ${close}`);
  return html.slice(a, b);
}
/** 從 data-dock="id" 那個元素開始，切到下一個 data-dock="…" 或 </main>（面板不巢狀） */
function dockBlock(id) {
  const a = html.indexOf(`data-dock="${id}"`);
  assert.ok(a >= 0, `找不到 data-dock="${id}"`);
  const rest = html.slice(a + 10);
  const next = rest.search(/data-dock="|<\/main>/);
  return html.slice(a, a + 10 + next);
}
const has = (block, id) => block.includes(`id="${id}"`);

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/videostage.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const store = { get: (_k, d) => d, set() {} };
  const context = vm.createContext({ console, store, app: {}, el: () => null, $: () => null, $$: () => [], debounce: (f) => f });
  vm.runInContext(source + '\nglobalThis.subject = { fmtShots, fmtWhere };', context, { filename: 'videostage.js' });
  return context.subject;
}

// ---- Task 1：頁首列、拍攝單抽屜
test('頁首列：#videoSub 不在 <details> 裡、旁邊有 #shOpen', () => {
  assert.equal(count(html, 'id="videoSub"'), 1);
  assert.equal(count(html, 'id="shFold"'), 0);
  assert.equal(count(html, 'id="shOpen"'), 1);
});
test('拍攝單抽屜：#shDrawer 包著 #shBody，有 #shClose', () => {
  const d = slice('id="shDrawer"', '</aside>');
  assert.ok(has(d, 'shBody'));
  assert.ok(has(d, 'shClose'));
});
test('狀態燈搬進頁首列', () => {
  assert.equal(count(html, 'id="h3Status"'), 1);
  assert.equal(count(html, 'id="hcStatus"'), 1);
  const head = slice('data-sub="sheet"', '</section>');
  assert.ok(has(head, 'h3Status'));
  assert.ok(has(head, 'hcStatus'));
});

// ---- Task 2：短片舞台
test('短片：膠囊、提示詞卡、工作與成品各一塊', () => {
  for (const id of ['start', 'board', 'shots', 'where', 'adv', 'h3compose', 'h3tasks']) {
    assert.equal(count(html, `data-dock="${id}"`), 1, id);
  }
  assert.equal(count(html, 'id="h3Stage"'), 1);
  assert.equal(count(html, 'id="h3CapRow"'), 1);
  const compose = dockBlock('h3compose');
  for (const id of ['h3Idea', 'h3Go', 'h3Secs', 'h3Aspect', 'h3Quality', 'h3Count', 'h3Est']) assert.ok(has(compose, id), id);
  const tasks = dockBlock('h3tasks');
  assert.ok(has(tasks, 'h3Jobs'));
  assert.ok(has(tasks, 'h3Out'));
});
test('短片：main.js 用 h3stage，不再用舊的 dockCols', () => {
  assert.ok(mainJs.includes('name: "h3stage"'));
  assert.ok(!mainJs.includes('dockCols("h3",'));
});
test('fmtShots、fmtWhere', () => {
  const { fmtShots, fmtWhere } = load();
  assert.equal(fmtShots(0), '—');
  assert.equal(fmtShots(3), '3 鏡');
  assert.equal(fmtWhere('  雲端顯卡 '), '雲端顯卡');
  assert.equal(fmtWhere(''), '—');
  assert.equal(fmtWhere(undefined), '—');
});

// ---- Task 3：長片舞台
test('長片：膠囊、開拍列、開拍與成品各一塊', () => {
  for (const id of ['film', 'shared', 'hcrun', 'hcshoot']) assert.equal(count(html, `data-dock="${id}"`), 1, id);
  const run = dockBlock('hcrun');
  for (const id of ['hcRun', 'hcStop', 'hcJoin', 'hcEst', 'hcWhere', 'hcAspect', 'hcQuality']) assert.ok(has(run, id), id);
  const shoot = dockBlock('hcshoot');
  for (const id of ['hcCloud', 'hcGoCloud', 'hcMsg', 'hcFinal']) assert.ok(has(shoot, id), id);
  const stage = slice('id="hcStage"', 'data-dock=');
  assert.ok(has(stage, 'hcSegs'));
  assert.ok(has(stage, 'hcAddSeg'));
});
test('長片：main.js 用 hcstage，不再用舊的 dockCols', () => {
  assert.ok(mainJs.includes('name: "hcstage"'));
  assert.ok(!mainJs.includes('dockCols("h3long",'));
});

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/topbar.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const store = { get: (_k, d) => d, set() {} };
  const context = vm.createContext({ console, store, app: {}, el: () => null, $: () => null, $$: () => [] });
  vm.runInContext(source + '\nglobalThis.subject = { altPage, topLevel, PAGE_ORDER };', context, { filename: 'topbar.js' });
  // vm 裡的物件原型不同，deepEqual 前先轉成本地的
  return Object.fromEntries(Object.entries(context.subject).map(([k, f]) => [k, typeof f === 'function' ? (...a) => JSON.parse(JSON.stringify(f(...a)) ?? 'null') : f]));
}

test('altPage：Alt＋1～9 對到鍵上 01～09，Alt＋0 是設定', () => {
  const { altPage } = load();
  assert.equal(altPage('1'), 'gen');
  assert.equal(altPage('2'), 'taglib');
  assert.equal(altPage('8'), 'director');
  assert.equal(altPage('9'), 'comic');
  assert.equal(altPage('0'), 'settings');
  assert.equal(altPage('q'), null);
});
test('PAGE_ORDER 跟 index.html 的功能鍵編號一致', () => {
  const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
  const { PAGE_ORDER } = load();
  PAGE_ORDER.forEach((p, i) => {
    const n = String(i + 1).padStart(2, '0');
    const re = p === 'jobs' ? new RegExp(`id="queueInfo"[^>]*>\\s*<span class="n">${n}<`) : new RegExp(`data-page="${p}"[^>]*>\\s*<span class="n">${n}<`);
    assert.match(html, re, `${p} 應該是 ${n}`);
  });
});

test('topLevel：放得下就完整版，放不下先藏小字，再不下才精簡', () => {
  const { topLevel } = load();
  // needs = [完整版要的寬, 藏掉型號和 AI 狀態字後要的寬]
  assert.equal(topLevel(0, [1386, 1300], 1400), 0);
  assert.equal(topLevel(0, [1386, 1300], 1350), 1);
  assert.equal(topLevel(0, [1386, 1300], 1299), 2);
  assert.equal(topLevel(0, [0, 0], 500), 0, '還沒量到就不動');
});
test('topLevel：要變寬鬆時多留幾 px 緩衝，不會在邊界來回跳', () => {
  const { topLevel } = load();
  assert.equal(topLevel(2, [1386, 1300], 1302), 2, '剛好夠精簡→藏小字的邊界，還不切回');
  assert.equal(topLevel(2, [1386, 1300], 1315), 1);
  assert.equal(topLevel(1, [1386, 1300], 1388), 1);
  assert.equal(topLevel(1, [1386, 1300], 1400), 0);
  assert.equal(topLevel(1, [1386, 1300], 1300), 1, '收緊不用緩衝：剛好放得下就留著');
  assert.equal(topLevel(1, [1386, 1300], 1299), 2);
});

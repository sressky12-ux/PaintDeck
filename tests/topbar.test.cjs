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
  vm.runInContext(source + '\nglobalThis.subject = { pinnedKeys, altPage, PAGE_ORDER };', context, { filename: 'topbar.js' });
  // vm 裡的物件原型不同，deepEqual 前先轉成本地的
  return Object.fromEntries(Object.entries(context.subject).map(([k, f]) => [k, typeof f === 'function' ? (...a) => JSON.parse(JSON.stringify(f(...a)) ?? 'null') : f]));
}

test('pinnedKeys：照釘選順序，目前頁不在就接最後', () => {
  const { pinnedKeys } = load();
  assert.deepEqual(pinnedKeys(['gen', 'gallery'], 'loras'), ['gen', 'gallery', 'loras']);
  assert.deepEqual(pinnedKeys(['gen', 'gallery'], 'gallery'), ['gen', 'gallery']);
});
test('pinnedKeys：丟掉不認得的、去重；任務和助手不接', () => {
  const { pinnedKeys } = load();
  assert.deepEqual(pinnedKeys(['gen', 'ghost', 'gen', 'comic'], 'jobs'), ['gen', 'comic']);
  assert.deepEqual(pinnedKeys([], 'agent'), []);
});
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

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/stage.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const store = { get: (_k, d) => d, set() {} };
  const context = vm.createContext({ console, store, app: {}, el: () => null, $: () => null, $$: () => [] });
  vm.runInContext(source + '\nglobalThis.subject = { fmtSampler, fmtCount, fmtModel, taskCards };', context, { filename: 'stage.js' });
  // vm 裡的物件原型不同，deepEqual 前先轉成本地的
  return Object.fromEntries(Object.entries(context.subject).map(([k, f]) => [k, typeof f === 'function' ? (...a) => JSON.parse(JSON.stringify(f(...a)) ?? 'null') : f]));
}

test('fmtSampler', () => {
  const { fmtSampler } = load();
  assert.equal(fmtSampler(8, 1, 4), '8 步 · CFG 1 · ×4');
  assert.equal(fmtSampler('25', '3', ''), '25 步 · CFG 3');
});
test('fmtCount', () => {
  const { fmtCount } = load();
  assert.equal(fmtCount(2), '2');
  assert.equal(fmtCount(0), '—');
});
test('fmtModel：去副檔名、去資料夾、太長截斷', () => {
  const { fmtModel } = load();
  assert.equal(fmtModel('anima\\anima-turbo-v1.1.safetensors'), 'anima-turbo-v1.1');
  assert.equal(fmtModel('moss-cat-tower_v2_int8-convrot.safetensors'), 'moss-cat-tower_v2…');
});

test('taskCards：執行中、排隊、最近結束的，最多 4 張', () => {
  const { taskCards } = load();
  const J = [
    { id: 'a', status: 'done' }, { id: 'b', status: 'queued' }, { id: 'c', status: 'running' },
    { id: 'd', status: 'queued' }, { id: 'e', status: 'error' }, { id: 'f', status: 'done' },
  ];
  assert.deepEqual(taskCards(J).map((j) => j.id), ['c', 'b', 'd', 'a']);
  assert.deepEqual(taskCards([]), []);
});

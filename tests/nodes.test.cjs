// node --test --test-isolation=none tests/nodes.test.cjs
// 工作節點（ui/js/nodes.js）：生成頁下拉選項、家族綁定、任務節點標籤；index.html 的掛點
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/nodes.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, $: () => null, $$: () => [], el: () => ({}), api: async () => ({}), toast() {} });
  vm.runInContext(source + '\nglobalThis.subject = { nodeOptions, bindFromChecks, jobNodeLabel };', context, { filename: 'nodes.js' });
  return context.subject;
}

// vm 沙盒裡做的陣列原型不同，比之前先轉成這邊的
const plain = (x) => JSON.parse(JSON.stringify(x));

test('nodeOptions：只有主節點在線＝不用選，回空', () => {
  const { nodeOptions } = load();
  assert.deepEqual(plain(nodeOptions([{ id: 'main', name: '主節點', kind: 'main', enabled: true, online: true, queue: 0 }])), []);
});

test('nodeOptions：主節點＋gpu1 都在線，第一個是「自動分配」、值依序', () => {
  const { nodeOptions } = load();
  const opts = nodeOptions([
    { id: 'main', name: '主節點', kind: 'main', enabled: true, online: true, queue: 3 },
    { id: 'gpu1', name: '第 1 號卡', kind: 'local', enabled: true, online: true, queue: 0 },
  ]);
  assert.deepEqual(plain(opts[0]), ['', '自動分配']);
  assert.deepEqual(plain(opts.map((o) => o[0])), ['', 'main', 'gpu1']);
});

test('nodeOptions：停用的不列', () => {
  const { nodeOptions } = load();
  const opts = nodeOptions([
    { id: 'main', kind: 'main', enabled: true, online: true },
    { id: 'gpu1', kind: 'local', enabled: false, online: true },
    { id: 'r1', kind: 'remote', enabled: true, online: true },
  ]);
  assert.deepEqual(plain(opts.map((o) => o[0])), ['', 'main', 'r1']);
});

test('nodeOptions：離線的帶「離線」、在線排隊的帶「排 N 件」', () => {
  const { nodeOptions } = load();
  const opts = nodeOptions([
    { id: 'main', kind: 'main', enabled: true, online: true, queue: 0 },
    { id: 'gpu1', name: '第 1 號卡', kind: 'local', enabled: true, online: false, queue: 0 },
    { id: 'r1', name: '書房', kind: 'remote', enabled: true, online: true, queue: 2 },
  ]);
  assert.match(opts.find((o) => o[0] === 'gpu1')[1], /離線/);
  assert.match(opts.find((o) => o[0] === 'r1')[1], /排 2 件/);
});

test('bindFromChecks：沒勾的不出現', () => {
  const { bindFromChecks } = load();
  const bind = bindFromChecks([
    { family: 'qwen21', node: 'gpu1', checked: true },
    { family: 'qwen21', node: 'main', checked: false },
    { family: 'anima', node: 'main', checked: false },
  ]);
  assert.deepEqual(plain(bind), { qwen21: ['gpu1'] });
});

test('jobNodeLabel：有 _node 給節點名，主節點、沒有 _node 給空', () => {
  const { jobNodeLabel } = load();
  const nodes = [{ id: 'gpu1', name: '第 1 號卡' }];
  assert.equal(jobNodeLabel({ params: { _node: 'gpu1' } }, nodes), '第 1 號卡');
  assert.equal(jobNodeLabel({ params: {} }, nodes), '');
  assert.equal(jobNodeLabel({ params: { _node: 'main' } }, nodes), '');
  assert.equal(jobNodeLabel({ params: { _node: 'notfound' } }, nodes), '');
});

test('ui/index.html 有節點設定的掛點和生成頁的下拉', () => {
  const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
  for (const id of ['id="nodesBox"', 'id="genNode"', 'id="genNodeWrap"']) assert.ok(html.includes(id), id);
});

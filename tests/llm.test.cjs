// node --test --test-isolation=none tests/llm.test.cjs
// 左邊「API」頁（ui/js/llm.js）：段落搬移、佔位段不能刪、腦袋選項、匯入報告
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/llm.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, $: () => null, el: () => ({}), api: async () => ({}), toast() {}, app: {}, openPrompt() {}, openCloudSettings() {} });
  vm.runInContext(source + '\nglobalThis.subject = { brainOptions, moveBlock, removeBlock, reportLines, BUILTIN_ID };', context, { filename: 'llm.js' });
  return context.subject;
}

// vm 沙盒裡做的陣列原型不同，比之前先轉成這邊的
const plain = (x) => JSON.parse(JSON.stringify(x));
const B = (kind, id = kind) => ({ id, kind, role: 'system', content: '', enabled: true });

test('腦袋選項：照頁面、本機、雲端，再加每組連線（值是 api:<id>）', () => {
  const { brainOptions } = load();
  const opts = brainOptions([{ id: 'a1', name: 'DeepSeek', ready: true }, { id: 'b2', name: '空的', ready: false }]);
  assert.deepEqual(plain(opts.map((o) => o[0])), ['page', '9b', '35b', 'cloud', 'api:a1', 'api:b2']);
  assert.match(opts[5][1], /沒填好/);
  assert.equal(brainOptions(undefined).length, 4);
});

test('段落上下移：佔位段也能搬，超出邊界不動', () => {
  const { moveBlock } = load();
  const blocks = [B('text', 'a'), B('builtin'), B('request')];
  assert.deepEqual(moveBlock(blocks, 1, -1).map((b) => b.id), ['builtin', 'a', 'request']);
  assert.deepEqual(moveBlock(blocks, 0, -1).map((b) => b.id), ['a', 'builtin', 'request']);
  assert.deepEqual(moveBlock(blocks, 2, 1).map((b) => b.id), ['a', 'builtin', 'request']);
  assert.deepEqual(blocks.map((b) => b.id), ['a', 'builtin', 'request'], '不改原陣列');
});

test('刪段落：功能內建指令、請求內容不能刪', () => {
  const { removeBlock } = load();
  const blocks = [B('text', 'a'), B('builtin'), B('request')];
  assert.deepEqual(removeBlock(blocks, 0).map((b) => b.id), ['builtin', 'request']);
  assert.deepEqual(removeBlock(blocks, 1).map((b) => b.id), ['a', 'builtin', 'request']);
  assert.deepEqual(removeBlock(blocks, 2).map((b) => b.id), ['a', 'builtin', 'request']);
  assert.deepEqual(removeBlock(blocks, 9).map((b) => b.id), ['a', 'builtin', 'request']);
});

test('匯入報告：一行一件，沒東西就說全部照原樣', () => {
  const { reportLines } = load();
  assert.deepEqual(plain(reportLines(['略過 x', '沒匯入 y'])), ['・略過 x', '・沒匯入 y']);
  assert.deepEqual(plain(reportLines([])), ['・全部照原樣匯入']);
});

test('每個功能都對到一份可以改的內建指令（預演台除外）', () => {
  const { BUILTIN_ID } = load();
  for (const f of ['agent', 'rewrite', 'translate', 'shotsheet', 'h3writer', 'director']) assert.ok(BUILTIN_ID[f], f);
  assert.equal(BUILTIN_ID.previz, '');
});

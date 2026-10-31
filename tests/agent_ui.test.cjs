// node --test --test-isolation=none tests/agent_ui.test.cjs
// 生圖助手「讓我看」（2026-10-27）：主視窗只認得白名單裡的頁，生成頁做完才把參數填進去。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/agentui.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, $: () => null, app: {}, gen: {} });
  vm.runInContext(source + '\nglobalThis.subject = { uiTarget, shouldFill, fillValues, PAGES };', context, { filename: 'agentui.js' });
  return context.subject;
}

test('只切到認得的頁', () => {
  const { uiTarget } = load();
  assert.equal(uiTarget({ page: 'gen' }), 'gen');
  assert.equal(uiTarget({ page: 'comic' }), 'comic');
  assert.equal(uiTarget({ page: '../x' }), null);
  assert.equal(uiTarget({ page: 'agent' }), null, '助手頁在獨立視窗，主視窗不切');
  assert.equal(uiTarget(null), null);
});

test('生成頁做完、有參數物件才填', () => {
  const { shouldFill } = load();
  assert.equal(shouldFill({ op: 'show', page: 'gen', params: { prompt: 'x' } }), true);
  assert.equal(shouldFill({ op: 'goto', page: 'gen', params: { prompt: 'x' } }), false, '還在做不填');
  assert.equal(shouldFill({ op: 'show', page: 'gallery', params: { prompt: 'x' } }), false);
  assert.equal(shouldFill({ op: 'show', page: 'gen', params: null }), false);
  assert.equal(shouldFill({ op: 'show', page: 'gen', params: [1] }), false);
});

test('頁面白名單跟後端一樣', () => {
  const { PAGES } = load();
  const rs = fs.readFileSync(path.join(__dirname, '../src-tauri/src/agent_tools.rs'), 'utf8');
  const m = rs.match(/pub const PAGES: &\[&str\] = &\[([^\]]+)\]/);
  assert.ok(m, '找得到 agent_tools.rs 的 PAGES');
  assert.deepEqual([...PAGES], m[1].split(',').map((s) => s.trim().replace(/"/g, '')).filter(Boolean));
});

test('只寫詞的填入：只填提示詞、負面詞、合理的尺寸', () => {
  const { fillValues } = load();
  const got = (f) => JSON.stringify(fillValues(f));
  assert.equal(got({ prompt: 'a cat', negative: '', width: 832, height: 1216 }), JSON.stringify([['prompt', 'a cat'], ['width', '832'], ['height', '1216']]));
  assert.equal(got({ prompt: 'a cat', negative: 'blurry', model: 'x.safetensors', loras: [] }), JSON.stringify([['prompt', 'a cat'], ['negative', 'blurry']]), '模型、LoRA 不動');
  assert.equal(got({ prompt: 'a', width: 'abc', height: 0 }), JSON.stringify([['prompt', 'a']]));
  assert.equal(got(null), '[]');
});

test('寫法清單跟後端一樣', () => {
  const rs = fs.readFileSync(path.join(__dirname, '../src-tauri/src/agent.rs'), 'utf8');
  const block = rs.slice(rs.indexOf('pub const GUIDES'), rs.indexOf('];', rs.indexOf('pub const GUIDES')));
  const names = [...block.matchAll(/\(\s*"([a-z0-9_]+)",\s*"agent_/g)].map((m) => m[1]);
  assert.ok(names.length >= 7, '找得到 GUIDES');
  const js = fs.readFileSync(path.join(__dirname, '../ui/js/agent.js'), 'utf8');
  const zh = js.match(/const GUIDE_ZH = \{([\s\S]*?)\};/)[1];
  assert.deepEqual([...zh.matchAll(/([a-z0-9_]+):/g)].map((m) => m[1]), names);
  const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
  const sel = html.match(/<select id="agTarget"[\s\S]*?<\/select>/)[0];
  assert.deepEqual([...sel.matchAll(/value="([a-z0-9_]+)"/g)].map((m) => m[1]), ['auto', ...names]);
});

test('只寫詞沒選繪圖模型也能送；動手做仍會提醒', async () => {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/agent.js'), 'utf8');
  const say = source.slice(source.indexOf('async function say()'), source.indexOf('\nasync function answer('));
  const sent = [], warnings = [];
  const nodes = { agGo: { disabled: false }, agSay: { value: '幫我寫橘貓' } };
  const context = vm.createContext({
    S: { id: 'test', settings: { mode: 'prompt', brain: '9b' } }, att: [],
    F: (id) => nodes[id], api: async (url, options) => { sent.push({ url, body: options.json }); return { queued: true }; },
    toast: (text, type) => { if (type === 'err') warnings.push(text); }, paintAtt() {},
  });
  vm.runInContext(say + '\nglobalThis.say = say;', context);
  await context.say();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, '/api/agent/say');
  assert.deepEqual(warnings, []);
  context.S.settings.mode = 'tools';
  nodes.agSay.value = '畫橘貓';
  await context.say();
  assert.equal(sent.length, 1);
  assert.equal(warnings.length, 1);
});

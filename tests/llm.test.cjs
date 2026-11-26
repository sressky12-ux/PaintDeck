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
  vm.runInContext(source + '\nglobalThis.subject = { brainOptions, moveBlock, removeBlock, reportLines, jailbreakTemplate, parseExtra, agentPresetView, BUILTIN_ID };', context, { filename: 'llm.js' });
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
  for (const f of ['caption', 'comic', 'colorbook', 'charsheet']) assert.equal(BUILTIN_ID[f], '');
  assert.equal(BUILTIN_ID.previz, '');
});

// 2026-11-24 玩家：「破限預設怎麼加？直接導入就回覆格式錯誤」
test('破限範本：破限段在功能內建指令前面、預填留空、參數照功能', () => {
  const { jailbreakTemplate } = load();
  const p = plain(jailbreakTemplate());
  assert.equal(p.name, '破限範本（助手、改寫可用）');
  assert.deepEqual(p.blocks.map((b) => b.kind), ['text', 'builtin', 'request']);
  assert.equal(p.blocks[0].name, '破限');
  assert.equal(p.blocks[0].role, 'system');
  assert.equal(p.blocks[0].enabled, true);
  assert.match(p.blocks[0].content, /貼在這裡/);
  assert.equal(p.prefill, '');
  assert.deepEqual(p.params, {});
  const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
  assert.match(html, /id="lpJailbreak"[^>]*>新增破限範本（給助手用）</);
});

test('額外請求參數：空白是 null、JSON 物件照收、其他都算錯', () => {
  const { parseExtra } = load();
  assert.equal(parseExtra('  '), null);
  assert.deepEqual(plain(parseExtra('{"reasoning_effort": "low"}')), { reasoning_effort: 'low' });
  assert.equal(parseExtra('[1]'), false);
  assert.equal(parseExtra('3'), false);
  assert.equal(parseExtra('{壞掉'), false);
});

// 2026-11-24 使用者：「在助手欄位即可改[預設]的設定」
test('助手欄的預設下拉：跟對照表「生圖助手」那格同一個設定，腦袋照原樣帶回', () => {
  const { agentPresetView } = load();
  const presets = [{ id: 'p1', name: '盧克之光' }, { id: 'p2', name: '破限範本' }];
  const v = plain(agentPresetView({ presets, routes: { agent: { brain: 'page', preset: 'p2' } } }));
  assert.deepEqual(v.options, [['', '不套'], ['p1', '盧克之光'], ['p2', '破限範本']]);
  assert.equal(v.value, 'p2');
  assert.equal(v.brain, 'page');
  assert.equal(v.note, '');
  // 沒設定過、讀不到
  assert.deepEqual(plain(agentPresetView({ presets, routes: {} })), { options: [['', '不套'], ['p1', '盧克之光'], ['p2', '破限範本']], value: '', brain: 'page', note: '' });
  assert.equal(agentPresetView(null).options.length, 1);
  // 選的預設被刪了：留一個選項讓下拉顯示得出來，並說明
  const gone = plain(agentPresetView({ presets, routes: { agent: { brain: 'page', preset: 'x9' } } }));
  assert.deepEqual(gone.options.at(-1), ['x9', '（選的預設不見了）']);
  assert.equal(gone.value, 'x9');
  assert.match(gone.note, /刪掉/);
  // 對照表把腦袋固定成某組連線：照原樣帶回，並提醒上面的腦袋不算數
  const fixed = plain(agentPresetView({ presets, routes: { agent: { brain: 'api:abc', preset: null } } }));
  assert.equal(fixed.brain, 'api:abc');
  assert.equal(fixed.value, '');
  assert.match(fixed.note, /腦袋固定/);
  const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
  assert.match(html, /<select id="agPreset"/);
  assert.match(html, /id="agPresetEdit"/);
});

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// 設定頁「外掛寫法」那格（ui/js/skills.js）：列出已裝的、綁定選單、找不到資料夾的綁定
function harness(data) {
  const node = () => ({
    value: '', textContent: '', hidden: false, disabled: false, children: [], listeners: {},
    set innerHTML(v) { this.children = []; }, append(...k) { this.children.push(...k); },
    addEventListener(ev, f) { this.listeners[ev] = f; },
  });
  const controls = new Map(['skInstall', 'skUrl', 'skOpen', 'skReload', 'skMsg', 'skPick', 'skList', 'skBind'].map(id => ['#' + id, node()]));
  const box = { querySelector: id => controls.get(id) };
  const el = (tag, attrs = {}, ...kids) => ({ tag, attrs, children: kids.flat().filter(k => k !== null && k !== undefined), value: attrs.value || '',
    append(...k) { this.children.push(...k); } });
  const calls = [];
  const api = async (p, o) => { calls.push([p, o]); return data; };
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/skills.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ el, api, toast() {}, confirm: () => true, encodeURIComponent });
  vm.runInContext(source + '\nglobalThis.subject = { mountSkills, license };', context, { filename: 'skills.js' });
  return { ...context.subject, box, calls, get: id => controls.get('#' + id) };
}

const text = (n) => typeof n === 'string' ? n : (n.children || []).map(text).join('');

test('已裝的 skill 列出段數和授權，綁到不見的資料夾要講當作不用', async () => {
  const h = harness({
    dir: 'D:/data/skills',
    skills: [{ id: 'moss-image-prompt-skill', name: 'moss-image-prompt-skill', desc: '規則集', parts: 46, tokens: 52000,
      source: { repo: 'Moeblack/moss-image-prompt-skill', license: 'NOASSERTION' } }],
    bind: { moss: 'moss-image-prompt-skill', anima: 'gone' },
    guides: [{ name: 'moss', zh: 'MOSS-Image' }, { name: 'anima', zh: 'Anima' }],
    pick: { semantic: true, model: 'Qwen3-Embedding-0.6B-Q8_0.gguf' },
  });
  h.mountSkills(h.box);
  await new Promise(r => setImmediate(r));
  const items = h.get('skList').children;
  assert.equal(items.length, 1);
  assert.match(text(items[0]), /46 段.*授權見 LICENSE.*來源 github\.com\/Moeblack\/moss-image-prompt-skill/);
  const [moss, anima] = h.get('skBind').children.map(c => c.children[1]);
  assert.equal(moss.value, 'moss-image-prompt-skill');
  assert.equal(anima.value, 'gone');
  assert.match(text(anima.children.at(-1)), /找不到資料夾，當作不用/);
  assert.equal(h.get('skMsg').textContent, '');
  assert.match(h.get('skPick').textContent, /照意思挑（Qwen3-Embedding-0\.6B-Q8_0\.gguf/);
});

test('沒裝意思座標模型：說照關鍵字挑、去哪補', async () => {
  const h = harness({ skills: [], bind: {}, guides: [], dir: 'D:/data/skills', pick: { semantic: false, model: null } });
  h.mountSkills(h.box);
  await new Promise(r => setImmediate(r));
  assert.match(h.get('skPick').textContent, /照關鍵字挑.*AI 改寫、生圖助手/);
});

test('授權欄：自己放的、沒標的、有標的', () => {
  const h = harness({ skills: [], bind: {}, guides: [], dir: '' });
  assert.equal(h.license(null), '自己放的');
  assert.equal(h.license({ repo: 'a/b', license: null }), '沒標授權（只能自己看）');
  assert.equal(h.license({ repo: 'a/b', license: 'MIT' }), '授權 MIT');
});

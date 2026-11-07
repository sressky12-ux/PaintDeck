const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function harness() {
  const controls = new Map();
  for (const id of ['loraQ', 'loraFamily', 'loraCategory', 'loraTag', 'loraFav', 'loraTotal', 'loraMore', 'loraGrid']) {
    controls.set('#' + id, {
      value: '', checked: false, options: [], children: [], hidden: false, textContent: '',
      replaceChildren(...children) { this.children = children; this.options = children; },
      append(child) { this.children.push(child); },
    });
  }
  const boxes = [];
  const el = (tag, attrs = {}, ...kids) => {
    const e = { tag, attrs, kids, classes: new Set(String(attrs.class || '').split(' ').filter(Boolean)), get value() { return attrs.value || ''; },
      get dataset() { return { lora: attrs['data-lora'] }; },
      classList: { toggle(c, on) { if (on) e.classes.add(c); else e.classes.delete(c); } },
      replaceChildren(...c) { e.kids = c; } };
    if (attrs['data-lora'] !== undefined) boxes.push(e);
    return e;
  };
  const calls = [];
  const document = { querySelectorAll: () => boxes };
  const app = { page: 'loras', state: { models: { family_names: { anima: 'Anima（動漫）' } } } };
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/loras.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ $, el, app, document, api: async (url, opts) => { calls.push([url, opts]); }, toast() {}, openModal() {}, closeModal() {}, debounce: f => f });
  function $(id) { return controls.get(id); }
  vm.runInContext(source + '\nglobalThis.subject = { onState, renderPage, categoryOf, previewUrl, onCover };', context, { filename: 'loras.js' });
  return { ...context.subject, app, calls, get: id => controls.get('#' + id) };
}

test('五千個 LoRA 只先建立 60 張卡片，分類與搜尋仍掃完整清單', () => {
  const h = harness();
  const loras = Array.from({ length: 5000 }, (_, i) => ({
    name: `item_${i}.safetensors`, family: 'anima',
    agent: i % 2 ? { kind: 'style', desc: 'watercolor style' } : null,
    tags: i === 4999 ? ['rare-tag'] : [],
  }));
  h.onState({ models: { loras } });
  assert.equal(h.get('loraGrid').children.length, 60);
  assert.equal(h.get('loraTotal').textContent, '5000 / 5000 個');
  assert.equal(h.get('loraMore').hidden, false);
  h.renderPage(false);
  assert.equal(h.get('loraGrid').children.length, 120);
  h.get('loraQ').value = 'rare-tag';
  h.renderPage(true);
  assert.equal(h.get('loraGrid').children.length, 1);
  assert.equal(h.get('loraTotal').textContent, '1 / 5000 個');
  assert.equal(h.get('loraMore').hidden, true);
  assert.equal(h.categoryOf(loras[1]), 'style');
  assert.equal(h.categoryOf({ ...loras[1], category: 'character' }), 'character');
});

test('手動安裝的 LoRA 能使用模型旁的預覽圖', () => {
  const h = harness();
  const image = h.previewUrl({ name: 'krea2\\sample.safetensors', local_preview: true });
  assert.match(image, /\/api\/loras\/local-preview\?name=krea2%5Csample\.safetensors/);
});

test('模型篩選同一個底模只出現一次，名稱用新寫法（Anima、MOSS Image）', () => {
  const h = harness();
  h.onState({ models: { loras: [
    { name: 'a.safetensors', family: 'anima', base_model: 'Anima' },
    { name: 'b.safetensors', family: 'anima' },
    { name: 'moss\c.safetensors', family: 'moss', base_model: 'MOSS Image' },
    { name: 'moss\d.safetensors', family: 'moss' },
    { name: 'q.safetensors', family: 'qwen21' },
    { name: 'ltx.safetensors', family: 'other', base_model: 'LTXV 2.3' },
    { name: 'x.safetensors', family: 'other' },
  ] } });
  const labels = h.get('loraFamily').options.map((o) => o.attrs.value).filter(Boolean);
  assert.deepEqual(labels, ['Anima', 'LTXV 2.3', 'MOSS Image', 'Qwen 2.1', '其他']);
  h.get('loraFamily').value = 'MOSS Image';
  h.renderPage(true);
  assert.equal(h.get('loraGrid').children.length, 2);
});

// 2026-11-07：預載的 LoRA 沒預覽圖整片黑。state 標 cover 的送去 /api/loras/covers 抓，抓完的廣播換上圖
test('沒預覽圖的卡片先寫名字、請後端抓封面，抓到後換成圖', () => {
  const h = harness();
  h.app.state.settings = { blur_nsfw: true };
  const loras = [
    { name: 'a.safetensors', title: 'A 風格', family: 'anima', cover: true, cover_nsfw: 1 },
    { name: 'b.safetensors', family: 'anima', preview: 'b.jpg' },
    { name: 'c.safetensors', family: 'anima' },
  ];
  h.app.state.models.loras = loras;
  h.onState({ models: { loras } });
  const phOf = (name) => h.get('loraGrid').children.map((c) => c.kids[0]).find((b) => b.attrs['data-lora'] === name);
  // 只有標 cover、還沒有預覽圖的送去抓，一次一批
  assert.deepEqual(h.calls.map(([u, o]) => [u, o.json.names]), [['/api/loras/covers', ['a.safetensors']]]);
  assert.deepEqual(phOf('a.safetensors').kids.map((k) => k.kids[0]), ['A 風格', '正在抓封面…']);
  assert.equal(phOf('b.safetensors').kids[0].tag, 'img');
  assert.equal(phOf('b.safetensors').kids[0].attrs.src, '/lora_preview/b.jpg');
  assert.deepEqual(phOf('c.safetensors').kids.map((k) => k.kids[0]), ['c', '沒有預覽圖']);
  // 同一批不重送
  h.renderPage(true);
  assert.equal(h.calls.length, 1);
  // 抓到：換成 <img>；封面那張 C 站尺度大於 1 時照「模糊成人預覽圖」模糊
  h.onCover({ name: 'a.safetensors', preview: 'a.jpg', nsfw: 4 });
  const box = phOf('a.safetensors');
  assert.equal(box.kids[0].attrs.src, '/lora_preview/a.jpg');
  assert.ok(box.classes.has('blur'));
  // 抓不到：之後就寫「沒有預覽圖」
  h.onCover({ name: 'c.safetensors', failed: true });
  assert.equal(loras[2].coverFailed, true);
});

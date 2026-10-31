// 下載中心（ui/js/hub.js）：卡片與搜尋結果。用假的 DOM（insertBefore 跟瀏覽器一樣：參考節點不是子節點就丟例外）
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function node(tag, attrs = {}) {
  const n = {
    tag, attrs, children: [], parentNode: null, hidden: false, disabled: false, textContent: '', value: '', checked: false, style: {}, dataset: {},
    classList: { add() {}, remove() {}, toggle() {} },
    append(...kids) { for (const k of kids) { if (k == null || k === false) continue; if (typeof k === 'object') { k.parentNode?.children.splice(k.parentNode.children.indexOf(k), 1); k.parentNode = n; } n.children.push(k); } },
    insertBefore(k, ref) {
      if (ref == null) return n.append(k);
      const i = n.children.indexOf(ref);
      if (i < 0) throw new Error("Failed to execute 'insertBefore' on 'Node': The node before which the new node is to be inserted is not a child of this node.");
      k.parentNode = n;
      n.children.splice(i, 0, k);
    },
    replaceChildren(...kids) { n.children = []; n.append(...kids); },
    remove() { if (n.parentNode) { n.parentNode.children.splice(n.parentNode.children.indexOf(n), 1); n.parentNode = null; } },
    get isConnected() { return true; },
    closest() { return { scrollTop: 0 }; },
    querySelector() { return null; },
    setAttribute() {},
  };
  return n;
}

function harness({ items = [], apiError = null } = {}) {
  const ids = new Map();
  const $ = (sel) => { if (!ids.has(sel)) ids.set(sel, node('div')); return ids.get(sel); };
  const el = (tag, attrs = {}, ...kids) => { const n = node(tag, attrs); n.append(...kids.map((k) => (typeof k === 'string' ? { text: k } : k))); return n; };
  const api = async () => { if (apiError) throw new Error(apiError); return { items, next: null }; };
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/hub.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({
    $, $$: () => [], el, api, toast() {}, fmtN: String, fmtBytes: (n) => `${n}B`, openModal() {}, makeDrop() {}, pickFile() {}, uploadFile() {},
    store: { get: (_k, d) => d, set() {} }, app: { state: { settings: {} } }, models: {}, URLSearchParams, console, confirm: () => true, setTimeout, clearTimeout,
  });
  vm.runInContext(source + '\nglobalThis.subject = { card, search, cur: () => cur };', context, { filename: 'hub.js' });
  return { ...context.subject, $ };
}

const texts = (n) => (n.text != null ? [n.text] : [n.textContent || '', ...(n.children || []).flatMap(texts)]);

const civitaiLora = (name, files) => ({
  source: 'civitai', id: '1', kind: 'lora', name, author: 'u', family: 'moss', family_label: 'MOSS-Image', usable: true, base: 'MOSS Image',
  cover: null, downloads: 1, likes: 0, size: 100, nsfw: false, tags: [], triggers: [], url: 'https://civitai.com/models/1',
  versions: [{ id: 11, name: 'v1', base: 'MOSS Image', family: 'moss', usable: true, triggers: [], files }],
});

test('一個版本有好幾個模型檔（例如 4 步、8 步）的卡片畫得出來，檔案選單在觸發詞前面', () => {
  const h = harness();
  const it = civitaiLora('Moss Image Turbo LoRas', [
    { name: 'moss_4step.safetensors', size: 10, primary: true }, { name: 'moss_8step.safetensors', size: 20, primary: false },
  ]);
  const c = h.card(it);
  const body = c.children[1];
  const sel = body.children.find((k) => k.tag === 'select');
  assert.ok(sel, '要有選檔案的下拉選單');
  assert.ok(body.children.indexOf(sel) < body.children.findIndex((k) => k.attrs?.class === 'trig'), '檔案選單放在觸發詞前面');
});

test('有一張卡片畫不出來時，其他卡片照樣顯示，畫面也看得到原因', async () => {
  const good = civitaiLora('Good', [{ name: 'good.safetensors', size: 1, primary: true }]);
  const bad = { ...civitaiLora('Bad', []), id: '2', versions: null, get name() { throw new Error('壞掉的資料'); } };
  const h = harness({ items: [good, bad, { ...good, id: '3', name: 'Good 2' }] });
  await h.search(false);
  const all = texts(h.$('#hubGrid')).join('|');
  assert.match(all, /Good\|/);
  assert.match(all, /Good 2/);
  assert.match(all, /壞掉的資料/);
});

test('來源重複送同一筆（魔塔推薦排序翻頁會這樣）只畫一張', async () => {
  const a = civitaiLora('First', [{ name: 'a.safetensors', size: 1, primary: true }]);
  const h = harness({ items: [a, { ...a, name: 'Dup' }, { ...a, id: '9', name: 'Other' }] });
  await h.search(false);
  const all = texts(h.$('#hubGrid')).join('|');
  assert.match(all, /First/);
  assert.match(all, /Other/);
  assert.doesNotMatch(all, /Dup/);
});

test('搜尋失敗時錯誤訊息留在畫面上', async () => {
  const h = harness({ apiError: 'C 站回應 500' });
  await h.search(false);
  assert.match(texts(h.$('#hubGrid')).join('|'), /C 站回應 500/);
});

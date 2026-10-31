// node --test tests/assets_ui.test.cjs
// 不需瀏覽器套件：在隔離的 VM 跑實際模組，以延遲 API 回應重現競爭。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const tick = () => new Promise(setImmediate);
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function element(tag, attrs = {}, ...children) {
  return Object.assign({ tag, value: '', checked: false, disabled: false, children, hidden: false,
    classList: { add() {}, toggle() {} }, focus() {}, remove() {},
    append(...kids) { this.children.push(...kids); },
    replaceChildren(...kids) { this.children = kids; },
    addEventListener() {}, dispatchEvent() {}, querySelectorAll() { return []; },
  }, attrs);
}
function moduleContext(file, globals, exposed) {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js', file), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ structuredClone, console, Event, ...globals });
  vm.runInContext(source + `\nglobalThis.subject = {${exposed}};`, context, { filename: file });
  return context.subject;
}
function editor(api, extras = {}) {
  const nodes = new Map();
  const node = (id) => {
    if (!nodes.has(id)) nodes.set(id, element('input'));
    return nodes.get(id);
  };
  node('asForm').querySelectorAll = () => [...nodes.values()].filter((e) => e !== node('asForm'));
  const messages = [];
  const subject = moduleContext('assets.js', {
    api, el: element, $: node, $$: () => [], toast: (s) => messages.push(s),
    library: { edit() {}, saveDialog() {} },
    document: { getElementById: node, addEventListener() {}, body: element('body') },
    ...extras,
  }, 'open, newOne, save, addFile, removeFile, init, current: () => cur');
  return { subject, node, messages };
}
const asset = (id, name) => ({ id, name, kind: 'char', data: {}, tags: [] });

test('儲存期間固定表單與對象，連按不重複新增', async () => {
  const reply = deferred(), calls = [];
  const e = editor((url, opts) => { calls.push([url, opts.json]); return reply.promise; });
  e.subject.newOne(); e.node('asName').value = 'A';
  const saving = e.subject.save();
  assert.equal(e.node('asName').disabled, true);
  e.subject.open(asset(2, 'B')); e.subject.newOne();
  await e.subject.save();
  assert.equal(calls.length, 1);
  assert.equal(e.node('asName').value, 'A');
  reply.resolve(asset(1, 'A'));
  await saving;
  assert.equal(e.subject.current().id, 1);
  assert.equal(e.node('asName').disabled, false);
});

test('加檔先存表單，整段操作維持鎖定並使用存好的 id', async () => {
  const saved = deferred(), attached = deferred(), calls = [];
  const e = editor((url, opts) => { calls.push([url, opts.json]); return calls.length === 1 ? saved.promise : attached.promise; });
  e.subject.newOne(); e.node('asName').value = 'new';
  const adding = e.subject.addFile({ input: 'a.png', role: 'src' });
  assert.equal(calls.length, 1);
  saved.resolve(asset(7, 'new')); await tick();
  assert.equal(calls[1][0], '/api/assets/7/file');
  assert.equal(e.node('asName').disabled, true);
  e.subject.open(asset(8, 'other'));
  attached.resolve({ ...asset(7, 'new'), data: { images: [{ file: '7_1.png', role: 'src' }] } });
  await adding;
  assert.equal(e.subject.current().id, 7);
  assert.equal(e.subject.current().data.images.length, 1);
  assert.equal(e.node('asName').disabled, false);
});

test('儲存失敗保留輸入並解除停用', async () => {
  const e = editor(async () => { throw Error('DB failed'); });
  e.subject.open(asset(1, 'A')); e.node('asName').value = 'edited';
  assert.equal(await e.subject.save(), null);
  assert.equal(e.node('asName').value, 'edited');
  assert.equal(e.node('asName').disabled, false);
  assert.deepEqual(e.messages, ['DB failed']);
});

test('開檔視窗等待期間換資產，不把原本選的圖上傳到新對象', async () => {
  const picked = deferred(), calls = [];
  const e = editor(async () => { throw Error('unexpected save'); }, {
    pickFile: () => picked.promise, uploadFile: async () => { calls.push('upload'); },
  });
  e.subject.init(); e.subject.open(asset(1, 'A'));
  const picking = e.node('asImgUp').onclick();
  e.subject.open(asset(2, 'B'));
  picked.resolve({ name: 'a.png' }); await picking;
  assert.equal(e.subject.current().id, 2);
  assert.equal(calls.length, 0);
});

test('新資產存好後加檔失敗，重試沿用同一個 id', async () => {
  const saves = [];
  const e = editor(async (url, opts) => {
    if (url === '/api/lib') { saves.push(opts.json); return asset(7, 'new'); }
    throw Error('copy failed');
  });
  e.subject.newOne(); e.node('asName').value = 'new';
  await e.subject.addFile({ input: 'a.png', role: 'src' });
  assert.equal(e.subject.current().id, 7);
  assert.equal(e.node('asName').disabled, false);
  await e.subject.addFile({ input: 'a.png', role: 'src' });
  assert.equal(saves[1].id, 7);
  assert.equal(e.node('asName').value, 'new');
});

function drawer(api, family = 'qwen21', initial = 0) {
  const refs = Array.from({ length: initial }, (_, i) => ({ name: `old${i}` }));
  const texts = [], messages = [];
  let fam = family;
  const d = { ctx: { family: () => fam, insert: (text) => texts.push(text), images: {
    ref: { max: 10, count: () => refs.length, add: (r) => refs.length < 10 ? refs.push(r) : null },
  } } };
  const subject = moduleContext('assetdrawer.js', {
    api, el: element, toast: (s) => messages.push(s), assets: {}, store: { get: () => false },
  }, 'use');
  return { use: (it) => subject.use(d, it), refs, texts, messages, family: (f) => { fam = f; } };
}

test('Qwen 連點按順序加入；部分複製失敗仍替成功的圖片寫正確標記', async () => {
  const first = deferred(), calls = [];
  const d = drawer(async (url, opts) => {
    calls.push(url);
    if (url.endsWith('/apply')) return { text: 'look', images: url.includes('/1/') ? ['a', 'bad', 'b'] : ['c'] };
    if (opts.json.file === 'a') return first.promise;
    if (opts.json.file === 'bad') throw Error('copy failed');
    return { name: opts.json.file };
  });
  const a = d.use(asset(1, 'A')), b = d.use(asset(2, 'B'));
  await tick();
  assert.equal(calls.includes('/api/assets/2/apply'), false);
  first.resolve({ name: 'a' }); await Promise.all([a, b]);
  assert.deepEqual(d.refs.map((r) => r.name), ['a', 'b', 'c']);
  assert.deepEqual(d.texts, ['A（<image1><image2>）：look', 'B（<image3>）：look']);
  assert.deepEqual(d.messages.filter((s) => s.includes('最多')), []);
});

test('Qwen 已滿時不複製、不寫空括號，仍插文字', async () => {
  let copies = 0;
  const d = drawer(async (url) => {
    if (url.endsWith('/apply')) return { text: 'look', images: ['a', 'b'] };
    copies++; return { name: 'a' };
  }, 'qwen21', 10);
  await d.use(asset(1, 'A'));
  assert.equal(copies, 0);
  assert.deepEqual(d.texts, ['look']);
  assert.equal(d.refs.length, 10);
  assert.ok(d.messages.some((s) => s.includes('最多 10')));
});

test('Qwen 複製期間手動加圖，提交時以最新張數編號及提醒', async () => {
  const copy = deferred();
  const d = drawer(async (url) => url.endsWith('/apply') ? { text: '', images: ['a'] } : copy.promise, 'qwen21', 3);
  const applying = d.use(asset(1, 'A')); await tick();
  d.refs.push({ name: 'manual' }); copy.resolve({ name: 'a' }); await applying;
  assert.deepEqual(d.texts, ['A（<image5>）']);
  assert.ok(d.messages.some((s) => s.includes('目前有 5 張')));
});

test('Qwen 複製期間換模型，不往新模型塞舊結果', async () => {
  const copy = deferred();
  const d = drawer(async (url) => url.endsWith('/apply') ? { text: 'look', images: ['a'] } : copy.promise);
  const applying = d.use(asset(1, 'A')); await tick();
  d.family('moss'); copy.resolve({ name: 'a' }); await applying;
  assert.equal(d.refs.length, 0); assert.equal(d.texts.length, 0);
});

test('助手：圖放進附圖，句子裡寫跟附圖一樣的編號', async () => {
  const refs = [], texts = [];
  const d = { ctx: { family: () => 'agent', insert: (x) => texts.push(x), images: { ref: {
    max: 4, count: () => refs.length, tag: (n) => `圖 ${n}`, add: (r) => { refs.push(r); return refs.length + 2; },
  } } } };
  const subject = moduleContext('assetdrawer.js', {
    api: async (url, opts) => url.endsWith('/apply') ? { text: '橘髮', images: ['a', 'b'] } : { name: opts.json.file },
    el: element, toast() {}, assets: {}, store: { get: () => false },
  }, 'use');
  await subject.use(d, asset(1, '小克'));
  assert.deepEqual(refs.map((r) => r.name), ['a', 'b']);
  assert.deepEqual(texts, ['小克（圖 3、圖 4）：橘髮']);
});

test('H3：頁面自己放圖（place），文字照插，不走 <imageN>', async () => {
  const placed = [], texts = [];
  const d = { ctx: { family: () => 'h3', insert: (x) => texts.push(x), images: { place: async (it, p, toInput) => { placed.push(it.name, ...p.images, (await toInput('a')).name); } } } };
  const subject = moduleContext('assetdrawer.js', {
    api: async (url, opts) => url.endsWith('/apply') ? { text: '橘髮', images: ['a', 'b'], views: { a: 'portrait' } } : { name: 'copied_' + opts.json.file },
    el: element, toast() {}, assets: {}, store: { get: () => false },
  }, 'use');
  await subject.use(d, asset(1, '小克'));
  assert.deepEqual(placed, ['小克', 'a', 'b', 'copied_a']);
  assert.deepEqual(texts, ['橘髮']);
});

test('H3 的聲音及語言會寫進提示詞（LTX 已退場）', async () => {
  for (const family of ['h3']) {
    const d = drawer(async () => ({ text: 'look', images: [], voice: 'soft voice', lang: 'Japanese' }), family);
    await d.use(asset(1, 'Alice'));
    assert.equal(d.texts.length, 1);
    assert.match(d.texts[0], /Alice/); assert.match(d.texts[0], /soft voice/); assert.match(d.texts[0], /Japanese/);
  }
});

// node --test --test-isolation=none tests/promptzh.test.cjs
// 提示詞框的中英對照（ui/js/promptzh.js）：怎麼切段、哪些算 tag、哪些併成句子交給 AI
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// 假的對照表：/api/tags/zh 只認得這幾個
const DICT = { '1girl': '1個女孩', 'long hair': '長髮', 'smile': '微笑', 'red': '紅色', 'blue': '藍色', 'masterpiece': '傑作（品質詞）' };

function load(aiSeen = []) {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/promptzh.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const calls = [];
  const sets = [];
  const context = vm.createContext({
    console,
    store: { get: (k, d) => (k === 'zhAi' ? aiSeen : d), set: (k, v) => sets.push([k, v]) },
    debounce: (fn) => fn,
    current: () => 'zh-TW',
    toast() {},
    el: (tag, attrs, ...kids) => ({ tag, attrs, kids }),
    api: async (url, { json }) => {
      calls.push([url, json]);
      return { zh: Object.fromEntries(json.tags.filter((t) => DICT[t]).map((t) => [t, DICT[t]])) };
    },
  });
  vm.runInContext(source + `
const mk = (v, key = "promptZh") => {
  const btn = { classList: { toggle() {} }, setAttribute() {}, hidden: false, onclick: null };
  return create({ value: v, addEventListener() {} }, { replaceChildren() {} }, btn, key);
};
globalThis.subject = {
  pieces, parse,
  mk,
  async units(v) { const i = mk(v, "promptZh"); await update(i); return i.units; },
  update,
};`, context, { filename: 'promptzh.js' });
  return { ...context.subject, calls, sets };
}
const plain = (v) => JSON.parse(JSON.stringify(v));

test('pieces：逗號切段，權重括號、{a|b} 裡的逗號不切，換行另起一行', () => {
  const { pieces } = load();
  const v = '1girl, (long hair, smile:1.2),{red|blue}\nA woman sits.';
  assert.deepEqual(plain(pieces(v)).map((p) => [p.text, p.line, v.slice(p.a, p.b)]), [
    ['1girl', 0, '1girl'],
    ['(long hair, smile:1.2)', 0, '(long hair, smile:1.2)'],
    ['{red|blue}', 0, '{red|blue}'],
    ['A woman sits.', 1, 'A woman sits.'],
  ]);
  // 跳脫的括號不算括號
  assert.deepEqual(plain(pieces('ui \\(blue archive\\), smile')).map((p) => p.text), ['ui \\(blue archive\\)', 'smile']);
});

test('對照：tag 查表、權重與隨機寫出來，<image1>／@畫師 直接給中文', async () => {
  const s = load();
  const u = plain(await s.units('masterpiece, 1girl, (long hair, smile:1.2), {red|blue}, <image1>, @wlop'));
  assert.deepEqual(u.map((x) => [x.kind, x.en, x.zh, x.need]), [
    ['tag', 'masterpiece', '傑作（品質詞）', []],
    ['tag', '1girl', '1個女孩', []],
    ['tag', '(long hair, smile:1.2)', '長髮、微笑（權重 1.2）', []],
    ['tag', '{red|blue}', '紅色／藍色（每張隨機挑一個）', []],
    ['tag', '<image1>', '第 1 張參考圖', []],
    ['tag', '@wlop', '畫師：wlop', []],
  ]);
  // 隨機選項夾在字中間：代回去各查；查不到的那個交給 AI
  const v = plain(await s.units('{red|blue} hair, {smile|zzbar}'));
  assert.deepEqual(v.map((x) => [x.zh, x.need]), [[null, ['red hair', 'blue hair']], ['微笑／zzbar（每張隨機挑一個）', ['zzbar']]]);
});

test('對照：同一行連續查不到的併成一句交給 AI；短的查不到算 tag；換行就斷開', async () => {
  const s = load();
  const v = '1girl, A woman sitting on a bench, smiling softly at sunset, long hair, zzfoo\nShe holds a book.';
  const u = plain(await s.units(v));
  assert.deepEqual(u.map((x) => [x.kind, x.en, x.zh, x.need]), [
    ['tag', '1girl', '1個女孩', []],
    ['sent', 'A woman sitting on a bench, smiling softly at sunset', null, ['A woman sitting on a bench, smiling softly at sunset']],
    ['tag', 'long hair', '長髮', []],
    ['tag', 'zzfoo', null, ['zzfoo']],
    ['sent', 'She holds a book.', null, ['She holds a book.']],
  ]);
  // 句子的位置要對得回原文（點了會選到那一段）
  const sent = u[1];
  assert.equal(v.slice(sent.a, sent.b), 'A woman sitting on a bench, smiling softly at sunset');
});

test('對照：AI 翻過的記住，不再列進要翻的；查過的字不重查', async () => {
  const s = load([['She holds a book.', '她拿著一本書。'], ['zzfoo', '某某']]);
  const v = 'zzfoo, smile\nShe   holds a book.';
  const u = plain(await s.units(v));
  assert.deepEqual(u.map((x) => [x.kind, x.zh, x.need]), [
    ['tag', '某某', []],
    ['tag', '微笑', []],
    ['sent', '她拿著一本書。', []],
  ]);
  // AI 記過的不送對照表；第二次對照全部有快取，不再打 API
  assert.deepEqual(plain(s.calls.map((c) => c[1].tags)), [['smile']]);
  await s.units(v);
  assert.equal(s.calls.length, 1);
});

test('對照：中文段落原樣列出、不送 AI', async () => {
  const s = load();
  const u = plain(await s.units('1girl, 穿紅色洋裝的女孩'));
  assert.deepEqual(u.map((x) => [x.en, x.zh, x.need]), [['1girl', '1個女孩', []], ['穿紅色洋裝的女孩', '', []]]);
});

test('兩個實例互不干擾：各自的切段、開關，快取共用不重查', async () => {
  const s = load();
  const a = s.mk('1girl, long hair', 'promptZh');
  const b = s.mk('smile, red', 'negZh');
  await s.update(a);
  await s.update(b);
  // 切段結果各自算，不會互相覆蓋
  assert.deepEqual(plain(a.units).map((x) => x.en), ['1girl', 'long hair']);
  assert.deepEqual(plain(b.units).map((x) => x.en), ['smile', 'red']);
  // 開關獨立：各按各的按鈕，各自寫進自己的 store key
  a.btn.onclick();
  a.btn.onclick();
  b.btn.onclick();
  assert.equal(a.on, false);
  assert.equal(b.on, true);
  assert.deepEqual(plain(s.sets.filter((x) => x[0] !== 'zhAi')), [['promptZh', true], ['promptZh', false], ['negZh', true]]);
  // 對照表快取共用：b 換成 a 剛查過的字，不送 API
  b.ta.value = 'long hair, 1girl';
  b.units = [];
  await s.update(b);
  assert.equal(s.calls.length, 2);  // 一開始 a、b 各查一次後就沒有了
  assert.deepEqual(plain(b.units).map((x) => [x.en, x.zh]), [['long hair', '長髮'], ['1girl', '1個女孩']]);
  // AI 翻譯快取（zhAi）也共用：load 給的 aiSeen 兩個實例都用得到
  const s2 = load([['dmke', '某設定的專有名詞']]);
  const a2 = s2.mk('dmke');
  const b2 = s2.mk('dmke, blue');
  await s2.update(a2);
  await s2.update(b2);
  assert.deepEqual(plain(a2.units).map((x) => x.zh), ['某設定的專有名詞']);
  assert.deepEqual(plain(b2.units).map((x) => x.zh), ['某設定的專有名詞', '藍色']);
});

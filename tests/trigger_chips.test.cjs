// node --test --test-isolation=none tests/trigger_chips.test.cjs
// 生成頁的觸發詞標籤（ui/js/gen.js 的 splitTags／normTag／insertAfterQuality／toggleTrigger）
// C 站的觸發詞常常一整串帶逗號、或尾巴多一個逗號：以前比對整串永遠對不上，標籤不會亮、一直重複加、按了沒反應
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const SRC = fs.readFileSync(path.join(__dirname, '../ui/js/gen.js'), 'utf8');

// 從 gen.js 抓出一個函式（大括號配對到結尾）
function grab(head) {
  const i = SRC.indexOf(head);
  assert.ok(i >= 0, `gen.js 找不到 ${head}`);
  let depth = 0;
  for (let k = SRC.indexOf('{', i); k < SRC.length; k++) {
    if (SRC[k] === '{') depth++;
    else if (SRC[k] === '}' && --depth === 0) return SRC.slice(i, k + 1).replace(/^export /, '');
  }
  throw new Error(`${head} 大括號沒配對`);
}

function load(prompt = '') {
  const box = { value: prompt };
  const quality = SRC.slice(SRC.indexOf('const QUALITY'), SRC.indexOf('\n', SRC.indexOf('const QUALITY')));
  const line = (head) => SRC.slice(SRC.indexOf(head), SRC.indexOf('\n', SRC.indexOf(head)));
  const code = [line('const TAG_SEP'), grab('function splitTags'), grab('function normTag'), line('const promptTagSet'), grab('function removeTags'),
    quality, grab('export function insertAfterQuality'), grab('function insertTriggers'), grab('function toggleTrigger'), grab('function watchPromptValue')].join('\n');
  const context = vm.createContext({ F: () => box, save() {}, renderTrig() {} });
  vm.runInContext(code + '\nglobalThis.subject = {splitTags, normTag, removeTags, insertAfterQuality, toggleTrigger, watchPromptValue};', context, { filename: 'gen.js' });
  return { box, ...context.subject };
}

const LONG = 'malfoid, green necktie, yellow gold earrings, green hogwarts school uniform, grey sweater, black skirt, black pantyhose,';
const BASE = 'masterpiece, best quality, safe, 1girl, smile';

test('一整串帶逗號的觸發詞：第一下整串加在品質詞後面，第二下整串拿掉', () => {
  const { box, toggleTrigger } = load(BASE);
  toggleTrigger(LONG);
  assert.equal(box.value, 'masterpiece, best quality, safe, malfoid, green necktie, yellow gold earrings, green hogwarts school uniform, grey sweater, black skirt, black pantyhose, 1girl, smile');
  toggleTrigger(LONG);
  assert.equal(box.value, BASE);
});

test('尾巴多逗號的觸發詞（@xipa2026late03-05,）不會一直重複加', () => {
  const { box, toggleTrigger } = load(BASE);
  toggleTrigger('@xipa2026late03-05,');
  assert.equal(box.value, 'masterpiece, best quality, safe, @xipa2026late03-05, 1girl, smile');
  toggleTrigger('@xipa2026late03-05,');
  assert.equal(box.value, BASE);
});

test('只有一部分在提示詞裡：按下去補齊缺的，不重複加已經有的（底線、權重、換行都算有）', () => {
  const { box, toggleTrigger } = load('masterpiece, green_necktie, (malfoid:1.2)\n1girl');
  toggleTrigger(LONG);
  const tags = box.value.split(/[,\n]/).map((s) => s.trim());
  assert.equal(tags.filter((t) => /necktie/.test(t)).length, 1);
  assert.equal(tags.filter((t) => /malfoid/.test(t)).length, 1);
  assert.ok(tags.includes('black pantyhose'));
});

test('normTag：大小寫、底線、跳脫括號、權重都視為同一個 tag', () => {
  const { normTag } = load();
  assert.equal(normTag('Green_Necktie'), 'green necktie');
  assert.equal(normTag('(malfoid:1.2)'), 'malfoid');
  assert.equal(normTag('ui \\(blue archive\\)'), 'ui (blue archive)');
});

test('insertAfterQuality：舊的呼叫方式（自動加入觸發詞）也會把整串拆開、去掉重複', () => {
  const { insertAfterQuality } = load();
  assert.equal(insertAfterQuality('masterpiece, 1girl', ['a, b,', 'B', 'c']), 'masterpiece, a, b, c, 1girl');
  assert.equal(insertAfterQuality('', ['x,']), 'x');
  assert.equal(insertAfterQuality('masterpiece, x', ['x,']), 'masterpiece, x');
});

test('全形逗號、((多層括號))、[降權]、權重寫法都比對得到，移除時只拿掉那顆', () => {
  const { box, toggleTrigger, normTag } = load('masterpiece，((caustics:1.3))，1girl\n[refraction], smile');
  assert.equal(normTag('((caustics:1.3))'), 'caustics');
  assert.equal(normTag('[refraction]'), 'refraction');
  toggleTrigger('caustics');
  assert.equal(box.value, 'masterpiece，1girl\n[refraction], smile');
  toggleTrigger('refraction');
  assert.equal(box.value, 'masterpiece，1girl\nsmile');
});

test('removeTags：拿掉第一個、最後一個、中間的都不留多餘逗號', () => {
  const { removeTags } = load();
  const set = (...a) => new Set(a);
  assert.equal(removeTags('x, a, b', set('x')), 'a, b');
  assert.equal(removeTags('a, b, x', set('x')), 'a, b');
  assert.equal(removeTags('a, x, b', set('x')), 'a, b');
  assert.equal(removeTags('a, x, , b,', set('x')), 'a, b');
  assert.equal(removeTags('x', set('x')), '');
});

test('權重寫在品質詞上也認得是品質詞，觸發詞照樣插在它後面', () => {
  const { insertAfterQuality } = load();
  assert.equal(insertAfterQuality('(masterpiece:1.2), score_9_up, 1girl', ['trig']), '(masterpiece:1.2), score_9_up, trig, 1girl');
});

test('watchPromptValue：程式直接改 value 也會通知，讀寫照常', () => {
  const { watchPromptValue } = load();
  class Fake { constructor() { this._v = ''; this.listeners = []; } addEventListener(ev, cb) { this.listeners.push(ev); } }
  Object.defineProperty(Fake.prototype, 'value', { configurable: true, get() { return this._v; }, set(v) { this._v = String(v); } });
  const ta = new Fake();
  let n = 0;
  watchPromptValue(ta, () => n++);
  ta.value = 'a, b';
  assert.equal(ta.value, 'a, b');
  assert.equal(n, 1);
  assert.deepEqual(ta.listeners, ['input']);
});

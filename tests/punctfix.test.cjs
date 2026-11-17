// node --test --test-isolation=none tests/punctfix.test.cjs
// 提示詞框：英文 tag 後面用中文輸入法打的全形標點自動換半形（ui/js/autocomplete.js 的 fixPunct）
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load(on = true) {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/autocomplete.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({
    console, Event: class { constructor(t) { this.type = t; } },
    store: { get: (k, d) => (k === 'punctFix' ? on : d) },
    document: { execCommand: () => false }, // 測試環境沒有 execCommand：走 setRangeText
    el() {}, debounce: (f) => f, escTag: (s) => s, fmtN: String,
  });
  vm.runInContext(source + '\nglobalThis.subject = { fixPunct };', context, { filename: 'autocomplete.js' });
  return context.subject.fixPunct;
}

// 假的 textarea：打完一個字之後的狀態（游標在最後）
function typed(value, caret = value.length) {
  return {
    value, selectionStart: caret, selectionEnd: caret,
    setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; },
    setRangeText(t, a, b) { this.value = this.value.slice(0, a) + t + this.value.slice(b); this.selectionStart = this.selectionEnd = a + t.length; },
    dispatchEvent() {},
  };
}

const fix = load();

test('英文 tag 後面的全形逗號、頓號換成「, 」', () => {
  for (const [v, want] of [['1girl，', '1girl, '], ['long hair、', 'long hair, '], ['smile ，', 'smile , ']]) {
    const t = typed(v);
    assert.equal(fix(t), true, v);
    assert.equal(t.value, want);
  }
});

test('權重括號與冒號：（long hair：1.2）→ (long hair:1.2)', () => {
  let t = typed('1girl, （');
  assert.equal(fix(t), true);
  t = typed(t.value + 'long hair：');
  assert.equal(fix(t), true);
  t = typed(t.value + '1.2）');
  assert.equal(fix(t), true);
  assert.equal(t.value, '1girl, (long hair:1.2)');
  const first = typed('（');
  assert.equal(fix(first), true);
  assert.equal(first.value, '(');
});

test('中文句子裡的不動', () => {
  for (const v of ['一個女孩，', '她笑著說：', '紅色（', '長髮）']) {
    const t = typed(v);
    assert.equal(fix(t), false, v);
    assert.equal(t.value, v);
  }
});

test('在中間打：後面已經有空白就不再補空白', () => {
  const t = typed('1girl， smile', '1girl，'.length);
  assert.equal(fix(t), true);
  assert.equal(t.value, '1girl, smile');
});

test('關掉就不換', () => {
  const off = load(false);
  const t = typed('1girl，');
  assert.equal(off(t), false);
  assert.equal(t.value, '1girl，');
});

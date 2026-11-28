// node --test --test-isolation=none tests/locked_chips.test.cjs
// 生成頁的鎖定角色標籤（ui/js/lockedchips.js）：送出時接在提示詞最前面的文字
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/lockedchips.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console });
  vm.runInContext(source + '\nglobalThis.subject = {chipText, lockedPrefix, withLocked, addChip};', context, { filename: 'lockedchips.js' });
  return context.subject;
}
const plain = (v) => JSON.parse(JSON.stringify(v));

const MIKA = { id: 7, name: '美香', zh: '女性，二十出頭，橘色長髮。', en: 'female, early 20s, long orange hair.', tags: '1girl, orange hair, long hair', refs: ['as_7_p.png', 'as_7_s.png'] };
const REN = { id: 9, name: '蓮', zh: '男性，黑短髮。', en: 'male, short black hair.', tags: '1boy, black hair', refs: [] };

test('lockedPrefix：Anima 用 tag、逗號接；其他一個角色一行、英文', () => {
  const { lockedPrefix } = load();
  assert.equal(lockedPrefix([MIKA, REN], 'anima'), '1girl, orange hair, long hair, 1boy, black hair');
  assert.equal(lockedPrefix([MIKA, REN], 'moss'), '美香: female, early 20s, long orange hair.\n蓮: male, short black hair.');
  assert.equal(lockedPrefix([], 'anima'), '');
});

test('lockedPrefix：Qwen 2.1 用 <imageN>，MiniMax H3 Image 用 <Picture N>，被刪掉的圖不寫', () => {
  const { lockedPrefix } = load();
  assert.equal(lockedPrefix([MIKA], 'qwen21', ['x.png', 'as_7_p.png', 'as_7_s.png']), '美香（<image2><image3>）：female, early 20s, long orange hair.');
  assert.equal(lockedPrefix([MIKA], 'qwen21', ['as_7_s.png']), '美香（<image1>）：female, early 20s, long orange hair.');
  assert.equal(lockedPrefix([MIKA, REN], 'qwen21', []), '美香：female, early 20s, long orange hair.\n蓮：male, short black hair.');
  assert.equal(lockedPrefix([MIKA], 'mmimage', ['x.png', 'as_7_p.png', 'as_7_s.png']), '美香 (<Picture 2> <Picture 3>): female, early 20s, long orange hair.');
  assert.equal(lockedPrefix([MIKA], 'mmimage', []), '美香: female, early 20s, long orange hair.');
});

test('withLocked：接在最前面；批次每行都接；已經接過不重複', () => {
  const { withLocked } = load();
  assert.equal(withLocked('masterpiece, best quality, smile', [MIKA], 'anima'), '1girl, orange hair, long hair, masterpiece, best quality, smile');
  assert.equal(withLocked('  ', [MIKA], 'anima'), '1girl, orange hair, long hair');
  assert.equal(withLocked('She waves.', [MIKA], 'custom'), '美香: female, early 20s, long orange hair.\nShe waves.');
  const once = withLocked('She waves.', [MIKA], 'custom');
  assert.equal(withLocked(once, [MIKA], 'custom'), once);
  assert.equal(withLocked('smile', [], 'anima'), 'smile');
  const lines = ['run', 'jump'].map((l) => withLocked(l, [REN], 'anima'));
  assert.deepEqual(lines, ['1boy, black hair, run', '1boy, black hair, jump']);
});

test('chipText：沒有英文設定的標籤在非 Anima 模型下不接字', () => {
  const { chipText } = load();
  assert.equal(chipText({ id: 1, name: 'A', en: '', tags: 'x' }, 'qwen21', []), '');
  assert.equal(chipText({ id: 1, name: 'A', en: '', tags: 'x' }, 'anima', []), 'x');
});

test('addChip：同一個角色再點一次換成新設定、順序不變、參考圖換成這次的', () => {
  const { addChip } = load();
  let list = addChip([], MIKA);
  list = addChip(list, REN);
  list = addChip(list, { ...MIKA, en: 'NEW', refs: ['as_7_p.png', 'more.png'] });
  assert.deepEqual(plain(list).map((c) => [c.id, c.en]), [[7, 'NEW'], [9, 'male, short black hair.']]);
  assert.deepEqual(plain(list[0].refs), ['as_7_p.png', 'more.png']);
  // 這次沒放圖（Anima 時點的）：留著上次記的
  list = addChip(list, { ...MIKA, refs: [] });
  assert.deepEqual(plain(list[0].refs), ['as_7_p.png', 'more.png']);
});

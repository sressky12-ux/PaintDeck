// node --test --test-isolation=none tests/taglib.test.cjs
// 生成頁詞庫抽屜（ui/js/taglib.js）：點詞加進提示詞框、再點拿掉
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/taglib.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '')
    .replace(/import\.meta\.url/g, '"file:///ui/js/taglib.js"').replace(/\bimport\(/g, '(async () => ({}))(');
  const context = vm.createContext({ console, store: { get: (k, d) => d, set() {} }, toast() {}, el: () => ({}) });
  vm.runInContext(source + '\nglobalThis.subject = { addTo, removeFrom, inBox, tokens, fmtCount };', context, { filename: 'taglib.js' });
  return context.subject;
}

// 假的 textarea：value＋游標
function ta(value, caret = value.length) {
  return {
    value, selectionStart: caret, selectionEnd: caret,
    setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; },
  };
}

const { addTo, removeFrom, inBox, fmtCount } = load();

test('加在最後面會補好逗號', () => {
  const t = ta('1girl, smile');
  addTo(t, 'long hair');
  assert.equal(t.value, '1girl, smile, long hair, ');
  assert.equal(t.selectionStart, t.value.length);
  const e = ta('');
  addTo(e, 'solo');
  assert.equal(e.value, 'solo, ');
});

test('游標在逗號後面：插在那裡，後面的詞不動', () => {
  const t = ta('1girl, smile', '1girl,'.length);
  addTo(t, 'long hair');
  assert.equal(t.value, '1girl, long hair, smile');
});

test('游標卡在詞中間：插到那個詞後面，不會切斷它', () => {
  const t = ta('1girl, smile, solo', '1girl, sm'.length);
  addTo(t, 'blush');
  assert.equal(t.value, '1girl, smile, blush, solo');
});

test('拿掉：大小寫、底線、空白都算同一個，其他地方原樣（含換行）', () => {
  const t = ta('masterpiece,\n1girl, Long_Hair, smile');
  removeFrom(t, 'long hair');
  assert.equal(t.value, 'masterpiece,\n1girl, smile');
  const last = ta('1girl, smile');
  removeFrom(last, 'smile');
  assert.equal(last.value, '1girl');
  const only = ta('smile');
  removeFrom(only, 'smile');
  assert.equal(only.value, '');
});

test('判斷在不在框裡：單一詞才算；整段提示詞（有逗號）不算', () => {
  const t = ta('1girl, long hair, smile');
  assert.equal(inBox(t, 'long_hair'), true);
  assert.equal(inBox(t, 'hair'), false);
  assert.equal(inBox(t, '1girl, smile'), false);
});

test('使用次數：12.3k、1.2M 這種寫法', () => {
  assert.equal(fmtCount(0), '');
  assert.equal(fmtCount(950), '950');
  assert.equal(fmtCount(1234), '1.2k');
  assert.equal(fmtCount(2000), '2k');
  assert.equal(fmtCount(12345), '12k');
  assert.equal(fmtCount(6008644), '6M');
  assert.equal(fmtCount(1250000), '1.3M');
});

test('Danbooru 分類詞庫：每條都在 tag 索引裡、一般類、照次數排、底線換空白括號跳脫', () => {
  const lib = JSON.parse(fs.readFileSync(path.join(__dirname, '../resources/tags/danbooru_groups/library.json'), 'utf8'));
  const csv = new Map();
  for (const line of fs.readFileSync(path.join(__dirname, '../resources/tags/danbooru.csv'), 'utf8').split(/\r?\n/)) {
    const q = /^"((?:[^"]|"")*)",(\d+),(\d+)/.exec(line);
    const [t, c, n] = q ? [q[1].replace(/""/g, '"'), q[2], q[3]] : line.split(',');
    if (t) csv.set(t, [+c, +n]);
  }
  const all = lib.groups.flatMap((g) => g.subs.flatMap((s) => s.tags));
  assert.ok(new Set(all.map((t) => t[0])).size > 10000, '要比 WeiLin 多很多');
  for (const [text, , n] of all.slice(0, 3000)) {
    // 顯示的寫法 → 索引的寫法：跳脫的括號還原、空白換底線（顏文字 \(^o^)/ 本來就帶反斜線，原樣找得到）
    const raw = text.replace(/\\([()])/g, '$1');
    const key = [text, raw, raw.replace(/ /g, '_')].find((k) => csv.has(k)) || raw;
    assert.ok(csv.has(key), text);
    assert.equal(csv.get(key)[0], 0, text);
    assert.equal(csv.get(key)[1], n, text);
  }
  assert.ok(!all.some(([t]) => /^[^()\\]+\([^()]+\)$/.test(t)), '括號都要跳脫');
  for (const g of lib.groups) for (const s of g.subs) {
    for (let i = 1; i < s.tags.length; i++) if (s.tags[i][3] === s.tags[i - 1][3]) assert.ok(s.tags[i][2] <= s.tags[i - 1][2], s.name);
  }
});

test('詞庫檔：括號已經跳脫、底線換成空白', () => {
  const lib = JSON.parse(fs.readFileSync(path.join(__dirname, '../resources/tags/weilin/library.json'), 'utf8'));
  const all = lib.groups.flatMap((g) => g.subs.flatMap((s) => s.tags.map((t) => t[0])));
  const single = all.filter((t) => !t.includes(','));
  assert.ok(single.length > 3000);
  assert.ok(!single.some((t) => /^[^()\\]+\([^()]+\)$/.test(t)), '單一 tag 的括號都要跳脫');
  assert.ok(single.includes('bead necklace'));
});

test('詞庫檔：沒有 A1111 的 <lora:…>、頭髮的「髮」沒被轉成「發」', () => {
  const lib = JSON.parse(fs.readFileSync(path.join(__dirname, '../resources/tags/weilin/library.json'), 'utf8'));
  const all = lib.groups.flatMap((g) => g.subs.flatMap((s) => s.tags));
  assert.ok(!all.some(([t]) => /<(lora|lyco):/i.test(t)));
  assert.ok(all.some(([t]) => t === 'hanfu, tang style outfits'));
  const bad = all.filter(([t, zh]) => /hair/i.test(t) && zh.includes('發'));
  assert.deepEqual(bad, []);
});

test('角色翻譯：校對過的換成正確的名字，AI 編錯的不留', () => {
  const extra = new Map(JSON.parse(fs.readFileSync(path.join(__dirname, '../resources/tags/weilin/zh_extra.json'), 'utf8')).map(([zh, t]) => [t, zh]));
  assert.match(extra.get('furina_(genshin_impact)') || '', /^芙寧娜/);
  assert.match(extra.get('kita_ikuyo') || '', /^喜多郁代/);
  assert.match(extra.get('firefly_(honkai:_star_rail)') || '', /^流螢/);
  assert.match(extra.get('gotoh_hitori') || '', /^後藤一里/);
  assert.ok(!/北一輝|東方/.test(extra.get('kita_ikuyo') || ''));
});

// node --test --test-isolation=none tests/taglib_sheet.test.cjs
// 固定舞台 A 案（2026-11-22 使用者：「這個詞庫收起來」）：生成頁的詞庫改成從提示詞卡上緣往上彈出的浮層，卡片不變高。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/taglib.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '').replace(/import\.meta\.url/g, '"file:///x/ui/js/taglib.js"');
  const context = vm.createContext({ console, store: { get: (_k, d) => d, set() {} }, el: () => null, toast() {} });
  vm.runInContext(source + '\nglobalThis.subject = { sheetRect };', context, { filename: 'taglib.js' });
  return { sheetRect: (...a) => JSON.parse(JSON.stringify(context.subject.sheetRect(...a))) };
}

test('sheetRect：貼在提示詞卡上緣、寬度一樣', () => {
  const { sheetRect } = load();
  assert.deepEqual(sheetRect({ left: 100, top: 600, width: 800 }, 900), { left: 100, width: 800, bottom: 306, maxHeight: 538 });
});

test('sheetRect：卡片太靠上時高度至少 160', () => {
  const { sheetRect } = load();
  assert.equal(sheetRect({ left: 0, top: 120, width: 500 }, 700).maxHeight, 160);
});

test('生成頁用浮層模式、浮層啟動時不自動打開', () => {
  const gen = fs.readFileSync(path.join(__dirname, '../ui/js/gen.js'), 'utf8');
  assert.match(gen, /taglib\.init\(\{[^\n]*sheet: \{ anchor: F\("promptBox"\) \}/);
  const src = fs.readFileSync(path.join(__dirname, '../ui/js/taglib.js'), 'utf8');
  assert.match(src, /if \(!sheet && store\.get\(prefix \+ "_open", false\)\) open\(true\)/);
});

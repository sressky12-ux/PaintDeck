// Ctrl+K 找功能（2026-11-23，生成頁左右兩欄那一次一起做）：純函式 score、rank
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/finder.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, el: () => null, $: () => null, $$: () => [] });
  vm.runInContext(source + '\nglobalThis.subject = { score, rank };', context, { filename: 'finder.js' });
  return context.subject;
}

test('score：完全相符 > 開頭相符 > 包含 > 沒有', () => {
  const { score } = load();
  assert.ok(score('反推', '反推') > score('反推提示詞', '反推'));
  assert.ok(score('反推提示詞', '反推') > score('從這張圖反推', '反推'));
  assert.ok(score('從這張圖反推', '反推') > 0);
  assert.equal(score('放大', '反推'), 0);
});

test('score：不分大小寫、也比對「在哪裡」', () => {
  const { score } = load();
  assert.ok(score('LoRA 卡匣', 'lora') > 0);
  assert.ok(score('畫遮罩', '輸入', '模型與輸入') > 0);
  assert.ok(score('畫遮罩', '輸入', '模型與輸入') < score('輸入圖', '輸入', '模型與輸入'));
});

test('rank：空字串照原順序；有字的照分數，同分照原順序；沒中的拿掉', () => {
  const { rank } = load();
  const items = [{ label: '放大' }, { label: '反推提示詞' }, { label: 'LoRA 卡匣' }, { label: '反推' }, { label: '從這張圖反推' }];
  assert.deepEqual(rank(items, '').map((x) => x.label), ['放大', '反推提示詞', 'LoRA 卡匣', '反推', '從這張圖反推']);
  assert.deepEqual(rank(items, '反推').map((x) => x.label), ['反推', '反推提示詞', '從這張圖反推']);
  assert.deepEqual(rank(items, 'LORA').map((x) => x.label), ['LoRA 卡匣']);
});

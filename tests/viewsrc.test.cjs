const test = require('node:test');
const assert = require('node:assert/strict');
const { nextView, resultLabel } = require('../ui/js/viewsrc.js');

const base = { mode: 'img2img', hasInput: true, current: 'input', fromCurrentInput: true, masking: false };
const check = (changes, view, hint = false) => assert.deepEqual(nextView({ ...base, ...changes }), { view, hint });

test('六種單張輸入模式，還沒出圖時先看輸入圖', () => {
  for (const mode of ['img2img', 'inpaint', 'outpaint', 'upscale', 'strip', 'sheet']) {
    check({ mode, event: 'mode', current: 'result', hasResult: false }, 'input');
    check({ mode, event: 'mode', current: 'result', hasInput: false }, 'input');
  }
});
test('換輸入圖回輸入畫面', () => check({ event: 'input', current: 'result' }, 'input'));
test('目前輸入圖的新成品自動顯示', () => check({ event: 'result' }, 'result'));
test('畫遮罩時亮點並留在目前畫面', () => check({ event: 'result', masking: true }, 'input', true));
test('換圖後才完成的成品不搶輸入圖或已選結果', () => {
  for (const current of ['input', 'result']) check({ event: 'result', current, fromCurrentInput: false }, current, true);
});
test('手動切換照使用者選擇並清除提示', () => {
  for (const view of ['input', 'result']) check({ event: 'select-' + view, masking: true }, view);
});
test('文生圖與多圖參考維持結果畫面', () => {
  for (const mode of ['txt2img', 'multiref']) check({ mode, event: 'input' }, 'result');
});
test('只有本批成品顯示編號', () => {
  assert.equal(resultLabel(3), '結果 #3');
  assert.equal(resultLabel(null), '結果');
});

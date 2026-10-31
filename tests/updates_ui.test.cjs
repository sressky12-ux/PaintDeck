const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
function load(name, names, extra = {}) {
  const src = fs.readFileSync(path.join(__dirname, '../ui/js', name), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const ctx = vm.createContext(extra);
  vm.runInContext(src + '\nglobalThis.subject = {' + names.join(',') + '};', ctx);
  return ctx.subject;
}
test('日期版號保留 V261024，同日不同包仍可從完整版本區別', () => {
  const {displayVersion} = load('updates.js', ['displayVersion']);
  assert.equal(displayVersion('26.10.24'), 'V261024');
  assert.equal(displayVersion('26.1024.1'), 'V261024');
  assert.equal(displayVersion('27.105.2'), 'V270105');
  assert.equal(displayVersion('1.1.3'), 'V1.1');
});
test('私人版、忙碌、檢查或下載中都不能按安裝', () => {
  const {canInstall} = load('updates.js', ['canInstall']);
  const ready = {enabled:true, channel:'public', update:{version:'26.1024.2'}, phase:'available'};
  assert.equal(canInstall(ready), true);
  for (const change of [{channel:'private'}, {enabled:false}, {busy_reason:'busy'}, {update:null},
    {phase:'downloading'}, {phase:'checking'}, {phase:'installing'}])
    assert.equal(canInstall({...ready, ...change}), false);
});
test('缺改寫模型可直接前往補下載通用 9B，不會自動抓數 GB 模型', () => {
  const {peModelFix} = load('pe.js', ['peModelFix'], {el:(tag, attrs, text) => ({tag, attrs, text})});
  const fix = peModelFix('找不到 AI 改寫用的通用模型（Qwen3.5-9B-The-Defiant-Fable*.gguf 或 Qwen3.6-35B-A3B*.gguf，放在 H:\\AnimaStudio\\ComfyUI\\models\\LLM）');
  assert.equal(fix.tag, 'a');
  assert.equal(fix.attrs.href, '/setup#models');
  assert.equal(peModelFix('找不到其他模型'), null);
  // 2026-10-30 起官方 PE 拿掉了，舊訊息不再出現，也不再給按鈕
  assert.equal(peModelFix('找不到 pe_t2i_heretic-*.gguf（放在 H:\\LLM）'), null);
});
test('Release 說明當純文字；忙碌原因與私人版狀態反映到按鈕', () => {
  const nodes = new Map();
  const $ = (id) => {
    if (!nodes.has(id)) nodes.set(id, {set innerHTML(_) {throw Error('Remote HTML must not be rendered');}});
    return nodes.get(id);
  };
  const {paint} = load('updates.js', ['paint'], {$});
  const available = {current_version:'26.1024.1', enabled:true, auto_check:true, channel:'public',
    update:{version:'26.1024.2', notes:'<img src=x onerror=alert(1)>'}, phase:'available', busy_reason:'busy'};
  paint(available);
  assert.equal($('#updateInstall').disabled, true);
  assert.equal($('#updateNotes').textContent, available.update.notes);
  assert.equal($('#updateNotice').hidden, false);
  paint({...available, busy_reason:null});
  assert.equal($('#updateInstall').disabled, false);
  paint({...available, enabled:false, channel:'private'});
  assert.equal($('#updateInstall').disabled, true);
  assert.equal($('#updateNotice').hidden, true);
});

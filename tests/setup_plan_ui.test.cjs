const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname, '../ui/js/setup.js'), 'utf8');
const gib = 2 ** 30;

// 只載入第三頁需要的程式碼：packDone 到「4. 安裝」之前（F、$$、DOM 換成假的）
function planHarness() {
  const nodes = new Map();
  const F = id => { if (!nodes.has(id)) nodes.set(id, {innerHTML:'', value:'', onclick:null}); return nodes.get(id); };
  const context = vm.createContext({
    F, $$: () => [],
    gb: n => `${(n / gib).toFixed(1)} GB`,
    esc: s => String(s ?? '').replace(/[&<>"]/g, c => `&#${c.charCodeAt(0)};`),
    isLinux: () => false,
    // 第三頁的「掃描範圍」（只掃指定的資料夾）載入時會讀記住的設定
    store: {get: (k, d) => d, set() {}}, addScanDir: list => list,
    scan: null, chk: null, pick: {packs: new Set()},
  });
  const begin = source.indexOf('const packDone =');
  const end = source.indexOf('// ---------------------------------------------------------------- 4. 安裝');
  assert.ok(begin > -1 && end > begin, 'setup.js 的第三頁區塊找不到');
  vm.runInContext(source.slice(begin, end), context);
  return context;
}

const pack = (id, label, bytes = 0) => ({id, label, requires:null, missing:{missing:bytes ? 1 : 0, bytes}, tool:true});
function fixture() {
  return {
    variant:'nvidia', rocm10:{need:false, bytes:0},
    tagger:true, llama:true, vcredist:true, studio_tools:true, workflows_missing:0,
    gpu:{}, data:{path:'D:\\data', free:100 * gib},
    comfy:{path:'D:\\comfy', ok:true, exists:true, free:100 * gib, download_size:2 * gib, moss_problem:''},
    packs:[pack('anima','動漫',10 * gib), pack('moss','MOSS',0), pack('studio','影音',5 * gib)],
  };
}

test('勾了未齊的包進 install、已齊的在 skip、沒勾的在 notInstalled', () => {
  const c = planHarness();
  const chk = fixture();
  const plan = c.buildPlan(chk, new Set(['anima']));
  assert.deepEqual(plan.install.map(p => p.id), ['anima']);
  assert.equal(plan.install[0].bytes, 10 * gib);
  assert.deepEqual(plan.skip.map(p => p.id), ['moss']);
  assert.deepEqual(plan.notInstalled.map(p => p.id), ['studio']);
});

test('moss_problem 只在勾了 moss 時才進 blockers', () => {
  const c = planHarness();
  const chk = fixture();
  chk.comfy.moss_problem = 'MOSS 需要新版 ComfyUI';
  chk.packs[1] = pack('moss', 'MOSS', 1 * gib);
  assert.equal(c.buildPlan(chk, new Set(['moss'])).blockers.includes('MOSS 需要新版 ComfyUI'), true);
  assert.equal(c.buildPlan(chk, new Set(['anima'])).blockers.includes('MOSS 需要新版 ComfyUI'), false);
});

test('diskPlan 的 total 等於 dataNeed 加 comfyNeed，同碟合併成一項檢查', () => {
  const c = planHarness();
  const chk = fixture();
  const plan = c.diskPlan(chk, [chk.packs[0]]);
  assert.equal(plan.total, plan.dataNeed + plan.comfyNeed);
  assert.equal(plan.checks.length, 1);
  assert.equal(plan.checks[0][0], '資料與 ComfyUI');
  assert.equal(plan.checks[0][1], plan.total);
});

test('ComfyUI 不存在且不能沿用時列進 extras 與 blockers', () => {
  const c = planHarness();
  const chk = fixture();
  chk.comfy.ok = false;
  chk.comfy.exists = true;
  chk.comfy.problem = '這個資料夾沒有 ComfyUI';
  chk.data.selection_problem = '資料夾不能寫入';
  assert.equal(c.buildPlan(chk, new Set()).blockers.includes('資料夾不能寫入'), true);
  assert.equal(c.buildPlan(chk, new Set()).blockers.includes('這個資料夾沒有 ComfyUI'), true);
  chk.comfy.exists = false;
  const plan = c.buildPlan(chk, new Set());
  assert.equal(plan.blockers.includes('這個資料夾沒有 ComfyUI'), false);
  assert.equal(plan.extras.some(s => /^下載 ComfyUI（約 /.test(s)), true);
});

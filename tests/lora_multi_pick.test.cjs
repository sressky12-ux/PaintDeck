// node --test --test-isolation=none tests/lora_multi_pick.test.cjs
// 固定舞台 A 案（2026-11-22 使用者：「選擇lora，可以一次多選擇lora，再加進去」「lora名字的字比較多會 ...」）：
// 生成頁、漫畫台的 LoRA 挑選視窗可以一次勾好幾個；卡匣顯示全名＋檔名，不切掉。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const src = (f) => fs.readFileSync(path.join(__dirname, '../ui/js', f), 'utf8');

function load() {
  const source = src('loras.js').replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, store: { get: (_k, d) => d, set() {} }, app: {}, $: () => null, el: () => null, api() {}, toast() {},
    openModal() {}, closeModal() {}, debounce: (f) => f, pickFile() {}, makeDrop() {} });
  vm.runInContext(source + '\nglobalThis.subject = { loraNames, pickAddLabel, turboHint };', context, { filename: 'loras.js' });
  return { loraNames: (...a) => JSON.parse(JSON.stringify(context.subject.loraNames(...a))), pickAddLabel: context.subject.pickAddLabel, turboHint: context.subject.turboHint };
}

test('loraNames：有 title 用 title，檔名不同才另外給', () => {
  const { loraNames } = load();
  assert.deepEqual(loraNames('真實細節增強', 'real_detail.safetensors'), { name: '真實細節增強', file: 'real_detail.safetensors' });
  assert.deepEqual(loraNames('', 'detail_slider.safetensors'), { name: 'detail_slider', file: null });
  assert.deepEqual(loraNames(undefined, 'a.safetensors'), { name: 'a', file: null });
  assert.deepEqual(loraNames('xiaokechr_e9', 'xiaokechr\\xiaokechr_e9.safetensors'), { name: 'xiaokechr_e9', file: 'xiaokechr\\xiaokechr_e9.safetensors' });
});

test('pickAddLabel', () => {
  assert.equal(load().pickAddLabel(3), '加入 3 個');
  assert.equal(load().pickAddLabel(0), '加入 0 個');
});

test('多選只開在生成頁、漫畫台；助手、資產庫維持單選', () => {
  assert.match(src('gen.js'), /openPicker\([^\n]*multi: true, have: new Set\(S\.loras/);
  assert.match(src('comic.js'), /openPicker\([^\n]*multi: true, have: new Set\(\(P\?\.loras/);
  for (const f of ['agent.js', 'assets.js']) assert.doesNotMatch(src(f), /multi: true/);
});

test('多選：已經在清單裡的不能勾、按加入時依序呼叫 cb、單選時底部列藏起來', () => {
  const s = src('loras.js');
  assert.match(s, /const had = !!pickOpts\.multi && !!pickOpts\.have\?\.has\(l\.name\)/);
  assert.match(s, /if \(had\) return;/);
  assert.match(s, /for \(const l of chosen\) cb\(l, trig\)/);
  assert.match(s, /\$\("#lpFoot"\)\.hidden = !opts\.multi/);
});

test('卡匣兩行卡：生成頁、漫畫台都用 .two，名字不切 ...', () => {
  assert.match(src('gen.js'), /class: "lora-item two"/);
  assert.match(src('comic.js'), /class: "lora-item cm-lora two"/);
  const css = fs.readFileSync(path.join(__dirname, '../ui/css/app.css'), 'utf8');
  assert.match(css, /\.lora-item\.two \.nm \{ white-space: normal; overflow: visible; text-overflow: clip;/);
});

// 2026-11-24 玩家：「LoRA 選擇器搜 Turbo 找不到加速 LoRA」→ 搜到加速相關的字就告訴他去哪裡開（加速 LoRA 照舊不放進一般清單）
test('turboHint：Anima 或沒指定家族、搜 turbo／加速／speed 才提示', () => {
  const { turboHint } = load();
  assert.match(turboHint('anima', 'Turbo'), /^加速 LoRA 不在這裡：生成頁在「加速」選單/);
  assert.ok(turboHint(null, '加速'));
  assert.ok(turboHint(null, 'SPEED up'));
  assert.ok(turboHint(() => true, 'turbo'), '工作流組（判斷函式）也提示');
  assert.equal(turboHint('qwen21', 'turbo'), '');
  assert.equal(turboHint('anima', 'detail'), '');
  assert.equal(turboHint('anima', ''), '');
});

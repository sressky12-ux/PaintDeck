// node --test --test-isolation=none tests/turbo.test.cjs
// Anima 加速 LoRA（ui/js/turbo.js）：生成頁「加速」選單和生圖助手的生圖設定共用（2026-11-24 玩家：「助手不能用加速 LoRA 嗎」）
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/turbo.js'), 'utf8').replace(/^export /gm, '');
  const context = vm.createContext({ Option: class { constructor(text, value) { Object.assign(this, { text, value }); } } });
  vm.runInContext(source + '\nglobalThis.subject = { turboLabel, turboSteps, defTurbo, fillTurbo, TURBO_OFF, speedState, speedToggle };', context, { filename: 'turbo.js' });
  const s = context.subject;
  return { ...s, turboSteps: (n) => ({ ...s.turboSteps(n) }) };
}

const LIST = [{ name: 'Anima\\TurboAnima-V4.safetensors', title: 'TURBO for ANIMA (less steps)', version: 'V4' }, { name: 'anima-turbo-lora-v0.2.safetensors' }];

test('missing turbo stays selected until cleared', () => {
  const sel = { replaceChildren(...opts) { this.options = opts; } };
  const { fillTurbo } = load();
  fillTurbo(sel, [], 'gone.safetensors');
  assert.equal(sel.value, 'gone.safetensors');
  assert.equal(sel.options.find(o => o.value === sel.value).text, '（找不到：gone.safetensors）');
  fillTurbo(sel, LIST.slice(0, 1), true);
  assert.equal(sel.value, 'anima-turbo-lora-v0.2.safetensors');
  assert.match(sel.options.find(o => o.value === sel.value).text, /找不到/);
  fillTurbo(sel, [], '');
  assert.deepEqual(sel.options.map(o => o.value), ['']);
});

test('turboLabel：官方寫 10 步，其他用標題＋版本、12 步', () => {
  const { turboLabel } = load();
  assert.equal(turboLabel(LIST[1]), '官方 Turbo LoRA v0.2（10 步）');
  assert.equal(turboLabel(LIST[0]), 'TURBO for ANIMA (less steps) V4（12 步）');
  assert.equal(turboLabel({ name: 'x_turbo.safetensors' }), 'x_turbo.safetensors（12 步）');
});

test('turboSteps：官方 10 步、其他 12 步、CFG 1；不使用 30 步 CFG 4（跟後端 api_agent_tools::with_turbo 一樣）', () => {
  const { turboSteps } = load();
  assert.deepEqual(turboSteps('anima-turbo-lora-v0.2.safetensors'), { steps: 10, cfg: 1 });
  assert.deepEqual(turboSteps('Anima\\TurboAnima-V4.safetensors'), { steps: 12, cfg: 1 });
  assert.deepEqual(turboSteps(''), { steps: 30, cfg: 4 });
  const rs = fs.readFileSync(path.join(__dirname, '../src-tauri/src/server/api_agent_tools.rs'), 'utf8');
  assert.match(rs, /contains\("anima-turbo-lora"\) \{ 10 \} else \{ 12 \}/);
  assert.match(rs, /None => \(json!\(false\), 30, 4\.0\)/);
});

test('defTurbo：官方優先，不然第一顆，沒有就空白', () => {
  const { defTurbo } = load();
  assert.equal(defTurbo(LIST), 'anima-turbo-lora-v0.2.safetensors');
  assert.equal(defTurbo(LIST.slice(0, 1)), 'Anima\\TurboAnima-V4.safetensors');
  assert.equal(defTurbo([]), '');
  assert.equal(defTurbo(undefined), '');
});

test('生成頁和助手頁都用 turbo.js，不再各寫一份', () => {
  const gen = fs.readFileSync(path.join(__dirname, '../ui/js/gen.js'), 'utf8');
  const agent = fs.readFileSync(path.join(__dirname, '../ui/js/agent.js'), 'utf8');
  assert.doesNotMatch(gen, /官方 Turbo LoRA v0\.2/);
  assert.match(gen, /from "\.\/turbo\.js"/);
  assert.match(agent, /from "\.\/turbo\.js"/);
  assert.match(agent, /setBase\(\{ turbo_lora: e\.target\.value \|\| false, \.\.\.speedup\.turboSteps\(e\.target\.value\) \}\)/);
});

// 2026-11-24 使用者：「助手哪裡能用加速 LoRA 不明顯啊，直接弄個加速 LoRA 開關不行嗎」→ 助手底下膠囊列的「⚡ 加速」開關
test('加速開關：Anima（非 turbo 模型）和 MOSS 才顯示，看得出開著沒', () => {
  const { speedState } = load();
  const st = (b) => ({ ...speedState(b) });
  assert.deepEqual(st({ family: 'anima', model: 'anima-base.safetensors', turbo_lora: 'x.safetensors' }), { kind: 'anima', on: true });
  assert.deepEqual(st({ family: 'anima', model: 'anima-base.safetensors', turbo_lora: false }), { kind: 'anima', on: false });
  assert.deepEqual(st({ family: 'anima', model: 'anima-turbo-v1.1.safetensors' }), { kind: '', on: false });
  assert.deepEqual(st({ family: 'moss', model: 'moss_image_sft.safetensors', loras: [{ name: 'moss\\moss_speed_dmd_lodestones.safetensors', strength: 0.35 }] }), { kind: 'moss', on: true });
  assert.deepEqual(st({ family: 'moss', model: 'moss_image_sft.safetensors', loras: [] }), { kind: 'moss', on: false });
  assert.deepEqual(st({ family: 'qwen21', model: 'q.safetensors' }), { kind: '', on: false });
  assert.deepEqual(st(null), { kind: '', on: false });
});

test('加速開關：Anima 開＝掛加速 LoRA（上次用的優先、再來官方）＋步數 CFG，關＝30 步 CFG 4', () => {
  const { speedToggle } = load();
  const t = (b, o) => JSON.parse(JSON.stringify(speedToggle(b, o)));
  const off = { family: 'anima', model: 'anima-base.safetensors', turbo_lora: false };
  assert.deepEqual(t(off, { turbos: LIST }), { turbo_lora: 'anima-turbo-lora-v0.2.safetensors', steps: 10, cfg: 1 });
  assert.deepEqual(t(off, { turbos: LIST, last: LIST[0].name }), { turbo_lora: LIST[0].name, steps: 12, cfg: 1 });
  assert.deepEqual(t(off, { turbos: LIST, last: 'gone.safetensors' }), { turbo_lora: 'anima-turbo-lora-v0.2.safetensors', steps: 10, cfg: 1 });
  assert.deepEqual(t({ ...off, turbo_lora: LIST[0].name }, { turbos: LIST }), { turbo_lora: false, steps: 30, cfg: 4 });
  assert.match(t(off, { turbos: [] }).error, /加速 LoRA/);
});

test('加速開關：MOSS 開＝掛 DMD 加速 LoRA 0.35、10 步，關＝拿掉、25 步，其他 LoRA 不動', () => {
  const { speedToggle } = load();
  const t = (b, o) => JSON.parse(JSON.stringify(speedToggle(b, o)));
  const dmd = 'moss\\moss_speed_dmd_lodestones.safetensors';
  const mine = { name: 'moss\\xiaokechr_moss.safetensors', strength: 0.8 };
  const off = { family: 'moss', model: 'moss_image_sft.safetensors', steps: 25, loras: [mine] };
  assert.deepEqual(t(off, { loras: [{ name: dmd }, { name: mine.name }] }), { loras: [mine, { name: dmd, strength: 0.35 }], steps: 10 });
  assert.deepEqual(t({ ...off, steps: 10, loras: [mine, { name: dmd, strength: 0.35 }] }, {}), { loras: [mine], steps: 25 });
  assert.match(t(off, { loras: [{ name: mine.name }] }).error, /MOSS 快速/);
  assert.ok(t({ family: 'qwen21', model: 'q' }, {}).error);
});

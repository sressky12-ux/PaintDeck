// node --test tests/model_defaults.test.cjs
// 生成頁切換模型時的預設採樣器與排程（ui/js/gen.js 的 modelDefaults／pick／fam）
// 切到 MOSS 會把排程改為 sgm_uniform，再切回 Anima／Qwen 2.1 時必須切回各自家族預設的 simple
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const SRC = fs.readFileSync(path.join(__dirname, '../ui/js/gen.js'), 'utf8');

function grab(head) {
  const i = SRC.indexOf(head);
  assert.ok(i >= 0, `gen.js 找不到 ${head}`);
  let depth = 0;
  for (let k = SRC.indexOf('{', i); k < SRC.length; k++) {
    if (SRC[k] === '{') depth++;
    else if (SRC[k] === '}' && --depth === 0) return SRC.slice(i, k + 1).replace(/^export /, '');
  }
  throw new Error(`${head} 大括號沒配對`);
}

function line(head) {
  const i = SRC.indexOf(head);
  assert.ok(i >= 0, `gen.js 找不到 ${head}`);
  return SRC.slice(i, SRC.indexOf('\n', i));
}

function load(initial = {}) {
  const mkSel = (opts, value) => ({ options: opts.map((v) => ({ value: v })), value });
  const fields = {
    model: mkSel(['anima-turbo.safetensors', 'anima-base.safetensors', 'moss-sft.safetensors', 'qwen-anime.safetensors', 'qwen_image_2.1_turbo.safetensors', 'krea2-turbo.safetensors', 'sdxl-noob.safetensors', 'sdxl-lightning.safetensors', 'wf:custom1'], initial.model || 'anima-turbo.safetensors'),
    steps: { value: initial.steps ?? 8 },
    cfg: { value: initial.cfg ?? 1 },
    sampler: mkSel(['euler', 'euler_ancestral', 'er_sde', 'dpmpp_2m'], initial.sampler || 'euler'),
    scheduler: mkSel(['simple', 'normal', 'karras', 'sgm_uniform', 'beta'], initial.scheduler || 'simple'),
    turboLora: mkSel(['', 'anima-turbo-lora-v0.2.safetensors', 'other-turbo.safetensors'], initial.turboLora || ''),
  };
  const S = { loras: initial.loras || [] };
  const app = {
    state: {
      models: {
        unet_family: {
          'anima-turbo.safetensors': 'anima',
          'anima-base.safetensors': 'anima',
          'moss-sft.safetensors': 'moss',
          'qwen-anime.safetensors': 'qwen21',
          'qwen_image_2.1_turbo.safetensors': 'qwen21',
          'krea2-turbo.safetensors': 'krea2',
          'sdxl-noob.safetensors': 'sdxl',
          'sdxl-lightning.safetensors': 'sdxl',
        },
      },
    },
  };
  const code = [
    line('const fam ='),
    line('const isFastSdxl ='),
    'const MOSS_FAST = { is: (n) => /moss_speed_dmd/i.test(n), steps: 10, normal: 25 };',
    'const MOSS_DEBAND = { is: (n) => /moss_image_sft_banding_fix/i.test(n) };',
    line('const fastOn ='),
    grab('function isTurboModel('),
    line('const OFFICIAL_TURBO ='),
    grab('function isOfficialTurbo('),
    grab('function pick('),
    grab('function modelDefaults('),
  ].join('\n');
  const context = vm.createContext({
    F: (id) => fields[id],
    S,
    app,
    renderLoras() {},
    modelHint() {},
  });
  vm.runInContext(code + '\nglobalThis.subject = { modelDefaults, fam };', context, { filename: 'gen.js' });
  const switchTo = (model, extra = {}) => {
    fields.model.value = model;
    if (extra.turboLora !== undefined) fields.turboLora.value = extra.turboLora;
    context.subject.modelDefaults();
    return {
      family: context.subject.fam(),
      steps: +fields.steps.value,
      cfg: +fields.cfg.value,
      sampler: fields.sampler.value,
      scheduler: fields.scheduler.value,
      loras: S.loras,
    };
  };
  return { fields, S, switchTo };
}

test('從 MOSS 切回 Anima Turbo／Anima 加速 LoRA／Anima 非加速版時，排程必須從 sgm_uniform 切回 simple', () => {
  const { switchTo } = load();
  assert.deepEqual(
    (() => { const r = switchTo('moss-sft.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [25, 3, 'euler', 'sgm_uniform'],
  );
  assert.deepEqual(
    (() => { const r = switchTo('anima-turbo.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [8, 1, 'euler', 'simple'],
  );

  switchTo('moss-sft.safetensors');
  assert.deepEqual(
    (() => { const r = switchTo('anima-base.safetensors', { turboLora: 'anima-turbo-lora-v0.2.safetensors' }); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [10, 1, 'euler', 'simple'],
  );

  switchTo('moss-sft.safetensors');
  assert.deepEqual(
    (() => { const r = switchTo('anima-base.safetensors', { turboLora: '' }); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [30, 4, 'er_sde', 'simple'],
  );
});

test('從 MOSS 切回 Qwen 2.1（一般版與 Turbo 版）時，排程必須從 sgm_uniform 切回 simple', () => {
  const { switchTo } = load({ model: 'moss-sft.safetensors', sampler: 'euler', scheduler: 'sgm_uniform' });
  assert.deepEqual(
    (() => { const r = switchTo('qwen-anime.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [25, 3, 'euler', 'simple'],
  );

  switchTo('moss-sft.safetensors');
  assert.deepEqual(
    (() => { const r = switchTo('qwen_image_2.1_turbo.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [8, 1, 'euler', 'simple'],
  );
});

test('從 SDXL（euler_ancestral＋normal）切到 Anima 或 Qwen 2.1 時，採樣器與排程也一併切回家族預設', () => {
  const { switchTo } = load();
  assert.deepEqual(
    (() => { const r = switchTo('sdxl-noob.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [28, 5, 'euler_ancestral', 'normal'],
  );
  assert.deepEqual(
    (() => { const r = switchTo('qwen-anime.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [25, 3, 'euler', 'simple'],
  );
  switchTo('sdxl-lightning.safetensors');
  assert.deepEqual(
    (() => { const r = switchTo('anima-turbo.safetensors'); return [r.steps, r.cfg, r.sampler, r.scheduler]; })(),
    [8, 1, 'euler', 'simple'],
  );
});

test('自訂工作流（wf:）不覆蓋排程與採樣器，離開 MOSS 時會清除 MOSS 專用 LoRA', () => {
  const { switchTo } = load({
    model: 'moss-sft.safetensors',
    sampler: 'euler',
    scheduler: 'sgm_uniform',
    loras: [{ name: 'loras/moss/moss_speed_dmd_lodestones.safetensors', enabled: true }],
  });
  const wf = switchTo('wf:custom1');
  assert.equal(wf.sampler, 'euler');
  assert.equal(wf.scheduler, 'sgm_uniform');
  assert.deepEqual(wf.loras, []);
});

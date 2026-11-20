// node --test --test-isolation=none tests/train_ui.test.cjs
// 訓練頁（ui/js/train.js）不靠畫面的那幾個判斷：哪一步能不能進、進度事件怎麼併、花錢的地方沒確認不能送
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const SRC = fs.readFileSync(path.join(__dirname, '../ui/js/train.js'), 'utf8');
const UTIL = fs.readFileSync(path.join(__dirname, '../ui/js/util.js'), 'utf8').replace(/^export /gm, '');

function grab(head) {
  const i = SRC.indexOf(head);
  assert.ok(i >= 0, `train.js 找不到 ${head}`);
  let depth = 0;
  for (let k = SRC.indexOf('{', i); k < SRC.length; k++) {
    if (SRC[k] === '{') depth++;
    else if (SRC[k] === '}' && --depth === 0) return SRC.slice(i, k + 1).replace(/^export /, '');
  }
  throw new Error(`${head} 大括號沒配對`);
}

const ctx = {};
vm.createContext(ctx);
vm.runInContext(
  ['export function liveFiles(', 'export function blockReason(', 'export function needsConfirm(', 'export function startBody(', 'export function applyEvent(', 'export function percent(', 'export function stateName(']
    .map(grab).join('\n') + '\nthis.T = { liveFiles, blockReason, needsConfirm, startBody, applyEvent, percent, stateName };',
  ctx,
);
const T = ctx.T;

const view = (images, captions, run = null) => ({ project: { id: 't0123456789ab', images, run }, captions });

test('沒有專案只能待在第一步', () => {
  assert.equal(T.blockReason(1, null), '');
  for (const s of [2, 3, 4, 5, 6]) assert.match(T.blockReason(s, null), /專案/);
});

test('沒圖不能到描述；勾掉的不算圖', () => {
  assert.equal(T.blockReason(2, view([], {})), '');
  assert.match(T.blockReason(3, view([], {})), /訓練圖/);
  assert.match(T.blockReason(3, view([{ file: 'a.png', off: true }], {})), /訓練圖/);
  assert.equal(T.blockReason(3, view([{ file: 'a.png' }], {})), '');
});

test('沒描述不能到參數和在哪煉，講得出還差幾張', () => {
  const v = view([{ file: 'a.png' }, { file: 'b.png' }, { file: 'c.png', off: true }], { 'a.png': 'x, 1girl', 'b.png': '', 'c.png': '' });
  assert.deepEqual([...T.liveFiles(v)], ['a.png', 'b.png']);
  assert.match(T.blockReason(4, v), /1 張/);
  assert.match(T.blockReason(5, v), /1 張/);
  v.captions['b.png'] = 'x, 1boy';
  assert.equal(T.blockReason(4, v), '');
  assert.equal(T.blockReason(5, v), '');
});

test('還沒開始訓練不能到驗收', () => {
  const caps = { 'a.png': 'x' };
  assert.match(T.blockReason(6, view([{ file: 'a.png' }], caps)), /還沒/);
  assert.equal(T.blockReason(6, view([{ file: 'a.png' }], caps, { state: 'running' })), '');
});

test('花錢的地方沒確認不送', () => {
  assert.equal(T.needsConfirm('local'), false);
  assert.equal(T.needsConfirm('remote'), false);
  for (const t of ['runpod', 'civitai', 'modelscope']) {
    assert.equal(T.needsConfirm(t), true);
    assert.equal(T.startBody(t, { gpu: 'a40', max_cost: 500 }, false), null);
    assert.equal(T.startBody(t, { gpu: 'a40', max_cost: 500 }, true).confirm, true);
  }
  const b = T.startBody('local', { model: 'x.safetensors', junk: undefined }, false);
  assert.equal(b.target, 'local');
  assert.equal(b.model, 'x.safetensors');
  assert.equal('confirm' in b, false);
  assert.equal('junk' in b, false);
});

function localStep(vram, confirm) {
  const elements = [], requests = [];
  const api = vm.runInNewContext(UTIL + '\napi;', {
    document: { addEventListener() {} }, AbortController, setTimeout, clearTimeout,
    fetch(path, opts) {
      requests.push({ path, method: opts.method, body: JSON.parse(opts.body) });
      return { ok: true, json: async () => null };
    },
  });
  const page = {
    S: { target: 'local', vram, trainer: { installed: true } },
    cur: () => ({ id: 't0123456789ab', family: 'anima' }),
    fam: () => ({ label: 'Anima', min_vram_gb: 10 }),
    busyKind: () => '', TARGETS: [], box: () => {}, note: () => {}, trainerBox: () => {},
    el(tag, attrs, ...kids) {
      const node = { tag, attrs, kids };
      elements.push(node);
      return node;
    },
    confirm, api,
  };
  vm.runInNewContext(
    ['export function needsConfirm(', 'export function startBody(', 'async function call(', 'async function startTrain(', 'function step5(']
      .map(grab).join('\n') + '\nstep5();', page,
  );
  const button = elements.find((n) => n.tag === 'button' && n.kids.includes('開始訓練'));
  const warning = elements.find((n) => n.attrs.class === 'tr-warn');
  return { button: button.attrs, warning: warning?.kids[0], requests };
}

test('顯存足夠或在原有 0.5GB 容許範圍，直接開始且不送 force', async () => {
  for (const vram of [10, 9.5]) {
    const { button, warning, requests } = localStep(vram, () => assert.fail('不應要求確認'));
    assert.equal(button.disabled, false);
    assert.equal(warning, undefined);
    await button.onclick();
    assert.deepEqual(requests, [{ path: '/api/train/t0123456789ab/start', method: 'POST', body: { target: 'local' } }]);
  }
});

test('顯存少 2GB 仍可按，取消不送，確認才送 force', async () => {
  let accept = false;
  const prompts = [];
  const { button, warning, requests } = localStep(8, (msg) => { prompts.push(msg); return accept; });
  assert.equal(button.disabled, false);
  assert.match(warning, /8GB.*10GB.*Anima 1550 步.*4～5 小時.*顯卡會被占滿/);
  await button.onclick();
  assert.equal(requests.length, 0);
  accept = true;
  await button.onclick();
  assert.equal(prompts.length, 2);
  for (const msg of prompts) assert.ok(msg.includes(warning));
  assert.deepEqual(requests, [{ path: '/api/train/t0123456789ab/start', method: 'POST', body: { target: 'local', force: true } }]);
});

test('顯存少超過 2GB 會停用本機開始按鈕', () => {
  const { button, warning } = localStep(7.9);
  assert.equal(button.disabled, true);
  assert.match(warning, /改用雲端顯卡或 C 站/);
});

test('進度事件只更新有帶的欄位', () => {
  let r = T.applyEvent({ state: 'starting', step: 0, total: 0, log: [] }, { state: 'running', step: 120, total: 1250, loss: 0.123, log: ['a', 'b'] });
  assert.deepEqual({ ...r }, { state: 'running', step: 120, total: 1250, loss: 0.123, log: ['a', 'b'], msg: '' });
  r = T.applyEvent(r, { state: 'running', msg: '上傳 6 筆訓練資料…' });
  assert.equal(r.step, 120);
  assert.equal(r.msg, '上傳 6 筆訓練資料…');
  r = T.applyEvent(r, { state: 'error', msg: '顯存不夠' });
  assert.equal(r.state, 'error');
  assert.equal(r.msg, '顯存不夠');
  assert.equal(T.percent({ step: 125, total: 1250 }), 10);
  assert.equal(T.percent({ step: 5, total: 0 }), 0);
  assert.equal(T.percent({ step: 9999, total: 100 }), 100);
  assert.equal(T.percent(null), 0);
});

test('狀態有中文名字，沒看過的原樣顯示', () => {
  assert.equal(T.stateName('running'), '訓練中');
  assert.equal(T.stateName('interrupted'), '中斷了');
  assert.equal(T.stateName('detached'), '還在外面跑');
  assert.equal(T.stateName(''), '還沒開始');
  assert.equal(T.stateName('weird'), 'weird');
});

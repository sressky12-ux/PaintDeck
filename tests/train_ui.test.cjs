// node --test --test-isolation=none tests/train_ui.test.cjs
// 訓練頁（ui/js/train.js）不靠畫面的那幾個判斷：哪一步能不能進、進度事件怎麼併、花錢的地方沒確認不能送
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const SRC = fs.readFileSync(path.join(__dirname, '../ui/js/train.js'), 'utf8');
const UTIL = fs.readFileSync(path.join(__dirname, '../ui/js/util.js'), 'utf8').replace(/^export /gm, '');

function grab(head) { return grabFrom(SRC, head); }

function grabFrom(source, head) {
  const i = source.indexOf(head);
  assert.ok(i >= 0, `train.js 找不到 ${head}`);
  let depth = 0;
  for (let k = source.indexOf('{', i); k < source.length; k++) {
    if (source[k] === '{') depth++;
    else if (source[k] === '}' && --depth === 0) return source.slice(i, k + 1).replace(/^export /, '');
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

function kohakuFn(name, file = 'trainkohaku.js') {
  const src = fs.readFileSync(path.join(__dirname, '../ui/js', file), 'utf8');
  return vm.runInNewContext(grabFrom(src, 'export function ' + name + '(') + '\n' + name);
}

test('安裝六階段保留斷點並標示進行中的階段', () => {
  const rows = kohakuFn('kohakuStageRows')({ busy: true, stage: 'torch', stages_done: ['uv','source','venv'] });
  assert.equal(rows.length, 6);
  assert.equal(rows[2].state, 'done');
  assert.equal(rows[3].state, 'running');
  assert.equal(rows[4].state, 'todo');
});

test('健檢黃燈可煉，紅燈與尚未取得健檢時不能開始', () => {
  const lights = kohakuFn('checkLights');
  assert.equal(lights({rows:[{state:'warn'}]}).canStart, true);
  assert.equal(lights({rows:[{state:'bad'}]}).canStart, false);
  assert.equal(lights(null).canStart, false);
});

test('三段進度分別呈現，訓練尚未出第一步時提醒編譯', () => {
  const rows = kohakuFn('phaseRows')({phase:'train',phase_index:3,step:0,state:'running'});
  assert.deepEqual(Array.from(rows, r => r.label), ['整理描述','整理圖片','訓練']);
  assert.equal(rows[0].state, 'done');
  assert.match(rows[2].text, /第一次要編譯顯卡程式/);
});

test('剩餘時間取當次速度優先，沒有資料就留白', () => {
  const eta = kohakuFn('etaText');
  assert.match(eta({step:400,total:800},5.8), /3[89] 分鐘/);
  assert.match(eta({step:400,total:800,sec_per_step:3},5.8), /20 分鐘/);
  assert.equal(eta({step:0,total:800}), '');
});

test('只有本機訓練才確認出圖', () => {
  const ask = kohakuFn('confirmGenWhileTraining', 'gen.js');
  assert.ok(ask({target:'local',state:'running'}));
  assert.equal(ask({target:'runpod',state:'running'}), '');
  assert.equal(ask({target:'local',state:'done'}), '');
  assert.equal(ask(null), '');
});

test('五種健檢修正先確認，網址與下載中心走各自的入口', async () => {
  const src = fs.readFileSync(path.join(__dirname,'../ui/js/trainkohaku.js'),'utf8');
  let accepted = false;
  const calls = [], questions = [], urls = [], pages = [];
  const repair = vm.runInNewContext(grabFrom(src,'async function repair(') + '\nrepair', {
    installConfirm:'安裝引擎', confirm:q => (questions.push(q),accepted),
    post:async (path,body) => { calls.push([path,body]); return {ok:false,url:'https://example.invalid/tools',id:1}; },
    api:async () => ({downloads:[{id:1,status:'done'}]}),
    window:{open:url => urls.push(url)}, app:{showPage:page => pages.push(page)},
  });
  for (const action of ['install_engine','install_buildtools','download_heal','open_url','open_hub']) {
    const fix = {action, url:'https://example.invalid/driver',confirm:'確認修正'};
    accepted = false;
    const before = calls.length + urls.length + pages.length;
    await repair(fix, () => {});
    assert.equal(calls.length + urls.length + pages.length,before);
    accepted = true;
    await repair(fix, () => {});
  }
  assert.equal(questions.length,10);
  assert.deepEqual(calls.map(([path]) => path),['/api/train/kohaku/install','/api/train/kohaku/buildtools','/api/train/kohaku/download_heal']);
  assert.ok(calls[1][1].confirm && calls[2][1].confirm);
  assert.deepEqual(urls,['https://example.invalid/tools','https://example.invalid/driver']);
  assert.deepEqual(pages,['civitai']);
});

test('缺少 heal 時優先用現有 Base，保留可下載的 heal 選項', () => {
  const choose = kohakuFn('defaultBase');
  const data = {bases:[{file:'heal',tested:true,missing:true},{file:'MOSS-Base'}],params:{base:'heal'}};
  assert.equal(choose(data),'MOSS-Base');
  data.bases[0].missing = false;
  assert.equal(choose(data),'heal');
});

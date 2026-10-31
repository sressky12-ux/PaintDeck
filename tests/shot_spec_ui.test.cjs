// node --test --test-isolation=none tests/shot_spec_ui.test.cjs
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const plain = (v) => JSON.parse(JSON.stringify(v));

function element(tag, attrs = {}, ...children) {
  return Object.assign({ tag, value: '', checked: false, children, dataset: {}, style: {},
    append(...kids) { this.children.push(...kids); }, replaceChildren(...kids) { this.children = kids; },
    addEventListener() {}, querySelector() { return null; }, querySelectorAll() { return []; },
    classList: { add() {}, remove() {}, toggle() {} }, focus() {}, setAttribute() {},
  }, attrs);
}
function subject({ api: handler, confirm = true, image = { model: 'test-image-model', family: 'anima' } } = {}) {
  const calls = [], opened = [], video = [], saved = {}, asks = [], errors = [];
  const api = async (url, opts = {}) => {
    calls.push([url, plain(opts.json || {})]);
    if (handler) return handler(url, opts);
    if (url === '/api/shotsheet/compose') return { segments: opts.json.sheet.segments.map((_, i) => ({ prompt: `shot ${i}`, mirror: `mirror ${i}` })), estimate: [] };
    if (url === '/api/shotsheet/check') return { issues: [] };
    if (url === '/api/director/from_sheet') return { id: 'new-project' };
    throw new Error(url);
  };
  const context = vm.createContext({ structuredClone, console, Event, setTimeout, clearTimeout, api, el: element,
    toast: (m, kind) => { if (kind === 'err') errors.push(m); },
    store: { get: (k, d) => saved[k] || d, set: (k, v) => { saved[k] = plain(v); } }, debounce: () => () => {},
    confirm: (message) => { asks.push(message); return confirm; },
    studio: { openVideo: (tab) => video.push(tab), openTab: (tab) => opened.push(`tab:${tab}`) }, assets: { list: async () => [] },
    gen: { baseParams: () => image },
    director: { openProject: async (id) => opened.push(id) },
    h3: { applySheet: async (s) => video.push(plain(s)) }, h3chain: { applySheet: async (s) => { video.push(plain(s)); return true; } },
    openModal() {}, closeModal() {}, pickFile() {}, uploadFile() {}, pickFromGallery() {},
    document: { getElementById: () => null, addEventListener() {} },
  });
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/shotsheet.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '').replace('await import("./director.js")', 'globalThis.director').replace('await import("./gen.js")', 'globalThis.gen');
  vm.runInContext(source + '\nglobalThis.subject = {load,current,stateBox:continuityStateEl,setEngine,effectiveEntry,setSegCount,removeCast,applyTo,region,requiresFirstFrame,setWorkflows:(v)=>{workflows=v},setAsk:(f)=>{ask=f}};', context);
  return { s: context.subject, calls, opened, video, saved, asks, errors };
}

test('legacy sheets default to H3 without invented continuity facts', () => {
  const { s } = subject();
  s.load({ segments: [{}, {}] });
  assert.equal(s.current().engine, 'h3');
  assert.equal(s.current().segments[0].inherit_entry, true);
  assert.deepEqual(plain(s.effectiveEntry(1)), { actors: [], light: '', camera: '', notes: '' });
  assert.deepEqual(plain(s.region(1)), [1.6, 10]);
});

test('entry inheritance respects explicit override and off switch without mutating previous shot', () => {
  const { s } = subject();
  const end = { actors: [{ who: 'S1', holding: 'cup', position: 'left' }], light: 'window left' };
  s.load({ segments: [{ exit: end }, {}] });
  assert.equal(s.effectiveEntry(1).actors[0].holding, 'cup');
  s.current().segments[1].entry.notes = 'user specified new pose';
  assert.equal(s.effectiveEntry(1).notes, 'user specified new pose');
  assert.equal(s.effectiveEntry(1).actors.length, 0);
  s.current().segments[1].entry.notes = '';
  s.current().segments[1].inherit_entry = false;
  assert.equal(s.effectiveEntry(1).actors.length, 0);
  assert.equal(s.current().segments[0].exit.actors[0].holding, 'cup');
});

test('switch engine rebuilds protocol locally, preserves state, and protects manual text', () => {
  const m = subject({ confirm: false });
  m.s.load({ segments: [{ manual: true, prompt: 'custom user words', exit: { notes: 'cup held' } }] });
  assert.equal(m.s.setEngine('ltx23'), false);
  assert.equal(m.s.current().engine, 'h3');
  assert.equal(m.s.current().segments[0].prompt, 'custom user words');
  const m2 = subject();
  m2.s.load({ segments: [{ manual: true, prompt: 'H3 words', exit: { notes: 'cup held' } }, {}] });
  assert.equal(m2.s.setEngine('my-workflow'), true);
  assert.equal(m2.s.current().segments[0].prompt, '');
  assert.equal(m2.s.current().segments[0].exit.notes, 'cup held');
  assert.deepEqual(plain(m2.s.region(1)), [0, 10]);
  assert.equal(m2.calls.length, 0, 'switching engines never changes server/global settings');
});

test('continuity-only segment requires confirmation before removal', () => {
  const m = subject({ confirm: false });
  m.s.load({ segments: [{}, { exit: { actors: [{ who: 'S1', wardrobe: 'blue coat' }] } }] });
  assert.equal(m.s.setSegCount(1), false);
  assert.equal(m.s.current().segments.length, 2);
});

test('removing cast updates state speaker ids but preserves user-written names', () => {
  const { s } = subject();
  s.load({ cast: [{ name: 'A' }, { name: 'B' }], segments: [{
    entry: { actors: [{ who: 'S2', holding: 'cup' }, { who: 'Guest', gaze: 'right' }] },
    exit: { actors: [{ who: 'S1', pose: 'standing' }] },
  }] });
  s.removeCast(0);
  assert.equal(s.current().segments[0].entry.actors[0].who, 'S1');
  assert.equal(s.current().segments[0].entry.actors[1].who, 'Guest');
  assert.equal(s.current().segments[0].exit.actors[0].who, '');
});

test('LTX/custom applies complete sheet to director and opens new project without starting generation', async () => {
  const m = subject();
  m.s.load({ engine: 'custom-workflow', first: { file: 'first.png' }, refs: { on: true, items: [{ file: 'ref.png', who: 'S1' }] },
    segments: [{ secs: 8, exit: { notes: 'cup in right hand' }, beats: [{ from: 0, to: 3, action: 'raise cup' }] }] });
  await m.s.applyTo();
  assert.deepEqual(m.opened, ['tab:plan', 'new-project']);
  assert.deepEqual(m.video, []);
  const [url, body] = m.calls.find(([url]) => url === '/api/director/from_sheet');
  assert.equal(url, '/api/director/from_sheet');
  assert.equal(body.engine, 'custom-workflow');
  assert.equal(body.sheet.segments[0].exit.notes, 'cup in right hand');
  assert.equal(body.sheet.refs.items[0].who, 'S1');
  assert.deepEqual(body.prompts, ['shot 0']);
  assert.equal(body.image.model, 'test-image-model');
  assert.ok(m.calls.every(([url]) => !/generate|settings|\/run/.test(url)));
  assert.deepEqual(m.errors, []);
});

test('cancelled check does not create director project', async () => {
  const m = subject({ api: async (url) => {
    if (url === '/api/shotsheet/compose') return { segments: [{ prompt: 'p', mirror: 'm' }], estimate: [] };
    if (url === '/api/shotsheet/check') return { issues: [{ seg: 0, level: 'warn', msg: 'timing' }] };
    throw new Error('must not create project');
  } });
  m.s.setAsk(async () => false);
  m.s.load({ engine: 'ltx23', segments: [{}] });
  await m.s.applyTo();
  assert.equal(m.calls.some(([url]) => url === '/api/director/from_sheet'), false);
});

test('H3 retains its short/long application path and continuity states', async () => {
  const m = subject();
  m.s.load({ segments: [{ exit: { light: 'left' } }, {}] });
  await m.s.applyTo();
  const applied = m.video.find((x) => typeof x === 'object');
  assert.equal(applied.sheet.engine, 'h3');
  assert.equal(applied.sheet.segments[0].exit.light, 'left');
  assert.equal(m.calls.some(([url]) => url === '/api/director/from_sheet'), false);
});

test('text-to-video workflow does not require a still image or image model', async () => {
  const m = subject({ image: {} });
  m.s.setWorkflows([{ id: 'text-video', info: { image: null }, media_slots: [] }]);
  m.s.load({ engine: 'text-video', segments: [{ beats: [{ from: 0, to: 3, action: 'camera pans across a room' }] }] });
  assert.equal(m.s.requiresFirstFrame(), false);
  await m.s.applyTo();
  const [, body] = m.calls.find(([url]) => url === '/api/director/from_sheet');
  assert.equal(body.engine, 'text-video');
  assert.equal(body.image, undefined);
  assert.deepEqual(m.errors, []);
});

test('first-frame engines require a frame or valid still-image model before creating project', async () => {
  const m = subject({ image: {} });
  m.s.setWorkflows([{ id: 'mapped', info: { image: null }, media_slots: [{ kind: 'image', role: 'first' }] }]);
  assert.equal(m.s.requiresFirstFrame('mapped'), true);
  m.s.load({ engine: 'mapped', segments: [{}] });
  await m.s.applyTo();
  assert.equal(m.calls.some(([url]) => url === '/api/director/from_sheet'), false);
  assert.match(m.errors[0], /首幀/);
  // 2026-10-30 LTX 退場：舊拍攝單的 ltx23 讀成 H3
  m.s.load({ engine: 'ltx23', segments: [{}] });
  assert.equal(m.s.current().engine, 'h3');
});


test('adding a continuity actor keeps the state editor expanded', () => {
  const {s}=subject();s.load({segments:[{}]});
  const walk=x=>[x,...(x?.children||[]).flat(Infinity).flatMap(c=>c&&typeof c==='object'?walk(c):[])];
  const box=s.stateBox(0,'exit','結束狀態');
  const add=walk(box).find(x=>x?.tag==='button'&&x.children.includes('＋ 角色狀態'));
  add.onclick();
  assert.equal(s.stateBox(0,'exit','結束狀態').open,true);
  assert.equal(s.current().segments[0].exit.actors.length,1);
});

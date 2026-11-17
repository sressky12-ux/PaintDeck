// node --test --test-isolation=none tests/wardrobe.test.cjs
// 詞庫頁的隨機產生器（ui/js/wardrobe.js）：固定欄位怎麼變成權重、送給 Wardrobe /roll 的內容
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/wardrobe.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '')
    .replace(/import\.meta\.url/g, '"file:///ui/js/wardrobe.js"').replace(/\bimport\(/g, '(async () => ({}))(');
  const context = vm.createContext({ console, URL, store: { get: (k, d) => d, set() {} }, toast() {}, el: () => ({}), api: async () => ({}) });
  vm.runInContext(source + '\nglobalThis.subject = { weightsWithPins, rollBody, sectionEligible, fixHair };', context, { filename: 'wardrobe.js' });
  return context.subject;
}
const { weightsWithPins, rollBody, sectionEligible, fixHair } = load();

test('段要不要顯示：看段裡有沒有 ≤ 分級的選項，不看最高級；純文字段恆顯示', () => {
  const count = { max_level: 4, fields: [{ opts: [{ level: 0 }, { level: 4 }] }] };
  const nsfw = { fields: [{ opts: [{ level: 2 }, { level: 3 }] }] };
  const text = { fields: [{ type: 'text', opts: [] }] };
  assert.equal(sectionEligible(count, 0), true);
  assert.equal(sectionEligible(nsfw, 0), false);
  assert.equal(sectionEligible(nsfw, 2), true);
  assert.equal(sectionEligible(nsfw, 99), true);
  assert.equal(sectionEligible(text, 0), true);
});

test('簡轉繁後頭髮的髮改回來，出發、發光不動', () => {
  assert.equal(fixHair('棕發、粉發、茶色發、發帶、發型'), '棕髮、粉髮、茶色髮、髮帶、髮型');
  assert.equal(fixHair('出發、發光、發情'), '出發、發光、發情');
});
const plain = (x) => JSON.parse(JSON.stringify(x));

const pools = { sections: [{ id: 'hair', fields: [{ key: 'hair_color', opts: [{ en: 'black hair' }, { en: 'Blonde Hair' }, { en: 'red hair' }] }] }] };

test('固定一欄：那欄必出、選中的選項 3、其他 0（key 小寫，跟引擎 parse_weights 一樣）', () => {
  const w = plain(weightsWithPins({ sections: { hair: 2 }, fields: {}, options: {} }, { hair_color: 'Blonde Hair' }, pools));
  assert.deepEqual(w.sections, { hair: 2 });
  assert.equal(w.fields.hair_color, 3);
  assert.deepEqual(w.options.hair_color, { 'black hair': 0, 'blonde hair': 3, 'red hair': 0 });
});

test('沒固定、或固定到不存在的欄位：權重表原樣、不會改到原本的物件', () => {
  const orig = { sections: {}, fields: { x: 0.5 }, options: { y: { a: 2 } } };
  const w = plain(weightsWithPins(orig, { nope: 'z', hair_color: '' }, pools));
  assert.deepEqual(w, { sections: {}, fields: { x: 0.5 }, options: { y: { a: 2 } } });
  weightsWithPins(orig, { hair_color: 'red hair' }, pools);
  assert.deepEqual(plain(orig), { sections: {}, fields: { x: 0.5 }, options: { y: { a: 2 } } });
});

const base = { level: 'explicit', seed: '', tag_cap: '自动', base_prob: '', breast_size: '不指定', outfit_style: '随机',
  appearance_follow: true, hair_follow: false, duo_role_fill: true, role_pick: true, role_fav_only: false, role_works: '', scene_pick: true, extra_body: '' };

test('送給 /roll：停用的段、點選的角色、勾選池、種子空白就用現在的亂數', () => {
  const b = plain(rollBody(base, { weights: { sections: {} }, roles: ['a_(x)', 'b'], off: new Set(['nsfw_act']), pool: ['sc_1', 'sc_2'],
    allSections: ['count', 'hair', 'nsfw_act'], seedNow: 42 }));
  assert.equal(b.section, 'all');
  assert.equal(b.seed, 42);
  assert.equal(b.nsfw_level, 'explicit');
  assert.equal(b.disabled_sections, 'nsfw_act');
  assert.equal(b.selected_roles, 'a_(x)|b');
  assert.equal(b.pick_pool_ids, 'sc_1,sc_2');
  assert.equal(b.pick_pool_mode, 'pose');
  assert.equal(b.hair_follow, false);
  assert.equal(b.scene_pick, true);
  assert.equal('base_prob' in b, false, '空白＝讓 Wardrobe 用它的預設');
  assert.equal(typeof b.weights_json, 'string');
});

test('填了種子就用那個數字、基本機率轉數字；勾選池空的時候不開', () => {
  const b = plain(rollBody({ ...base, seed: ' 123 ', base_prob: '0.4' }, { weights: {}, roles: [], off: new Set(), pool: [], allSections: ['a'], seedNow: 9 }));
  assert.equal(b.seed, 123);
  assert.equal(b.base_prob, 0.4);
  assert.equal(b.pick_pool_mode, '');
  assert.equal(b.disabled_sections, '');
  const bad = plain(rollBody({ ...base, seed: 'abc' }, { weights: {}, roles: [], off: new Set(), pool: [], allSections: [], seedNow: 7 }));
  assert.equal(bad.seed, 7);
});

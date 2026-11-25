const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const read = file => fs.readFileSync(path.join(__dirname, '../ui', file), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
const light = 'html.style-a';
const system = 'html.style-a:not([data-theme="light"])';
const dark = 'html.style-a[data-theme="dark"]';
const media = '@media (prefers-color-scheme: dark)';
const colors = 'case panel well hi lo key key-side key-down ink ink-2 ink-off screen ph-info ph-num ph-err go danger ok warn err focus field field-line stripe-1 stripe-2 stripe-3 stripe-4'.split(' ');
const sizes = 'xs sm md lg xl 2xl'.split(' ').map(size => 'fz-' + size);
const names = [...colors, ...sizes, ...'r-sm r-md r-lg r-round sh-key sh-down sh-well sh-float sp-1 sp-2 sp-3 sp-4 sp-5 sp-6'.split(' ')];
const scopedColors = /#[0-9a-f]{3,8}\b|\b(?:rgb|hsl)a?\(/i;

// 保留條件規則的父層，同時拆開群組選擇器（:is() 內的逗號留著）。
function rules(css = read('css/style-a.css')) {
  const stack = [], result = [];
  for (const [, raw, brace] of css.matchAll(/([^{}]*)([{}])/g)) {
    const text = raw.trim();
    if (brace === '{') { stack.push(text); continue; }
    const selector = stack.pop();
    assert.ok(selector);
    if (!selector.startsWith('@')) result.push({ selector, parent: stack.join(' '), body: text });
  }
  assert.equal(stack.length, 0);
  return result;
}
function selectors(rule) { return rule.selector.split(/,(?![^()]*\))/).map(s => s.trim()); }
function vars(body) {
  return Object.fromEntries([...body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map(m => [m[1], m[2].trim()]));
}
const variables = s => vars(rules().find(r => r.selector === s).body);
const componentRules = () => rules().filter(r => ![light, system, dark].includes(r.selector));

test('每條選擇器都受 html.style-a 限制', () => {
  for (const rule of rules()) for (const s of selectors(rule)) assert.match(s, /^html\.style-a(?=[\s:[]|$)/);
});
test('淺色變數涵蓋指定的 47 個名字', () => {
  const actual = Object.keys(variables(light)).filter(k => k.startsWith('--a-'));
  assert.deepEqual(actual.sort(), names.map(k => '--a-' + k).sort());
});
test('系統深色與明選深色都定義同一組 27 色，明選淺色不套系統深色', () => {
  const all = rules();
  assert.equal(all.find(r => r.selector === system)?.parent, media);
  assert.equal(all.find(r => r.selector === dark)?.parent, '');
  const defaults = variables(light), explicit = variables(dark);
  assert.deepEqual(Object.keys(explicit).sort(), colors.map(k => '--a-' + k).sort());
  assert.deepEqual(variables(system), explicit);
  assert.equal(defaults['--a-go'], explicit['--a-go']);
});
test('六級字級符合現行縮放表', () => {
  const values = variables(light);
  for (const size of sizes) assert.match(values['--a-' + size], /^(10\.5|11|11\.5|12|12\.5|13|13\.5|14|15|16|18|19)px$/);
});
test('保留第一個 :root 的每個舊變數，深色舊宣告不能蓋掉 A 的配色別名', () => {
  const base = rules(read('css/app.css'));
  const old = vars(base.find(r => r.selector === ':root').body);
  const overrides = vars(base.find(r => r.selector === ':root[data-theme="dark"]').body);
  const aliases = variables(light);
  for (const key of Object.keys(old)) {
    assert.ok(aliases[key], key);
    if (!key.startsWith('--f-')) assert.match(aliases[key], /var\(--a-/);
    if (key in overrides) assert.match(aliases[key], /!important$/, key);
  }
  assert.equal(aliases['--f-vfd'], 'var(--f-ui)');
});
test('A 是正式頁的預設：<html> 直接帶 style-a，不再看網址開關', () => {
  const html = read('index.html');
  assert.match(html.match(/<html\b[^>]*>/)[0], /class="style-a"/);
  assert.doesNotMatch(html, /URLSearchParams\(location\.search\)\.get\("style"\)/);
  assert.match(html, /href="\/static\/css\/motion\.css">\s*<link rel="stylesheet" href="\/static\/css\/style-a\.css">/);
});
test('色碼只出現在三個變數區塊', () => {
  for (const r of componentRules()) assert.doesNotMatch(r.body, scopedColors);
});

test('元件的圓角、陰影與字級使用指定級數', () => {
  const components = componentRules();
  assert.ok(components.length > 0);
  const formats = { 'border-radius': /^(var\(--a-r-(?:sm|md|lg|round)\)\s*)+$/,
    'box-shadow': /^(none|var\(--a-sh-(?:key|down|well|float)\))$/,
    'font-size': /^var\(--a-fz-(?:xs|sm|md|lg|xl|2xl)\)$/ };
  for (const { selector, body } of components) {
    for (const [, key, value] of body.matchAll(/([\w-]+)\s*:\s*([^;]+);/g)) {
      if (formats[key]) assert.match(value.trim(), formats[key], selector + ' ' + key);
    }
  }
});
test('A1 所有模式鍵預留上緣，只有選中的燈亮起', () => {
  const body = selector => componentRules().find(r => selectors(r).includes('html.style-a .piano ' + selector))?.body;
  const key = body('button') ?? '';
  assert.ok(Number(key.match(/padding(?:-top)?:\s*(\d+)px/)?.[1]) >= 11);
  assert.match(body('button::before') ?? '', /background:\s*transparent/);
  assert.match(body('button.on::before') ?? '', /background:\s*var\(--a-ph-info\)/);
  assert.match(body('button.on') ?? '', /box-shadow:\s*var\(--a-sh-down\)/);
});
test('正式頁拍攝單語言鍵、影片長短切換都為燈留出至少 11px', () => {
  const app = rules(read('css/app.css'));
  for (const selector of ['.sh-head .piano button', '.vhead #videoSub button']) {
    const body = app.find(r => r.selector === selector).body;
    assert.ok(Number(body.match(/padding(?:-block)?:\s*(\d+)px/)?.[1]) >= 11, selector);
  }
});
test('銘牌可用標牌字型，元件標籤使用系統字型', () => {
  const branded = componentRules().filter(r => r.body.includes('var(--f-label)'));
  assert.ok(branded.length > 0);
  for (const rule of branded) for (const s of selectors(rule)) assert.match(s, /\.badge\b/);
  assert.ok(componentRules().some(r => selectors(r).includes('html.style-a .lbl') && /var\(--f-ui\)/.test(r.body)));
});


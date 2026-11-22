// 中央舞台第三～五期：共用元件（專案膠囊、段落目錄）與各頁結構
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
const mainJs = fs.readFileSync(path.join(__dirname, '../ui/js/main.js'), 'utf8');

function load(file, names) {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/', file), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, store: { get: (_k, d) => d, set() {} }, el: () => null, $: () => null, $$: () => [] });
  vm.runInContext(source + `\nglobalThis.subject = { ${names.join(', ')} };`, context, { filename: file });
  return Object.fromEntries(Object.entries(context.subject).map(([k, f]) => [k, (...a) => JSON.parse(JSON.stringify(f(...a)) ?? 'null')]));
}

test('pillText：去空白、空的回 —、太長截斷', () => {
  const { pillText } = load('pagehead.js', ['pillText']);
  assert.equal(pillText('  第一部片 '), '第一部片');
  assert.equal(pillText(''), '—');
  assert.equal(pillText(undefined), '—');
  assert.equal(pillText('一二三四五六七八九十', 6), '一二三四五…');
});
test('groupSections：照 order 排組、組內維持原順序、沒列的組放最後', () => {
  const { groupSections } = load('formtoc.js', ['groupSections']);
  const items = [{ group: 'net', id: 'a' }, { group: 'general', id: 'b' }, { group: 'odd', id: 'c' }, { group: 'net', id: 'd' }];
  assert.deepEqual(groupSections(items, ['general', 'net']), [
    { group: 'general', items: [{ group: 'general', id: 'b' }] },
    { group: 'net', items: [{ group: 'net', id: 'a' }, { group: 'net', id: 'd' }] },
    { group: 'odd', items: [{ group: 'odd', id: 'c' }] },
  ]);
});

// ---- 第五期：設定頁每段都有組別
test('設定頁：每個段落都有 data-group，而且是五組之一', () => {
  const a = html.indexOf('<main id="page-settings"');
  const b = html.indexOf('</main>', a);
  const page = html.slice(a, b);
  const boxes = page.match(/<section class="box"[^>]*>/g);
  assert.ok(boxes.length >= 20);
  for (const tag of boxes) {
    const g = /data-group="([^"]+)"/.exec(tag);
    assert.ok(g, `沒有 data-group：${tag}`);
    assert.ok(['general', 'net', 'model', 'ai', 'system'].includes(g[1]), tag);
  }
});

// ---- 第三期：助手頁改用 dock
test('助手頁：main.js 用 agstage，膠囊與面板都在', () => {
  assert.ok(mainJs.includes('name: "agstage"'));
  for (const id of ['agbrain', 'agbase', 'agcloud', 'agstrip']) assert.equal(html.split(`data-dock="${id}"`).length - 1, 1, id);
  assert.ok(html.includes('id="agCapRow"'));
});

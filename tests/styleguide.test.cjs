const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const read = file => fs.readFileSync(path.join(__dirname, '../ui', file), 'utf8');
const sheet = () => read('styleguide.html');
const ids = 'primary-key akey minikey piano segmented readout text-input textarea select checkbox switch slider chip panel-head card dialog toast progress empty waiting icons prompt-paper prompt-screen'.split(' ');
const sections = () => [...sheet().matchAll(/<section class="sg-cell" data-sg="([\w-]+)">([\s\S]*?)<\/section>/g)];

test('樣張固定開 A，樣式順序與正式頁相同', () => {
  assert.match(sheet(), /<html lang="zh-Hant-TW" class="style-a">/);
  const links = html => [...html.matchAll(/<link\b[^>]+href="([^"]+)"[^>]*>/g)].map(m => m[1]);
  const expected = ['fonts', 'app', 'play', 'motion', 'style-a'].map(n => '/static/css/' + n + '.css');
  assert.deepEqual(links(sheet()), expected);
  assert.deepEqual(links(read('index.html')).filter(link => expected.includes(link)), expected);
});
test('樣張內的樣式限用 .sg- 選擇器，紙面用 A 的變數', () => {
  const css = sheet().match(/<style>([\s\S]+?)<\/style>/)[1];
  for (const [, group] of css.matchAll(/([^{}]+)\{/g)) {
    if (group.trim().startsWith('@media')) continue;
    for (const selector of group.split(',')) assert.match(selector.trim(), /^\.sg-/);
  }
  const paper = css.match(/\.sg-paper[^{]*\{([^}]+)\}/)[1];
  assert.match(paper, /var\(--a-field\)/);
  assert.doesNotMatch(paper, /#[0-9a-f]{3,8}\b|var\(--(?!a-)/i);
});
test('23 格各有一份正式元件作為六種狀態的來源', () => {
  const cells = sections();
  assert.deepEqual(cells.map(m => m[1]).sort(), [...ids].sort());
  const css = read('css/app.css') + read('css/style-a.css');
  for (const [, id, html] of cells) {
    assert.match(html, /data-sg-target\b/, id);
    const classes = [...html.matchAll(/class="([^"]+)"/g)].flatMap(m => m[1].split(/\s+/));
    assert.ok(classes.some(c => !c.startsWith('sg-') && css.includes('.' + c)), id);
  }
  assert.match(sheet(), /Object\.keys\(ICONS\)/);
  assert.match(sheet(), /Object\.entries\(GLYPH_ICON\)/);
});
test('樣張情境逐一強制三種虛擬狀態，焦點包含鍵盤焦點環', async () => {
  const { forceStyleguideStates } = await import('../tools/ui_audit/scenarios.mjs');
  const requests = [];
  const edge = { async send(method, params) {
    requests.push([method, params]);
    if (method === 'DOM.getDocument') return { root: { nodeId: 1 } };
    if (method === 'DOM.querySelectorAll') return { nodeIds: [2, 3] };
    return {};
  } };
  await forceStyleguideStates(edge);
  assert.deepEqual(requests.slice(0, 3).map(r => r[0]), ['DOM.enable', 'CSS.enable', 'DOM.getDocument']);
  const queries = requests.filter(r => r[0] === 'DOM.querySelectorAll').map(r => r[1].selector);
  assert.deepEqual(queries, ['hover', 'active', 'focus'].map(s => '[data-sg-state="' + s + '"]'));
  const calls = requests.filter(r => r[0] === 'CSS.forcePseudoState').map(r => r[1]);
  for (const state of ['hover', 'active', 'focus']) {
    const forced = calls.filter(p => p.forcedPseudoClasses.includes(state));
    assert.deepEqual(forced.map(p => p.nodeId), [2, 3]);
    if (state === 'focus') assert.ok(forced.every(p => p.forcedPseudoClasses.includes('focus-visible')));
  }
});

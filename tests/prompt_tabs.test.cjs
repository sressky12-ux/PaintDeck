const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { negIsCustom } = require('../ui/js/prompttabs.js');

test('負面詞小點只標示與目前預設不同的內容', () => {
  assert.equal(negIsCustom('a,  b', 'a, b'), false);
  assert.equal(negIsCustom(' a,\n b\t', 'a, b'), false);
  assert.equal(negIsCustom('a, b, c', 'a, b'), true);
  assert.equal(negIsCustom('', ''), false);
});

test('正負分頁位於同一塊螢幕，兩邊各有權重高亮與自動完成', () => {
  const html = fs.readFileSync(path.join(__dirname, '../ui/index.html'), 'utf8');
  const start = html.indexOf('<div class="scr prompt-scr"');
  const screen = html.slice(start, html.indexOf('<div class="taglib"', start)).trimEnd();
  assert.match(screen, /role="tablist"[^]*id="promptTabPos"[^]*id="promptTabNeg"/);
  assert.match(screen, /id="posPane"[^]*id="promptHl"[^]*id="acPrompt"/);
  assert.match(screen, /<div id="negDetails"[^>]*role="tabpanel"[^]*id="negHl"[^]*id="acNeg"/);
  assert.doesNotMatch(html, /<details[^>]*id="negDetails"/);
  const gen = fs.readFileSync(path.join(__dirname, '../ui/js/gen.js'), 'utf8');
  assert.match(gen, /F\("negDetails"\).hidden = mm/);
});

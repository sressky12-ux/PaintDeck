const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const ui = path.join(__dirname, '../ui');
const js = path.join(ui, 'js');
const source = fs.readFileSync(path.join(js, 'deck.js'), 'utf8');
const object = source.match(/export const ICONS = \{([\s\S]*?)^\};/m)[1];
const names = new Set(Array.from(object.matchAll(/^\s*(\w+):/gm), m => m[1]));
const glyphs = () => import('../ui/js/glyph_icons.js').then(m => m.GLYPH_ICON);
const groups = {
  starOn: '★', star: '☆', check: '✓', close: '✕✗', dot: '●', ring: '○', gear: '⚙',
  arrowL: '←', arrowR: '→➜', arrowU: '↑', arrowD: '↓', external: '↗', redo: '↻↷', undo: '↺↶',
  diamond: '◆', sparkle: '✦', warn: '⚠', caretD: '▾', caretU: '▴', caretR: '▸', caretL: '◀',
  play: '▶', stop: '■', bolt: '⚡', lock: '🔒', pencil: '✎', list: '☰', grid: '▦', swap: '⇄⇆↔',
  tip: '💡', die: '🎲', heart: '♡', note: '♪', thumb: '👍', clap: '🎬', folder: '📁',
};

test('對照表每個圖示名都在 ICONS 裡，別名符合設計表', async () => {
  const map = await glyphs();
  for (const name of Object.values(map)) assert.ok(names.has(name), name);
  for (const [name, symbols] of Object.entries(groups)) {
    for (const symbol of symbols) assert.equal(map[symbol], name, symbol);
  }
});

test('index.html 與 ui/js/*.js 的符號都有對照', async () => {
  const map = await glyphs();
  const files = fs.readdirSync(js).filter(f => f.endsWith('.js') && f !== 'glyph_icons.js').map(f => path.join(js, f));
  const missing = new Set();
  for (const file of [path.join(ui, 'index.html'), ...files]) {
    const source = fs.readFileSync(file, 'utf8');
    for (const [glyph] of source.matchAll(/[\u2190-\u21ff\u25a0-\u25ff\u2600-\u27bf\u{1f300}-\u{10ffff}]/gu)) {
      if (!Object.hasOwn(map, glyph)) missing.add(path.basename(file) + ' ' + glyph);
    }
  }
  assert.deepEqual([...missing], []);
});

test('新圖示只用線條圖元與指定填色 class，座標都在 24 格內', () => {
  const existing = new Set(['starOn', 'star', 'die', 'clap']);
  for (const name of Object.keys(groups).filter(n => !existing.has(n))) {
    const markup = object.match(new RegExp("^\\s*" + name + ": '([^']+)',?$", 'm'))?.[1];
    assert.ok(markup, name);
    const tags = [...markup.matchAll(/<(path|circle|rect|line|polyline|polygon)\s+([^<>]+?)\/>/g)];
    assert.equal(tags.map(t => t[0]).join(''), markup, name);
    for (const [, tag, attrs] of tags) {
      const matches = [...attrs.matchAll(/(\w+)="([^"]+)"/g)];
      assert.equal(matches.map(a => a[0]).join(' '), attrs, name);
      const allowed = { path: ['d'], circle: ['cx', 'cy', 'r'], rect: ['x', 'y', 'width', 'height', 'rx', 'ry'],
        line: ['x1', 'y1', 'x2', 'y2'], polygon: ['points'], polyline: ['points'] };
      const values = {};
      for (const [, key, value] of matches) {
        if (key === 'class') { assert.match(value, /^(f|a|af)$/); continue; }
        assert.ok(allowed[tag].includes(key), name + ' ' + key);
        // 新圖只用絕對座標，曲線的控制點也必須在框內。
        if (key === 'd') assert.match(value, /^[MLHVQCZ0-9.,\s]+$/);
        for (const n of value.match(/-?\d+(?:\.\d+)?/g) || []) assert.ok(+n >= 0 && +n <= 24, name);
        values[key] = Number(value);
      }
      if (tag === 'circle') {
        const { cx, cy, r } = values;
        assert.ok(cx - r >= 0 && cy - r >= 0 && cx + r <= 24 && cy + r <= 24, name);
      }
      if (tag === 'rect') assert.ok(values.x + values.width <= 24 && values.y + values.height <= 24, name);
    }
  }
});

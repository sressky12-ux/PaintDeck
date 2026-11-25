const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

test('偵測：壓字、切字、溢出，各抓對、不誤報', async (t) => {
  if (!fs.existsSync('C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe')) return t.skip('沒有 Edge');
  const source = fs.readFileSync(path.join(__dirname, '../tools/ui_audit/detect.js'), 'utf8');
  const { openEdge } = await import('../tools/ui_audit/cdp.mjs');
  const edge = await openEdge({ port: 0 });
  t.after(() => edge.close());
  const fixture = pathToFileURL(path.join(__dirname, 'fixtures/ui_audit_fixture.html')).href;
  await edge.navigate(fixture);
  await edge.evaluate(source);
  const defects = await edge.evaluate('__uiAuditDetect()');
  assert.deepEqual(defects.map(d => [d.selector, d.kind]), [
    ['button#a', 'overlap'], ['div#c', 'clip'], ['select#e', 'clip'], ['span#f', 'overflow'],
  ]);
  for (const d of defects) {
    assert.ok(d.text.length > 0 && [...d.text].length <= 20);
    assert.equal(d.rect.length, 4);
    assert.ok(d.rect.every(Number.isFinite));
  }
});

const { test } = require('node:test');
const assert = require('node:assert/strict');
const reportModule = () => import('../tools/ui_audit/report.mjs');
const defect = (selector, x = 0) => ({ kind: 'overlap', selector, text: '燈', rect: [x, 0, 12, 3] });
const shot = (defects, overrides = {}) => ({ scenario: 'director:shotsheet', theme: 'dark', width: 1920,
  file: 'director_shotsheet_dark_1920.png', defects, classes: { piano: 1 }, ...overrides });

test('newDefects 只回新的，比對不看 rect 和 text', async () => {
  const { defectKey, newDefects } = await reportModule();
  const a = defect('#a'), b = defect('#b');
  assert.equal(defectKey('s', 'dark', 1920, a), 's|dark|1920|overlap|#a');
  const prev = { shots: [shot([a])] };
  const current = { shots: [shot([{ ...defect('#a', 9), text: '別的字' }, b])] };
  assert.deepEqual(newDefects(prev, current), [{ scenario: 'director:shotsheet', theme: 'dark', width: 1920, ...b }]);
});

test('情境、主題、寬度或種類改變都算新的', async () => {
  const { newDefects } = await reportModule();
  const a = defect('#a');
  const shots = [{ scenario: 'page:gen' }, { theme: 'light' }, { width: 1100 },
    { defects: [{ ...a, kind: 'clip' }] }].map(change => shot([a], change));
  assert.equal(newDefects({ shots: [shot([a])] }, { shots }).length, 4);
});

test('Markdown 列出各張截圖、缺陷位置、文字與 class 次數', async () => {
  const { renderMarkdown } = await reportModule();
  const report = { meta: { base: 'http://127.0.0.1:8193/', style: '', lang: 'zh-TW', date: '2026-11-25' },
    shots: [shot([defect('#a')]), shot([], { width: 1100, file: 'narrow.png' })] };
  const md = renderMarkdown(report);
  for (const value of ['director:shotsheet', 'dark', '1920', '1100', 'director_shotsheet_dark_1920.png',
    'narrow.png', '#a', 'overlap', '燈', '12', 'piano']) assert.ok(md.includes(value), value);
});

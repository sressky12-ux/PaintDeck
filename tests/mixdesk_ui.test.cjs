// node --test --test-isolation=none tests/mixdesk_ui.test.cjs
// 混音台（私人版，設計 docs/superpowers/specs/2026-11-04-mixdesk-panel-design.md）：導演台分頁、iframe、放大／全螢幕的接線
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const read = (p) => fs.readFileSync(path.join(__dirname, '..', p), 'utf8');

test('tab_button_and_section_exist', () => {
  const html = read('ui/index.html');
  assert.match(html, /<button data-t="mixdesk"[^>]*>混音台<\/button>/);
  const sec = html.slice(html.indexOf('data-tab="mixdesk"'), html.indexOf('</section>', html.indexOf('data-tab="mixdesk"')));
  assert.ok(sec.length > 0, '沒有 data-tab="mixdesk" 的 section');
  assert.match(sec, /<iframe id="mdFrame"[^>]*allow="fullscreen"/);
  for (const id of ['mdFull', 'mdFs', 'mdBody', 'mdUnfull']) assert.match(sec, new RegExp(`id="${id}"`), id);
});

test('studio_js_wires_mixdesk', () => {
  const js = read('ui/js/studio.js');
  // 公開版匯出時預演台被切掉，"previz" 這格不在
  assert.match(js, /\["plan", "edit", "frames", "video", (?:"previz", )?"mixdesk"\]\.includes\(t\)/);
  assert.match(js, /if \(t === "mixdesk"\) mixdeskShow\(\);/);
  const fn = js.slice(js.indexOf('function mixdeskShow'), js.indexOf('\n}\n', js.indexOf('function mixdeskShow')));
  assert.match(fn, /fr\.src = "\/mixdesk"/);
  assert.match(fn, /requestFullscreen/);
});

test('mixdesk_html_loads_module', () => {
  const html = read('ui/mixdesk.html');
  assert.match(html, /<script type="module" src="\/static\/js\/mixdesk\/main\.js"><\/script>/);
  assert.match(html, /\/static\/css\/mixdesk\.css/);
});

// 2026-11-17 起公開版也帶混音台：匯出時 remove_previz.py 切預演台，刀要停在混音台前面，混音台的分頁、區塊、說明書都要留著
const os = require('node:os');
const { spawnSync } = require('node:child_process');
function findPython() {
  for (const p of [process.env.ANIMA_PY, 'C:/Users/User/AppData/Local/Python/bin/python.exe', 'py', 'python3', 'python'].filter(Boolean)) {
    const r = spawnSync(p, ['-I', '-c', 'print(1)'], { encoding: 'utf8' });
    if (r.status === 0 && r.stdout.trim() === '1') return p;
  }
  return null;
}
const TANGLED = ['src-tauri/src/lib.rs', 'src-tauri/src/server/mod.rs', 'src-tauri/tests/api_routes.rs', 'ui/index.html', 'ui/js/studio.js',
  'resources/extras/fablecut/anima-guide.md', 'resources/extras/fablecut/anima-tools.js', 'src-tauri/src/setup.rs', 'src-tauri/src/brains.rs', 'ui/manual.html'];

test('public_export_keeps_mixdesk_drops_previz', (t) => {
  const script = path.join(__dirname, '..', 'tools', 'public', 'remove_previz.py');
  if (!fs.existsSync(script)) return t.skip('公開版沒有 tools/public（匯出腳本只在私人版）');
  const py = findPython();
  if (!py) return t.skip('找不到 Python');
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mixdesk_public_'));
  for (const rel of TANGLED) {
    fs.mkdirSync(path.join(tmp, path.dirname(rel)), { recursive: true });
    fs.copyFileSync(path.join(__dirname, '..', rel), path.join(tmp, rel));
  }
  const r = spawnSync(py, ['-I', '-X', 'utf8', script, tmp], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr || r.stdout);
  const txt = (rel) => fs.readFileSync(path.join(tmp, rel), 'utf8');
  for (const rel of ['ui/index.html', 'ui/js/studio.js', 'src-tauri/src/server/mod.rs']) assert.ok(!/previz/i.test(txt(rel)), `${rel} 還有預演台`);
  assert.match(txt('ui/index.html'), /data-t="mixdesk"/);
  assert.match(txt('ui/index.html'), /<!-- 混音台：/);
  assert.match(txt('ui/js/studio.js'), /\["plan", "edit", "frames", "video", "mixdesk"\]/);
  assert.match(txt('ui/js/studio.js'), /if \(t === "mixdesk"\) mixdeskShow\(\);/);
  assert.match(txt('ui/js/studio.js'), /混音台（ui\/mixdesk\.html/);
  assert.equal((txt('src-tauri/src/server/mod.rs').match(/api_mixdesk::/g) || []).length, 15);
  assert.match(txt('resources/extras/fablecut/anima-guide.md'), /## 混音台：/);
  fs.rmSync(tmp, { recursive: true, force: true });
});

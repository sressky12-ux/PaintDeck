const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const ROOT = path.join(__dirname, '..');

function makeClassList(onChange) {
  const set = new Set();
  return {
    add: (...cs) => { cs.forEach((c) => set.add(c)); onChange?.(); },
    remove: (...cs) => { cs.forEach((c) => set.delete(c)); onChange?.(); },
    toggle: (c, force) => {
      if (force === undefined) {
        const next = !set.has(c);
        if (next) set.add(c); else set.delete(c);
        onChange?.();
        return next;
      }
      if (force) set.add(c); else set.delete(c);
      onChange?.();
      return Boolean(force);
    },
    contains: (c) => set.has(c),
  };
}

function makeStyle() {
  const props = new Map();
  return {
    setProperty: (k, v) => props.set(k, String(v)),
    removeProperty: (k) => props.delete(k),
    getPropertyValue: (k) => props.get(k) || '',
  };
}

function loadFontScale(initialSaved = null) {
  let saved = initialSaved;
  const presetBtns = ['compact', 'default', 'comfort', 'large'].map((id) => ({
    dataset: { preset: id },
    classList: makeClassList(),
  }));
  const el = (extra = {}) => ({
    value: '',
    textContent: '',
    checked: false,
    disabled: false,
    offsetHeight: 180,
    scrollTop: 12,
    style: makeStyle(),
    classList: makeClassList(),
    ...extra,
  });
  const presetsBox = { onclick: null };
  const byId = {
    '#fsPresets': presetsBox,
    '#fsUiRange': el({ value: '1' }),
    '#fsPromptRange': el({ value: '1' }),
    '#fsUiOut': el(),
    '#fsPromptOut': el(),
    '#fsFrameBoost': el({ checked: false }),
    '#fsReset': el({ disabled: true }),
    '#fsStatus': el(),
    '#prompt': el({ offsetHeight: 220, scrollTop: 35 }),
    '#promptHl': el({ style: makeStyle(), scrollTop: 0 }),
  };
  const root = { classList: makeClassList(), style: makeStyle() };
  const source = fs.readFileSync(path.join(ROOT, 'ui/js/fontscale.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '')
    .replace(/^export /gm, '');

  const store = {
    get: (k, d) => (k === 'fontScale' && saved !== null ? saved : d),
    set: (k, v) => { if (k === 'fontScale') saved = v; },
  };

  const context = vm.createContext({
    document: { documentElement: root },
    $: (sel) => byId[sel] || null,
    $$: (sel) => (sel === '#fsPresets [data-preset]' ? presetBtns : []),
    store,
    setTimeout: () => 1,
    clearTimeout: () => {},
  });
  vm.runInContext(
    source + '\nglobalThis.subject = { PRESETS, DEFAULT_CONFIG, clampScale, detectPreset, normalizeConfig, configsEqual, computeCssVars, formatScaleLabel, apply, init };',
    context,
    { filename: 'fontscale.js' },
  );

  return {
    ...context.subject,
    root,
    byId,
    presetBtns,
    presetsBox,
    getSaved: () => saved,
    clickPreset(id) {
      const btn = presetBtns.find((b) => b.dataset.preset === id);
      presetsBox.onclick({ target: { closest: (s) => (s === 'button[data-preset]' ? btn : null) } });
    },
  };
}

test('fontscale: 配置規範化、步長吸附、極限值夾緊與標籤格式化', () => {
  const h = loadFontScale();
  assert.deepEqual(h.normalizeConfig(null), h.DEFAULT_CONFIG);

  const high = h.normalizeConfig({ uiScale: 9.9, promptScale: 3.0, frameBoost: true });
  assert.equal(high.uiScale, 1.45);
  assert.equal(high.promptScale, 1.6);
  assert.equal(high.frameBoost, true);
  assert.equal(high.preset, 'custom');

  const low = h.normalizeConfig({ uiScale: 0.2, promptScale: 'bad', frameBoost: 0 });
  assert.equal(low.uiScale, 0.85);
  assert.equal(low.promptScale, 1.0);
  assert.equal(low.frameBoost, false);

  const comfort = h.normalizeConfig({ uiScale: 1.14, promptScale: 1.21 });
  assert.equal(comfort.uiScale, 1.15);
  assert.equal(comfort.promptScale, 1.2);
  assert.equal(comfort.preset, 'comfort');
  assert.equal(h.formatScaleLabel(1.15, 13), '115% (15px)');
});

test('fontscale: 預設鎖定外框倍率，僅在勾選高分屏微擴時依上限放大', () => {
  const h = loadFontScale();
  const locked = h.computeCssVars({ uiScale: 1.45, promptScale: 1.6, frameBoost: false });
  assert.equal(locked['--fs-ui'], '1.45');
  assert.equal(locked['--fs-prompt'], '1.6');
  assert.equal(locked['--fs-frame'], '1');
  assert.equal(locked['--fs-top'], '1');

  const boosted = h.computeCssVars({ uiScale: 1.45, promptScale: 1.4, frameBoost: true });
  assert.equal(boosted['--fs-frame'], '1.2');
  assert.equal(boosted['--fs-top'], '1.12');
});

test('fontscale: index.html、settings.js 與 fontscale.css 接線及防破版守則', () => {
  const html = fs.readFileSync(path.join(ROOT, 'ui/index.html'), 'utf8');
  assert.match(html, /href="\/static\/css\/fontscale\.css"/);
  for (const id of ['fontScaleBox', 'fsPresets', 'fsUiRange', 'fsPromptRange', 'fsUiOut', 'fsPromptOut', 'fsFrameBoost', 'fsReset', 'fsStatus']) {
    assert.ok(html.includes(`id="${id}"`), `index.html 缺少 #${id}`);
  }

  const settings = fs.readFileSync(path.join(ROOT, 'ui/js/settings.js'), 'utf8');
  assert.match(settings, /BROWSER_KEYS\s*=\s*\[[^\]]*"fontScale"/, '恢復原廠瀏覽器清理清單須包含 fontScale');
  assert.match(settings, /fontscale\.init\(\)/, 'settings.init() 須呼叫 fontscale.init()');

  const css = fs.readFileSync(path.join(ROOT, 'ui/css/fontscale.css'), 'utf8');
  assert.doesNotMatch(css, /^\s*html\.fs-on\s+\*\s*\{/m, '禁止使用全局 * 覆蓋字號');
  assert.doesNotMatch(css, /html\.fs-on\s+\.check(?![\w-])/, '禁止以裸 .check 覆蓋圖庫 .gallery .g .check (font-size: 0)');
  assert.match(css, /html\.fs-on \.hl-wrap textarea,\s*html\.fs-on \.hl[\s\S]*--fs-prompt/);
  assert.match(css, /html\.fs-on \.hl-wrap textarea,\s*html\.fs-on \.hl\s*\{[^}]*line-height:\s*1\.6/);
  assert.match(css, /html\.fs-frame \.deck\s*\{[^}]*calc\(76px \* var\(--fs-frame, 1\)\)/);
});

test('fontscale: 初始化載入已存設定、滑桿即時預覽、預設切換與恢復原版', () => {
  const h = loadFontScale({ uiScale: 1.15, promptScale: 1.2, frameBoost: false });
  h.init();

  assert.equal(h.root.classList.contains('fs-on'), true);
  assert.equal(h.root.classList.contains('fs-frame'), false);
  assert.equal(h.root.style.getPropertyValue('--fs-ui'), '1.15');
  assert.equal(h.root.style.getPropertyValue('--fs-prompt'), '1.2');
  assert.equal(h.byId['#fsUiOut'].textContent, '115% (15px)');
  assert.equal(h.byId['#fsReset'].disabled, false);
  assert.equal(h.byId['#promptHl'].style.height, '220px');
  assert.equal(h.byId['#promptHl'].scrollTop, 35);

  // 拖動滑桿即時預覽，此時尚未寫回 store
  h.byId['#fsUiRange'].value = '1.3';
  h.byId['#fsUiRange'].oninput();
  assert.equal(h.root.style.getPropertyValue('--fs-ui'), '1.3');
  assert.equal(h.getSaved().uiScale, 1.15);

  // 放開滑桿正式存檔
  h.byId['#fsUiRange'].onchange();
  assert.equal(h.getSaved().uiScale, 1.3);
  assert.equal(h.byId['#fsStatus'].textContent, '已儲存');

  // 勾選高分屏外框同步微擴
  h.byId['#fsFrameBoost'].checked = true;
  h.byId['#fsFrameBoost'].onchange();
  assert.equal(h.root.classList.contains('fs-frame'), true);
  assert.equal(h.getSaved().frameBoost, true);

  // 點「護眼大字 130%」預設
  h.clickPreset('large');
  assert.equal(h.getSaved().preset, 'large');
  assert.equal(h.getSaved().promptScale, 1.4);

  // 點「恢復原版」清除 fs-on / fs-frame 與內聯變數
  h.byId['#fsReset'].onclick();
  assert.equal(h.root.classList.contains('fs-on'), false);
  assert.equal(h.root.classList.contains('fs-frame'), false);
  assert.equal(h.root.style.getPropertyValue('--fs-ui'), '');
  assert.equal(h.byId['#fsReset'].disabled, true);
  assert.equal(h.byId['#fsStatus'].textContent, '已恢復為原版字號');
});

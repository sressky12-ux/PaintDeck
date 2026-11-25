import { readFile, writeFile, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { parseArgs } from 'node:util';
import { setTimeout as sleep } from 'node:timers/promises';
import { openEdge } from './cdp.mjs';
import { IMAGE, scenariosFor, forceStyleguideStates } from './scenarios.mjs';
import { newDefects, renderMarkdown } from './report.mjs';

function options() {
  const names = ['base', 'out', 'style', 'lang', 'widths', 'themes', 'only', 'compare'];
  const { values } = parseArgs({ options: Object.fromEntries(names.map(k => [k, { type: 'string' }])) });
  if (!values.base || !values.out) throw new Error('請提供 --base 與 --out');
  const base = new URL(values.base);
  if (base.origin !== 'http://127.0.0.1:8193' || base.username || base.password) throw new Error('--base 只接受 127.0.0.1:8193 測試站');
  const style = values.style ?? '', lang = values.lang ?? 'zh-TW';
  if (!['', 'a'].includes(style) || !['zh-TW', 'zh-CN'].includes(lang)) throw new Error('無效的 --style 或 --lang');
  const widths = (values.widths ?? '1920,1100').split(',').map(Number);
  const themes = (values.themes ?? 'dark,light').split(',');
  if (!widths.every(w => Number.isSafeInteger(w) && w > 0) || !themes.every(t => ['dark', 'light'].includes(t))) {
    throw new Error('無效的 --widths 或 --themes');
  }
  return { ...values, base: base.href, style, lang, widths, themes, only: values.only?.split(',') };
}

// 繪匣有一直開著的連線（即時事件、長輪詢），永遠不會 loadingFinished；開超過 LONG_LIVED_MS 的請求當成常駐連線，不等它
const LONG_LIVED_MS = 4000;

function monitorLoads(edge) {
  const loading = new Map();
  let last = Date.now();
  edge.on('Network.requestWillBeSent', ({ requestId, type }) => {
    if (!['Document', 'Fetch', 'XHR', 'Script', 'Stylesheet', 'Font', 'Image'].includes(type)) return;
    loading.set(requestId, Date.now());
    last = Date.now();
  });
  function finish({ requestId }) {
    if (loading.delete(requestId)) last = Date.now();
  }
  edge.on('Network.loadingFinished', finish);
  edge.on('Network.loadingFailed', finish);
  const busy = () => [...loading.values()].filter((t) => Date.now() - t < LONG_LIVED_MS).length;
  return async function settle() {
    const start = Date.now();
    while (busy() || Date.now() - Math.max(last, start) < 350) {
      if (Date.now() - start > 25000) throw new Error('等待畫面資源逾時，未完成 ' + busy() + ' 個請求');
      await sleep(50);
    }
    await edge.evaluate('document.fonts.ready.then(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(true)))))');
  };
}

async function runningScene(edge, setup, base) {
  async function fulfill({ requestId, responseStatusCode, responseHeaders }) {
    if (responseStatusCode !== 200) throw new Error('/api/state HTTP ' + responseStatusCode);
    const data = await edge.send('Fetch.getResponseBody', { requestId });
    const state = JSON.parse(data.base64Encoded ? Buffer.from(data.body, 'base64').toString() : data.body);
    state.running = 'ui-audit';
    state.progress = { job: state.running, value: 8, max: 20 };
    const headers = responseHeaders.filter(h => !['content-encoding', 'content-length', 'transfer-encoding'].includes(h.name.toLowerCase()));
    await edge.send('Fetch.fulfillRequest', { requestId, responseCode: 200, responseHeaders: headers,
      body: Buffer.from(JSON.stringify(state)).toString('base64') });
  }

  let off;
  const failed = new Promise((_, reject) => {
    off = edge.on('Fetch.requestPaused', params => { fulfill(params).catch(reject); });
  });
  try {
    await edge.send('Fetch.enable', { patterns: [{ urlPattern: new URL('/api/state', base).href + '*', requestStage: 'Response' }] });
    await Promise.race([edge.evaluate(setup), failed]);
  } finally {
    off();
    await edge.send('Fetch.disable');
  }
}

async function drawMask(edge, { x, y, dx, dy }) {
  const mouse = (type, x, y, buttons) => edge.send('Input.dispatchMouseEvent', {
    type, x, y, button: 'left', buttons, clickCount: 1,
  });
  await mouse('mousePressed', x, y, 1);
  await mouse('mouseMoved', x + dx, y + dy, 1);
  await mouse('mouseReleased', x + dx, y + dy, 0);
  const drawn = await edge.evaluate('(() => { const c = document.querySelector("#maskDraw"); return c.getContext("2d").getImageData(c.width * 0.4, c.height * 0.45, 1, 1).data[3] > 0; })()');
  if (!drawn) throw new Error('遮罩畫布缺少測試筆觸');
}

function inspect(screenshotOnly) {
  if (screenshotOnly) return { defects: [], classes: {} };
  const classes = Object.create(null);
  for (const el of document.querySelectorAll('[class]')) {
    if (!el.checkVisibility({ opacityProperty: true, visibilityProperty: true })) continue;
    for (const c of el.classList) classes[c] = (classes[c] ?? 0) + 1;
  }
  const defects = globalThis.__uiAuditDetect();
  const gen = document.querySelector('#page-gen.on');
  if (!gen) return { defects, classes };
  const monitor = gen.querySelector('.monitor'), viewer = monitor.querySelector('.viewer');
  const bottom = gen.querySelector('[data-zone="bottom"]'), acts = monitor.querySelector('#viewActions');
  const height = el => el.getBoundingClientRect().height;
  const viewerRatio = height(viewer) / (height(monitor) + height(bottom));
  const buttons = [...acts.querySelectorAll(':scope > button')].filter(b => b.checkVisibility());
  const actsRows = new Set(buttons.map(b => b.offsetTop)).size;
  function flag(kind, selector, el, value) {
    const { x, y, width, height } = el.getBoundingClientRect();
    defects.push({ kind, selector, text: String(value), rect: [x, y, width, height] });
  }
  if (viewerRatio < 0.5) flag('viewer-squeezed', '.monitor .viewer', viewer, viewerRatio);
  if (actsRows > 1) flag('acts-wrapped', '#viewActions', acts, actsRows);
  return { defects, classes, viewerRatio, actsRows };
}

async function capture(edge, opts, report) {
  const home = new URL(opts.base), guide = new URL('static/styleguide.html', home);
  for (const url of [home, guide]) {
    if (opts.style) url.searchParams.set('style', opts.style);
  }
  const detect = await readFile(new URL('detect.js', import.meta.url), 'utf8');
  const settle = monitorLoads(edge);
  await edge.send('Network.enable');
  await edge.send('Network.setBlockedURLs', { urls: ['*://*:8190/*'] });
  // 在 App 初次載入前保存原值。每次換情境都從同一份表單、分頁與語言設定開始。
  const snapshot = await edge.send('Page.addScriptToEvaluateOnNewDocument', {
    source: 'if (self === top) globalThis.__uiAuditStorage = Object.entries(localStorage);',
  });
  await edge.navigate(home.href);
  const original = await edge.evaluate('globalThis.__uiAuditStorage');
  await edge.send('Page.removeScriptToEvaluateOnNewDocument', snapshot);
  const restore = 'localStorage.clear(); for (const [k,v] of ' + JSON.stringify(original) + ') localStorage.setItem(k,v);';
  const reset = await edge.send('Page.addScriptToEvaluateOnNewDocument', {
    source: 'if (self === top && location.origin === ' + JSON.stringify(home.origin) + ') {' + restore +
      'localStorage.setItem("aw_lang",' + JSON.stringify(JSON.stringify(opts.lang)) + ');}',
  });
  try {
    const { pages, tabs } = await edge.evaluate('({pages: [...document.querySelectorAll("#tabs [data-page]")].map(b => b.dataset.page), tabs: [...document.querySelectorAll("#page-director [data-t][role=tab]")].map(b => b.dataset.t)})');
    if (!pages.length || !tabs.length) throw new Error('測試站缺少主頁或導演台頁籤');
    const scenarios = scenariosFor(pages, tabs);
    for (const id of opts.only ?? []) {
      if (!scenarios.some(s => s.id === id)) throw new Error('未知情境 ' + id);
    }
    let imageData;
    for (const { id, setup } of scenarios) {
      if (opts.only && !opts.only.includes(id)) continue;
      const isGuide = id === 'styleguide';
      if (isGuide) {
        const res = await fetch(guide, { redirect: 'error', signal: AbortSignal.timeout(15000) });
        await res.body?.cancel();
        if (res.status === 404) { console.log('略過 styleguide（HTTP 404）'); continue; }
        if (!res.ok) throw new Error(guide.href + ' HTTP ' + res.status);
      }
      const paste = ['gen:inpaint', 'gen:mask'].includes(id);
      if (paste && !imageData) {
        // 由 Node 讀指定圖片，避免 ComfyUI 跨埠 CORS 擋住瀏覽器產生 File。
        const res = await fetch(IMAGE, { redirect: 'error', signal: AbortSignal.timeout(15000) });
        if (!res.ok) throw new Error('讀取測試圖 HTTP ' + res.status);
        imageData = 'data:image/png;base64,' + Buffer.from(await res.arrayBuffer()).toString('base64');
      }
      for (const theme of opts.themes) for (const width of opts.widths) {
        console.log(id, theme, width);
        await edge.setViewport(width, 910);
        await edge.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: theme }] });
        await edge.navigate(isGuide ? guide.href : home.href);
        // 頁面自己的 main.js 走 /static-<啟動時間>/ 前綴；再 import /static/js/main.js 會多跑一份 init。改成等圖示畫好（deck.js init 跑完）
        if (!isGuide) await edge.evaluate('new Promise((ok, no) => { const t0 = Date.now(); (function wait() { if (document.querySelector("[data-ic] svg")) ok(true); else if (Date.now() - t0 > 15000) no(new Error("App 15 秒內沒有初始化")); else setTimeout(wait, 100); })(); })');
        await edge.evaluate('document.documentElement.setAttribute("data-theme",' + JSON.stringify(theme) + ')');
        await settle();
        if (paste) await edge.evaluate('void (globalThis.__uiAuditImageData = ' + JSON.stringify(imageData) + ')');
        if (id === 'gen:running') await runningScene(edge, setup, home);
        else {
          const result = await edge.evaluate(setup);
          if (id === 'gen:mask') await drawMask(edge, result);
          if (isGuide) {
            await edge.setViewport(width, result);
            await forceStyleguideStates(edge);
          }
        }
        await settle();
        await edge.evaluate(detect);
        const result = await edge.evaluate('(' + inspect.toString() + ')(' + (id === 'director:edit') + ')');
        const file = id.replace(':', '_') + '_' + theme + '_' + width + '.png';
        await edge.screenshot(path.join(opts.out, file));
        report.shots.push({ scenario: id, theme, width, file, ...result });
      }
    }
  } finally {
    await edge.send('Page.removeScriptToEvaluateOnNewDocument', reset);
    await edge.evaluate('if (location.origin === ' + JSON.stringify(home.origin) + ') {' + restore + '}');
  }
}

async function main() {
  const opts = options();
  const previous = opts.compare ? JSON.parse(await readFile(opts.compare, 'utf8')) : null;
  const { base, style, lang } = opts;
  const report = { meta: { base, style, lang, date: new Date().toISOString() }, shots: [] };
  const edge = await openEdge({ port: 0 });
  try {
    await mkdir(opts.out, { recursive: true });
    await capture(edge, opts, report);
    await writeFile(path.join(opts.out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
    await writeFile(path.join(opts.out, 'report.md'), renderMarkdown(report));
    if (previous) {
      const added = newDefects(previous, report);
      console.log('新增缺陷 ' + added.length + ' 筆');
      for (const d of added) console.log(JSON.stringify(d));
      if (added.length) process.exitCode = 1;
    }
  } finally { await edge.close(); }
}

main().catch(error => { console.error(error); process.exitCode = 1; });

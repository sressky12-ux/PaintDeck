export const IMAGE = 'http://127.0.0.1:8188/view?filename=txt2img_02300_.png&subfolder=AnimaWeb&type=output';

// 序列化到頁面執行。每個情境可單獨從剛載入的頁面開始。
async function setupScenario(id, image) {
  const q = selector => document.querySelector(selector);
  async function until(test, label) {
    const deadline = performance.now() + 15000;
    while (!test()) {
      if (performance.now() >= deadline) throw new Error(id + ' 等不到 ' + label);
      await new Promise(resolve => setTimeout(resolve, 50));
    }
  }

  if (id === 'styleguide') {
    await until(() => q('#glyph-map tr') && q('[data-sg-state="focus"]'), '樣張狀態與圖示表');
    await document.fonts.ready;
    return document.documentElement.scrollHeight;
  }
  const [group, name] = id.split(':');
  const page = group === 'page' ? name : group;
  q('#tabs [data-page="' + CSS.escape(page) + '"]').click();
  await until(() => q('#page-' + CSS.escape(page) + '.on'), page);

  if (group === 'director') {
    const tab = name === 'shotsheet' ? 'video' : name;
    const button = q('#page-director [role="tab"][data-t="' + CSS.escape(tab) + '"]');
    button.click();
    await until(() => button.getAttribute('aria-selected') === 'true', tab);
    if (name === 'shotsheet') {
      if (q('#shDrawer').hidden) q('#shOpen').click();
      await until(() => !q('#shDrawer').hidden, '#shDrawer');
    }
  }
  if (group !== 'gen') return;
  if (name === 'extras') {
    if (q('#promptTabNeg').hidden) {
      const model = q('#model');
      const option = [...model.options].find(o => o.closest('optgroup')?.label === 'Anima');
      if (!option) throw new Error('gen:extras 需要支援負面提示詞的模型');
      model.value = option.value;
      model.dispatchEvent(new Event('change', { bubbles: true }));
    }
    q('#modeSeg [data-mode="txt2img"]').click();
    q('#promptTabNeg').click();
    if (q('#tagLib').hidden) q('#tagLibBtn').click();
    await until(() => q('.prompt-scr[data-tab="neg"]') && !q('#tagLib').hidden, '負面分頁與詞庫抽屜');
  }
  if (name === 'result') {
    const img = q('#viewImg');
    img.src = image;
    img.hidden = false;
    img.classList.add('on');
    q('#viewEmpty').hidden = true;
    await img.decode();
  }
  if (name === 'inpaint' || name === 'mask') {
    q('#modeSeg [data-mode="inpaint"]').click();
    await until(() => q('#modeSeg [data-mode="inpaint"].on'), '局部重繪');
    const response = await fetch(globalThis.__uiAuditImageData ?? image);
    if (!response.ok) throw new Error('讀取測試圖 HTTP ' + response.status);
    const transfer = new DataTransfer();
    transfer.items.add(new File([await response.blob()], 'txt2img_02300_.png', { type: 'image/png' }));
    document.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer }));
    await until(() => q('#initDrop.has') && q('#initImg').naturalWidth > 0, '貼上的輸入圖');
    if (name === 'mask') {
      q('#maskEdit').click();
      const canvas = q('#maskDraw');
      await until(() => !q('#maskModal').hidden && canvas.style.width && canvas.width === q('#initImg').naturalWidth, '遮罩畫布');
      // CDP 的真實滑鼠事件會產生有效的 pointerId，供 mask.js 的 setPointerCapture 使用。
      const r = canvas.getBoundingClientRect();
      return { x: r.x + r.width * 0.4, y: r.y + r.height * 0.45, dx: r.width * 0.18, dy: r.height * 0.12 };
    }
  }
  if (name === 'posewear') {
    q('#pwBtn').click();
    await until(() => q('#pwDrawer') && !q('#pwDrawer').hidden, '衣姿抽屜');
  }
  if (name === 'running') {
    // 頁面的模組走 /static-<啟動時間>/ 前綴；import('/static/js/…') 會拿到另一份新的模組，讀不到頁面的狀態
    const root = q('script[type="module"][src$="/js/main.js"]').src.replace(/\/js\/main\.js$/, '');
    const { app } = await import(root + '/js/state.js');
    await app.refreshState();
    const { running, progress } = app.state;
    if (running !== 'ui-audit' || !progress?.max) throw new Error('未收到 CDP 模擬的進度');
    // main.js 由 WebSocket 通知畫面更新進度，refreshState 本身只讀狀態。
    const deck = await import(root + '/js/deck.js');
    deck.onStart(running, true);
    deck.onProgress(progress);
    q('#topProg > div').style.width = (100 * progress.value / progress.max) + '%';
  }
}

// CSS.forcePseudoState 屬於 DevTools，必須由截圖端透過 CDP 呼叫。
export async function forceStyleguideStates(edge) {
  await edge.send('DOM.enable');
  await edge.send('CSS.enable');
  const { root } = await edge.send('DOM.getDocument');
  for (const state of ['hover', 'active', 'focus']) {
    const selector = '[data-sg-state="' + state + '"]';
    const { nodeIds } = await edge.send('DOM.querySelectorAll', { nodeId: root.nodeId, selector });
    if (!nodeIds.length) throw new Error('樣張缺少 ' + selector);
    const forcedPseudoClasses = state === 'focus' ? ['focus', 'focus-visible'] : [state];
    for (const nodeId of nodeIds) await edge.send('CSS.forcePseudoState', { nodeId, forcedPseudoClasses });
  }
}

export function scenariosFor(pages, tabs) {
  const ids = [...pages.map(p => 'page:' + p), ...tabs.map(t => 'director:' + t),
    'gen:extras', 'gen:result', 'gen:inpaint', 'gen:mask', 'gen:running', 'director:shotsheet', 'gen:posewear', 'styleguide'];
  return ids.map(id => ({ id, setup: '(' + setupScenario.toString() + ')(' + JSON.stringify(id) + ',' + JSON.stringify(IMAGE) + ')' }));
}

// 目前的清單也可供離線使用；run 會從測試站當下的頁籤產生同樣格式。
export const SCENARIOS = scenariosFor(
  ['gen', 'taglib', 'gallery', 'assets', 'posewear', 'loras', 'civitai', 'director', 'comic', 'agent', 'train', 'api', 'booru', 'settings'],
  ['plan', 'edit', 'frames', 'video', 'previz', 'mixdesk'],
);

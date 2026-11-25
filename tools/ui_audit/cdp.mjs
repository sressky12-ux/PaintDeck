import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

const EDGE = 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
const TIMEOUT = 30000;

async function bounded(promise, label, ms = TIMEOUT) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(label + ' 逾時')), ms);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

export async function openEdge({ port = 9335, profileDir, width = 1920, height = 910 } = {}) {
  const profile = profileDir ?? await mkdtemp(path.join(tmpdir(), 'paintdeck-audit-'));
  let child, ws, closed;
  const pending = new Map(), listeners = new Map();
  let seq = 0;

  function on(method, handler) {
    if (!listeners.has(method)) listeners.set(method, new Set());
    listeners.get(method).add(handler);
    return () => listeners.get(method).delete(handler);
  }

  async function send(method, params = {}) {
    const id = ++seq;
    try {
      return await bounded(new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject, method });
        ws.send(JSON.stringify({ id, method, params }));
      }), method);
    } finally {
      pending.delete(id);
    }
  }

  async function evaluate(expression) {
    const { result, exceptionDetails } = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (exceptionDetails) throw new Error(exceptionDetails.exception?.description || exceptionDetails.text);
    return result.value;
  }

  async function navigate(url) {
    // 載入事件可能先於 navigate 的回覆。只接受這次主框架的 loader。
    const loaded = new Set();
    let loader, notify;
    const ready = new Promise(resolve => { notify = resolve; });
    const off = on('Page.lifecycleEvent', event => {
      if (event.name !== 'load') return;
      loaded.add(event.loaderId);
      if (event.loaderId === loader) notify();
    });
    try {
      const nav = await send('Page.navigate', { url });
      if (nav.errorText) throw new Error(url + ': ' + nav.errorText);
      loader = nav.loaderId;
      if (loader && !loaded.has(loader)) await bounded(ready, '載入 ' + url);
    } finally { off(); }
  }

  const setViewport = (width, height) => send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
  async function screenshot(file) {
    const { data } = await send('Page.captureScreenshot', { format: 'png' });
    await writeFile(file, Buffer.from(data, 'base64'));
  }

  async function close() {
    if (ws?.readyState === WebSocket.OPEN) {
      try { await send('Browser.close'); } catch { /* Edge 也可能先關閉連線 */ }
    }
    ws?.close();
    if (child?.pid && child.exitCode === null && child.signalCode === null) {
      try { await bounded(closed, '關閉 Edge', 1500); }
      catch { child.kill(); await bounded(closed, '結束 Edge'); }
    }
    if (profileDir == null) await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }

  try {
    child = spawn(EDGE, ['--headless=new', '--disable-gpu', '--no-first-run',
      '--remote-debugging-port=' + port, '--user-data-dir=' + profile,
      '--window-size=' + width + ',' + height, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'], windowsHide: true });
    closed = new Promise(resolve => child.once('close', resolve));
    // 等這個程序自己公布埠號，避免連到另一個 Edge 或反覆啟動。
    const endpoint = await bounded(new Promise((resolve, reject) => {
      let stderr = '';
      child.once('error', reject);
      child.stderr.setEncoding('utf8').on('data', chunk => {
        stderr = (stderr + chunk).slice(-8192);
        const match = stderr.match(/DevTools listening on (ws:\/\/[^\s]+)/);
        if (match) resolve(new URL(match[1]));
      });
      child.once('exit', (code, signal) => reject(new Error(stderr.trim() || 'Edge exited: ' + (signal ?? code))));
    }), 'Edge DevTools');
    const response = await fetch('http://' + endpoint.host + '/json/list', { signal: AbortSignal.timeout(TIMEOUT) });
    if (!response.ok) throw new Error('CDP /json/list HTTP ' + response.status);
    const target = (await response.json()).find(t => t.type === 'page' && t.url === 'about:blank');
    if (!target) throw new Error('CDP 找不到 about:blank 分頁');
    ws = new WebSocket(target.webSocketDebuggerUrl);
    await bounded(new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = event => reject(event.error ?? new Error('CDP WebSocket 連線失敗'));
      ws.onclose = event => {
        const error = new Error('CDP WebSocket closed: ' + event.code + ' ' + event.reason);
        for (const entry of pending.values()) entry.reject(error);
        reject(error);
      };
    }), 'CDP WebSocket');
    ws.onmessage = event => {
      const msg = JSON.parse(event.data);
      if (msg.id) {
        const entry = pending.get(msg.id);
        if (!entry) return;
        if (msg.error) entry.reject(new Error(entry.method + ': ' + msg.error.message));
        else entry.resolve(msg.result);
      } else {
        for (const handler of listeners.get(msg.method) || []) handler(msg.params);
      }
    };
    await send('Page.enable');
    await send('Page.setLifecycleEventsEnabled', { enabled: true });
    await setViewport(width, height);
    return { send, on, evaluate, navigate, screenshot, setViewport, close };
  } catch (error) {
    await close();
    throw error;
  }
}

// 設定頁「介面與提示詞字號」：分層縮放正文／提示詞字號並同步高亮底板，預設鎖定外側功能欄防止破版。
import { $, $$, store } from "./util.js";

export const UI_MIN = 0.85;
export const UI_MAX = 1.45;
export const PROMPT_MIN = 0.85;
export const PROMPT_MAX = 1.60;
export const SCALE_STEP = 0.05;
export const FRAME_MIN = 0.94;
export const FRAME_MAX = 1.20;
export const TOP_MAX = 1.12;

export const PRESETS = [
  { id: "compact", uiScale: 0.90, promptScale: 0.95 },
  { id: "default", uiScale: 1.00, promptScale: 1.00 },
  { id: "comfort", uiScale: 1.15, promptScale: 1.20 },
  { id: "large", uiScale: 1.30, promptScale: 1.40 },
];

export const DEFAULT_CONFIG = Object.freeze({
  preset: "default",
  uiScale: 1.00,
  promptScale: 1.00,
  frameBoost: false,
});

const STORE_KEY = "fontScale";
let active = { ...DEFAULT_CONFIG };
let committed = { ...DEFAULT_CONFIG };
let statusTimer = null;

export function clampScale(value, min, max, fallback = 1, step = SCALE_STEP) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  const clamped = Math.min(max, Math.max(min, n));
  const snapped = Math.round(clamped / step) * step;
  return Number(Math.min(max, Math.max(min, snapped)).toFixed(2));
}

export function detectPreset(uiScale, promptScale) {
  for (const p of PRESETS) {
    if (Math.abs(p.uiScale - uiScale) < 0.001 && Math.abs(p.promptScale - promptScale) < 0.001) return p.id;
  }
  return "custom";
}

export function normalizeConfig(raw) {
  const src = raw && typeof raw === "object" ? raw : {};
  const uiScale = clampScale(src.uiScale, UI_MIN, UI_MAX, DEFAULT_CONFIG.uiScale);
  const promptScale = clampScale(src.promptScale, PROMPT_MIN, PROMPT_MAX, DEFAULT_CONFIG.promptScale);
  const frameBoost = Boolean(src.frameBoost);
  return { preset: detectPreset(uiScale, promptScale), uiScale, promptScale, frameBoost };
}

export function configsEqual(a, b) {
  if (!a || !b) return false;
  return Math.abs(a.uiScale - b.uiScale) < 0.001
    && Math.abs(a.promptScale - b.promptScale) < 0.001
    && Boolean(a.frameBoost) === Boolean(b.frameBoost);
}

export function computeCssVars(cfg) {
  const safe = normalizeConfig(cfg);
  let frameScale = 1;
  if (safe.frameBoost) {
    const raw = 1 + (safe.uiScale - 1) * 0.55;
    frameScale = Number(Math.min(FRAME_MAX, Math.max(FRAME_MIN, raw)).toFixed(3));
  }
  const topScale = safe.frameBoost ? Number(Math.min(TOP_MAX, Math.max(FRAME_MIN, frameScale)).toFixed(3)) : 1;
  const panelScale = Number(Math.min(1.25, Math.max(1, safe.uiScale)).toFixed(3));
  return {
    "--fs-ui": String(safe.uiScale),
    "--fs-prompt": String(safe.promptScale),
    "--fs-frame": String(frameScale),
    "--fs-top": String(topScale),
    "--fs-panel": String(panelScale),
  };
}

export function formatScaleLabel(scale, basePx = 13) {
  const pct = Math.round(scale * 100);
  const pxVal = Math.round((basePx * pct) / 10) / 10;
  return `${pct}% (${pxVal.toFixed(1).replace(/\.0$/, "")}px)`;
}

function syncPromptHighlight() {
  const ta = $("#prompt"), hl = $("#promptHl");
  if (ta && hl) {
    hl.style.height = `${ta.offsetHeight}px`;
    hl.scrollTop = ta.scrollTop;
  }
}

function showStatus(msg) {
  const el = $("#fsStatus");
  if (!el) return;
  el.textContent = msg;
  clearTimeout(statusTimer);
  statusTimer = setTimeout(() => { if ($("#fsStatus")) $("#fsStatus").textContent = ""; }, 3200);
}

function paintControls() {
  const uiRange = $("#fsUiRange"), promptRange = $("#fsPromptRange");
  if (!uiRange || !promptRange) return;
  uiRange.value = String(active.uiScale);
  promptRange.value = String(active.promptScale);
  if ($("#fsUiOut")) $("#fsUiOut").textContent = formatScaleLabel(active.uiScale, 13);
  if ($("#fsPromptOut")) $("#fsPromptOut").textContent = formatScaleLabel(active.promptScale, 13);
  if ($("#fsFrameBoost")) $("#fsFrameBoost").checked = Boolean(active.frameBoost);
  for (const b of $$("#fsPresets [data-preset]")) b.classList.toggle("on", b.dataset.preset === active.preset);
  if ($("#fsReset")) $("#fsReset").disabled = configsEqual(active, DEFAULT_CONFIG);
}

export function apply(cfg) {
  const next = normalizeConfig(cfg);
  active = next;
  const root = document.documentElement;
  if (root) {
    const vars = computeCssVars(next);
    const isDefault = configsEqual(next, DEFAULT_CONFIG);
    root.classList.toggle("fs-on", !isDefault);
    root.classList.toggle("fs-frame", Boolean(next.frameBoost) && vars["--fs-frame"] !== "1");
    for (const [k, v] of Object.entries(vars)) {
      if (isDefault) root.style.removeProperty(k);
      else root.style.setProperty(k, v);
    }
  }
  syncPromptHighlight();
  paintControls();
  return next;
}

function commit(cfg, msg = "已儲存") {
  const next = apply(cfg);
  if (configsEqual(next, committed)) return next;
  committed = next;
  store.set(STORE_KEY, next);
  showStatus(msg);
  return next;
}

export function init() {
  const saved = normalizeConfig(store.get(STORE_KEY, DEFAULT_CONFIG));
  committed = saved;
  apply(saved);

  const presets = $("#fsPresets");
  if (presets) {
    presets.onclick = (e) => {
      const b = e.target.closest("button[data-preset]");
      if (!b) return;
      const p = PRESETS.find((x) => x.id === b.dataset.preset);
      if (p) commit({ ...active, uiScale: p.uiScale, promptScale: p.promptScale });
    };
  }
  const uiRange = $("#fsUiRange");
  if (uiRange) {
    uiRange.oninput = () => apply({ ...active, uiScale: Number(uiRange.value) });
    uiRange.onchange = () => commit({ ...active, uiScale: Number(uiRange.value) });
  }
  const promptRange = $("#fsPromptRange");
  if (promptRange) {
    promptRange.oninput = () => apply({ ...active, promptScale: Number(promptRange.value) });
    promptRange.onchange = () => commit({ ...active, promptScale: Number(promptRange.value) });
  }
  const frameCheck = $("#fsFrameBoost");
  if (frameCheck) {
    frameCheck.onchange = () => commit({ ...active, frameBoost: frameCheck.checked });
  }
  const resetBtn = $("#fsReset");
  if (resetBtn) {
    resetBtn.onclick = () => commit(DEFAULT_CONFIG, "已恢復為原版字號");
  }
}

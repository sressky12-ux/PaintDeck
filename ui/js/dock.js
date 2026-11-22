// 固定版面（2026-11-22 使用者：「可以改成固定介面，不要讓人拉來拉去。頂多收起展開」；看完三份草圖選 A 案「固定舞台」）。
// 之前的自由版面（2026-10-26 起：拖曳、吸附、浮動、貼邊隱藏、拉縫改大小、釘選看圖區）整套拿掉了。
// 每個版面有一塊固定的看圖區（主區），其他標了的區塊是面板，一律放在 spec 寫的那一區（上／左／右／下，或膠囊），寬高照 spec.sizes。
// 使用者能做的只有：整區收起成細條／點細條打開、單塊面板收起／展開（標題列或 ▾）。
// 收起的狀態存在 localStorage（store 的 dock_<名稱>_fixed）；舊的 dock_<名稱>（自由版面的存檔）不讀也不刪。設定頁「全部展開」清掉。
// 上半是純函式（tests/dock_layout.test.cjs 測），下半是 DOM。
import { el, store } from "./util.js";

import { makeBorrower } from "./borrow.js";

export const DOCK_V = 2;
export const FIXED_V = 1;
export const ZONES = ["top", "left", "right", "bottom"];
const RAIL = 28;          // 收起後剩的直條（橫條）寬
const GAP = 10;           // 區跟看圖區之間的縫
const MAIN_MIN_W = 420, MAIN_MIN_H = 320;   // 看圖區至少留這麼大
const MIN = { top: 100, left: 260, right: 100, bottom: 100 };
const MAX_RATIO = { top: 0.5, left: 0.5, right: 0.5, bottom: 0.5 };
const NARROW = 1180;      // 視窗比這窄：左右區當作收起（點開只算這次）
const ZONE_NAME = { top: "上面", left: "左邊", right: "右邊", bottom: "下面" };

const copy = (L) => JSON.parse(JSON.stringify(L));
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

/** 照每塊面板的預設區排好的版面 */
export function defaultLayout(spec) {
  const zones = {};
  for (const z of ZONES) {
    const ids = spec.panels.filter((p) => p.zone === z).map((p) => p.id);
    zones[z] = { ids, size: spec.sizes[z] ?? MIN[z] * 2, collapsed: false, tabs: false, active: ids[0] ?? null };
  }
  const cap = spec.panels.filter((p) => p.zone === "cap").map((p) => p.id);
  return { v: DOCK_V, zones, ...(cap.length ? { cap } : {}), float: {}, hide: {}, folded: [], pinned: true };
}

/** 讀回來的收起狀態 → 一定能用：版本不對或壞掉就全展開；collapsed 只收 true；folded 只留認得的面板、不重複 */
export function normalizeFixed(saved, spec) {
  const out = { v: FIXED_V, collapsed: Object.fromEntries(ZONES.map((z) => [z, false])), folded: [] };
  if (!saved || typeof saved !== "object" || saved.v !== FIXED_V) return out;
  const c = saved.collapsed && typeof saved.collapsed === "object" ? saved.collapsed : {};
  for (const z of ZONES) out.collapsed[z] = c[z] === true;
  const known = new Set(spec.panels.map((p) => p.id));
  out.folded = [...new Set((Array.isArray(saved.folded) ? saved.folded : []).filter((id) => known.has(id)))];
  return out;
}

/** 固定版面：預設版面＋收起狀態 */
export function fixedLayout(spec, state) {
  const L = defaultLayout(spec);
  for (const z of ZONES) L.zones[z].collapsed = state.collapsed[z] === true;
  L.folded = [...state.folded];
  return L;
}

/** 面板在哪：{kind:"zone", zone}｜{kind:"cap"}｜null */
export function whereIs(layout, id) {
  const z = ZONES.find((k) => layout.zones[k].ids.includes(id));
  if (z) return { kind: "zone", zone: z };
  if (layout.cap?.includes(id)) return { kind: "cap" };
  return null;
}

/** 每區有最小值，最大是可用空間的一半 */
export function clampSize(zone, px, avail) {
  const max = Math.max(MIN[zone], Math.floor(avail * MAX_RATIO[zone]));
  return Math.round(Math.max(MIN[zone], Math.min(max, px)));
}

/** 實際要套用的像素：沒面板的區 0、收起的區剩直條；太大就按比例縮，看圖區至少留 MAIN_MIN_W×MAIN_MIN_H */
export function fitSizes(layout, viewW, viewH) {
  const out = {};
  for (const z of ZONES) {
    const s = layout.zones[z];
    out[z] = !s.ids.length ? 0 : s.collapsed ? RAIL : Math.max(MIN[z], s.size);
  }
  const open = (z) => layout.zones[z].ids.length && !layout.zones[z].collapsed;
  const squeeze = (pair, view, mainMin) => {
    const gaps = pair.filter((z) => out[z]).length * GAP;
    const flex = pair.filter(open);
    const fixed = pair.filter((z) => !open(z)).reduce((a, z) => a + out[z], 0);
    const room = Math.max(0, view - mainMin - gaps - fixed);
    const want = flex.reduce((a, z) => a + out[z], 0);
    if (want > room && want > 0) for (const z of flex) out[z] = Math.max(RAIL, Math.floor(out[z] * room / want));
    for (const z of flex) out[z] = Math.min(out[z], Math.max(MIN[z], Math.floor(view * MAX_RATIO[z])));
  };
  squeeze(["left", "right"], viewW, MAIN_MIN_W);
  squeeze(["top", "bottom"], viewH, MAIN_MIN_H);
  return out;
}

// ================================================================ DOM
export function popPosition(chip, pop, view) {
  const left = Math.max(8, Math.min(chip.left, view.w - pop.w - 8));
  return { left, top: Math.max(8, chip.top - pop.h - 8),
    arrowX: clamp(chip.left + chip.width / 2 - left, 16, pop.w - 16) };
}

export function closeAllCaps() {
  for (const dock of docks.values()) dock.closeCaps();
}

const docks = new Map();   // 名稱 → { reset }

/** 全部展開：給名稱只動那個，不給就全部 */
export function resetDock(name) {
  for (const [n, d] of docks) if (!name || n === name) d.reset();
}

/**
 * 把 page（整頁，或頁裡面一個放欄位的容器）的面板排成固定版面；page 原本的子元素會被換成四區＋看圖區。spec：
 *   name     版面名稱（存收起狀態用，每個容器一個）
 *   main     看圖區要放的元素（可以是陣列）
 *   panels   [{ id, title, zone, min, el }]：zone 是放在哪一邊（"cap"＝膠囊）；el 是面板內容，不給就找 page 裡 data-dock="<id>" 的元素
 *   sizes    { top, left, right, bottom }：各區尺寸
 *   inset    true＝容器本身已經在一塊機殼面板裡（導演台、漫畫台…），區不再畫成一塊塊機殼
 *   caps     { host, chips: { id: { label, summary(), visible?() } } }：膠囊列
 *   panels 的 chip: true：放在區裡的面板也做一顆膠囊（生成頁的左區面板給隨機頁用：頁面藏著時點膠囊改成彈出、借內容）
 *   onRender 每次排完版面叫一次（生成頁用來決定成品動作放右區還是觀景窗下面）
 */
export function mountDock(page, spec) {
  const key = "dock_" + spec.name + "_fixed";
  const title = Object.fromEntries(spec.panels.map((p) => [p.id, p.title]));
  let L = fixedLayout(spec, normalizeFixed(store.get(key, null), spec));
  const peek = new Set();      // 窄視窗時點開的區（不存）

  const chips = new Map();
  let opened = null;

  function closeCaps() {
    if (!opened) return;
    const { borrower, pop, button, events, observer } = opened;
    opened = null;
    events.abort();
    observer.disconnect();
    borrower.giveBack();
    pop.remove();
    button.setAttribute("aria-expanded", "false");
    button.focus({ preventScroll: true });
  }

  function refreshCaps() {
    for (const [id, { button, content, value }] of chips) {
      const config = spec.caps.chips[id];
      const summary = config.summary(content);
      if (value.textContent !== summary) value.textContent = summary;
      button.hidden = config.visible ? !config.visible() : false;
      if (button.hidden && opened?.id === id) closeCaps();
    }
  }

  function openCap(id) {
    if (opened?.id === id) { closeCaps(); return; }
    const chip = chips.get(id);
    if (!chip || chip.button.hidden) return;
    closeAllCaps();
    const at = whereIs(L, id);
    if (at?.kind === "zone" && page.getClientRects().length) {
      L.folded = L.folded.filter((x) => x !== id);
      L.zones[at.zone].collapsed = false;
      peek.add(at.zone);
      commit();
      const panel = wrap[id];
      panel.scrollIntoView({ block: "nearest" });
      panel.classList.add("flash");
      setTimeout(() => panel.classList.remove("flash"), 600);
      return;
    }
    const { button, content } = chip;
    const popBody = el("div", { class: "cap-body" });
    const pop = el("section", { class: "cap-pop", role: "dialog", id: button.getAttribute("aria-controls"), "aria-label": title[id] },
      el("header", {}, el("strong", {}, title[id]), el("button", { type: "button", class: "akey", onclick: closeCaps }, "關閉")), popBody);
    document.getElementById("capLayer").append(pop);
    const borrower = makeBorrower([content], popBody);
    borrower.lend();
    const position = () => {
      const pos = popPosition(button.getBoundingClientRect(), { w: pop.offsetWidth, h: pop.offsetHeight }, { w: innerWidth, h: innerHeight });
      pop.style.left = pos.left + "px";
      pop.style.top = pos.top + "px";
      pop.style.setProperty("--arrow-x", pos.arrowX + "px");
    };
    const events = new AbortController();
    const observer = new ResizeObserver(position);
    opened = { id, borrower, pop, button, events, observer };
    observer.observe(pop);
    window.addEventListener("resize", position, { signal: events.signal });
    window.addEventListener("scroll", position, { signal: events.signal, capture: true });
    document.addEventListener("pointerdown", (e) => {
      if (!pop.contains(e.target) && !button.contains(e.target)) closeCaps();
    }, { capture: true, signal: events.signal });
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && !document.querySelector(".modal:not([hidden])")) {
        e.preventDefault(); e.stopImmediatePropagation(); closeCaps();
      }
    }, { capture: true, signal: events.signal });
    button.setAttribute("aria-expanded", "true");
    position();
    pop.querySelector("header .akey").focus({ preventScroll: true });
  }

  // ---- 骨架
  page.classList.add("dock-host", "dock-fixed");
  const wrap = {}, zone = {}, body = {}, bar = {}, rail = {}, split = {};
  for (const p of spec.panels) {
    const content = p.el || page.querySelector(`[data-dock="${p.id}"]`);
    if (!content) continue;
    const fold = el("button", { type: "button", class: "dock-fold", title: "收起／展開這塊", "aria-label": `收起／展開「${p.title}」`, onclick: () => toggleFold(p.id) }, "▾");
    const head = el("div", { class: "dock-head", title: "點一下收起／展開這塊" },
      el("span", { class: "dock-title" }, p.title), el("span", { class: "grow" }), fold);
    head.addEventListener("click", (e) => { if (!e.target.closest("button")) toggleFold(p.id); });
    if (p.zone === "cap" || p.chip) {
      const config = spec.caps.chips[p.id];
      const value = el("span", { class: "v" });
      const button = el("button", { type: "button", class: "cap-chip", "data-cap": p.id, "aria-haspopup": "dialog",
        "aria-expanded": "false", "aria-controls": `cap-${spec.name}-${p.id}`, onclick: () => openCap(p.id) },
        el("span", { class: "k" }, config.label), value, el("span", { class: "dd", "aria-hidden": "true" }, "▾"));
      spec.caps.host.append(button);
      chips.set(p.id, { button, value, content });
    }
    const b = el("div", { class: "dock-body" });
    wrap[p.id] = el("section", { class: "dock-panel", "data-id": p.id, "aria-label": p.title }, head, b);
    b.append(content);
  }
  for (const z of ZONES) {
    bar[z] = el("div", { class: "dock-zbar" });
    body[z] = el("div", { class: "dock-zbody" });
    rail[z] = el("button", { type: "button", class: "dock-rail", title: "打開這一區", onclick: () => openZone(z) });
    zone[z] = el("div", { class: spec.inset ? "dock-zone dock-inset" : "dock-zone panel screws", "data-zone": z, "aria-label": `${ZONE_NAME[z]}的停靠區` }, bar[z], body[z], rail[z]);
    split[z] = el("div", { class: "dock-split off", "data-split": z, "aria-hidden": "true" });   // 只是間距，拖不動
  }
  const main = el("div", { class: "dock-main" }, spec.main);
  page.replaceChildren(zone.top, split.top, zone.left, split.left, main, split.right, zone.right, split.bottom, zone.bottom);

  // ---- 畫
  const narrow = () => window.innerWidth <= NARROW;
  const side = (z) => z === "left" || z === "right";
  const collapsedNow = (z) => side(z) && narrow() ? !peek.has(z) : L.zones[z].collapsed;
  function effective() {
    const E = copy(L);
    for (const z of ZONES) E.zones[z].collapsed = collapsedNow(z);
    return E;
  }

  function apply() {
    const W = page.clientWidth, H = page.clientHeight;
    if (!W || !H) return;      // 頁面藏著，等顯示時 ResizeObserver 會再叫
    const s = fitSizes(effective(), W, H);
    const v = { t: s.top, l: s.left, r: s.right, b: s.bottom };
    for (const [k, px] of Object.entries(v)) {
      page.style.setProperty(`--dock-${k}`, px + "px");
      page.style.setProperty(`--gap-${k}`, (px ? GAP : 0) + "px");
    }
  }

  function render() {
    for (const z of ZONES) {
      const S = L.zones[z], col = collapsedNow(z);
      zone[z].classList.toggle("empty", !S.ids.length);
      zone[z].classList.toggle("collapsed", col);
      body[z].replaceChildren(...S.ids.map((id) => wrap[id]).filter(Boolean));
      for (const id of S.ids) if (wrap[id]) wrap[id].classList.toggle("solo", S.ids.length === 1);
      rail[z].textContent = S.ids.map((id) => title[id]).join("・");
      bar[z].replaceChildren(el("span", { class: "grow" }),
        el("button", { type: "button", class: "dock-btn", title: "收起這一區", "aria-label": `收起${ZONE_NAME[z]}的停靠區`, onclick: () => closeZone(z) },
          { top: "▴", left: "«", right: "»", bottom: "▾" }[z]));
    }
    for (const p of spec.panels) {
      const w = wrap[p.id];
      if (!w) continue;
      const at = whereIs(L, p.id);
      w.hidden = at?.kind === "cap";
      w.classList.toggle("folded", L.folded.includes(p.id));
      if (at?.kind === "cap" && w.parentElement !== page) page.append(w);   // 膠囊面板藏在容器裡，內容（#model 這些）還要留在畫面上給別的程式找
    }
    apply();
    spec.onRender?.();
  }

  function commit() {
    store.set(key, { v: FIXED_V, collapsed: Object.fromEntries(ZONES.map((z) => [z, L.zones[z].collapsed])), folded: L.folded });
    render();
  }

  function toggleFold(id) {
    L.folded = L.folded.includes(id) ? L.folded.filter((x) => x !== id) : [...L.folded, id];
    commit();
  }
  function openZone(z) {
    if (side(z) && narrow()) { peek.add(z); render(); return; }
    L.zones[z].collapsed = false;
    commit();
  }
  function closeZone(z) {
    if (side(z) && narrow()) { peek.delete(z); render(); return; }
    L.zones[z].collapsed = true;
    commit();
  }
  /** 收起／打開一整區；open 沒給就反過來 */
  function toggleZone(z, open) {
    if (!ZONES.includes(z)) return;
    if (open ?? collapsedNow(z)) openZone(z); else closeZone(z);
  }
  function reset() {
    closeCaps();
    store.set(key, null);
    L = fixedLayout(spec, normalizeFixed(null, spec));
    peek.clear();
    render();
  }

  new ResizeObserver(() => apply()).observe(page);
  let wasNarrow = narrow();
  addEventListener("resize", () => { if (narrow() !== wasNarrow) { wasNarrow = narrow(); peek.clear(); render(); } });
  render();
  refreshCaps();
  const api = { reset, refreshCaps, openCap, closeCaps, layout: () => copy(L), toggleZone, zoneOpen: (z) => !collapsedNow(z) };
  docks.set(spec.name, api);
  return api;
}

// 中央舞台第三～五期的共用外框：頁首列的「專案膠囊」、ⓘ 說明。
// 只搬元素位置，不換節點：原本用 id 綁的事件、各頁 js 重畫 <select> 選項都照舊。
import { $$, el } from "./util.js";

/** 膠囊上顯示的字：去空白、空的回 —、太長截斷 */
export function pillText(text, max = 16) {
  const s = String(text ?? "").trim();
  if (!s) return "—";
  const chars = Array.from(s);
  return chars.length > max ? chars.slice(0, max - 1).join("") + "…" : s;
}

let openPop = null;
function closeOpen() {
  if (!openPop) return;
  const { pop, btn } = openPop;
  openPop = null;
  pop.hidden = true;
  btn.setAttribute("aria-expanded", "false");
}
/** 點外面、按 Esc：關掉專案膠囊彈出窗和打開的 ⓘ（main.js 啟動時呼叫一次） */
export function initPageHead() {
  document.addEventListener("pointerdown", (e) => {
    if (openPop && !openPop.pop.contains(e.target) && !openPop.btn.contains(e.target)) closeOpen();
    for (const d of $$("details.info-tip[open]")) if (!d.contains(e.target)) d.open = false;
  }, true);
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (openPop) { const b = openPop.btn; closeOpen(); b.focus({ preventScroll: true }); }
    for (const d of $$("details.info-tip[open]")) d.open = false;
  });
  // 同時只開一個 ⓘ
  document.addEventListener("toggle", (e) => {
    const d = e.target;
    if (!d.matches?.("details.info-tip") || !d.open) return;
    for (const o of $$("details.info-tip[open]")) if (o !== d) o.open = false;
  }, true);
}

/** 膠囊本體：anchor 原位換成膠囊，contents 搬進彈出窗；getText 算膠囊上的字，watch 變了就重算 */
function makePill(anchor, contents, { label, getText, watch, at = anchor }) {
  const text = el("span", { class: "v" });
  const btn = el("button", { type: "button", class: "tkey proj-btn", "aria-haspopup": "dialog", "aria-expanded": "false", title: label },
    el("small", {}, label), text, el("span", { class: "dd", "aria-hidden": "true" }, "▾"));
  const pop = el("div", { class: "top-pop proj-pop", role: "dialog", "aria-label": label, hidden: true });
  const wrap = el("span", { class: "proj-pill" }, btn, pop);
  at.before(wrap);
  pop.append(...contents);
  const paint = () => { text.textContent = getText(); };
  new MutationObserver(paint).observe(watch, { childList: true, subtree: true, characterData: true, attributes: true });
  btn.onclick = () => {
    if (openPop?.pop === pop) { closeOpen(); return; }
    closeOpen();
    pop.hidden = false;
    btn.setAttribute("aria-expanded", "true");
    openPop = { pop, btn };
    pop.querySelector("select, button:not([disabled])")?.focus({ preventScroll: true });
  };
  // 按彈出窗裡的按鈕（新增、改名、刪除會跳視窗；清單項目會換專案）就先收起來，免得蓋住
  pop.addEventListener("click", (e) => { if (e.target.closest("button")) closeOpen(); });
  paint();
  return { paint, close: closeOpen };
}

/**
 * 專案膠囊：把「選哪個專案」的 <select> 和旁邊的新增／改名／刪除收成一顆膠囊，點開是彈出窗（元素原樣搬，id 不變）。
 * @param {HTMLSelectElement} select
 * @param {Element[]} extras
 * @param {{label: string, max?: number}} opts
 */
export function projPill(select, extras = [], { label, max = 18 } = {}) {
  if (!select || select.closest(".proj-pill")) return null;
  const pill = makePill(select, [select, el("div", { class: "row gap wrap proj-acts" }, ...extras.filter(Boolean))],
    { label, watch: select, getText: () => pillText(select.selectedOptions[0]?.textContent, max) });
  select.addEventListener("change", () => { pill.paint(); closeOpen(); });
  // select.value 被程式直接改（不發 change）時也要跟上
  const desc = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value");
  Object.defineProperty(select, "value", { configurable: true, get() { return desc.get.call(this); }, set(v) { desc.set.call(this, v); pill.paint(); } });
  return pill;
}

/** 同上，但專案是一串按鈕（訓練頁的 #trList，目前那個有 .on） */
export function listPill(list, extras = [], { label, max = 18, at } = {}) {
  if (!list || list.closest(".proj-pill")) return null;
  // 先在要放膠囊的地方做記號：at 可能是等一下要搬進彈出窗的按鈕
  const mark = document.createComment("pill");
  (at || list).before(mark);
  const pill = makePill(list, [el("div", { class: "row gap wrap proj-acts" }, ...extras.filter(Boolean)), list],
    { label, at: mark, watch: list, getText: () => pillText(list.querySelector(".on b, .on")?.textContent, max) });
  mark.remove();
  return pill;
}

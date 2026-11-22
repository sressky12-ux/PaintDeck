// Ctrl+K 找功能（2026-11-23 生成頁左右兩欄一起做；使用者：「有蠻多東西找不到」）
// 搜生成頁現在用得到（沒被模式、模型藏起來）的按鈕、勾選、選單、區塊標題，加上各模式、各膠囊。
// 選了就切模式，或打開那個膠囊／那一區、展開那塊面板，再捲過去閃一下、把焦點放上去。
// 上半是純函式（tests/finder.test.cjs 測），下半是 DOM。
import { $, $$, el } from "./util.js";

/** 一筆的分數：完全相符 > 開頭相符 > 包含 > 只有「在哪」對得上；0＝沒中。不分大小寫 */
export function score(label, q, where = "") {
  const k = q.trim().toLowerCase();
  if (!k) return 1;
  const t = label.toLowerCase();
  if (t === k) return 100;
  if (t.startsWith(k)) return 60;
  if (t.includes(k)) return 30;
  if (where.toLowerCase().includes(k)) return 5;
  return 0;
}

/** 照分數排，同分照原本（畫面上）的順序；沒中的拿掉 */
export function rank(items, q) {
  return items.map((it, i) => ({ it, i, s: score(it.label, q, it.where || "") }))
    .filter((x) => x.s > 0)
    .sort((a, b) => b.s - a.s || a.i - b.i)
    .map((x) => x.it);
}

// ================================================================ DOM
const ZONE = { left: "左欄", right: "右欄", bottom: "", top: "上面" };
const MAX_ROWS = 60;
let dock = null, items = [], shown = [], sel = 0;

const clean = (s) => (s || "").replace(/\s+/g, " ").trim();
const short = (s) => (s.length > 34 ? s.slice(0, 33) + "…" : s);

/** 這個元素現在用不用得到：自己或上面（到面板殼為止）有 hidden 就是被模式、模型藏起來了。膠囊面板的殼本來就藏著，不算 */
function usable(e) {
  for (let n = e; n && n.id !== "page-gen"; n = n.parentElement) {
    if (n.classList?.contains("dock-panel")) return true;
    if (n.hidden) return false;
  }
  return true;
}

/** 在哪：「左欄・模型與輸入」「膠囊・LoRA 卡匣」「提示詞卡」「觀景窗」 */
function whereOf(e) {
  const panel = e.closest(".dock-panel");
  if (!panel) return { where: e.closest(".monitor") ? "觀景窗" : "模式列", panel: null };
  const title = clean(panel.querySelector(".dock-title")?.textContent);
  const zone = panel.closest("[data-zone]")?.dataset.zone;
  return { where: zone ? [ZONE[zone], title].filter(Boolean).join("・") : `膠囊・${title}`, panel: panel.dataset.id };
}

function labelOf(e) {
  if (e.matches(".lbl")) return clean([...e.childNodes].filter((n) => n.nodeType === 3 || n.matches?.("label")).map((n) => n.textContent).join(""));
  if (e.matches("select, input, textarea")) return clean(e.getAttribute("aria-label")) || clean(e.labels?.[0]?.textContent);
  return clean(e.textContent) || clean(e.title) || clean(e.getAttribute("aria-label"));
}

/** 生成頁現在找得到的東西 */
function collect() {
  const out = [], seen = new Set();
  const add = (it) => {
    const key = it.label + "|" + it.where;
    if (!it.label || seen.has(key)) return;
    seen.add(key);
    out.push(it);
  };
  for (const b of $$("#modeSeg [data-mode]")) add({ label: clean(b.textContent), where: "切換模式", kind: "mode", el: b });
  for (const c of $$("#capRow .cap-chip:not([hidden])")) {
    const title = clean($(`.dock-panel[data-id="${c.dataset.cap}"] .dock-title`)?.textContent) || clean(c.textContent);
    add({ label: title, where: "膠囊", kind: "cap", el: c, panel: c.dataset.cap });
  }
  const skip = ".dock-head, .dock-zbar, .dock-rail, .cap-chip, #modeSeg, #viewMoreMenu, #viewMore";
  // 滑桿、數字框、文字框只收有名字的（main.js labelControls 會幫沒名字的補 aria-label）；勾選框收外面那個 label.check
  for (const e of $$("#page-gen :is(button, a.akey, label.check, select, .lbl, input[type=range], input[type=number], input[type=text], textarea)")) {
    if (e.closest(skip) || !usable(e)) continue;
    const label = short(labelOf(e)), at = whereOf(e);
    if (e.matches(".lbl") && at.where.endsWith(label)) continue;   // 區塊標題跟面板同名（LoRA 卡匣）：膠囊那筆就夠了
    add({ label, ...at, kind: "el", el: e });
  }
  return out;
}

function paint() {
  const list = $("#finderList");
  list.replaceChildren(...shown.slice(0, MAX_ROWS).map((it, i) =>
    el("li", { role: "option", class: i === sel ? "on" : "", "aria-selected": String(i === sel), onclick: () => choose(it) },
      el("b", {}, it.label), el("small", {}, it.where))));
  if (!shown.length) list.append(el("li", { class: "none" }, "找不到。試試別的字，例如「反推」「遮罩」「LoRA」「種子」。"));
  list.querySelector(".on")?.scrollIntoView({ block: "nearest" });
}

function filter() {
  shown = rank(items, $("#finderIn").value);
  sel = 0;
  paint();
}

export function open() {
  if (!$("#page-gen").classList.contains("on")) return;
  dock?.closeCaps();
  items = collect();
  $("#finder").hidden = false;
  $("#finderIn").value = "";
  filter();
  $("#finderIn").focus();
}

function close() { $("#finder").hidden = true; }

function choose(it) {
  close();
  if (it.kind === "mode") { it.el.click(); it.el.focus(); return; }
  if (it.panel) dock?.openCap(it.panel);   // 膠囊 → 彈出來；左右區、提示詞卡 → 打開那一區、展開那塊
  if (it.kind === "cap") return;
  setTimeout(() => {
    const e = it.el;
    e.scrollIntoView({ block: "center", behavior: "smooth" });
    e.classList.add("find-flash");
    setTimeout(() => e.classList.remove("find-flash"), 1600);
    const f = e.matches("label") ? e.querySelector("input, select") : e.matches(".lbl") ? null : e;
    f?.focus({ preventScroll: true });
  }, 80);   // 等膠囊彈出、區打開排好
}

export function init(api) {
  dock = api;
  $("#findBtn").onclick = open;
  $("#finder").addEventListener("click", (e) => { if (e.target.id === "finder") close(); });
  $("#finderIn").addEventListener("input", filter);
  $("#finderIn").addEventListener("keydown", (e) => {
    const n = Math.min(shown.length, MAX_ROWS);
    if (e.key === "ArrowDown" && n) { e.preventDefault(); sel = (sel + 1) % n; paint(); }
    else if (e.key === "ArrowUp" && n) { e.preventDefault(); sel = (sel - 1 + n) % n; paint(); }
    else if (e.key === "Enter" && shown[sel]) { e.preventDefault(); choose(shown[sel]); }
    else if (e.key === "Escape") { e.preventDefault(); close(); $("#findBtn").focus(); }
  });
  // 生成頁、沒有視窗開著時才搶 Ctrl+K
  document.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey || e.key.toLowerCase() !== "k") return;
    if (!$("#page-gen").classList.contains("on") || document.querySelector(".modal:not([hidden])")) return;
    e.preventDefault();
    if ($("#finder").hidden) open(); else close();
  });
}

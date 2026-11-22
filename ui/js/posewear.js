// 生成頁「衣姿」抽屜（2026-11-21，設計：docs\superpowers\specs\2026-11-21-posewear-drawer-design.md；後端 server\api_posewear.rs）
// 服裝、姿勢縮圖，拖進提示詞框（英文描述）、構圖控制（姿勢→深度圖）、多圖參考（角色自動設「服裝」「姿勢」）。每格也有同樣功能的按鈕。
// 素材是 ギア 的 AI 服裝／姿勢圖庫：一張一張當生成參考可以；禁止大量下載與轉載。所以縮圖直接連原站（不經後端、不存），
// 原圖只在拖放／按鈕那一下由後端抓那一張。中文、英文提示詞改自 posewear（MIT）。
import { el, api, toast, store, uploadFile } from "./util.js";
import { addTo } from "./taglib.js";
import { app } from "./state.js";
import { planHandoff, handoffPrompt } from "./pw_handoff.js";

export const PW_MIME = "application/x-paintdeck-pw";
/** 姿勢拖進多圖參考用哪張：深度圖（"depth"）或線稿（"pose"）。2026-11-21 實測後定案，見 HANDOFF */
const POSE_REF_LIB = "depth";
const BATCH = 60;
const CC = [["春季", "春"], ["夏季", "夏"], ["秋季", "秋"], ["冬季", "冬"], ["內衣", "內衣"], ["派對禮服", "派對禮服"], ["泳裝", "泳裝"], ["性感", "性感"]];
const SOURCES = [
  ["動畫服裝", "https://ai-costume-anime-gallery.pages.dev/"], ["寫實服裝", "https://ai-costume-gallery.pages.dev/"],
  ["姿勢", "https://ai-pose-gallery.pages.dev/"], ["深度姿勢", "https://ai-pose-depth-gallery.pages.dev/"],
];

let ctx = null;   // gen.js 給的：family()、mode()、control(r)、ref(r, role)
let ui = null;
const data = {};  // lib → {items, stale, fetched, error} 或 Promise
const st = {
  tab: store.get("pw_tab", "costume"), clib: store.get("pw_clib", "anime"),
  cat: store.get("pw_cat", ""), line: store.get("pw_line", ""), pcat: store.get("pw_pcat", ""), q: "", shown: BATCH,
};
const angle = new Map();  // 姿勢組 id → 目前角度的索引
let lastTa = null;
let drag = null;          // 拖動中的 {lib, item, kind, text}
let dropped = false;

const save = () => { for (const k of ["tab", "clib", "cat", "line", "pcat"]) store.set("pw_" + k, st[k]); };
const visible = (x) => x && x.isConnected && x.offsetParent !== null;
const promptBoxes = () => [document.getElementById("prompt"), ...document.querySelectorAll("#promptSecs textarea")].filter(visible);
const ctrlZone = () => { const z = document.getElementById("ctrlDrop"); return visible(z) ? z : null; };
const refZone = () => { const z = document.getElementById("refTray"); return visible(z) ? z : null; };

// ---------------------------------------------------------------- 資料
async function load(lib, refresh = false) {
  if (!refresh && data[lib] && !(data[lib] instanceof Promise)) return data[lib];
  if (!refresh && data[lib]) return data[lib];
  data[lib] = api(`/api/posewear/index?lib=${lib}${refresh ? "&refresh=1" : ""}`, { timeout: 90000 })
    .then((r) => (data[lib] = r))
    .catch((e) => { delete data[lib]; throw e; });
  return data[lib];
}

const curLib = () => (st.tab === "pose" ? "pose" : st.clib);
const poseAngle = (g) => g.angles[Math.min(angle.get(g.id) ?? 0, g.angles.length - 1)];
const poseText = (g) => [g.en, g.line_en, poseAngle(g).en].filter(Boolean).join(", ");

function filtered(items) {
  const q = st.q.trim().toLowerCase();
  const hit = (...xs) => !q || q.split(/\s+/).every((w) => xs.some((x) => (x || "").toLowerCase().includes(w)));
  if (st.tab === "pose") return items.filter((g) => (!st.line || g.line === st.line) && (!st.pcat || g.cat === st.pcat) && hit(g.zh, g.en, g.ja, g.cat, g.line));
  return items.filter((c) => (!st.cat || c.cat === st.cat) && hit(c.title, c.zh, c.en, c.kw, c.cat));
}

// ---------------------------------------------------------------- 用（拖放、按鈕共用）
/** 後端抓一張原圖放進 ComfyUI input → {name, width, height}。
 * 衣姿整頁 ギア 以外的素材（表情、人像、場景…）給 url，後端檢查它在官網資料檔裡才抓 */
export async function fetchUse(lib, id, url) {
  return api("/api/posewear/use", { json: url ? { url } : { lib, id }, timeout: 90000 });
}

function promptTarget() {
  if (visible(lastTa)) return lastTa;
  return promptBoxes()[0] || null;
}

const norm = (s) => s.trim().toLowerCase().replace(/[_\s]+/g, " ");

function addText(text, ta = promptTarget()) {
  if (!ta) return toast("先點一下要放詞的提示詞框", "err");
  // 框裡已經有的詞（例如 solo、full body）不再重複加
  const have = new Set(ta.value.split(/[,，\n]/).map(norm));
  text = text.split(",").map((x) => x.trim()).filter((x) => x && !have.has(norm(x))).join(", ");
  if (!text) return toast("這些詞框裡都有了");
  addTo(ta, text);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
  lastTa = ta;
  toast("已加進提示詞", "ok");
}

const HINT_POSE = "現在看不到構圖控制或多圖參考。Anima、MOSS 在文生圖用構圖控制；Qwen 2.1 切到「多圖參考」模式再拖";
const HINT_COSTUME = "服裝請拖到多圖參考或提示詞框（多圖參考要先切到「多圖參考」模式）";

async function busy(card, fn) {
  if (card?.classList.contains("busy")) return;
  card?.classList.add("busy");
  try { await fn(); } catch (e) { toast(e.message, "err"); } finally { card?.classList.remove("busy"); }
}

async function toCtrl(d, card) {
  if (d.kind !== "pose") return toast("服裝請拖到多圖參考或提示詞框", "err");
  const f = ctx.family();
  if (f !== "anima" && f !== "moss") return toast(HINT_POSE, "err");
  await busy(card, async () => { ctx.control(await fetchUse("depth", d.item)); toast("深度圖放進構圖控制了", "ok"); });
}

async function toRef(d, card) {
  await busy(card, async () => {
    const r = d.url ? await fetchUse(null, null, d.url) : await fetchUse(d.kind === "pose" ? POSE_REF_LIB : d.lib, d.item);
    const role = d.role || (d.kind === "pose" ? "pose" : "outfit");
    const msg = d.kind === "pose" ? "姿勢放進多圖參考了" : d.kind === "costume" ? "服裝放進多圖參考了" : "放進多圖參考了";
    if (ctx.ref(r, role) != null) toast(msg, "ok");
  });
}

// ---------------------------------------------------------------- 衣姿整頁（ui\posewear\，iframe）
/** 第一次切到衣姿頁才載入；繁體 index.html、簡體 index.s.html，照頁面裡「繁／簡」存的選擇（同一個網域，localStorage 共用） */
export function showPage() {
  const fr = document.getElementById("pwFrame");
  if (!fr || fr.dataset.on) return;
  let zh = "t";
  try { zh = localStorage.getItem("pw-zh") || "t"; } catch { /* 讀不到就用繁體 */ }
  fr.src = `/static/posewear/${zh === "s" ? "index.s.html" : "index.html"}`;
  fr.dataset.on = "1";
}

// 試穿工作台「➜ 交給繪匣」交出的整套（bridge.js：{pw:"bundle", en, zh, pl, custom, items:[{blob, role, name}]}）
// → 切到能做多圖參考的引擎、換掉參考匣、逐張上傳（角色照對照表）、填提示詞。2026-11-22，設計第四節
let handing = false;
async function onBundle(m) {
  if (handing) return toast("上一套還在放，等一下");
  const h = ctx.handoff;
  const items = Array.isArray(m.items) ? m.items : [];
  if (items.some((x) => !(x?.blob instanceof Blob))) return toast("衣姿送來的圖有缺，整套沒有放進去，請在工作台再按一次", "err");
  const p = planHandoff({ family: ctx.family(), hasMmimage: h.hasFamily("mmimage"), refsNow: h.refsNow(), items });
  if (!p.ok) return toast(p.reason, "err");
  if (p.replace && !confirm("換掉現在的參考圖？")) return;
  app.showPage("gen");
  if (!h.prepare(p.family)) return toast(p.family === "mmimage" ? "找不到 MiniMax H3 Image 模型" : "找不到 Qwen 2.1 模型：到下載中心或安裝精靈裝「Qwen 2.1 編輯」", "err");
  handing = true;
  try {
    for (const [i, x] of items.entries()) {
      try {
        const file = new File([x.blob], String(x.name || `pw${i + 1}.jpg`), { type: x.blob.type || "image/jpeg" });
        if (h.add(await uploadFile(file), p.roles[i]) == null) throw new Error("參考匣放不下");
      } catch (e) {
        h.done();
        return toast(`第 ${i + 1} 張上傳失敗：${e.message}（前面 ${i} 張已經放進去了）`, "err");
      }
    }
    h.done();
    h.setPrompt(handoffPrompt({ en: String(m.en || ""), zh: String(m.zh || ""), pl: m.pl, custom: String(m.custom || "") }, p.family));
    toast(`衣姿的 ${items.length} 張已經放好，看過再按生成`, "ok");
  } finally {
    handing = false;
  }
}

// 送過來的「送到繪匣」
// bridge.js：{pw:"use", act:"prompt"|"ref"|"ctrl", lib, id | url, text, kind, role}；{pw:"bundle", …} 見 onBundle
function onPageMsg(e) {
  const fr = document.getElementById("pwFrame");
  if (e.origin !== location.origin || !fr || e.source !== fr.contentWindow) return;
  const m = e.data;
  if (m && m.pw === "bundle") return void onBundle(m);
  if (!m || m.pw !== "use" || !["prompt", "ref", "ctrl"].includes(m.act)) return;
  const d = { lib: m.lib, item: m.id, url: m.url, kind: m.kind, role: m.role, text: String(m.text || "") };
  app.showPage("gen");
  if (m.act === "prompt") addText(d.text);
  else if (m.act === "ctrl") toCtrl(d, null);
  else toRef(d, null);
}

// ---------------------------------------------------------------- 拖放（整頁攔截：capture 階段先處理，不讓原生的文字插入、檔案拖放那幾條接到）
function zoneAt(target) {
  if (!target?.closest) return null;
  const ta = target.closest("textarea");
  if (ta && promptBoxes().includes(ta)) return { kind: "prompt", el: ta };
  const c = target.closest("#ctrlDrop");
  if (c && visible(c)) return { kind: "ctrl", el: c };
  const r = target.closest("#refTray");
  if (r && visible(r)) return { kind: "ref", el: r };
  return null;
}

function clearOver() { for (const x of document.querySelectorAll(".pw-over, .pw-target")) x.classList.remove("pw-over", "pw-target"); }

function bindDrops() {
  document.addEventListener("dragover", (e) => {
    if (!drag) return;
    const z = zoneAt(e.target);
    for (const x of document.querySelectorAll(".pw-over")) if (x !== z?.el) x.classList.remove("pw-over");
    if (!z) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = "copy";
    z.el.classList.add("pw-over");
  }, true);
  document.addEventListener("drop", (e) => {
    if (!drag) return;
    const z = zoneAt(e.target);
    if (!z) return;
    e.preventDefault();
    e.stopPropagation();
    dropped = true;
    const d = drag, card = ui?.grid.querySelector(`[data-item="${CSS.escape(d.item)}"]`);
    clearOver();
    if (z.kind === "prompt") addText(d.text, z.el);
    else if (z.kind === "ctrl") toCtrl(d, card);
    else toRef(d, card);
  }, true);
}

function dragStart(e, d, img) {
  drag = d; dropped = false;
  e.dataTransfer.effectAllowed = "copy";
  e.dataTransfer.setData(PW_MIME, JSON.stringify(d));
  e.dataTransfer.setData("text/plain", d.text);
  if (img?.complete && img.naturalWidth) e.dataTransfer.setDragImage(img, img.width / 2, img.height / 2);
  for (const z of [...promptBoxes(), d.kind === "pose" ? ctrlZone() : null, refZone()]) z?.classList.add("pw-target");
}

function dragEnd() {
  const d = drag;
  drag = null;
  clearOver();
  if (!d || dropped) return;
  // 放開的地方不收：這個家族／模式根本沒有能放圖的框，就講清楚該怎麼做
  if (d.kind === "pose" && !ctrlZone() && !refZone()) toast(HINT_POSE);
  else if (d.kind === "costume" && !refZone()) toast(HINT_COSTUME);
}

// ---------------------------------------------------------------- 畫面
function chip(label, on, onclick) {
  return el("button", { type: "button", class: "tl-tab" + (on ? " on" : ""), onclick }, label);
}

function costumeCard(c) {
  const d = { lib: st.clib, item: c.id, kind: "costume", text: c.en };
  const img = el("img", { src: c.thumb, alt: "", loading: "lazy", referrerpolicy: "no-referrer", draggable: "false" });
  const card = el("div", { class: "pw-card", draggable: "true", "data-item": c.id, title: `${c.zh}\n${c.en}\n拖到提示詞框、多圖參考`,
    ondragstart: (e) => dragStart(e, d, img), ondragend: dragEnd },
  img,
  el("div", { class: "pw-t" }, c.title || c.id),
  el("div", { class: "pw-s", translate: "no" }, c.en),
  el("div", { class: "pw-btns" },
    el("button", { type: "button", class: "minikey", title: "把英文描述加進提示詞框", onclick: () => addText(d.text) }, "＋詞"),
    el("button", { type: "button", class: "minikey", title: "加進多圖參考（角色：服裝）", onclick: () => toRef(d, card) }, "參考")));
  return card;
}

function poseCard(g) {
  const a = poseAngle(g);
  const d = { lib: "pose", item: a.item, kind: "pose", text: poseText(g) };
  const img = el("img", { src: a.thumb, alt: "", loading: "lazy", referrerpolicy: "no-referrer", draggable: "false" });
  const card = el("div", { class: "pw-card", draggable: "true", "data-item": a.item, title: `${g.zh}（${a.zh}）\n${d.text}\n拖到提示詞框、構圖控制、多圖參考`,
    ondragstart: (e) => dragStart(e, d, img), ondragend: dragEnd },
  img,
  el("div", { class: "pw-t" }, g.zh),
  el("div", { class: "pw-s" }, `${g.line}・${g.cat}`),
  el("div", { class: "pw-angles" }, ...g.angles.map((x, i) => el("button", {
    type: "button", class: "pw-ang" + (x === a ? " on" : ""), title: x.zh,
    onclick: () => { angle.set(g.id, i); card.replaceWith(poseCard(g)); },
  }, x.zh.slice(0, 1)))),
  el("div", { class: "pw-btns" },
    el("button", { type: "button", class: "minikey", title: "把英文姿勢描述加進提示詞框", onclick: () => addText(d.text) }, "＋詞"),
    el("button", { type: "button", class: "minikey", title: "加進多圖參考（角色：姿勢）", onclick: () => toRef(d, card) }, "參考"),
    el("button", { type: "button", class: "minikey", title: "深度圖放進構圖控制（Anima、MOSS）", onclick: () => toCtrl(d, card) }, "控制")));
  return card;
}

function filters(items) {
  const f = ui.filters;
  f.replaceChildren();
  if (st.tab === "costume") {
    f.append(el("div", { class: "tl-tabs" },
      chip("動畫", st.clib === "anime", () => { st.clib = "anime"; st.shown = BATCH; save(); render(); }),
      chip("寫實", st.clib === "costume", () => { st.clib = "costume"; st.shown = BATCH; save(); render(); })),
    el("div", { class: "tl-tabs" }, chip("全部", !st.cat, () => { st.cat = ""; st.shown = BATCH; save(); paint(); }),
      ...CC.map(([v, label]) => chip(label, st.cat === v, () => { st.cat = v; st.shown = BATCH; save(); paint(); }))));
    return;
  }
  const uniq = (k) => [...new Set(items.map((g) => g[k]).filter(Boolean))];
  f.append(
    el("div", { class: "tl-tabs" }, chip("全部", !st.line, () => { st.line = ""; st.shown = BATCH; save(); paint(); }),
      ...uniq("line").map((v) => chip(v, st.line === v, () => { st.line = v; st.shown = BATCH; save(); paint(); }))),
    el("div", { class: "tl-tabs pw-cats" }, chip("全部", !st.pcat, () => { st.pcat = ""; st.shown = BATCH; save(); paint(); }),
      ...uniq("cat").map((v) => chip(v, st.pcat === v, () => { st.pcat = v; st.shown = BATCH; save(); paint(); }))));
}

/** 畫格子（篩選、搜尋改了才重畫；捲到底再加一批） */
function paint(more = false) {
  const r = data[curLib()];
  if (!r || r instanceof Promise) return;
  if (!more) filters(r.items);
  const list = filtered(r.items);
  const g = ui.grid;
  const from = more ? g.childElementCount : 0;
  if (!more) { g.replaceChildren(); g.scrollTop = 0; }
  for (const x of list.slice(from, st.shown)) g.append(st.tab === "pose" ? poseCard(x) : costumeCard(x));
  const age = r.stale ? `・目錄是 ${Math.max(1, Math.round((Date.now() / 1000 - r.fetched) / 86400))} 天前的（連不上原站）` : "";
  ui.info.textContent = `${list.length.toLocaleString()} ${st.tab === "pose" ? "組" : "套"}${age}・拖到提示詞框、構圖控制或多圖參考，或用每格的按鈕`;
  ui.info.classList.toggle("warn", !!r.stale);
}

async function render(refresh = false) {
  for (const b of ui.tabs.children) b.classList.toggle("on", b.dataset.tab === st.tab);
  const lib = curLib();
  if (!data[lib] || data[lib] instanceof Promise || refresh) {
    ui.grid.replaceChildren(el("div", { class: "muted sm pw-msg" }, "讀取目錄中…（第一次要連原站）"));
    ui.filters.replaceChildren();
    try { await load(lib, refresh); } catch (e) {
      ui.grid.replaceChildren(el("div", { class: "pw-msg err" }, e.message));
      ui.info.textContent = "";
      return;
    }
    if (curLib() !== lib) return;  // 讀的時候切走了
  }
  st.shown = Math.max(st.shown, BATCH);
  paint();
}

function build() {
  const tabs = el("div", { class: "tl-tabs" },
    el("button", { type: "button", class: "tl-tab", "data-tab": "costume", onclick: () => { st.tab = "costume"; st.shown = BATCH; save(); render(); } }, "服裝"),
    el("button", { type: "button", class: "tl-tab", "data-tab": "pose", onclick: () => { st.tab = "pose"; st.shown = BATCH; save(); render(); } }, "姿勢"));
  const q = el("input", { type: "search", class: "pw-q", placeholder: "搜尋（中、英、日都可以）", "aria-label": "搜尋衣姿",
    oninput: () => { st.q = q.value; st.shown = BATCH; paint(); } });
  const grid = el("div", { class: "pw-grid" });
  grid.addEventListener("scroll", () => {
    if (grid.scrollTop + grid.clientHeight > grid.scrollHeight - 300) {
      const r = data[curLib()];
      if (r && !(r instanceof Promise) && st.shown < filtered(r.items).length) { st.shown += BATCH; paint(true); }
    }
  });
  const grip = el("div", { class: "pw-grip", title: "拖這裡調寬度" });
  const box = el("aside", { class: "pw", id: "pwDrawer", hidden: true, "aria-label": "衣姿：服裝、姿勢參考" },
    grip,
    el("div", { class: "pw-head" }, el("b", {}, "衣姿"), tabs, q,
      el("button", { type: "button", class: "minikey", title: "開衣姿整頁（跟 posewear 官網一樣的大格子、12 個分類）", onclick: () => { toggle(false); app.showPage("posewear"); } }, "開整頁"),
      el("button", { type: "button", class: "icon", title: "重新抓目錄（平常 7 天自動更新一次）", "aria-label": "重新抓目錄", onclick: () => render(true) }, "↻"),
      el("button", { type: "button", class: "icon", title: "關閉（Esc）", "aria-label": "關閉衣姿", onclick: () => toggle(false) }, "✕")),
    el("div", { class: "pw-filters" }),
    el("div", { class: "muted sm pw-info" }),
    grid,
    el("div", { class: "pw-foot muted sm" },
      "素材：ギア 的 AI 圖庫（",
      ...SOURCES.flatMap(([n, u], i) => [i ? "、" : "", el("a", { href: u, target: "_blank", rel: "noopener" }, n)]),
      "）。一張一張當參考可以商用；禁止大量下載與轉載。中文、提示詞改自 ",
      el("a", { href: "https://github.com/atuizz/posewear", target: "_blank", rel: "noopener" }, "posewear"), "（MIT）。"));
  document.body.append(box);
  grip.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    const move = (ev) => { box.style.width = `${clampW(innerWidth - ev.clientX)}px`; };
    const up = () => { grip.removeEventListener("pointermove", move); store.set("pw_w", parseInt(box.style.width, 10)); };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", up, { once: true });
  });
  return { box, tabs, q, grid, filters: box.querySelector(".pw-filters"), info: box.querySelector(".pw-info") };
}

/** 寬度：最小 300、最大視窗 70%（視窗還沒有寬度時不夾上限） */
const clampW = (w) => Math.max(300, Math.min(w, Math.round(innerWidth * 0.7) || w));

function toggle(on = ui?.box.hidden ?? true) {
  ui ||= build();
  ui.box.hidden = !on;
  if (on) ui.box.style.width = `${clampW(store.get("pw_w", 460))}px`;
  ctx.btn?.classList.toggle("on", on);
  ctx.btn?.setAttribute("aria-pressed", String(on));
  if (on) render();
}

/** gen.js 掛上：{btn, family(), control(r), ref(r, role),
 *  handoff: {hasFamily(f), refsNow(), prepare(f)→bool（切模型、多圖參考模式、清空參考匣）, add(r, role), done(), setPrompt(t)}} */
export function init(c) {
  ctx = c;
  c.btn.addEventListener("click", () => toggle());
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && ui && !ui.box.hidden && !document.querySelector("dialog[open], .modal:not([hidden])")) toggle(false); });
  document.addEventListener("focusin", (e) => { if (e.target?.tagName === "TEXTAREA" && promptBoxes().includes(e.target)) lastTa = e.target; });
  bindDrops();
  window.addEventListener("message", onPageMsg);
}

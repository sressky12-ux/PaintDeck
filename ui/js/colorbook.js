// 整本上色（漫畫台的第二個分頁）：整本黑白漫畫（zip 或一堆頁面圖）丟進來 → 35B（或選 9B）看彩頁寫角色配色表、看每一頁寫上色指令
// → Qwen 2.1 一頁一頁上色 → 原稿亮度回貼（線條、字用原稿）→ 這裡看前後對照、下載整本
// 後端在 src-tauri\src\colorbook.rs（純邏輯）、server\api_colorbook.rs（API、背景工作）；websocket type=colorbook
// 研究與設計：design\整本上色_設計.md
import { api, el, toast, store, openModal } from "./util.js";

const F = (id) => document.getElementById(id);
let tab = "draw";      // 漫畫台目前的分頁：draw（畫漫畫）｜color（整本上色）
let B = null;          // 目前這本（book.json）
let running = false;
let view = -1;         // 對照視窗看的頁（pages 的索引）
const openPrompts = new Set();   // 展開著「上色指令」的頁碼（重畫卡片時保持展開）

const KIND = { color: "彩頁", bw: "黑白", blank: "跳過" };
const BY = { ai: "AI 看這頁寫的", user: "你改過的", plain: "通用指令（沒有看圖）", "": "還沒寫（開始上色時 AI 會寫）" };
const STATUS = { todo: "還沒上", done: "上好了", error: "失敗" };
const pad = (n) => String(n).padStart(4, "0");
// 原頁和縮圖不會變；成品用 tries（第幾次上色）當版本，重上之後換新圖
const src = (dir, name, v = 0) => `/api/colorbook/${B.id}/file/${dir}/${encodeURIComponent(name)}?v=${v}`;

// ---------------------------------------------------------------- 分頁
function setTab(t) {
  tab = t === "color" ? "color" : "draw";
  store.set("cmTab", tab);
  document.querySelectorAll("#cmTabs button").forEach((b) => { const on = b.dataset.t === tab; b.classList.toggle("on", on); b.setAttribute("aria-selected", String(on)); });
  document.querySelectorAll("#page-comic > section[data-tab]").forEach((s) => (s.hidden = s.dataset.tab !== tab));
  if (tab === "color") show();
}

/** 漫畫台打開時（main.js）：在整本上色分頁才讀 */
export async function show() {
  if (tab !== "color") return;
  await loadBooks(B?.id || store.get("cbBook", null));
}

// ---------------------------------------------------------------- 書
async function loadBooks(want) {
  let r;
  try { r = await api("/api/colorbook"); } catch (e) { return toast("讀不到整本上色的清單：" + e.message, "err"); }
  const sel = F("cbBook");
  sel.replaceChildren(...r.books.map((b) => el("option", { value: b.id }, `${b.title}（${b.done}／${b.bw} 頁）`)));
  if (!r.books.length) sel.append(el("option", { value: "" }, "（還沒有書：把整本丟進來）"));
  const id = r.books.some((b) => b.id === want) ? want : r.books[0]?.id || "";
  sel.value = id;
  await openBook(id);
}

async function openBook(id) {
  store.set("cbBook", id);
  B = null;
  if (id) {
    try { B = await api(`/api/colorbook/${id}`); } catch (e) { toast(e.message, "err"); }
  }
  setRunning(!!B?.running);
  render();
}

async function reload() {
  if (!B) return;
  try { B = await api(`/api/colorbook/${B.id}`); } catch { return; }
  render();
}

async function upload(list) {
  const files = [...list].filter((f) => /\.(zip|cbz|png|jpe?g|webp)$/i.test(f.name));
  if (!files.length) return toast("收 zip（或 cbz）、PNG、JPG、WebP；rar、7z 請先解開或重新壓成 zip", "err");
  const fd = new FormData();
  for (const f of files) fd.append("file", f, f.name);
  status("讀取、分頁中…", "amb");
  try {
    const b = await api("/api/colorbook/new", { method: "POST", body: fd, timeout: 0 });
    const n = (k) => b.pages.filter((p) => p.kind === k).length;
    toast(`建好了「${b.title}」：共 ${b.pages.length} 頁（彩頁 ${n("color")}、黑白 ${n("bw")}、空白 ${n("blank")}）`, "ok");
    await loadBooks(b.id);
  } catch (e) { toast(e.message, "err"); }
  status(running ? "上色中…" : "待機", running ? "amb" : "g");
}

// ---------------------------------------------------------------- 畫面
function status(msg, cls = "g") {
  const s = F("cbStatus");
  s.textContent = msg;
  s.className = `vfd ${cls} dir-status`;
}

function setRunning(on) {
  running = on;
  F("cbRun").hidden = on;
  F("cbStop").hidden = !on;
  for (const id of ["cbSheetAi", "cbSheetSave", "cbDel"]) F(id).disabled = on || !B;
  // 中途打開正在跑的書：下一個進度事件來之前先顯示「上色中」
  if (on && F("cbStatus").textContent === "待機") status("上色中…", "amb");
  if (!on && F("cbStatus").textContent.endsWith("…")) status("待機");
}

function render() {
  const pages = B?.pages || [];
  const n = (k) => pages.filter((p) => p.kind === k).length;
  const done = pages.filter((p) => p.kind === "bw" && p.status === "done").length;
  const failed = pages.filter((p) => p.kind === "bw" && p.status === "error").length;
  F("cbInfo").textContent = B
    ? `「${B.title}」共 ${pages.length} 頁：彩頁 ${n("color")}、黑白 ${n("bw")}、跳過 ${n("blank")}。上好 ${done}／${n("bw")} 頁${failed ? `，失敗 ${failed} 頁` : ""}。`
    : "還沒有書。";
  F("cbCount").textContent = B ? `上好 ${done}／${n("bw")} 頁` : "";
  if (document.activeElement !== F("cbSheet")) F("cbSheet").value = B?.sheet || "";
  for (const id of ["cbRun", "cbZip", "cbSheetAi", "cbSheetSave", "cbDel"]) F(id).disabled = !B || (running && id !== "cbZip");
  const box = F("cbPages");
  const old = new Map([...box.children].map((c) => [c.dataset.n, c]));
  box.replaceChildren(...pages.map((p, i) => {
    // 正在改指令的那張卡不換掉（不然打到一半的字會不見）
    const keep = old.get(String(p.n));
    return keep?.contains(document.activeElement) ? keep : card(p, i);
  }));
  if (!F("cbView").hidden && view >= 0) renderView();
}

function card(p, i) {
  const done = p.kind === "bw" && p.status === "done" && p.out;
  const imgs = el("div", { class: "cb-imgs" + (done ? "" : " one"), title: "點開看前後對照", onclick: () => openView(i) });
  if (p.file) imgs.append(el("img", { src: src("thumb", pad(p.n) + ".jpg"), alt: `第 ${p.n} 頁`, loading: "lazy" }));
  if (done) imgs.append(el("img", { src: src("othumb", pad(p.n) + ".jpg", p.tries), alt: `第 ${p.n} 頁（上色）`, loading: "lazy" }));
  const kind = el("select", { "aria-label": "這頁是", disabled: running || !p.file }, ...Object.entries(KIND).map(([k, v]) => el("option", { value: k }, v)));
  kind.value = p.kind;
  kind.onchange = () => setPage(p.n, { kind: kind.value });
  const st = p.kind === "bw" ? STATUS[p.status] || p.status : p.kind === "color" ? "配色參考" : "不上色";
  const head = el("div", { class: "row gap" }, el("b", {}, `第 ${p.n} 頁`), kind, el("span", { class: "grow" }), el("span", { class: "muted sm" }, st));
  const c = el("div", { class: `cb-page ${p.kind} ${p.status}`, "data-n": p.n }, imgs, head);
  if (p.error) c.append(el("div", { class: "sm warn-line" }, p.error));
  if (p.kind === "bw") {
    const ta = el("textarea", { rows: 6, spellcheck: "false", translate: "no", placeholder: "空白＝開始上色時讓 AI 看這頁寫" });
    ta.value = p.prompt || "";
    const det = el("details", { class: "cb-pr" }, el("summary", {}, `上色指令：${BY[p.prompt_by] ?? p.prompt_by}`), ta,
      el("div", { class: "row gap wrap" },
        el("button", { class: "akey sm", type: "button", onclick: () => setPage(p.n, { prompt: ta.value }) }, "存指令"),
        el("button", { class: "akey sm", type: "button", title: "清掉這頁的指令，下次上色時讓 AI 重寫", onclick: () => setPage(p.n, { prompt: "" }) }, "讓 AI 重寫")));
    det.open = openPrompts.has(p.n);
    det.ontoggle = () => (det.open ? openPrompts.add(p.n) : openPrompts.delete(p.n));
    c.append(det, el("div", { class: "row gap" },
      el("button", { class: "akey sm", type: "button", disabled: running, onclick: () => runBook([p.n]) }, done ? "重上這頁" : "只上這頁")));
  }
  return c;
}

async function setPage(n, body) {
  try { B = await api(`/api/colorbook/${B.id}/page`, { json: { i: n, ...body } }); render(); toast(`第 ${n} 頁已存`, "ok"); } catch (e) { toast(e.message, "err"); }
}

// ---------------------------------------------------------------- 前後對照
function openView(i) { view = i; renderView(); openModal("#cbView"); }

function renderView() {
  const p = B?.pages[view];
  if (!p) return;
  const done = p.kind === "bw" && p.status === "done" && p.out;
  F("cbViewTitle").textContent = `第 ${p.n} 頁／共 ${B.pages.length} 頁`;
  F("cbViewInfo").textContent = p.kind === "bw" ? (done ? `第 ${p.tries} 次上色` : STATUS[p.status] || "") : KIND[p.kind];
  F("cbViewRedo").hidden = p.kind !== "bw";
  F("cbViewRedo").disabled = running;
  F("cbViewImgs").classList.toggle("one", !done);
  F("cbViewImgs").replaceChildren(
    ...(p.file ? [el("figure", {}, el("img", { src: src("src", p.file), alt: "原稿" }), el("figcaption", { class: "muted sm" }, "原稿"))] : []),
    ...(done ? [el("figure", {}, el("img", { src: src("out", p.out, p.tries), alt: "上色後" }), el("figcaption", { class: "muted sm" }, "上色後"))] : []));
}

function stepView(d) {
  if (!B?.pages.length) return;
  view = (view + d + B.pages.length) % B.pages.length;
  renderView();
}

// ---------------------------------------------------------------- 上色
async function runBook(pages) {
  if (!B || running) return;
  // 狀態先寫：後端的進度事件可能比這個請求的回應先到
  status("開始上色…", "amb");
  try {
    const r = await api(`/api/colorbook/${B.id}/run`, { json: { pages: pages || [], redo: !pages && F("cbRedo").checked, ai: F("cbAi").checked, brain: F("cbBrain").value, keep_lines: F("cbKeep").checked } });
    setRunning(true);
    render();
    toast(`開始上色：${r.pages} 頁`, "ok");
  } catch (e) { status("待機"); toast(e.message, "err"); }
}

async function download() {
  if (!B) return;
  toast("打包整本中…");
  try {
    const r = await fetch(`/api/colorbook/${B.id}/zip`);
    if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || `HTTP ${r.status}`);
    const url = URL.createObjectURL(await r.blob());
    const name = `${B.title.replace(/[\\/:*?"<>|]/g, "_")}_上色.zip`;
    el("a", { href: url, download: name }).click();
    setTimeout(() => URL.revokeObjectURL(url), 60000);
    toast(`已下載 ${name}`, "ok");
  } catch (e) { toast("下載失敗：" + e.message, "err"); }
}

/** websocket：main.js 收到 type=colorbook 轉給這裡 */
export function onEvent(m) {
  if (!B || m.id !== B.id) return;
  if (m.phase === "end" || m.phase === "error") {
    setRunning(false);
    status(m.phase === "end" ? "完成" : "失敗", m.phase === "end" ? "g" : "r");
    toast(`整本上色：${m.msg}`, m.phase === "end" ? "ok" : "err");
    reload();
    return;
  }
  status(m.msg || m.phase, "amb");
  if (m.phase === "page") reload();
}

// ---------------------------------------------------------------- 初始化
export function init() {
  F("cmTabs").addEventListener("click", (e) => { const b = e.target.closest("button[data-t]"); if (b) setTab(b.dataset.t); });
  setTab(store.get("cmTab", "draw"));
  F("cbBook").onchange = () => openBook(F("cbBook").value);
  F("cbBrain").value = store.get("cbBrain", "35b") === "9b" ? "9b" : "35b";
  F("cbBrain").onchange = () => store.set("cbBrain", F("cbBrain").value);
  F("cbNew").onclick = () => F("cbFile").click();
  F("cbFile").onchange = () => { const f = F("cbFile").files; if (f.length) upload(f); F("cbFile").value = ""; };
  const drop = F("cbDrop");
  drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => { e.preventDefault(); drop.classList.remove("over"); upload(e.dataTransfer.files); });
  drop.onclick = () => F("cbFile").click();
  F("cbDel").onclick = async () => {
    if (!B || !confirm(`把「${B.title}」整本（原頁和上好色的頁）移到待刪除？`)) return;
    try { await api(`/api/colorbook/${B.id}/delete`, { json: {} }); B = null; await loadBooks(null); } catch (e) { toast(e.message, "err"); }
  };
  F("cbSheetAi").onclick = async () => {
    if (!B) return;
    if (F("cbSheet").value.trim() && !confirm("AI 重寫會蓋掉現在的配色表。繼續？")) return;
    status("AI 看彩頁寫配色表…", "amb");
    F("cbSheetAi").disabled = true;
    try { B = await api(`/api/colorbook/${B.id}/sheet`, { json: { auto: true, brain: F("cbBrain").value }, timeout: 0 }); F("cbSheet").blur(); render(); toast("配色表寫好了：看一下有沒有認錯人", "ok"); } catch (e) { toast(e.message, "err"); }
    F("cbSheetAi").disabled = false;
    status("待機");
  };
  F("cbSheetSave").onclick = async () => {
    if (!B) return;
    try { B = await api(`/api/colorbook/${B.id}/sheet`, { json: { text: F("cbSheet").value } }); toast("配色表已存", "ok"); } catch (e) { toast(e.message, "err"); }
  };
  F("cbRun").onclick = () => runBook(null);
  F("cbStop").onclick = async () => { try { await api(`/api/colorbook/${B.id}/stop`, { json: {} }); status("停止中…", "amb"); } catch (e) { toast(e.message, "err"); } };
  F("cbZip").onclick = download;
  F("cbViewPrev").onclick = () => stepView(-1);
  F("cbViewNext").onclick = () => stepView(1);
  F("cbViewRedo").onclick = () => { const p = B?.pages[view]; if (p) runBook([p.n]); };
  document.addEventListener("keydown", (e) => {
    if (F("cbView").hidden || e.target.closest("input, textarea, select")) return;
    if (e.key === "ArrowLeft") stepView(-1);
    if (e.key === "ArrowRight") stepView(1);
  });
}

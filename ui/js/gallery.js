// 圖庫：翻頁檢視（每頁 10／30／50／100 張）、篩選搜尋、多選批次（收藏、回收區、匯出 zip、比較）、回收區、燈箱（含參數、動作、快捷鍵）、從圖庫挑圖
import { $, $$, el, api, toast, debounce, openModal, closeModal, modeName, store, copyText, typing, isVideo, IMAGE_ONLY_ACTS, VIDEO_ONLY_ACTS, fmtSecs } from "./util.js";
import { app } from "./state.js";

let items = [], total = 0, loaded = false, reqSeq = 0;
let page = 1;                     // 目前第幾頁（從 1 開始）；每頁幾張看 #galPer
let view = "all";                 // all／fav／trash
let lb = { list: [], i: 0, gal: false };  // gal：從圖庫開的，翻到頭會接著翻上／下一頁
const sel = new Set();            // 選取的圖庫編號（換頁也留著）
const known = new Map();          // 看過的圖的紀錄（跨頁選取時 A／B 比較要用）
let selMode = false, lastClicked = null;

const F = (id) => $("#" + id);
const FILTER_IDS = ["galMode", "galFamily", "galModel", "galLora", "galDate", "galFrom", "galTo", "galSort", "galSize", "galPer"];
const per = () => +F("galPer").value || 50;
const pages = () => Math.max(1, Math.ceil(total / per()));

export function init() {
  F("galFilter").onclick = (e) => {
    const b = e.target.closest("button"); if (!b) return;
    view = b.dataset.f;
    $$("#galFilter button").forEach((x) => x.classList.toggle("on", x === b));
    clearSel();
    reload();
  };
  F("galQ").oninput = debounce(reload, 300);
  F("galFiltToggle").onclick = () => {
    F("galFilters").hidden = !F("galFilters").hidden;
    F("galFiltToggle").setAttribute("aria-expanded", String(!F("galFilters").hidden));
    saveFilters();
  };
  for (const id of FILTER_IDS) F(id).addEventListener("change", () => {
    if (id === "galDate") F("galDateCustom").hidden = F("galDate").value !== "custom";
    if (id === "galSize") applySize();
    saveFilters();
    if (id === "galPer") goPage(1);
    else if (id !== "galSize") reload();
  });
  F("galClearF").onclick = () => {
    for (const id of ["galMode", "galFamily", "galModel", "galLora", "galDate", "galFrom", "galTo"]) F(id).value = "";
    F("galSort").value = "new"; F("galDateCustom").hidden = true; F("galQ").value = "";
    saveFilters(); reload();
  };
  F("galRepair").onclick = async () => {
    if (!confirm("生圖站找不到檔案的圖，直接跟 ComfyUI 要回來？\n（圖還在 ComfyUI 手上就救得回來；ComfyUI 要開著）")) return;
    const r = await api("/api/gallery/repair_missing", { json: {} });
    if (!r.missing) return toast("圖庫的檔案都在，不用修", "ok");
    if (r.repaired) {
      toast(r.still ? `救回 ${r.repaired} 張；還有 ${r.still} 張 ComfyUI 手上也沒有` : `救回 ${r.repaired} 張`, r.still ? "err" : "ok");
    } else {
      toast(`救不回來：${r.error || "ComfyUI 手上沒有這些圖"}`, "err");
    }
    reload();
  };
  F("galPrune").onclick = async () => {
    if (!confirm("把「檔案已經不在硬碟上」的圖從圖庫拿掉？\n（只清圖庫紀錄，不會動任何檔案）")) return;
    const r = await api("/api/gallery/prune_missing", { json: {} });
    toast(r.count ? `已清掉 ${r.count} 筆遺失的紀錄` : "沒有遺失的紀錄", "ok");
    reload();
  };
  F("galEmpty").onclick = emptyTrash;
  F("galPrev").onclick = () => goPage(page - 1);
  F("galNext").onclick = () => goPage(page + 1);

  // 多選
  F("galSelMode").onclick = () => setSelMode(!selMode);
  F("selAll").onclick = () => selectAll();
  F("selAllPages").onclick = () => selectAll(true);
  F("selNone").onclick = clearSel;
  F("selBar").addEventListener("click", (e) => { const b = e.target.closest("[data-sel]"); if (b) batch(b.dataset.sel); });

  // 燈箱
  F("lbPrev").onclick = () => step(-1);
  F("lbNext").onclick = () => step(1);
  document.addEventListener("keydown", onKey);
  for (const box of ["#lbActions", "#lbTrashActions"]) $(box).addEventListener("click", lbAction);

  restoreFilters();
  initCompare();
}

// ---------------------------------------------------------------- 篩選
function saveFilters() {
  const f = { open: !F("galFilters").hidden };
  for (const id of FILTER_IDS) f[id] = F(id).value;
  store.set("galFilters", f);
}
function restoreFilters() {
  const f = store.get("galFilters", {});
  F("galFilters").hidden = !f.open;
  F("galFiltToggle").setAttribute("aria-expanded", String(!!f.open));
  for (const id of FILTER_IDS) if (f[id] !== undefined && id !== "galModel" && id !== "galLora") F(id).value = f[id];
  F("galSize").value = f.galSize || "220";
  F("galPer").value = ["10", "30", "50", "100"].includes(f.galPer) ? f.galPer : "50";
  F("galDateCustom").hidden = F("galDate").value !== "custom";
  applySize();
}
function applySize() { F("galGrid").style.setProperty("--tile", F("galSize").value + "px"); }

function dayStart(d) { const x = new Date(d); x.setHours(0, 0, 0, 0); return x.getTime() / 1000; }
function dateRange() {
  const v = F("galDate").value;
  if (v === "today") return [dayStart(Date.now()), null];
  if (v === "7" || v === "30") return [dayStart(Date.now() - (+v - 1) * 864e5), null];
  if (v === "custom") {
    const a = F("galFrom").value, b = F("galTo").value;
    return [a ? dayStart(a + "T00:00") : null, b ? dayStart(b + "T00:00") + 86400 : null];
  }
  return [null, null];
}

function query(offset, limit) {
  const [from, to] = dateRange();
  const qs = new URLSearchParams({ offset, limit, q: F("galQ").value.trim(), sort: F("galSort").value });
  if (view === "fav") qs.set("fav", 1);
  if (view === "trash") qs.set("trash", 1);
  for (const [k, id] of [["mode", "galMode"], ["family", "galFamily"], ["model", "galModel"], ["lora", "galLora"]]) if (F(id).value) qs.set(k, F(id).value);
  if (from) qs.set("from", from);
  if (to) qs.set("to", to);
  return qs;
}

function activeFilters() {
  return ["galMode", "galFamily", "galModel", "galLora", "galDate"].filter((id) => F(id).value).length + (F("galSort").value !== "new" ? 1 : 0);
}

async function loadFacets() {
  try {
    const f = await api("/api/gallery/facets");
    const saved = store.get("galFilters", {});
    const fill = (id, list, label, fmt) => {
      const s = F(id), want = s.value || saved[id] || "";
      s.innerHTML = "";
      s.append(el("option", { value: "" }, label));
      for (const x of list) s.append(el("option", { value: x }, fmt(x)));
      s.value = list.includes(want) ? want : "";
    };
    const short = (x) => x.replace(/\.safetensors$/, "").replace(/^.*[\\/]/, "");
    fill("galModel", f.models, "所有模型", short);
    fill("galLora", f.loras, "所有 LoRA", (x) => (app.state?.models?.loras || []).find((l) => l.name === x)?.title || short(x));
    F("galTrashN").textContent = f.trash ? ` ${f.trash}` : "";
  } catch { /* 伺服器重開中 */ }
}

// ---------------------------------------------------------------- 載入
export function show() { if (!loaded) reload(); else loadFacets(); }
export function invalidate() { loaded = false; clearSel(); }  // 恢復原廠、救回之後：下次打開圖庫重新載入

export function onNewImage(rec) {
  if (!loaded || view !== "all" || F("galQ").value || activeFilters()) return;
  total++;
  // 新圖在第 1 頁最前面：在第 1 頁就直接插進去，擠出這頁最後一張；在別頁只更新張數
  if (page === 1) {
    // 換一個新陣列（燈箱可能正拿著舊的那個在翻）
    items = [rec, ...items];
    F("galGrid").querySelector(".empty-state")?.remove();
    F("galGrid").prepend(tile(rec));
    if (items.length > per()) { $(`#galGrid [data-id="${items.at(-1).id}"]`)?.remove(); items = items.slice(0, -1); }
  }
  renderPager();
}

async function reload() {
  loaded = true;
  clearSel();  // 篩選換了，看不到的圖不能還留在選取裡（不然按 Delete 會刪到看不見的）
  F("galTrashBar").hidden = view !== "trash";
  F("selNormal").hidden = view === "trash";
  F("selTrash").hidden = view !== "trash";
  F("galFiltN").textContent = activeFilters() ? ` ${activeFilters()}` : "";
  loadFacets();
  await loadPage(1);
}

/** 換到第 p 頁（超出範圍的自動拉回第 1 頁／最後一頁） */
export function goPage(p) { return loadPage(Math.min(Math.max(1, p), pages())); }

// keep：原地重新整理這一頁（刪掉幾張之後補滿），捲動位置不動
async function loadPage(p, keep = false) {
  const my = ++reqSeq;
  const grid = F("galGrid");
  if (!keep) {
    grid.innerHTML = "";
    grid.append(el("div", { class: "empty-state", id: "galLoading" }, el("span", { class: "spinner" }), "載入中…"));
  }
  try {
    const r = await api("/api/gallery?" + query((p - 1) * per(), per()));
    if (my !== reqSeq) return;  // 篩選或頁數又改了，舊的結果不要
    total = r.total;
    // 刪掉一些之後這頁不存在了：改看最後一頁
    if (p > 1 && !r.items.length) return loadPage(pages(), keep);
    page = p;
    items = r.items;
    items.forEach((rec) => known.set(rec.id, rec));
    const frag = document.createDocumentFragment();
    // --i：進場動畫一張張排上來的順序（最多錯開 24 張，後面的一起出現）
    items.forEach((rec, i) => { const t = tile(rec); if (!keep) t.style.setProperty("--i", Math.min(i, 24)); frag.append(t); });
    grid.replaceChildren(frag);
    if (!total) grid.append(emptyState());
    if (!keep) grid.closest(".lib-scroll").scrollTop = 0;
    renderPager();
    renderSel();
  } catch (e) {
    if (my !== reqSeq) return;
    grid.replaceChildren(el("div", { class: "empty-state err" }, "圖庫載入失敗：" + e.message, el("button", { class: "btn sm", onclick: () => loadPage(p) }, "重試")));
  }
}

// 上排：共幾張、第幾頁；下排：頁碼（第一頁、最後一頁、目前附近幾頁）＋跳到第幾頁
function renderPager() {
  const n = pages();
  F("galTotal").textContent = view === "trash" ? `回收區 ${total} 張` : `共 ${total} 張`;
  F("galPageInfo").textContent = `${page} / ${n} 頁`;
  F("galPageMini").hidden = n <= 1;
  F("galPrev").disabled = page <= 1;
  F("galNext").disabled = page >= n;
  const box = F("galPager");
  box.innerHTML = "";
  box.hidden = n <= 1;
  if (n <= 1) return;
  const btn = (p, label = String(p), title) => el("button", { class: "akey sm pg" + (p === page ? " on" : ""), type: "button", title,
    disabled: p < 1 || p > n || p === page, "aria-current": p === page ? "page" : null, onclick: () => goPage(p) }, label);
  box.append(btn(page - 1, "‹ 上一頁", "上一頁（←）"));
  const want = new Set([1, n, page - 2, page - 1, page, page + 1, page + 2].filter((x) => x >= 1 && x <= n));
  let last = 0;
  for (const p of [...want].sort((a, b) => a - b)) {
    if (p - last > 1) box.append(el("span", { class: "pg-gap" }, "…"));
    box.append(btn(p));
    last = p;
  }
  box.append(btn(page + 1, "下一頁 ›", "下一頁（→）"));
  const jump = el("input", { type: "number", min: 1, max: n, class: "pg-jump", "aria-label": "跳到第幾頁", placeholder: String(page),
    onkeydown: (e) => { if (e.key === "Enter" && e.target.value) goPage(+e.target.value); } });
  box.append(el("label", { class: "row pg-go" }, "跳到第", jump, "頁"));
}

function emptyState() {
  if (view === "trash") return el("div", { class: "empty-state" }, "回收區是空的。");
  if (F("galQ").value || activeFilters()) return el("div", { class: "empty-state" }, "沒有符合條件的圖。", el("button", { class: "btn sm", onclick: () => F("galClearF").click() }, "清除篩選"));
  if (view === "fav") return el("div", { class: "empty-state" }, "還沒有收藏的圖。在圖上按「☆ 收藏」或燈箱裡按 F。");
  return el("div", { class: "empty-state" }, "還沒有圖片。到「生成」分頁畫第一張吧。", el("button", { class: "btn sm primary", onclick: () => app.showPage("gen") }, "去生成"));
}

function tile(rec) {
  known.set(rec.id, rec);
  const t = el("div", { class: "g" + (sel.has(rec.id) ? " selected" : "") + (rec.missing ? " missing" : ""), "data-id": rec.id, tabindex: 0,
    role: "button", "aria-label": (rec.idea || rec.prompt || "圖片").slice(0, 80), "data-i18n-keep": rec.idea || rec.prompt ? "aria-label" : undefined },
    el("img", { src: `/thumb/${rec.id}`, loading: "lazy", alt: "", width: rec.width, height: rec.height,
      onerror: (e) => { e.target.closest(".g").classList.add("missing"); e.target.removeAttribute("src"); } }),
    el("span", { class: "check", "aria-hidden": "true" }, "✓"),
    el("span", { class: "fav", hidden: !rec.fav }, "★"),
    el("span", { class: "miss-tag" }, "檔案不見了"),
    rec.gen_seconds != null ? el("span", { class: "dur", title: "從開始畫到存進圖庫（排隊不算）" }, fmtSecs(rec.gen_seconds)) : null,
    el("div", { class: "cap", translate: "no" }, rec.idea ? `💡 ${rec.idea}` : rec.prompt || ""));
  t.addEventListener("click", (e) => onTileClick(e, rec));
  t.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onTileClick(e, rec); } });
  return t;
}

// ---------------------------------------------------------------- 多選
function setSelMode(on) {
  selMode = on;
  F("galSelMode").classList.toggle("on", on);
  F("galGrid").classList.toggle("selecting", on || sel.size > 0);
  if (!on && !sel.size) F("selBar").hidden = true;
  renderSel();
}

function onTileClick(e, rec) {
  const idx = items.findIndex((x) => x.id === rec.id);
  // Shift＋點：從上一次點的那張選到這張（上一次點的在別頁就當一般點選）
  const a = lastClicked == null ? -1 : items.findIndex((x) => x.id === lastClicked);
  if (e.shiftKey && a >= 0) {
    const [lo, hi] = [Math.min(a, idx), Math.max(a, idx)];
    for (let i = lo; i <= hi; i++) sel.add(items[i].id);
    selMode = true;
  } else if (selMode || e.ctrlKey || e.metaKey || sel.size) {
    sel.has(rec.id) ? sel.delete(rec.id) : sel.add(rec.id);
    if (e.ctrlKey || e.metaKey) selMode = true;
  } else {
    openLightbox(items, idx, true);
    return;
  }
  lastClicked = rec.id;
  renderSel();
}

function renderSel() {
  $$("#galGrid .g").forEach((t) => t.classList.toggle("selected", sel.has(+t.dataset.id)));
  F("galGrid").classList.toggle("selecting", selMode || sel.size > 0);
  F("galSelMode").classList.toggle("on", selMode);
  F("selBar").hidden = !(selMode || sel.size);
  F("selCount").textContent = sel.size ? `已選 ${sel.size} 張` : "點圖片選取（Shift＋點選一段）";
  F("selAllPages").hidden = pages() <= 1;
  F("selAllPages").textContent = `全選全部 ${total} 張`;
  $$("#selBar [data-sel]").forEach((b) => (b.disabled = !sel.size || (b.dataset.sel === "compare" && sel.size !== 2)));
}

function clearSel() { sel.clear(); lastClicked = null; selMode = false; renderSel(); }

// 全選這一頁；allPages：篩選後的全部頁（一次抓 500 筆的編號）
async function selectAll(allPages = false) {
  items.forEach((x) => sel.add(x.id));
  if (allPages && total > items.length) {
    F("selCount").textContent = "選取全部中…";
    try {
      for (let off = 0; off < total; off += 500) {
        const r = await api("/api/gallery?" + query(off, 500));
        r.items.forEach((x) => { known.set(x.id, x); sel.add(x.id); });
        if (r.items.length < 500) break;
      }
    } catch (e) { toast("沒辦法選取全部：" + e.message, "err"); }
  }
  selMode = true;
  renderSel();
}

async function batch(act) {
  const ids = [...sel];
  if (!ids.length) return;
  if (act === "compare") return openCompare(ids.map((id) => known.get(id)).filter(Boolean));
  if (act === "zip") return exportZip(ids);
  if (act === "multiref") {
    const gen = await import("./gen.js");
    for (const id of ids.slice(0, 10)) await gen.imageAction("multiref", { id }, null, true);
    toast(ids.length > 10 ? "多圖參考最多 10 張，只加了前 10 張" : `已加入 ${ids.length} 張到多圖參考`, ids.length > 10 ? "err" : "ok");
    app.showPage("gen");
    return;
  }
  const label = { trash: "移到回收區", restore: "還原", purge: "永久刪除（丟進 Windows 資源回收筒）", fav: "收藏", unfav: "取消收藏" }[act];
  if ((act === "trash" || act === "purge") && !confirm(`${label} ${ids.length} 張圖？` + (act === "trash" ? "\n（之後可以到「回收區」還原）" : "\n（還能從 Windows 資源回收筒救回）"))) return;
  try {
    const r = await api("/api/images/batch", { json: { ids, action: act } });
    const bad = new Set(r.failed || []), done = ids.filter((id) => !bad.has(id));
    toast(`已${label} ${r.count} 張` + (bad.size ? `；${bad.size} 張沒辦法${label}：${r.error || ""}` : ""), bad.size ? "err" : "ok");
    if (act === "fav" || act === "unfav") {
      for (const x of known.values()) if (sel.has(x.id)) x.fav = act === "fav" ? 1 : 0;
      $$("#galGrid .g").forEach((t) => { if (sel.has(+t.dataset.id)) t.querySelector(".fav").hidden = act !== "fav"; });
      if (view === "fav" && act === "unfav") removeTiles(ids);
    } else {
      removeTiles(done);
      if (act === "trash") (await import("./gen.js")).forget(done);
    }
    clearSel();
    loadFacets();
  } catch (e) { toast(`${label}失敗：${e.message}`, "err"); }
}

// 拿掉之後重新抓這一頁，後面的圖往前補滿
function removeTiles(ids) {
  const s = new Set(ids);
  items = items.filter((x) => !s.has(x.id));
  ids.forEach((id) => { $(`#galGrid [data-id="${id}"]`)?.remove(); known.delete(id); });
  loadPage(page, true);
}

async function exportZip(ids) {
  toast(`打包 ${ids.length} 張中…`);
  try {
    const r = await fetch("/api/export", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ids }) });
    if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || `HTTP ${r.status}`);
    const name = /filename=([^;]+)/.exec(r.headers.get("Content-Disposition") || "")?.[1] || "anima.zip";
    const url = URL.createObjectURL(await r.blob());
    el("a", { href: url, download: name }).click();
    setTimeout(() => URL.revokeObjectURL(url), 60000);
    toast(`已匯出 ${name}`, "ok");
  } catch (e) { toast("匯出失敗：" + e.message, "err"); }
}

async function emptyTrash() {
  if (!confirm("把回收區全部丟進 Windows 資源回收筒？\n（圖庫裡就看不到了；需要的話還能從 Windows 資源回收筒救回）")) return;
  try {
    const r = await api("/api/trash/empty", { json: {} });
    toast(`已清空回收區（${r.count} 張丟進資源回收筒）`, "ok");
    reload();
  } catch (e) { toast("清空失敗：" + e.message, "err"); }
}

// ---------------------------------------------------------------- 鍵盤
function onKey(e) {
  if (!F("lightbox").hidden) {
    // 燈箱上面又開了別的視窗（存到提示詞庫、反推…）：按鍵給上面那個
    if (e.defaultPrevented || typing(e) || $$(".modal:not([hidden])").length > 1) return;
    const k = e.key.toLowerCase();
    if (e.key === "ArrowLeft") step(-1);
    else if (e.key === "ArrowRight") step(1);
    else if (e.key === "Escape") closeModal("#lightbox");
    else if (k === "f" && view !== "trash") lbAct("fav");
    else if (k === "c" && !e.ctrlKey) lbAct("copy");
    else if (k === "r" && view !== "trash") lbAct("reuse");
    else if (e.key === "Delete") lbAct(view === "trash" ? "purge" : "delete");
    else return;
    e.preventDefault();
    return;
  }
  if (app.page !== "gallery" || typing(e) || $(".modal:not([hidden])")) return;
  if (e.key === "/") { e.preventDefault(); F("galQ").focus(); }
  else if ((e.key === "ArrowLeft" || e.key === "PageUp") && !e.ctrlKey && !e.altKey && page > 1) { e.preventDefault(); goPage(page - 1); }
  else if ((e.key === "ArrowRight" || e.key === "PageDown") && !e.ctrlKey && !e.altKey && page < pages()) { e.preventDefault(); goPage(page + 1); }
  else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") { e.preventDefault(); selectAll(); }
  else if (e.key === "Escape" && (sel.size || selMode)) clearSel();
  else if (e.key === "Delete" && sel.size) batch(view === "trash" ? "purge" : "trash");
}

// ---------------------------------------------------------------- 燈箱
export function openLightbox(list, i, gal = false) { lb = { list, i, gal }; renderLb(); openModal("#lightbox"); }
export function openLightboxRecord(rec, list) { const l = list?.length ? list : [rec]; openLightbox(l, Math.max(0, l.findIndex((x) => x.id === rec.id))); }

async function step(d) {
  if (!lb.list.length) return;
  // 燈箱開著時這頁重新抓過（新圖進來、刪圖補滿）：對回同一張
  if (lb.gal && lb.list !== items) {
    const j = items.findIndex((x) => x.id === lb.list[lb.i]?.id);
    if (j >= 0) { lb.list = items; lb.i = j; }
  }
  const ni = lb.i + d;
  // 從圖庫開的：翻過這頁的頭尾就接著看上一頁／下一頁（第 1 頁往前＝最後一頁）
  if (lb.gal && lb.list === items && pages() > 1 && (ni < 0 || ni >= lb.list.length)) {
    const np = ni < 0 ? (page > 1 ? page - 1 : pages()) : page < pages() ? page + 1 : 1;
    const before = items;
    await loadPage(np);
    if (items === before || !items.length) return;  // 沒換成（失敗或被別的請求取代）
    lb.list = items;
    lb.i = ni < 0 ? items.length - 1 : 0;
    return renderLb();
  }
  lb.i = (ni + lb.list.length) % lb.list.length;
  renderLb();
}

const inTrash = () => view === "trash" && app.page === "gallery";

function lbAct(act) { const b = $(`#${inTrash() ? "lbTrashActions" : "lbActions"} [data-act="${act}"]`); if (b && !b.hidden) b.click(); }

async function lbAction(e) {
  const b = e.target.closest("[data-act]");
  if (!b || b.dataset.act === "download") return;
  const rec = lb.list[lb.i];
  const act = b.dataset.act;
  if (act === "copy" || act === "copyall") return copyMeta(rec, act === "copyall");
  if (act === "restore" || act === "purge") {
    if (act === "purge" && !confirm("永久刪除這張？（丟進 Windows 資源回收筒）")) return;
    try { await api("/api/images/batch", { json: { ids: [rec.id], action: act } }); } catch (err) { return toast(err.message, "err"); }
    toast(act === "restore" ? "已還原" : "已丟進資源回收筒", "ok");
    return dropFromLb(rec);
  }
  const { imageAction } = await import("./gen.js");
  if (["reuse", "img2img", "inpaint", "upscale", "multiref", "strip", "sheet", "charsheet", "frames"].includes(act)) closeModal("#lightbox");
  if (act === "video" || act === "frames") F("lbVid").pause();
  const r = await imageAction(act, rec, b);
  if (r === "deleted") dropFromLb(rec);
  if (act === "fav") $(`#galGrid [data-id="${rec.id}"] .fav`)?.toggleAttribute("hidden", !rec.fav);
}

function dropFromLb(rec) {
  const own = lb.list === items;
  if (items.some((x) => x.id === rec.id)) removeTiles([rec.id]);
  lb.list = (own ? items : lb.list).filter((x) => x.id !== rec.id);
  if (own) lb.gal = false;  // 這頁正在重新抓，燈箱改用自己的清單
  loadFacets();
  if (!lb.list.length) closeModal("#lightbox"); else { lb.i = Math.min(lb.i, lb.list.length - 1); renderLb(); }
}

async function copyMeta(rec, all) {
  try {
    const full = await api(`/api/image/${rec.id}`);
    const p = full.params || {};
    const text = all ? JSON.stringify({ prompt: full.prompt, negative: full.negative, seed: full.seed, idea: p.idea || full.idea || undefined,
      model: p.model, steps: p.steps, cfg: p.cfg, sampler: p.sampler, scheduler: p.scheduler, width: full.width, height: full.height,
      loras: p.loras, mode: full.mode, denoise: p.denoise }, null, 1) : full.prompt || "";
    await copyText(text);
    toast(all ? "已複製全部參數（JSON）" : "已複製提示詞", "ok");
  } catch (e) { toast("複製失敗：" + e.message, "err"); }
}

async function renderLb() {
  const rec = lb.list[lb.i];
  const trash = inTrash();
  F("lbActions").hidden = trash;
  F("lbTrashActions").hidden = !trash;
  const vid = isVideo(rec);
  F("lbImg").hidden = vid;
  F("lbVid").hidden = !vid;
  if (vid) { F("lbImg").removeAttribute("src"); F("lbVid").src = `/img/${rec.id}`; F("lbVid").play().catch(() => {}); }
  else { F("lbVid").pause(); F("lbVid").removeAttribute("src"); F("lbImg").src = `/img/${rec.id}`; }
  F("lbImg").classList.toggle("alpha", rec.mode === "rembg");  // 去背的透明圖墊棋盤格（play.css）
  for (const a of IMAGE_ONLY_ACTS) $$(`#lbActions [data-act="${a}"]`).forEach((b) => (b.hidden = vid));
  for (const a of VIDEO_ONLY_ACTS) $$(`#lbActions [data-act="${a}"]`).forEach((b) => (b.hidden = !vid));
  const pos = lb.gal && lb.list === items ? `　${(page - 1) * per() + lb.i + 1} / ${total}` : lb.list.length > 1 ? `　${lb.i + 1} / ${lb.list.length}` : "";
  F("lbTitle").textContent = `${modeName[rec.mode] || rec.mode || ""}・${rec.width}×${rec.height}` + pos;
  $$("#lightbox [data-act=download]").forEach((dl) => { dl.href = `/img/${rec.id}`; dl.download = rec.filename; });
  $("#lbActions [data-act=fav]").textContent = rec.fav ? "★ 已收藏" : "☆ 收藏";
  const m = F("lbMeta");
  m.innerHTML = "載入中…";
  let full;
  try { full = await api(`/api/image/${rec.id}`); } catch { m.textContent = "讀不到參數"; return; }
  if (lb.list[lb.i] !== rec) return;
  const p = full.params || {};
  // raw：使用者寫的內容（提示詞、想法），換介面語言也不翻
  const kv = (k, v, raw) => (v === undefined || v === null || v === "" ? null : [el("div", { class: "k" }, k), el("div", { class: "v", translate: raw ? "no" : undefined }, String(v))]);
  m.innerHTML = "";
  m.append(...[
    kv("原始想法（AI 改寫前）", p.idea || full.idea, true),
    kv(p.pe_prompt && p.pe_prompt.trim() !== (p.prompt || "").trim() ? "提示詞（實際使用，改寫後有手動修改）" : "提示詞", full.prompt, true),
    kv("AI 改寫結果", p.pe_prompt && p.pe_prompt.trim() !== (p.prompt || "").trim() ? p.pe_prompt : null, true),
    kv("負面提示詞", full.negative, true), kv("種子", full.seed),
    kv("模型", p.model ? p.model.replace(/\.safetensors$/, "") + (p.turbo_lora ? " + " + (p.turbo_lora === true ? "Turbo LoRA" : String(p.turbo_lora).replace(/\.safetensors$/, "")) : "") : null),
    kv("取樣", p.steps ? `${p.steps} 步・CFG ${p.cfg}・${p.sampler || ""} / ${p.scheduler || ""}` : null),
    kv("LoRA", (p.loras || []).map((l) => `${l.name.replace(/\.safetensors$/, "")}（${l.strength}）`).join("\n")),
    kv("重繪幅度", p.denoise), kv("高解析修復", p.hires?.enabled ? `×${p.hires.scale}・幅度 ${p.hires.denoise}` : null),
    kv("構圖控制", p.control?.enabled ? `${p.control.type}・強度 ${p.control.strength}` : null),
    kv("花費時間", full.gen_seconds != null ? `${fmtSecs(full.gen_seconds)}（從開始畫到存進圖庫，排隊不算）` : null),
    kv("建立時間", new Date(full.created * 1000).toLocaleString(document.documentElement.lang)),
    kv("移到回收區的時間", full.trashed ? new Date(full.trashed * 1000).toLocaleString(document.documentElement.lang) : null),
    kv("檔案", `${full.subfolder ? full.subfolder + "/" : ""}${full.filename}`),
  ].filter(Boolean).flat());
}

// ---------------------------------------------------------------- A／B 比較
let cmp = { a: null, b: null, x: 50 };

function initCompare() {
  const stage = F("cmpStage");
  const setX = (clientX) => {
    const r = F("cmpA").getBoundingClientRect();
    cmp.x = Math.max(0, Math.min(100, ((clientX - r.left) / r.width) * 100));
    drawCmp();
  };
  let drag = false;
  stage.addEventListener("pointerdown", (e) => { drag = true; stage.setPointerCapture(e.pointerId); setX(e.clientX); });
  stage.addEventListener("pointermove", (e) => { if (drag || e.pointerType === "mouse") setX(e.clientX); });
  stage.addEventListener("pointerup", () => { drag = false; });
  F("cmpMode").onclick = (e) => {
    const b = e.target.closest("button"); if (!b) return;
    $$("#cmpMode button").forEach((x) => x.classList.toggle("on", x === b));
    stage.classList.toggle("side", b.dataset.m === "side");
    drawCmp();
  };
  F("cmpSwap").onclick = () => { [cmp.a, cmp.b] = [cmp.b, cmp.a]; loadCmp(); };
  document.addEventListener("keydown", (e) => {
    if (F("cmpModal").hidden) return;
    if (e.key === "ArrowLeft") cmp.x = Math.max(0, cmp.x - 2);
    else if (e.key === "ArrowRight") cmp.x = Math.min(100, cmp.x + 2);
    else if (e.key === "Escape") return closeModal("#cmpModal");
    else return;
    e.preventDefault(); drawCmp();
  });
}

export function openCompare(recs) {
  if (recs.length !== 2) return toast("比較要剛好選兩張", "err");
  cmp = { a: recs[0], b: recs[1], x: 50 };
  openModal("#cmpModal");
  loadCmp();
}

async function loadCmp() {
  F("cmpA").onload = drawCmp;
  F("cmpA").src = `/img/${cmp.a.id}`;
  F("cmpB").src = `/img/${cmp.b.id}`;
  drawCmp();
  const box = F("cmpDiff");
  box.textContent = "比對參數中…";
  try {
    const [a, b] = await Promise.all([api(`/api/image/${cmp.a.id}`), api(`/api/image/${cmp.b.id}`)]);
    const pick = (r) => ({ 提示詞: r.prompt, 負面提示詞: r.negative, 種子: r.seed, 模型: r.params?.model?.replace(/\.safetensors$/, ""),
      步數: r.params?.steps, CFG: r.params?.cfg, 取樣器: r.params?.sampler, 排程: r.params?.scheduler, 尺寸: `${r.width}×${r.height}`,
      LoRA: (r.params?.loras || []).map((l) => `${l.name.replace(/\.safetensors$/, "")} ${l.strength}`).join("、") || "（沒用）", 重繪幅度: r.params?.denoise });
    const A = pick(a), B = pick(b);
    const rows = Object.keys(A).filter((k) => String(A[k] ?? "") !== String(B[k] ?? ""));
    box.innerHTML = "";
    if (!rows.length) { box.textContent = "兩張的參數完全一樣。"; return; }
    box.append(el("div", { class: "muted sm" }, `不一樣的地方（${rows.length} 項）：`));
    const t = el("div", { class: "cmp-table" });
    t.append(el("div", { class: "k" }), el("div", { class: "h" }, "A"), el("div", { class: "h" }, "B"));
    for (const k of rows) t.append(el("div", { class: "k" }, k), el("div", {}, String(A[k] ?? "—")), el("div", {}, String(B[k] ?? "—")));
    box.append(t);
  } catch (e) { box.textContent = "讀不到參數：" + e.message; }
}

function drawCmp() {
  const side = F("cmpStage").classList.contains("side");
  F("cmpBWrap").style.clipPath = side ? "" : `inset(0 0 0 ${cmp.x}%)`;
  F("cmpHandle").style.left = side ? "" : `calc(${handleLeft()}px)`;
}
function handleLeft() {
  const s = F("cmpStage").getBoundingClientRect(), r = F("cmpA").getBoundingClientRect();
  return r.left - s.left + (r.width * cmp.x) / 100;
}

// ---------------------------------------------------------------- 從圖庫挑一張（給輸入圖用）
// video：false＝只列圖片（當輸入圖／參考圖，後端 /api/to_input 也會擋影片）、"only"＝只列影片（拆幀台）、"also"＝都列（送到剪輯台）
export async function pickFromGallery(cb, { video = false, title = "" } = {}) {
  const g = F("gpGrid");
  g.innerHTML = "載入中…";
  F("gpTitle").textContent = title || (video === "only" ? "從圖庫選一支影片" : "從圖庫選一張");
  openModal("#galPicker");
  const r = await api(`/api/gallery?limit=120${video === "only" ? "&mode=video" : ""}`);
  g.innerHTML = "";
  let n = 0;
  for (const rec of r.items) {
    const vid = isVideo(rec);
    if (vid ? !video : video === "only") continue;
    n++;
    g.append(el("div", { class: "g" + (rec.missing ? " missing" : "") + (vid ? " vid" : ""), onclick: () => { if (rec.missing) return toast("這張的檔案不見了", "err"); closeModal("#galPicker"); cb(rec); } },
      el("img", { src: `/thumb/${rec.id}`, loading: "lazy", alt: "" }), el("span", { class: "miss-tag" }, "檔案不見了")));
  }
  if (!n) g.textContent = video === "only" ? "圖庫裡還沒有影片" : "圖庫是空的";
}

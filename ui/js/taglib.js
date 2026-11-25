// 詞庫（2026-11-17）：分類點選 tag，點一下塞進最後點過的提示詞框（大框、分段小框、負面都行），已經在框裡的再點一下拿掉。
// 兩個來源（GET /api/tags/library?src=…）：
//   danbooru（預設）＝照 Danbooru tag group 分類（tools\danbooru_taggroups.mjs），tag 帶使用次數、照 wiki 的段落分組，熱門的在前；
//   weilin＝WeiLin 的分類詞庫（MIT，照搬轉繁體）。
// 掛兩份：生成頁提示詞框下面的抽屜（init）、左邊「詞庫」頁（create）；詞庫資料、簡體轉換共用。
// 2026-11-17 第二版：拿掉第一版「每個小類抽幾個」的隨機抽（使用者：不是要這個）→ 改成隨機頁的「隨機產生器」（wardrobe.js）。
import { el, store, toast } from "./util.js";

const SEP = /[,，、]/;
const SOURCES = [["danbooru", "Danbooru 分類"], ["weilin", "WeiLin"]];
const PAGE = 400;   // 一個小類一次畫幾顆（Danbooru 有的小類幾千條）
const data = {}, loading = {};

async function load(src) {
  if (data[src]) return data[src];
  loading[src] ||= fetch(`/api/tags/library?src=${src}`).then(async (r) => {
    if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || `詞庫讀不到（${r.status}）`);
    return r.json();
  });
  try { data[src] = await loading[src]; } finally { loading[src] = null; }
  return data[src];
}

// 繁→簡（簡體玩家用簡體字搜詞庫）：跟簡中介面同一支 OpenCC（vendor\opencc-t2cn.js）
let toCN = null, cnLoading = null;
const cnCache = new Map();
async function loadCN() {
  cnLoading ||= import(new URL("../vendor/opencc-t2cn.js", import.meta.url)).then(({ Converter }) => { toCN = Converter({ from: "twp", to: "cn" }); });
  try { await cnLoading; } catch { cnLoading = null; }
}
function cnOf(s) {
  let v = cnCache.get(s);
  if (v === undefined) { v = toCN(s); cnCache.set(s, v); }
  return v;
}

const visible = (ta) => ta && ta.isConnected && ta.offsetParent !== null && !ta.disabled;

const norm = (s) => s.trim().toLowerCase().replace(/_/g, " ");
const tokens = (v) => v.split(SEP).map(norm).filter(Boolean);
const inBox = (ta, text) => !!ta && !text.includes(",") && tokens(ta.value).includes(norm(text));

/** 使用次數 → 12.3k、1.2M 這種 */
export function fmtCount(n) {
  if (!(n > 0)) return "";
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1).replace(/\.0$/, "") + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(n >= 1e4 ? 0 : 1).replace(/\.0$/, "") + "k";
  return String(n);
}

// 加：插在游標的位置（游標在某個詞中間就插在那個詞後面），前後補好逗號
export function addTo(ta, text) {
  const v = ta.value;
  let at = Math.min(ta.selectionEnd ?? v.length, v.length);
  const prev = v.slice(0, at).trimEnd().slice(-1), rest = v.slice(at).trimStart();
  if (prev && !SEP.test(prev) && rest && !SEP.test(rest[0]) && !/\s/.test(v[at - 1])) {
    // 游標卡在一個詞中間：插到那個詞後面
    const next = v.slice(at).search(SEP);
    at = next < 0 ? v.length : at + next + 1;
  }
  const before = v.slice(0, at).replace(/\s+$/, ""), after = v.slice(at).replace(/^\s*[,，、]?\s*/, "");
  const lead = before && !SEP.test(before.slice(-1)) ? ", " : before ? " " : "";
  const ins = lead + text + ", ";
  ta.value = before + ins + after;
  const caret = before.length + ins.length;
  ta.setSelectionRange(caret, caret);
}

// 拿掉：同一個詞（大小寫、底線、空白都算一樣）連同一個逗號一起拿掉；其他地方（換行、分段標題）原樣不動
export function removeFrom(ta, text) {
  const key = norm(text);
  const parts = ta.value.split(/([,，、])/); // 偶數格是詞、奇數格是逗號
  for (let i = 0; i < parts.length; i += 2) {
    if (norm(parts[i]) !== key) continue;
    if (i + 1 < parts.length) parts.splice(i, 2);
    else if (i > 0) parts.splice(i - 1, 2);
    else parts.splice(i, 1);
    break;
  }
  ta.value = parts.join("").replace(/^\s+/, "");
}

function label(ta) {
  if (ta.id === "negative") return "負面提示詞";
  if (ta.id === "prompt") return "提示詞";
  return (ta.getAttribute("aria-label") || "提示詞").replace(/^提示詞分段：/, "分段：");
}

function tabBtn(name, on, onclick, title) {
  return el("button", { type: "button", class: "tl-tab" + (on ? " on" : ""), onclick, ...(title ? { title } : {}) }, name);
}

/**
 * 掛一份詞庫。box：放面板的空 div；btn：開關鈕（抽屜才有）；main：大框；watch：要追蹤焦點的容器（分段小框、負面框所在的區塊）；
 * prefix：localStorage 鍵的開頭
 */
/**
 * 詞庫浮層的高度：want 是拖過記下來的（0／沒存過＝預設 min(60vh, 560)），夾在 [200, 視窗高 − 80]。
 */
export function sheetH(want, viewH) {
  const h = want > 0 ? want : Math.min(viewH * 0.6, 560);
  return Math.round(Math.max(200, Math.min(h, viewH - 80)));
}

/**
 * 詞庫浮層的位置（固定舞台 A 案：按「詞」從提示詞卡上緣往上彈出，卡片不變高）。
 * anchor 是提示詞卡在畫面上的方框；回傳 fixed 定位用的 left、width、top、height。
 * 上面空間不夠放就往下長，蓋住提示詞卡上半（2026-11-24 玩家：「詞庫面板太小，點分類很難點」），
 * 但整個留在畫面裡：頂邊不高過 minTop，底邊離視窗底至少 8。
 */
export function sheetRect(anchor, viewH, want = 0, minTop = 56, minHeight = 0) {
  const bottom = Math.max(0, viewH - 8);
  const height = Math.max(0, Math.min(Math.max(minHeight, sheetH(want, viewH)), bottom - minTop));
  const top = Math.min(bottom - height, Math.max(minTop, anchor.top - 6 - height));
  return { left: anchor.left, width: anchor.width, top, height };
}

export function create({ box, btn = null, main, watch = [], prefix = "taglib", sheet = null }) {
  let src = SOURCES.some(([k]) => k === store.get(prefix + "_src", "danbooru")) ? store.get(prefix + "_src", "danbooru") : "danbooru";
  const pos = { g: store.get(prefix + "_g", 0), s: store.get(prefix + "_s", 0) };
  const sources = [main.parentElement, ...watch];
  let lastTa = null;         // 最後點過的提示詞框
  let shownN = PAGE;         // 目前小類畫了幾顆
  let ui = null;

  // 塞到哪個框：最後點過、還看得到的那個；不然大框；大框藏起來（分段模式）就第一個分段小框
  function target() {
    if (visible(lastTa)) return lastTa;
    if (visible(main)) return main;
    for (const c of sources) {
      const ta = c.querySelector("textarea");
      if (visible(ta)) return ta;
    }
    return null;
  }

  const shown = () => ui && !ui.box.hidden && (sheet ? true : ui.box.offsetParent !== null);   // fixed 定位的浮層 offsetParent 是 null

  function toggle(text) {
    const ta = target();
    if (!ta) return toast("先點一下要放詞的提示詞框", "err");
    if (inBox(ta, text)) removeFrom(ta, text); else addTo(ta, text);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    lastTa = ta;
    paintOn();
  }

  function chip([text, zh, n], path) {
    const long = text.length > 48, c = fmtCount(n);
    return el("button", { type: "button", class: "tl-chip", "data-t": text, title: (path ? path + "\n" : "") + text + (n ? `\nDanbooru 上用過 ${n.toLocaleString()} 次` : ""), onclick: () => toggle(text) },
      el("b", {}, zh || text),
      zh || c ? el("small", { translate: "no" }, (zh ? (long ? text.slice(0, 46) + "…" : text) : "") + (c ? (zh ? " · " : "") + c : "")) : "");
  }

  // 已經在目標框裡的詞亮起來
  function paintOn() {
    if (!shown()) return;
    const ta = target();
    const have = new Set(ta ? tokens(ta.value) : []);
    for (const b of ui.list.querySelectorAll(".tl-chip")) b.classList.toggle("on", !b.dataset.t.includes(",") && have.has(norm(b.dataset.t)));
    ui.info.textContent = ta ? `點一下加進「${label(ta)}」，亮著的再點一下拿掉` : "先點一下要放詞的提示詞框";
  }

  // 一個小類：照段落分組（Danbooru 的 wiki 小標題），只畫前 shownN 顆，後面按「再顯示」
  function subList(s) {
    const out = [];
    const tags = s.tags.slice(0, shownN);
    let sec = -1;
    for (const t of tags) {
      const si = t[3] ?? 0;
      if (si !== sec) {
        sec = si;
        const name = s.secs?.[si];
        if (name) out.push(el("div", { class: "tl-sec" }, name));
      }
      out.push(chip(t));
    }
    if (s.tags.length > shownN) {
      out.push(el("button", { type: "button", class: "akey sm tl-more", onclick: () => { shownN += PAGE; render(); } }, `再顯示 ${Math.min(PAGE, s.tags.length - shownN)} 個（還有 ${s.tags.length - shownN} 個）`));
    }
    return out;
  }

  function render() {
    const d = data[src];
    if (!d || !ui) return;
    const { groups, subs, list, q, srcSel } = ui;
    srcSel.value = src;
    const g = d.groups[pos.g] || d.groups[0];
    const query = q.value.trim().toLowerCase();
    groups.replaceChildren(...d.groups.map((x, i) => tabBtn(x.name, !query && i === pos.g, () => { pos.g = i; pos.s = 0; shownN = PAGE; q.value = ""; save(); render(); }, x.en)));
    subs.hidden = !!query;
    list.replaceChildren();
    if (query) {
      // 中文：詞庫是繁體，簡體輸入（双马尾）兩邊都轉成簡體再比；轉換器第一次打中文才載入
      const cjk = /[㐀-鿿]/.test(query);
      if (cjk && !toCN) { loadCN().then(render); }
      const fq = cjk && toCN ? toCN(query) : query;
      const qs = query.replace(/_/g, " ");
      const hits = [], seen = new Set();
      outer: for (const x of d.groups) for (const s of x.subs) for (const t of s.tags) {
        if (seen.has(t[0])) continue;
        const zh = (t[1] || "").toLowerCase();
        if (t[0].toLowerCase().includes(qs) || zh.includes(query) || (cjk && toCN && cnOf(zh).includes(fq))) { seen.add(t[0]); hits.push(chip(t, `${x.name} › ${s.name}`)); }
        if (hits.length >= 300) break outer;
      }
      list.replaceChildren(...(hits.length ? hits : [el("span", { class: "muted sm" }, "詞庫裡沒有，直接在提示詞框打字也會跳出建議")]));
    } else {
      const s = g.subs[pos.s] || g.subs[0];
      subs.replaceChildren(...g.subs.map((x, i) => tabBtn(`${x.name} ${x.tags.length}`, i === pos.s, () => { pos.s = i; shownN = PAGE; save(); render(); }, x.en)));
      list.replaceChildren(...subList(s));
    }
    paintOn();
    if (sheet && shown()) placeSheet();
  }

  const save = () => { store.set(prefix + "_g", pos.g); store.set(prefix + "_s", pos.s); store.set(prefix + "_src", src); };

  async function show() {
    if (!data[src]) {
      ui.list.replaceChildren(el("span", { class: "muted sm" }, "載入詞庫…"));
      try { await load(src); } catch (e) { ui.list.replaceChildren(el("span", { class: "err sm" }, e.message)); return; }
    }
    render();
  }

  // 浮層模式：開著的時候跟著提示詞卡移動；點詞庫和提示詞卡以外的地方、或按 Esc 就收起來
  let sheetEvents = null, dragH = 0;
  const sheetKey = prefix + "_sheet_h";
  function placeSheet() {
    const anchor = sheet.anchor.getBoundingClientRect();
    const want = dragH || store.get(sheetKey, 0);
    let r = sheetRect(anchor, innerHeight, want);
    const box = ui.box;
    box.style.width = r.width + "px";
    box.style.height = r.height + "px";
    // 標題和分類會換行，小類與清單的高度又跟著百分比變。只有真的溢出才量能放下的最小高度。
    if (box.scrollHeight > box.clientHeight) {
      let low = r.height, high = Math.max(low, innerHeight - 56 - 8);
      while (low < high) {
        const mid = Math.floor((low + high) / 2);
        box.style.height = mid + "px";
        if (box.scrollHeight > box.clientHeight) low = mid + 1; else high = mid;
      }
      r = sheetRect(anchor, innerHeight, want, 56, low);
    }
    Object.assign(box.style, { left: r.left + "px", top: r.top + "px", height: r.height + "px" });
  }
  // 浮層上緣的把手：往上拖變高、往下拖變矮，放開記住；按兩下回預設高度。滑鼠、觸控都走 pointer 事件
  function sheetGrip() {
    const grip = el("div", { class: "tl-grip", role: "separator", "aria-orientation": "horizontal", tabindex: "0",
      "aria-label": "調整詞庫高度", title: "拖曳調整詞庫高度，按兩下恢復預設高度" });
    let y0 = 0, h0 = 0;
    const done = () => { if (dragH) store.set(sheetKey, dragH); dragH = 0; placeSheet(); };   // 只點一下沒拖就不記
    grip.addEventListener("pointerdown", (e) => {
      if (e.button) return;
      e.preventDefault();
      grip.setPointerCapture(e.pointerId);
      y0 = e.clientY;
      h0 = ui.box.offsetHeight;
    });
    grip.addEventListener("pointermove", (e) => {
      if (!grip.hasPointerCapture(e.pointerId)) return;
      dragH = sheetH(h0 + y0 - e.clientY, innerHeight);
      placeSheet();
    });
    grip.addEventListener("pointerup", (e) => { if (grip.hasPointerCapture(e.pointerId)) done(); });
    grip.addEventListener("pointercancel", () => { dragH = 0; placeSheet(); });
    grip.addEventListener("dblclick", () => { store.set(sheetKey, 0); dragH = 0; placeSheet(); });
    grip.addEventListener("keydown", (e) => {
      const d = { ArrowUp: 40, ArrowDown: -40 }[e.key];
      if (!d) return;
      e.preventDefault();
      dragH = sheetH(ui.box.offsetHeight + d, innerHeight);
      done();
    });
    return grip;
  }
  function watchSheet(on) {
    sheetEvents?.abort();
    sheetEvents = null;
    if (!on) return;
    sheetEvents = new AbortController();
    const signal = sheetEvents.signal;
    placeSheet();
    addEventListener("resize", placeSheet, { signal });
    addEventListener("scroll", (e) => {
      if (!(e.target instanceof Node && ui.box.contains(e.target))) placeSheet();
    }, { signal, capture: true });
    document.addEventListener("pointerdown", (e) => {
      if (!ui.box.contains(e.target) && !btn?.contains(e.target) && !sheet.anchor.contains(e.target)) open(false);
    }, { signal, capture: true });
    addEventListener("keydown", (e) => {
      if (e.key === "Escape" && !document.querySelector(".modal:not([hidden])")) { e.preventDefault(); open(false); }
    }, { signal, capture: true });
  }

  async function open(force) {
    const on = force ?? ui.box.hidden;
    ui.box.hidden = !on;
    if (sheet) watchSheet(on);
    if (btn) {
      btn.classList.toggle("on", on);
      btn.setAttribute("aria-pressed", String(on));
      store.set(prefix + "_open", on);
    }
    if (on) await show();
  }

  for (const c of [main, ...watch]) {
    c.addEventListener("focusin", (e) => { if (e.target.tagName === "TEXTAREA") { lastTa = e.target; paintOn(); } });
    c.addEventListener("input", () => paintOn());
  }
  const q = el("input", { type: "search", class: "tl-q", placeholder: "在詞庫裡找（中文、英文都可以）", "aria-label": "在詞庫裡找" });
  const srcSel = el("select", { class: "sm tl-src-sel", "aria-label": "詞庫來源", title: "Danbooru 分類：照 Danbooru 的 tag group 分類，附使用次數，熱門的在前面；WeiLin：WeiLin 整理的分類詞庫" },
    ...SOURCES.map(([k, name]) => el("option", { value: k }, name)));
  srcSel.onchange = () => { src = srcSel.value; pos.g = 0; pos.s = 0; shownN = PAGE; save(); show(); };
  const info = el("span", { class: "muted sm tl-info" });
  const groups = el("div", { class: "tl-tabs tl-groups" }), subs = el("div", { class: "tl-tabs tl-subs" });
  const list = el("div", { class: "tl-list" });
  let t = 0;
  q.addEventListener("input", () => { clearTimeout(t); t = setTimeout(() => data[src] && render(), 150); });
  box.replaceChildren(
    el("div", { class: "row between wrap gap tl-head" }, el("b", {}, "詞庫"), srcSel, q, info),
    groups, subs, list,
    el("div", { class: "muted sm tl-src" }, "詞庫來源：Danbooru tag group（danbooru.donmai.us 的 wiki）、WeiLin-Comfyui-Tools-Prompt（MIT）"),
  );
  ui = { box, q, srcSel, groups, subs, list, info };
  if (sheet) {
    box.classList.add("tl-sheet");
    box.prepend(sheetGrip());
    (document.getElementById("capLayer") || document.body).append(box);
  }
  if (btn) {
    btn.onclick = () => open();
    if (!sheet && store.get(prefix + "_open", false)) open(true);   // 浮層模式啟動時一律收著
  }
  return { open, refresh: paintOn };
}

// 生成頁的抽屜
export function init(opts) { return create({ ...opts, prefix: "taglib" }); }

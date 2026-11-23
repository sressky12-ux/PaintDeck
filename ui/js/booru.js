// 「畫廊」頁（2026-11-23，玩家建議：「加一个画廊页，直接搜 Danbooru／Gelbooru／Safebooru 的参考图？图源走 ComfyUI-Aaalice-Nodes，
// 图能直接丢进多图参考或图生图，也能一键复用提示词」）。
// 圖源＝ComfyUI 外掛 ComfyUI-Aaalice-Nodes 的 Booru Gallery（MIT，Aaalice233）：繪匣只轉送 /api/booru/x/* → /aaalice/booru-gallery/*
// （api_booru.rs），搜尋、快取、各站帳號金鑰都在外掛那邊。純函式在 booru_prompt.js。
import { el, api, toast, store, uploadFile, copyText, fmtN } from "./util.js";
import { app } from "./state.js";
import { buildPrompt, canSend, pickMediaUrl, fileName } from "./booru_prompt.js";
import { hasCjk, splitQuery, booruTag, translateQuery, pushHistory, matchHistory, replaceToken, COMMON, toggleTag, aliasOf } from "./booru_search.js";

const X = "/api/booru/x/";
const RATING = { general: "全年齡", safe: "全年齡", sensitive: "微色", questionable: "色", explicit: "露骨" };
const RATING_SHORT = { g: "全", general: "全", safe: "全", s: "微", sensitive: "微", q: "色", questionable: "色", e: "露", explicit: "露" };
const SORT = { latest: "最新", new: "最新", score: "高分", favcount: "最多收藏" };
const PERIOD = { day: "今天", week: "這週", month: "這個月" };
const CAT = [["character", "角色"], ["copyright", "作品"], ["artist", "畫師"], ["general", "一般"], ["meta", "其他（不放進提示詞）"]];
const LIMIT = 60;
const PAGE_TRIES = 6;   // 一頁全被濾掉時最多連翻幾頁

let ctx = null, ui = null;
const S = {
  status: null, sources: [], creds: {}, blacklist: [],
  src: store.get("bq_src", "danbooru"), sort: store.get("bq_sort", "latest"), ratings: store.get("bq_ratings", {}),
  rank: false, period: "day", posts: [], cursor: null, ended: false, loading: false, seq: 0, err: "", warn: [], trNote: "",
  sel: null, detail: null, opts: { artist: false, rating: true, ...store.get("bq_opts", {}) },
};

const media = (src, url) => `${X}media?source=${encodeURIComponent(src)}&url=${encodeURIComponent(url)}`;
const cap = () => S.sources.find((s) => s.source === S.src) || S.sources[0];

async function getJson(path) {
  const r = await fetch(X + path);
  let d = null;
  try { d = await r.json(); } catch { /* 非 JSON */ }
  if (!r.ok) throw new Error(d?.message || d?.error || `HTTP ${r.status}`);
  return d;
}

// ---------------------------------------------------------------- 安裝狀態（跟隨機頁的 Wardrobe 同一套）
function statusView(st) {
  if (st?.error) return [el("div", { class: "err sm" }, st.error)];
  if (st.loaded) return null;   // 有載入就能用：自己用 ComfyUI Manager 裝的（資料夾名不同）也算
  if (st.remote) return [el("div", { class: "notice sm" }, "現在連的是雲端的 ComfyUI：畫廊的圖源外掛要裝在本機的 ComfyUI。")];
  if (!st.installed) return [
    el("p", { class: "sm" }, "畫廊的圖源用 ComfyUI 外掛 ComfyUI-Aaalice-Nodes（MIT，作者 Aaalice233）的 Booru Gallery：直接搜 Danbooru、Gelbooru、Safebooru、AI TAG，縮圖和原圖都由它抓、在 ComfyUI 那邊快取。"),
    el("p", { class: "muted sm" }, "裝在本機 ComfyUI 的 custom_nodes（釘在繪匣審過的版本）。同一包還有它自己的 ComfyUI 介面小工具（儀表板、提示詞庫、Krita 橋接…），裝了會一起出現在 ComfyUI 網頁裡，不會改你的工作流。"),
    el("button", { type: "button", class: "akey go", onclick: install }, "安裝圖源外掛")];
  if (!st.online) return [el("div", { class: "notice sm" }, "本機的 ComfyUI 沒有連線：ComfyUI 開著才能搜圖。")];
  return [
    el("div", { class: "notice sm" }, "裝好了，ComfyUI 重開一次才會載入。"),
    el("button", { type: "button", class: "akey go", onclick: restartComfy }, "重開 ComfyUI")];
}
async function install() {
  toast("安裝中…（從 GitHub 抓，約幾十秒）");
  try { const r = await api("/api/booru/install", { json: {}, timeout: 300000 }); toast(r.restart ? "裝好了：重開 ComfyUI 就能用" : "已經裝好了", "ok"); } catch (e) { toast("安裝失敗：" + e.message, "err"); }
  await show();
}
async function restartComfy() {
  if ((app.state?.remaining || 0) > 0 && !confirm(`ComfyUI 還有 ${app.state.remaining} 個工作在跑或排隊，重開會中斷。確定要重開？`)) return;
  try { const deck = await import("./deck.js"); await deck.restartComfy(); } catch (e) { toast(e.message, "err"); }
  setTimeout(show, 9000);
}

// ---------------------------------------------------------------- 搜尋列
function ratingKeys() {
  const c = cap();
  const want = S.ratings[S.src];
  return (c?.ratings || []).filter((r) => Array.isArray(want) && want.includes(r));
}

function toolbar() {
  const c = cap();
  const srcSel = el("select", { "aria-label": "圖源", title: "要搜哪個站" },
    ...S.sources.map((s) => el("option", { value: s.source, ...(s.source === S.src ? { selected: true } : {}) }, s.displayName + (s.authRequired && !hasCreds(s.source) ? "（要帳號）" : ""))));
  srcSel.onchange = () => { S.src = srcSel.value; store.set("bq_src", S.src); S.rank = false; if (!cap()?.sortValues?.includes(S.sort)) S.sort = cap()?.sortValues?.[0] || "latest"; render(); search(true); };
  const q = ui.q;
  const go = el("button", { type: "button", class: "akey sm go", onclick: doSearch }, "搜尋");
  const rts = (c?.ratings || []).length > 1 ? el("div", { class: "row bq-rt", role: "group", "aria-label": "分級（都不選＝全部）", title: "分級：都不選＝全部" },
    ...c.ratings.map((r) => el("button", { type: "button", class: "chip" + (ratingKeys().includes(r) ? " on" : ""), "aria-pressed": String(ratingKeys().includes(r)),
      onclick: () => { const cur = new Set(ratingKeys()); cur.has(r) ? cur.delete(r) : cur.add(r); S.ratings = { ...S.ratings, [S.src]: [...cur] }; store.set("bq_ratings", S.ratings); render(); search(true); } }, RATING[r] || r))) : null;
  const sort = el("select", { "aria-label": "排序", class: "sm", hidden: S.rank || (c?.sortValues || []).length < 2 },
    ...(c?.sortValues || []).map((v) => el("option", { value: v, ...(v === S.sort ? { selected: true } : {}) }, SORT[v] || v)));
  sort.onchange = () => { S.sort = sort.value; store.set("bq_sort", S.sort); search(true); };
  const periods = c?.rankingPeriods || [];
  const rank = periods.length ? el("button", { type: "button", class: "akey sm" + (S.rank ? " on" : ""), "aria-pressed": String(S.rank), title: "熱門排行（不看搜尋字）",
    onclick: () => { S.rank = !S.rank; if (!periods.includes(S.period)) S.period = periods[0]; render(); search(true); } }, "熱門") : null;
  const period = S.rank && periods.length > 1 ? el("select", { class: "sm", "aria-label": "排行期間" }, ...periods.map((p) => el("option", { value: p, ...(p === S.period ? { selected: true } : {}) }, PERIOD[p] || p))) : null;
  if (period) period.onchange = () => { S.period = period.value; search(true); };
  return el("div", { class: "toolbar bq-bar" }, srcSel,
    el("div", { class: "scr search-scr bq-qwrap ac-wrap" }, el("span", { class: "sic", "data-ic": "search" }), q, ui.ac), ui.star, go, rts, sort, rank, period,
    el("span", { class: "grow" }), ui.info,
    el("button", { type: "button", class: "akey sm", onclick: () => { ui.setBox.hidden = !ui.setBox.hidden; if (!ui.setBox.hidden) paintSettings(); } }, "帳號與過濾"));
}

const hasCreds = (src) => Object.values(S.creds?.[src] || {}).some(Boolean);

// 搜尋框的下拉推薦（2026-11-23，使用者：「像 google 網頁搜索，打幾個字母／單字下面就跑出多種推薦的選項」）。由上到下：
//   1. 有中文 → 整句翻成 tag 的那一列（按 Enter 也是搜這個）
//   2. 搜過的：收藏 ★、最近 ↺（框是空的時候一點進去就列出來）；打了字只留對得上的
//   3. 游標所在那個詞的 tag 補完（繪匣本機的 Danbooru tag 庫：中文、使用次數、分類）
// 純函式在 booru_search.js
// 中文切段每打一個字都要查好幾次同樣的詞：查過的記著（最多 600 筆，滿了從最舊的丟）
const tagCache = new Map();
const tagLookup = async (q, limit = 40) => {
  const k = limit + "\u0000" + q;
  if (tagCache.has(k)) return tagCache.get(k);
  let r = [];
  try { const j = await (await fetch(`/api/tags?q=${encodeURIComponent(q)}&limit=${limit}`)).json(); r = Array.isArray(j) ? j : []; } catch { return []; }
  tagCache.set(k, r);
  if (tagCache.size > 600) tagCache.delete(tagCache.keys().next().value);
  return r;
};
const SPECIAL = /^(rating|order|score|id|user|fav|ordfav|status|date|width|height|age|md5|source|pool):/i;   // booru 的語法詞不補完
const hist = () => store.get("bq_hist", []);
const favs = () => store.get("bq_favs", []);
let acSeq = 0, acItems = [], acSel = -1;

async function suggest() {
  const my = ++acSeq;
  const v = ui.q.value, caret = ui.q.selectionEnd ?? v.length;
  const items = [];
  let lastZh = "";   // 中文沒空格（初音雙馬尾）查不到補完時，改補最後一段（雙馬尾 → 低位雙馬尾…）
  if (hasCjk(v)) {
    const tr = await translateQuery(v, (s) => tagLookup(s));
    lastZh = tr.notes.at(-1)?.[0] || "";
    if (my !== acSeq) return;
    if (tr.query && tr.query !== v.trim()) items.push({ kind: "go", q: tr.query, note: tr.notes.map(([a, b]) => `${a}→${b}`).join("、") + (tr.miss.length ? `；找不到：${tr.miss.join("、")}` : "") });
  }
  for (const h of matchHistory(hist(), favs(), v, v.trim() ? 4 : 8)) items.push({ kind: "hist", q: h.q, fav: h.fav });
  const a = v.slice(0, caret).search(/\S+$/);
  const tok = a < 0 ? "" : v.slice(a, caret).replace(/^[-~]/, "");
  if (tok && !SPECIAL.test(tok)) {
    let res = await tagLookup(tok.replace(/_/g, " "), 8);
    let pre = "";
    if (!res.length && hasCjk(tok) && lastZh && tok.endsWith(lastZh) && tok !== lastZh) {
      res = await tagLookup(lastZh, 8);
      pre = (await translateQuery(tok.slice(0, -lastZh.length), (s) => tagLookup(s))).query;   // 前面那幾段照翻好的留著
    }
    if (my !== acSeq) return;
    const al = hasCjk(tok) ? aliasOf(tok) : "";   // 口語對照（白絲 → white_thighhighs）排第一個；兩個 tag 的（黑長直）交給最上面那列整句搜
    if (al && !al.includes(" ")) { items.push({ kind: "tag", tag: al, zh: tok, pre: "" }); res = res.filter((x) => booruTag(x.tag) !== al); }
    for (const it of res) items.push({ kind: "tag", tag: booruTag(it.tag), zh: it.zh, count: it.count, cat: it.cat, alias: it.alias, pre });
  }
  if (my !== acSeq || document.activeElement !== ui.q) return;
  acItems = items;
  acSel = -1;   // 預設不選：直接按 Enter＝搜框裡的字（有中文會自動換）
  paintAc();
}

function paintAc() {
  if (!acItems.length) return closeAc();
  const row = (it, i) => {
    const cls = (i === acSel ? "sel " : "") + "ac-" + it.kind;
    const pick = (e) => { e.preventDefault(); acceptAc(i); };
    if (it.kind === "go") return el("div", { class: cls, onmousedown: pick }, el("b", {}, "→ 搜 " + it.q), el("i", {}, it.note));
    if (it.kind === "hist") return el("div", { class: cls, onmousedown: pick }, el("b", {}, (it.fav ? "★ " : "↺ ") + it.q), el("i", {}, it.fav ? "收藏的搜尋" : "最近搜過"),
      el("button", { type: "button", class: "ac-x", title: "從紀錄拿掉", "aria-label": "從紀錄拿掉", onmousedown: (e) => { e.preventDefault(); e.stopPropagation(); forget(it.q); suggest(); } }, "×"));
    return el("div", { class: cls, onmousedown: pick }, el("b", {}, (it.pre ? it.pre + " " : "") + it.tag),
      el("i", {}, (it.alias ? `← ${it.alias} ` : "") + (it.zh || "")), el("small", {}, [it.cat && it.cat !== "中文" ? it.cat : "", it.count ? fmtN(it.count) : ""].filter(Boolean).join(" · ")));
  };
  ui.ac.replaceChildren(...acItems.map(row));
  ui.ac.classList.add("on");
}
function closeAc() { acSeq++; acItems = []; ui.ac.classList.remove("on"); ui.ac.replaceChildren(); }
function acceptAc(i) {
  const it = acItems[i];
  if (!it) return closeAc();
  if (it.kind === "tag") {
    const r = replaceToken(ui.q.value, ui.q.selectionEnd, it.tag);
    let { value, caret } = r;
    if (it.pre) { const at = caret - booruTag(it.tag).length - 1; value = value.slice(0, at) + it.pre + " " + value.slice(at); caret += it.pre.length + 1; }
    ui.q.value = value;
    ui.q.setSelectionRange(caret, caret);
    return suggest();   // 接著打下一個詞
  }
  ui.q.value = it.q;
  doSearch();
}
function doSearch() { closeAc(); S.rank = false; render(); search(true); }

// 最近、收藏、常用（搜尋列下面那排）
function forget(q) { store.set("bq_hist", hist().filter((x) => x !== q)); store.set("bq_favs", favs().filter((x) => x !== q)); paintChips(); }
function toggleFav() {
  const q = ui.q.value.trim();
  if (!q) return toast("先打要收藏的搜尋");
  const f = favs();
  store.set("bq_favs", f.includes(q) ? f.filter((x) => x !== q) : [q, ...f].slice(0, 20));
  toast(f.includes(q) ? "拿掉收藏了" : "收藏這個搜尋了", "ok");
  paintChips();
}
function paintChips() {
  if (!ui) return;
  const q = ui.q.value.trim(), words = new Set(splitQuery(q));
  ui.star.textContent = favs().includes(q) && q ? "★" : "☆";
  ui.star.title = favs().includes(q) && q ? "拿掉這個收藏" : "收藏這個搜尋";
  const pill = (text, onclick, title, extra = "") => el("button", { type: "button", class: "chip bq-pill" + extra, title, onclick }, text);
  const recents = hist().filter((x) => !favs().includes(x)).slice(0, 6);
  ui.chips.replaceChildren(
    ...(favs().length || recents.length ? [el("span", { class: "lbl" }, "搜過")] : []),
    ...favs().map((x) => pill("★ " + x, () => { ui.q.value = x; doSearch(); }, "收藏的搜尋：點一下再搜一次")),
    ...recents.map((x) => pill("↺ " + x, () => { ui.q.value = x; doSearch(); }, "最近搜過：點一下再搜一次")),
    el("span", { class: "lbl" }, "常用"),
    ...COMMON.map(([tag, zh]) => pill(zh, () => { ui.q.value = toggleTag(ui.q.value, tag); doSearch(); }, `${tag}：點一下加進搜尋／再點拿掉`, words.has(tag) ? " on" : "")));
}

// ---------------------------------------------------------------- 搜尋、格子
async function search(reset = false) {
  if (!ui || !S.sources.length) return;
  const c = cap();
  // 重新搜：上一個還沒回來的作廢（seq 對不上就丟掉）
  if (reset) { S.seq++; S.loading = false; S.posts = []; S.cursor = null; S.ended = false; S.err = ""; S.warn = []; ui.grid.replaceChildren(); }
  if (S.loading || S.ended) return;
  if (c?.authRequired && !hasCreds(c.source)) {
    S.err = `${c.displayName} 要帳號才能搜（站方從 2025 年起擋匿名的 API）：按右上「帳號與過濾」填 User ID 和 API Key（在 ${c.credentialsUrl || "站方的帳號設定"} 拿）。`;
    return paintFoot();
  }
  if (reset && !S.rank) {
    S.trNote = "";
    if (hasCjk(ui.q.value)) {
      const tr = await translateQuery(ui.q.value, (s) => tagLookup(s));
      // 全部都對不到：框裡的字留著讓人改，不搜（空的搜尋會變成「全部最新的圖」）
      if (!tr.query) { S.err = `這些中文找不到對應的 tag：${tr.miss.join("、")}。換個說法，或打幾個字從下拉選單挑 tag。`; return paintFoot(); }
      ui.q.value = tr.query;
      S.trNote = (tr.notes.length ? "中文換成了：" + tr.notes.map(([a, b]) => `${a}→${b}`).join("、") : "") + (tr.miss.length ? `（找不到 tag、沒放進搜尋：${tr.miss.join("、")}）` : "");
    }
    if (ui.q.value.trim()) { store.set("bq_hist", pushHistory(hist(), ui.q.value.trim())); }
    paintChips();
  }
  const seq = ++S.seq;
  S.loading = true; paintFoot();
  const base = new URLSearchParams({ source: S.src, limit: String(Math.min(LIMIT, c?.maxPageSize || LIMIT)) });
  for (const r of ratingKeys()) base.append("rating", r);
  if (S.rank) base.set("period", S.period);
  else { base.set("query", ui.q.value.trim()); base.set("sort", S.sort); }
  store.set("bq_q", ui.q.value.trim());
  try {
    // 熱門排行的分級是外掛抓回來才濾：一整頁都被濾光不代表沒有了，往下多翻幾頁（最多 PAGE_TRIES 頁）
    for (let tries = 1; ; tries++) {
      const p = new URLSearchParams(base);
      if (S.cursor) p.set("cursor", S.cursor);
      const d = await getJson((S.rank ? "ranking?" : "search?") + p);
      if (seq !== S.seq) return;
      const fresh = (d.posts || []).filter((x) => !S.posts.some((y) => y.postId === x.postId));
      S.posts.push(...fresh);
      S.cursor = d.nextCursor || null;
      S.ended = !!d.ended || !S.cursor;
      S.warn = d.warnings || [];
      ui.grid.append(...fresh.map(card));
      if (fresh.length || S.ended) break;
      if (tries >= PAGE_TRIES) { S.ended = true; break; }
    }
  } catch (e) {
    if (seq !== S.seq) return;
    S.err = e.message;
    S.ended = true;
  } finally {
    if (seq === S.seq) { S.loading = false; paintFoot(); }
  }
}

function card(p) {
  const rk = String(p.rating || "").toLowerCase(), r = RATING_SHORT[rk];
  return el("button", { type: "button", class: "bq-card" + (S.sel === p.postId ? " on" : ""), "data-id": p.postId, title: `${p.width}×${p.height}`, onclick: () => openDetail(p) },
    el("img", { src: media(p.source, p.previewUrl || p.sampleUrl), alt: "", loading: "lazy", style: p.width && p.height ? `aspect-ratio:${p.width}/${p.height}` : null,
      onerror: (e) => e.target.replaceWith(el("span", { class: "bq-noimg" }, "縮圖抓不到")) }),
    r ? el("span", { class: "bq-r r-" + rk[0], title: RATING[RATING_FULL[rk] || rk] || rk }, r) : null);
}

function paintFoot() {
  const n = S.posts.length;
  ui.info.textContent = n ? `${n} 張` : "";
  const kids = [];
  if (S.trNote && !S.rank) kids.push(el("div", { class: "muted sm" }, S.trNote));
  if (S.err) kids.push(el("div", { class: "err sm" }, S.err));
  for (const w of S.warn) kids.push(el("div", { class: "muted sm" }, w));
  if (S.loading) kids.push(el("span", { class: "muted sm" }, "搜尋中…"));
  else if (!S.err && !n && S.ended) kids.push(el("span", { class: "muted sm" }, S.rank ? "這段期間的熱門裡沒有符合分級的圖：換個期間或拿掉分級試試。" : "沒有找到圖：換個 tag（英文、用底線，例如 long_hair）或拿掉分級試試。"));
  else if (!S.ended && n) kids.push(el("button", { type: "button", class: "akey sm", onclick: () => search() }, "再載入"));
  if (S.src === "danbooru" && !hasCreds("danbooru") && !S.rank) kids.push(el("div", { class: "muted sm" }, "Danbooru 沒登入時一次最多搜 2 個 tag（分級不算）。"));
  ui.foot.replaceChildren(...kids);
}

// ---------------------------------------------------------------- 右邊：詳細、送圖、複用提示詞
async function openDetail(p) {
  S.sel = p.postId;
  S.detail = null;
  for (const c of ui.grid.querySelectorAll(".bq-card")) c.classList.toggle("on", c.dataset.id === p.postId);
  ui.side.hidden = false;
  paintDetail(p);
  try {
    const d = await getJson(`detail?source=${encodeURIComponent(p.source)}&postId=${encodeURIComponent(p.postId)}`);
    if (S.sel !== p.postId) return;
    S.detail = d;
    paintDetail(p);
  } catch (e) {
    if (S.sel !== p.postId) return;
    ui.side.querySelector(".bq-tags")?.replaceChildren(el("div", { class: "err sm" }, "讀不到這張的 tag：" + e.message));
  }
}

const prompt = () => S.detail ? buildPrompt(S.detail.tags, S.detail.rating, S.opts) : "";

function paintDetail(p) {
  const d = S.detail;
  const big = d?.sampleUrl || p.sampleUrl || p.previewUrl;
  const send = d ? canSend(d) : false;
  const opt = (k, label, title) => {
    const b = el("input", { type: "checkbox", ...(S.opts[k] !== false ? { checked: true } : {}) });
    if (k === "artist") b.checked = !!S.opts.artist;
    b.onchange = () => { S.opts = { ...S.opts, [k]: b.checked }; store.set("bq_opts", S.opts); ui.side.querySelector(".bq-prompt").value = prompt(); };
    return el("label", { class: "check sm", title }, b, label);
  };
  const busy = (fn) => async (e) => {
    const b = e.currentTarget;
    b.disabled = true;
    try { await fn(); } catch (err) { toast(err.message, "err"); } finally { b.disabled = false; }
  };
  const tags = d ? CAT.filter(([k]) => d.tags?.[k]?.length).map(([k, name]) => el("div", { class: "bq-cat" }, el("b", {}, name),
    el("div", { class: "bq-tagrow" }, ...d.tags[k].map((t) => el("button", { type: "button", class: "bq-tag c-" + k, title: "用這個 tag 搜", onclick: () => { ui.q.value = t; S.rank = false; render(); search(true); } }, t.replace(/_/g, " ")))))) : [el("span", { class: "muted sm" }, "讀取 tag…")];
  ui.side.replaceChildren(
    el("div", { class: "row between" }, el("b", {}, `${p.source} #${p.postId}`),
      el("button", { type: "button", class: "icon", title: "收起", "aria-label": "收起", onclick: () => { ui.side.hidden = true; S.sel = null; for (const c of ui.grid.querySelectorAll(".bq-card.on")) c.classList.remove("on"); } }, "✕")),
    el("a", { class: "bq-big", href: p.postUrl, target: "_blank", rel: "noopener", title: "在原站開啟" },
      el("img", { src: media(p.source, big), alt: "", style: p.width && p.height ? `aspect-ratio:${p.width}/${p.height}` : null })),
    el("div", { class: "muted sm" }, `${p.width}×${p.height}` + (d?.fileExt ? ` · ${d.fileExt}` : "") + (d?.fileSize ? ` · ${(d.fileSize / 1048576).toFixed(1)} MB` : "") + (p.rating ? ` · ${RATING[RATING_FULL[p.rating] || p.rating] || p.rating}` : "") + (p.score ? ` · ${p.score} 分` : "")),
    el("div", { class: "bq-acts" },
      el("button", { type: "button", class: "akey sm go", disabled: !send, title: send ? "原圖放進生成頁的圖生圖輸入圖" : "動圖或影片不能送", onclick: busy(async () => { const r = await fetchUpload(d); ctx.toInit(r); toast("放進圖生圖了", "ok"); }) }, "送到圖生圖"),
      el("button", { type: "button", class: "akey sm", disabled: !send, title: send ? "加進生成頁的多圖參考（模型不是 Qwen 2.1／MiniMax 時會自動換）" : "動圖或影片不能送", onclick: busy(async () => { const r = await fetchUpload(d); if (ctx.toRef(r) != null) toast("加進多圖參考了", "ok"); }) }, "加進多圖參考")),
    el("div", { class: "bq-cats" }, ...tags),
    el("div", { class: "bq-pbox" },
      el("div", { class: "row wrap gap" }, el("b", { class: "sm" }, "複用提示詞"), opt("rating", "評級", "safe／sensitive／nsfw／explicit（Anima 一定要寫）"), opt("character", "角色"), opt("copyright", "作品"),
        opt("artist", "畫師（加 @）", "Anima 的畫師 tag 前面要加 @ 才有效；要不要學這位畫師的畫風你自己決定"), opt("general", "一般")),
      el("textarea", { class: "bq-prompt", rows: 4, readonly: true, "aria-label": "從這張圖整理出的提示詞" }, prompt()),
      el("div", { class: "row wrap gap" },
        el("button", { type: "button", class: "akey sm go", disabled: !d, onclick: () => { ctx.setPrompt(prompt(), "replace"); toast("提示詞換成這張圖的 tag 了", "ok"); } }, "取代提示詞"),
        el("button", { type: "button", class: "akey sm", disabled: !d, onclick: () => { ctx.setPrompt(prompt(), "append"); toast("加到提示詞後面了（重複的不加）", "ok"); } }, "加到後面"),
        el("button", { type: "button", class: "akey sm", disabled: !d, onclick: async () => { await copyText(prompt()); toast("複製了", "ok"); } }, "複製"))));
}
const RATING_FULL = { g: "general", s: "sensitive", q: "questionable", e: "explicit" };

async function fetchUpload(d) {
  const url = pickMediaUrl(d);
  if (!url) throw new Error("這張是動圖或影片，不能送");
  toast("抓圖中…");
  const r = await fetch(media(d.source, url));
  if (!r.ok) {
    let m = `HTTP ${r.status}`;
    try { const j = await r.json(); m = j.message || j.error || m; } catch { /* 非 JSON */ }
    throw new Error("抓不到原圖：" + m);
  }
  const blob = await r.blob();
  if (!blob.type.startsWith("image/")) throw new Error("抓回來的不是圖片（" + (blob.type || "不明格式") + "）");
  return uploadFile(new File([blob], fileName(d, url), { type: blob.type }));
}

// ---------------------------------------------------------------- 帳號與過濾（金鑰只寫進外掛的設定檔，讀回來只有「有沒有填」）
function paintSettings() {
  const f = {};
  const row = (src, key, label, secret) => {
    f[`${src}.${key}`] = el("input", { type: secret ? "password" : "text", class: "sm", autocomplete: "off", placeholder: S.creds?.[src]?.[`has${key[0].toUpperCase()}${key.slice(1)}`] ? "已填（不顯示）；要換才填" : "沒填" });
    return el("label", { class: "sub" }, label, f[`${src}.${key}`]);
  };
  const clear = (src, keys) => el("button", { type: "button", class: "akey sm warn", disabled: !hasCreds(src),
    onclick: async () => { try { await api(X + "settings/save", { json: { clearCredentials: { [src]: keys } } }); toast("清掉了", "ok"); await loadSettings(); paintSettings(); render(); } catch (e) { toast(e.message, "err"); } } }, "清掉");
  const bl = el("textarea", { rows: 2, class: "sm", placeholder: "例：guro, scat（逗號或換行隔開）" }, (S.blacklist || []).join(", "));
  ui.setBox.replaceChildren(
    el("p", { class: "muted sm" }, "帳號金鑰存在 ComfyUI 的使用者資料夾（user\\default\\aaalice-nodes），繪匣和這個網頁都讀不到內容，只看得到有沒有填。"),
    el("div", { class: "bq-set" },
      el("fieldset", {}, el("legend", {}, "Gelbooru（一定要）"), row("gelbooru", "userId", "User ID"), row("gelbooru", "apiKey", "API Key（可以整串貼 &api_key=…&user_id=…）", true),
        el("a", { href: "https://gelbooru.com/index.php?page=account&s=options", target: "_blank", rel: "noopener", class: "sm" }, "到 Gelbooru 帳號設定拿 ↗"), clear("gelbooru", ["userId", "apiKey"])),
      el("fieldset", {}, el("legend", {}, "Danbooru（選填：填了可以搜更多 tag）"), row("danbooru", "username", "帳號名"), row("danbooru", "apiKey", "API Key", true),
        el("a", { href: "https://danbooru.donmai.us/profile", target: "_blank", rel: "noopener", class: "sm" }, "到 Danbooru 個人頁拿 ↗"), clear("danbooru", ["username", "apiKey"])),
      el("fieldset", {}, el("legend", {}, "黑名單"), el("label", { class: "sub" }, "有這些 tag 的圖不顯示，也不會放進提示詞", bl))),
    el("div", { class: "row gap" },
      el("button", { type: "button", class: "akey sm go", onclick: async () => {
        const creds = {};
        for (const [k, inp] of Object.entries(f)) { const v = inp.value.trim(); if (!v) continue; const [src, key] = k.split("."); (creds[src] ||= {})[key] = v; }
        const blacklist = bl.value.split(/[,，、\n]+/).map((s) => s.trim()).filter(Boolean);
        try { await api(X + "settings/save", { json: { credentials: creds, blacklist } }); toast("存好了", "ok"); await loadSettings(); ui.setBox.hidden = true; render(); search(true); } catch (e) { toast("存不了：" + e.message, "err"); }
      } }, "存"),
      el("button", { type: "button", class: "akey sm", onclick: () => { ui.setBox.hidden = true; } }, "關掉")));
}

async function loadSettings() {
  const s = await getJson("settings");
  S.creds = s.credentialStatus || {};
  S.blacklist = s.blacklist || [];
}

// ---------------------------------------------------------------- 畫面
function render() {
  if (!ui) return;
  const blocked = S.status && statusView(S.status);
  if (blocked) { ui.box.replaceChildren(el("div", { class: "bq-install" }, el("b", {}, "畫廊"), ...blocked)); return; }
  ui.box.replaceChildren(toolbar(), ui.chips, ui.setBox, el("div", { class: "bq-body" }, el("div", { class: "lib-scroll bq-scroll" }, ui.grid, ui.foot, ui.sentinel), ui.side),
    el("div", { class: "muted sm tl-src" }, "圖源：ComfyUI-Aaalice-Nodes 的 Booru Gallery（MIT，Aaalice233）。圖和 tag 版權屬原作者，拿來當參考就好。"));
  paintChips();
  paintFoot();
}

export async function show() {
  if (!ui) return;
  try { S.status = await api("/api/booru/status"); } catch (e) { S.status = { error: e.message }; }
  if (S.status?.loaded && !S.sources.length) {
    try {
      S.sources = (await getJson("sources")).sources || [];
      await loadSettings();
      if (!S.sources.some((s) => s.source === S.src)) S.src = S.sources[0]?.source || "danbooru";
      if (!cap()?.sortValues?.includes(S.sort)) S.sort = cap()?.sortValues?.[0] || "latest";
    } catch (e) { S.status = { error: "讀不到圖源：" + e.message }; }
  }
  render();
  if (S.status?.loaded && !S.posts.length && !S.loading && !S.err) search(true);
}

/** gen.js 掛上：{toInit(r)：放進圖生圖輸入圖, toRef(r)→第幾張或 null：加進多圖參考, setPrompt(text, "replace"|"append")} */
export function init(c) {
  ctx = c;
  const box = document.getElementById("booruBox");
  if (!box) return;
  const q = el("input", { type: "search", class: "bq-q", value: store.get("bq_q", ""), placeholder: "打 tag 或中文：初音 雙馬尾、hatsune_miku 1girl…（下面會跳推薦）", "aria-label": "搜尋 tag", autocomplete: "off" });
  ui = { box, q, ac: el("div", { class: "ac" }), info: el("span", { class: "vfd g", "aria-live": "polite" }), grid: el("div", { class: "bq-grid" }),
    foot: el("div", { class: "bq-foot" }), sentinel: el("div", { class: "bq-sentinel" }), side: el("aside", { class: "bq-side", hidden: true }), setBox: el("div", { class: "bq-setbox", hidden: true }),
    chips: el("div", { class: "row wrap bq-chips" }), star: el("button", { type: "button", class: "icon bq-star", onclick: toggleFav }, "☆") };
  q.addEventListener("input", () => { clearTimeout(q._t); q._t = setTimeout(suggest, 150); });
  q.addEventListener("input", () => paintChips());
  q.addEventListener("focus", () => suggest());   // 框是空的：一點進去就列搜過的（像 Google）
  q.addEventListener("blur", () => setTimeout(closeAc, 150));
  q.addEventListener("keydown", (e) => {
    if (ui.ac.classList.contains("on")) {
      // 上下鍵選（最上面再往上＝回到自己打的字）；Tab＝補完選到的（沒選就第一個 tag）；Esc 關掉
      if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); const n = acItems.length + 1; acSel = ((acSel + 1 + (e.key === "ArrowDown" ? 1 : n - 1)) % n) - 1; return paintAc(); }
      if (e.key === "Tab" && !e.shiftKey) { const i = acSel >= 0 ? acSel : acItems.findIndex((x) => x.kind === "tag"); if (i >= 0) { e.preventDefault(); return acceptAc(i); } }
      if (e.key === "Escape") { e.preventDefault(); return closeAc(); }
      if (e.key === "Enter" && acSel >= 0) { e.preventDefault(); return acceptAc(acSel); }
    }
    if (e.key === "Enter") { e.preventDefault(); doSearch(); }
  });
  // 捲到底自動載下一批
  new IntersectionObserver((es) => { if (es.some((x) => x.isIntersecting) && S.posts.length && !S.loading && !S.ended) search(); }, { rootMargin: "400px" }).observe(ui.sentinel);
}

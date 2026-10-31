// 資產抽屜：生成、導演分鏡、H3 影片台、漫畫、助手共用。點一個資產 → 後端照目前的模型轉換（/api/assets/{id}/apply）→ 插文字、放圖。
// mount(box, ctx)：ctx = { family(), hint?, insert(text), images?: { base?(r), control?(r), ref?: {add(r), count(), max, has?(name), tag?(n)}, place?(it, p, toInput) }, lock?(chip) }。
// ref.tag：提示詞裡怎麼寫第 n 張（預設 <imageN>）；place：頁面自己放（H3 素材板）。
// lock：鎖定設定表的角色不插字，交給頁面做成標籤（生成頁，見 lockedchips.js）；chip = {id, name, zh, en, tags, refs}。
// r 是 /api/to_input 的 {name, width, height}；ref.add 回傳加入後的 1 起算編號，放不下回 null。
import { api, el, toast, store } from "./util.js";
import * as assets from "./assets.js";

/** 小抄：每個模型吃得下什麼（設計文件 4.5） */
const HINT = {
  anima: "Anima：插 tag＋一句外觀英文；有圖可以選當圖生圖底圖或構圖控制（各 1 張）",
  qwen21: "Qwen 2.1：外觀文字＋參考圖最多 10 張（4 張以內細節最好），提示詞會自動寫上第幾張圖",
  moss: "MOSS：只吃文字，圖不會送",
  h3: "H3：圖放進素材板（同一個角色的圖算同一個人）、音檔放聲音格，外觀和聲音寫進點子",
  agent: "助手：圖放進附圖，外觀文字寫進句子",
  comic: "漫畫：插外觀文字和 tag",
};
const KINDS = ["char", "scene", "frame", "outfit", "prop", "fx", "sound", "snippet"];

const drawers = [];
const isOpen = () => store.get("assetDrawer", false);

export function mount(box, ctx) {
  const d = { box, ctx, kind: "char", q: "", menu: null };
  drawers.push(d);
  box.classList.add("adr");
  paint(d);
  return { refresh: () => paint(d) };
}

/** 收起／打開：每一頁的抽屜一起 */
function toggle() {
  store.set("assetDrawer", !isOpen());
  for (const d of drawers) paint(d);
}

async function paint(d) {
  const open = isOpen();
  const fam = d.ctx.family();
  const head = el("div", { class: "row adr-head" },
    el("button", { class: "akey sm", type: "button", "aria-expanded": String(open), onclick: toggle }, open ? "▾ " : "▸ ", "資產"),
    el("span", { class: "muted sm adr-hint" }, HINT[fam] || "這個模型只插文字"));
  if (!open) return d.box.replaceChildren(head);
  let items = [];
  try { items = await assets.list(); } catch { /* list() 自己會報錯 */ }
  const q = d.q.trim().toLowerCase();
  const shown = items.filter((x) => x.kind === d.kind && (!q || [x.name, ...(x.tags || []), x.data?.look_zh, x.data?.look_en, x.data?.text].join("\n").toLowerCase().includes(q)));
  const search = el("input", { type: "search", class: "adr-q", placeholder: "搜尋…", value: d.q, "aria-label": "搜尋資產" });
  search.oninput = () => { d.q = search.value; paintCards(d, cards, items); };
  const cards = el("div", { class: "adr-cards" });
  d.box.replaceChildren(head,
    el("div", { class: "row wrap adr-kinds" },
      ...KINDS.map((k) => el("button", { class: "chip" + (d.kind === k ? " on" : ""), type: "button", onclick: () => { d.kind = k; paint(d); } }, assets.KIND_NAME[k])),
      search,
      el("button", { class: "akey sm", type: "button", title: "重新讀資產庫", onclick: async () => { await assets.load(); paint(d); } }, "↻")),
    cards, d.menu || "");
  paintCards(d, cards, items, shown);
}

function paintCards(d, box, items, shown) {
  const q = d.q.trim().toLowerCase();
  shown = shown || items.filter((x) => x.kind === d.kind && (!q || [x.name, ...(x.tags || []), x.data?.look_zh, x.data?.look_en].join("\n").toLowerCase().includes(q)));
  box.replaceChildren(...shown.map((it) => el("button", { class: "adr-card", type: "button", title: it.data?.look_zh || it.data?.look_en || it.data?.text || "", onclick: () => use(d, it) },
    it.has_thumb ? el("img", { src: `/api/lib/${it.id}/thumb?v=${Math.round(it.updated || 0)}`, alt: "", loading: "lazy" }) : el("span", { class: "adr-ph" }, assets.KIND_NAME[it.kind]),
    el("span", { class: "adr-name" }, it.name))));
  if (!shown.length) box.append(el("span", { class: "muted sm" }, "這一類還沒有資產：到左邊「03 資產庫」新增"));
}

const toInput = (it, file) => api(`/api/assets/${it.id}/to_input`, { json: { file } });

/** 點一個資產：插文字；照模型放圖 */
async function use(d, it) {
  const fam = d.ctx.family();
  // 同一頁連點多個資產時，圖片與提示詞依點擊順序一起加入。
  d.pending = (d.pending || Promise.resolve()).then(() => apply(d, it, fam)).catch((e) => toast(e.message, "err"));
  return d.pending;
}

async function apply(d, it, fam) {
  if (d.ctx.family() !== fam) return;
  let p;
  try { p = await api(`/api/assets/${it.id}/apply`, { json: { family: fam } }); } catch (e) { return toast(e.message, "err"); }
  if (d.ctx.family() !== fam) return;
  if (fam !== "qwen21") for (const w of p.warn || []) toast(w, "err");
  const imgs = d.ctx.images || {};
  let text = p.text || "";
  const added = [];  // 這次放進多圖參考的檔名（鎖定標籤送出時照目前順序換成 <imageN>）
  try {
    if ((fam === "qwen21" || fam === "agent") && imgs.ref && p.images.length) {
      const max = imgs.ref.max || 10;
      const room = Math.max(0, max - imgs.ref.count());
      const ready = [];
      for (const f of p.images.slice(0, room)) {
        try { ready.push(await toInput(it, f)); } catch (e) { toast(e.message, "err"); }
        if (d.ctx.family() !== fam) return;
      }
      // 複製期間使用者仍可改參考圖；等全部複製完再同步加入、取得實際編號及插文字。
      const tags = [];
      for (const r of ready) {
        // 鎖定角色再點一次：已經在參考圖裡的同一張不重複放，照樣記給標籤
        if (p.locked && d.ctx.lock && imgs.ref.has?.(r.name)) { added.push(r.name); continue; }
        if (imgs.ref.count() >= max) break;
        const n = imgs.ref.add(r);
        if (n) { tags.push(imgs.ref.tag ? imgs.ref.tag(n) : `<image${n}>`); added.push(r.name); }
      }
      if (p.images.length > room || added.length < ready.length || (p.warn || []).some((w) => w.includes("10"))) toast(`參考圖最多 ${max} 張`, "err");
      if (tags.length && imgs.ref.count() > 4) toast(`目前有 ${imgs.ref.count()} 張參考圖，4 張以內細節最好`, "err");
      if (tags.length) text = `${it.name}（${tags.join(imgs.ref.tag ? "、" : "")}）${text ? "：" + text : ""}`;
    }
  } catch (e) { toast(e.message, "err"); }
  if (fam === "h3" && (p.voice || p.lang)) {
    const voice = `「${it.name}」聲音：${p.voice || "—"}；語言：${p.lang || "—"}`;
    text = [text, voice].filter(Boolean).join("\n");
  }
  const locked = !!(p.locked && d.ctx.lock);
  if (locked) {
    d.ctx.lock({ id: it.id, name: it.name, zh: p.spec?.zh || "", en: p.spec?.en || "", tags: p.spec?.tags || "", refs: added });
    toast(`「${it.name}」的設定表鎖在提示詞最前面（送出時才接上）`, "ok");
  } else if (text) d.ctx.insert(text);
  if (imgs.place) await imgs.place(it, p, (f) => toInput(it, f));
  if (fam === "anima" && p.images.length && (imgs.base || imgs.control)) {
    d.menu = el("div", { class: "row wrap adr-menu" },
      el("span", { class: "sm" }, `「${it.name}」有圖：`),
      imgs.base ? el("button", { class: "akey sm", type: "button", onclick: () => putImage(d, it, p.images[0], imgs.base) }, "當圖生圖底圖") : null,
      imgs.control ? el("button", { class: "akey sm", type: "button", onclick: () => putImage(d, it, p.images[0], imgs.control) }, "當構圖控制") : null,
      el("button", { class: "akey sm", type: "button", onclick: () => { d.menu = null; paint(d); } }, "不用圖"));
    paint(d);
  } else if (text && !locked) toast(`已插入「${it.name}」`, "ok");
}

async function putImage(d, it, file, put) {
  d.menu = null;
  paint(d);
  try { put(await toInput(it, file)); toast(`「${it.name}」的圖放好了`, "ok"); } catch (e) { toast(e.message, "err"); }
}

/** 一般文字框：接在最後（換一行），並觸發 input 讓各頁照常存檔 */
export function appendTo(ta) {
  return (text) => {
    const cur = ta.value.replace(/\s+$/, "");
    ta.value = cur ? `${cur}\n${text}` : text;
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  };
}

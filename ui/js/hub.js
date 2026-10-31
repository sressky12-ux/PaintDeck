// 下載中心（2026-10-29，設計見 design\下載中心_設計.md；後端 server\api_hub.rs、modelhub.rs）：
// 來源（C 站／魔塔／RunningHub）→ 種類（LoRA、主模型、VAE、工作流）→ 關鍵字、底模（站內的家族）、排序 → 卡片。
// 下載進度：C 站的 LoRA 走 civitai.rs（ws 的 download 事件，key＝版本編號），其他走 /api/models/downloads（輪詢）。
// 工作流：匯入生圖站（/api/hub/workflow 拿檔 → 設定頁的工作流匯入）；RunningHub 的還可以「在 RunningHub 跑」（花錢，先試算再問）。
import { $, $$, el, api, toast, fmtN, fmtBytes, store, openModal, makeDrop, pickFile, uploadFile } from "./util.js";
import { app } from "./state.js";
import * as models from "./models.js";

const KIND_NAME = { lora: "LoRA", model: "主模型", vae: "VAE", workflow: "工作流" };
const SRC_SHORT = { civitai: "C 站", modelscope: "魔塔", runninghub: "RunningHub" };
const SORTS = {
  civitai: [["downloads", "下載最多"], ["rating", "評價最高"], ["newest", "最新"]],
  modelscope: [["popular", "熱門"], ["newest", "最新"], ["recommend", "推薦"], ["stars", "收藏最多"]],
  runninghub: [],
};
const MACHINE = { default: "24GB", plus: "48GB", ultra: "84GB" };

let SRC = [], FAMS = [];
let cur = { source: store.get("hubSrc", "civitai"), kind: store.get("hubKind", "lora") };
let next = null, loaded = false, srcLoaded = null, seq = 0;
const shown = new Set(); // 這次搜尋已經畫過的 id：魔塔「推薦」「收藏最多」翻頁會重複送同一筆（2026-10-30 實測），不再畫第二張
let TAGS = null, tag = ""; // 魔塔的分類標籤（網頁版上面那排「寫實攝影、二次元…」），tag＝現在選的（空＝全部）
const civKeys = new Map(); // C 站 LoRA 下載：版本編號 → 卡片
const dlIds = new Map();   // /api/models/downloads 的 id → 卡片
let dlTimer = null;

export function init() {
  $("#hubSrc").addEventListener("click", (e) => { const b = e.target.closest("button[data-src]"); if (b && !b.disabled) pickSource(b.dataset.src); });
  $("#hubKind").addEventListener("click", (e) => { const b = e.target.closest("button[data-kind]"); if (b && !b.disabled) pickKind(b.dataset.kind); });
  $("#hubGo").onclick = () => search(false);
  $("#hubQ").onkeydown = (e) => { if (e.key === "Enter") search(false); };
  $("#hubAll").onchange = () => { fillFams(); search(false); };
  for (const id of ["#hubFam", "#hubSort", "#hubNsfw"]) $(id).onchange = () => search(false);
  $("#hubMore").onclick = () => search(true);
  initRun();
  // 設定頁匯入工作流後，缺的模型「到下載中心找」
  app.hubFind = find;
}

async function loadSources() {
  if (!srcLoaded) srcLoaded = api("/api/hub/sources").then((r) => { SRC = r.sources; FAMS = r.families; }).catch((e) => { srcLoaded = null; throw e; });
  return srcLoaded;
}

export async function show() {
  try { await loadSources(); } catch (e) { $("#hubNote").textContent = "讀不到下載中心的來源：" + e.message; return; }
  if (!SRC.some((s) => s.id === cur.source)) cur.source = "civitai";
  paintBar();
  if (!loaded) search(false);
}

const srcOf = (id) => SRC.find((s) => s.id === id);

function pickSource(id) {
  if (cur.source === id) return;
  cur.source = id;
  store.set("hubSrc", id);
  paintBar();
  search(false);
}

function pickKind(k) {
  if (cur.kind === k) return;
  cur.kind = k;
  store.set("hubKind", k);
  paintBar();
  search(false);
}

/** 上面兩排鋼琴鍵、底模、排序、成人內容：照現在的來源和種類 */
function paintBar() {
  const s = srcOf(cur.source);
  if (!s) return;
  if (!s.kinds.includes(cur.kind)) cur.kind = s.kinds[0];
  for (const b of $$("#hubSrc button")) { const on = b.dataset.src === cur.source; b.classList.toggle("on", on); b.setAttribute("aria-selected", String(on)); }
  for (const b of $$("#hubKind button")) {
    const k = b.dataset.kind, has = s.kinds.includes(k), on = k === cur.kind;
    b.disabled = !has;
    b.title = has ? "" : `${SRC_SHORT[s.id]}沒有${KIND_NAME[k]}可以搜`;
    b.classList.toggle("on", on);
    b.setAttribute("aria-selected", String(on));
  }
  const famOn = cur.kind !== "workflow";
  $("#hubFamBox").hidden = !famOn;
  fillFams();
  const sorts = SORTS[cur.source] || [];
  const keep = $("#hubSort").value;
  $("#hubSort").replaceChildren(...sorts.map(([v, t]) => el("option", { value: v }, t)));
  if (sorts.some(([v]) => v === keep)) $("#hubSort").value = keep;
  $("#hubSort").hidden = !sorts.length;
  $("#hubNsfwBox").hidden = cur.source !== "civitai";
  paintTags();
  $("#hubQ").placeholder = cur.source === "runninghub" ? "搜尋 RunningHub 的工作流（中文關鍵字就可以）"
    : cur.source === "modelscope" ? `搜尋魔塔的${KIND_NAME[cur.kind]}（中英文都可以）` : `搜尋 C 站的${KIND_NAME[cur.kind]}（英文關鍵字較準）`;
}

/** 魔塔的分類標籤：第一次切到魔塔才去讀清單；按一下篩、再按一下取消 */
async function paintTags() {
  const box = $("#hubTags"), on = cur.source === "modelscope" && cur.kind !== "workflow";
  box.hidden = true;
  if (!on) return;
  if (!TAGS) {
    try { TAGS = (await api("/api/hub/tags?source=modelscope")).tags || []; } catch { TAGS = []; }
    if (cur.source !== "modelscope") return;
  }
  const chip = (t, label) => el("button", { class: "chip" + (tag === t ? " on" : ""), type: "button", "aria-pressed": String(tag === t),
    onclick: () => { tag = t && tag !== t ? t : ""; paintTags(); search(false); } }, label);
  box.replaceChildren(chip("", "全部分類"), ...TAGS.map((t) => chip(t.tag, t.label)));
  box.hidden = !TAGS.length;
}

/** 底模選單：預設只列生圖站內建畫得了的；勾「也顯示生圖站用不了的」才有其他家族和「不限」 */
function fillFams() {
  const sel = $("#hubFam"), all = $("#hubAll").checked;
  const keep = sel.options.length ? sel.value : "anima"; // 空字串＝不限底模
  const list = FAMS.filter((f) => all || f.usable);
  sel.replaceChildren(...(all ? [el("option", { value: "" }, "不限底模")] : []),
    ...list.map((f) => el("option", { value: f.id }, f.usable ? f.label : `${f.label}（生圖站用不了）`)));
  sel.value = list.some((f) => f.id === keep) || (all && keep === "") ? keep : (list.find((f) => f.id === "anima") || list[0])?.id || "";
}

function note() {
  const s = srcOf(cur.source), fam = FAMS.find((f) => f.id === $("#hubFam").value);
  const parts = [];
  if (cur.source === "civitai") parts.push(cur.kind === "workflow" ? "C 站的工作流：按「匯入生圖站」會下載工作流檔、交給設定頁的工作流匯入。" : "下載後照底模分開存放，LoRA 的觸發詞、預覽圖也會一起存好。");
  if (cur.source === "modelscope") parts.push("魔塔在中國大陸不用梯子就能下載。按「選檔案」看這個模型有哪些檔，最大的通常是主檔。");
  if (cur.source === "modelscope" && ["recommend", "stars"].includes($("#hubSort").value)) parts.push("魔塔的「推薦」「收藏最多」排序翻頁時會重複、跳過一些（網站本身也一樣），要一筆不漏看完請選「熱門」或「最新」。");
  if (cur.source === "modelscope" && cur.kind !== "workflow" && fam?.id === "anima") parts.push("魔塔沒有 Anima 這個底模，作者什麼都標：這裡列的是名字或說明有 anima 的，名字明寫別的底模的不算。兩邊都沒寫 anima 的，勾「也顯示生圖站用不了的」、底模選「不限底模」再用關鍵字找。");
  if (cur.source === "runninghub") parts.push("RunningHub 只有工作流（模型在它們的雲端機器上）：可以匯進生圖站，或直接送到 RunningHub 跑（照秒扣 RH 幣）。");
  if (cur.kind !== "workflow" && fam && !fam.usable) parts.push(`${fam.label}生圖站內建的流程畫不了，下載後要配匯入的工作流才用得到。`);
  if (cur.source === "civitai" && !s?.has_key) parts.push("有些模型要登入才能下載：到設定頁填 Civitai API key。");
  if (cur.source === "runninghub" && !s?.has_key) parts.push("要先填 RunningHub 的 API Key（H3 影片台「在哪跑 → RunningHub」）才能匯入或跑。");
  $("#hubNote").textContent = parts.join("");
}

async function search(append) {
  loaded = true;
  const my = ++seq;
  if (!append) { $("#hubGrid").replaceChildren(); next = null; shown.clear(); $("#hubGrid").closest(".lib-scroll").scrollTop = 0; }
  note();
  const qs = new URLSearchParams({ source: cur.source, kind: cur.kind, q: $("#hubQ").value.trim() });
  if (cur.kind !== "workflow" && $("#hubFam").value) qs.set("family", $("#hubFam").value);
  if (!$("#hubSort").hidden && $("#hubSort").value) qs.set("sort", $("#hubSort").value);
  if (cur.source === "civitai") qs.set("nsfw", $("#hubNsfw").checked ? 1 : 0);
  if (cur.source === "modelscope" && cur.kind !== "workflow" && tag) qs.set("tag", tag);
  if (append && next != null) qs.set(cur.source === "civitai" ? "cursor" : "page", next);
  $("#hubMore").hidden = true;
  const loading = el("div", { class: "muted pad" }, "搜尋中…");
  $("#hubGrid").append(loading);
  try {
    const r = await api("/api/hub/search?" + qs, { timeout: 90000 });
    if (my !== seq) return; // 切了來源或種類：舊的結果不要了
    loading.remove();
    next = r.next ?? null;
    // 一張卡片畫壞（來源的資料長得不一樣）只影響那一張，不要讓後面的全部不見
    for (const it of r.items) {
      if (shown.has(it.id)) continue;
      shown.add(it.id);
      try { $("#hubGrid").append(card(it)); }
      catch (e) { console.error("下載中心：卡片畫不出來", it, e); $("#hubGrid").append(el("div", { class: "card muted pad" }, `這一筆顯示不出來：${e.message}`)); }
    }
    if (!r.items.length && !append) $("#hubGrid").append(el("div", { class: "muted pad" }, qs.has("tag")
      ? "這個分類下沒有找到：魔塔只有一小部分模型有標分類，換「全部分類」或別的底模試試。" : "沒有找到，換個關鍵字或底模試試。"));
    $("#hubMore").hidden = next == null;
  } catch (e) {
    if (my !== seq) return;
    if (loading.parentNode) loading.textContent = e.message;
    else $("#hubGrid").append(el("div", { class: "muted pad" }, e.message));
  }
}

// ---------------------------------------------------------------- 卡片
function badges(it) {
  const out = [el("span", { class: "badge src" }, SRC_SHORT[it.source] || it.source)];
  if (it.kind !== "workflow") {
    if (it.family_guess) out.push(el("span", { class: "badge ok", title: `魔塔沒有 Anima 這個底模標籤，作者標的是「${it.base}」；看名字或說明猜是 Anima，下載後生圖站會看檔頭再確認` }, `${it.family_label}？`));
    else if (it.family_label) out.push(el("span", { class: "badge " + (it.usable ? "ok" : "no"), title: it.usable ? "生圖站內建的流程畫得了" : "生圖站內建的流程畫不了，要配匯入的工作流" }, it.usable ? it.family_label : `${it.family_label}・生圖站用不了`));
    else if (it.base) out.push(el("span", { class: "badge no", title: "生圖站不認得這個底模" }, `${it.base}・生圖站用不了`));
  } else if (it.base && it.source === "runninghub") out.push(el("span", { class: "badge" }, it.base));
  if (it.nsfw) out.push(el("span", { class: "badge nsfw" }, "成人"));
  return el("div", { class: "hub-badges" }, ...out);
}

function card(it) {
  const blur = app.state?.settings?.blur_nsfw && it.cover && (it.cover_nsfw || 1) > 1;
  const c = { it, ft: el("div", { class: "ft" }), trig: el("div", { class: "trig" }), sel: null };
  const stat = [it.author, it.downloads ? (it.source === "runninghub" ? `跑過 ${fmtN(it.downloads)} 次` : `下載 ${fmtN(it.downloads)}`) : "", it.gens ? `生圖 ${fmtN(it.gens)}` : "",
    it.likes ? `👍 ${fmtN(it.likes)}` : "", it.size ? fmtBytes(it.size) : ""].filter(Boolean).join("・");
  const body = el("div", { class: "bd" },
    el("div", { class: "t" }, it.name || it.id),
    el("div", { class: "st" }, stat),
    badges(it));
  if (it.source === "civitai" && it.kind !== "workflow") civitaiPart(c, body);
  else if (it.source === "modelscope") msPart(c, body);
  else flowPart(c);
  if (it.description) body.append(el("div", { class: "st hub-desc", title: it.description }, it.description));
  body.append(c.trig, c.ft);
  return el("div", { class: "card" },
    el("div", { class: "ph" + (blur ? " blur" : "") + (it.cover ? " zoom" : " none"), style: it.cover ? `background-image:url('${String(it.cover).replace(/'/g, "%27")}')` : "",
      title: it.cover ? "看樣張大圖" : "", onclick: it.cover ? () => view(it) : null }, it.cover ? "" : "沒有預覽圖"),
    body);
}

/** 點封面：樣張大圖（C 站、魔塔最多 6 張）、觸發詞、說明，跟在網站上點進模型頁差不多 */
function view(it) {
  const imgs = it.images?.length ? it.images : [{ url: it.cover, nsfw: it.cover_nsfw || 1 }];
  const blur = app.state?.settings?.blur_nsfw;
  const big = (u) => String(u).replace("/width=450/", "/width=1024/").replace("resize,w_500", "resize,w_1200");
  $("#hubViewTitle").textContent = it.name || it.id;
  $("#hubViewMeta").textContent = [SRC_SHORT[it.source], it.author, it.base, it.gens ? `生圖 ${fmtN(it.gens)}` : "", it.downloads ? `下載 ${fmtN(it.downloads)}` : ""].filter(Boolean).join("・");
  $("#hubViewImgs").replaceChildren(...imgs.map((i) => el("img", { src: big(i.url), loading: "lazy", alt: "", class: blur && (i.nsfw || 1) > 1 ? "blur" : "",
    title: blur && (i.nsfw || 1) > 1 ? "點一下取消模糊" : null, onclick: (e) => e.currentTarget.classList.remove("blur") })));
  const tw = it.triggers?.length ? "觸發詞：" + it.triggers.join(", ") : "";
  $("#hubViewText").replaceChildren(...[tw, it.description].filter(Boolean).map((t) => el("p", {}, t)), link(it));
  openModal("#hubView");
}

const link = (it) => el("a", { class: "icon", href: it.url, target: "_blank", rel: "noopener", title: `在${SRC_SHORT[it.source]}開啟` }, "↗");

/** C 站 LoRA／主模型／VAE：版本選單（同一個模型有好幾個版本）、檔案選單（一個版本有好幾個模型檔，例如 fp16／fp32） */
function civitaiPart(c, body) {
  const it = c.it, vs = it.versions || [];
  c.ver = vs[0];
  const chooseFile = () => {
    const fs = (c.ver?.files || []).filter((f) => /\.(safetensors|ckpt|pt|pth|bin|gguf|sft)$/i.test(f.name || ""));
    c.files = fs.length ? fs : c.ver?.files || [];
    c.file = c.files.find((f) => f.primary) || c.files[0];
    c.fileSel?.remove();
    c.fileSel = c.files.length > 1
      ? el("select", { class: "sm", title: "選要下載的檔", onchange: (e) => { c.file = c.files[+e.target.value]; paint(); } },
        ...c.files.map((f, i) => el("option", { value: i, selected: f === c.file }, `${f.name}${f.size ? "（" + fmtBytes(f.size) + "）" : ""}`)))
      : null;
    // 第一次是在 card() 把觸發詞那行接上去之前呼叫的：那時先接在最後（card() 之後才接的觸發詞會排在它後面）
    if (c.fileSel) body.insertBefore(c.fileSel, c.trig.parentNode === body ? c.trig : null);
  };
  const paint = () => {
    c.ft.replaceChildren();
    if (c.ver?.installed) c.ft.append(el("span", { class: "badge ok" }, "已安裝"));
    else if (c.file) c.ft.append(el("button", { class: "btn sm primary", type: "button", onclick: () => download(c, c.ver, c.file) }, `下載（${c.file.size ? fmtBytes(c.file.size) : "?"}）`));
    else c.ft.append(el("span", { class: "muted sm" }, "這個版本沒有檔案"));
    c.ft.append(link(it));
    const tw = c.ver?.triggers || [];
    c.trig.textContent = tw.length ? "觸發詞：" + tw.slice(0, 6).join(", ") : "";
  };
  c.paint = paint;
  if (vs.length > 1) {
    body.append(el("select", { class: "sm", title: "選擇版本", onchange: (e) => { c.ver = vs.find((v) => String(v.id) === e.target.value); chooseFile(); paint(); } },
      ...vs.map((v) => el("option", { value: v.id }, `${v.name}${v.published ? "（" + v.published + "）" : ""}${v.base ? "・" + v.base : ""}${v.installed ? " ✓" : ""}`))));
  }
  chooseFile();
  for (const v of vs) civKeys.set(String(v.id), c);
  paint();
}

/** 魔塔：檔案清單要另外查（按「選檔案」） */
function msPart(c) {
  const it = c.it;
  c.trig.textContent = it.triggers?.length ? "觸發詞：" + it.triggers.slice(0, 6).join(", ") : "";
  c.paint = () => {
    c.ft.replaceChildren();
    if (c.done) c.ft.append(el("span", { class: "badge ok" }, "已安裝"));
    else if (!c.files) c.ft.append(el("button", { class: "btn sm primary", type: "button", onclick: () => msFiles(c) }, it.installed ? "選檔案（這台有其中一個）" : "選檔案"));
    else if (!c.files.length) c.ft.append(el("span", { class: "muted sm" }, "這個模型沒有模型檔"));
    else {
      const sel = el("select", { class: "sm", title: "選要下載的檔" },
        ...c.files.map((f, i) => el("option", { value: i, selected: f.primary }, `${f.installed ? "✓ " : ""}${f.name}（${fmtBytes(f.size)}）`)));
      const go = el("button", { class: "btn sm primary", type: "button" }, "下載");
      go.onclick = () => { const f = c.files[+sel.value]; if (f.installed && !confirm(`這台電腦已經有「${f.name}」了，還要再下載一次嗎？`)) return; download(c, null, f); };
      c.ft.append(sel, go);
    }
    c.ft.append(link(it));
  };
  c.paint();
}

async function msFiles(c) {
  c.ft.replaceChildren(el("span", { class: "muted sm" }, "讀檔案清單…"));
  try {
    c.files = (await api("/api/hub/files?" + new URLSearchParams({ source: "modelscope", id: c.it.id }), { timeout: 60000 })).files;
    c.files.sort((a, b) => (b.primary - a.primary) || (b.size || 0) - (a.size || 0));
  } catch (e) { toast(e.message, "err"); c.files = null; }
  c.paint();
}

/** 工作流卡片：匯入生圖站；RunningHub 的多「在 RunningHub 跑」 */
function flowPart(c) {
  const it = c.it;
  c.ft.append(el("button", { class: "btn sm primary", type: "button", onclick: (e) => importFlow(it, e.target) }, "匯入生圖站"));
  if (it.source === "runninghub") c.ft.append(el("button", { class: "btn sm", type: "button", title: "直接送到 RunningHub 的雲端機器跑，照秒扣 RH 幣；成品進圖庫", onclick: () => openRun(it) }, "在 RunningHub 跑"));
  c.ft.append(link(it));
}

// ---------------------------------------------------------------- 下載
function setBar(c, frac, text) {
  let bar = c.ft.querySelector(".bar > div"), lab = c.ft.querySelector(".sub");
  if (!bar) {
    c.ft.replaceChildren(el("div", { class: "bar", style: "flex:1" }, el("div")), el("span", { class: "sub" }, ""), link(c.it));
    bar = c.ft.querySelector(".bar > div"); lab = c.ft.querySelector(".sub");
  }
  bar.style.width = `${Math.round(100 * frac)}%`;
  lab.textContent = text;
}

async function download(c, ver, f) {
  const it = c.it;
  const fam = ver?.family || it.family, usable = ver ? ver.usable : it.usable;
  if (!usable && !confirm(`「${it.name}」的底模（${ver?.base || it.base || "不明"}）生圖站內建的流程畫不了，下載後只能配匯入的工作流用。還是要下載嗎？`)) return;
  const body = { source: it.source, id: it.id, kind: it.kind, family: fam || "", base: ver?.base || it.base || "", version: ver?.id ?? null, title: it.name,
    triggers: ver?.triggers || it.triggers || [], file: { name: f.name, path: f.path, size: f.size, sha256: f.sha256, url: f.url } };
  try {
    const r = await api("/api/hub/download", { json: body });
    setBar(c, 0, "準備中");
    if (r.via === "civitai") civKeys.set(String(r.key), c);
    else { dlIds.set(r.id, c); pollDl(); }
  } catch (e) { toast(e.message, "err"); }
}

function finish(c, ok, msg) {
  if (ok) {
    if (c.ver) c.ver.installed = true;
    c.done = true;
    toast(`「${c.it.name}」下載好了${c.it.kind === "lora" ? "，LoRA 清單裡可以用了" : ""}`, "ok");
    app.refreshState();
  } else toast(`下載失敗：${msg}`, "err");
  c.paint?.();
}

async function pollDl() {
  clearTimeout(dlTimer);
  let list;
  try { list = (await api("/api/models/downloads")).downloads; } catch { dlTimer = setTimeout(pollDl, 2000); return; }
  for (const [id, c] of dlIds) {
    const d = list.find((x) => x.id === id);
    if (!d) continue;
    if (d.status === "run") { setBar(c, d.total ? d.done / d.total : 0, d.total ? `${fmtBytes(d.done)} / ${fmtBytes(d.total)}` : fmtBytes(d.done)); continue; }
    dlIds.delete(id);
    if (d.status === "done") finish(c, true);
    else if (d.status === "need_kind") finish(c, false, "看不出是哪一種模型：到設定頁「模型與工作流」手動指定分類再放");
    else finish(c, false, d.error || "失敗");
  }
  if (dlIds.size) dlTimer = setTimeout(pollDl, 1000);
}

/** ws 的 download（civitai.rs：C 站 LoRA） */
export function onDownload(m) {
  const c = civKeys.get(String(m.key));
  if (m.status === "done") { if (c) { c.ver = c.it.versions?.find((v) => String(v.id) === String(m.key)) || c.ver; finish(c, true); } else { toast(`「${m.name}」下載完成`, "ok"); app.refreshState(); } }
  else if (m.status === "error") { if (c) finish(c, false, m.error); else toast(`下載失敗：${m.error}`, "err"); }
  else if (c && m.total) setBar(c, m.done / m.total, `${Math.round((100 * m.done) / m.total)}%`);
}

// ---------------------------------------------------------------- 工作流匯入
async function importFlow(it, btn) {
  btn.disabled = true;
  try {
    const r = await api("/api/hub/workflow", { json: { source: it.source, id: it.id, version: it.versions?.[0]?.id ?? null }, timeout: 180000 });
    app.showPage("settings");
    await models.importData(r.file_name, r.data_b64, String(it.name || r.file_name).replace(/[\\/:*?"<>|]/g, " ").trim().slice(0, 60));
  } catch (e) { toast(e.message, "err"); }
  finally { btn.disabled = false; }
}

/** 設定頁「到下載中心找」：用檔名去 C 站搜（不限底模） */
export async function find(name, kind) {
  try { await loadSources(); } catch { /* show() 會講 */ }
  cur.source = "civitai";
  cur.kind = ["lora", "model", "vae"].includes(kind) ? kind : "model";
  $("#hubAll").checked = true;
  $("#hubQ").value = String(name).replace(/\.[^.]+$/, "").replace(/[_]+/g, " ").trim();
  loaded = true; // show() 不要再自己搜一次
  app.showPage("civitai");
  paintBar();
  $("#hubFam").value = "";
  search(false);
}

// ---------------------------------------------------------------- 在 RunningHub 跑
let run = null; // {it, dry, image, task}

function initRun() {
  makeDrop($("#hubRunDrop"), setRunImage);
  $("#hubRunPick").onclick = async () => { const f = await pickFile("image/*"); if (f) setRunImage(f); };
  $("#hubRunGo").onclick = sendRun;
  $("#hubRunStop").onclick = async () => {
    if (!run?.task) return;
    try { await api("/api/hub/rh_stop", { json: { task: run.task } }); } catch (e) { toast(e.message, "err"); }
  };
}

async function setRunImage(f) {
  if (!run) return;
  try {
    const r = await uploadFile(f);
    run.image = r.name;
    $("#hubRunImg").src = `/input/${encodeURIComponent(r.name)}`;
    $("#hubRunDrop").classList.add("has");
  } catch (e) { toast(e.message, "err"); }
}

async function openRun(it) {
  if (run?.task) { openModal("#hubRunModal"); return toast("上一個還在跑：等它跑完或按停止", "err"); }
  run = { it, dry: null, image: null, task: null };
  $("#hubRunTitle").textContent = `在 RunningHub 跑：${it.name}`;
  $("#hubRunInfo").textContent = "讀這個工作流…";
  $("#hubRunPromptBox").hidden = $("#hubRunDrop").hidden = true;
  $("#hubRunPrompt").value = "";
  $("#hubRunDrop").classList.remove("has");
  $("#hubRunMsg").textContent = "";
  $("#hubRunGo").disabled = true;
  $("#hubRunStop").hidden = true;
  openModal("#hubRunModal");
  try {
    const d = await api("/api/hub/rh_run", { json: { id: it.id, dry: true }, timeout: 90000 });
    if (run?.it !== it) return;
    run.dry = d;
    const where = `會用 RunningHub 的 ${MACHINE[d.instance] || d.instance} 機器跑，每秒約 ${d.coins_per_sec} RH 幣，跑多久扣多久（失敗不扣）；成品放進圖庫。機器大小在 H3 影片台「在哪跑 → RunningHub」改。`;
    const lines = d.understood
      ? [d.has_prompt ? "生圖站看得懂這個工作流：下面寫的提示詞會換掉它原本的（留空＝照它原本的）。" : "這個工作流沒有寫提示詞的地方。", d.needs_image ? "它要一張輸入圖。" : "", where]
      : [`生圖站看不懂這個工作流（${d.why}）：只能照原樣跑（用作者原本的提示詞和圖），或到 RunningHub 網頁上跑。`, where];
    $("#hubRunInfo").textContent = lines.filter(Boolean).join("");
    $("#hubRunPromptBox").hidden = !(d.understood && d.has_prompt);
    $("#hubRunDrop").hidden = !(d.understood && d.needs_image);
    $("#hubRunGo").disabled = false;
  } catch (e) {
    if (run?.it === it) $("#hubRunInfo").textContent = e.message;
  }
}

async function sendRun() {
  const d = run?.dry;
  if (!d) return;
  if (d.understood && d.needs_image && !run.image) return toast("先放一張輸入圖", "err");
  if (!confirm(`送到 RunningHub 跑「${run.it.name}」？\n${MACHINE[d.instance] || d.instance} 機器、每秒約 ${d.coins_per_sec} RH 幣，跑多久扣多久。`)) return;
  const prompt = d.understood && d.has_prompt ? $("#hubRunPrompt").value.trim() : "";
  $("#hubRunGo").disabled = true;
  try {
    const r = await api("/api/hub/rh_run", { json: { id: run.it.id, name: run.it.name, prompt, image: d.understood && d.needs_image ? run.image : "" }, timeout: 120000 });
    run.task = r.task;
    $("#hubRunStop").hidden = false;
    $("#hubRunMsg").textContent = "送出了，等 RunningHub…（可以關掉這個視窗，跑完會在圖庫）";
  } catch (e) {
    $("#hubRunGo").disabled = false;
    $("#hubRunMsg").textContent = e.message;
  }
}

/** ws 的 hubrun（api_hub::rh_run 的進度） */
export function onRun(m) {
  if (!run || m.task !== run.task) {
    if (m.state === "done") toast(`RunningHub 跑完了：成品在圖庫`, "ok");
    else if (m.state === "error") toast(`RunningHub：${m.msg}`, "err");
    return;
  }
  $("#hubRunMsg").textContent = m.msg;
  if (["done", "error", "stopped"].includes(m.state)) {
    run.task = null;
    $("#hubRunStop").hidden = true;
    $("#hubRunGo").disabled = false;
    if (m.state === "done") toast("RunningHub 跑完了：成品在圖庫", "ok");
    else if (m.state === "error") toast(`RunningHub：${m.msg}`, "err");
  }
}

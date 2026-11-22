// LoRA：已安裝清單頁、挑選視窗（加到生成）、觸發詞與預設強度編輯、移到待刪除
import { $, el, api, toast, openModal, closeModal, debounce, store, pickFile, makeDrop } from "./util.js";
import { app } from "./state.js";

let list = [], pickCb = null, pickFam = null, pickAll = false, shown = 0, pickShown = 0, pickOpts = {};
const pickSel = new Set();   // 多選時勾了哪些（l.name）
// 卡片／列表兩種排法（2026-11-17 網友：「或者是列表化选择」）：LoRA 庫和挑選視窗各記各的
const view = { page: store.get("lora_view", "grid"), pick: store.get("lora_pick_view", "grid") };
const PAGE = 60;
const KIND_ZH = { style: "畫風", character: "角色", clothing: "服裝", concept: "概念", pose: "姿勢", detail: "細節", lighting: "光影", other: "其他" };
const KINDS = Object.entries(KIND_ZH);

export function init() {
  $("#loraQ").oninput = debounce(() => renderPage(true), 150);
  for (const id of ["#loraFamily", "#loraCategory", "#loraTag", "#loraFav"]) $(id).onchange = () => renderPage(true);
  $("#loraMore").onclick = () => renderPage(false);
  $("#loraLearn").onclick = () => $("#assistBtn").click();
  $("#lpQ").oninput = debounce(() => { pickShown = 0; renderPicker(); }, 150);
  $("#loraRefresh").onclick = async () => { await app.refreshState(); toast("已重新整理", "ok"); };
  $("#loraView").onclick = () => { view.page = view.page === "list" ? "grid" : "list"; store.set("lora_view", view.page); renderPage(true); };
  $("#lpAdd").onclick = () => {
    const trig = !pickOpts.noTrig && $("#lpTrig").checked, cb = pickCb;
    const chosen = list.filter((x) => pickSel.has(x.name));
    closeModal("#loraPicker");
    for (const l of chosen) cb(l, trig);
  };
  $("#lpView").onclick = () => { view.pick = view.pick === "list" ? "grid" : "list"; store.set("lora_pick_view", view.pick); pickShown = 0; renderPicker(); };
  initPreviewDialog();
}

// 切換鈕寫「現在按下去會變成什麼」
function syncViewBtn(btn, mode, box) {
  btn.textContent = mode === "list" ? "▦ 卡片" : "☰ 列表";
  btn.title = mode === "list" ? "改回大卡片（看得到大預覽圖）" : "改成一行一個的列表（一次看比較多）";
  box.classList.toggle("as-list", mode === "list");
}

export function onState(st) {
  list = (st.models?.loras || []).slice().sort((a, b) => (b.fav ? 1 : 0) - (a.fav ? 1 : 0) || (a.title || a.name).localeCompare(b.title || b.name));
  if (app.page === "loras") { fillFilters(); renderPage(true); }
}

export function showPage() { fillFilters(); renderPage(true); }

const title = (l) => l.title || l.name.replace(/\.safetensors$/, "");

/** 卡匣上要顯示的名字：有自訂名稱用名稱，檔名不一樣才另外列出來（固定舞台 A 案：長名字換行、不切掉） */
export function loraNames(t, file) {
  const bare = String(file || "").replace(/\.safetensors$/, "");
  const name = t || bare;
  return { name, file: name === bare ? null : file };
}
/** 多選挑選視窗的加入鍵 */
export function pickAddLabel(n) { return `加入 ${n} 個`; }
const famName = (l) => app.state?.models?.family_names?.[l.family] || "";
// 卡片的底模小標和「模型」篩選：照站上判斷的家族分組，名稱用 Civitai 的寫法（2026-10-28 使用者：同一個底模出現「Anima」「Anima（動漫）」、「MOSS」「MOSS Image」兩個選項）
const FAM_LABEL = { anima: "Anima", moss: "MOSS Image", qwen21: "Qwen 2.1", sdxl: "SDXL", sd15: "SD1.5", flux: "Flux", sd3: "SD3", mmimage: "MiniMax H3 Image" };
const tagsOf = (l) => Array.isArray(l.tags) ? l.tags.filter((x) => typeof x === "string" && x.trim()) : [];
const categoryOf = (l) => {
  if (l.category) return l.category;
  if (KIND_ZH[l.agent?.kind] && l.agent?.kind !== "other") return l.agent.kind;
  const words = (tagsOf(l).join(" ") + " " + title(l) + " " + (l.agent?.desc_zh || l.agent?.desc || "")).toLowerCase();
  const clues = [
    ["character", /character|人物|角色|chara|cosplay/],
    ["clothing", /clothing|outfit|costume|fashion|服裝|服饰|衣服/],
    ["pose", /pose|姿勢|姿势|動作|动作/],
    ["lighting", /lighting|light effect|光影|燈光|灯光/],
    ["style", /style|artstyle|畫風|画风|aesthetic|anime style/],
    ["detail", /detail|texture|細節|细节|材質|材质/],
    ["concept", /concept|subject|object|背景|場景|场景/],
  ];
  return clues.find(([, re]) => re.test(words))?.[0] || l.agent?.kind || "";
};
const categoryLabel = (c) => KIND_ZH[c] || c || "未分類";
const modelLabel = (l) => FAM_LABEL[l.family] || l.base_model || "其他";
const match = (l, q) => !q || (title(l) + " " + modelLabel(l) + " " + l.name + " " + (l.triggers || []).join(" ") + " " + tagsOf(l).join(" ") + " " + categoryLabel(categoryOf(l)) + " " + (l.note || "") + " " + (l.agent?.desc_zh || "") + " " + (l.agent?.desc || "")).toLowerCase().includes(q.toLowerCase());
// 生圖助手對這個 LoRA 的認識（agent.rs 的 parse_lora_desc 寫進 lora_meta.agent）
function agentDesc(l) {
  const a = l.agent;
  if (!a?.desc) return null;
  return el("div", { class: "agent-desc", title: (a.src === "user" ? "你寫的" : a.src === "probe" ? "助手試畫比較後寫的" : "助手讀線索寫的") + "（給助手挑 LoRA 用）\n" + a.desc },
    el("span", { class: "ag-kind" + (a.sure === false ? " unsure" : "") }, "助手：" + (KIND_ZH[a.kind] || a.kind || "其他") + (a.sure === false ? "？" : "")), a.desc_zh || a.desc);
}
// 預覽圖的順序：自己下載／抓過存的 → 模型旁邊的同名圖 → 程式附的封面縮圖（cover_file，2026-11-08：不用連網）
// 換過的預覽圖檔名不變（custom_<雜湊>.jpg），用 preview_t 讓瀏覽器重抓
const previewUrl = (l) => l.preview ? `/lora_preview/${encodeURIComponent(l.preview)}` + (l.preview_t ? `?v=${Math.round(l.preview_t)}` : "")
  : l.local_preview ? `/api/loras/local-preview?name=${encodeURIComponent(l.name)}`
  : l.cover_file ? `/api/loras/bundled-cover/${encodeURIComponent(l.cover_file)}` : "";
// 模糊成人預覽圖：LoRA 標成人的，或封面那張圖 C 站的尺度等級大於 1（同下載中心的 cover_nsfw）
const blurOf = (l) => !!app.state?.settings?.blur_nsfw && (!!l.nsfw || (l.preview_nsfw ?? l.cover_nsfw ?? 1) > 1);

// 卡片上方的圖：有圖用 <img>（載不出來換成寫著名字的替代卡）；還在抓封面的先寫名字＋「正在抓封面…」（2026-11-07：預載的 LoRA 整片黑不好找）
function ph(l, attrs = {}) {
  const box = el("div", { class: "ph", "data-lora": l.name, ...attrs });
  fillPh(box, l);
  return box;
}
function fillPh(box, l) {
  const url = previewUrl(l);
  const noImg = (waiting) => [el("div", { class: "ph-name" }, title(l)), el("div", { class: "ph-note" }, waiting ? "正在抓封面…" : "沒有預覽圖")];
  box.classList.toggle("blur", blurOf(l));
  if (!url) return box.replaceChildren(...noImg(l.cover && !l.coverFailed));
  const img = el("img", { class: "ph-img", src: url, loading: "lazy", alt: "" });
  img.onerror = () => box.replaceChildren(...noImg(false));
  box.replaceChildren(img);
}
// 沒預覽圖、抓得到封面的（/api/state 的 cover）：後端在背景抓，抓完廣播 lora_cover（main.js 轉給 onCover）
const coverAsked = new Set();
const COVER_WAIT = 120000;
function askCovers(items) {
  const names = items.filter((l) => l.cover && !l.preview && !coverAsked.has(l.name)).map((l) => l.name);
  if (!names.length) return;
  for (const n of names) coverAsked.add(n);
  api("/api/loras/covers", { json: { names } }).catch(() => { for (const n of names) coverAsked.delete(n); });
  // 連不上（例如大陸連 C 站）又一直沒消息：兩分鐘後改寫「沒有預覽圖」，不要永遠停在「正在抓封面…」
  setTimeout(() => {
    for (const n of names) {
      const l = list.find((x) => x.name === n);
      if (l && !l.preview && !l.coverFailed) onCover({ name: n, failed: true });
    }
  }, COVER_WAIT);
}
export function onCover(m) {
  for (const l of [list.find((x) => x.name === m.name), app.state?.models?.loras?.find((x) => x.name === m.name)]) {
    if (!l) continue;
    if (m.failed) l.coverFailed = true;
    else Object.assign(l, { preview: m.preview, preview_nsfw: m.nsfw });
  }
  const l = list.find((x) => x.name === m.name);
  if (l) for (const box of document.querySelectorAll(".ph[data-lora]")) if (box.dataset.lora === m.name) fillPh(box, l);
}

function fillFilters() {
  const fill = (id, options) => {
    const box = $(id), old = box.value;
    box.replaceChildren(...options.map(([value, label]) => el("option", { value }, label)));
    box.value = [...box.options].some((o) => o.value === old) ? old : "";
  };
  const families = [...new Set(list.map(modelLabel))].sort((a, b) => (a === "其他") - (b === "其他") || a.localeCompare(b));
  const categories = [...new Set(list.map(categoryOf))].filter((c) => c && !KIND_ZH[c]).sort((a, b) => a.localeCompare(b));
  const counts = new Map();
  for (const l of list) for (const tag of tagsOf(l)) counts.set(tag, (counts.get(tag) || 0) + 1);
  const tags = [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  fill("#loraFamily", [["", "全部模型"], ...families.map((f) => [f, f])]);
  fill("#loraCategory", [["", "全部分類"], ["_none", "未分類"], ...KINDS.map(([id, label]) => [id, label]), ...categories.map((c) => [c, c])]);
  fill("#loraTag", [["", "常用標籤"], ...tags.slice(0, 100).map(([tag, n]) => [tag, `${tag} (${n})`])]);
}

function filtered() {
  const q = $("#loraQ").value.trim(), fam = $("#loraFamily").value, category = $("#loraCategory").value, tag = $("#loraTag").value;
  return list.filter((l) => match(l, q) && (!fam || modelLabel(l) === fam) && (!category || (category === "_none" ? !categoryOf(l) : categoryOf(l) === category)) && (!tag || tagsOf(l).includes(tag)) && (!$("#loraFav").checked || l.fav));
}

function renderPage(reset = true) {
  const items = filtered(), g = $("#loraGrid");
  if (reset) { shown = 0; g.replaceChildren(); }
  syncViewBtn($("#loraView"), view.page, g);
  const end = Math.min(items.length, shown + PAGE);
  for (const l of items.slice(shown, end)) g.append(view.page === "list" ? row(l) : card(l));
  askCovers(items.slice(shown, end));
  shown = end;
  $("#loraMore").hidden = shown >= items.length;
  $("#loraTotal").textContent = `${items.length} / ${list.length} 個`;
  if (!list.length) g.append(el("div", { class: "empty-span muted pad" }, "還沒有 LoRA。到「Civitai」分頁搜尋下載。"));
  else if (!items.length) g.append(el("div", { class: "empty-span muted pad" }, "沒有符合條件的 LoRA。"));
}

// 預覽圖可以直接拖一張圖上去換掉
function dropPh(l, attrs) {
  const box = ph(l, { title: "把圖拖到這裡就能換成這個 LoRA 的預覽圖", ...attrs });
  makeDrop(box, (f) => setPreview(l, f, isAdult(l)));
  return box;
}
const isAdult = (l) => !!l.nsfw || (l.preview_nsfw ?? l.cover_nsfw ?? 1) > 1;

// 列表模式的一行：小縮圖（滑過放大）＋名稱、觸發詞＋強度＋按鈕
function row(l) {
  const add = () => import("./gen.js").then((g) => { g.addLora(l, true); app.showPage("gen"); toast(`已加入「${title(l)}」`, "ok"); });
  return el("div", { class: "card lrow" },
    dropPh(l),
    el("div", { class: "bd" },
      el("div", { class: "t" }, (l.fav ? "★ " : "") + title(l), modelLabel(l) ? el("span", { class: "badge fam" }, modelLabel(l)) : null,
        el("span", { class: "badge" + (!l.category ? " auto" : "") }, categoryLabel(categoryOf(l)))),
      el("div", { class: "st", title: l.name }, l.name),
      l.triggers?.length ? el("div", { class: "trig", title: l.triggers.join(", ") }, "觸發詞：" + l.triggers.join(", ")) : null),
    el("div", { class: "ft" },
      el("span", { class: "sub" }, "強度"),
      el("input", { type: "number", step: 0.05, min: -2, max: 3, value: l.strength ?? 0.8, style: "width:62px", "aria-label": "預設強度", onchange: (e) => setMeta(l, { strength: +e.target.value }) }),
      el("button", { class: "btn sm primary", onclick: add }, "加到生成"),
      el("button", { class: "btn sm", onclick: () => openPreview(l) }, "換預覽圖"),
      el("button", { class: "btn sm", onclick: () => editTriggers(l) }, "改觸發詞"),
      el("button", { class: "icon", title: "收藏", onclick: () => setMeta(l, { fav: !l.fav }, true) }, l.fav ? "★" : "☆"),
      l.url ? el("a", { class: "icon", href: l.url, target: "_blank", title: "在 Civitai 開啟" }, "↗") : null));
}

function card(l) {
  return el("div", { class: "card" },
    dropPh(l),
    el("div", { class: "bd" },
      el("div", { class: "t" }, (l.fav ? "★ " : "") + title(l), modelLabel(l) ? el("span", { class: "badge fam" }, modelLabel(l)) : null),
      el("div", { class: "st" }, l.name),
      el("div", { class: "lora-card-meta" },
        el("span", { class: "badge" + (!l.category ? " auto" : ""), title: l.category ? "手動分類" : "依助手認識結果、標籤與名稱自動分類" }, categoryLabel(categoryOf(l))),
        ...tagsOf(l).slice(0, 8).map((tag) => el("button", { class: "chip", type: "button", title: `只看標籤：${tag}`, onclick: () => { const box = $("#loraTag"); if (![...box.options].some((o) => o.value === tag)) box.append(el("option", { value: tag }, tag)); box.value = tag; renderPage(true); } }, tag))),
      l.triggers?.length ? el("div", { class: "trig", title: "觸發詞" }, "觸發詞：" + l.triggers.join(", ")) : el("div", { class: "st" }, "沒有觸發詞"),
      l.note ? el("div", { class: "st", style: "white-space:pre-wrap" }, l.note) : null,
      agentDesc(l),
      el("div", { class: "ft" },
        el("span", { class: "sub" }, "預設強度"),
        el("input", { type: "number", step: 0.05, min: -2, max: 3, value: l.strength ?? 0.8, style: "width:70px", onchange: (e) => setMeta(l, { strength: +e.target.value }) })),
      el("div", { class: "ft" },
        el("button", { class: "btn sm primary", onclick: () => { import("./gen.js").then((g) => { g.addLora(l, true); app.showPage("gen"); toast(`已加入「${title(l)}」`, "ok"); }); } }, "加到生成"),
        el("button", { class: "btn sm", onclick: () => editTriggers(l) }, "改觸發詞"),
        el("button", { class: "btn sm", title: "選一張圖、貼上或拖進來，當這個 LoRA 的預覽圖", onclick: () => openPreview(l) }, "換預覽圖"),
        el("button", { class: "btn sm", onclick: () => editCategory(l) }, "分類"),
        el("button", { class: "btn sm", onclick: () => editTags(l) }, "標籤"),
        el("button", { class: "btn sm", title: "生圖助手靠這句話決定什麼時候掛這個 LoRA", onclick: () => editAgentDesc(l) }, "改用途"),
        el("button", { class: "icon", title: "收藏", onclick: () => setMeta(l, { fav: !l.fav }, true) }, l.fav ? "★" : "☆"),
        l.url ? el("a", { class: "icon", href: l.url, target: "_blank", title: "在 Civitai 開啟" }, "↗") : null,
        el("button", { class: "btn sm danger", title: "把這個 LoRA 檔搬到「待刪除」資料夾（不是真的刪，搬回去就能再用）", onclick: () => remove(l) }, "移到待刪除"))));
}

// 移到待刪除：檔案和預覽圖搬到 <資料>\待刪除\LoRA\，觸發詞等備註留著（搬回 LoRA 資料夾就回來）
async function remove(l) {
  if (!confirm(`把「${title(l)}」移到待刪除？\n\n檔案不會真的刪掉，會搬到生圖站資料夾的「待刪除\\LoRA」裡；想再用就搬回 LoRA 資料夾。\n生成頁、預設組裡用到它的地方會找不到這個 LoRA。`)) return;
  try {
    const r = await api("/api/loras/delete", { json: { name: l.name } });
    toast(`已移到待刪除：${r.moved_to}`, "ok");
    await app.refreshState();
    fillFilters(); renderPage(true);
  } catch (e) { toast(e.message, "err"); }
}

async function setMeta(l, patch, rerender) {
  Object.assign(l, await api("/api/loras/meta", { json: { name: l.name, ...patch } }));
  if (rerender) { await app.refreshState(); fillFilters(); renderPage(true); }
}

async function editCategory(l) {
  const current = l.category || categoryOf(l);
  const value = prompt("用途分類（畫風／角色／服裝／概念／姿勢／細節／光影／其他；可自訂）。\n清空＝恢復自動分類。", categoryLabel(current) === "未分類" ? "" : categoryLabel(current));
  if (value === null) return;
  const t = value.trim();
  const known = KINDS.find(([, zh]) => zh === t)?.[0] || (KIND_ZH[t] ? t : t);
  await setMeta(l, { category: known }, true);
}

async function editTags(l) {
  const value = prompt("標籤（用逗號分隔；可搜尋與篩選）：", tagsOf(l).join(", "));
  if (value === null) return;
  const tags = [...new Set(value.split(/[,，]/).map((s) => s.trim()).filter(Boolean))].slice(0, 60);
  await setMeta(l, { tags }, true);
}

async function editTriggers(l) {
  const v = prompt("觸發詞（用逗號分隔）：", (l.triggers || []).join(", "));
  if (v === null) return;
  await setMeta(l, { triggers: v.split(",").map((s) => s.trim()).filter(Boolean) }, true);
}

// 用途說明：生圖助手挑 LoRA 時看的那一句。自己寫的會蓋掉助手猜的；清空＝讓助手重新認識
async function editAgentDesc(l) {
  const a = l.agent || {};
  const v = prompt("這個 LoRA 是做什麼用的？一句話（中文、英文都可以）。\n例：把畫面變成雪夜發光的柔焦畫風，適合冬天、夜景。\n清空＝讓助手之後重新認識。", a.desc_zh || a.desc || "");
  if (v === null) return;
  const t = v.trim();
  await setMeta(l, { agent: t ? { ...a, desc: t, desc_zh: t, kind: a.kind || "other", sure: true, src: "user", t: Date.now() / 1000 } : {} }, true);
}

// ---------------------------------------------------------------- 換預覽圖（2026-11-17）
// 後端縮成長邊 640 的 JPEG 存進 previews\custom_<雜湊>.jpg；原本的預覽圖（C 站抓的）記著，按「還原」換回去
let prevLora = null;
function initPreviewDialog() {
  makeDrop($("#lpvDrop"), (f) => prevLora && setPreview(prevLora, f, $("#lpvNsfw").checked));
  $("#lpvPick").onclick = async () => { const f = await pickFile("image/png,image/jpeg,image/webp"); if (f && prevLora) setPreview(prevLora, f, $("#lpvNsfw").checked); };
  $("#lpvReset").onclick = async () => {
    if (!prevLora) return;
    const name = prevLora.name;
    try {
      applyMeta(prevLora, await api("/api/loras/preview/set", { json: { name, reset: true } }));
      // 本來沒有預覽圖的：重抓清單，才拿得到內建縮圖（cover_file）或抓封面的標記
      await app.refreshState();
      prevLora = list.find((x) => x.name === name) || prevLora;
      fillPreviewDialog();
      toast("已還原原本的預覽圖", "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  document.addEventListener("paste", (e) => {
    if ($("#loraPrev").hidden || !prevLora) return;
    const f = [...(e.clipboardData?.files || [])].find((x) => x.type.startsWith("image/"));
    if (!f) return;
    e.preventDefault();
    setPreview(prevLora, f, $("#lpvNsfw").checked);
  });
}
function openPreview(l) {
  prevLora = l;
  $("#lpvTitle").textContent = `換預覽圖：${title(l)}`;
  $("#lpvNsfw").checked = isAdult(l);
  fillPreviewDialog();
  openModal("#loraPrev");
  setTimeout(() => $("#lpvDrop").focus(), 50);
}
function fillPreviewDialog() {
  const l = prevLora;
  if (!l) return;
  const box = $("#lpvPh");
  box.dataset.lora = l.name;
  fillPh(box, l);
  $("#lpvReset").hidden = !l.preview_custom;
}
// 後端回的 LoRA 資訊蓋回清單（還原後沒有 preview 的欄位也要拿掉），重畫畫面上這個 LoRA 的所有預覽圖
function applyMeta(l, meta) {
  for (const x of [l, list.find((y) => y.name === l.name), app.state?.models?.loras?.find((y) => y.name === l.name)]) {
    if (!x) continue;
    for (const k of ["preview", "preview_nsfw", "preview_custom", "preview_t", "preview_orig", "preview_orig_nsfw"]) delete x[k];
    Object.assign(x, meta, { name: l.name });
  }
  for (const box of document.querySelectorAll(".ph[data-lora]")) if (box.dataset.lora === l.name) fillPh(box, l);
  if (prevLora?.name === l.name) fillPreviewDialog();
}
async function setPreview(l, file, nsfw) {
  const fd = new FormData();
  fd.append("image", file, file.name || "preview.png");
  try {
    applyMeta(l, await api(`/api/loras/preview?name=${encodeURIComponent(l.name)}&nsfw=${nsfw ? 1 : 0}`, { method: "POST", body: fd }));
    toast(`已換上「${title(l)}」的預覽圖`, "ok");
  } catch (e) { toast(e.message, "err"); }
}

/** 圖庫／生成結果的「設為 LoRA 預覽圖」：這張圖用了哪些 LoRA 就從那幾個挑（只用一個就直接問要不要換） */
export async function previewFromImage(rec) {
  let used = rec.params?.loras;
  if (!used) used = (await api(`/api/image/${rec.id}`)).params?.loras;
  const names = new Set((used || []).map((x) => x.name).filter(Boolean));
  const apply = async (l) => {
    try {
      applyMeta(l, await api("/api/loras/preview/set", { json: { name: l.name, id: rec.id, nsfw: isAdult(l) } }));
      toast(`已把這張圖設成「${title(l)}」的預覽圖`, "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  const mine = list.filter((l) => names.has(l.name));
  if (mine.length === 1) {
    if (confirm(`把這張圖設成「${title(mine[0])}」的預覽圖？`)) await apply(mine[0]);
    return;
  }
  // 用了好幾個（或一個都沒用）：開挑選視窗，先只列這張圖用到的
  openPicker(apply, mine.length ? (x) => names.has(x.name) : null, { title: "這張圖要當哪個 LoRA 的預覽圖？", noTrig: true });
}

// ---------------------------------------------------------------- 挑選視窗
// fam：只列出這個家族的 LoRA（Anima 的 LoRA 套在 Qwen 2.1 上沒有用，反之亦然）。
// 也可以給判斷函式 (l) => 能不能用（匯入的工作流組：同家族或家族不明的都能接）；用函式時列表下面有「顯示全部」可以看被藏起來的
// opts：{title, noTrig}（圖庫「設為 LoRA 預覽圖」借用這個視窗時換標題、藏「自動加入觸發詞」）
export function openPicker(cb, fam, opts = {}) {
  pickCb = cb;
  pickFam = fam || null;
  pickOpts = opts;
  pickAll = false;
  pickShown = 0;
  pickSel.clear();
  $("#lpFoot").hidden = !opts.multi;
  paintPickFoot();
  $("#lpQ").value = "";
  $("#lpTitle").textContent = opts.title || "選擇 LoRA 卡匣";
  $("#lpTrig").closest("label").hidden = !!opts.noTrig;
  renderPicker();
  openModal("#loraPicker");
  setTimeout(() => $("#lpQ").focus(), 50);
}

function paintPickFoot() {
  $("#lpCount").textContent = pickSel.size ? `勾了 ${pickSel.size} 個` : "點卡片勾選，可以一次勾好幾個";
  $("#lpAdd").textContent = pickAddLabel(pickSel.size);
  $("#lpAdd").disabled = !pickSel.size;
}

function renderPicker() {
  const q = $("#lpQ").value.trim();
  const g = $("#lpGrid");
  g.innerHTML = "";
  syncViewBtn($("#lpView"), view.pick, g);
  const asList = view.pick === "list";
  const fits = (x) => !pickFam || (typeof pickFam === "function" ? pickFam(x) : (x.family || "anima") === pickFam);
  const usable = pickAll ? list : list.filter(fits);
  if (!usable.length) g.append(el("div", { class: "empty-span muted pad" }, "還沒有這個模型能用的 LoRA。到「Civitai」分頁搜尋下載（記得選對基底模型）。"));
  const matches = usable.filter((x) => match(x, q));
  askCovers(matches.slice(0, pickShown + 80));
  for (const l of matches.slice(0, pickShown + 80)) {
    const had = !!pickOpts.multi && !!pickOpts.have?.has(l.name);
    // 多選：點卡片是勾／取消勾，視窗不關（已經在清單裡的灰掉、不能勾）；單選：點了就選、就關
    const pick = pickOpts.multi
      ? (e) => {
        if (had) return;
        pickSel.has(l.name) ? pickSel.delete(l.name) : pickSel.add(l.name);
        const card = e.currentTarget;
        card.classList.toggle("picked", pickSel.has(l.name));
        card.querySelector(".lp-ck").checked = pickSel.has(l.name);
        paintPickFoot();
      }
      : () => { closeModal("#loraPicker"); pickCb(l, !pickOpts.noTrig && $("#lpTrig").checked); };
    const ck = pickOpts.multi ? el("input", { type: "checkbox", class: "lp-ck", tabindex: "-1", "aria-hidden": "true", checked: had || pickSel.has(l.name), disabled: had }) : null;
    const hadTag = had ? el("span", { class: "badge lp-had" }, "已加入") : null;
    const cls = (base) => base + (had ? " had" : "") + (pickSel.has(l.name) ? " picked" : "");
    if (asList) {
      // 列表：一行一個，小縮圖＋名稱＋家族＋觸發詞，按整行就選
      g.append(el("div", { class: cls("card pick lrow"), onclick: pick },
        ck, ph(l),
        el("div", { class: "bd" },
          el("div", { class: "t" }, (l.fav ? "★ " : "") + title(l), modelLabel(l) ? el("span", { class: "badge fam" }, modelLabel(l)) : null, hadTag),
          l.triggers?.length ? el("div", { class: "trig", title: l.triggers.join(", ") }, l.triggers.slice(0, 6).join(", ")) : null,
          l.note ? el("div", { class: "st", title: l.note }, l.note) : null)));
      continue;
    }
    g.append(el("div", { class: cls("card pick"), onclick: pick },
      ck, ph(l, { style: "aspect-ratio:1" }),
      el("div", { class: "bd" }, el("div", { class: "t" }, title(l), hadTag), l.triggers?.length ? el("div", { class: "trig" }, l.triggers.slice(0, 4).join(", ")) : null,
        l.note ? el("div", { class: "pick-note", title: l.note }, l.note) : null)));
  }
  pickShown = Math.min(matches.length, pickShown + 80);
  if (pickShown < matches.length) g.append(el("button", { class: "akey sm", type: "button", onclick: () => renderPicker() }, `載入更多（${matches.length - pickShown} 個）`));
  // 匯入的工作流組：不同家族的 LoRA 先藏起來，需要時按「顯示全部」（選了也會在生成時被略過）
  const hidden = typeof pickFam === "function" && !pickAll ? list.filter((x) => !fits(x)).length : 0;
  if (hidden) g.append(el("div", { class: "muted sm pick-more" }, `已藏起 ${hidden} 個其他模型家族的 LoRA（用了也會被略過）。`,
    el("button", { class: "link", type: "button", onclick: () => { pickAll = true; pickShown = 0; renderPicker(); } }, "顯示全部")));
}

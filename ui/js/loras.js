// LoRA：已安裝清單頁、挑選視窗（加到生成）、觸發詞與預設強度編輯、移到待刪除
import { $, el, api, toast, openModal, closeModal, debounce } from "./util.js";
import { app } from "./state.js";

let list = [], pickCb = null, pickFam = null, pickAll = false, shown = 0, pickShown = 0;
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
}

export function onState(st) {
  list = (st.models?.loras || []).slice().sort((a, b) => (b.fav ? 1 : 0) - (a.fav ? 1 : 0) || (a.title || a.name).localeCompare(b.title || b.name));
  if (app.page === "loras") { fillFilters(); renderPage(true); }
}

export function showPage() { fillFilters(); renderPage(true); }

const title = (l) => l.title || l.name.replace(/\.safetensors$/, "");
const famName = (l) => app.state?.models?.family_names?.[l.family] || "";
// 卡片的底模小標和「模型」篩選：照站上判斷的家族分組，名稱用 Civitai 的寫法（2026-10-28 使用者：同一個底模出現「Anima」「Anima（動漫）」、「MOSS」「MOSS Image」兩個選項）
const FAM_LABEL = { anima: "Anima", moss: "MOSS Image", qwen21: "Qwen 2.1", sdxl: "SDXL", sd15: "SD1.5", flux: "Flux", sd3: "SD3" };
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
const previewUrl = (l) => l.preview ? `/lora_preview/${encodeURIComponent(l.preview)}` : l.local_preview ? `/api/loras/local-preview?name=${encodeURIComponent(l.name)}` : "";
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
function askCovers(items) {
  const names = items.filter((l) => l.cover && !l.preview && !coverAsked.has(l.name)).map((l) => l.name);
  if (!names.length) return;
  for (const n of names) coverAsked.add(n);
  api("/api/loras/covers", { json: { names } }).catch(() => { for (const n of names) coverAsked.delete(n); });
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
  const end = Math.min(items.length, shown + PAGE);
  for (const l of items.slice(shown, end)) g.append(card(l));
  askCovers(items.slice(shown, end));
  shown = end;
  $("#loraMore").hidden = shown >= items.length;
  $("#loraTotal").textContent = `${items.length} / ${list.length} 個`;
  if (!list.length) g.append(el("div", { class: "muted pad" }, "還沒有 LoRA。到「Civitai」分頁搜尋下載。"));
  else if (!items.length) g.append(el("div", { class: "muted pad" }, "沒有符合條件的 LoRA。"));
}

function card(l) {
  return el("div", { class: "card" },
    ph(l),
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

// ---------------------------------------------------------------- 挑選視窗
// fam：只列出這個家族的 LoRA（Anima 的 LoRA 套在 Qwen 2.1 上沒有用，反之亦然）。
// 也可以給判斷函式 (l) => 能不能用（匯入的工作流組：同家族或家族不明的都能接）；用函式時列表下面有「顯示全部」可以看被藏起來的
export function openPicker(cb, fam) {
  pickCb = cb;
  pickFam = fam || null;
  pickAll = false;
  pickShown = 0;
  $("#lpQ").value = "";
  renderPicker();
  openModal("#loraPicker");
  setTimeout(() => $("#lpQ").focus(), 50);
}

function renderPicker() {
  const q = $("#lpQ").value.trim();
  const g = $("#lpGrid");
  g.innerHTML = "";
  const fits = (x) => !pickFam || (typeof pickFam === "function" ? pickFam(x) : (x.family || "anima") === pickFam);
  const usable = pickAll ? list : list.filter(fits);
  if (!usable.length) g.append(el("div", { class: "muted pad" }, "還沒有這個模型能用的 LoRA。到「Civitai」分頁搜尋下載（記得選對基底模型）。"));
  const matches = usable.filter((x) => match(x, q));
  askCovers(matches.slice(0, pickShown + 80));
  for (const l of matches.slice(0, pickShown + 80)) {
    g.append(el("div", { class: "card pick", onclick: () => { pickCb(l, $("#lpTrig").checked); closeModal("#loraPicker"); } },
      ph(l, { style: "aspect-ratio:1" }),
      el("div", { class: "bd" }, el("div", { class: "t" }, title(l)), l.triggers?.length ? el("div", { class: "trig" }, l.triggers.slice(0, 4).join(", ")) : null,
        l.note ? el("div", { class: "pick-note", title: l.note }, l.note) : null)));
  }
  pickShown = Math.min(matches.length, pickShown + 80);
  if (pickShown < matches.length) g.append(el("button", { class: "akey sm", type: "button", onclick: () => renderPicker() }, `載入更多（${matches.length - pickShown} 個）`));
  // 匯入的工作流組：不同家族的 LoRA 先藏起來，需要時按「顯示全部」（選了也會在生成時被略過）
  const hidden = typeof pickFam === "function" && !pickAll ? list.filter((x) => !fits(x)).length : 0;
  if (hidden) g.append(el("div", { class: "muted sm pick-more" }, `已藏起 ${hidden} 個其他模型家族的 LoRA（用了也會被略過）。`,
    el("button", { class: "link", type: "button", onclick: () => { pickAll = true; pickShown = 0; renderPicker(); } }, "顯示全部")));
}

// 漫畫台：選版型、每格寫景別／內容／台詞／狀聲詞 → MOSS-Image 一次畫完整頁和字（2026-10-20 起只剩這一條路；以前的 Anima＋Qwen 填字、Qwen 一次畫完拿掉了）
// 後端在 src-tauri\src\comic.rs（版型、組提示詞）、server\api_comic.rs（背景工作）；websocket type=comic
// 實測與模板：design\漫畫頁提示詞設計.md（不用 AI 改寫模型）
import { api, el, toast, debounce, store, copyText } from "./util.js";
import { openLightbox } from "./gallery.js";
import * as gen from "./gen.js";
import * as drawer from "./assetdrawer.js";
import * as CL from "./comiclayout.js";

const F = (id) => document.getElementById(id);
let META = null;     // 版型、景別、對話框位置（後端給，跟組提示詞同一份）
let P = null;        // 目前的作品
let running = false;
let shown = false;
let SLOTS = [];      // 這一頁每一格的中文位置名（自訂版面由後端算，跟組提示詞同一份程式）

// 版型縮圖（30×42 的格子示意，閱讀順序右上先）
const R = (x, y, w, h) => `<rect x="${x}" y="${y}" width="${w}" height="${h}"/>`;
const THUMB = {
  L3: R(16, 1, 13, 40) + R(1, 1, 14, 19.5) + R(1, 21.5, 14, 19.5),
  L4: R(15.75, 1, 13.25, 11) + R(1, 1, 13.25, 11) + R(1, 13.5, 28, 27.5) + '<polygon points="18,30.5 29,28.5 29,41 19.5,41"/>',
  L4w: R(1, 1, 28, 10) + R(15.75, 12.5, 13.25, 12) + R(1, 12.5, 13.25, 12) + R(1, 26, 28, 15),
  L5: R(1, 1, 28, 12) + R(15.75, 14.5, 13.25, 12.5) + R(1, 14.5, 13.25, 12.5) + R(15.75, 28.5, 13.25, 12.5) + R(1, 28.5, 13.25, 12.5),
  L6: [1, 14.67, 28.33].map((y) => R(15.75, y, 13.25, 12.67) + R(1, y, 13.25, 12.67)).join(""),
  K4: [1, 11.25, 21.5, 31.75].map((y) => R(1, y, 28, 9.25)).join(""),
  CUSTOM: R(1, 1, 13.5, 13.5) + R(15.5, 1, 13.5, 13.5) + R(1, 16.5, 13.5, 24.5) + R(15.5, 16.5, 13.5, 24.5),
};
const SFX = ["ドキドキ", "ズキュン", "チュッ", "ギュッ", "ビクッ", "スリッ", "ズチュッ", "パンッ", "ズプッ", "ビュルッ", "ズバッ", "ドーン", "ゴゴゴ", "バンッ", "シーン"];

// 實測過的範例（design\漫畫頁提示詞設計.md）；套用時保留作品名、模型和觸發詞
const EXAMPLES = [
  { name: "奇幻戰鬥（全年齡）", layout: "L4w",
    cast: "The main character is a mature woman knight in plate armor and a flowing cape, with long grey hair in a ponytail and blue eyes, holding a sword. The dragon is huge and black.",
    light: "Orange firelight and drifting embers against a dark smoky sky.", mood: "epic fantasy battle",
    panels: [
      { shot: "a wide shot", what: "the grey-haired woman knight standing in burning castle ruins, raising her sword at a huge black dragon above her", line: "就是你吃了村裡的羊？", bubble: "top left", sfx: "" },
      { shot: "a close-up", what: "the dragon's huge face, smoke curling from its nostrils, glaring down", line: "區區人類……", bubble: "top right", sfx: "" },
      { shot: "a close-up", what: "the knight smirking confidently, her blue eyes shining", line: "我只是來收帳的。", bubble: "top left", sfx: "" },
      { shot: "a dynamic wide shot", what: "the knight leaping into the air and slashing at the dragon as it breathes a torrent of fire at her", line: "", bubble: "top", sfx: "ズバッ" }] },
  { name: "搞笑四格（全年齡）", layout: "K4",
    cast: "The main character is a young office worker in her twenties with messy black shoulder-length hair, wearing pastel pajamas, later a rumpled gray office suit.",
    light: "Bright morning sunlight, then flat office light.", mood: "light, silly slice-of-life comedy",
    panels: [
      { shot: "a medium shot", what: "the woman asleep in bed hugging a pillow, drooling, while a small alarm clock on the nightstand shows 8:50", line: "再五分鐘……", bubble: "top right", sfx: "" },
      { shot: "a close-up", what: "her eyes snapping open in shock as she stares at the alarm clock", line: "八點五十！？", bubble: "top left", sfx: "" },
      { shot: "a dynamic wide shot", what: "her sprinting down the street with toast in her mouth, her suit half buttoned and one shoe missing", line: "要遲到了啦！", bubble: "top right", sfx: "" },
      { shot: "a medium shot", what: "her collapsing at an empty office desk next to a wall calendar circled on Saturday, with a blank stare", line: "今天……星期六。", bubble: "top left", sfx: "" }] },
  { name: "成人：魅魔騎乘", layout: "L3",
    cast: "The succubus is an adult woman with long pink hair, yellow slit-pupil eyes, fangs, demon horns, demon wings and a tail, and a curvy nude body. The man is a young adult with short black hair.",
    light: "Flickering purple candlelight on sweaty skin in a dark bedroom.", mood: "steamy, erotic",
    panels: [
      { shot: "a full-body view", what: "the naked pink-haired succubus riding the man in the cowgirl position, sitting upright on his hips with her hands on his chest, wings spread wide, looking down at him with a hungry smile and flushed face", line: "你的精氣，我全都要～", bubble: "top left", sfx: "" },
      { shot: "a close-up", what: "the man lying on his back, sweating and gritting his teeth", line: "等、等一下……！", bubble: "top right", sfx: "" },
      { shot: "an extreme close-up", what: "his penis inside her pussy, dripping wet, her tail curling around his thigh", line: "", bubble: "top", sfx: "ズチュッ" }] },
  { name: "成人：辦公室挑逗", layout: "L4",
    cast: "The woman is an adult office worker with long black hair, red eyes and glasses, in a white shirt, a pencil skirt and black pantyhose. The man is a young adult in a suit with a loosened necktie.",
    light: "Cold blue moonlight through the office windows and the warm glow of a single desk lamp.", mood: "flirty, tense late-night office romance",
    panels: [
      { shot: "a wide shot", what: "the black-haired woman in glasses leaning over the man's desk in the dark empty office, smiling as he looks up from his laptop", line: "辛苦了，前輩♡", bubble: "top left", sfx: "" },
      { shot: "a close-up", what: "her fingers slowly unbuttoning her white shirt, showing her cleavage", line: "只剩我們了喔", bubble: "top right", sfx: "" },
      { shot: "a low-angle view", what: "her sitting on the desk with her legs crossed, pulling his necktie toward her with a seductive smile and half-closed eyes, seen from the man's point of view", line: "今晚……要不要加點別的班？", bubble: "top left", sfx: "" },
      { shot: "an extreme close-up", what: "her foot in black pantyhose pressing on the bulge in his trousers", line: "", bubble: "top", sfx: "スリッ" }] },
];

const blankPanel = () => ({ shot: "a medium shot", what: "", line: "", bubble: "top", sfx: "" });
const layoutOf = (id) => META?.layouts.find((l) => l.id === id) || META?.layouts[0];

// ---------------------------------------------------------------- 自訂版面（排版工作台：comiclayout.js）
/** 自訂版面一定是工作台的格式：舊作品的列／格轉成樹（閱讀順序不變，寫好的格子不用搬） */
function freeCustom() {
  if (!CL.isFree(P.custom)) P.custom = Array.isArray(P.custom?.rows) && P.custom.rows.length ? CL.rowsToTree(P.custom.rows) : CL.blank();
  return P.custom;
}
/** 這一頁有幾格（自訂版面自己算，不靠後端） */
function layoutCount() {
  if (!P) return 0;
  if (P.layout === "custom") return CL.count(freeCustom());
  return layoutOf(P.layout)?.slots.length || 0;
}
const n = () => layoutCount();

/** 每一格的中文位置名：預設版型用後端附的清單；自訂版面問後端（跟組提示詞同一份程式）。晚回的舊結果不蓋新的 */
let slotSeq = 0;
async function refreshSlots() {
  if (!P) return;
  const seq = ++slotSeq;
  if (P.layout !== "custom") {
    SLOTS = layoutOf(P.layout)?.slots || [];
    return;
  }
  freeCustom();
  try {
    const r = await api("/api/comic/layout", { json: { project: payload() } });
    if (seq !== slotSeq) return;
    SLOTS = r.slots.map((s) => s.zh);
    if (F("cmWireInfo")) F("cmWireInfo").textContent = r.desc;
  } catch (e) {
    if (seq !== slotSeq) return;
    SLOTS = [];
    if (F("cmWireInfo")) F("cmWireInfo").textContent = e.message;
  }
  bench?.refresh();
}

// ---------------------------------------------------------------- 載入
export async function show() {
  shown = true;
  if (!META) {
    try { META = await api("/api/comic/meta"); } catch (e) { return toast("讀不到漫畫台設定：" + e.message, "err"); }
    fillStatic();
  }
  await loadProjects(store.get("cmProject", null));
}

function fillStatic() {
  F("cmLayouts").replaceChildren(
    ...META.layouts.map((l) => el("button", { type: "button", class: "cm-lay", "data-id": l.id, title: l.name,
      onclick: () => setLayout(l.id) }, svgThumb(l.id), el("span", {}, l.id))),
    el("button", { type: "button", class: "cm-lay", "data-id": "custom", title: "自己排：切格、拉大小、疊小格、斜切、破格，也可以用說的請 AI 排",
      onclick: () => setLayout("custom") }, svgThumb("CUSTOM"), el("span", {}, "自訂")));
  F("cmExample").append(...EXAMPLES.map((x, i) => el("option", { value: String(i) }, x.name)));
  const dl = el("datalist", { id: "cmSfxList" }, ...SFX.map((s) => el("option", { value: s })));
  document.body.append(dl);
}

function svgThumb(id) {
  const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  s.setAttribute("viewBox", "0 0 30 42");
  s.setAttribute("aria-hidden", "true");
  s.innerHTML = THUMB[id] || "";
  return s;
}

async function loadProjects(want) {
  const r = await api("/api/comic/projects");
  let list = r.projects || [];
  if (!list.length) {
    const p = await api("/api/comic/project/new", { json: { title: "第一頁漫畫" } });
    list = [{ id: p.id, title: p.title }];
  }
  const sel = F("cmProj");
  sel.replaceChildren(...list.map((p) => el("option", { value: p.id }, p.title)));
  const id = list.some((p) => p.id === want) ? want : list[0].id;
  sel.value = id;
  await openProject(id);
}

// 換作品時晚回的舊回應不能蓋掉現在選的作品（重讀另外由 reloadSeq 擋）
let openSeq = 0;
async function openProject(id) {
  const seq = ++openSeq;
  ++reloadSeq;
  const p = await api(`/api/comic/project/${id}`);
  if (seq !== openSeq) return;
  P = p;
  store.set("cmProject", id);
  await fillForm();
  renderRuns();
  setRunning(!!P.running);
}

// 事件一來就重讀；回應晚到的舊結果不要蓋掉新的（不然畫完了按鈕還是停用）
let reloadSeq = 0;
async function reload() {
  if (!P) return;
  const seq = ++reloadSeq;
  try { const p = await api(`/api/comic/project/${P.id}`); if (seq !== reloadSeq || p.id !== P.id) return; P.runs = p.runs; setRunning(!!p.running); } catch { return; }
  renderRuns();
}

// ---------------------------------------------------------------- 左邊：表單
async function fillForm() {
  F("cmTrigger").value = P.trigger || "";
  F("cmCast").value = P.cast || "";
  F("cmLight").value = P.light || "";
  F("cmMood").value = P.mood || "";
  F("cmScript").value = P.script || "";
  F("cmCount").value = String(Math.min(4, P.count || 4));
  layoutUi();
  await refreshSlots();
  ensurePanels();
  layoutUi();
  renderPanels();
  renderWire();
  imgInfo();
  preview();
}

function layoutUi() {
  F("cmLayouts").querySelectorAll(".cm-lay").forEach((b) => b.classList.toggle("on", b.dataset.id === P.layout));
  F("cmLayoutName").textContent = P.layout === "custom" ? "自己排的版面" : layoutOf(P.layout)?.name || "";
  if (F("cmCustomBox")) F("cmCustomBox").hidden = P.layout !== "custom";
}

function ensurePanels() {
  const k = Math.max(n(), 1);
  P.panels = P.panels || [];
  while (P.panels.length < k) P.panels.push(blankPanel());
}

async function setLayout(id) {
  if (running || id === P.layout) return;
  // 第一次切到自訂：從剛剛的版型開始改（格子內容照順序對得上）；以前排過的自訂版面照舊留著
  if (id === "custom" && !CL.isFree(P.custom) && !P.custom?.rows?.length) P.custom = CL.presetCustom(P.layout) || CL.blank();
  P.layout = id;
  await refreshSlots();
  // 格數變了：留著前面寫好的，不夠的補空白；多出來的先藏起來（換回來還在）
  ensurePanels();
  layoutUi();
  renderPanels();
  renderWire();
  changed();
}

// ---------------------------------------------------------------- 自訂版面：示意圖＋排版工作台＋用說的排
let bench = null;   // 開著的排版工作台
function renderWire() {
  const box = F("cmWire");
  const ctl = F("cmWireCtl");
  if (!box || !ctl) return;
  if (!P || P.layout !== "custom") {
    box.replaceChildren();
    ctl.replaceChildren();
    return;
  }
  const custom = freeCustom();
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "這一頁的版面示意圖");
  const g = CL.draw(svg, custom);
  // 點一格跳到那一格的內容；點空白處打開工作台
  svg.addEventListener("click", (e) => {
    const id = e.target.closest?.("[data-id]")?.dataset.id;
    const i = id ? g.cells.findIndex((c) => c.id === id) : -1;
    if (i < 0) return openBench();
    F("cmPanels").querySelectorAll(".cm-panel")[i]?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  });
  box.replaceChildren(svg);
  ctl.replaceChildren(
    el("div", { class: "row gap wrap" },
      el("button", { type: "button", class: "akey sm go", disabled: running, onclick: () => openBench() }, "打開排版工作台"),
      el("span", { class: "muted sm" }, `${g.cells.length}／${CL.MAX_PANELS} 格`)),
    CL.aiBox(benchHost(), (c) => commitCustom(c, { byOrder: true })));
  if (running) ctl.querySelectorAll("input, select, button").forEach((x) => (x.disabled = true));
}

/** 套上新版面。byOrder＝格子內容照順序（換版型、AI 排的）；不然照格子 id 搬（切、併、疊小格時寫好的內容跟著格子走） */
function commitCustom(next, { byOrder = false } = {}) {
  if (!P || running) return;
  if (!byOrder) P.panels = CL.remap(CL.readingIds(freeCustom()), CL.readingIds(next), P.panels, blankPanel);
  P.custom = next;
  wireChanged();
}

function wireChanged() {
  ensurePanels();
  renderWire();
  renderPanels();
  changed();
  refreshSlots().then(() => { renderPanels(); renderWire(); });
}

function benchHost() {
  return {
    get: () => ({ custom: freeCustom(), panels: P.panels }),
    commit: commitCustom,
    restore: (s) => { if (!P || running) return; P.custom = s.custom; P.panels = s.panels; wireChanged(); },
    names: () => SLOTS,
    ai: async (ask, brain) => (await api("/api/comic/layout/ai", { json: { project: payload(), ask, brain }, timeout: 0 })).custom,
    locked: () => running,
    layouts: META?.layouts || [],
    onClose: () => { bench = null; },
  };
}

function openBench() {
  if (!P || P.layout !== "custom") return;
  bench = CL.openWorkbench(benchHost());
}

function renderPanels() {
  const box = F("cmPanels");
  box.innerHTML = "";
  const slots = SLOTS.length ? SLOTS : Array.from({ length: n() }, (_, i) => `第 ${i + 1} 格`);
  slots.forEach((slot, i) => {
    const c = P.panels[i] || (P.panels[i] = blankPanel());
    const sel = (opts, val, on) => {
      const s = el("select", {}, ...opts.map(([v, t]) => el("option", { value: v }, t)));
      s.value = val; s.disabled = running; s.onchange = () => { on(s.value); changed(); };
      return s;
    };
    const inp = (tag, val, attrs, on) => {
      const t = el(tag, { ...attrs, disabled: running });
      t.value = val || ""; t.oninput = () => { on(t.value); changed(); };
      return t;
    };
    box.append(el("article", { class: "cm-panel" },
      el("div", { class: "cm-panel-hd" }, el("span", { class: "vfd amb dir-no" }, String(i + 1).padStart(2, "0")), el("b", {}, slot),
        sel(META.shots, c.shot || "a medium shot", (v) => (c.shot = v))),
      inp("textarea", c.what, { rows: 2, spellcheck: "false", placeholder: i === 0 ? "這格畫什麼（英文最準），例：the woman leaning over his desk, smiling as he looks up" : "這格畫什麼（英文最準）" }, (v) => (c.what = v)),
      el("div", { class: "cm-line" },
        inp("input", c.line, { type: "text", placeholder: "台詞（中文，可空）" }, (v) => (c.line = v)),
        sel(META.bubbles.map(([en, zh]) => [en, `框在${zh}`]), c.bubble || "top", (v) => (c.bubble = v)),
        inp("input", c.sfx, { type: "text", list: "cmSfxList", placeholder: "狀聲詞（可空）", class: "cm-sfx" }, (v) => (c.sfx = v)))));
  });
}

function imgInfo() {
  const im = P.image;
  let t;
  if (!im) t = "自動：挑 MOSS-Image（25 步、CFG 3）；不掛 LoRA。";
  else if (im.family !== "moss") t = `生成頁帶來的是 ${im.model}，漫畫台只用 MOSS-Image，會自動挑 MOSS 模型。`;
  else t = `${im.model}（MOSS-Image）・${im.steps} 步・CFG ${im.cfg}${im.loras?.length ? `・LoRA：${im.loras.map((l) => `${l.name.split(/[\\/]/).pop().replace(/\.safetensors$/, "")} ${l.strength}`).join("、")}` : ""}`;
  F("cmImgInfo").textContent = t;
}

function readForm() {
  if (!P) return;
  Object.assign(P, { trigger: F("cmTrigger").value, cast: F("cmCast").value, light: F("cmLight").value, mood: F("cmMood").value, script: F("cmScript").value, count: +F("cmCount").value });
  changed();
}

const payload = () => ({ ...P, panels: (P.panels || []).slice(0, Math.max(n(), 1)) });

function changed() {
  saveSoon();
  previewSoon();
  const vis = P.panels.slice(0, n());
  const orphan = vis.filter((c) => !c.what?.trim() && (c.line?.trim() || c.sfx?.trim())).length;
  F("cmCountInfo").textContent = `${vis.filter((c) => c.what?.trim()).length} / ${n()} 格有內容`
    + (orphan ? `（${orphan} 格只寫了台詞或狀聲詞：沒寫內容的格不會畫，字也不會填）` : "");
}

const saveSoon = debounce(() => save(), 600);
async function save() {
  if (!P) return;
  const body = { id: P.id };
  for (const k of ["title", "layout", "trigger", "cast", "light", "mood", "panels", "custom", "script", "image", "count"]) body[k] = P[k];
  try { await api("/api/comic/project", { json: body }); } catch (e) { toast("作品存檔失敗：" + e.message, "err"); }
}

const previewSoon = debounce(() => preview(), 400);
async function preview() {
  if (!P) return;
  try {
    const r = await api("/api/comic/prompts", { json: { project: payload() } });
    F("cmPrPage").value = r.page || "";
  } catch (e) { F("cmPrPage").value = e.message; }
}

// ---------------------------------------------------------------- 右邊：結果
function openRec(id) {
  api(`/api/image/${id}`).then((r) => openLightbox([r], 0)).catch((e) => toast(e.message, "err"));
}

function renderRuns() {
  const box = F("cmRuns");
  box.innerHTML = "";
  const runs = P?.runs || [];
  if (!runs.length) {
    box.append(el("div", { class: "muted pad" }, "還沒畫過。左邊選版型、寫好每一格，按「畫這一頁」。不知道怎麼寫？上面「套用範例」挑一個先跑跑看。"));
    return;
  }
  for (const r of runs) {
    // r.flow "a"＝舊版 Anima 畫完再填字的紀錄（page 是沒字的整頁、fill 是填好字的）；其他＝MOSS 一次畫完（字在整頁裡）
    const pic = (id, label) => el("button", { type: "button", class: "cm-pic", title: `${label}（點一下看大圖）`, onclick: () => openRec(id) },
      el("img", { src: `/thumb/${id}`, alt: label, loading: "lazy" }), el("span", { class: "cm-tag" }, label));
    box.append(el("article", { class: "cm-run" },
      el("div", { class: "cm-pics" }, pic(r.page, r.flow === "a" ? "整頁" : "整頁（含字）"), r.fill ? pic(r.fill, "填好字") : null),
      el("div", { class: "row gap wrap" },
        el("span", { class: "muted sm grow" }, r.seed != null ? `種子 ${r.seed}` : ""),
        el("button", { type: "button", class: "minikey", title: "從這個清單拿掉（圖還在圖庫）", "aria-label": "從清單拿掉",
          onclick: () => removeRun(r.page) }, "✕"))));
  }
}

async function removeRun(page) {
  try { await api("/api/comic/run/remove", { json: { id: P.id, page } }); await reload(); } catch (e) { toast(e.message, "err"); }
}

// ---------------------------------------------------------------- 背景工作
function setRunning(on) {
  running = on;
  F("cmStop").hidden = !on;
  for (const id of ["cmRun", "cmNew", "cmDel", "cmRename", "cmProj", "cmExample", "cmImgUse", "cmImgAuto"]) F(id).disabled = on;
  // 畫的時候改了也不會影響這一輪（提示詞已經組好送出），乾脆鎖住免得以為有生效
  for (const id of ["cmTrigger", "cmCast", "cmLight", "cmMood", "cmScript", "cmScriptGo", "cmCount"]) F(id).disabled = on;
  for (const box of ["cmPanels", "cmLayouts"]) F(box).querySelectorAll("input, textarea, select, button").forEach((x) => (x.disabled = on));
  if (P?.layout === "custom") renderWire();
  bench?.refresh();
  if (!on && F("cmStatus").classList.contains("amb")) status("待機");
  renderRuns();
}

function status(msg, cls = "g") {
  const s = F("cmStatus");
  s.textContent = msg;
  s.className = `vfd ${cls} dir-status`;
}

async function run() {
  if (!P) return;
  await save();
  // 狀態先寫：後端的進度事件可能比這個請求的回應先到，寫在後面會把它蓋掉
  status("開始畫…", "amb");
  try {
    await api("/api/comic/run", { json: { id: P.id, count: +F("cmCount").value } });
    setRunning(true);
    reload(); // 萬一一開始就失敗、end 事件已經先到了，照後端的狀態解除
  } catch (e) { status("待機"); toast(e.message, "err"); }
}

/** websocket：main.js 收到 type=comic 轉給這裡 */
export function onEvent(m) {
  if (!P || m.id !== P.id) return;
  if (m.phase === "end") { status(m.msg || "完成", "g"); toast(`漫畫台：${m.msg || "完成"}`, "ok"); setRunning(false); openProject(P.id); return; }
  if (m.phase === "error") { status("失敗", "r"); toast(`漫畫台：${m.msg}`, "err"); setRunning(false); reload(); return; }
  status(m.msg || m.phase, "amb");
  if (m.phase === "page") reload();
}

// ---------------------------------------------------------------- 初始化
export function init() {
  // 漫畫：外觀文字和 tag 接在角色外貌後面
  drawer.mount(F("cmAssets"), { family: () => "comic", insert: drawer.appendTo(F("cmCast")) });
  F("cmProj").onchange = async () => { await save(); openProject(F("cmProj").value); };
  F("cmNew").onclick = async () => {
    const title = prompt("新作品的名字", "");
    if (title === null) return;
    await save();
    const p = await api("/api/comic/project/new", { json: { title, copy_from: P?.id } });
    await loadProjects(p.id);
  };
  F("cmRename").onclick = async () => {
    const t = prompt("作品名字", P?.title || "");
    if (!t?.trim()) return;
    P.title = t.trim();
    await save();
    await loadProjects(P.id);
  };
  F("cmDel").onclick = async () => {
    if (!P || !confirm(`把作品「${P.title}」移到待刪除？（畫好的圖還在圖庫）`)) return;
    try { await api("/api/comic/project/delete", { json: { id: P.id } }); await loadProjects(null); } catch (e) { toast(e.message, "err"); }
  };
  F("cmExample").onchange = async () => {
    const x = EXAMPLES[+F("cmExample").value];
    F("cmExample").value = "";
    if (!x || running) return;
    if (P.panels.some((c) => c.what?.trim()) && !confirm(`套用範例「${x.name}」會蓋掉現在寫的內容。繼續？`)) return;
    Object.assign(P, { layout: x.layout, cast: x.cast || "", light: x.light || "", mood: x.mood || "", panels: x.panels.map((c) => ({ ...c })) });
    await fillForm();
    changed();
  };
  // cmScript（劇情）也要聽：2026-11-10 以前漏了，只打劇情不動別的欄位就不會存，重開作品劇情是空的
  for (const id of ["cmTrigger", "cmCast", "cmLight", "cmMood", "cmScript"]) F(id).addEventListener("input", readForm);
  F("cmCount").addEventListener("change", readForm);
  F("cmImgUse").onclick = () => {
    const b = gen.baseParams();
    if (!b.model) return toast("生成頁還沒選模型", "err");
    if (b.family === "custom") return toast("生圖工作流只能在生成頁用：先在生成頁換成一般模型", "err");
    P.image = b;
    // 觸發詞空著就帶生成頁掛的 LoRA 的觸發詞（jgdx 之類的畫風 LoRA 要有觸發詞才穩）
    if (!F("cmTrigger").value.trim()) { const t = gen.triggerWords(); if (t.length) F("cmTrigger").value = t.join(", "); }
    readForm();
    imgInfo();
    toast("畫整頁會用：" + b.model, "ok");
  };
  F("cmImgAuto").onclick = () => { P.image = null; imgInfo(); changed(); };
  // AI 排分鏡：35B 照劇情把每格寫好（背景跑，回來走 type=comic 的 script／end 事件）
  F("cmScriptGo").onclick = async () => {
    if (!P || running) return;
    const story = F("cmScript").value.trim();
    if (!story) return toast("先在上面寫這段劇情（中文就好）", "err");
    const fillOnly = F("cmScriptFill").checked;
    const vis = (P.panels || []).slice(0, Math.max(n(), 1));
    if (fillOnly && !vis.some((c) => !c.what?.trim())) return toast("這一頁的格子都寫好了，沒有空白格可以填", "err");
    if (!fillOnly && vis.some((c) => c.what?.trim()) && !confirm("會蓋掉這一頁已經寫好的格子。繼續？")) return;
    P.script = F("cmScript").value;
    await save();
    status("排分鏡中…", "amb");
    try {
      await api("/api/comic/script", { json: { id: P.id, story, mode: fillOnly ? "fill" : "all", brain: F("cmScriptBrain").value } });
      setRunning(true);
      reload(); // 跟 run() 一樣：一開始就失敗時 error 事件可能比這個回應先到，照後端的狀態解除，不然會一直卡在執行中
    } catch (e) { status("待機"); toast(e.message, "err"); }
  };
  F("cmCopyPage").onclick = () => copyText(F("cmPrPage").value).then(() => toast("已複製整頁提示詞", "ok"));
  F("cmRun").onclick = () => run();
  F("cmStop").onclick = async () => { await api("/api/comic/stop", { json: { id: P.id } }); status("停止中…", "amb"); };
}

export const isShown = () => shown;

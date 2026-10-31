// 導演台：長劇情 → AI 分段 → 每段第一格＋圖生影片 → 接成長片；角色設定卡（多視圖設定圖）
// 後端在 src-tauri\src\server\api_director.rs；背景工作的進度從 websocket 收 type=director／director_llm
import { $, api, el, toast, debounce, openModal, pickFile, uploadFile, store } from "./util.js";
import { pickFromGallery, openLightbox } from "./gallery.js";
import * as gen from "./gen.js";
import { layoutEditor } from "./layout.js";
import { mmprojFix } from "./pe.js";
import * as drawer from "./assetdrawer.js";
import { renderTakePanel } from "./film-takes.js";
import { mediaSlotsControl } from "./video-slots.js";

const F = (id) => document.getElementById(id);
const CAMERA = [
  ["緩慢推鏡", "slow push-in"], ["拉遠揭示", "slow pull-back reveal"], ["橫移跟拍", "lateral tracking shot"], ["升降鏡頭", "crane shot"],
  ["環繞", "orbit around the subject"], ["手持晃動", "handheld camera"], ["固定鏡頭", "static locked-off camera"], ["第一人稱", "POV shot"],
  ["低角度仰拍", "low-angle shot"], ["高角度俯拍", "high-angle shot"], ["特寫", "close-up"], ["過肩", "over-the-shoulder shot"],
];
const LIGHT = [
  ["黃金時刻", "golden hour sunlight"], ["藍調時刻", "blue hour"], ["霓虹", "neon lights"], ["燭火暖光", "warm candlelight"],
  ["月光冷色", "cold moonlight"], ["逆光輪廓", "backlit rim light"], ["體積光", "volumetric light rays"], ["陰天柔光", "soft overcast light"],
  ["黑色電影", "film noir high contrast"], ["雨夜反光", "rainy night reflections"], ["丁達爾效應", "Tyndall effect god rays"], ["火光", "flickering firelight"],
];

let P = null;        // 目前的專案
let chars = [];      // 角色卡
let running = false; // 目前專案有背景工作在跑
let shown = false;
let videoChoices = [], defaultVideoEngine = "h3";
const slotControls = new Map();

// ---------------------------------------------------------------- 載入
export async function show() {
  shown = true;
  await loadChars();
  await loadProjects(store.get("dirProject", null));
}

async function loadChars() {
  try { chars = (await api("/api/director/chars")).chars || []; } catch (e) { toast("讀角色卡失敗：" + e.message, "err"); }
  renderCharChips();
}

async function loadProjects(want) {
  const r = await api("/api/director/projects");
  let list = r.projects || [];
  if (!list.length) {
    const p = await api("/api/director/project/new", { json: { title: "第一部片" } });
    list = [{ id: p.id, title: p.title }];
  }
  const sel = F("dirProj");
  sel.innerHTML = "";
  for (const p of list) sel.append(el("option", { value: p.id }, `${p.title}${p.segments ? `（${p.segments} 段）` : ""}`));
  const id = list.some((p) => p.id === want) ? want : list[0].id;
  sel.value = id;
  await openProject(id);
}

// 換專案、重讀都可能晚回：舊請求的回應不能蓋掉現在選的專案（不然存檔會存到別的專案）
let openSeq = 0, reloadSeq = 0;
export async function openProject(id) {
  const seq = ++openSeq;
  ++reloadSeq; // 換專案：還在路上的重讀一律作廢
  const [p, catalog] = await Promise.all([api(`/api/director/project/${id}`), api("/api/video/workflows").catch(() => null)]);
  if (seq !== openSeq) return;
  P = p;
  // 2026-10-30 LTX 退場：舊專案、舊段落記的 ltx23 就是內建 H3（下次存檔寫回去）
  const legacy = (o) => { if (o?.video_engine === "ltx23") o.video_engine = "h3"; if (o?.engine === "ltx23") o.engine = "h3"; };
  legacy(P.settings);
  (P.segments || []).forEach(legacy);
  if (catalog) {
    defaultVideoEngine = catalog.engine || "h3";
    videoChoices = [{ id: catalog.builtin?.id || "h3", name: gen.H3_NAME }, ...(catalog.workflows || [])];
  }
  const selector = F("dirProj");
  if (![...(selector.options || [])].some((o) => o.value === id)) selector.append(el("option", { value: id }, p.title || "未命名"));
  selector.value = id;
  running = !!P.running;
  store.set("dirProject", id);
  fillForm();
  const engine = P.settings?.video_engine || defaultVideoEngine;
  const choice = videoChoices.find((v) => v.id === engine);
  F("dirVidEngine").textContent = `本專案影片模型：${choice?.name || engine}・可在每鏡選模型與秒數`;
  renderSegs();
  setRunning(running);
}

async function reload() {
  if (!P) return;
  const seq = ++reloadSeq, os = openSeq, id = P.id;
  let p;
  try { p = await api(`/api/director/project/${id}`); } catch { return; }
  if (seq !== reloadSeq || os !== openSeq || P?.id !== id) return;
  P = p;
  running = !!P.running;
  setRunning(running);
  renderSegs();
}

// ---------------------------------------------------------------- 左邊：劇情與參数
function fillForm() {
  const s = P.settings || {};
  F("dirStory").value = P.story || "";
  F("dirSegN").value = String(s.segments || 0);
  const seconds = String(s.seconds || 5);
  if (![...(F("dirSecs").options || [])].some((o) => o.value === seconds)) F("dirSecs").append(el("option", { value: seconds }, `${seconds} 秒`));
  F("dirSecs").value = seconds;
  F("dirRes").value = s.res || "720p";
  F("dirWhere").value = s.h3_where === "cloud" ? "cloud" : "local";
  F("dirStyle").value = s.style || "anime";
  F("dirVisual").value = s.visual || "";
  F("dirExtra").value = s.extra || "";
  if (F("dirReview")) F("dirReview").checked = s.review !== false;
  renderChips("dirCamera", CAMERA, s.camera || []);
  renderChips("dirLight", LIGHT, s.light || []);
  renderCharChips();
  imgInfo();
  storyCount();
}

function renderChips(id, list, on) {
  const box = F(id);
  box.innerHTML = "";
  for (const [zh, en] of list) {
    box.append(el("button", { type: "button", class: "chip" + (on.includes(en) ? " on" : ""), title: en, "data-v": en,
      onclick: (e) => { if (running) return; e.currentTarget.classList.toggle("on"); readForm(); } }, zh));
  }
}

function renderCharChips() {
  const box = F("dirChars");
  if (!box) return;
  box.innerHTML = "";
  const on = P?.settings?.characters || [];
  if (!chars.length) box.append(el("span", { class: "muted sm" }, "還沒有角色卡：按「管理角色…」新增"));
  for (const c of chars) {
    box.append(el("button", { type: "button", class: "chip" + (on.includes(c.id) ? " on" : ""), "data-id": c.id,
      title: (c.sheet ? "有多視圖設定圖。" : "沒有設定圖。") + (c.desc || ""),
      onclick: (e) => { if (running) return; e.currentTarget.classList.toggle("on"); readForm(); } }, c.name + (c.sheet ? " ◆" : "")));
  }
}

function imgInfo() {
  const im = P.settings?.image;
  const fam = { qwen21: "Qwen 2.1", moss: "MOSS-Image", anima: "Anima" }[im?.family] || im?.family || "";
  F("dirImgInfo").textContent = im
    ? `${im.model}（${fam}）・${im.steps} 步・CFG ${im.cfg}${im.loras?.length ? `・LoRA ${im.loras.length} 個` : ""}${im.family === "qwen21" ? "・角色有設定圖時用多圖參考" : "・角色長相靠外觀描述"}${im.family === "moss" ? "・每段可以拉版面框" : ""}`
    : "還沒設定：先到「生成」頁選好模型和設定，再回來按下面的按鈕。";
}

function storyCount() {
  const n = F("dirStory").value.trim().length;
  const segs = +F("dirSegN").value || Math.min(24, Math.max(3, Math.floor(n / 120)));
  F("dirStoryN").textContent = n ? `${n} 字` : "";
  const secs = +F("dirSecs").value;
  // H3 實測：本機 8GB 顯卡 w4a8 512×768 5 秒約 3～4 分鐘；雲端 L40S 10 秒約 4～4.5 分鐘
  const cloud = F("dirWhere").value === "cloud";
  const per = cloud ? 0.45 * secs + 0.5 : (F("dirRes").value === "480p" ? 3.5 : 6) * (secs / 5) + 0.5;
  F("dirEst").textContent = n ? `${+F("dirSegN").value ? "" : "AI 自動大約 "}${segs} 段 × ${secs} 秒 ≈ ${segs * secs} 秒的片；全部生成約 ${Math.round(segs * per)} 分鐘（${cloud ? "雲端 L40S 實測 10 秒一段約 4 分鐘" : "8GB 顯卡實測 512×768 5 秒一段約 3～4 分鐘，720p 更久"}）` : "";
}

const picked = (id) => [...F(id).querySelectorAll(".chip.on")].map((b) => b.dataset.v || b.dataset.id);

function readForm() {
  if (!P) return;
  P.story = F("dirStory").value;
  P.settings = { ...(P.settings || {}),
    segments: +F("dirSegN").value, seconds: +F("dirSecs").value, res: F("dirRes").value, h3_where: F("dirWhere").value, style: F("dirStyle").value,
    visual: F("dirVisual").value, extra: F("dirExtra").value, camera: picked("dirCamera"), light: picked("dirLight"), characters: picked("dirChars"), review: F("dirReview")?.checked !== false };
  storyCount();
  saveSoon();
}

const saveSoon = debounce(() => save(), 600);
async function save() {
  if (!P || running) return false;
  const project = P, id = project.id;
  try {
    const r = await api("/api/director/project", { json: { id, title: project.title, story: project.story, settings: project.settings, segments: project.segments } });
    if (P?.id === id) P.updated = r.updated;
    return true;
  } catch (e) { toast("專案存檔失敗：" + e.message, "err"); return false; }
}

// ---------------------------------------------------------------- 右邊：段落
const STATUS = { running: ["生成中", "amb"], done: ["已採用", "g"], review: ["待審片", "amb"], todo: ["待生成", ""], error: ["失敗", "r"], stopped: ["已停止", "amb"] };

function thumbOf(seg) {
  if (seg.frame_id) return `/thumb/${seg.frame_id}`;
  if (seg.frame_lock) return `/input/${encodeURIComponent(seg.frame_lock)}`;
  if (seg.frame_image) return `/input/${encodeURIComponent(seg.frame_image)}`;
  return null;
}

function segCard(seg, i) {
  const [stTxt, stCls] = STATUS[seg.status] || (seg.video ? ["待生成", ""] : ["沒有提示詞", ""]);
  const ta = (key, rows, ph) => {
    const t = el("textarea", { rows, spellcheck: "false", placeholder: ph });
    t.value = seg[key] || "";
    t.disabled = running;
    t.oninput = () => { seg[key] = t.value; saveSoon(); };
    return t;
  };
  const cont = el("input", { type: "checkbox" });
  cont.checked = !!seg.continue; cont.disabled = running || i === 0;
  cont.onchange = () => { seg.continue = cont.checked; saveSoon(); renderSegs(); };
  const title = el("input", { type: "text", class: "dir-title", value: seg.title || "" });
  title.disabled = running;
  title.oninput = () => { seg.title = title.value; saveSoon(); };
  const summary = el("input", { type: "text", value: seg.summary || "", placeholder: "這段發生什麼（中文）" });
  summary.disabled = running;
  summary.oninput = () => { seg.summary = summary.value; saveSoon(); };

  const th = thumbOf(seg);
  const frameBox = el("div", { class: "dir-thumb", title: "第一格" },
    th ? el("img", { src: th, alt: "第一格", loading: "lazy" }) : el("span", { class: "muted sm" }, seg.continue && i > 0 ? "接上一段最後一格" : "第一格"));
  if (seg.frame_id) frameBox.onclick = () => openRec(seg.frame_id);
  const vidBox = el("div", { class: "dir-thumb vid", title: "影片" },
    seg.video_id ? el("video", { src: `/img/${seg.video_id}`, poster: `/thumb/${seg.video_id}`, controls: true, preload: "none", playsinline: true })
      : el("span", { class: "muted sm" }, "影片"));

  const btn = (label, fn, cls = "akey sm", tip = "") => el("button", { type: "button", class: cls, title: tip, disabled: running, onclick: fn }, label);
  const chips = (seg.characters || []).map((n) => el("span", { class: "badge-s" }, n));
  const engine = seg.engine || P.settings?.video_engine || defaultVideoEngine;
  const enginePick = el("select", { disabled: running, "aria-label": `第 ${i + 1} 鏡影片模型` },
    el("option", { value: "" }, "沿用專案影片模型"), ...videoChoices.map((v) => el("option", { value: v.id }, v.name || v.id)));
  if (seg.engine && !videoChoices.some((v) => v.id === seg.engine)) enginePick.append(el("option", { value: seg.engine }, seg.engine));
  enginePick.value = seg.engine || "";
  enginePick.onchange = () => { seg.engine = enginePick.value; saveSoon(); renderSegs(); };
  const seconds = el("input", { type: "number", min: "0.1", max: "120", step: "0.1", value: seg.seconds ?? P.settings?.seconds ?? 5, disabled: running, "aria-label": `第 ${i + 1} 鏡秒數`, style: "width:90px" });
  seconds.onchange = () => { const n = Number(seconds.value); if (n > 0 && Number.isFinite(n)) { seg.seconds = n; saveSoon(); } };
  const slots = mediaSlotsControl({ engine: videoChoices.find((v) => v.id === engine), values: seg.media_slots || {},
    isCurrent: () => !running && P?.segments?.[i] === seg && (seg.engine || P.settings?.video_engine || defaultVideoEngine) === engine,
    onchange: (values) => { seg.media_slots = values; saveSoon(); } });
  slotControls.set(i, slots);
  const slotBox = el("fieldset", { disabled: running, style: "border:0;padding:0;margin:0" }, slots.element);
  const card = el("article", { class: `dir-seg${seg.continue && i > 0 ? " cont" : ""}`, "data-i": i },
    el("div", { class: "dir-seg-hd" },
      el("span", { class: "vfd amb dir-no" }, String(i + 1).padStart(2, "0")), title,
      el("span", { class: `vfd ${stCls} dir-st` }, stTxt),
      el("label", { class: "check inline", title: "接上一段：直接拿上一段影片的最後一格當第一格（同一個場景、同一個時刻）" }, cont, " 接上一段")),
    summary,
    seg.error ? el("div", { class: "pe-status err" }, seg.error) : null,
    el("div", { class: "row gap wrap" }, ...chips, seg.scene ? el("span", { class: "muted sm" }, seg.scene) : null),
    el("div", { class: "row gap wrap" }, el("label", {}, "影片模型 ", enginePick), el("label", {}, " 秒數 ", seconds)),
    slotBox,
    el("div", { class: "dir-media" }, frameBox, vidBox),
    renderTakePanel({ seg, disabled: running, onSelect: (take) => selectTake(i, take) }),
    seg.draft ? el("details", { class: "dir-pr" }, el("summary", {}, "切版前保留的未拍草稿"),
      el("pre", { style: "white-space:pre-wrap;max-height:200px;overflow:auto" }, seg.draft.video || JSON.stringify(seg.draft, null, 2)),
      btn("套回這份草稿", () => { Object.assign(seg, seg.draft); save().then(renderSegs); }, "akey sm", "只還原待重拍的文字與規格，已採用的影片維持原版")) : null,
    el("details", { class: "dir-pr", open: !seg.video },
      el("summary", {}, "提示詞（可以直接改）"),
      el("span", { class: "sub" }, seg.continue && i > 0 ? "第一格（接上一段時不會用到）" : "第一格的畫面"), ta("frame", 3, "AI 寫好會出現在這裡"),
      seg.frame_zh ? el("div", { class: "muted sm zh" }, seg.frame_zh) : null,
      el("span", { class: "sub" }, "影片：動作、鏡頭、聲音"), ta("video", 4, "AI 寫好會出現在這裡"),
      seg.video_zh ? el("div", { class: "muted sm zh" }, seg.video_zh) : null),
    layBox(seg, i),
    el("div", { class: "row gap wrap" },
      btn("重寫提示詞", () => write("shot", i)),
      btn("只生這段", () => run({ only: i }), "akey sm go"),
      btn("從這段開始", () => run({ from: i, skip_done: false }), "akey sm", "從這段開始往後全部重做，最後接成長片"),
      btn(seg.frame_lock ? "換第一格…" : "指定第一格…", () => lockFrame(i), "akey sm", "從圖庫挑一張當這段的第一格（不讓 AI 畫）"),
      seg.video_id && seg.selected_take ? btn("選接續畫面", () => pickTail(i)) : null,
      seg.frame_lock ? btn("取消指定", () => { delete seg.frame_lock; save().then(renderSegs); }) : null));
  return card;
}

async function pickTail(index) {
  if (!P || running) return;
  const id = P.id, seq = openSeq;
  const review = await import("./video-review.js");
  if (P?.id !== id || openSeq !== seq) return;
  return review.pickTail({ id, seg: index, onDone: (p) => {
    if (P?.id !== id || openSeq !== seq) return;
    P = p; renderSegs();
  } });
}

async function selectTake(index, take_id) {
  if (!P || running || !(await save())) return;
  const id = P.id, seq = openSeq;
  try {
    const p = await api("/api/director/take/select", { json: { id, index, take_id } });
    if (P?.id !== id || openSeq !== seq) return;
    P = p; running = false; renderSegs();
    status("已採用，可以接著拍下一鏡");
  } catch (e) { toast(e.message, "err"); }
}

// 版面框（MOSS 專用）：拉框指定第一格裡誰、什麼放在哪裡。框跟著段落存（seg.layout），
// 畫第一格時後端照 FRAME_W×FRAME_H（api_director.rs，1344×768）換算；寫提示詞時也會告訴 AI 框的位置
const layOpen = new Set();
function layBox(seg, i) {
  // 接上一段、指定第一格的段不畫第一格，用不到框
  if (P.settings?.image?.family !== "moss" || (seg.continue && i > 0) || seg.frame_lock) return null;
  const cnt = el("span", { class: "vfd sm", hidden: !seg.layout?.length }, String(seg.layout?.length || 0).padStart(2, "0"));
  const ed = layoutEditor({ maxH: 230, disabled: () => running,
    onChange: (list) => { seg.layout = list; cnt.hidden = !list.length; cnt.textContent = String(list.length).padStart(2, "0"); saveSoon(); } });
  ed.setAspect(1344, 768);
  ed.set(seg.layout || []);
  const box = el("details", { class: "pe lay-wrap dir-lay", open: layOpen.has(i) },
    el("summary", {}, "版面框 ", cnt, el("span", { class: "muted sm" }, "MOSS：拉框指定第一格裡誰、什麼放在哪裡；按「重寫提示詞」讓 AI 照框寫")),
    ed.el);
  box.ontoggle = () => (box.open ? layOpen.add(i) : layOpen.delete(i));
  return box;
}

function renderSegs() {
  const box = F("dirSegs");
  box.innerHTML = "";
  slotControls.clear();
  const segs = P?.segments || [];
  const done = segs.filter((s) => s.status === "done").length;
  const total = segs.reduce((n, s) => n + Number(s.seconds ?? P.settings?.seconds ?? 5), 0);
  F("dirCount").textContent = segs.length ? `${segs.length} 段・完成 ${done}・約 ${Math.round(total * 10) / 10} 秒` : "";
  if (!segs.length) box.append(el("div", { class: "muted pad" }, "還沒有段落。左邊寫好劇情，按「AI 分段＋寫提示詞」。"));
  segs.forEach((s, i) => box.append(segCard(s, i)));
  const needsReview = segs.some((s) => !!s.pending_take);
  F("dirConcat").disabled = running || needsReview || segs.some((s) => !s.video_id || s.stale || s.status !== "done");
  F("dirRun").disabled = running || needsReview;
  const fin = F("dirFinal");
  fin.hidden = !P?.final?.id;
  fin.innerHTML = "";
  if (P?.final?.id) {
    fin.append(el("div", { class: "row gap" }, el("b", {}, "成片"), el("span", { class: "muted sm" }, `${P.final.segments || segs.length} 段接成一支，已存進圖庫`)),
      el("video", { src: `/img/${P.final.id}`, poster: `/thumb/${P.final.id}`, controls: true, preload: "metadata", playsinline: true }));
  }
}

async function openRec(id) {
  try { const r = await api(`/api/image/${id}`); openLightbox([r], 0); } catch (e) { toast(e.message, "err"); }
}

function lockFrame(i) {
  pickFromGallery(async (rec) => {
    try {
      const r = await api("/api/to_input", { json: { id: rec.id } });
      P.segments[i].frame_lock = r.name;
      await save();
      renderSegs();
    } catch (e) { toast(e.message, "err"); }
  });
}

// ---------------------------------------------------------------- 背景工作
function setRunning(on) {
  running = on;
  F("dirStop").hidden = !on;
  for (const id of ["dirRun", "dirConcat", "dirPlan", "dirShots", "dirImgUse", "dirNew", "dirDel", "dirRename", "dirProj"]) F(id).disabled = on;
  for (const id of ["dirStory", "dirSegN", "dirSecs", "dirRes", "dirWhere", "dirStyle", "dirVisual", "dirExtra", "dirReview"]) if (F(id)) F(id).disabled = on;
  if (!on && P) {
    const segs = P.segments || [];
    F("dirRun").disabled = segs.some((s) => !!s.pending_take);
    F("dirConcat").disabled = !segs.length || segs.some((s) => !!s.pending_take || !s.video_id || s.stale || s.status !== "done");
  }
  if (!on && F("dirStatus").classList.contains("amb")) status("待機");
}

function status(msg, cls = "g") {
  const s = F("dirStatus");
  s.textContent = msg;
  s.className = `vfd ${cls} dir-status`;
}

async function write(what, index) {
  if (!P) return;
  if (what === "plan" && P.segments?.length && !confirm("重新分段會清掉現有的段落和提示詞（已經生好的影片還在圖庫）。繼續？")) return;
  if (!(await save())) return;
  try {
    await api("/api/director/write", { json: { id: P.id, what, index } });
    status(what === "plan" ? "AI 分段中…" : "寫提示詞中…", "amb");
    await reload();
  } catch (e) { toast(e.message, "err"); }
}

async function run(opt = {}) {
  if (!P) return;
  try { for (const [i, control] of slotControls) P.segments[i].media_slots = control.read(); }
  catch (e) { toast(e.message, "err"); return; }
  if (!(await save())) return;
  const body = { id: P.id, review: F("dirReview")?.checked !== false, ...opt };
  if (opt.only === undefined && opt.skip_done === undefined) body.skip_done = !F("dirRedo").checked;
  try {
    await api("/api/director/run", { json: body });
    status("開始生成…", "amb");
    await reload();
  } catch (e) { toast(e.message, "err"); }
}

/** websocket：main.js 收到 type=director／director_llm 轉給這裡 */
export function onEvent(m) {
  if (!P) return;
  if (m.type === "director_llm") {
    if (running && m.rid === `director-${P.id}` && m.msg) status(m.msg, "amb");
    if (m.rid === "director-describe" && m.msg) F("dcStatus").textContent = m.msg;
    return;
  }
  if (m.id !== P.id) return;
  if (m.phase === "end") { status(m.msg || "完成", "g"); toast(`導演台：${m.msg || "完成"}`, "ok"); reload(); return; }
  if (m.phase === "error") {
    status("失敗", "r"); toast(`導演台：${m.msg}`, "err");
    F("dirStatus").parentElement.querySelector(".mmproj-fix")?.remove();
    const fx = mmprojFix(m.msg); // 缺讀圖用的 mmproj：狀態旁邊附上下載按鈕
    if (fx) F("dirStatus").after(fx);
    reload(); return;
  }
  status(m.msg || m.phase, "amb");
  if (["seg", "frame", "video", "concat"].includes(m.phase)) reload();
}

// ---------------------------------------------------------------- 角色設定卡
let cur = null;        // 編輯中的角色（未存的新角色 id 是空字串）

function openChars() {
  openModal("#dirCharModal");
  renderCharList();
  editChar(chars.find((c) => c.id === cur?.id) || chars[0] || null);
}

function renderCharList() {
  const box = F("dcList");
  box.innerHTML = "";
  for (const c of chars) {
    const pic = c.sheet_url || c.src_url;
    box.append(el("div", { class: "dir-charitem" + (cur?.id === c.id ? " on" : ""), onclick: () => editChar(c) },
      pic ? el("img", { src: pic, alt: "" }) : el("span", { class: "ph" }),
      el("div", {}, el("b", {}, c.name), el("div", { class: "muted sm" }, c.sheet ? "◆ 有設定圖" : "沒有設定圖"))));
  }
  if (!chars.length) box.append(el("div", { class: "muted sm" }, "還沒有角色"));
}

function picBox(id, url) {
  const b = F(id);
  b.innerHTML = "";
  b.append(url ? el("img", { src: url, alt: "" }) : el("span", { class: "muted sm" }, id === "dcSrc" ? "還沒有圖" : "還沒有"));
}

function editChar(c) {
  cur = c ? { ...c } : { id: "", name: "", desc: "", desc_zh: "" };
  F("dcName").value = cur.name || "";
  F("dcDesc").value = cur.desc || "";
  F("dcDescZh").value = cur.desc_zh || "";
  F("dcStatus").textContent = "";
  F("dcDel").hidden = !cur.id;
  picBox("dcSrc", cur.src_url);
  picBox("dcSheet", cur.sheet_url);
  renderCharList();
}

/** 原圖在 ComfyUI input 裡的檔名（新選的、或已存的） */
const srcInput = () => cur?.new_src || cur?.src_in || null;

async function setSrc(r, url) {
  cur.new_src = r.name; delete cur.new_src_id;
  picBox("dcSrc", url);
}

async function saveChar(extra = {}) {
  const name = F("dcName").value.trim();
  if (!name) { toast("角色要有名字", "err"); return null; }
  const body = { id: cur.id || undefined, name, desc: F("dcDesc").value, desc_zh: F("dcDescZh").value, ...extra };
  if (cur.new_src) body.src = cur.new_src;
  const r = await api("/api/director/chars", { json: body });
  await loadChars();
  const c = chars.find((x) => x.id === r.id);
  editChar(c);
  return c;
}

function initChars() {
  F("dirCharsOpen").onclick = () => openChars();
  F("dcNew").onclick = () => editChar(null);
  F("dcSrcUp").onclick = async () => {
    const f = await pickFile();
    if (!f) return;
    try { const r = await uploadFile(f); await setSrc(r, `/input/${encodeURIComponent(r.name)}`); } catch (e) { toast(e.message, "err"); }
  };
  F("dcSrcGal").onclick = () => pickFromGallery(async (rec) => {
    try { const r = await api("/api/to_input", { json: { id: rec.id } }); await setSrc(r, `/thumb/${rec.id}`); } catch (e) { toast(e.message, "err"); }
  });
  F("dcSheetGal").onclick = () => pickFromGallery(async (rec) => {
    try { await saveChar({ sheet_id: rec.id }); toast("設定圖換好了", "ok"); } catch (e) { toast(e.message, "err"); }
  });
  F("dcSheetClear").onclick = async () => { if (cur?.id) { await saveChar({ clear_sheet: true }); } };
  F("dcSave").onclick = async () => { try { if (await saveChar()) toast("角色存好了", "ok"); } catch (e) { toast(e.message, "err"); } };
  F("dcDel").onclick = async () => {
    if (!cur?.id || !confirm(`把「${cur.name}」移到待刪除？（角色存在資產庫：圖片和描述會搬到資料夾的 待刪除\\資產庫，不會真的刪掉）`)) return;
    try { await api("/api/director/chars/delete", { json: { id: cur.id } }); cur = null; await loadChars(); editChar(chars[0] || null); } catch (e) { toast(e.message, "err"); }
  };
  F("dcDescGo").onclick = async () => {
    const img = srcInput();
    if (!img) return toast("先放一張角色原圖", "err");
    F("dcStatus").textContent = "AI 看圖中…（第一次要載入模型）";
    F("dcStatus").className = "pe-status run";
    try {
      const r = await api("/api/director/describe", { json: { image: img } });
      F("dcDesc").value = r.desc || ""; F("dcDescZh").value = r.desc_zh || "";
      F("dcStatus").textContent = "寫好了，可以直接修改"; F("dcStatus").className = "pe-status ok";
    } catch (e) {
      F("dcStatus").textContent = e.message; F("dcStatus").className = "pe-status err";
      const fx = mmprojFix(e.message); // 缺讀圖用的 mmproj：附上下載按鈕
      if (fx) F("dcStatus").append(fx);
    }
  };
  // 多視圖設定圖：打開角色設定圖工坊（charsheet.js），做完存回這個角色
  F("dcSheetGo").onclick = async () => {
    if (!cur.id) { try { if (!(await saveChar())) return; } catch (e) { return toast(e.message, "err"); } }
    const c = chars.find((x) => x.id === cur.id);
    try {
      const it = (await api("/api/lib")).find((x) => x.id === c?.asset_id);
      if (!it) return toast("找不到這個角色的資產", "err");
      const id = cur.id;
      (await import("./charsheet.js")).open({ asset: it, onSaved: async () => { await loadChars(); editChar(chars.find((x) => x.id === id) || null); } });
    } catch (e) { toast(e.message, "err"); }
  };
}

// ---------------------------------------------------------------- 初始化
export function init() {
  if (!F("dirReview")) {
    const review = el("input", { type: "checkbox", id: "dirReview", checked: true });
    review.onchange = readForm;
    F("dirRun").parentElement.append(el("label", { class: "check inline", title: "每拍完一鏡先預覽並採用；取消勾選會自動採用新版本並連拍" }, review, " 逐鏡審片"));
  }
  // 導演分鏡：資產的外觀文字接在劇情後面
  drawer.mount(F("dirAssets"), { family: () => "h3", insert: drawer.appendTo(F("dirStory")) });
  F("dirSegN").append(...Array.from({ length: 29 }, (_, k) => el("option", { value: k + 2 }, `${k + 2} 段`)));
  F("dirProj").onchange = async () => { await save(); openProject(F("dirProj").value); };
  F("dirNew").onclick = async () => {
    const title = prompt("新專案的名字", "");
    if (title === null) return;
    await save();
    const p = await api("/api/director/project/new", { json: { title, copy_from: P?.id } });
    await loadProjects(p.id);
  };
  F("dirRename").onclick = async () => {
    const t = prompt("專案名字", P?.title || "");
    if (!t) return;
    P.title = t.trim();
    await save();
    await loadProjects(P.id);
  };
  F("dirDel").onclick = async () => {
    if (!P || !confirm(`把專案「${P.title}」移到待刪除？（生好的圖和影片還在圖庫）`)) return;
    try { await api("/api/director/project/delete", { json: { id: P.id } }); await loadProjects(null); } catch (e) { toast(e.message, "err"); }
  };
  for (const id of ["dirStory", "dirVisual", "dirExtra"]) F(id).addEventListener("input", readForm);
  for (const id of ["dirSegN", "dirSecs", "dirRes", "dirWhere", "dirStyle"]) F(id).addEventListener("change", readForm);
  F("dirImgUse").onclick = () => {
    const b = gen.baseParams();
    if (!b.model) return toast("生成頁還沒選模型", "err");
    if (b.family === "custom") return toast("生圖工作流只能在生成頁用：先在生成頁換成一般模型", "err");
    P.settings = { ...(P.settings || {}), image: b };
    imgInfo();
    save();
    toast("第一格會用：" + b.model, "ok");
  };
  F("dirPlan").onclick = () => write("plan");
  F("dirShots").onclick = () => write("shots");
  F("dirRun").onclick = () => run();
  F("dirConcat").onclick = async () => {
    try { await api("/api/director/concat", { json: { id: P.id } }); status("接成長片中…", "amb"); await reload(); } catch (e) { toast(e.message, "err"); }
  };
  F("dirStop").onclick = async () => { await api("/api/director/stop", { json: { id: P.id } }); status("停止中…", "amb"); };
  initChars();
}

export const isShown = () => shown;

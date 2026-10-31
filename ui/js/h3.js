// H3 影片台（2026-10-22；2026-10-23 大改成素材板）：導演頁的「H3 影片」分頁。
// 起手式 → 素材板（每個素材選用途）＋分鏡卡 → 網頁組成 H3 官方格式提示詞 → 雲端或這台電腦出片，成品進圖庫。
// 後端：src-tauri\src\h3.rs（h3_mode "board" 組節點圖）、server\api_h3.rs（/api/h3/*）、server\api_cloud.rs（雲端功能包、上傳素材、送圖收成品）。
// 設計在 design\H3素材板_設計.md；舊範本 resources\h3\templates.json 收在「舊範本」下拉（由 E:\AI\minimax_h3\templates_lib\make_templates.py 產生）。
import { $$, el, api, toast, store } from "./util.js";
import { pickFromGallery, openLightboxRecord } from "./gallery.js";
import { app } from "./state.js";
import * as agent from "./agent.js";
import * as drawer from "./assetdrawer.js";

const F = (id) => document.getElementById(id);
/** 工作列表用的名字（websocket 先到時還不知道起手式名） */
const MODE_NAME = { t2v: "文生影片", i2v: "圖生影片", fl2v: "首尾幀", ref2v: "多素材參考", keys: "多關鍵幀", continue: "接續上一支",
  swap: "換角色", vfx: "特效改片", style: "換畫風", live: "動態桌布", control: "動作控制", board: "H3 影片" };
const KIND_NAME = { image: "圖", video: "影片", audio: "聲音" };
const ACCEPT = { image: "image/png,image/jpeg,image/webp", video: "video/*,.mp4,.webm,.mov,.mkv,.m4v,.gif", audio: "audio/*,.wav,.mp3,.flac,.ogg,.m4a,.aac,.opus" };
/** 參考素材上限（跟 h3.rs 一樣）；釘在時間點的圖最多 6 張、釘住的聲音最多 3 段 */
const MAX = { image: 9, video: 3, audio: 3, all: 12, pins: 6, audioPins: 3, live: 3 };
/** 上傳的檔最大多大（後端收 220MB 的 JSON，base64 會多三分之一） */
const FILE_MAX = 150 * 2 ** 20;
const SECS = [3, 5, 8, 10, 12, 15];
const QUALITY_MP = { fast: 0.4, std: 0.6, high: 0.98 };
const STATE_NAME = { queued: "排隊", running: "進行中", done: "完成", queued_local: "已排入", error: "失敗", stopped: "已停止" };
/** 工作到這些狀態就結束了（本機的「已排入」：之後的進度、停止在右上角的任務面板） */
const END_STATES = ["done", "queued_local", "error", "stopped"];

// ---------------------------------------------------------------- 素材的用途
/** 下拉的值：use（ref／anchor／continue／guide／control）＋「:角色」 */
const USES = {
  image: [["ref:character", "角色（臉＋身材）"], ["ref:outfit", "服裝"], ["ref:scene", "場景"], ["ref:prop", "道具"], ["ref:storyboard", "分鏡板"], ["ref:style", "畫風"], ["anchor", "釘在第幾秒（首幀、尾幀、關鍵幀）"]],
  video: [["ref:motion", "取動作"], ["ref:camera", "取運鏡"], ["ref:character", "取角色"], ["continue", "從它結尾接著拍"], ["guide", "整段改造（換角色、特效、換畫風）"], ["control", "骨架控制"]],
  audio: [["ref:voice", "取音色（用這個聲音講新台詞）"], ["ref:music", "背景音樂"], ["ref:sfx", "音效"], ["anchor", "原封不動放進去（畫面配合、對嘴）"]],
};
const KEEP_IMG = [["fully_preserved", "完整保留"], ["partially_preserved", "大致保留"], ["attribute_transfer", "只取特徵"], ["weak_reference", "參考就好"]];
const KEEP_AUD = [["reference", "參考（音色、風格）"], ["fully_copy", "完全照用"], ["partially_copy", "照用一部分"], ["weak_reference", "隨便參考"]];
const SPEAKERS = ["S1", "S2", "S3", "S4"];
const LANGS = ["Chinese", "English", "Japanese", "Cantonese", "Korean"];
const LORA_NAME = [["swap", "換角色（社群 LoRA）"], ["vfx", "加特效（社群 LoRA）"], ["style", "換畫風（社群 LoRA）"], ["", "只當引導（不掛 LoRA）"]];
const CONTROL_NAME = [["pose", "人體姿勢（SDPose）"], ["canny", "線條（Canny）"], ["none", "直接用（已經是骨架或深度影片）"]];

/** 起手式：只是先擺好素材格、分鏡、長度（adv＝雲端要勾「H3 進階玩法」） */
const STARTS = [
  { id: "blank", name: "空白開始", secs: 5, items: [], desc: "什麼都沒放＝文生影片。按「＋ 圖／影片／聲音」自己加，每一格選用途。" },
  { id: "voice", name: "用某人的聲音講新台詞", secs: 8,
    items: [{ kind: "image", use: "ref", role: "character", speaker: "S1" }, { kind: "audio", use: "ref", role: "voice", speaker: "S1" }],
    lines: [{ speaker: "S1" }],
    desc: "角色圖＋一段 5 秒左右的說話錄音（聲音乾淨、只有一個人）。在分鏡的台詞寫新的話：她會用這個聲音講、嘴型對上。" },
  { id: "motion", name: "照影片的動作／運鏡", secs: 10,
    items: [{ kind: "image", use: "ref", role: "character" }, { kind: "video", use: "ref", role: "motion", audio: true }],
    desc: "角色圖＋一段動作清楚的影片（跳舞、打鬥、走路…）。只想借鏡頭，就把影片的用途改成「取運鏡」；不要原片的聲音就把「也取它的聲音」取消。" },
  { id: "fl", name: "首尾幀", secs: 5, items: [{ kind: "image", use: "anchor", secs: 0 }, { kind: "image", use: "anchor", secs: -1 }],
    desc: "兩張圖：開頭長這樣、結尾長這樣，中間自己補。想在中間再釘一張，加一張圖、用途選「釘在第幾秒」。" },
  { id: "cont", name: "接著上一支拍", secs: 5, items: [{ kind: "video", use: "continue", audio: true }],
    desc: "放上一支影片，從它最後一刻接著拍，不切鏡、不重來。成品縮圖下面的「接著拍」也會帶你到這裡。" },
  { id: "swap", name: "影片換角色", secs: 10, adv: true,
    items: [{ kind: "image", use: "ref", role: "character" }, { kind: "video", use: "guide", lora: "swap", audio: true }],
    desc: "原影片的動作、鏡頭、時間都不變，人換成圖上的角色。" },
  { id: "vfx", name: "影片加特效／換畫風", secs: 5, adv: true, items: [{ kind: "video", use: "guide", lora: "vfx", audio: true }],
    desc: "在原影片上加特效；把「改造方式」換成「換畫風」就是整支換風格。" },
  { id: "pose", name: "骨架控制", secs: 5, adv: true,
    items: [{ kind: "image", use: "ref", role: "character" }, { kind: "video", use: "control", control_type: "pose" }],
    desc: "照影片裡的人體姿勢動，人和畫面照圖和分鏡。" },
  { id: "sync", name: "照這段聲音對嘴／對拍", secs: 10,
    items: [{ kind: "image", use: "ref", role: "character", speaker: "S1" }, { kind: "audio", use: "anchor", secs: 0 }],
    desc: "放一段唱歌、說話或音樂：聲音原封不動放進片子，畫面跟著它對嘴、對拍。" },
  { id: "live", name: "動態桌布", secs: 5, adv: true, lora: "live", items: [{ kind: "image", use: "ref", role: "scene" }],
    desc: "1～3 張圖做成會動的循環桌布。" },
];

let info = null;          // /api/h3/info
/** 這一支片：{name, lora, items, shots, style, summary, soundscape, music} */
let board = null;
let manual = false;       // 完整提示詞手改過：分鏡改了不再蓋掉
let fromSheet = false;    // 這支是拍攝單套用過來的：送出前要健檢；換起手式、舊範本就清掉
let where = store.get("h3Where", "cloud");
let cloud = null;         // /api/agent/cloud
let loras = [];           // [{name, strength}]
const tasks = new Map();  // task → {row, state, msg, mode}
const localJobs = new Set();
let outs = [];            // 成品（圖庫紀錄）
let loading = null;

// ================================================================ 共用
/** 秒數 → H3 的格數（17k＋5，跟 h3::frames 一樣） */
export function frames(secs) {
  const s = Math.min(15, Math.max(1, +secs || 5));
  const n = Math.max(5, Math.round(s * 24));
  return n + ((((5 - (n % 17)) % 17) + 17) % 17);
}
const dur = () => frames(secs()) / 24;
const durText = () => dur().toFixed(2);
const secs = () => +F("h3Secs").value || 5;
const btn = (label, onclick, cls = "akey sm", title = "") => el("button", { class: cls, type: "button", title: title || undefined, onclick }, label);
const fileB64 = (file) => new Promise((ok, no) => {
  const fr = new FileReader();
  fr.onload = () => ok(String(fr.result).split(",")[1] || "");
  fr.onerror = () => no(new Error("讀不到這個檔案"));
  fr.readAsDataURL(file);
});
/** 一行一個 div：英文介面的字典是整句比對，用換行接成一段就查不到 */
function lines(box, list, warn = false) {
  box.replaceChildren(...list.filter(Boolean).map((t) => el("div", {}, t)));
  box.classList.toggle("warn-text", warn && list.some(Boolean));
}
function setStatus(msg, cls = "g") {
  const s = F("h3Status");
  s.textContent = msg;
  s.className = `vfd ${cls} dir-status`;
}
/** 下拉選單（opts：[[值, 顯示]]） */
function selectEl(opts, value, onchange, label) {
  const s = el("select", { class: "sm", "aria-label": label }, ...opts.map(([v, t]) => el("option", { value: v }, t)));
  s.value = value ?? opts[0][0];
  s.onchange = () => onchange(s.value);
  return s;
}
function textEl(value, oninput, attrs = {}) {
  const i = el("input", { type: "text", spellcheck: "false", ...attrs });
  i.value = value ?? "";
  i.oninput = () => oninput(i.value);
  return i;
}

// ================================================================ 載入
async function load() {
  info = await api("/api/h3/info", { timeout: 60000 });
  cloud = info.cloud;
  paintTpl();
  paintCloud();
  paintLocal();
  paintModels();
}

/** 每次切到這個分頁（studio.js 的 setTab）：第一次等它讀完；之後先用舊的，背景重讀（本機 ComfyUI、雲端的模型清單會變） */
export async function show() {
  if (!loading) {
    loading = load().catch((e) => { loading = null; F("h3Desc").textContent = `讀不到 H3 影片台的資料：${e.message}`; });
    await loading;
    refreshCloud();
    refreshRh();
    return;
  }
  await loading;
  reload();
}
let reloading = false;
function reload() {
  if (reloading || !info) return;
  reloading = true;
  load().catch(() => {}).finally(() => { reloading = false; });
}
async function refreshCloud() {
  try { cloud = await api("/api/agent/cloud"); paintCloud(); } catch { /* 伺服器重開中 */ }
}

// ================================================================ 起手式、舊範本
function paintStarts() {
  F("h3Starts").replaceChildren(...STARTS.map((s) => {
    const b = btn(s.name, () => start(s), "akey sm" + (board?.name === s.name ? " on" : ""), s.desc);
    return b;
  }));
}

function paintTpl() {
  const sel = F("h3Tpl");
  const groups = new Map();
  for (const t of info.templates) {
    if (!groups.has(t.group_zh)) groups.set(t.group_zh, []);
    groups.get(t.group_zh).push(t);
  }
  sel.replaceChildren(el("option", { value: "" }, `舊範本（${info.templates.length} 個）：選一個帶進來`),
    ...[...groups].map(([g, list]) => el("optgroup", { label: g }, ...list.map((t) => el("option", { value: t.id }, `${t.name_zh}（${t.seconds} 秒）`)))));
  sel.value = "";
}

function newItem(o) {
  return { kind: "image", use: "ref", role: "", name: "", speaker: "", voice: "", keep: "", secs: 0, audio: false, lora: "", control_type: "pose", strength: 1, shots: "", note: "", hint: "", asset: null, view: "", m: null, busy: false, ...o };
}
/** 資產圖片用途 → 提示詞裡怎麼說這張圖提供什麼 */
const VIEW_EN = { src: "overall look", portrait: "face", face: "facial expressions", sheet: "full-body views from every angle", sheet_nude: "body proportions", detail: "small details", pose: "pose", outfit: "outfit", other: "appearance" };
/** 同名的角色圖是同一個人（官方：同一主體來自多個素材時寫在同一條）；沒名字的各算各的 */
function sameChar(x) {
  const isChar = (y) => y.kind === "image" && y.use === "ref" && y.role === "character";
  const name = x.name?.trim();
  if (!isChar(x) || !name) return [x];
  return board.items.filter((y) => isChar(y) && y.m && y.name.trim() === name);
}
function newShot(start = 0) {
  return { start, camera: "", picture: "", lines: [], sound: "" };
}
function newLine(o = {}) {
  return { speaker: "S1", to: "", lang: "Chinese", text: "", offscreen: false, ...o };
}

/** 換一支新的片：起手式 */
function start(s) {
  board = { name: s.name, lora: s.lora || "", items: s.items.map(newItem), shots: [newShot(0)], style: "", summary: "", soundscape: "", music: "" };
  if (s.lines) board.shots[0].lines = s.lines.map(newLine);
  manual = false;
  fromSheet = false;
  setSecs(s.secs);
  F("h3Desc").textContent = s.desc;
  afterNew();
}

/** 舊範本 → 素材板：格子照用途換過來，填好預設值的範本文字放進「完整提示詞」（手改狀態） */
function startTpl(t) {
  const items = (t.slots || []).map((sl) => {
    const base = { hint: sl.role || "" };
    switch (sl.slot) {
      case "first": return newItem({ ...base, kind: "image", use: "anchor", secs: 0 });
      case "last": return newItem({ ...base, kind: "image", use: "anchor", secs: -1 });
      case "key": return newItem({ ...base, kind: "image", use: "anchor", secs: +sl.secs || 0 });
      case "ref_image": return newItem({ ...base, kind: "image", use: "ref", role: "character" });
      case "ref_video": return newItem({ ...base, kind: "video", use: "ref", role: "motion", audio: !!sl.soundtrack });
      case "ref_audio": return newItem({ ...base, kind: "audio", use: "ref", role: "voice" });
      case "prev": return newItem({ ...base, kind: "video", use: "continue", audio: t.continue_audio !== false });
      case "source": return t.mode === "control"
        ? newItem({ ...base, kind: "video", use: "control", control_type: t.control_type || "pose" })
        : newItem({ ...base, kind: "video", use: "guide", lora: ["swap", "vfx", "style"].includes(t.mode) ? t.mode : "", audio: !!t.keep_audio });
      default: return newItem({ ...base, kind: sl.kind || "image" });
    }
  });
  board = { name: t.name_zh, lora: t.mode === "live" ? "live" : "", items, shots: [newShot(0)], style: "", summary: "", soundscape: "", music: "" };
  fromSheet = false;
  setSecs(t.seconds || 5);
  let text = t.template_en.split("{secs}").join(durText());
  for (const i of t.inputs || []) text = text.split(`{${i.key}}`).join(String(i.default_en ?? "").trim());
  manual = true;
  F("h3Prompt").value = text;
  F("h3PromptFold").open = true;
  const tips = [t.tips_zh].flat().filter(Boolean);
  F("h3Desc").replaceChildren(el("div", {}, t.desc_zh), ...tips.map((x) => el("div", { class: "muted sm" }, `・${x}`)),
    el("div", { class: "muted sm" }, "範本的提示詞已經放進「完整提示詞」（手改狀態）：直接改那裡，或寫點子按「AI 幫我寫滿分鏡」改用分鏡卡。"));
  afterNew();
}

/** 拍攝單「套用到短片」：開一塊新的素材板，首幀釘在第 0 秒、參考圖當角色參考，拍攝單組好的提示詞放進「完整提示詞」（手改狀態） */
export async function applySheet({ sheet = null, prompt, secs, first = null, refs = [] }) {
  const media = (name) => new Promise((res) => {
    const im = new Image();
    im.onload = () => res({ name, kind: "image", w: im.naturalWidth, h: im.naturalHeight, secs: 0, audio: false });
    im.onerror = () => res({ name, kind: "image", w: 0, h: 0, secs: 0, audio: false });
    im.src = `/input/${encodeURIComponent(name)}`;
  });
  const items = [];
  if (first) items.push(newItem({ kind: "image", use: "anchor", secs: 0, m: await media(first) }));
  for (const r of refs) items.push(newItem({ kind: "image", use: "ref", role: "character", m: await media(r) }));
  board = { name: "拍攝單", lora: "", items, shots: [newShot(0)], style: "", summary: "", soundscape: "", music: "" };
  setSecs(secs);
  F("h3Desc").textContent = "從上面的拍攝單套用：提示詞在「完整提示詞」（手改狀態）。要改內容建議回拍攝單改完再套用一次；按送出前會先健檢。";
  afterNew();
  manual = true;
  fromSheet = sheet ? structuredClone(sheet) : false;
  F("h3Prompt").value = prompt;
  F("h3PromptFold").open = true;
  compose();
}

function afterNew() {
  F("h3Tpl").value = "";
  F("h3TwoPass").checked = false;
  paintStarts();
  paintBoard();
  paintShots();
  compose();
  paintSize();
  paintCloud();
  F("h3ColA").scrollTop = 0;
  F("h3ColB").scrollTop = 0;
}

function setSecs(v) {
  const sel = F("h3Secs");
  const opts = [...new Set([...SECS, v || 5])].sort((a, b) => a - b);
  sel.replaceChildren(...opts.map((s) => el("option", { value: s }, `${s} 秒（${frames(s)} 格）`)));
  sel.value = String(v || 5);
}

// ================================================================ 素材板
const isEdge = (t) => +t === 0 || +t === -1;
const isPinImg = (it) => it.kind === "image" && it.use === "anchor";
const special = (it) => it.kind === "video" && ["continue", "guide", "control"].includes(it.use);
/** 走參考路（跟 h3.rs board_plan 一樣）：有參考素材、首尾以外的釘圖、社群 LoRA、骨架控制 */
function refsPath() {
  const it = board.items;
  return it.some((x) => x.use === "ref") || it.some((x) => isPinImg(x) && !isEdge(x.secs)) || !!board.lora || it.some((x) => x.use === "guide" && x.lora) || it.some((x) => x.use === "control");
}
/** 素材在提示詞裡的名字：參考路＝圖照素材板順序；關鍵幀路＝首幀 1、尾幀 2。Audio 先算參考影片的原聲再算參考聲音 */
function labels() {
  const out = new Map();
  let pi = 0, vi = 0, ai = 0;
  const imgs = board.items.filter((x) => x.kind === "image");
  if (refsPath()) imgs.forEach((x) => out.set(x, `<Picture ${++pi}>`));
  else {
    const f = imgs.find((x) => +x.secs === 0), l = imgs.find((x) => +x.secs === -1);
    if (f) out.set(f, `<Picture ${++pi}>`);
    if (l) out.set(l, `<Picture ${++pi}>`);
  }
  for (const x of board.items) if (x.kind === "video" && x.use === "ref") out.set(x, `<Video ${++vi}>`);
  for (const x of board.items) x.audioLabel = x.kind === "video" && x.use === "ref" && x.audio ? `<Audio ${++ai}>` : "";
  for (const x of board.items) if (x.kind === "audio" && x.use === "ref") out.set(x, `<Audio ${++ai}>`);
  return out;
}
/** 提示詞能提到的 <Picture N>、<Video N>、<Audio N> 有幾個 */
function counts() {
  const L = [...labels().values()];
  const n = (k) => L.filter((x) => x.startsWith(`<${k} `)).length;
  return { Picture: n("Picture"), Video: n("Video"), Audio: n("Audio") + board.items.filter((x) => x.audioLabel).length };
}

function paintBoard() {
  if (!board) return;
  const L = labels();
  const its = board.items;
  const nImg = its.filter((x) => x.kind === "image").length;
  const nVid = its.filter((x) => x.kind === "video" && x.use === "ref").length;
  const nAud = its.filter((x) => x.kind === "audio" && x.use === "ref").length;
  const refsN = its.filter((x) => x.use === "ref").length + its.filter((x) => isPinImg(x) && refsPath()).length;
  F("h3Counts").replaceChildren(...[el("span", {}, `圖 ${nImg}/${MAX.image}`), el("span", {}, `參考影片 ${nVid}/${MAX.video}`),
    el("span", {}, `參考聲音 ${nAud}/${MAX.audio}`), el("span", {}, `參考合計 ${refsN}/${MAX.all}`),
    board.lora === "live" ? el("span", { class: "h3-chip" }, "動態桌布 LoRA", btn("×", () => { board.lora = ""; paintBoard(); compose(); }, "h3-x", "不掛動態桌布 LoRA")) : null].filter(Boolean));
  F("h3Items").replaceChildren(...its.map((x, i) => itemEl(x, i, L.get(x))));
  lines(F("h3BoardNote"), boardProblems(), true);
  F("h3AddImage").disabled = nImg >= MAX.image;
  F("h3AddVideo").disabled = its.filter((x) => x.kind === "video").length >= MAX.video + 1;
  F("h3AddAudio").disabled = its.filter((x) => x.kind === "audio").length >= MAX.audio * 2;
  // 二采只能用在純文生／純參考
  const guided = its.some((x) => x.use === "anchor" || special(x)) || !!board.lora;
  F("h3TwoPass").disabled = guided;
  if (guided) F("h3TwoPass").checked = false;
}

/** 素材板上看得出來的問題（跟後端擋的一樣，先在網頁講） */
function boardProblems() {
  const its = board.items;
  const out = [];
  const nImg = its.filter((x) => x.kind === "image").length;
  if (nImg > MAX.image) out.push(`圖最多 ${MAX.image} 張`);
  if (its.filter((x) => x.kind === "video" && x.use === "ref").length > MAX.video) out.push(`參考影片最多 ${MAX.video} 段`);
  if (its.filter((x) => x.kind === "audio" && x.use === "ref").length > MAX.audio) out.push(`參考聲音最多 ${MAX.audio} 段`);
  const refsN = its.filter((x) => x.use === "ref").length + (refsPath() ? its.filter(isPinImg).length : 0);
  if (refsN > MAX.all) out.push(`參考素材合計最多 ${MAX.all} 個（現在 ${refsN} 個）`);
  if (its.filter(special).length > 1) out.push("「接著拍」「整段改造」「骨架控制」的影片合起來只能放一段");
  if (its.filter(isPinImg).length > MAX.pins) out.push(`釘在時間點的圖最多 ${MAX.pins} 張`);
  if (its.filter((x) => x.kind === "audio" && x.use === "anchor").length > MAX.audioPins) out.push(`原封不動放進去的聲音最多 ${MAX.audioPins} 段`);
  for (const t of [0, -1]) if (its.filter((x) => isPinImg(x) && +x.secs === t).length > 1) out.push("同一個時間點只能釘一張圖");
  if (its.some((x) => x.use === "continue") && its.some((x) => isPinImg(x) && +x.secs === 0)) out.push("「接著拍」的開頭已經是上一支的畫面：第 0 秒不能再釘圖");
  const pinOut = its.find((x) => x.use === "anchor" && +x.secs !== -1 && (+x.secs < 0 || +x.secs >= dur()));
  if (pinOut) out.push(`釘的時間 ${pinOut.secs} 秒超出影片長度（0～${durText()} 秒）`);
  if (its.some((x) => x.use === "guide" && x.lora === "swap") && !its.some((x) => x.kind === "image" && x.use === "ref")) out.push("換角色要放一張角色圖");
  if (board.lora === "live" && its.filter((x) => x.kind === "image").length > MAX.live) out.push(`動態桌布最多 ${MAX.live} 張圖`);
  if (its.some((x) => x.kind === "audio" && x.use === "ref") && !its.some((x) => x.kind === "image" || (x.kind === "video" && x.use === "ref"))) out.push("參考聲音不能單獨用：至少再放一張圖或一段影片");
  return out;
}

function metaText(m) {
  if (m.kind === "image") return `${m.w}×${m.h}`;
  const t = `${(+m.secs).toFixed(1)} 秒`;
  return m.kind === "video" ? [`${m.w}×${m.h}・${t}`, m.audio ? "（有聲音）" : "（沒有聲音）"] : t;  // 分兩段：英文字典才對得準
}

function previewEl(x) {
  const src = x.m ? `/input/${encodeURIComponent(x.m.name)}` : "";
  if (x.busy) return el("div", { class: "h3-prev" }, "處理中…");
  if (!x.m) return el("div", { class: "h3-prev empty" }, "還沒放");
  if (x.kind === "image") return el("img", { class: "h3-prev", src, alt: "" });
  if (x.kind === "video") {
    const v = el("video", { class: "h3-prev", src, muted: true, loop: true, playsinline: true, preload: "metadata" });
    v.muted = true;
    v.onmouseenter = () => v.play().catch(() => {});
    v.onmouseleave = () => v.pause();
    return v;
  }
  return el("div", { class: "h3-prev" }, "♪");
}

/** 素材板一格：預覽、檔案按鈕、用途、用途的小選項 */
function itemEl(x, i, label) {
  const redo = () => { paintBoard(); compose(); paintSize(); };
  const useVal = x.use === "ref" ? `ref:${x.role || USES[x.kind][0][0].split(":")[1]}` : x.use;
  const use = selectEl(USES[x.kind], useVal, (v) => {
    const [u, r] = v.split(":");
    x.use = u;
    x.role = r || "";
    if (u === "anchor" && x.kind === "image" && !board.items.some((y) => y !== x && isPinImg(y) && +y.secs === 0)) x.secs = 0;
    if (u === "guide" && !x.lora) x.lora = "";
    redo();
  }, "用途");
  const opts = [];
  const role = x.use === "ref" ? x.role : x.use;
  if (x.kind === "image" && x.use === "ref") {
    if (role === "character") opts.push(textEl(x.name, (v) => { for (const y of board.items) if (x.asset != null && y.asset === x.asset) y.name = v; x.name = v; compose(); }, { placeholder: "名字（例：小克）", "aria-label": "角色名字", class: "h3-name" }),
      selectEl([["", "不說話"], ...SPEAKERS.map((s) => [s, `說話的人 ${s}`])], x.speaker, (v) => { x.speaker = v; compose(); paintShots(); }, "說話的人"),
      textEl(x.voice, (v) => { x.voice = v; compose(); }, { placeholder: "聲音（例：少女聲，偏低，性感知性）", "aria-label": "聲音描述", class: "h3-note", title: "寫進提示詞：這個角色每句台詞前面都會註明用這種聲音。要每支片都同一把聲音，在聲音格按「聲音庫」做一段存起來" }));
    if (role === "storyboard") opts.push(textEl(x.shots, (v) => { x.shots = v; compose(); }, { placeholder: "對應第幾鏡（例：1-3）", "aria-label": "對應第幾鏡" }));
    else opts.push(selectEl(KEEP_IMG, x.keep || (role === "character" ? "fully_preserved" : "partially_preserved"), (v) => { x.keep = v; compose(); }, "保留多少"));
    if (role !== "storyboard") opts.push(textEl(x.note, (v) => { x.note = v; compose(); }, { placeholder: "長什麼樣（英文，AI 會填）", "aria-label": "描述", class: "h3-note" }));
  }
  if (x.use === "anchor") {
    const kinds = x.kind === "image" ? [["0", "第 0 秒（首幀）"], ["-1", "最後一幀（尾幀）"], ["t", "自己填秒數"]] : [["0", "從第 0 秒開始"], ["t", "從第幾秒開始"]];
    const mode = +x.secs === 0 ? "0" : +x.secs === -1 && x.kind === "image" ? "-1" : "t";
    opts.push(selectEl(kinds, mode, (v) => { x.secs = v === "t" ? Math.min(2, Math.floor(dur())) : +v; redo(); }, "釘在哪"));
    if (mode === "t") {
      const n = el("input", { type: "number", min: 0, step: 0.5, value: x.secs, "aria-label": "第幾秒", class: "h3-secs" });
      n.oninput = () => { x.secs = +n.value; lines(F("h3BoardNote"), boardProblems(), true); compose(); };
      opts.push(el("label", { class: "h3-inline" }, el("span", { class: "sub inline" }, "第"), n, el("span", { class: "sub inline" }, "秒")));
    }
  }
  if (x.kind === "video" && ["ref", "continue", "guide"].includes(x.use)) {
    const mute = !!x.m && !x.m.audio;
    const c = el("input", { type: "checkbox" });
    c.checked = !!x.audio && !mute;
    c.disabled = mute;
    c.onchange = () => { x.audio = c.checked; redo(); };
    const txt = { ref: x.audioLabel ? `也取它的聲音（${x.audioLabel}）` : "也取它的聲音", continue: "接上它的聲音", guide: "保留原片的聲音" }[x.use];
    opts.push(el("label", { class: "check", title: mute ? "這段影片沒有聲音" : "" }, c, ` ${txt}`));
    if (x.use === "ref" && x.role === "character") opts.push(textEl(x.name, (v) => { x.name = v; compose(); }, { placeholder: "名字", "aria-label": "角色名字", class: "h3-name" }));
  }
  if (x.use === "guide") opts.push(selectEl(LORA_NAME, x.lora, (v) => { x.lora = v; redo(); paintCloud(); }, "改造方式"));
  if (x.use === "control") {
    opts.push(selectEl(CONTROL_NAME, x.control_type, (v) => { x.control_type = v; }, "控制方式"));
    const n = el("input", { type: "number", min: 0, max: 2, step: 0.05, value: x.strength, "aria-label": "控制強度", class: "h3-secs" });
    n.oninput = () => { x.strength = +n.value; };
    opts.push(el("label", { class: "h3-inline" }, el("span", { class: "sub inline" }, "強度"), n));
  }
  if (x.kind === "audio" && x.use === "ref") {
    if (x.role === "voice") opts.push(selectEl(SPEAKERS.map((s) => [s, `給說話的人 ${s}`]), x.speaker || "S1", (v) => { x.speaker = v; compose(); }, "給誰"));
    opts.push(selectEl(KEEP_AUD, x.keep || (x.role === "voice" ? "reference" : "fully_copy"), (v) => { x.keep = v; compose(); }, "怎麼用"));
  }
  const tag = label || (x.use === "continue" ? "接續" : x.use === "guide" ? "原片" : x.use === "control" ? "骨架" : x.use === "anchor" ? "釘聲音" : KIND_NAME[x.kind]);
  return el("div", { class: "h3-item" + (x.m ? " on" : "") },
    el("div", { class: "h3-item-prev" }, previewEl(x),
      el("div", { class: "row gap wrap" },
        x.kind !== "audio" ? btn("圖庫", () => fromGallery(x), "akey sm", "從圖庫選") : null,
        btn("上傳", () => fromFile(x)),
        x.kind === "audio" ? btn("聲音庫", () => voiceLib(x), "akey sm", "從聲音庫挑、把這格存進聲音庫、用 TTS 做一把聲音") : null)),
    el("div", { class: "h3-item-body" },
      el("div", { class: "h3-item-hd" }, el("b", { translate: "no" }, tag), el("span", { class: "grow muted sm" }, x.m ? metaText(x.m) : (x.hint || "")),
        btn("×", () => { board.items.splice(i, 1); redo(); }, "h3-x", "拿掉這格")),
      use, ...opts, x.lib ? libEl(x) : null));
}

async function fill(x, body) {
  x.busy = true;
  paintBoard();
  try {
    const r = await api("/api/h3/media", { json: body, timeout: 15 * 60 * 1000 });
    if (r.kind !== x.kind) throw new Error(`這格要放${KIND_NAME[x.kind]}，放進來的是${KIND_NAME[r.kind] || r.kind}`);
    x.m = r;
    // 沒有聲音的影片（無聲片、GIF）不能當聲音參考：送上雲端才報錯就已經在計費了
    if (x.kind === "video" && !r.audio && x.audio) {
      x.audio = false;
      toast("這段影片沒有聲音：已取消「取它的聲音」，提示詞裡的聲音編號會跟著變，記得檢查", "ok");
    }
  } catch (e) { toast(e.message, "err"); }
  finally { x.busy = false; paintBoard(); compose(); paintSize(); }
}

function fromGallery(x) {
  pickFromGallery((rec) => fill(x, { gallery_id: rec.id }), {
    video: x.kind === "video" ? "only" : false,
    title: x.kind === "video" ? "從圖庫選一支影片" : "從圖庫選一張圖",
  }).catch((e) => toast(e.message, "err"));
}

function fromFile(x) {
  const inp = el("input", { type: "file", accept: ACCEPT[x.kind], hidden: true });
  inp.onchange = async () => {
    const f = inp.files[0];
    inp.remove();
    if (!f) return;
    if (f.size > FILE_MAX) return toast(`檔案太大（${(f.size / 2 ** 20).toFixed(0)} MB）：先剪短到 15 秒以內，或存成小一點的檔`, "err");
    try { await fill(x, { file_name: f.name, data_b64: await fileB64(f) }); } catch (e) { toast(e.message, "err"); }
  };
  document.body.append(inp);
  inp.click();
}

// ================================================================ 聲音庫（角色的專屬聲音存起來，每支片放同一把）
let voices = null;  // GET /api/voices：{voices, tts}
async function loadVoices() { voices = await api("/api/voices"); }

async function voiceLib(x) {
  x.lib = !x.lib;
  if (x.lib) try { await loadVoices(); } catch (e) { toast(e.message, "err"); x.lib = false; }
  paintBoard();
}

/** 這格聲音給誰：素材板上同一個說話的人的角色圖 */
const ownerOf = (x) => board.items.find((y) => y.kind === "image" && y.speaker && y.speaker === (x.speaker || "S1"));

function libEl(x) {
  const V = voices || { voices: [], tts: {} };
  const use = async (v) => {
    try {
      const r = await fetch(`/api/voices/${encodeURIComponent(v.id)}/file`);
      if (!r.ok) throw new Error("聲音庫的檔案讀不到");
      x.lib = false;
      await fill(x, { file_name: v.file, data_b64: await fileB64(await r.blob()) });
    } catch (e) { toast(e.message, "err"); }
  };
  const remove = async (v) => {
    if (!confirm(`把「${v.name}」移出聲音庫？（搬到「待刪除\\聲音庫」，不會真的刪）`)) return;
    try { await api(`/api/voices/${encodeURIComponent(v.id)}`, { method: "DELETE" }); await loadVoices(); paintBoard(); } catch (e) { toast(e.message, "err"); }
  };
  const save = async () => {
    const o = ownerOf(x);
    const name = prompt("存進聲音庫：取個名字", o?.name ? `${o.name}的聲音` : "");
    if (!name?.trim()) return;
    try {
      const r = await fetch(`/input/${encodeURIComponent(x.m.name)}`);
      if (!r.ok) throw new Error("讀不到這格的聲音檔");
      await api("/api/voices", { json: { name: name.trim(), desc: o?.voice || "", file_name: x.m.name, data_b64: await fileB64(await r.blob()) }, timeout: 5 * 60 * 1000 });
      toast("存進聲音庫了", "ok");
      await loadVoices();
      paintBoard();
    } catch (e) { toast(e.message, "err"); }
  };
  const importTts = () => {
    const inp = el("input", { type: "file", accept: ".json,application/json,image/png,image/webp", hidden: true });
    inp.onchange = async () => {
      const f = inp.files[0];
      inp.remove();
      if (!f) return;
      try {
        await api("/api/voices/tts_workflow", { json: { file_name: f.name, data_b64: await fileB64(f) }, timeout: 5 * 60 * 1000 });
        toast("TTS 工作流接好了", "ok");
        await loadVoices();
        paintBoard();
      } catch (e) { toast(e.message, "err"); }
    };
    document.body.append(inp);
    inp.click();
  };
  const rows = V.voices.map((v) => el("div", { class: "h3-voice" },
    el("div", { class: "grow" }, el("b", { translate: "no" }, v.name), v.desc ? el("div", { class: "muted sm", translate: "no" }, v.desc) : null),
    el("audio", { controls: true, preload: "none", src: `/api/voices/${encodeURIComponent(v.id)}/file` }),
    btn("用這個", () => use(v)), btn("×", () => remove(v), "h3-x", "移出聲音庫")));
  // TTS：照聲音描述「捏」一段，存進聲音庫再放進這格
  const tts = V.tts || {};
  let ttsBox;
  if (tts.ready) {
    const o = ownerOf(x);
    x.tts = x.tts || { name: o?.name ? `${o.name}的聲音` : "", desc: o?.voice || "", text: "" };
    const t = x.tts;
    const go = btn("用 TTS 做一段", async () => {
      if (!t.name.trim() || !t.text.trim()) return toast("要有名字和要念的句子", "err");
      go.disabled = true;
      go.textContent = "做中…";
      try {
        const v = await api("/api/voices/tts", { json: { name: t.name, desc: t.desc, text: t.text }, timeout: 16 * 60 * 1000 });
        toast("做好了，存進聲音庫", "ok");
        await loadVoices();
        await use(v);
      } catch (e) { toast(e.message, "err"); }
      finally { go.disabled = false; go.textContent = "用 TTS 做一段"; }
    }, "akey sm primary");
    ttsBox = el("div", { class: "h3-voice-tts" },
      textEl(t.name, (v) => { t.name = v; }, { placeholder: "名字（例：小克的聲音）", "aria-label": "聲音名字" }),
      textEl(t.desc, (v) => { t.desc = v; }, { placeholder: "聲音描述（例：少女聲，偏低，性感知性）", "aria-label": "聲音描述" }),
      textEl(t.text, (v) => { t.text = v; }, { placeholder: "要念的句子（念 5～15 秒，講什麼都行）", "aria-label": "要念的句子" }),
      tts.voice_field ? null : el("div", { class: "muted sm" }, "這個 TTS 工作流沒有 {{voice}} 欄位：聲音描述不會送進去"),
      go);
  } else ttsBox = el("div", { class: "muted sm" }, tts.why || "還沒接 TTS");
  return el("div", { class: "h3-lib" },
    el("div", { class: "sub" }, "聲音庫"),
    ...(rows.length ? rows : [el("div", { class: "muted sm" }, "聲音庫是空的：把這格的聲音存進來，或用 TTS 做一段")]),
    el("div", { class: "row gap wrap" }, x.m ? btn("把這格存進聲音庫", save) : null, btn("匯入 TTS 工作流…", importTts, "akey sm", "ComfyUI 的 TTS 工作流：要念的文字欄位寫 {{text}}、聲音描述欄位寫 {{voice}}，要有存聲音的節點")),
    ttsBox);
}

function addItem(kind) {
  if (!board) start(STARTS[0]);
  const it = newItem({ kind, use: kind === "audio" ? "ref" : "ref", role: { image: "character", video: "motion", audio: "voice" }[kind] });
  board.items.push(it);
  paintBoard();
  compose();
}

// ================================================================ 分鏡
/** 說話的人 → 名字（素材板上的角色） */
function speakerName(id) {
  const c = board.items.find((x) => x.speaker === id && x.kind === "image" && x.name);
  return c ? c.name : "";
}
/** 說話的人 → 聲音描述（素材板上的角色寫的） */
function speakerVoice(id) {
  const c = board.items.find((x) => x.speaker === id && x.kind === "image" && x.voice?.trim());
  return c ? c.voice.trim() : "";
}

function paintRuler() {
  const d = dur();
  const sh = board?.shots || [];
  F("h3Ruler").replaceChildren(...sh.map((s, i) => {
    const end = i + 1 < sh.length ? +sh[i + 1].start : d;
    const w = Math.max(0, Math.min(d, end) - Math.max(0, +s.start)) / d * 100;
    return el("div", { style: `width:${w.toFixed(2)}%` }, `鏡${i + 1} ${(+s.start).toFixed(1)}–${end.toFixed(1)}s`);
  }));
}

function paintShots() {
  if (!board) return;
  const L = labels();
  F("h3Shots").replaceChildren(...board.shots.map((s, i) => shotEl(s, i, L)));
  paintRuler();
}

function shotEl(s, i, L) {
  const redo = () => { compose(); paintRuler(); };
  const start = el("input", { type: "number", min: 0, step: 0.5, value: s.start, "aria-label": "從第幾秒", class: "h3-secs", disabled: i === 0 });
  start.oninput = () => { s.start = +start.value; redo(); };
  const cam = textEl(s.camera, (v) => { s.camera = v; compose(); }, { placeholder: "鏡頭（例：medium shot, slow push-in）", "aria-label": "鏡頭", class: "grow" });
  const pic = el("textarea", { rows: 3, spellcheck: "false", "aria-label": "畫面", placeholder: "畫面和動作（英文最準；可以直接寫素材格左上角的編號）" });
  pic.value = s.picture;
  pic.oninput = () => { s.picture = pic.value; compose(); chips.replaceChildren(...usedChips(s, L)); };
  const chips = el("div", { class: "h3-chips" }, ...usedChips(s, L));
  const lineBox = el("div", { class: "h3-lines" }, ...s.lines.map((ln, j) => lineEl(s, ln, j)));
  const snd = textEl(s.sound, (v) => { s.sound = v; compose(); }, { placeholder: "這一鏡的聲音（例：door bell chimes, espresso machine hiss）", "aria-label": "聲音" });
  return el("div", { class: "h3-shot" },
    el("div", { class: "h3-shot-hd" }, el("b", {}, `鏡 ${i + 1}`), el("span", { class: "sub inline" }, "從第"), start, el("span", { class: "sub inline" }, "秒"), cam,
      board.shots.length > 1 ? btn("×", () => { board.shots.splice(i, 1); if (board.shots[0]) board.shots[0].start = 0; paintShots(); compose(); }, "h3-x", "刪掉這一鏡") : null),
    chips, pic,
    el("div", { class: "h3-row" }, el("span", { class: "sub" }, "台詞"), lineBox,
      btn("＋ 台詞", () => { s.lines.push(newLine({ speaker: board.items.find((x) => x.speaker)?.speaker || "S1" })); paintShots(); compose(); })),
    el("div", { class: "h3-row" }, el("span", { class: "sub" }, "聲音"), snd));
}

/** 這一鏡提到哪些素材（畫面裡寫了 <Picture 1>、角色名字） */
function usedChips(s, L) {
  const out = [];
  for (const [x, tag] of L) if (s.picture.includes(tag) || (x.name && s.picture.includes(x.name))) out.push(el("span", { class: "h3-chip", translate: "no" }, x.name ? `${tag} ${x.name}` : tag));
  return out;
}

function lineEl(s, ln, j) {
  const who = selectEl(SPEAKERS.map((id) => [id, speakerName(id) ? `${id} ${speakerName(id)}` : id]), ln.speaker, (v) => { ln.speaker = v; compose(); }, "誰說");
  const to = textEl(ln.to, (v) => { ln.to = v; compose(); }, { placeholder: "對誰（可空白）", "aria-label": "對誰", class: "h3-to" });
  const lang = selectEl(LANGS.map((l) => [l, l]), ln.lang, (v) => { ln.lang = v; compose(); }, "語言");
  const text = textEl(ln.text, (v) => { ln.text = v; compose(); }, { placeholder: "台詞原文（不用翻）", "aria-label": "台詞", class: "grow" });
  const off = el("input", { type: "checkbox" });
  off.checked = !!ln.offscreen;
  off.onchange = () => { ln.offscreen = off.checked; compose(); };
  return el("div", { class: "h3-line" }, who, to, lang, text, el("label", { class: "check", title: "畫外音：嘴巴不動" }, off, " 畫外音"),
    btn("×", () => { s.lines.splice(j, 1); paintShots(); compose(); }, "h3-x", "刪掉這句"));
}

/** AI 寫回來的分鏡 → 素材板、分鏡卡 */
function applyPlan(plan, L) {
  const str = (v) => (typeof v === "string" ? v.trim() : "");
  board.summary = str(plan.summary) || board.summary;
  board.style = str(plan.style) || board.style;
  board.soundscape = str(plan.soundscape) || board.soundscape;
  board.music = str(plan.music) || board.music;
  for (const [x, tag] of L) if (plan.notes && str(plan.notes[tag]) && x.kind === "image" && x.use === "ref") x.note = str(plan.notes[tag]);
  const d = dur();
  const shots = (plan.shots || []).filter((s) => s && typeof s === "object").map((s, i) => ({
    start: i === 0 ? 0 : Math.max(0, Math.min(d - 0.5, +s.start || 0)),
    camera: str(s.camera), picture: str(s.picture), sound: str(s.sound),
    lines: (Array.isArray(s.lines) ? s.lines : []).filter((l) => l && str(l.text)).map((l) => newLine({
      speaker: SPEAKERS.includes(str(l.speaker)) ? str(l.speaker) : "S1", to: str(l.to), lang: str(l.lang) || "Chinese", text: str(l.text), offscreen: !!l.offscreen })),
  }));
  if (shots.length) board.shots = shots;
  F("h3Style").value = board.style;
  F("h3Summary").value = board.summary;
  F("h3Soundscape").value = board.soundscape;
  F("h3Music").value = board.music;
}

// ================================================================ 組提示詞（H3 官方格式）
const sentence = (t) => { t = String(t || "").trim(); return !t ? "" : /[.!?。！？"」]$/.test(t) ? t : `${t}.`; };
const cap = (t) => (t ? t[0].toUpperCase() + t.slice(1) : t);
/** 秒 → 00:04.000 */
function ts(t) {
  const ms = Math.round(Math.max(0, +t) * 1000);
  const m = Math.floor(ms / 60000);
  return `${String(m).padStart(2, "0")}:${((ms % 60000) / 1000).toFixed(3).padStart(6, "0")}`;
}

// 聲音描述照官方格式寫在每句 <d> 前面（「says in a soft low female voice: <d>…」）；放進角色定義會被 H3 當台詞念出來
function voicePhrase(v) {
  v = v.replace(/[.。，,\s]+$/, "");
  return v ? ` in ${/voice/i.test(v) ? v : `a voice that is ${v}`}` : "";
}

function lineText(ln) {
  const name = speakerName(ln.speaker);
  const who = name ? `${name} (${ln.speaker})` : `The speaker (${ln.speaker})`;
  const voice = voicePhrase(speakerVoice(ln.speaker));
  const d = `<d>[${ln.lang || "Chinese"}] ${ln.text.trim()}</d>`;
  if (ln.offscreen) return `${who} says in an off-screen voiceover${voice}: ${d} while the on-screen lips remain completely closed.`;
  const to = ln.to.trim() ? ` to ${speakerName(ln.to.trim()) || ln.to.trim()}` : "";
  return `${who} says${to}${voice}: ${d}`;
}

/** 一鏡的文字：[Shot n]（後面的鏡頭帶切點）＋鏡頭＋畫面＋台詞＋聲音 */
function shotText(s, i, lead = "") {
  const parts = [];
  if (i === 0) parts.push(`[Shot 1]${lead ? ` ${lead}` : ""}`, s.camera.trim() ? sentence(cap(s.camera)) : "");
  else parts.push(`[Shot ${i + 1}] At ${ts(s.start)}, the camera cuts to ${s.camera.trim() ? sentence(s.camera.trim()) : "a new shot."}`);
  parts.push(sentence(s.picture));
  for (const ln of s.lines) if (ln.text.trim()) parts.push(lineText(ln));
  parts.push(sentence(cap(s.sound.trim())));
  return parts.filter(Boolean).join(" ");
}

/** 沒有編號的素材（接續、整段改造、骨架控制、釘住的聲音）要講清楚的事 */
function facts() {
  const out = [];
  const its = board.items;
  if (its.some((x) => x.use === "continue")) out.push("The target video continues directly from the exact final moment of the previous clip with no cut or reset.");
  const g = its.find((x) => x.use === "guide");
  if (g) out.push(`The target video edits a source video and keeps its timing, motion and camera${{ swap: ", replacing the main character", vfx: ", adding the requested visual effects", style: ", changing the visual style" }[g.lora] || ""}.`);
  if (its.some((x) => x.use === "control")) out.push("The body motion follows a pose control video exactly.");
  for (const a of its.filter((x) => x.kind === "audio" && x.use === "anchor")) out.push(`An exact audio track plays from ${(+a.secs).toFixed(2)} seconds and the picture matches it, with lips following any speech or singing.`);
  return out;
}

function assemble() {
  if (!board) return "";
  const L = labels();
  const its = board.items;
  const shots = board.shots;
  const sound = board.soundscape.trim() || "Natural ambient sound that matches the scene.";
  const music = board.music.trim() || "N/A";
  if (!refsPath()) {
    // 關鍵幀路：官方指示句＋三段
    const imgs = its.filter((x) => x.kind === "image");
    const f = imgs.find((x) => +x.secs === 0), l = imgs.find((x) => +x.secs === -1);
    const N = shots.length;
    let head = "";
    if (f && l) head = `How the reference pictures align with the target video — Picture 1 (from Shot 1) aligns with the 0.00-second mark of the target video; Picture 2 (from Shot ${N}) aligns with the ${durText()}-second mark of the target video.`;
    else if (f) head = "For the target video, at 0.00 seconds into the target video, <Picture 1> (from [Shot 1]) is fully referenced.";
    else if (l) head = `How the reference pictures align with the target video — <Picture 1> (from [Shot ${N}]) aligns with the ${durText()}-second mark of the target video.`;
    const lead = [board.style.trim() ? sentence(cap(board.style)) : "", ...facts()].filter(Boolean).join(" ");
    const body = shots.map((s, i) => shotText(s, i, i === 0 ? lead : "")).join(" ");
    return [head, `integrated_multimodal_description: ${body}`, `overall_soundscape: ${sound}`, `non_diegetic_music: ${music}`].filter(Boolean).join("\n\n");
  }
  // 參考路：六段
  const defs = [], keep = [], types = new Set();
  let k = 0;
  const appears = (x, tag) => {
    const hit = shots.map((s, i) => (s.picture.includes(tag) || sameChar(x).some((y) => s.picture.includes(L.get(y))) || (x.name && s.picture.includes(x.name)) ? `[Shot ${i + 1}]` : "")).filter(Boolean);
    return (hit.length ? hit : shots.map((_, i) => `[Shot ${i + 1}]`)).join(", ");
  };
  // AI 的描述常以名字開頭（「小克, a young woman…」）：名字前面已經寫了，去掉
  const note = (x) => {
    let t = x.note.trim().replace(/\.$/, "");
    if (x.name.trim() && t.startsWith(x.name.trim())) t = t.slice(x.name.trim().length).replace(/^[\s,，:：-]+/, "");
    return t ? `, ${t}` : "";
  };
  for (const x of its) {
    const tag = L.get(x);
    if (x.kind === "image" && x.use === "ref") {
      types.add("reference generation");
      if (x.role === "storyboard") {
        defs.push(`${tag} is a storyboard reference${x.shots.trim() ? ` for shots ${x.shots.trim()}` : ""}, defining their viewpoint, subject placement and shot order.`);
        keep.push(`${tag}: weak_reference - used only for shot planning; its drawing style is not copied.`);
        continue;
      }
      const same = sameChar(x);
      if (same.length && same[0] !== x) continue; // 同一個人的第二張以後：併進第一張那一條
      const S = `<Subject ${++k}>`;
      const lv = x.keep || (x.role === "character" ? "fully_preserved" : "partially_preserved");
      const from = same.length > 1 ? `one single character shown in several references: ${same.map((y) => `${VIEW_EN[y.view] || "appearance"} in ${L.get(y)}`).join(", ")}` : `whose appearance comes from ${tag}`;
      const tags = same.length > 1 ? same.map((y) => L.get(y)).join(" and ") : tag;
      const what = {
        character: `${x.name.trim() || "the character"}${x.speaker ? ` (${x.speaker})` : ""}, ${from}${note(x)}`,
        outfit: `the outfit shown in ${tag}${note(x)}`,
        scene: `the location shown in ${tag}${note(x)}`,
        prop: `the object shown in ${tag}${note(x)}`,
        style: `the visual style of ${tag}${note(x)}`,
      }[x.role] || `the content of ${tag}${note(x)}`;
      defs.push(`${S} is ${what}.`);
      const kept = { character: "the face, hair, outfit and body proportions", outfit: "the garments, colors and materials", scene: "the layout, lighting and key objects of the place", prop: "the shape, colors and details", style: "the rendering style, palette and line quality" }[x.role] || "its key features";
      keep.push(`${S} (appears in ${appears(x, tag)}): ${lv} - ${kept} from ${tags} ${lv === "weak_reference" ? "loosely guide" : "are carried into"} the target video.`);
    } else if (isPinImg(x)) {
      types.add("keyframe completion");
      const at = +x.secs === -1 ? `the last frame of the final shot, at ${durText()} seconds` : +x.secs === 0 ? "the first frame of [Shot 1], at 0.00 seconds" : `the exact frame at ${(+x.secs).toFixed(2)} seconds`;
      defs.push(`${tag} is ${at}${note(x)}.`);
      keep.push(`${tag}: fully_preserved - the target video shows exactly this frame at that moment.`);
    } else if (x.kind === "video" && x.use === "ref") {
      types.add("reference generation");
      if (x.role === "camera") {
        defs.push(`${tag} provides the camera movement, cuts and pacing structure only.`);
        keep.push(`${tag}: weak_reference - only its camera movement and pacing are followed; its people, clothing and setting are not carried over.`);
      } else if (x.role === "character") {
        const S = `<Subject ${++k}>`;
        defs.push(`${S} is ${x.name.trim() || "the person"} shown in ${tag}.`);
        keep.push(`${S} (appears in ${appears(x, tag)}): fully_preserved - the identity and outfit from ${tag} are retained.`);
      } else {
        const S = `<Subject ${++k}>`;
        defs.push(`${S} is the movement sequence, timing and body mechanics performed in ${tag}; it transfers to the main character without the source person's appearance, clothing or setting.`);
        keep.push(`${S}: attribute_transfer - the movement and rhythm of ${tag} are transferred, while the source person's identity, clothing and background are not carried over.`);
      }
      if (x.audioLabel) {
        types.add("audio reuse");
        defs.push(`${x.audioLabel} is the synchronized audio track of ${tag} and is reused in the target video.`);
        keep.push(`${x.audioLabel}: fully_copy - the audio of ${tag} is reused and the action stays in time with it.`);
      }
    } else if (x.kind === "audio" && x.use === "ref") {
      const lv = x.keep || (x.role === "voice" ? "reference" : "fully_copy");
      types.add(lv === "fully_copy" || lv === "partially_copy" ? "audio reuse" : "audio reference");
      const name = speakerName(x.speaker || "S1");
      const what = { voice: `the voice-timbre reference for ${name ? `${name} (${x.speaker || "S1"})` : `(${x.speaker || "S1"})`}; the speaker says new lines in this voice`, music: "the background music", sfx: "a sound-effect reference" }[x.role] || "an audio reference";
      defs.push(`${tag} is ${what}.`);
      keep.push(`${tag}: ${lv} - ${{ voice: "the timbre and delivery are followed, not the original words", music: "the music is used as the score", sfx: "the sound texture is followed" }[x.role] || "used as audio guidance"}.`);
    }
  }
  if (its.some((x) => x.use === "continue")) types.add("video continuation");
  if (its.some((x) => x.use === "guide")) types.add("video editing");
  if (its.some((x) => x.kind === "audio" && x.use === "anchor")) types.add("audio reuse");
  if (!types.size) types.add("reference generation");
  const summary = [board.summary.trim() ? sentence(board.summary) : "", ...facts()].filter(Boolean).join(" ") || "The target video follows the shots below.";
  const desc = [board.style.trim() ? `The target video is ${sentence(board.style.trim())}` : "", ...shots.map((s, i) => shotText(s, i))].filter(Boolean).join("\n");
  return [
    `subject_definitions:\n${defs.join("\n") || "N/A"}`,
    `summary:\n[${[...types].join(" + ")}] ${summary}`,
    `retention_analysis:\n${keep.join("\n") || "N/A"}`,
    `detailed_description:\n${desc}`,
    `overall_soundscape:\n${sound}`,
    `non_diegetic_music:\n${music}`,
  ].join("\n\n");
}

/** 分鏡、素材改了 → 重組完整提示詞（手改過就不蓋） */
function compose() {
  if (!board) return;
  if (!manual) F("h3Prompt").value = assemble();
  checkPrompt();
}

function checkPrompt() {
  const p = F("h3Prompt").value;
  const n = counts();
  const notes = [];
  const miss = [...new Set(p.match(/<(Picture|Video|Audio) \d+>/g) || [])].filter((tag) => {
    const [, k, i] = tag.match(/<(\w+) (\d+)>/);
    return +i < 1 || +i > n[k];
  });
  if (miss.length) notes.push(`提示詞提到 ${miss.join("、")}，素材板上沒有這個編號`);
  const left = [...new Set(p.match(/\{[a-z_]+\}/g) || [])];
  if (left.length) notes.push(`還有沒換掉的 ${left.join("、")}`);
  if (manual) notes.push("完整提示詞手改過：改分鏡不會再蓋掉（按「照分鏡重組」重新組）");
  lines(F("h3PromptNote"), notes, miss.length > 0 || left.length > 0);
}

// @ 選單：插入素材的名字
let atMenu = null;
function closeAt() { atMenu?.remove(); atMenu = null; }
document.addEventListener("pointerdown", (e) => { if (atMenu && !atMenu.contains(e.target)) closeAt(); }, true);
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && atMenu) { closeAt(); F("h3Prompt").focus(); } });
function openAt(anchor, replaceAt) {
  closeAt();
  if (!board) return;
  const ta = F("h3Prompt");
  const L = labels();
  const items = [];
  for (const [x, tag] of L) {
    items.push([tag, x.name || USES[x.kind].find(([v]) => v === (x.use === "ref" ? `ref:${x.role}` : x.use))?.[1] || KIND_NAME[x.kind]]);
    if (x.audioLabel) items.push([x.audioLabel, `${tag} 的原聲`]);
  }
  if (!items.length) return toast("素材板上還沒有可以點名的素材", "err");
  atMenu = el("div", { class: "play-menu", role: "menu" }, ...items.map(([tag, role]) => el("button", { type: "button", onclick: () => {
    closeAt();
    const at = ta.selectionStart;
    const from = replaceAt && ta.value[at - 1] === "@" ? at - 1 : at;
    ta.setRangeText(tag, from, ta.selectionEnd, "end");
    ta.focus();
    manual = true;
    checkPrompt();
  } }, `${tag} ${role}`)));
  document.body.append(atMenu);
  const r = anchor.getBoundingClientRect();
  atMenu.style.left = `${Math.min(r.left, innerWidth - 260)}px`;
  atMenu.style.top = `${Math.min(r.bottom + 4, innerHeight - atMenu.offsetHeight - 8)}px`;
}

// ---------------------------------------------------------------- 長度與畫面
function paintSize() {
  if (!board) return;
  // 跟 h3.rs 一樣：首幀 → 第一張釘圖 → 接續／原片／骨架影片 → 比例選項
  const pins = board.items.filter((x) => isPinImg(x) && x.m?.w);
  const vid = board.items.find((x) => special(x) && x.m?.w);
  const from = pins.find((x) => +x.secs === 0) || pins[0] || vid;
  F("h3Aspect").disabled = !!from;
  const [aw, ah] = from ? [from.m.w, from.m.h] : F("h3Aspect").value.split(":").map(Number);
  // 跟 graphs::fit_pixels 一樣：縮到大約這麼多像素、對齊 32（四捨六入五成雙）、最小 256
  // 高畫質二采：一采做 1/2.25 的像素，再放大 1.5 倍（跟 h3.rs 一樣）
  const two = F("h3TwoPass").checked && !F("h3TwoPass").disabled;
  const sc = Math.sqrt((QUALITY_MP[F("h3Quality").value] / (two ? 2.25 : 1) * 1048576) / (aw * ah));
  const snap = (v) => { const x = v / 32, f = Math.floor(x), d = x - f; return Math.max(256, (d > 0.5 || (d === 0.5 && f % 2) ? f + 1 : f) * 32); };
  const up = (v) => (two ? Math.round((v * 1.5) / 32) * 32 : v);
  const w = up(snap(aw * sc)), h = up(snap(ah * sc));
  F("h3SizeNote").replaceChildren(`約 ${w}×${h}・${frames(secs())} 格（${durText()} 秒，每秒 24 格）`, from ? "・比例照素材" : "");
  F("h3Est").textContent = `${durText()} 秒・約 ${w}×${h}`;
}

// ================================================================ 在哪跑
function livePacks() {
  if (!cloud) return null;
  return cloud.pod_packs || cloud.packs || null;
}

function paintWhere() {
  $$("#h3Where button").forEach((b) => { const on = b.dataset.w === where; b.classList.toggle("on", on); b.setAttribute("aria-selected", String(on)); });
  F("h3Cloud").hidden = where !== "cloud";
  F("h3Local").hidden = where !== "local";
  F("h3Rh").hidden = where !== "runninghub";
}

// ---------------------------------------------------------------- RunningHub
let rhub = null;     // GET /api/runninghub
let rhCoins = null;  // 測試回來的餘額

async function refreshRh() {
  try { rhub = await api("/api/runninghub"); } catch { rhub = null; }
  paintRh();
}

function paintRh() {
  if (!rhub) return;
  F("h3RhDot").className = "ag-dot" + (rhub.has_key ? (rhCoins != null ? " on" : "") : " err");
  F("h3RhMsg").textContent = rhub.has_key ? (rhCoins != null ? `連上了：剩 ${rhCoins} RH 幣` : "金鑰存好了（按「測試」看能不能用）") : "還沒填 API Key";
  F("h3RhKey").placeholder = rhub.has_key ? "已存（要換才貼新的）" : "貼上 API Key（存起來會加密）";
  F("h3RhSite").value = rhub.site;
  F("h3RhInst").value = rhub.instance;
  F("h3RhModels").value = Object.entries(rhub.models || {}).map(([a, b]) => `${a} => ${b}`).join("\n");
  const rate = (rhub.rates || []).find((r) => r.instance === rhub.instance);
  F("h3RhMeta").textContent = rate ? `${rhub.instance}：每秒約 ${rate.coins_per_sec} RH 幣，10 秒的影片大約要算幾分鐘` : "";
}

/** 「這邊 => 那邊」一行一個 → {這邊: 那邊} */
function rhModels() {
  const out = {};
  for (const line of F("h3RhModels").value.split("\n")) {
    const [a, b] = line.split("=>").map((x) => (x || "").trim());
    if (a && b) out[a] = b;
  }
  return out;
}

async function saveRh() {
  const body = { site: F("h3RhSite").value, instance: F("h3RhInst").value, models: rhModels() };
  if (F("h3RhKey").value.trim()) body.key = F("h3RhKey").value.trim();
  try {
    rhub = await api("/api/runninghub/config", { json: body });
    F("h3RhKey").value = "";
    toast("RunningHub 設定存好了", "ok");
  } catch (e) { toast(e.message, "err"); }
  paintRh();
}

async function testRh() {
  try {
    const r = await api("/api/runninghub/test", { json: {}, timeout: 60000 });
    rhCoins = r.coins ?? "?";
    toast(`RunningHub 連上了：剩 ${rhCoins} RH 幣${r.running != null ? `，正在跑 ${r.running} 個任務` : ""}`, "ok");
  } catch (e) { rhCoins = null; toast(e.message, "err"); }
  paintRh();
}

const priceRange = (g) => agent.priceRange(g);
const packNames = (p) => [p.h3 && "H3 影片", p.h3x && "H3 進階玩法", p.agent && "生圖助手"].filter(Boolean).join("、") || "（沒勾）";
const packGb = (p) => 32 * !!p.agent + 42 * !!p.h3 + 32 * !!p.h3x;

function paintCloud() {
  paintWhere();
  const st = cloud;
  if (!st) return;
  const kind = st.kind || st.provider || "runpod";
  F("h3CloudDot").className = "ag-dot" + ({ ready: " on", starting: " busy", stopping: " busy", error: " err", left: " err" }[st.phase] || "");
  F("h3CloudMsg").textContent = st.msg || { off: "沒開", ready: "雲端好了", starting: "開機中…", stopping: "關機中…" }[st.phase] || st.phase;
  const packs = livePacks() || {};
  const meta = [];
  meta.push(st.pod ? `這台裝了：${packNames(packs)}` : `開機會裝：${packNames(packs)}`);
  if (kind === "runpod") meta.push(`${st.gpu}・每小時約 US$${(+st.price).toFixed(2)}` + (st.pod ? `・已開 ${st.minutes} 分鐘・目前約 US$${(+st.cost).toFixed(2)}` : ""));
  if (st.idle_left != null) meta.push(`再閒置 ${st.idle_left} 分鐘自動關`);
  if (st.missing && !st.pod) meta.push(st.missing);
  lines(F("h3CloudMeta"), meta);
  const notes = [];
  if (!packs.h3) notes.push("雲端設定還沒勾「H3 影片」：按「雲端設定…」勾起來");
  else if (needsAdv() && !packs.h3x) notes.push("這支用到「H3 進階玩法」（社群 LoRA 或骨架控制）：按「雲端設定…」勾起來");
  if (notes.length && st.pod) notes.push("機器開著時改功能包，要先關機再開才會裝");
  lines(F("h3CloudNote"), notes, true);
  // 選顯卡（RunPod 才有）：機器開著就鎖住
  const busy = ["starting", "ready", "stopping"].includes(st.phase);
  const sel = F("h3CloudGpu");
  F("h3CloudGpuRow").hidden = kind !== "runpod";
  agent.fillGpus(sel, st);
  if (st.pod || busy) sel.value = st.gpu_key;
  paintGpuNote();
  sel.disabled = busy || !!st.pod;
  F("h3CloudStart").disabled = busy || (!st.has_key && !st.pod);
  F("h3CloudStart").textContent = st.phase === "left" ? "接回來" : { runpod: "開雲端", ssh: "連線並安裝", url: "連線" }[kind];
  F("h3CloudStop").textContent = { runpod: "關機刪除", ssh: "停止", url: "斷線" }[kind];
  F("h3CloudStop").disabled = !st.pod && !["starting", "ready"].includes(st.phase);
  F("h3CloudCfg").disabled = busy || !!st.pod;
  if (busy) F("h3CloudConfirm").hidden = true;
}

function paintGpuNote() {
  const g = (cloud?.gpus || []).find((x) => x.key === F("h3CloudGpu").value);
  F("h3CloudGpuNote").textContent = cloud?.pod ? "" : agent.gpuNote(g, cloud);
  F("h3CloudGpuNote").style.whiteSpace = "pre-wrap";
}

function paintConfirm() {
  paintGpuNote();
  const g = (cloud?.gpus || []).find((x) => x.key === F("h3CloudGpu").value);
  const packs = livePacks() || {};
  F("h3CloudPrice").textContent = g ? `${g.name}，每小時約 US$${priceRange(g)}` : "";
  const warn = [`開著就一直計費，按「關機刪除」或閒置太久自動關才會停。這次會裝：${packNames(packs)}，開機要在雲端下載約 ${packGb(packs)} GB（機房網路很快），開機到能用大約 5～10 分鐘，這段時間也算錢。`];
  if (g && packs.agent && packs.h3 && g.vram < 90) warn.push(`${g.name} 放不下生圖助手＋H3：選 RTX PRO 6000 96GB，或到「雲端設定…」只勾 H3。`);
  lines(F("h3CloudWarn"), warn);
}

async function startCloud(body) {
  try {
    await api("/api/agent/cloud/start", { json: body });
    toast("雲端開機中：下載模型＋安裝，進度看這裡的燈號", "ok");
  } catch (e) { toast(e.message, "err"); }
  refreshCloud();
}

function paintLocal() {
  const l = info?.local;
  if (!l) return;
  const out = [];
  if (!l.online) out.push("這台電腦的 ComfyUI 沒有連線。");
  else if (!l.nodes) out.push("這台電腦的 ComfyUI 沒有 MiniMax H3 節點：要更新到新版 ComfyUI。");
  else out.push(l.models.length ? `找到 H3 模型：${l.models.join("、")}` : "ComfyUI 有 H3 節點，但 diffusion_models 裡沒有 H3 模型（10Eros 或官方 H3）。");
  if (l.pick) out.push(`模型選「預設」時本機用：${l.pick}`);
  if (l.online && l.nodes && !l.chunk) out.push("沒裝 ComfyUI-KJNodes：「低顯存」切塊用不了（8～12GB 顯卡建議裝）。");
  out.push("8GB 顯卡用 10Eros w4a8（14GB）＋低顯存切塊跑得動：512×768 5 秒約 3～4 分鐘、640×960 約 6 分鐘；記憶體要 32GB。安裝精靈的「H3 影片（本機，選配）」包會下載它。");
  lines(F("h3Local"), out);
}

// ---------------------------------------------------------------- 進階：模型、LoRA
function paintModels() {
  if (!info) return;
  const d = info.defaults;
  const pool = where === "local" ? info.local?.models : info.cloud_models;
  const names = [...new Set([d.model, d.official_ref2va, ...(pool || [])])];
  const sel = F("h3Model");
  const cur = sel.value;
  const def = where === "local" && info.local?.pick ? `預設：${info.local.pick}（本機最省顯存的）` : "預設：10Eros-Max（無審核、8 步）";
  sel.replaceChildren(el("option", { value: "" }, def),
    ...names.filter((n) => n !== d.model).map((n) => el("option", { value: n }, n === d.official_ref2va ? "官方 H3 ref2va（會自動加 8 步 Turbo LoRA）" : n)));
  sel.value = [...sel.options].some((o) => o.value === cur) ? cur : "";
  const sl = where === "local" ? info.local?.selflift : info.cloud_selflift;
  F("h3SelfLift").disabled = !sl;
  F("h3SelfLiftNote").textContent = sl ? "" : where === "cloud" ? "（雲端開好才知道有沒有裝）" : "（這台電腦的 ComfyUI 沒裝 SelfLiftH3Sampler 節點）";
  paintLoras();
}

function allLoras() {
  return (where === "local" ? info?.local?.loras : info?.cloud_loras) || [];
}

function loraPool() {
  return allLoras().filter((n) => /h3|minimax/i.test(n));
}

/** LoRA 組合包（照 YZ金鱼「文武雙全」工作流的組合與強度）：[檔名關鍵字, 強度, 找不到時顯示的名字] */
const PACKS = {
  repair: [["motionrepair", 0.5, "Motion_Repair_V2"]],
  combat: [["h3combat", 0.65, "H3_Combat_V2"], ["motionrepair", 0.35, "Motion_Repair_V2"], ["fightflow", 0.35, "H3_Fight_Flow_V1"], ["speedslider", 3, "H3_speed_slider_1.1"]],
  adult: [["mysticxxx", 0.35, "MysticXXX_MMH3"]],
};
const squash = (n) => n.toLowerCase().replace(/[^a-z0-9]/g, "");

/** 換組合包：拿掉上一包加的、照檔名找這一包的（同名多版挑排最後的＝通常是最新版），找不到的列出來 */
function applyPack(key) {
  loras = loras.filter((l) => !l.pack);
  const miss = [];
  for (const [k, strength, label] of PACKS[key] || []) {
    const name = allLoras().filter((n) => squash(n).includes(k)).sort().pop();
    if (name) loras.push({ name, strength, pack: true });
    else miss.push(label);
  }
  F("h3PackNote").textContent = miss.length ? `找不到：${miss.join("、")}。到 Civitai 搜這個名字，下載到 ComfyUI 的 loras 資料夾（雲端要放到雲端機器上），再重開這一頁。` : "";
  paintLoras();
}

function paintLoras() {
  const pool = loraPool();
  F("h3Loras").replaceChildren(...loras.map((l, i) => {
    const sel = el("select", { class: "sm", "aria-label": "LoRA" }, el("option", { value: "" }, "（選一顆）"), ...[...new Set([...pool, l.name].filter(Boolean))].map((n) => el("option", { value: n }, n)));
    sel.value = l.name || "";
    sel.onchange = () => { l.name = sel.value; };
    // 速度滑桿 LoRA 的強度就是速度（作者建議 ±8 以內）
    const wide = squash(l.name || "").includes("speedslider");
    const st = el("input", { type: "number", min: wide ? -10 : -2, max: wide ? 10 : 2, step: wide ? 1 : 0.05, value: l.strength, "aria-label": "強度", style: "width:80px" });
    st.oninput = () => { l.strength = +st.value; };
    return el("div", { class: "row gap" }, sel, st, btn("拿掉", () => { loras.splice(i, 1); paintLoras(); }));
  }), ...(pool.length ? [] : [el("div", { class: "muted sm" }, where === "cloud" ? "雲端開好後才列得出來（換角色、特效的社群 LoRA 會照素材用途自動掛，不用自己加）。" : "這台電腦的 ComfyUI loras 裡沒有 H3 的 LoRA。")]));
}


// ================================================================ 送出
/** 雲端要勾「H3 進階玩法」：社群 LoRA（換角色、特效、換畫風、動態桌布）、骨架控制（跟 api_h3::needs_advanced 一樣） */
function needsAdv() {
  return !!board && (!!board.lora || board.items.some((x) => x.use === "control" || (x.use === "guide" && x.lora)));
}

function params() {
  const items = board.items.filter((x) => x.m).map((x) => {
    const o = { kind: x.kind, name: x.m.name, use: x.use };
    if (x.use === "anchor") o.secs = +x.secs;
    if (x.kind === "video" && ["ref", "continue", "guide"].includes(x.use)) o.audio = !!x.audio && !!x.m.audio;
    if (x.use === "guide") o.lora = x.lora || "";
    return o;
  });
  const p = { h3_mode: "board", items, prompt: F("h3Prompt").value.trim(), seconds: secs(), aspect: F("h3Aspect").value, quality: F("h3Quality").value };
  if (board.lora) p.lora_mode = board.lora;
  const ctl = board.items.find((x) => x.use === "control");
  if (ctl) { p.control_type = ctl.control_type || "pose"; p.control_strength = Number.isFinite(+ctl.strength) ? +ctl.strength : 1; }
  const vid = board.items.find((x) => special(x) && x.m?.w);
  if (vid) p.src_wh = [vid.m.w, vid.m.h];
  if (F("h3Model").value) p.model = F("h3Model").value;
  if (F("h3Chunks").value) p.ff_chunks = +F("h3Chunks").value;
  p.steps = +F("h3Steps").value || 8;
  p.sampler = F("h3Sampler").value.trim() || "res_multistep";
  p.scheduler = F("h3Scheduler").value.trim() || "simple";
  if (F("h3TwoPass").checked && !F("h3TwoPass").disabled) p.two_pass = true;
  else if (F("h3SelfLift").checked && !F("h3SelfLift").disabled) p.selflift = true;
  if (F("h3Speedup").checked) p.speedup = true;
  const seed = F("h3Seed").value.trim();
  p.seed = seed === "" ? -1 : +seed;
  p.loras = loras.filter((l) => l.name).map((l) => ({ name: l.name, strength: +l.strength || 1 }));
  // 拍攝單勾了「用 RefMod 鎖臉」：鎖定、做過 RefMod 的角色（後端 refmod.rs 檢查名字）
  if (fromSheet?.refmod) {
    const mods = (fromSheet.cast || []).filter((c) => c.locked && c.refmod).map((c) => c.refmod);
    if (mods.length) p.refmods = mods;
  }
  return p;
}

/** 送出前在網頁先擋：沒放的格子、素材板的問題、分鏡時間、提示詞編號 */
function precheck() {
  if (!board) return "先選一個起手式";
  const L = labels();
  const empty = board.items.find((x) => !x.m);
  if (empty) return `還沒放：${L.get(empty) || KIND_NAME[empty.kind]}（放素材，或按 × 拿掉這格）`;
  if (board.items.some((x) => x.busy)) return "素材還在處理中，等一下";
  const bad = boardProblems();
  if (bad.length) return bad[0];
  if (!manual) {
    const d = dur();
    for (let i = 1; i < board.shots.length; i++) {
      const t = +board.shots[i].start;
      if (!(t > +board.shots[i - 1].start) || t >= d) return `鏡 ${i + 1} 的開始秒數要比上一鏡晚、也要在影片長度（${durText()} 秒）以內`;
    }
    if (!board.shots.some((s) => s.picture.trim() || s.lines.some((l) => l.text.trim()))) return "分鏡還是空的：寫點子按「AI 幫我寫滿分鏡」，或自己在鏡 1 寫畫面";
  }
  if (!F("h3Prompt").value.trim()) return "要寫提示詞";
  return "";
}

async function go() {
  const bad = precheck();
  if (bad) return toast(bad, "err");
  if (fromSheet && !(await (await import("./shotsheet.js")).precheck([F("h3Prompt").value], fromSheet))) return;
  const b = F("h3Go");
  b.disabled = true;
  // 等回應時可能已經換了起手式：名字先記下來
  const name = board.name || "H3 影片";
  try {
    const r = await api("/api/h3/generate", { json: { params: params(), count: +F("h3Count").value || 1, where }, timeout: 15 * 60 * 1000 });
    addTask(r.task, name);
    setStatus(`已送出：${name}`);
  } catch (e) { toast(e.message, "err"); }
  finally { b.disabled = false; }
}

/** 給 AI 看的素材清單：標籤＋英文用途（沒有標籤的也列，它才知道有接續、釘聲音這些事） */
function writerMedia() {
  const L = labels();
  const out = [];
  const keepWord = (k) => ({ fully_preserved: "keep fully", partially_preserved: "keep mostly", attribute_transfer: "take only its features", weak_reference: "loose reference" }[k] || "keep");
  for (const x of board.items) {
    if (!x.m) continue;
    const tag = L.get(x) || "";
    let role = "";
    if (x.kind === "image" && x.use === "ref") {
      role = x.role === "character" ? `character "${x.name || "unnamed"}"${x.speaker ? ` (speaker ${x.speaker}${x.voice.trim() ? `, voice: ${x.voice.trim()} (the site adds this to the prompt itself)` : ""})` : " (does not speak)"}, ${keepWord(x.keep || "fully_preserved")}`
        : x.role === "storyboard" ? `storyboard${x.shots ? ` for shots ${x.shots}` : ""}`
          : `${x.role} reference, ${keepWord(x.keep || "partially_preserved")}`;
      if (sameChar(x).length > 1) role += `; one of several references of the same single person, showing the ${VIEW_EN[x.view] || "appearance"}`;
    } else if (isPinImg(x)) role = +x.secs === -1 ? "pinned as the last frame" : `pinned as the exact frame at ${(+x.secs).toFixed(2)} s`;
    else if (x.kind === "video" && x.use === "ref") role = { motion: "motion source (copy only the movement)", camera: "camera source (copy only camera movement and pacing)", character: `character "${x.name || "unnamed"}" from this video` }[x.role] + (x.audioLabel ? `; its soundtrack is ${x.audioLabel} and is reused` : "");
    else if (x.use === "continue") role = "this clip continues from the end of this previous clip";
    else if (x.use === "guide") role = `whole-video edit source: ${{ swap: "replace its main character", vfx: "add visual effects", style: "restyle it" }[x.lora] || "keep its timing and motion"}`;
    else if (x.use === "control") role = "pose control video: follow its body motion exactly";
    else if (x.kind === "audio" && x.use === "ref") role = { voice: `voice timbre for speaker ${x.speaker || "S1"} (new lines in this voice)`, music: "background music", sfx: "sound effects" }[x.role] || "audio reference";
    else if (x.kind === "audio" && x.use === "anchor") role = `exact audio placed from ${(+x.secs).toFixed(2)} s; the picture must match it (lip-sync if it is speech or singing); do not write "lines" for this audio, describe only the visible delivery`;
    out.push({ label: tag, role, name: x.kind === "image" ? x.m.name : "" });
  }
  return out;
}

async function write() {
  if (!board) start(STARTS[0]);
  const idea = F("h3Idea").value.trim();
  if (!idea) return toast("先用中文寫想要的畫面、動作、台詞或聲音", "err");
  const b = F("h3Write");
  b.disabled = true;
  setStatus("AI 寫分鏡中…", "");
  const was = board;
  const plan = { shots: board.shots.filter((s) => s.picture.trim() || s.lines.length), style: board.style, summary: board.summary };
  try {
    const r = await api("/api/h3/write", { json: { idea, mode: "board", path: refsPath() ? "Ref2VA (six sections)" : "keyframe / text-only (three sections)",
      seconds: secs(), media: writerMedia(), plan, brain: F("h3Brain").value }, timeout: 10 * 60 * 1000 });
    // 寫的這段時間換了一支：不要蓋到別支
    if (board !== was) { setStatus("AI 寫好了，但已經換了一支：沒有填進去", ""); return; }
    if (r.plan) {
      applyPlan(r.plan, labels());
      manual = false;
      paintBoard();
      paintShots();
      compose();
      setStatus("AI 寫好了：看一下分鏡再送出");
    } else {
      F("h3Prompt").value = r.prompt;
      manual = true;
      F("h3PromptFold").open = true;
      checkPrompt();
      setStatus("AI 沒照分鏡格式回：整段放進「完整提示詞」了，看一下再送出", "");
    }
  } catch (e) { toast(e.message, "err"); setStatus("AI 沒寫成", "r"); }
  finally { b.disabled = false; }
}

// ---------------------------------------------------------------- 工作列表、成品
function addTask(task, name) {
  // websocket 的「排隊」可能比送出的回應先到：那時先用模式名，回應到了換成玩法名
  if (tasks.has(task)) {
    const t = tasks.get(task);
    if (name) { t.name = name; t.row.querySelector("b").textContent = name; }
    return t;
  }
  const st = el("span", { class: "h3-st" }, "排隊");
  const msg = el("span", { class: "grow" }, "");
  const stop = btn("停止", async () => {
    try { await api("/api/h3/stop", { json: { task } }); } catch (e) { toast(e.message, "err"); }
  }, "akey sm warn");
  const row = el("div", { class: "h3-job" }, st, el("b", {}, name), msg, stop);
  const box = F("h3Jobs");
  if (!tasks.size) box.replaceChildren();
  box.prepend(row);
  const t = { row, st, msg, stop, name };
  tasks.set(task, t);
  return t;
}

async function addOuts(ids) {
  for (const id of ids) {
    if (outs.some((r) => r.id === id)) continue;
    try { addOutRec(await api(`/api/image/${id}`)); } catch { /* 被刪了 */ }
  }
}

function addOutRec(rec) {
  if (!rec || outs.some((r) => r.id === rec.id)) return;
  outs = [rec, ...outs];
  const box = F("h3Out");
  if (outs.length === 1) box.replaceChildren();
  box.prepend(el("div", { class: "h3-outcell" },
    el("button", { type: "button", class: "h3-thumb", title: "打開", onclick: () => openLightboxRecord(rec, outs) }, el("img", { src: `/thumb/${rec.id}`, alt: "", loading: "lazy" })),
    btn("接著拍", () => continueFrom(rec), "akey sm", "新的一支：從這支的結尾接著拍")));
}

/** websocket：h3（工作進度）、agent_cloud（雲端狀態） */
export function onEvent(m) {
  if (m.type === "agent_cloud") {
    const was = cloud?.phase;
    cloud = m.status;
    paintCloud();
    // 雲端剛開好：重讀一次，雲端的模型、LoRA 選單才有東西
    if (cloud?.phase === "ready" && was !== "ready") reload();
    return;
  }
  if (m.type !== "h3") return;
  const t = tasks.get(m.task) || addTask(m.task, MODE_NAME[m.mode] || "H3");
  t.st.textContent = STATE_NAME[m.state] || m.state;
  t.st.className = "h3-st " + m.state;
  t.msg.textContent = m.msg || "";
  t.stop.hidden = END_STATES.includes(m.state);
  if (m.ids) addOuts(m.ids);
  for (const j of m.jobs || []) localJobs.add(j);
  setStatus(`${t.name}：${m.msg || STATE_NAME[m.state] || ""}`, m.state === "error" ? "r" : "g");
  if (m.state === "error") toast(`${t.name}：${m.msg}`, "err");
  if (m.state === "done" && m.ids?.length) toast(`${t.name}：做好了，${m.ids.length} 支進圖庫`, "ok");
}

/** 本機 ComfyUI 做好的（job 是 run_local 排進去的那幾個） */
export function onImage(job, rec) {
  if (!localJobs.has(job)) return;
  localJobs.delete(job);
  addOutRec(rec);
}


// ================================================================ 從別的地方帶進來
/** 生成頁、圖庫的「動起來」（影片模型選 H3 時）：這張圖當首幀 */
export async function startFrom(rec) {
  app.showPage("director");
  const studio = await import("./studio.js");
  studio.openTab("h3");
  await show();
  if (!info) return;
  start({ ...STARTS[0], name: "圖生影片", items: [{ kind: "image", use: "anchor", secs: 0 }], desc: "這張圖是第 0 秒的畫面（首幀）。要加角色圖、聲音、動作參考，按「＋ 圖／影片／聲音」。" });
  await fill(board.items[0], { gallery_id: rec.id });
  toast("已帶進 H3 影片台：這張圖是首幀", "ok");
}

/** 成品的「接著拍」：新的一支，素材板放這支影片、用途「從它結尾接著拍」 */
async function continueFrom(rec) {
  start(STARTS.find((s) => s.id === "cont"));
  await fill(board.items[0], { gallery_id: rec.id });
  setStatus(`接著拍：從 #${rec.id} 的結尾開始`);
}

/** 音檔有多長（秒）：讀不到回 0 */
const audioSecs = (name) => new Promise((res) => {
  const a = new Audio(`/input/${encodeURIComponent(name)}`);
  a.onloadedmetadata = () => res(a.duration || 0);
  a.onerror = () => res(0);
});

/** 資產抽屜點一個資產：圖、音檔放進素材板。角色的圖帶名字、聲音、用途，同名算同一個人（assemble 併成一條 <Subject>）；
 *  先填空著的同類格子（起手式留的），放過的不重複 */
async function placeAsset(it, p, toInput) {
  if (!board) start(STARTS[0]);
  const was = board;
  const role = { char: "character", scene: "scene", outfit: "outfit", prop: "prop", fx: "style" }[it.kind];
  const used = (name) => board.items.some((x) => x.m?.name === name);
  const had = board.items.find((x) => x.asset === it.id && x.speaker);
  const speaker = it.kind !== "char" ? "" : had ? had.speaker
    : (p.voice || p.audio) ? SPEAKERS.find((s) => !board.items.some((x) => x.speaker === s)) || "" : "";
  let put = 0;
  for (const f of p.images || []) {
    if (board.items.filter((x) => x.kind === "image" && x.m).length >= MAX.image) { toast(`圖最多 ${MAX.image} 張`, "err"); break; }
    const r = await toInput(f);
    if (board !== was) return;
    if (used(r.name)) continue;
    const pin = it.kind === "frame" && !board.items.some((x) => isPinImg(x) && +x.secs === 0);
    const fields = { use: pin ? "anchor" : "ref", role: pin ? "" : role || "character", secs: 0, asset: it.id, view: p.views?.[f] || "", name: it.kind === "char" ? it.name : "",
      note: it.kind === "char" ? p.spec?.en || it.data?.look_en || "" : "", speaker, voice: it.kind === "char" ? p.voice || "" : "",
      m: { name: r.name, kind: "image", w: r.width || 0, h: r.height || 0, secs: 0, audio: false } };
    const empty = board.items.find((x) => x.kind === "image" && !x.m && x.use === fields.use && (x.use !== "ref" || x.role === fields.role));
    if (empty) Object.assign(empty, fields); else board.items.push(newItem({ kind: "image", ...fields }));
    put++;
  }
  if (p.audio) {
    const r = await toInput(p.audio);
    if (board !== was) return;
    if (!used(r.name)) {
      const secs = await audioSecs(r.name);
      if (board !== was) return;
      const fields = { use: "ref", role: it.kind === "sound" ? "sfx" : "voice", speaker, asset: it.id, m: { name: r.name, kind: "audio", w: 0, h: 0, secs, audio: true } };
      const empty = board.items.find((x) => x.kind === "audio" && !x.m && x.use === "ref" && x.role === fields.role);
      if (empty) Object.assign(empty, fields);
      else if (board.items.filter((x) => x.kind === "audio" && x.use === "ref" && x.m).length >= MAX.audio) toast(`參考聲音最多 ${MAX.audio} 段`, "err");
      else board.items.push(newItem({ kind: "audio", ...fields }));
      put++;
    }
  }
  if (!put) return;
  paintBoard();
  compose();
  paintSize();
  toast(`「${it.name}」放進素材板了`, "ok");
}

// ================================================================ 初始化
export function init() {
  // H3 影片台：中文外觀接在點子後面，圖和音檔放進素材板
  drawer.mount(F("h3Assets"), { family: () => "h3", insert: drawer.appendTo(F("h3Idea")), images: { place: placeAsset } });
  F("h3Quality").value = "std";  // 瀏覽器重新整理會還原上次的選擇：預設一律標準
  setSecs(5);
  start(STARTS[0]);
  F("h3Tpl").onchange = () => {
    const t = info?.templates.find((x) => x.id === F("h3Tpl").value);
    if (t) startTpl(t);
  };
  F("h3AddImage").onclick = () => addItem("image");
  F("h3AddVideo").onclick = () => addItem("video");
  F("h3AddAudio").onclick = () => addItem("audio");
  F("h3AddShot").onclick = () => {
    const last = board.shots[board.shots.length - 1];
    const t = Math.min(dur() - 0.5, (+last?.start || 0) + Math.max(1, Math.round((dur() - (+last?.start || 0)) / 2)));
    board.shots.push(newShot(Math.max(0.5, +t.toFixed(1))));
    paintShots();
    compose();
  };
  for (const [id, k] of [["h3Style", "style"], ["h3Summary", "summary"], ["h3Soundscape", "soundscape"], ["h3Music", "music"]]) {
    F(id).oninput = () => { board[k] = F(id).value; compose(); };
  }
  F("h3Prompt").addEventListener("input", (e) => {
    manual = true;
    checkPrompt();
    if (e.data === "@") openAt(F("h3Prompt"), true);
  });
  F("h3At").onclick = () => openAt(F("h3At"), false);
  F("h3Reset").onclick = () => { manual = false; compose(); };
  F("h3Secs").onchange = () => { paintBoard(); paintRuler(); compose(); paintSize(); };
  F("h3Quality").onchange = paintSize;
  // 二采跟 SelfLift 只能選一個
  F("h3TwoPass").onchange = () => { if (F("h3TwoPass").checked) F("h3SelfLift").checked = false; paintSize(); };
  F("h3SelfLift").onchange = () => { if (F("h3SelfLift").checked) F("h3TwoPass").checked = false; paintSize(); };
  F("h3LoraPack").onchange = () => applyPack(F("h3LoraPack").value);
  F("h3Aspect").onchange = paintSize;
  F("h3Where").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-w]");
    if (!b) return;
    where = b.dataset.w;
    store.set("h3Where", where);
    paintCloud();
    paintModels();
  });
  F("h3CloudGpu").onchange = paintConfirm;
  F("h3CloudGpuRe").onclick = () => agent.refreshGpus(refreshCloud);
  F("h3CloudStart").onclick = () => {
    const kind = cloud?.kind || cloud?.provider || "runpod";
    if (kind === "runpod" && !cloud?.pod) { paintConfirm(); F("h3CloudConfirm").hidden = false; return; }
    startCloud({});
  };
  F("h3CloudNo").onclick = () => { F("h3CloudConfirm").hidden = true; };
  F("h3CloudYes").onclick = () => {
    F("h3CloudConfirm").hidden = true;
    const body = { confirm: true };
    if (!cloud?.pod) body.gpu = F("h3CloudGpu").value;
    startCloud(body);
  };
  F("h3CloudStop").onclick = async () => {
    const kind = cloud?.kind || cloud?.provider || "runpod";
    if (!confirm({ runpod: "關機並刪除雲端機器？刪掉就不再計費；下次開要重新下載模型。", ssh: "停止主機上的服務並斷線？主機本身要到那家的網站關機才會停止計費。", url: "斷開雲端網址？" }[kind])) return;
    try { cloud = await api("/api/agent/cloud/stop", { json: {} }); paintCloud(); toast(cloud.msg || "已停止", "ok"); }
    catch (e) { toast(kind === "runpod" ? `停止失敗：${e.message}（請到 RunPod 網站確認機器有沒有刪掉）` : `停止失敗：${e.message}`, "err"); }
  };
  F("h3CloudCfg").onclick = () => agent.openCloudSettings();
  F("h3RhSave").onclick = saveRh;
  F("h3RhTest").onclick = testRh;
  // 雲端設定視窗存檔後（勾了 H3 包）重讀
  document.addEventListener("aw-cloud-cfg", () => { refreshCloud(); if (info) load().catch(() => {}); });
  F("h3LoraAdd").onclick = () => { loras.push({ name: "", strength: 1 }); paintLoras(); };
  F("h3Write").onclick = write;
  F("h3Go").onclick = go;
  // 雲端開著時每 30 秒更新一次（已開幾分鐘、花了多少）
  setInterval(() => { if (app.page === "director" && !F("h3Work").closest("section").hidden && !document.hidden && cloud?.pod) refreshCloud(); }, 30000);
}

// 拍攝單（2026-10-24，子專案 2）：影片分頁最上面那張表。角色、首幀、參考圖、場景、鏡位、聲音、每段的時間軸，
// 後端照固定結構組成 H3 提示詞（/api/shotsheet/*，src-tauri\src\shotsheet.rs），送出前健檢，套用到下面的短片（h3.js）或長片（h3chain.js）。
// 草稿存在 localStorage 的 aw_shotsheet；範本是資產庫 kind=shot。設計在 design\資產庫與影片台合併_設計.md 第 5 節。
import { el, api, toast, store, debounce, openModal, closeModal, pickFile, uploadFile } from "./util.js";
import { pickFromGallery } from "./gallery.js";
import * as studio from "./studio.js";
import * as assets from "./assets.js";
import * as h3 from "./h3.js";
import * as h3chain from "./h3chain.js";

const F = (id) => document.getElementById(id);
const LANGS = [["", "跟角色"], ["Chinese", "中文"], ["Japanese", "日文"], ["English", "英文"], ["Korean", "韓文"], ["Cantonese", "粵語"]];
/** 語言代碼／中文名 → 選單用的英文名（跟 shotsheet.rs 的 lang_name 同一張表；空的、認不得的留空＝中文） */
const LANG_ALIAS = { Chinese: ["zh", "cn", "chinese", "中文", "國語", "普通話"], Japanese: ["ja", "jp", "japanese", "日文", "日語"],
  English: ["en", "english", "英文", "英語"], Korean: ["ko", "kr", "korean", "韓文", "韓語"], Cantonese: ["yue", "cantonese", "粵語", "廣東話"] };
const normLang = (v) => { const c = String(v || "").trim().toLowerCase(); return Object.keys(LANG_ALIAS).find((k) => LANG_ALIAS[k].includes(c)) || ""; };
const LAYOUT = [["top", "上方"], ["bottom", "下方"], ["left", "左邊"], ["right", "右邊"], ["fg", "前景"], ["mid", "中景"], ["bg", "遠景"], ["light", "光線"]];
const BRAINS = [["9b", "9B（快）"], ["35b", "35B（準）"], ["cloud", "雲端"]];
// /api/shotsheet/meta 回來前先用這組（跟 shotsheet.rs 的常數一樣）
const LIM = { tail: 1.6, handover: 0.7, early: 1, min_secs: 5, max_secs: 15, max_segs: 30, max_cast: 4, max_refs: 8 };

const emptyState = () => ({ actors: [], light: "", camera: "", notes: "" });
const stateHasContent = (s) => !!s && ((s.actors || []).some((a) => Object.values(a).some((v) => String(v || "").trim())) || [s.light, s.camera, s.notes].some((v) => String(v || "").trim()));
function fillState(v = {}) {
  v = v && typeof v === "object" ? v : {};
  const text = (x, n) => String(x || "").trim().slice(0, n);
  return { actors: (Array.isArray(v.actors) ? v.actors : []).slice(0, 8).map((a) => Object.fromEntries(["who", "position", "pose", "holding", "gaze", "wardrobe"].map((k) => [k, text(a?.[k], k === "who" ? 80 : 500)]))),
    light: text(v.light, 1000), camera: text(v.camera, 1000), notes: text(v.notes, 2000) };
}
const seg = (secs = 10) => ({ secs, beats: [], prompt: "", mirror: "", manual: false, ref_off: [], entry: emptyState(), exit: emptyState(), inherit_entry: true });
function blank() {
  return { v: 1, engine: "h3", title: "", lang: "zh", cast: [], first: { file: null, asset_id: null, gallery_id: null }, refs: { on: false, items: [] },
    scene: { text: "", layout: {} }, style: "", camera: "", sound: { ambient: "", music: "" }, early: true, segments: [seg()] };
}

let S = blank();          // 目前的拍攝單
let meta = null;          // /api/shotsheet/meta（鏡位小抄、危險字、時間常數）
let estimate = [];        // 開拍預估（每張卡一筆）
let issues = [];          // 最近一次健檢
let box = null;           // #shBody；init 之前（測試）不畫
let seq = 0;              // 自動重組的順序：舊的回應晚到就丟掉
let ask = issuesDialog;   // 健檢有問題時問使用者（測試換成假的）
let workflows = [];       // 可選引擎只存在拍攝單，不改全域影片設定
let applying = false;
const stateOpen = new WeakSet();
const castImgs = new Map(); // 角色資產的圖（/apply 回來的），「用角色的圖」當參考圖時用

/** 補齊欄位：草稿、範本、長片帶回來的都過一次 */
function fill(x) {
  const b = blank();
  x = x && typeof x === "object" ? x : {};
  const segs = Array.isArray(x.segments) && x.segments.length ? x.segments.slice(0, LIM.max_segs) : [{}];
  return {
    ...b, engine: ["ltx", "ltx23"].includes(x.engine) ? "h3" : x.engine || "h3", title: x.title || "", lang: x.lang === "en" ? "en" : "zh", style: x.style || "", camera: x.camera || "", early: x.early !== false,
    first: { ...b.first, ...(x.first || {}) },
    refs: { on: !!x.refs?.on, items: (x.refs?.items || []).slice(0, LIM.max_refs).map((r) => ({ file: r.file || null, asset_id: r.asset_id ?? null, who: r.who || "" })) },
    scene: { text: x.scene?.text || "", layout: { ...(x.scene?.layout || {}) } },
    sound: { ambient: x.sound?.ambient || "", music: x.sound?.music || "" },
    cast: (x.cast || []).slice(0, LIM.max_cast).map((c, i) => ({ asset_id: c.asset_id ?? null, speaker: `S${i + 1}`, name: c.name || "",
      look_zh: c.look_zh || "", look_en: c.look_en || "", voice: c.voice || "", lang: normLang(c.lang), locked: !!c.locked, refmod: c.refmod || "" })),
    refmod: !!x.refmod,
    segments: segs.map((s) => ({ ...seg(Number(s.secs) || 10), prompt: s.prompt || "", mirror: s.mirror || "", manual: !!s.manual,
      ref_off: (Array.isArray(s.ref_off) ? s.ref_off : []).filter((f) => typeof f === "string" && f),
      entry: fillState(s.entry), exit: fillState(s.exit), inherit_entry: s.inherit_entry !== false,
      beats: (s.beats || []).map((t) => ({ from: Number(t.from) || 0, to: Number(t.to) || 0, action: t.action || "", who: t.who || "", lang: normLang(t.lang), line: t.line || "", sfx: t.sfx || "" })) })),
  };
}

// ================================================================ 對外
export function current() { return S; }

export function requiresFirstFrame(engine = S.engine) {
  if (engine === "h3") return true;
  const flow = workflows.find((w) => w.id === engine);
  if (!flow?.info || !("image" in flow.info)) return true;
  return !!flow.info.image || (flow.media_slots || flow.info.media_slots || []).some((s) => s.kind === "image" && s.role === "first");
}

export function effectiveEntry(i) {
  const s = S.segments[i];
  return i > 0 && s.inherit_entry && !stateHasContent(s.entry) ? S.segments[i - 1].exit : s.entry;
}

export function setEngine(engine) {
  engine = engine || "h3";
  if (engine === S.engine) return true;
  if (S.segments.some((s) => s.manual && (s.prompt.trim() || s.mirror.trim()))
      && !confirm("切換引擎會依拍攝欄位重新組提示詞，覆蓋手改內容。繼續？")) { paint(); return false; }
  S.engine = engine;
  for (const s of S.segments) Object.assign(s, { prompt: "", mirror: "", manual: false });
  issues = []; estimate = [];
  save(); paint();
  return true;
}

/** 從範本或長片的 sheet 帶入 */
export function load(sheet) {
  ++seq;
  S = fill(sheet);
  issues = [];
  estimate = [];
  save(false);
  paint();
}

/** 送出前健檢：沒問題回 true；有問題開對話框，「照樣送」回 true、「回去改」回 false */
export async function precheck(prompts, sheet = S) {
  let r;
  try { r = await api("/api/shotsheet/check", { json: { sheet, prompts } }); } catch (e) { toast(e.message, "err"); return false; }
  issues = r.issues || [];
  paintIssues();
  if (!issues.length) return true;
  return ask(issues);
}

// ================================================================ 狀態
const autoCompose = debounce(() => composeNow(), 300);
function save(recompose = true) {
  store.set("shotsheet", S);
  if (recompose) { ++seq; autoCompose(); }
}

/** 組提示詞。ask＝使用者按「直接組」：有手改過的段先問要不要蓋掉；打字時的自動重組只更新沒手改過的段 */
export async function composeNow({ ask: askFirst = false } = {}) {
  const my = ++seq;
  let r;
  try { r = await api("/api/shotsheet/compose", { json: { sheet: S } }); } catch (e) { if (askFirst) toast(e.message, "err"); return false; }
  if (my !== seq) return false;
  estimate = r.estimate || [];
  let overwrite = false;
  if (askFirst) {
    const manual = S.segments.map((s, i) => (s.manual ? i + 1 : 0)).filter(Boolean);
    overwrite = !manual.length || confirm(`第 ${manual.join("、")} 段手改過，要用欄位重組蓋掉嗎？`);
  }
  (r.segments || []).forEach((p, i) => {
    const s = S.segments[i];
    if (!s) return;
    // 手改過的段：後端把鎖定角色的設定表接回去（kept），其他照使用者寫的
    if (s.manual && !overwrite) { if (p.kept != null) Object.assign(s, { prompt: p.kept, mirror: p.kept_mirror ?? s.mirror }); return; }
    Object.assign(s, { prompt: p.prompt, mirror: p.mirror, manual: false });
  });
  save(false);
  paintPrompts();
  paintEstimate();
  return true;
}

// 提示詞、對照手改過會標 manual；沒手改的是程式組的，隨時能重組，不算使用者寫的東西
const hasContent = (s) => s.manual || stateHasContent(s.entry) || stateHasContent(s.exit) || s.beats.some((b) => b.action.trim() || b.line.trim() || b.sfx.trim());
const hasDraft = () => S.title.trim() || S.cast.length || S.first.file || S.refs.items.length || S.scene.text.trim() || Object.values(S.scene.layout).some(Boolean) || S.camera.trim() || S.style.trim() || Object.values(S.sound).some(Boolean) || S.segments.some(hasContent);

/** 改段數。要拿掉的段寫了東西先問；1 段切到短片區、2 段以上切到長片區 */
export function setSegCount(n) {
  n = Math.max(1, Math.min(LIM.max_segs, Math.round(Number(n)) || 1));
  const cur = S.segments.length;
  if (n < cur) {
    const lost = S.segments.slice(n).map((s, k) => (hasContent(s) ? n + k + 1 : 0)).filter(Boolean);
    if (lost.length && !confirm(`第 ${lost.join("、")} 段已經寫了東西，確定拿掉？`)) { paint(); return false; }
    S.segments.splice(n);
  } else while (S.segments.length < n) S.segments.push(seg());
  if (S.engine === "h3" && (cur === 1) !== (n === 1)) studio.openVideo(n === 1 ? "short" : "long");
  save();
  paint();
  return true;
}

/** 從資產加一個角色（外觀、聲音、語言由 /apply 帶入），最多 4 個，依順序叫 S1～S4。
 *  鎖定設定表的角色：外觀用設定表原文（每段描述最前面原封不動放上），正面特寫＋轉身圖自動放進參考圖、綁這個角色（圖文雙管齊下） */
export async function addCast(it) {
  if (S.cast.length >= LIM.max_cast) { toast(`角色最多 ${LIM.max_cast} 個`, "err"); return false; }
  let p;
  try { p = await api(`/api/assets/${it.id}/apply`, { json: { family: "h3" } }); } catch (e) { toast(e.message, "err"); return false; }
  if (S.cast.length >= LIM.max_cast) { toast(`角色最多 ${LIM.max_cast} 個`, "err"); return false; }
  castImgs.set(it.id, p.images || []);
  const locked = !!p.locked;
  const c = { asset_id: it.id, speaker: `S${S.cast.length + 1}`, name: it.name || "",
    look_zh: locked ? p.spec?.zh || "" : p.text || it.data?.look_zh || "", look_en: locked ? p.spec?.en || "" : it.data?.look_en || "",
    voice: p.voice || "", lang: normLang(p.lang), locked, refmod: locked ? p.refmod || "" : "" };
  S.cast.push(c);
  if (locked) await lockedRefs(c, p.images || []);
  save();
  paint();
  return true;
}

/** 鎖定角色的固定參考圖（apply 給 H3 的是正面特寫＋轉身圖）放進參考圖、綁這個角色；範本留下的空格先補；放不下就提醒 */
async function lockedRefs(c, files) {
  const draft = S;
  let put = 0;
  for (const f of files) {
    if (S.refs.items.filter((x) => x.file).length >= LIM.max_refs) {
      toast(`參考圖最多 ${LIM.max_refs} 張：「${c.name}」還有 ${files.length - put} 張沒放進去`, "err");
      break;
    }
    let r;
    try { r = await api(`/api/assets/${c.asset_id}/to_input`, { json: { file: f } }); } catch (e) { toast(e.message, "err"); continue; }
    if (S !== draft || !S.cast.includes(c)) return;
    const item = { file: r.name, asset_id: c.asset_id, who: c.speaker };
    const empty = S.refs.items.find((x) => !x.file && x.asset_id === c.asset_id);
    if (empty) Object.assign(empty, item); else S.refs.items.push(item);
    put++;
  }
  if (put) S.refs.on = true;
}

/** 拿掉一個角色：後面的往前補號，時間軸和參考圖的「誰」跟著換，指到被拿掉的人就清空 */
export function removeCast(i) {
  const [gone] = S.cast.splice(i, 1);
  // 鎖定角色自動放的參考圖一起拿掉（同一個資產、綁同一個人的那幾張）
  if (gone?.locked) S.refs.items = S.refs.items.filter((r) => !(r.asset_id === gone.asset_id && r.who === gone.speaker));
  const map = {};
  S.cast.forEach((c, k) => { map[c.speaker] = `S${k + 1}`; c.speaker = `S${k + 1}`; });
  const re = (w) => (w ? map[w] || "" : "");
  for (const s of S.segments) {
    for (const b of s.beats) b.who = re(b.who);
    for (const state of [s.entry, s.exit]) for (const a of state.actors) if (/^S\d+$/.test(a.who)) a.who = re(a.who);
  }
  for (const r of S.refs.items) r.who = re(r.who);
  save();
  paint();
}

/** 第 i 段能安排新動作的範圍（跟 shotsheet.rs 的 region 一樣） */
function region(i) {
  const secs = S.segments[i].secs;
  if (S.engine !== "h3") return [0, secs];
  return [i > 0 ? LIM.tail : 0, i + 1 < S.segments.length ? Math.round((secs - LIM.handover) * 1000) / 1000 : secs];
}

// ================================================================ 對話框
function issueEl(x, cancel = () => closeModal("#shCheckModal")) {
  return el("li", { class: "sh-issue " + (x.level || "warn"), onclick: () => { cancel(); F(`shSeg${x.seg}`)?.scrollIntoView({ block: "center", behavior: "smooth" }); } },
    el("b", {}, `第 ${x.seg + 1} 段`), " ", el("code", { class: "sh-word" }, x.word || ""), " ", el("span", {}, x.msg || ""));
}

function issuesDialog(list) {
  return new Promise((res) => {
    const m = F("shCheckModal");
    F("shCheckList").replaceChildren(...list.map((x) => issueEl(x, () => done(false))));
    const done = (v) => { m.oncancel = null; m.onclick = null; closeModal("#shCheckModal"); res(v); };
    m.oncancel = () => done(false);
    F("shCheckSend").onclick = () => done(true);
    F("shCheckBack").onclick = () => done(false);
    m.onclick = (e) => { if (e.target === m) done(false); };
    openModal("#shCheckModal");
  });
}

/** 從資產庫挑一個（kinds 第一個是預設那類）；按關閉回 null */
function pickAsset(kinds, title) {
  return new Promise(async (res) => {
    const m = F("shAssetModal");
    let items = [];
    try { items = await assets.list(); } catch { /* list() 自己會報錯 */ }
    let kind = kinds[0];
    const grid = F("shAssetGrid");
    const done = (v) => { m.oncancel = null; m.onclick = null; closeModal("#shAssetModal"); res(v); };
    const draw = () => {
      F("shAssetKinds").replaceChildren(...(kinds.length > 1 ? kinds.map((k) => el("button", { class: "chip" + (k === kind ? " on" : ""), type: "button", onclick: () => { kind = k; draw(); } }, assets.KIND_NAME[k])) : []));
      const shown = items.filter((x) => x.kind === kind);
      grid.replaceChildren(...shown.map((it) => el("button", { class: "adr-card", type: "button", title: it.data?.look_zh || it.data?.look_en || "", onclick: () => done(it) },
        it.has_thumb ? el("img", { src: `/api/lib/${it.id}/thumb?v=${Math.round(it.updated || 0)}`, alt: "", loading: "lazy" }) : el("span", { class: "adr-ph" }, assets.KIND_NAME[it.kind]),
        el("span", { class: "adr-name" }, it.name))));
      if (!shown.length) grid.append(el("span", { class: "muted sm" }, "這一類還沒有資產：到左邊「03 資產庫」新增"));
    };
    m.oncancel = () => done(null);
    F("shAssetTitle").textContent = title;
    m.onclick = (e) => { if (e.target === m || e.target.closest("[data-close]")) done(null); };
    draw();
    openModal("#shAssetModal");
  });
}

// ================================================================ 畫面
const help = () => el("span", { class: "sh-help", "aria-hidden": "true" });
function sec(title, ...kids) { return el("section", { class: "sh-sec" }, el("div", { class: "sh-sec-hd" }, el("b", {}, title), help()), ...kids); }
/** 輸入框：v 是初始值，on(v) 收改過的值；改完存草稿＋自動重組 */
function field(tag, v, on, attrs = {}) {
  const e = el(tag, attrs);
  e.value = v ?? "";
  e.addEventListener(tag === "select" || attrs.type === "checkbox" ? "change" : "input", () => { on(attrs.type === "checkbox" ? e.checked : e.value); save(); });
  if (attrs.type === "checkbox") e.checked = !!v;
  return e;
}
function select(v, opts, on, attrs = {}) {
  const e = field("select", null, on, attrs);
  e.append(...opts.map(([k, t]) => el("option", { value: k }, t)));
  e.value = v ?? "";
  return e;
}
const btn = (text, onclick, cls = "akey sm", title = "") => el("button", { class: cls, type: "button", title: title || null, onclick }, text);
const thumbOf = (name) => (name ? el("img", { class: "sh-thumb", src: `/input/${encodeURIComponent(name)}`, alt: "" }) : el("span", { class: "sh-thumb ph" }, "還沒放"));

function paint() {
  if (!box) return;
  box.replaceChildren(
    paintHeadEl(),
    el("div", { class: "sh-grid" },
      el("div", { class: "sh-col" }, castEl(), firstEl(), refsEl(), sceneEl(), cameraEl(), soundEl()),
      el("div", { class: "sh-col" }, timelineEl(), promptsEl())));
  paintEstimate();
  paintIssues();
}

function paintHeadEl() {
  const n = S.segments.length;
  const count = el("input", { type: "number", min: 1, max: LIM.max_segs, step: 1, class: "sh-num", "aria-label": "段數" });
  count.value = n;
  count.onchange = () => setSegCount(count.value);
  const tpl = el("select", { class: "sm", "aria-label": "從範本開始", onchange: () => useTemplate(tpl) }, el("option", { value: "" }, "從範本開始…"));
  fillTemplates(tpl);
  const engines = [["h3", "MiniMax H3"], ...workflows.filter((w) => w.id !== "h3").map((w) => [w.id, `自訂：${w.name || w.id}`])];
  if (!engines.some(([id]) => id === S.engine)) engines.push([S.engine, `自訂：${S.engine}（目前找不到）`]);
  return el("div", { class: "row wrap sh-head" },
    el("label", { class: "row" }, el("span", {}, "影片引擎"), select(S.engine, engines, setEngine, { "aria-label": "拍攝單影片引擎" })),
    el("label", { class: "row" }, el("span", {}, "段數"), count, help()),
    el("span", { class: "piano row", role: "group", "aria-label": "提示詞語言" },
      ...[["zh", "中文"], ["en", "English"]].map(([k, t]) => el("button", { type: "button", class: S.lang === k ? "on" : "", onclick: () => { S.lang = k; save(); paint(); } }, t))),
    tpl, btn("存成範本", saveTemplate),
    el("span", { class: "grow" }),
    el("span", { class: "sh-est", id: "shEst", "aria-live": "polite" }));
}

async function fillTemplates(sel) {
  let items = [];
  try { items = await assets.list(); } catch { /* list() 自己會報錯 */ }
  sel.append(...items.filter((x) => x.kind === "shot").map((x) => el("option", { value: String(x.id) }, x.name)), el("option", { value: "wall" }, "玩法牆…"));
}

async function useTemplate(sel) {
  const v = sel.value;
  sel.value = "";
  if (v === "wall") { studio.openVideo("short"); return F("h3Tpl")?.scrollIntoView({ block: "center", behavior: "smooth" }); }
  const it = (await assets.list()).find((x) => String(x.id) === v);
  if (!it) return;
  if (hasDraft() && !confirm(`用範本「${it.name}」蓋掉目前的拍攝單？`)) return;
  load(it.data);
  api(`/api/lib/used`, { json: { id: it.id } }).catch(() => {});
  if (S.engine === "h3" && S.segments.length > 1) studio.openVideo("long");
  composeNow();
}

async function saveTemplate() {
  const name = (prompt("範本名稱", S.title || "") || "").trim();
  if (!name) return;
  try {
    await api("/api/shotsheet/template", { json: { name, sheet: S } });
    toast(`範本「${name}」存進資產庫了（不含這次的圖和寫好的提示詞）`, "ok");
    await assets.load();
    paint();
  } catch (e) { toast(e.message, "err"); }
}

function paintEstimate() {
  const e = F("shEst");
  if (!e) return;
  e.replaceChildren(...estimate.map((x) => el("span", { class: "sh-est-row" },
    el("b", { translate: "no" }, x.gpu), el("span", {}, "約"), el("span", { class: "num" }, String(Math.max(1, Math.ceil(x.secs / 60)))),
    el("span", {}, "分鐘"), el("span", { class: "num" }, Number(x.usd).toFixed(2)), el("span", {}, "美元"))));
}

// ---------------------------------------------------------------- 左欄
function castEl() {
  const rows = S.cast.map((c, i) => el("div", { class: "sh-cast" },
    el("b", { class: "sh-sp", translate: "no" }, c.speaker),
    field("input", c.name, (v) => (c.name = v), { placeholder: "名字", "aria-label": "名字", class: "sh-name" }),
    ...(c.locked
      ? [c.look_zh, c.look_en].map((t) => el("div", { class: "sh-locked sm", title: "資產庫鎖定的設定表：每段描述最前面原封不動放上，AI 潤飾也不會動；要改請到資產庫解鎖" }, "🔒 ", t || "—"))
      : [field("textarea", c.look_zh, (v) => (c.look_zh = v), { rows: 2, placeholder: "外觀（中文）", "aria-label": "外觀（中文）" }),
        field("textarea", c.look_en, (v) => (c.look_en = v), { rows: 2, placeholder: "Look (English)", "aria-label": "外觀（英文）" })]),
    field("input", c.voice, (v) => (c.voice = v), { placeholder: "聲音（例：溫柔的成年女聲）", "aria-label": "聲音", class: "sh-voice" }),
    select(c.lang, LANGS.slice(1).concat([["", "（沒寫＝中文）"]]), (v) => (c.lang = v), { "aria-label": "說話語言", class: "sh-lang" }),
    btn("×", () => removeCast(i), "h3-x", "拿掉這個角色")));
  return sec("角色", ...rows,
    S.cast.length < LIM.max_cast ? btn("＋ 從資產", async () => { const it = await pickAsset(["char"], "選一個角色"); if (it) addCast(it); }) : el("span", { class: "muted sm" }, `角色最多 ${LIM.max_cast} 個`));
}

async function setFirst(get) {
  try {
    const r = await get();
    if (!r) return;
    S.first = { file: r.name, asset_id: r.asset_id ?? null, gallery_id: r.gallery_id ?? null };
    save();
    paint();
  } catch (e) { toast(e.message, "err"); }
}
const assetImage = async (it) => {
  const p = await api(`/api/assets/${it.id}/apply`, { json: { family: "h3" } });
  if (!p.images?.length) throw new Error(`「${it.name}」沒有圖`);
  const r = await api(`/api/assets/${it.id}/to_input`, { json: { file: p.images[0] } });
  return { ...r, asset_id: it.id };
};
const uploaded = async () => { const f = await pickFile(); return f ? uploadFile(f) : null; };

function firstEl() {
  const f = S.first.file;
  return sec("首幀",
    el("div", { class: "row wrap sh-first" }, thumbOf(f),
      el("div", { class: "row wrap" },
        btn("從資產", () => setFirst(async () => { const it = await pickAsset(["frame", "char", "scene"], "選一張首幀"); return it ? assetImage(it) : null; })),
        btn("圖庫", () => pickFromGallery((rec) => setFirst(async () => ({ ...(await api("/api/to_input", { json: { id: rec.id } })), gallery_id: rec.id })), { title: "從圖庫選一張首幀" }).catch((e) => toast(e.message, "err"))),
        btn("上傳", () => setFirst(uploaded)),
        btn("生圖並排挑", () => setFirst(async () => (await import("./framepick.js")).pick({ count: store.get("framepickCount", 4) })), "akey sm", "用生成頁目前的設定一次畫好幾張，挑一張當首幀"),
        select(String(store.get("framepickCount", 4)), [2, 3, 4, 5, 6, 7, 8].map((n) => [String(n), `${n} 張`]), (v) => store.set("framepickCount", Number(v)), { "aria-label": "並排挑要畫幾張", class: "sh-fpn" }),
        f ? btn("拿掉", () => { S.first = { file: null, asset_id: null, gallery_id: null }; save(); paint(); }) : null)),
    el("p", { class: "muted sm" }, "第 1 段從這張開始動；接下來每一段接上一段的最後一刻。"));
}

async function addRef(get, who = "") {
  if (S.refs.items.filter((x) => x.file).length >= LIM.max_refs) return toast(`參考圖最多 ${LIM.max_refs} 張`, "err");
  try {
    const draft = S;
    const r = await get();
    if (!r || S !== draft) return;
    if (S.refs.items.filter((x) => x.file).length >= LIM.max_refs) return toast(`參考圖最多 ${LIM.max_refs} 張`, "err");
    const empty = S.refs.items.find((x) => !x.file && r.asset_id && x.asset_id === r.asset_id) || S.refs.items.find((x) => !x.file);
    const item = { file: r.name, asset_id: r.asset_id ?? empty?.asset_id ?? null, who: who || empty?.who || "" };
    if (empty) Object.assign(empty, item); else S.refs.items.push(item);
    save();
    paint();
  } catch (e) { toast(e.message, "err"); }
}

function refsEl() {
  const on = S.refs.on;
  const who = [["", "（不指定）"], ...S.cast.map((c) => [c.speaker, `${c.speaker} ${c.name}`])];
  const items = S.refs.items.map((r, i) => el("div", { class: "row sh-ref" }, thumbOf(r.file),
    select(r.who, who, (v) => (r.who = v), { "aria-label": "這張是誰" }),
    r.file ? null : el("span", { class: "muted sm" }, "範本帶來的，要重新放圖"),
    btn("×", () => { S.refs.items.splice(i, 1); save(); paint(); }, "h3-x", "拿掉這張")));
  const fromCast = () => {
    for (const c of S.cast) if (c.asset_id) addRef(() => assetImage({ id: c.asset_id, name: c.name }), c.speaker);
  };
  const locked = S.cast.filter((c) => c.locked);
  const noMod = locked.filter((c) => !c.refmod).map((c) => c.name || c.speaker);
  return sec("參考圖",
    el("label", { class: "check inline" }, field("input", on, (v) => { S.refs.on = v; paint(); }, { type: "checkbox" }), " 送參考圖（長相）"),
    S.engine === "h3" && locked.length ? el("label", { class: "check inline", title: "鎖定角色在資產庫做過 RefMod 的，H3 拍片時把它接在條件上（實驗）；雲端開機時會裝 RefMod 外掛" },
      field("input", S.refmod, (v) => { S.refmod = v; paint(); }, { type: "checkbox" }), " 用 RefMod 鎖臉（實驗）") : null,
    S.engine === "h3" && S.refmod && noMod.length ? el("p", { class: "sh-warn sm" }, `${noMod.join("、")} 還沒做 RefMod：到資產庫的角色卡按「做 RefMod」，再把角色重新加一次`) : null,
    on ? el("p", { class: "sh-warn sm" }, "參考圖只拿來看長相；仰臉的圖會讓接續段一開頭就仰臉。") : null,
    on ? el("div", {}, ...items,
      el("div", { class: "row wrap" }, S.cast.some((c) => c.asset_id) ? btn("＋ 用角色的圖", fromCast) : null, btn("＋ 上傳", () => addRef(uploaded)))) : null);
}

function sceneEl() {
  const lay = S.scene.layout;
  const ta = field("textarea", S.scene.text, (v) => (S.scene.text = v), { rows: 2, placeholder: "例：夜晚的小臥室，暖黃檯燈", "aria-label": "場景" });
  const fromAsset = async () => {
    const it = await pickAsset(["scene"], "選一個場景");
    if (!it) return;
    S.scene = { text: (S.lang === "en" ? it.data?.look_en || it.data?.look_zh : it.data?.look_zh || it.data?.look_en) || "", layout: { ...(it.data?.layout || {}) } };
    save();
    paint();
  };
  return sec("場景", ta, btn("從資產", fromAsset),
    el("details", { class: "sh-layout", open: LAYOUT.some(([k]) => lay[k]) || null },
      el("summary", { class: "sm" }, "物件位置（選填）"),
      el("div", { class: "sh-lay-grid" }, ...LAYOUT.map(([k, t]) => el("label", { class: "row sm" }, el("span", {}, t), field("input", lay[k], (v) => (lay[k] = v), { "aria-label": t }))))));
}

function cameraEl() {
  const ta = field("textarea", S.camera, (v) => (S.camera = v), { rows: 2, placeholder: "例：側面中景，鏡頭慢慢推近", "aria-label": "鏡位與運鏡" });
  const put = (zh, en) => {
    const t = S.lang === "en" ? en : zh;
    const before = ta.value.slice(0, ta.selectionStart).replace(/\s+$/, "");
    const sep = before && !/[，。,.;；]$/.test(before) ? (S.lang === "en" ? ", " : "，") : "";
    ta.setRangeText(sep + t, ta.selectionStart, ta.selectionEnd, "end");
    ta.focus();
    ta.dispatchEvent(new Event("input"));
  };
  const chips = (list, label) => el("div", { class: "row wrap sh-cheat", role: "group", "aria-label": label },
    el("span", { class: "muted sm" }, label), ...(list || []).map(([b, zh, en]) => el("button", { class: "chip", type: "button", title: S.lang === "en" ? en : zh, onclick: () => put(zh, en) }, b)));
  return sec("鏡位與運鏡", ta, chips(meta?.cheat?.view, "視角"), chips(meta?.cheat?.move, "運鏡"),
    field("input", S.style, (v) => (S.style = v), { placeholder: "畫風（例：日系動畫、柔和光影）", "aria-label": "畫風" }));
}

function soundEl() {
  return sec("聲音",
    field("input", S.sound.ambient, (v) => (S.sound.ambient = v), { placeholder: "環境音（空著＝符合場景的自然環境音）", "aria-label": "環境音" }),
    field("input", S.sound.music, (v) => (S.sound.music = v), { placeholder: "配樂（空著＝不要配樂）", "aria-label": "配樂" }));
}

// ---------------------------------------------------------------- 右欄：時間軸
function scaleEl(i) {
  const s = S.segments[i];
  const pct = (t) => `${Math.max(0, Math.min(100, (t / s.secs) * 100))}%`;
  const [lo, hi] = region(i);
  return el("div", { class: "sh-scale", "aria-hidden": "true" },
    lo > 0 ? el("span", { class: "sh-tail", style: `width:${pct(lo)}`, title: "上一段的尾巴：接片時切掉，不要放新動作" }, "上一段尾巴") : null,
    hi < s.secs ? el("span", { class: "sh-hand", style: `left:${pct(hi)};width:${pct(s.secs - hi)}`, title: "交棒裁切區：留給下一段接手，不要放新動作" }, "🔒 交棒裁切區") : null,
    ...s.beats.map((b, k) => el("span", { class: "sh-beat" + (b.to <= b.from || b.from < lo || b.to > hi ? " bad" : ""), style: `left:${pct(b.from)};width:${pct(Math.max(0, b.to - b.from))}`, title: b.action }, String(k + 1))));
}

function beatEl(i, k) {
  const s = S.segments[i];
  const b = s.beats[k];
  const who = [["", "—"], ...S.cast.map((c) => [c.speaker, `${c.speaker} ${c.name}`])];
  const num = (key, label) => field("input", b[key], (v) => { b[key] = Number(v) || 0; F(`shScale${i}`)?.replaceChildren(...scaleEl(i).childNodes); }, { type: "number", min: 0, max: s.secs, step: 0.1, class: "sh-num", "aria-label": label });
  return el("div", { class: "sh-beat-row" },
    el("span", { class: "sh-bn" }, String(k + 1)), num("from", "從（秒）"), el("span", {}, "→"), num("to", "到（秒）"),
    field("input", b.action, (v) => (b.action = v), { class: "sh-act", placeholder: "動作：哪個部位、往哪裡（例：右手把杯子舉到胸前）", "aria-label": "動作" }),
    select(b.who, who, (v) => (b.who = v), { "aria-label": "誰" }),
    select(b.lang, LANGS, (v) => (b.lang = v), { "aria-label": "台詞語言" }),
    field("input", b.line, (v) => (b.line = v), { class: "sh-line", placeholder: "台詞", "aria-label": "台詞" }),
    field("input", b.sfx, (v) => (b.sfx = v), { class: "sh-sfx", placeholder: "音效", "aria-label": "音效" }),
    btn("×", () => { s.beats.splice(k, 1); save(); paint(); }, "h3-x", "拿掉這一截"));
}

function addBeat(i) {
  const s = S.segments[i];
  const [lo, hi] = region(i);
  const from = Math.min(hi, s.beats.length ? Math.max(...s.beats.map((b) => b.to)) : lo);
  s.beats.push({ from, to: Math.min(hi, Math.round((from + 3) * 10) / 10), action: "", who: S.cast.length === 1 ? "S1" : "", lang: "", line: "", sfx: "" });
  save();
  paint();
}

function continuityStateEl(i, key, label) {
  const s = S.segments[i], value = s[key];
  const inherited = key === "entry" && i > 0 && s.inherit_entry && !stateHasContent(value) && stateHasContent(S.segments[i - 1].exit);
  const labels = [["who", "角色（S1 或名字）"], ["position", "站位"], ["pose", "姿態"], ["holding", "持物與左右手"], ["gaze", "視線"], ["wardrobe", "服裝"]];
  const rows = value.actors.map((actor, k) => el("div", { class: "sh-layout", style: "margin-top:8px" },
    el("div", { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(120px,1fr));gap:8px" },
      ...labels.map(([name, title]) => el("label", { class: "sm" }, el("span", {}, title),
        field("input", actor[name], (v) => (actor[name] = v), { "aria-label": `${label} ${k + 1} ${title}`, placeholder: title })))),
    btn("拿掉角色狀態", () => { value.actors.splice(k, 1); save(); paint(); })));
  const inheritedText = inherited ? [
    ...S.segments[i - 1].exit.actors.map((a) => labels.filter(([k]) => a[k]).map(([k, t]) => `${t}：${a[k]}`).join("；")),
    ...[["light", "光向"], ["camera", "鏡頭軸線"], ["notes", "備註"]].filter(([k]) => S.segments[i - 1].exit[k]).map(([k, t]) => `${t}：${S.segments[i - 1].exit[k]}`),
  ].join("\n") : "";
  return el("details", { class: "sh-layout", open: stateOpen.has(value), ontoggle: (event) => {
    if (event.currentTarget.open) stateOpen.add(value); else stateOpen.delete(value);
  } },
    el("summary", { class: "sm" }, label + (inherited ? "（沿用上鏡結束）" : stateHasContent(value) ? "（已填）" : "（選填）")),
    key === "entry" && i > 0 ? el("label", { class: "check inline" },
      field("input", s.inherit_entry, (v) => { s.inherit_entry = v; paint(); }, { type: "checkbox", "aria-label": `第 ${i + 1} 段沿用上鏡結束` }), " 開始狀態空白時，沿用上鏡結束") : null,
    inherited ? el("pre", { class: "muted sm", style: "white-space:pre-wrap" }, inheritedText) : null,
    key === "entry" && i > 0 ? el("p", { class: "muted sm" }, "手填開始狀態會優先使用；留白才沿用。沒有填寫的細節不會自動補造。") : null,
    ...rows,
    value.actors.length < 8 ? btn("＋ 角色狀態", () => { stateOpen.add(value); value.actors.push({ who: "", position: "", pose: "", holding: "", gaze: "", wardrobe: "" }); save(); paint(); }) : null,
    ...[["light", "光向"], ["camera", "鏡頭軸線"], ["notes", "備註"]].map(([name, title]) => el("label", { class: "row sm", style: "margin-top:6px" },
      el("span", {}, title), field("input", value[name], (v) => (value[name] = v), { "aria-label": `${label} ${title}`, placeholder: title }))));
}

/** H3 每段送哪幾張參考圖：點圖切換；關掉的這段不送，提示詞的圖號照剩下的重排 */
function segRefsEl(i) {
  const files = S.refs.items.filter((r) => r.file);
  if (S.engine !== "h3" || !S.refs.on || !files.length) return null;
  const s = S.segments[i];
  const off = new Set(s.ref_off);
  const used = files.filter((r) => !off.has(r.file)).length;
  return el("div", { class: "row wrap sh-segrefs" }, el("span", { class: "muted sm", title: "某個角色的圖這段全部關掉＝這段沒有這個角色：圖、外觀描述、鎖定設定都不送" }, `這段送的參考圖 ${used}／${files.length}`),
    ...files.map((r) => {
      const c = S.cast.find((x) => x.speaker === r.who);
      const name = c ? `${r.who} ${c.name}` : r.who || "參考圖";
      const on = !off.has(r.file);
      return el("button", { type: "button", class: "sh-segref" + (on ? "" : " off"), "aria-pressed": String(on),
        title: `${name}：${on ? "這段會送，點一下改成不送" : "這段不送，點一下改成送"}`,
        onclick: () => { s.ref_off = on ? [...s.ref_off, r.file] : s.ref_off.filter((f) => f !== r.file); save(); paint(); } }, thumbOf(r.file));
    }));
}

/** 第 i 段實際送的參考圖檔名 */
function segRefFiles(i) {
  if (!S.refs.on) return [];
  const off = S.segments[i]?.ref_off || [];
  return S.refs.items.filter((r) => r.file && !off.includes(r.file)).map((r) => r.file);
}

function timelineEl() {
  const n = S.segments.length;
  const cards = S.segments.map((s, i) => el("div", { class: "sh-seg", id: `shSeg${i}` },
    el("div", { class: "row sh-seg-hd" }, el("b", {}, `第 ${i + 1} 段`),
      field("input", s.secs, (v) => { s.secs = Math.max(LIM.min_secs, Math.min(LIM.max_secs, Number(v) || 10)); }, { type: "number", min: LIM.min_secs, max: LIM.max_secs, step: 1, class: "sh-num", "aria-label": "秒數", onchange: () => paint() }),
      el("span", { class: "muted sm" }, "秒"), el("span", { class: "grow" }), s.manual ? el("span", { class: "sh-manual sm" }, "提示詞手改過") : null),
    segRefsEl(i),
    el("div", { class: "sh-scale-wrap", id: `shScale${i}` }, ...scaleEl(i).childNodes),
    continuityStateEl(i, "entry", "開始狀態"), continuityStateEl(i, "exit", "結束狀態"),
    ...s.beats.map((_, k) => beatEl(i, k)),
    btn("＋ 一截", () => addBeat(i))));
  return sec("時間軸",
    S.engine === "h3" ? el("label", { class: "check inline" }, field("input", S.early, (v) => (S.early = v), { type: "checkbox" }), ` 自動提早 ${LIM.early} 秒（H3 的動作常常比寫的晚出來）`) : null,
    S.engine === "h3" && n > 1 ? el("p", { class: "muted sm" }, `第 2 段起，開頭 ${LIM.tail} 秒是上一段的尾巴；後面還有段的話，最後 ${LIM.handover} 秒留給下一段接手。這兩塊不要放新動作。`) : null,
    ...cards);
}

// ---------------------------------------------------------------- 右欄：提示詞
async function polish(i, brain) {
  const s = S.segments[i];
  const draft = S, version = seq, before = JSON.stringify(s);
  const b = F(`shPol${i}`);
  if (b) b.disabled = true;
  try {
    const r = await api("/api/shotsheet/polish", { json: { sheet: S, seg: i, prompt: s.prompt, brain }, timeout: 10 * 60 * 1000 });
    if (S !== draft || seq !== version || JSON.stringify(s) !== before) return;
    Object.assign(s, { prompt: r.prompt, mirror: r.mirror, manual: true });
    save(false);
    paint();
  } catch (e) { toast(e.message, "err"); }
  finally { if (b) b.disabled = false; }
}

async function runCheck() {
  let r;
  try { r = await api("/api/shotsheet/check", { json: { sheet: S, prompts: S.segments.map((s) => s.prompt) } }); } catch (e) { return toast(e.message, "err"); }
  issues = r.issues || [];
  paintIssues();
  if (!issues.length) toast("健檢沒抓到問題", "ok");
}

function promptsEl() {
  const brain = select(store.get("shotsheetBrain", "9b"), BRAINS, (v) => store.set("shotsheetBrain", v), { "aria-label": "AI 潤飾用哪顆腦袋" });
  const rows = S.segments.map((s, i) => el("div", { class: "sh-prompt" },
    el("div", { class: "row" }, el("b", {}, `第 ${i + 1} 段`), el("span", { class: "grow" }),
      el("button", { class: "akey sm", type: "button", id: `shPol${i}`, onclick: () => polish(i, brain.value) }, "AI 潤飾這段")),
    field("textarea", s.prompt, (v) => { s.prompt = v; s.manual = true; }, { rows: 6, id: `shP${i}`, class: "sh-ta", "aria-label": `第 ${i + 1} 段提示詞`, spellcheck: "false" }),
    el("details", {}, el("summary", { class: "sm" }, S.lang === "en" ? "中文對照" : "英文對照"),
      field("textarea", s.mirror, (v) => { s.mirror = v; s.manual = true; }, { rows: 5, id: `shM${i}`, class: "sh-ta", "aria-label": `第 ${i + 1} 段對照`, spellcheck: "false" }))));
  const n = S.segments.length;
  return sec("提示詞",
    el("div", { class: "row wrap" }, btn("直接組", () => composeNow({ ask: true }), "akey sm", "照上面的欄位重新組每一段"), el("span", { class: "muted sm" }, "AI 潤飾用"), brain, btn("健檢", runCheck)),
    ...rows,
    el("ol", { class: "sh-issues", id: "shIssues" }),
    el("div", { class: "row" }, el("span", { class: "grow" }),
      btn(S.engine !== "h3" ? "套用到導演台" : n === 1 ? "套用到短片" : "套用到長片", () => applyTo(), "akey",
        S.engine !== "h3" ? "建立導演專案，保留這份拍攝單、引擎與連戲狀態" : n === 1 ? "把首幀、參考圖、提示詞放進下面的 H3 短片" : "把每段的提示詞、首幀、參考圖寫進下面的長片")));
}

/** 自動重組只換掉「沒手改過、而且沒在打字」的那幾格，不整個重畫（游標才不會跳走） */
function paintPrompts() {
  if (!box) return;
  S.segments.forEach((s, i) => {
    for (const [id, v] of [[`shP${i}`, s.prompt], [`shM${i}`, s.mirror]]) {
      const ta = F(id);
      if (ta && document.activeElement !== ta) ta.value = v;
    }
  });
}

function paintIssues() {
  const o = F("shIssues");
  if (o) o.replaceChildren(...issues.map((x) => issueEl(x)));
}

/** 套用到下面的短片（1 段）或長片（2 段以上）。還沒組過的段先組 */
export async function applyTo() {
  if (applying) return;
  applying = true;
  try {
  if (!(await composeNow())) return;
  const draft = S, version = seq;
  const prompts = S.segments.map((s) => s.prompt);
  if (prompts.some((p) => !p.trim())) return toast("有段落還沒有提示詞：先按「直接組」", "err");
  const first = S.first.file || null;
  const refs = S.refs.on ? S.refs.items.filter((r) => r.file).map((r) => r.file) : [];
  if (S.engine !== "h3") {
    if (!(await precheck(prompts)) || S !== draft || seq !== version) return;
    const needFirst = requiresFirstFrame();
    const gen = needFirst ? await import("./gen.js") : null;
    const image = gen?.baseParams() || {};
    const imageOK = image.model && image.family !== "custom";
    if (needFirst && !first && !imageOK) return toast("請先放首幀，或在生成頁選生第一格用的模型", "err");
    const body = { sheet: structuredClone(S), prompts, engine: S.engine };
    if (imageOK) body.image = image;
    const r = await api("/api/director/from_sheet", { json: body });
    const director = await import("./director.js");
    studio.openTab("plan");
    await director.openProject(r.id);
    toast("拍攝單已套用到導演台", "ok");
  } else if (S.segments.length === 1) {
    studio.openVideo("short");
    await h3.applySheet({ sheet: structuredClone(S), prompt: prompts[0], secs: S.segments[0].secs, first, refs: segRefFiles(0) });
    toast("套用到短片了：按送出前會先健檢", "ok");
  } else if (await h3chain.applySheet({ sheet: S, prompts, first, refs, segRefs: S.segments.map((_, i) => (segRefFiles(i).length === refs.length ? null : segRefFiles(i))) })) {
    studio.openVideo("long");
    toast(`套用到長片了：${S.segments.length} 段`, "ok");
  }
  } catch (e) { toast(e.message, "err"); }
  finally { applying = false; }
}

/** 長片打開時帶回它存的拍攝單：一樣的就不動；目前草稿有寫東西先問 */
export function offerLoad(sheet) {
  const canon = (x) => JSON.stringify(fill(x));
  if (canon(sheet) === canon(S)) return;
  const busy = hasDraft();
  if (busy && !confirm("這條長片有存拍攝單，要帶回上面嗎？（目前的拍攝單草稿會被換掉）")) return;
  load(sheet);
}

// ================================================================ 啟動
export function init() {
  box = F("shBody");
  S = fill(store.get("shotsheet", null));
  document.addEventListener("lib-updated", () => { if (box && !box.closest("[hidden]")) paint(); });
  paint();
}

export async function show() {
  if (!meta) {
    try {
      meta = await api("/api/shotsheet/meta");
      Object.assign(LIM, meta.limits || {});
    } catch (e) { toast(e.message, "err"); }
  }
  try { const r = await api("/api/video/workflows"); workflows = r.workflows || []; } catch (e) { toast(e.message, "err"); }
  paint();
  if (!estimate.length) composeNow();
}

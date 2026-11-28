// 角色設定圖工坊（design\角色設定圖工坊_設計.md；2026-10-27 改成角色卡 docs\superpowers\specs\2026-10-27-character-asset-design.md）：
// ① 定稿（一張全身正面站好的角色圖：放圖，或寫文字用生成頁的模型畫 4 張挑一張）
// ② Qwen 2.1 照定稿展開「全身轉身圖」「臉部特寫表」（各 2 張挑一張）
// ③ 正面特寫（鎖臉的固定參考圖）④ 選填：轉身圖走四視圖編輯變裸體版
// ⑤ 角色設定表（AI 看圖填 → 改 → 鎖定；鎖定的固定文字由後端 charspec.rs 組）⑥ 外觀描述＋反推 tag，存成資產庫的角色（新的或加到現有的）
// 生成頁四視圖、資產庫角色卡、導演台角色設定卡、圖片動作「做設定圖」都開這一個視窗。資產庫的角色開進來會帶入已有的圖和設定表，缺什麼補什麼。
import { api, el, toast, openModal, pickFile, uploadFile, store } from "./util.js";
import { pickFromGallery } from "./gallery.js";
import { app } from "./state.js";
import * as gen from "./gen.js";
import { SHEET_PRESETS } from "./gen.js";

const F = (id) => document.getElementById(id);
const KEY = "aw_charsheet";
const STEPS = ["base", "turnaround", "face", "portrait", "nude"];
/** 工坊的步驟 → 資產圖片用途 */
const STEP_ROLE = [["turnaround", "sheet"], ["face", "face"], ["portrait", "portrait"], ["nude", "sheet_nude"]];
// 男性裸體版（沒實測過，畫面上有註明）：照女性那句的寫法，拿掉胸部、陰部描述
const NUDE_M = "把图1中角色身上的衣服全部脱掉，变成全裸，连鞋子也脱掉、光着脚。他是成年男性，身材依原本衣服的轮廓延续；"
  + "原本被衣服盖住的地方画成皮肤，肤色和光影跟露出的皮肤一致；胸膛和下体依各视角自然露出，背面视角只露出背部和臀部。";
/** 設定表欄位（跟 src-tauri\src\charspec.rs 的 FIELDS 同順序）：key、中文名、中文範例、英文範例 */
export const SPEC_FIELDS = [
  ["sex", "性別", "女性", "female"], ["age", "年齡", "外觀二十出頭", "looks in her early 20s"],
  ["hair_style", "髮型", "及腰長直髮", "long straight hair reaching the waist"], ["hair_color", "髮色", "柑橘色頭髮", "orange hair"],
  ["face", "臉型", "鵝蛋臉", "oval face"], ["eyes", "眼睛", "橘色杏眼", "orange almond-shaped eyes"],
  ["skin", "膚色", "白皙膚色", "fair skin"], ["body", "體型", "纖細體型", "slender build"],
  ["marks", "專屬標誌", "右眼下方一顆美人痣", "a beauty mark under the right eye"], ["outfit", "預設服裝", "橘白短禮服", "an orange and white short dress"],
];

// ---------------------------------------------------------------- 純函式（tests/charsheet_ui.test.cjs）
/** 挑改圖模型：engine 為 "qwen21"｜"mmimage"｜"auto"（auto 時生成頁選的就是兩者之一就用它，否則先 Qwen 2.1、沒有再 MiniMax H3 Image） */
export function editModel(models, base, engine = "auto") {
  const fams = models?.unet_family || {};
  const listOf = (f) => Object.entries(fams).filter(([, x]) => x === f).map(([n]) => n);
  const pickQwen = () => {
    if (base?.family === "qwen21" && base.model) return base.model;
    const ms = listOf("qwen21");
    return ms.find((n) => /qwen[_-]image[_-]2\.1[_-]turbo/i.test(n)) || ms.find((n) => /turbo/i.test(n)) || ms[0] || null;
  };
  const pickMm = () => {
    if (base?.family === "mmimage" && base.model) return base.model;
    const ms = listOf("mmimage");
    return ms.find((n) => /w4a8/i.test(n)) || ms[0] || null;
  };
  if (engine === "mmimage") { const m = pickMm(); return m ? { model: m, family: "mmimage" } : null; }
  if (engine === "qwen21") { const m = pickQwen(); return m ? { model: m, family: "qwen21" } : null; }
  if (base?.family === "mmimage" && base.model) return { model: base.model, family: "mmimage" };
  const q = pickQwen();
  if (q) return { model: q, family: "qwen21" };
  const mm = pickMm();
  return mm ? { model: mm, family: "mmimage" } : null;
}

/** 向下相容（tests/charsheet_ui.test.cjs） */
export function qwenModel(models, base) {
  return editModel(models, base, "qwen21")?.model || null;
}

/** 改圖（Qwen 2.1 或 MiniMax H3 Image）多圖參考的參數 */
export function qwenParams(base, model, prompt, refInput, w, h, engine = "qwen21") {
  const isMm = engine === "mmimage" || base?.family === "mmimage" && base.model === model;
  const same = (isMm ? base.family === "mmimage" : base.family === "qwen21") && base.model === model;
  return {
    mode: "multiref", model, prompt, negative: "", seed: null, turbo_lora: false,
    loras: same ? base.loras || [] : [],
    steps: same ? base.steps : (isMm ? 6 : 8), cfg: same ? base.cfg : 1,
    sampler: same ? base.sampler : "euler", scheduler: same ? base.scheduler : "simple",
    refs: [{ image: refInput }], size_mode: "fixed", width: w, height: h, pixels: 1024 * 1024, ref_pixels: 1024 * 1024,
    ...(isMm ? { mm_pe: "" } : {}),
  };
}

/** 裸體版的修改要求（四視圖編輯的指令，簡體跟原工作流一致） */
export function nudeReq(sex) {
  return sex === "m" ? NUDE_M : SHEET_PRESETS.nude;
}

/** 存進資產的圖：[{role, gallery_id | input}]；定稿、已經在資產裡的圖（from_asset）不再存一次 */
export function rolesToSave(st) {
  const out = [];
  const s = st.start;
  if (s && !s.from_asset) out.push(s.gallery_id ? { role: "src", gallery_id: s.gallery_id } : { role: "src", input: s.input });
  for (const [k, role] of STEP_ROLE) {
    const p = st.pick?.[k];
    if (p?.gallery_id && !p.from_asset) out.push({ role, gallery_id: p.gallery_id });
  }
  return out;
}

/** 空白設定表 */
export function emptySpec() {
  return { locked: false, fields: Object.fromEntries(SPEC_FIELDS.map(([k]) => [k, { zh: "", en: "", tags: [] }])) };
}

/** 資產裡的設定表（可能沒有、可能缺欄位）→ 完整形狀；鎖定時帶著固定文字（只顯示，存檔時後端照舊） */
export function specOf(sp) {
  const out = emptySpec();
  for (const [k] of SPEC_FIELDS) {
    const f = sp?.fields?.[k] || {};
    out.fields[k] = { zh: String(f.zh || ""), en: String(f.en || ""), tags: Array.isArray(f.tags) ? f.tags.map(String) : [] };
  }
  out.locked = !!sp?.locked;
  if (out.locked) for (const k of ["block_zh", "block_en", "block_tags"]) out[k] = String(sp?.[k] || "");
  return out;
}

/** 設定表有沒有寫東西（全空的鎖不起來） */
export const specFilled = (sp) => SPEC_FIELDS.some(([k]) => sp?.fields?.[k]?.zh?.trim() || sp?.fields?.[k]?.en?.trim());

// 每個步驟目前這一輪送出的 job；換新一輪後，上一輪晚到的圖不收
const rounds = {};
export function startRound(step, jobs) { rounds[step] = { jobs: new Set(jobs), recs: [] }; }
/** 這張圖屬於哪個步驟目前這一輪（不屬於回 null） */
export function takeImage(job, rec) {
  for (const step of STEPS) {
    const r = rounds[step];
    if (r?.jobs.has(job)) { r.recs.push(rec); return step; }
  }
  return null;
}

// ---------------------------------------------------------------- 狀態
let st = blank();
let onSaved = null;
function blank() {
  return { name: "", sex: "f", text: "", start: null, pick: { turnaround: null, face: null, portrait: null, nude: null }, desc: "", desc_zh: "", tags: "",
    asset_id: null, spec: emptySpec() };
}
function persist() {
  try { localStorage.setItem(KEY, JSON.stringify(st)); } catch { /* 私密視窗之類：不記也能用 */ }
}
function restore() {
  try {
    const s = JSON.parse(localStorage.getItem(KEY) || "null");
    if (s && typeof s === "object") st = { ...blank(), ...s, pick: { ...blank().pick, ...s.pick }, spec: specOf(s.spec) };
  } catch { /* 壞掉就從頭 */ }
}

const base = () => gen.baseParams();
const curEngine = () => F("csEngine")?.value || store.get("editEngine", "qwen21");
const editTarget = () => editModel(app.state?.models, base(), curEngine());
const fileUrl = (f) => `/api/assets/file/${encodeURIComponent(f)}`;

/**
 * 打開工坊。opts：
 *   {gallery: rec}             圖庫／生成結果那張當定稿
 *   {asset: row}               資產庫的角色（名字、描述、tag、設定表、已有的圖帶進來；存的時候加回這個角色）
 *   {onSaved: fn}              存進資產庫之後叫一下（導演台、資產庫重新讀）
 */
export async function open(opts = {}) {
  restore();
  onSaved = opts.onSaved || null;
  if (opts.asset || opts.gallery) {
    st = blank();
    for (const s of STEPS) rounds[s] = undefined;
  }
  openModal("#csModal");
  if (opts.asset) {
    const a = opts.asset, d = a.data || {};
    Object.assign(st, { name: a.name || "", asset_id: a.id, desc: d.look_en || "", desc_zh: d.look_zh || "", tags: (d.tags_anima || []).join(", "), spec: specOf(d.spec) });
    const first = (role) => (d.images || []).find((im) => im.role === role);
    // 已經有的轉身圖、臉部特寫表、正面特寫、裸體版：顯示出來，存的時候不重存（補做缺的那幾樣）
    for (const [k, role] of STEP_ROLE) {
      const im = first(role);
      if (im) st.pick[k] = { file: im.file, url: fileUrl(im.file), from_asset: true };
    }
    const src = first("src");
    if (src) {
      try {
        const r = await api(`/api/assets/${a.id}/to_input`, { json: { file: src.file } });
        st.start = { input: r.name, url: fileUrl(src.file), from_asset: true };
      } catch (e) { toast(e.message, "err"); }
    }
  } else if (opts.gallery) {
    await setStartFromGallery(opts.gallery);
  }
  persist();
  await loadTargets();
  paint();
}

async function setStartFromGallery(rec) {
  try {
    const r = await api("/api/to_input", { json: { id: rec.id } });
    st.start = { input: r.name, gallery_id: rec.id, url: `/thumb/${rec.id}` };
    persist(); paint();
  } catch (e) { toast(e.message, "err"); }
}

/** 選好的圖 → 複製到 ComfyUI input：{name, width, height}（圖庫的、資產裡的都可以）；沒有回 null */
async function inputOf(p) {
  if (p?.gallery_id) return api("/api/to_input", { json: { id: p.gallery_id } });
  if (p?.file && st.asset_id) return api(`/api/assets/${st.asset_id}/to_input`, { json: { file: p.file } });
  return null;
}

// ---------------------------------------------------------------- 畫面
function pic(id, url, empty) {
  F(id).replaceChildren(url ? el("img", { src: url, alt: "" }) : el("span", { class: "muted sm" }, empty));
}

function paint() {
  F("csName").value = st.name;
  F("csSex").value = st.sex;
  F("csText").value = st.text;
  F("csDesc").value = st.desc;
  F("csDescZh").value = st.desc_zh;
  F("csTags").value = st.tags;
  F("csTarget").value = st.asset_id ? String(st.asset_id) : "";
  pic("csStart", st.start?.url, "還沒有定稿");
  pic("csTurn", st.pick.turnaround?.url, "還沒選");
  pic("csFace", st.pick.face?.url, "還沒選");
  pic("csPort", st.pick.portrait?.url, "還沒選");
  pic("csNude", st.pick.nude?.url, "還沒選");
  const engSel = F("csEngine");
  if (engSel && !engSel.dataset.synced) {
    engSel.value = store.get("csEngine", store.get("editEngine", "auto"));
    engSel.dataset.synced = "1";
  }
  const q = editTarget();
  F("csNoQwen").hidden = !!q;
  if (!q) {
    const e = curEngine();
    F("csNoQwen").textContent = e === "mmimage"
      ? "找不到 MiniMax H3 Image 模型，請先安裝或改選 Qwen 2.1。"
      : e === "qwen21"
      ? "找不到 Qwen 2.1 模型，請先安裝或改選 MiniMax H3 Image。"
      : "找不到 Qwen 2.1 或 MiniMax H3 Image 模型：展開轉身圖、臉部特寫和正面特寫需要其中一種模型。";
  }
  F("csTurnGo").disabled = F("csFaceGo").disabled = F("csPortGo").disabled = !q || !st.start;
  F("csNudeGo").disabled = !q || !st.pick.turnaround;
  F("csDescGo").disabled = F("csTagsGo").disabled = !st.start;
  F("csSave").disabled = !st.start && !rolesToSave(st).length && !st.asset_id;
  paintSpec();
}

/** 設定表：每一格中文、英文、tag；鎖住時全部唯讀，下面顯示固定文字 */
function paintSpec() {
  const sp = st.spec, off = sp.locked;
  const inp = (k, lang, ph) => {
    const f = sp.fields[k];
    const t = el("input", { type: "text", spellcheck: "false", placeholder: ph, disabled: off, "aria-label": `${SPEC_FIELDS.find((x) => x[0] === k)[1]}（${lang === "tags" ? "tag" : lang === "zh" ? "中文" : "英文"}）` });
    t.value = lang === "tags" ? f.tags.join(", ") : f[lang];
    t.onchange = () => {
      if (lang === "tags") f.tags = t.value.split(/[,，、]/).map((x) => x.trim()).filter(Boolean);
      else f[lang] = t.value.trim();
      persist();
      F("csSpecLock").disabled = !specFilled(st.spec);
    };
    return t;
  };
  F("csSpec").replaceChildren(
    el("div", { class: "cs-spec-hd" }, el("span", {}, "欄位"), el("span", {}, "中文"), el("span", {}, "英文（給影片、Qwen）"), el("span", {}, "tag（給 Anima）")),
    ...SPEC_FIELDS.map(([k, name, zh, en]) => el("div", { class: "cs-spec-row" },
      el("b", {}, name), inp(k, "zh", zh), inp(k, "en", en), inp(k, "tags", k === "marks" ? "mole under eye" : ""))),
    off && sp.block_en ? el("div", { class: "cs-spec-block" },
      el("span", { class: "sub" }, "鎖定的固定文字（用的時候原封不動放在最前面）"),
      el("div", {}, sp.block_zh), el("div", {}, sp.block_en), el("div", { class: "muted" }, sp.block_tags)) : null);
  F("csSpecState").textContent = off ? "🔒 已鎖定" : specFilled(sp) ? "還沒鎖定（存檔前按「鎖定」才會變成固定文字）" : "";
  F("csSpecLock").textContent = off ? "解鎖" : "鎖定";
  F("csSpecLock").disabled = !off && !specFilled(sp);
  F("csSpecGo").disabled = off || !st.start;
}

function readForm() {
  st.name = F("csName").value.trim();
  st.sex = F("csSex").value;
  st.text = F("csText").value;
  st.desc = F("csDesc").value;
  st.desc_zh = F("csDescZh").value;
  st.tags = F("csTags").value;
  persist();
}

/** 候選格：等待中的字、畫好的縮圖（點一張就選定） */
function paintCands(step) {
  const box = F({ base: "csBaseCands", turnaround: "csTurnCands", face: "csFaceCands", portrait: "csPortCands", nude: "csNudeCands" }[step]);
  const r = rounds[step];
  box.hidden = !r;
  if (!r) return;
  const left = r.jobs.size - r.recs.length;
  box.replaceChildren(
    ...r.recs.map((rec) => el("button", { type: "button", class: "dir-cand", title: "選這張", onclick: () => choose(step, rec) },
      el("img", { src: `/thumb/${rec.id}`, alt: "候選" }))),
    ...(left > 0 ? [el("span", { class: "muted sm" }, `畫圖中…還有 ${left} 張（每張約 1 分鐘，第一張要載入模型）`)] : []));
}

async function choose(step, rec) {
  if (step === "base") {
    await setStartFromGallery(rec);
  } else {
    st.pick[step] = { gallery_id: rec.id, url: `/thumb/${rec.id}` };
    // 轉身圖換了，舊的裸體版就不是這張的了
    if (step === "turnaround") st.pick.nude = null;
  }
  rounds[step] = undefined;
  persist(); paint(); paintCands(step);
}

/** websocket 的 image 事件 */
export function onImage(job, rec) {
  if (!rec) return;
  const step = takeImage(job, rec);
  if (step) paintCands(step);
}

async function send(step, params, count) {
  try {
    const r = await api("/api/generate", { json: { params, count } });
    startRound(step, r.jobs.map((j) => j.job));
    paintCands(step);
  } catch (e) { toast(e.message, "err"); }
}

// ---------------------------------------------------------------- 各步
async function drawBase() {
  readForm();
  const text = st.text.trim();
  if (!text) return toast("先寫角色的外觀（或直接放一張圖）", "err");
  const b = base();
  const { prompt } = await api("/api/charsheet/prompt", { json: { kind: "pose", family: b.family } });
  const { family, ...p } = b;
  await send("base", { ...p, mode: "txt2img", prompt: text.replace(/[\s,.，。]+$/, "") + prompt, seed: null, width: 832, height: 1216 }, 4);
}

/** kind：turnaround｜face｜portrait（Qwen 2.1 或 MiniMax H3 Image 照定稿畫） */
async function drawSheet(kind) {
  readForm();
  const target = editTarget();
  if (!target || !st.start) return;
  const b = base();
  try {
    const { prompt, width, height } = await api("/api/charsheet/prompt", { json: { kind, desc: st.desc, family: b.family, engine: target.family } });
    await send(kind, qwenParams(b, target.model, prompt, st.start.input, width, height, target.family), 2);
  } catch (e) { toast(e.message, "err"); }
}

async function drawNude() {
  readForm();
  const target = editTarget(), t = st.pick.turnaround;
  if (!target || !t) return;
  try {
    const r = await inputOf(t);
    if (!r) return;
    const { mode, refs, size_mode, prompt, ...qp } = qwenParams(base(), target.model, "", r.name, r.width, r.height, target.family);
    await send("nude", { ...qp, mode: "sheet", prompt: "", init_image: r.name, sheet: { req: nudeReq(st.sex), engine: target.family } }, 2);
  } catch (e) { toast(e.message, "err"); }
}

function status(msg, cls = "") {
  F("csStatus").textContent = msg;
  F("csStatus").className = "pe-status " + cls;
}

async function describe() {
  if (!st.start) return;
  status("AI 看圖中…（第一次要載入模型）", "run");
  try {
    const r = await api("/api/director/describe", { json: { image: st.start.input } });
    st.desc = r.desc || ""; st.desc_zh = r.desc_zh || "";
    persist(); paint(); status("寫好了，可以直接改", "ok");
  } catch (e) { status(e.message, "err"); }
}

async function tags() {
  if (!st.start) return;
  status("反推中…", "run");
  try {
    const r = await api("/api/charsheet/tags", { json: { name: st.start.input } });
    st.tags = r.tags.join(", ");
    persist(); paint(); status(r.tags.length ? "反推好了（姿勢、背景那些已經拿掉），可以直接改" : "反推沒找到外觀類的 tag", "ok");
  } catch (e) { status(e.message, "err"); }
}

/** 讀圖模型看定稿＋正面特寫＋轉身圖填設定表（只填，不鎖） */
let filling = false;  // AI 看圖填表中：不能鎖（不然填回來會蓋掉剛鎖的）
async function fillSpec() {
  readForm();
  if (!st.start || st.spec.locked) return;
  if (specFilled(st.spec) && !confirm("AI 會重填整張設定表（蓋掉現在寫的）。繼續？")) return;
  const btn = F("csSpecGo");
  btn.disabled = true;
  filling = true;
  status("AI 看圖填設定表中…（第一次要載入模型）", "run");
  try {
    const images = [st.start.input];
    for (const k of ["portrait", "turnaround"]) {
      const p = st.pick[k];
      if (p) { try { const r = await inputOf(p); if (r?.name) images.push(r.name); } catch { /* 少一張也能填 */ } }
    }
    const r = await api("/api/charsheet/spec", { json: { images, name: st.name, sex: st.sex, desc: st.desc, desc_zh: st.desc_zh, tags: st.tags,
      brain: F("csSpecBrain").value }, timeout: 0 });
    if (st.spec.locked) return status("填表期間設定表被鎖定了：AI 填的沒有用上", "err");
    st.spec = specOf({ fields: r.fields, locked: false });
    showSpecUnver(r.unverified || []);
    persist(); paint();
    status("填好了：逐格看過、改好，再按「鎖定」", "ok");
  } catch (e) { status(e.message, "err"); } finally { filling = false; paintSpec(); }
}

function showSpecUnver(list) {
  const box = F("csSpecUnver");
  box.hidden = !list.length;
  box.replaceChildren(el("span", {}, "這些 tag 在 tag 表裡找不到（照樣留著，確定要用再留）："), ...list.map((t) => el("span", { class: "as-bad" }, t)));
}

function toggleLock() {
  if (filling) return toast("AI 還在填設定表：填完再鎖", "err");
  const sp = st.spec;
  if (sp.locked) {
    if (!confirm("解鎖後才能改欄位；改完要再按一次「鎖定」，固定文字會照新的欄位重新組。解鎖？")) return;
    st.spec = specOf({ fields: sp.fields, locked: false });
  } else {
    if (!specFilled(sp)) return toast("設定表還是空的", "err");
    st.spec = { ...sp, locked: true };
    toast("鎖好了：存進資產庫時會組成固定文字", "ok");
  }
  persist(); paint();
}

/** 「存到」下拉：新角色＋現有的角色資產 */
let chars = [];
async function loadTargets() {
  try { chars = (await api("/api/lib")).filter((it) => it.kind === "char"); } catch { chars = []; }
  F("csTarget").replaceChildren(el("option", { value: "" }, "存成新角色"),
    ...chars.map((c) => el("option", { value: String(c.id) }, `加到「${c.name}」`)));
}

async function save() {
  readForm();
  if (!st.name && !st.asset_id) return toast("角色要有名字", "err");
  const list = rolesToSave(st);
  if (!list.length && !st.asset_id) return toast("還沒有可以存的圖（先放定稿或選設定圖）", "err");
  const tagList = st.tags.split(/[,，、\n]/).map((t) => t.trim()).filter(Boolean);
  F("csSave").disabled = true;
  status("存檔中…", "run");
  try {
    let id = st.asset_id;
    const old = id ? chars.find((c) => c.id === id) : null;
    // 加到現有角色：data 整包送，所以先帶上舊的（聲音、語言、物件位置）再蓋描述、tag、設定表；圖片、鎖住的設定表後端會保留
    const data = { ...(old?.data || {}), look_en: st.desc.trim(), look_zh: st.desc_zh.trim(), tags_anima: tagList, spec: st.spec };
    // 加到現有角色不改它的名字
    const r = await api("/api/lib", { json: id ? { id, data } : { kind: "char", name: st.name, data } });
    id = r.id;
    for (const f of list) await api(`/api/assets/${id}/file`, { json: { ...f, replace: true } });
    const locked = r.data?.spec?.locked ? "，設定表已鎖定" : "";
    toast(`「${st.name || old?.name}」存進資產庫了（${list.length} 張圖${locked}）`, "ok");
    if (r.unverified?.length) toast(`這些 tag 在 Danbooru 對不到，存了但要注意：${r.unverified.join(", ")}`, "err");
    st = blank();
    try { localStorage.removeItem(KEY); } catch { /* 同上 */ }
    for (const s of STEPS) rounds[s] = undefined;
    status("", "");
    showSpecUnver([]);
    await loadTargets();
    paint();
    STEPS.forEach(paintCands);
    onSaved?.(id);
  } catch (e) {
    status(e.message, "err");
  } finally {
    paint();
  }
}

export function init() {
  for (const id of ["csName", "csSex", "csText", "csDesc", "csDescZh", "csTags"]) F(id).onchange = readForm;
  const engSel = F("csEngine");
  if (engSel) {
    engSel.value = store.get("csEngine", store.get("editEngine", "auto"));
    engSel.dataset.synced = "1";
    engSel.onchange = () => {
      store.set("csEngine", engSel.value);
      if (engSel.value !== "auto") store.set("editEngine", engSel.value);
      paint();
    };
  }
  F("csUp").onclick = async () => {
    const f = await pickFile();
    if (!f) return;
    try { const r = await uploadFile(f); st.start = { input: r.name, url: `/input/${encodeURIComponent(r.name)}` }; persist(); paint(); } catch (e) { toast(e.message, "err"); }
  };
  F("csGal").onclick = () => pickFromGallery((rec) => setStartFromGallery(rec));
  F("csBaseGo").onclick = () => drawBase().catch((e) => toast(e.message, "err"));
  F("csTurnGo").onclick = () => drawSheet("turnaround");
  F("csFaceGo").onclick = () => drawSheet("face");
  F("csPortGo").onclick = () => drawSheet("portrait");
  F("csNudeGo").onclick = drawNude;
  F("csDescGo").onclick = describe;
  F("csTagsGo").onclick = tags;
  F("csSpecGo").onclick = fillSpec;
  F("csSpecLock").onclick = toggleLock;
  F("csSave").onclick = save;
  F("csTarget").onchange = async () => {
    const id = Number(F("csTarget").value) || null;
    st.asset_id = id;
    const c = chars.find((x) => x.id === id);
    // 選了現有角色、還沒有定稿：拿那個角色的原圖當定稿
    if (c && !st.start) return open({ asset: c, onSaved });
    if (c && !st.name) st.name = c.name;
    persist(); paint();
  };
  F("csReset").onclick = () => {
    if (!confirm("清空這次的進度？（畫好的圖都還在圖庫，不會刪）")) return;
    st = blank();
    for (const s of STEPS) rounds[s] = undefined;
    try { localStorage.removeItem(KEY); } catch { /* 同上 */ }
    showSpecUnver([]);
    paint(); STEPS.forEach(paintCands); status("", "");
  };
}

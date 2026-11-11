// H3 長片（2026-10-23）：導演頁的「長片」分頁。好幾段排成一條，雲端一段一段拍（第 2 段起直接吃上一段的 AV latent 接著拍），
// 每段自動切掉定住的尾巴、用拆幀台檢查跳點，全部拍好在這台電腦接成一支進圖庫。
// 後端：src-tauri\src\h3chain.rs（存檔格式、提示詞、節點圖）、server\api_h3chain.rs（/api/h3/chain*）。設計在 design\H3長片接續_設計.md。
import { el, api, toast, store } from "./util.js";
import { pickFromGallery, openLightboxRecord } from "./gallery.js";

const F = (id) => document.getElementById(id);
const SECS = [5, 6, 8, 10, 12, 15];
const LANGS = ["Chinese", "English", "Japanese", "Cantonese", "Korean"];
const ST = { todo: "還沒拍", running: "拍攝中", done: "拍好了", error: "失敗", stopped: "已停止", review: "待選片" };
const FILE_MAX = 30 * 2 ** 20;
let list = [];          // /api/h3/chains
let chain = null;       // 正在看的那條（/api/h3/chain/{id}）
let cloud = null;       // /api/agent/cloud
let rhub = null;        // /api/runninghub（長片選 RunningHub 拍時看金鑰）
let saveTimer = null;
let lastMsg = "";
let openRevision = 0;
let listRevision = 0, saveRevision = 0, editRevision = 0;
let selecting = null;

const btn = (label, onclick, cls = "akey sm", title = "") => el("button", { class: cls, type: "button", title: title || undefined, onclick }, label);
const running = () => !!chain?.running || (!!chain && selecting === chain);
const current = (owner, revision) => chain === owner && openRevision === revision;
const sizeLocked = () => Array.isArray(chain?.size) || (chain?.segments || []).some(s => s.status === "done" || s.selected_take || s.pending_take || (s.takes || []).length);
const fileB64 = (file) => new Promise((ok, no) => {
  const fr = new FileReader();
  fr.onload = () => ok(String(fr.result).split(",")[1] || "");
  fr.onerror = () => no(new Error("讀不到這個檔案"));
  fr.readAsDataURL(file);
});
const mins = (s) => (s >= 3600 ? `${(s / 3600).toFixed(1)} 小時` : `${Math.max(1, Math.round(s / 60))} 分鐘`);

function setStatus(msg, cls = "g") {
  const s = F("hcStatus");
  s.textContent = msg;
  s.className = `vfd ${cls} dir-status`;
}

// ================================================================ 讀、存
async function loadList(want) {
  const revision = openRevision, ticket = ++listRevision;
  let loaded;
  try { loaded = (await api("/api/h3/chains")).chains; }
  catch (e) { if (revision === openRevision && ticket === listRevision) toast(e.message, "err"); return; }
  if (revision !== openRevision || ticket !== listRevision) return;
  list = loaded;
  const sel = F("hcList");
  sel.replaceChildren(...list.map((c) => el("option", { value: c.id }, `${c.title}（${c.done}／${c.segments} 段）`)));
  if (!list.length) sel.append(el("option", { value: "" }, "還沒有長片：按「＋ 新的」"));
  const id = [want, chain?.id, store.get("hcId", "")].find((x) => x && list.some((c) => c.id === x)) || list[0]?.id || "";
  sel.value = id;
  await open(id);
}

async function open(id) {
  const ticket = ++openRevision;
  clearTimeout(saveTimer);
  if (!id) { chain = null; return paint(); }
  try { const loaded = await api(`/api/h3/chain/${id}`); if (ticket !== openRevision) return; chain = loaded; }
  catch (e) { if(ticket !== openRevision) return; chain = null; toast(e.message, "err"); }
  if (chain) store.set("hcId", chain.id);
  paint();
  // 從拍攝單套用過來的長片：拍攝單帶回上面（草稿有東西時先問）
  const owner = chain;
  if (owner?.sheet) import("./shotsheet.js").then((m) => { if (current(owner, ticket)) m.offerLoad(owner.sheet); }).catch(() => {});
}

/** 拍攝單「套用到長片」：每段的提示詞、首幀、參考圖、拍攝單本身寫進目前這條（已經有拍好的段就問要不要另存一條新的）。回 true＝存好了 */
export async function applySheet({ sheet, prompts, first = null, refs = [], segRefs = [] }) {
  const owner = chain, revision = openRevision;
  let base = chain;
  if (base && running()) { toast("這條長片正在拍：等拍完或停掉再套用", "err"); return false; }
  if (base && (base.segments || []).some((s) => s.status === "done")) {
    if (!confirm(`「${base.title}」已經有拍好的段，套用會蓋掉。要另存成一條新的長片嗎？`)) return false;
    base = null;
  }
  const body = {
    ...(base || {}), title: base?.title && base.title !== "未命名長片" ? base.title : sheet.title || "拍攝單長片",
    look: "", scene: "", style: "", voice: "", sound: "", ref: null, first: first || null, refs, sheet,
    segments: prompts.map((text, i) => ({ secs: sheet.segments[i]?.secs || 10, text, lines: [], last: null, status: "todo", ...(Array.isArray(segRefs[i]) ? { refs: segRefs[i] } : {}),
      entry: sheet.segments[i]?.entry, exit: sheet.segments[i]?.exit, inherit_entry: sheet.segments[i]?.inherit_entry })),
    final: null,
  };
  if (!base) delete body.id;
  let r;
  try { r = await api("/api/h3/chain/save", { json: { chain: body } }); } catch (e) { toast(e.message, "err"); return false; }
  if (!current(owner, revision)) return false;
  await loadList(r.id);
  return true;
}

/** 欄位改了：0.8 秒後存（拍攝中不能存） */
function changed() {
  editRevision++;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(save, 800);
}

async function save() {
  clearTimeout(saveTimer);
  if (!chain || running()) return false;
  const owner = chain, revision = openRevision, edit = editRevision, ticket = ++saveRevision;
  try {
    const r = await api("/api/h3/chain/save", { json: { chain: structuredClone(owner) } });
    if (!current(owner, revision) || edit !== editRevision || ticket !== saveRevision) return false;
    if (!chain.id) return loadList(r.id);
    // 不整個換掉：畫面上的段落卡還拿著原本的物件，換掉之後再打的字會改到舊物件
    for (const k of ["id", "est", "running", "size", "updated"]) chain[k] = r[k];
    let resultsChanged = false;
    for (let i = 0; i < (r.segments || []).length; i++) {
      const local = owner.segments[i], saved = r.segments[i];
      if (!local) continue;
      for (const k of ["status", "error", "seed", "took", "head", "frames", "handover", "files", "check", "takes", "selected_take", "pending_take"]) {
        if (JSON.stringify(local[k] ?? null) !== JSON.stringify(saved[k] ?? null)) resultsChanged = true;
        local[k] = structuredClone(saved[k] ?? null);
      }
    }
    owner.final = structuredClone(r.final ?? null);
    if (resultsChanged) paintSegs();
    paintRun();
    const o = [...F("hcList").options].find((x) => x.value === r.id);
    if (o) o.textContent = `${r.title}（${r.segments.filter((x) => x.status === "done").length}／${r.segments.length} 段）`;
    paintEst();  // 段落卡不重畫：正在打字的框會失去游標
    return true;
  } catch (e) { if (current(owner, revision)) toast(e.message, "err"); return false; }
}

async function create() {
  const owner = chain, revision = openRevision;
  try {
    const r = await api("/api/h3/chain/save", { json: { chain: { title: "未命名長片", review: true, segments: [{ secs: 10, text: "" }] } } });
    if (!current(owner, revision)) return;
    await loadList(r.id);
  } catch (e) { toast(e.message, "err"); }
}

async function remove() {
  if (!chain || !confirm(`「${chain.title}」移到待刪除？（已經進圖庫的長片不受影響）`)) return;
  const owner = chain, revision = openRevision;
  try { await api("/api/h3/chain/delete", { json: { id: owner.id } }); } catch (e) { return toast(e.message, "err"); }
  if (!current(owner, revision)) return;
  chain = null;
  toast("已移到待刪除", "ok");
  loadList();
}

// ================================================================ 畫面
function paint() {
  const on = !!chain;
  for (const id of ["hcTitle", "hcLook", "hcScene", "hcStyle", "hcVoice", "hcSound", "hcAspect", "hcQuality", "hcWhere", "hcDel", "hcAddSeg", "hcRun", "hcJoin"]) F(id).disabled = !on;
  F("hcTitle").value = chain?.title || "";
  for (const [id, k] of [["hcLook", "look"], ["hcScene", "scene"], ["hcStyle", "style"], ["hcVoice", "voice"], ["hcSound", "sound"]]) F(id).value = chain?.[k] || "";
  F("hcAspect").value = chain?.aspect || "9:16";
  F("hcQuality").value = chain?.quality || "std";
  F("hcWhere").value = chain?.where || "runpod";
  paintPic("hcRef", "ref", "角色參考圖（臉、髮型、服裝）");
  paintPic("hcFirst", "first", "第 1 段的第一格（可不放）");
  let mode = F("hcReviewMode");
  if (!mode) {
    const input = el("input", { type:"checkbox", id:"hcReviewMode" });
    const row = el("label", {class:"check film-review-mode"}, input, "逐鏡審片：拍好一鏡後先選版本，再繼續下一鏡");
    F("hcSegs").before(row);
    input.onchange = () => { if(chain) { chain.review=input.checked; changed(); } };
    mode = input;
  }
  mode.checked = chain?.review === true;
  mode.disabled = !chain || running();
  paintSegs();
  paintCloud();
  paintRun();
}

function paintPic(box, key, label) {
  const name = chain?.[key];
  const prev = name ? el("img", { class: "h3-prev", src: `/input/${encodeURIComponent(name)}`, alt: "" }) : el("div", { class: "h3-prev empty" }, "還沒放");
  F(box).replaceChildren(el("span", { class: "sub" }, label), prev, el("div", { class: "row gap wrap" },
    btn("圖庫", () => pickInto((n) => { chain[key] = n; paint(); changed(); }, "gallery"), "akey sm", "從圖庫選一張圖"),
    btn("電腦", () => pickInto((n) => { chain[key] = n; paint(); changed(); }, "file"), "akey sm", "從電腦選一張圖"),
    name ? btn("×", () => { chain[key] = null; paint(); changed(); }, "h3-x", "拿掉") : null));
}

/** 選一張圖放進 input（走 H3 影片台的 /api/h3/media），拿到檔名給 cb */
function pickInto(cb, from) {
  if (!chain || running()) return;
  const owner = chain, revision = openRevision;
  const put = async (body) => {
    if (!current(owner, revision)) return;
    try {
      const r = await api("/api/h3/media", { json: body, timeout: 5 * 60 * 1000 });
      if (!current(owner, revision)) return;
      if (r.kind !== "image") throw new Error("這裡要放圖");
      cb(r.name);
    } catch (e) { if (current(owner, revision)) toast(e.message, "err"); }
  };
  if (from === "gallery") return pickFromGallery((rec) => put({ gallery_id: rec.id }), { title: "從圖庫選一張圖" }).catch((e) => toast(e.message, "err"));
  const inp = el("input", { type: "file", accept: "image/png,image/jpeg,image/webp", hidden: true });
  inp.onchange = async () => {
    const f = inp.files[0];
    inp.remove();
    if (!f) return;
    if (f.size > FILE_MAX) return toast("圖太大了", "err");
    put({ file_name: f.name, data_b64: await fileB64(f) });
  };
  document.body.append(inp);
  inp.click();
}

/** 最後一段已經拍好的是第幾段（-1＝都還沒拍）：只能在它後面加段、刪段（前面的段刪掉，後面的接續就斷了） */
const lastDone = () => (chain?.segments || []).reduce((a, s, i) => (s.status === "done" ? i : a), -1);

function paintSegs() {
  const box = F("hcSegs");
  if (!chain) return box.replaceChildren(el("div", { class: "muted sm" }, "選一條長片，或按「＋ 新的」"));
  box.replaceChildren(...chain.segments.map(segEl));
}

function segEl(s, i) {
  const lock = running() || s.status === "done" || !!s.pending_take;
  const st = el("span", { class: `h3-st ${s.status === "todo" ? "" : s.status}` }, ST[s.status] || s.status);
  const secs = el("select", { "aria-label": "秒數", disabled: lock }, ...SECS.map((n) => el("option", { value: n }, `${n} 秒`)));
  secs.value = String(s.secs);
  secs.onchange = () => { s.secs = +secs.value; changed(); };
  const text = el("textarea", { rows: 3, placeholder: i === 0 ? "這段的畫面和動作（英文最穩，例：She kneels and smiles up at the camera, then…）" : "接著發生什麼（例：She slowly stands up and turns to the side…）", disabled: lock });
  text.value = s.text || "";
  text.oninput = () => { s.text = text.value; changed(); };
  const hd = el("div", { class: "h3-shot-hd" }, el("b", {}, `第 ${i + 1} 段`), secs, st,
    s.took ? el("span", { class: "muted sm" }, `花了 ${mins(s.took)}`) : null,
    el("span", { class: "grow" }),
    s.status !== "todo" && !running() ? btn("從這段重拍", () => rerun(i), "akey sm", "這段和後面的段全部重拍（後面的段是接這段拍的）") : null,
    !lock && !(s.takes || []).length && i > lastDone() && chain.segments.length > 1 ? btn("×", () => { chain.segments.splice(i, 1); paintSegs(); changed(); }, "h3-x", "刪掉這一段") : null);
  const lines = el("div", { class: "h3-lines" }, ...(s.lines || []).map((ln, j) => lineEl(s, ln, j, lock)));
  const out = [hd, text, lines];
  if (!lock) out.push(el("div", { class: "row gap wrap" }, btn("＋ 台詞", () => { (s.lines ||= []).push({ at: 1, lang: "Chinese", text: "" }); paintSegs(); changed(); })));
  out.push(lastEl(s, lock));
  if (Array.isArray(s.refs)) out.push(el("div", { class: "muted sm" }, `參考圖：這段只送 ${s.refs.length}／${(chain.refs || []).length} 張（在拍攝單改）`));
  if (s.error) out.push(el("div", { class: "warn-text sm" }, s.error));
  if (s.status === "done") out.push(resultEl(s, i));
  if ((s.takes || []).length) out.push(takesEl(s,i));
  if (s.draft) out.push(el("details",{class:"film-takes"},el("summary",{},"切版前保留的未拍草稿"),
    el("pre",{translate:"no",style:"white-space:pre-wrap"},JSON.stringify(s.draft,null,2)),
    btn("套回草稿",async()=>{if(running())return;Object.assign(s,s.draft);delete s.draft;await save();await open(chain.id);},"akey sm")));
  return el("div", { class: "h3-shot" }, ...out);
}

function takesEl(s, i) {
  const list = el("div", {class:"film-takes-grid"});
  for (const take of s.takes || []) {
    const chosen = take.id === s.selected_take;
    const file = take.result?.files?.end;
    const adopt = btn(chosen ? "已採用" : "採用這版", async () => {
      if (running()) return;
      if (!chosen && chain.segments.slice(i+1).some(x=>x.status==="done") && !confirm("採用不同版本後，後面的鏡頭需要重新接續；所有舊版本會保留。繼續嗎？")) return;
      const owner = chain, revision = openRevision;
      clearTimeout(saveTimer);
      if (!(await save()) || !current(owner, revision)) return;
      selecting = owner;
      paint();
      try {
        const selected = await api("/api/h3/chain/take/select", {json:{id:owner.id,seg:i,take_id:take.id}});
        if (current(owner, revision)) chain = selected;
      } catch(e) { if (current(owner, revision)) toast(e.message,"err"); }
      finally { if (selecting === owner) selecting = null; if (openRevision === revision) paint(); }
    });
    adopt.disabled = (chosen && !s.pending_take) || running();
    list.append(el("div", {class:`film-take ${chosen ? "selected" : ""}`}, el("b",{},take.label || "拍攝版本"),
      file ? el("video", {class:"h3-prev",src:`/api/h3/chain/${chain.id}/file/${encodeURIComponent(file)}`,controls:true,preload:"none",playsinline:true}) : null,
      adopt,el("details",{},el("summary",{},"拍攝配方"),el("pre",{translate:"no"},JSON.stringify(take.recipe,null,2)))));
  }
  return el("details",{class:"film-takes",open:!!s.pending_take},el("summary",{},"拍攝版本",el("span",{class:"muted sm"},` (${s.takes.length})`)),
    s.pending_take ? el("p",{class:"muted sm"},"新版本尚未採用，下一鏡會等待你的選擇。") : null,list,
    !running() ? btn("重拍這鏡並審片",()=>{
      // 跟「從這段重拍」一樣要先講：會再花一次錢，後面拍好的段也要重新接（2026-11-10 以前這顆按了就直接開拍）
      const after = chain.segments.slice(i+1).filter(x=>x.status==="done").length;
      if (!confirm(`重拍第 ${i + 1} 段並審片？${after ? `後面拍好的 ${after} 段要重新接續（舊版本都會保留）。` : ""}會再花一次${chain.where === "runninghub" ? " RunningHub 的幣" : "雲端的錢"}。`)) return;
      chain.review=true;run(i);
    },"akey sm") : null);
}

function lineEl(s, ln, j, lock) {
  const at = el("input", { type: "number", class: "h3-to", min: 0, max: s.secs, step: 0.5, value: ln.at, "aria-label": "第幾秒說", disabled: lock });
  at.oninput = () => { ln.at = +at.value; changed(); };
  const lang = el("select", { "aria-label": "語言", disabled: lock }, ...LANGS.map((l) => el("option", { value: l }, l)));
  lang.value = ln.lang || "Chinese";
  lang.onchange = () => { ln.lang = lang.value; changed(); };
  const t = el("input", { type: "text", class: "grow", placeholder: "台詞", value: ln.text || "", disabled: lock });
  t.oninput = () => { ln.text = t.value; changed(); };
  return el("div", { class: "h3-line" }, el("span", { class: "sub" }, "秒"), at, lang, t,
    lock ? null : btn("×", () => { s.lines.splice(j, 1); paintSegs(); changed(); }, "h3-x", "刪掉這句"));
}

/** 可選的尾幀：預設不釘（釘了不同狀態的尾幀，模型會在半路瞬移） */
function lastEl(s, lock) {
  if (!s.last) return lock ? el("span") : el("div", { class: "row gap wrap muted sm" }, "尾幀（可不放；放了模型會趕著到這張，容易跳）",
    btn("圖庫", () => pickInto((n) => { s.last = n; paintSegs(); changed(); }, "gallery")), btn("電腦", () => pickInto((n) => { s.last = n; paintSegs(); changed(); }, "file")));
  return el("div", { class: "row gap" }, el("span", { class: "sub" }, "尾幀"), el("img", { class: "hc-last", src: `/input/${encodeURIComponent(s.last)}`, alt: "" }),
    lock ? null : btn("×", () => { s.last = null; paintSegs(); changed(); }, "h3-x", "拿掉尾幀"));
}

function resultEl(s, i) {
  const src = `/api/h3/chain/${chain.id}/file/${s.files?.end || `seg${String(i + 1).padStart(2, "0")}_end.mp4`}`;
  const v = el("video", { class: "h3-prev", src, controls: true, preload: "metadata", playsinline: true });
  const c = s.check || {};
  const flags = c.error ? el("span", { class: "muted sm" }, `沒檢查到：${c.error}`)
    : el("span", { class: (c.cuts?.length || c.jumps?.length) ? "warn-text sm" : "muted sm" },
      (c.cuts?.length || c.jumps?.length) ? `⚠ 硬剪接 ${c.cuts?.length || 0} 處、跳點 ${c.jumps?.length || 0} 處（第 ${[...(c.cuts || []), ...(c.jumps || [])].join("、")} 格）` : "沒有跳點");
  return el("div", { class: "h3-outcell" }, v, el("div", { class: "row gap wrap" }, flags,
    c.sid ? btn("拆幀台看", () => openFramesSess(c.sid), "akey sm", "在拆幀台逐格看這一段") : null));
}

async function openFramesSess(sid) {
  const studio = await import("./studio.js");
  studio.openFramesSession(sid);
}

function paintEst() {
  if (!chain) return (F("hcEst").textContent = "");
  const todo = chain.segments.filter((s) => s.status !== "done");
  const secs = todo.reduce((a, s) => a + s.secs, 0);
  const price = cloud?.pod && cloud.price ? `・每小時約 US$${(+cloud.price).toFixed(2)}，這次約 US$${((chain.est / 3600) * cloud.price).toFixed(2)}` : "";
  // RunningHub：照秒扣幣（48GB 每秒 0.4），不用載模型、沒有開機時間
  const rate = (rhub?.rates || []).find((r) => r.instance === rhub?.instance)?.coins_per_sec ?? 0.4;
  F("hcEst").textContent = !todo.length ? "全部拍好了"
    : chain.where === "runninghub" ? `還有 ${todo.length} 段、${secs} 秒影片，RunningHub 大約要算 ${mins(chain.est)}，約 ${Math.round(chain.est * rate)} RH 幣（實際照秒算）`
    : `還有 ${todo.length} 段、${secs} 秒影片，雲端大約要算 ${mins(chain.est)}（第一次載入模型再加約 15 分鐘）${price}`;
  const sz = chain.size ? `畫面 ${chain.size[0]}×${chain.size[1]}（第 1 段拍好後固定）` : "畫面大小在第 1 段開拍時決定：有首幀就照首幀的比例";
  F("hcSizeNote").textContent = sz;
}

function paintCloud() {
  if (chain?.where === "runninghub") {
    F("hcCloud").textContent = !rhub ? "讀不到 RunningHub 設定" : rhub.has_key
      ? "用 RunningHub 拍：不用開雲端；每段一個任務，上一段最後約 0.9 秒的畫面和聲音當下一段的開頭（RefMod 不能用）"
      : "還沒填 RunningHub 的 API Key：到「H3 影片」分頁「在哪跑 → RunningHub」填";
    F("hcGoCloud").hidden = !!rhub?.has_key;
    F("hcGoCloud").textContent = "到「H3 影片」分頁填金鑰";
    paintEst();
    return;
  }
  F("hcGoCloud").textContent = "到「H3 影片」分頁開雲端";
  const st = cloud;
  const packs = st?.pod_packs || st?.packs || {};
  const ok = st?.phase === "ready" && packs.h3;
  F("hcCloud").textContent = !st ? "讀不到雲端狀態" : ok ? `雲端好了（${st.gpu || "雲端"}）${st.idle_left != null ? `・再閒置 ${st.idle_left} 分鐘自動關` : ""}`
    : st.phase === "ready" ? "雲端這台沒裝 H3 影片包" : `雲端${{ off: "沒開", starting: "開機中…", stopping: "關機中…" }[st.phase] || st.phase}：要先開雲端，或在「在哪拍」改用 RunningHub`;
  F("hcGoCloud").hidden = ok;
  paintEst();
}

function paintRun() {
  const r = running();
  F("hcRun").disabled = !chain || r || chain.segments.some(s=>s.pending_take);
  if (F("hcReviewMode")) F("hcReviewMode").disabled = !chain || r;
  F("hcStop").disabled = !r;
  F("hcJoin").disabled = !chain || r || !chain.segments.every((s) => s.status === "done");
  F("hcAddSeg").disabled = !chain || r || chain.segments.length >= 30;
  for (const id of ["hcAspect", "hcQuality", "hcWhere"]) F(id).disabled = !chain || r || sizeLocked();
  const fin = chain?.final;
  const ck = fin?.check;
  const hits = ck?.seam_hits || [];
  const seamNote = !ck ? null : ck.error ? el("span", { class: "muted sm" }, `接縫沒檢查到：${ck.error}`)
    : hits.length ? el("span", { class: "warn-text sm" }, `⚠ 接縫跳了：${hits.map((h) => `第 ${h.seam}→${h.seam + 1} 段（第 ${h.frame} 格）`).join("、")}`)
    : el("span", { class: "muted sm" }, "接縫檢查：沒有跳");
  F("hcFinal").replaceChildren(...(fin?.id ? [el("div", { class: "row gap wrap" }, btn(`看接好的長片 #${fin.id}`, () => openFinal(fin.id), "akey sm go"),
    seamNote, ck?.sid ? btn("拆幀台看", () => openFramesSess(ck.sid), "akey sm", "在拆幀台逐格看接好的整支") : null)] : [el("span")]));
  F("hcMsg").textContent = r ? lastMsg : "";
}

async function openFinal(id) {
  try {
    openLightboxRecord(await api(`/api/image/${id}`));
  } catch (e) { toast(e.message, "err"); }
}

// ================================================================ 開拍、停、接
async function run(from) {
  if (!chain) return;
  const owner = chain, revision = openRevision;
  // 從拍攝單來的長片：開拍前健檢（舊長片沒有拍攝單，時間軸那幾條規則對不上，不檢）
  if (owner.sheet) {
    const ss = await import("./shotsheet.js");
    if (!current(owner, revision) || !(await ss.precheck(owner.segments.map((s) => s.text || ""), owner.sheet))) return;
  }
  if (!current(owner, revision) || !(await save()) || !current(owner, revision)) return;
  const body = { id: owner.id };
  if (from != null) body.from = from;
  try { await api("/api/h3/chain/run", { json: body }); } catch (e) { if (current(owner, revision)) toast(e.message, "err"); return; }
  if (!current(owner, revision)) return;
  chain.running = true;
  lastMsg = "開拍";
  setStatus("拍攝中");
  open(chain.id);
}

function rerun(i) {
  const after = chain.segments.length - i - 1;
  if (!confirm(`第 ${i + 1} 段${after ? `和後面 ${after} 段` : ""}全部重拍？（後面的段是接這段拍的，拍好的會作廢；會花雲端的錢）`)) return;
  run(i);
}

async function stop() {
  if (!chain) return;
  try { await api("/api/h3/chain/stop", { json: { id: chain.id } }); } catch (e) { toast(e.message, "err"); }
}

async function join() {
  if (!chain) return;
  const owner = chain, revision = openRevision;
  try { await api("/api/h3/chain/join", { json: { id: owner.id } }); } catch (e) { if (current(owner, revision)) toast(e.message, "err"); return; }
  if (!current(owner, revision)) return;
  chain.running = true;
  paintRun();
}

// ================================================================ 外面來的
/** websocket：h3chain（拍攝進度）、agent_cloud（雲端狀態） */
export function onEvent(m) {
  if (m.type === "agent_cloud") { cloud = m.status; if (F("hcCloud")) paintCloud(); return; }
  if (m.type !== "h3chain") return;
  if (chain?.id !== m.id) { if (["seg_done", "done"].includes(m.state)) loadList(chain?.id); return; }
  lastMsg = m.msg || "";
  if (m.state === "running") {
    setStatus(m.msg || "拍攝中");
    F("hcMsg").textContent = lastMsg;
    if (!chain.running || (m.seg != null && chain.segments[m.seg]?.status !== "running")) open(chain.id);
    return;
  }
  setStatus({ seg_done: m.msg, done: "全部拍好、接好了", error: "失敗", stopped: "已停止", review: "待選片" }[m.state] || m.msg, m.state === "error" ? "r" : "g");
  if (m.state === "error") toast(m.msg, "err");
  if (m.state === "done") toast(`${chain.title}：接好了，進圖庫了`, "ok");
  if (m.state === "seg_done") open(chain.id); else loadList(chain.id);
}

export async function show() {
  try { cloud = await api("/api/agent/cloud"); } catch { cloud = null; }
  try { rhub = await api("/api/runninghub"); } catch { rhub = null; }
  paintCloud();
  await loadList();
}

export function init() {
  F("hcList").onchange = () => open(F("hcList").value);
  F("hcNew").onclick = create;
  F("hcDel").onclick = remove;
  F("hcTitle").oninput = () => { if (chain) { chain.title = F("hcTitle").value; changed(); } };
  for (const [id, k] of [["hcLook", "look"], ["hcScene", "scene"], ["hcStyle", "style"], ["hcVoice", "voice"], ["hcSound", "sound"]]) {
    F(id).oninput = () => { if (chain) { chain[k] = F(id).value; changed(); } };
  }
  for (const [id, k] of [["hcAspect", "aspect"], ["hcQuality", "quality"], ["hcWhere", "where"]]) {
    F(id).onchange = () => { if (chain) { chain[k] = F(id).value; changed(); paintCloud(); } };
  }
  F("hcAddSeg").onclick = () => {
    if (!chain) return;
    chain.segments.push({ secs: 10, text: "", lines: [], last: null, status: "todo" });
    paintSegs();
    changed();
  };
  F("hcRun").onclick = () => run();
  F("hcStop").onclick = stop;
  F("hcJoin").onclick = join;
  F("hcGoCloud").onclick = async () => (await import("./studio.js")).openTab("h3");
  paint();
}

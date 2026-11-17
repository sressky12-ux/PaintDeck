// 資產庫頁：角色、場景、道具…全站共用（後端 server\api_assets.rs；資料跟提示詞庫同一張 prompt_lib 表）。
// 整套設定、提示詞片段也列在這裡，點下去沿用提示詞庫原本的編輯窗（library.js）
import { $, $$, api, el, toast, pickFile, uploadFile, openModal, closeModal } from "./util.js";
import { pickFromGallery } from "./gallery.js";
import * as library from "./library.js";
import * as loras from "./loras.js";

const F = (id) => document.getElementById(id);

/** 分類：鍵、中文名（照設計文件 4.2） */
export const KINDS = [
  ["char", "角色"], ["scene", "場景"], ["frame", "首幀／姿勢"], ["outfit", "服裝"], ["prop", "道具"],
  ["fx", "特效"], ["sound", "聲音"], ["snippet", "提示詞片段"], ["set", "整套設定"], ["shot", "拍攝單範本"],
];
export const KIND_NAME = Object.fromEntries(KINDS);
const MEDIA = new Set(["char", "scene", "frame", "outfit", "prop", "fx", "sound", "shot"]);
/** 圖片用途：鍵、中文名（順序就是拿去用時的排序） */
export const ROLES = [
  ["src", "原圖"], ["portrait", "正面特寫"], ["sheet", "多視圖（穿衣）"], ["face", "臉部特寫表"], ["sheet_nude", "多視圖（裸體）"], ["detail", "細節"],
  ["pose", "姿勢"], ["outfit", "服裝"], ["other", "其他"],
];
const LAYOUT = [["top", "上方"], ["bottom", "下方"], ["left", "左邊"], ["right", "右邊"], ["fg", "前景"], ["mid", "中景"], ["bg", "遠景"], ["light", "光線"]];
const TEXT_KINDS = new Set(["frame", "outfit", "prop", "fx", "sound"]);

let items = [];        // /api/lib 的全部（含整套設定、片段）
let kind = "all";      // 目前的分類晶片
let cur = null;        // 編輯中的那一筆（新的還沒存：id 是 null）
let loaded = false;
let busy = false;

// 檔案操作也會先存表單；同一段操作完成前固定編輯對象和欄位。
async function editAction(action) {
  if (busy || !cur) return null;
  const initial = cur;
  busy = true;
  const controls = [...F("asForm").querySelectorAll("input, select, textarea, button"), F("asNew")];
  const disabled = controls.map((e) => e.disabled);
  controls.forEach((e) => { e.disabled = true; });
  try { return await action(); }
  catch (e) { toast(e.message, "err"); return null; }
  finally {
    // 存表單成功、後續上傳失敗時，也要保留已取得的 id 與伺服器清理後的內容。
    if (cur !== initial) {
      fill();
      if (cur) showUnverified(cur.unverified || []);
    }
    controls.forEach((e, i) => { e.disabled = disabled[i]; });
    busy = false;
  }
}

export const fileUrl = (f) => `/api/assets/file/${encodeURIComponent(f)}`;
const thumbUrl = (it) => (it.has_thumb ? `/api/lib/${it.id}/thumb?v=${Math.round(it.updated || 0)}` : "");
const splitList = (s) => s.split(/[,，、\n]/).map((t) => t.trim()).filter(Boolean);

export async function load() {
  try { items = await api("/api/lib"); loaded = true; } catch (e) { return toast("讀不到資產庫：" + e.message, "err"); }
  paint();
}

/** 給其他頁用（抽屜）：只要資產，不要整套設定 */
export async function list() {
  if (!loaded) await load();
  return items;
}

export function show() { load(); }

// ---------------------------------------------------------------- 左邊：分類、搜尋、卡片
function paintKinds() {
  const box = F("asKinds");
  const count = (k) => items.filter((x) => k === "all" || x.kind === k).length;
  box.replaceChildren(...[["all", "全部"], ...KINDS].map(([k, name]) =>
    el("button", { class: "chip" + (kind === k ? " on" : ""), type: "button", role: "tab", "aria-selected": String(kind === k),
      onclick: () => { kind = k; paint(); } }, name, ` ${count(k)}`)));
}

const hay = (it) => [it.name, it.folder, ...(it.tags || []), it.note, it.data?.look_zh, it.data?.look_en, it.data?.text, it.data?.prompt,
  ...(it.data?.tags_anima || [])].join("\n").toLowerCase();

function paint() {
  paintKinds();
  const q = F("asQ").value.trim().toLowerCase();
  const shown = items.filter((x) => (kind === "all" || x.kind === kind) && (!q || hay(x).includes(q)));
  F("asTotal").textContent = shown.length ? String(shown.length) : "";
  const g = F("asGrid");
  g.replaceChildren(...shown.map((it) => el("div", { class: "card pick as-card" + (cur?.id === it.id ? " on" : ""), onclick: () => open(it) },
    el("div", { class: "ph", style: thumbUrl(it) ? `background-image:url("${thumbUrl(it)}")` : undefined }, thumbUrl(it) ? "" : KIND_NAME[it.kind] || it.kind),
    el("div", { class: "bd" },
      el("div", { class: "t" }, (it.fav ? "★ " : "") + it.name),
      el("div", { class: "row wrap as-tags" }, el("span", { class: "badge-s" }, KIND_NAME[it.kind] || it.kind),
        ...(it.tags || []).slice(0, 3).map((t) => el("span", { class: "muted sm" }, "#" + t)))))));
  if (!shown.length) g.append(el("div", { class: "muted sm pad" }, items.length ? "沒有符合的資產" : "還沒有資產：按右上「＋ 新增」"));
}

// ---------------------------------------------------------------- 右邊：編輯區
function open(it) {
  if (busy) return;
  if (!MEDIA.has(it.kind)) return library.edit(it);  // 整套設定、片段：沿用提示詞庫的編輯窗
  cur = structuredClone(it);
  fill();
}

function newOne() {
  if (busy) return;
  if (kind === "set" || kind === "snippet") return library.saveDialog({ kind });
  cur = { id: null, kind: MEDIA.has(kind) ? kind : "char", name: "", folder: "", tags: [], fav: 0, note: "", data: {} };
  fill();
  F("asName").focus();
}

function fill() {
  F("asEmpty").hidden = !!cur;
  F("asForm").hidden = !cur;
  if (!cur) return paint();
  const d = cur.data || {};
  F("asName").value = cur.name || "";
  F("asKind").value = cur.kind;
  F("asFolder").value = cur.folder || "";
  F("asLabels").value = (cur.tags || []).join(", ");
  F("asFav").checked = !!cur.fav;
  F("asLookZh").value = d.look_zh || "";
  F("asLookEn").value = d.look_en || "";
  F("asAnima").value = (d.tags_anima || []).join(", ");
  F("asVoice").value = d.voice || "";
  F("asLang").value = d.lang || "";
  F("asText").value = d.text || "";
  F("asNote").value = cur.note || "";
  for (const [k] of LAYOUT) F("asL_" + k).value = d.layout?.[k] || "";
  F("asUnver").hidden = true;
  F("asDel").hidden = !cur.id;
  paintImgs();
  paintAudio();
  paintLoras();
  paintCard();
  byKind();
  paint();
}

/** 角色卡綁的 LoRA（漫畫台加角色時自動掛上）：名字、強度、觸發詞；存檔時跟著 data 送 */
function paintLoras() {
  const box = F("asLoras");
  if (!box || !cur) return;
  const list = cur.data?.loras || [];
  if (!list.length) return box.replaceChildren(el("div", { class: "muted sm" }, "沒有綁 LoRA"));
  box.replaceChildren(...list.map((l, i) => el("div", { class: "lora-item as-lora", title: l.name },
    el("span", { class: "nm" }, String(l.name).split(/[\\/]/).pop().replace(/\.safetensors$/, "")),
    el("input", { type: "number", min: -2, max: 3, step: 0.05, value: l.strength ?? 0.8, "aria-label": "強度",
      onchange: (e) => { l.strength = Math.min(3, Math.max(-2, +e.target.value || 0)); e.target.value = l.strength; } }),
    el("input", { type: "text", value: l.trigger || "", placeholder: "觸發詞（可空）", "aria-label": "觸發詞", oninput: (e) => { l.trigger = e.target.value; } }),
    el("button", { type: "button", class: "icon", title: "解除綁定", onclick: () => { list.splice(i, 1); paintLoras(); } }, "✕"))));
}

function bindLora(l) {
  if (!cur) return;
  cur.data = cur.data || {};
  const list = (cur.data.loras = cur.data.loras || []);
  if (list.some((x) => x.name === l.name)) return toast("這個 LoRA 已經綁了");
  if (list.length >= 4) return toast("一個角色最多綁 4 個 LoRA", "err");
  list.push({ name: l.name, strength: 0.8, trigger: (l.triggers || []).map((t) => String(t).replace(/[,\s]+$/, "")).filter(Boolean).join(", ") });
  paintLoras();
  toast("綁好了，記得按「存檔」", "ok");
}

/** 分類不同，顯示的欄位不同 */
function byKind() {
  const k = F("asKind").value;
  F("asCharBox").hidden = k !== "char";
  F("asSheetGo").hidden = k !== "char" || !cur?.id;
  F("asSceneBox").hidden = k !== "scene";
  F("asTextBox").hidden = !TEXT_KINDS.has(k);
  F("asShotNote").hidden = k !== "shot";
}

function paintImgs() {
  const box = F("asImgs");
  const imgs = cur.data?.images || [];
  box.replaceChildren(...imgs.map((im) => el("div", { class: "as-img" },
    el("img", { src: fileUrl(im.file), alt: "", loading: "lazy" }),
    el("select", { "aria-label": "用途", onchange: (e) => { im.role = e.target.value; } },
      ...ROLES.map(([r, name]) => el("option", { value: r, selected: im.role === r }, name))),
    el("button", { class: "akey sm", type: "button", title: "拿掉這張（搬到待刪除）", onclick: () => removeFile(im.file) }, "拿掉"))));
  if (!imgs.length) box.append(el("div", { class: "muted sm" }, "還沒有圖片"));
}

/** 角色卡狀態：四樣東西有沒有；設定表鎖住時顯示固定文字（唯讀，要改去工坊解鎖） */
export function cardState(d) {
  const has = (role) => (d?.images || []).some((im) => im.role === role);
  const sp = d?.spec;
  return [
    ["多視圖", has("sheet")], ["正面特寫", has("portrait")],
    ["設定表", sp?.locked ? "locked" : sp && Object.values(sp.fields || {}).some((f) => f?.zh || f?.en) ? "open" : false],
    ["RefMod", !!d?.refmod?.file],
  ];
}

function paintCard() {
  const d = cur.data || {};
  const word = (v) => (v === "locked" ? "🔒 已鎖定" : v === "open" ? "寫了還沒鎖" : v ? "✓ 有" : "— 還沒有");
  const sp = d.spec;
  const canMod = cur.kind === "char" && (d.images || []).length;
  F("asCard").replaceChildren(
    el("div", { class: "row wrap gap as-card-state" }, ...cardState(d).map(([name, v]) => el("span", { class: "chip" + (v ? " on" : "") }, `${name}：${word(v)}`))),
    sp?.locked ? el("div", { class: "cs-spec-block" }, el("div", {}, sp.block_zh), el("div", {}, sp.block_en), el("div", { class: "muted" }, sp.block_tags)) : "",
    canMod ? el("div", { class: "row wrap gap" },
      el("button", { class: "akey sm", type: "button", onclick: makeRefmod,
        title: "用本機 ComfyUI 把正面特寫編成 H3 的 RefMod（只用 H3 影片 VAE，幾十秒）；拍攝單勾「用 RefMod 鎖臉」時，H3 拍片會接上它" },
        d.refmod?.file ? "重做 RefMod" : "做 RefMod"),
      el("span", { class: "muted sm" }, d.refmod?.file ? `已做：${d.refmod.name}` : "RefMod（實驗）：正面特寫編成 H3 的固定參考，拍片鎖臉用")) : "");
}

/** 做 RefMod：本機沒有外掛就問要不要裝（裝完要重開 ComfyUI），有了就做 */
async function makeRefmod() {
  if (!cur?.id) return toast("先存這個角色", "err");
  let st;
  try { st = await api("/api/refmod/status"); } catch (e) { return toast(e.message, "err"); }
  if (!st.online) return toast("本機的 ComfyUI 沒有連線", "err");
  if (!st.loaded) {
    if (!st.installed) {
      if (!confirm(`做 RefMod 要先在本機 ComfyUI 裝外掛 ComfyUI-MiniMaxH3Mod（繪匣審過的版本 ${st.commit.slice(0, 7)}，只用 torch／numpy／Pillow，不連網）。\n要裝嗎？裝完要重開 ComfyUI。`)) return;
      try { await api("/api/refmod/install", { json: {}, timeout: 10 * 60 * 1000 }); } catch (e) { return toast(e.message, "err"); }
    }
    if (!confirm("RefMod 外掛裝好了，要重開 ComfyUI 才會載入。現在重開嗎？（正在跑的工作會中斷）")) return toast("之後重開 ComfyUI，再按一次「做 RefMod」", "ok");
    try { await api("/api/models/restart_comfy", { json: {} }); } catch (e) { return toast(e.message, "err"); }
    return toast("ComfyUI 重開中：等狀態列變綠，再按一次「做 RefMod」", "ok");
  }
  return editAction(async () => {
    const it = await saveCurrent(true);
    if (!it) return;
    toast("做 RefMod 中…（本機 ComfyUI，第一次要載入 H3 影片 VAE）", "ok");
    cur = await api(`/api/assets/${it.id}/refmod`, { json: {}, timeout: 16 * 60 * 1000 });
    const i = items.findIndex((x) => x.id === cur.id);
    if (i >= 0) items[i] = cur;
    toast(`RefMod 做好了：${cur.data?.refmod?.name || ""}`, "ok");
  });
}

function paintAudio() {
  const a = cur.data?.audio?.file;
  F("asAudio").replaceChildren(a ? el("audio", { src: fileUrl(a), controls: true, preload: "none" }) : el("span", { class: "muted sm" }, "沒有音檔"));
  F("asAudioDel").hidden = !a;
}

/** 表單 → 送出的內容（圖片、音檔沿用 cur 裡的，用途可能剛改過） */
function collect() {
  const k = F("asKind").value;
  const d = { ...(cur.data || {}) };
  Object.assign(d, { look_zh: F("asLookZh").value, look_en: F("asLookEn").value, tags_anima: splitList(F("asAnima").value) });
  if (k === "char") Object.assign(d, { voice: F("asVoice").value, lang: F("asLang").value, loras: cur.data?.loras || [] });
  if (k === "scene") d.layout = Object.fromEntries(LAYOUT.map(([key]) => [key, F("asL_" + key).value]));
  if (TEXT_KINDS.has(k)) d.text = F("asText").value;
  return { id: cur.id || undefined, kind: k, name: F("asName").value.trim(), folder: F("asFolder").value.trim(),
    tags: splitList(F("asLabels").value), fav: F("asFav").checked, note: F("asNote").value.trim(), data: d };
}

/** 存檔；quiet＝加檔之前順手存（不跳成功提示）。回傳存好的那筆，失敗回 null */
async function save(quiet = false) {
  return editAction(() => saveCurrent(quiet));
}

async function saveCurrent(quiet = false) {
  const body = collect();
  if (!body.name) { F("asName").focus(); toast("資產要有名字", "err"); return null; }
  try {
    const r = await api("/api/lib", { json: body });
    cur = r;
    const i = items.findIndex((x) => x.id === r.id);
    if (i >= 0) items[i] = r; else items.unshift(r);
    // 寫入時表單仍是同一筆，不重建控制項，避免解除操作中的停用狀態。
    F("asDel").hidden = false;
    paint();
    if (!quiet) toast(`已存好：${r.name}`, "ok");
    return r;
  } catch (e) { toast(e.message, "err"); return null; }
}

function showUnverified(list) {
  const box = F("asUnver");
  box.hidden = !list.length;
  box.replaceChildren(el("span", {}, "這些 tag 在 tag 表裡找不到（照樣存了，確定要用再留）："),
    ...list.map((t) => el("span", { class: "as-bad" }, t)));
}

/** 加一個檔（圖片或音檔）：先存表單（新的才有 id），再請後端複製進資產資料夾 */
async function attach(it, body) {
  const r = await api(`/api/assets/${it.id}/file`, { json: body });
  cur = r;
  const i = items.findIndex((x) => x.id === r.id);
  if (i >= 0) items[i] = r;
}

async function addFile(body, target = cur) {
  if (cur !== target) return;
  return editAction(async () => {
    const it = await saveCurrent(true);
    if (it) await attach(it, body);
  });
}

async function removeFile(file) {
  if (!cur?.id) return;
  return editAction(async () => {
    const it = await saveCurrent(true);
    if (!it) return;
    cur = await api(`/api/assets/${it.id}/file/remove`, { json: { file } });
    const i = items.findIndex((x) => x.id === cur.id);
    if (i >= 0) items[i] = cur;
  });
}

const fileB64 = (file) => new Promise((ok, no) => {
  const fr = new FileReader();
  fr.onload = () => ok(String(fr.result).split(",")[1] || "");
  fr.onerror = () => no(new Error("讀不到這個檔案"));
  fr.readAsDataURL(file);
});

/** 音檔：走 H3 影片台的上傳（會轉成 wav 放進 ComfyUI input），再加進資產 */
function pickAudio() {
  if (busy || !cur) return;
  const target = cur;
  const inp = el("input", { type: "file", accept: "audio/*,.mp3,.wav,.flac,.ogg,.m4a", hidden: true });
  inp.onchange = async () => {
    const f = inp.files[0];
    inp.remove();
    if (!f || cur !== target) return;
    await editAction(async () => {
      const it = await saveCurrent(true);
      if (!it) return;
      const r = await api("/api/h3/media", { json: { file_name: f.name, data_b64: await fileB64(f) }, timeout: 10 * 60 * 1000 });
      if (r.kind !== "audio") throw new Error("這個檔案不是聲音");
      await attach(it, { input: r.name, role: "audio" });
    });
  };
  inp.oncancel = () => inp.remove();
  document.body.append(inp);
  inp.click();
}

async function del() {
  if (busy) return;
  if (!cur?.id || !confirm(`把「${cur.name}」移到待刪除？\n圖片、音檔和資料會搬到資料夾的 待刪除\\資產庫，不會真的刪掉。`)) return;
  return editAction(async () => {
    await api(`/api/lib/${cur.id}`, { method: "DELETE" });
    items = items.filter((x) => x.id !== cur.id);
    cur = null;
    toast("已移到待刪除", "ok");
  });
}

export function init() {
  F("asKind").replaceChildren(...KINDS.filter(([k]) => MEDIA.has(k)).map(([k, name]) => el("option", { value: k }, name)));
  F("asRole").replaceChildren(...ROLES.map(([r, name]) => el("option", { value: r }, "新圖片用途：", name)));
  F("asLayout").replaceChildren(...LAYOUT.map(([k, name]) => el("label", {}, el("span", { class: "sub" }, name), el("input", { type: "text", id: "asL_" + k }))));
  F("asKind").onchange = byKind;
  F("asQ").oninput = paint;
  F("asNew").onclick = newOne;
  F("asSave").onclick = () => save();
  F("asDel").onclick = del;
  F("asLoraAdd").onclick = () => { if (cur) loras.openPicker((l) => bindLora(l), null); };
  F("asImgUp").onclick = async () => {
    if (busy || !cur) return;
    const target = cur, role = F("asRole").value;
    const f = await pickFile();
    if (!f || cur !== target) return;
    await editAction(async () => {
      const it = await saveCurrent(true);
      if (!it) return;
      const r = await uploadFile(f);
      await attach(it, { input: r.name, role });
    });
  };
  // 角色設定圖工坊：做完存回這個角色，再重讀這一筆
  F("asSheetGo").onclick = async () => {
    if (!cur?.id) return;
    const id = cur.id;
    (await import("./charsheet.js")).open({ asset: cur, onSaved: async () => { await load(); const it = items.find((x) => x.id === id); if (it) open(it); } });
  };
  F("asImgGal").onclick = () => {
    if (busy || !cur) return;
    const target = cur, role = F("asRole").value;
    pickFromGallery((rec) => addFile({ gallery_id: rec.id, role }, target));
  };
  F("asAudioUp").onclick = pickAudio;
  // 「存成資產」小視窗
  F("asSaveKind").replaceChildren(...KINDS.filter(([k]) => MEDIA.has(k) && k !== "shot").map(([k, name]) => el("option", { value: k }, name)));
  F("asSaveRole").replaceChildren(...ROLES.map(([r, name]) => el("option", { value: r }, name)));
  $$("input[name=asSaveTo]").forEach((r) => (r.onchange = syncSaveTo));
  F("asAudioDel").onclick = () => { const a = cur?.data?.audio?.file; if (a) removeFile(a); };
  // 提示詞庫的編輯窗存好、刪掉之後（lib-updated），或設定匯入之後（lib-changed），這邊跟著重讀
  for (const ev of ["lib-updated", "lib-changed"]) document.addEventListener(ev, () => { if (loaded) load(); });
}

/** 圖庫、燈箱、生成結果的「存成資產」：小視窗二選一（新資產／加到現有資產），圖由後端從圖庫複製（只收使用者自己選的這張） */
export async function saveFromGallery(rec) {
  await list();
  const media = items.filter((x) => MEDIA.has(x.kind) && x.kind !== "shot");
  F("asSaveTarget").replaceChildren(...media.map((x) => el("option", { value: x.id }, KIND_NAME[x.kind], `・${x.name}`)));
  F("asSaveName").value = "";
  const old = $("input[name=asSaveTo][value=old]");
  old.disabled = !media.length;
  $(`input[name=asSaveTo][value=new]`).checked = true;
  syncSaveTo();
  openModal("#asSaveModal");
  F("asSaveName").focus();
  F("asSaveGo").onclick = async () => {
    if (busy) return;
    const toOld = old.checked;
    const role = F("asSaveRole").value;
    const name = F("asSaveName").value.trim();
    if (!toOld && !name) { F("asSaveName").focus(); return toast("新資產要有名字", "err"); }
    F("asSaveGo").disabled = true;
    try {
      const id = toOld ? +F("asSaveTarget").value : (await api("/api/lib", { json: { kind: F("asSaveKind").value, name, data: {} } })).id;
      const r = await api(`/api/assets/${id}/file`, { json: { gallery_id: rec.id, role } });
      closeModal("#asSaveModal");
      await load();
      // 這個小視窗也可能從別頁開啟；只同步檔案，不蓋掉編輯區未儲存的文字／用途。
      if (!busy && cur?.id === r.id) {
        const roles = new Map((cur.data.images || []).map((im) => [im.file, im.role]));
        cur.data.images = (r.data.images || []).map((im) => ({ ...im, role: roles.get(im.file) || im.role }));
        cur.data.audio = r.data.audio;
        paintImgs(); paintAudio();
      }
      toast(`已存進資產庫：${KIND_NAME[r.kind]}「${r.name}」（${r.data.images?.length || 0} 張圖）`, "ok");
    } catch (e) { toast(e.message, "err"); } finally { F("asSaveGo").disabled = false; }
  };
}

function syncSaveTo() {
  const toOld = $("input[name=asSaveTo][value=old]").checked;
  F("asSaveNew").hidden = toOld;
  F("asSaveOld").hidden = !toOld;
}

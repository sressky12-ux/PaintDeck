// 左邊「API」頁（2026-11-02，設計：design\API總機與指令預設_設計.md；後端 server\api_llm.rs、brains.rs）：
// 大模型連線（多組）、誰用哪顆腦袋、指令預設（學 SillyTavern，可匯入它的預設 JSON）、各種金鑰集中顯示
import { $, el, api, toast } from "./util.js";
import { app } from "./state.js";
import { openPrompt } from "./settings.js";
import { openCloudSettings } from "./agent.js";

let D = null;        // GET /api/llm
let editing = null;  // 正在編的那組預設（整份物件）
const F = (id) => $("#" + id);

// 每個功能的內建指令在設定頁「自訂系統提示詞」的哪一份（空字串＝內建指令是程式依內容產生的，不能改）
export const BUILTIN_ID = { agent: "agent", rewrite: "t2i", translate: "translate", shotsheet: "agent_h3_sheet", h3writer: "agent_h3_base", director: "director_describe", previz: "" };

// ---------------------------------------------------------------- 純函式（tests\llm.test.cjs）

/** 對照表的腦袋選項：照頁面、本機、雲端，再加每一組連線 */
export function brainOptions(profiles) {
  const base = [["page", "照頁面上的選擇"], ["9b", "本機 9B"], ["35b", "本機 35B"], ["cloud", "雲端"]];
  return base.concat((profiles || []).map((p) => [`api:${p.id}`, `API：${p.name}${p.ready ? "" : "（沒填好）"}`]));
}

/** 段落上下移（佔位段也能搬）。回傳新陣列 */
export function moveBlock(blocks, i, dir) {
  const j = i + dir;
  if (j < 0 || j >= blocks.length) return blocks.slice();
  const out = blocks.slice();
  [out[i], out[j]] = [out[j], out[i]];
  return out;
}

/** 刪段落：功能內建指令、請求內容兩個佔位段不能刪 */
export function removeBlock(blocks, i) {
  if (!blocks[i] || blocks[i].kind !== "text") return blocks.slice();
  return blocks.filter((_, k) => k !== i);
}

/** 匯入報告 → 一行一件 */
export function reportLines(report) {
  return (report || []).length ? report.map((r) => "・" + r) : ["・全部照原樣匯入"];
}

// ---------------------------------------------------------------- 畫面

export async function show() {
  try { D = await api("/api/llm"); } catch (e) { toast("讀不到 API 設定：" + e.message, "err"); return; }
  paintProfiles();
  paintRoutes();
  paintPresetList();
  paintKeys();
}

/** 給其他頁做下拉用：[{id, name}] */
export function profiles() { return (D?.profiles || []).map((p) => ({ id: p.id, name: p.name })); }

function visionText(v) { return v === true ? "看得到圖" : v === false ? "看不到圖（會改用反推）" : "還沒測看不看得到圖"; }

function paintProfiles() {
  const box = F("llmProfiles");
  box.replaceChildren();
  if (!D.profiles.length) box.append(el("div", { class: "muted sm" }, "還沒有任何連線。按「＋新增連線」，選服務商、貼金鑰、填模型名稱。"));
  for (const p of D.profiles) {
    box.append(el("div", { class: "llm-card" },
      el("div", { class: "llm-card-h" }, el("b", {}, p.name), p.id === D.default ? el("span", { class: "llm-tag" }, "預設") : "",
        el("span", { class: "muted sm" }, providerName(p.provider))),
      el("div", { class: "muted sm" }, `${p.model || "（還沒填模型）"}・${p.has_key ? "金鑰已設定" : "沒有金鑰"}・${visionText(p.vision)}`),
      el("div", { class: "row gap wrap" },
        el("button", { class: "akey sm", type: "button", onclick: () => editProfile(p) }, "編輯"),
        el("button", { class: "akey sm", type: "button", onclick: () => testProfile(p.id) }, "測試"),
        el("button", { class: "akey sm warn", type: "button", onclick: () => delProfile(p) }, "刪除"))));
  }
}

function providerName(id) { return (D.providers.find((x) => x.id === id) || {}).name || "自訂"; }

function editProfile(p) {
  const f = F("llmForm");
  f.hidden = false;
  F("lfId").value = p?.id || "";
  const sel = F("lfProvider");
  sel.replaceChildren(...D.providers.map((x) => el("option", { value: x.id }, x.name)));
  sel.value = p?.provider || "deepseek";
  F("lfName").value = p?.name || "";
  F("lfUrl").value = p?.url ?? (D.providers.find((x) => x.id === sel.value)?.url || "");
  F("lfModel").value = p?.model || "";
  F("lfKey").value = "";
  F("lfKey").placeholder = p?.has_key ? "已設定（要換才貼新的；清空按右邊）" : "貼上金鑰（加密存在這台電腦）";
  F("lfMsg").textContent = "";
  F("lfModels").replaceChildren();
  f.scrollIntoView({ block: "nearest" });
}

async function saveProfile(extra = {}) {
  const body = { id: F("lfId").value, provider: F("lfProvider").value, name: F("lfName").value.trim() || providerName(F("lfProvider").value),
    url: F("lfUrl").value.trim(), model: F("lfModel").value.trim(), ...extra };
  if (F("lfKey").value.trim()) body.key = F("lfKey").value.trim();
  const r = await api("/api/llm/profile", { json: body });
  F("lfId").value = r.id;
  F("lfKey").value = "";
  await show();
  return r;
}

async function testProfile(id) {
  toast("測試中…（會花一點點 token）");
  try { const r = await api("/api/llm/profile/test", { json: { id }, timeout: 150000 }); toast(r.msg, "ok"); await show(); }
  catch (e) { toast("測試失敗：" + e.message, "err"); }
}

async function delProfile(p) {
  if (!confirm(`刪掉連線「${p.name}」？金鑰也會一起清掉。`)) return;
  try { await api("/api/llm/profile/delete", { json: { id: p.id } }); await show(); } catch (e) { toast(e.message, "err"); }
}

function paintRoutes() {
  const tb = F("llmRoutes");
  tb.replaceChildren();
  const opts = brainOptions(D.profiles);
  for (const f of D.features) {
    const r = D.routes[f.id] || { brain: "page", preset: null };
    const b = el("select", { class: "sm", "aria-label": `${f.name}用哪顆腦袋` }, ...opts.map(([v, t]) => el("option", { value: v }, t)));
    b.value = opts.some((o) => o[0] === r.brain) ? r.brain : "page";
    const p = el("select", { class: "sm", "aria-label": `${f.name}套哪組指令預設` }, el("option", { value: "" }, "不套"), ...D.presets.map((x) => el("option", { value: x.id }, x.name)));
    p.value = r.preset || "";
    const save = async () => {
      try { D.routes = await api("/api/llm/routes", { json: { [f.id]: { brain: b.value, preset: p.value } } }); toast(`${f.name}：已存`, "ok"); }
      catch (e) { toast(e.message, "err"); }
    };
    b.onchange = save; p.onchange = save;
    const pid = BUILTIN_ID[f.id];
    const link = pid
      ? el("button", { class: "akey sm", type: "button", title: "打開設定頁的「自訂系統提示詞」，改這個功能自己的指令（改壞了可以還原）", onclick: () => { app.showPage("settings"); openPrompt(pid); } }, "改內建指令")
      : el("span", { class: "muted sm", title: "這個功能的內建指令是程式依內容產生的，只能在指令預設裡前後加段" }, "程式產生");
    tb.append(el("tr", {}, el("td", {}, f.name), el("td", {}, b), el("td", {}, p), el("td", {}, link)));
  }
}

// ---------------------------------------------------------------- 指令預設

function paintPresetList() {
  const sel = F("lpSel");
  const keep = editing?.id || sel.value;
  sel.replaceChildren(el("option", { value: "" }, D.presets.length ? "（選一組來編）" : "（還沒有預設）"), ...D.presets.map((x) => el("option", { value: x.id }, x.name)));
  if (D.presets.some((x) => x.id === keep)) sel.value = keep;
  if (!sel.value) { editing = null; paintEditor(); }
}

async function loadPreset(id) {
  if (!id) { editing = null; paintEditor(); return; }
  try { editing = await api("/api/llm/preset/get", { json: { id } }); paintEditor(); } catch (e) { toast(e.message, "err"); }
}

function newPreset() {
  editing = { id: "", name: "新的預設", blocks: [{ id: "builtin", name: "功能內建指令", role: "system", content: "", enabled: true, kind: "builtin" }, { id: "request", name: "功能的請求內容", role: "user", content: "", enabled: true, kind: "request" }],
    params: {}, prefill: "", squash_system: false, user_name: "User", char_name: "Assistant" };
  F("lpSel").value = "";
  paintEditor();
}

const PARAMS = [["temperature", "溫度", "0.7"], ["top_p", "top_p", "0.95"], ["top_k", "top_k（只有本機）", "40"], ["frequency_penalty", "頻率懲罰", "0"], ["presence_penalty", "存在懲罰", "0"], ["max_tokens", "最多回幾個字（token）", "4096"]];

function paintEditor() {
  const box = F("lpEditor");
  box.hidden = !editing;
  if (!editing) return;
  F("lpName").value = editing.name;
  F("lpPrefill").value = editing.prefill || "";
  F("lpSquash").checked = !!editing.squash_system;
  F("lpUser").value = editing.user_name || "";
  F("lpChar").value = editing.char_name || "";
  const pbox = F("lpParams");
  pbox.replaceChildren(...PARAMS.map(([k, name, ph]) => el("label", { class: "llm-param" }, name,
    el("input", { type: "number", step: "any", class: "sm", placeholder: `照功能（例 ${ph}）`, value: editing.params?.[k] ?? "", oninput: (e) => {
      const v = e.target.value.trim();
      editing.params = { ...editing.params, [k]: v === "" ? null : Number(v) };
    } }))));
  paintBlocks();
}

function paintBlocks() {
  const list = F("lpBlocks");
  list.replaceChildren();
  editing.blocks.forEach((b, i) => {
    const marker = b.kind !== "text";
    const head = el("div", { class: "row gap wrap" },
      el("input", { type: "checkbox", checked: b.enabled !== false, disabled: marker, title: marker ? "這一段一定會送" : "關掉就不送", onchange: (e) => { b.enabled = e.target.checked; } }),
      marker ? el("b", {}, b.kind === "builtin" ? "〔功能內建指令〕" : "〔功能的請求內容〕")
        : el("input", { class: "sm", value: b.name || "", placeholder: "段落名稱", oninput: (e) => { b.name = e.target.value; } }),
      marker ? el("span", { class: "muted sm" }, b.kind === "builtin" ? "每個功能自己的系統指令會放在這裡" : "功能本來要送的對話或請求會放在這裡")
        : el("select", { class: "sm", "aria-label": "角色", onchange: (e) => { b.role = e.target.value; } }, ...["system", "user", "assistant"].map((r) => el("option", { value: r, selected: b.role === r }, r))),
      el("span", { class: "sp" }),
      el("button", { class: "akey sm", type: "button", title: "往上移", onclick: () => { editing.blocks = moveBlock(editing.blocks, i, -1); paintBlocks(); } }, "↑"),
      el("button", { class: "akey sm", type: "button", title: "往下移", onclick: () => { editing.blocks = moveBlock(editing.blocks, i, 1); paintBlocks(); } }, "↓"),
      marker ? "" : el("button", { class: "akey sm warn", type: "button", onclick: () => { editing.blocks = removeBlock(editing.blocks, i); paintBlocks(); } }, "刪"));
    const body = marker ? "" : el("textarea", { rows: 3, spellcheck: "false", value: b.content || "", oninput: (e) => { b.content = e.target.value; } });
    if (body) body.value = b.content || "";
    list.append(el("div", { class: "llm-block" + (marker ? " marker" : "") }, head, body));
  });
}

async function savePreset() {
  if (!editing) return;
  editing.name = F("lpName").value.trim() || "未命名預設";
  editing.prefill = F("lpPrefill").value;
  editing.squash_system = F("lpSquash").checked;
  editing.user_name = F("lpUser").value;
  editing.char_name = F("lpChar").value;
  try { editing = await api("/api/llm/preset", { json: editing }); toast("預設存好了", "ok"); await show(); F("lpSel").value = editing.id; }
  catch (e) { toast("存不了：" + e.message, "err"); }
}

async function delPreset() {
  if (!editing?.id) { editing = null; paintEditor(); return; }
  if (!confirm(`刪掉預設「${editing.name}」？對照表用到它的功能會變成不套預設。`)) return;
  try { await api("/api/llm/preset/delete", { json: { id: editing.id } }); editing = null; await show(); } catch (e) { toast(e.message, "err"); }
}

async function importPreset(file) {
  if (!file) return;
  try {
    const text = await file.text();
    const r = await api("/api/llm/preset/import", { json: { json: text, name: file.name } });
    editing = r.preset;
    await show();
    F("lpSel").value = editing.id;
    paintEditor();
    F("lpReport").hidden = false;
    F("lpReport").replaceChildren(el("b", {}, `匯入「${editing.name}」：`), ...reportLines(r.report).map((t) => el("div", {}, t)));
  } catch (e) { toast("匯入失敗：" + e.message, "err"); }
}

// ---------------------------------------------------------------- 其他金鑰（RunPod、雲端腦袋、RunningHub 在原本的設定視窗改，這裡只看狀態）

async function paintKeys() {
  const set = (id, ok, text) => { F(id).textContent = text; F(id).classList.toggle("ok", ok); };
  try {
    const c = (await api("/api/agent/cloud")).cfg || {};
    set("lkRunpod", !!(c.has_runpod_key || c.runpod_env), c.has_runpod_key ? "已設定" : c.runpod_env ? "用環境變數 RUNPOD_API_KEY" : "還沒設定");
    set("lkCloudLlm", !!c.has_llm_key, c.has_llm_key ? "已設定" : "還沒設定（自己架的不用）");
  } catch { set("lkRunpod", false, "讀不到"); }
  try { const r = await api("/api/runninghub"); set("lkRh", !!r.has_key, r.has_key ? "已設定" : "還沒設定"); } catch { set("lkRh", false, "讀不到"); }
}

export function init() {
  F("llmAdd").onclick = () => editProfile(null);
  F("lfProvider").onchange = () => { const p = D.providers.find((x) => x.id === F("lfProvider").value); if (p && p.url) F("lfUrl").value = p.url; if (!F("lfName").value.trim()) F("lfName").value = p?.name || ""; };
  F("lfSave").onclick = async () => { try { await saveProfile(); F("lfMsg").textContent = "存好了"; } catch (e) { F("lfMsg").textContent = e.message; } };
  F("lfTest").onclick = async () => { try { const r = await saveProfile(); await testProfile(r.id); } catch (e) { F("lfMsg").textContent = e.message; } };
  F("lfClearKey").onclick = async () => { if (!F("lfId").value || !confirm("清掉這組的金鑰？")) return; try { await saveProfile({ key: "" }); F("lfMsg").textContent = "金鑰清掉了"; } catch (e) { F("lfMsg").textContent = e.message; } };
  F("lfFetch").onclick = async () => {
    try {
      const r = await saveProfile();
      const m = await api("/api/llm/profile/models", { json: { id: r.id }, timeout: 40000 });
      F("lfModels").replaceChildren(...m.models.map((x) => el("option", { value: x })));
      F("lfMsg").textContent = m.models.length ? `抓到 ${m.models.length} 個模型：點模型欄就能挑` : "這個服務沒回任何模型，請自己打模型名稱";
    } catch (e) { F("lfMsg").textContent = e.message; }
  };
  F("lfClose").onclick = () => { F("llmForm").hidden = true; };
  F("lpSel").onchange = () => loadPreset(F("lpSel").value);
  F("lpNew").onclick = newPreset;
  F("lpAddBlock").onclick = () => { if (!editing) return; editing.blocks.push({ id: "", name: "新段落", role: "system", content: "", enabled: true, kind: "text" }); paintBlocks(); };
  F("lpSave").onclick = savePreset;
  F("lpDelete").onclick = delPreset;
  F("lpImport").onclick = () => F("lpFile").click();
  F("lpFile").onchange = (e) => { importPreset(e.target.files[0]); e.target.value = ""; };
  F("lkCloudGo").onclick = () => { app.showPage("agent"); openCloudSettings(); };
  F("lkRhGo").onclick = () => { app.showPage("director"); toast("在導演頁「H3 影片」分頁，引擎選 RunningHub 就看得到 API Key 欄"); };
}

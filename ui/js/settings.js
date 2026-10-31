// 設定：Civitai API key、成人預覽模糊、預設負面提示詞、AI 改寫、影片模型、模型與工作流（models.js）、匯出／匯入、恢復原廠（可救回）
import { $, el, api, toast } from "./util.js";
import { app } from "./state.js";
import * as i18n from "./i18n.js";
import { resetDock } from "./dock.js";
import { H3_NAME } from "./gen.js";
import * as models from "./models.js";
import { mountNet } from "./net.js";
import { mountComfyOpts } from "./comfyopts.js";
import { mountSkills } from "./skills.js";
import { applyStrip } from "./feature.js";
import { openAi } from "./studio.js";
import * as vidmap from "./vidmap.js";
import * as updates from "./updates.js";
import * as cache from "./cache.js";
import * as diagnostics from "./diagnostics.js";

// 恢復原廠預設勾選：圖庫、上傳暫存、預設組（LoRA 備註和設定預設不勾，免得連 Civitai 存好的觸發詞也清掉）
const DEFAULT_PARTS = ["gallery", "uploads", "presets"];
// 這個瀏覽器記住的東西（localStorage 前綴 aw_）
const BROWSER_KEYS = ["form", "pe", "recent", "snippets", "snippetsMoved", "libView", "theme", "galFilters", "notify"];  // 介面語言（lang）恢復原廠也保留

export function init() {
  updates.init();
  cache.init();
  diagnostics.init();
  $("#setAiOpen").onclick = openAi;
  models.init();
  // 介面語言（i18n.js）：存起來、重新整理頁面
  $("#setLang").value = i18n.current();
  $("#setLang").onchange = () => i18n.choose($("#setLang").value);
  // 自由版面（dock.js）：每頁的面板位置、大小、浮動、隱藏全部恢復預設
  $("#dockReset").onclick = () => {
    if (!confirm("把所有頁面的面板位置、大小都恢復成預設？")) return;
    resetDock();
    toast("版面都恢復成預設了", "ok");
  };
  // 儲存失敗要講出來（不然按了沒反應，使用者以為存好了）
  $("#setKeySave").onclick = async () => {
    try {
      await api("/api/settings", { json: { civitai_key: $("#setKey").value } });
      $("#setKey").value = "";
      await app.refreshState();
      toast("已儲存", "ok");
    } catch (e) { toast("儲存失敗：" + e.message, "err"); }
  };
  // 魔塔金鑰（下載中心，server\api_hub.rs）：只回有沒有填，不回內容
  const msSave = async (key) => {
    try {
      const r = await api("/api/hub/modelscope", { json: { key } });
      $("#setMsKey").value = "";
      paintMs(r.has_key);
      toast(r.has_key ? "魔塔金鑰存好了" : "魔塔金鑰清掉了", "ok");
    } catch (e) { toast("儲存失敗：" + e.message, "err"); }
  };
  $("#setMsSave").onclick = () => { const k = $("#setMsKey").value.trim(); if (!k) return toast("先貼上 Access Token", "err"); msSave(k); };
  $("#setMsClear").onclick = () => { if (confirm("清掉魔塔金鑰？")) msSave(""); };
  $("#setMsTest").onclick = async () => {
    $("#setMsState").textContent = "測試中…";
    try { await api("/api/hub/modelscope/test", { json: {}, timeout: 60000 }); toast("魔塔登入成功", "ok"); $("#setMsState").textContent = "目前狀態：已設定，登入成功"; }
    catch (e) { $("#setMsState").textContent = "測試失敗：" + e.message; }
  };
  $("#setBlur").onchange = async () => {
    try { await api("/api/settings", { json: { blur_nsfw: $("#setBlur").checked } }); await app.refreshState(); }
    catch (e) { $("#setBlur").checked = !$("#setBlur").checked; toast("儲存失敗：" + e.message, "err"); }
  };
  $("#setNegSave").onclick = async () => {
    try { await api("/api/settings", { json: { negative: $("#setNeg").value } }); await app.refreshState(); toast("已儲存", "ok"); }
    catch (e) { toast("儲存失敗：" + e.message, "err"); }
  };

  $("#setPeMode").onchange = () => ($("#setPeLayersRow").hidden = $("#setPeMode").value !== "partial");
  $("#setPeSave").onclick = async () => {
    const idle = +$("#setPeIdle").value, temp = +$("#setPeTemp").value, layers = +$("#setPeLayers").value;
    if (!(idle >= 10 && idle <= 3600)) return toast("閒置秒數請填 10～3600", "err");
    if (!(temp >= 0.1 && temp <= 1.5)) return toast("隨機程度請填 0.1～1.5", "err");
    if ($("#setPeMode").value === "partial" && !(layers >= 1 && layers <= 40)) return toast("層數請填 1～40", "err");
    try {
      await api("/api/settings", { json: { pe_mode: $("#setPeMode").value, pe_general: $("#setPeGeneral").value, pe_partial_layers: layers || 16, pe_idle: idle, pe_temperature: temp,
        pe_close_after: $("#setPeClose").checked, pe_zh: $("#setPeZh").checked, pe_think: $("#setPeThink").checked } });
      await app.refreshState();
      // 「預設開啟中文對照／深度思考」改了：AI 改寫面板的兩個勾選框跟著換（面板自己會記住）
      for (const [id, on] of [["peZhOn", $("#setPeZh").checked], ["peThinkOn", $("#setPeThink").checked]]) {
        const box = $("#" + id);
        if (box && box.checked !== on) { box.checked = on; box.dispatchEvent(new Event("change", { bubbles: true })); }
      }
      toast("已儲存（下次改寫生效；換執行方式會重新載入模型）", "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  $("#setPeStop").onclick = async () => {
    try { await api("/api/pe/stop", { json: {} }); toast("已關閉改寫模型", "ok"); } catch (e) { toast(e.message, "err"); }
  };

  // 自訂 AI 改寫的系統提示詞（存到 data\pe_prompts，空白或跟預設一樣＝還原預設）
  $("#setPrSel").onchange = showPrompt;
  $("#setPrText").oninput = countPrompt;
  $("#setPrSave").onclick = () => savePrompt($("#setPrText").value);
  $("#setPrReset").onclick = () => { if (confirm("還原成預設的系統提示詞？自訂的內容會刪掉。")) savePrompt(""); };
  loadPrompts();

  // 影片模型：內建 MiniMax H3，或匯入的 ComfyUI 工作流（選了就存）
  $("#setVidEngine").onchange = async () => {
    const id = $("#setVidEngine").value;
    showVideo();
    try {
      await api("/api/settings", { json: { video_engine: id } });
      VID.engine = id;
      await app.refreshState();
      toast("影片模型已切換", "ok");
    } catch (e) { $("#setVidEngine").value = VID.engine; showVideo(); toast("切換失敗：" + e.message, "err"); }
  };
  $("#setVidImport").onclick = () => $("#setVidFile").click();
  $("#setVidFile").onchange = importVideo;
  $("#setVidMap").onclick = () => {
    const id = $("#setVidEngine").value;
    vidmap.open(id, () => loadVideo());
  };
  vidmap.init();
  $("#setVidDel").onclick = async () => {
    const w = VID?.workflows.find((x) => x.id === $("#setVidEngine").value);
    if (!w || !confirm(`刪掉影片工作流「${w.name}」？影片會換回內建的 MiniMax H3。（你電腦上原本的 JSON 檔不受影響）`)) return;
    try {
      await api(`/api/video/workflows/${encodeURIComponent(w.id)}`, { method: "DELETE" });
      await loadVideo();
      await app.refreshState();
      toast("已刪除", "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  loadVideo();

  // 匯入設定
  $("#setImport").onclick = () => $("#setImportFile").click();
  $("#setImportFile").onchange = async () => {
    const f = $("#setImportFile").files[0];
    $("#setImportFile").value = "";
    if (!f) return;
    try {
      const data = JSON.parse(await f.text());
      const r = await api("/api/settings/import", { json: data });
      await app.refreshState();
      const parts = [`提示詞庫 ${r.prompt_lib || r.presets} 筆`, `${r.lora_meta} 個 LoRA 備註`];
      if (r.image_workflows) parts.push(`生圖工作流 ${r.image_workflows} 組`);
      if (r.video_workflows) parts.push(`影片工作流 ${r.video_workflows} 個`);
      if (r.pe_prompts) parts.push(`AI 改寫提示詞 ${r.pe_prompts} 個`);
      let msg = "已匯入：設定、" + parts.join("、");
      if (r.backup_dir) msg += `\n同名但內容不同的舊檔搬到資料資料夾的 ${r.backup_dir}`;
      const w = r.warnings || [];
      if (w.length) msg += "\n這幾項沒匯入：\n・" + w.slice(0, 6).join("\n・") + (w.length > 6 ? `\n…還有 ${w.length - 6} 項` : "");
      toast(msg, w.length ? "err" : "ok");
      document.dispatchEvent(new Event("lib-changed"));
      loadVideo();
    } catch (e) { toast("匯入失敗：" + (e instanceof SyntaxError ? "檔案不是 JSON" : e.message), "err"); }
  };

  $("#resetGo").onclick = doReset;
}

// ---------------------------------------------------------------- 影片模型
let VID = null;
async function loadVideo() {
  try { VID = await api("/api/video/workflows"); } catch (e) { $("#setVidInfo").textContent = "讀不到：" + e.message; return; }
  const sel = $("#setVidEngine");
  sel.replaceChildren(el("option", { value: VID.builtin.id }, H3_NAME),
    ...VID.workflows.map((w) => el("option", { value: w.id }, w.name + (w.error ? "（讀不懂）" : ""))));
  // 設定裡的工作流檔不見了：列出來讓人知道，生成時會報錯
  if (![VID.builtin.id, ...VID.workflows.map((w) => w.id)].includes(VID.engine)) sel.append(el("option", { value: VID.engine }, VID.engine + "（找不到）"));
  sel.value = VID.engine;
  showVideo();
}

/** 選到的影片模型的說明：內建的寫規格，自訂的寫生圖站看懂了什麼 */
function showVideo() {
  const id = $("#setVidEngine").value;
  const w = VID?.workflows.find((x) => x.id === id);
  const box = $("#setVidInfo");
  $("#setVidDel").hidden = !w;
  $("#setVidMap").hidden = !w;
  if (id === VID?.builtin.id) {
    box.textContent = "內建：MiniMax H3（10Eros 無審核版），1～15 秒、每秒 24 格、有聲音；「動起來」和導演台都用它，快速＝480p、高畫質＝720p。本機要到安裝精靈勾「H3 影片（本機，選配）」：8GB 顯卡用 w4a8 版跑得動（512×768 5 秒約 3～4 分鐘）；也可以開雲端跑。更多玩法（首尾幀、多素材參考、換角色、特效…）在「導演 → H3 影片」。";
    return;
  }
  if (!w) { box.textContent = "找不到這個工作流的檔案：重新匯入，或換一個。"; return; }
  if (w.error) { box.textContent = "這個工作流讀不懂：" + w.error + "（按「對應與參數…」自己指定）"; return; }
  const i = w.info, node = (n) => `「${n.title}」（#${n.node}）`;
  const lines = [
    i.placeholder ? `提示詞：放進 ${i.prompt.map(node).join("、")} 的 {{prompt}}` : `提示詞：整格換掉 ${i.prompt.map(node).join("、")}`,
    i.image ? `首格圖：放進 ${node(i.image)}` : "首格圖：沒有 Load Image（只能文生影片，不能用在「動起來」和導演台）",
    i.seeds ? `種子：${i.seeds} 個欄位` : "種子：找不到種子欄位（每次結果都一樣）",
    i.sizes ? "長寬：跟著首格圖的比例調" : "長寬：照工作流",
    i.seconds ? `長度：可以選秒數（每秒 ${i.fps} 格）` : "長度：照工作流",
    `存檔：${i.outputs.map(node).join("、")}`,
  ];
  if (w.pins?.length) lines.push(`「動起來」可調：${w.pins.map((p) => p.label).join("、")}`);
  if (w.mapped) lines.push("有自己指定的對應與參數（按「對應與參數…」看）");
  box.replaceChildren(...lines.map((s) => el("div", {}, s)));
}

// 檔案 → base64（跟生圖工作流一樣交給後端轉：API／一般「儲存」的 JSON、ComfyUI 生成的 PNG／WebP 都收）
const fileB64 = (file) => new Promise((ok, no) => {
  const fr = new FileReader();
  fr.onload = () => ok(String(fr.result).split(",")[1] || "");
  fr.onerror = () => no(new Error("讀不到這個檔案"));
  fr.readAsDataURL(file);
});

async function importVideo() {
  const f = $("#setVidFile").files[0];
  $("#setVidFile").value = "";
  if (!f) return;
  const name = $("#setVidName").value.trim() || f.name.replace(/\.(json|png|webp)$/i, "");
  if (VID?.workflows.some((w) => w.name === name) && !confirm(`已經有叫「${name}」的工作流，要換成這個新的嗎？`)) return;
  const btn = $("#setVidImport");
  btn.disabled = true;
  try {
    const r = await api("/api/video/workflows", { json: { name, file_name: f.name, data_b64: await fileB64(f) }, timeout: 5 * 60 * 1000 });
    $("#setVidName").value = "";
    const warns = r.warnings || [];
    const use = confirm(`已匯入「${r.workflow.name}」。${warns.length ? `\n\n轉換時的提醒：\n・${warns.slice(0, 6).join("\n・")}\n\n` : ""}現在就改用它來做影片（「動起來」和導演台）嗎？`);
    if (use) await api("/api/settings", { json: { video_engine: r.workflow.id } });
    await loadVideo();
    await app.refreshState();
    toast(use ? `已匯入，影片改用「${r.workflow.name}」` : `已匯入「${r.workflow.name}」，影片還是用原本的`, "ok");
  } catch (e) { toast("匯入失敗：" + e.message, "err"); }
  finally { btn.disabled = false; }
}

// ---------------------------------------------------------------- 恢復原廠
let resetInfo = null;

let PROMPTS = [];
async function loadPrompts(keep) {
  try { PROMPTS = (await api("/api/pe/prompts")).prompts || []; } catch (e) { $("#setPrDesc").textContent = "讀不到：" + e.message; return; }
  const sel = $("#setPrSel"), want = keep || sel.value;
  sel.innerHTML = "";
  for (const p of PROMPTS) sel.append(el("option", { value: p.id }, (p.custom ? "✎ " : "") + p.name));
  if (PROMPTS.some((p) => p.id === want)) sel.value = want;
  showPrompt();
}
function showPrompt() {
  const p = PROMPTS.find((x) => x.id === $("#setPrSel").value);
  if (!p) return;
  $("#setPrDesc").textContent = p.desc;
  $("#setPrText").value = p.text;
  $("#setPrState").textContent = p.custom ? "已自訂" : "預設";
  $("#setPrState").classList.toggle("ok", p.custom);
  $("#setPrReset").disabled = !p.custom;
  countPrompt();
}
function countPrompt() {
  const p = PROMPTS.find((x) => x.id === $("#setPrSel").value);
  const t = $("#setPrText").value;
  const miss = (p?.keep || []).filter((k) => !t.includes(k));
  $("#setPrCount").textContent = `${t.length} 字` + (p && t !== p.text ? "・還沒儲存" : "") + (miss.length ? `・缺少 ${miss.join("、")}` : "");
}
async function savePrompt(text) {
  const id = $("#setPrSel").value;
  try {
    const r = await api("/api/pe/prompts", { json: { id, text } });
    toast(r.custom ? "已儲存，下次改寫就用新的（快取會分開，不會拿到舊結果）" : "已還原預設", "ok");
    await loadPrompts(id);
  } catch (e) { toast(e.message, "err"); }
}

async function loadReset() {
  try { resetInfo = await api("/api/reset"); } catch (e) { $("#resetHistory").textContent = "讀不到：" + e.message; return; }
  const box = $("#resetParts");
  if (!box.childElementCount) {
    for (const [k, label] of Object.entries(resetInfo.parts)) {
      box.append(el("label", { class: "check" }, el("input", { type: "checkbox", value: k, checked: DEFAULT_PARTS.includes(k) }), label));
    }
  }
  const h = $("#resetHistory");
  h.innerHTML = "";
  if (!resetInfo.history.length) h.append(el("span", { class: "muted sm" }, "還沒有做過恢復原廠。"));
  for (const r of resetInfo.history) {
    const names = r.parts.map((p) => (resetInfo.parts[p] || p).split("：")[0].split("（")[0]).join("、");
    h.append(el("div", { class: "reset-item" },
      el("div", {}, el("b", {}, new Date(r.time * 1000).toLocaleString(document.documentElement.lang)), el("div", { class: "muted sm" }, `${names}・搬走 ${r.files} 項・資料夾 待刪除\\${r.name}`)),
      r.restored ? el("span", { class: "badge ok" }, "已救回") : el("button", { class: "btn sm", onclick: () => undoReset(r) }, "救回")));
  }
}

async function doReset() {
  const parts = [...document.querySelectorAll("#resetParts input:checked")].map((x) => x.value);
  const browser = $("#resetBrowser").checked;
  if (!parts.length && !browser) return toast("先勾選要清掉哪些部分", "err");
  // 換了介面語言，畫面要人打的是翻過的字（Factory Reset／恢复原厂），兩種都收；送給伺服器的一律是「恢復原廠」
  const typed = $("#resetConfirm").value.trim().toLowerCase();
  if (typed !== "恢復原廠" && typed !== i18n.tr("恢復原廠").toLowerCase()) { $("#resetConfirm").focus(); return toast("請在框裡輸入「恢復原廠」四個字", "err"); }
  const list = parts.map((p) => "・" + resetInfo.parts[p]).concat(browser ? ["・這個瀏覽器記住的東西"] : []).join("\n");
  if (!confirm(`確定恢復原廠？會清掉：\n${list}\n\n伺服器上的舊資料會搬到資料資料夾的「待刪除」，可以救回。`)) return;
  $("#resetGo").disabled = true;
  try {
    if (parts.length) {
      const r = await api("/api/reset", { json: { parts, confirm: "恢復原廠" } });
      toast(`已恢復原廠（搬走 ${r.moved} 項到 待刪除\\${r.name}）`, "ok");
    }
    $("#resetConfirm").value = "";
    if (browser) {
      for (const k of BROWSER_KEYS) localStorage.removeItem("aw_" + k);
      toast("網頁記住的東西也清掉了，馬上重新載入", "ok");
      setTimeout(() => location.reload(), 1200);
      return;
    }
    await app.refreshState();
    loadReset();
    (await import("./gallery.js")).invalidate();
    document.dispatchEvent(new Event("lib-changed"));
    if (parts.includes("gallery")) { const g = await import("./gen.js"); g.forget(g.S.results.map((r) => r.id)); }
  } catch (e) { toast(e.message, "err"); }
  finally { $("#resetGo").disabled = false; }
}

async function undoReset(r) {
  if (!confirm(`救回 ${new Date(r.time * 1000).toLocaleString(document.documentElement.lang)} 那次清掉的東西？\n檔案搬回原位（撞名的會自動改名），資料表的紀錄加回去，現在的資料不會被蓋掉。`)) return;
  try {
    const x = await api("/api/reset/undo", { json: { name: r.name } });
    toast(`已救回（${x.files} 項${x.renamed ? `，${x.renamed} 個檔案因為撞名改了名字` : ""}${x.missing ? `，${x.missing} 項在待刪除裡找不到` : ""}）`, "ok");
    await app.refreshState();
    loadReset();
    (await import("./gallery.js")).invalidate();
    document.dispatchEvent(new Event("lib-changed"));
  } catch (e) { toast(e.message, "err"); }
}

// 「通用模型」選單：自動／35B／9B 之外，列出 LLM 資料夾裡每一顆能對話的模型讓人自己指定（2026-10-29 使用者：「讓別人自己決定要用什麼模型」）。
// 寫明能不能看圖、是不是 Qwen 系列（別家不保證寫得出生圖站要的格式）；指定的檔不見了，後端照自動挑
function fillGeneral(list, cur) {
  const sel = $("#setPeGeneral");
  const key = JSON.stringify([list, cur]);
  if (sel.dataset.key === key) return;
  sel.dataset.key = key;
  sel.querySelector("optgroup")?.remove();
  if (list.length || !["auto", "35b", "9b"].includes(cur)) {
    const g = el("optgroup", { label: "指定這一顆（LLM 資料夾裡的模型）" });
    for (const m of list) {
      const gb = (m.size / 1e9).toFixed(1), n = m.name;
      // 整句寫死（介面翻譯照整句比對，拼起來的翻不到）
      const label = m.qwen
        ? (m.vision ? `${n}（${gb}GB，能看圖）` : `${n}（${gb}GB，沒有看圖檔：助手看不了圖）`)
        : (m.vision ? `${n}（${gb}GB，能看圖；非 Qwen 系列，不保證寫得出生圖站要的格式）` : `${n}（${gb}GB，沒有看圖檔；非 Qwen 系列，不保證寫得出生圖站要的格式）`);
      g.append(el("option", { value: n }, label));
    }
    if (!["auto", "35b", "9b"].includes(cur) && !list.some((m) => m.name === cur)) g.append(el("option", { value: cur }, `${cur}（找不到這個檔，現在照自動挑）`));
    sel.append(g);
  }
  sel.value = cur;
}

export function onState(st) {
  const s = st.settings || {};
  $("#setKeyState").textContent = s.has_civitai_key ? "目前狀態：已設定 API key（要更換就貼上新的再按儲存，留空儲存＝清除）" : "目前狀態：未設定";
  $("#setBlur").checked = !!s.blur_nsfw;
  if (document.activeElement !== $("#setNeg")) $("#setNeg").value = s.negative || "";
  if (!$("#setPe").contains(document.activeElement)) {
    $("#setPeMode").value = s.pe_mode || "gpu";
    fillGeneral(st.pe?.general_models || [], s.pe_general || "auto");
    $("#setPeLayers").value = s.pe_partial_layers ?? 16;
    $("#setPeIdle").value = s.pe_idle ?? 120;
    $("#setPeTemp").value = s.pe_temperature ?? 1;
    $("#setPeClose").checked = !!s.pe_close_after;
    $("#setPeZh").checked = !!s.pe_zh;
    $("#setPeThink").checked = !!s.pe_think;
    $("#setPeLayersRow").hidden = $("#setPeMode").value !== "partial";
  }
  onPe(st.pe);
}

export function onPe(pe) {
  if (!pe) return;
  if (!pe.ok) { $("#setPeState").textContent = "無法使用：" + pe.error; return; }
  const a = pe.available || {};
  // 只剩通用模型（2026-10-30 官方 PE 拿掉了）：缺 9B／35B，或缺它的看圖檔
  const miss = !a.adult ? ["通用模型 9B／35B"] : a.vision ? [] : ["看圖檔 mmproj（改圖、生圖助手看圖要用）"];
  $("#setPeState").textContent = (pe.running ? `執行中：${pe.kind}（${pe.ngl === 0 ? "CPU" : pe.ngl >= 99 ? "GPU" : "GPU " + pe.ngl + " 層"}）` : "目前沒有載入")
    + (miss.length ? `・缺模型：${miss.join("、")}` : "") + (a.server ? "" : "・缺 llama-server.exe");
}

// 開機時改寫節點還沒就緒、提示詞清單讀失敗的話，進設定頁再讀一次
let netShown = false;
export function show() {
  diagnostics.scan();
  loadReset(); loadVideo(); models.show(); if (!PROMPTS.length) loadPrompts();
  if (!netShown) { netShown = true; mountNet($("#netBox")); } // 網路（代理、下載來源）：第一次打開設定頁才讀
  mountComfyOpts($("#comfyOptsBox")); // ComfyUI 啟動方式：每次打開都重讀（安裝與修復可能改過）
  mountSkills($("#setSkills")); // 外掛寫法：每次打開都重讀（可能自己放了資料夾進去）
  mountFeatures(); // 外掛功能（內容補丁）：每次打開都重讀
  api("/api/hub/modelscope").then((r) => paintMs(r.has_key)).catch(() => {});
}

function paintMs(has) {
  $("#setMsState").textContent = has ? "目前狀態：已設定（要換就貼新的再按儲存）" : "目前狀態：未設定（搜、下載公開模型不用填）";
  $("#setMsKey").placeholder = has ? "已設定" : "未設定";
}

let featuresWired = false;
function mountFeatures() {
  const refresh = async () => {
    try {
      const r = await api("/api/features/strip");
      $("#featStripState").textContent = r.enabled ? `已安裝：${r.ui?.button || "外掛功能"}` : "目前沒有裝任何外掛功能內容補丁。";
    } catch { /* 伺服器沒掛這個 API（舊版）就不顯示 */ }
  };
  if (!featuresWired) {
    featuresWired = true;
    const file = $("#featStripFile");
    $("#featStripImport").onclick = () => file.click();
    file.onchange = async () => {
      const f = file.files[0];
      if (f) {
        try {
          const pack = JSON.parse(await f.text());
          const r = await api("/api/features/strip/install", { json: { pack } });
          applyStrip(r.ui);
          toast("補丁已匯入，功能已啟用", "ok");
        } catch (e) { toast("匯入失敗：" + e.message, "err"); }
      }
      file.value = "";
      refresh();
    };
  }
  refresh();
}

// 設定頁「模型與工作流」（2026-10-21）：外部模型資料夾（電腦裡本來就有的模型原地使用）、掃描、加裝模型（拖檔案、貼網址、從路徑複製）、
// 生圖工作流組（匯入 ComfyUI 的工作流：什麼格式都收；一組可以放好幾個模式；生成頁模型選單最後面「⚙ 生圖工作流」）、取代內建功能、模型總覽。
// 後端在 server\api_models.rs、model_dirs.rs、api_wf.rs、image_wf.rs、wf_import.rs
import { $, el, api, toast, fmtBytes } from "./util.js";
import { app } from "./state.js";
import { tr } from "./i18n.js";
import { exportSet } from "./wfpanel.js";

let KINDS = [];          // [{id, label}]
let DIRS = null;         // /api/models/dirs 的回應
let FLOWS = [];          // /api/image/workflows 的 workflows（工作流組）
let FMODES = [];         // 同上的 modes：[{id, name}]
let FFLOWS = {};         // 同上的 feature_flows：{"家族.模式": 工作流組代號}
let scanTimer = null, dlTimer = null;
const label = (k) => KINDS.find((x) => x.id === k)?.label || k;
const FAM = { anima: "Anima", qwen21: "Qwen 2.1", moss: "MOSS", sdxl: "SDXL", sd15: "SD1.5", flux: "Flux", sd3: "SD3", other: "" };

export function init() {
  $("#mdAdd").onclick = addDir;
  $("#mdPath").onkeydown = (e) => { if (e.key === "Enter") addDir(); };
  $("#mdPick").onclick = pickDir;
  $("#mdScan").onclick = startScan;
  $("#mdRestart").onclick = restartComfy;
  // 加裝
  const drop = $("#mdDrop");
  drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => { e.preventDefault(); drop.classList.remove("over"); uploadAll([...e.dataTransfer.files]); });
  $("#mdPickFile").onclick = () => $("#mdFile").click();
  $("#mdFile").onchange = () => { const f = [...$("#mdFile").files]; $("#mdFile").value = ""; uploadAll(f); };
  $("#mdDl").onclick = download;
  $("#mdUrl").onkeydown = (e) => { if (e.key === "Enter") download(); };
  $("#mdCopy").onclick = copyIn;
  // 生圖工作流
  $("#mdFlowImport").onclick = () => $("#mdFlowFile").click();
  $("#mdFlowFile").onchange = importFlow;
  $("#mdFlowTarget").onchange = () => { $("#mdFlowName").disabled = !!$("#mdFlowTarget").value; };
  $("#mdInvBox").addEventListener("toggle", () => { if ($("#mdInvBox").open) loadInventory(); });
}

/** 進設定頁時重讀 */
let loading = Promise.resolve();
export function show() { loading = Promise.all([loadDirs(), loadFlows()]).catch(() => {}); }

/** 生成頁「拿去 ComfyUI 改」的「到設定匯入」：等上面的清單載完（版面不再往下長）再捲到匯入欄 */
export async function focusImport() {
  await loading;
  const b = $("#mdFlowImport");
  b.scrollIntoView({ block: "center" });
  b.focus({ preventScroll: true });
}

// ---------------------------------------------------------------- 外部模型資料夾
async function loadDirs() {
  try { DIRS = await api("/api/models/dirs"); } catch (e) { $("#mdDirs").textContent = "讀不到：" + e.message; return; }
  if (!KINDS.length) {
    KINDS = DIRS.kinds;
    const opts = () => KINDS.map((k) => el("option", { value: k.id }, k.label));
    $("#mdKind").replaceChildren(el("option", { value: "auto" }, "自動判斷分類"), ...opts());
    $("#mdAddKind").replaceChildren(el("option", { value: "" }, "自動判斷分類"), ...opts());
  }
  // 唯讀說明：另外沿用的 ComfyUI 自己的 extra_model_paths.yaml（不用在這裡再加一次）
  const cy = DIRS.comfy_yaml, yb = $("#mdYaml");
  yb.hidden = !cy?.count;
  if (cy?.count) {
    yb.textContent = `另外沿用 ComfyUI 的 extra_model_paths.yaml：${cy.count} 個資料夾`;
    yb.title = cy.dirs.map((d) => `${d.label}：${d.path}`).join("\n");
  }
  const box = $("#mdDirs");
  if (!DIRS.dirs.length) {
    box.replaceChildren(el("div", { class: "muted sm" }, "還沒有外部資料夾：目前只讀生圖站自己的 ComfyUI（" + DIRS.comfy_models + "）。"));
    return;
  }
  box.replaceChildren(...DIRS.dirs.map((d) => el("div", { class: "md-row" + (d.exists ? "" : " gone") },
    el("span", { class: "md-kind" }, d.label),
    el("span", { class: "md-path", title: d.path, translate: "no" }, d.path),
    el("span", { class: "muted sm" }, d.exists ? `${d.files} 個檔・${fmtBytes(d.bytes)}` : "資料夾不見了"),
    el("button", { class: "akey sm", type: "button", onclick: () => removeDir(d) }, "移除"))));
}

function needRestart(msg) {
  $("#mdMsg").textContent = msg + (DIRS?.can_restart ? "：按「重開 ComfyUI」生效。" : "：要把 ComfyUI 關掉再開一次（或重開生圖站）才生效。");
  $("#mdRestart").hidden = !DIRS?.can_restart;
}

// 「選資料夾…」：Windows 的選資料夾視窗（後端 setup.rs pick_folder，精靈的 API 要帶 X-Anima-Setup）
async function pickDir() {
  try {
    const r = await api("/api/setup/pick_folder", { json: { title: tr("選模型資料夾"), start: $("#mdPath").value }, headers: { "X-Anima-Setup": "1" }, timeout: 0 });
    if (r.path) $("#mdPath").value = r.path;
  } catch (e) { toast(e.message, "err"); }
}

async function addDir() {
  const path = $("#mdPath").value.trim().replace(/^"|"$/g, "");
  if (!path) return toast("先填資料夾的完整路徑", "err");
  try {
    const r = await api("/api/models/dirs/add", { json: { path, kind: $("#mdKind").value } });
    $("#mdPath").value = "";
    if (!r.added.length) return toast("沒有新加入的資料夾：生圖站本來就會讀這裡，或已經在清單裡了", "err");
    await loadDirs();
    toast(`已加入 ${r.added.length} 個資料夾：` + r.added.map((a) => `${a.label}（${a.path}）`).join("、"), "ok");
    needRestart("外部資料夾改了");
    app.refreshState();
  } catch (e) { toast("加入失敗：" + e.message, "err"); }
}

async function removeDir(d) {
  if (!confirm(`不再讀「${d.path}」（${d.label}）？資料夾和裡面的檔案都不會被刪。`)) return;
  try {
    await api("/api/models/dirs/remove", { json: { kind: d.kind, path: d.path } });
    await loadDirs();
    needRestart("外部資料夾改了");
    app.refreshState();
  } catch (e) { toast(e.message, "err"); }
}

async function restartComfy() {
  try {
    await api("/api/models/restart_comfy", { json: {} });
    $("#mdRestart").hidden = true;
    $("#mdMsg").textContent = "ComfyUI 重開中，大約半分鐘後就讀得到新的資料夾。";
  } catch (e) { toast(e.message, "err"); }
}

// ---------------------------------------------------------------- 掃描
async function startScan() {
  try { await api("/api/models/scan", { json: {} }); } catch (e) { if (!/已經在掃描/.test(e.message)) return toast(e.message, "err"); }
  $("#mdScanBox").hidden = false;
  $("#mdScan").disabled = true;
  pollScan();
}

async function pollScan() {
  clearTimeout(scanTimer);
  let s;
  try { s = await api("/api/models/scan"); } catch (e) { $("#mdScanBox").textContent = e.message; $("#mdScan").disabled = false; return; }
  const box = $("#mdScanBox");
  if (s.running) {
    box.replaceChildren(
      el("div", {}, `掃描中…已看過 ${s.dirs.toLocaleString()} 個資料夾、${s.files.toLocaleString()} 個模型檔`),
      el("div", { class: "muted sm md-path", translate: "no" }, s.current || ""),
      el("button", { class: "akey sm", type: "button", onclick: () => api("/api/models/scan/cancel", { json: {} }) }, "停止"));
    scanTimer = setTimeout(pollScan, 1000);
    return;
  }
  $("#mdScan").disabled = false;
  showScan(s);
}

function showScan(s) {
  const box = $("#mdScanBox");
  const head = el("div", {}, (s.cancelled ? "已停止。" : "掃完了。") + `花了 ${s.seconds} 秒，看過 ${s.dirs.toLocaleString()} 個資料夾、${s.files.toLocaleString()} 個模型檔。`);
  const newFound = s.found.filter((f) => !f.used).length;
  const kids = [head, el("div", { class: "muted sm" }, `安裝精靈清單裡的模型：找到 ${s.found.length} 個（${s.found.length - newFound} 個本來就在用）。`)];
  if (!s.suggest.length) {
    kids.push(el("div", { class: "muted sm" }, "沒有找到還沒在用的模型資料夾。"));
    box.replaceChildren(...kids);
    return;
  }
  kids.push(el("div", {}, "勾選要原地使用的資料夾（不會搬動或複製檔案）："));
  const checks = s.suggest.map((x) => {
    const cb = el("input", { type: "checkbox", checked: true });
    cb._item = x;
    const why = x.why.type === "manifest" ? "清單裡的模型" : x.why.type === "a1111" ? "A1111／Forge 的資料夾" : x.why.type === "yaml" ? "別的 ComfyUI 設定檔裡寫的" : "別的 ComfyUI 的資料夾";
    const extra = [x.manifest_files ? `清單模型 ${x.manifest_files} 個` : "", x.why.files ? `${x.why.files} 個檔・${fmtBytes(x.why.bytes)}` : ""].filter(Boolean).join("・");
    return el("label", { class: "check md-row" }, cb, el("span", { class: "md-kind" }, x.label),
      el("span", { class: "md-path", title: x.path, translate: "no" }, x.path), el("span", { class: "muted sm" }, `${why}${extra ? "・" + extra : ""}`));
  });
  kids.push(el("div", { class: "md-list" }, ...checks));
  kids.push(el("div", { class: "row gap" }, el("button", { class: "akey sm go", type: "button", onclick: async () => {
    const dirs = checks.map((c) => c.querySelector("input")).filter((c) => c.checked).map((c) => ({ kind: c._item.kind, path: c._item.path }));
    if (!dirs.length) return toast("沒有勾選任何資料夾", "err");
    try {
      await api("/api/models/scan/apply", { json: { dirs } });
      await loadDirs();
      box.replaceChildren(el("div", {}, `已加入 ${dirs.length} 個資料夾。`));
      needRestart("外部資料夾改了");
      app.refreshState();
    } catch (e) { toast("套用失敗：" + e.message, "err"); }
  } }, "使用勾選的資料夾")));
  if (s.near?.length) {
    kids.push(el("details", { class: "fold muted sm" }, el("summary", {}, `檔名對、但大小或資料夾不對的（${s.near.length} 個，不會自動用）`),
      ...s.near.map((n) => el("div", { translate: "no" }, `${n.file}：${n.why === "size" ? `大小 ${fmtBytes(n.size)}，清單要 ${fmtBytes(n.want_size)}（可能是不同版本）` : `要放在 ${n.rel} 這樣的子資料夾裡`}`))));
  }
  box.replaceChildren(...kids);
}

// ---------------------------------------------------------------- 加裝模型
function jobRow(title) {
  const bar = el("progress", { max: 1, value: 0 });
  const msg = el("span", { class: "muted sm" }, "");
  const row = el("div", { class: "md-job" }, el("span", { class: "md-path", translate: "no" }, title), bar, msg);
  $("#mdJobs").prepend(row);
  return { row, bar, msg };
}

/** 歸位結果：放好了、要選分類、出錯 */
function showPlaced(job, r) {
  job.bar.value = 1;
  if (r.placed) {
    job.msg.textContent = `已放進「${r.label || label(r.kind)}」${FAM[r.family] ? `（${FAM[r.family]}）` : ""}`;
    job.row.classList.add("done");
    app.refreshState();
    return;
  }
  if (r.need_kind) {
    const sel = el("select", {}, el("option", { value: "" }, "選分類…"), ...KINDS.map((k) => el("option", { value: k.id }, k.label)));
    const go = el("button", { class: "akey sm", type: "button", onclick: async () => {
      if (!sel.value) return toast("先選分類", "err");
      try { showPlaced(job, await api("/api/models/place", { json: { name: r.name, kind: sel.value } })); } catch (e) { toast(e.message, "err"); }
    } }, "放進去");
    job.msg.replaceChildren(r.why + "：", sel, go);
    return;
  }
  job.msg.textContent = r.error || "失敗";
  job.row.classList.add("err");
}

function uploadAll(files) {
  for (const f of files) upload(f);
}

function upload(file) {
  const job = jobRow(file.name);
  const xhr = new XMLHttpRequest();
  xhr.open("PUT", `/api/models/upload?name=${encodeURIComponent(file.name)}&kind=${encodeURIComponent($("#mdAddKind").value)}`);
  xhr.upload.onprogress = (e) => { if (e.lengthComputable) { job.bar.value = e.loaded / e.total; job.msg.textContent = `${fmtBytes(e.loaded)} / ${fmtBytes(e.total)}`; } };
  xhr.onload = () => {
    let r = null;
    try { r = JSON.parse(xhr.responseText); } catch { /* 非 JSON */ }
    if (xhr.status !== 200) { job.msg.textContent = r?.error || `HTTP ${xhr.status}`; job.row.classList.add("err"); return; }
    showPlaced(job, r);
  };
  xhr.onerror = () => { job.msg.textContent = "上傳失敗（連線中斷）"; job.row.classList.add("err"); };
  xhr.send(file);
}

async function copyIn() {
  const path = $("#mdFrom").value.trim().replace(/^"|"$/g, "");
  if (!path) return toast("先填模型檔的完整路徑", "err");
  const job = jobRow(path);
  job.msg.textContent = "複製中…（大檔要一點時間）";
  try {
    const r = await api("/api/models/import", { json: { path, kind: $("#mdAddKind").value }, timeout: 0 });
    $("#mdFrom").value = "";
    showPlaced(job, r);
  } catch (e) { job.msg.textContent = e.message; job.row.classList.add("err"); }
}

const DL_ROWS = new Map();
async function download() {
  const url = $("#mdUrl").value.trim();
  if (!url) return toast("先貼網址", "err");
  try {
    const r = await api("/api/models/download", { json: { url, kind: $("#mdAddKind").value } });
    $("#mdUrl").value = "";
    DL_ROWS.set(r.id, jobRow(url));
    pollDownloads();
  } catch (e) { toast("不能下載：" + e.message, "err"); }
}

async function pollDownloads() {
  clearTimeout(dlTimer);
  let list;
  try { list = (await api("/api/models/downloads")).downloads; } catch { dlTimer = setTimeout(pollDownloads, 2000); return; }
  let running = false;
  for (const d of list) {
    const job = DL_ROWS.get(d.id);
    if (!job || job.finished) continue;
    if (d.status === "run") {
      running = true;
      if (d.name) job.row.firstChild.textContent = d.name;
      job.bar.value = d.total ? d.done / d.total : 0;
      job.msg.textContent = d.total ? `${fmtBytes(d.done)} / ${fmtBytes(d.total)}` : fmtBytes(d.done);
      continue;
    }
    job.finished = true;
    if (d.result) showPlaced(job, d.result);
    else { job.msg.textContent = d.error || "失敗"; job.row.classList.add("err"); }
  }
  if (running) dlTimer = setTimeout(pollDownloads, 1000);
}

// ---------------------------------------------------------------- 生圖工作流（工作流組：一個名字底下可以放好幾個模式）
// 後端在 server\api_wf.rs、image_wf.rs、wf_import.rs；設計在 design\萬用工作流與匯入匯出.md
const MODE_NAMES = { txt2img: "文生圖", img2img: "圖生圖", inpaint: "局部重繪", upscale: "放大", multiref: "多圖參考" };
const modeLabel = (m) => FMODES.find((x) => x.id === m)?.name || MODE_NAMES[m] || m;
const BUILTIN_FAMS = ["anima", "qwen21", "moss"];
const famName = (f) => app.state?.models?.family_names?.[f] || FAM[f] || f;

async function loadFlows() {
  let r;
  try { r = await api("/api/image/workflows"); } catch (e) { $("#mdFlows").textContent = "讀不到：" + e.message; return; }
  FLOWS = r.workflows;
  FMODES = r.modes?.length ? r.modes : Object.entries(MODE_NAMES).map(([id, name]) => ({ id, name }));
  FFLOWS = r.feature_flows || {};
  renderFlows();
  fillImportForm();
  renderFeat();
}

/** 小標籤（家族、來源） */
const tag = (text, title, cls = "") => el("span", { class: "wf-tag " + cls, title: title || null }, text);

/** 「它怎麼認的」：analyze 的摘要（image_wf.rs Analysis::summary）用白話列出來 */
function describe(s, append) {
  const nd = (n) => `「${n.title}」（#${n.node}）`;
  const lines = [];
  lines.push(!s.prompt.length ? "提示詞：找不到要放的地方" : s.placeholder ? `提示詞放進 ${s.prompt.map(nd).join("、")} 的 {{prompt}}` : (append ? `提示詞接在 ${s.prompt.map(nd).join("、")} 原本的文字後面` : `提示詞整格換掉 ${s.prompt.map(nd).join("、")}`));
  lines.push(!s.negative.length ? "負面提示詞：照工作流" : (s.negative_placeholder ? `負面提示詞放進 ${s.negative.map(nd).join("、")} 的 {{negative}}` : `負面提示詞整格換掉 ${s.negative.map(nd).join("、")}（生成頁空白時照工作流）`));
  if (s.image) lines.push(`輸入圖：${nd(s.image)}`);
  if (s.mask) lines.push(`遮罩：${nd(s.mask)}`);
  else if (s.mask_from_image) lines.push("遮罩：用輸入圖那個節點的遮罩輸出");
  for (const r of s.refs || []) lines.push(`參考圖 ${r.n}：${nd(r)}`);
  lines.push(s.sizes ? "寬高：文生圖照生成頁" : "寬高：照工作流");
  lines.push(s.seeds ? `種子：${s.seeds} 個欄位` : "種子：找不到（每次結果都一樣）");
  if (s.model) lines.push(`主模型：${nd(s.model)}${s.model.value ? " " + s.model.value : ""}`);
  if (s.samplers?.length) lines.push(`取樣節點：${s.samplers.map(nd).join("、")}${s.samplers.some((x) => x.marked) ? "（有標 {{sampler}}）" : ""}`);
  if (s.loras?.length) lines.push(`工作流自帶的 LoRA 節點：${s.loras.map(nd).join("、")}`);
  return lines;
}

function flowRow(w) {
  const fam = w.family && w.family !== "other" ? w.family : "";
  const head = el("div", { class: "wf-head" },
    el("span", { class: "md-kind", translate: "no" }, "⚙ " + w.name),
    fam ? tag(FAM[fam] || fam, famName(fam)) : tag("家族不明", "看不出這組工作流用的模型家族：生成頁的 LoRA 都不篩，全部會接上去", "dim"),
    tag(w.source === "builtin" ? "內建匯出" : "匯入", w.source === "builtin" ? "從生圖站內建功能匯出、改過再匯入的" : "從 ComfyUI 的工作流匯入的", "dim"),
    w.model_file ? el("span", { class: "muted sm md-path", translate: "no", title: "主模型檔" }, w.model_file) : null);
  const bad = Object.entries(w.errors || {});
  const chips = el("div", { class: "wf-chips" },
    ...w.modes.map((m) => el("span", { class: "wf-chip" }, modeLabel(m),
      el("button", { class: "x", type: "button", title: `移除這組的「${modeLabel(m)}」`, "aria-label": `移除${modeLabel(m)}`, onclick: () => removeMode(w, m) }, "×"))),
    ...bad.map(([m]) => el("span", { class: "wf-chip bad", title: "這個模式的工作流現在讀不懂" }, modeLabel(m),
      el("button", { class: "x", type: "button", title: `移除這組的「${modeLabel(m)}」`, "aria-label": `移除${modeLabel(m)}`, onclick: () => removeMode(w, m) }, "×"))));
  // 取樣參數分成好幾個節點、又找不到 Scheduler／KSamplerSelect 可以套的工作流：開了也沒用，灰掉
  const psOk = w.page_sampler_ok !== false;
  const ps = el("input", { type: "checkbox", checked: psOk && !!w.options?.page_sampler, disabled: !psOk });
  ps.onchange = () => setPageSampler(w, ps);
  const append = w.options?.prompt_mode === "append";
  const pm = el("input", { type: "checkbox", checked: append });
  pm.onchange = () => setPromptMode(w, pm);
  // 分析時的提醒（種子改不到、FaceDetailer 的提示詞維持原本的…）：每個模式的合起來、去重複
  const warns = [...new Set(Object.values(w.summary || {}).flatMap((x) => x.warnings || []))];
  const acts = el("div", { class: "row gap wrap" },
    el("button", { class: "akey sm", type: "button", onclick: () => exportFlow(w) }, "匯出"),
    el("button", { class: "akey sm warn", type: "button", onclick: () => removeFlow(w) }, "刪除整組"),
    el("label", { class: "check inline", title: psOk ? "打開：工作流裡取樣節點的步數、CFG、取樣器、排程用生成頁的旋鈕；關掉：照工作流" : "這個工作流的取樣器分成好幾個節點，生圖站找不到可以套的地方：請在參數面板改" }, ps, "用生成頁的步數、CFG"),
    el("label", { class: "check inline", title: "打開：工作流原本寫的提示詞（品質詞、觸發詞…）保留，生成頁的提示詞接在後面；關掉：整格換成生成頁的（有寫 {{prompt}} 的照標記放）" }, pm, "保留工作流原本的提示詞，接在後面"));
  const sums = Object.entries(w.summary || {});
  const how = sums.length ? el("details", { class: "fold muted sm" }, el("summary", {}, "它怎麼認的"),
    ...sums.map(([m, s]) => el("div", { class: "wf-how" }, el("b", {}, modeLabel(m)),
      el("ul", { class: "vid-rules" }, ...describe(s, append).map((t) => el("li", { translate: "no" }, t)))))) : null;
  return el("div", { class: "wf-set" }, head, chips,
    ...bad.map(([m, why]) => el("div", { class: "err sm" }, `${modeLabel(m)}：${why}`)),
    w.error ? el("div", { class: "err sm" }, "讀不懂：" + w.error) : null,
    warns.length ? el("div", { class: "warnbox sm" }, ...warns.map((t) => el("div", {}, t))) : null, acts, how);
}

function renderFlows() {
  const box = $("#mdFlows");
  if (!FLOWS.length) { box.replaceChildren(el("div", { class: "muted sm" }, "還沒有匯入生圖工作流。")); return; }
  box.replaceChildren(...FLOWS.map(flowRow));
}

/** 匯入表單的選單：模式、放進哪一組 */
function fillImportForm() {
  const ms = $("#mdFlowMode"), ts = $("#mdFlowTarget");
  const keepM = ms.value, keepT = ts.value;
  // 自動判斷：生成頁「拿去 ComfyUI 改…」存的檔帶著 {{mode:…}}，後端照它；其他的檔當文生圖
  ms.replaceChildren(el("option", { value: "auto" }, "自動判斷"), ...FMODES.map((m) => el("option", { value: m.id }, m.name)));
  ts.replaceChildren(el("option", { value: "" }, "新的一組"), ...FLOWS.map((w) => el("option", { value: w.id }, `加進「${w.name}」`)));
  if ([...ms.options].some((o) => o.value === keepM)) ms.value = keepM;
  if ([...ts.options].some((o) => o.value === keepT)) ts.value = keepT;
  $("#mdFlowName").disabled = !!ts.value;
}

/** 讀成 base64（不含 data: 前綴） */
const toB64 = (file) => new Promise((ok, no) => {
  const fr = new FileReader();
  fr.onload = () => ok(String(fr.result).split(",")[1] || "");
  fr.onerror = () => no(new Error("讀不到這個檔案"));
  fr.readAsDataURL(file);
});

/** 結果區：成功的提醒（轉換時拿掉了什麼）、失敗的原因（缺哪些自訂節點、要怎麼標 {{mask}}…） */
function flowOut(kind, head, lines = []) {
  const out = $("#mdFlowOut");
  out.hidden = !head;
  if (!head) return out.replaceChildren();
  out.className = "wf-out" + (kind === "err" ? " warnbox" : "");
  out.replaceChildren(el("div", { class: kind === "err" ? "st-err" : "st-ok" }, head),
    ...(kind === "err" ? [el("div", { class: "wf-msg" }, lines.join("\n"))] : lines.length ? [el("ul", { class: "vid-rules" }, ...lines.map((t) => el("li", {}, t)))] : []));
}

/** 生成頁「讀圖片參數」的「匯入成工作流」：用這張圖新建一組（名字＝檔名，模式自動判斷），結果顯示在匯入欄 */
export async function importImageFile(f) {
  await focusImport();
  const name = f.name.replace(/\.[^.]+$/, "");
  if (FLOWS.some((w) => w.name === name) && !confirm(`已經有叫「${name}」的工作流組：這張圖會加進去（同一個模式會換成新的，其他模式保留）。要繼續嗎？`)) return;
  let body;
  try { body = { mode: "auto", file_name: f.name, data_b64: await toB64(f) }; }
  catch (e) { flowOut("err", "匯入失敗", [e.message]); return; }
  await sendFlow(body);
}

/** 下載中心「匯入生圖站」：/api/hub/workflow 拿到的檔（base64）新建一組（名字＝卡片名稱，模式自動判斷），結果顯示在匯入欄 */
export async function importData(fileName, b64, name) {
  await focusImport();
  if (FLOWS.some((w) => w.name === name) && !confirm(`已經有叫「${name}」的工作流組：這個檔會加進去（同一個模式會換成新的，其他模式保留）。要繼續嗎？`)) return;
  await sendFlow({ mode: "auto", file_name: fileName, data_b64: b64, name });
}

async function importFlow() {
  const f = $("#mdFlowFile").files[0];
  $("#mdFlowFile").value = "";
  if (!f) return;
  const mode = $("#mdFlowMode").value || "auto", target = $("#mdFlowTarget").value;
  const tset = FLOWS.find((w) => w.id === target);
  let name = target ? "" : $("#mdFlowName").value.trim();
  // 生圖站自己匯出的工作流組檔（整組）：名字用檔裡的，模式不用選
  let isSet = false, want = mode === "auto" ? "" : mode;
  if (/\.json$/i.test(f.name) && f.size < 64 * 2 ** 20) {
    const text = await f.text();
    // 自動判斷會收成哪個模式（跟後端 image_wf::mode_mark 一樣只看節點標題裡的 {{mode:…}}），拿來問要不要換掉同一個模式
    if (!want) want = /"title"\s*:\s*"[^"]*\{\{mode:(txt2img|img2img|inpaint|upscale|multiref)\}\}/.exec(text)?.[1] || "txt2img";
    try {
      const o = JSON.parse(text);
      if (o && typeof o === "object" && o.graphs && typeof o.graphs === "object") { isSet = true; if (!name && !target && o.name) name = String(o.name); }
    } catch { /* 不是 JSON：交給後端說明 */ }
  }
  const shown = name || f.name.replace(/\.[^.]+$/, "");
  if (!target && FLOWS.some((w) => w.name === shown) && !confirm(`已經有叫「${shown}」的工作流組：這個檔會加進去（同一個模式會換成新的，其他模式保留）。要繼續嗎？`)) return;
  if (tset && !isSet && want && tset.modes.includes(want) && !confirm(`「${tset.name}」已經有「${modeLabel(want)}」的工作流，要換成這個新的嗎？`)) return;
  // PNG／WebP 匯入前讀不到是哪個模式：加進已經有東西的組時先講一聲
  if (tset && !want && tset.modes.length && !confirm(`這個檔加進「${tset.name}」後，「${tset.name}」裡同一個模式的工作流會換成它（沒有標記的檔當文生圖）。要繼續嗎？`)) return;
  let body;
  try { body = { mode, file_name: f.name, data_b64: await toB64(f) }; }
  catch (e) { flowOut("err", "匯入失敗", [e.message]); return; }
  if (target) body.set = target; else if (name) body.name = name;
  await sendFlow(body);
}

// 送出匯入（檔案內容留在 body 裡：分析失敗、使用者指定好節點後，帶著 marks 再送一次）
async function sendFlow(body) {
  const btn = $("#mdFlowImport");
  btn.disabled = true;
  flowOut("ok", "匯入中…（一般「儲存」的工作流要靠 ComfyUI 轉換，稍等一下）");
  try {
    const r = await api("/api/image/workflows", { json: body, timeout: 5 * 60 * 1000 });
    $("#mdFlowName").value = "";
    const w = r.workflow;
    const got = r.mode ? `這個檔收成「${modeLabel(r.mode)}」；` : "";
    flowOut("ok", `已匯入「${w.name}」：${got}這組現在有${w.modes.map(modeLabel).join("、")}。到生成頁的模型選單最下面選「⚙ ${w.name}」。`, r.warnings || []);
    missingOut(r.missing_models || []);
    toast(`已匯入「${w.name}」`, "ok");
    await loadFlows();
    await app.refreshState();
    offerReplace(w, r.mode);
  } catch (e) {
    flowOut("err", "匯入失敗", [e.message]);
    if (e.data?.need && e.data.candidates) markForm(body, e.data);
    toast("匯入失敗，原因在匯入欄下面", "err");
  } finally {
    btn.disabled = false;
  }
}

/** 匯入結果底下：工作流用到、這台電腦沒有的模型（api_wf::import 的 missing_models），LoRA／主模型／VAE 給一顆「到下載中心找」 */
function missingOut(list) {
  if (!list.length) return;
  const KIND = { lora: "LoRA", model: "主模型", vae: "VAE", te: "文字編碼器" };
  $("#mdFlowOut").append(el("div", { class: "wf-missing" },
    el("div", { class: "st-err" }, `這個工作流用到 ${list.length} 個這台電腦沒有的模型，畫之前要先放好：`),
    el("ul", { class: "vid-rules" }, ...list.map((m) => el("li", {},
      el("span", { translate: "no" }, m.name), `（${KIND[m.kind] || m.kind}）`,
      m.kind !== "te" && app.hubFind ? el("button", { class: "akey sm", type: "button", onclick: () => app.hubFind(m.name, m.kind) }, "到下載中心找") : null)))));
}

// 分析找不到某個地方（api_wf::import 回的 need）：問使用者是哪個節點。mark＝寫進去的標記，list＝candidates 裡的哪一類
const NEED = {
  prompt: { list: "texts", mark: "{{prompt}}", ask: "哪一格是正面提示詞？（生成頁寫的提示詞會放進這格）" },
  image: { list: "images", mark: "{{image}}", ask: "哪一個 Load Image 放輸入圖？" },
  mask: { list: "images", mark: "{{mask}}", ask: "哪一個 Load Image 放遮罩？" },
  ref1: { list: "images", mark: "{{ref1}}", ask: "哪一個 Load Image 放第一張參考圖？" },
  model: { list: "models", mark: "{{model}}", ask: "哪一個節點載入主模型？" },
  sampler: { list: "samplers", mark: "{{sampler}}", ask: "哪一個是主要的取樣節點？" },
};
// 文字格要連欄位一起標（「6.text」），其他標在節點上
const candKey = (c) => (c.text !== undefined ? `${c.node}.${c.field}` : c.node);
function candLabel(c) {
  const what = c.text !== undefined ? `${c.field}：${c.text || "（空的）"}` : c.image !== undefined ? `圖：${c.image}` : c.value !== undefined ? String(c.value) : "";
  return `#${c.node} ${c.title || c.class}${what ? `　${what}` : ""}`;
}

/** 匯入失敗底下的「幫生圖站指定」：選好節點（和模式），帶著 marks 重送同一個檔 */
function markForm(body, d) {
  const need = NEED[d.need], list = d.candidates[need?.list] || [];
  const out = $("#mdFlowOut");
  if (!need) return;
  if (!list.length) {
    out.append(el("div", { class: "muted sm" }, "這個工作流裡找不到可以選的節點：照上面說的在 ComfyUI 改好再匯入。"));
    return;
  }
  const pick = el("select", { "aria-label": need.ask, style: "max-width:100%" }, ...list.map((c) => el("option", { value: candKey(c) }, candLabel(c))));
  // 正面提示詞順便問負面的（可以不選）
  const neg = d.need === "prompt" && list.length > 1
    ? el("select", { "aria-label": "負面提示詞放哪一格", style: "max-width:100%" },
      el("option", { value: "" }, "不換（照工作流原本的）"), ...list.map((c) => el("option", { value: candKey(c) }, candLabel(c))))
    : null;
  const modes = FMODES.length ? FMODES : Object.entries(MODE_NAMES).map(([id, name]) => ({ id, name }));
  const mode = el("select", { "aria-label": "這個工作流是做什麼的" }, ...modes.map((m) => el("option", { value: m.id, selected: m.id === d.mode }, m.name)));
  const go = el("button", { class: "akey sm go", type: "button" }, "照這樣匯入");
  go.onclick = () => {
    const marks = { ...(body.marks || {}) };
    // 同一格不能同時是正面和負面
    if (neg?.value && neg.value === pick.value) { toast("正面和負面提示詞不能選同一格", "err"); return; }
    marks[pick.value] = need.mark;
    if (neg?.value) marks[neg.value] = "{{negative}}";
    sendFlow({ ...body, marks, mode: mode.value });
  };
  out.append(el("div", { class: "wf-mark" },
    el("div", { class: "sm" }, "也可以直接告訴生圖站是哪個節點（不用回 ComfyUI 改）："),
    el("label", { class: "sm" }, need.ask, pick),
    neg && el("label", { class: "sm" }, "負面提示詞是哪一格？（可以不選）", neg),
    el("label", { class: "sm" }, "這個工作流是做什麼的？", mode),
    el("div", {}, go)));
}

async function removeMode(w, mode) {
  const left = w.modes.length + Object.keys(w.errors || {}).length;
  const msg = left <= 1
    ? `「${w.name}」只剩「${modeLabel(mode)}」這一個模式，移除後整組都會刪掉。要刪嗎？（你電腦上原本的檔案不受影響）`
    : `移除「${w.name}」的「${modeLabel(mode)}」？這組其他的模式不受影響。`;
  if (!confirm(msg)) return;
  try {
    const r = await api(`/api/image/workflows/${encodeURIComponent(w.id)}?mode=${encodeURIComponent(mode)}`, { method: "DELETE" });
    toast(r.removed_set ? `「${w.name}」已經刪掉` : `已移除「${w.name}」的「${modeLabel(mode)}」`, "ok");
    await loadFlows();
    await app.refreshState();
  } catch (e) { toast(e.message, "err"); }
}

async function removeFlow(w) {
  if (!confirm(`刪掉整組生圖工作流「${w.name}」？（你電腦上原本的 JSON 檔不受影響；指定用它取代內建功能的也會一起取消）`)) return;
  try { await api(`/api/image/workflows/${encodeURIComponent(w.id)}`, { method: "DELETE" }); await loadFlows(); await app.refreshState(); }
  catch (e) { toast(e.message, "err"); }
}

/** 匯入完：這組的家族是有內建功能的（Anima、Qwen 2.1、MOSS），就在結果區問要不要直接拿它取代那個功能 */
function offerReplace(w, mode) {
  const f = w.family, m = FMODES.find((x) => x.id === mode);
  if (!m || !BUILTIN_FAMS.includes(f) || FFLOWS[`${f}.${mode}`] === w.id) return;
  const b = el("button", { class: "akey sm go", type: "button" }, "取代");
  b.onclick = async () => {
    b.disabled = true;
    if (await setFeat(`${f}.${mode}`, w.id, f, m)) b.replaceWith(el("span", { class: "st-ok sm" }, "已取代"));
    else b.disabled = false;
  };
  $("#mdFlowOut").append(el("div", { class: "row gap wrap" },
    el("span", { class: "sm" }, `要直接拿它取代${famName(f)}的「${m.name}」嗎？之後隨時可以在下面的「取代內建功能」選回「內建」。`), b));
}

async function exportFlow(w) {
  try {
    const r = await exportSet(w.id);
    if (!r.cancelled) toast(`已存到 ${r.path}（之後可以用「匯入工作流…」整組匯回來）`, "ok");
  } catch (e) { toast("匯出失敗：" + e.message, "err"); }
}

async function setPromptMode(w, box) {
  try {
    await api(`/api/image/workflows/${encodeURIComponent(w.id)}/options`, { json: { prompt_mode: box.checked ? "append" : "replace" } });
    await loadFlows();
  } catch (e) { box.checked = !box.checked; toast("沒辦法改：" + e.message, "err"); }
}

async function setPageSampler(w, box) {
  try {
    await api(`/api/image/workflows/${encodeURIComponent(w.id)}/options`, { json: { page_sampler: box.checked } });
    await loadFlows();
    await app.refreshState();
  } catch (e) { box.checked = !box.checked; toast("沒辦法改：" + e.message, "err"); }
}

// ---------------------------------------------------------------- 取代內建功能（settings 的 feature_flows：「家族.模式」→ 工作流組）
function renderFeat() {
  const box = $("#mdFeat");
  const head = [el("span", {}), ...BUILTIN_FAMS.map((f) => el("b", { class: "wf-fh", title: famName(f) }, FAM[f] || f))];
  const cells = [];
  for (const m of FMODES) {
    cells.push(el("span", { class: "wf-fm" }, m.name));
    for (const f of BUILTIN_FAMS) {
      const key = `${f}.${m.id}`, cur = FFLOWS[key] || "";
      const sets = FLOWS.filter((w) => w.modes.includes(m.id));
      const opts = [el("option", { value: "" }, "內建"), ...sets.map((w) => el("option", { value: w.id }, "⚙ " + w.name))];
      if (cur && !sets.some((w) => w.id === cur)) opts.push(el("option", { value: cur }, `${cur}（找不到）`));
      const sel = el("select", { "aria-label": `${FAM[f] || f}的${m.name}`, title: `${famName(f)}的「${m.name}」：用內建的，或換成你匯入的工作流` }, opts);
      sel.value = cur;
      sel.onchange = () => setFeat(key, sel.value, f, m);
      cells.push(sel);
    }
  }
  box.replaceChildren(...head, ...cells);
}

/** 改「取代內建功能」的一格；成功回 true */
async function setFeat(key, id, f, m) {
  let done = false;
  try {
    const r = await api("/api/feature_flows", { json: { key, id: id || null } });
    FFLOWS = r.feature_flows || {};
    const w = FLOWS.find((x) => x.id === id);
    toast(id ? `${FAM[f] || f}的「${m.name}」改用工作流「${w?.name || id}」` : `${FAM[f] || f}的「${m.name}」切回內建`, "ok");
    done = true;
  } catch (e) { toast(e.message, "err"); }
  renderFeat();
  return done;
}

// ---------------------------------------------------------------- 總覽
async function loadInventory() {
  const box = $("#mdInv");
  box.textContent = "讀取中…";
  let v;
  try { v = await api("/api/models/inventory", { timeout: 0 }); } catch (e) { box.textContent = "讀不到：" + e.message; return; }
  const groups = v.groups.filter((g) => g.files.length).map((g) => el("details", { class: "fold" },
    el("summary", {}, `${g.label}（${g.files.length}）`),
    el("div", { class: "md-list" }, ...g.files.map((f) => el("div", { class: "md-row sm" },
      el("span", { class: "md-path", translate: "no", title: f.root }, f.name),
      el("span", { class: "muted sm" }, [FAM[f.family] || "", fmtBytes(f.size), f.external ? "外部資料夾" : ""].filter(Boolean).join("・")))))));
  const miss = (w, title) => {
    const list = v.missing[w];
    if (!list.length) return el("div", { class: "muted sm" }, `${title}：${v.missing.total[w]} 個都有了。`);
    return el("details", { class: "fold muted sm" }, el("summary", {}, `${title}：還缺 ${list.length} 個（共 ${fmtBytes(list.reduce((a, m) => a + m.size, 0))}）`),
      ...list.map((m) => el("div", { translate: "no" }, `${m.rel}（${fmtBytes(m.size)}）${m.download ? "" : "：沒有下載來源"}`)));
  };
  box.replaceChildren(...(groups.length ? groups : [el("div", { class: "muted sm" }, "一個模型檔都沒有。")]),
    el("div", { class: "md-sep" }), miss("basic", "安裝精靈「基本版」清單"), miss("full", "「完整版」清單"),
    el("div", { class: "muted sm" }, "缺的模型可以到「安裝精靈 → 安裝與修復」下載，或先按上面的「掃描這台電腦已有的模型」。"));
}

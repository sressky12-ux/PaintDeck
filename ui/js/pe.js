// AI 改寫提示詞（通用模型 9B／35B；官方 PE 2026-10-30 拿掉了）：想法 → 官方格式的完整提示詞；版本歷史、差異標示、反覆微調、中文對照、建議比例、生成時自動改寫。
// 後端是 /api/pe/rewrite（SSE 串流）；llama-server 的啟動、顯存協調、快取都在伺服器那邊。
import { $, $$, el, toast, store, api, fmtBytes } from "./util.js";
import { app } from "./state.js";

// 快速微調按鈕：[標籤, 指令]；指令以「…」結尾的會先填進輸入框讓你補完，其他按了直接送出
const QUICK = [
  ["更寫實", "讓畫面更寫實：改成真實照片的質感、自然的膚質與光線"],
  ["改夜景", "把時間改成夜晚：天空、光源和陰影都跟著變成夜景"],
  ["鏡頭拉遠", "鏡頭拉遠：拍到全身和更多周圍環境，人物在畫面中變小"],
  ["拉近特寫", "鏡頭拉近成特寫：只拍到臉和肩膀"],
  ["加強光影", "加強光影：更強烈的明暗對比、清楚的光源方向和輪廓光"],
  ["縮短", "縮短成大約一半長度，保留最重要的細節"],
  ["換衣服", "把衣服換成…"],
  ["換背景", "把背景換成…"],
];
const TASK_TEXT = {
  t2i: "文生圖版：把一句想法擴寫成 Qwen 官方建議的完整英文描述（約 20 句）",
  i2i: "改圖版：會讀參考圖，寫成官方格式的改圖指令（多張圖保留 <image1>、<image2> 標記）",
  anima: "Anima 版：寫成 Danbooru tag＋至少兩句英文（評級 tag、@畫師 照官方建議）",
  adult: "成人（體位）版：通用版去審查模型寫人物和場景，姿勢照「姿勢大彙整」原文插入（體位名稱、口語說法都認得）",
};
let POSES = [];           // /api/poses：姿勢大彙整的選單

let ctx = null;           // gen.js 給的：{ prompt, family(), mode(), model(), images(), setSize(w,h), size(), generate }
let V = { list: [], i: -1 };   // 版本歷史
let busy = null;          // { ctrl: AbortController, rid }
let applied = null;       // 最後一次「套用到提示詞」的 { idea, prompt }
let peSeed = 0;
let zhCost = null;        // 上次中文對照實際多花的秒數

const F = (id) => $("#" + id);
const cur = () => V.list[V.i];

export function init(c) {
  ctx = c;
  const s = store.get("pe", {});
  V = { list: (s.list || []).slice(-30), i: Math.min(s.i ?? -1, (s.list || []).length - 1) };
  applied = s.applied || null;
  peSeed = s.seed || 0;
  zhCost = s.zhCost ?? null;
  F("peIdea").value = s.idea || "";
  // 還沒自己切過：照設定頁的「預設開啟」（2026-10-29 盤點：以前永遠照瀏覽器存的舊值，設定頁勾了沒反應）
  const def = app.state?.settings || {};
  F("peZhOn").checked = !!(s.zh ?? def.pe_zh);
  F("peThinkOn").checked = !!(s.think ?? def.pe_think);
  F("peAuto").checked = !!s.auto;
  F("peDiffOn").checked = s.diff ?? true;
  // 改寫預設：舊版只有「成人（體位）」勾選框，勾著的就換成選單的 adult
  F("pePreset").value = s.preset || (s.adult ? "adult" : "auto");
  if (!F("pePreset").value) F("pePreset").value = "auto";
  F("pePoseCtl").checked = !!s.poseCtl;
  if (s.open) F("peBox").open = true;
  loadPoses(s.pose || "auto");
  F("pePreset").onchange = () => { refreshTask(); save(); };
  F("pePose").onchange = () => { refreshTask(); save(); };
  F("pePoseCtl").onchange = save;
  // 2026-10-31 使用者：「可以從35b/9b進行自由切換 因為疑似這裡都強制使用35b」。跟設定頁「通用模型」同一個設定（pe_general）
  F("peGeneral").onchange = async () => {
    try {
      await api("/api/settings", { json: { pe_general: F("peGeneral").value } });
      await app.refreshState();
      toast("AI 改寫改用：" + F("peGeneral").selectedOptions[0].textContent, "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  // 到設定頁改這個類型的系統提示詞
  F("peEditSys").onclick = () => {
    const t = task();
    const id = t === "adult" ? (prose() ? "adult_pose" : "adult_pose_anima") : t;
    location.hash = "#settings";
    setTimeout(() => {
      const s = $("#setPrSel");
      if ([...s.options].some((o) => o.value === id)) { s.value = id; s.dispatchEvent(new Event("change")); }
      $("#setPrompts").scrollIntoView({ block: "start", behavior: "smooth" });
    }, 150);
  };

  F("peGo").onclick = () => run({ fresh: true });
  F("peCancel").onclick = cancel;
  F("peAgain").onclick = () => { peSeed++; run({ fresh: true, again: true }); };
  F("peApply").onclick = applyToPrompt;
  F("peFromPrompt").onclick = () => { F("peIdea").value = ctx.prompt.value.trim(); save(); F("peIdea").focus(); };
  F("pePrev").onclick = () => go(-1);
  F("peNext").onclick = () => go(1);
  F("peRefine").onclick = () => refine(F("peInstr").value);
  F("peInstr").addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); e.stopPropagation(); refine(F("peInstr").value); } });
  F("peIdea").addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); e.stopPropagation(); run({ fresh: true }); } });
  F("peShowThink").onclick = () => { F("peThinkBox").hidden = !F("peThinkBox").hidden; };
  F("peDiffOn").onchange = () => { renderDiff(); save(); };
  F("peRatioApply").onclick = applyRatio;
  F("peOut").addEventListener("input", () => { if (cur()) { cur().prompt = F("peOut").value; cur().edited = true; save(); renderDiff(); } });
  for (const id of ["peIdea", "peZhOn", "peThinkOn", "peAuto"]) F(id).addEventListener("change", save);
  F("peIdea").addEventListener("input", save);
  F("peBox").addEventListener("toggle", save);
  const q = F("peQuick");
  for (const [label, text] of QUICK) {
    q.append(el("button", { class: "chip", type: "button", title: text, onclick: () => {
      if (text.endsWith("…")) { F("peInstr").value = text.slice(0, -1); F("peInstr").focus(); }
      else refine(text);
    } }, label));
  }
  render();
  refreshTask();
}

function save() {
  store.set("pe", { list: V.list.slice(-30), i: V.i, applied, seed: peSeed, zhCost, idea: F("peIdea").value,
    zh: F("peZhOn").checked, think: F("peThinkOn").checked, auto: F("peAuto").checked, open: F("peBox").open, diff: F("peDiffOn").checked,
    preset: F("pePreset").value, pose: F("pePose").value, poseCtl: F("pePoseCtl").checked });
}

// 姿勢大彙整的下拉選單：依類別分組，有 3D 參考圖的標 ◆
async function loadPoses(want) {
  try {
    const r = await fetch("/api/poses").then((x) => x.json());
    POSES = r.poses || [];
  } catch { POSES = []; }
  const sel = F("pePose");
  const groups = {};
  for (const p of POSES) (groups[p.category] ||= []).push(p);
  for (const [cat, list] of Object.entries(groups)) {
    const g = el("optgroup", { label: cat });
    for (const p of list) g.append(el("option", { value: p.id, title: p.desc }, (p.control ? "◆ " : "") + p.zh));
    sel.append(g);
  }
  sel.value = [...sel.options].some((o) => o.value === want) ? want : "auto";
  refreshTask();
}

// ---------------------------------------------------------------- 改寫類型：選單選「自動」就照家族和模式決定
const I2I_MODES = ["img2img", "inpaint", "multiref"];
// Qwen 2.1、MOSS-Image 都吃英文完整句子（文生圖版）；改圖版要看參考圖，只有 Qwen 2.1 會用到
const prose = () => ctx.family() === "qwen21" || ctx.family() === "moss";
function autoTask() {
  const fam = ctx.family();
  if (fam === "qwen21" && I2I_MODES.includes(ctx.mode())) return "i2i";
  return prose() ? "t2i" : "anima";
}
export function task() {
  const p = F("pePreset")?.value || "auto";
  return p === "auto" ? autoTask() : p;
}
const isNoctAnime = () => ctx.family() === "qwen21" && /anime/i.test(ctx.model() || "") && task() === "t2i";
const PRESET_NAME = { t2i: "文生圖版", i2i: "改圖版", anima: "Anima 版", adult: "成人（體位）" };

// 選的預設跟目前的模型、模式對不上時的提示（不擋，照樣可以改寫）
function mismatch(t) {
  const qwen = ctx.family() === "qwen21", mode = ctx.mode(), withImg = I2I_MODES.includes(mode);
  if (!prose() && (t === "t2i" || t === "i2i"))
    return `目前的模型是 Anima，「${PRESET_NAME[t]}」寫的是 Qwen 用的英文長段落，Anima 看 tag 比較準。建議選「自動」或「Anima 版」。`;
  if (prose() && t === "anima") return `目前的模型是 ${qwen ? "Qwen 2.1" : "MOSS-Image"}，「Anima 版」會寫 Danbooru tag 和權重，它看不懂權重，tag 的效果也比句子差。建議選「自動」。`;
  if (!qwen && t === "i2i") return "「改圖版」是給 Qwen 2.1 看參考圖用的，目前的模型用不到。建議選「自動」。";
  if (t === "i2i" && !withImg) return "「改圖版」要看參考圖，現在的模式沒有放圖，改寫會失敗。換到圖生圖、局部重繪或多圖參考再用。";
  if (qwen && t === "t2i" && withImg) return "現在是要放圖的模式，「文生圖版」不會看你放的圖，只照想法寫。要它看圖請選「自動」或「改圖版」。";
  // 2026-10-29 盤點：MOSS 的圖生圖／局部重繪按「自動」會走文生圖版、不送原圖，以前一聲不吭
  if (!qwen && prose() && t === "t2i" && withImg) return "MOSS-Image 的改寫不會看你放的圖，只照想法寫整張畫面的描述。只想改一小處（例如換個手勢）又要其他不變，建議換 Qwen 2.1 用「改圖版」。";
  if (t === "adult" && withImg) return "「成人（體位）」只照想法寫文生圖用的提示詞，不會看你放的圖。";
  return "";
}

// 底下那行「模型：…」照後端 pe.rs::rewrite_kind 寫出這次會用哪顆。2026-10-30 官方 PE 整個拿掉：只剩通用模型，看圖的還要它的看圖檔
function modelNote(t) {
  const av = app.state?.pe?.available;
  if (!av) return "模型：通用模型 Qwen3.5-9B／Qwen3.6-35B（跟生圖助手同一顆）";
  if (!av.adult) return "模型：找不到改寫模型，到「安裝與修復」勾「AI 改寫、生圖助手」下載通用 9B";
  if (t === "i2i" && !av.vision) return `模型：通用模型 ${av.adult}（看圖要它的看圖檔 mmproj，改寫時會附下載按鈕）`;
  return `模型：通用模型 ${t === "i2i" ? av.vision : av.adult}`;
}

// 設定頁指定了 LLM 資料夾裡的某一顆：選單多一項顯示它（選回自動／35B／9B 就不再出現）
function syncGeneral(cur) {
  const sel = F("peGeneral");
  if (document.activeElement === sel) return;
  sel.querySelector("option[data-custom]")?.remove();
  if (!["auto", "35b", "9b"].includes(cur)) sel.append(el("option", { value: cur, "data-custom": "1" }, `指定：${cur}`));
  sel.value = cur;
}

export function refreshTask() {
  if (!ctx) return;
  const t = task(), qwen = ctx.family() === "qwen21";
  // 「自動」那一項直接寫出現在會用哪一份，選單上就看得出目前的預設
  F("pePreset").options[0].textContent = `自動（現在＝${PRESET_NAME[autoTask()]}）`;
  F("peAdultRow").hidden = t !== "adult";
  F("pePoseCtlRow").hidden = !(qwen || ctx.family() === "moss");
  const warn = mismatch(t);
  F("peWarn").textContent = warn ? "⚠ " + warn : "";
  F("peWarn").hidden = !warn;
  let txt = TASK_TEXT[t];
  if (t === "adult") txt += prose() ? `；${qwen ? "Qwen 2.1" : "MOSS-Image"}：寫成英文句子` :"；Anima：品質、評級、人數、姿勢 tag 由程式加，模型補外觀和場景 tag";
  if (isNoctAnime()) txt += "；Noct Q Anime：開頭固定 An anime illustration of";
  if (t === "adult" && app.state?.pe?.available && !app.state.pe.available.adult) txt += "（找不到通用版模型 Qwen3.5-9B Defiant Fable，請到「安裝與修復」勾「AI 改寫、生圖助手」下載）";
  if (t === "i2i") {
    const n = ctx.images().length;
    txt += n ? `（目前 ${n} 張圖）` : "（還沒有放圖）";
  }
  F("peTask").textContent = txt;
  F("peModelNote").textContent = modelNote(t);
  syncGeneral(app.state?.settings?.pe_general || "auto");
  F("peZhCost").textContent = zhCost != null ? `（約多 ${Math.max(1, Math.round(zhCost))} 秒）` : "（會多花幾秒）";
  const st = app.state?.pe;
  F("peBox").classList.toggle("disabled", !!st && !st.ok);
  if (st && !st.ok) F("peTask").textContent = "AI 改寫無法使用：" + st.error;
}

// ---------------------------------------------------------------- 串流請求
async function sse(body, on) {
  const ctrl = new AbortController();
  const rid = Math.random().toString(36).slice(2);
  busy = { ctrl, rid };
  setBusy(true);
  try {
    const r = await fetch("/api/pe/rewrite", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ ...body, rid }), signal: ctrl.signal });
    if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`);
    const rd = r.body.getReader(), dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await rd.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let k;
      while ((k = buf.indexOf("\n\n")) >= 0) {
        const block = buf.slice(0, k); buf = buf.slice(k + 2);
        const ev = /^event: (.*)$/m.exec(block)?.[1], data = /^data: (.*)$/m.exec(block)?.[1];
        if (ev) on(ev, data ? JSON.parse(data) : {});
      }
    }
  } catch (e) {
    if (e.name === "AbortError") on("cancelled", {});
    else on("error", { message: e.message });
  } finally {
    busy = null;
    setBusy(false);
  }
}

function setBusy(b) {
  F("peGo").hidden = b; F("peCancel").hidden = !b;
  for (const id of ["peAgain", "peRefine", "peApply"]) F(id).disabled = b;
  $$("#peQuick .chip").forEach((c) => (c.disabled = b));
  F("peBox").classList.toggle("busy", b);
}

function cancel() {
  if (!busy) return;
  fetch("/api/pe/cancel", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ rid: busy.rid }) }).catch(() => {});
  busy.ctrl.abort();
}

// 串流中的 JSON → 只顯示 rewritten_prompt 的內容
function liveText(raw) {
  const m = /"rewritten_prompt"\s*:\s*"/.exec(raw);
  if (!m) return raw.startsWith("{") ? "" : raw;
  let s = raw.slice(m.index + m[0].length);
  const end = s.search(/(?<!\\)"\s*,\s*"(wh_ratio|ratio_follow)"/);
  if (end >= 0) s = s.slice(0, end);
  return s.replace(/\\"/g, '"').replace(/\\n/g, " ").replace(/\\\\/g, "\\").replace(/"\s*}?\s*$/, "");
}

function status(msg, kind = "") {
  F("peStatus").textContent = msg || "";
  const fix = peModelFix(msg);
  if (fix) F("peStatus").append(" ", fix);
  F("peStatus").className = "pe-status " + kind;
}

// 一次改寫或微調。opts：{ fresh, again, instruction, auto }
async function run(opts = {}) {
  if (busy) return null;
  const t = task();
  const idea = F("peIdea").value.trim();
  const refining = !!opts.instruction;
  if (!refining && !idea) { toast("先在「想法」寫一句你想要的畫面", "err"); F("peIdea").focus(); return null; }
  const images = t === "i2i" ? ctx.images() : [];
  if (t === "i2i" && !images.length) { toast("這個模式要先放入圖片，AI 才看得到要改哪張圖", "err"); return null; }
  const prev = refining ? (F("peOut").value.trim() || cur()?.prompt || "") : "";
  if (refining && !prev) { toast("先改寫出第一版，才能微調", "err"); return null; }
  const body = {
    task: refining ? "refine" : t, base: t, idea, images, prev, instruction: opts.instruction || "", ratio: cur()?.wh_ratio || "",
    seed: peSeed, zh: F("peZhOn").checked, think: F("peThinkOn").checked, noct_anime: isNoctAnime(), family: ctx.family(),
  };
  if (t === "adult") {
    const qwen = ctx.family() === "qwen21";
    body.pose = F("pePose").value; body.pose_control = (qwen || ctx.family() === "moss") && F("pePoseCtl").checked; body.noct_anime = qwen && /anime/i.test(ctx.model() || "");
    if (!prose()) body.target = "anima";
  }
  if (refining) body.idea = cur()?.idea || idea;  // 接著上一版（含之前的調整）改
  F("peBox").open = true;
  F("peResult").hidden = false;
  let raw = "", think = "", result = null, failed = null;
  const out = F("peOut"), thinkBox = F("peThinkText");
  out.value = ""; out.readOnly = true; thinkBox.textContent = "";
  F("peDiff").hidden = true;
  F("peZh").hidden = true;
  status(refining ? "微調中…" : "改寫中…", "run");
  const t0 = performance.now();
  await sse(body, (ev, d) => {
    if (ev === "status") status(d.msg, "run");
    else if (ev === "content") { raw += d.text; out.value = liveText(raw); out.scrollTop = out.scrollHeight; }
    else if (ev === "reasoning") { think += d.text; thinkBox.textContent = think; status(`思考中…（${think.length} 字）`, "run"); }
    else if (ev === "reset") { raw = ""; out.value = ""; }
    else if (ev === "zh_delta") F("peZh").hidden = true;
    else if (ev === "result") result = d;
    else if (ev === "error") failed = d.message;
    else if (ev === "cancelled") failed = "已取消";
  });
  out.readOnly = false;
  if (!result) {
    status(failed === "已取消" ? "已取消。" : "改寫失敗：" + (failed || "沒有收到結果"), failed === "已取消" ? "" : "err");
    render();
    return null;
  }
  const v = {
    task: body.task, base: t, idea: result.idea || body.idea, instruction: body.instruction, prompt: result.prompt, wh_ratio: result.wh_ratio,
    ratio_follow: result.ratio_follow, zh: result.zh || "", reasoning: result.reasoning || think, seconds: result.seconds,
    load: result.load_seconds, cached: result.cached, fallback: result.fallback, note: result.note, tokens: result.tokens,
    family: ctx.family(), model: ctx.model(), at: Date.now(), pose: result.pose || null,
  };
  if (result.zh_seconds) zhCost = result.zh_seconds;
  V.list = V.list.slice(0, V.i + 1).concat([v]).slice(-30);
  V.i = V.list.length - 1;
  save();
  render();
  const secs = ((performance.now() - t0) / 1000).toFixed(1);
  status(result.fallback ? "⚠ " + result.note : (result.cached ? "用了快取（同樣的輸入改寫過）。" : `完成：${secs} 秒` + (result.load_seconds ? `（含載入模型 ${result.load_seconds.toFixed(1)} 秒）` : "")) + (result.note && !result.fallback ? "　" + result.note : ""),
    result.fallback ? "err" : "ok");
  if (t === "i2i" && images.length > 1 && !/<image\d+>/.test(result.prompt)) toast("改寫結果裡沒有 <image1> 這類標記，套用前可以自己補上，模型才分得清楚哪張做什麼");
  return v;
}

function refine(instr) {
  instr = (instr || "").trim();
  if (!instr) { toast("寫一下想怎麼調整，或點下面的快速按鈕", "err"); F("peInstr").focus(); return; }
  F("peInstr").value = "";
  run({ instruction: instr });
}

function go(d) {
  if (busy) return;
  const n = V.i + d;
  if (n < 0 || n >= V.list.length) return;
  V.i = n; save(); render();
}

// ---------------------------------------------------------------- 顯示
function render() {
  const v = cur();
  F("peResult").hidden = !v;
  F("peVer").textContent = v ? `第 ${V.i + 1} / ${V.list.length} 版` : "";
  F("pePrev").disabled = V.i <= 0;
  F("peNext").disabled = V.i >= V.list.length - 1;
  if (!v) return;
  if (!busy) F("peOut").value = v.prompt;
  const pose = v.pose ? (v.pose.id ? `姿勢：${v.pose.zh}（${v.pose.how}）${v.pose.control ? "◆參考圖" : ""}` : `姿勢：${v.pose.how}`) : "";
  const meta = [v.instruction ? `調整：${v.instruction}` : "第一次改寫", pose, v.cached ? "快取" : v.seconds != null ? `${v.seconds} 秒` : "", v.edited ? "手動改過" : "",
    v.prompt ? `${v.prompt.split(/\s+/).length} 字` : ""].filter(Boolean);
  F("peMeta").textContent = meta.join("・");
  F("peMeta").title = v.instruction || v.idea || "";
  F("peZh").hidden = !v.zh;
  F("peZh").textContent = v.zh || "";
  F("peThinkText").textContent = v.reasoning || "（這一版沒有思考過程；勾「深度思考」再改寫才會有）";
  F("peShowThink").hidden = false;
  const r = v.wh_ratio;
  F("peRatioRow").hidden = !(r || v.ratio_follow);
  if (r) {
    const [w, h] = ratioSize(r);
    F("peRatio").textContent = `建議比例 ${r}（${w}×${h}）`;
    F("peRatioApply").hidden = !ctx.canSize();
  } else if (v.ratio_follow) {
    F("peRatio").textContent = `建議跟 ${v.ratio_follow} 一樣比例`;
    F("peRatioApply").hidden = true;
  }
  renderDiff();
}

// 跟上一版比：新增的字標綠底，刪掉的字劃線
function renderDiff() {
  const v = cur(), p = V.list[V.i - 1];
  const box = F("peDiff");
  const on = F("peDiffOn").checked;
  F("peDiffRow").hidden = !p;
  if (!on || !v || !p || busy) { box.hidden = true; return; }
  const a = tokens(p.prompt), b = tokens(v.prompt);
  const ops = diff(a, b);
  box.innerHTML = "";
  let changed = 0;
  for (const [op, t] of ops) {
    if (op === "=") box.append(t);
    else { changed++; box.append(el(op === "+" ? "ins" : "del", {}, t)); }
  }
  F("peDiffInfo").textContent = changed ? `跟第 ${V.i} 版比，改了 ${changed} 處` : "跟上一版完全一樣";
  box.hidden = false;
}

const tokens = (s) => (s || "").match(/\s+|[^\s]+/g) || [];

// 逐字（以空白切）的最長共同子序列差異
function diff(a, b) {
  const n = a.length, m = b.length;
  if (n * m > 4e6) return [["-", a.join("")], ["+", b.join("")]];
  const L = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) L[i][j] = a[i] === b[j] ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1]);
  const out = [];
  const push = (op, t) => { const last = out[out.length - 1]; if (last && last[0] === op) last[1] += t; else out.push([op, t]); };
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) { push("=", a[i]); i++; j++; }
    else if (L[i + 1][j] >= L[i][j + 1]) push("-", a[i++]);
    else push("+", b[j++]);
  }
  while (i < n) push("-", a[i++]);
  while (j < m) push("+", b[j++]);
  // 只有空白的增刪不算差異
  return out.map(([op, t]) => (op !== "=" && !t.trim() ? ["=", op === "+" ? t : ""] : [op, t]));
}

// ---------------------------------------------------------------- 套用
function cleanForModel(p) {
  // Anima aesthetic 版：官方建議不要用 score_ 開頭的 tag
  if (ctx.family() === "anima" && /aesthetic/i.test(ctx.model() || "")) p = p.replace(/\bscore_\d+(_up)?\s*,\s*/g, "");
  return p;
}

function applyToPrompt() {
  const v = cur();
  if (!v) return;
  const p = cleanForModel(F("peOut").value.trim());
  ctx.prompt.value = p;
  applied = { idea: v.idea, prompt: p, ai: v.prompt, control: v.pose?.control || null };
  save();
  ctx.save();
  toast("已套用到提示詞", "ok");
}

export function ratioSize(r) {
  const [a, b] = String(r).split(":").map(Number);
  const [w0, h0] = ctx.size();
  const px = Math.max(512 * 512, w0 * h0 || 1024 * 1024);
  if (!(a > 0 && b > 0)) return [w0, h0];
  const w = Math.sqrt((px * a) / b), h = (w * b) / a;
  return [Math.max(256, Math.round(w / 32) * 32), Math.max(256, Math.round(h / 32) * 32)];
}

function applyRatio() {
  const v = cur();
  if (!v?.wh_ratio) return;
  const [w, h] = ratioSize(v.wh_ratio);
  ctx.setSize(w, h);
  toast(`尺寸改成 ${w}×${h}（${v.wh_ratio}，總像素跟原本差不多，對齊 32）`, "ok");
}

// ---------------------------------------------------------------- 給生成用
// 生成前：開了「生成時自動改寫」就先改寫（同樣的想法＋設定會直接用快取，不會重複改寫）。回傳 false＝不要生成
export async function beforeGenerate() {
  if (!F("peAuto").checked) return true;
  const idea = F("peIdea").value.trim();
  if (!idea) return true;
  if (busy) { toast("AI 還在改寫，等它寫完再生成", "err"); return false; }
  const v = await run({ fresh: true, auto: true });
  if (!v) return false;
  if (v.fallback) { toast("AI 改寫沒成功，這次先用原本的提示詞生成", "err"); return true; }
  applyToPrompt();
  return true;
}

// 存進生成參數（圖庫顯示、重用參數用）：目前的提示詞是不是從 AI 改寫來的
export function paramsFor(prompt) {
  if (!applied || !prompt) return {};
  const norm = (s) => s.replace(/\s+/g, " ").trim();
  if (norm(prompt) !== norm(applied.prompt) && !norm(prompt).startsWith(norm(applied.prompt).slice(0, 80))) return {};
  const out = { idea: applied.idea, pe_prompt: applied.ai || applied.prompt };
  // 姿勢參考圖：提示詞是這次成人改寫的結果、而且還勾著「姿勢參考圖」才送（graphs.rs 接 Fun ControlNet）
  if (applied.control && F("pePoseCtl").checked && ["qwen21", "moss"].includes(ctx.family())) out.pose_control = applied.control;
  return out;
}

// 重用參數：把原始想法和 AI 改寫結果都還原
export function restore(p) {
  if (!p?.idea) return;
  F("peIdea").value = p.idea;
  if (p.pe_prompt) {
    const last = cur();
    if (!last || last.prompt !== p.pe_prompt) {
      V.list = V.list.slice(0, V.i + 1).concat([{ task: task(), base: task(), idea: p.idea, prompt: p.pe_prompt, wh_ratio: "", cached: true,
        restored: true, instruction: "", at: Date.now() }]).slice(-30);
      V.i = V.list.length - 1;
    }
    applied = { idea: p.idea, prompt: p.prompt || p.pe_prompt, ai: p.pe_prompt };
  }
  F("peBox").open = true;
  save();
  render();
}

// 升級換模型（拿掉官方 PE、換上新的通用 9B，後端 server\api_pe.rs retire_old_models）：改寫框裡顯示進度，換階段時跳一次通知
let retirePhase = "";
function showRetire(r) {
  const box = $("#peRetire");
  if (box) {
    box.hidden = !r?.msg;
    box.textContent = r?.msg ? r.msg + (r.phase === "downloading" ? `（已下載 ${r.pct ?? 0}%）` : "") : "";
  }
  if (r?.phase && r.phase !== retirePhase) {
    retirePhase = r.phase;
    toast(r.msg, r.phase === "error" ? "err" : r.phase === "done" ? "ok" : "");
  }
}

// 狀態列顯示的模型種類（後端的 kind）
const KIND_NAME = { adult: "通用", adult_vision: "通用＋看圖" };

export function onStatus(st) {
  showRetire(st?.retire);
  const chip = $("#peChip");
  if (!chip) return;
  if (!st?.ok) { chip.hidden = true; return; }
  chip.hidden = !st.running && !st.busy;
  const where = st.ngl === 0 ? "CPU" : st.ngl >= 99 ? "GPU" : `GPU ${st.ngl} 層`;
  // 生圖助手借用同一個 llama-server：kind 是 adult_vision@9b／@35b
  const agent = /^adult_vision@/.test(st.kind || "");
  const kind = agent ? "助手 " + st.kind.split("@")[1].toUpperCase() : KIND_NAME[st.kind] || st.kind || "";
  chip.textContent = st.busy ? (agent ? "助手思考中" : "AI 改寫中") : `改寫模型 ${kind}・${where}` + (st.idle_left != null ? `・${st.idle_left} 秒後關閉` : "");
  chip.title = "AI 改寫用的 llama-server（127.0.0.1:8199）。按下生成前會自動關掉，把顯存讓給生圖。點一下立刻關閉。";
}

// ---------------------------------------------------------------- 缺讀圖用的 mmproj（生圖助手、導演台看圖）
// 錯誤訊息裡有「下載讀圖檔」（後端 pe.rs 的 MMPROJ_BUTTON）就附上按鈕：從安裝清單的網址下載到語言模型資料夾（/api/pe/mmproj），
// 進度看 /api/models/downloads。畫面會整個重畫（助手的對話紀錄），所以進度記在這裡，每個按鈕都照它顯示
const mm = { busy: false, text: "" };

function mmPaint() {
  document.querySelectorAll(".mmproj-fix").forEach((b) => {
    b.querySelector("button").disabled = mm.busy;
    b.querySelector(".note").textContent = mm.text;
  });
}

export function mmprojFix(text) {
  if (/找不到 AI 改寫用的通用模型/.test(text || "")) return peModelFix(text);
  if (!/下載讀圖檔/.test(text || "")) return null;
  const btn = el("button", { class: "akey go", type: "button" }, "下載讀圖檔");
  const box = el("div", { class: "mmproj-fix" }, btn, " ", el("span", { class: "note muted" }, mm.text));
  btn.disabled = mm.busy;
  btn.onclick = async () => {
    mm.busy = true;
    mm.text = "準備下載…";
    mmPaint();
    try {
      const r = await api("/api/pe/mmproj", { json: { brain: /qwen3\.6/i.test(text) ? "35b" : "9b" } });
      if (r.have) {
        mm.busy = false;
        mm.text = "讀圖檔已經在了，再試一次就好";
        return mmPaint();
      }
      const t = setInterval(async () => {
        let d;
        try { d = ((await api("/api/models/downloads")).downloads || []).find((x) => x.id === r.id); } catch { return; }
        if (!d) return;
        if (d.status === "run") {
          mm.text = d.note || `下載 ${r.name}：${fmtBytes(d.done)} / ${fmtBytes(d.total || r.size)}`;
          return mmPaint();
        }
        clearInterval(t);
        mm.busy = false;
        if (d.status === "done") {
          mm.text = "下載好了，再試一次就能看圖";
          toast("讀圖檔下載好了", "ok");
        } else mm.text = "下載失敗：" + (d.error || "不明原因") + "（可以再按一次，已經下載的部分會接著下載）";
        mmPaint();
      }, 1000);
    } catch (e) {
      mm.busy = false;
      mm.text = e.message;
      mmPaint();
    }
  };
  return box;
}

export function peModelFix(text) {
  if (!/找不到 AI 改寫用的通用模型/.test(text || "")) return null;
  return el("a", { class: "akey sm go", href: "/setup#models" }, "去下載通用 9B");
}

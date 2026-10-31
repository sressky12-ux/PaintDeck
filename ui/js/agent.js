// 生圖助手：用中文說想要什麼 → 助手寫提示詞、畫、看圖、挑毛病、再畫 → 交卷。
// 後端在 src-tauri\src\server\api_agent.rs（對話、背景工作）、api_cloud.rs（雲端：RunPod／任何雲端主機 SSH／網址）；規則在 src-tauri\src\agent.rs。
// websocket：type=agent（對話事件、進度）、agent_llm（模型載入／字數）、agent_lora（認識 LoRA）、agent_cloud（雲端狀態）
import { api, el, toast, store, uploadFile, openModal, copyText } from "./util.js";
import { openLightbox, pickFromGallery } from "./gallery.js";
import * as gen from "./gen.js";
import { mmprojFix } from "./pe.js";
import * as drawer from "./assetdrawer.js";
import * as loraPick from "./loras.js";
import { app } from "./state.js";

const F = (id) => document.getElementById(id);
const BRAINS = ["9b", "35b", "cloud"];
const BRAIN_NAME = { "9b": "9B", "35b": "35B", cloud: "CLOUD" };
const BRAIN_NOTE = {
  "9b": "9B：最快，本機一輪約 1～2 分鐘；挑毛病比較粗，適合一般的圖。",
  "35b": "35B 精細：數東西、分左右手、挑細節比較準；本機每輪看圖要 1～2 分鐘以上。",
  cloud: "雲端：雲端的大顯卡跑 35B＋生圖，一輪十幾秒；要先在下面接好雲端（RunPod、其他家的主機或網址都行），只能畫 MOSS-Image。",
};
const KIND_ZH = { style: "畫風", character: "角色", clothing: "服裝", concept: "概念", pose: "姿勢", detail: "細節", lighting: "光影", other: "其他" };
const FAM_NAME = { qwen21: "Qwen 2.1", moss: "MOSS-Image", anima: "Anima" };
// 工具的中文名（跟 src-tauri/src/agent_tools.rs 的 zh 一樣）
const TOOL_ZH = { models: "看模型清單", set_model: "換生圖模型", gallery: "查圖庫", image_info: "看圖的參數", look: "看圖", edit: "改圖", upscale: "放大",
  remove_bg: "去背", fix: "修臉修手", outpaint: "擴圖", video: "做影片", character_sheet: "角色多視圖", comic: "畫漫畫", assets: "看資產庫",
  trash: "移到回收區", guide: "讀寫法", docs: "讀說明書", api: "直接呼叫 API", open_page: "切換頁面" };
// 寫法庫（跟 src-tauri/src/agent.rs 的 GUIDES 一樣）
const GUIDE_ZH = { anima: "Anima", qwen: "Qwen 2.1 文生圖", qwen_edit: "Qwen 2.1 改圖", qwen_multiref: "Qwen 2.1 多圖參考", moss: "MOSS-Image",
  other: "SDXL 系", h3_video: "H3 影片" };
const toolZh = (t) => TOOL_ZH[t] || t;

let S = null;          // 目前的對話（整份）
let running = false;   // 目前的對話有背景工作在跑
let phase = "";        // think／render（跑的時候）
let live = "";         // 進度文字
let liveLlm = "";      // 模型那邊的細節（載入、字數）
let cloud = null;      // 雲端狀態
let loraFam = null;    // LoRA 清單目前是哪個家族
let shown = false;
let att = [];          // 這次要附的圖（已經放進 ComfyUI input）：{name, width, height}
const MAX_ATT = 4;
const inputUrl = (name) => `/input/${encodeURIComponent(name)}`;
// 看大圖（使用者附的圖不在圖庫裡，不能用燈箱）：點一下或按 Esc 關掉
function peek(name) {
  const close = () => { box.remove(); removeEventListener("keydown", onKey, true); };
  const onKey = (e) => { if (e.key === "Escape") { e.stopPropagation(); close(); } };
  const box = el("div", { class: "ag-peek", title: "點一下關掉", onclick: close }, el("img", { src: inputUrl(name), alt: "" }));
  addEventListener("keydown", onKey, true);
  document.body.append(box);
}

// ---------------------------------------------------------------- 載入
export async function show() {
  shown = true;
  try {
    await loadSessions(store.get("agSession", null));
  } catch (e) {
    toast("讀助手對話失敗：" + e.message, "err");
  }
  loadCloud();
  loadLoras();
}

async function loadSessions(want) {
  const r = await api("/api/agent/sessions");
  let list = r.sessions || [];
  if (!list.length) {
    const s = await api("/api/agent/session/new", { json: { settings: {} } });
    list = [{ id: s.id, title: s.title, images: 0 }];
  }
  const sel = F("agSess");
  sel.innerHTML = "";
  for (const s of list) {
    const busy = (r.running || []).includes(s.id);
    sel.append(el("option", { value: s.id }, `${busy ? "● " : ""}${s.title}${s.images ? `（${s.images} 張）` : ""}`));
  }
  const id = list.some((s) => s.id === want) ? want : list[0].id;
  sel.value = id;
  await openSession(id);
}

// 換對話、重讀都可能晚回：舊請求的回應不能蓋掉現在選的對話（不然後面的「說」會送錯對話）
let openSeq = 0, reloadSeq = 0;
async function openSession(id) {
  const seq = ++openSeq;
  ++reloadSeq; // 換對話：還在路上的重讀一律作廢
  const s = await api(`/api/agent/session/${id}`);
  if (seq !== openSeq) return;
  S = s;
  running = !!S.running;
  // 跑到一半才打開（換對話、重新整理）：照最後一件事推它現在在想、在畫還是在看圖
  const last = (S.events || []).at(-1);
  const drawing = last?.kind === "act" && ["generate", "call"].includes(last.action?.action);
  phase = running ? (drawing ? "render" : "think") : "";
  live = !running ? "" : drawing ? "畫圖中…" : last?.kind === "result" ? "看圖、逐項對照中…" : "想提示詞…";
  liveLlm = "";
  store.set("agSession", id);
  paintSettings();
  paintAll(true);
  setRunning(running);
  if (familyOf() !== loraFam) loadLoras();
}

async function reload() {
  if (!S) return;
  const seq = ++reloadSeq, os = openSeq, id = S.id;
  let s;
  try { s = await api(`/api/agent/session/${id}`); } catch { return; }
  if (seq !== reloadSeq || os !== openSeq || S?.id !== id) return;
  S = s;
  running = !!S.running;
  paintAll();
  setRunning(running);
}

const familyOf = () => S?.settings?.base?.family || "moss";

// ---------------------------------------------------------------- 設定（腦袋、輪數、張數、開關、生圖設定）
function paintSettings() {
  const st = S?.settings || {};
  const b = BRAINS.includes(st.brain) ? st.brain : "9b";
  F("agBrain").style.setProperty("--pos", BRAINS.indexOf(b));
  for (const btn of F("agBrain").querySelectorAll("button")) btn.setAttribute("aria-checked", String(btn.dataset.b === b));
  F("agBrainNote").textContent = BRAIN_NOTE[b];
  F("agBrainR").textContent = BRAIN_NAME[b];
  F("agRounds").value = String(st.rounds || 3);
  F("agCount").value = String(st.count || 2);
  F("agAuto").checked = st.auto !== false;
  // 伺服器會把舊值（true／五段時期的 mid、max）換成三段，這裡只是保險
  F("agThink").value = { true: "high", mid: "low", max: "high" }[st.think] || (["off", "low", "high"].includes(st.think) ? st.think : "off");
  F("agLoras").checked = st.loras !== false;
  const show = st.show !== false;
  F("agMode").style.setProperty("--pos", show ? 0 : 1);
  for (const btn of F("agMode").querySelectorAll("button")) btn.setAttribute("aria-checked", String((btn.dataset.v === "show") === show));
  // 動手做／只寫詞：只寫詞不畫圖，「讓我看」和輪數用不到
  const words = st.mode === "prompt";
  F("agKind").style.setProperty("--pos", words ? 1 : 0);
  for (const btn of F("agKind").querySelectorAll("button")) btn.setAttribute("aria-checked", String((btn.dataset.k === "prompt") === words));
  F("agMode").hidden = words;
  F("agTarget").hidden = !words;
  F("agTarget").value = GUIDE_ZH[st.target] ? st.target : "auto";
  baseInfo();
}

function baseInfo() {
  const b = S?.settings?.base;
  const box = F("agBaseInfo");
  if (S?.settings?.mode === "prompt") {
    box.textContent = "只寫詞不需要啟動 ComfyUI；可在上方選擇寫法。";
    return;
  }
  if (!b) { box.textContent = "還沒設定：先到「生成」頁選好模型和設定，再回來按下面的按鈕。"; return; }
  const fam = FAM_NAME[b.family] || b.family || "";
  let t = `${b.model}（${fam}）・${b.steps} 步・CFG ${b.cfg}${b.loras?.length ? `・固定掛 LoRA ${b.loras.length} 個` : ""}`;
  if (S.settings.brain === "cloud" && b.family !== "moss") t += "\n雲端只能畫 MOSS-Image：換一個 MOSS 的模型，或把腦袋切回本機。";
  box.textContent = t;
  box.style.whiteSpace = "pre-wrap";
  paintBaseEdit(b);
}

// 生圖設定直接在助手頁改：模型只列助手能用的三家；換模型走後端（家族預設跟助手自己換模型同一套）
function paintBaseEdit(b) {
  const box = F("agBaseEdit");
  box.hidden = !b || S?.settings?.mode === "prompt";
  if (box.hidden) return;
  const m = app.state?.models || {};
  const fams = m.unet_family || {};
  const fill = (sel, items, cur, label = (x) => x) => {
    sel.innerHTML = "";
    const list = items.includes(cur) || !cur ? items : [cur, ...items];
    for (const it of list) sel.append(el("option", { value: it }, label(it)));
    sel.value = cur || "";
  };
  const models = Object.keys(fams).filter((n) => FAM_NAME[fams[n]]).sort();
  fill(F("agBModel"), models, b.model, (n) => `${n.replace(/\.safetensors$/i, "")}（${FAM_NAME[fams[n]] || fams[n] || "?"}）`);
  fill(F("agBSampler"), m.samplers || [], b.sampler);
  fill(F("agBSched"), m.schedulers || [], b.scheduler);
  F("agBSteps").value = b.steps ?? "";
  F("agBCfg").value = b.cfg ?? "";
  if (document.activeElement !== F("agBNeg")) F("agBNeg").value = b.negative || "";
  const wrap = F("agBLoras");
  wrap.innerHTML = "";
  (b.loras || []).forEach((l, i) => {
    const str = el("input", { type: "number", min: "-3", max: "3", step: "0.05", value: l.strength ?? 1, title: "強度" });
    str.onchange = () => setBase({ loras: b.loras.map((x, k) => (k === i ? { ...x, strength: +str.value } : x)) });
    wrap.append(el("span", { class: "ag-base-lora", title: l.name }, String(l.name).replace(/^.*[\\/]/, "").replace(/\.safetensors$/i, ""), str,
      el("button", { class: "link", type: "button", title: "拿掉", onclick: () => setBase({ loras: b.loras.filter((_, k) => k !== i) }) }, "✕")));
  });
  wrap.append(el("button", { class: "akey sm", type: "button", onclick: () => loraPick.openPicker((l) => {
    if ((b.loras || []).some((x) => x.name === l.name)) return toast("這個 LoRA 已經掛上了");
    setBase({ loras: [...(b.loras || []), { name: l.name, strength: l.strength ?? 0.8 }] });
  }, b.family) }, "＋ LoRA"));
}

// 只改生圖設定的幾個欄位，其他照舊
function setBase(patch) {
  const b = S?.settings?.base;
  if (b) saveSettings({ base: { ...b, ...patch } });
}

async function saveSettings(patch) {
  if (!S) return;
  try {
    const r = await api("/api/agent/session/settings", { json: { id: S.id, settings: patch } });
    S.settings = r.settings;
  } catch (e) {
    toast("存設定失敗：" + e.message, "err");
  }
  paintSettings();
  paintReadout();
}

// ---------------------------------------------------------------- 對話螢幕
const nearBottom = (box) => box.scrollHeight - box.scrollTop - box.clientHeight < 80;
const secs = (s) => (s == null ? "" : s < 60 ? `${Math.round(s)}s` : `${Math.floor(s / 60)}m${String(Math.round(s % 60)).padStart(2, "0")}s`);

function paintAll(jump) {
  paintTranscript(jump);
  paintStrip();
  paintReadout();
}

function checkList(items) {
  if (!items?.length) return null;
  return el("ul", { class: "ag-check" }, items.map((t) => el("li", { class: /:\s*no\b/i.test(t) ? "no" : "yes" }, t)));
}

function thoughtBox(t) {
  if (!t) return null;
  return el("details", {}, el("summary", {}, `它的思考過程（${t.length} 字）`), el("pre", {}, t));
}

// 標題：英文用螢幕字（VT323），後面的中文用一般字體
const who = (label, zh) => el("span", { class: "who" }, label, zh ? el("small", {}, " · " + zh) : null);

function eventNodes(ev, prevResult) {
  const out = [];
  switch (ev.kind) {
    case "user": {
      const imgs = (ev.images || []).map((im) => el("figure", {},
        el("img", { src: inputUrl(im.name), alt: `你的圖 ${im.n}`, loading: "lazy", onclick: () => peek(im.name) }),
        el("figcaption", {}, `圖 ${im.n}`)));
      out.push(el("div", { class: "ag-msg user" }, who("YOU", ev.mid ? "插話（下一輪照改）" : ""), el("span", { translate: "no" }, ev.text),
        imgs.length ? el("div", { class: "ag-uimgs" }, ...imgs) : null));
      break;
    }
    case "act": {
      const a = ev.action || {};
      const look = prevResult ? `看完第 ${prevResult} 輪` : "";
      if (a.action === "generate") {
        if (a.say || a.check?.length) {
          out.push(el("div", { class: "ag-msg agent" }, who("AGENT", look), el("span", { translate: "no" }, a.say || ""), checkList(a.check)));
        }
        const loras = (a.loras || []).map((l) => `${String(l.name).replace(/^.*[\\/]/, "").replace(/\.safetensors$/i, "")}@${l.strength}`);
        // 改使用者的一張圖（圖生圖）：尺寸跟著原圖，不是它寫的長寬
        const ownSize = a.refs?.length && (a.denoise != null || a.refs.length === 1);
        out.push(el("div", { class: "ag-tool" },
          el("span", { class: "t" }, `▶ GENERATE R${ev.round || "?"}`),
          `${ownSize ? "照原圖比例" : `${a.width}×${a.height}`} · ${S.settings.count} 張${ev.secs ? ` · 想 ${secs(ev.secs)}` : ""}`,
          a.changes ? el("div", {}, "改：" + a.changes) : null,
          a.refs?.length ? el("div", {}, `用你的圖：${a.refs.map((n) => "圖 " + n).join("、")}${a.denoise != null ? `（重繪強度 ${a.denoise}）` : ""}`) : null,
          loras.length ? el("div", {}, "LoRA：" + loras.join("、")) : null,
          el("details", {}, el("summary", {}, "提示詞"), el("pre", {}, ev.used_prompt || a.prompt || "")),
          thoughtBox(ev.thought)));
      } else if (a.action === "call") {
        if (a.say) out.push(el("div", { class: "ag-msg agent" }, who("AGENT", look), el("span", { translate: "no" }, a.say)));
        const args = JSON.stringify(a.args || {});
        out.push(el("div", { class: "ag-tool" },
          el("span", { class: "t" }, `▶ TOOL`), `${toolZh(a.tool)}${ev.secs ? ` · 想 ${secs(ev.secs)}` : ""}`,
          args !== "{}" ? el("details", {}, el("summary", {}, "參數"), el("pre", {}, args)) : null,
          thoughtBox(ev.thought)));
      } else if (a.action === "prompt") {
        out.push(promptCard(a, ev));
      } else if (a.action === "finish") {
        out.push(el("div", { class: "ag-msg agent" },
          who("AGENT", `交卷${a.best?.length ? `：選 #${a.best.join("、#")}` : ""}`),
          el("span", { translate: "no" }, a.message || ""), checkList(a.check)));
        const t = thoughtBox(ev.thought);
        if (t) out.push(el("div", { class: "ag-tool note" }, t));
      } else {
        out.push(el("div", { class: "ag-msg agent" }, who("AGENT"), el("span", { translate: "no" }, a.message || "")));
        const t = thoughtBox(ev.thought);
        if (t) out.push(el("div", { class: "ag-tool note" }, t));
      }
      break;
    }
    case "tool":
      out.push(el("div", { class: "ag-tool" + (ev.ok === false ? " err" : "") },
        el("span", { class: "t" }, ev.ok === false ? "✗ TOOL" : "■ TOOL"), ev.zh || toolZh(ev.tool), ev.secs ? ` · ${secs(ev.secs)}` : "",
        ev.text && ev.ok !== false ? el("details", {}, el("summary", {}, "給助手的結果"), el("pre", {}, ev.text)) : null));
      break;
    case "confirm": {
      const wait = ev.state === "wait";
      const args = JSON.stringify(ev.call?.args || {});
      out.push(el("div", { class: "ag-confirm" + (wait ? "" : " done"), role: wait ? "alert" : null },
        el("b", {}, wait ? "助手想做這件事，要你准：" : ev.state === "yes" ? "你准了：" : "你沒准："),
        el("div", {}, `${toolZh(ev.call?.tool)}・${ev.why || ""}`),
        args !== "{}" ? el("details", {}, el("summary", {}, "細節"), el("pre", {}, args)) : null,
        wait && !running ? el("div", { class: "row" },
          el("button", { class: "akey sm warn", type: "button", onclick: () => answer(true) }, "准"),
          el("button", { class: "akey sm", type: "button", onclick: () => answer(false) }, "不准")) : null));
      break;
    }
    case "result": {
      const n = ev.ids?.length || 0;
      out.push(el("div", { class: "ag-tool" + (n ? "" : " err") },
        el("span", { class: "t" }, ev.tool === "look" ? "■ LOOK" : `■ R${ev.round} DONE`),
        ev.tool === "look" ? `看 #${(ev.ids || []).join("、#")}` : `${ev.tool ? toolZh(ev.tool) + " · " : ""}${n} 張 · ${secs(ev.secs)}${n ? ` · #${ev.ids.join("、#")}` : ""}`,
        ev.errors?.length ? el("div", { style: "color:var(--ph-red)" }, ev.errors.join("；")) : null));
      break;
    }
    case "note":
      out.push(el("div", { class: "ag-tool note" }, "· " + ev.text));
      break;
    case "error":
      out.push(el("div", { class: "ag-tool err" }, "✗ " + ev.text, mmprojFix(ev.text)));
      break;
  }
  return out;
}

// 只寫詞的成果：提示詞卡（填進主視窗的生成頁、複製）
function promptCard(a, ev) {
  const size = a.width && a.height ? ` · ${a.width}×${a.height}` : "";
  const fill = async () => {
    try {
      await api("/api/agent/fill", { json: { prompt: a.prompt, negative: a.negative || "", width: a.width, height: a.height } });
      toast("已填進主視窗的生成頁", "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  return el("div", { class: "ag-prompt" },
    el("div", { class: "hd" }, el("b", {}, "提示詞卡"), ` · ${GUIDE_ZH[a.guide] || a.guide || ""}${size}${ev.secs ? ` · 想 ${secs(ev.secs)}` : ""}`),
    a.note ? el("div", { class: "note", translate: "no" }, a.note) : null,
    el("pre", { translate: "no" }, a.prompt || ""),
    a.negative ? el("div", { class: "neg", translate: "no" }, "負面：" + a.negative) : null,
    el("div", { class: "row" },
      el("button", { class: "akey sm", type: "button", onclick: fill }, "填入生成頁"),
      el("button", { class: "akey sm", type: "button", onclick: () => copyText(a.prompt || "").then(() => toast("已複製提示詞", "ok")) }, "複製")),
    thoughtBox(ev.thought));
}

function paintTranscript(jump) {
  const box = F("agScreen");
  const stick = jump || nearBottom(box);
  box.innerHTML = "";
  const evs = S?.events || [];
  if (!evs.length) {
    box.append(el("div", { class: "ag-empty" },
      "用中文說你想要什麼圖，越具體越好（人物、衣服、地點、光線、構圖）。\n助手會自己寫英文提示詞、畫、看圖對照你的每一項要求，不對就改了再畫，最後把最好的交給你。\n畫到一半也可以插話，下一輪就照你說的改。\n也能叫它做別的：放大、去背、修臉修手、改圖、擴圖、角色三視圖、做影片、畫漫畫、查圖庫、換模型…花錢、刪東西、改設定之前會先問你。"));
    box.lastChild.style.whiteSpace = "pre-line";
  }
  let lastRound = 0;
  for (const ev of evs) {
    box.append(...eventNodes(ev, ev.kind === "act" ? lastRound : 0));
    if (ev.kind === "result") lastRound = ev.round;
    if (ev.kind === "user" && !ev.mid) lastRound = 0;
  }
  if (running) box.append(el("div", { class: "ag-live", id: "agLive" }, liveText()));
  if (stick) box.scrollTop = box.scrollHeight;
}

const liveText = () => [live || "準備中…", liveLlm].filter(Boolean).join("　");

function paintLive() {
  const n = F("agLive");
  if (n) n.textContent = liveText();
  else if (running) paintTranscript();
}

// ---------------------------------------------------------------- 幻燈片（每一輪的圖，新的在上面）
function paintStrip() {
  const box = F("agStrip");
  box.innerHTML = "";
  const evs = S?.events || [];
  const best = new Set(evs.filter((e) => e.kind === "act" && e.action?.action === "finish").flatMap((e) => e.action.best || []));
  const size = {};
  for (const e of evs) if (e.kind === "act" && e.action?.action === "generate") size[e.round] = [e.action.width, e.action.height];
  const results = evs.filter((e) => e.kind === "result" && e.tool !== "look");
  const all = results.flatMap((e) => e.ids || []);
  const rows = [];
  if (running && phase === "render") {
    const r = results.length + 1, [w, h] = size[r] || [2, 3], n = S.settings.count || 2;
    rows.push(el("div", { class: "ag-row", style: `--n:${n}` }, el("div", { class: "ag-rnd" }, `R${r}`, el("small", {}, "畫圖中")),
      Array.from({ length: n }, () => el("div", { class: "ag-slide wait" }, el("div", { style: `aspect-ratio:${w} / ${h}` }, el("span", { class: "spinner" }))))));
  }
  for (const e of results.slice().reverse()) {
    const ids = e.ids || [];
    if (!ids.length) continue;
    const [w, h] = size[e.round] || [2, 3];
    rows.push(el("div", { class: "ag-row", style: `--n:${Math.min(4, Math.max(2, ids.length))}` },
      el("div", { class: "ag-rnd" }, `R${e.round}`, el("small", {}, secs(e.secs))),
      ids.map((id) => el("button", { type: "button", class: "ag-slide" + (best.has(id) ? " best" : ""), title: best.has(id) ? "助手選中的" : "點開看大圖",
        "aria-label": `第 ${e.round} 輪 #${id}${best.has(id) ? "，助手選中" : ""}`, onclick: () => openImages(all, id) },
        el("img", { src: `/thumb/${id}`, alt: "", loading: "lazy", style: `--ar:${w} / ${h}` }), el("span", {}, `#${id}`)))));
  }
  if (!rows.length) box.append(el("div", { class: "ag-strip-empty" }, "每一輪畫好的圖會排在這裡，助手選中的會框起來。"));
  box.append(...rows);
}

async function openImages(ids, id) {
  const recs = (await Promise.all(ids.map((i) => api(`/api/image/${i}`).catch(() => null)))).filter(Boolean);
  if (!recs.length) return toast("這些圖在圖庫裡找不到了（可能被刪掉）", "err");
  openLightbox(recs, Math.max(0, recs.findIndex((r) => r.id === id)));
}

// ---------------------------------------------------------------- 讀數列、狀態
function paintReadout() {
  const evs = S?.events || [];
  let i = evs.length - 1;
  while (i >= 0 && !(evs[i].kind === "user" && !evs[i].mid)) i--;
  const since = evs.slice(i + 1);
  const used = since.filter((e) => e.kind === "result" && !e.tool).length;
  F("agRoundR").textContent = `${used}/${S?.settings?.auto === false ? 1 : S?.settings?.rounds || 3}`;
  const lastAct = evs.slice().reverse().find((e) => e.kind === "act");
  F("agSecsR").textContent = lastAct?.secs ? secs(lastAct.secs) : "—";
  const st = F("agStateR");
  const last = evs[evs.length - 1];
  const [t, c] = running ? (phase === "render" ? ["RENDER", ""] : ["THINK", ""])
    : last?.kind === "error" ? ["ERROR", "r"] : last?.kind === "confirm" && last.state === "wait" ? ["WAIT", ""]
    : last?.kind === "act" && last.action?.action === "finish" ? ["DONE", "g"] : ["IDLE", "g"];
  st.textContent = t;
  st.className = c;
}

function status(msg, cls = "g") {
  const s = F("agStatus");
  s.textContent = msg;
  s.className = `vfd ${cls} ag-status`;
}

function setRunning(on) {
  running = on;
  F("agStop").hidden = !on;
  F("agGo").classList.toggle("busy", on);
  F("agGo").querySelector("span").textContent = on ? "插話" : "交給助手";
  F("agGo").title = on ? "助手還在做：你說的話會在下一輪照改" : "";
  for (const id of ["agNew", "agDel"]) F(id).disabled = on;
  if (on) status(live || "助手工作中…", "amb");
  else if (F("agStatus").classList.contains("amb")) status("待機");
  paintReadout();
}

// ---------------------------------------------------------------- 說話、停止
async function say() {
  if (F("agGo").disabled) return;  // 上一句還在送（Ctrl+Enter 連按、按住不放）
  const text = F("agSay").value.trim();
  if ((!text && !att.length) || !S) return;
  if (S.settings.mode !== "prompt" && !S.settings.base) return toast("還沒選生圖模型：先按右邊的「用生成頁目前的設定」", "err");
  if (S.settings.brain === "cloud" && cloud?.phase !== "ready") return toast("雲端還沒開好：先按「開雲端」，等燈號變綠；或把腦袋切回 9B／35B", "err");
  F("agGo").disabled = true;
  try {
    const r = await api("/api/agent/say", { json: { id: S.id, text, images: att } });
    F("agSay").value = "";
    att = [];
    paintAtt();
    if (r.queued) toast("收到，助手下一輪會照你的話改", "ok");
    else {
      phase = "think"; live = "想提示詞…"; liveLlm = "";
      setRunning(true);
      paintTranscript(true);
      // 萬一很快就出錯結束了（websocket 的 end 比這裡先到），以伺服器的狀態為準
      await reload();
      refreshTitles();
    }
  } catch (e) {
    toast(e.message, "err");
  } finally {
    F("agGo").disabled = false;
  }
}

async function answer(yes) {
  if (!S || running) return;
  try {
    await api("/api/agent/confirm", { json: { id: S.id, ok: yes } });
    phase = "render"; live = yes ? "照你准的去做…" : "換個方式想…"; liveLlm = "";
    setRunning(true);
    paintTranscript(true);
    await reload();
  } catch (e) {
    toast(e.message, "err");
  }
}

async function refreshTitles() {
  try {
    const r = await api("/api/agent/sessions");
    const sel = F("agSess"), cur = sel.value;
    sel.innerHTML = "";
    for (const s of r.sessions || []) {
      const busy = (r.running || []).includes(s.id);
      sel.append(el("option", { value: s.id }, `${busy ? "● " : ""}${s.title}${s.images ? `（${s.images} 張）` : ""}`));
    }
    sel.value = cur;
  } catch { /* 伺服器重開中 */ }
}

// ---------------------------------------------------------------- 雲端
async function loadCloud() {
  try { cloud = await api("/api/agent/cloud"); } catch { return; }
  paintCloud();
}

// 雲端三種連法：runpod（自動租、用完刪機）／ssh（任何一家雲端租好的主機）／url（現成的網址）
const CLOUD_WAYS = ["runpod", "ssh", "url"];
const CLOUD_BTN = {
  runpod: ["開雲端", "關機刪除"], ssh: ["連線並安裝", "停止"], url: ["連線", "斷線"],
};
const STOP_ASK = {
  runpod: "關機並刪除雲端機器？刪掉就不再計費；下次開要重新下載模型。",
  ssh: "停止主機上的腦袋和 ComfyUI 並斷線？裝好的檔案會留著，下次連線很快。\n主機本身要到雲端服務的網站關機才會停止計費（設定裡勾了「順便叫主機關機」的會送關機指令）。",
  url: "斷開雲端網址？",
};
// 目前這台是哪種：開著（或上次留下）的機器照它自己的，不然照設定
const cloudKind = () => cloud?.kind || cloud?.provider || "runpod";
const hostOf = (u) => { try { return new URL(u).host; } catch { return u || ""; } };

function paintCloud() {
  const st = cloud;
  if (!st) return;
  const kind = cloudKind(), c = st.cfg || {};
  const dot = F("agCloudDot");
  dot.className = "ag-dot" + ({ ready: " on", starting: " busy", stopping: " busy", error: " err", left: " err" }[st.phase] || "");
  F("agCloudMsg").textContent = st.msg || { off: "沒開", ready: "雲端好了", starting: "開機中…", stopping: "關機中…" }[st.phase] || st.phase;
  F("agCloudWay").textContent = "連法：" + ({
    runpod: "RunPod 自動租卡",
    ssh: `自己的主機 ${c.ssh_user || "root"}@${c.ssh_host || "（還沒填）"}${c.ssh_port && c.ssh_port !== 22 ? ":" + c.ssh_port : ""}`,
    url: `直接連網址（${hostOf(c.comfy_url) || "還沒填"}）`,
  }[kind]);
  F("agCloudGpuRow").hidden = kind !== "runpod";
  let meta = "";
  if (kind === "runpod") {
    meta = `${st.gpu}・每小時約 US$${(+st.price).toFixed(2)}`;
    if (st.pod) meta += `・已開 ${st.minutes} 分鐘・目前約 US$${(+st.cost).toFixed(2)}`;
  } else if (kind === "ssh" && st.pod) {
    meta = st.gpu + (+st.price > 0 ? `・每小時約 US$${(+st.price).toFixed(2)}・目前約 US$${(+st.cost).toFixed(2)}` : "") + `・已連 ${st.minutes} 分鐘`;
  }
  if (st.idle_left != null) meta += `・再閒置 ${st.idle_left} 分鐘自動關`;
  if (st.missing && !st.pod) meta += (meta ? "\n" : "") + st.missing;
  F("agCloudMeta").textContent = meta.replace(/^・/, "");
  F("agCloudMeta").style.whiteSpace = "pre-wrap";
  const busy = ["starting", "ready", "stopping"].includes(st.phase);
  F("agCloudStart").disabled = busy || (!st.has_key && !st.pod);
  F("agCloudStart").textContent = st.phase === "left" ? "接回來" : CLOUD_BTN[kind][0];
  F("agCloudStop").textContent = CLOUD_BTN[kind][1];
  F("agCloudStop").disabled = !st.pod && !["starting", "ready"].includes(st.phase);
  F("agCloudCfg").disabled = busy || !!st.pod;
  F("agCloudIdleRow").hidden = !st.idle_on && kind !== "runpod";
  const idle = F("agCloudIdle");
  if (![...idle.options].some((o) => +o.value === st.idle_min)) idle.append(el("option", {}, String(st.idle_min)));
  idle.value = String(st.idle_min);
  // 選顯卡：機器開著（或上次留下的）就鎖住，顯示實際租到的那張
  const sel = F("agCloudGpu");
  fillGpus(sel, st);
  if (st.pod || busy) sel.value = st.gpu_key;
  sel.disabled = busy || !!st.pod;
  paintGpuNote();
  if (busy) F("agCloudConfirm").hidden = true;
}

export const priceRange = (g) => g.price_lo === g.price_hi ? g.price_lo.toFixed(2) : `${g.price_lo.toFixed(2)}～${g.price_hi.toFixed(2)}`;

// RunPod 目錄的貨量：null＝目錄裡沒有／還沒抓到
const stockTxt = (g) => g.stock == null ? "" : ({ High: "・有貨", Medium: "・有貨", Low: "・剩不多" }[g.stock] || "・目前租光");

// 顯卡下拉選單（助手、H3 共用）：推薦三張＋RunPod 目錄自動抓到的其他卡；價格或貨量變了才重畫，保留目前選的
export function fillGpus(sel, st) {
  const gpus = st.gpus || [];
  const sig = JSON.stringify(gpus.map((g) => [g.key, g.price_lo, g.price_hi, g.stock]));
  if (sel.dataset.sig === sig) return;
  const cur = sel.value || st.gpu_key;
  const opt = (g) => el("option", { value: g.key }, `${g.name}・每小時約 US$${priceRange(g)}${stockTxt(g)}`);
  const more = gpus.filter((g) => !g.preset);
  sel.replaceChildren(el("optgroup", { label: "推薦" }, ...gpus.filter((g) => g.preset).map(opt)),
    ...(more.length ? [el("optgroup", { label: "其他卡（RunPod 自動抓的，還沒實測）" }, ...more.map(opt))] : []));
  sel.value = gpus.some((g) => g.key === cur) ? cur : st.gpu_key;
  sel.dataset.sig = sig;
}

// 選中那張卡的說明：備註、租光提醒、價格多久前更新
export function gpuNote(g, st) {
  const out = [];
  if (g?.note) out.push(g.note);
  if (g && g.stock === "") out.push(`${g.name} 現在 RunPod 上租光了：開機多半會失敗（不會計費），等一下或換一張`);
  const at = st?.gpus_at;
  out.push(at ? `價格、貨量 ${Math.max(0, Math.round((Date.now() / 1000 - at) / 60))} 分鐘前從 RunPod 更新` : "價格還沒從 RunPod 更新（按 ↻）");
  return out.join("\n");
}

// ↻：馬上重抓 RunPod 顯卡目錄，抓完叫 done 重畫
export async function refreshGpus(done) {
  try {
    const r = await api("/api/agent/cloud/gpus");
    toast(`顯卡清單更新好了（${r.count} 張可以選）`, "ok");
  } catch (e) { toast(e.message, "err"); }
  done?.();
}

// 選中的那張卡：說明＋確認框裡的價格（機器開著就照實際租到的價格）
function paintGpuNote() {
  const g = (cloud?.gpus || []).find((x) => x.key === F("agCloudGpu").value);
  const p = cloud?.pod_packs || cloud?.packs || { agent: true };
  const gb = 32 * !!p.agent + 42 * !!p.h3 + 32 * !!p.h3x;
  F("agCloudDl").textContent = `開著就一直計費，按「關機刪除」或閒置太久自動關才會停。每次開機都會在雲端重新下載約 ${gb} GB 模型（機房網路很快），開機到能用大約 ${gb > 40 ? "5～10" : "3～5"} 分鐘，這段時間也算錢。`
    + (p.agent && p.h3 && g && g.vram < 90 ? `\n${g.name} 放不下生圖助手＋H3：選 RTX PRO 6000 96GB，或到「雲端設定…」只勾一個。` : "");
  F("agCloudDl").style.whiteSpace = "pre-wrap";
  F("agCloudGpuNote").textContent = gpuNote(g, cloud);
  F("agCloudGpuNote").style.whiteSpace = "pre-wrap";
  F("agCloudPrice").textContent = cloud?.pod ? `${cloud.gpu}，每小時約 US$${(+cloud.price).toFixed(2)}` : g ? `${g.name}，每小時約 US$${priceRange(g)}` : "";
}

async function startCloud(body) {
  const kind = cloudKind(), again = !!cloud?.pod;
  try {
    await api("/api/agent/cloud/start", { json: body });
    toast(again ? "接回上次的雲端機器中…" : {
      runpod: "雲端開機中：下載模型＋安裝，大約 3～5 分鐘",
      ssh: "連線中：第一次會在主機上安裝、下載約 32 GB 模型，快慢看那家的網路；裝過的話一兩分鐘就好",
      url: "連線中…",
    }[kind], "ok");
  } catch (e) { toast(e.message, "err"); }
  loadCloud();
}

// ---------------------------------------------------------------- 雲端設定視窗
let acWay = "runpod";
function setWay(v) {
  acWay = CLOUD_WAYS.includes(v) ? v : "runpod";
  F("acWay").style.setProperty("--pos", CLOUD_WAYS.indexOf(acWay));
  for (const b of F("acWay").querySelectorAll("button")) b.setAttribute("aria-checked", String(b.dataset.v === acWay));
  F("acRunpod").hidden = acWay !== "runpod";
  F("acSsh").hidden = acWay !== "ssh";
  F("acUrl").hidden = acWay !== "url";
}

function acMsg(text, cls = "") {
  const m = F("acMsg");
  m.hidden = !text;
  m.textContent = text;
  m.className = "pe-status" + (cls ? " " + cls : "");
}

// 金鑰欄位只顯示「有沒有存」，值不會回到網頁
function paintKeys() {
  const c = cloud?.cfg || {};
  F("acRpHas").textContent = c.has_runpod_key ? "已存一組（要換才貼新的）" : c.runpod_env ? "目前用環境變數 RUNPOD_API_KEY" : "還沒填";
  F("acRpClear").hidden = !c.has_runpod_key;
  F("acComfyKey").placeholder = c.has_comfy_key ? "已存（要換才填）" : "";
  F("acLlmKey").placeholder = c.has_llm_key ? "已存（要換才填）" : "";
  F("acKeysHas").textContent = c.has_comfy_key || c.has_llm_key ? "金鑰欄位空著＝沿用已存的；要清掉已存的金鑰，在欄位裡打一個減號 - 再儲存。" : "";
  F("acPub").value = cloud?.pubkey || "";
  for (const id of ["acRpKey", "acComfyKey", "acLlmKey"]) F(id).value = "";
}

/** H3 影片台的「雲端設定…」：同一個視窗（先讀最新狀態，助手頁沒開過也能用） */
export async function openCloudSettings() {
  await loadCloud();
  openCloudCfg();
}

function openCloudCfg() {
  const c = cloud?.cfg || {};
  setWay(c.provider);
  F("acSshCmd").value = "";
  F("acHost").value = c.ssh_host || ""; F("acPort").value = c.ssh_port || 22; F("acUser").value = c.ssh_user || "";
  F("acDir").value = c.ssh_dir || ""; F("acKey").value = c.ssh_key || ""; F("acPrice").value = c.ssh_price || ""; F("acShutdown").checked = !!c.ssh_shutdown;
  F("acComfy").value = c.comfy_url || ""; F("acLlm").value = c.llm_url || ""; F("acModel").value = c.llm_model || "";
  const p = cloud?.packs || { agent: true };
  F("acPackAgent").checked = !!p.agent; F("acPackH3").checked = !!p.h3; F("acPackH3x").checked = !!p.h3x;
  paintKeys();
  acMsg("");
  openModal("#agCloudModal");
}

// 「ssh -p 41234 root@123.45.67.89 -L 8080:localhost:8080」→ 主機、埠、帳號（各家網站給的格式都差不多）
function parseSsh(cmd) {
  const t = cmd.trim().split(/\s+/);
  let port = null, user = null, host = null;
  for (let i = 0; i < t.length; i++) {
    const w = t[i];
    if (w === "-p" && t[i + 1]) { port = +t[++i]; continue; }
    if (/^-p\d+$/.test(w)) { port = +w.slice(2); continue; }
    if (w === "-l" && t[i + 1]) { user = t[++i]; continue; }
    if (/^-[LRDiIoJFEcmOQSWbBw]$/.test(w)) { i++; continue; }  // 帶參數的選項：連參數一起跳過
    if (w.startsWith("-") || w === "ssh") continue;
    const m = w.match(/^([^@\s]+)@([^@\s]+)$/);
    if (m) { user = m[1]; host = m[2]; } else if (!host && /^[\w.-]+$/.test(w)) host = w;
  }
  if (host && /:\d+$/.test(host) && !port) { port = +host.split(":").pop(); host = host.replace(/:\d+$/, ""); }
  return { port, user, host };
}

function cfgBody() {
  const b = {
    provider: acWay,
    ssh_host: F("acHost").value.trim(), ssh_port: +F("acPort").value || 22, ssh_user: F("acUser").value.trim(),
    ssh_dir: F("acDir").value.trim(), ssh_key: F("acKey").value.trim().replace(/^"|"$/g, ""), ssh_price: +F("acPrice").value || 0,
    ssh_shutdown: F("acShutdown").checked,
    comfy_url: F("acComfy").value.trim(), llm_url: F("acLlm").value.trim(), llm_model: F("acModel").value.trim(),
    // 雲端要裝的功能包；什麼都沒勾＝只有生圖助手（後端也這樣認）
    packs: { agent: F("acPackAgent").checked || !(F("acPackH3").checked || F("acPackH3x").checked), h3: F("acPackH3").checked || F("acPackH3x").checked, h3x: F("acPackH3x").checked },
  };
  // 金鑰：空著＝不動；打一個 - ＝清掉
  for (const [id, k] of [["acRpKey", "runpod_key"], ["acComfyKey", "comfy_key"], ["acLlmKey", "llm_key"]]) {
    const v = F(id).value.trim();
    if (v) b[k] = v === "-" ? "" : v;
  }
  return b;
}

async function saveCfg() {
  try {
    cloud = await api("/api/agent/cloud/config", { json: cfgBody() });
    paintCloud();
    paintKeys();
    document.dispatchEvent(new CustomEvent("aw-cloud-cfg"));  // H3 影片台跟著重讀
    return true;
  } catch (e) { acMsg(e.message, "err"); return false; }
}

// ---------------------------------------------------------------- LoRA 認識度
async function loadLoras() {
  const fam = familyOf();
  loraFam = fam;
  let r;
  try { r = await api(`/api/agent/loras?family=${encodeURIComponent(fam)}`); } catch (e) { F("agLoraSum").textContent = "讀不到 LoRA 清單：" + e.message; return; }
  const list = r.loras || [];
  const pick = list.filter((l) => l.pickable !== false);
  const known = pick.filter((l) => l.known).length;
  const off = list.length - pick.length;
  F("agLoraSum").textContent = list.length
    ? `${FAM_NAME[fam] || fam} 能用的 LoRA：認識 ${known} / ${pick.length} 個。助手從認識的裡面挑得比較準；不認識的它只能看檔名猜。` +
      (off ? `另外 ${off} 個是調年齡的，助手不會自己掛（你在生成頁親手掛才會用）。` : "")
    : `還沒有 ${FAM_NAME[fam] || fam} 能用的 LoRA。`;
  setLearning(!!r.learning);
  const box = F("agLoraList");
  box.innerHTML = "";
  for (const l of list) {
    const name = l.title || String(l.name).replace(/^.*[\\/]/, "").replace(/\.safetensors$/i, "");
    const desc = l.desc_zh || l.desc;
    box.append(el("div", { class: "ag-lora" },
      el("div", { class: "ph", style: l.preview ? `background-image:url(/lora_preview/${encodeURIComponent(l.preview)})` : "" }),
      el("div", { style: "min-width:0" },
        el("div", { class: "nm", title: l.name }, (l.fav ? "★ " : "") + name),
        el("div", { class: "ds" + (desc && l.pickable !== false ? "" : " none"), title: l.desc || "" },
          l.kind ? el("span", { class: "ag-kind" + (l.sure === false ? " unsure" : ""), title: l.sure === false ? "助手沒什麼把握" : "" }, (KIND_ZH[l.kind] || l.kind) + (l.sure === false ? "？" : "")) : null,
          " ", l.pickable === false ? "調年齡的，助手不會自己掛" : desc || "還不認識")),
      l.pickable === false ? el("span") :
        el("button", { class: "icon", type: "button", title: l.known ? "重新認識這一個" : "只認識這一個", onclick: () => learn([l.name]) }, l.known ? "↻" : "▶")));
  }
}

function setLearning(on) {
  F("agLearn").disabled = on;
  F("agLearnStop").hidden = !on;
  F("agLoraBar").hidden = !on;
}

async function learn(names) {
  if (!S?.settings?.base) return toast("還沒選生圖模型：先按「用生成頁目前的設定」（決定是哪一家的 LoRA、試畫用什麼模型）", "err");
  const body = { base: S.settings.base, brain: S.settings.brain === "35b" ? "35b" : "9b", probe: F("agProbe").checked };
  if (names) { body.names = names; body.redo = true; }
  try {
    const r = await api("/api/agent/loras/learn", { json: body });
    if (!r.total) return toast("這一家的 LoRA 都認識了。要重來可以按單個的 ↻", "ok");
    setLearning(true);
    F("agLoraMsg").textContent = `開始認識 ${r.total} 個…`;
    F("agLoraBar").firstElementChild.style.width = "0";
  } catch (e) {
    toast(e.message, "err");
  }
}

// ---------------------------------------------------------------- websocket（main.js 轉過來）
export function onEvent(m) {
  if (m.type === "agent_cloud") { cloud = m.status; if (shown) paintCloud(); return; }
  if (m.type === "agent_lora") {
    if (!shown) return;
    F("agLoraMsg").textContent = m.msg || "";
    if (m.total) F("agLoraBar").firstElementChild.style.width = `${(100 * (m.done || 0)) / m.total}%`;
    if (m.phase === "end" || m.phase === "error") {
      setLearning(false);
      if (m.phase === "error") toast("認識 LoRA 失敗：" + m.msg, "err");
      loadLoras();
    }
    return;
  }
  if (m.type === "agent_llm") {
    if (!shown) return;
    if (m.rid === "agent-learn") { if (m.msg) F("agLoraMsg").textContent = m.msg; return; }
    if (S && m.rid === `agent-${S.id}` && running) { liveLlm = m.msg || ""; paintLive(); }
    return;
  }
  // type=agent
  if (!shown) return;
  if (!S || m.id !== S.id) { if (m.phase === "end") refreshTitles(); return; }
  if (m.phase === "event") {
    S.events = S.events || [];
    S.events.push(m.event);
    if (m.event.kind === "result") { phase = "think"; live = "看圖、逐項對照中…"; liveLlm = ""; }
    paintAll();
  } else if (m.phase === "think" || m.phase === "render") {
    const changed = phase !== m.phase;
    phase = m.phase; live = m.msg || ""; liveLlm = "";
    if (!running) setRunning(true);
    status(live, "amb");
    if (changed) paintAll(); else paintLive();
  } else if (m.phase === "end") {
    phase = ""; live = ""; liveLlm = "";
    setRunning(false);
    status(m.msg || "待機", m.msg === "出錯了" ? "r" : "g");
    reload();
    refreshTitles();
  }
}

// ---------------------------------------------------------------- 附圖（上傳到 ComfyUI input，送出時一起交給助手）
/** 之前的對話已經附過幾張：新附的圖從下一個編號算 */
const attBase = () => (S?.events || []).reduce((n, e) => n + (e.kind === "user" ? (e.images || []).length : 0), 0);

function paintAtt() {
  const box = F("agAttList");
  box.innerHTML = "";
  const base = attBase();
  att.forEach((a, k) => box.append(el("figure", {},
    el("img", { src: inputUrl(a.name), alt: a.name, onclick: () => peek(a.name) }),
    el("figcaption", {}, `圖 ${base + k + 1}`),
    el("button", { type: "button", title: "拿掉這張", onclick: () => { att.splice(k, 1); paintAtt(); } }, "×"))));
  F("agAttHint").hidden = att.length > 0;
}

async function addFiles(files) {
  const imgs = [...files].filter((f) => f.type.startsWith("image/"));
  if (!imgs.length) return;
  const room = MAX_ATT - att.length;
  if (room <= 0) return toast(`一次最多附 ${MAX_ATT} 張`, "err");
  if (imgs.length > room) toast(`一次最多附 ${MAX_ATT} 張，只加前 ${room} 張`);
  for (const f of imgs.slice(0, room)) {
    try { att.push(await uploadFile(f)); paintAtt(); } catch (e) { toast(e.message, "err"); }
  }
}

function initAttach() {
  F("agAttPick").onclick = () => {
    const inp = el("input", { type: "file", accept: "image/*", multiple: true });
    inp.onchange = () => addFiles(inp.files);
    inp.click();
  };
  F("agAttGallery").onclick = () => pickFromGallery(async (rec) => {
    if (att.length >= MAX_ATT) return toast(`一次最多附 ${MAX_ATT} 張`, "err");
    try { att.push(await api("/api/to_input", { json: { id: rec.id } })); paintAtt(); } catch (e) { toast(e.message, "err"); }
  });
  F("agSay").addEventListener("paste", (e) => {
    const files = [...(e.clipboardData?.files || [])];
    if (files.some((f) => f.type.startsWith("image/"))) { e.preventDefault(); addFiles(files); }
  });
  const form = F("agForm");
  form.addEventListener("dragover", (e) => { if (e.dataTransfer?.types.includes("Files")) { e.preventDefault(); form.classList.add("over"); } });
  form.addEventListener("dragleave", () => form.classList.remove("over"));
  form.addEventListener("drop", (e) => { e.preventDefault(); form.classList.remove("over"); addFiles(e.dataTransfer.files); });
}

// ---------------------------------------------------------------- 按鈕
export function init() {
  // 助手：外觀文字寫進句子，圖放進附圖（句子裡寫「圖 N」，跟附圖的編號一樣）
  drawer.mount(F("agAssets"), { family: () => "agent", insert: drawer.appendTo(F("agSay")), images: { ref: {
    max: MAX_ATT, count: () => att.length, has: (name) => att.some((a) => a.name === name), tag: (n) => `圖 ${n}`,
    add: (r) => {
      let k = att.findIndex((a) => a.name === r.name);
      if (k < 0) {
        if (att.length >= MAX_ATT) return null;
        k = att.push(r) - 1;
        paintAtt();
      }
      return attBase() + k + 1;
    },
  } } });
  initAttach();
  F("agSess").onchange = (e) => openSession(e.target.value).catch((err) => toast(err.message, "err"));
  F("agNew").onclick = async () => {
    // 目前這個還沒講過話就直接用它，不另外開
    if (S && !(S.events || []).length) { F("agSay").focus(); return; }
    try {
      const s = await api("/api/agent/session/new", { json: { settings: S?.settings || {} } });
      await loadSessions(s.id);
      F("agSay").focus();
    } catch (e) { toast(e.message, "err"); }
  };
  F("agDel").onclick = async () => {
    if (!S || !confirm(`把對話「${S.title}」移到待刪除？（圖庫裡的圖不會動）`)) return;
    try { await api("/api/agent/session/delete", { json: { id: S.id } }); store.set("agSession", null); await loadSessions(null); } catch (e) { toast(e.message, "err"); }
  };
  F("agForm").onsubmit = (e) => { e.preventDefault(); say(); };
  F("agSay").addEventListener("keydown", (e) => { if (e.key === "Enter" && e.ctrlKey) { e.preventDefault(); say(); } });
  F("agStop").onclick = async () => {
    if (!S) return;
    try { await api("/api/agent/stop", { json: { id: S.id } }); status("停止中…", "amb"); } catch (e) { toast(e.message, "err"); }
  };
  F("agMode").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-v]");
    if (b) saveSettings({ show: b.dataset.v === "show" });
  });
  F("agKind").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-k]");
    if (b) saveSettings({ mode: b.dataset.k });
  });
  F("agTarget").onchange = (e) => saveSettings({ target: e.target.value });
  F("agBrain").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-b]");
    if (!b || !S || b.dataset.b === S.settings.brain) return;
    // 雲端預設「先想再答」（快到不差這點時間）；本機預設不開（35B 先想會很久）
    saveSettings({ brain: b.dataset.b, ...(b.dataset.b === "cloud" && S?.settings?.think === "off" ? { think: "high" } : {}) });
  });
  F("agRounds").addEventListener("change", (e) => saveSettings({ rounds: +e.target.value }));
  F("agCount").addEventListener("change", (e) => saveSettings({ count: +e.target.value }));
  F("agAuto").onchange = (e) => saveSettings({ auto: e.target.checked });
  F("agThink").onchange = (e) => saveSettings({ think: e.target.value });
  F("agLoras").onchange = (e) => saveSettings({ loras: e.target.checked });
  F("agBaseUse").onclick = async () => {
    const b = gen.baseParams();
    if (!b.model) return toast("生成頁還沒選模型", "err");
    if (b.family === "custom") return toast("生圖工作流只能在生成頁用：先在生成頁換成一般模型", "err");
    await saveSettings({ base: b });
    toast("助手會用：" + b.model, "ok");
    if (familyOf() !== loraFam) loadLoras();
  };
  F("agBModel").onchange = async (e) => {
    try {
      const r = await api("/api/agent/base/model", { json: { id: S.id, model: e.target.value } });
      S.settings = r.settings;
    } catch (err) { toast("換模型失敗：" + err.message, "err"); }
    paintSettings();
    if (familyOf() !== loraFam) loadLoras();
  };
  F("agBSteps").onchange = (e) => setBase({ steps: Math.max(1, Math.round(+e.target.value || 1)) });
  F("agBCfg").onchange = (e) => setBase({ cfg: +e.target.value || 1 });
  F("agBSampler").onchange = (e) => setBase({ sampler: e.target.value });
  F("agBSched").onchange = (e) => setBase({ scheduler: e.target.value });
  F("agBNeg").onchange = (e) => setBase({ negative: e.target.value });
  F("agCloudGpu").onchange = () => paintGpuNote();
  F("agCloudGpuRe").onclick = () => refreshGpus(loadCloud);
  // RunPod 要先看價格按確認才開機；自己的主機、網址不是生圖站在收錢，直接連
  F("agCloudStart").onclick = () => {
    if (cloudKind() === "runpod" && !cloud?.pod) { paintGpuNote(); F("agCloudConfirm").hidden = false; return; }
    startCloud({});
  };
  F("agCloudNo").onclick = () => { F("agCloudConfirm").hidden = true; };
  F("agCloudYes").onclick = () => {
    F("agCloudConfirm").hidden = true;
    const body = { confirm: true };
    if (!cloud?.pod) body.gpu = F("agCloudGpu").value;
    startCloud(body);
  };
  F("agCloudStop").onclick = async () => {
    const kind = cloudKind();
    if (!confirm(STOP_ASK[kind])) return;
    try { cloud = await api("/api/agent/cloud/stop", { json: {} }); paintCloud(); toast(cloud.msg || "已停止", "ok"); }
    catch (e) { toast((kind === "runpod" ? "關機失敗：" : "停止失敗：") + e.message + (kind === "runpod" ? "\n請到 RunPod 網站確認機器有沒有刪掉。" : ""), "err"); }
  };
  F("agCloudCfg").onclick = () => openCloudCfg();
  F("acWay").addEventListener("click", (e) => { const b = e.target.closest("button[data-v]"); if (b) setWay(b.dataset.v); });
  // H3 進階一定連 H3；拿掉 H3 就沒有進階
  F("acPackH3x").onchange = () => { if (F("acPackH3x").checked) F("acPackH3").checked = true; };
  F("acPackH3").onchange = () => { if (!F("acPackH3").checked) F("acPackH3x").checked = false; };
  F("acSshCmd").oninput = () => {
    const r = parseSsh(F("acSshCmd").value);
    if (r.host) F("acHost").value = r.host;
    if (r.port) F("acPort").value = r.port;
    if (r.user) F("acUser").value = r.user;
  };
  F("acPubCopy").onclick = async () => {
    if (!F("acPub").value) return acMsg("還沒有公鑰：選「任何雲端主機」按一次「儲存」就會產生", "err");
    await copyText(F("acPub").value);
    toast("公鑰複製好了，貼到雲端服務的 SSH Keys", "ok");
  };
  F("acSave").onclick = async () => { if (await saveCfg()) acMsg("存好了", "ok"); };
  F("acTest").onclick = async () => {
    if (!(await saveCfg())) return;
    acMsg(acWay === "ssh" ? "連線中（最多半分鐘）…" : "測試中…", "run");
    try { const r = await api("/api/agent/cloud/test", { json: {} }); acMsg(r.msg, "ok"); } catch (e) { acMsg(e.message, "err"); }
  };
  F("acRpClear").onclick = async () => {
    if (!confirm("清掉已存的 RunPod 金鑰？")) return;
    try { cloud = await api("/api/agent/cloud/config", { json: { runpod_key: "" } }); paintCloud(); paintKeys(); acMsg("已清掉", "ok"); } catch (e) { acMsg(e.message, "err"); }
  };
  F("agCloudIdle").onchange = async (e) => {
    try { cloud = await api("/api/agent/cloud/idle", { json: { minutes: +e.target.value } }); paintCloud(); } catch (err) { toast(err.message, "err"); }
  };
  F("agLearn").onclick = () => learn(null);
  F("agLearnStop").onclick = async () => { try { await api("/api/agent/loras/stop", { json: {} }); } catch (e) { toast(e.message, "err"); } };
  // 雲端開著時每 30 秒更新一次（已開幾分鐘、花了多少）
  setInterval(() => { if (shown && !document.hidden && cloud?.pod) loadCloud(); }, 30000);
}

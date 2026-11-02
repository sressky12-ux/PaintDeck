// 任務面板：每個任務的狀態、進度百分比、預估剩餘時間；單獨取消、重試、重用參數；畫好時跳通知、提示音；分頁標題顯示進度
import { $, el, api, toast, debounce, store, modeName, fmtSecs } from "./util.js";
import { app } from "./state.js";
import { play } from "./sfx.js";
import { downloadForJob } from "./diagnostics.js";
import { jobNodeLabel } from "./nodes.js";

const F = (id) => $("#" + id);
let jobs = [], running = null, avgFallback = null, queued = 0;
let nodesCache = [], nodesAt = 0;  // GET /api/nodes 的節點清單（refresh 時順便更新，任務列要標「送到哪台」）
const prog = new Map();     // job → { t0, v0, value, max, stage, t } 用來算這一段的速度
let batchDone = [];          // 這一批畫好的圖（全部畫完時一起通知）
const STATUS = { queued: "排隊中", running: "執行中", done: "完成", error: "失敗", cancelled: "已取消", lost: "遺失" };

export function init() {
  F("queueInfo").onclick = toggle;
  F("jobsClose").onclick = () => (F("jobsDrawer").hidden = true);
  F("jobsClear").onclick = async () => { await api("/api/jobs/clear", { json: {} }); refresh(); };
  F("jobsStopAll").onclick = async () => {
    if (!confirm("清空佇列並停止目前這張？")) return;
    await api("/api/clear_queue", { json: {} });
    (await import("./gen.js")).S.pending.clear();
    refresh();
  };
  const s = store.get("notify", {});
  F("jobsNotify").checked = !!s.on && typeof Notification !== "undefined" && Notification.permission === "granted";
  F("jobsSound").checked = !!s.sound;
  F("jobsNotify").onchange = async () => {
    if (F("jobsNotify").checked) {
      if (typeof Notification === "undefined") { F("jobsNotify").checked = false; return toast("這個瀏覽器不支援通知", "err"); }
      const p = Notification.permission === "granted" ? "granted" : await Notification.requestPermission();
      if (p !== "granted") { F("jobsNotify").checked = false; toast("瀏覽器沒有允許通知：網址列左邊的圖示 → 通知 → 允許", "err"); }
    }
    saveNotify();
  };
  F("jobsSound").onchange = saveNotify;
  document.addEventListener("click", (e) => {
    const d = F("jobsDrawer");
    if (!d.hidden && !d.contains(e.target) && !F("queueInfo").contains(e.target) && !e.target.closest(".modal")) d.hidden = true;
  });
  setInterval(() => { if (!F("jobsDrawer").hidden && running) renderSummary(); tick(); }, 1000);
}

const saveNotify = () => store.set("notify", { on: F("jobsNotify").checked, sound: F("jobsSound").checked });

export function toggle() {
  const d = F("jobsDrawer");
  d.hidden = !d.hidden;
  if (!d.hidden) refresh();
}

export const refresh = debounce(async () => {
  if (F("jobsDrawer").hidden) return;
  try {
    const r = await api("/api/jobs?limit=80");
    jobs = r.jobs;
    running = r.running;
    if (r.progress && running) noteProgress(running, r.progress.value, r.progress.max);
    avgFallback = jobs.find((j) => j.avg_seconds)?.avg_seconds ?? null;
    if (Date.now() - nodesAt > 10000) {  // 節點名稱不常變：最多 10 秒問一次
      nodesAt = Date.now();
      api("/api/nodes").then((n) => { nodesCache = n.nodes || []; render(); }).catch(() => { nodesCache = []; });
    }
    render();
  } catch (e) {
    F("jobsList").innerHTML = "";
    F("jobsList").append(el("div", { class: "empty-state err" }, "讀不到任務：" + e.message));
  }
}, 250);

// ---------------------------------------------------------------- websocket 事件（main.js 轉過來）
export function onStart(job) {
  running = job;
  prog.delete(job);
  const j = jobs.find((x) => x.id === job);
  if (j) { j.status = "running"; j.started = Date.now() / 1000; }
  refresh();
}

export function onProgress(m) {
  noteProgress(m.job, m.value, m.max);
  const card = F("jobsList").querySelector(`[data-job="${m.job}"]`);
  if (card) paintProgress(card, m.job);
  tick();
}

export function onQueue(remaining) {
  queued = remaining || 0;
  F("qcnt").textContent = String(queued);
  F("qtxt").textContent = `佇列 ${queued}`;
  F("queueInfo").classList.toggle("busy", queued > 0);
  if (!remaining) { running = null; tick(); }
  refresh();  // 有新任務送進來或跑完：面板開著就更新
}

export function onImage(job, rec) {
  batchDone.push(rec);
  refresh();
}

export async function onFinish(m, pendingLeft, wasMine) {
  prog.delete(m.job);
  if (m.job === running) running = null;
  refresh();
  tick();
  if (!wasMine) return;
  if (m.status === "error") notify("生成失敗", (m.error || "").split("\n")[0], null, true);
  if (pendingLeft === 0 && batchDone.length) {
    const n = batchDone.length, last = batchDone[0];
    batchDone = [];
    notify(n > 1 ? `${n} 張都畫好了` : "畫好了", (last.idea || last.prompt || "").slice(0, 80), last.id);
  }
}

function notify(title, body, imgId, isErr = false) {
  if (F("jobsSound").checked) beep(isErr);
  if (!F("jobsNotify").checked || typeof Notification === "undefined" || Notification.permission !== "granted") return;
  if (!document.hidden && !isErr) return;  // 正在看網頁就不用跳
  try {
    const n = new Notification(title, { body, icon: imgId ? `/thumb/${imgId}` : undefined, tag: "anima-done" });
    n.onclick = () => { window.focus(); n.close(); };
  } catch { /* 部分瀏覽器要 service worker 才能跳 */ }
}

// 畫完的鐘聲／失敗的悶響（sfx.js）；跟著任務面板的「完成時發出聲音」，不受按鍵音開關影響
const beep = (isErr) => play(isErr ? "error" : "done");

// ---------------------------------------------------------------- 進度與預估
function noteProgress(job, value, max) {
  const now = performance.now();
  let p = prog.get(job);
  // 一張圖可能有好幾段取樣（高解析修復、精修）：數字變小＝進入下一段，重新計速
  if (!p || value < p.value || max !== p.max) p = { t0: now, v0: value, stage: (p?.stage || 0) + (p ? 1 : 0) };
  Object.assign(p, { value, max, t: now });
  prog.set(job, p);
}

function etaOf(j) {
  const p = prog.get(j.id);
  if (p && p.value > p.v0) {
    const per = (p.t - p.t0) / (p.value - p.v0) / 1000;
    return { pct: p.value / p.max, left: (p.max - p.value) * per, stage: p.stage };
  }
  const avg = j.avg_seconds || avgFallback;
  if (avg && j.started) {
    const el2 = Date.now() / 1000 - j.started;
    return { pct: p ? p.value / p.max : Math.min(0.99, el2 / avg), left: Math.max(1, avg - el2), stage: p?.stage || 0 };
  }
  return p ? { pct: p.value / p.max, left: null, stage: p.stage } : null;
}

// 分頁標題顯示進度：切去別的分頁也看得到
function tick() {
  const p = running && prog.get(running);
  const n = queued;
  const name = document.documentElement.classList.contains("solo") ? "繪匣 助手" : "繪匣 PaintDeck"; // 助手獨立視窗（main.js）
  document.title = (p ? `(${Math.round((100 * p.value) / p.max)}%${n > 1 ? `・剩 ${n}` : ""}) ` : "") + name;
}

// ---------------------------------------------------------------- 畫面
function renderSummary() {
  const q = jobs.filter((j) => j.status === "queued"), r = jobs.find((j) => j.status === "running");
  let left = 0;
  if (r) left += etaOf(r)?.left || r.avg_seconds || 0;
  for (const j of q) left += j.avg_seconds || avgFallback || 0;
  const parts = [];
  if (r) parts.push("執行中 1");
  if (q.length) parts.push(`排隊 ${q.length}`);
  if (left) parts.push(`全部畫完約 ${fmtSecs(left)}`);
  F("jobsSummary").textContent = parts.length ? parts.join("・") : "目前沒有在跑的任務。";
  if (r) { const c = F("jobsList").querySelector(`[data-job="${r.id}"]`); if (c) paintProgress(c, r.id); }
}

function render() {
  const box = F("jobsList");
  box.innerHTML = "";
  if (!jobs.length) box.append(el("div", { class: "empty-state" }, "還沒有任務。按「生成」之後會出現在這裡。"));
  let qpos = 0;
  // 排隊中的照送出順序排（先送的先跑）
  const order = [...jobs].sort((a, b) => rank(a) - rank(b) || (a.status === "queued" ? a.created - b.created : b.created - a.created));
  for (const j of order) box.append(card(j, j.status === "queued" ? ++qpos : 0));
  renderSummary();
}
const rank = (j) => ({ running: 0, queued: 1 }[j.status] ?? 2);

const nodeTag = (j) => jobNodeLabel(j, nodesCache);

function card(j, qpos) {
  const s = j.summary || {};
  const thumbs = (j.images || []).filter((x) => !x.deleted);
  const c = el("div", { class: `job ${j.status}`, "data-job": j.id },
    el("div", { class: "job-head" },
      el("span", { class: `badge st-${j.status}` }, STATUS[j.status] + (qpos ? ` 第 ${qpos} 個` : "")),
      el("span", { class: "muted sm" }, `${modeName[s.mode] || s.mode || ""}・${(s.model || "").replace(/\.safetensors$/, "")}・${s.width || "?"}×${s.height || "?"}`),
      nodeTag(j) ? el("span", { class: "nodes-tag" }, nodeTag(j)) : null,
      el("span", { class: "grow" }),
      el("span", { class: "muted sm", title: new Date(j.created * 1000).toLocaleString(document.documentElement.lang) }, timeText(j))),
    el("div", { class: "job-prompt", title: s.prompt }, s.idea ? `💡 ${s.idea}` : s.prompt || "（沒有提示詞）"),
    j.status === "running" ? el("div", { class: "job-prog" }, el("div", { class: "bar" }, el("div")), el("span", { class: "sm muted job-eta" })) : null,
    j.error ? el("div", { class: "job-err" }, j.error) : null,
    thumbs.length ? el("div", { class: "job-thumbs" }, thumbs.map((t) => el("img", { src: `/thumb/${t.id}`, alt: "", loading: "lazy", onclick: () => openImg(t.id) }))) : null,
    el("div", { class: "row gap wrap job-acts" },
      ["queued", "running"].includes(j.status) ? el("button", { class: "btn sm danger", onclick: () => cancel(j) }, "取消") : null,
      !["queued", "running"].includes(j.status) ? el("button", { class: "btn sm", title: "同樣的參數、同一個種子再跑一次", onclick: () => retry(j, false) }, j.status === "done" ? "再跑一次" : "重試") : null,
      !["queued", "running"].includes(j.status) ? el("button", { class: "btn sm", title: "同樣的參數，換一個隨機種子", onclick: () => retry(j, true) }, "換種子再跑") : null,
      el("button", { class: "btn sm", onclick: () => reuse(j) }, "重用參數"),
      el("button", { class: "btn sm", onclick: () => downloadForJob(j.id) }, "診斷包")));
  if (j.status === "running") paintProgress(c, j.id);
  return c;
}

function timeText(j) {
  if (j.status === "done" && j.started && j.finished) return `花了 ${fmtSecs(j.finished - j.started)}`;
  const d = new Date(j.created * 1000);
  return d.toDateString() === new Date().toDateString() ? d.toLocaleTimeString(document.documentElement.lang, { hour: "2-digit", minute: "2-digit" }) : d.toLocaleDateString(document.documentElement.lang);
}

function paintProgress(c, id) {
  const j = jobs.find((x) => x.id === id) || { id };
  const e = etaOf(j);
  const bar = c.querySelector(".job-prog .bar > div"), t = c.querySelector(".job-eta");
  if (!bar) return;
  bar.style.width = e ? `${Math.round(e.pct * 100)}%` : "0";
  t.textContent = e ? `${Math.round(e.pct * 100)}%` + (e.stage ? `（第 ${e.stage + 1} 段）` : "") + (e.left != null ? `・約剩 ${fmtSecs(e.left)}` : "") : "載入模型中…";
}

async function cancel(j) {
  try { await api("/api/cancel", { json: { job: j.id } }); toast("已取消", "ok"); } catch (e) { toast(e.message, "err"); }
  refresh();
}

async function retry(j, newSeed) {
  try {
    const r = await api("/api/jobs/retry", { json: { job: j.id, new_seed: newSeed } });
    const gen = await import("./gen.js");
    for (const x of r.jobs) gen.S.pending.set(x.job, { seed: x.seed });
    gen.renderStrip();
    toast(newSeed ? `已換種子重新送出（${r.jobs[0]?.seed}）` : "已重新送出", "ok");
  } catch (e) { toast("無法重新送出：" + e.message, "err"); }
  refresh();
}

async function reuse(j) {
  const gen = await import("./gen.js");
  app.showPage("gen");
  gen.applyParams({ ...j.params, prompt: j.params.prompt });
  F("jobsDrawer").hidden = true;
}

async function openImg(id) {
  try {
    const rec = await api(`/api/image/${id}`);
    (await import("./gallery.js")).openLightboxRecord(rec, [rec]);
  } catch (e) { toast(e.message, "err"); }
}

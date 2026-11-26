// 影音工作台：導演頁的四個分頁（分鏡／剪輯台／拆幀台／影片：拍攝單（js/shotsheet.js）＋H3 短片（js/h3.js）或長片（js/h3chain.js））、剪輯台（嵌開源的 FableCut）、拆幀台、給外面 AI 操作的設定
// 後端在 src-tauri\src\studio.rs、server\api_studio.rs；工具（Node.js、ffmpeg）由安裝精靈下載（/api/setup/run {only: "tools"}）
import { $, $$, el, api, toast, openModal, closeModal, pickFile, store, copyText, typing, fmtBytes } from "./util.js";
import { pickFromGallery, openLightbox } from "./gallery.js";
import * as gen from "./gen.js";
import { app } from "./state.js";
import * as i18n from "./i18n.js";
import * as h3 from "./h3.js";
import * as h3chain from "./h3chain.js";

const F = (id) => document.getElementById(id);
let tab = store.get("dirTab", "plan");
let videoSub = store.get("videoSub", "short");

// ================================================================ 分頁
export function init() {
  $("#dirTabs").addEventListener("click", (e) => { const b = e.target.closest("button[data-t]"); if (b) setTab(b.dataset.t); });
  // 頁首列的「短片／長片」
  $("#videoSub").addEventListener("click", (e) => { const b = e.target.closest("button[data-sub]"); if (!b) return; e.preventDefault(); openVideo(b.dataset.sub); });
  F("stAiOpen").onclick = openAi;
  // 標題列的「AI MCP」：每一頁都看得到（同樣開這個視窗）
  const top = F("aiBtn");
  if (top) top.onclick = openAi;
  // 第一次開程式的一次性導引（按「知道了」或開過視窗就不再跳）
  if (!store.get(AI_HINT_KEY, false)) {
    F("aiHint").hidden = false;
    F("aiBtn").classList.add("pip");
  }
  if (F("aiHintOpen")) F("aiHintOpen").onclick = () => F("stAiMcp").focus();
  if (F("aiHintHide")) F("aiHintHide").onclick = hideAiHint;
  // AI MCP 燈號：開程式先問一次，之後每 10 秒問（MCP 伺服器 15 秒一次心跳）
  refreshAiLink();
  const aiTimer = setInterval(refreshAiLink, 10000);
  if (aiTimer.unref) aiTimer.unref();
  initEdit();
  initFrames();
  setTab(tab, true);
}

// 每次切到導演頁（main.js 的 showPage）
export function show() { setTab(tab); }
// 別的地方直接打開某個分頁（生成頁的「動起來」選了 H3）；舊名 h3／chain 等於影片分頁的短片／長片
export function openTab(t) { setTab(t); }
/** 打開影片分頁的短片（short）或長片（long） */
export function openVideo(sub) { setTab(sub === "long" ? "chain" : "h3"); }
/** 長片分頁的「拆幀台看」：打開拆幀台的某一份 */
export function openFramesSession(sid) { setTab("frames", true); framesShow(sid); }

function setTab(t, quiet = false) {
  // 2026-10-24 起 H3 影片和長片合成「影片」一個分頁；舊值（存在 localStorage 的、別處呼叫的）換成子區塊
  if (t === "h3" || t === "chain") { videoSub = t === "chain" ? "long" : "short"; t = "video"; }
  if (!["plan", "edit", "frames", "video", "mixdesk"].includes(t)) t = "plan";
  if (!["short", "long"].includes(videoSub)) videoSub = "short";
  tab = t;
  store.set("dirTab", t);
  store.set("videoSub", videoSub);
  $$("#dirTabs button").forEach((b) => { const on = b.dataset.t === t; b.classList.toggle("on", on); b.setAttribute("aria-selected", String(on)); });
  $$("#videoSub button").forEach((b) => { const on = b.dataset.sub === videoSub; b.classList.toggle("on", on); b.setAttribute("aria-selected", String(on)); });
  // 頁首列只亮目前這一種的狀態燈
  F("h3Status").hidden = videoSub !== "short";
  F("hcStatus").hidden = videoSub !== "long";
  $$("#page-director > section[data-tab]").forEach((s) => (s.hidden = s.dataset.tab !== t || (t === "video" && s.dataset.sub !== "sheet" && s.dataset.sub !== videoSub)));
  $("#page-director").classList.toggle("video", t === "video");
  if (t !== "frames") stopPlay();
  if (quiet || app.page !== "director") return;
  if (t === "edit") editShow();
  if (t === "frames") framesShow();
  if (t === "mixdesk") mixdeskShow();
  if (t === "video") {
    if (videoSub === "long") h3chain.show(); else h3.show();
    import("./shotsheet.js").then((m) => m.show()).catch(() => {});
  }
}

// ================================================================ 混音台（ui/mixdesk.html；同一個伺服器的獨立頁面）
function mixdeskShow() {
  const fr = F("mdFrame");
  if (!fr.dataset.on) { fr.src = "/mixdesk"; fr.dataset.on = "1"; }
  F("mdFull").onclick = () => { F("mdBody").classList.add("full"); F("mdUnfull").hidden = false; };
  F("mdUnfull").onclick = () => { F("mdBody").classList.remove("full"); F("mdUnfull").hidden = true; };
  // 全螢幕：先放大到整個視窗（瀏覽器不准蓋滿螢幕時也有效），再請瀏覽器蓋滿螢幕；Esc 退出全螢幕後還是放大狀態，紅鈕縮回
  F("mdFs").onclick = () => { F("mdFull").onclick(); F("mdBody").requestFullscreen?.().catch(() => {}); };
  // 混音台的「🎬 影片 → 從圖庫挑」借用這裡的圖庫挑選器（全螢幕時先退出，不然看不到視窗）
  window.mixdeskPickVideo = (cb) => {
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    pickFromGallery(cb, { video: "only", title: "選一支影片給混音台配樂" });
  };
}

// ================================================================ 剪輯台（FableCut）
let st = null;          // /api/studio/status
let toolsPoll = null;

function initEdit() {
  F("stAdd").onclick = () => pickFromGallery(addToEditor, { video: "also", title: "選一張圖或一支影片放進剪輯台" });
  F("stExp").onclick = openExports;
  F("stTips").onclick = () => openModal("#stTipsModal");
  F("stFull").onclick = () => full(true);
  F("stUnfull").onclick = () => full(false);
  F("stStop").onclick = stopEditor;
  F("stGetTools").onclick = downloadTools;
  F("stExpOpen").onclick = () => openFolder("exports");
  F("stOpenSfx").onclick = () => openFolder("sfx");
}

const setStatus = (s) => (F("stStatus").textContent = s);

async function editShow() {
  try { st = await api("/api/studio/status"); } catch (e) { return off("讀不到剪輯台的狀態：" + e.message, []); }
  if (st.running) return showFrame();
  if (!st.installed) return off("找不到剪輯台的程式檔（resources\\extras\\fablecut），請重新安裝生圖站。", []);
  if (!st.node) return needTools();
  start();
}

function off(msg, btns) {
  F("stFrame").hidden = true;
  F("stOff").hidden = false;
  F("stOffMsg").textContent = msg;
  F("stOffBtns").replaceChildren(...btns);
  F("stStop").hidden = true;
  F("stGetTools").hidden = true;
  setStatus("剪輯台沒開");
}

function needTools() {
  off("剪輯台要用 Node.js 和 ffmpeg（免費的開源工具，約 150MB）。按下面的按鈕自動下載，放在生圖站自己的資料夾裡，不會動到電腦的其他程式。",
    [el("button", { class: "akey go", type: "button", onclick: downloadTools }, "下載剪輯工具（約 150MB）")]);
}

async function downloadTools() {
  try {
    await api("/api/setup/run", { json: { only: "tools" }, headers: { "X-Anima-Setup": "1" } });
  } catch (e) {
    return toast(/404/.test(e.message) ? "這裡找不到安裝精靈：請從系統列選單打開「安裝與修復」，再按一次開始安裝" : e.message, "err");
  }
  F("stOffBtns").replaceChildren();
  F("stOffMsg").textContent = "下載中，好了會自動打開剪輯台…";
  F("stProg").hidden = false;
  clearInterval(toolsPoll);
  toolsPoll = setInterval(async () => {
    let s;
    try { s = await api("/api/setup/status"); } catch { return; }
    F("stProg").replaceChildren(...(s.items || []).map((it) => el("div", { class: "st-prog-row" },
      el("span", {}, it.label),
      el("span", { class: "muted" }, it.status === "done" || it.status === "skip" ? "✓ 好了" : it.status === "error" ? "✗ " + (it.error || "失敗")
        : it.total ? `${fmtBytes(it.done)} / ${fmtBytes(it.total)}` : it.status === "run" ? "處理中…" : "等待"))));
    if (!s.finished) return;
    clearInterval(toolsPoll);
    F("stProg").hidden = true;
    if (s.problems?.length) {
      off("工具沒裝好：" + s.problems.join("；"), [el("button", { class: "akey", type: "button", onclick: downloadTools }, "再試一次")]);
    } else {
      toast("剪輯工具裝好了", "ok");
      editShow();
    }
  }, 1000);
}

async function start() {
  off("剪輯台啟動中…", []);
  setStatus("啟動中…");
  try {
    st = await api("/api/studio/start", { json: {} });
    showFrame();
  } catch (e) {
    off("剪輯台開不起來：" + e.message, [el("button", { class: "akey", type: "button", onclick: start }, "再試一次")]);
  }
}

function showFrame() {
  const fr = F("stFrame");
  // 剪輯台的介面語言跟著生圖站（resources\extras\fablecut\anima-i18n.js 讀 ?lang=）
  const url = st.url + "?lang=" + i18n.current();
  if (fr.dataset.url !== url) { fr.src = url; fr.dataset.url = url; }
  F("stOff").hidden = true;
  fr.hidden = false;
  F("stStop").hidden = !st.ours;
  F("stGetTools").hidden = !!st.ffmpeg;
  setStatus(`剪輯台開著（${st.url}）` + (st.ffmpeg ? "" : "・沒有 ffmpeg：拆幀台不能用、匯出比較慢"));
}

async function stopEditor() {
  if (!confirm("關掉剪輯台？（專案會自動存，下次打開還在）")) return;
  try { await api("/api/studio/stop", { json: {} }); } catch (e) { return toast(e.message, "err"); }
  const fr = F("stFrame");
  fr.src = "about:blank";
  delete fr.dataset.url;
  off("剪輯台已經關了", [el("button", { class: "akey go", type: "button", onclick: start }, "打開剪輯台")]);
}

function full(on) {
  F("stBody").classList.toggle("full", on);
  F("stUnfull").hidden = !on;
  if (on) toast("右下角的紅色按鈕可以縮回來", "ok");
}

async function addToEditor(rec) {
  setStatus("放進剪輯台中…");
  try {
    await api("/api/studio/import", { json: { id: rec.id } });
    toast("已放進剪輯台的素材區（左上 Project），拖到時間軸就能剪", "ok");
  } catch (e) { toast(e.message, "err"); }
  editShow();
}

async function openFolder(which) {
  try { await api("/api/studio/open", { json: { which } }); } catch (e) { toast(e.message, "err"); }
}

async function openExports() {
  const box = F("stExpList");
  box.textContent = "讀取中…";
  openModal("#stExpModal");
  let r;
  try { r = await api("/api/studio/exports"); } catch (e) { box.textContent = e.message; return; }
  box.replaceChildren();
  if (!r.exports.length) { box.textContent = "還沒有匯出的影片。"; return; }
  for (const x of r.exports) {
    const b = el("button", { class: "akey sm go", type: "button" }, "存回圖庫");
    b.onclick = async () => {
      b.disabled = true;
      try {
        const s = await api("/api/studio/exports/save", { json: { name: x.name } });
        toast(`已存進圖庫（#${s.image.id}）`, "ok");
        b.textContent = "✓ 已存";
      } catch (e) { toast(e.message, "err"); b.disabled = false; }
    };
    box.append(el("div", { class: "st-exp" },
      el("span", { class: "grow" }, x.name),
      el("span", { class: "muted sm" }, `${fmtBytes(x.size)}・${new Date(x.time * 1000).toLocaleString()}`), b));
  }
}

// ---------------------------------------------------------------- 給 AI 操作
/** 第一次接 AI 時貼給它的開場白（要它先看狀態、讀說明、動手前先問） */
const AI_HELLO = "先用 anima_status 看生圖站的狀態，需要細節就用 anima_docs 讀說明。要花錢、刪東西、改設定之前一定要先問我；每做完一步就跟我說結果，不要自己一直往下做。";
const AI_HINT_KEY = "aiHintSeen";

/** 關掉導引橫幅，並記住（之後不再跳） */
function hideAiHint() {
  const h = F("aiHint");
  if (h) h.hidden = true;
  F("aiBtn").classList.remove("pip");
  store.set(AI_HINT_KEY, true);
}

/** 標題列的 AI MCP 燈號：問後端最近一次 MCP 心跳（未接上紅燈、接上綠燈） */
async function refreshAiLink() {
  const chip = F("aiBtn");
  if (!chip) return;
  try {
    const r = await api("/api/ai/link");
    chip.classList.toggle("on", !!r.online);
    const st = F("aiState");
    if (st) st.textContent = r.online ? "已接上" : "未接上";
    chip.title = r.online
      ? `AI MCP：已接上（${r.client || "AI 程式"}），點這裡看設定`
      : "AI MCP：還沒接上（點這裡看怎麼接）";
  } catch {
    chip.classList.remove("on");
  }
}

/** 「給 AI 操作」視窗（影音工作台、設定頁、標題列的 AI MCP 共用） */
export async function openAi() {
  refreshAiLink();
  openModal("#stAiModal");
  F("stAiNote").textContent = "";
  let r;
  try { r = await api("/api/studio/ai"); } catch (e) { return toast(e.message, "err"); }
  F("stAiMcp").value = r.mcp_json;
  F("stAiGuide").value = r.guide;
  F("stAiHello").value = AI_HELLO;
  const copy = (id, text, what) => (F(id).onclick = async () => { await copyText(text); F("stAiNote").textContent = `已複製${what}`; });
  copy("stAiCopyMcp", r.mcp_json, " MCP 設定");
  copy("stAiCopyDsh", r.dsh_yaml, " dsh 版設定");
  copy("stAiCopyToml", r.codex_toml, " Codex 版設定");
  copy("stAiCopyGuide", r.guide, "說明，貼給 AI 就行");
  copy("stAiCopyHello", AI_HELLO, "開場白，貼進 AI 的第一個訊息就行");
  if (!r.mcp.mcpServers[r.mcp_name].command.toLowerCase().endsWith("node.exe")) {
    F("stAiNote").textContent = "還沒有 Node.js：到「剪輯台」分頁按「下載剪輯工具」，或打開「安裝與修復」再跑一次安裝，設定裡的路徑才會是對的";
  }
}

// ================================================================ 拆幀台
let sess = null;   // 目前這份的 meta
let cur = 1;       // 目前第幾幀（從 1 開始）
let playing = null;

const fUrl = (n, thumb) => `/api/studio/frames/${sess.id}/${thumb ? "t" : "f"}/${n}?v=${sess.rev?.[n] || 0}`;
const fmtFps = (f) => (Math.round(f * 100) / 100).toString();
// 原片第幾秒（只拆一段時加上開始秒數）
const secs = (n) => ((+sess.range?.start || 0) + (n - 1) / (sess.fps || 24)).toFixed(2);
const setFrStatus = (s) => (F("frStatus").textContent = s);

function initFrames() {
  F("frNew").onclick = () => pickFromGallery(splitRec, { video: "only" });
  F("frFile").onclick = () => { F("frFilePick").value = ""; F("frFilePick").click(); };
  F("frFilePick").onchange = () => { const f = F("frFilePick").files[0]; if (f) splitFile(f); };
  F("frSess").onchange = () => openSess(F("frSess").value);
  F("frDel").onclick = delSess;
  F("frGpu").checked = store.get("frGpu", true);
  F("frGpu").onchange = () => store.set("frGpu", F("frGpu").checked);
  F("frStorage").onclick = showFrameStorage;
  F("frPrev").onclick = () => go(cur - 1);
  F("frNext").onclick = () => go(cur + 1);
  F("frPlay").onclick = togglePlay;
  F("frSlider").oninput = () => { stopPlay(); go(+F("frSlider").value); };
  F("frBuild").onclick = build;
  F("frOps").addEventListener("click", (e) => { const b = e.target.closest("button[data-op]"); if (b) op(b.dataset.op, b); });
  document.addEventListener("keydown", (e) => {
    if (app.page !== "director" || tab !== "frames" || !sess || typing(e) || e.ctrlKey || e.altKey || e.metaKey) return;
    if (document.querySelector(".modal:not([hidden])")) return;
    if (e.key === "ArrowLeft") { e.preventDefault(); stopPlay(); go(cur - (e.shiftKey ? 10 : 1)); }
    else if (e.key === "ArrowRight") { e.preventDefault(); stopPlay(); go(cur + (e.shiftKey ? 10 : 1)); }
    else if (e.key === " ") { e.preventDefault(); togglePlay(); }
  });
}

// 外部影片沒有圖庫編號：用檔名；只拆一段的標出秒數
const srcLabel = (s) => (s.source?.id != null ? `#${s.source.id}` : `📁${s.source?.filename || "外部影片"}`) +
  (s.range && (s.range.start > 0 || s.range.end < s.range.of - 0.05) ? `（${(+s.range.start).toFixed(1)}～${(+s.range.end).toFixed(1)}s）` : "") +
  (s.range?.sampled ? `（每秒抽 ${fmtFps(s.fps)} 幀）` : "");
const sessLabel = (s) => `${srcLabel(s)}・${s.frames} 幀・${fmtFps(s.fps)}fps・${s.width}×${s.height}` +
  (s.edited ? `・改了 ${s.edited} 幀` : "") + `・${new Date(s.created * 1000).toLocaleString([], { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })}`;

async function framesShow(want) {
  let r;
  try { r = await api("/api/studio/frames"); } catch (e) { return toast(e.message, "err"); }
  const sel = F("frSess");
  sel.replaceChildren();
  for (const s of r.sessions) sel.append(el("option", { value: s.id }, sessLabel(s)));
  if (!r.sessions.length) {
    sel.append(el("option", { value: "" }, "還沒拆過影片"));
    return openSess("");
  }
  const id = [want, sess?.id, store.get("frSess", "")].find((x) => x && r.sessions.some((s) => s.id === x)) || r.sessions[0].id;
  sel.value = id;
  if (sess?.id !== id || want) await openSess(id); else renderPending();
}

async function openSess(id) {
  stopPlay();
  if (!id) { sess = null; renderSess(); return; }
  try { sess = await api(`/api/studio/frames/${id}`); } catch (e) { toast(e.message, "err"); sess = null; }
  if (sess) store.set("frSess", sess.id);
  cur = 1;
  renderSess();
}

function renderSess() {
  const has = !!sess;
  F("frBig").hidden = !has;
  F("frEmpty").hidden = has;
  F("frDel").disabled = !has;
  F("frBuild").disabled = !has;
  $$("#frOps button, #frPrev, #frNext, #frPlay, #frSlider").forEach((b) => (b.disabled = !has));
  const g = F("frGrid");
  g.replaceChildren();
  if (!has) { F("frNo").textContent = "—"; F("frInfo").textContent = ""; F("frEditInfo").textContent = ""; renderPending(); setFrStatus("待機"); return; }
  F("frSlider").max = sess.frames;
  const frag = document.createDocumentFragment();
  for (let n = 1; n <= sess.frames; n++) {
    frag.append(el("button", { class: "fr-cell" + (sess.edited?.[n] ? " ed" : ""), type: "button", "data-n": n, title: `第 ${n} 幀（${secs(n)} 秒）`, onclick: () => { stopPlay(); go(n); } },
      el("img", { src: fUrl(n, true), loading: "lazy", alt: "" }), el("span", { class: "fr-n" }, n)));
  }
  g.append(frag);
  renderInfo();
  renderPending();
  setFrStatus(sess.source?.id != null ? `來自圖庫 #${sess.source.id}` : `外部影片：${sess.source?.filename || "?"}`);
  go(cur);
}

function renderInfo() {
  const k = Object.keys(sess.edited || {}).length;
  const o = [...F("frSess").options].find((x) => x.value === sess.id);
  if (o) o.textContent = sessLabel({ ...sess, edited: k });
  F("frInfo").textContent = `${sess.frames} 幀・${fmtFps(sess.fps)}fps・${(+sess.duration || 0).toFixed(1)} 秒・${sess.audio ? "有聲音（接回時照原樣放回去）" : "沒有聲音"}・改了 ${k} 幀` +
    (sess.builds?.length ? `・已接回 ${sess.builds.length} 次（圖庫 #${sess.builds.join("、#")}）` : "") +
    (sess.decode === "cpu_fallback" ? "・CUDA 無法使用，已改 CPU 完成" : sess.decode === "cpu" ? "・CPU 解碼" : sess.decode === "cuda_preferred" ? "・GPU 解碼優先" : "");
}

function go(n, thumb = false) {
  if (!sess) return;
  cur = Math.max(1, Math.min(sess.frames, n));
  F("frBig").src = fUrl(cur, thumb);
  F("frSlider").value = cur;
  F("frNo").textContent = `${cur} / ${sess.frames}・${secs(cur)}s`;
  const ed = sess.edited?.[cur];
  F("frEditInfo").textContent = ed ? `這一幀換過（${ed}），不滿意可以按「換回原本的」` : "";
  F("frGrid").querySelector(".fr-cell.on")?.classList.remove("on");
  const c = F("frGrid").children[cur - 1];  // 格子照幀號排，第 n 幀是第 n 個
  if (c) { c.classList.add("on"); if (!playing) c.scrollIntoView({ block: "nearest" }); }
}

function togglePlay() { if (playing) stopPlay(); else startPlay(); }
function startPlay() {
  if (!sess) return;
  if (cur >= sess.frames) cur = 1;
  F("frPlay").textContent = "暫停";
  // 播放時用縮圖（小、快），停下來換回原圖
  playing = setInterval(() => { if (cur >= sess.frames) return stopPlay(); go(cur + 1, true); }, 1000 / (sess.fps || 24));
}
function stopPlay() {
  if (!playing) return;
  clearInterval(playing);
  playing = null;
  F("frPlay").textContent = "播放";
  go(cur);
}

function refreshCell(n) {
  const c = $(`#frGrid .fr-cell[data-n="${n}"]`);
  if (!c) return;
  c.classList.toggle("ed", !!sess.edited?.[n]);
  c.querySelector("img").src = fUrl(n, true);
}

function applyMeta(m, n) {
  sess = m;
  refreshCell(n);
  renderInfo();
  go(cur);
}

async function edit(n, body) {
  applyMeta(await api(`/api/studio/frames/${sess.id}/edit`, { json: { n, ...body } }), n);
}

async function op(kind, btn) {
  if (!sess) return toast("先拆一支影片", "err");
  stopPlay();
  const n = cur;
  btn.disabled = true;
  try {
    if (kind === "blend") { await edit(n, { op: "blend" }); toast(`第 ${n} 幀改成前後兩幀的平均`, "ok"); }
    else if (kind === "prev" || kind === "next") {
      const from = kind === "prev" ? n - 1 : n + 1;
      if (from < 1 || from > sess.frames) return toast(kind === "prev" ? "這是第一幀，前面沒有了" : "這是最後一幀，後面沒有了", "err");
      await edit(n, { op: "copy", from });
      toast(`第 ${n} 幀改成跟第 ${from} 幀一樣`, "ok");
    } else if (kind === "restore") { await edit(n, { op: "restore" }); toast(`第 ${n} 幀換回原本的`, "ok"); }
    else if (kind === "gallery") {
      pickFromGallery(async (rec) => {
        try { await edit(n, { op: "gallery", image_id: rec.id }); toast(`第 ${n} 幀換成圖庫 #${rec.id}`, "ok"); } catch (e) { toast(e.message, "err"); }
      }, { title: `選一張圖換掉第 ${n} 幀` });
    } else if (kind === "upload") {
      const f = await pickFile();
      if (!f) return;
      const r = await fetch(`/api/studio/frames/${sess.id}/upload?n=${n}`, { method: "POST", body: f });
      const d = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(d.error || `HTTP ${r.status}`);
      applyMeta(d, n);
      toast(`第 ${n} 幀換成 ${f.name}`, "ok");
    } else if (kind === "inpaint" || kind === "img2img") {
      const r = await api(`/api/studio/frames/${sess.id}/to_input`, { json: { n } });
      store.set("frPending", { sid: sess.id, n, at: Date.now() });
      gen.useInput(kind, r);
      toast("畫好之後回到「導演 → 拆幀台」，按黃框裡的「選畫好的圖換進去」", "ok");
    }
  } catch (e) {
    toast(e.message, "err");
  } finally {
    btn.disabled = false;
  }
}

// 送去重畫的那一幀：回來時提醒換進去
function renderPending() {
  const p = store.get("frPending", null);
  const box = F("frPending");
  if (!p || !sess || p.sid !== sess.id) { box.hidden = true; return; }
  box.hidden = false;
  box.replaceChildren(
    el("span", {}, `第 ${p.n} 幀送去重畫了。畫好之後：`),
    el("button", { class: "akey sm go", type: "button", onclick: () => pickFromGallery(async (rec) => {
      try {
        await edit(p.n, { op: "gallery", image_id: rec.id });
        store.set("frPending", null);
        renderPending();
        go(p.n);
        toast(`第 ${p.n} 幀換成圖庫 #${rec.id}`, "ok");
      } catch (e) { toast(e.message, "err"); }
    }, { title: `選畫好的圖換掉第 ${p.n} 幀` }) }, "選畫好的圖換進去"),
    el("button", { class: "akey sm", type: "button", onclick: () => { store.set("frPending", null); renderPending(); } }, "不用了"));
}

/** 範圍／抽樣／縮小欄位 → {start, end, fps, max_h}（空白的不送） */
function splitOpts() {
  const o = { cpu: !F("frGpu").checked };
  for (const [k, id] of [["start", "frStart"], ["end", "frEnd"], ["fps", "frFps"], ["max_h", "frMaxH"]]) {
    const v = parseFloat(F(id).value);
    if (Number.isFinite(v) && v > 0) o[k] = v;
  }
  return o;
}

/** 電腦裡的影片：整個檔案送上去（不轉 base64，大檔也行），拆完生圖站會刪掉那份暫存 */
async function splitFile(file) {
  const o = splitOpts();
  const q = new URLSearchParams({ name: file.name, ...Object.fromEntries(Object.entries(o).map(([k, v]) => [k, String(v)])) });
  F("frNew").disabled = F("frFile").disabled = true;
  setFrStatus(`上傳並拆幀中…（${fmtBytes(file.size)}）`);
  try {
    const r = await fetch(`/api/studio/frames/split_file?${q}`, { method: "POST", body: file });
    const m = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(m.error || `HTTP ${r.status}`);
    toast(`拆好了：${m.frames} 幀`, "ok");
    await framesShow(m.id);
  } catch (e) {
    toast(e.message, "err");
    setFrStatus("拆幀失敗");
  } finally {
    F("frNew").disabled = F("frFile").disabled = false;
  }
}

async function splitRec(rec) {
  let r;
  try { r = await api("/api/studio/frames"); } catch { r = { sessions: [] }; }
  const old = r.sessions.find((s) => s.source?.id === rec.id);
  if (old && confirm(`這支影片拆過了（${old.frames} 幀，改了 ${old.edited} 幀）。\n按「確定」打開上次的；按「取消」重新拆一份新的。`)) {
    return framesShow(old.id);
  }
  F("frNew").disabled = true;
  setFrStatus("拆幀中…（10 秒的影片大約幾秒）");
  try {
    const m = await api("/api/studio/frames/split", { json: { id: rec.id, ...splitOpts() }, timeout: 15 * 60 * 1000 });
    toast(`拆好了：${m.frames} 幀`, "ok");
    await framesShow(m.id);
  } catch (e) {
    toast(e.message, "err");
    setFrStatus("拆幀失敗");
  } finally {
    F("frNew").disabled = false;
  }
}

// 圖庫／預覽區的「拆幀修片」
export async function openFrames(rec) {
  tab = "frames";
  store.set("dirTab", "frames");
  app.showPage("director");
  await splitRec(rec);
}

async function showFrameStorage() {
  stopPlay();
  const sid = sess?.id || "";
  const opener = F("frStorage");
  opener.disabled = true;
  try {
    const info = await api(`/api/studio/frames/storage?sid=${encodeURIComponent(sid)}`);
    const box = F("frStorageInfo");
    box.replaceChildren();
    for (const [scope, label] of [["session", "這份拆幀"], ["trash", "已移除拆幀"]]) {
      const data = info[scope];
      const button = F(scope === "session" ? "frClearSession" : "frClearTrash");
      button.disabled = !data?.sessions || data.skipped > 0;
      if (!data) continue;
      box.append(el("p", {}, `${label}：${fmtBytes(data.bytes)}（${data.sessions} 份）`), el("p", { class: "muted sm" }, data.path));
      if (data.skipped) box.append(el("p", { class: "muted" }, "含連結或無法讀取的檔案，暫不清除。"));
      button.onclick = async () => {
        if (button.disabled) return;
        if (!confirm(`永久清除「${label}」？約 ${fmtBytes(data.bytes)}。\n包含拆幀圖片、聲音及所有修幀紀錄，無法復原。原影片和接回後的影片會保留。`)) return;
        F("frClearSession").disabled = F("frClearTrash").disabled = true;
        F("frStorage").disabled = true;
        try {
          const result = await api("/api/studio/frames/clear", { json: { scope, sid, confirm: true }, timeout: 15 * 60 * 1000 });
          toast(`已釋放 ${fmtBytes(result.bytes)}`, "ok");
          if (result.skipped) toast("部分檔案未清除，可能正在使用；請稍後重試。", "err");
          if (scope === "session" && result.removed) {
            if (store.get("frPending", null)?.sid === sid) store.set("frPending", null);
            if (sess?.id === sid) { stopPlay(); sess = null; renderSess(); }
          }
          closeModal("#frStorageModal");
          await framesShow();
        } catch (e) {
          toast(e.message, "err");
          closeModal("#frStorageModal");
        } finally { F("frStorage").disabled = false; }
      };
    }
    openModal("#frStorageModal");
  } catch (e) { toast(e.message, "err"); }
  finally { opener.disabled = false; }
}

async function delSess() {
  if (!sess || !confirm("這份拆開的幀移到待刪除？（圖庫裡的影片不受影響）")) return;
  try { await api(`/api/studio/frames/${sess.id}/delete`, { json: {} }); } catch (e) { return toast(e.message, "err"); }
  if (store.get("frPending", null)?.sid === sess.id) store.set("frPending", null);
  sess = null;
  toast("已移到待刪除", "ok");
  framesShow();
}

async function build() {
  if (!sess) return;
  stopPlay();
  const b = F("frBuild");
  b.disabled = true;
  setFrStatus("接回影片中…");
  try {
    const r = await api(`/api/studio/frames/${sess.id}/build`, { json: {} });
    sess.builds = [...(sess.builds || []), r.image.id];
    renderInfo();
    setFrStatus(`接好了：圖庫 #${r.image.id}`);
    toast(`接好了，存進圖庫（#${r.image.id}）`, "ok");
    F("frInfo").append(" ", el("button", { class: "akey sm", type: "button", onclick: () => openLightbox([r.image], 0) }, "看影片"),
      " ", el("button", { class: "akey sm", type: "button", onclick: async () => {
        try { await api("/api/studio/import", { json: { id: r.image.id } }); toast("已放進剪輯台的素材區", "ok"); } catch (e) { toast(e.message, "err"); }
      } }, "送到剪輯台"));
  } catch (e) {
    toast(e.message, "err");
    setFrStatus("接回失敗");
  } finally {
    b.disabled = false;
  }
}

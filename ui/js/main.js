// 進入點：分頁切換、websocket、載入伺服器狀態
import { $, $$, api, toast } from "./util.js";
import * as gen from "./gen.js";
import * as gallery from "./gallery.js";
import * as posewear from "./posewear.js";
import * as booru from "./booru.js";
import * as assets from "./assets.js";
import * as loras from "./loras.js";
import * as hub from "./hub.js";
import * as feature from "./feature.js";
import * as settings from "./settings.js";
import * as llm from "./llm.js";
import * as pe from "./pe.js";
import * as queue from "./queue.js";
import * as system from "./system.js";
import { app } from "./state.js";
import * as deck from "./deck.js";
import * as director from "./director.js";
import * as studio from "./studio.js";
import * as comic from "./comic.js";
import * as colorbook from "./colorbook.js";
import * as agent from "./agent.js";
import { onAgentUi } from "./agentui.js";
import * as play from "./play.js";
import * as h3 from "./h3.js";
import * as h3chain from "./h3chain.js";
import * as train from "./train.js";
import * as shotsheet from "./shotsheet.js";
import * as framepick from "./framepick.js";
import * as charsheet from "./charsheet.js";
import * as i18n from "./i18n.js";
import * as diagnostics from "./diagnostics.js";
import * as stage from "./stage.js";
import * as finder from "./finder.js";
import * as videostage from "./videostage.js";
import * as pagehead from "./pagehead.js";
import { mountToc } from "./formtoc.js";
import * as topbar from "./topbar.js";
import { closeAllCaps, mountDock } from "./dock.js";
import * as taglibPage from "./taglibpage.js";

function showPage(p) {
  if (app.page !== p) closeAllCaps();
  // 隨機頁借走的生成頁面板：離開就還回去（要在生成頁顯示之前，自由版面才量得到）
  if (app.page === "taglib" && p !== "taglib") taglibPage.hide();
  app.page = p;
  topbar.markPage(p);
  if (p === "taglib") taglibPage.show();
  $$(".page").forEach((pg) => pg.classList.toggle("on", pg.id === `page-${p}`));
  stage.refreshChips();   // ENGINE、尺寸膠囊看生成頁有沒有顯示（隨機頁才出現），要在換好 on 之後算
  if (p === "gallery") gallery.show();
  if (p === "assets") assets.show();
  if (p === "posewear") posewear.showPage();
  if (p === "booru") booru.show();
  if (p === "loras") loras.showPage();
  if (p === "civitai") hub.show();
  if (p === "settings") settings.show();
  if (p === "api") llm.show();
  if (p === "director") { director.show(); studio.show(); }
  if (p === "comic") { comic.show(); colorbook.show(); }
  if (p === "agent") agent.show();
  if (p === "train") train.show();
}
$("#tabs").addEventListener("click", (e) => { const b = e.target.closest("button[data-page]"); if (b && !b.disabled) showPage(b.dataset.page); });
// 頁面裡「到 API 頁」這種跳頁按鈕：data-go＝頁名
document.addEventListener("click", (e) => { const b = e.target.closest("button[data-go]"); if (b) showPage(b.dataset.go); });
app.showPage = showPage;

// 好幾個地方會同時要求重讀（換設定、ComfyUI 上線、重新連線）：只收最後一次的結果，晚回的舊狀態不能蓋掉新的
let stateSeq = 0;
async function refreshState() {
  const seq = ++stateSeq;
  let st;
  try {
    st = await api("/api/state");
  } catch (e) {
    if (seq === stateSeq) toast("無法連到生圖站伺服器：" + e.message + "\n解法：從右下角系統列的生圖站圖示按右鍵「結束」，再重新開啟「繪匣 PaintDeck」（按視窗的 X 常常只是縮到系統列，程式還在）。", "err");
    return;
  }
  if (seq !== stateSeq) return;
  app.state = st;
  setOnline(app.state.online, app.state.remaining);
  gen.onState(app.state);
  loras.onState(app.state);
  settings.onState(app.state);
  pe.onStatus(app.state.pe);
  pe.refreshTask();
}

app.refreshState = refreshState;

function setOnline(on, remaining) {
  $("#comfyDot").classList.toggle("on", !!on);
  $("#comfyDot").title = on ? "ComfyUI 已連線" : "ComfyUI 沒有連線：點狀態列的系統狀態，按「重開 ComfyUI」";
  $("#comfyDot").setAttribute("aria-label", $("#comfyDot").title);
  deck.setOnline(!!on);
  if (remaining !== undefined) queue.onQueue(remaining);
}

// 斷線重連（伺服器重開、換版）後要重讀整份狀態：斷線那段時間的 comfy 事件收不到，模型清單和設定可能已經變了
let connectedOnce = false;
function connect() {
  const ws = new WebSocket(`ws://${location.host}/ws`);
  ws.binaryType = "blob";
  ws.onmessage = (ev) => {
    if (typeof ev.data !== "string") return gen.onPreview(ev.data);
    const m = JSON.parse(ev.data);
    diagnostics.onEvent(m);
    switch (m.type) {
      case "hello": setOnline(m.online, m.remaining); if (connectedOnce) { refreshState(); gen.reconcile(); } connectedOnce = true; break;
      case "comfy": setOnline(m.online); if (m.online) refreshState(); else toast("ComfyUI 連線中斷，正在重新連線…", "err"); break;
      case "queue": setOnline(true, m.remaining); if (!m.remaining) { $("#topProg > div").style.width = "0"; deck.idle(); } break;
      case "start": gen.onStart(m.job); queue.onStart(m.job); deck.onStart(m.job, gen.S.pending.has(m.job)); break;
      case "progress": gen.onProgress(m); queue.onProgress(m); deck.onProgress(m); $("#topProg > div").style.width = `${(100 * m.value) / m.max}%`; break;
      case "image": if (gen.S.pending.has(m.job)) queue.onImage(m.job, m.image); play.onImage(m.job, m.image); gen.onImage(m.job, m.image); gallery.onNewImage(m.image); charsheet.onImage(m.job, m.image); h3.onImage(m.job, m.image); framepick.onImage(m.job, m.image); train.onImage(m.job, m.image); break;
      case "director": case "director_llm": director.onEvent(m); break;
      case "comic": comic.onEvent(m); break;
      case "colorbook": colorbook.onEvent(m); break;
      case "agent": case "agent_llm": case "agent_lora": agent.onEvent(m); break;
      case "agent_ui": onAgentUi(m); break;
      case "agent_cloud": agent.onEvent(m); h3.onEvent(m); h3chain.onEvent(m); break;
      case "h3": h3.onEvent(m); break;
      case "h3chain": h3chain.onEvent(m); break;
      case "train": train.onEvent(m); break;
      case "finish": {
        const mine = gen.S.pending.has(m.job);
        play.onFinish(m);
        gen.onFinish(m);
        queue.onFinish(m, gen.S.pending.size, mine);
        $("#topProg > div").style.width = "0";
        deck.onFinish(m);
        break;
      }
      case "download": hub.onDownload(m); break;
      case "lora_cover": loras.onCover(m); break;
      // 酒館（A1111 相容接口）送來的圖：LoRA 找不到、跟模型不合這類提醒酒館不會顯示，在這裡講
      case "sdapi": if (m.warnings?.length) toast("酒館生圖：" + m.warnings.join("；"), "err"); break;
      case "hubrun": hub.onRun(m); break;
      case "pe": if (app.state) app.state.pe = m; pe.onStatus(m); settings.onPe?.(m); deck.onPe(m); break;
    }
  };
  ws.onclose = () => { setOnline(false); setTimeout(connect, 2000); };
}

$("#peChip").onclick = async () => {
  if (!confirm("現在關掉 AI 改寫模型（llama-server）？正在改寫的會中斷。")) return;
  try { await api("/api/pe/stop", { json: {} }); toast("已關閉改寫模型", "ok"); } catch (e) { toast(e.message, "err"); }
};
// 改寫模型的閒置倒數：每 5 秒更新一次狀態列
setInterval(async () => { if (app.state?.pe?.running && !document.hidden) { try { const st = await api("/api/pe/status"); app.state.pe = st; pe.onStatus(st); } catch { /* 伺服器重開中 */ } } }, 5000);

// 沒綁 <label> 的選單、輸入框：拿旁邊那行小標題（.sub／.name／.lbl）當無障礙名稱，讀屏軟體才念得出是什麼（2026-10-20）。
// 要在 i18n.init() 前面做，換語言時 aria-label 會跟著翻。
function labelControls() {
  const near = (c) => {
    for (const p of [c.previousElementSibling, c.parentElement?.previousElementSibling]) {
      if (!p) continue;
      const t = p.matches(".sub, .name, .lbl, summary") ? p : p.querySelector(".sub, .name, .lbl");
      const s = t?.textContent.trim().replace(/\s+/g, " ");
      if (s) return s;
    }
    return "";
  };
  for (const c of $$("select, textarea, input:not([type=checkbox]):not([type=radio]):not([type=hidden])")) {
    if (c.closest("label, .hidden-inputs, [aria-hidden=true]") || c.getAttribute("aria-label") || c.getAttribute("aria-labelledby") || c.title) continue;
    if (c.id && document.querySelector(`label[for="${c.id}"]`)) continue;
    const s = near(c);
    if (s) c.setAttribute("aria-label", s);
  }
}
labelControls();

// 中央舞台與其他頁各自儲存停靠位置。
// 2026-11-23 左右兩欄（小克案）：模型與輸入、尺寸與模式設定從膠囊搬到左區（切模式時裡面跟著換），右區上面是「這張圖」的成品動作。
// 左區兩塊也留膠囊（chip）：隨機頁只借提示詞卡、看不到左區，靠它們換模型、改尺寸（stage.capsSpec 的 visible）
const stageDock = mountDock($("#page-gen"), {
  name: "stage", caps: stage.capsSpec(),
  main: [$("#stageModes"), $("#page-gen > .monitor")],
  panels: [
    { id: "source", title: "模型與輸入", zone: "left", chip: true },
    { id: "params", title: "尺寸與模式設定", zone: "left", chip: true },
    { id: "lora", title: "LoRA 卡匣", zone: "cap" },
    { id: "sampler", title: "取樣與種子", zone: "cap" },
    { id: "extra", title: "附加", zone: "cap" },
    { id: "layout", title: "版面框", zone: "cap" },
    { id: "pe", title: "AI 改寫", zone: "cap" },
    { id: "compose", title: "提示詞卡", zone: "bottom", min: 200 },
    { id: "imgacts", title: "這張圖", zone: "right" },
    { id: "tasks", title: "任務", zone: "right", min: 300 },
  ],
  sizes: { left: 300, right: 300, bottom: 300, top: 160 },   // 下區 300：標題列＋提示詞＋負面詞＋膠囊列剛好放得下，不用在卡裡捲
  onRender: () => stage.placeActs(),
});
// 影片分頁（中央舞台第二期）：短片、長片跟生成頁同一副骨架；舊的 dock_h3、dock_h3long 存檔不讀也不刪
const h3Dock = mountDock($("#h3Work"), {
  name: "h3stage", inset: true, caps: videostage.shortCaps(), main: $("#h3Stage"),
  panels: [
    { id: "start", title: "起手式", zone: "cap" },
    { id: "board", title: "素材板", zone: "cap" },
    { id: "shots", title: "分鏡與完整提示詞", zone: "cap" },
    { id: "where", title: "在哪跑", zone: "cap" },
    { id: "adv", title: "進階（模型、LoRA、低顯存）", zone: "cap" },
    { id: "h3compose", title: "提示詞卡", zone: "bottom", min: 200 },
    { id: "h3tasks", title: "工作與成品", zone: "right", min: 280 },
  ],
  sizes: { left: 360, right: 330, bottom: 300, top: 160 },
});
const hcDock = mountDock($("#hcHost"), {
  name: "hcstage", inset: true, caps: videostage.longCaps(), main: $("#hcStage"),
  panels: [
    { id: "film", title: "長片", zone: "cap" },
    { id: "shared", title: "整條共用", zone: "cap" },
    { id: "hcrun", title: "開拍", zone: "bottom", min: 100 },
    { id: "hcshoot", title: "開拍與成品", zone: "right", min: 280 },
  ],
  sizes: { left: 360, right: 330, bottom: 120, top: 160 },
});
// 助手頁（中央舞台第三期）：主區＝對話螢幕＋輸入框，右邊＝每輪的圖，其他設定收進輸入框下面的膠囊
const agText = (id) => (document.getElementById(id)?.textContent || "").replace(/\s+/g, " ").trim();
const agDock = mountDock($("#agHost"), {
  name: "agstage", inset: true, main: $("#agHost > .ag-bezel"),
  caps: { host: $("#agCapRow"), chips: {
    agbrain: { label: "腦袋", summary: () => `${agText("agBrainR") || "—"} · ${$("#agRounds").value} 輪 · ×${$("#agCount").value}` },
    agbase: { label: "生圖設定", summary: () => pagehead.pillText($("#agBModel")?.selectedOptions[0]?.textContent || agText("agBaseInfo"), 14) },
    agcloud: { label: "雲端", summary: () => pagehead.pillText(agText("agCloudMsg"), 8) },
    aglora: { label: "LoRA", summary: () => "" },
  } },
  panels: [
    { id: "agbrain", title: "腦袋與旋鈕", zone: "cap" },
    { id: "agbase", title: "生圖設定", zone: "cap" },
    { id: "agcloud", title: "雲端顯卡", zone: "cap" },
    { id: "aglora", title: "LoRA 認識度", zone: "cap" },
    { id: "agstrip", title: "每一輪的圖", zone: "right", min: 260 },
  ],
  sizes: { left: 360, right: 380, bottom: 200, top: 160 },
});
// 其他頁（2026-10-26 使用者：「我剛講的是所有，包含其他功能的介面」）：每頁原本的欄位 → 一欄當主區，其他欄變面板
{
  const cols = (host) => [...$(host).children];
  const dockCols = (name, host, mainAt, panels, sizes) => {
    const c = cols(host);
    mountDock($(host), { name, inset: true, main: c[mainAt], panels: panels.map((p) => ({ ...p, el: c[p.at] })), sizes: { top: 220, left: 380, right: 340, bottom: 260, ...sizes } });
  };
  dockCols("assets", "#page-assets .as-wrap", 0, [{ id: "edit", title: "編輯資產", zone: "right", at: 1, min: 360 }], { right: 420 });
  dockCols("director", "#page-director section[data-tab=plan] > .dir-cols", 1, [{ id: "story", title: "劇情與設定", zone: "left", at: 0, min: 300 }], { left: 400 });
  dockCols("frames", "#frCols", 0, [{ id: "frames", title: "全部的幀", zone: "right", at: 1 }], { right: 420 });
  dockCols("comic", "#page-comic .cm-cols", 1, [{ id: "layout", title: "版型與分格", zone: "left", at: 0, min: 320 }], { left: 440 });
}

// 介面語言：存的不是繁中才載入（簡中 OpenCC、英文字典）；先翻好原始畫面再讓各頁初始化（觀景窗的字會被 deck.js 切成小段），
// 之後新長出來的字由 MutationObserver 接著翻。繁中直接回來，不用等。
await i18n.init();
deck.init();
system.init();
topbar.init();
gen.init();
stage.init(stageDock);
finder.init(stageDock);
taglibPage.init();   // 要在 gen.init 之後：隨機頁接的是生成頁的提示詞框
gallery.init();
assets.init();
loras.init();
hub.init();
train.init();
feature.initFeatures();
settings.init();
llm.init();
queue.init();
director.init();
studio.init();
h3.init();
h3chain.init();
shotsheet.init();
videostage.init();
videostage.initShort(h3Dock);
videostage.initLong(hcDock);
videostage.watchCaps(agDock, $("#agHost"), [$("#agBrainR"), $("#agBaseInfo"), $("#agBModel"), $("#agCloudMsg")]);
// 中央舞台第三～五期：頁首列的專案膠囊（選單和新增／改名／刪除收成一顆）、ⓘ、表單頁的段落目錄
// 外框只管外觀：哪一個掛不上就略過（console 留紀錄），不能讓後面各頁的初始化跟著停掉
const safe = (name, fn) => { try { fn(); } catch (e) { console.error(`版面外框「${name}」掛不上`, e); } };
safe("ⓘ與彈出窗", () => pagehead.initPageHead());
safe("導演專案膠囊", () => pagehead.projPill($("#dirProj"), [$("#dirNew"), $("#dirRename"), $("#dirDel")], { label: "專案" }));
safe("拆幀台膠囊", () => pagehead.projPill($("#frSess"), [$("#frStorage"), $("#frDel")], { label: "影片" }));
safe("漫畫作品膠囊", () => pagehead.projPill($("#cmProj"), [$("#cmNew"), $("#cmRename"), $("#cmDel"), $("#cmExample")], { label: "作品" }));
safe("助手對話膠囊", () => pagehead.projPill($("#agSess"), [$("#agNew"), $("#agDel")], { label: "對話" }));
safe("訓練專案膠囊", () => pagehead.listPill($("#trList"), [$("#trNew"), $("#trDel")], { label: "專案", at: $("#trNew") }));
safe("設定頁目錄", () => mountToc($("#page-settings .lib-scroll"), $("#page-settings .settings"), [
  { key: "general", label: "一般" }, { key: "net", label: "連線" }, { key: "model", label: "模型" }, { key: "ai", label: "AI" }, { key: "system", label: "系統" },
]));
safe("API 頁目錄", () => mountToc($("#page-api .lib-scroll"), $("#page-api .settings")));
comic.init();
colorbook.init();
agent.init();
charsheet.init();
refreshState();
connect();

// 網址的 #gallery、#loras、#civitai、#settings、#director、#comic、#agent 直接打開那一頁；#jobs 打開任務面板、#help 打開說明（系統列選單也用這個）
function fromHash() {
  const h = location.hash.slice(1);
  if (["gen", "taglib", "gallery", "assets", "loras", "civitai", "settings", "director", "comic", "agent", "train", "api"].includes(h)) showPage(h);
  else if (h === "jobs") queue.toggle();
  else if (h === "help") system.openHelp();
}
window.addEventListener("hashchange", fromHash);
fromHash();

// 生圖助手改成獨立視窗：頂列的「✦ 助手」開新視窗（桌面版走 main.rs 的 open_assistant，瀏覽器版用 window.open），
// 新視窗載入同一頁加 ?solo=agent，只顯示助手頁（css 的 .solo）
const solo = new URLSearchParams(location.search).get("solo") === "agent";
if (solo) {
  document.documentElement.classList.add("solo");
  document.title = "繪匣 助手";
  showPage("agent");
}
$("#assistBtn").addEventListener("click", () => {
  const tauri = window.__TAURI__;
  if (tauri?.core) tauri.core.invoke("open_assistant").catch((e) => console.warn("開不了助手視窗", e));
  else window.open("/?solo=agent#agent", "paintdeck-assistant", "width=560,height=860");
});

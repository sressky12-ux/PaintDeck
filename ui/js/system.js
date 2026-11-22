// 系統狀態列（顯存、記憶體、硬碟、ComfyUI、改寫模型）、淺色／深色主題、快捷鍵說明、全域快捷鍵
import { $, el, api, toast, store, fmtBytes, GB, typing, openModal } from "./util.js";
import { app } from "./state.js";
import { setComfyNote, restartComfy } from "./deck.js";
import { altPage } from "./topbar.js";
import { displayVersion } from "./updates.js";

const F = (id) => $("#" + id);
let sys = null;

export function init() {
  // 主題：記住上次選的；沒選過就跟系統
  applyTheme(store.get("theme", "auto"));
  F("rocker").onclick = (e) => {
    const b = e.target.closest("button[data-t]");
    if (!b) return;
    store.set("theme", b.dataset.t);
    // 新配色從按下的那顆開關推開（motion.css 的 theme-wipe）；不支援或關了動態效果就直接換
    if (!document.startViewTransition || matchMedia("(prefers-reduced-motion: reduce)").matches) return applyTheme(b.dataset.t);
    const r = b.getBoundingClientRect(), root = document.documentElement.style;
    root.setProperty("--vt-x", `${r.left + r.width / 2}px`);
    root.setProperty("--vt-y", `${r.top + r.height / 2}px`);
    document.startViewTransition(() => applyTheme(b.dataset.t));
  };
  F("helpBtn").onclick = () => openHelp();
  F("sysBtn").onclick = (e) => { e.stopPropagation(); F("sysPop").hidden ? (renderPop(), (F("sysPop").hidden = false)) : (F("sysPop").hidden = true); };
  document.addEventListener("click", (e) => { if (!F("sysPop").hidden && !F("sysPop").contains(e.target)) F("sysPop").hidden = true; });
  document.addEventListener("keydown", globalKeys);
  poll();
  setInterval(() => { if (!document.hidden) poll(); }, 5000);
}

function applyTheme(t) {
  const root = document.documentElement;
  if (t === "auto") root.removeAttribute("data-theme"); else root.setAttribute("data-theme", t);
  const pos = { light: 0, auto: 1, dark: 2 }[t] ?? 1;
  F("rocker").style.setProperty("--pos", pos);
  for (const b of F("rocker").querySelectorAll("button")) b.setAttribute("aria-checked", String(b.dataset.t === t));
}

export function openHelp() {
  F("helpVersion").textContent = `繪匣 PaintDeck v${app.state?.version || sys?.version || "?"}` + (sys?.comfy_version ? `・ComfyUI ${sys.comfy_version}` : "") ;
  openModal("#helpModal");
}

// ---------------------------------------------------------------- 全域快捷鍵
function globalKeys(e) {
  if (e.altKey && !e.ctrlKey && /^[0-9]$/.test(e.key) && !typing(e)) {
    const page = altPage(e.key);
    if (page) { e.preventDefault(); app.showPage(page); }
    return;
  }
  if (e.key === "Escape") {
    // 由上往下關最上面那個（燈箱、比較視窗有自己的處理，這裡補其他的）
    const open = [...document.querySelectorAll(".modal:not([hidden])")].filter((m) => !["lightbox", "cmpModal", "maskModal"].includes(m.id));
    // preventDefault：同一個 Esc 不要再讓燈箱自己也關掉（燈箱上面開著存檔、反推之類的視窗時）
    if (open.length) { const modal = open[open.length - 1]; if (modal.oncancel) modal.oncancel(); else modal.hidden = true; e.preventDefault(); return; }
    if (!F("jobsDrawer").hidden) { F("jobsDrawer").hidden = true; return; }
    if (!F("sysPop").hidden) { F("sysPop").hidden = true; return; }
  }
  if (typing(e) || e.ctrlKey || e.metaKey || e.altKey || document.querySelector(".modal:not([hidden])")) return;
  if (e.key === "?") { e.preventDefault(); openHelp(); }
  else if (e.key.toLowerCase() === "q") { e.preventDefault(); import("./queue.js").then((q) => q.toggle()); }
}

// ---------------------------------------------------------------- 狀態列
async function poll() {
  try { sys = await api("/api/system"); } catch { sys = null; }
  paint();
  comfyNote();
  if (!F("sysPop").hidden) renderPop();
}

// ComfyUI 沒開起來、或自己關掉了（後端從 comfyui.log 找出原因）：跳一次提示；狀態列的彈出視窗也看得到
let noteShown = "";
function comfyNote() {
  const n = sys && !sys.online ? sys.comfy_note || "" : "";
  if (n && n !== noteShown) toast(n, "err");
  if (n !== noteShown) setComfyNote(n);
  noteShown = n;
}

function pct(used, total) { return total ? Math.round((100 * used) / total) : 0; }

function paint() {
  const bar = F("sysVram").firstElementChild;
  if (!sys?.vram) {
    bar.style.width = "0";
    F("sysText").textContent = sys ? (sys.online ? "—" : "未連線") : "伺服器未連線";
    paintMore();
    F("sysBtn").classList.remove("warn", "crit");
    return;
  }
  const v = sys.vram, used = v.total - v.free, p = pct(used, v.total);
  bar.style.width = p + "%";
  const diskLow = sys.disk && sys.disk.free < 10e9;
  F("sysText").textContent = `${(used / GB).toFixed(1)}/${Math.round(v.total / GB)}G`;
  paintMore();
  F("sysBtn").classList.toggle("warn", p >= 85 || diskLow);
  F("sysBtn").classList.toggle("crit", p >= 95 || (sys.disk && sys.disk.free < 3e9));
}

// 狀態列其他讀數：記憶體、輸出硬碟剩餘、版本
function paintMore() {
  F("ramText").textContent = sys?.ram ? `${((sys.ram.total - sys.ram.free) / GB).toFixed(0)}/${Math.round(sys.ram.total / GB)}G` : "—";
  F("diskText").textContent = sys?.disk ? `${sys.disk.path} ${fmtBytes(sys.disk.free).replace(" ", "")}` : "—";
  F("diskText").classList.toggle("w", !!sys?.disk && sys.disk.free < 10e9);
  // 2026-10-21 起版號用日期：26.10.21 顯示成 V261021（年.月.日，主版號 24 以上就是日期）；舊的 1.1.3 這種照舊只顯示主、次版號
  F("verText").textContent = displayVersion(sys?.version);
  F("verText").title = sys?.version || "";
}

function meter(label, used, total, note) {
  const p = pct(used, total);
  return el("div", { class: "sys-row" },
    el("div", { class: "row between" }, el("span", {}, label), el("span", { class: "muted sm" }, total ? `${fmtBytes(used)} / ${fmtBytes(total)}（${p}%）` : "—")),
    el("div", { class: "bar" + (p >= 95 ? " crit" : p >= 85 ? " warn" : "") }, el("div", { style: `width:${p}%` })),
    note ? el("div", { class: "muted sm" }, note) : null);
}

function renderPop() {
  const box = F("sysPop");
  box.innerHTML = "";
  if (!sys) { box.append(el("div", {}, "連不到生圖站伺服器。")); return; }
  const pe = sys.pe || {};
  box.append(
    el("div", { class: "row between" }, el("b", {}, "系統狀態"), el("span", { class: "muted sm" }, `v${sys.version}`)),
    el("div", { class: "sys-row row between" }, el("span", {}, "ComfyUI（生圖引擎）"),
      el("span", { class: sys.online ? "ok" : "err" }, sys.online ? `已連線${sys.comfy_version ? "・" + sys.comfy_version : ""}` : "沒有連線：按下面的「重開 ComfyUI」")),
    !sys.online && sys.comfy_note ? el("div", { class: "warnbox sm" }, sys.comfy_note) : null,
    sys.vram ? meter(`顯存（${sys.vram.name.replace(/^cuda:\d+\s*/, "") || "顯示卡"}）`, sys.vram.total - sys.vram.free, sys.vram.total,
      "整張卡的用量（含遊戲等其他程式）。生圖時本來就會接近滿；卡住或爆顯存時按「釋放顯存」") : null,
    sys.ram ? meter("記憶體", sys.ram.total - sys.ram.free, sys.ram.total) : null,
    sys.disk ? meter(`輸出資料夾所在硬碟（${sys.disk.path}）`, sys.disk.total - sys.disk.free, sys.disk.total, sys.disk.free < 10e9 ? "空間快不夠了：可以清空圖庫回收區，或把舊圖匯出 zip 後移走" : null) : null,
    el("div", { class: "sys-row row between" }, el("span", {}, "AI 改寫模型（llama-server）"),
      el("span", { class: "muted sm" }, !pe.ok ? "無法使用" : pe.running ? `執行中：${pe.kind}${pe.idle_left != null ? `・${pe.idle_left} 秒後關閉` : ""}` : "沒有載入")),
    el("div", { class: "row gap wrap" },
      el("button", { class: "btn sm", title: "叫 ComfyUI 卸載模型、清顯存；也會關掉改寫模型。下一張會重新載入（比較慢）", onclick: freeVram }, "釋放顯存"),
      el("button", { class: "btn sm", onclick: poll }, "重新整理"),
      // ComfyUI 斷線或當掉：直接在這裡重開，不用把整個生圖站關掉（關閉鍵預設只是縮到系統列）
      !sys.online ? el("button", { class: "btn sm primary", onclick: async () => { if (await restartComfy()) setTimeout(poll, 3000); } }, "重開 ComfyUI") : null,
      el("a", { class: "btn sm", href: sys.comfy_url || "http://127.0.0.1:8188", target: "_blank" }, "ComfyUI 原生介面 ↗")));
}

async function freeVram() {
  try {
    const r = await api("/api/free", { json: {} });
    toast("已釋放顯存" + (r.pe_stopped ? "（也關掉了改寫模型）" : "") + "。下一張會重新載入模型。", "ok");
    setTimeout(poll, 1500);
  } catch (e) { toast(e.message, "err"); }
}

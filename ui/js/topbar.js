import { $, $$ } from "./util.js";
import { app } from "./state.js";
import * as queue from "./queue.js";

export const PAGE_ORDER = ["gen", "taglib", "gallery", "assets", "posewear", "loras", "civitai", "director", "comic", "agent", "train", "api", "booru", "jobs", "settings"];
export function altPage(key) {
  if (key === "0") return "settings";
  return /^[1-9]$/.test(key) ? PAGE_ORDER[Number(key) - 1] : null;
}

// 頂列要收到第幾級：0 完整、1 藏型號和 AI 狀態字（top-snug）、2 精簡（top-tight：藏顯存等次要資訊）。
// needs[i] = 第 i 級完整排開要的寬（0 = 還沒量到）；要變寬鬆時多留 slack px，免得在邊界來回跳
export function topLevel(level, needs, avail, slack = 12) {
  if (!needs[0]) return level;
  for (let i = 0; i < needs.length; i++) if (needs[i] + (i < level ? slack : 0) <= avail) return i;
  return needs.length;
}

// 量頂列：暫時拿掉收合的 class、寬度改 max-content，量完馬上還原（同一個 frame 裡，不會閃）。
// 只動 .top 自己，不碰 html，免得整頁重算樣式。桌面版的視窗鍵 #wctl 也在 .top 裡，一起算進去
const LEVELS = ["top-snug", "top-tight"];
let topLv = 0, topNeeds = [0, 0];
function measureTop(top) {
  const keep = top.className, w = top.style.width;
  top.classList.remove(...LEVELS);
  top.classList.add("top-fit");   // 第一次量的時候 media query 的備案還沒關掉，先關
  top.style.width = "max-content";
  const needs = [top.offsetWidth];
  top.classList.add(LEVELS[0]);
  needs.push(top.offsetWidth);
  top.className = keep;
  top.style.width = w;
  return needs;
}
function fitTop(top, head, remeasure) {
  const avail = head.clientWidth;
  if (!avail) return;   // 助手獨立視窗沒有頂列
  if (remeasure || !topNeeds[0]) topNeeds = measureTop(top);
  const lv = topLevel(topLv, topNeeds, avail);
  if (lv === topLv && top.classList.contains("top-fit")) return;
  topLv = lv;
  top.classList.add("top-fit");
  LEVELS.forEach((c, i) => top.classList.toggle(c, lv > i));
}
// 用量的決定頂列要不要收（以前寫死 ≤1450／≤1180px，不知道桌面版多了視窗鍵，整頁被撐寬、右邊被切掉）
function watchTop() {
  const top = $(".top"), head = $(".unit-head");
  if (!top || !head || typeof ResizeObserver !== "function") return;   // 舊環境：交給 app.css 的 media query
  let raf = 0, dirty = true;
  const run = () => { raf = 0; fitTop(top, head, dirty); dirty = false; };
  const later = (remeasure) => { dirty = dirty || remeasure; if (!raf) raf = requestAnimationFrame(run); };
  // 視窗寬度改了：間距是跟著 vw 變的，也重量一次（只量頂列，很便宜）
  new ResizeObserver(() => later(true)).observe(head);
  // 頂列內容變了（目前頁名、AI 晶片字、更新通知、視窗鍵出現…）：快取的寬度作廢
  new MutationObserver(() => later(true)).observe(top, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["hidden"] });
  document.fonts?.ready?.then(() => later(true));
  run();
}

let opened;
function closeMenu(focus = false) {
  if (!opened) return;
  const { panel, button } = opened;
  panel.hidden = true;
  button.setAttribute("aria-expanded", "false");
  opened = null;
  if (focus) button.focus();
}

export function markPage(page) {
  const label = $(`#tabs [data-page="${page}"] .sn-name`);
  $("#topPage").textContent = label?.textContent || "任務";
  for (const key of $$("#tabs .pkey")) {
    const selected = key.dataset.page === page;
    key.classList.toggle("on", selected);
    if (selected) key.setAttribute("aria-current", "page");
    else key.removeAttribute("aria-current");
  }
}

export function init() {
  markPage(app.page || "gen");
  watchTop();
  for (const button of $$("#statusMore, #themeBtn")) {
    const panel = document.getElementById(button.getAttribute("aria-controls"));
    button.addEventListener("click", () => {
      const show = panel.hidden;
      closeMenu();
      if (show) {
        panel.hidden = false;
        button.setAttribute("aria-expanded", "true");
        opened = { button, panel };
      }
    });
  }
  document.addEventListener("pointerdown", (e) => {
    if (opened && !opened.panel.contains(e.target) && !opened.button.contains(e.target)) closeMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (opened && e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeMenu(true); }
  }, true);
  $("#qtxt").onclick = (e) => { e.stopPropagation(); queue.toggle(); };
  $("#rocker").addEventListener("click", (e) => { if (e.target.closest("[data-t]")) closeMenu(true); });
}

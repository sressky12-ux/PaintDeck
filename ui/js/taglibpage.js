// 隨機產生器與生成頁共用提示詞卡、參數膠囊和生成操作。
import { $ } from "./util.js";
import { closeAllCaps } from "./dock.js";
import { refreshChips } from "./stage.js";
import { app } from "./state.js";
import { makeBorrower } from "./borrow.js";
import * as pe from "./pe.js";
import * as wardrobe from "./wardrobe.js";

let lender;

/** 隨機產生器抽出來的整組提示詞 → 提示詞框（取代或接在後面），發 input 讓分段、自動完成那些跟著更新 */
function applyPrompt(text, how) {
  const ta = $("#prompt");
  const cur = ta.value.trim();
  ta.value = how === "append" && cur ? `${cur.replace(/,\s*$/, "")}, ${text}` : text;
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

export function init() {
  lender = makeBorrower([$('[data-dock="compose"]')], $("#tlBorrowCompose"));
  wardrobe.init({ box: $("#tlWardrobe"), onApply: applyPrompt });
  $("#tlCompose").onclick = () => {
    const chip = $('#capRow [data-cap="pe"]');
    if ($("#prompt").value.trim() && chip.getAttribute("aria-expanded") !== "true") chip.click();
    pe.composeFromPrompt();
  };
  // 最新一張成品：跟著看圖區換；點了回生成頁看大圖
  const img = $("#viewImg"), last = $("#tlLast");
  const sync = () => { const src = img.currentSrc || img.getAttribute("src") || ""; last.hidden = !src; if (src) last.querySelector("img").src = src; };
  img.addEventListener("load", sync);
  sync();
  last.onclick = () => app.showPage("gen");
}

// 面板搬走時停靠區內容變短，捲動位置會被夾回 0：借之前記下來，還回去再捲回原位
let scrolls = [];

export function show() {
  closeAllCaps();
  if (!lender.lent) scrolls = [...document.querySelectorAll("#page-gen .dock-zbody, #page-gen .dock-body")].map((e) => [e, e.scrollTop]);
  lender.lend();
  wardrobe.show();
}

export function hide() {
  closeAllCaps();
  lender.giveBack();
  for (const [e, top] of scrolls) e.scrollTop = top;
  scrolls = [];
  refreshChips();
}

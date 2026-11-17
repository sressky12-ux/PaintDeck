// 左邊「隨機」頁（#taglib；2026-11-17，設計：docs\superpowers\specs\2026-11-17-taglib-page-design.md、2026-11-17-wardrobe-and-danbooru-library-design.md）：生成頁的分身。
// 三欄：左＝隨機產生器（wardrobe.js，Comfyui-Anima-Wardrobe-V6 的引擎）；中＝借來的提示詞（含 AI 改寫）、LoRA 卡匣，底下黏著生成鈕＋最新一張；
// 右＝借來的模型與輸入、尺寸與模式、取樣（步數、CFG、張數、種子、整套設定）、附加（高解析修復、構圖控制、自動修臉、X/Y）。
// 進這頁借、離開還（main.js 的 showPage，borrow.js）；「AI 組成完整提示詞」＝把提示詞框的 tag 交給 AI 改寫（pe.js）。
// 2026-11-17 晚：使用者「把詞庫刪掉。僅保留那個隨機產生器，好用。把右邊的生成頁多加一些參數設定」「重新設計版面，使其好用」→ 拿掉詞庫分頁（生成頁的「詞」抽屜照留）。
import { $ } from "./util.js";
import { app } from "./state.js";
import { makeBorrower } from "./borrow.js";
import * as pe from "./pe.js";
import * as wardrobe from "./wardrobe.js";

let lenders = [];

/** 隨機產生器抽出來的整組提示詞 → 提示詞框（取代或接在後面），發 input 讓分段、自動完成那些跟著更新 */
function applyPrompt(text, how) {
  const ta = $("#prompt");
  const cur = ta.value.trim();
  ta.value = how === "append" && cur ? `${cur.replace(/,\s*$/, "")}, ${text}` : text;
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

const docks = (ids) => ids.map((id) => $(`[data-dock="${id}"]`)).filter(Boolean);

export function init() {
  lenders = [
    makeBorrower(docks(["prompt", "lora"]), $("#tlBorrowMid")),
    makeBorrower([$("#genRow")].filter(Boolean), $("#tlBorrowGen")),
    makeBorrower(docks(["source", "params", "sampler", "extra"]), $("#tlBorrowSide")),
  ];
  wardrobe.init({ box: $("#tlWardrobe"), onApply: applyPrompt });
  $("#tlCompose").onclick = () => pe.composeFromPrompt();
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
  if (!lenders[0].lent) scrolls = [...document.querySelectorAll("#page-gen .dock-zbody, #page-gen .dock-body")].map((e) => [e, e.scrollTop]);
  for (const b of lenders) b.lend();
  wardrobe.show();
}

export function hide() {
  for (const b of lenders) b.giveBack();
  for (const [e, top] of scrolls) e.scrollTop = top;
  scrolls = [];
}

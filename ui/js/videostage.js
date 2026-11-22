// 中央舞台第二期：導演頁「影片」分頁（短片 h3.js、長片 h3chain.js、拍攝單 shotsheet.js）的舞台外框。
// 只管外觀：膠囊設定與摘要、短片舞台播最新成品、拍攝單抽屜開關。出片的邏輯都還在原本的檔，元素 id 一個都沒換。
import * as h3 from "./h3.js";
import { fmtCount, fmtModel } from "./stage.js";
import { $, $$, el, store, debounce } from "./util.js";

/** 分鏡數：0 → —，其他 → 「3 鏡」 */
export function fmtShots(n) { return n > 0 ? `${n} 鏡` : "—"; }

/** 「在哪跑」按鈕上的字，空的回 — */
export function fmtWhere(text) { return String(text ?? "").trim() || "—"; }

const selText = (sel, max) => {
  const o = sel?.selectedOptions[0];
  return o && o.value !== "" ? fmtModel(o.textContent.trim(), max) : "—";
};

export function shortCaps() {
  return { host: $("#h3CapRow"), chips: {
    start: { label: "起手式", summary: () => selText($("#h3Tpl"), 12) },
    board: { label: "素材", summary: () => fmtCount($("#h3Items").children.length) },
    shots: { label: "分鏡", summary: () => fmtShots($("#h3Shots").children.length) },
    where: { label: "在哪跑", summary: () => fmtWhere($("#h3Where .on")?.textContent) },
    adv: { label: "模型", summary: () => selText($("#h3Model"), 16) },
  } };
}

export function longCaps() {
  return { host: $("#hcCapRow"), chips: {
    film: { label: "長片", summary: () => selText($("#hcList"), 14) },
    shared: { label: "共用設定", summary: () => ($("#hcLook").value.trim() ? "已填" : "—") },
  } };
}

/** 面板內容一變（打字、選單、程式重畫）就重算膠囊摘要 */
export function watchCaps(api, host, watched, attrs = []) {
  const refresh = debounce(() => api.refreshCaps(), 100);
  for (const g of $$(".dock-grp", host)) {
    g.addEventListener("input", refresh);
    g.addEventListener("change", refresh);
  }
  const observer = new MutationObserver(refresh);
  for (const node of watched) observer.observe(node, { childList: true, characterData: true, subtree: true, attributes: true, attributeFilter: attrs.length ? attrs : ["class"] });
  refresh();
  return refresh;
}

// ---------------------------------------------------------------- 短片
/** 舞台播這一支（圖庫紀錄） */
export function showLatest(rec) {
  if (!rec) return;
  const video = el("video", { src: `/img/${rec.id}`, controls: "", loop: "", playsinline: "", preload: "metadata" });
  $("#h3StageView").replaceChildren(video);
  $("#h3StageTag").textContent = `#${rec.id}`;
}

export function initShort(api) {
  const host = $("#h3Work");
  watchCaps(api, host, [$("#h3Items"), $("#h3Shots"), $("#h3Where"), $("#h3Model"), $("#h3Tpl")]);
  // 提示詞卡上的「＋圖／影片／聲音」＝素材板上那三顆（素材板收在膠囊裡時也加得到）
  for (const [quick, real] of [["h3QAddImage", "h3AddImage"], ["h3QAddVideo", "h3AddVideo"], ["h3QAddAudio", "h3AddAudio"]]) {
    $("#" + quick).onclick = () => $("#" + real).click();
  }
  const counts = $("#h3Counts"), mirror = $("#h3QCounts");
  const paintCounts = () => {
    const n = $("#h3Items").children.length;
    mirror.textContent = n ? `素材 ${n}` : "素材 —";
    mirror.title = counts.textContent.replace(/\s+/g, " ").trim() || "素材板上的素材";
  };
  new MutationObserver(paintCounts).observe(counts, { childList: true, subtree: true, characterData: true });
  new MutationObserver(paintCounts).observe($("#h3Items"), { childList: true });
  paintCounts();
  h3.onOut(showLatest);
}

// ---------------------------------------------------------------- 長片
export function initLong(api) {
  watchCaps(api, $("#hcHost"), [$("#hcList")]);
}

// ---------------------------------------------------------------- 拍攝單抽屜
/** 打開／關上右邊的拍攝單抽屜（記在 store 的 shDrawer） */
export function setDrawer(open) {
  const d = $("#shDrawer");
  if (!d) return;
  d.hidden = !open;
  placeDrawer();
  $("#shOpen").setAttribute("aria-expanded", String(open));
  $("#shOpen").classList.toggle("on", open);
  store.set("shDrawer", open);
}

/** 抽屜的上緣貼在全站頂列下面（頂列高度會隨字級、窄視窗換行改變） */
function placeDrawer() {
  const head = $(".unit-head");
  const top = head && head.getClientRects().length ? head.getBoundingClientRect().bottom : 0;
  $("#shDrawer").style.top = `${Math.max(0, Math.round(top))}px`;
}

export function init() {
  setDrawer(store.get("shDrawer", false));
  window.addEventListener("resize", debounce(placeDrawer, 100));
  $("#shOpen").onclick = () => setDrawer($("#shDrawer").hidden);
  $("#shClose").onclick = () => { setDrawer(false); $("#shOpen").focus({ preventScroll: true }); };
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || $("#shDrawer").hidden || !$("#shDrawer").getClientRects().length) return;
    if (document.querySelector(".modal:not([hidden]), .cap-pop")) return;
    setDrawer(false);
  });
}

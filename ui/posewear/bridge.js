// 衣姿整頁 → 繪匣（繪匣手寫，posewear_vendor.py 不會覆蓋；設計：docs\superpowers\specs\2026-11-22-posewear-page-design.md）。
// 詳情面板多三顆按鈕：加到提示詞／當參考／當構圖控制（姿勢、深度才有），按了 postMessage 給繪匣（ui\js\posewear.js 收），
// 繪匣切到生成頁照抽屜的做法放進去。不改 app.js：目前這一筆用它自己放在全域的 window.APP（st.list[li()]）。
(function () {
  "use strict";
  const A = window.APP;
  if (!A || window.parent === window) return; // 不在繪匣裡開（單獨開這頁）就不加
  const tw = window.PWZH?.mode() === "t";
  const L = tw
    ? { prompt: "加到提示詞", ref: "當參考", ctrl: "當構圖控制", tip: "送到繪匣的生成頁" }
    : { prompt: "加到提示词", ref: "当参考", ctrl: "当构图控制", tip: "送到绘匣的生成页" };
  // ギア 的四個圖庫：後端用 {lib, id} 查原站目錄抓圖；其他（表情、人像、場景、CharaDB…）用 {url}
  const GEAR = { pose: "pose", depth: "depth", costume: "costume", anime: "anime" };
  // posewear 的分類 → 繪匣多圖參考的角色（ui\js\refs.js 的 ROLES）
  const ROLE = { pose: "pose", depth: "pose", costume: "outfit", anime: "outfit", dyn: "outfit", portrait: "person", expr: "person",
    hair: "person", scene: "scene", light: "scene", prop: "object", furn: "object" };

  const base = (u) => (u || "").split(/[?#]/)[0].split("/").pop().replace(/\.[a-z0-9]+$/i, "");

  function payload(it, act) {
    const a = it.angles ? it.angles[it.ai] : null;
    const big = A.ath(a ? a.i : it.i || it.m || it.t);
    const lib = GEAR[it.type] && !(it.type === "anime" && String(it.id).startsWith("charadb-")) ? GEAR[it.type] : "";
    return {
      pw: "use", act, text: A.prompt(it, "en"), role: ROLE[it.type] || "object",
      kind: it.type === "pose" || it.type === "depth" ? "pose" : ROLE[it.type] === "outfit" ? "costume" : "other",
      ...(lib ? { lib, id: a ? base(a.t) : it.id } : { url: big }),
    };
  }

  function add() {
    const side = document.getElementById("lbSide");
    const acts = side?.querySelector(".acts");
    if (!acts || side.querySelector(".pd-acts")) return;
    const it = A.st.list[A.li()];
    if (!it) return;
    const row = document.createElement("div");
    row.className = "acts pd-acts";
    row.title = L.tip;
    const btn = (act, label) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = "➜ " + label;
      b.onclick = () => window.parent.postMessage(payload(A.st.list[A.li()], act), location.origin);
      row.appendChild(b);
    };
    btn("prompt", L.prompt);
    btn("ref", L.ref);
    if (it.type === "pose" || it.type === "depth") btn("ctrl", L.ctrl);
    acts.after(row);
  }

  // 詳情面板每次 lbDraw 都重寫 #lbSide 的 innerHTML：看到重寫就補上按鈕
  const side = document.getElementById("lbSide");
  if (side) new MutationObserver(add).observe(side, { childList: true });

  // 試穿工作台：生成鍵（#wbGo）旁邊「➜ 交給繪匣」，把整套原圖（人物、服裝、姿勢、表情）連同提示詞送到生成頁的多圖參考
  // （2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md；WB.bundle 是搬家／同步時替換進 studio.js 的）
  const HL = tw ? { go: "➜ 交給繪匣", tip: "整套原圖和提示詞送到繪匣生成頁的多圖參考（不用 API 金鑰，用本機模型畫）", busy: "整理中…", none: "這一版的工作台沒有「交出整套」" }
    : { go: "➜ 交给绘匣", tip: "整套原图和提示词送到绘匣生成页的多图参考（不用 API 密钥，用本机模型画）", busy: "整理中…", none: "这一版的工作台没有「交出整套」" };
  function addHandoff() {
    const go = document.getElementById("wbGo");
    if (!go || go.parentElement.querySelector(".pd-handoff")) return;
    const b = document.createElement("button");
    b.type = "button";
    b.className = "pd-handoff ghost";
    b.textContent = HL.go;
    b.title = HL.tip;
    b.onclick = async () => {
      if (typeof window.WB?.bundle !== "function") return A.toast(HL.none);
      b.disabled = true;
      b.textContent = HL.busy;
      try {
        const { en, zh, pl, custom, items } = await window.WB.bundle();
        window.parent.postMessage({ pw: "bundle", en, zh, pl, custom, items }, location.origin);
      } catch (e) {
        A.toast(e.message || String(e));
      } finally {
        b.disabled = false;
        b.textContent = HL.go;
      }
    };
    go.after(b);
  }
  addHandoff();
  const bar = document.getElementById("wbGo")?.parentElement;
  if (bar) new MutationObserver(addHandoff).observe(bar, { childList: true });
})();

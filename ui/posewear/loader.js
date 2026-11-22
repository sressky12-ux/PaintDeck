// 衣姿整頁的載入器（繪匣手寫，posewear_vendor.py 不會覆蓋；設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md）。
// 先問後端 /api/posewear/front/status：官網版能用（live／cache）就載後端套好規則的官網前端檔，繁體頁在這裡即時轉繁；
// 抓不到、規則沒命中、或官網版跑起來出錯，就載安裝檔內建的那一套（app.js… 由 tools\posewear_vendor.py 產生）。
// 官網版的程式頂層有 const，跑到一半出錯不能在同一頁再載內建版（會重複宣告），所以出錯時記一個旗標、重新整理改走內建版。
// 角落一行顯示目前跑的是哪一版，旁邊「檢查官網更新」強制重抓。
(function () {
  "use strict";
  const ORDER = ["app", "zip", "extras", "studio"];
  const FILE = "/api/posewear/front/file/";
  const FLAG = "pw-front-bad"; // sessionStorage：這一版官網程式出過錯（值＝它的 app.js 路徑）

  /** st＝status（抓不到是 null）、tw＝繁體頁、bad＝這個分頁記過出錯的官網版 app.js 路徑 → 這次載哪一套 */
  function plan(st, tw, bad) {
    const sync = !!st && (st.source === "live" || st.source === "cache") && !(bad && st.hash?.["app.js"] === bad);
    if (sync) return { sync, scripts: ORDER.map((n) => `${FILE}${n}.js`).concat("bridge.js"), style: `${FILE}style.css` };
    return { sync, scripts: ORDER.map((n) => (tw && n !== "zip" ? `${n}.tw.js` : `${n}.js`)).concat("bridge.js"), style: null };
  }

  const ver = (st) => (st?.hash?.["app.js"] || "").replace(/^.*\/app\.|\.js$/g, "");

  /** 角落那一行字；fell＝官網版剛出錯、退回內建 */
  function label(st, tw, fell) {
    const T = tw
      ? { live: "衣姿：官網最新版", cache: "衣姿：官網版（舊快取，連不上官網）", built: "衣姿：內建版", changed: "官網改版了，暫時用內建版本", bad: "官網版出錯，暫時用內建版本" }
      : { live: "衣姿：官网最新版", cache: "衣姿：官网版（旧缓存，连不上官网）", built: "衣姿：内置版", changed: "官网改版了，暂时用内置版本", bad: "官网版出错，暂时用内置版本" };
    const b = st?.builtin ? ` ${st.builtin}` : "";
    if (fell) return `${T.built}${b}・${T.bad}`;
    if (st?.source === "live") return `${T.live} ${ver(st)}`;
    if (st?.source === "cache") return `${T.cache} ${ver(st)}`;
    if (st?.failed_rule) return `${T.built}${b}・${T.changed}`;
    return T.built + b;
  }

  if (typeof module !== "undefined" && module.exports) { module.exports = { plan, label }; return; }

  // ---------------------------------------------------------------- 瀏覽器
  const tw = window.PWZH?.mode() === "t";
  const ss = (k, v) => { try { return v === undefined ? sessionStorage.getItem(k) : (v === null ? sessionStorage.removeItem(k) : sessionStorage.setItem(k, v)); } catch { return null; } };

  function script(src) {
    return new Promise((ok, no) => {
      const s = document.createElement("script");
      s.src = src;
      s.async = false;
      s.onload = () => ok();
      s.onerror = () => no(new Error(`載不到 ${src}`));
      document.body.appendChild(s);
    });
  }

  function badge(st, fell) {
    const d = document.createElement("div");
    d.id = "pdFront";
    d.style.cssText = "position:fixed;left:8px;bottom:6px;z-index:50;font:11px/1.6 system-ui,sans-serif;color:#888;background:rgba(255,255,255,.85);border-radius:6px;padding:0 6px;pointer-events:auto";
    d.textContent = label(st, tw, fell) + "　";
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = tw ? "檢查官網更新" : "检查官网更新";
    b.style.cssText = "font:inherit;color:inherit;background:none;border:0;padding:0;text-decoration:underline;cursor:pointer";
    b.onclick = async () => {
      b.disabled = true;
      b.textContent = tw ? "檢查中…" : "检查中…";
      try { await fetch("/api/posewear/front/refresh", { cache: "no-store" }); } catch { /* 連不上就照舊重新整理 */ }
      ss(FLAG, null);
      location.reload();
    };
    d.appendChild(b);
    document.body.appendChild(d);
  }

  let syncStyle = null;

  async function loadSync(p, st) {
    // 先全部抓好再執行：抓的時候出錯還能乾淨地退回內建版
    const get = async (u) => { const r = await fetch(u, { cache: "no-store" }); if (!r.ok) throw new Error(`${u}：HTTP ${r.status}`); return r.text(); };
    const [css, ...js] = await Promise.all([get(p.style), ...p.scripts.slice(0, -1).map(get)]);
    const conv = tw ? await window.PWZH.conv() : null;
    const fix = (t) => (conv ? window.PWZH.twText(t, conv) : t);
    syncStyle = document.createElement("style");
    syncStyle.textContent = fix(css);
    document.head.appendChild(syncStyle);
    for (const l of document.querySelectorAll('link[rel="stylesheet"][href="style.css"]')) l.disabled = true;
    // 官網版跑起來（含之後幾秒）丟出錯誤 → 記旗標、重新整理改走內建版
    const urls = new Set();
    const onErr = (e) => {
      if (![...urls].some((u) => (e.filename || "").startsWith(u))) return;
      ss(FLAG, st.hash?.["app.js"] || "x");
      console.warn("衣姿：官網版出錯，改用內建版重新載入", e.message);
      location.reload();
    };
    window.addEventListener("error", onErr);
    setTimeout(() => window.removeEventListener("error", onErr), 8000);
    for (const t of js) {
      const u = URL.createObjectURL(new Blob([fix(t)], { type: "text/javascript" }));
      urls.add(u);
      await script(u);
    }
    await script("bridge.js");
  }

  async function main() {
    let st = null;
    try { const r = await fetch("/api/posewear/front/status", { cache: "no-store" }); if (r.ok) st = await r.json(); } catch { /* 繪匣後端連不上就用內建 */ }
    const bad = ss(FLAG);
    const p = plan(st, tw, bad);
    if (p.sync) {
      try { await loadSync(p, st); badge(st, false); return; }
      catch (e) {
        // 已經有官網版程式跑起來就不能同頁再載內建版：記旗標重新整理
        if (document.querySelector('script[src^="blob:"]')) { ss(FLAG, st.hash?.["app.js"] || "x"); location.reload(); return; }
        console.warn("衣姿：官網版載入失敗，改用內建版", e);
        syncStyle?.remove();
        for (const l of document.querySelectorAll('link[rel="stylesheet"][href="style.css"]')) l.disabled = false;
        const q = plan(null, tw);
        for (const s of q.scripts) await script(s);
        badge(st, true);
        return;
      }
    }
    for (const s of p.scripts) await script(s);
    badge(st, !!bad && st?.hash?.["app.js"] === bad);
  }
  main();
})();

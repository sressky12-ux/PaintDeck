// 衣姿整頁的簡繁切換（繪匣手寫，posewear_vendor.py 不會覆蓋；設計：docs\superpowers\specs\2026-11-22-posewear-page-design.md）。
// 兩套頁面：index.html（繁體：程式、畫面文字在搬家時已經轉好，資料在這裡抓下來時轉）、index.s.html（簡體＝官網原樣，什麼都不轉）。
// 程式裡的中文字串和資料用同一個 conv 轉（搬家時 tools\posewear_tw.mjs 也是 require 這支），篩選、搜尋才對得上。
// 選擇存在 localStorage["pw-zh"]（"t"／"s"），繪匣開 iframe 時照它挑頁面。
(function () {
  "use strict";
  // OpenCC 轉不到的台灣說法：簡中原詞 → 繁中（跟 tools\posewear_text.mjs 的 TW 同一份；最長的先比）
  const TW = {
    "吊带": "細肩帶", "连衣裙": "洋裝", "开衫": "開襟衫", "连帽卫衣": "連帽上衣", "连裤袜": "褲襪", "长筒袜": "過膝襪",
    "文胸": "胸罩", "马甲": "背心", "波点": "圓點", "毛线帽": "毛帽", "颈链": "頸鍊", "浅口鞋": "包鞋", "乐福鞋": "樂福鞋",
    "运动鞋": "運動鞋", "毛绒披肩": "毛絨披肩", "针织衫": "針織衫", "皮夹克": "皮夾克",
  };
  // OpenCC 轉完之後再改的（它的台灣詞表跟一般寫法不一樣的）
  const POST = [["傢俱", "家具"]];
  // 這些欄位不轉：英文、網址、id、日文原名
  const SKIP = new Set(["en", "kw", "lineEn", "catEn", "t", "i", "m", "o", "id", "no", "ja", "catJa", "lineJa", "site", "authorUrl"]);
  const HAN = /[㐀-鿿]/;

  /** OpenCC 把頭髮的「发」轉成「發」：顏色、髮型字旁邊的改回「髮」（同 ui\js\wardrobe.js 的 fixHair） */
  function fixHair(s) {
    return s.replace(/([黑白銀金棕茶紅粉藍紫綠灰橙黃青褐色短長捲卷直亂濕染假劉中])發/g, "$1髮")
      .replace(/發(?=[型色帶夾飾圈箍髻梢尾量絲辮束根])/g, "髮");
  }

  /** cc＝OpenCC Converter({from:"cn", to:"twp"}) → 帶快取的轉換函式 */
  function make(cc) {
    const keys = Object.keys(TW).sort((a, b) => b.length - a.length);
    const re = new RegExp(keys.join("|"), "g");
    const cache = new Map();
    return (s) => {
      if (typeof s !== "string" || !HAN.test(s)) return s;
      let v = cache.get(s);
      if (v === undefined) {
        v = fixHair(cc(s.replace(re, (m) => TW[m])));
        for (const [a, b] of POST) v = v.split(a).join(b);
        cache.set(s, v);
      }
      return v;
    };
  }

  /** 程式碼轉繁：只轉含中文的字元段（中文、全形標點連在一起的那一段），其他原封不動。
   *  搬家時的 tools\posewear_tw.mjs、執行時同步的 loader.js 都用這支，兩邊轉出來一樣 */
  function twText(text, conv) {
    return text.replace(/[　-〿㐀-鿿＀-￯]+/g, (m) => conv(m));
  }

  /** 資料檔遞迴轉繁：SKIP 的欄位、網址不轉 */
  function deep(v, conv, k) {
    if (typeof v === "string") return SKIP.has(k) || v.startsWith("http") ? v : conv(v);
    if (Array.isArray(v)) return v.map((x) => deep(x, conv, k));
    if (v && typeof v === "object") {
      const o = {};
      for (const [kk, x] of Object.entries(v)) o[kk] = deep(x, conv, kk);
      return o;
    }
    return v;
  }

  if (typeof module !== "undefined" && module.exports) { module.exports = { make, deep, fixHair, TW, twText }; return; }

  // ---------------------------------------------------------------- 瀏覽器
  const KEY = "pw-zh";
  const tw = document.documentElement.dataset.pwzh === "t"; // 繁體頁才轉資料
  let conv = null, ready = null;
  // conv()：繁體頁的轉換函式（loader.js 轉同步下來的官網程式碼用）；簡體頁回 null
  window.PWZH = { mode: () => (tw ? "t" : "s"), make, deep, twText, conv: async () => (tw ? (await ready, conv) : null) };

  if (tw) {
    ready = import(new URL("../vendor/opencc-cn2t.js", document.currentScript?.src || location.href))
      .then(({ Converter }) => { conv = make(Converter({ from: "cn", to: "twp" })); });
    const orig = window.fetch.bind(window);
    window.fetch = async (input, init) => {
      const url = typeof input === "string" ? input : input?.url || "";
      const r = await orig(input, init);
      // 資料檔；試穿代理的錯誤訊息（照官網是簡體）也轉
      const proxyErr = url.includes("/api/proxy") && !r.ok && (r.headers.get("content-type") || "").includes("json");
      if (!(url.includes("/api/posewear/spa/") && r.ok) && !proxyErr) return r;
      try {
        await ready;
        const v = deep(await r.clone().json(), conv);
        return new Response(JSON.stringify(v), { status: r.status, statusText: r.statusText, headers: r.headers });
      } catch { return r; } // OpenCC 載不到就照原樣（簡體）顯示，不讓整頁壞掉
    };
  }

  // 「繁／簡」按鈕：放在描述語言（中／EN）後面，樣式借它的
  function addButton() {
    const seg = document.querySelector('.seg[role="group"]');
    if (!seg || document.getElementById("pwZh")) return;
    const g = document.createElement("div");
    g.className = "seg";
    g.setAttribute("role", "group");
    g.setAttribute("aria-label", tw ? "簡繁" : "简繁");
    for (const [m, label] of [["t", "繁"], ["s", "简"]]) {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = label;
      b.title = m === "t" ? "繁體中文（台灣）" : "简体中文（跟官网一样）";
      if ((m === "t") === tw) b.className = "on";
      b.setAttribute("aria-pressed", String((m === "t") === tw));
      b.onclick = () => {
        if ((m === "t") === tw) return;
        try { localStorage.setItem(KEY, m); } catch { /* 存不了就只切這一次 */ }
        location.replace((m === "t" ? "index.html" : "index.s.html") + location.search + location.hash);
      };
      g.appendChild(b);
    }
    g.id = "pwZh";
    seg.after(g);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", addButton);
  else addButton();
})();

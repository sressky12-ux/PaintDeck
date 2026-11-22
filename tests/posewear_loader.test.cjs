// 衣姿頁跟官網同步：zh.js 共用的繁體轉換、loader.js 決定載哪一套（設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md）
const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");
const fs = require("node:fs");

const dir = path.join(__dirname, "..", "ui", "posewear");
const zh = require(path.join(dir, "zh.js"));
const loader = require(path.join(dir, "loader.js"));

test("twText 只轉中文字元段，ASCII 原樣", () => {
  const conv = (s) => `[${s}]`;
  const src = "toast('请先选择服装');const a=1;// 注释，ok\nx='EN only'";
  assert.equal(zh.twText(src, conv), "toast('[请先选择服装]');const a=1;// [注释，]ok\nx='EN only'");
  assert.equal(zh.twText("abc<>{}", conv), "abc<>{}");
});

test("status 是 live／cache → 同步版；builtin 或抓不到 → 內建版", () => {
  for (const source of ["live", "cache"]) {
    const p = loader.plan({ source }, true);
    assert.equal(p.sync, true);
    assert.deepEqual(p.scripts, ["app", "zip", "extras", "studio"].map((n) => `/api/posewear/front/file/${n}.js`).concat("bridge.js"));
    assert.equal(p.style, "/api/posewear/front/file/style.css");
  }
  for (const st of [{ source: "builtin" }, null, undefined, { error: "x" }]) {
    const p = loader.plan(st, true);
    assert.equal(p.sync, false);
    assert.deepEqual(p.scripts, ["app.tw.js", "zip.js", "extras.tw.js", "studio.tw.js", "bridge.js"]);
    assert.equal(p.style, null);
  }
  assert.deepEqual(loader.plan({ source: "builtin" }, false).scripts, ["app.js", "zip.js", "extras.js", "studio.js", "bridge.js"]);
  // 同步版剛出過錯（這個分頁記了旗標）→ 內建版
  assert.equal(loader.plan({ source: "live", hash: { "app.js": "assets/app.1.js" } }, true, "assets/app.1.js").sync, false);
  assert.equal(loader.plan({ source: "live", hash: { "app.js": "assets/app.2.js" } }, true, "assets/app.1.js").sync, true);
});

test("角落那一行：官網版、內建版、官網改版、出錯退回", () => {
  assert.match(loader.label({ source: "live", hash: { "app.js": "assets/app.d529c13529.js" } }, true, false), /官網.*d529c13529/);
  assert.match(loader.label({ source: "cache", hash: { "app.js": "assets/app.d529c13529.js" } }, true, false), /舊/);
  assert.match(loader.label({ source: "builtin", failed_rule: "app.js：圖片主機 AH", builtin: "70f48e7" }, true, false), /官網改版了，暫時用內建版本/);
  assert.match(loader.label({ source: "builtin", builtin: "70f48e7" }, true, false), /內建.*70f48e7/);
  assert.match(loader.label({ source: "live", builtin: "70f48e7" }, true, true), /出錯.*內建/);
  assert.match(loader.label({ source: "builtin", failed_rule: "x" }, false, false), /官网改版了，暂时用内置版本/);
});

test("內建頁只載 zh.js、loader.js，其餘交給 loader", () => {
  for (const f of ["index.html", "index.s.html"]) {
    const html = fs.readFileSync(path.join(dir, f), "utf8");
    assert.match(html, /<script src="zh\.js" defer><\/script><script src="loader\.js" defer><\/script>/, f);
    assert.doesNotMatch(html, /src="(app|studio|extras|zip|bridge)(\.tw)?\.js"/, f);
  }
});

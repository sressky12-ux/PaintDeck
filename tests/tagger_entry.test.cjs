// 2026-11-23 使用者：「那個[反推圖片提示詞]的，死去哪了」——固定舞台改版後，反推只剩成品動作列「⋯」裡、
// 和 ENGINE 膠囊彈出框的輸入圖區。釘住：提示詞卡工具列有反推鍵、成品動作列的反推不收進「⋯」
const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const html = fs.readFileSync(path.join(__dirname, "..", "ui", "index.html"), "utf8");
const gen = fs.readFileSync(path.join(__dirname, "..", "ui", "js", "gen.js"), "utf8");

test("提示詞卡工具列有反推鍵，跟庫、詞、衣排在一起", () => {
  const tools = html.slice(html.indexOf('id="promptLib"'), html.indexOf('id="promptSecBtn"'));
  assert.match(tools, /id="promptTagger"[^>]*>反推</);
  assert.match(gen, /F\("promptTagger"\)\.onclick/);
});

test("成品動作列的反推在外面，不在「⋯」選單裡", () => {
  const row = html.slice(html.indexOf('id="viewActions"'), html.indexOf('id="viewMoreMenu"'));
  const menu = html.slice(html.indexOf('id="viewMoreMenu"'), html.indexOf("</div>", html.indexOf('id="viewMoreMenu"')));
  assert.match(row, /data-act="tagger"/);
  assert.doesNotMatch(menu, /data-act="tagger"/);
});

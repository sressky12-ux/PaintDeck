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

// 2026-11-23 玩家：「佬，反推好像没办法自己上传图片了」——反推視窗綁死開啟時那張圖（有成品就一定推成品），
// 視窗裡沒有換圖的地方。釘住：視窗上方顯示正在推哪張＋「換一張圖…」，也能拖進來、Ctrl+V 貼上；貼上時生成頁不會順手換掉輸入圖
const tagger = fs.readFileSync(path.join(__dirname, "..", "ui", "js", "tagger.js"), "utf8");

test("反推視窗可以自己換一張圖（選檔、拖進來、貼上）", () => {
  const modal = html.slice(html.indexOf('id="taggerModal"'), html.indexOf('id="tgThr"'));
  assert.match(modal, /id="tgImg"/);
  assert.match(modal, /id="tgPick"[^>]*>換一張圖/);
  assert.match(tagger, /\$\("#tgPick"\)\.onclick/);
  assert.match(tagger, /makeDrop\(/);
  assert.match(tagger, /addEventListener\("paste"/);
});

test("反推視窗開著時，貼上的圖給反推、不給生成頁的輸入圖", () => {
  const paste = gen.slice(gen.indexOf('document.addEventListener("paste"'), gen.indexOf('document.addEventListener("paste"') + 400);
  assert.match(paste, /taggerModal/);
});

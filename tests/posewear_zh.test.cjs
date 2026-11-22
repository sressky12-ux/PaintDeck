// 衣姿整頁的簡繁轉換（ui\posewear\zh.js；設計：docs\superpowers\specs\2026-11-22-posewear-page-design.md）
const test = require("node:test");
const assert = require("node:assert");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { make, deep } = require("../ui/posewear/zh.js");

let conv;
test.before(async () => {
  const { Converter } = await import(pathToFileURL(path.join(__dirname, "..", "ui", "vendor", "opencc-cn2t.js")));
  conv = make(Converter({ from: "cn", to: "twp" }));
});

test("台灣說法：OpenCC 轉不到的詞先換掉", () => {
  assert.strictEqual(conv("连衣裙"), "洋裝");
  assert.strictEqual(conv("米色短款连衣裙"), "米色短款洋裝");
  assert.strictEqual(conv("吊带"), "細肩帶");
  assert.strictEqual(conv("运动鞋"), "運動鞋");
  assert.strictEqual(conv("家具库"), "家具庫"); // OpenCC 會轉成「傢俱」
});

test("頭髮的发轉成髮，不是發", () => {
  assert.strictEqual(conv("棕发"), "棕髮");
  assert.strictEqual(conv("发型"), "髮型");
});

test("沒有中文的字串原樣回傳", () => {
  assert.strictEqual(conv("white sneakers"), "white sneakers");
  assert.strictEqual(conv(""), "");
  assert.strictEqual(conv(undefined), undefined);
});

test("資料檔遞迴轉繁：英文、網址、id、日文欄位不轉", () => {
  const v = deep({ zh: "运动鞋", en: "sneakers", t: "https://x/运动", id: "SPR-0001", ja: "画像",
    angles: [{ zh: "正面", t: "p/t/a.webp" }], n: 3, a: null }, conv);
  assert.deepStrictEqual(v, { zh: "運動鞋", en: "sneakers", t: "https://x/运动", id: "SPR-0001", ja: "画像",
    angles: [{ zh: "正面", t: "p/t/a.webp" }], n: 3, a: null });
});

test("程式字串和資料用同一個轉法：分類名稱轉完一樣", () => {
  // app.js 裡寫死的分類（搬家時轉）跟資料裡的分類（執行時轉）要對得上
  for (const s of ["内衣", "性感", "派对礼服", "双人互动", "全身·性感", "挤胸·乳沟"]) {
    assert.strictEqual(conv(s), deep({ cat: s }, conv).cat);
  }
});

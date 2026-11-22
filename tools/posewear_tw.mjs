// 衣姿整頁（ui\posewear\）的繁體版：由 tools\posewear_vendor.py 呼叫，不用自己跑。
// index.s.html、app.js、extras.js、studio.js 是官網原樣（簡體）；這支產生 index.html（繁體頁）和 *.tw.js（繁體頁由 loader.js 載 *.tw.js）。
// 轉換函式跟執行時轉資料用的是同一支（ui\posewear\zh.js 的 make），程式裡的中文字串和資料才對得上（篩選、搜尋、複製）。
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Converter } from "../ui/vendor/opencc-cn2t.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const dir = join(root, "ui", "posewear");
const { make, twText } = createRequire(import.meta.url)(join(dir, "zh.js"));
const conv = make(Converter({ from: "cn", to: "twp" }));
const JS = ["app.js", "extras.js", "studio.js"];

/** 只轉含中文的字元段（zh.js 的 twText；執行時同步的 loader.js 也用它，兩邊轉出來一樣） */
const tw = (text) => twText(text, conv);

function once(text, from, to, name) {
  const n = text.split(from).length - 1;
  if (n !== 1) { console.error(`${name}：預期 1 處，找到 ${n} 處：${from}`); process.exit(1); }
  return text.replace(from, to);
}

for (const f of JS) writeFileSync(join(dir, f.replace(/\.js$/, ".tw.js")), tw(readFileSync(join(dir, f), "utf8")));

let html = tw(readFileSync(join(dir, "index.s.html"), "utf8"));
html = once(html, '<html lang="zh-CN"', '<html data-pwzh="t" lang="zh-TW"', "index.html");
writeFileSync(join(dir, "index.html"), html);
console.log("繁體頁 OK：index.html、" + JS.map((f) => f.replace(/\.js$/, ".tw.js")).join("、"));

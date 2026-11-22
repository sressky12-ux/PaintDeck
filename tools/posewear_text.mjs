// 衣姿抽屜（2026-11-21）翻譯資料第二段：把 tools\posewear_text.py 抽出的簡中資料轉成繁體台灣用語，
// 寫出 resources\posewear\text.json 和 Rust 測試用的 src-tauri\tests\fixtures\posewear_costume_cases.json。
// 用法：node tools/posewear_text.mjs <posewear_text.py 的輸出 json>
//
// 服裝描述是程式（posewear.rs 的 costume_text）用簡中詞表＋連接詞拼出來，最後才轉繁：
// 這樣句子長短判斷（標題超過 14 字才去顏色）跟 posewear 原版一模一樣。轉繁用 text.json 的 tw 片語表（最長的先比），
// 比不到的單字查 tw_chars。片語表＝每個詞表項目、每個連接詞各自轉好的結果，再套下面 TW 的台灣說法。
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Converter } from "../ui/vendor/opencc-cn2t.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const raw = JSON.parse(readFileSync(process.argv[2], "utf8"));
const cc = Converter({ from: "cn", to: "twp" });

// 台灣說法（OpenCC 轉不到的）：簡中原詞 → 繁中
const TW = {
  "吊带": "細肩帶", "连衣裙": "洋裝", "开衫": "開襟衫", "连帽卫衣": "連帽上衣", "连裤袜": "褲襪", "长筒袜": "過膝襪",
  "文胸": "胸罩", "马甲": "背心", "波点": "圓點", "毛线帽": "毛帽", "颈链": "頸鍊", "浅口鞋": "包鞋", "乐福鞋": "樂福鞋",
  "运动鞋": "運動鞋", "托特包": "托特包", "毛绒披肩": "毛絨披肩", "针织衫": "針織衫", "皮夹克": "皮夾克", "牛仔外套": "牛仔外套",
};
// posewear.rs 的 costume_text 裡寫死的連接詞（要跟 Rust 那邊一字不差）
const JOINERS = ["身穿", "一条", "穿", "内搭", "叠穿", "上身是", "上身穿", "上衣选", "外披", "外搭", "外面套", "外穿", "下身配",
  "下装是", "配", "搭配", "腿上是", "脚踩", "鞋子选", "再加", "作点缀", "点缀", "与", "和", "、", "拼", "裙裤", "银灰色",
  "短款", "中长", "长款", "，", "。", " + "];

const conv = (s) => (TW[s] ?? cc(s));
const phrases = new Map();
const addPhrase = (s) => { if (s && /[一-鿿]/.test(s)) phrases.set(s, conv(s)); };
for (const v of Object.values(raw.col)) addPhrase(v);
for (const v of Object.values(raw.mod)) addPhrase(v);
for (const [zh] of Object.values(raw.n)) addPhrase(zh);
for (const [zh] of Object.values(raw.solo)) addPhrase(zh);
for (const v of Object.values(raw.skirt_len)) addPhrase(v);
for (const v of Object.keys(TW)) addPhrase(v);
for (const v of JOINERS) addPhrase(v);
const tw = [...phrases].sort((a, b) => b[0].length - a[0].length);
const chars = {};
for (const [s] of tw) for (const ch of s) { const t = cc(ch); if (t !== ch) chars[ch] = t; }

// 跟 posewear.rs 的 to_tw 同一個演算法：每個位置先比最長的片語，比不到就單字查表
function toTw(s) {
  let out = "";
  for (let i = 0; i < s.length;) {
    const hit = tw.find(([a]) => s.startsWith(a, i));
    if (hit) { out += hit[1]; i += hit[0].length; continue; }
    out += chars[s[i]] ?? s[i]; i += 1;
  }
  return out;
}

const pair = (o) => Object.fromEntries(Object.entries(o).map(([k, [zh, en]]) => [k, [cc(zh), en]]));
const text = {
  source: "posewear（MIT，github.com/atuizz/posewear）的 tr_pose.txt、build.py、costume_text.py；由 tools/posewear_text.py＋posewear_text.mjs 產生，不要手改",
  tr_pose: Object.fromEntries(Object.entries(raw.tr_pose).map(([ja, [zh, en]]) => [ja, [cc(zh), en]])),
  line: pair(raw.line),
  cat: Object.fromEntries(Object.entries(raw.cat).map(([k, v]) => [k, cc(v)])),
  ang: pair(raw.ang),
  cc: pair(raw.cc),
  fix: Object.fromEntries(Object.entries(raw.fix).map(([k, v]) => [k, cc(v)])),
  // 以下是簡中原樣（程式拼完句子才轉繁）
  col: raw.col, mod: raw.mod, n: raw.n, solo: raw.solo, en_solo: raw.en_solo, skirt_len: raw.skirt_len,
  tw, tw_chars: chars,
};
mkdirSync(join(root, "resources", "posewear"), { recursive: true });
writeFileSync(join(root, "resources", "posewear", "text.json"), JSON.stringify(text, null, 1) + "\n");
const cases = raw.cases.map((c) => ({ ...c, zh: toTw(c.zh), title: toTw(c.title) }));
writeFileSync(join(root, "src-tauri", "tests", "fixtures", "posewear_costume_cases.json"), JSON.stringify(cases, null, 1) + "\n");
console.log(`ok：${Object.keys(text.tr_pose).length} 個姿勢名、${tw.length} 個片語、${cases.length} 筆服裝測試`);
for (const c of cases.slice(0, 6)) console.log(c.zh, "｜", c.title);

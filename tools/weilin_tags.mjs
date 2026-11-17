// 產生 resources\tags\weilin\：WeiLin 的分類詞庫（生成頁「詞庫」抽屜用）＋ danbooru 中文翻譯（補進中文找 tag）。2026-11-17
//
//   node tools/weilin_tags.mjs <WeiLin-Comfyui-Tools-Prompt 的 SQL 資料夾> <opencc-js 的 dist/umd/full.js>
//
// 資料來源：github.com/weilin9999/WeiLin-Comfyui-Tools-Prompt（MIT，授權全文抄進 resources\tags\weilin\LICENSE.txt）
//   <資料夾>\tags.sql   = tags/2025_03_31/tags_2025_03_31.sql（10 大類、131 小類、約 3900 條）
//   <資料夾>\nsfw.sql   = tags/2025_04_01/weilin_NSFW词库_2025_04_01_001.sql
//   <資料夾>\dan\*.sql  = danbooru/2025_04_01/danbooru_2025_04_01_001～025.sql（tag、translate、hot）
// 詞庫內容照搬，只做這些：簡體轉台灣繁體（OpenCC cn→twp，只在跑這支時用，不進程式）、
// 單一 danbooru tag 的底線換空白、括號跳脫成 Anima 要的 \( \)；整段提示詞（有逗號的）原樣不動；
// 簡轉繁一對多轉錯的字照英文 tag 改回來（hair → 髮、curl → 捲、beard → 鬍…，FIX_CHARS）；
// 拿掉 A1111 的 <lora:…>（漢服那幾條是 SD 1.5 的 LoRA，繪匣用不到，留著只會變成雜訊送進文字編碼器）。
// 角色翻譯（color_id 4）WeiLin 錯很多（看起來是 AI 編的），<資料夾> 裡有 wiki_chars.json 時一條條校對（tools\weilin_chars.mjs），
// 校對不到的不收；沒有那個檔就整批角色都不收。一般 tag 的翻譯照收。
// opencc-js 不用裝進專案：npm i opencc-js@1.0.5 到暫存資料夾，第二個參數指它的 dist/umd/full.js。
//
// 校對資料怎麼來（在 <資料夾> 底下跑；每支都可以中斷再跑）：
//   1. node tools/weilin_tags.mjs <資料夾> <full.js> --list   列出要查的角色 → <資料夾>\chars.txt
//   2. python -I tools/danbooru_wiki_fetch.py chars.txt wiki_chars.json
//   3. node tools/weilin_tags.mjs <資料夾> <full.js> --list   再跑一次：補列本尊 → chars_more.txt、作品 → copyrights.txt
//   4. python -I tools/danbooru_wiki_fetch.py chars_more.txt wiki_chars.json；… copyrights.txt wiki_copyrights.json
//   5. python -I tools/bangumi_fetch.py wiki_chars.json bangumi_chars.json
//   6. node tools/weilin_tags.mjs <資料夾> <full.js>   產生檔案，另外寫 <資料夾>\verify_report.json 給人抽查
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const [srcDir, occPath, mode] = process.argv.slice(2);
if (!srcDir || !occPath) {
  console.error("用法：node tools/weilin_tags.mjs <SQL 資料夾> <opencc-js dist/umd/full.js>");
  process.exit(1);
}
const OpenCC = createRequire(import.meta.url)(path.resolve(occPath));
const tw = OpenCC.Converter({ from: "cn", to: "twp" });
const outDir = path.join(root, "resources", "tags", "weilin");
fs.mkdirSync(outDir, { recursive: true });

// INSERT … INTO "表" ("欄",…) VALUES (值,…); → {表, 欄: 值}
function rows(file) {
  const s = fs.readFileSync(file, "utf8");
  const out = [];
  const re = /INSERT OR REPLACE INTO\s+"(\w+)"\s*\(([^)]*)\)\s*VALUES\s*\(/g;
  let m;
  while ((m = re.exec(s))) {
    const cols = m[2].split(",").map((c) => c.trim().replace(/"/g, ""));
    const vals = [];
    let i = re.lastIndex;
    while (i < s.length) {
      while (s[i] === " " || s[i] === ",") i++;
      if (s[i] === ")") break;
      if (s[i] === "'") {
        let v = "";
        i++;
        for (;;) {
          if (s[i] === "'" && s[i + 1] === "'") { v += "'"; i += 2; }
          else if (s[i] === "'") { i++; break; }
          else v += s[i++];
        }
        vals.push(v);
      } else {
        const j = s.slice(i).search(/[,)]/);
        vals.push(s.slice(i, i + j).trim());
        i += j;
      }
    }
    re.lastIndex = i;
    const r = { table: m[1] };
    cols.forEach((c, k) => (r[c] = vals[k]));
    out.push(r);
  }
  return out;
}

const esc = (t) => t.replace(/(?<!\\)([()])/g, "\\$1");
// 一個 danbooru tag（沒有逗號、沒有權重括號）：底線換空白、括號跳脫；顏文字（^_^、o_o）和整段提示詞不動
function tagText(t) {
  t = t.trim();
  if (t.includes(",") || /^\(|:\d/.test(t)) return t;
  if (t.length > 3 && /[a-z0-9)]_[a-z0-9(]/i.test(t)) t = t.replace(/_/g, " ");
  return /^[^()]+\([^()]+\)$/.test(t) ? esc(t) : t;
}

// 簡轉繁一對多轉錯的字：[錯的字, 對的字, 英文 tag 的線索]
const FIX_CHARS = [
  ["發", "髮", /hair|bang|ponytail|braid|twintail|pigtail|afro|bob[ _]?cut|hime[ _]cut|sidelock|ahoge|updo|chignon/i],
  ["卷", "捲", /curl|drill|wavy|ringlet/i],
  ["胡", "鬍", /beard|mustache|moustache|stubble|goatee|sideburn/i],
  ["須", "鬚", /beard|whisker|mustache|moustache/i],
  ["面", "麵", /noodle|ramen|udon|soba|pasta|spaghetti/i],
  ["松", "鬆", /loose|baggy/i],
  ["幹", "乾", /\bdry/i],
  ["干", "乾", /\bdry/i],
];
function fixZh(zh, tag) {
  for (const [bad, good, clue] of FIX_CHARS) if (zh.includes(bad) && clue.test(tag)) zh = zh.replaceAll(bad, good);
  return zh.replace(/(?<=[㐀-鿿])_(?=[㐀-鿿])/g, ""); // 「髮_帶」→「髮帶」
}
// <lora:…>、<lyco:…>：拿掉，前後的逗號收乾淨
const noLora = (t) => t.replace(/<(?:lora|lyco|hypernet):[^>]*>/gi, "").replace(/\s*,\s*(,\s*)+/g, ", ").replace(/^[\s,]+|[\s,]+$/g, "");

// 分類詞庫：大類 → 小類 → [插入的字, 中文說明]
const groups = [], gByUuid = new Map(), sByUuid = new Map();
let nTags = 0;
for (const f of ["tags.sql", "nsfw.sql"]) {
  for (const r of rows(path.join(srcDir, f))) {
    if (r.table === "tag_groups") {
      const g = { name: tw(r.name.replace(/（WeiLin专属）/, "")), subs: [] };
      groups.push(g);
      gByUuid.set(r.p_uuid, g);
    } else if (r.table === "tag_subgroups") {
      const s = { name: tw(r.name), tags: [] };
      gByUuid.get(r.p_uuid)?.subs.push(s);
      sByUuid.set(r.g_uuid, s);
    } else if (r.table === "tag_tags") {
      const s = sByUuid.get(r.g_uuid);
      const text = noLora(r.text || "");
      if (!s || !text) continue;
      s.tags.push([tagText(text), fixZh(tw((r.desc || "").trim()), text)]);
      nTags++;
    }
  }
}
for (const g of groups) g.subs = g.subs.filter((s) => s.tags.length);
const lib = { source: "WeiLin-Comfyui-Tools-Prompt（MIT）tags 2025-03-31、NSFW 2025-04-01", groups: groups.filter((g) => g.subs.length) };
fs.writeFileSync(path.join(outDir, "library.json"), JSON.stringify(lib));

// danbooru 翻譯：只收繪匣 zh_map.json 還沒有的 tag，依熱門度排
const zhPairs = JSON.parse(fs.readFileSync(path.join(root, "resources", "tags", "zh_map.json"), "utf8"));
const zhMap = new Map();
for (const [z, t] of zhPairs) {
  const k = t.replace(/ /g, "_").toLowerCase();
  if (!zhMap.has(k)) zhMap.set(k, []);
  zhMap.get(k).push(z);
}
const cats = new Map();
for (const line of fs.readFileSync(path.join(root, "resources", "tags", "danbooru.csv"), "utf8").split("\n")) {
  const [t, c] = line.split(",");
  if (t) cats.set(t, Number(c));
}
const readJson = (f) => (fs.existsSync(path.join(srcDir, f)) ? JSON.parse(fs.readFileSync(path.join(srcDir, f), "utf8")) : null);
const wikiChars = readJson("wiki_chars.json");
const { makeVerifier } = await import(new URL("./weilin_chars.mjs", import.meta.url));
const verify = wikiChars
  ? makeVerifier({
      wikiChars, wikiCopy: readJson("wiki_copyrights.json") || {}, cats, zhMap, OpenCC,
      // Bangumi 可以分好幾個檔平行抓（bangumi_chars.json、bangumi_chars2.json…），有結果的蓋過沒結果的
      bangumi: fs.readdirSync(srcDir).filter((f) => /^bangumi_chars.*\.json$/.test(f)).sort()
        .reduce((all, f) => { for (const [k, v] of Object.entries(readJson(f))) if (v || !(k in all)) all[k] = v; return all; }, {}),
    })
  : null;

const dan = [];
const danDir = path.join(srcDir, "dan");
for (const f of fs.readdirSync(danDir).filter((x) => x.endsWith(".sql")).sort()) dan.push(...rows(path.join(danDir, f)));
const isChar = (r) => r.color_id === "4";
const todoChars = [...new Set(dan.filter(isChar).map((r) => r.tag.trim()).filter((t) => !zhMap.has(t.toLowerCase())))];

if (mode === "--list") {
  // 要抓 wiki 的清單：角色本身；抓過之後再列本尊和作品
  const w = (f, list) => { fs.writeFileSync(path.join(srcDir, f), list.join("\n")); console.log(f, list.length); };
  w("chars.txt", todoChars);
  if (verify) {
    const more = new Set(), copy = new Set();
    for (const t of todoChars) {
      for (const b of verify.bases(t)) if (!(b in wikiChars)) more.add(b);
      const ct = verify.copyrightOf(t);
      if (ct) copy.add(ct);
    }
    w("chars_more.txt", [...more]);
    w("copyrights.txt", [...copy]);
  }
  process.exit(0);
}

const extra = [];
const seen = new Set();
const report = { kept: [], fixed: [], dropped: [] };
let nChar = 0;
for (const r of dan) {
  const tag = (r.tag || "").trim();
  const key = tag.replace(/ /g, "_").toLowerCase();
  const raw = (r.translate || "").trim();
  if (!tag || !raw || raw === tag || !/[㐀-鿿]/.test(raw) || zhMap.has(key) || seen.has(key)) continue;
  seen.add(key);
  let zh;
  if (isChar(r)) {
    nChar++;
    const v = verify ? verify(tag, raw) : { zh: null, status: "沒有校對資料" };
    const line = `${tag} | ${raw} → ${v.zh ?? "（不收）"}`;
    if (!v.zh) { report.dropped.push(`${v.status}｜${line}`); continue; }
    (v.status.endsWith("對") ? report.kept : report.fixed).push(`${v.status}｜${line}`);
    zh = v.zh;
  } else {
    zh = fixZh(tw(raw), tag);
  }
  extra.push([zh, tag, Number(r.hot) || 0]);
}
extra.sort((a, b) => b[2] - a[2]);
fs.writeFileSync(path.join(outDir, "zh_extra.json"), JSON.stringify(extra.map(([zh, tag]) => [zh, tag])));
fs.writeFileSync(path.join(srcDir, "verify_report.json"), JSON.stringify(report, null, 1));
fs.copyFileSync(path.join(srcDir, "LICENSE.txt"), path.join(outDir, "LICENSE.txt"));
console.log(`大類 ${lib.groups.length}、小類 ${lib.groups.reduce((n, g) => n + g.subs.length, 0)}、詞 ${nTags}；翻譯補 ${extra.length} 條`);
console.log(`角色 ${nChar}：對 ${report.kept.length}、換掉 ${report.fixed.length}、不收 ${report.dropped.length}`);

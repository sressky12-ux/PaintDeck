// Danbooru tag group 詞庫（2026-11-17，設計：docs\superpowers\specs\2026-11-17-wardrobe-and-danbooru-library-design.md）
// 抓 wiki「tag groups」列的每一組 tag group，照 wiki 的小標題切段，只收繪匣 tag 索引（resources\tags\danbooru.csv）
// 裡真的有、一般類、用過 MIN_COUNT 次以上的 tag，產出 resources\tags\danbooru_groups\library.json。
//
// 用法（專案根目錄）：
//   node tools/danbooru_taggroups.mjs            抓（有快取就不抓）＋產出（DB_GROUPS_MIN 改最低次數，預設 10）
//   node tools/danbooru_taggroups.mjs --todo     另外列出還沒中文的名稱與 tag → <快取>\todo.json（翻好寫進 tools\danbooru_groups_zh.json）
// 快取：F:\AI\小克工作區\測試區\danbooru_groups\（每頁一個 json，重跑不重抓；要更新就刪掉）
// 不送任何個人資料，只帶通用 User-Agent。中文：zh_map.json → weilin\zh_extra.json → tools\danbooru_groups_zh.json（後者優先）。
import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1")), "..");
const CACHE = process.env.DB_GROUPS_CACHE || "F:\\AI\\小克工作區\\測試區\\danbooru_groups";
const OUT = path.join(ROOT, "resources", "tags", "danbooru_groups", "library.json");
const ZH_FILE = path.join(ROOT, "tools", "danbooru_groups_zh.json");
const UA = "PaintDeck-taglib/1.0 (tag group index)";
const MIN_COUNT = +(process.env.DB_GROUPS_MIN || 10);   // tag group 裡的 tag 至少用過幾次
const POP_MIN = 300;                                     // 沒分到類的一般 tag：至少用過幾次才收進「其他熱門」
const POP_EN = "Other popular tags";
const SKIP_SECTION = /related|see also|external link|^notes?$|^references?$|^navigation|^table of contents/i;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const slug = (title) => title.trim().toLowerCase().replace(/\s+/g, "_");
const norm = (t) => t.trim().toLowerCase().replace(/\s+/g, "_").replace(/#.*$/, "");

async function wiki(title) {
  fs.mkdirSync(CACHE, { recursive: true });
  const file = path.join(CACHE, slug(title).replace(/[:\\/*?"<>|]/g, "_") + ".json");
  if (fs.existsSync(file)) return JSON.parse(fs.readFileSync(file, "utf8"));
  const url = `https://danbooru.donmai.us/wiki_pages/${encodeURIComponent(slug(title))}.json`;
  for (let k = 0; k < 5; k++) {
    const r = await fetch(url, { headers: { "User-Agent": UA } });
    if (r.status === 404) { fs.writeFileSync(file, "null"); return null; }
    if (r.status === 429 || r.status >= 500) { await sleep(2000 * (k + 1)); continue; }
    if (!r.ok) throw new Error(`${title}: HTTP ${r.status}`);
    const j = await r.json();
    const v = { title: j.title, body: j.body || "" };
    fs.writeFileSync(file, JSON.stringify(v));
    await sleep(400);
    return v;
  }
  throw new Error(`${title}: 一直被擋`);
}

const links = (line) => [...line.matchAll(/\[\[([^\]|]+)(?:\|[^\]]*)?\]\]/g)].map((m) => m[1].trim());
const heading = (line) => /^h([1-6])(?:#[\w-]+)?\.\s*(.+?)\s*$/i.exec(line.trim());
const stripToc = (b) => b.replace(/\[expand=Table of Contents\][\s\S]*?\[\/expand\]/i, "");

// tag 索引：tag → [類別, 次數]
function loadIndex() {
  const m = new Map();
  for (const line of fs.readFileSync(path.join(ROOT, "resources", "tags", "danbooru.csv"), "utf8").split(/\r?\n/)) {
    // 第一欄可能是加引號的（"don't_say_""lazy"""）
    const q = /^"((?:[^"]|"")*)",(\d+),(\d+)/.exec(line);
    const [t, c, n] = q ? [q[1].replace(/""/g, '"'), q[2], q[3]] : line.split(",");
    if (t) m.set(t, [+c, +n]);
  }
  return m;
}

function loadZh() {
  const zh = new Map();
  for (const [z, t] of JSON.parse(fs.readFileSync(path.join(ROOT, "resources", "tags", "zh_map.json"), "utf8"))) if (!zh.has(t)) zh.set(t, z);
  for (const [z, t] of JSON.parse(fs.readFileSync(path.join(ROOT, "resources", "tags", "weilin", "zh_extra.json"), "utf8"))) if (!zh.has(t)) zh.set(t, z);
  const mine = fs.existsSync(ZH_FILE) ? JSON.parse(fs.readFileSync(ZH_FILE, "utf8")) : { names: {}, tags: {} };
  for (const [t, z] of Object.entries(mine.tags || {})) zh.set(t, z);
  return { zh, names: mine.names || {} };
}

// 跟 weilin_tags.mjs 同一套：底線換空白、單一 tag 的括號跳脫；顏文字（^_^）不動
const esc = (t) => t.replace(/(?<!\\)([()])/g, "\\$1");
function tagText(t) {
  if (t.length > 3 && /[a-z0-9)]_[a-z0-9(]/i.test(t)) t = t.replace(/_/g, " ");
  return /^[^()]+\([^()]+\)$/.test(t) ? esc(t) : t;   // 只有「名字 (說明)」這種才跳脫；\(^o^)/ 這類顏文字原樣
}

// tag groups 索引頁：Visual characteristics 底下的 h6 是大類，清單裡的 tag group 是小類，直接連到的單一 tag 收進「其他」
function parseIndex(body) {
  const cats = [];
  let inVisual = false, cur = null;
  for (const line of stripToc(body).split(/\r?\n/)) {
    const h = heading(line);
    if (h) {
      if (h[1] === "5") { inVisual = /visual/i.test(h[2]); cur = null; continue; }
      if (h[1] === "6" && inVisual) { cur = { en: h[2], groups: [], singles: [] }; cats.push(cur); }
      continue;
    }
    if (!cur || !/^\*+/.test(line.trim())) continue;
    for (const l of links(line)) {
      if (/^tag group:/i.test(l)) { if (!cur.groups.some((g) => slug(g) === slug(l))) cur.groups.push(l); }
      else if (!/^list of/i.test(l)) cur.singles.push(l);
    }
  }
  return cats;
}

// 一組 tag group 的內文 → [{段名, [tag…]}]
function parseGroup(body) {
  const secs = [{ en: "", tags: [] }];
  let skip = false;
  for (const line of stripToc(body).split(/\r?\n/)) {
    const h = heading(line);
    if (h) {
      const name = h[2].replace(/\[\[([^\]|]+)(?:\|([^\]]*))?\]\]/g, (_, a, b) => b || a).trim();
      skip = SKIP_SECTION.test(name);
      if (!skip) secs.push({ en: name, tags: [] });
      continue;
    }
    if (skip) continue;
    for (const l of links(line)) if (!/^(tag group|list of|howto|help|about):/i.test(l)) secs[secs.length - 1].tags.push(l);
  }
  return secs.filter((s) => s.tags.length);
}

async function main() {
  const todo = process.argv.includes("--todo");
  const index = loadIndex();
  const { zh, names } = loadZh();
  const zhName = (en) => names[en] || en;
  const keep = (raw) => {
    const t = norm(raw), e = index.get(t);
    return e && e[0] === 0 && e[1] >= MIN_COUNT ? [t, e[1]] : null;
  };
  const idx = await wiki("tag groups");
  const cats = parseIndex(idx.body);
  const missingNames = new Set(), missingTags = new Map();
  const groups = [];
  let total = 0;
  for (const c of cats) {
    const subs = [];
    for (const g of c.groups) {
      const page = await wiki(g);
      if (!page) { console.warn("找不到", g); continue; }
      const en = page.title.replace(/^tag_group:/i, "").replace(/_/g, " ");
      const seen = new Set(), secNames = [], tags = [];
      for (const s of parseGroup(page.body)) {
        const rows = [];
        for (const raw of s.tags) {
          const k = keep(raw);
          if (!k || seen.has(k[0])) continue;
          seen.add(k[0]);
          rows.push(k);
        }
        if (!rows.length) continue;
        const si = secNames.length;
        secNames.push(s.en ? zhName(s.en) : "");
        if (s.en && !names[s.en]) missingNames.add(s.en);
        rows.sort((a, b) => b[1] - a[1]);
        for (const [t, n] of rows) {
          const z = zh.get(t) || "";
          if (!z) missingTags.set(t, n);
          tags.push([tagText(t), z, n, si]);
        }
      }
      if (!tags.length) continue;
      if (!names[en]) missingNames.add(en);
      subs.push({ name: zhName(en), en, secs: secNames, tags });
      total += tags.length;
    }
    const singles = [];
    for (const raw of c.singles) {
      const k = keep(raw);
      if (!k || singles.some((x) => x[0] === tagText(k[0]))) continue;
      const z = zh.get(k[0]) || "";
      if (!z) missingTags.set(k[0], k[1]);
      singles.push([tagText(k[0]), z, k[1], 0]);
    }
    if (singles.length) {
      singles.sort((a, b) => b[2] - a[2]);
      subs.push({ name: zhName("Other"), en: "Other", secs: [""], tags: singles });
      total += singles.length;
      if (!names.Other) missingNames.add("Other");
    }
    if (!subs.length) continue;
    if (!names[c.en]) missingNames.add(c.en);
    groups.push({ name: zhName(c.en), en: c.en, subs });
  }
  // tag group 沒收到、但很常用的一般 tag：另外一大類，照使用次數分段（使用者：「danbooru 的 tag 也太少了吧」）
  const inGroups = new Set(groups.flatMap((g) => g.subs.flatMap((s) => s.tags.map((t) => norm(t[0].replace(/\\/g, ""))))));
  const TIERS = [[100000, "Used 100k+ times"], [10000, "Used 10k–100k times"], [1000, "Used 1k–10k times"], [POP_MIN, `Used ${POP_MIN}–1k times`]];
  const tiers = TIERS.map(() => []);
  for (const [t, [cat, n]] of index) {
    if (cat !== 0 || n < POP_MIN || inGroups.has(t)) continue;
    tiers[TIERS.findIndex(([min]) => n >= min)].push([t, n]);
  }
  const popSubs = [];
  TIERS.forEach(([, en], i) => {
    if (!tiers[i].length) return;
    tiers[i].sort((a, b) => b[1] - a[1]);
    const tags = tiers[i].map(([t, n]) => {
      const z = zh.get(t) || "";
      if (!z) missingTags.set(t, n);
      return [tagText(t), z, n, 0];
    });
    if (!names[en]) missingNames.add(en);
    popSubs.push({ name: zhName(en), en, secs: [""], tags });
    total += tags.length;
  });
  if (popSubs.length) {
    if (!names[POP_EN]) missingNames.add(POP_EN);
    groups.push({ name: zhName(POP_EN), en: POP_EN, subs: popSubs });
  }
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify({ source: "Danbooru tag groups（danbooru.donmai.us wiki）＋繪匣 tag 索引的熱門一般 tag", min_count: MIN_COUNT, groups }));
  const uniq = new Set(groups.flatMap((g) => g.subs.flatMap((s) => s.tags.map((t) => t[0])))).size;
  console.log(`大類 ${groups.length}、小類 ${groups.reduce((a, g) => a + g.subs.length, 0)}、tag ${total}（不重複 ${uniq}）→ ${OUT}`);
  console.log(`還沒中文：名稱 ${missingNames.size}、tag ${missingTags.size}`);
  if (todo) {
    const f = path.join(CACHE, "todo.json");
    fs.writeFileSync(f, JSON.stringify({ names: [...missingNames], tags: [...missingTags].sort((a, b) => b[1] - a[1]).map(([t]) => t) }, null, 1));
    console.log("待翻 →", f);
  }
}

main().catch((e) => { console.error(e); process.exit(1); });

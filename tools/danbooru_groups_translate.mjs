// Danbooru tag group 詞庫還沒中文的名稱與 tag：直接打本機 llama-server（OpenAI 相容 /v1/chat/completions）批次「直翻」，
// 寫進 tools\danbooru_groups_zh.json（{names:{英文:中文}, tags:{tag:中文}}），翻過的不再送。之後重跑 danbooru_taggroups.mjs 就會帶上。
// 2026-11-17：先試過繪匣的「中英對照」（/api/pe/translate），它的指令是寫給提示詞的，翻短標籤會寫成一段散文，整批刪掉重來。
// 使用者：「記得設定，讓他直翻，不要讓他寫多有的沒的。」→ 自己下系統指令：只輸出 JSON 字串陣列、不准解釋；關思考、temperature 0；
// 回來的數量不對、不是陣列、或某一條太長（> 24 字，多半是在解釋）就整批重試，再不行那批跳過。
// 用法：先 `node tools/danbooru_taggroups.mjs --todo`，再
//   node tools/danbooru_groups_translate.mjs [llama-server 網址，預設 http://127.0.0.1:8199（繪匣 AI 改寫開的那顆）]
import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1")), "..");
const CACHE = process.env.DB_GROUPS_CACHE || "F:\\AI\\小克工作區\\測試區\\danbooru_groups";
const ZH_FILE = path.join(ROOT, "tools", "danbooru_groups_zh.json");
const LLM = process.argv[2] || "http://127.0.0.1:8199";
const BATCH = +(process.env.TR_BATCH || 30);
const MAX_LEN = 24;
const SYS = "你是翻譯機。把使用者給的每一行英文（動漫插畫的 Danbooru 標籤，或標籤的分類名稱）翻成繁體中文（台灣用語），簡短、直譯，"
  + "專有名詞（作品、角色、品牌、型號）沒有通用中文就保留原文。只輸出 JSON 字串陣列，順序和數量跟輸入一樣；不要解釋、不要加註、不要加任何其他文字。";

const todo = JSON.parse(fs.readFileSync(path.join(CACHE, "todo.json"), "utf8"));
const out = fs.existsSync(ZH_FILE) ? JSON.parse(fs.readFileSync(ZH_FILE, "utf8")) : { names: {}, tags: {} };
out.names ||= {};
out.tags ||= {};
const save = () => fs.writeFileSync(ZH_FILE, JSON.stringify(out, null, 1) + "\n");

// 顏文字、年份、純符號不用翻
const worth = (s) => /[a-z]{2}/i.test(s) && !/^\d{4}$/.test(s);
const shown = (t) => t.replace(/_/g, " ");

function parse(text, n) {
  const m = /\[[\s\S]*\]/.exec(text || "");
  if (!m) return null;
  let a;
  try { a = JSON.parse(m[0]); } catch { return null; }
  while (Array.isArray(a) && a.length === 1 && Array.isArray(a[0])) a = a[0];   // 有時包兩層
  if (!Array.isArray(a) || a.length !== n || !a.every((x) => typeof x === "string" && x.trim() && x.length <= MAX_LEN)) return null;
  return a.map((x) => x.trim());
}

async function translate(texts) {
  for (let k = 0; k < 2; k++) {
    const r = await fetch(`${LLM}/v1/chat/completions`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
      messages: [{ role: "system", content: SYS }, { role: "user", content: JSON.stringify(texts) }],
      temperature: 0, max_tokens: 60 * texts.length, chat_template_kwargs: { enable_thinking: false } }) });
    if (!r.ok) throw new Error(`HTTP ${r.status}：${(await r.text()).slice(0, 200)}`);
    const got = parse((await r.json()).choices?.[0]?.message?.content, texts.length);
    if (got) return got;
  }
  return null;
}

async function run(list, store, label, form) {
  const left = list.filter((x) => worth(x) && !store[x]);
  console.log(`${label}：要翻 ${left.length}`);
  let skipped = 0;
  for (let i = 0; i < left.length; i += BATCH) {
    const part = left.slice(i, i + BATCH);
    const zh = await translate(part.map(form));
    if (!zh) { skipped += part.length; console.warn(`  第 ${i + 1}～${i + part.length} 條翻不好，跳過`); continue; }
    part.forEach((x, k) => { store[x] = zh[k]; });
    save();
    console.log(`  ${Math.min(i + BATCH, left.length)}/${left.length}`);
  }
  if (skipped) console.warn(`${label}：跳過 ${skipped} 條（重跑會再試）`);
}

await run(todo.names, out.names, "名稱", (x) => x);
await run(todo.tags, out.tags, "tag", shown);
console.log("寫進", ZH_FILE);

// 畫廊搜尋框的純函式（2026-11-23，使用者：「像 google 網頁搜索，打幾個字母／單字下面就跑出多種推薦的選項」＋「中文直接搜」＋「最近搜過＋常用快捷」）。
// 不 import 任何東西：booru.js 用，tests\booru_search.test.cjs 直接測。

export const CJK = /[㐀-鿿豈-﫿]/;
export const hasCjk = (s) => CJK.test(String(s || ""));

/** 搜尋字切成一個個 tag（空白、逗號、頓號都算分隔） */
export function splitQuery(q) {
  return String(q || "").split(/[\s,，、]+/).filter(Boolean);
}

/** booru 的寫法：底線、拿掉跳脫括號 */
export const booruTag = (tag) => String(tag || "").replace(/\\([()])/g, "$1").trim().replace(/\s+/g, "_");

/** 口語、簡稱的對照：詞庫沒有，或詞庫裡同一個中文對到冷門 tag 的常見搜尋字（2026-11-23 拿一百個常用詞實測，翻歪的補在這裡）。
 *  值可以是兩個 tag（空白隔開；Danbooru 不登入一次只能 2 個 tag，不要放超過 2 個）。只放通用的詞，不放角色名（角色交給詞庫＋評分） */
export const ALIAS = {
  粉髮: "pink_hair", 紫髮: "purple_hair", 綠髮: "green_hair", 橘髮: "orange_hair", 紅髮: "red_hair", 藍髮: "blue_hair",
  棕髮: "brown_hair", 灰髮: "grey_hair", 銀髮: "grey_hair", 黑長直: "black_hair straight_hair",
  單馬尾: "ponytail", 側馬尾: "side_ponytail", 公主切: "hime_cut", 姬髮式: "hime_cut", 丸子頭: "hair_bun", 雙丸子頭: "double_bun", 齊瀏海: "blunt_bangs",
  碧眼: "blue_eyes", 金瞳: "yellow_eyes", 金眼: "yellow_eyes",
  狐耳: "fox_ears", 獸耳: "animal_ears", 狼耳: "wolf_ears",
  女僕裝: "maid", 制服: "school_uniform", 泳裝: "swimsuit", 學校泳裝: "school_swimsuit", 死庫水: "school_swimsuit", 睡衣: "pajamas",
  白絲: "white_thighhighs", 黑絲: "black_pantyhose", 絲襪: "pantyhose", 褲襪: "pantyhose", 過膝襪: "thighhighs", 吊帶襪: "garter_straps",
  連身裙: "dress", 洋裝: "dress",
  站著: "standing", 坐著: "sitting", 躺: "lying", 躺著: "lying", 趴著: "lying on_stomach",
  從背後: "from_behind", 背影: "from_behind", 側面: "from_side", 側臉: "profile", 俯視: "from_above", 仰視: "from_below",
  比讚: "thumbs_up", 剪刀手: "v", 比耶: "v",
  生氣: "angry", 眨眼: "one_eye_closed",
  海邊: "beach", 沙灘: "beach", 夜景: "night cityscape", 下雨: "rain", 床上: "on_bed", 櫻花: "cherry_blossoms",
};
/** 口語對照（含常用膠囊上的中文：點「全身」跟打「全身」搜到的一樣）；沒有回 "" */
export function aliasOf(word) {
  const w = String(word || "").trim();
  if (ALIAS[w]) return ALIAS[w];
  const c = COMMON.find(([, zh]) => zh === w);
  return c ? c[0] : "";
}

const lg = (n) => Math.log10(Math.max(0, n || 0) + 1);
const ALT = /[、,，／/|]/;
/** /api/tags 查中文的結果裡挑一個。每個候選打分：
 *    分數 = log10(張數)（越常用越高）＋ 中文完全相符 1.5／開頭相符 0.3 － 中文比打的多出幾個字 × 0.12
 *  完全相符但主對照表不是這樣翻的（r.main 對不上，只有副詞庫這樣寫），相符加分只給 0.6：
 *  副詞庫有標錯的（WeiLin 把 domineering 標成「修女服」、gohei 標成「神社」），兩邊都這樣翻才給滿分。
 *  例：「床上」→ 在床上（on_bed，11 萬張，多 1 字）贏過 床上的衣服（clothes_on_bed，157 張，開頭相符但多 3 字）。
 *  回 {tag, zh, exact, score} 或 null；exact＝中文完全相符 */
export function pickZh(token, results) {
  const t = String(token || "").trim();
  if (!t || !Array.isArray(results) || !results.length) return null;
  let best = null;
  for (const r of results) {
    const zh = String(r.zh || "");
    const alts = zh.split(ALT).map((s) => s.trim()).filter(Boolean);
    const exact = zh === t || alts.includes(t);
    const starts = !exact && alts.some((a) => a.startsWith(t));
    const near = alts.filter((a) => a.includes(t)).map((a) => a.length);
    const extra = Math.max(0, (near.length ? Math.min(...near) : zh.length) - t.length);
    // 沒有 main 欄位（舊版後端）就當有對上
    const backed = !("main" in r) || r.main === zh || String(r.main || "").split(ALT).some((a) => a.trim() && a.includes(t));
    const score = lg(r.count) + (exact ? (backed ? 1.5 : 0.6) : starts ? 0.3 : 0) - 0.12 * extra;
    if (!best || score > best.score) best = { r, exact, score };
  }
  return { tag: booruTag(best.r.tag), zh: best.r.zh || "", exact: best.exact, score: Math.round(best.score * 100) / 100 };
}

const SEG_K = 9, SEG_MIN = 2, SEG_MISS = 12, SEG_MAX = 8;
const PARTICLE = /^[的著着了在和與与跟及之個个]$/;
/** 沒空格的中文（「初音雙馬尾」「躺在床上」）切成幾段：每一種切法都算成本，挑最便宜的。
 *    一段對到 tag 的成本 ＝ 9 － 那個 tag 的分數（分數見 pickZh；口語對照表算 8 分）→ 切越少段、每段越準越便宜
 *    對不到的字一個 12（一定比對到貴）；「的、著、在」這類虛字對不到就丟掉（成本 2）
 *    一個字的段只收中文完全相符；兩個字以上要分數 ≥ 2（太冷門、差太遠的不收）
 *  lookup(text) → Promise<結果陣列>（/api/tags）。回 [{text, hit, drop?}]；對不到 hit＝null（連在一起的合成一段），drop＝丟掉的虛字 */
export async function segment(token, lookup) {
  const s = String(token || "");
  if (!s) return [];
  const cache = new Map();
  const hitOf = async (p) => {
    if (!cache.has(p)) {
      const a = aliasOf(p);
      cache.set(p, a ? { tag: a, zh: p, exact: true, score: 8 } : pickZh(p, await lookup(p)));
    }
    return cache.get(p);
  };
  const best = [{ cost: 0, segs: [] }];
  for (let i = 1; i <= s.length; i++) {
    for (let j = Math.max(0, i - SEG_MAX); j < i; j++) {
      const part = s.slice(j, i), h = await hitOf(part);
      const ok = h && (part.length === 1 ? h.exact : h.exact || h.score >= SEG_MIN);
      let seg, c;
      if (ok) { seg = { text: part, hit: h }; c = Math.max(0.5, SEG_K - h.score); }
      else if (part.length === 1) { const drop = PARTICLE.test(part); seg = { text: part, hit: null, drop }; c = drop ? 2 : SEG_MISS; }
      else continue;   // 兩個字以上對不到：由單字一個個拼起來，最後再合併
      const cost = best[j].cost + c;
      if (!best[i] || cost < best[i].cost) best[i] = { cost, segs: [...best[j].segs, seg] };
    }
  }
  const out = [];
  for (const g of best[s.length].segs) {
    const last = out.at(-1);
    if (!g.hit && !g.drop && last && !last.hit && !last.drop) last.text += g.text;
    else out.push({ ...g });
  }
  return out;
}

/** 整句中文換成 tag：英文的原樣留，中文的逐段換。回 {query, notes:[[原文, tag]], miss:[找不到的字]}。
 *  找不到的字不放進 query（booru 沒有中文 tag，放進去整個搜尋會變 0 張），只列在 miss */
export async function translateQuery(q, lookup) {
  const words = [], notes = [], miss = [];
  for (const w of splitQuery(q)) {
    const neg = /^[-~]/.test(w) && w.length > 1 ? w[0] : "";
    const body = neg ? w.slice(1) : w;
    if (!hasCjk(body)) { words.push(w); continue; }
    for (const { text, hit, drop } of await segment(body, lookup)) {
      if (drop) continue;
      if (hit) { for (const t of hit.tag.split(" ")) words.push(neg + t); notes.push([text, hit.tag]); }
      else miss.push(text);
    }
  }
  return { query: [...new Set(words)].join(" "), notes, miss };
}

/** 最近搜過：新的放最前面、同樣的只留一個、最多 max 筆 */
export function pushHistory(list, q, max = 30) {
  const s = String(q || "").trim();
  if (!s) return [...(list || [])].slice(0, max);
  return [s, ...(list || []).filter((x) => x !== s)].slice(0, max);
}

/** 下拉選單裡「搜過的」：收藏的在前；打了字就只留對得上的（整句包含、或每個詞都對得上某個 tag 的開頭） */
export function matchHistory(hist, favs, text, n = 5) {
  const t = String(text || "").trim().toLowerCase();
  const pool = [...(favs || []).map((q) => ({ q, fav: true })), ...(hist || []).filter((q) => !(favs || []).includes(q)).map((q) => ({ q, fav: false }))];
  const ok = (q) => {
    if (!t) return true;
    const ql = q.toLowerCase();
    if (ql.includes(t)) return true;
    const tags = splitQuery(ql);
    return splitQuery(t).every((w) => tags.some((x) => x.startsWith(w) || x.replace(/_/g, " ").includes(w)));
  };
  return pool.filter((x) => ok(x.q) && x.q.toLowerCase() !== t).slice(0, n);
}

/** 換掉搜尋字裡游標所在的那個詞（保留前面的 - 排除號），回 {value, caret} */
export function replaceToken(value, caret, tag) {
  const v = String(value || ""), b = Math.min(caret ?? v.length, v.length);
  const a = v.slice(0, b).search(/\S+$/);
  const start = a < 0 ? b : a;
  const end = b + (v.slice(b).match(/^\S*/)?.[0].length || 0);
  const neg = /^[-~]/.test(v.slice(start, end)) ? v[start] : "";
  const ins = neg + booruTag(tag) + " ";
  const after = v.slice(end).replace(/^\s+/, "");
  return { value: v.slice(0, start) + ins + after, caret: start + ins.length };
}

/** 常用 tag（搜尋列下面那排，點一下加進／拿掉） */
export const COMMON = [
  ["solo", "單人"], ["1girl", "1 女"], ["full_body", "全身"], ["upper_body", "上半身"], ["portrait", "大頭"],
  ["looking_at_viewer", "看鏡頭"], ["smile", "微笑"], ["standing", "站姿"], ["sitting", "坐姿"], ["from_behind", "背面"],
  ["simple_background", "簡單背景"], ["white_background", "白背景"], ["scenery", "風景"],
];

/** 常用 tag 點一下：搜尋字裡有就拿掉、沒有就加在最後 */
export function toggleTag(q, tag) {
  const words = splitQuery(q);
  return (words.includes(tag) ? words.filter((w) => w !== tag) : [...words, tag]).join(" ");
}

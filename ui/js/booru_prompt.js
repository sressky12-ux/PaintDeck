// 畫廊（booru.js）的純函式（2026-11-23）：booru 原圖的 tag → 繪匣提示詞、挑哪個網址抓圖、檔名。
// 不 import 任何東西：booru.js 用，tests\booru.test.cjs 直接測。

/** 底線不換空白的顏文字 tag（跟 tagger.rs 的 KAOMOJI 同一份） */
const KAOMOJI = new Set(["0_0", "(o)_(o)", "+_+", "+_-", "._.", "<o>_<o>", "<|>_<|>", "=_=", ">_<", "3_3", "6_9", ">_o",
  "@_@", "^_^", "o_o", "u_u", "x_x", "|_|", "||_||"]);

/** tag 名稱 → 提示詞寫法：底線換空白（顏文字除外）、括號跳脫（跟 tagger.rs 的 fmt_tag 一樣） */
export function fmtTag(tag) {
  const t = String(tag || "").trim();
  return (KAOMOJI.has(t) ? t : t.replace(/_/g, " ")).replace(/\\?\(/g, "\\(").replace(/\\?\)/g, "\\)");
}

/** 各站的分級 → Anima 的評級 tag（Danbooru 回 g／s／q／e，Gelbooru、Safebooru 回全名；AI TAG 沒有分級） */
export function ratingTag(rating) {
  const r = String(rating || "").trim().toLowerCase();
  if (["g", "general", "safe"].includes(r)) return "safe";
  if (["s", "sensitive"].includes(r)) return "sensitive";
  if (["q", "questionable"].includes(r)) return "nsfw";
  if (["e", "explicit"].includes(r)) return "explicit";
  return "";
}

/** 不放進提示詞的雜訊 tag（浮水印、簽名、網址、文字…；跟 tagclean.rs 的 NOISE 同一份） */
const NOISE = new Set(["signature", "artist name", "watermark", "web address", "dated", "twitter username", "patreon username",
  "pixiv username", "copyright notice", "artist logo", "company name", "character name", "sample watermark",
  "jpeg artifacts", "lowres", "highres", "absurdres", "commentary request",
  "english text", "japanese text", "chinese text", "korean text", "translated", "translation request", "copyright name"]);
const noise = (t) => NOISE.has(t.toLowerCase().replace(/_/g, " "));

/** 人數 tag（1girl、2boys、multiple girls、solo、no humans…）：Anima 要排在角色前面 */
const COUNT = /^(\d+\+?(girl|boy|other)s?|multiple_(girls|boys|others)|solo|solo_focus|no_humans)$/;

/** 原圖的 tag 分組（artist／copyright／character／general／meta）→ 提示詞。
 *  順序照 Anima 官方：評級 → 人數 → 角色 → 作品 → 畫師（加 @）→ 其他。meta（highres、commentary…）和浮水印、簽名這類雜訊不放。
 *  opts：{rating: 要不要放評級（預設要）, artist: 要不要放畫師（預設不要）, character, copyright, general（預設都要）} */
export function buildPrompt(tags, rating, opts = {}) {
  const o = { rating: true, artist: false, character: true, copyright: true, general: true, ...opts };
  const g = (k) => (Array.isArray(tags?.[k]) ? tags[k] : []).map((t) => String(t).trim()).filter(Boolean);
  const general = o.general ? g("general").filter((t) => !noise(t)) : [];
  const counts = general.filter((t) => COUNT.test(t));
  const rest = general.filter((t) => !COUNT.test(t));
  const out = [];
  const rt = o.rating ? ratingTag(rating) : "";
  if (rt) out.push(rt);
  out.push(...counts.map(fmtTag));
  if (o.character) out.push(...g("character").map(fmtTag));
  if (o.copyright) out.push(...g("copyright").map(fmtTag));
  if (o.artist) out.push(...g("artist").map((t) => "@" + fmtTag(t)));
  out.push(...rest.map(fmtTag));
  return [...new Set(out)].join(", ");
}

/** 能送進圖生圖／多圖參考的副檔名（動圖、影片、ugoira 的 zip 不行） */
const STILL = new Set(["jpg", "jpeg", "png", "webp", "bmp"]);
export const MAX_BYTES = 20 * 1024 * 1024;
export const MAX_PIXELS = 40e6;

function extOf(d) {
  const e = String(d?.fileExt || "").toLowerCase();
  if (e) return e;
  const m = String(d?.mediaUrl || d?.sampleUrl || "").match(/\.([a-z0-9]+)(?:\?|$)/i);
  return m ? m[1].toLowerCase() : "";
}

export function canSend(d) {
  return STILL.has(extOf(d)) && !!(d?.mediaUrl || d?.sampleUrl);
}

/** 要抓哪張：原圖不大就抓原圖；太大（超過 20MB，或不知道大小但超過 4000 萬像素）抓站方的縮小版（sample） */
export function pickMediaUrl(d) {
  if (!canSend(d)) return "";
  const big = d.fileSize > 0 ? d.fileSize > MAX_BYTES : (d.width || 0) * (d.height || 0) > MAX_PIXELS;
  if (d.mediaUrl && (!big || !d.sampleUrl)) return d.mediaUrl;
  return d.sampleUrl || d.mediaUrl;
}

/** 上傳時的檔名：booru_<站>_<編號>.<副檔名>（只留英數，免得奇怪字元進 ComfyUI 的 input） */
export function fileName(d, url) {
  const m = String(url || "").match(/\.([a-z0-9]+)(?:\?|$)/i);
  const ext = (m ? m[1] : extOf(d) || "jpg").toLowerCase();
  const safe = (s) => String(s || "").replace(/[^a-z0-9]/gi, "").slice(0, 32) || "x";
  return `booru_${safe(d?.source)}_${safe(d?.postId)}.${ext === "jpeg" ? "jpg" : ext}`;
}

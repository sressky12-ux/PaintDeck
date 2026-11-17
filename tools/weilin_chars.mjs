// WeiLin 角色翻譯校對（2026-11-17）：WeiLin 的角色中文看起來是 AI 產生的（東風谷早苗寫成河城荷取、Furina 寫成芙莉蓮、
// 孤獨搖滾的喜多郁代寫成「北一輝-東方Project」），一條條拿人工維護的資料對：
//   1. Danbooru wiki 的 other_names（各語言別名）裡的漢字別名、繪匣原本的 zh_map.json；
//   2. 沒有漢字別名的，用 wiki 的日文名去 Bangumi（bgm.tv）找「日文名完全一樣」的角色，拿它的簡體中文名、第二中文名、別名；
//   3. 自己都沒有的換裝／分身 tag（kaname_madoka_(magical_girl)、byleth_(female)_(fire_emblem)）找本尊：
//      拿掉一組括號後的 tag，或 wiki 內文連到的、而且是這個 tag 開頭的角色。
//   名字：WeiLin 的跟任一個候選對得上（簡繁日字形都折成簡體再比）→ 留；對不上 → 換成候選（優先簡體中文那個）；沒有候選 → 不收。
//   作品：tag 括號裡的作品或 wiki 內文第一個連到的作品，用那個作品的別名／繪匣對照表比；對得上留，對不上換，查不到作品只留名字。
// 資料：tools\danbooru_wiki_fetch.py 抓的 wiki_chars.json、wiki_copyrights.json，tools\bangumi_fetch.py 抓的 bangumi_chars.json。
const HAN = /[㐀-鿿豈-﫿]/;
const KANA_HANGUL = /[ぁ-ヺー-ヿㇰ-ㇿ가-힯]/;

export function makeVerifier({ wikiChars, wikiCopy = {}, bangumi = {}, cats, zhMap, OpenCC }) {
  const jp2t = OpenCC.Converter({ from: "jp", to: "t" });
  const t2s = OpenCC.Converter({ from: "t", to: "cn" });
  const s2t = OpenCC.Converter({ from: "cn", to: "t" });
  const cn2twRaw = OpenCC.Converter({ from: "cn", to: "tw" }); // 人名、作品名用逐字轉，不套詞彙（twp 會把名字裡的字換成台灣用詞）
  // 日文新字體（黒、蛍、戦、沢、桜）逐字換成繁體；本身也是正常繁體字、換了會錯的不換（郁 → 鬱、芸 → 藝…）
  const JP_KEEP = new Set([..."郁芸弁予余欠糸缶台体灯浜絵"]);
  const jpChar = (c) => (JP_KEEP.has(c) ? c : jp2t(c));
  // 人名常用、繁體也這樣寫的字不轉（喜多郁代不是喜多鬱代、余、范、沈是姓）
  const NAME_KEEP = new Set([..."郁余范沈于周里"]);
  const cn2tw = (s) => [...s].map((c) => (NAME_KEEP.has(c) ? c : jpChar(cn2twRaw(c)))).join("");
  const fold = (s) => t2s(jp2t(s)).replace(/[·・•\s\-_.]/g, "").toLowerCase();
  // 結尾的括號註記（可能好幾組：貝雷絲(女)(夏日)）全部拿掉
  const TAIL = /(\s*[（(\[［【][^（）()\[\]［］【】]*[）)\]］】])+\s*$/;
  const strip = (s) => s.replace(TAIL, "").trim();
  // 「-无-无」垃圾尾巴；「犬冢美和-犬冢美和」這種重複
  const clean = (s) => s.trim().replace(/(-无)+$/, "").replace(/^(.+?)-\1(?=-|$)/, "$1").replace(/-+$/, "");

  // 漢字候選：有漢字、沒有假名／韓文、沒有英文字母，去掉結尾的括號註記
  const hanzi = (names) =>
    [...new Set((names || []).map((s) => strip(String(s))).filter((s) => s && s.length <= 24 && HAN.test(s) && !KANA_HANGUL.test(s) && !/[A-Za-z_]/.test(s)))];
  const looksSimplified = (s) => s2t(s) !== s;
  // 挑一個：包含別的候選的長名字（「星穹铁道花火」裡有「花火」、「芙宁娜·德·枫丹」裡有「芙宁娜」）往後排，再來簡體中文優先
  const best = (cands) => {
    const plain = cands.filter((c) => !cands.some((o) => o !== c && o.length >= 2 && c.includes(o)));
    const pool = plain.length ? plain : cands;
    return pool.find(looksSimplified) || pool[0];
  };
  const same = (a, b) => {
    const x = fold(a), y = fold(b);
    if (!x || !y) return false;
    if (x === y) return true;
    const [s, l] = x.length <= y.length ? [x, y] : [y, x];
    return s.length >= 2 && l.includes(s);
  };

  const linkTags = (body) =>
    [...(body || "").matchAll(/\[\[([^\]|]+)(?:\|[^\]]*)?\]\]/g)].map((m) => m[1].trim().toLowerCase().replace(/\s+/g, "_"));
  const zhOf = (tag) => hanzi((zhMap.get(tag) || []).flatMap((z) => z.split("/")));
  // Bangumi 用短日文名（リコ、セキ、ニケ，三個字以內）找到的：同名角色太多，只拿來確認 WeiLin 的對不對，不拿來換
  const bgmShort = (tag) => fold(bangumi[tag]?.kw || "").length <= 3;
  const own = (tag) => [...hanzi(wikiChars[tag]?.other_names), ...zhOf(tag), ...(bgmShort(tag) ? [] : hanzi(bangumi[tag]?.cands))];
  const checkOnly = (tag) => (bgmShort(tag) ? hanzi(bangumi[tag]?.cands) : []);

  // 本尊候選：拿掉任一組括號（byleth_(female)_(fire_emblem) → byleth_(fire_emblem)、byleth_(female)），
  // 加上 wiki 內文連到、而且這個 tag 是以它開頭的角色
  function bases(tag) {
    const out = [];
    // 只拿掉「換裝、性別、形態」這類括號；作品括號（_(blue_archive)）不拿，不然 hibiki_(blue_archive) 會變成別部作品的響
    const groups = [...tag.matchAll(/_\(([^()]+)\)/g)].filter((g) => cats.get(g[1]) !== 3);
    for (const g of groups) out.push(tag.slice(0, g.index) + tag.slice(g.index + g[0].length));
    for (const t of linkTags(wikiChars[tag]?.body)) if (t !== tag && tag.startsWith(t)) out.push(t);
    return [...new Set(out)].filter((t) => t && t !== tag && cats.get(t) === 4);
  }

  function nameCands(tag, depth = 0) {
    const c = own(tag);
    if (c.length || depth >= 2) return { cands: c, base: null };
    for (const b of bases(tag)) {
      const r = nameCands(b, depth + 1);
      if (r.cands.length) return { cands: r.cands, base: b };
    }
    return { cands: [], base: null };
  }

  function copyrightOf(tag) {
    for (const q of [...tag.matchAll(/_\(([^()]+)\)/g)].map((m) => m[1]).reverse()) if (cats.get(q) === 3) return q;
    // original（原創角色）不算作品，不寫「（原創的）」
    return linkTags(wikiChars[tag]?.body).find((t) => cats.get(t) === 3 && t !== "original") || null;
  }

  function workName(ct, weilinWork) {
    if (!ct) return { work: null, how: "無作品" };
    const cands = [...zhOf(ct), ...hanzi(wikiCopy[ct]?.other_names)];
    if (weilinWork && cands.some((c) => same(c, weilinWork))) return { work: cn2tw(weilinWork), how: "作品對" };
    if (!cands.length) return { work: null, how: "作品查無中文" };
    return { work: zhOf(ct)[0] || cn2tw(best(cands)), how: weilinWork ? "作品換" : "作品補" };
  }

  // raw：WeiLin 原文（簡體，「名字-作品-英文 tag」或只有名字）。回 {zh: 台灣繁體或 null, status, ...}
  verify.bases = bases;
  verify.copyrightOf = copyrightOf;
  return verify;

  function verify(tag, raw) {
    raw = clean(raw);
    // 「名字-作品-英文」「名字-英文」「名字」三種寫法
    // 照「-」切：第一段是名字，後面第一段有中文的是作品（英文 tag、「无」不算）
    const parts = raw.split("-");
    const name = parts[0];
    const wWork = parts.slice(1).find((p) => HAN.test(p) && !/^无/.test(p) && p !== name) || null;
    const suffix = name.match(TAIL)?.[0] || "";
    const core = strip(name).replace(/[_\s]+$/, "");
    const { cands, base } = nameCands(tag);
    if (!cands.length && checkOnly(tag).some((c) => same(c, core))) {
      const { work } = workName(copyrightOf(tag), wWork);
      return { zh: cn2tw(core) + (work && !same(work, core) ? `（${work}）` : ""), status: "短名對", core };
    }
    // 2B、9S 這種名字本身就是英數字、跟 tag 一樣的：不用翻，原樣收
    if (!cands.length && /^[A-Za-z0-9+.\-]{1,6}$/.test(core) && tag.toLowerCase().startsWith(core.toLowerCase())) {
      const ct = copyrightOf(tag), { work } = workName(ct, wWork);
      return { zh: core + (work ? `（${work}）` : ""), status: "英數名對", core };
    }
    if (!cands.length) return { zh: null, status: tag in wikiChars ? "查無中文" : "沒抓", core };
    // 換裝版自己的 wiki 別名常常是那套衣服、那首歌的名字（kagamine_rin_(roshin_yuukai) → 爐心融解）：
    // WeiLin 的名字跟本尊對得上，就算對
    const baseHit = !base && bases(tag).some((b) => nameCands(b, 1).cands.some((c) => same(c, core)));
    let out, status;
    if (cands.some((c) => same(c, core)) || baseHit) { out = cn2tw(core); status = base || baseHit ? "本尊對" : "對"; }
    else { out = cn2tw(best(cands)); status = base ? "本尊換" : "換"; }
    // 換裝版：WeiLin 寫了「（第一套服裝）」這類註記就留著
    let sfx = (base || baseHit) && suffix ? cn2tw(suffix.replace(/\s+/g, "").replace(/[(\[［【]/g, "（").replace(/[)\]］】]/g, "）")) : "";
    if (/[A-Za-z]/.test(sfx)) sfx = "";
    // WeiLin 的換裝註記也可能是編的（kagamine_rin_(roshin_yuukai) 寫成「露神誘拐」）：
    // 只留一般的服裝／形態詞，或是 wiki 別名裡對得上的
    const generic = /服|装|裝|泳|制|便|私|形态|形態|女|男|兔|新娘|花嫁|阶段|階段|灵基|靈基|再臨|版|模式|季|冬|夏|聖誕|圣诞|正月|年/;
    if (sfx && !generic.test(sfx) && !(wikiChars[tag]?.other_names || []).some((n) => same(n, sfx.replace(/[（）]/g, "")))) sfx = "";
    const ct = copyrightOf(tag);
    const { work, how } = workName(ct, wWork);
    if (sfx && work && same(sfx, work)) sfx = "";
    const zh = out + sfx + (work && !same(work, out + sfx) ? `（${work}）` : "");
    return { zh, status, how, core, cands, base, ct };
  }
}

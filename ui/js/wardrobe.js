// 隨機頁的「隨機產生器」（2026-11-17，設計：docs\superpowers\specs\2026-11-17-wardrobe-and-danbooru-library-design.md）。
// 引擎是 ComfyUI 外掛 Comfyui-Anima-Wardrobe-V6（GPL-3.0，作者灵冻不冻；底層腳本作者飞蓬）：繪匣後端只把 /api/wardrobe/x/* 轉給
// 本機 ComfyUI 的 /anima-wardrobe-v6/*（server\api_wardrobe.rs），抽籤全是它的引擎做的——跟它的節點同參數、同種子一模一樣。
// 它的文字是簡體：顯示前轉繁體（vendor\opencc-cn2t.js）；送回去的值一律用它原本的 key／英文／簡體，不送轉過的字。
// 使用者原話：「隨機抽的效果則是真的隨機抽。參考看吧 https://github.com/lingdongbudong/Comfyui-Anima-Wardrobe-V6」
import { el, store, toast, api } from "./util.js";

const X = "/api/wardrobe/x/";
const LEVELS = [["sfw", "全年齡"], ["suggestive", "擦邊"], ["explicit", "成人"], ["hardcore", "過激"], ["extreme", "極端"], ["any", "不限"]];
const LEVEL_NUM = { sfw: 0, suggestive: 1, explicit: 2, hardcore: 3, extreme: 4, any: 99 };
const TAG_CAP = ["自动", "16", "20", "24", "28", "32", "36", "40", "48"];
const BREAST = ["不指定", "平胸", "小胸", "中等", "大胸", "巨乳", "爆乳"];
const STYLE = ["随机", "不限", "学院", "职场", "传统", "运动", "日常", "居家", "夜店", "奇幻", "泳装", "哥特", "洛丽塔", "朋克", "战术风", "暗黑少女", "辣妹"];
const WEIGHTS = [["0", "關"], ["0.5", "少"], ["1", "一般"], ["2", "多"], ["3", "必出"]];
const PAGE = 120;

const DEF = {
  level: "sfw", seed: "", tag_cap: "自动", base_prob: "", breast_size: "不指定", outfit_style: "随机",
  appearance_follow: true, hair_follow: true, duo_role_fill: true, role_pick: true, role_fav_only: false, role_works: "",
  scene_pick: false, apply: "replace", auto: false, extra_body: "",
};
const S = {
  o: { ...DEF, ...store.get("wd_opts", {}) },
  w: store.get("wd_weights", { sections: {}, fields: {}, options: {} }),
  pins: store.get("wd_pins", {}),           // 欄位 → 固定的選項（英文）
  off: new Set(store.get("wd_off", [])),     // 停用的段
  roles: store.get("wd_roles", []),          // 點選的角色 tag
  pool: store.get("wd_pool", []),            // 場景勾選池（id）
  tab: store.get("wd_tab", "fields"),
  last: null,
};
const D = { status: null, pools: null, chars: null, favs: new Set(), scenes: null, lib: null };
let ui = null, applyFn = null;

const save = () => {
  store.set("wd_opts", S.o); store.set("wd_weights", S.w); store.set("wd_pins", S.pins);
  store.set("wd_off", [...S.off]); store.set("wd_roles", S.roles); store.set("wd_pool", S.pool); store.set("wd_tab", S.tab);
};

// ---------------------------------------------------------------- 簡→繁（只給人看）
let cn2tw = null, twLoading = null;
const twCache = new Map();
async function loadTW() {
  twLoading ||= import(new URL("../vendor/opencc-cn2t.js", import.meta.url)).then(({ Converter }) => { cn2tw = Converter({ from: "cn", to: "twp" }); });
  try { await twLoading; } catch { twLoading = null; }
}
export function tw(s) {
  if (!s || !cn2tw) return s || "";
  let v = twCache.get(s);
  if (v === undefined) { v = fixHair(cn2tw(s)); twCache.set(s, v); }
  return v;
}
/** OpenCC 把頭髮的「发」轉成「發」（棕發、粉發、發帶）：顏色、髮型字旁邊的改回「髮」 */
export function fixHair(s) {
  return s.replace(/([黑白銀金棕茶紅粉藍紫綠灰橙黃青褐色短長捲卷直亂濕染假劉])發/g, "$1髮")
    .replace(/發(?=[型色帶夾飾圈箍髻梢尾量絲辮束根])/g, "髮");
}

const xget = (p) => api(X + p, { timeout: 60000 });
const xpost = (p, json) => api(X + p, { json, timeout: 120000 });

// ---------------------------------------------------------------- 純函式（tests\wardrobe.test.cjs）

/** 權重表＋固定選項 → 送給 /roll 的 weights_json。固定＝那一欄必出、其他選項權重 0、只留固定的那個 */
export function weightsWithPins(w, pins, pools) {
  const out = { sections: { ...(w.sections || {}) }, fields: { ...(w.fields || {}) }, options: {} };
  for (const [k, v] of Object.entries(w.options || {})) out.options[k] = { ...v };
  for (const [key, en] of Object.entries(pins || {})) {
    if (!en) continue;
    const f = pools?.sections?.flatMap((s) => s.fields).find((x) => x.key === key);
    if (!f) continue;
    out.fields[key] = 3;
    out.options[key] = Object.fromEntries(f.opts.map((o) => [String(o.en).toLowerCase(), o.en === en ? 3 : 0]));
  }
  return out;
}

/** 這一段在目前分級下有沒有事做：段裡還有 ≤ 分級的選項就算（照 Wardrobe 面板的 sectionEligible；
 *  不能用 max_level——「① 數量與身份」最高 4 級，用它判會在全年齡把整段藏掉）。沒有選項的純文字段恆算 */
export function sectionEligible(sec, lv) {
  let optFields = 0;
  for (const f of sec.fields || []) {
    const opts = f.opts || [];
    if (!opts.length) continue;
    optFields++;
    if (lv === 99 || opts.some((o) => (o.level ?? 0) <= lv)) return true;
  }
  return optFields === 0;
}

/** 這一抽送給 /roll 的內容（跟 Wardrobe 面板的 rollOnce 同一組欄位） */
export function rollBody(o, { weights, roles, off, pool, allSections, seedNow }) {
  const seed = String(o.seed ?? "").trim();
  const enabled = allSections.filter((id) => !off.has(id));
  return {
    section: "all",
    seed: /^\d+$/.test(seed) ? +seed : seedNow,
    nsfw_level: o.level,
    tag_cap: o.tag_cap,
    ...(o.base_prob !== "" && o.base_prob != null ? { base_prob: +o.base_prob } : {}),
    weights_json: JSON.stringify(weights),
    disabled_sections: allSections.filter((id) => !enabled.includes(id)).join(","),
    selected_roles: roles.join("|"),
    role_works: o.role_works || "",
    extra_body: o.extra_body || "",
    appearance_follow: !!o.appearance_follow,
    hair_follow: !!o.hair_follow,
    breast_size: o.breast_size,
    outfit_style: o.outfit_style,
    role_fav_only: !!o.role_fav_only,
    role_pick: !!o.role_pick,
    scene_pick: !!o.scene_pick,
    duo_role_fill: !!o.duo_role_fill,
    pick_pool_mode: pool.length ? "pose" : "",
    pick_pool_ids: pool.join(","),
  };
}

// ---------------------------------------------------------------- 抽
async function roll() {
  if (!D.pools) await loadPools();
  const body = rollBody(S.o, {
    weights: weightsWithPins(S.w, S.pins, D.pools), roles: S.roles, off: S.off, pool: S.pool,
    allSections: D.pools.sections.map((s) => s.id), seedNow: Math.floor(Math.random() * 2147483000),
  });
  const r = await xpost("roll", body);
  S.last = r;
  paintResult();
  return r;
}

function apply(r = S.last) {
  if (!r?.text) return toast("先按「全部隨機」抽一次", "err");
  applyFn?.(r.text, S.o.apply);
}

/** 按「生成」前：勾了「每次生成前重抽」就先抽一次套進提示詞（gen.js 呼叫）；失敗只提示，照樣生成 */
export async function beforeGenerate() {
  if (!S.o.auto || !applyFn) return;
  try { apply(await roll()); } catch (e) { toast("隨機產生器沒抽成（照原本的提示詞生成）：" + e.message, "err"); }
}

// ---------------------------------------------------------------- 資料
async function loadPools() {
  D.pools = await xget("pools");
}
async function loadChars() {
  if (D.chars) return;
  const [c, f] = await Promise.all([xget("characters"), xget("favorites").catch(() => ({ favorites: [] }))]);
  D.chars = c;
  D.favs = new Set(f.favorites || []);
}
async function loadScenes() { D.scenes ||= await xget("scenes"); }
async function loadLib() { D.lib = await xget("outfit-library"); }

async function refreshStatus() {
  try { D.status = await api("/api/wardrobe/status"); } catch (e) { D.status = { error: e.message }; }
  return D.status;
}

// ---------------------------------------------------------------- 畫面
const sel = (opts, val, onchange, attrs = {}) => {
  const s = el("select", { class: "sm", ...attrs }, ...opts.map(([v, t]) => el("option", { value: v }, t)));
  s.value = val;
  s.onchange = () => onchange(s.value);
  return s;
};
const chk = (label, key, title) => el("label", { class: "check inline", title: title || "" },
  el("input", { type: "checkbox", checked: !!S.o[key], onchange: (e) => { S.o[key] = e.target.checked; save(); } }), " " + label);
const set = (key) => (v) => { S.o[key] = v; save(); };

function topBar() {
  const seed = el("input", { type: "text", class: "sm wd-seed", value: S.o.seed, placeholder: "空白＝每次重抽", "aria-label": "種子", title: "填數字＝同樣的設定每次抽出一樣的結果；空白＝每次重抽" });
  seed.oninput = () => { S.o.seed = seed.value.trim(); save(); };
  const prob = el("input", { type: "number", class: "sm wd-prob", min: 0, max: 1, step: 0.05, value: S.o.base_prob, placeholder: "預設", "aria-label": "基本機率", title: "非必填欄位各自出現的基本機率（0～1）；空白＝Wardrobe 的預設" });
  prob.oninput = () => { S.o.base_prob = prob.value; save(); };
  return el("div", { class: "wd-top" },
    el("div", { class: "row wrap gap" },
      el("span", { class: "sub" }, "分級"), sel(LEVELS, S.o.level, (v) => { S.o.level = v; save(); render(); }, { "aria-label": "分級", title: "先照分級縮小選項池再抽；全年齡不會出現 NSFW 那幾段" }),
      el("span", { class: "sub" }, "種子"), seed,
      el("span", { class: "sub" }, "tag 上限"), sel(TAG_CAP.map((v) => [v, v === "自动" ? "自動" : v]), S.o.tag_cap, set("tag_cap"), { "aria-label": "tag 上限" }),
      el("span", { class: "sub" }, "基本機率"), prob),
    el("div", { class: "row wrap gap" },
      el("span", { class: "sub" }, "服裝風格"), sel(STYLE.map((v) => [v, tw(v)]), S.o.outfit_style, set("outfit_style"), { "aria-label": "服裝風格", title: "隨機＝自動配成同一種風格；不限＝各件各抽" }),
      el("span", { class: "sub" }, "胸部"), sel(BREAST.map((v) => [v, tw(v)]), S.o.breast_size, set("breast_size"), { "aria-label": "胸部大小" }),
      chk("抽場景", "scene_pick", "從場景搭配庫隨機抽一組（姿勢、地點、光影一起）"),
      chk("每次生成前重抽", "auto", "按「生成」時先抽一次、套進提示詞再畫")),
    el("div", { class: "row wrap gap" },
      el("button", { type: "button", class: "akey go", onclick: async () => { try { await roll(); } catch (e) { toast(e.message, "err"); } } }, "🎲 全部隨機"),
      sel([["replace", "取代提示詞"], ["append", "接在提示詞後面"]], S.o.apply, set("apply"), { "aria-label": "套用方式" }),
      el("button", { type: "button", class: "akey", onclick: () => apply() }, "套用到提示詞")),
    ui.result);
}

function paintResult() {
  const r = S.last;
  if (!r) { ui.result.replaceChildren(el("span", { class: "muted sm" }, "按「🎲 全部隨機」抽一組，滿意再「套用到提示詞」")); return; }
  const rep = r.report || {};
  ui.result.replaceChildren(
    el("div", { class: "muted sm" }, `${tw(r.level_label || "")}・${rep.n ?? ""} 個 tag・欄位 ${rep.ok_f ?? "?"}/${rep.total_f ?? "?"}`),
    el("textarea", { class: "wd-text", rows: 4, readonly: true, spellcheck: "false", "aria-label": "抽出來的提示詞" }, r.text),
    r.cn_summary ? el("div", { class: "wd-zh sm" }, tw(r.cn_summary)) : "",
    (r.notes || []).length ? el("details", { class: "sm" }, el("summary", {}, "抽籤說明"), el("ul", {}, ...(r.notes || []).map((n) => el("li", {}, tw(String(n)))))) : "",
  );
}

function tabs() {
  const T = [["fields", "欄位"], ["roles", "角色"], ["outfits", "服裝"], ["scenes", "場景"]];
  return el("div", { class: "tl-tabs wd-tabs" }, ...T.map(([k, name]) =>
    el("button", { type: "button", class: "tl-tab" + (S.tab === k ? " on" : ""), onclick: async () => {
      S.tab = k; save();
      try { await loadTab(); } catch (e) { toast(e.message, "err"); }
      render();
    } }, name)));
}

// 只載目前分頁要的資料（角色庫 JSON 近 1MB，用到才拿）
async function loadTab() {
  if (!D.pools) await loadPools();
  if (S.tab === "roles") await loadChars();
  if (S.tab === "outfits" && !D.lib) await loadLib();
  if (S.tab === "scenes") await loadScenes();
}

// 欄位：每段可停用、段權重；每欄權重、固定成某個選項
function fieldsTab() {
  const lv = LEVEL_NUM[S.o.level] ?? 0;
  const wsel = (scope, key) => sel(WEIGHTS.map(([v, t]) => [v, t]), String(S.w[scope][key] ?? 1), (v) => {
    if (v === "1") delete S.w[scope][key]; else S.w[scope][key] = +v;
    save();
  }, { "aria-label": "權重", title: "這一段／這一欄出現的機會：關＝不出、必出＝一定出現" });
  // 角色交給「角色」分頁、場景（n_canon）交給「場景」分頁、純文字欄沒東西可選：這幾欄不列；一欄都不剩的段整段不列
  const shown = (s) => s.fields.filter((f) => f.type !== "text" && f.key !== "role" && f.key !== "n_canon" && (f.opts || []).length);
  return D.pools.sections.filter((s) => sectionEligible(s, lv) && shown(s).length).map((s) => {
    const on = !S.off.has(s.id);
    const head = el("div", { class: "row wrap gap wd-sec-head" },
      el("label", { class: "check inline" }, el("input", { type: "checkbox", checked: on, onchange: (e) => { if (e.target.checked) S.off.delete(s.id); else S.off.add(s.id); save(); render(); } }), el("b", {}, " " + tw(s.title))),
      on ? wsel("sections", s.id) : "");
    const body = on ? el("div", { class: "wd-fields" }, ...shown(s).map((f) => {
      const opts = f.opts.filter((o) => (o.level ?? 0) <= lv || lv === 99);
      const pin = sel([["", "隨機"], ...opts.map((o) => [o.en, tw(o.zh) + (o.en ? `（${o.en}）` : "")])], S.pins[f.key] || "", (v) => {
        if (v) S.pins[f.key] = v; else delete S.pins[f.key];
        save();
      }, { "aria-label": tw(f.lab) + "：固定成", title: "選一個＝這一欄固定出這個；隨機＝照權重抽" });
      return el("div", { class: "wd-field" }, el("span", { class: "wd-lab" }, tw(f.lab).replace(/\s*\*$/, "")), wsel("fields", f.key), pin);
    })) : "";
    return el("div", { class: "wd-sec" + (on ? "" : " off") }, head, body);
  });
}

// 角色：縮圖格、搜尋、作品、收藏、點選
let roleQ = "", roleWork = "", roleFav = false, roleN = PAGE;
function rolesTab() {
  const c = D.chars;
  const q = el("input", { type: "search", class: "sm", value: roleQ, placeholder: "找角色（中文、英文、作品）", "aria-label": "找角色" });
  q.oninput = () => { roleQ = q.value; roleN = PAGE; paintRoles(); };
  const works = sel([["", `全部作品（${c.characters.length}）`], ...c.works.map((w) => [w.id, tw(w.zh || w.en) + (w.count ? `（${w.count}）` : "")])], roleWork, (v) => { roleWork = v; roleN = PAGE; paintRoles(); }, { "aria-label": "作品" });
  const fav = el("label", { class: "check inline" }, el("input", { type: "checkbox", checked: roleFav, onchange: (e) => { roleFav = e.target.checked; roleN = PAGE; paintRoles(); } }), " 只看收藏");
  const picked = el("div", { class: "wd-picked" });
  const grid = el("div", { class: "wd-grid" });
  ui.roleGrid = grid; ui.rolePicked = picked;
  setTimeout(paintRoles);
  return [
    el("div", { class: "row wrap gap" },
      el("label", { class: "check inline", title: "勾了就一個角色都不抽：純詞庫人物" }, el("input", { type: "checkbox", checked: !S.o.role_pick, onchange: (e) => { S.o.role_pick = !e.target.checked; save(); } }), " 不指定角色"),
      chk("外觀跟角色", "appearance_follow", "抽到角色時髮色、眼色、身材照角色的官方設定"),
      chk("髮型跟角色", "hair_follow", "抽到角色時髮型照角色的官方設定"),
      chk("只在收藏裡抽", "role_fav_only", "沒點選角色時，只從收藏的角色裡隨機"),
      chk("雙人自動補位", "duo_role_fill", "只點了一位、人數卻是兩人時，自動補同作品的另一位")),
    el("div", { class: "row wrap gap" }, q, works, fav,
      el("span", { class: "muted sm" }, "點卡片＝固定出這個角色（可多選）；☆＝收藏")),
    picked, grid];
}

function paintRoles() {
  const c = D.chars;
  if (!c || !ui.roleGrid) return;
  const q = roleQ.trim().toLowerCase();
  const hit = c.characters.filter((x) => (!roleWork || x.work_id === roleWork) && (!roleFav || D.favs.has(x.tag))
    && (!q || [x.name, x.tag, x.zh, tw(x.zh), x.work].some((s) => (s || "").toLowerCase().includes(q))));
  const card = (x) => {
    const on = S.roles.includes(x.tag), fav = D.favs.has(x.tag);
    return el("div", { class: "wd-card" + (on ? " on" : ""), title: `${x.trigger || x.tag}\n${(x.tags || []).join(", ")}`, onclick: () => {
      S.roles = on ? S.roles.filter((t) => t !== x.tag) : [...S.roles, x.tag];
      save(); paintRoles();
    } },
      x.thumb ? el("img", { src: `${X}thumb/${encodeURIComponent(x.thumb)}`, alt: "", loading: "lazy" }) : el("div", { class: "wd-noimg" }, "?"),
      el("b", {}, tw(x.zh) || x.name),
      el("small", {}, x.work || ""),
      el("button", { type: "button", class: "icon wd-fav" + (fav ? " on" : ""), title: fav ? "取消收藏" : "收藏", "aria-label": fav ? "取消收藏" : "收藏", onclick: async (e) => {
        e.stopPropagation();
        try { const r = await xpost("favorites/toggle", { tag: x.tag, on: !fav }); D.favs = new Set(r.favorites || []); paintRoles(); } catch (err) { toast(err.message, "err"); }
      } }, fav ? "★" : "☆"));
  };
  ui.roleGrid.replaceChildren(...hit.slice(0, roleN).map(card),
    ...(hit.length > roleN ? [el("button", { type: "button", class: "akey sm wd-more", onclick: () => { roleN += PAGE; paintRoles(); } }, `再顯示 ${Math.min(PAGE, hit.length - roleN)} 個（共 ${hit.length}）`)] : []));
  const byTag = new Map(c.characters.map((x) => [x.tag, x]));
  ui.rolePicked.replaceChildren(...(S.roles.length
    ? [el("span", { class: "sub" }, "固定："), ...S.roles.map((t) => el("span", { class: "tl-rc" }, tw(byTag.get(t)?.zh) || t,
      el("button", { type: "button", class: "icon", "aria-label": "拿掉", onclick: () => { S.roles = S.roles.filter((x) => x !== t); save(); paintRoles(); } }, "✕"))),
      el("button", { type: "button", class: "link sm", onclick: () => { S.roles = []; save(); paintRoles(); } }, "全部清掉")]
    : [el("span", { class: "muted sm" }, "沒有固定角色：照上面的設定隨機抽")]));
}

// 服裝：服裝庫（固定一套／分類池）
let outfitCat = "";
function outfitsTab() {
  const L = D.lib;
  const pool = L.pool || {};
  const cats = L.categories || [];
  const poolRow = el("div", { class: "row wrap gap" },
    el("label", { class: "check inline", title: "每次抽的時候，從勾選的分類裡隨機挑一整套衣服" }, el("input", { type: "checkbox", checked: !!pool.on, onchange: (e) => setPool({ on: e.target.checked }) }), " 從服裝庫隨機挑整套"),
    el("span", { class: "muted sm" }, `候選 ${pool.count ?? "?"} 套`),
    ...cats.map((c) => el("label", { class: "check inline" }, el("input", { type: "checkbox", checked: (pool.cats || []).includes(c.id), onchange: (e) => {
      const next = new Set(pool.cats || []);
      if (e.target.checked) next.add(c.id); else next.delete(c.id);
      setPool({ cats: [...next] });
    } }), " " + tw(c.zh))));
  const active = L.active;
  const list = (L.outfits || []).filter((x) => !outfitCat || x.cat === outfitCat);
  return [
    el("div", { class: "muted sm" }, active ? `目前固定：${tw((L.outfits || []).find((x) => x.id === active)?.zh || active)}（再點一次那套就取消）` : "點一套＝每次都穿這套（固定）；也可以用下面的分類池每次隨機挑一套"),
    poolRow,
    el("div", { class: "tl-tabs" }, el("button", { type: "button", class: "tl-tab" + (!outfitCat ? " on" : ""), onclick: () => { outfitCat = ""; render(); } }, `全部 ${L.count ?? ""}`),
      ...cats.map((c) => el("button", { type: "button", class: "tl-tab" + (outfitCat === c.id ? " on" : ""), title: tw(c.desc || ""), onclick: () => { outfitCat = c.id; render(); } }, `${tw(c.zh)} ${L.counts?.[c.id] ?? ""}`))),
    el("div", { class: "wd-grid" }, ...list.map((x) => el("div", { class: "wd-card wd-outfit" + (x.id === active ? " on" : ""), title: (x.tags || []).join(", ") + (x.note ? `\n${tw(x.note)}` : ""), onclick: async () => {
      try { D.lib = (await xpost("outfit-library/enable", { id: x.id === active ? "" : x.id })).library; render(); } catch (e) { toast(e.message, "err"); }
    } },
      x.thumb ? el("img", { src: `${X}outfit-thumb/${encodeURIComponent(x.id)}`, alt: "", loading: "lazy" }) : el("div", { class: "wd-noimg" }, `L${x.level ?? 0}`),
      el("b", {}, tw(x.zh)), el("small", {}, `L${x.level ?? 0}・${(x.tags || []).slice(0, 3).join(", ")}`)))),
  ];
}
async function setPool(p) {
  try { D.lib = (await xpost("outfit-library/pool", p)).library; render(); } catch (e) { toast(e.message, "err"); }
}

// 場景：分組、縮圖、勾選池
let sceneGroup = "";
function scenesTab() {
  const sc = D.scenes;
  const lv = LEVEL_NUM[S.o.level] ?? 0;
  const groups = sc.groups || [];
  const g = sceneGroup || groups[0]?.id || "";
  const list = (sc.scenes || []).filter((x) => x.group === g && (x.level <= lv || lv === 99));
  const inPool = new Set(S.pool);
  return [
    el("div", { class: "muted sm" }, "勾選的場景會放進「勾選池」：每次抽從池子裡挑一組（姿勢、構圖、光影一起）。池子空的時候照上面的「抽場景」決定要不要從整個庫隨機抽"),
    el("div", { class: "row wrap gap" }, el("span", { class: "sub" }, `勾選池 ${S.pool.length} 組`),
      S.pool.length ? el("button", { type: "button", class: "link sm", onclick: () => { S.pool = []; save(); render(); } }, "清空") : ""),
    el("div", { class: "tl-tabs" }, ...groups.map((x) => el("button", { type: "button", class: "tl-tab" + (x.id === g ? " on" : ""), title: tw(x.desc || ""), onclick: () => { sceneGroup = x.id; render(); } }, tw(x.title)))),
    el("div", { class: "wd-grid" }, ...list.map((x) => el("div", { class: "wd-card wd-scene" + (inPool.has(x.id) ? " on" : ""), title: (x.fields || []).map((f) => `${tw(f.lab)}：${f.en}`).join("\n") + (x.extra ? `\n${x.extra}` : ""), onclick: () => {
      S.pool = inPool.has(x.id) ? S.pool.filter((i) => i !== x.id) : [...S.pool, x.id];
      save(); render();
    } },
      x.thumb ? el("img", { src: `${X}scene-thumb/${encodeURIComponent(x.id)}`, alt: "", loading: "lazy" }) : el("div", { class: "wd-noimg" }, `L${x.level}`),
      el("b", {}, tw(x.zh)), el("small", {}, tw(x.theme || x.work_zh || x.desc || ""))))),
  ];
}

// ---------------------------------------------------------------- 安裝狀態
function statusView(st) {
  if (st?.error) return [el("div", { class: "err sm" }, st.error)];
  // 外掛有載入就能用：使用者自己用 ComfyUI Manager 裝（資料夾名不同）、或雲端那台本來就有，都不用再按安裝
  if (st.loaded) return null;
  if (st.remote) return [el("div", { class: "notice sm" }, "現在連的是雲端的 ComfyUI：隨機產生器要裝在本機的 ComfyUI。")];
  if (!st.installed) return [
    el("div", { class: "notice sm" }, "隨機產生器用的是 ComfyUI 外掛 Comfyui-Anima-Wardrobe-V6（GPL-3.0，作者灵冻不冻）：15 段 57 欄的提示詞產生器、2400 多個角色、服裝套組、場景搭配。裝在本機 ComfyUI 的 custom_nodes，不連網、不改你的工作流。"),
    el("button", { type: "button", class: "akey go", onclick: install }, "安裝隨機產生器")];
  if (!st.online) return [el("div", { class: "notice sm" }, "本機的 ComfyUI 沒有連線：開著 ComfyUI 才能抽。")];
  if (!st.loaded) return [
    el("div", { class: "notice sm" }, "裝好了，ComfyUI 重開一次才會載入。"),
    el("button", { type: "button", class: "akey go", onclick: restartComfy }, "重開 ComfyUI")];
  return null;
}
async function install() {
  toast("安裝中…（從 GitHub 抓，約幾十秒）");
  try { const r = await api("/api/wardrobe/install", { json: {}, timeout: 300000 }); toast(r.restart ? "裝好了：重開 ComfyUI 就能用" : "已經裝好了", "ok"); } catch (e) { toast("安裝失敗：" + e.message, "err"); }
  await show();
}
async function restartComfy() {
  try { const deck = await import("./deck.js"); await deck.restartComfy(); } catch (e) { toast(e.message, "err"); }
  setTimeout(show, 8000);
}

// ---------------------------------------------------------------- 進入點
function render() {
  if (!ui) return;
  const st = D.status;
  const blocked = st && statusView(st);
  if (blocked) { ui.box.replaceChildren(el("b", {}, "隨機產生器"), ...blocked); return; }
  let body;
  try {
    body = S.tab === "roles" ? rolesTab() : S.tab === "outfits" ? outfitsTab() : S.tab === "scenes" ? scenesTab() : fieldsTab();
  } catch (e) { body = [el("div", { class: "err sm" }, e.message)]; }
  ui.box.replaceChildren(topBar(), tabs(), el("div", { class: "wd-body" }, ...[body].flat()),
    el("div", { class: "muted sm tl-src" }, "引擎：Comfyui-Anima-Wardrobe-V6（GPL-3.0，灵冻不冻；底層腳本 飞蓬）"));
  paintResult();
}

export async function show() {
  if (!ui) return;
  ui.box.replaceChildren(el("span", { class: "muted sm" }, "讀取隨機產生器…"));
  await loadTW();
  const st = await refreshStatus();
  if (st?.loaded) {
    D.lib = null;   // 服裝庫的「固定／池」是外掛那邊的全域狀態：每次進來重拿
    try { await loadTab(); } catch (e) { D.status = { error: e.message }; }
  }
  render();
}

/** box：放面板的空 div；onApply(text, mode)：把抽出來的提示詞套進提示詞框（mode＝replace／append） */
export function init({ box, onApply }) {
  applyFn = onApply;
  ui = { box, result: el("div", { class: "wd-result" }) };
}

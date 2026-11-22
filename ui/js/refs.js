// 多圖參考（Qwen 2.1）：參考圖卡片（編號、角色、拖曳排序）、插入 <imageN> 標記、範本與依角色自動寫提示詞。
// 提示詞寫法照 Qwen 官方改圖規則：兩張以上用 <image1>、<image2>… 指名，並講清楚每張圖的角色；第 1 張預設是底圖。
import { $, el, api, toast, uploadFile } from "./util.js";
import { pickFromGallery } from "./gallery.js";

export const MAX = 10;
export const ROLES = {
  base: "底圖（保留構圖）", person: "人物（臉、髮型）", outfit: "服裝", scene: "場景／背景",
  pose: "姿勢", style: "畫風", object: "物件",
};
// 範本＝依序給參考圖設角色，再自動寫提示詞。
// 實測（Noct Q）：「重新組一張圖」（人物當來源、場景用文字或場景圖）效果最好；「鎖住底圖只改局部」（底圖＋換衣服）常常只改一半、質感變脆，
// 所以換裝／照姿勢都用人物＋素材重新組圖，只有套畫風、加物件用底圖。
const TEMPLATES = [
  ["人＋衣服＋場景", ["person", "outfit", "scene"], "第 1 張放人物，第 2 張放衣服，第 3 張放場景（實測效果最好）"],
  ["換裝", ["person", "outfit"], "第 1 張放人物，第 2 張放衣服；場景在提示詞後面用文字描述"],
  ["換場景", ["person", "scene"], "第 1 張放人物，第 2 張放場景"],
  ["照姿勢", ["person", "pose"], "第 1 張放人物，第 2 張放想要的姿勢"],
  ["合照", ["person", "person", "person", "person"], "每張放一個人，最多 10 張；建議取消「跟第 1 張一樣比例」改橫圖"],
  ["套畫風", ["base", "style"], "第 1 張是要重畫的圖，第 2 張是畫風來源"],
  ["加物件", ["base", "object"], "第 1 張是底圖，第 2 張是要加進去的東西（局部修改，效果較不穩）"],
];

let refs = [], ctx = null, dragFrom = -1;
export function maxRefs(family = ctx?.family?.()) { return family === "mmimage" ? 16 : MAX; }
const tag = (i) => ctx?.family?.() === "mmimage" ? `<Picture ${i + 1}>` : `<image${i + 1}>`;

export function init(c) {
  ctx = c; // { prompt: textarea, save(), onChange() }
  $("#refPick").onclick = pickMany;
  $("#refFromGallery").onclick = () => pickFromGallery(async (rec) => add(await api("/api/to_input", { json: { id: rec.id } })));
  $("#refCompose").onclick = () => compose(true);
  const tpl = $("#refTemplates");
  for (const [name, roles, hint] of TEMPLATES) {
    tpl.append(el("button", { class: "chip", title: hint, onclick: () => applyTemplate(roles, name) }, name));
  }
  render();
}

export const get = () => refs;
export function set(list) { refs = [...(list || [])]; render(); }

export async function addFiles(files) {
  const imgs = [...files].filter((f) => f.type.startsWith("image/"));
  for (const f of imgs) {
    if (refs.length >= maxRefs()) { toast(`參考圖最多 ${maxRefs()} 張`, "err"); break; }  // 已經加進去的照樣要畫出來、存起來
    try { add(await uploadFile(f), false); } catch (e) { toast(e.message, "err"); }
  }
  render(); ctx.save();
}

/** role 不給＝第一張底圖、其他人物；衣姿抽屜（posewear.js）會給 "outfit"／"pose" */
export function add(r, rerender = true, role = null) {
  if (refs.length >= maxRefs()) { toast(`參考圖最多 ${maxRefs()} 張`, "err"); return null; }
  refs.push({ name: r.name, width: r.width, height: r.height, role: role && ROLES[role] ? role : refs.length ? "person" : "base" });
  if (rerender) { render(); ctx.save(); }
  return refs.length;
}

function pickMany() {
  const inp = el("input", { type: "file", accept: "image/*", multiple: true });
  inp.onchange = () => addFiles(inp.files);
  inp.click();
}

// 把 <imageN> 的編號照 map（舊索引 → 新索引，null＝被刪掉）改掉
function remapPrompt(map) {
  const t = ctx.prompt;
  let lost = false;
  t.value = t.value.replace(/<(?:image|Picture )(\d+)>/g, (m, n) => {
    const to = map[+n - 1];
    if (to === undefined) return m;
    if (to === null) { lost = true; return "<image?>"; }
    return tag(to);
  });
  if (lost) toast("提示詞裡提到被移除的那張圖（已標成 <image?>），記得改一下", "err");
}

function move(from, to) {
  if (from === to) return;
  const order = refs.map((_, i) => i);
  const [it] = refs.splice(from, 1); refs.splice(to, 0, it);
  const [o] = order.splice(from, 1); order.splice(to, 0, o);
  const map = []; order.forEach((old, now) => (map[old] = now));
  remapPrompt(map);
  render(); ctx.save();
  if (refs.some((r, k) => k > 0 && r.role === "base")) toast("底圖不在第 1 張：輸出尺寸會照第 1 張。要換底圖，在那張的角色選「底圖」就會自動移到第 1 張");
}

function remove(i) {
  refs.splice(i, 1);
  const map = []; for (let k = 0; k <= refs.length; k++) map[k] = k < i ? k : k === i ? null : k - 1;
  remapPrompt(map);
  render(); ctx.save();
}

// 底圖只能一張，而且一定是第 1 張：輸出尺寸照第 1 張（編碼器給的 latent 就是它的大小），兩者不一致構圖會跑掉
function setRole(i, role) {
  if (role === "base") {
    refs.forEach((r, k) => { if (k !== i && r.role === "base") r.role = "person"; });
    refs[i].role = "base";
    if (i > 0) { move(i, 0); toast("底圖已移到第 1 張（輸出的構圖和尺寸照底圖）", "ok"); return; }
  } else {
    refs[i].role = role;
  }
  render(); ctx.save();
}

function insertTag(i) {
  const t = ctx.prompt, s = t.selectionStart ?? t.value.length, e = t.selectionEnd ?? s;
  const ins = (s > 0 && !/\s$/.test(t.value.slice(0, s)) ? " " : "") + tag(i) + " ";
  t.value = t.value.slice(0, s) + ins + t.value.slice(e);
  t.focus(); t.selectionStart = t.selectionEnd = s + ins.length;
  ctx.save();
}

export function render() {
  const tray = $("#refTray");
  tray.innerHTML = "";
  refs.forEach((r, i) => {
    const card = el("div", { class: "ref-card" + (i === 0 && r.role === "base" ? " first" : ""), draggable: "true",
      ondragstart: (e) => { dragFrom = i; e.dataTransfer.effectAllowed = "move"; card.classList.add("dragging"); },
      ondragend: () => { dragFrom = -1; card.classList.remove("dragging"); },
      ondragover: (e) => { if (dragFrom >= 0) { e.preventDefault(); card.classList.add("over"); } },
      ondragleave: () => card.classList.remove("over"),
      ondrop: (e) => { if (dragFrom >= 0) { e.preventDefault(); e.stopPropagation(); const f = dragFrom; dragFrom = -1; move(f, i); } } },
      el("img", { src: `/input/${encodeURIComponent(r.name)}`, alt: "", draggable: "false" }),
      el("button", { class: "ref-num", title: `把 ${tag(i)} 插進提示詞（游標的位置）`, onclick: () => insertTag(i) }, tag(i)),
      el("button", { class: "icon ref-del", title: "移除這張", onclick: () => remove(i) }, "✕"),
      el("select", { class: "ref-role", title: "這張圖要拿來做什麼（自動寫提示詞時用）",
        onchange: (e) => setRole(i, e.target.value) },
        ...Object.entries(ROLES).map(([k, v]) => el("option", { value: k, selected: r.role === k }, v))));
    tray.append(card);
  });
  if (refs.length < maxRefs()) {
    const zone = el("div", { class: "ref-add", title: "拖曳圖片到這裡（可一次多張），或點一下選檔案", onclick: pickMany },
      el("div", {}, "＋"), el("div", { class: "sm" }, refs.length ? "再加一張" : "拖曳或點此加入參考圖（可多選）"));
    zone.addEventListener("dragover", (e) => { if (e.dataTransfer.types.includes("Files")) { e.preventDefault(); zone.classList.add("over"); } });
    zone.addEventListener("dragleave", () => zone.classList.remove("over"));
    zone.addEventListener("drop", (e) => { e.preventDefault(); zone.classList.remove("over"); addFiles(e.dataTransfer.files); });
    tray.append(zone);
  }
  $("#refCount").textContent = `${refs.length} / ${maxRefs()}` + (refs.length > 4 ? "・張數多會比較慢" : "");
  ctx?.onChange?.();
}

function applyTemplate(roles, name) {
  if (!refs.length) return toast(`先加入參考圖：${TEMPLATES.find((t) => t[0] === name)[2]}`, "err");
  // 範本只決定前幾張的角色；多出來的照原本設定（合照則全部當人物）
  refs.forEach((r, i) => { if (i < roles.length) r.role = roles[i]; else if (name === "合照") r.role = "person"; else if (r.role === "base") r.role = "person"; });
  render();
  compose(false, name === "合照");
  toast(`已套用「${name}」：角色已設好、提示詞已寫入，可以再補充細節（例如光線、表情、畫面風格）`, "ok");
}

// 依角色寫出指令（中文；Qwen 2.1 的編碼器看得懂中文，<imageN> 標記照官方規則保留）
export function compose(ask = false, group = false) {
  if (!refs.length) return toast("先加入參考圖", "err");
  const t = ctx.prompt;
  if (ask && t.value.trim() && !confirm("用自動寫的提示詞取代目前的內容？")) return;
  const n = refs.length;
  const T = (i) => (n === 1 ? "圖中" : tag(i));
  const base = refs.findIndex((r) => r.role === "base");
  const lines = [];
  const people = refs.map((r, i) => (r.role === "person" ? i : -1)).filter((i) => i >= 0);
  const hasOutfit = refs.some((r) => r.role === "outfit"), hasScene = refs.some((r) => r.role === "scene");
  if (base < 0 && (group || people.length >= 2)) {
    lines.push(`${people.map(T).join("、")} 的人物一起出現在同一張照片裡，每個人都保留自己的臉、髮型和服裝。`);
  } else if (base >= 0) {
    lines.push(`以 ${T(base)} 為底圖，保持它的構圖、鏡頭角度和下面沒有提到的部分不變。`);
  }
  refs.forEach((r, i) => {
    if (i === base) return;
    const s = T(i);
    switch (r.role) {
      case "person":
        if (base >= 0) lines.push(`把 ${T(base)} 裡的人物換成 ${s} 的人物，保留這個人的臉、髮型和身材。`);
        else if (!(group || people.length >= 2)) lines.push(`畫面主角是 ${s} 的人物，保留這個人的臉、髮型和身材${hasOutfit ? "" : "和服裝"}。`);
        break;
      // 官方改圖規則：要改的地方改到底（改太輕是主要失敗之一），所以明講原本的衣服全部換掉
      case "outfit": lines.push(`讓人物完全換穿 ${s} 的服裝，原本的衣服全部換掉，布料、顏色和款式照 ${s}。`); break;
      case "scene": lines.push(base >= 0 ? `把背景換成 ${s} 的場景，光線和透視配合這個場景。` : `場景使用 ${s} 的環境，光線和透視配合這個場景。`); break;
      case "pose": lines.push(`人物擺出和 ${s} 中人物完全相同的姿勢與動作。`); break;
      case "style": lines.push(`整張圖改用 ${s} 的畫風，包括線條、上色和光影。`); break;
      case "object": lines.push(`把 ${s} 裡的物件自然地加進畫面中。`); break;
      case "base": lines.push(`${s} 也是重要參考，保留它的主要內容。`); break;
    }
  });
  t.value = lines.join("");
  t.focus(); t.selectionStart = t.selectionEnd = t.value.length;
  ctx.save();
  // 沒有底圖也沒有場景圖：場景要用文字補，不然模型會把第 1 張的背景整個照搬（換裝就會只改一半）
  if (base < 0 && !hasScene) toast("接著在提示詞最後用一句話描述場景、姿勢和光線，例如：她站在咖啡廳窗邊，柔和的自然光，全身照。", "ok");
}

// 送出前檢查：張數、提示詞裡的標記
export function check(prompt) {
  if (!refs.length) return "請先加入至少一張參考圖";
  if (refs.length > maxRefs()) return `目前有 ${refs.length} 張參考圖，這個家族最多 ${maxRefs()} 張，請先移除多出的圖。`;
  const used = [...prompt.matchAll(/<(?:image|Picture )(\d+)>/g)].map((m) => +m[1]);
  const bad = used.filter((k) => k < 1 || k > refs.length);
  if (bad.length) return `提示詞裡的 ${tag(bad[0] - 1)} 沒有對應的參考圖（目前只有 ${refs.length} 張）`;
  if (prompt.includes("<image?>")) return "提示詞裡還有 <image?>（被移除的圖），請先改掉";
  if (refs.length >= 2 && !used.length) toast(`提示詞裡沒有用 ${tag(0)} 這類標記指名圖片（點圖上的編號可以插入）`);
  return null;
}

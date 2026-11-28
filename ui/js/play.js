// 生圖玩法（2026-10-20 加）：X/Y 對照表、塗鴉畫布、差分（表情／動作）、修臉／修手（SAM3）、一鍵去背（BiRefNet）、變體，
// 以及觀景窗／燈箱的「玩法…」選單。浮層都由這支程式自己建（不寫在 index.html），樣式在 css\play.css。
// 後端：/api/generate 的 mode outpaint／diff／rembg／detail（graphs.rs）、/api/detail、/api/play/save_grid、/api/play/models（server\api_play.rs）
import { $, $$, api, el, toast, uploadFile, closeModal, store } from "./util.js";
import { setIc } from "./deck.js";
import * as gen from "./gen.js";
import { app } from "./state.js";

/** 差分用的改圖模型（跟一鍵脫衣、四視圖同一顆）；生成頁選的是 Qwen 2.1 時改用生成頁的 */
const QWEN_EDIT = "qwen_image_2.1_turbo_int8_convrot.safetensors";
const famOf = (m) => app.state?.models?.unet_family?.[m] || "anima";
const curModel = () => $("#model").value;
/** 這些欄位只跟原本那種模式有關，拿原圖參數重畫時要拿掉 */
const DROP = ["refs", "mask", "inpaint", "upscale", "control", "strip", "sheet", "diff", "detail", "outpaint", "xy", "pose_control",
  "video_preview", "poster", "idea", "pe_prompt", "prompt_resolved", "director", "agent", "comic", "hires"];
/** 原圖是這些模式時，記的提示詞是給 Qwen 的改圖指令或根本沒有，不能拿來重畫 */
const NO_REDRAW = ["rembg", "xy", "video", "strip", "sheet", "diff", "upscale"];

// ---------------------------------------------------------------- 共用
async function fullParams(rec) {
  const p = (await api(`/api/image/${rec.id}`)).params || {};
  const q = p.model ? { ...p } : { ...gen.baseParams(), ...p };  // ComfyUI 匯入的圖沒有模型：用生成頁目前的
  if (q.prompt_resolved) q.prompt = q.prompt_resolved;  // {紅|藍} 已經抽好的那句
  return q;
}
const toInput = (rec) => api("/api/to_input", { json: { id: rec.id } });
function track(jobs) {
  for (const j of jobs) gen.S.pending.set(j.job, { seed: j.seed });
  gen.renderStrip();
}

function dialog(id, title, cls = "") {
  const close = el("button", { class: "minikey red", "data-close": "", type: "button", "aria-label": "關閉" });
  setIc(close, "power");
  const body = el("div", { class: "play-body" });
  const m = el("div", { class: "modal", id, hidden: true },
    el("div", { class: `dialog play-dlg ${cls}` }, el("div", { class: "row between" }, el("b", {}, title), close), body));
  document.body.append(m);
  return { m, body, open: () => (m.hidden = false), close: () => (m.hidden = true) };
}

// ---------------------------------------------------------------- 「玩法…」選單（觀景窗、燈箱）
let menuEl = null;
function closeMenu() { menuEl?.remove(); menuEl = null; }
document.addEventListener("pointerdown", (e) => { if (menuEl && !menuEl.contains(e.target)) closeMenu(); }, true);
document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeMenu(); });

export function menu(rec, btn) {
  closeMenu();
  const item = (label, tip, fn) => el("button", { type: "button", title: tip, onclick: () => { closeMenu(); fn(); } }, label);
  const leaveLb = (fn) => () => { closeModal("#lightbox"); fn(); };
  menuEl = el("div", { class: "play-menu", role: "menu" },
    item("擴圖", "把畫面往上下左右延伸出去（生成頁的「擴圖」模式）", leaveLb(() => gen.imageAction("outpaint", rec))),
    item("變體（4 張）", "同樣的設定、低重繪，畫 4 張相似但不一樣的", () => vary(rec)),
    item("修臉", "自動找出每張臉，放大重畫細節再貼回去", () => detail(rec, "face")),
    item("修手", "自動找出每隻手，放大重畫再貼回去", () => detail(rec, "hand")),
    item("修臉＋修手", "臉和手一起修", () => detail(rec, "both")),
    item("差分（表情／動作）", "同一個角色一次改出好幾種表情或動作（Qwen 2.1／MiniMax H3 Image）", () => openDiff(rec)),
    item("一鍵去背", "去掉背景，存成透明背景的 PNG", () => rembg(rec)));
  document.body.append(menuEl);
  const r = (btn || document.body).getBoundingClientRect();
  const mw = menuEl.offsetWidth, mh = menuEl.offsetHeight;
  menuEl.style.left = Math.max(8, Math.min(innerWidth - mw - 8, r.left)) + "px";
  menuEl.style.top = (r.bottom + mh + 8 < innerHeight ? r.bottom + 4 : Math.max(8, r.top - mh - 4)) + "px";
  menuEl.querySelector("button")?.focus();
}

// ---------------------------------------------------------------- 變體
export async function vary(rec) {
  try {
    const p = await fullParams(rec);
    if (NO_REDRAW.includes(p.mode)) return toast("這種圖沒辦法直接做變體：先按「圖生圖」自己調", "err");
    for (const k of DROP) delete p[k];
    const f = famOf(p.model);
    const r = await toInput(rec);
    if (f === "mmimage") {
      Object.assign(p, { mode: "img2img", init_image: r.name, size_mode: "auto", denoise: 1.0, mm_pe: "", mm_max_pixels: 1280 * 1280, seed: null });
    } else {
      Object.assign(p, { mode: "img2img", init_image: r.name, size_mode: "auto", pixels: f === "anima" ? 1024 * 1536 : 1024 * 1024,
        denoise: f === "qwen21" ? 0.55 : f === "moss" ? 0.5 : 0.45, seed: null });
    }
    const res = await api("/api/generate", { json: { params: p, count: 4 } });
    track(res.jobs);
    toast(f === "mmimage" ? "已送出 4 張變體（MiniMax 參考改圖、換種子）" : `已送出 4 張變體（重繪 ${p.denoise}、換種子）`, "ok");
  } catch (e) { toast("變體失敗：" + e.message, "err"); }
}

// ---------------------------------------------------------------- 修臉／修手（SAM3 找位置 → 每處裁下來放大重畫 → 貼回）
const WHAT = { face: "臉", hand: "手", both: "臉和手" };
export async function detail(rec, what = "face", quiet = false) {
  try {
    const p = await fullParams(rec);
    if (["rembg", "xy", "video"].includes(p.mode)) return toast("這張圖不能修細節", "err");
    const r = await toInput(rec);
    p.init_image = r.name;
    if (!quiet) toast(`正在找${WHAT[what]}…（第一次要載入偵測模型，多等十幾秒）`);
    const denoise = +($("#detailDenoise")?.value || 0.4);
    const res = await api("/api/detail", { json: { params: p, what, denoise } });
    track(res.jobs);
    toast(`找到 ${res.found} 處${WHAT[what]}，已排進佇列重畫（重繪 ${denoise}）`, "ok");
  } catch (e) {
    toast((quiet ? "自動修臉：" : "") + e.message, quiet ? "" : "err");
  }
}

// ---------------------------------------------------------------- 一鍵去背
export async function rembg(rec) {
  try {
    const m = await api("/api/play/models");
    if (!m.bg) return toast(`缺少去背模型 ${m.bg_name}：到「設定 → 安裝與修復」再按一次安裝補下載（基本版就有），或自己放進 ComfyUI\\models\\background_removal`, "err");
    const r = await toInput(rec);
    const res = await api("/api/generate", { json: { count: 1, params: { mode: "rembg", model: curModel(), init_image: r.name, prompt: "", negative: "", seed: 0 } } });
    track(res.jobs);
    toast("去背中：完成後是透明背景的 PNG（圖庫縮圖用棋盤格表示透明）", "ok");
  } catch (e) { toast("去背失敗：" + e.message, "err"); }
}

// ---------------------------------------------------------------- 差分（表情／動作）
// 給 Qwen 的修改要求照四視圖的慣例寫簡體（graphs.rs 的 DIFF_KEEP 也是簡體）
const DIFF = {
  表情: [
    ["微笑", "把角色的表情改成温柔的微笑，嘴角微微上扬"],
    ["大笑", "把角色的表情改成开心的大笑，张开嘴，眼睛弯起来"],
    ["生氣", "把角色的表情改成生气，眉头紧皱，瞪着眼，嘴角向下"],
    ["哭泣", "把角色的表情改成哭泣，眼眶泛红含着泪，泪水沿着脸颊流下"],
    ["害羞", "把角色的表情改成害羞，脸颊泛红，视线微微移开"],
    ["驚訝", "把角色的表情改成惊讶，睁大眼睛，嘴巴张成圆形"],
    ["得意", "把角色的表情改成得意的坏笑，一边嘴角上扬"],
    ["無奈", "把角色的表情改成无奈，半闭着眼，嘴角下垂"],
    ["眨眼", "让角色俏皮地闭上一只眼睛眨眼，另一只眼睛睁开，带着微笑"],
    ["睡著", "让角色闭上双眼睡着，表情放松"],
  ],
  動作: [
    ["揮手", "让角色举起一只手挥手打招呼"],
    ["比讚", "让角色一只手竖起大拇指"],
    ["比 V", "让角色一只手在脸旁比出V字手势"],
    ["比愛心", "让角色用双手在胸前比出爱心手势"],
    ["雙手抱胸", "让角色双手交叉抱在胸前"],
    ["托腮", "让角色用一只手托着脸颊"],
    ["回頭看", "让角色转身回头看向镜头"],
    ["舉手", "让角色把一只手高高举起"],
  ],
};
let diffDlg = null, diffSrc = null;

function buildDiff() {
  const d = dialog("diffModal", "差分：同一個角色改表情／動作");
  const chips = Object.entries(DIFF).map(([group, list]) => el("div", { class: "play-group" },
    el("span", { class: "sub" }, group),
    el("div", { class: "row gap wrap" }, ...list.map(([label, req]) =>
      el("button", { type: "button", class: "chip", "data-req": req, onclick: (e) => { e.currentTarget.classList.toggle("on"); count(); } }, label)))));
  const custom = el("textarea", { id: "diffCustom", rows: 3, placeholder: "自訂：一行一個，例如「把头发改成双马尾」「让角色戴上眼镜」（中文、英文都可以）", oninput: () => count() });
  const eng = el("select", { id: "diffEngine", class: "sm", title: "差分改圖引擎（與其他編輯功能共用預設）", onchange: () => store.set("editEngine", eng.value) },
    el("option", { value: "qwen21" }, "Qwen 2.1"),
    el("option", { value: "mmimage" }, "MiniMax H3 Image"));
  const n = el("select", { id: "diffN", class: "sm", onchange: () => count() }, ...[1, 2, 3].map((k) => el("option", { value: k }, `每項 ${k} 張`)));
  const go = el("button", { type: "button", class: "akey go", id: "diffGo", onclick: sendDiff }, "送出");
  const info = el("span", { class: "muted sm", id: "diffInfo" });
  d.body.append(
    el("div", { class: "row gap" }, el("img", { id: "diffImg", class: "play-src", alt: "原圖" }),
      el("div", { class: "muted sm" }, "選好要的表情或動作，每一項各畫一張。臉、髮型、衣服、畫風盡量跟原圖一樣，只改選的那件事。",
        el("br"), "可選 Qwen 2.1 或 MiniMax H3 Image 改圖。")),
    ...chips, el("span", { class: "sub" }, "自訂"), custom,
    el("div", { class: "row gap wrap" }, el("span", { class: "sub inline" }, "改圖引擎"), eng, n, info, el("span", { class: "grow" }), go));
  const count = () => {
    const k = reqs().length * +n.value;
    info.textContent = k ? `共 ${k} 張` : "還沒選";
    go.disabled = !k;
  };
  d.count = count;
  return d;
}

const reqs = () => [...$$("#diffModal .chip.on").map((b) => b.dataset.req),
  ...$("#diffCustom").value.split("\n").map((s) => s.trim()).filter(Boolean)];

/** rec：圖庫的一張圖；或直接給 input 裡的圖 {name, width, height} */
export async function openDiff(rec) {
  try {
    const fams = Object.values(app.state?.models?.unet_family || {});
    const hasQwen = !!app.state?.models?.unet_family?.[QWEN_EDIT] || fams.includes("qwen21");
    const hasMm = fams.includes("mmimage");
    if (!hasQwen && !hasMm)
      return toast(`差分需要 Qwen 2.1（${QWEN_EDIT}）或 MiniMax H3 Image，模型清單裡都找不到`, "err");
    diffSrc = rec.name ? rec : await toInput(rec);
    diffDlg ||= buildDiff();
    const eng = $("#diffEngine");
    const saved = store.get("editEngine", "qwen21");
    eng.value = saved === "mmimage" && hasMm ? "mmimage" : hasQwen ? "qwen21" : "mmimage";
    $("#diffImg").src = `/input/${encodeURIComponent(diffSrc.name)}`;
    diffDlg.count();
    diffDlg.open();
  } catch (e) { toast(e.message, "err"); }
}

async function sendDiff() {
  const list = reqs(), n = +$("#diffN").value;
  if (!list.length) return;
  const engine = $("#diffEngine")?.value || "qwen21";
  const same = famOf(curModel()) === engine;
  // 生成頁同家族：連同步數、CFG、LoRA 照生成頁；不然只帶模型，後端自動挑該引擎的模型
  const base = same ? gen.baseParams() : { model: curModel() };
  $("#diffGo").disabled = true;
  try {
    let k = 0;
    for (const req of list) {
      const res = await api("/api/generate", { json: { count: n, params: { ...base, mode: "diff", init_image: diffSrc.name, diff: { req, engine }, prompt: "", negative: "", seed: null } } });
      track(res.jobs);
      k += res.jobs.length;
    }
    diffDlg.close();
    toast(`已送出 ${k} 張差分`, "ok");
  } catch (e) { toast("差分送出失敗：" + e.message, "err"); }
  finally { $("#diffGo").disabled = false; }
}

// ---------------------------------------------------------------- 塗鴉畫布
let sk = null;  // {dlg, cv, ctx, undo, tool, drawing, last}

function buildSketch() {
  const d = dialog("sketchModal", "塗鴉畫布：畫草圖 → 交給 AI 畫完", "wide");
  const cv = el("canvas", { id: "skCanvas" });
  const guide = el("img", { id: "skGuide", alt: "", hidden: true });
  const tools = el("div", { class: "piano row", id: "skTool" },
    el("button", { type: "button", "data-t": "pen", class: "on" }, "筆"), el("button", { type: "button", "data-t": "erase" }, "橡皮擦"));
  const color = el("input", { type: "color", id: "skColor", value: "#000000", title: "筆的顏色（當構圖控制時用黑色最準）" });
  const size = el("input", { type: "range", id: "skSize", min: 2, max: 80, value: 6, "aria-label": "筆的粗細" });
  const guideOn = el("input", { type: "checkbox", id: "skGuideOn" });
  d.body.append(
    el("div", { class: "row gap wrap" }, tools, color, el("span", { class: "sub inline" }, "粗細"), size,
      el("button", { type: "button", class: "akey sm", onclick: () => undo() }, "復原"),
      el("button", { type: "button", class: "akey sm", onclick: () => { snap(); clear(); } }, "清空"),
      el("label", { class: "check inline", title: "把生成頁的輸入圖淡淡地墊在下面描（不會畫進草圖）" }, guideOn, " 墊輸入圖描")),
    el("div", { class: "sk-stage" }, cv, guide),
    el("div", { class: "muted sm" }, "Anima：黑線白底的線稿當「構圖控制（塗鴉草圖）」最準，AI 照線條構圖、自己上色。其他模型：當圖生圖的底圖（可以畫色塊），重繪 0.85。"),
    el("div", { class: "row gap wrap" }, el("span", { class: "grow" }),
      el("button", { type: "button", class: "akey", id: "skAsInit", onclick: () => done("init") }, "當圖生圖底圖"),
      el("button", { type: "button", class: "akey go", id: "skAsCtrl", onclick: () => done("ctrl") }, "當構圖控制")));
  tools.onclick = (e) => {
    const b = e.target.closest("button"); if (!b) return;
    sk.tool = b.dataset.t;
    $$("#skTool button").forEach((x) => x.classList.toggle("on", x === b));
  };
  guideOn.onchange = () => { guide.hidden = !guideOn.checked || !gen.S.init; };
  const pos = (e) => { const r = cv.getBoundingClientRect(); return { x: (e.clientX - r.left) * cv.width / r.width, y: (e.clientY - r.top) * cv.height / r.height }; };
  cv.addEventListener("pointerdown", (e) => { cv.setPointerCapture(e.pointerId); snap(); sk.drawing = true; sk.last = pos(e); line(sk.last, sk.last); });
  cv.addEventListener("pointermove", (e) => { if (!sk.drawing) return; const p = pos(e); line(sk.last, p); sk.last = p; });
  const end = () => (sk.drawing = false);
  cv.addEventListener("pointerup", end);
  cv.addEventListener("pointercancel", end);
  document.addEventListener("keydown", (e) => { if (!d.m.hidden && e.key === "z" && e.ctrlKey) { e.preventDefault(); undo(); } });
  return { dlg: d, cv, ctx: cv.getContext("2d"), undo: [], tool: "pen", drawing: false, last: null };
}

function line(a, b) {
  const c = sk.ctx, k = sk.cv.width / sk.cv.getBoundingClientRect().width;
  c.strokeStyle = c.fillStyle = sk.tool === "erase" ? "#ffffff" : $("#skColor").value;
  c.lineWidth = +$("#skSize").value * k * (sk.tool === "erase" ? 3 : 1);
  c.lineCap = c.lineJoin = "round";
  c.beginPath(); c.moveTo(a.x, a.y); c.lineTo(b.x, b.y); c.stroke();
}
function snap() { sk.undo.push(sk.ctx.getImageData(0, 0, sk.cv.width, sk.cv.height)); if (sk.undo.length > 20) sk.undo.shift(); }
function undo() { if (sk.undo.length) sk.ctx.putImageData(sk.undo.pop(), 0, 0); }
function clear() { sk.ctx.fillStyle = "#ffffff"; sk.ctx.fillRect(0, 0, sk.cv.width, sk.cv.height); }

/** 畫布比例跟生成頁的寬高一樣（長邊最多 1024） */
export function openSketch() {
  sk ||= buildSketch();
  const w = +$("#width").value || 1024, h = +$("#height").value || 1024, k = Math.min(1, 1024 / Math.max(w, h));
  const W = Math.round(w * k), H = Math.round(h * k);
  if (sk.cv.width !== W || sk.cv.height !== H) { sk.cv.width = W; sk.cv.height = H; sk.undo = []; clear(); }
  const g = $("#skGuide");
  if (gen.S.init) g.src = `/input/${encodeURIComponent(gen.S.init.name)}`;
  g.hidden = !$("#skGuideOn").checked || !gen.S.init;
  $("#skAsCtrl").disabled = famOf(curModel()) !== "anima";
  $("#skAsCtrl").title = famOf(curModel()) === "anima" ? "" : "構圖控制只有 Anima 能用";
  sk.dlg.open();
}

async function done(as) {
  const blob = await new Promise((r) => sk.cv.toBlob(r, "image/png"));
  try {
    const r = await uploadFile(new File([blob], "sketch.png", { type: "image/png" }), "image");
    sk.dlg.close();
    if (as === "ctrl") {
      gen.setCtrl(r, { type: "scribble", preprocess: "none", invert: false });
      toast("草圖已放進「構圖控制」（塗鴉草圖），寫好提示詞按生成", "ok");
    } else {
      gen.setInit(r);
      const isMm = famOf(curModel()) === "mmimage";
      const dn = $("#denoise");
      if (dn && !isMm) { dn.value = 0.85; dn.dispatchEvent(new Event("input", { bubbles: true })); }
      toast(isMm ? "草圖已放進圖生圖／改圖，寫好提示詞按生成" : "草圖已放進圖生圖／改圖（重繪 0.85），寫好提示詞按生成", "ok");
    }
  } catch (e) { toast("草圖上傳失敗：" + e.message, "err"); }
}

// ---------------------------------------------------------------- X/Y 對照表
const rand = () => Math.floor(Math.random() * 2 ** 32);
const AXES = {
  seed: { name: "種子", def: () => [rand(), rand(), rand(), rand()].join(", "), num: true, apply: (p, v) => (p.seed = Math.round(v)) },
  steps: { name: "步數", def: () => "8, 16, 25", num: true, apply: (p, v) => (p.steps = Math.round(v)) },
  cfg: { name: "CFG", def: () => "1, 3, 5", num: true, apply: (p, v) => (p.cfg = v) },
  sampler: { name: "取樣器", def: () => [$("#sampler").value, "er_sde", "dpmpp_2m"].filter((v, i, a) => a.indexOf(v) === i).join(", "), sel: "#sampler", apply: (p, v) => (p.sampler = v) },
  scheduler: { name: "排程", def: () => [$("#scheduler").value, "simple", "sgm_uniform"].filter((v, i, a) => a.indexOf(v) === i).join(", "), sel: "#scheduler", apply: (p, v) => (p.scheduler = v) },
  lora: { name: "LoRA 強度", def: () => "0, 0.5, 1", num: true, apply: (p, v, ax) => {
    p.loras = (p.loras || []).filter((l) => l.name !== ax.lora);
    if (v) p.loras.push({ name: ax.lora, strength: v });
  } },
  sr: { name: "提示詞替換", def: () => ($("#prompt").value.split(/[,，\n]/).map((s) => s.trim()).filter(Boolean).pop() || ""), apply: (p, v, ax) => (p.prompt = p.prompt.split(ax.vals[0]).join(v)) },
  denoise: { name: "重繪強度", def: () => "0.3, 0.5, 0.7", num: true, apply: (p, v) => (p.denoise = v) },
};
const MAX_CELLS = 36;
let xyDlg = null, run = null;

function axisRow(k, withNone) {
  const type = el("select", { id: `xy${k}Type`, class: "sm" },
    withNone ? el("option", { value: "" }, "（不用）") : null,
    ...Object.entries(AXES).map(([v, a]) => el("option", { value: v }, a.name)));
  const lora = el("select", { id: `xy${k}Lora`, class: "sm", hidden: true });
  const vals = el("input", { type: "text", id: `xy${k}Vals`, placeholder: "用逗號分開，例如 1, 3, 5", oninput: () => xyInfo() });
  type.onchange = () => {
    const a = AXES[type.value];
    vals.value = a ? a.def() : "";
    vals.disabled = !a;
    lora.hidden = type.value !== "lora";
    if (type.value === "lora") {
      const ls = gen.S.loras.filter((l) => l.enabled);
      lora.replaceChildren(...ls.map((l) => el("option", { value: l.name }, l.title || l.name.replace(/\.safetensors$/, ""))));
      if (!ls.length) toast("生成頁還沒掛 LoRA：先在 LoRA 區加一個再比較強度", "err");
    }
    if (type.value === "sr") toast("提示詞替換：第一個值是要被換掉的字，後面每個值各畫一格（第一格就是原本的）");
    xyInfo();
  };
  return el("div", { class: "xy-axis" }, el("b", {}, `${k} 軸`), type, lora, vals);
}

function buildXY() {
  const d = dialog("xyModal", "X/Y 對照表：一次比較兩種設定", "wide");
  d.body.append(
    el("div", { class: "muted sm" }, "用生成頁目前的所有設定（模型、提示詞、LoRA、模式），只改你選的那一兩個參數，每個組合畫一格，最後拼成一張表存進圖庫。種子沒選成軸的話，所有格子用同一個種子。"),
    axisRow("X", false), axisRow("Y", true),
    el("div", { class: "row gap wrap" }, el("span", { class: "muted sm", id: "xyInfo" }), el("span", { class: "grow" }),
      el("button", { type: "button", class: "akey go", id: "xyGo", onclick: startXY }, "開始")),
    el("div", { class: "vfd g sm", id: "xyProg", hidden: true }),
    el("div", { class: "xy-out", id: "xyOut", hidden: true }, el("img", { id: "xyImg", alt: "對照表" }),
      el("div", { class: "row gap" }, el("span", { class: "grow" }), el("a", { class: "akey sm", id: "xyDl", download: "xy.png" }, "下載"))));
  $("#xyXType", d.m).value = "seed";
  $("#xyXType", d.m).dispatchEvent(new Event("change"));
  return d;
}

function readAxis(k) {
  const t = $(`#xy${k}Type`).value;
  if (!t) return null;
  const a = AXES[t];
  let vals = $(`#xy${k}Vals`).value.split(/[,，\n]/).map((s) => s.trim()).filter(Boolean);
  if (a.num) {
    if (vals.some((v) => !isFinite(+v))) throw new Error(`${k} 軸（${a.name}）只能填數字`);
    vals = vals.map(Number);
  }
  if (a.sel) {
    const ok = new Set([...$(a.sel).options].map((o) => o.value));
    const bad = vals.filter((v) => !ok.has(v));
    if (bad.length) throw new Error(`${k} 軸：沒有這個${a.name}：${bad.join("、")}`);
  }
  if (!vals.length) throw new Error(`${k} 軸還沒填值`);
  const ax = { k, type: t, name: a.name, vals, lora: t === "lora" ? $(`#xy${k}Lora`).value : null };
  if (t === "lora" && !ax.lora) throw new Error("生成頁還沒掛 LoRA");
  if (t === "sr" && vals.length < 2) throw new Error("提示詞替換至少要兩個值：要換掉的字＋換成什麼");
  return ax;
}

function xyInfo() {
  try {
    const x = readAxis("X"), y = readAxis("Y");
    const n = x.vals.length * (y ? y.vals.length : 1);
    $("#xyInfo").textContent = n > MAX_CELLS ? `${n} 格，太多了（最多 ${MAX_CELLS}）` : `共 ${n} 格`;
    $("#xyGo").disabled = n > MAX_CELLS || !!run;
  } catch (e) { $("#xyInfo").textContent = e.message; $("#xyGo").disabled = true; }
}

export function openXY() {
  xyDlg ||= buildXY();
  xyInfo();
  xyDlg.open();
}

const label = (ax, v) => (ax.type === "lora" ? `${v}` : String(v));

async function startXY() {
  let x, y;
  try { x = readAxis("X"); y = readAxis("Y"); } catch (e) { return toast(e.message, "err"); }
  const base = gen.collectParams();
  if (["upscale", "strip", "sheet"].includes(base.mode)) return toast("這個模式不能做對照表：換成文生圖、圖生圖／改圖、局部重繪或擴圖", "err");
  if (base.mode === "multiref" ? !base.refs?.length : base.mode !== "txt2img" && !base.init_image) return toast("請先放入圖片", "err");
  for (const ax of [x, y].filter(Boolean)) {
    if (ax.type === "denoise" && base.denoise == null) return toast("重繪強度只有圖生圖、局部重繪能比", "err");
    if (ax.type === "sr" && !base.prompt.includes(ax.vals[0])) return toast(`提示詞裡找不到「${ax.vals[0]}」`, "err");
  }
  if (base.seed == null) base.seed = rand();
  const ys = y ? y.vals : [null];
  run = { id: Date.now().toString(36), x, y, base, total: x.vals.length * ys.length, jobs: new Map(), cells: new Map(), sent: 0 };
  $("#xyGo").disabled = true;
  $("#xyOut").hidden = true;
  progress();
  try {
    for (const [yi, yv] of ys.entries()) {
      for (const [xi, xv] of x.vals.entries()) {
        const q = structuredClone(base);
        if (y) AXES[y.type].apply(q, yv, y);
        AXES[x.type].apply(q, xv, x);
        q.xy = { run: run.id, x: `${x.name} ${label(x, xv)}`, y: y ? `${y.name} ${label(y, yv)}` : null };
        const res = await api("/api/generate", { json: { params: q, count: 1 } });
        track(res.jobs);
        run.jobs.set(res.jobs[0].job, `${xi},${yi}`);
        run.sent++;
        progress();
      }
    }
    toast(`對照表：已送出 ${run.total} 格，畫完會自動拼好存進圖庫`, "ok");
  } catch (e) {
    toast(`對照表送到第 ${run.sent + 1} 格失敗：${e.message}`, "err");
    run.total = run.sent;  // 已經送出的照樣拼
    if (!run.total) { run = null; xyInfo(); }
  }
  check();
}

function progress() {
  const p = $("#xyProg");
  if (!p || !run) return;
  p.hidden = false;
  p.textContent = `送出 ${run.sent}／${run.total}・完成 ${run.cells.size}／${run.total}`;
}

/** main.js 收到新圖時呼叫（要在 gen.onImage 之前） */
export function onImage(job, rec) {
  if (run?.jobs.has(job)) { run.cells.set(run.jobs.get(job), rec); progress(); check(); }
  else if (gen.S.pending.has(job) && ["txt2img", "img2img", "multiref"].includes(rec?.mode) && $("#autoDetail")?.checked && !rec?.params?.xy) detail(rec, "face", true);
}
/** main.js 收到任務結束時呼叫（要在 gen.onFinish 之前）：失敗、取消的格子也算完成 */
export function onFinish(m) {
  if (!run?.jobs.has(m.job) || m.status === "done") return;
  const key = run.jobs.get(m.job);
  if (!run.cells.has(key)) run.cells.set(key, { failed: m.error || m.status || "失敗" });
  progress();
  check();
}

function check() {
  if (run && run.total && run.sent === run.total && run.cells.size >= run.total) {
    const r = run;
    run = null;
    compose(r).catch((e) => toast("拼對照表失敗：" + e.message, "err")).finally(() => xyDlg && xyInfo());
  }
}

const loadImg = (src) => new Promise((res) => { const i = new Image(); i.onload = () => res(i); i.onerror = () => res(null); i.src = src; });

function wrapText(c, text, maxW) {
  const out = [];
  let cur = "";
  for (const ch of text) {
    if (c.measureText(cur + ch).width > maxW && cur) { out.push(cur); cur = ch; } else cur += ch;
  }
  if (cur) out.push(cur);
  return out;
}

async function compose(r) {
  const ys = r.y ? r.y.vals : [null];
  const keys = ys.flatMap((_, yi) => r.x.vals.map((_, xi) => `${xi},${yi}`));
  const imgs = new Map();
  await Promise.all(keys.map(async (k) => { const c = r.cells.get(k); if (c?.id) imgs.set(k, await loadImg(`/img/${c.id}`)); }));
  const first = [...imgs.values()].find(Boolean);
  if (!first) throw new Error("每一格都失敗了");
  const cw = Math.min(512, first.naturalWidth), ch = Math.round(cw * first.naturalHeight / first.naturalWidth);
  const gap = 8, lw = r.y ? 200 : 0, title = 64, head = 44;
  const W = lw + r.x.vals.length * (cw + gap) + gap, H = title + head + ys.length * (ch + gap) + gap;
  const cv = el("canvas", { width: W, height: H }), c = cv.getContext("2d");
  // 簡中介面用雅黑（跟 app.css 的 :lang(zh-Hans) 一樣）
  const font = (px, w = 600) => `${w} ${px}px ${document.documentElement.lang.startsWith("zh-Hans") ? '"Microsoft YaHei", "PingFang SC"' : '"Microsoft JhengHei", "PingFang TC"'}, sans-serif`;
  c.fillStyle = "#1b1714"; c.fillRect(0, 0, W, H);
  c.fillStyle = "#f2e6d0"; c.font = font(22, 800); c.textBaseline = "middle";
  const seedTxt = r.x.type === "seed" || r.y?.type === "seed" ? "" : `　種子 ${r.base.seed}`;
  c.fillText(`X：${r.x.name}${r.y ? `　Y：${r.y.name}` : ""}　${r.base.model || ""}${seedTxt}`, gap + 4, 22);
  c.font = font(15, 400); c.fillStyle = "#b9ab95";
  c.fillText(wrapText(c, r.base.prompt || "", W - 2 * gap)[0] || "", gap + 4, 48);
  c.font = font(18); c.fillStyle = "#e2a72b"; c.textAlign = "center";
  r.x.vals.forEach((v, xi) => c.fillText(label(r.x, v), lw + gap + xi * (cw + gap) + cw / 2, title + head / 2, cw));
  ys.forEach((yv, yi) => {
    const y0 = title + head + yi * (ch + gap);
    if (r.y) {
      c.textAlign = "left"; c.fillStyle = "#e2a72b"; c.font = font(18);
      wrapText(c, label(r.y, yv), lw - 2 * gap).slice(0, 4).forEach((t, i) => c.fillText(t, gap, y0 + ch / 2 + (i - 0.5) * 24));
    }
    r.x.vals.forEach((_, xi) => {
      const k = `${xi},${yi}`, x0 = lw + gap + xi * (cw + gap), im = imgs.get(k);
      if (im) {
        const s = Math.min(cw / im.naturalWidth, ch / im.naturalHeight), w = im.naturalWidth * s, h = im.naturalHeight * s;
        c.drawImage(im, x0 + (cw - w) / 2, y0 + (ch - h) / 2, w, h);
      } else {
        c.fillStyle = "#2a2420"; c.fillRect(x0, y0, cw, ch);
        c.fillStyle = "#e0533a"; c.textAlign = "center"; c.font = font(20);
        c.fillText("失敗", x0 + cw / 2, y0 + ch / 2);
      }
    });
  });
  const blob = await new Promise((res) => cv.toBlob(res, "image/png"));
  if (xyDlg) {
    const url = URL.createObjectURL(blob);
    $("#xyImg").src = url; $("#xyDl").href = url; $("#xyOut").hidden = false;
  }
  const fd = new FormData();
  fd.append("image", blob, "xy.png");
  fd.append("meta", JSON.stringify({ prompt: r.base.prompt || "", xy: { x: { name: r.x.name, vals: r.x.vals, lora: r.x.lora }, y: r.y && { name: r.y.name, vals: r.y.vals, lora: r.y.lora }, seed: r.base.seed, model: r.base.model, mode: r.base.mode } }));
  const saved = await api("/api/play/save_grid", { method: "POST", body: fd });
  toast(`對照表拼好了，已存進圖庫（#${saved.id}）`, "ok");
}

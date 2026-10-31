// 機台面板（設計稿第 6 版）：圖示、按鍵音、旋鈕、觀景窗讀數、卡帶捲軸、狀態列、視窗按鈕。
// 旋鈕綁在原本的輸入框上（#steps、#cfg、#count），舊的程式照樣讀寫那幾個輸入框，旋鈕自己跟著變
import { $, $$, store, modeName, api, toast } from "./util.js";
import { play } from "./sfx.js";

const F = (id) => $("#" + id);

// ---------------------------------------------------------------- 圖示：24 格、線寬 2；.f＝實心、.a＝色塊加外框、.af＝實心色塊
const STAR = "M12 3.5l2.4 5.8 6.2.4-4.8 4 1.5 6.1L12 16.5l-5.3 3.3 1.5-6.1-4.8-4 6.2-.4z";
const SPK = '<path d="M3 9h4l5-4v14l-5-4H3z"/>';
export const ICONS = {
  cam: '<path d="M8 6V4h5v2"/><rect x="3" y="6" width="18" height="14" rx="2"/><circle cx="12" cy="13" r="4"/><circle class="af" cx="12" cy="13" r="1.8"/><rect class="f" x="16" y="8" width="3" height="2"/>',
  slides: '<path d="M7 3h12a2 2 0 0 1 2 2v12"/><rect x="3" y="7" width="14" height="14" rx="2"/><rect class="a" x="6" y="11" width="8" height="6" rx="1.5"/>',
  cart: '<path d="M4 3h16v15h-2v3H6v-3H4z"/><rect class="a" x="7" y="6" width="10" height="7" rx="1"/><path d="M10 18v3M14 18v3"/>',
  dish: '<path d="M3 9a9 9 0 0 0 12 12z"/><path d="M9 15l5-5"/><circle class="af" cx="15" cy="9" r="2"/><path d="M17 3a4 4 0 0 1 4 4"/>',
  punch: '<path d="M7 3h13v18H4V6z"/><rect class="f" x="7" y="7" width="2" height="3"/><rect class="f" x="15" y="7" width="2" height="3"/><rect class="af" x="11" y="11" width="2" height="3"/><rect class="f" x="15" y="11" width="2" height="3"/><rect class="f" x="7" y="15" width="2" height="3"/><rect class="f" x="11" y="15" width="2" height="3"/>',
  tape: '<rect x="2" y="5" width="20" height="14" rx="2"/><circle class="a" cx="8" cy="10" r="2.5"/><circle class="a" cx="16" cy="10" r="2.5"/><path d="M8 12.5h8M6 19l1.5-3h9l1.5 3"/>',
  bubble: '<path d="M4 4h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1h-9l-5 4v-4H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z"/><rect class="af" x="7" y="8" width="3" height="3"/><rect class="af" x="14" y="8" width="3" height="3"/>',
  comic: '<rect x="4" y="2.5" width="16" height="19" rx="1"/><path d="M4 11h16M11 11v10.5"/><path class="af" d="M8 5h8a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1h-5l-2 1.5V9H8a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z"/>',
  palette: '<path d="M12 3a9 9 0 1 0 0 18c1.1 0 1.9-.8 1.9-1.8 0-.5-.2-.9-.5-1.2-.3-.3-.5-.7-.5-1.2 0-1 .8-1.8 1.8-1.8H17a4 4 0 0 0 4-4c0-4.4-4-8-9-8z"/><circle class="af" cx="7.5" cy="11.5" r="1.6"/><circle class="f" cx="10" cy="7.2" r="1.5"/><circle class="f" cx="15" cy="7.6" r="1.5"/>',
  clap: '<path d="M3 10h18v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M3 10l-.6-3.4L18.8 3.6l.7 3.4z"/><path class="a" d="M7.5 5.8l2.6 3.4M12.5 4.9l2.6 3.4"/><rect class="af" x="6" y="13" width="5" height="2"/>',
  faders: '<path d="M5 3v18M12 3v18M19 3v18"/><rect class="f" x="2" y="13" width="6" height="4" rx="1"/><rect class="af" x="9" y="6" width="6" height="4" rx="1"/><rect class="f" x="16" y="10" width="6" height="4" rx="1"/>',
  t2i: '<rect x="3" y="3" width="18" height="18" rx="3"/><path d="M7.5 8h9M12 8v9"/>',
  i2i: '<path d="M2 12h7M6 9l3 3-3 3"/><rect x="11" y="5" width="11" height="14" rx="2"/><rect class="af" x="14" y="9" width="5" height="6" rx="1"/>',
  inpaint: '<rect x="3" y="3" width="13" height="13" rx="1" stroke-dasharray="3 2"/><path class="a" d="M12 21l1-4 7-7 3 3-7 7z"/>',
  multi: '<path d="M10 2h10a2 2 0 0 1 2 2v10M6 6h10a2 2 0 0 1 2 2v10"/><rect x="2" y="10" width="12" height="12" rx="2"/><rect class="af" x="5" y="13" width="6" height="6" rx="1"/>',
  zoom: '<circle cx="10" cy="10" r="6.5"/><path d="M15 15l6 6" stroke-width="3"/><path d="M10 7v6M7 10h6"/>',
  search: '<circle cx="10" cy="10" r="6.5"/><path d="M15 15l6 6" stroke-width="3"/>',
  loop: '<path d="M4 12a4 4 0 0 1 4-4h11M16 5l3 3-3 3M20 12a4 4 0 0 1-4 4H5M8 13l-3 3 3 3"/>',
  rewind: '<path class="f" d="M11 6v12L3 12zM20 6v12l-8-6z"/>',
  rec: '<circle cx="12" cy="12" r="8.5"/><circle class="af" cx="12" cy="12" r="4.5"/>',
  eject: '<path class="f" d="M12 4l8 9H4z"/><rect class="f" x="4" y="16" width="16" height="3"/>',
  star: `<path d="${STAR}"/>`,
  starOn: `<path class="a" d="${STAR}"/>`,
  cmp: '<rect x="3" y="4" width="18" height="16" rx="2"/><rect class="af" x="4" y="5" width="7" height="14"/><path d="M12 2v20"/>',
  floppy: '<path d="M4 3h13l3 3v15H4z"/><path d="M8 3v5h7V3"/><rect class="a" x="7" y="13" width="10" height="8"/>',
  die: '<rect x="3" y="3" width="18" height="18" rx="4"/><circle class="f" cx="8" cy="8" r="1.7"/><circle class="f" cx="16" cy="8" r="1.7"/><circle class="af" cx="12" cy="12" r="1.7"/><circle class="f" cx="8" cy="16" r="1.7"/><circle class="f" cx="16" cy="16" r="1.7"/>',
  spk: SPK + '<path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11"/>',
  mute: SPK + '<path d="M16 9l5 6M21 9l-5 6"/>',
  chip: '<rect x="6" y="6" width="12" height="12" rx="1"/><rect class="af" x="9" y="9" width="6" height="6"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>',
  win: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/>',
  min: '<path d="M5 18h14"/>',
  max: '<rect x="5" y="5" width="14" height="14" rx="1"/><path d="M5 9h14"/>',
  power: '<path d="M12 3v9M7.8 7.4a7 7 0 1 0 8.4 0"/>',
  expand: '<rect x="8" y="8" width="8" height="8" rx="1"/><path d="M3 8V3h5M16 3h5v5M21 16v5h-5M8 21H3v-5"/>',
};
export const ic = (n) => `<svg class="ic" viewBox="0 0 24 24" aria-hidden="true">${ICONS[n] || ""}</svg>`;
export function setIc(el, n) {
  el.dataset.ic = n;
  el.querySelector(":scope > .ic")?.remove();
  el.insertAdjacentHTML("afterbegin", ic(n));
}

// 程式圖示：米白機身、深色小窗裡一個琥珀色點陣 A、底下四色條紋
export const appIcon = (px) => `<svg class="appic" width="${px}" height="${px}" viewBox="0 0 64 64" aria-hidden="true">
  <rect x="2.5" y="2.5" width="59" height="59" rx="13" fill="#e6dfcd" stroke="#a99f86" stroke-width="2"/>
  <path d="M4 16a13 13 0 0 1 13-13h30a13 13 0 0 1 13 13" fill="none" stroke="#f7f2e4" stroke-width="2"/>
  <rect x="10" y="10" width="44" height="24" rx="4" fill="#0b120d"/>
  <g fill="#ffb547"><rect x="23.25" y="16.75" width="3.5" height="14"/><rect x="37.25" y="16.75" width="3.5" height="14"/><rect x="26.75" y="13.25" width="10.5" height="3.5"/><rect x="26.75" y="20.25" width="10.5" height="3.5"/></g>
  <rect x="10" y="40" width="44" height="3.5" fill="#e2a72b"/><rect x="10" y="44.5" width="44" height="3.5" fill="#e6742c"/>
  <rect x="10" y="49" width="44" height="3.5" fill="#c8412d"/><rect x="10" y="53.5" width="44" height="3.5" fill="#6b4a2b"/>
</svg>`;

// ---------------------------------------------------------------- 按鍵音（可以關；聲音本身在 sfx.js）
let soundOn = store.get("sound", true), keyDown = false;
export function click(name = "key") { if (soundOn) play(name); }
// 按下時依元件挑聲音；一般按鍵放開時再補一聲回彈
function onPointerDown(e) {
  const t = e.target;
  if (t.closest(".knob")) return;  // 旋鈕自己會一格一格響
  const sw = t.closest('input[type=checkbox], label.check');
  if (sw) { if (!(sw.disabled || sw.querySelector?.("input:disabled"))) click("toggle"); return; }
  const b = t.closest("button, .akey, summary");
  if (!b || b.disabled) return;
  if (b.closest(".rocker")) return click("slide");
  click(b.classList.contains("bigkey") ? "big" : "key");
  keyDown = true;
}
function onPointerUp() { if (keyDown) { keyDown = false; click("keyUp"); } }
function paintSound() {
  const b = F("soundBtn");
  b.setAttribute("aria-pressed", String(soundOn));
  b.title = soundOn ? "按鍵音效：開" : "按鍵音效：關";
  setIc(b, soundOn ? "spk" : "mute");
}

// ---------------------------------------------------------------- 輸入框的值被程式改掉時也通知（旋鈕、尺寸讀數用）
function watchValue(input, cb) {
  const proto = Object.getPrototypeOf(input);
  const desc = Object.getOwnPropertyDescriptor(proto, "value");
  Object.defineProperty(input, "value", {
    configurable: true,
    get() { return desc.get.call(this); },
    set(v) { desc.set.call(this, v); cb(); },
  });
  input.addEventListener("input", cb);
  input.addEventListener("change", cb);
  new MutationObserver(cb).observe(input, { attributes: true, attributeFilter: ["disabled"] });
  if (input.tagName === "SELECT") new MutationObserver(cb).observe(input, { childList: true });
}

// ---------------------------------------------------------------- 旋鈕：繞圈拖曳、滾輪、方向鍵；有刻度的會喀一聲
/** 指標從角度 a0 轉到 a1（度，atan2 的 -180～180）轉了幾度：走近的那邊，順時針為正 */
export function turnDelta(a0, a1) {
  let d = a1 - a0;
  while (d > 180) d -= 360;
  while (d <= -180) d += 360;
  return d;
}
/** 轉幾度跳一格：刻度照 270 度的弧平分（指針跟著手），選項很多時至少 6 度一格，免得手抖就跳 */
export function stepDegrees(count) {
  return Math.max(6, 270 / Math.max(1, count - 1));
}

function knob(box) {
  const input = F(box.dataset.for);
  box.innerHTML = `<div class="dial" role="slider" tabindex="0" aria-label="${box.dataset.label}"><span class="ind"></span></div><span class="vfd g"></span><span class="k">${box.dataset.label}</span>`;
  const dial = box.querySelector(".dial"), ind = box.querySelector(".ind"), out = box.querySelector(".vfd");
  const isSel = input.tagName === "SELECT";
  // 可選的值：選單＝選項；數字框＝min～max 每 step 一格
  const values = () => {
    if (isSel) return [...input.options].map((o) => o.value);
    const min = +input.min, max = +input.max, step = +input.step || 1, v = [];
    for (let x = min; x <= max + 1e-9; x += step) v.push(+x.toFixed(4));
    return v;
  };
  const index = () => {
    const vs = values(), cur = isSel ? input.value : +input.value;
    if (isSel) return Math.max(0, vs.indexOf(cur));
    let best = 0;
    vs.forEach((x, i) => { if (Math.abs(x - cur) < Math.abs(vs[best] - cur)) best = i; });
    return best;
  };
  const paint = () => {
    const vs = values(), i = index(), n = Math.max(1, vs.length - 1);
    ind.style.setProperty("--rot", `${-135 + (i / n) * 270}deg`);
    const v = isSel ? input.value : +input.value;
    out.textContent = isSel ? (input.selectedOptions[0]?.textContent || v) : (+input.step || 1) < 1 ? (+v).toFixed(1) : String(v);
    dial.setAttribute("aria-valuenow", String(v));
    box.classList.toggle("off", input.disabled);
    dial.setAttribute("aria-disabled", String(input.disabled));
    box.title = input.title || "";
  };
  const setIndex = (i) => {
    if (input.disabled) return;
    const vs = values();
    i = Math.max(0, Math.min(vs.length - 1, i));
    if (i === index()) return;
    input.value = String(vs[i]);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
    click("detent");
  };
  watchValue(input, paint);
  // 像真的旋鈕一樣繞著中心轉：累積轉過的角度，順時針加、逆時針減，可以連轉好幾圈（超過兩端就停在兩端）
  dial.addEventListener("pointerdown", (e) => {
    if (input.disabled) return;
    dial.setPointerCapture(e.pointerId);
    const r = dial.getBoundingClientRect(), cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    const at = (ev) => Math.atan2(ev.clientY - cy, ev.clientX - cx) * 180 / Math.PI;
    const near = (ev) => Math.hypot(ev.clientX - cx, ev.clientY - cy) < 6; // 太靠中心角度會亂跳，不算
    const i0 = index(), per = stepDegrees(values().length);
    let last = at(e), acc = 0;
    dial.onpointermove = (ev) => {
      const a = at(ev);
      if (!near(ev)) acc += turnDelta(last, a);
      last = a;
      setIndex(i0 + Math.round(acc / per));
    };
  });
  dial.addEventListener("pointerup", () => (dial.onpointermove = null));
  dial.addEventListener("wheel", (e) => { e.preventDefault(); setIndex(index() + (e.deltaY < 0 ? 1 : -1)); }, { passive: false });
  dial.addEventListener("keydown", (e) => {
    const d = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 3, PageDown: -3 }[e.key];
    if (d) { e.preventDefault(); setIndex(index() + d); }
  });
  paint();
}

// ---------------------------------------------------------------- 尺寸讀數（生成鍵旁邊）
function sizeReadout() {
  const w = F("width"), h = F("height");
  const gcd = (a, b) => (b ? gcd(b, a % b) : a);
  const paint = () => {
    const W = +w.value, H = +h.value;
    if (!W || !H) return;
    F("sizeVfd").textContent = `${W}×${H}`;
    const g = gcd(W, H), r = `${W / g}:${H / g}`;
    F("sizeAspect").textContent = (W === H ? "方 " : W < H ? "直 " : "橫 ") + (r.length <= 5 ? r : (W / H).toFixed(2));
  };
  watchValue(w, paint); watchValue(h, paint);
  paint();
}

// ---------------------------------------------------------------- 觀景窗讀數、卡帶
let t0 = 0, mine = false;
const reel = (p) => { F("reelL").style.setProperty("--d", `${28 - p * 0.12}px`); F("reelR").style.setProperty("--d", `${16 + p * 0.12}px`); };

// ---------------------------------------------------------------- 等待動畫（ComfyUI 暖機、載入模型）
// warm＝ComfyUI 還沒連上；load＝任務開始了但還沒有進度（在載入模型）。進度一來就收起來
let bootMode = null, bootT0 = 0, bootTick = 0, loadTimer = 0;
const BOOT = {
  warm: { tag: "WARM-UP", title: "WARMING UP", text: "ComfyUI 暖機中", hint: "程式剛打開時會在背景啟動 ComfyUI，大約 20～60 秒。好了這個畫面會自己收起來。",
          slow: 120, slowHint: "等太久了：ComfyUI 可能沒開起來。按下面的「重開 ComfyUI」；還是不行，從右下角系統列的生圖站圖示按右鍵「結束」，再開一次生圖站，或看資料夾裡的 data\\logs\\comfyui.log 最後幾行。" },
  load: { tag: "C-60", title: "LOADING", text: "載入模型中", hint: "第一張圖要先把模型讀進顯卡，大約 20～60 秒；之後同一個模型就快了。",
          slow: 180, slowHint: "比平常久：可能是模型很大、或顯存不夠在搬資料。可以再等一下，或到任務面板按「全停」。" },
};
const mmss = (s) => `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;

function boot(mode) {
  if (bootMode === mode) return;
  const b = BOOT[mode];
  bootMode = mode; bootT0 = Date.now();
  F("bootTag").textContent = b.tag;
  F("bootTitle").textContent = b.title;
  F("bootText").textContent = b.text;
  F("bootHint").textContent = b.hint;
  F("bootTime").textContent = "00:00";
  F("boot").classList.remove("warn");
  // 原因和重開鈕只在暖機（ComfyUI 沒連上）時出現
  F("bootNote").hidden = mode !== "warm" || !F("bootNote").textContent;
  F("bootRestart").hidden = F("bootNote").hidden;
  F("boot").hidden = false;
  clearInterval(bootTick);
  bootTick = setInterval(() => {
    const s = Math.floor((Date.now() - bootT0) / 1000);
    F("bootTime").textContent = mmss(s);
    if (s === b.slow) {
      F("bootHint").textContent = b.slowHint;
      F("boot").classList.add("warn");
      if (mode === "warm") F("bootRestart").hidden = false;
    }
  }, 500);
}

// ComfyUI 沒開起來、或自己關掉了：暖機畫面顯示原因（system.js 從 /api/system 的 comfy_note 傳進來）和「重開 ComfyUI」
export function setComfyNote(note) {
  F("bootNote").textContent = note || "";
  F("bootNote").hidden = !note || bootMode !== "warm";
  if (note && bootMode === "warm") F("bootRestart").hidden = false;
}

/** 重開 ComfyUI（生圖站自己開的才行；沒在跑的會直接開）。暖機畫面和狀態列的系統狀態共用 */
export async function restartComfy() {
  try {
    await api("/api/models/restart_comfy", { json: {} });
    toast("ComfyUI 重開中，大約半分鐘到一分鐘", "ok");
    F("bootNote").hidden = true;
    F("bootRestart").hidden = true;
    if (bootMode === "warm") { bootT0 = Date.now(); F("bootHint").textContent = BOOT.warm.hint; F("boot").classList.remove("warn"); }
    return true;
  } catch (e) { toast("重開不了：" + e.message, "err"); return false; }
}

function unboot(mode) {
  if (mode && bootMode !== mode) return;
  bootMode = null;
  clearInterval(bootTick);
  F("boot").hidden = true;
}

export function setOnline(on) {
  F("comfyTxt").textContent = on ? "COMFYUI 連線" : "COMFYUI 斷線";
  F("comfyTxt").classList.toggle("w", !on);
  if (on) { unboot("warm"); setComfyNote(""); F("bootRestart").hidden = true; }
  else if (bootMode !== "load") boot("warm");
}

// ---------------------------------------------------------------- 軟糖小熊（gummy.js，第一次用到才載入 three.js）
// 開始畫 → 從上面倒一把熊進觀景窗；全部畫完（idle）→ 地板打開、熊掉出去，掉完就停掉畫面迴圈
let gummy = null, gummyLoad = null;
const gummyOn = () => store.get("gummy", true);
function gummyWorld() {
  gummyLoad ??= import("./gummy.js").then(({ GummyWorld }) => {
    const win = F("win");
    gummy = new GummyWorld(F("gummy"), { dark: true, viewHeight: 4.6, elevation: 16, floorNdc: -0.9, res: 32, pixelRatio: 1.25, transmissionScale: 0.5,
      input: win, onCursor: (s) => (s ? (win.dataset.gummy = s) : delete win.dataset.gummy) });
    window.__gummy = gummy; // 除錯用
    return gummy;
  }).catch((e) => { console.warn("軟糖小熊載入失敗", e); gummyLoad = null; return null; });
  return gummyLoad;
}
async function gummyStart() {
  if (!gummyOn()) return;
  F("gummy").hidden = false; // 先顯示，畫布才量得到觀景窗的大小
  const w = await gummyWorld();
  if (!w || !busyNow) { if (!gummy?.bears.length) F("gummy").hidden = true; return; }
  w.resize();
  if (w.floorOpen) {
    // 掉到一半又開始畫：地板關回來，已經掉出去的不要了
    w.floorOpen = false;
    w.onLeft = null;
    for (const b of [...w.bears]) if (b.lowest < w.floorY + 0.3) { b.dispose(); w.bears.splice(w.bears.indexOf(b), 1); }
  }
  const want = Math.round(Math.min(7, Math.max(4, w.halfW * 1.2)));
  if (w.bears.length < want) w.pour(want - w.bears.length);
  w.start();
}
function gummyLeave() {
  if (!gummy || !gummy.bears.length) return;
  gummy.leave(() => { F("gummy").hidden = true; delete F("win").dataset.gummy; });
}
let busyNow = false;

export function onStart(job, isMine) {
  mine = isMine; t0 = Date.now();
  busyNow = true;
  gummyStart();
  // 1.2 秒內就有進度（模型已經在顯卡裡）就不跳動畫，免得一閃而過
  clearTimeout(loadTimer);
  loadTimer = setTimeout(() => boot("load"), 1200);
  F("cassette").classList.add("play");
  F("genBtn").classList.add("busy");
  const l = F("osdl"); l.textContent = "● RENDER"; l.className = "r";
  F("pct").textContent = "0%";
  reel(0);
}

export function onProgress(m) {
  clearTimeout(loadTimer);
  unboot("load");
  const p = Math.round((100 * m.value) / m.max);
  F("pct").textContent = `${p}%`;
  reel(p);
}

export function onFinish(m) {
  clearTimeout(loadTimer);
  unboot("load");
  if (m.status === "error") { const l = F("osdl"); l.textContent = "● ERROR"; l.className = "r"; }
}

export function idle() {
  busyNow = false;
  gummyLeave();
  clearTimeout(loadTimer);
  unboot("load");
  F("cassette").classList.remove("play");
  F("genBtn").classList.remove("busy");
  const l = F("osdl");
  if (l.textContent !== "● ERROR") { l.textContent = "● READY"; l.className = ""; }
  F("pct").textContent = "--%";
  reel(0);
}

export function showImage(rec) {
  const secs = mine && t0 ? `・${Math.max(1, Math.round((Date.now() - t0) / 1000))} 秒` : "";
  F("osdm").textContent = `${modeName[rec.mode] || rec.mode || ""} ${rec.width}×${rec.height}${secs}`;
  F("osdr").textContent = rec.seed != null ? `SEED ${rec.seed}` : "";
  const w = F("win");
  w.classList.remove("swap"); void w.offsetWidth; w.classList.add("swap");
  mine = false;
}

export function onPe(st) {
  const t = F("peText");
  if (!st?.ok) { t.textContent = "AI 改寫 無法使用"; t.className = "st-pe"; return; }
  t.textContent = st.busy ? "AI 改寫 執行中" : st.running ? `AI 改寫 待命 ${st.idle_left ?? ""}${st.idle_left != null ? "S" : ""}` : "AI 改寫 待機";
  t.className = "st-pe" + (st.busy ? " a" : "");
}

// ---------------------------------------------------------------- 螢幕字裡的中文包成 .zh（VT323 沒有中文字）
const ZH = /[　-〿㐀-鿿＀-￯]+/g, HAS_ZH = /[　-〿㐀-鿿＀-￯]/;
function wrapZh(root) {
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT), nodes = [];
  while (w.nextNode()) if (HAS_ZH.test(w.currentNode.data) && !w.currentNode.parentElement.closest(".zh")) nodes.push(w.currentNode);
  for (const n of nodes) {
    const s = document.createElement("span");
    s.innerHTML = n.data.replace(/[&<>]/g, (c) => `&#${c.charCodeAt(0)};`).replace(ZH, (m) => `<span class="zh">${m}</span>`);
    n.replaceWith(...s.childNodes);
  }
}
function watchZh(el) {
  wrapZh(el);
  const opts = { childList: true, subtree: true, characterData: true };
  const mo = new MutationObserver(() => { mo.disconnect(); wrapZh(el); mo.observe(el, opts); });
  mo.observe(el, opts);
}

// ---------------------------------------------------------------- 視窗（桌面版才有：自畫的最小化、最大化、關閉）
// 關閉鍵和 Alt+F4 都走 askClose：照設定頁的選擇縮到系統列，或跳出對話框問（main.rs 會把 Alt+F4 轉成 anima-close 事件）。
// 設定成「結束程式」也會再確認一次（容易不小心關掉）：對話框只剩「結束程式」「取消」，預設按鈕是取消
function windowControls() {
  const tauri = window.__TAURI__;
  if (!tauri?.window) return;
  F("wctl").hidden = false;
  F("setWinBox").hidden = false;
  const win = tauri.window.getCurrentWindow();
  const act = (a) => tauri.core.invoke(a === "quit" ? "quit_app" : "hide_window");
  const askClose = () => {
    const pref = store.get("closeAct", "ask");
    if (pref === "hide") return act("hide");
    const sure = pref === "quit";
    for (const id of ["closeAsk", "closeWhy", "closeHide", "closeRememberRow"]) F(id).hidden = sure;
    for (const id of ["closeSure", "closeSureWhy"]) F(id).hidden = !sure;
    const n = +F("qcnt").textContent || 0;
    F("closeBusy").hidden = !n;
    F("closeBusy").textContent = n ? `ComfyUI 還有 ${n} 個任務在排隊。如果 ComfyUI 是這個程式開的，結束時會一起關掉，排隊的就不會畫了；縮到系統列會繼續畫。` : "";
    F("closeRemember").checked = false;
    F("closeModal").hidden = false;
    F(sure ? "closeCancel" : "closeHide").focus();
  };
  const choose = (a) => {
    if (F("closeRemember").checked) { store.set("closeAct", a); F("setCloseAct").value = a; }
    F("closeModal").hidden = true;
    act(a);
  };
  F("closeHide").onclick = () => choose("hide");
  F("closeQuit").onclick = () => choose("quit");
  F("setCloseAct").value = store.get("closeAct", "ask");
  F("setCloseAct").onchange = (e) => store.set("closeAct", e.target.value);
  F("wctl").addEventListener("click", (e) => {
    const b = e.target.closest("[data-win]");
    if (!b) return;
    if (b.dataset.win === "min") win.minimize();
    else if (b.dataset.win === "max") win.toggleMaximize();
    else askClose();
  });
  tauri.event.listen("anima-close", askClose);
  document.documentElement.classList.add("app");
}

// ---------------------------------------------------------------- 初始化
/** 看圖框（#win）釘在原點：圖片貼著它定位，被捲到就整張歪掉（2026-10-26 朋友回報縮放、拉寬後圖片飄移）。
 *  CSS 已經是 overflow: clip，這裡是舊瀏覽器的保險：一開始、被捲到、變大小時都歸零 */
export function pinView(win, Observer = globalThis.ResizeObserver) {
  const zero = () => { if (win.scrollTop || win.scrollLeft) { win.scrollTop = 0; win.scrollLeft = 0; } };
  zero();
  win.addEventListener("scroll", zero);
  if (Observer) new Observer(zero).observe(win);
}

export function init() {
  pinView(F("win"));
  $$("[data-ic]").forEach((el) => setIc(el, el.dataset.ic));
  $$("[data-app]").forEach((el) => (el.outerHTML = appIcon(+el.dataset.app)));
  paintSound();
  F("soundBtn").onclick = () => { soundOn = !soundOn; store.set("sound", soundOn); paintSound(); };
  F("bootRestart").onclick = restartComfy;
  document.addEventListener("pointerdown", onPointerDown);
  document.addEventListener("pointerup", onPointerUp);
  document.addEventListener("pointercancel", () => (keyDown = false));
  $$(".knob[data-for]").forEach(knob);
  sizeReadout();
  reel(0);
  for (const el of [$(".readout"), $(".status")]) watchZh(el);
  windowControls();
  F("setGummy").checked = gummyOn();
  F("setGummy").onchange = (e) => {
    store.set("gummy", e.target.checked);
    if (e.target.checked) { if (busyNow) gummyStart(); } else gummyLeave();
  };
}

// 拍攝版本並排比較：兩欄各自選版本、同步播放，方便挑一個採用。
import { el } from "./util.js";

// 右欄要拉到哪裡：差在容許範圍內不動，超過右欄長度就停在結尾。
export function syncPlan(leftTime, rightTime, rightDuration, tolerance = 0.08) {
  if (Math.abs(leftTime - rightTime) <= tolerance) return null;
  if (Number.isFinite(rightDuration) && leftTime >= rightDuration) return rightDuration;
  return leftTime;
}

// 逐格前後：預設 24fps，時間夾在 0 到影片長度之間。
export function stepTime(t, dir, duration, fps = 24) {
  const max = Number.isFinite(duration) ? Math.max(0, duration) : Infinity;
  return Math.min(Math.max(0, t + dir / fps), max);
}

const fmt = (t) => {
  const s = Number.isFinite(t) && t > 0 ? t : 0;
  const m = Math.floor(s / 60), sec = Math.floor(s % 60), cs = Math.floor((s % 1) * 100);
  return `${m}:${String(sec).padStart(2, "0")}.${String(cs).padStart(2, "0")}`;
};

export function openTakeCompare({ title = "並排比較拍攝版本", items = [], onAdopt, disabled = false } = {}) {
  const list = (items || []).filter(Boolean);
  if (!list.length) return null;
  const cols = [];
  let sound = "left", closing = false;

  const mask = el("div", { class: "take-cmp-mask" });
  const panel = el("div", { class: "take-cmp" });

  function close() {
    if (closing) return;
    closing = true;
    for (const c of cols) c.video.pause();
    document.removeEventListener("keydown", onKey);
    mask.remove();
  }
  function onKey(e) {
    if (e.key === "Escape") { e.preventDefault(); close(); }
  }

  const playBtn = el("button", { type: "button", class: "akey sm", onclick: togglePlay }, "播放");
  const prevBtn = el("button", { type: "button", class: "akey sm", onclick: () => stepFrame(-1) }, "上一格");
  const nextBtn = el("button", { type: "button", class: "akey sm", onclick: () => stepFrame(1) }, "下一格");
  const range = el("input", { type: "range", class: "take-cmp-range", min: 0, max: 0, step: 0.01, value: 0, "aria-label": "播放時間", oninput: seekAll });
  const timeText = el("span", { class: "take-cmp-time muted sm" }, `${fmt(0)} / ${fmt(0)}`);
  const soundBtn = el("button", { type: "button", class: "akey sm", onclick: toggleSound }, "聲音：左");

  const withSrc = () => cols.filter((c) => c.item?.src);
  function pauseAll() { for (const c of cols) c.video.pause(); }
  function master() { return cols[0].item?.src ? cols[0] : cols[1]; }
  function totalDuration() {
    let max = 0;
    for (const c of cols) { const d = c.video.duration; if (Number.isFinite(d) && d > max) max = d; }
    return max;
  }
  function updateTime() {
    const t = master()?.video.currentTime || 0;
    timeText.textContent = `${fmt(t)} / ${fmt(totalDuration())}`;
    if (document.activeElement !== range) range.value = String(t);
  }
  function refreshRange() {
    const max = totalDuration();
    range.max = String(max);
    updateTime();
  }
  function seekAll() {
    const v = +range.value;
    for (const c of withSrc()) {
      const d = c.video.duration;
      c.video.currentTime = Number.isFinite(d) ? Math.min(v, d) : v;
    }
    updateTime();
  }
  function stepFrame(dir) {
    pauseAll();
    for (const c of withSrc()) c.video.currentTime = stepTime(c.video.currentTime, dir, c.video.duration);
    align();
  }
  function togglePlay() {
    if (withSrc().some((c) => !c.video.paused)) { pauseAll(); return; }
    for (const c of withSrc()) c.video.play().catch(() => {});
  }
  function toggleSound() {
    sound = sound === "left" ? "right" : "left";
    applySound();
  }
  function applySound() {
    cols[0].video.muted = sound !== "left";
    cols[1].video.muted = sound !== "right";
    soundBtn.textContent = `聲音：${sound === "left" ? "左" : "右"}`;
  }
  // 以左欄為主；左欄沒影片時才換右欄當主。
  function align() {
    const [a, b] = cols;
    if (a.item?.src && b.item?.src) {
      const plan = syncPlan(a.video.currentTime, b.video.currentTime, b.video.duration);
      if (plan !== null) b.video.currentTime = plan;
    }
    updateTime();
  }
  function onPlay() { playBtn.textContent = "暫停"; }
  function onPause() { if (!withSrc().some((c) => !c.video.paused)) playBtn.textContent = "播放"; }

  function column(side, initial) {
    const select = el("select", { class: "take-cmp-select", "aria-label": side === "left" ? "左欄版本" : "右欄版本", onchange: () => apply(list[+select.value]) },
      ...list.map((it, j) => el("option", { value: String(j) }, it.label || `版本 ${j + 1}`)));
    const video = el("video", { class: "take-cmp-video", playsinline: true, preload: "auto", muted: side === "right",
      ontimeupdate: () => (side === "left" ? align() : updateTime()),
      onloadedmetadata: refreshRange, onended: () => { pauseAll(); updateTime(); }, onplay: onPlay, onpause: onPause });
    const label = el("div", { class: "take-cmp-label" });
    const note = el("div", { class: "take-cmp-note muted sm" });
    const adoptBtn = el("button", { type: "button", class: "akey sm take-cmp-adopt", disabled: disabled || undefined, onclick: () => {
      if (disabled || !col.item) return;
      onAdopt?.(col.item.id);
      close();
    } }, "採用這版");
    const col = { side, item: null, select, video, label, note, adoptBtn };
    function apply(item) {
      col.item = item || null;
      col.label.textContent = item?.label || "這版沒有標籤";
      col.note.textContent = item?.note || "";
      col.adoptBtn.disabled = !!disabled || !item;
      if (item?.src) {
        video.src = item.src;
        video.poster = item.poster || "";
        video.load();
      } else {
        video.pause();
        video.removeAttribute("src");
        video.removeAttribute("poster");
        video.load();
      }
      onPause();
    }
    select.value = String(initial < list.length ? initial : 0);
    cols.push(col);
    apply(list[+select.value]);
    return el("div", { class: `take-cmp-col take-cmp-col-${side}` }, select, video, label, note, adoptBtn);
  }

  const body = el("div", { class: "take-cmp-body" }, column("left", 0), column("right", 1));
  const controls = el("div", { class: "take-cmp-controls" }, playBtn, prevBtn, nextBtn, range, timeText, soundBtn);

  panel.append(
    el("div", { class: "take-cmp-head" }, el("b", { class: "take-cmp-title" }, title), el("span", { class: "grow" }),
      el("button", { type: "button", class: "take-cmp-x", title: "關閉", "aria-label": "關閉", onclick: close }, "×")),
    body, controls);
  mask.append(panel);
  applySound();
  document.addEventListener("keydown", onKey);
  document.body.append(mask);
  return mask;
}

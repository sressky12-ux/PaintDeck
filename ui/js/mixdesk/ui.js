// 混音台共用的小畫面元件：右鍵選單、表單對話框、確認框、拖曳、狀態列。沒有狀態，各分頁直接 import。
const $ = (s) => document.querySelector(s);

export const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};

/** 拖曳：移動超過 3px 才算拖，否則當成點一下 */
export function drag(ev, { move, end, click }) {
  const x0 = ev.clientX, y0 = ev.clientY;
  let moved = false;
  const mv = (e) => {
    const dx = e.clientX - x0, dy = e.clientY - y0;
    if (!moved && Math.hypot(dx, dy) < 3) return;
    moved = true;
    if (move) move(dx, dy, e);
  };
  const up = (e) => {
    removeEventListener("mousemove", mv);
    removeEventListener("mouseup", up);
    if (moved) { if (end) end(e.clientX - x0, e.clientY - y0, e); } else if (click) click(e);
  };
  addEventListener("mousemove", mv);
  addEventListener("mouseup", up);
}

/** 右鍵選單：items＝[{label, fn, off}]，點外面或按 Esc 關掉 */
export function menu(x, y, items) {
  const m = $("#mdMenu");
  m.replaceChildren();
  for (const it of items.filter(Boolean)) {
    const b = el("button", "", it.label);
    b.type = "button";
    b.disabled = !!it.off;
    b.onclick = () => { close(); it.fn(); };
    m.append(b);
  }
  m.hidden = false;
  m.style.left = Math.min(x, innerWidth - m.offsetWidth - 6) + "px";
  m.style.top = Math.min(y, innerHeight - m.offsetHeight - 6) + "px";
  const close = () => { m.hidden = true; removeEventListener("mousedown", out, true); removeEventListener("keydown", esc, true); };
  const out = (e) => { if (!m.contains(e.target)) close(); };
  const esc = (e) => { if (e.key === "Escape") close(); };
  setTimeout(() => { addEventListener("mousedown", out, true); addEventListener("keydown", esc, true); });
}

/**
 * 表單對話框。fields＝[{key, label, type: text|number|select|textarea, value, options:[[值, 顯示]], step, min, max}]
 * buttons＝[{label, value, acc}]（預設「確定」「取消」）。回傳 {button, values}，取消或 Esc 回 null。
 */
export function ask(title, fields = [], { note = "", buttons = null } = {}) {
  const wrap = $("#mdDialog"), box = $("#mdDialogBox");
  box.replaceChildren(el("h3", "", title));
  if (note) box.append(el("div", "md-note", note));
  const inputs = {};
  for (const f of fields) {
    const lab = el("label", "", f.label);
    let inp;
    if (f.type === "select") {
      inp = el("select");
      for (const [v, t] of f.options) { const o = el("option", "", t); o.value = v; inp.append(o); }
      inp.value = f.value ?? f.options[0]?.[0];
    } else if (f.type === "textarea") {
      inp = el("textarea");
      inp.rows = 3;
      inp.value = f.value ?? "";
    } else {
      inp = el("input");
      inp.type = f.type || "text";
      if (f.step != null) inp.step = f.step;
      if (f.min != null) inp.min = f.min;
      if (f.max != null) inp.max = f.max;
      inp.value = f.value ?? "";
    }
    lab.append(inp);
    box.append(lab);
    inputs[f.key] = [inp, f];
  }
  const row = el("div", "row");
  box.append(row);
  wrap.hidden = false;
  return new Promise((resolve) => {
    const done = (button) => {
      wrap.hidden = true;
      removeEventListener("keydown", key, true);
      if (button == null) return resolve(null);
      const values = {};
      for (const [k, [inp, f]] of Object.entries(inputs)) values[k] = f.type === "number" ? Number(inp.value) : inp.value;
      resolve({ button, values });
    };
    const list = buttons || [{ label: "確定", value: "ok", acc: true }, { label: "取消", value: null }];
    for (const b of list) {
      const btn = el("button", "md-btn" + (b.acc ? " acc" : ""), b.label);
      btn.type = "button";
      btn.onclick = () => done(b.value);
      row.append(btn);
    }
    const key = (e) => {
      if (e.key === "Escape") { e.preventDefault(); done(null); }
      else if (e.key === "Enter" && e.target.tagName !== "TEXTAREA") { e.preventDefault(); done(list[0].value); }
    };
    addEventListener("keydown", key, true);
    const first = Object.values(inputs)[0];
    setTimeout(() => (first ? (first[0].focus(), first[0].select?.()) : row.firstChild?.focus()));
  });
}

export async function confirmBox(title, note, okLabel = "確定", cancelLabel = "取消") {
  const r = await ask(title, [], { note, buttons: [{ label: okLabel, value: "ok", acc: true }, { label: cancelLabel, value: null }] });
  return !!r;
}

/** 狀態列（綠字一行；ms 之後自動收起，0＝一直顯示） */
let msgTimer = 0;
export function say(text, ms = 6000) {
  const m = $("#mdMsg");
  clearTimeout(msgTimer);
  m.textContent = text || "";
  m.hidden = !text;
  if (text && ms) msgTimer = setTimeout(() => { m.hidden = true; }, ms);
}

/** 頂端紅條：mixdesk 的錯誤原文照樣顯示；空字串收起 */
export function showErr(text) {
  const e = $("#mdErr");
  e.textContent = text || "";
  e.hidden = !text;
}

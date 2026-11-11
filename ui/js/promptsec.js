// 提示詞分段（2026-11-11 使用者要的：「分角色、畫風、姿勢等等，當然也要保留完整一塊的正面提示詞欄位」）
// 大框是正本：分區用【畫風】【角色】【姿勢】【鏡頭】這種標題行寫在大框裡；
// 「分段」開關打開時把大框拆成幾個小框，兩邊雙向同步。送去畫圖前 forModel() 會把標題拿掉。
import { el, store } from "./util.js";
import { attachAutocomplete } from "./autocomplete.js";

export const SECTIONS = [
  { key: "畫風", hint: "畫風／品質：masterpiece、畫師、cel shading…" },
  { key: "角色", hint: "角色＋服裝：髮色、眼睛、衣服狀態…" },
  { key: "姿勢", hint: "姿勢＋場景：動作、體位、地點…" },
  { key: "鏡頭", hint: "鏡頭＋光影＋句子：pov、from above、光線、短句…" },
];
const OTHER = "其他";
const HEAD = /^\s*【([^】\n]{1,12})】[ \t]*/;

export const hasSections = (text) => (text || "").split("\n").some((l) => HEAD.test(l));

// 拆成 [{key, text}]；第一個標題之前的字算「其他」
export function parse(text) {
  const out = new Map([[OTHER, []]]);
  let cur = OTHER;
  for (const line of (text || "").split("\n")) {
    const m = line.match(HEAD);
    if (m) {
      cur = m[1].trim();
      if (!out.has(cur)) out.set(cur, []);
      const rest = line.slice(m[0].length);
      if (rest.trim()) out.get(cur).push(rest);
    } else out.get(cur).push(line);
  }
  return [...out].map(([key, lines]) => ({ key, text: lines.join("\n").trim() }));
}

// 組回大框：「其他」放最前面（沒標題），預設四區一定都留標題，自己加的區照原順序接在後面
export function compose(parts) {
  const map = new Map(parts.map((p) => [p.key, p.text.trim()]));
  const keys = [...SECTIONS.map((s) => s.key), ...parts.map((p) => p.key).filter((k) => k !== OTHER && !SECTIONS.some((s) => s.key === k))];
  const chunks = [];
  if (map.get(OTHER)) chunks.push(map.get(OTHER));
  for (const k of keys) chunks.push(`【${k}】` + (map.get(k) ? "\n" + map.get(k) : ""));
  return chunks.join("\n");
}

// 送去畫圖用：拿掉標題；各區之間補逗號（不然上一區最後一個 tag 會跟下一區第一個黏在一起）。沒有標題就原封不動
export function forModel(text) {
  if (!hasSections(text)) return text;
  const chunks = parse(text).map((p) => p.text).filter(Boolean);
  return chunks.map((c, i) => (i < chunks.length - 1 && !/[,，.。!?！？]$/.test(c) ? c + "," : c)).join("\n");
}

// ---------------------------------------------------------------- 介面
export function init({ prompt, wrap, mount, toggle, batch, save }) {
  let on = !!store.get("promptSecOn", false);
  let selfSet = false;
  const boxes = new Map();

  const write = () => {
    selfSet = true;
    prompt.value = compose([...boxes].map(([key, ta]) => ({ key, text: ta.value })));
    selfSet = false;
    save();
  };

  function render() {
    mount.innerHTML = "";
    boxes.clear();
    const parts = parse(prompt.value);
    const known = new Map(parts.map((p) => [p.key, p.text]));
    const rows = [];
    if (known.get(OTHER)) rows.push({ key: OTHER, hint: "還沒分區的內容：剪下來貼到下面的格子，或留著也會一起送出", text: known.get(OTHER) });
    for (const s of SECTIONS) rows.push({ ...s, text: known.get(s.key) || "" });
    for (const p of parts) if (p.key !== OTHER && !SECTIONS.some((s) => s.key === p.key)) rows.push({ key: p.key, hint: "", text: p.text });
    for (const r of rows) {
      const ta = el("textarea", { rows: 2, spellcheck: "false", placeholder: r.hint, "aria-label": "提示詞分段：" + r.key });
      ta.value = r.text;
      ta.addEventListener("input", write);
      const ac = el("div", { class: "ac" });
      mount.append(el("div", { class: "psec-row" + (r.key === OTHER ? " other" : "") },
        el("span", { class: "psec-key" }, r.key),
        el("div", { class: "ac-wrap" }, ta, ac)));
      attachAutocomplete(ta, ac);
      boxes.set(r.key, ta);
    }
  }

  function apply() {
    const blocked = batch.checked;  // 「每行一句」批次跟分段互斥：分段會把一句拆成好幾行
    const show = on && !blocked;
    toggle.classList.toggle("on", show);
    toggle.setAttribute("aria-pressed", show ? "true" : "false");
    toggle.title = blocked ? "「每行一句」批次開著時不能分段" : "大框／小框切換：分成畫風、角色、姿勢、鏡頭幾格寫，大框會同步";
    wrap.hidden = show;
    mount.hidden = !show;
    if (show) {
      if (!hasSections(prompt.value)) write_initial();
      render();
    }
  }

  // 第一次切到小框：大框沒有分區標題時，原本的字放「其他」，四個標題接在後面
  function write_initial() {
    selfSet = true;
    prompt.value = compose([{ key: OTHER, text: prompt.value }]);
    selfSet = false;
    save();
  }

  // 大框被程式改掉（提示詞庫、AI 改寫、最近用過…）時，小框跟著重畫
  const desc = Object.getOwnPropertyDescriptor(prompt, "value") || Object.getOwnPropertyDescriptor(Object.getPrototypeOf(prompt), "value");
  Object.defineProperty(prompt, "value", {
    configurable: true,
    get() { return desc.get.call(this); },
    set(v) { desc.set.call(this, v); if (!selfSet && on && !mount.hidden) render(); },
  });

  toggle.onclick = () => {
    if (batch.checked) return;
    on = !on;
    store.set("promptSecOn", on);
    apply();
  };
  batch.addEventListener("change", apply);
  apply();
}

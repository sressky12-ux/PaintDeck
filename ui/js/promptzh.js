// 提示詞框的中英對照（2026-11-01，使用者轉來的回饋「可以给这个提示词弄一个英汉对比吗？」，補充「最終希望就連自然語言也不放過」）
// tag 查對照表（/api/tags/zh，立刻回）；對照表查不到的句子、片語按「AI 翻句子」交給通用模型（/api/pe/translate，跟 AI 改寫同一顆）。
// 點對照裡的某一段，提示詞框會選到那一段。英文介面不顯示（對照是中文）
// 正向跟負面提示詞框共用對照表快取（dict）跟 AI 翻譯紀錄（ai），切開的狀態（開關、切段結果、翻譯中）每個實例各自一份
import { el, api, store, debounce, toast } from "./util.js";
import { current } from "./i18n.js";

const dict = new Map();   // 對照表：原字串 → 中文；查過沒有的記 ""，不重查（兩個實例共用）
const AI_MAX = 400;
const ai = new Map(store.get("zhAi", []));  // AI 翻過的：英文 → 中文（存 localStorage，留最近 400 筆；兩個實例共用）
let def = null;  // 預設實例（正向提示詞框），舊的 init／refresh 包著它

const norm = (s) => s.trim().split(/\s+/).join(" ");
const CJK = /[㐀-鿿]/;

// 建一個實例：textarea＝提示詞框、panel＝對照面板、toggle＝底列按鈕、storeKey＝開關存 localStorage 的 key
// 回傳的物件上有 refresh()（提示詞變動時叫）跟內部狀態；面板內容變動自己會聽 textarea 的 input
export function create(textarea, panel, toggle, storeKey = "promptZh") {
  const self = { ta: textarea, box: panel, btn: toggle, on: false, units: [], busy: null };
  const refresh = debounce(() => { if (self.on) update(self); }, 250);
  self.refresh = refresh;
  if (current() === "en") { self.btn.hidden = true; }
  else {
    self.on = !!store.get(storeKey, false);
    self.btn.onclick = () => { self.on = !self.on; store.set(storeKey, self.on); show(self); };
    show(self);
  }
  textarea.addEventListener("input", refresh);
  return self;
}

// 舊接法：正向提示詞框當預設實例（promptx.js 會叫 refresh()）
export function init(textarea, panel, toggle) {
  def = create(textarea, panel, toggle, "promptZh");
}
export function refresh() { if (def) def.refresh(); }

function show(self) {
  self.btn.classList.toggle("on", self.on);
  self.btn.setAttribute("aria-pressed", self.on);
  self.box.hidden = !self.on;
  if (self.on) update(self);
}

// ---------------------------------------------------------------- 切段
// 照逗號（括號、大括號裡的不算）和換行切段，記住每段在原文的位置
function pieces(v) {
  const out = [];
  let depth = 0, start = 0, line = 0;
  const push = (end) => {
    const raw = v.slice(start, end), text = raw.trim();
    const a = start + raw.length - raw.trimStart().length;
    if (text) out.push({ a, b: a + text.length, text, line });
  };
  for (let i = 0; i < v.length; i++) {
    const c = v[i];
    if (c === "\\") { i++; continue; }
    if (c === "(" || c === "{") depth++;
    else if ((c === ")" || c === "}") && depth > 0) depth--;
    else if (c === "\n") { push(i); start = i + 1; line++; depth = 0; }
    else if (!depth && (c === "," || c === "，")) { push(i); start = i + 1; }
  }
  push(v.length);
  return out;
}

// 一段 → 要查的字：(tag:1.2) 拆掉權重、{a|b} 拆開、<image1>／@畫師 直接給中文；plain＝一般文字，查不到可能是句子
function parse(t) {
  let m;
  if ((m = /^<image(\d+)>$/.exec(t))) return { fixed: `第 ${m[1]} 張參考圖` };
  if (t.startsWith("@")) return { fixed: `畫師：${t.slice(1).trim()}` };
  if (CJK.test(t)) return { fixed: "" };
  if ((m = /^\(((?:\\.|[^()\\])*?):\s*(-?\d+(?:\.\d+)?)\)$/.exec(t))) return { keys: split(m[1]), sep: "、", w: m[2] };
  // {紅|藍} 或 {red|blue} dress：每個選項代回去各查一次
  if ((m = /\{([^{}]*\|[^{}]*)\}/.exec(t))) {
    const keys = m[1].split("|").map((o) => norm(t.slice(0, m.index) + o + t.slice(m.index + m[0].length))).filter(Boolean);
    return { keys: [...new Set(keys)], sep: "／", rand: true };
  }
  return { keys: [norm(t)], sep: "", plain: true };
}
const split = (s) => s.split(/[,，]/).map(norm).filter(Boolean);

const known = (k) => dict.get(k) || ai.get(k) || "";

// ---------------------------------------------------------------- 對照
async function update(self) {
  const v = self.ta.value, ps = pieces(v);
  const parsed = ps.map((p) => parse(p.text));
  const ask = [...new Set(parsed.flatMap((s) => s.keys || []).filter((k) => !dict.has(k) && !ai.has(k)))].slice(0, 500);
  if (ask.length) {
    try {
      const r = await api("/api/tags/zh", { json: { tags: ask } });
      for (const k of ask) dict.set(k, r.zh[k] || "");
    } catch { /* 查不到就當沒有，句子還能交給 AI */ }
    if (self.ta.value !== v) return;  // 查的途中又改了：等下一次
  }
  self.units = [];
  let run = [];  // 同一行連續查不到的一般文字：併成一句
  const flush = () => {
    if (!run.length) return;
    const a = run[0].a, b = run.at(-1).b, en = norm(v.slice(a, b));
    const sent = run.length > 1 || en.split(" ").length > 3 || /[.!?]/.test(en);
    self.units.push({ kind: sent ? "sent" : "tag", a, b, en: v.slice(a, b), zh: ai.get(en) || null, need: ai.has(en) ? [] : [en] });
    run = [];
  };
  ps.forEach((p, i) => {
    const s = parsed[i];
    if (run.length && p.line !== run[0].line) flush();
    if (s.fixed !== undefined) { flush(); self.units.push({ kind: "tag", a: p.a, b: p.b, en: p.text, zh: s.fixed, need: [] }); return; }
    // 一般文字只要對照表沒有就先收著（AI 翻過的也是，flush 再查），句子翻完才不會變成 tag 的樣子
    if (s.plain && !dict.get(s.keys[0])) { run.push(p); return; }
    const zs = s.keys.map(known);
    flush();
    const zh = zs.some(Boolean) ? zs.map((z, j) => z || s.keys[j]).join(s.sep) + (s.w ? `（權重 ${s.w}）` : "") + (s.rand ? "（每張隨機挑一個）" : "") : null;
    self.units.push({ kind: "tag", a: p.a, b: p.b, en: p.text, zh, need: s.keys.filter((k, j) => !zs[j]) });
  });
  flush();
  render(self);
}

function render(self) {
  const need = [...new Set(self.units.flatMap((u) => u.need))];
  const done = self.units.filter((u) => u.zh !== null).length;
  const head = el("div", { class: "pzh-head" },
    el("span", {}, self.units.length ? `已對照 ${done}／${self.units.length} 段` : "提示詞是空的"),
    self.busy
      ? el("span", { class: "pzh-busy" }, "AI 翻譯中…（第一次要等模型載入）", el("button", { type: "button", class: "pzh-btn", onclick: () => stop(self) }, "停止"))
      : need.length
        ? el("button", { type: "button", class: "pzh-btn", title: "對照表查不到的句子、片語，交給 AI 改寫用的本機模型翻（翻過的會記住）", onclick: () => translate(self) }, `AI 翻句子（${need.length} 段）`)
        : null);
  const body = el("div", { class: "pzh-body" }, self.units.map((u) => {
    const zh = u.zh === null ? el("i", { class: "miss" }, u.kind === "sent" ? "（對照表翻不了，按「AI 翻句子」）" : "？") : u.zh ? el("i", {}, u.zh) : null;
    const pick = () => { self.ta.focus(); self.ta.setSelectionRange(u.a, u.b); };
    return u.kind === "sent"
      ? el("button", { type: "button", class: "pzh-s", title: "點一下：在提示詞框選到這一段", onclick: pick }, el("b", {}, u.en), zh)
      : el("button", { type: "button", class: "pzh-t", title: "點一下：在提示詞框選到這一段", onclick: pick }, el("b", {}, u.en), zh);
  }));
  self.box.replaceChildren(head, body);
}

// ---------------------------------------------------------------- AI 翻句子
async function translate(self) {
  const need = [...new Set(self.units.flatMap((u) => u.need))];
  if (!need.length || self.busy) return;
  const rid = self.busy = `ptr-${Date.now().toString(36)}`;
  render(self);
  try {
    for (const batch of batches(need)) {
      const r = await api("/api/pe/translate", { json: { texts: batch, rid }, timeout: 0 });
      if (self.busy !== rid) return;
      batch.forEach((t, i) => { if (r.zh?.[i]) { ai.delete(t); ai.set(t, r.zh[i]); } });
      store.set("zhAi", [...ai].slice(-AI_MAX));
      await update(self);
    }
    const left = self.units.flatMap((u) => u.need).length;
    if (left) toast(`還有 ${left} 段模型沒翻出來，可以再按一次`, "err");
  } catch (e) {
    if (self.busy === rid) toast(`AI 翻譯失敗：${e.message}`, "err");
  } finally {
    if (self.busy === rid) { self.busy = null; render(self); }
  }
}

async function stop(self) {
  const rid = self.busy;
  self.busy = null;
  render(self);
  try { await api("/api/pe/cancel", { json: { rid } }); } catch { /* 已經結束了 */ }
}

// 一次最多 30 段、3000 字（後端上限 40 段、4000 字）
function batches(list) {
  const out = [];
  let cur = [], n = 0;
  for (const t of list) {
    if (cur.length && (cur.length >= 30 || n + t.length > 3000)) { out.push(cur); cur = []; n = 0; }
    cur.push(t); n += t.length;
  }
  if (cur.length) out.push(cur);
  return out;
}

// 提示詞輔助：權重高亮與 Ctrl+↑/↓ 調權重、長度提示、最近用過的提示詞（我的片段搬到提示詞庫 library.js）
// 權重只有 Anima 有效：ComfyUI 的 Qwen 2.1、MOSS-Image 分詞器都設了 disable_weights，(tag:1.2) 會原封不動當成文字送進模型
import { $, el, toast, store } from "./util.js";
import * as promptzh from "./promptzh.js";

const F = (id) => $("#" + id);
let ctx = null;  // { prompt, negative, family(), save() }
let negZh = null;  // 負面框的中英對照
const MAX_RECENT = 40;

export function init(c) {
  ctx = c;
  for (const ta of [ctx.prompt, ctx.negative]) {
    ta.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
        e.preventDefault();
        e.stopImmediatePropagation();
        adjustWeight(ta, e.key === "ArrowUp" ? 0.05 : -0.05);
      }
    }, true);
  }
  const hl = F("promptHl");
  ctx.prompt.addEventListener("input", refresh);
  ctx.prompt.addEventListener("scroll", () => { hl.scrollTop = ctx.prompt.scrollTop; });
  new ResizeObserver(() => { hl.style.height = ctx.prompt.offsetHeight + "px"; }).observe(ctx.prompt);
  F("recentSel").onchange = () => {
    const v = F("recentSel").value;
    F("recentSel").value = "";
    if (!v) return;
    if (v === "__clear") { if (confirm("清空「最近用過的提示詞」？")) { store.set("recent", []); fillRecent(); } return; }
    const r = store.get("recent", [])[+v];
    if (!r) return;
    ctx.prompt.value = r.prompt;
    ctx.prompt.dispatchEvent(new Event("input", { bubbles: true }));
    ctx.save();
    toast("已換成這段提示詞（Ctrl+Z 可以復原）", "ok");
  };
  fillRecent();
  promptzh.init(ctx.prompt, F("promptZh"), F("promptZhBtn"));
  // 負面提示詞框另開一個實例（開關存在「negZh」）；打字由 create 自己聽 input，程式填值（載入設定）靠下面 refresh
  negZh = promptzh.create(ctx.negative, F("negZh"), F("negZhBtn"), "negZh");
  refresh();
}

// ---------------------------------------------------------------- 高亮與長度
const esc = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]));

export function refresh() {
  if (!ctx) return;
  const v = ctx.prompt.value;
  const qwen = ["qwen21", "moss"].includes(ctx.family());
  // 一次掃過：(文字:權重)、{a|b}、<image1>、@畫師
  const re = /\(((?:\\.|[^()\\])*?):\s*(-?\d+(?:\.\d+)?)\)|\{[^{}]*\|[^{}]*\}|<image\d+>|<image\?>|(?<=^|[,\s])@[^,\n]+/g;
  let html = "", last = 0, weights = 0;
  for (const m of v.matchAll(re)) {
    html += esc(v.slice(last, m.index));
    let cls;
    if (m[2] !== undefined) { weights++; cls = qwen ? "w-bad" : +m[2] >= 1 ? "w-up" : "w-down"; }
    else if (m[0][0] === "{") cls = "w-rand";
    else if (m[0][0] === "<") cls = m[0] === "<image?>" ? "w-bad" : "w-img";
    else cls = "w-artist";
    html += `<mark class="${cls}">${esc(m[0])}</mark>`;
    last = m.index + m[0].length;
  }
  F("promptHl").innerHTML = html + esc(v.slice(last)) + "\n";
  F("promptHl").scrollTop = ctx.prompt.scrollTop;

  // 長度：tag 數、英文字數；中文字另外算
  const tags = v.split(/[,\n]/).map((s) => s.trim()).filter(Boolean).length;
  const words = (v.match(/[A-Za-z0-9_'-]+/g) || []).length;
  const cjk = (v.match(/[㐀-鿿]/g) || []).length;
  const approx = Math.round(words * 1.35 + cjk * 1.1 + (v.match(/[,.()]/g) || []).length * 0.5);
  const parts = [`約 ${approx} token`, `${tags} 段`];
  if (cjk) parts.push(`含 ${cjk} 個中文字`);
  let warn = "";
  if (qwen && weights) warn = "Qwen 2.1／MOSS 不吃權重：(tag:1.2) 會被當成文字，建議拿掉括號，用句子描述想強調的東西";
  else if (!qwen && cjk > 8 && words < 3) warn = "Anima 看英文 tag 最準：中文可以打在框裡找對應的 tag（會跳建議）";
  else if (approx > 600) warn = "提示詞很長：越後面的內容影響越小，重要的放前面";
  F("promptStats").textContent = parts.join("・");
  F("promptWarn").textContent = warn;
  F("promptWarn").hidden = !warn;
  promptzh.refresh();
  negZh?.refresh();
}

// ---------------------------------------------------------------- Ctrl+↑/↓ 調權重
function adjustWeight(ta, d) {
  if (["qwen21", "moss"].includes(ctx.family()) && ta === ctx.prompt) toast("提醒：Qwen 2.1／MOSS 不吃權重，(tag:1.2) 會被當成文字", "err");
  const v = ta.value;
  let a = ta.selectionStart, b = ta.selectionEnd;
  // 游標在 (xxx:1.2) 裡面：整組調
  const around = findWeighted(v, a, b);
  if (around) {
    const w = round(around.w + d);
    const rep = Math.abs(w - 1) < 1e-9 ? around.inner : `(${around.inner}:${w.toFixed(2)})`;
    replace(ta, around.start, around.end, rep, around.inner);
    return;
  }
  if (a === b) {  // 沒選：用游標所在的那一段（逗號之間）
    const s = Math.max(v.lastIndexOf(",", a - 1), v.lastIndexOf("\n", a - 1)) + 1;
    let e = v.slice(a).search(/[,\n]/);
    e = e < 0 ? v.length : a + e;
    a = s; b = e;
  }
  while (a < b && /\s/.test(v[a])) a++;
  while (b > a && /\s/.test(v[b - 1])) b--;
  if (a === b) return;
  const inner = v.slice(a, b);
  replace(ta, a, b, `(${inner}:${round(1 + d).toFixed(2)})`, inner);
}

function findWeighted(v, a, b) {
  const re = /\(((?:\\.|[^()\\])*?):\s*(-?\d+(?:\.\d+)?)\)/g;
  for (const m of v.matchAll(re)) {
    const s = m.index, e = s + m[0].length;
    if (a >= s && b <= e) return { start: s, end: e, inner: m[1], w: +m[2] };
  }
  return null;
}

const round = (x) => Math.round(x * 100) / 100;

function replace(ta, s, e, text, inner) {
  ta.focus();
  ta.setSelectionRange(s, e);
  // execCommand 會進瀏覽器的復原紀錄，Ctrl+Z 可以退回
  if (!document.execCommand("insertText", false, text)) ta.setRangeText(text, s, e, "end");
  const i = s + text.indexOf(inner);
  ta.setSelectionRange(i, i + inner.length);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

// ---------------------------------------------------------------- 最近用過的提示詞
export function remember(prompt, model) {
  prompt = (prompt || "").trim();
  if (!prompt) return;
  const list = store.get("recent", []).filter((r) => r.prompt !== prompt);
  list.unshift({ prompt, model: model || "", at: Date.now() });
  store.set("recent", list.slice(0, MAX_RECENT));
  fillRecent();
}

function fillRecent() {
  const s = F("recentSel"), list = store.get("recent", []);
  s.innerHTML = "";
  s.append(el("option", { value: "" }, `最近用過（${list.length}）`));
  list.forEach((r, i) => {
    const d = new Date(r.at);
    const when = d.toDateString() === new Date().toDateString() ? d.toLocaleTimeString(document.documentElement.lang, { hour: "2-digit", minute: "2-digit" }) : d.toLocaleDateString(document.documentElement.lang);
    s.append(el("option", { value: i, title: r.prompt }, `${when}　${r.prompt.replace(/\s+/g, " ").slice(0, 60)}`));
  });
  if (list.length) s.append(el("option", { value: "__clear" }, "－ 清空這個清單 －"));
}

// 提示詞自動完成：輸入英文找 Danbooru tag，輸入中文找對應的英文 tag
import { el, debounce, escTag, fmtN, store } from "./util.js";

// 逗號：半形 , 、全形 ，（中文輸入法打的）、頓號 、 都算 tag 的分隔
const SEP = /[,，、]/;

// 中文輸入法打出來的全形標點（2026-11-17，玩家回報「手動打 tag 時老是來回切輸入法」）：
// 前一個字是英數或半形符號（在打英文 tag）才換成半形；前一個字是中文（在寫中文句子）就不動。
// 用 execCommand("insertText") 換，Ctrl+Z 一下就回到全形。設定記在 aw_punctFix（預設開），提示詞框下面有開關
const FULL = { "，": ", ", "、": ", ", "：": ":", "（": "(", "）": ")" };
export const punctFixOn = () => store.get("punctFix", true);
export function fixPunct(ta) {
  if (!punctFixOn()) return false;
  const pos = ta.selectionStart;
  if (pos !== ta.selectionEnd || pos < 1) return false;
  const v = ta.value, ch = v[pos - 1];
  let rep = FULL[ch];
  if (!rep) return false;
  const prev = v.slice(0, pos - 1).replace(/[ \t]+$/, "").slice(-1);
  // 「（」開權重括號常常在詞的開頭（逗號、換行後面，或整框第一個字）
  if (!(/[\x21-\x7e]/.test(prev) || (ch === "（" && (!prev || /[\n,]/.test(prev))))) return false;
  if (rep === ", " && /^\s/.test(v.slice(pos))) rep = ",";
  ta.setSelectionRange(pos - 1, pos);
  if (!document.execCommand("insertText", false, rep)) {
    ta.setRangeText(rep, pos - 1, pos, "end");
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  }
  return true;
}

export function attachAutocomplete(ta, box) {
  let items = [], sel = 0, range = null, reqId = 0, composing = false;

  const close = () => { box.classList.remove("on"); items = []; };

  function tokenAtCaret() {
    const pos = ta.selectionStart, v = ta.value;
    let a = 0, b = v.length;
    for (let i = pos - 1; i >= 0; i--) if (SEP.test(v[i])) { a = i + 1; break; }
    for (let i = pos; i < v.length; i++) if (SEP.test(v[i])) { b = i; break; }
    const raw = v.slice(a, b);
    const lead = raw.length - raw.trimStart().length;
    return { a: a + lead, b, text: raw.trim() };
  }

  const query = debounce(async () => {
    if (composing) return;
    const t = tokenAtCaret();
    const cjk = /[㐀-鿿]/.test(t.text);
    if (!t.text || (!cjk && t.text.length < 2) || t.text.length > 40) return close();
    const my = ++reqId;
    let res = [];
    try { res = await (await fetch(`/api/tags?q=${encodeURIComponent(t.text)}&limit=12`)).json(); } catch { return; }
    if (my !== reqId) return;
    range = t;
    items = res;
    sel = 0;
    render();
  }, 120);

  function render() {
    box.innerHTML = "";
    if (!items.length) return close();
    items.forEach((it, i) => {
      box.append(el("div", { class: i === sel ? "sel" : "", onmousedown: (e) => { e.preventDefault(); accept(i); } },
        el("b", {}, it.tag), el("i", {}, (it.alias ? `← ${it.alias.replace(/_/g, " ")} ` : "") + (it.zh || "")), el("small", {}, it.count ? fmtN(it.count) : it.cat)));
    });
    box.classList.add("on");
  }

  function accept(i) {
    const it = items[i];
    if (!it || !range) return;
    const v = ta.value;
    const before = v.slice(0, range.a), after = v.slice(range.b).replace(/^\s*[,，、]?\s*/, "");
    const ins = escTag(it.tag) + ", ";
    ta.value = before + ins + after;
    const caret = before.length + ins.length;
    ta.setSelectionRange(caret, caret);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    close();
  }

  // 輸入法組字中（拼音、注音還沒選字）：不查，也不攔按鍵，否則 Enter／Tab／方向鍵會被選單搶走；選字完成（compositionend）才查
  ta.addEventListener("compositionstart", () => { composing = true; reqId++; close(); });
  ta.addEventListener("compositionend", () => { composing = false; if (!fixPunct(ta)) query(); });
  ta.addEventListener("input", (e) => {
    if (e.isTrusted === false || composing || e.isComposing) return;
    if (/^insert/.test(e.inputType || "") && fixPunct(ta)) return;
    query();
  });
  ta.addEventListener("keydown", (e) => {
    if (composing || e.isComposing || e.keyCode === 229 || e.key === "Process") return;
    if (!box.classList.contains("on")) return;
    if (e.key === "ArrowDown") { e.preventDefault(); sel = (sel + 1) % items.length; render(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); sel = (sel - 1 + items.length) % items.length; render(); }
    else if ((e.key === "Enter" && !e.ctrlKey) || e.key === "Tab") { e.preventDefault(); accept(sel); }
    else if (e.key === "Escape") close();
  });
  ta.addEventListener("blur", () => setTimeout(close, 150));
}

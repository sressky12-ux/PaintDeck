// 生成頁的「鎖定角色」標籤：資產庫鎖定的角色設定表不插進提示詞框（AI 改寫碰不到），送出時才原封不動接在最前面。
// 標籤 = { id, name, zh, en, tags, refs }；refs 是這個角色放進多圖參考的檔名，送出時照目前的順序換成 <imageN>（使用者拖曳、刪圖都跟得上）。
import { el } from "./util.js";

/** 一個標籤在這個模型下要接的文字：Anima 用 tag；Qwen 2.1／MiniMax H3 Image 寫名字＋第幾張圖；其他用英文 */
export function chipText(c, fam, refNames = []) {
  if (fam === "anima") return c.tags || c.en || "";
  if (!c.en) return "";
  if (fam === "qwen21" || fam === "mmimage") {
    const fmt = (i) => (fam === "mmimage" ? `<Picture ${i + 1}>` : `<image${i + 1}>`);
    const tags = (c.refs || []).map((n) => refNames.indexOf(n)).filter((i) => i >= 0).map(fmt).join(fam === "mmimage" ? " " : "");
    return fam === "mmimage"
      ? `${c.name}${tags ? ` (${tags})` : ""}: ${c.en}`
      : `${c.name}${tags ? `（${tags}）` : ""}：${c.en}`;
  }
  return `${c.name}: ${c.en}`;
}

/** 所有標籤合起來的開頭（Anima 用逗號接，其他一個角色一行） */
export function lockedPrefix(list, fam, refNames = []) {
  const parts = (list || []).map((c) => chipText(c, fam, refNames)).filter(Boolean);
  return parts.join(fam === "anima" ? ", " : "\n");
}

/** 接在提示詞最前面；已經接過（例如從圖庫套回參數）就不重複接 */
export function withLocked(prompt, list, fam, refNames = []) {
  const head = lockedPrefix(list, fam, refNames);
  const body = (prompt || "").trim();
  if (!head || body.startsWith(head)) return body;
  if (!body) return head;
  return fam === "anima" ? `${head}, ${body.replace(/^,\s*/, "")}` : `${head}\n${body}`;
}

/** 加一個標籤：同一個角色再點一次就換成最新的設定（參考圖也換成這次的），順序不變；
 *  這次沒放圖（例如換成 Anima 時點的）就留著上次記的 */
export function addChip(list, chip) {
  const i = list.findIndex((c) => c.id === chip.id);
  if (i < 0) return [...list, chip];
  const out = list.slice();
  out[i] = { ...chip, refs: chip.refs?.length ? chip.refs : list[i].refs || [] };
  return out;
}

/** 畫標籤列：名字＋✕；滑過去看這個模型下實際會接的全文 */
export function render(box, list, { fam, refNames, onRemove }) {
  box.hidden = !list.length;
  box.replaceChildren(...(list.length ? [el("span", { class: "muted sm" }, "鎖定角色（送出時接在最前面）")] : []),
    ...list.map((c) => el("span", { class: "chip on lk-chip", title: chipText(c, fam, refNames) || "（這個模型下沒有文字）" },
      "🔒 ", c.name,
      el("button", { class: "lk-x", type: "button", "aria-label": `拿掉「${c.name}」`, onclick: () => onRemove(c.id) }, "✕"))));
}

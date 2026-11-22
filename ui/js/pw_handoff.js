// 衣姿工作台「➜ 交給繪匣」的純函式（2026-11-22，設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md 第四節）。
// 不 import 任何東西：posewear.js 收訊息時用，tests\posewear_handoff.test.cjs 直接測。

/** 參考圖上限，跟 refs.js 的 maxRefs() 一樣：MiniMax H3 Image 16 張，Qwen 2.1 10 張 */
export const LIMIT = { qwen21: 10, mmimage: 16 };
const NAME = { qwen21: "Qwen 2.1", mmimage: "MiniMax H3 Image" };
/** 工作台的角色 → 多圖參考的角色（refs.js 的 ROLES）；表情只取神態，當人物 */
const ROLE = { person: "person", outfit: "outfit", pose: "pose", expression: "person" };

/** 決定送進哪個引擎、能不能送、每張的角色、要不要先問換掉現有的參考圖 */
export function planHandoff({ family, hasMmimage, refsNow, items }) {
  const f = family === "qwen21" || family === "mmimage" ? family : hasMmimage ? "mmimage" : "qwen21";
  const max = LIMIT[f], n = items?.length || 0;
  const roles = (items || []).map((x) => ROLE[x.role] || "person");
  const base = { family: f, max, roles, replace: refsNow > 0 };
  if (!n) return { ...base, ok: false, reason: "工作台這一套沒有任何圖" };
  if (n > max) return { ...base, ok: false, reason: `這一套有 ${n} 張，${NAME[f]} 最多 ${max} 張，請在工作台少選幾個姿勢` };
  return { ...base, ok: true };
}

/** 「image 1」「images 2–4」「第 1 张」「第 2–3 張」→ 引擎的寫法（Qwen <image1>；MiniMax <Picture 1>），範圍展開 */
export function renumber(text, engine) {
  const tag = (k) => (engine === "mmimage" ? `<Picture ${k}>` : `<image${k}>`);
  const span = (a, b, sep) => {
    const out = [];
    for (let k = +a; k <= +(b || a); k++) out.push(tag(k));
    return out.join(sep);
  };
  return String(text || "")
    .replace(/\bimages?\s+(\d+)(?:\s*[–-]\s*(\d+))?/gi, (m, a, b) => span(a, b, ", "))
    .replace(/第\s*(\d+)(?:\s*[–-]\s*(\d+))?\s*[张張]/g, (m, a, b) => span(a, b, "、"));
}

/** 填進提示詞框的那一段：MiniMax 用英文；Qwen 用工作台選的語言（pl）；工作台手改過的（custom）優先，但 MiniMax 只收英文的手改版 */
export function handoffPrompt({ en, zh, pl, custom }, engine) {
  const mine = (custom || "").trim();
  const text = engine === "mmimage"
    ? (mine && pl === "en" ? mine : en)
    : mine || (pl === "en" ? en : zh);
  return renumber(text, engine);
}

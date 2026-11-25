// Anima 加速（turbo）LoRA：生成頁「加速」選單和生圖助手「生圖設定」的「加速」共用的名稱、步數
// （後端同一套在 src-tauri/src/server/api_agent_tools.rs 的 turbo_steps／default_turbo）

export const TURBO_OFF = "不使用（30 步、品質優先）";

const isOfficial = (name) => /anima-turbo-lora/i.test(name || "");

/** 選單上的名稱：官方那顆寫版本和 10 步，其他照 LoRA 庫的標題＋版本、12 步 */
export function turboLabel(t) {
  return isOfficial(t.name) ? "官方 Turbo LoRA v0.2（10 步）" : `${t.title || t.name}${t.version ? " " + t.version : ""}（12 步）`;
}

/** 開／關加速要配的步數、CFG（取樣器、排程不動）：官方 10 步、其他 12 步、CFG 1；不使用 30 步 CFG 4 */
export function turboSteps(name) {
  return name ? { steps: isOfficial(name) ? 10 : 12, cfg: 1 } : { steps: 30, cfg: 4 };
}

/** 預設的加速 LoRA：官方的優先，沒有就清單第一顆，都沒有就空白 */
export function defTurbo(list) {
  return ((list || []).find((t) => isOfficial(t.name)) || (list || [])[0])?.name || "";
}

/** 找不到之前選的檔名也要顯示，才能切回「不使用」。 */
export function fillTurbo(sel, list, want) {
  // 助手舊對話的 true 是 graphs.rs 實際會載入的固定檔名。
  if (want === true) want = "anima-turbo-lora-v0.2.safetensors";
  const options = (list || []).map((t) => new Option(turboLabel(t), t.name));
  if (want && !options.some((o) => o.value === want)) options.push(new Option(`（找不到：${want}）`, want));
  sel.replaceChildren(new Option(TURBO_OFF, ""), ...options);
  sel.value = want || "";
}

// ---------------------------------------------------------------- 助手底下膠囊列的「⚡ 加速」開關（2026-11-24 使用者：「直接弄個加速 LoRA 開關不行嗎」）
// MOSS 的加速＝DMD 加速 LoRA 0.35、10 步（跟生成頁「MOSS 快速」同一顆、同一組數字，gen.js 的 MOSS_FAST）
const MOSS_DMD = /(^|[\\/])moss_speed_dmd[^\\/]*\.safetensors$/i;
const MOSS_SPEED = { strength: 0.35, steps: 10, normal: 25 };

/** 這組生圖設定能不能加速、現在開著沒：kind＝anima（模型本身不是 turbo）／moss／空字串（不用加速） */
export function speedState(b) {
  if (b?.family === "anima" && !/turbo/i.test(b.model || "")) return { kind: "anima", on: !!b.turbo_lora };
  if (b?.family === "moss") return { kind: "moss", on: (b.loras || []).some((l) => MOSS_DMD.test(l.name || "")) };
  return { kind: "", on: false };
}

/**
 * 按一下開關要改哪些生圖設定欄位。turbos＝Anima 加速 LoRA 清單、loras＝LoRA 庫、last＝上次用的 Anima 加速 LoRA。
 * 缺檔回 {error}
 */
export function speedToggle(b, { turbos = [], loras = [], last = "" } = {}) {
  const s = speedState(b);
  if (s.kind === "anima") {
    if (s.on) return { turbo_lora: false, ...turboSteps("") };
    const name = turbos.some((t) => t.name === last) ? last : defTurbo(turbos);
    return name ? { turbo_lora: name, ...turboSteps(name) } : { error: "這台電腦還沒有 Anima 加速 LoRA：到下載中心或 LoRA 頁下載" };
  }
  if (s.kind === "moss") {
    const keep = (b.loras || []).filter((l) => !MOSS_DMD.test(l.name || ""));
    if (s.on) return { loras: keep, steps: MOSS_SPEED.normal };
    const f = loras.find((l) => MOSS_DMD.test(l.name || ""));
    return f ? { loras: [...keep, { name: f.name, strength: MOSS_SPEED.strength }], steps: MOSS_SPEED.steps }
      : { error: "還沒下載 MOSS 加速 LoRA：到生成頁勾「MOSS 快速」會帶你下載（約 1.1 GB）" };
  }
  return { error: "這個模型不用掛加速 LoRA" };
}

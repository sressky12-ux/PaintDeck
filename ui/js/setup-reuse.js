// 安裝精靈的選擇規則；有多份資料時不以圖庫大小替使用者決定。
export const cleanPath = value => String(value || "").trim().replace(/^"(.*)"$/, "$1").trim();
export const samePath = (a, b) => cleanPath(a).replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase() ===
  cleanPath(b).replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

export function reuseDefaults(scan) {
  const current = scan.current;
  const legacy = scan.installations?.some(x => x.legacy);
  const existing = !!current || scan.data.length > 0 || legacy;
  const selected = current?.data_root || (scan.data.length === 1 && scan.data[0].valid ? scan.data[0].path : scan.data.length ? "" : legacy ? "manual" : "new");
  const sibling = scan.data.find(d => samePath(d.path, selected))?.comfy;
  const comfy = current?.comfy_dir || sibling || (scan.comfy.length === 1 ? scan.comfy[0].path : scan.comfy.length || existing ? "manual" : "new");
  return { data: selected, comfy };
}

// 第 3 步「只掃指定的資料夾」：加進清單（去掉複製來的引號；空的、重複的不加，回傳原本那份）
export function addScanDir(list, raw) {
  const p = cleanPath(raw);
  return !p || list.some(x => samePath(x, p)) ? list : [...list, p];
}
// 送給 /api/setup/model_scan 的掃描範圍：整台電腦＝不給 roots
export function scanRequest(scope, dirs) {
  return scope === "dirs" ? { roots: dirs.map(cleanPath).filter(Boolean) } : {};
}

export function locationRequest(pick) {
  return {
    data_root: cleanPath(pick.data === "new" ? pick.dataNew : pick.data === "manual" ? pick.dataManual : pick.data),
    data_mode: pick.data === "new" ? "new" : "reuse",
    comfy_dir: cleanPath(pick.comfy === "new" ? pick.comfyNew : pick.comfy === "manual" ? pick.comfyManual : pick.comfy),
  };
}

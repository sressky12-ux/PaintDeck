// 共用小工具
export const $ = (s, root = document) => root.querySelector(s);
export const $$ = (s, root = document) => [...root.querySelectorAll(s)];

export function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") e.className = v;
    else if (k === "html") e.innerHTML = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (v !== undefined && v !== null && v !== false) e.setAttribute(k, v === true ? "" : v);
  }
  for (const k of kids.flat()) if (k !== null && k !== undefined) e.append(k.nodeType ? k : document.createTextNode(k));
  return e;
}

// 伺服器卡住時請求不能永遠掛著（按鈕會一直停用）：預設 5 分鐘沒回應就報錯；
// 正常就會比較久的呼叫自己帶 timeout（毫秒，0＝不限）
const API_TIMEOUT = 5 * 60 * 1000;

export async function api(path, opts = {}) {
  const { timeout = API_TIMEOUT, ...o } = opts;
  if (o.json !== undefined) {
    o.method = o.method || "POST";
    o.headers = { ...(o.headers || {}), "Content-Type": "application/json" };
    o.body = JSON.stringify(o.json);
    delete o.json;
  }
  const ac = timeout > 0 && !o.signal ? new AbortController() : null;
  if (ac) o.signal = ac.signal;
  const timer = ac && setTimeout(() => ac.abort(), timeout);
  try {
    const r = await fetch(path, o);
    const diagnosticId = r.headers?.get?.("x-anima-diagnostic-id");
    const unavailable = r.headers?.get?.("x-anima-diagnostic") === "unavailable";
    if ((diagnosticId || unavailable) && typeof CustomEvent !== "undefined") document.dispatchEvent(new CustomEvent("anima-diagnostic",{detail:{id:diagnosticId,failed:!r.ok,unavailable}}));
    let data = null;
    try { data = await r.json(); } catch { /* 非 JSON */ }
    if (ac?.signal.aborted) throw new Error("timeout");
    if (!r.ok) throw Object.assign(new Error((data && data.error) || `HTTP ${r.status}`), { data });
    return data;
  } catch (e) {
    if (ac?.signal.aborted) throw new Error(`伺服器超過 ${Math.ceil(timeout / 1000)} 秒沒有回應`);
    throw e;
  } finally {
    clearTimeout(timer);
  }
}

export function toast(msg, kind = "") {
  const t = el("div", { class: `toast ${kind}` }, msg);
  $("#toasts").append(t);
  // 時間到先播滑出動畫再移除；關掉動態效果時 animationend 不會來，300ms 後直接移除
  setTimeout(() => { t.classList.add("out"); t.addEventListener("animationend", () => t.remove()); setTimeout(() => t.remove(), 300); }, kind === "err" ? 9000 : 3500);
}

export const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

export function openModal(id) { $(id).hidden = false; }
// 關掉浮層時，裡面的影片也停掉（不然聲音會繼續播）
const hideModal = (m) => { m.hidden = true; m.querySelectorAll("video").forEach((v) => v.pause()); };
export function closeModal(id) { hideModal($(id)); }
document.addEventListener("click", (e) => {
  const c = e.target.closest("[data-close]");
  if (c) hideModal(c.closest(".modal"));
  else if (e.target.classList.contains("modal")) hideModal(e.target);
});

export async function uploadFile(file, kind = "image") {
  const fd = new FormData();
  fd.append("image", file, file.name || `${kind}.png`);
  const endpoint = kind === "video" || kind === "audio" ? "/api/video/media/upload" : "/api/upload";
  return api(`${endpoint}?kind=${encodeURIComponent(kind)}`, { method: "POST", body: fd });
}

export function pickFile(accept = "image/*") {
  return new Promise((res) => {
    const inp = $("#filePick");
    inp.value = "";
    inp.accept = accept;
    inp.onchange = () => res(inp.files[0] || null);
    inp.click();
  });
}

// 拖放、選檔共用：on(file) 收到圖片檔
export function makeDrop(zone, on) {
  zone.addEventListener("dragover", (e) => { e.preventDefault(); zone.classList.add("over"); });
  zone.addEventListener("dragleave", () => zone.classList.remove("over"));
  zone.addEventListener("drop", (e) => {
    e.preventDefault(); zone.classList.remove("over");
    const f = [...e.dataTransfer.files].find((x) => x.type.startsWith("image/"));
    if (f) on(f);
  });
}

export const store = {
  get(k, d) { try { const v = localStorage.getItem("aw_" + k); return v === null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem("aw_" + k, JSON.stringify(v)); } catch { /* 無痕模式等 */ } },
};

// 複製文字（127.0.0.1 算安全來源，可以用 clipboard API；失敗就退回舊方法）
export async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return; } catch { /* 退回舊方法 */ }
  const ta = el("textarea", { style: "position:fixed;opacity:0" });
  ta.value = text;
  document.body.append(ta);
  ta.select();
  document.execCommand("copy");
  ta.remove();
}

// 鍵盤事件是不是發生在輸入框裡（快捷鍵要避開）
export const typing = (e) => {
  const t = e.target;
  return t && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName));
};

// 跟 Windows 檔案總管一樣用 1024 進位（8GB 顯卡才會顯示 8.0 GB，不是 8.6）
export const GB = 2 ** 30;
export const fmtBytes = (n) => (n == null ? "—" : n >= 2 ** 40 ? (n / 2 ** 40).toFixed(2) + " TB" : n >= GB ? (n / GB).toFixed(1) + " GB" : (n / 2 ** 20).toFixed(0) + " MB");
export const fmtSecs = (s) => (s == null || !isFinite(s) ? "—" : s < 60 ? `${Math.max(1, Math.round(s))} 秒` : `${Math.floor(s / 60)} 分 ${Math.round(s % 60)} 秒`);

export const escTag =(t) => t.replace(/\\?\(/g, "\\(").replace(/\\?\)/g, "\\)");
export const fmtN = (n) => (n >= 10000 ? (n / 1000).toFixed(0) + "k" : n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n || 0));
export const modeName = { txt2img: "文生圖", img2img: "圖生圖", inpaint: "局部重繪", multiref: "多圖參考", upscale: "放大", strip: "改圖", sheet: "四視圖編輯", imported: "ComfyUI", video: "影片",
  outpaint: "擴圖", diff: "差分", detail: "修細節", rembg: "去背", xy: "對照表" };
/** 圖庫紀錄是影片（mp4）：大圖要用 <video>，圖片才有的功能（圖生圖、局部重繪…）要藏起來 */
export const isVideo = (rec) => /\.(mp4|webm|mkv)$/i.test(rec?.filename || "");
/** 挑種子用的低解析度預覽影片（graphs::build_video 的 video_preview 存成 video_preview_*.mp4） */
/** 只對圖片有用的動作 */
// 只有影片才有的動作（拆幀台）
export const VIDEO_ONLY_ACTS = ["frames"];
export const IMAGE_ONLY_ACTS = ["reuse", "img2img", "inpaint", "multiref", "upscale", "strip", "sheet", "charsheet", "tagger", "video", "lib", "asset", "play"];

// 混音台打後端的小包裝：錯誤一律丟出 Error（訊息是後端回的繁中原文，畫面照原樣顯示）。

async function parse(r) {
  const text = await r.text();
  let v = null;
  try { v = text ? JSON.parse(text) : null; } catch { /* 不是 JSON */ }
  if (!r.ok) throw new Error((v && v.error) || text || `HTTP ${r.status}`);
  return v;
}

export async function get(url) {
  return parse(await fetch(url, { cache: "no-store" }));
}

export async function post(url, body) {
  return parse(await fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body ?? {}) }));
}

export const fileUrl = (path) => "/api/mixdesk/file?path=" + encodeURIComponent(path);
export const q = (path) => encodeURIComponent(path);

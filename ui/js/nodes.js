// 設定頁「工作節點」：把第二張以上的顯示卡另外開一份 ComfyUI、把區網／遠端電腦的 ComfyUI 加進來一起分工。
// 後端 nodes.rs 的 /api/nodes；生成頁的「送到」下拉（gen.js）與任務面板的節點標籤（queue.js）也用這裡的純函式。
import { $, $$, el, api, toast } from "./util.js";

const FAMS = [["anima", "Anima"], ["qwen21", "Qwen 2.1"], ["other", "其他模型"], ["video", "影片"]];

/** 純函式：勾選框資料 [{family, node, checked}] → 家族綁定 {family:[node,…]}。沒勾的家族不出現（＝哪台都行）。 */
export function bindFromChecks(rows) {
  const bind = {};
  for (const { family, node, checked } of rows || []) {
    if (!checked) continue;
    (bind[family] ??= []).push(node);
  }
  return bind;
}

/** 純函式：GET /api/nodes 的 nodes → 生成頁「送到」下拉的選項 [["", "自動分配"], [id, "名稱（排 N 件）" | "名稱（離線）"], …]。
 * 只列 enabled 的節點；在線的少於 2 台回 []（下拉要隱藏，一台不用選）；離線的也列出。 */
export function nodeOptions(nodes) {
  const list = (nodes || []).filter((n) => !!n.enabled);
  if (list.filter((n) => n.online).length < 2) return [];
  return [["", "自動分配"],
    ...list.map((n) => [n.id, `${n.name}（${n.online ? `排 ${n.queue || 0} 件` : "離線"}）`])];
}

/** 純函式：任務在哪個節點（job.params._node，沒有＝主節點）→ 顯示名稱。沒有 _node、是主節點或找不到回 ""（佇列不顯示）。 */
export function jobNodeLabel(job, nodes) {
  const id = job?.params?._node;
  if (!id) return "";
  const n = (nodes || []).find((x) => x.id === id);
  return n && n.id !== "main" ? n.name : "";
}

/** 從畫面讀出 POST /api/nodes 的 body（{nodes, bind, ram_guard}）。token 輸入框空的就不送 token 欄位。 */
export function collectNodes(box) {
  const nodes = [];
  for (const row of $$(".nodes-local", box)) {
    if (!row.querySelector(".nodes-on").checked) continue;
    const gpu = +row.dataset.gpu;
    nodes.push({ kind: "local", name: row.dataset.name || `第 ${gpu} 號卡`, gpu, enabled: true, args: row.querySelector(".nodes-args").value.trim() });
  }
  for (const row of $$(".nodes-remote", box)) {
    const url = row.querySelector(".nodes-url").value.trim();
    // 沒填網址的新列（按了「加一台」又沒填）跳過；改成列要留 name 給後端報錯
    if (!url && !row.dataset.id) continue;
    const e = { kind: "remote", name: row.querySelector(".nodes-name").value.trim() || "遠端 ComfyUI", url, enabled: row.querySelector(".nodes-enabled").checked };
    if (row.dataset.id) e.id = row.dataset.id;
    const tok = row.querySelector(".nodes-token").value.trim();
    if (tok) e.token = tok;
    if (row.dataset.hasToken && row.querySelector(".nodes-clearTok")?.checked) e.clear_token = true;
    nodes.push(e);
  }
  const bind = bindFromChecks($$(".nodes-bind", box).map((c) => ({ family: c.dataset.family, node: c.dataset.node, checked: c.checked })));
  return { nodes, bind, ram_guard: !!$(".nodes-ramguard", box).checked };
}

const statusEl = (n) => n?.online
  ? el("span", { class: "nodes-status" }, el("span", { class: "nodes-dot on" }), `在線・排 ${n.queue || 0} 件`)
  : el("span", { class: "nodes-status" }, el("span", { class: "nodes-dot off" }), "離線");

/** 設定頁「工作節點」整塊：讀 /api/nodes 後畫出顯示卡、遠端 ComfyUI、家族綁定、記憶體防呆與儲存。 */
export async function mountNodes(box) {
  let d;
  try { d = await api("/api/nodes"); }
  catch (e) { box.replaceChildren(el("div", { class: "muted sm" }, "讀不到：" + e.message)); return; }
  const nodes = d.nodes || [];
  const main = nodes.find((n) => n.kind === "main");
  const locals = nodes.filter((n) => n.kind === "local");
  const remotes = nodes.filter((n) => n.kind === "remote");

  // 測試一台遠端：有貼新的 token 用新的測，沒有用存好的（id）
  const testRemote = async (row, btn) => {
    const body = { url: row.querySelector(".nodes-url").value.trim() };
    if (!body.url) return toast("先填網址再測試", "err");
    const tok = row.querySelector(".nodes-token").value.trim();
    if (tok) body.token = tok;
    else if (row.dataset.id) body.id = row.dataset.id;
    btn.disabled = true;
    try {
      const r = await api("/api/nodes/test", { json: body, timeout: 30000 });
      toast(`連上了：${r.gpu ?? "—"}，顯存 ${r.vram_gb} GB，ComfyUI ${r.comfy ?? "—"}`, "ok");
    } catch (e) { toast(e.message, "err"); }
    finally { btn.disabled = false; }
  };

  // 遠端 ComfyUI 一列：名稱、網址、token、清掉 token、測試、啟用、刪除、在線狀態
  const remoteBox = el("div", { class: "nodes-list" });
  const addRemote = (r = {}) => {
    const row = el("div", { class: "nodes-row nodes-remote" });
    if (r.id) row.dataset.id = r.id;
    if (r.has_token) row.dataset.hasToken = "1";
    row.append(
      el("span", { class: "nodes-f" }, el("span", { class: "sub" }, "名稱"), el("input", { type: "text", class: "nodes-name", placeholder: "例如 書房", value: r.name || "", spellcheck: "false" })),
      el("span", { class: "nodes-f nodes-f-url" }, el("span", { class: "sub" }, "網址"), el("input", { type: "text", class: "nodes-url", placeholder: "http://192.168.1.5:8188", value: r.url || "", spellcheck: "false" })),
      el("span", { class: "nodes-f" }, el("span", { class: "sub" }, "連線 token"), el("input", { type: "password", class: "nodes-token", placeholder: r.has_token ? "已存（留空＝不改）" : "沒有就留空", autocomplete: "new-password" })));
    if (r.has_token) row.append(el("label", { class: "check inline" }, el("input", { type: "checkbox", class: "nodes-clearTok" }), "清掉 token"));
    row.append(el("label", { class: "check inline" }, el("input", { type: "checkbox", class: "nodes-enabled", checked: r.enabled !== false }), "啟用"),
      el("button", { class: "btn sm", type: "button", onclick: (e) => testRemote(row, e.target) }, "測試"),
      el("button", { class: "btn sm danger", type: "button", title: "只從畫面拿掉，按「儲存」才真的刪掉", onclick: () => row.remove() }, "刪除"),
      statusEl(r));
    remoteBox.append(row);
  };
  remotes.forEach(addRemote);

  // 本機顯示卡：每張卡一列；index 0 是主節點自己在用，不能勾
  const gpus = d.gpus || [];
  const localRows = [];
  if (gpus.length <= 1) localRows.push(el("div", { class: "muted sm" }, "這台電腦只找到一張 NVIDIA 顯示卡"));
  else for (const g of gpus) {
    const node = locals.find((n) => n.gpu === g.index);
    // index 0 是主節點在用，不掛 nodes-local（collectNodes 只掃有勾選框的列）
    const row = el("div", { class: g.index === 0 ? "nodes-row nodes-mainrow" : "nodes-row nodes-local", "data-gpu": g.index, "data-name": node?.name || g.name },
      el("span", { class: "nodes-gpu" }, g.name),
      el("span", { class: "muted sm" }, `顯存 ${(g.vram_mb / 1024).toFixed(1)} GB`));
    if (g.index === 0) row.append(el("span", { class: "nodes-status" }, "主節點用這張"));
    else {
      row.append(el("label", { class: "check inline" }, el("input", { type: "checkbox", class: "nodes-on", checked: !!node?.enabled }), "用這張卡另外開一個 ComfyUI"),
        el("input", { type: "text", class: "nodes-args", placeholder: "例如 --lowvram", value: node?.args || "", spellcheck: "false" }));
      if (node) row.append(statusEl(node));
    }
    localRows.push(row);
  }
  // 設定裡有、但這次偵測不到的卡（驅動出狀況、偵測失敗）：照樣列出來，不然一按儲存就被洗掉
  for (const node of locals.filter((n) => !gpus.some((g) => g.index === n.gpu))) {
    localRows.push(el("div", { class: "nodes-row nodes-local", "data-gpu": node.gpu, "data-name": node.name },
      el("span", { class: "nodes-gpu" }, `第 ${node.gpu} 號卡`),
      el("span", { class: "muted sm" }, "現在偵測不到這張卡"),
      el("label", { class: "check inline" }, el("input", { type: "checkbox", class: "nodes-on", checked: !!node.enabled }), "用這張卡另外開一個 ComfyUI"),
      el("input", { type: "text", class: "nodes-args", placeholder: "例如 --lowvram", value: node.args || "", spellcheck: "false" }),
      statusEl(node)));
  }

  // 家族綁定：每個家族一列、每個節點（含主節點）一個勾選框
  const targets = [main, ...locals, ...remotes].filter(Boolean);
  const bindRows = [];
  for (const [fam, famName] of FAMS) {
    const checked = new Set((d.bind || {})[fam] || []);
    bindRows.push(el("div", { class: "nodes-row" },
      el("span", { class: "nodes-bindf" }, famName),
      ...targets.map((n) => el("label", { class: "check inline nodes-bindcell" },
        el("input", { type: "checkbox", class: "nodes-bind", "data-family": fam, "data-node": n.id, checked: checked.has(n.id) }), n.name))));
  }

  const restartBox = el("div", { class: "warnbox sm", hidden: true },
    "本機顯示卡的設定要重開 ComfyUI 才會生效。",
    ...(d.can_restart ? [el("button", { class: "akey sm", type: "button", onclick: async () => {
      try { await api("/api/models/restart_comfy", { json: {} }); toast("ComfyUI 重開中，大約半分鐘後就照新設定開機", "ok"); }
      catch (e) { toast(e.message, "err"); }
    } }, "現在重開 ComfyUI")] : []));

  box.replaceChildren(
    el("div", { class: "muted sm" }, "一台 ComfyUI 一次只畫一張。有兩張以上的 N 卡、或區網／租的電腦也開著 ComfyUI，可以加進來一起分工：每張圖會自動送到比較閒的那台。"),
    el("div", { class: "nodes-sec" }, el("span", { class: "sub" }, "本機顯示卡"), ...localRows),
    el("div", { class: "nodes-sec" }, el("span", { class: "sub" }, "遠端 ComfyUI"),
      remoteBox,
      el("div", {}, el("button", { class: "akey sm", type: "button", onclick: () => addRemote() }, "＋加一台遠端 ComfyUI"))),
    el("div", { class: "nodes-sec" }, el("span", { class: "sub" }, "家族綁定"), ...bindRows,
      el("div", { class: "muted sm" }, "勾了就只送到勾的那幾台（例如大模型只給顯存大的卡）。不勾＝自動。")),
    el("label", { class: "check" }, el("input", { type: "checkbox", class: "nodes-ramguard", checked: !!d.ram_guard }),
      `電腦記憶體不到 ${d.ram_guard_gb} GB 時，Qwen 2.1 和影片一次只跑一件（避免記憶體爆掉）`),
    el("div", { class: "muted sm" }, `這台電腦：${d.ram_gb} GB`),
    el("div", { class: "row gap wrap" }, el("button", { class: "akey sm", type: "button", onclick: async (e) => {
      const btn = e.target;
      btn.disabled = true;
      try {
        const r = await api("/api/nodes", { json: collectNodes(box) });
        toast("存好了", "ok");
        if (r.restart_needed) restartBox.hidden = false;
        else mountNodes(box);  // 存完重新讀（新的節點 id、遠端 token 有沒有存好）
      } catch (e) { toast(e.message, "err"); }
      finally { btn.disabled = false; }
    } }, "儲存")),
    restartBox);
}

/** 生成頁「送到」下拉：讀 /api/nodes 填選項；在線節點少於 2 台（或讀失敗）時整個隱藏。保留原本選的值。 */
export async function refreshGenNode() {
  const wrap = $("#genNodeWrap"), sel = $("#genNode");
  if (!wrap || !sel) return;
  try {
    const d = await api("/api/nodes");
    const opts = nodeOptions(d.nodes);
    const cur = sel.value;
    sel.replaceChildren(...opts.map(([v, label]) => el("option", { value: v }, label)));
    if (opts.length) {
      wrap.hidden = false;
      if (opts.some(([v]) => v === cur)) sel.value = cur;
    } else wrap.hidden = true;
  } catch { wrap.hidden = true; }  // 讀不到（伺服器重開中等）：靜靜收起來，不吵生成頁
}

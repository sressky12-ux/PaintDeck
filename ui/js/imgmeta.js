// 生成頁「讀圖片參數…」（2026-10-29）：拖一張圖進來 → /api/image/meta 讀出生成參數（生圖站、ComfyUI、WebUI／C 站、NovelAI…）→
// 摘要（模型、LoRA 標出這台電腦有沒有）→ 套用到生成頁；圖裡有 ComfyUI 工作流時可以直接匯入成工作流。
import { $, el, toast, openModal, closeModal, makeDrop, pickFile, copyText } from "./util.js";
import { app } from "./state.js";

let opts = { apply: () => {}, importWf: () => {} };
let cur = null; // { file, url, r }

const SOURCE = {
  anima: "生圖站（繪匣）",
  comfyui: "ComfyUI",
  webui: "WebUI（A1111／Forge／C 站）",
  novelai: "NovelAI",
  other: "其他軟體（SwarmUI、Fooocus…）",
};

/** opts.apply(params)：套用到生成頁；opts.importWf(file)：匯入成工作流 */
export function init(o) {
  opts = { ...opts, ...o };
  makeDrop($("#imDrop"), read);
  $("#imPick").onclick = async () => { const f = await pickFile("image/png,image/webp,image/jpeg"); if (f) read(f); };
  document.addEventListener("paste", (e) => {
    if ($("#imModal").hidden) return;
    const f = [...(e.clipboardData?.files || [])].find((x) => x.type.startsWith("image/"));
    if (!f) return;
    e.preventDefault();
    read(f);
  });
  $("#imApply").onclick = apply;
  $("#imWf").onclick = () => { if (cur?.file) { closeModal("#imModal"); opts.importWf(cur.file); } };
  $("#imCopy").onclick = () => { const t = cur?.r?.params?.prompt; if (t) copyText(t).then(() => toast("已複製提示詞", "ok")); };
}

/** 打開視窗；有給檔案就直接讀 */
export function open(file) {
  openModal("#imModal");
  if (file) read(file);
}

/** 這台電腦有沒有：先比完整名稱，再比去掉資料夾、副檔名後的名字（WebUI 只寫檔名、沒有副檔名） */
const stem = (s) => String(s).replace(/\\/g, "/").split("/").pop().replace(/\.(safetensors|ckpt|pt|pth|bin|gguf)$/i, "").toLowerCase();
function findIn(names, want) {
  if (!want) return null;
  return names.find((n) => n === want) || names.find((n) => stem(n) === stem(want)) || null;
}
const modelNames = () => [...$("#model").options].map((o) => o.value).filter((v) => v && !v.startsWith("wf:"));
const loraNames = () => (app.state?.models?.loras || []).map((l) => l.name);

async function read(file) {
  if (cur?.url) URL.revokeObjectURL(cur.url);
  cur = { file, url: URL.createObjectURL(file), r: null };
  $("#imImg").src = cur.url;
  $("#imDrop").classList.add("has");
  $("#imOut").replaceChildren(el("div", { class: "muted sm" }, "讀取中…"));
  buttons();
  const fd = new FormData();
  fd.append("file", file, file.name);
  try {
    const res = await fetch("/api/image/meta", { method: "POST", body: fd });
    const r = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(r.error || `讀取失敗（${res.status}）`);
    cur.r = r;
    render(r);
  } catch (e) {
    $("#imOut").replaceChildren(el("div", { class: "warnbox sm" }, e.message));
  }
  buttons();
}

function buttons() {
  const r = cur?.r;
  $("#imApply").disabled = !r?.params;
  $("#imCopy").disabled = !r?.params?.prompt;
  $("#imWf").hidden = !r?.workflow;
}

function render(r) {
  const p = r.params, out = [];
  const row = (k, v) => out.push(el("dt", {}, k), el("dd", {}, v));
  if (r.source) row("來源", SOURCE[r.source] || r.source);
  if (p) {
    if (p.prompt) row("提示詞", el("pre", {}, p.prompt));
    if (p.negative) row("負面", el("pre", {}, p.negative));
    const nums = [
      p.steps != null && `步數 ${p.steps}`, p.cfg != null && `CFG ${p.cfg}`,
      p.sampler && `取樣器 ${p.sampler}`, p.scheduler && `排程 ${p.scheduler}`,
      p.seed != null && `種子 ${p.seed}`, p.width && p.height && `${p.width}×${p.height}`,
    ].filter(Boolean);
    if (nums.length) row("取樣", nums.join("・"));
    if (p.model) {
      const have = findIn(modelNames(), p.model);
      row("模型", el("span", {}, p.model, " ", have ? el("b", { class: "ok" }, "✓ 這台有") : el("b", { class: "err" }, "✗ 這台沒有（套用時維持目前的模型）")));
    }
    const ls = p.loras || [];
    if (ls.length) {
      const names = loraNames();
      row("LoRA", el("div", {}, ...ls.map((l) => {
        const have = findIn(names, l.name);
        return el("div", {}, `${l.name}（${l.strength ?? 1}）`, " ", have ? el("b", { class: "ok" }, "✓") : el("b", { class: "err" }, "✗ 這台沒有"));
      })));
    }
  }
  const box = [el("dl", { class: "im-grid" }, ...out)];
  if (r.workflow) box.push(el("div", { class: "muted sm" }, "圖裡有完整的 ComfyUI 工作流：按「匯入成工作流」就能在生成頁的模型選單用它（缺的節點、模型會列出來）。"));
  for (const n of r.notes || []) box.push(el("div", { class: "notice sm" }, n));
  $("#imOut").replaceChildren(...box);
}

/** 模型、LoRA 換成這台電腦上的檔名；沒有的拿掉並提醒 */
function apply() {
  const p = cur?.r?.params;
  if (!p) return;
  const q = { ...p }, miss = [];
  if (q.model) {
    const m = findIn(modelNames(), q.model);
    if (m) q.model = m; else { miss.push(`模型「${q.model}」`); delete q.model; }
  }
  const names = loraNames();
  q.loras = (p.loras || []).flatMap((l) => {
    const n = findIn(names, l.name);
    if (!n) { miss.push(`LoRA「${l.name}」`); return []; }
    return [{ ...l, name: n }];
  });
  const turbo = [...$("#turboLora").options].map((o) => o.value);
  if (typeof q.turbo_lora === "string" && !turbo.includes(q.turbo_lora)) q.turbo_lora = false;
  closeModal("#imModal");
  opts.apply(q);
  if (miss.length) toast(`這台電腦沒有：${miss.join("、")}（可以到「下載」找）`, "err");
}

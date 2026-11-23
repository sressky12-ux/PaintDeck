// 反推提示詞：WD tagger 分析圖片，點 tag 可排除，再取代或加到提示詞；
// 「自動清理」（後端 tagclean.rs）先把雜訊、重複、衝突、跟畫風 LoRA 打架的 tag 標成排除，每個都附原因、點一下可加回；
// 每個 tag 旁邊有中文對照；有掛 LoRA 時觸發詞放在品質詞後面。
// 另外可以請讀圖模型（有通用版就用它，沒有才用 PE i2i Heretic；task=caption）寫一段英文說明（含畫風），接在 tag 後面，也可以順便要中文摘要
// 2026-11-23 玩家：「反推好像没办法自己上传图片了」→ 視窗上方顯示正在推哪張，「換一張圖…」、拖進視窗、Ctrl+V 貼上都能換成自己的圖
import { $, el, api, toast, openModal, closeModal, debounce, copyText, store, pickFile, uploadFile, makeDrop } from "./util.js";

const RATING = { general: "safe", sensitive: "sensitive", questionable: "nsfw", explicit: "explicit" };
const KIND = [["雜訊", "雜訊"], ["寫實", "寫實／畫風類"], ["衝突", "衝突"], ["重複", "重複"]];  // 原因的開頭 → 摘要名稱（「髮色衝突」也算衝突）
let src = null, res = null, off = new Set(), cb = null, triggers = () => [];
let qwen = false;  // 現在選的是 Qwen 2.1 模型：說明改寫成官方格式的長段落，套用時不加 Danbooru tag
let inputName = null, capBusy = null;  // 寫說明用的輸入圖檔名（圖庫的圖要先複製到 input）、進行中的請求
let runSeq = 0;  // 換圖、調門檻時，晚回來的舊結果丟掉

// getTriggers：回傳目前掛著（有啟用）的 LoRA 觸發詞；有的話自動清理會拿掉寫實類 tag、套用時把觸發詞放前面
export function open(source, onApply, family = "anima", getTriggers = () => []) {
  cb = onApply;
  qwen = family === "qwen21" || family === "moss";
  triggers = getTriggers;
  $("#tgClean").checked = store.get("tg_clean", true);
  $("#tgZh").checked = store.get("tg_zh", true);
  $("#tgCapZh").checked = store.get("tg_cap_zh", true);
  $("#tgCapGo").textContent = qwen ? "AI 寫英文段落描述（Qwen 2.1／MOSS）" : "AI 寫英文說明（含畫風）";
  $("#tgCapUseLabel").textContent = qwen ? "套用時只用這段描述（Qwen／MOSS 不需要 tag）" : "套用時接在 tag 後面";
  openModal("#taggerModal");
  use(source);
}

/** 換成這張圖重新反推：source＝{name}（ComfyUI input 裡的檔）或 {id}（圖庫的圖） */
function use(source) {
  if (capBusy) cancelCaption();
  src = source; off = new Set(); res = null; inputName = source.name || null;
  $("#tgImg").src = source.name ? `/input/${encodeURIComponent(source.name)}` : `/thumb/${source.id}`;
  $("#tgCleanInfo").textContent = "";
  $("#tgCapZhText").textContent = "";
  $("#tgCapZhText").hidden = true;
  $("#tgResult").textContent = "分析中…（第一次要載入模型，約 10～20 秒）";
  $("#tgCap").value = "";
  for (const id of ["tgCap", "tgCapHint", "tgCapUseRow"]) $("#" + id).hidden = true;
  $("#tgCapGo").hidden = false; $("#tgCapCancel").hidden = true;
  capStatus("");
  run();
}

// 自己的圖：上傳到 ComfyUI 的 input 再推
async function useFile(f) {
  if (!f || !f.type.startsWith("image/")) return toast("這不是圖片", "err");
  $("#tgResult").textContent = "上傳中…";
  try { use({ name: (await uploadFile(f)).name }); } catch (e) { $("#tgResult").textContent = e.message; toast(e.message, "err"); }
}

async function run() {
  const my = ++runSeq;
  try {
    const r = await api("/api/tagger", { json: { ...src, general: +$("#tgThr").value, style_lora: triggers().length > 0 } });
    if (my !== runSeq) return;
    res = r;
    resetOff();
    render();
  } catch (e) {
    if (my === runSeq) $("#tgResult").textContent = e.message;
  }
}

// 自動清理開著：先把後端建議拿掉的 tag 設成排除（使用者再點可以加回）
function resetOff() {
  off = new Set($("#tgClean").checked ? Object.keys(res.notes || {}) : []);
}

// 自動清理開著用後端的建議順序（人數在前、建議拿掉的在後），關著照分數
function tagsInOrder() {
  if (!res) return [];
  const prob = Object.fromEntries([...res.character, ...res.general]);
  const order = $("#tgClean").checked && res.order ? res.order : Object.keys(prob);
  return order.map((t) => [t, prob[t]]);
}

function render() {
  const box = $("#tgResult");
  const clean = $("#tgClean").checked, zhOn = $("#tgZh").checked, notes = res.notes || {};
  box.innerHTML = "";
  box.append(el("span", { class: "chip on" }, "評級：" + RATING[res.rating]));
  for (const [t, p] of tagsInOrder()) {
    const why = clean ? notes[t] : null, zh = zhOn ? res.zh?.[t] : null;
    const c = el("span", { class: "chip" + (off.has(t) ? " off" : "") + (why ? " auto" : ""),
      title: (why ? "自動清理：" + why + "\n" : "") + (off.has(t) ? "點一下加回" : "點一下排除"),
      onclick: () => { off.has(t) ? off.delete(t) : off.add(t); render(); } },
      t, zh ? el("span", { class: "zh" }, zh) : null, el("small", {}, Math.round(p * 100) + "%"));
    box.append(c);
  }
  const info = $("#tgCleanInfo");
  if (!clean) { info.textContent = ""; return; }
  const reasons = Object.values(notes), trig = triggers();
  const parts = KIND.map(([k, name]) => [name, reasons.filter((w) => w.startsWith(k) || (k === "衝突" && /^\S+衝突/.test(w))).length])
    .filter(([, n]) => n).map(([name, n]) => `${name} ${n}`);
  info.textContent = (reasons.length ? `自動清理拿掉 ${reasons.length} 個（${parts.join("、")}）：虛線框的滑過去看原因，點一下可以加回。` : "自動清理：沒有要拿掉的。")
    + (trig.length ? `觸發詞 ${trig.join(", ")} 會放在品質詞後面。` : "沒有掛 LoRA，寫實類 tag 先留著。");
}

// tag＋（有勾的話）英文說明：Anima 的格式就是 tag 串後面接句子
function caption() {
  const c = $("#tgCap").value.trim();
  return c && $("#tgCapUse").checked && !$("#tgCap").hidden && !$("#tgCapUseRow").hidden ? c : "";  // 寫失敗、取消留下的半截不算
}

function text() {
  if (qwen && caption()) return caption();
  const tags = tagsInOrder().map(([t]) => t).filter((t) => !off.has(t));
  const trig = $("#tgClean").checked ? triggers().filter((w) => !tags.includes(w)) : [];
  const head = res ? ["masterpiece, best quality", RATING[res.rating], ...trig, ...tags].join(", ") : "";
  const c = caption();
  return head && c ? `${head}. ${c}` : head || c;
}

// ---------------------------------------------------------------- AI 寫說明（SSE 串流，跟 AI 改寫同一個後端）
function capStatus(msg, kind = "") {
  $("#tgCapStatus").textContent = msg;
  $("#tgCapStatus").className = "pe-status " + kind;
}

async function writeCaption() {
  if (capBusy) return;
  const ta = $("#tgCap");
  try {
    if (!inputName) inputName = (await api("/api/to_input", { json: { id: src.id } })).name;  // 圖庫的圖：先複製到 ComfyUI\input
  } catch (e) { return capStatus("讀不到這張圖：" + e.message, "err"); }
  const ctrl = new AbortController(), rid = Math.random().toString(36).slice(2);
  capBusy = { ctrl, rid };
  $("#tgCapGo").hidden = true; $("#tgCapCancel").hidden = false;
  ta.hidden = false; ta.readOnly = true; ta.value = "";
  $("#tgCapUseRow").hidden = true;  // 這次寫完才再打開；失敗的半截不會被套用
  capStatus("準備中…", "run");
  const t0 = performance.now();
  let result = null, failed = null;
  try {
    const r = await fetch("/api/pe/rewrite", { method: "POST", headers: { "Content-Type": "application/json" }, signal: ctrl.signal,
      body: JSON.stringify({ task: qwen ? "caption_qwen" : "caption", images: [inputName], temperature: 0.6, seed: 1, think: false, zh: $("#tgCapZh").checked,
        tags: tagsInOrder().map(([t]) => t).filter((t) => !off.has(t)), rid }) });  // 留著的 tag 當提示：讀圖模型自己看常漏掉裸露和體位
    if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`);
    const rd = r.body.getReader(), dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await rd.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let k;
      while ((k = buf.indexOf("\n\n")) >= 0) {
        const block = buf.slice(0, k); buf = buf.slice(k + 2);
        const ev = /^event: (.*)$/m.exec(block)?.[1], data = JSON.parse(/^data: (.*)$/m.exec(block)?.[1] || "{}");
        if (ev === "status") capStatus(data.msg.replace("改寫", "寫說明"), "run");
        else if (ev === "content") { ta.value += data.text; ta.scrollTop = ta.scrollHeight; }
        else if (ev === "reset") ta.value = "";
        else if (ev === "result") result = data;
        else if (ev === "error") failed = data.message;
        else if (ev === "cancelled") failed = "已取消";
      }
    }
  } catch (e) {
    failed = e.name === "AbortError" ? "已取消" : e.message;
  } finally {
    capBusy = null;
    $("#tgCapGo").hidden = false; $("#tgCapCancel").hidden = true;
    ta.readOnly = false;
  }
  if (!result || result.fallback) {
    capStatus(failed === "已取消" ? "已取消。" : "寫說明失敗：" + (failed || result?.note || "沒有收到結果"), failed === "已取消" ? "" : "err");
    return;
  }
  ta.value = result.prompt;
  $("#tgCapHint").hidden = false; $("#tgCapUseRow").hidden = false;
  $("#tgCapZhText").textContent = result.zh || "";
  $("#tgCapZhText").hidden = !result.zh;
  const secs = ((performance.now() - t0) / 1000).toFixed(1);
  capStatus(result.cached ? "用了快取（這張圖寫過了）。" : `完成：${secs} 秒` + (result.load_seconds ? `（含載入模型 ${result.load_seconds.toFixed(1)} 秒）` : ""), "ok");
}

function cancelCaption() {
  if (!capBusy) return;
  fetch("/api/pe/cancel", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ rid: capBusy.rid }) }).catch(() => {});
  capBusy.ctrl.abort();
}

$("#tgThr").oninput = () => { $("#tgThrOut").textContent = $("#tgThr").value; deb(); };
const deb = debounce(run, 400);
$("#tgClean").onchange = () => { store.set("tg_clean", $("#tgClean").checked); if (res) { resetOff(); render(); } };
$("#tgZh").onchange = () => { store.set("tg_zh", $("#tgZh").checked); if (res) render(); };
$("#tgCapZh").onchange = () => store.set("tg_cap_zh", $("#tgCapZh").checked);
$("#tgCapGo").onclick = writeCaption;
$("#tgCapCancel").onclick = cancelCaption;
$("#tgReplace").onclick = () => { const t = text(); if (t) { cb(t, true); closeModal("#taggerModal"); } };
$("#tgAppend").onclick = () => { const t = text(); if (t) { cb(t.replace(/^masterpiece, best quality, \w+, /, ""), false); closeModal("#taggerModal"); } };
$("#tgCopy").onclick = async () => { const t = text(); if (t) { await copyText(t); toast("已複製", "ok"); } };
// 換一張圖：選檔、拖進視窗、Ctrl+V 貼上（視窗開著時貼上的圖給反推；capture 先接走，生成頁的貼上不會再收到）
$("#tgPick").onclick = async () => { const f = await pickFile(); if (f) useFile(f); };
makeDrop($("#taggerModal .dialog"), useFile);
document.addEventListener("paste", (e) => {
  if ($("#taggerModal").hidden) return;
  const f = [...(e.clipboardData?.files || [])].find((x) => x.type.startsWith("image/"));
  if (!f) return;
  e.preventDefault();
  e.stopPropagation();
  useFile(f);
}, true);

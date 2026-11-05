import { $, el, api, toast, fmtBytes, openModal, closeModal } from "./util.js";

let snapshot = null, scanSeq = 0, dirty = false, working = false, reportId = null, reportBusy = false;
const downloads = new Set();
const statuses = {running:"進行中",success:"完成",error:"失敗",unknown:"結果未確認",cancelled:"已取消"};
const validId = (id) => typeof id === "string" && /^diag-[a-zA-Z0-9-]{1,74}$/.test(id);

function controls() {
  for (const id of ["diagSave","diagClean","diagClear"]) if ($("#"+id)) $("#"+id).disabled = working || !snapshot?.available;
  for (const id of ["diagSuccess","diagError","diagCap"]) if ($("#"+id)) $("#"+id).disabled = working;
}
function notice(id, text) {
  if (!validId(id)) return;
  const box = $("#diagNotice");
  box.hidden = false;
  box.replaceChildren(el("span",{class:"grow"},text),
    el("button",{class:"akey sm",type:"button",onclick:()=>download(id)},"下載診斷包"),
    el("button",{class:"akey sm",type:"button",onclick:()=>report(id)},"結果不符"),
    el("button",{class:"minikey",type:"button","aria-label":"關閉",onclick:()=>{box.hidden=true;}},"×"));
}
export function onEvent(m) {
  if (m.status === "error" || m.state === "error" || m.phase === "error" || m.event?.kind === "error") {
    notice(m.diagnostic_id,"這次操作有錯誤，可下載診斷包協助查修。");
  }
}
export async function scan() {
  const seq = ++scanSeq;
  try {
    const result = await api("/api/diagnostics");
    if (seq !== scanSeq) return;
    snapshot = result;
    if (!dirty) {
      $("#diagSuccess").value = result.policy.success_days;
      $("#diagError").value = result.policy.error_days;
      $("#diagCap").value = result.policy.max_mib;
    }
    $("#diagSummary").textContent = `診斷資料：${fmtBytes(result.bytes)}／${result.policy.max_mib} MiB` + (result.warning ? "・"+result.warning : "");
    const rows = (result.records || []).map(r => {
      const row = el("details",{class:"diag-record"});
      row.append(...[el("summary",{},`${new Date(r.created*1000).toLocaleString(document.documentElement.lang)} · ${statuses[r.status] || r.status} · ${r.route}`),
        el("div",{class:"muted sm"},r.id),
        el("div",{class:"diag-error sm"},r.first_error || "沒有已記錄的錯誤"),
        r.truncated ? el("div",{class:"err sm"},"部分資料因容量限制未保存，請參考包內說明。") : null,
        el("div",{class:"row gap wrap"},
          el("button",{class:"akey sm",type:"button",onclick:()=>download(r.id)},"下載診斷包"),
          el("button",{class:"akey sm",type:"button",onclick:()=>report(r.id)},"結果不符"))].filter(Boolean));
      return row;
    });
    $("#diagRecords").replaceChildren(...(rows.length ? rows : [el("div",{class:"muted sm"},"還沒有診斷紀錄；下一次生成或助手操作會自動記錄。") ]));
  } catch (e) {
    if (seq === scanSeq) $("#diagSummary").textContent = "診斷紀錄讀取失敗："+e.message;
  } finally { if (seq === scanSeq) controls(); }
}
export async function download(id) {
  if (!validId(id) || downloads.has(id)) return;
  downloads.add(id);
  toast("正在整理診斷包…");
  // 在這台電腦上開的（繪匣視窗、本機瀏覽器）：直接存進「下載」資料夾並打開檔案總管。
  // 繪匣視窗裡的 blob 下載常常沒反應（2026-11-05 回報），存不了才退回讓瀏覽器下載
  if (["127.0.0.1","localhost","[::1]"].includes(location.hostname)) {
    try {
      const r = await api(`/api/diagnostics/${encodeURIComponent(id)}/save`,{json:{},timeout:120000});
      toast(`診斷包存好了：${r.path}（已打開資料夾）；分享前請先檢查提示詞與回報內容。`,"ok");
      downloads.delete(id);
      return;
    } catch {}
  }
  const ac = new AbortController(), timer = setTimeout(()=>ac.abort(),120000);
  try {
    const response = await fetch(`/api/diagnostics/${encodeURIComponent(id)}/export`,{signal:ac.signal});
    if (!response.ok) {
      const data = await response.json().catch(()=>null);
      throw new Error(data?.error || `HTTP ${response.status}`);
    }
    if (!(response.headers.get("content-type") || "").startsWith("application/zip")) throw new Error("伺服器沒有回傳診斷 ZIP");
    const blob = await response.blob(), url = URL.createObjectURL(blob);
    const link = el("a",{href:url,download:`paintdeck-${id}.zip`});
    document.body.append(link); link.click(); link.remove();
    setTimeout(()=>URL.revokeObjectURL(url),60000);
    toast("診斷包已交給瀏覽器下載；分享前請先檢查提示詞與回報內容。","ok");
  } catch (e) { toast("診斷包下載失敗："+(e.name === "AbortError" ? "請求逾時" : e.message),"err"); }
  finally { clearTimeout(timer); downloads.delete(id); }
}
export async function downloadForJob(job) {
  try {
    const result = await api(`/api/diagnostics/job/${encodeURIComponent(job)}`);
    if (!result.diagnostic_id) return toast("這個任務沒有診斷紀錄，可能建立於舊版或已過保留期限。","err");
    await download(result.diagnostic_id);
  } catch (e) { toast(e.message,"err"); }
}
function report(id) {
  if (!validId(id) || reportBusy) return;
  reportId = id;
  $("#diagReportId").textContent = id;
  $("#diagExpected").value = ""; $("#diagActual").value = "";
  openModal("#diagReportModal"); $("#diagExpected").focus();
}
async function saveReport() {
  if (reportBusy) return;
  const id = reportId, expected = $("#diagExpected").value.trim(), actual = $("#diagActual").value.trim();
  if (!id || (!expected && !actual)) return toast("請描述原本想要的結果或實際遇到的情況。","err");
  reportBusy = true;
  for (const id of ["diagReportSave","diagExpected","diagActual"]) $("#"+id).disabled = true;
  try {
    await api(`/api/diagnostics/${encodeURIComponent(id)}/report`,{json:{expected,actual}});
    closeModal("#diagReportModal"); reportId = null;
    toast("回報已保存到診斷紀錄，可下載後交給查修的人。","ok"); await scan();
  } catch (e) { toast("回報保存失敗："+e.message,"err"); }
  finally { reportBusy=false; for (const id of ["diagReportSave","diagExpected","diagActual"]) $("#"+id).disabled = false; }
}
async function save() {
  if (working) return;
  const p = {success_days:Number($("#diagSuccess").value),error_days:Number($("#diagError").value),max_mib:Number($("#diagCap").value)};
  if (![p.success_days,p.error_days,p.max_mib].every(Number.isInteger) || p.success_days<1 || p.success_days>30 || p.error_days<1 || p.error_days>30 || p.max_mib<50 || p.max_mib>2048) return toast("保留天數需為 1～30 天，容量需為 50～2048 MiB。","err");
  working = true; controls();
  try { await api("/api/diagnostics/preferences",{json:p}); dirty=false; toast("診斷保留設定已儲存。","ok"); }
  catch (e) { toast("設定儲存失敗："+e.message,"err"); }
  finally { working=false; await scan(); }
}
async function clear(all) {
  if (working) return;
  if (all && !confirm("永久刪除已結束的診斷紀錄？執行中及下載中的紀錄會略過；已下載的 ZIP 需自行刪除。")) return;
  working=true; controls();
  try { const r=await api("/api/diagnostics/clear",{json:{confirm:true,all}}); toast(`診斷清理已釋放 ${fmtBytes(r.freed_bytes)}`+(r.warning ? "・"+r.warning : ""),"ok"); }
  catch (e) {toast("診斷清理失敗："+e.message,"err");}
  finally {working=false;await scan();}
}
export function init() {
  if (!$("#diagScan")) return;
  $("#diagScan").onclick=scan; $("#diagSave").onclick=save;
  $("#diagClean").onclick=()=>clear(false); $("#diagClear").onclick=()=>clear(true);
  $("#diagReportSave").onclick=saveReport;
  for (const id of ["diagSuccess","diagError","diagCap"]) $("#"+id).oninput=()=>{dirty=true;};
  document.addEventListener("anima-diagnostic",e=>{
    if (e.detail.failed) notice(e.detail.id,"這次操作有錯誤，可下載診斷包協助查修。");
    else if (e.detail.unavailable) toast("這次操作的診斷紀錄無法保存，請到設定查看原因。","err");
  });
  controls();
  scan().then(()=>{
    const previous=snapshot?.records?.find(r=>r.status === "unknown" && !r.active);
    if (previous) notice(previous.id,"有尚未確認結果的操作，可下載診斷包協助查修。");
  });
}

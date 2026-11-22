import * as queue from "./queue.js";
import { $, $$, el, modeName, debounce } from "./util.js";

export function fmtSampler(steps, cfg, count) {
  return [steps === "" ? "" : `${steps} 步`, cfg === "" ? "" : `CFG ${cfg}`,
    count === "" ? "" : `×${count}`].filter(Boolean).join(" · ");
}

export function fmtCount(n) { return n > 0 ? String(n) : "—"; }

export function fmtModel(name, max = 18) {
  const base = name.split(/[\\/]/).pop().replace(/\.(safetensors|gguf|ckpt)$/i, "");
  const chars = Array.from(base);
  return chars.length > max ? chars.slice(0, max - 1).join("") + "…" : base;
}

// 生成頁沒在顯示（隨機頁借走提示詞卡）：左區看不到，ENGINE、尺寸膠囊才出現
const offGen = () => !$("#page-gen").classList.contains("on");

export function capsSpec() {
  return { host: $("#capRow"), chips: {
    source: { label: "ENGINE", visible: offGen, summary: () => fmtModel($("#model").selectedOptions[0]?.textContent.trim() || "—") },
    lora: { label: "LORA", summary: () => fmtCount($$("#loraStack .lora-item:not(.off)").length) },
    params: { label: "尺寸", visible: offGen, summary: () => {
      if (!$("#autoSizeRow").hidden && $("#autoSize").checked) return $("#sizeInfo").textContent;
      return $("#width").value && $("#height").value ? `${$("#width").value}×${$("#height").value}` : "—";
    } },
    sampler: { label: "取樣", summary: () => fmtSampler($("#steps").value, $("#cfg").value, $("#count").value) },
    extra: { label: "附加", summary: () => fmtCount($$("#addons details:not([hidden]) > summary input:checked").length) },
    layout: { label: "版面框", summary: () => fmtCount(Number($("#layCount").textContent)), visible: () => !$("#layBox").hidden },
    pe: { label: "AI 改寫", summary: () => "" },
  } };
}

let dock;
export function refreshChips() {
  dock?.refreshCaps();
}

// 成品動作（#viewActions）放哪（2026-11-23 左右兩欄）：右區打開、「這張圖」那塊沒收起 → 放在那塊面板裡分組攤開；
// 右區收起（「任務」鍵）、那塊收起、視窗太窄側欄變細條 → 搬到觀景窗下面排成一排。兩種都一直看得到，不用滑鼠停在圖上
export function placeActs() {
  if (!dock) return;   // 版面第一次排好時 init 還沒跑：init 最後會再叫一次
  const acts = $("#viewActions");
  const inPanel = dock.zoneOpen("right") && !dock.layout().folded.includes("imgacts");
  const host = inPanel ? $('[data-dock="imgacts"]') : $("#page-gen .dock-main .monitor");
  if (host && acts.parentElement !== host) host.append(acts);
  acts.classList.toggle("under", !inPanel);
}

export function init(api) {
  dock = api;
  queue.onJobs(renderTasks);
  // 模式列右邊的「任務」鍵：收起／打開右邊的任務欄（固定版面 A 案）
  const tasksBtn = $("#stageTasksBtn");
  const paintTasksBtn = () => tasksBtn.setAttribute("aria-pressed", String(dock.zoneOpen("right")));
  tasksBtn.onclick = () => { dock.toggleZone("right"); paintTasksBtn(); };
  addEventListener("resize", debounce(paintTasksBtn, 150));
  $("#page-gen").addEventListener("click", (e) => { if (e.target.closest(".dock-rail, .dock-btn")) paintTasksBtn(); });   // 按細條、« » 開關右區時同步
  paintTasksBtn();
  $("#stageJobs").onclick = (e) => {
    e.stopPropagation();
    if ($("#jobsDrawer").hidden) queue.toggle();
  };
  const assets = $("#genAssets"), assetButton = $("#stageAssets");
  const refresh = debounce(() => {
    refreshChips();
    assetButton.setAttribute("aria-expanded", String(!!assets.querySelector('[aria-expanded="true"]')));
  }, 100);
  for (const content of $$("#page-gen .dock-grp, #stageModes")) {
    content.addEventListener("input", refresh);
    content.addEventListener("change", refresh);
  }
  const observer = new MutationObserver(refresh);
  for (const node of $$("#model, #sizeInfo, #sizeVfd, #loraStack, #advBox .knobs, #layCount, #peStatus, #genAssets")) {
    observer.observe(node, { childList: true, characterData: true, subtree: true });
  }
  observer.observe($("#layBox"), { attributes: true, attributeFilter: ["hidden"] });
  $("#stageAddRef").onclick = () => ($("#refBox").hidden ? $("#initPick") : $("#refPick")).click();
  assetButton.onclick = () => assets.querySelector(".adr-head button")?.click();
  const compose = $('[data-dock="compose"]');
  $("#promptZoom").onclick = (e) => {
    e.currentTarget.setAttribute("aria-pressed", String(compose.classList.toggle("expanded")));
  };
  const more = $("#viewMore"), menu = $("#viewMoreMenu");
  function setMenu(show) {
    menu.hidden = !show;
    more.setAttribute("aria-expanded", String(show));
  }
  more.onclick = () => setMenu(menu.hidden);   // 「⋯」現在是藏起來的空殼（按鈕都攤開在「這張圖」），留著不影響
  menu.addEventListener("click", (e) => { if (e.target.closest("[data-act]")) setMenu(false); });
  document.addEventListener("pointerdown", (e) => {
    if (!menu.hidden && !e.target.closest("#viewActions")) setMenu(false);
  });
  document.addEventListener("keydown", (e) => {
    if (!menu.hidden && e.key === "Escape") { setMenu(false); more.focus(); }
  });
  refresh();
  placeActs();
}

export function taskCards(jobs, max = 4) {
  const running = jobs.filter((j) => j.status === "running");
  const queued = jobs.filter((j) => j.status === "queued");
  const finished = jobs.filter((j) => j.status !== "running" && j.status !== "queued")
    .sort((a, b) => (b.finished || b.created || 0) - (a.finished || a.created || 0));
  return [...running, ...queued, ...finished].slice(0, max);
}

export function renderTasks(jobs) {
  const cards = taskCards(jobs).map((job) => {
    const s = job.summary || {};
    const running = job.status === "running";
    const thumb = job.images?.find((img) => !img.deleted);
    const card = el("article", { class: `job ${job.status}`, "data-job": job.id },
      el("div", { class: "job-head" },
        el("span", { class: `badge st-${job.status}` }, queue.STATUS[job.status] || job.status),
        el("span", { class: "muted sm" }, modeName[s.mode] || s.mode || ""),
        el("span", { class: "grow" }),
        running ? el("button", { type: "button", class: "akey sm", onclick: () => queue.cancel(job) }, "停止") : null),
      el("div", { class: "job-prompt" }, s.idea || s.prompt || ""),
      thumb ? el("img", { class: "task-thumb", src: `/thumb/${thumb.id}`, alt: "成品縮圖", loading: "lazy" }) : null,
      running ? el("div", { class: "job-prog" }, el("div", { class: "bar" }, el("div")), el("span", { class: "job-eta sm" })) : null);
    if (running) queue.paintProgress(card, job.id);
    return card;
  });
  const active = jobs.filter((j) => ["running", "queued"].includes(j.status)).length;
  $("#stageQueue").textContent = `QUEUE · ${active}`;
  $("#stageTasksN").textContent = String(active);
  $("#stageTasksN").hidden = !active;
  $("#stageTasks").replaceChildren(...(cards.length ? cards : [el("p", { class: "muted sm" }, "目前沒有任務。")]));
}

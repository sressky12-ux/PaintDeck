// 第一次啟動精靈（也是「安裝與修復」）：檢查電腦 → 資料與 ComfyUI 位置 → 生圖模型 → 安裝。後端是 src-tauri\src\setup.rs
import { $, $$, api as baseApi, store, fmtBytes, debounce } from "./util.js";
import { setIc, appIcon } from "./deck.js";
import * as i18n from "./i18n.js";
import { mountNet } from "./net.js";
import { reuseDefaults, locationRequest, samePath } from "./setup-reuse.js";

// 介面語言：設定頁選過簡中／英文，精靈也照著翻（先翻原始畫面，之後長出來的字由 i18n.js 的監看器接手）
await i18n.init();

const F = (id) => $("#" + id);
// 精靈的 API 要帶這個標頭（後端用來擋別的網站送來的請求）
const api = (path, opts = {}) => baseApi(path, { ...opts, headers: { ...(opts.headers || {}), "X-Anima-Setup": "1" } });
let scan = null, chk = null, page = 0, poll = null, finished = false, usable = false;
// failedRun：這次安裝有失敗或被停止（按鈕變「重試」）；scanHint：第一頁「下一步」按不了的原因
let failedRun = false, scanHint = "正在檢查這台電腦…";
// /setup#models：主畫面的「下載模型…」「去下載模型」。位置照目前的設定自動填好，檢查過就直接跳到「要下載哪些模型」
let jumpModels = location.hash === "#models";
// packs：勾選的功能包（Set，第三頁第一次打開時預設基本版）；gpu、amd_gfx：第二頁的 ComfyUI 版本、A 卡架構（gpu.rs，存在 comfy.json）
const pick = { data: null, dataNew: "", dataManual: "", comfy: null, comfyNew: "", comfyManual: "", packs: null, gpu: "auto", amd_gfx: "", python: "", external: false };
const inMain = location.pathname === "/setup"; // 正式畫面裡打開的「安裝與修復」

// ---------------------------------------------------------------- 外觀、視窗按鈕
const theme = store.get("theme", "auto");
if (theme !== "auto") document.documentElement.setAttribute("data-theme", theme);
$$("[data-ic]").forEach((e) => setIc(e, e.dataset.ic));
$$("[data-app]").forEach((e) => (e.outerHTML = appIcon(+e.dataset.app)));
F("backHome").hidden = !inMain;
// 介面語言（跟設定頁同一個設定；換了會重新整理）
F("wizLang").innerHTML = Object.entries(i18n.LANGS).map(([k, v]) => `<option value="${k}"${k === i18n.current() ? " selected" : ""}>${v}</option>`).join("");
F("wizLang").onchange = () => i18n.choose(F("wizLang").value);
const tauri = window.__TAURI__;
if (tauri?.window) {
  document.documentElement.classList.add("app");
  F("wctl").hidden = false;
  const win = tauri.window.getCurrentWindow();
  // 結束前先確認一次（容易不小心關掉）；安裝跑到一半的話多講一句
  const quit = () => {
    const msg = poll && !finished
      ? "安裝還沒跑完：現在結束，這次的安裝會中斷，之後要再打開精靈按「開始安裝」。確定要結束嗎？"
      : "確定要結束生圖站嗎？";
    if (confirm(msg)) tauri.core.invoke("quit_app");
  };
  // 關閉鍵和 Alt+F4 同一套：正式畫面裡打開的照設定頁（選了結束程式才結束），第一次的精靈一律結束
  const close = () => (inMain && store.get("closeAct", "ask") !== "quit" ? tauri.core.invoke("hide_window") : quit());
  F("wctl").addEventListener("click", (e) => {
    const b = e.target.closest("[data-win]");
    if (!b) return;
    if (b.dataset.win === "min") win.minimize();
    else if (b.dataset.win === "max") win.toggleMaximize();
    else close();
  });
  tauri.event.listen("anima-close", close);
}

// ---------------------------------------------------------------- 換頁
function go(n) {
  if (n > 0 && n < 3 && !scan) n = 0; // 檢查電腦還沒成功：後面幾頁沒有資料可以顯示
  page = n;
  $$("[data-page]").forEach((p) => (p.hidden = +p.dataset.page !== n));
  $$(".step").forEach((s) => {
    s.classList.toggle("on", +s.dataset.s === n);
    s.classList.toggle("done", +s.dataset.s < n);
  });
  F("prevBtn").hidden = n === 0 || n === 3;
  F("cancelBtn").hidden = true;
  F("skipBtn").hidden = true;
  // 第一頁：電腦還沒檢查完（或檢查失敗）之前不能按「下一步」，不然第二頁是空白的
  F("nextBtn").disabled = n === 0 && !scan;
  F("nextBtn").textContent = n === 2 ? "開始安裝" : "下一步";
  F("navHint").textContent = n === 0 && !scan ? scanHint : "";
  F("body").scrollTop = 0;
  if (n === 1) renderPlaces();
  if (n === 2) { renderModels(); autoScan(); }
  if (n === 3) startRun();
}
F("prevBtn").onclick = () => go(page - 1);
F("nextBtn").onclick = () => {
  if (page === 0 && !scan) return;
  if (page === 1 && !placesReady()) return;
  if (page === 2 && pick.data === "new" && (scan.current || scan.data.length) &&
      !confirm("你選擇建立空白圖庫，舊資料不會搬移或刪除。確定要全新開始，而不是沿用原資料嗎？")) return;
  if (page === 3) {
    if (!finished) return;
    if (failedRun) startRun(); // 有失敗或停止：用剛才的選擇再跑一次（已經下載的不會重抓）
    else finish();
    return;
  }
  go(page + 1);
};
F("skipBtn").onclick = () => { if (finished && usable) finish(); };

// ---------------------------------------------------------------- 1. 檢查電腦
const gb = (n) => (n == null ? "—" : `${(n / 2 ** 30).toFixed(n >= 100 * 2 ** 30 ? 0 : 1)} GB`);
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

// 檢查失敗：講白話原因和怎麼辦，給「重試」鈕（技術細節留給回報用）
function scanFailed(e) {
  scan = null;
  const m = String(e?.message || "");
  const why = /超過|timeout/i.test(m) ? "檢查花的時間太久，沒有等到結果（電腦的硬碟很多、或有硬碟睡著了，會拖很久）。"
    : /Failed to fetch|NetworkError|Load failed/i.test(m) ? "連不到生圖站自己的後台（它可能剛好關掉了）。"
    : "檢查這台電腦的時候出了問題。";
  scanHint = "檢查失敗了，按「重試」再檢查一次";
  F("sysScreen").innerHTML = `<span class="r zh">${why}</span>`;
  F("sysWarn").innerHTML = `<div class="warnbox">${esc(why)}<div class="sm">按「重試」再檢查一次。一直失敗的話，先把生圖站完全結束（系統列的圖示也要按「結束」）再重新打開。</div>
    <div class="muted sm">技術細節（回報問題時請一起截圖）：<span translate="no">${esc(m || "沒有回應")}</span></div>
    <div style="margin-top:6px"><button class="akey sm go" type="button" id="scanRetry">重試</button></div></div>`;
  F("scanRetry").onclick = () => start();
  if (page === 0) { F("nextBtn").disabled = true; F("navHint").textContent = scanHint; }
}

async function loadScan() {
  scan = null;
  scanHint = "正在檢查這台電腦…";
  F("sysScreen").textContent = "正在檢查這台電腦…";
  F("sysWarn").innerHTML = "";
  if (page === 0) { F("nextBtn").disabled = true; F("navHint").textContent = scanHint; }
  try {
    scan = await api("/api/setup/scan", { timeout: 240000 }); // 會掃每顆硬碟找舊的安裝，硬碟大時可能很久（最多等 4 分鐘）
  } catch (e) {
    return scanFailed(e);
  }
  if (!scan || !Array.isArray(scan.gpus)) return scanFailed(new Error("掃描結果是空的"));
  const lines = [];
  // N 卡驅動沒裝好（nvidia-smi 看不到、WMI 才找到的）：不當成可用的卡，另外講「驅動沒裝好」
  const nvNoDriver = scan.gpus.filter((g) => g.vendor === "nvidia" && g.driver_missing);
  const gpus = scan.gpus.filter((g) => !(g.vendor === "nvidia" && g.driver_missing));
  if (gpus.length) for (const g of gpus) lines.push(`GPU  <span class="a">${esc(g.name)}</span>  VRAM <span class="a">${g.vram_mb ? (g.vram_mb / 1024).toFixed(0) + " GB" : "?"}</span>${g.driver ? `  DRV <span class="${g.driver_old ? "r" : "a"}">${esc(g.driver)}</span>` : ""}${g.gfx ? `  ARCH <span class="a">${esc(g.gfx)}</span>` : ""}`);
  else if (!scan.detected) lines.push(`<span class="r">GPU  <span class="zh">偵測不到顯示卡</span></span>`);
  else if (nvNoDriver.length) lines.push(`<span class="r">GPU  <span class="a">${esc(nvNoDriver[0].name)}</span>  <span class="zh">NVIDIA 顯示卡的驅動沒有裝好</span></span>`);
  else lines.push(`<span class="r">GPU  <span class="zh">找不到顯示卡</span></span>`);
  if (scan.lite?.ram) lines.push(`RAM  <span class="${scan.lite.tiny_ram ? "r" : "a"}">${gb(scan.lite.ram)}</span>`);
  lines.push(scan.vcredist.ok ? `VC++ <span class="a">${esc(scan.vcredist.version)}</span>`
    : `<span class="r">VC++ <span class="zh">${scan.vcredist.version ? `${esc(scan.vcredist.version)} 太舊` : "沒有裝"}，安裝時會一起裝</span></span>`);
  for (const d of scan.disks) lines.push(`DISK ${esc(d.drive)}  <span class="zh">可用</span> <span class="a">${gb(d.free)}</span> / ${gb(d.total)}`);
  lines.push(`<span class="zh">找到舊版生圖站資料</span> <span class="a">${scan.data.length}</span>  <span class="zh">找到 ComfyUI</span> <span class="a">${scan.comfy.length}</span>`);
  F("sysScreen").innerHTML = lines.join("<br>");
  const warn = [];
  // 該裝哪一版（gpu.rs auto_plan）：N 卡照舊；A 卡、Intel Arc 實驗性；什麼都不能用的一開始就講清楚
  const auto = scan.auto, why = auto.why;
  const usableGpu = gpus.filter((g) => g.vendor === "nvidia" || (g.vendor === "amd" && g.route !== "old") || (g.vendor === "intel" && g.arc));
  if (nvNoDriver.length) warn.push({ html: `偵測到 NVIDIA 顯示卡（<span translate="no">${esc(nvNoDriver[0].name)}</span>），但驅動沒有裝好：請先到 <a href="https://www.nvidia.com/Download/index.aspx" target="_blank" rel="noopener">NVIDIA 官網</a>（或 NVIDIA App）裝最新的驅動，裝完重開機，再重新開啟這個精靈。` });
  else if (why === "nvidia" && !gpus.some((g) => g.vendor === "nvidia")) warn.push("沒有偵測到 NVIDIA 顯示卡。ComfyUI 的免安裝版需要 NVIDIA 顯示卡（顯存 8GB 以上）才能正常畫圖。");
  else if (usableGpu.length && usableGpu.every((g) => g.vram_mb && g.vram_mb < 7800)) warn.push("顯示卡的顯存不到 8GB，畫圖可能會很慢或失敗。");
  if (why === "amd_portable") warn.push({ html: `偵測到 AMD 顯示卡（架構 <span translate="no">${esc(auto.gfx)}</span>）：A 卡目前是實驗性支援。安裝時會改下載 ComfyUI 官方的 AMD 版免安裝包（PyTorch＋ROCm 7.2.1），AI 改寫改用 llama.cpp 的 Vulkan 版。請先裝好最新的 AMD Adrenalin 驅動（<a href="https://www.amd.com/en/support/download/drivers.html" target="_blank" rel="noopener">AMD 官網</a>）。生圖站作者手上沒有 A 卡，這條路還沒實際測過，遇到問題請回報。` });
  if (why === "amd_rocm10") warn.push({ html: `偵測到 AMD 顯示卡（架構 <span translate="no">${esc(auto.gfx)}</span>）：A 卡目前是實驗性支援。官方 AMD 版免安裝包沒有附這張卡的程式，安裝時會先下載它，再用 pip 從 AMD 官方的套件庫把 PyTorch 換成支援這張卡的 ROCm 10 版（多下載約 ${gb(auto.rocm10_bytes)}）。AI 改寫用 llama.cpp 的 Vulkan 版。請先裝好最新的 AMD Adrenalin 驅動（<a href="https://www.amd.com/en/support/download/drivers.html" target="_blank" rel="noopener">AMD 官網</a>）。這條路只在 N 卡電腦上試裝過（PyTorch 裝得起來），還沒在 A 卡上實際畫過圖，遇到問題請回報。` });
  if (why === "amd_unknown") warn.push({ html: `偵測到 AMD 顯示卡，但認不出是哪一代（<span translate="no">${esc(auto.card)}</span>）：會先照 AMD 版免安裝包裝；知道型號的話，可以在下一頁的「A 卡架構」手動選。A 卡目前是實驗性支援，還沒實際測過。` });
  if (why === "intel_arc") warn.push({ html: `偵測到 Intel Arc 顯示卡：Intel 目前是實驗性支援。安裝時會改下載 ComfyUI 官方的 Intel 版免安裝包（PyTorch XPU，解壓縮後約 5.3GB），AI 改寫用 llama.cpp 的 Vulkan 版。請先裝好最新的 Intel 顯示卡驅動（<a href="https://www.intel.com/content/www/us/en/download/785597/intel-arc-graphics-windows.html" target="_blank" rel="noopener">Intel 官網</a>）。還沒實際測過，遇到問題請回報。` });
  if (["none", "amd_old", "intel_old"].includes(why)) {
    const names = scan.gpus.map((g) => g.name).join("、");
    const head = why === "amd_old" ? `${names} 太舊了，AMD 沒有給它用的 PyTorch（要 RX 5000 以後的獨顯，或 680M、780M 以後的內顯）。`
      : why === "intel_old" ? `${names} 是 Intel 的 UHD／Iris Xe 內顯，Intel 版 ComfyUI 不支援（要 Arc 獨顯，或 Core Ultra 的 Arc 內顯）。`
      : "這台電腦沒有可以畫圖的顯示卡。";
    const own = why === "amd_old" ? `<div>已經在用 ZLUDA 版或 DirectML 版 ComfyUI（例如秋葉的 A 卡整合包）畫圖的，下一頁選那份 ComfyUI 就好，生圖站認得它們，不會改成 CPU。</div>` : "";
    warn.unshift({ html: `<b>${esc(head)}</b>${own}<div>生圖站照樣可以裝：圖庫、提示詞、預設組都能用；生圖助手可以改用「雲端顯卡」（租別人的顯示卡，按小時計費，在生圖助手的「雲端設定…」）。這種情況精靈會裝 ComfyUI，但讓它用 CPU 跑（畫一張要好幾分鐘，只適合試試看），生圖模型預設不下載，免得白下載好幾十 GB。</div>` });
  }
  if (scan.detect_timeout) warn.unshift("偵測顯示卡等了很久都沒有回應（顯示卡驅動可能卡住了）。重新開機後再開精靈通常就會好；也可以先繼續，到下一頁自己選 ComfyUI 版本。");
  if (why === "failed") warn.unshift("偵測不到這台電腦有什麼顯示卡（精簡版 Windows 沒有 PowerShell，或 WMI 壞掉）。下一頁請在「ComfyUI 版本」自己選：NVIDIA 顯示卡選 NVIDIA；AMD 選 AMD（實驗性）；Intel Arc 選 Intel（實驗性）；沒有獨立顯示卡選「用 CPU 跑」。");
  const amd = gpus.filter((g) => g.vendor === "amd");
  if (auto.variant === "amd" && amd.some((g) => g.driver_missing)) warn.push("AMD 顯示卡的驅動好像還沒裝（Windows 顯示成「基本顯示卡」）：先到 AMD 官網裝最新的 Adrenalin 驅動，重開機再回來。");
  // AMD、Intel 版 ComfyUI 官方只寫 Windows 11：只提醒、不擋
  if (["amd", "intel"].includes(auto.variant) && scan.windows_build && scan.windows_build < 22000)
    warn.push(auto.variant === "amd" ? "這台是 Windows 10：AMD 版 ComfyUI 官方只寫 Windows 11，Windows 10 沒驗證過，可能會開不起來。"
      : "這台是 Windows 10：Intel 版 ComfyUI 官方只寫 Windows 11，Windows 10 沒驗證過，可能會開不起來。");
  // 驅動太舊：ComfyUI 0.38 的 PyTorch 是 CUDA 13，要 R580 以上。驅動要自己裝（要重開機、要選版本），這裡只提醒
  const oldDrv = scan.gpus.find((g) => g.driver_old);
  if (oldDrv) warn.push({ html: `顯示卡驅動 ${esc(oldDrv.driver)} 太舊：ComfyUI 需要 ${scan.driver_min} 版以上，不然會用不了顯示卡。請到 <a href="https://www.nvidia.com/Download/index.aspx" target="_blank" rel="noopener">NVIDIA 官網</a>（或 NVIDIA App）更新驅動，更新完重開機再回來。` });
  // 找舊安裝有時間上限（setup.rs FIND_BUDGET）：沒掃完的硬碟講一聲，不擋
  if (scan.unscanned?.length) warn.push(`有幾顆硬碟太慢，沒在時間內找完舊的安裝（${scan.unscanned.join("、")}）：硬碟在休眠、檔案很多、或是掛載成磁碟的網盤都會這樣。可以直接繼續；舊的圖庫或 ComfyUI 要是放在那裡，下一頁用「自己指定舊資料」「自己指定 ComfyUI 資料夾」選它的位置。`);
  if (!scan.vcredist.ok) warn.push("這台電腦缺少（或太舊）Visual C++ 執行階段，ComfyUI 少了它會開不起來。安裝時精靈會從微軟官網下載（約 18MB）幫你裝，到時候會跳出系統確認視窗，請按「是」。");
  F("sysWarn").innerHTML = warn.map((w) => `<div class="warnbox">${w.html ?? esc(w)}</div>`).join("");
  presetPicks();
  if (page === 0) { F("nextBtn").disabled = false; F("navHint").textContent = ""; } // 檢查完了才能按「下一步」
}
// 網路（net.js）：一打開就測一次，連不上的來源先講清楚（大陸使用者要開梯子）
mountNet(F("netBox"), { autoTest: true });

// 原設定優先；沒有設定且找到多份資料時，必須自己選，不以圖片數決定。
function presetPicks() {
  Object.assign(pick, reuseDefaults(scan));
  pick.dataNew = scan.fresh.data_root;
  pick.comfyNew = scan.fresh.comfy_dir;
  pick.gpu = scan.prefs?.gpu || "auto";
  pick.amd_gfx = scan.prefs?.amd_gfx || "";
  pick.python = scan.prefs?.python || "";
  pick.external = !!scan.prefs?.external;
  renderReuse();
}

function renderReuse() {
  const box = F("reuseNotice");
  const installed = scan.installations || [];
  box.hidden = !(scan.current || scan.data.length || installed.some(x => x.legacy));
  if (box.hidden) return;
  const message = scan.current ? "已找到原設定，優先沿用原本的資料與 ComfyUI；若路徑失效，請在下一頁指定正確位置。"
    : scan.data.length === 1 && scan.data[0].valid ? "找到一份既有資料，已優先選取。下一頁確認位置後就能接續使用。"
    : scan.data.length ? "找到多份或待修復的資料，下一頁請選你原本使用的那份；不會用圖片數量替你決定。"
    : "找到舊版安裝紀錄，但還沒找到圖庫；下一頁可用「自己指定舊資料」找回原位置。";
  box.innerHTML = `<b>接續使用 Anima／PaintDeck 的既有資料</b><p>${message}</p>
    <p class="muted sm">改名不需要搬動圖庫或重下載模型；資料資料夾仍可叫 anima_data。</p>
    <details><summary>程式位置與舊版安裝紀錄</summary>
      <p>目前程式位置：<span class="path" translate="no">${esc(scan.program_dir || "—")}</span></p>
      ${installed.map(x => `<p>${esc(x.name)}：<span class="path" translate="no">${esc(x.path)}</span>${x.exists ? "" : "（位置已不存在）"}</p>`).join("")}
      <p class="muted sm">程式位置和資料位置是分開的。請勿同時啟動新舊版；若兩版裝在相同位置，先確認路徑再處理舊的解除安裝項目，避免移除新版。MCP 請到設定頁重新複製。</p>
    </details>`;
}

// ---------------------------------------------------------------- 2. 位置
function opt(name, value, checked, html) {
  const id = `${name}-${Math.random().toString(36).slice(2, 8)}`;
  return `<label class="opt" for="${id}"><input type="radio" name="${name}" id="${id}" value="${esc(value)}"${checked ? " checked" : ""}><div>${html}</div></label>`;
}

function renderPlaces() {
  if (!scan) return;
  const known = new Set(scan.data.map((d) => d.path));
  let h = "";
  if (scan.current && !known.has(scan.current.data_root))
    h += opt("data", scan.current.data_root, pick.data === scan.current.data_root, `<b>目前使用中</b><div class="path">${esc(scan.current.data_root)}</div>`);
  for (const d of scan.data)
    h += opt("data", d.path, pick.data === d.path,
      `<b>沿用這份資料</b><div class="path">${esc(d.path)}</div><div class="muted sm">圖庫 ${d.images ?? "?"} 張・最後使用 ${esc(d.modified || "—")}${d.comfy ? "・已找到對應 ComfyUI" : ""}</div>${d.problem ? `<div class="st-err sm">${esc(d.problem)}</div>` : ""}`);
  h += opt("data", "manual", pick.data === "manual", `<b>自己指定舊資料</b><div class="muted sm">選包含 data 資料夾的那一層；若選到裡面的 data，也會辨識並沿用外層，不另建空白圖庫。</div><div class="pickrow"><input type="text" id="dataManual" value="${esc(pick.dataManual)}" aria-label="舊資料位置" spellcheck="false"><button class="akey sm" type="button" data-pick="dataManual">選資料夾…</button></div>`);
  h += opt("data", "new", pick.data === "new", `<b>全新開始</b><div class="muted sm">建立新的資料夾（圖庫是空的）</div><div class="pickrow"><input type="text" id="dataNew" value="${esc(pick.dataNew)}" aria-label="新資料夾位置" spellcheck="false"><button class="akey sm" type="button" data-pick="dataNew">選資料夾…</button></div>`);
  F("dataOpts").innerHTML = h;

  const knownC = new Set(scan.comfy.map((c) => c.path));
  h = "";
  if (scan.current && !knownC.has(scan.current.comfy_dir))
    h += opt("comfy", scan.current.comfy_dir, pick.comfy === scan.current.comfy_dir, `<b>目前使用中</b><div class="path">${esc(scan.current.comfy_dir)}</div>`);
  for (const c of scan.comfy)
    h += opt("comfy", c.path, pick.comfy === c.path, `<b>沿用這個 ComfyUI</b>${c.version ? `（${esc(c.version)}）` : ""}<div class="path">${esc(c.path)}</div>`);
  // 掃描只看每顆硬碟往下三層（加上幾個啟動器的固定位置）：沒被找到的，自己貼路徑
  h += opt("comfy", "manual", pick.comfy === "manual",
    `<b>自己指定 ComfyUI 資料夾</b><div class="muted sm">選整包的最外層就好：官方免安裝版、秋葉整合包、StabilityMatrix、Pinokio、SwarmUI、自己 git clone 的（venv）都認得；官方桌面版選它的資料夾（有 .venv 和 models 的那個）。可以在檔案總管的網址列複製路徑貼上。</div><div class="pickrow"><input type="text" id="comfyManual" value="${esc(pick.comfyManual)}" placeholder="例如 D:\\ComfyUI" aria-label="ComfyUI 資料夾位置" spellcheck="false"><button class="akey sm" type="button" data-pick="comfyManual">選資料夾…</button></div>` +
    `<div class="pickrow"><input type="text" id="comfyPy" value="${esc(pick.python)}" placeholder="Python 位置：找不到它的 Python 時才填（conda、放在外面的 venv…的 python.exe）" aria-label="Python 位置" spellcheck="false"></div>` +
    `<span class="check sm"><input type="checkbox" id="comfyExt"${pick.external ? " checked" : ""}> 我自己開這份 ComfyUI（用它自己的啟動器；生圖站只連過去，不找它的 Python）</span>`);
  h += opt("comfy", "new", pick.comfy === "new",
    `<b>自動下載 ComfyUI ${esc(scan.comfy_download.version)}</b><span id="dlNote">${dlNote(chk?.variant ?? scan.variant, chk?.comfy.download_size ?? scan.comfy_download.size, chk?.gpu.legacy ?? scan.comfy_download.legacy)}</span><div class="pickrow"><input type="text" id="comfyNew" value="${esc(pick.comfyNew)}" aria-label="ComfyUI 要放的位置" spellcheck="false"><button class="akey sm" type="button" data-pick="comfyNew">選資料夾…</button></div>`);
  F("comfyOpts").innerHTML = h;
  renderGpuPick();

  $$("input[name=data]").forEach((r) => (r.onchange = () => {
    pick.data = r.value;
    const sib = scan.data.find((d) => d.path === r.value)?.comfy;
    if (sib) pick.comfy = sib;
    else if (r.value === "new" && !scan.comfy.length) pick.comfy = "new";
    renderPlaces();
  }));
  $$("input[name=comfy]").forEach((r) => (r.onchange = () => { pick.comfy = r.value; recheck(); }));
  F("dataNew").oninput = (e) => { pick.dataNew = e.target.value; pick.data = "new"; $("input[name=data][value=new]").checked = true; recheck(); };
  F("dataManual").oninput = (e) => { pick.dataManual = e.target.value; pick.data = "manual"; $("input[name=data][value=manual]").checked = true; recheck(); };
  F("comfyNew").oninput = (e) => { pick.comfyNew = e.target.value; recheck(); };
  F("comfyManual").oninput = (e) => { pick.comfyManual = e.target.value.trim().replace(/^"(.*)"$/, "$1"); recheck(); };
  F("comfyPy").oninput = (e) => { pick.python = e.target.value.trim().replace(/^"(.*)"$/, "$1"); manualPick(); recheck(); };
  F("comfyExt").onchange = (e) => { pick.external = e.target.checked; manualPick(); recheck(); };
  $$("[data-pick]").forEach((b) => (b.onclick = (e) => { e.preventDefault(); pickInto(b.dataset.pick); }));
  F("comfyManual").onfocus = () => { if (pick.comfy !== "manual") { manualPick(); recheck(); } };
  recheck();
}

// 動到「自己指定」裡面的欄位：順便選起「自己指定」
function manualPick() {
  if (pick.comfy === "manual") return;
  pick.comfy = "manual";
  F("comfyManual").closest("label").querySelector("input[type=radio]").checked = true;
}

// 「自動下載」那一項的說明：照要下載的版本（legacy＝N 卡舊卡版）
function dlNote(variant, size, legacy) {
  if (variant === "nvidia" && legacy) return `（${gb(size)}，官方 Windows 免安裝版的舊卡版（CUDA 12.6，給 GTX 10 系列以前），解壓縮後約 5GB）`;
  if (variant === "amd") return `（${gb(size)}，官方 Windows 免安裝版的 AMD 版，實驗性，解壓縮後約 6GB）`;
  if (variant === "intel") return `（${gb(size)}，官方 Windows 免安裝版的 Intel 版，實驗性，解壓縮後約 5.3GB）`;
  if (variant === "cpu") return `（${gb(size)}，官方 Windows 免安裝版，用 CPU 跑，解壓縮後約 5GB）`;
  if (variant === "unknown") return `（要先在上面選 ComfyUI 版本）`;
  return `（${gb(size)}，官方 Windows 免安裝版，解壓縮後約 5GB）`;
}

// ComfyUI 版本（gpu.rs）：自動照偵測；偵測失敗、偵測錯、或想用自己的 ComfyUI 時手動選。A 卡另外可以手動選架構
const AUTO_TEXT = { nvidia: "NVIDIA", amd: "AMD（實驗性）", intel: "Intel Arc（實驗性）", cpu: "沒有能用的顯示卡，用 CPU 跑" };
function renderGpuPick() {
  const a = scan.auto;
  const auto = a.variant === "unknown" ? "自動（偵測失敗，請改選下面其中一項）" : `自動（偵測到：${a.variant === "nvidia" && a.legacy ? "NVIDIA 舊卡" : AUTO_TEXT[a.variant]}）`;
  const opts = [["auto", auto], ["nvidia", "NVIDIA 顯示卡（RTX 20 系列以後、GTX 16 系列）"], ["nvidia_legacy", "NVIDIA 舊卡（GTX 10 系列以前）"], ["amd", "AMD 顯示卡（實驗性）"], ["intel", "Intel Arc 顯示卡（實驗性）"],
    ["cpu", "沒有能用的顯示卡：用 CPU 跑（非常慢）"], ["own", "用我自己已經能跑的 ComfyUI（不下載、不檢查版本）"]];
  F("gpuSel").innerHTML = opts.map(([v, t]) => `<option value="${v}"${v === pick.gpu ? " selected" : ""}>${esc(t)}</option>`).join("");
  const detected = scan.gpus.find((g) => g.vendor === "amd" && g.gfx)?.gfx;
  F("gfxSel").innerHTML = `<option value="">${detected ? `照偵測（${esc(detected)}）` : "照偵測"}</option>`
    // 型號說明先查字典再接架構代號（整串「…系列（gfx1201）」字典查不到）
    + scan.amd_choices.map((c) => `<option value="${esc(c.gfx)}"${c.gfx === pick.amd_gfx ? " selected" : ""}>${esc(i18n.tr(c.label))}${i18n.current() === "en" ? ` (${esc(c.gfx)})` : `（${esc(c.gfx)}）`}</option>`).join("");
  F("gpuSel").onchange = () => { pick.gpu = F("gpuSel").value; recheck(); };
  F("gfxSel").onchange = () => { pick.amd_gfx = F("gfxSel").value; recheck(); };
}

// 第二頁「ComfyUI 版本」底下的說明（照檢查結果 chk.gpu）
function gpuNote(c) {
  const g = c.gpu;
  F("gfxRow").hidden = !(g.variant === "amd" && !g.own);
  if (g.own) return `<div class="muted sm">不會下載 ComfyUI，也不檢查它的 PyTorch 跟顯示卡合不合；上面要選「沿用」或「自己指定」你的 ComfyUI。</div>`;
  if (g.variant === "unknown") return `<div class="st-err sm">偵測不到顯示卡：請選一個 ComfyUI 版本，不然沒辦法自動下載 ComfyUI。</div>`;
  if (g.variant === "cpu") return `<div class="muted sm">用 CPU 畫圖非常慢（一張圖要好幾分鐘），只適合試試看；生圖助手可以改用「雲端顯卡」。</div>`;
  if (g.variant === "nvidia" && g.legacy) return `<div class="muted sm">比較舊的 N 卡（GTX 10 系列以前）：新版 PyTorch 不支援，會改下載官方給舊卡的版本（CUDA 12.6）。驅動要 ${esc(scan.driver_min_legacy || "")} 版以上；顯存小的卡畫 Qwen、MOSS 會很慢或不夠。</div>`;
  if (g.variant === "amd" && g.rocm10.length) return `<div class="muted sm">這張卡（<span translate="no">${esc(g.rocm10.join("、"))}</span>）官方 AMD 版沒有附：裝完 ComfyUI 會再用 pip 換成 AMD 官方的 ROCm 10 版 PyTorch，多下載約 ${gb(g.rocm10_bytes)}。</div>`;
  if (g.variant === "amd" && !g.gfx) return `<div class="muted sm">認不出這張 A 卡的架構，先照官方 AMD 版裝；知道型號的話，在右邊選架構。</div>`;
  return "";
}

// 「選資料夾…」：後端跳出 Windows 的選資料夾視窗（setup.rs pick_folder）。
// 新建的兩個（資料、下載 ComfyUI）選的是「放在哪裡」，名字自動接上；選到的本來就叫這個名字就不接
const PICK = {
  dataManual: { title: "選原本的生圖站資料資料夾", leaf: "", radio: ["data", "manual"] },
  dataNew: { title: "選資料要放在哪裡", leaf: "anima_data", radio: ["data", "new"] },
  comfyNew: { title: "選 ComfyUI 要下載到哪裡", leaf: "ComfyUI_windows_portable", radio: ["comfy", "new"] },
  comfyManual: { title: "選 ComfyUI 的資料夾", leaf: "", radio: ["comfy", "manual"] },
};
async function pickInto(id) {
  const c = PICK[id], box = F(id);
  let r;
  try { r = await api("/api/setup/pick_folder", { json: { title: i18n.tr(c.title), start: box.value }, timeout: 0 }); }
  catch (e) { F("navHint").textContent = e.message; return; }
  if (!r.path) return; // 按了取消
  let v = r.path.replace(/[\\/]+$/, "") || r.path;
  if (c.leaf && v.split(/[\\/]/).pop().toLowerCase() !== c.leaf.toLowerCase()) v += "\\" + c.leaf;
  box.value = v;
  box.dispatchEvent(new Event("input")); // 照打字一樣：記下來、重新檢查
  const radio = $(`input[name=${c.radio[0]}][value="${c.radio[1]}"]`);
  if (radio && !radio.checked) { radio.checked = true; radio.dispatchEvent(new Event("change")); }
}

const chosen = () => ({
  ...locationRequest(pick),
  gpu: pick.gpu,
  amd_gfx: pick.amd_gfx,
  // 沿用目前 ComfyUI 時保留自訂 Python／自行啟動設定。
  python: pick.comfy === "manual" || samePath(locationRequest(pick).comfy_dir, scan?.current?.comfy_dir) ? pick.python : "",
  external: pick.comfy === "manual" || samePath(locationRequest(pick).comfy_dir, scan?.current?.comfy_dir) ? pick.external : false,
});

// 只收最後一次檢查的結果：改了位置以後，晚回的舊檢查不能蓋掉新的（不然會拿舊位置的結果放行「下一步」）
let checkSeq = 0;
function recheck() {
  const seq = ++checkSeq;
  chk = null;
  F("nextBtn").disabled = true;
  F("checkOut").innerHTML = `<div class="muted sm">檢查中…</div>`;
  checkPlaces(seq, chosen());
}
const checkPlaces = debounce(async (seq, request) => {
  F("nextBtn").disabled = true;
  F("checkOut").innerHTML = `<div class="muted sm">檢查中…</div>`;
  let r;
  try {
    r = await api("/api/setup/check", { json: request });
  } catch (e) {
    if (seq !== checkSeq) return;
    chk = null;
    F("checkOut").innerHTML = `<div class="warnbox">${esc(e.message)}</div>`;
    return;
  }
  if (seq !== checkSeq) return;
  chk = r;
  renderCheck();
  F("nextBtn").disabled = !placesReady();
  if (jumpModels) {
    jumpModels = false;
    if (placesReady()) go(2); // 位置有問題就停在這一頁，讓使用者看原因
  }
}, 250);

function placesReady() {
  if (!chk) return false;
  let why = "";
  if (!pick.data) why = "請先選擇要沿用哪份資料，或明確選擇全新開始";
  else if (chk.data.selection_problem) why = chk.data.selection_problem;
  else if (pick.data !== "new" && !chk.data.valid) why = chk.data.problem || "找不到可沿用的圖庫，請重新指定資料位置";
  else if (pick.comfy !== "new" && !chk.comfy.ok) why = "選的 ComfyUI 資料夾不能用，請換一個或選「自動下載」";
  else if (pick.comfy === "new" && !chk.comfy.ok && chk.comfy.exists) why = "要下載 ComfyUI 的位置已經有別的東西了，請換一個空的位置";
  else if (!chk.comfy.ok && chk.gpu.own) why = "選了「用我自己已經能跑的 ComfyUI」：請選你的 ComfyUI 資料夾，不要選「自動下載」";
  else if (!chk.comfy.ok && chk.variant === "unknown") why = "偵測不到顯示卡：請先選 ComfyUI 版本";
  F("navHint").textContent = why;
  return !why;
}

// 說明另外包一層：英文介面查字典時「●」不會黏在句子前面（「● 會下載約 1.2GB」整句查不到）
const yes = (b, okText = "已經有了", noText = "會自動安裝") => (b ? `<span class="st-ok">✓ <span>${okText}</span></span>` : `<span class="st-no">● <span>${noText}</span></span>`);

// 自己指定的是整包的最外層（StabilityMatrix、Pinokio、SwarmUI…），ComfyUI 在裡面：給一個按鈕換過去（不自動換：打字打到一半也會檢查）
const inner = () => pick.comfy === "manual" && chk.comfy.inner
  ? `<div><button class="akey sm" type="button" id="useInner">改用裡面的 ComfyUI</button> <span class="path">${esc(chk.comfy.inner)}</span></div>` : "";

// 選的 ComfyUI 是哪一種（gpu.rs comfy_backend）、跟這台的顯示卡合不合：NVIDIA 版裝在只有 A 卡的電腦、AMD 版裝在 N 卡電腦…。
// ZLUDA、DirectML（老手的 A 卡 ComfyUI）認得，照樣可以用；選了「用我自己的 ComfyUI」就不檢查
function torchNote(c) {
  const b = c.comfy.backend;
  if (b === "zluda") return `<div class="muted sm">ZLUDA 版（A 卡）：${c.comfy.zluda_exe ? "生圖站會照它的 bat 用 zluda.exe 開" : "要用它自己的啟動器開（例如绘世启动器），生圖站只連過去"}</div>`;
  if (b === "directml") return `<div class="muted sm">DirectML 版：生圖站開 ComfyUI 時會自動加 --directml（DirectML 很慢，ComfyUI 也已經不太維護它）</div>`;
  if (c.gpu.own) return "";
  if (b === "cuda" && c.variant === "amd") return `<div class="st-err sm">這份 ComfyUI 的 PyTorch 是 NVIDIA 版，A 卡用不了：建議改選「自動下載」裝 AMD 版（實驗性）</div>`;
  if (b === "cuda" && c.variant === "intel") return `<div class="st-err sm">這份 ComfyUI 的 PyTorch 是 NVIDIA 版，Intel 顯示卡用不了：建議改選「自動下載」裝 Intel 版（實驗性）</div>`;
  if (b === "rocm" && scan?.gpus.some((g) => g.vendor === "nvidia")) return `<div class="st-err sm">這份 ComfyUI 是 AMD 版（ROCm），NVIDIA 顯示卡用不了</div>`;
  if (b === "xpu" && scan?.gpus.some((g) => g.vendor === "nvidia")) return `<div class="st-err sm">這份 ComfyUI 是 Intel 版（XPU），NVIDIA 顯示卡用不了</div>`;
  if (c.rocm10.need) return `<div class="sm"><span class="st-no">● <span>會換成支援這張卡的 PyTorch（ROCm 10，約 ${gb(c.rocm10.bytes)}）</span></span></div>`;
  if (c.comfy.legacy_conflict) return `<div class="st-err sm">這張是比較舊的 N 卡（GTX 10 系列以前），這份 ComfyUI 的 PyTorch（CUDA 13）不支援它：建議改選「自動下載」裝舊卡版</div>`;
  return "";
}

// 沿用的 ComfyUI 比生圖站測過的版本舊：內建功能可能缺節點
// 官方免安裝版比生圖站要的新版舊（setup.rs comfy_upgradable）：安裝時會自動升級
const oldNote = (c) => c.comfy.upgrade_to
  ? `<div class="muted sm">⬆ 開始安裝時會把這份 ComfyUI 從 ${esc(c.comfy.version)} 升級到 ${esc(c.comfy.upgrade_to)}（MOSS 編輯、色彩深度控制都要新版；模型、圖片、外掛不動，換掉的檔會備份）</div>`
  : c.comfy.old
  ? `<div class="st-err sm">這份 ComfyUI 是 ${esc(c.comfy.version)} 版，比生圖站測過的 ${esc(c.comfy.min_version)} 舊，有些內建功能會缺節點：建議改選「自動下載」，或先把它更新到新版</div>` : "";

// 還沒有 ComfyUI、要下載的那一列
function comfyDownload(c) {
  if (c.gpu.own) return `<span class="st-err">✗ <span>選了「用我自己的 ComfyUI」：請在上面選你的 ComfyUI 資料夾</span></span>`;
  if (c.variant === "unknown") return `<span class="st-err">✗ <span>不知道該下載哪一版：請在上面選 ComfyUI 版本</span></span>`;
  const size = gb(c.comfy.download_size);
  const what = c.variant === "amd" ? `會下載 AMD 版 ${size}` : c.variant === "intel" ? `會下載 Intel 版 ${size}` : `會下載 ${size}`;
  const extra = c.rocm10.need ? `<div class="sm"><span class="st-no">● <span>裝完再換成支援這張卡的 PyTorch（ROCm 10，約 ${gb(c.rocm10.bytes)}）</span></span></div>` : "";
  return `<span class="st-no">● <span>${what}</span></span>${extra}`;
}

function renderCheck() {
  const c = chk;
  const vulkan = c.variant !== "nvidia"; // N 卡以外的 llama.cpp 是 Vulkan 版
  F("gpuNote").innerHTML = gpuNote(c);
  if (F("dlNote")) F("dlNote").innerHTML = dlNote(c.variant, c.comfy.download_size, c.gpu.legacy);
  const rows = [
    ["資料資料夾", `<div class="path" translate="no">${esc(c.data.path)}</div>` + (c.data.selection_problem ? `<span class="st-err">${esc(c.data.selection_problem)}</span>` : c.data.valid ? `<span class="st-ok">✓ 沿用</span>・圖庫 ${c.data.images ?? 0} 張` : `<span class="st-no">● 會新建</span>`) +
      (!samePath(c.data.path, chosen().data_root) ? `<div class="muted sm">已辨識內層 data，實際使用上方的外層資料夾。</div>` : "") +
      (pick.data === "new" && c.data.valid ? `<button class="akey sm" type="button" id="reuseSelected">改為沿用這份資料</button>` : "")],
    ["ComfyUI", c.comfy.ok ? `<span class="st-ok">✓ ${esc(c.comfy.version || "可以用")}</span>${c.comfy.portable === false ? `<div class="muted sm">整合包或自己安裝的 ComfyUI（用它自己的 Python 開）</div>` : ""}${c.comfy.external ? `<div class="muted sm">你自己開這份 ComfyUI，生圖站只連過去</div>` : ""}${oldNote(c)}${torchNote(c)}`
      : c.comfy.exists ? `<span class="st-err">✗ 這個資料夾不能用</span><div class="muted sm">${esc(c.comfy.problem || "")}</div>${inner()}`
      : comfyDownload(c)],
    ["AI 改寫節點", yes(c.node)],
    ["llama.cpp（AI 改寫用）", yes(c.llama, "已經有了", vulkan ? "勾「AI 改寫」會下載 Vulkan 版，約 30MB" : "勾「AI 改寫」會下載約 0.6GB")],
    ["反推模型", yes(c.tagger, "已經有了", "會下載約 1.2GB")],
    ["Visual C++ 執行階段", yes(c.vcredist, "已經有了", "會下載約 18MB 並安裝（會跳出系統確認視窗）")],
    ["影音工作台工具（Node.js、ffmpeg）", yes(c.studio_tools, "已經有了", "勾「影音工作台」會下載約 150MB")],
    ["ComfyUI 工作流程", yes(!c.workflows_missing, "已經有了", `會放入 ${c.workflows_missing} 個`)],
    c.loras_bundled ? ["程式附帶的畫風 LoRA", yes(!c.loras_missing, "已經有了", `會放入 ${c.loras_missing} 個`)] : null,
    ["Civitai API key", c.data.has_key ? `<span class="st-ok">✓ 已填</span>` : `<span class="muted">沒填（常用 LoRA 有些要它）</span>`],
    ["硬碟可用空間", `資料 ${gb(c.data.free)}・ComfyUI ${gb(c.comfy.free)}`],
  ];
  F("checkOut").innerHTML = `<table class="tbl">${rows.filter(Boolean).map(([a, b]) => `<tr><td>${a}</td><td>${b}</td></tr>`).join("")}</table>`;
  const reuse = F("reuseSelected");
  if (reuse) reuse.onclick = () => { pick.data = "manual"; pick.dataManual = c.data.path; renderPlaces(); };
  const use = F("useInner");
  if (use) use.onclick = () => { pick.comfyManual = F("comfyManual").value = c.comfy.inner; recheck(); };
}

// ---------------------------------------------------------------- 3. 模型（功能包打勾自選；每一包的內容在 setup.rs 的 PACKS）
// 已經齊全的包固定打勾、不能取消（取消也不會刪檔）。勾了要「先有」別包的，那包跟著勾；取消一包，靠它的也跟著取消
const packDone = (p) => !p.missing.missing && p.tool;
const wanted = (p) => packDone(p) || pick.packs.has(p.id);

function packLine(p) {
  // 模型檔齊了不代表能畫：ComfyUI 的程式沒有 MOSS 支援時（setup.rs moss_support），卡片上直接講（2026-10-30 有人因此一直重下模型）
  const moss = p.id === "moss" && chk.comfy.moss_problem ? `<div class="st-err sm">⚠ ${esc(chk.comfy.moss_problem)}</div>` : "";
  if (packDone(p)) return moss ? `<span class="st-ok">✓ 模型檔已經有了</span>${moss}` : `<span class="st-ok">✓ 已經有了</span>`;
  return packMissing(p) + moss;
}

function packMissing(p) {
  const m = p.missing;
  let s = m.missing ? `還缺 ${m.missing} 個檔，約 ${gb(m.bytes)}` : `還缺約 ${gb(m.bytes)}`;
  // Civitai 那段另外包一層：英文介面一段一段查字典比較準
  if (m.civitai) s += `<span>${(chk.data.has_key || F("wizCivitaiKey").value.trim()) ? `（其中 ${m.civitai} 個來自 Civitai）` : `（其中 ${m.civitai} 個來自 Civitai，要先填 API key）`}</span>`;
  return s;
}

function renderModels() {
  F("locationSummary").innerHTML = `<b>本次使用的位置</b><p>${chk.data.valid ? "沿用圖庫與設定" : "建立空白圖庫"}：<span class="path" translate="no">${esc(chk.data.path)}</span></p>
    <p>${chk.comfy.ok ? "沿用 ComfyUI 與模型" : "下載新的 ComfyUI"}：<span class="path" translate="no">${esc(chk.comfy.path)}</span></p>
    <p class="muted sm">資料留在原位置，下面只補勾選項目缺少的檔案；確認位置後再開始。</p>`;
  F("wizCivitaiSaved").textContent = chk.data.has_key ? "這個資料位置已儲存金鑰；留空即可沿用。" : "尚未儲存 Civitai 金鑰；沒有金鑰也可以繼續安裝其他項目。";
  F("wizCivitaiKey").oninput = renderModels;
  const packs = chk.packs, basic = chk.basic_packs;
  // 沒有能用的顯示卡：預設全不選，免得白下載好幾十 GB（沿用老手的 ZLUDA／DirectML 版就照常：舊 A 卡靠它們也能畫）
  // 選「用我自己的 ComfyUI」也照常（生圖站不會替它加 --cpu）
  const cpu = chk.variant === "cpu" && !chk.gpu.own && !(chk.comfy.ok && ["zluda", "directml", "rocm", "xpu"].includes(chk.comfy.backend));
  // 低配（顯存 8GB 以下、記憶體 16GB 以下）：基本版裡的 Qwen、MOSS 模型很大，預設只裝動漫（Anima）
  const lite = !cpu && !!scan?.lite?.lite;
  // 第一次打開、或回第二頁改了版本（CPU ↔ 有顯示卡）：重設預設勾選
  const reuse = !!chk.data.valid;
  const key = `${cpu}|${lite}|${reuse}|${chk.data.path}`;
  if (!pick.packs || pick.packsKey !== key) { pick.packs = new Set(cpu || reuse ? [] : lite ? ["anima"] : basic); pick.packsKey = key; }
  const label = Object.fromEntries(packs.map((p) => [p.id, p.label]));
  const row = (p) => `<label class="opt"><input type="checkbox" data-pack="${esc(p.id)}"${wanted(p) ? " checked" : ""}${packDone(p) ? " disabled" : ""}>
    <div><b>${esc(p.label)}</b>${p.requires ? ` <span class="muted sm">要先有：</span><span class="muted sm">${esc(label[p.requires])}</span>` : ""}
    <div class="muted sm">${esc(p.desc)}</div><div class="sm">${packLine(p)}</div></div></label>`;
  F("modelOpts").innerHTML = (cpu ? `<div class="warnbox">這台電腦沒有能用的顯示卡（ComfyUI 會用 CPU 跑）：生圖模型預設全不選，免得白下載好幾十 GB。想試試 CPU 畫圖可以自己勾，畫一張要好幾分鐘。</div>` : "") +
    liteNote(packs, basic, cpu) + ltxNote() +
    `<div class="lbl">基本版：三種畫風的生圖功能</div>` + packs.filter((p) => basic.includes(p.id)).map(row).join("") +
    `<div class="lbl" style="margin-top:6px">額外功能（完整版）</div>` + packs.filter((p) => !basic.includes(p.id)).map(row).join("");
  if (F("ltxDel")) F("ltxDel").onclick = removeLtx;
  $$("#modelOpts input[data-pack]").forEach((c) => (c.onchange = () => {
    const id = c.dataset.pack;
    if (c.checked) {
      pick.packs.add(id);
      const req = packs.find((p) => p.id === id).requires;
      if (req) pick.packs.add(req);
    } else {
      pick.packs.delete(id);
      for (const p of packs) if (p.requires === id) pick.packs.delete(p.id);
    }
    renderModels();
  }));
  // 快速鈕：目前勾的剛好是哪一組就按下去（基本版都裝齊時「基本版」跟「全不選」一樣，只亮前面那顆）
  const ids = packs.filter((p) => !packDone(p)).map((p) => p.id);
  const now = ids.filter((id) => pick.packs.has(id)).sort().join();
  const sets = { basic: ids.filter((id) => basic.includes(id)), full: ids, none: [], anima: ids.filter((id) => id === "anima") };
  const lit = ids.length ? ["basic", "full", "none", "anima"].find((k) => sets[k].sort().join() === now) : null;
  $$("#packQuick [data-set]").forEach((b) => b.classList.toggle("on", b.dataset.set === lit));
  diskWarn();
}

// 低配提醒：顯存 8GB 以下或記憶體 16GB 以下（後端 setup.rs lite_view）。不擋，只是預設只裝動漫，並提醒會很慢
function liteNote(packs, basic, cpu) {
  const l = scan?.lite;
  if (!l || cpu) return "";
  const left = (ids) => packs.filter((p) => ids.includes(p.id) && !packDone(p)).reduce((a, p) => a + p.missing.bytes, 0);
  let h = "";
  if (l.low_vram || l.low_ram) {
    const spec = [l.low_vram && `顯示卡的顯存只有約 ${(l.vram_mb / 1024).toFixed(0)} GB`, l.low_ram && `記憶體 ${gb(l.ram)}`].filter(Boolean).join("、");
    h += `<div class="warnbox"><b>這台電腦的配備比較小</b>（${spec}）。基本版裡的 Qwen 編輯、MOSS 繪圖模型都很大，基本版全部要下載約 ${gb(left(basic))}，在這種電腦上會跑得很吃力、甚至失敗。所以先幫你選好「只裝動漫（Anima）」（約 ${gb(left(["anima"]))}）；之後想要其他功能，隨時回這一頁補勾。</div>`;
  }
  if (l.tiny_ram) h += `<div class="warnbox">記憶體只有 ${gb(l.ram)}，不到 16GB：畫圖可能會很慢，甚至當掉。建議畫圖時關掉瀏覽器和其他大程式。生圖站不會擋你，想裝別的功能還是可以自己勾。</div>`;
  return h;
}

// 2026-10-30 LTX-2.3 退場：以前「影片」包下載的檔還在硬碟上就提醒，按了才刪（伺服器自己重新找，只認那三個檔名）
function ltxNote() {
  const l = chk?.ltx_leftovers;
  if (!l?.files?.length) return "";
  return `<div class="warnbox"><b>舊的 LTX-2.3 影片模型已經不用了</b>：內建影片改成 MiniMax H3（下面的「H3 影片」包）。這台電腦還留著以前下載的 ${l.files.length} 個 LTX 檔，約 ${gb(l.bytes)}：
    <div class="sm muted" translate="no">${l.files.map(esc).join("<br>")}</div>
    <button type="button" class="akey sm" id="ltxDel">刪掉這些舊檔（空出 ${gb(l.bytes)}）</button></div>`;
}
async function removeLtx() {
  const b = F("ltxDel");
  if (!confirm("確定刪掉舊的 LTX-2.3 影片模型？刪掉的檔不會進資源回收筒。")) return;
  b.disabled = true;
  try {
    const r = await api("/api/setup/ltx_remove", { json: chosen() });
    if (r.fails?.length) alert(`有幾個檔刪不掉（可能 ComfyUI 正開著它）：\n${r.fails.join("\n")}`);
    chk = await api("/api/setup/check", { json: chosen() });
    renderModels();
  } catch (e) { b.disabled = false; alert(e.message); }
}

$$("#packQuick [data-set]").forEach((b) => (b.onclick = () => {
  // 選配包（35B、影片）「完整版」也不勾，要自己勾
  const ids = chk.packs.filter((p) => !p.optional).map((p) => p.id);
  const s = b.dataset.set;
  pick.packs = new Set(s === "full" ? ids : s === "basic" ? chk.basic_packs : s === "anima" ? ["anima"] : []);
  renderModels();
}));

// 掃描這台電腦已有的模型（model_dirs.rs）：勾選的資料夾加進外部模型清單，再檢查一次「還缺幾個」
// 第一次進到這頁就自動掃（不用自己按）；之後要再掃按按鈕
let scanTimer = null, scanned = false, scanRunning = false;
function autoScan() {
  if (!scanned) F("mdScan").onclick();
  else if (scanRunning) pollScan(); // 掃到一半跑去上一頁又回來：接著顯示進度
}
F("mdScan").onclick = async () => {
  scanned = true;
  F("mdScanOut").hidden = false; // 自動掃的時候出錯也要看得到
  try { await api("/api/setup/model_scan", { method: "POST" }); } catch (e) { if (!/已經在掃描/.test(e.message)) return F("mdScanOut").replaceChildren(e.message); }
  F("mdScan").disabled = true;
  pollScan();
};

async function pollScan() {
  clearTimeout(scanTimer);
  const out = F("mdScanOut");
  let s;
  try { s = await api("/api/setup/model_scan/status", { json: chosen() }); } catch (e) { F("mdScan").disabled = false; out.textContent = e.message; return; }
  scanRunning = s.running;
  if (s.running) {
    out.innerHTML = `<div>掃描中…已看過 ${s.dirs.toLocaleString()} 個資料夾、${s.files.toLocaleString()} 個模型檔</div>
      <div class="muted sm md-path" translate="no">${esc(s.current || "")}</div><div><button class="akey sm" type="button" id="mdStop">停止</button></div>`;
    F("mdStop").onclick = () => api("/api/setup/model_scan/cancel", { method: "POST" }).catch(() => {});
    if (page === 2) scanTimer = setTimeout(pollScan, 1000);
    else F("mdScan").disabled = false;
    return;
  }
  F("mdScan").disabled = false;
  const head = `<div>${s.cancelled ? "已停止。" : "掃完了。"}安裝清單裡的模型找到 ${s.found.length} 個${s.found.some((f) => f.used) ? `（${s.found.filter((f) => f.used).length} 個本來就在用）` : ""}。</div>`;
  if (!s.suggest.length) { out.innerHTML = head + `<div class="muted sm">沒有找到還沒在用的模型資料夾。</div>`; return; }
  const why = { manifest: "清單裡的模型", a1111: "A1111／Forge 的資料夾", yaml: "別的 ComfyUI 設定檔裡寫的", comfy: "別的 ComfyUI 的資料夾" };
  out.innerHTML = head + `<div>勾選要原地使用的資料夾：</div><div class="md-list">${s.suggest.map((x, i) => `<label class="check md-row"><input type="checkbox" data-i="${i}" checked>
    <span class="md-kind">${esc(x.label)}</span><span class="md-path" translate="no">${esc(x.path)}</span>
    <span class="muted sm">${why[x.why.type] || why.comfy}${x.manifest_files ? `・清單模型 ${x.manifest_files} 個` : ""}${x.why.files ? `・${x.why.files} 個檔・${fmtBytes(x.why.bytes)}` : ""}</span></label>`).join("")}</div>
    <div><button class="akey sm go" type="button" id="mdApply">使用勾選的資料夾</button></div>`;
  F("mdApply").onclick = async () => {
    const dirs = $$("#mdScanOut input[data-i]").filter((c) => c.checked).map((c) => s.suggest[+c.dataset.i]).map((x) => ({ kind: x.kind, path: x.path }));
    if (!dirs.length) return;
    F("mdApply").disabled = true;
    try {
      await api("/api/setup/model_dirs", { json: { ...chosen(), dirs } });
      chk = await api("/api/setup/check", { json: chosen() });
      out.innerHTML = `<div class="st-ok">✓ 已加入 ${dirs.length} 個資料夾，下面的「還缺」已經扣掉找到的模型。</div>`;
      renderModels();
    } catch (e) { F("mdApply").disabled = false; out.insertAdjacentHTML("beforeend", `<div class="warnbox">${esc(e.message)}</div>`); }
  };
}

function diskWarn() {
  const gib = 2 ** 30;
  // 勾選的包還缺多少：後端已經把 llama.cpp 解開後的大小算進「AI 改寫」、Node.js＋ffmpeg 算進「影音工作台」
  const on = chk.packs.filter((p) => !packDone(p) && pick.packs.has(p.id));
  const has = (id) => on.some((p) => p.id === id);
  // N 卡以外：llama.cpp 是 Vulkan 版（壓縮檔約 33MB）；ComfyUI 的 AMD 版解開約 5.8GB、Intel 版約 5.3GB；
  // A 卡換 ROCm 10 PyTorch：下載的檔（約 1.1GB 以上）裝完搬到資料的「待刪除」，ComfyUI 那邊多約 0.6GB
  const v = chk.variant, rocm = chk.rocm10.need ? chk.rocm10.bytes : 0;
  const dataNeed = (chk.tagger ? 0 : 1.3 * gib) + (chk.llama || !has("pe") ? 0 : (v === "nvidia" ? 0.7 : 0.04) * gib) // llama.cpp 的壓縮檔
    + on.filter((p) => p.id === "studio").reduce((a, p) => a + p.missing.bytes, 0) // 影音工具放在資料資料夾
    + (chk.comfy.ok ? 0 : chk.comfy.download_size) + rocm; // 壓縮檔安裝後搬到資料的「待刪除」
  const comfyNeed = on.filter((p) => p.id !== "studio").reduce((a, p) => a + p.missing.bytes, 0)
    + (chk.comfy.ok ? 0 : (v === "amd" ? 5.8 : v === "intel" ? 5.3 : 5.5) * gib) + (rocm ? 0.6 * gib : 0);
  const drive = (path) => /^[a-z]:/i.exec(path || "")?.[0]?.toUpperCase();
  const sameDisk = drive(chk.data.path) && drive(chk.data.path) === drive(chk.comfy.path);
  const checks = sameDisk
    ? [["資料與 ComfyUI", dataNeed + comfyNeed - (chk.comfy.ok ? 0 : chk.comfy.download_size), chk.data.free]]
    : [["資料", dataNeed, chk.data.free], ["ComfyUI", comfyNeed, chk.comfy.free]];
  const warnings = checks.filter(([, need, free]) => free != null && need > free * 0.95)
    .map(([name, need, free]) => `<div class="warnbox">${name} 所在的硬碟可能不夠：大約需要 ${gb(need)}，只剩 ${gb(free)}。</div>`);
  F("diskWarn").innerHTML = warnings.join("") || (dataNeed + comfyNeed
    ? `<div class="muted sm">這次大約會用掉 ${gb(dataNeed + comfyNeed)} 的硬碟空間。</div>` : "");
}

// ---------------------------------------------------------------- 4. 安裝
async function startRun() {
  finished = false;
  failedRun = false;
  usable = false;
  F("nextBtn").disabled = true;
  F("nextBtn").textContent = "安裝中…";
  F("cancelBtn").hidden = false;
  F("cancelBtn").disabled = false;
  F("skipBtn").hidden = true;
  F("prevBtn").hidden = true;
  F("navHint").textContent = "";
  F("runTitle").textContent = "安裝中";
  // 正式畫面裡重新打開、安裝本來就在跑：scan 可能沒有（檢查失敗），不用再送一次，直接接著看進度
  if (scan) {
    try {
      const key = F("wizCivitaiKey").value.trim();
      await api("/api/setup/run", { json: { ...chosen(), packs: [...(pick.packs || [])], ...(key ? {civitai_key:key} : {}) } });
      if (key && chk?.data) chk.data.has_key = true;
      F("wizCivitaiKey").value = "";
    } catch (e) {
      if (!/已經在安裝/.test(e.message)) {
        // 送不出去（例如路徑填錯）：按鈕換成「重試」，上一步可以回去改
        F("runTitle").textContent = "安裝還沒完成";
        F("runPhase").innerHTML = `<span class="r zh">${esc(e.message)}</span>`;
        finished = true;
        failedRun = true;
        F("cancelBtn").hidden = true;
        F("prevBtn").hidden = false;
        F("nextBtn").disabled = false;
        F("nextBtn").textContent = "重試";
        F("navHint").textContent = "沒辦法開始安裝。看一下上面的原因，按「重試」再試一次，或按「上一步」回去改。";
        return;
      }
    }
  }
  clearInterval(poll);
  poll = setInterval(refresh, 1000);
  refresh();
}

F("cancelBtn").onclick = async () => {
  F("cancelBtn").disabled = true;
  await api("/api/setup/cancel", { method: "POST" }).catch(() => {});
};

const STATUS = { wait: "等待", run: "進行中", done: "完成", skip: "已經有了", error: "失敗", stop: "已停止", warn: "要手動處理" };

// 剩下的時間：「不到 1 分鐘」「15 分鐘」「2 小時 5 分鐘」
function fmtEta(s) {
  if (s < 60) return "不到 1 分鐘";
  const m = Math.round(s / 60);
  return m < 60 ? `${m} 分鐘` : `${Math.floor(m / 60)} 小時 ${m % 60} 分鐘`;
}

// 總進度：已下載／總共、速度、大約剩多久（解壓縮、安裝這些沒有大小的步驟不算在裡面）
function renderOverall(st) {
  const o = st.overall, box = F("overall");
  if (!o || !o.total) { box.hidden = true; return; }
  const pct = Math.min(100, (100 * o.done) / o.total);
  const bits = [`<b>總進度</b> <span>${fmtBytes(o.done)} / ${fmtBytes(o.total)}（${pct.toFixed(0)}%）</span>`];
  if (st.running && o.speed > 0) bits.push(`<span>速度</span> <span>${fmtBytes(o.speed)}/s</span>`);
  if (st.running && o.eta != null) bits.push(`<span>大約還要</span> <span>${fmtEta(o.eta)}</span>`);
  box.hidden = false;
  F("overallText").innerHTML = bits.join("<span class=\"muted\">・</span>");
  F("overallBar").style.width = `${pct.toFixed(1)}%`;
}

async function refresh() {
  let st;
  try { st = await api("/api/setup/status"); } catch { return; }
  F("runPhase").innerHTML = `<span class="zh">${esc(st.phase || "—")}</span>`;
  renderOverall(st);
  F("items").innerHTML = st.items.map((it) => {
    const pct = it.total ? Math.min(100, (100 * it.done) / it.total) : it.status === "run" && it.done ? 50 : 0;
    const amount = it.unit === "%" ? `${it.done}%` : it.total ? `${fmtBytes(it.done)} / ${fmtBytes(it.total)}` : it.done ? fmtBytes(it.done) : "…";
    const num = it.status === "run" ? amount : STATUS[it.status] || it.status;
    return `<div class="it ${it.status}"><span class="name">${esc(it.label)}</span><span class="bar"><i style="width:${pct.toFixed(1)}%"></i></span><span class="num">${esc(num)}</span>${it.hint && it.status === "run" ? `<span class="hintmsg">${esc(it.hint)}</span>` : ""}${it.error ? `<span class="errmsg">${esc(it.error)}</span>` : ""}</div>`;
  }).join("");
  F("log").textContent = st.log.join("\n");
  F("problems").innerHTML = st.problems.map((p) => `<div class="warnbox">${esc(p)}</div>`).join("");
  if (st.finished && !st.running) {
    clearInterval(poll);
    finished = true;
    F("cancelBtn").hidden = true;
    F("cancelBtn").disabled = false;
    usable = !st.problems.some((p) => /暫時不能開始使用/.test(p));
    // 有失敗或被停止：標題、按鈕、說明都講「還沒完成」，主按鈕是「重試」（直接用剛才的選擇再跑一次）
    failedRun = !!st.cancelled || st.problems.length > 0;
    F("runTitle").textContent = failedRun ? "安裝還沒完成" : "全部裝好了！";
    F("nextBtn").disabled = false;
    F("nextBtn").textContent = failedRun ? "重試" : "開始使用";
    F("skipBtn").hidden = !(failedRun && usable);
    F("prevBtn").hidden = false;
    F("navHint").textContent = !failedRun ? ""
      : st.cancelled ? "安裝已經停止。按「重試」會接著安裝，已經下載的部分不會重抓。"
      : usable ? "有幾項沒裝好（上面標紅色的）。按「重試」會再試一次；也可以先開始使用，沒裝好的功能之後再到「安裝與修復」補。"
      : "有幾項沒裝好，現在還不能開始使用。按「重試」再試一次，或按「上一步」回去改位置。";
  }
}

async function finish() {
  F("nextBtn").disabled = true;
  let r;
  try {
    r = await api("/api/setup/finish", { method: "POST" });
  } catch (e) {
    F("navHint").textContent = e.message;
    F("nextBtn").disabled = false;
    return;
  }
  F("runTitle").textContent = r.action === "restart" ? "重新啟動中…" : "開啟主畫面中…";
  // 伺服器換成正式畫面要一兩秒：等 /api/state 回應再換頁
  for (let i = 0; i < 120; i++) {
    await new Promise((res) => setTimeout(res, 500));
    try {
      const x = await fetch("/api/state", { cache: "no-store" });
      if (x.ok) { location.href = "/"; return; }
    } catch { /* 伺服器還在切換 */ }
  }
  F("navHint").textContent = "主畫面一直沒有開好，請關掉程式再開一次。";
}

// 正式畫面裡打開、而且已經在安裝中：直接跳到進度頁
function start() {
  return loadScan().then(async () => {
    const st = await api("/api/setup/status").catch(() => null);
    if (st?.running) {
      go(3);
    } else if (jumpModels && scan) {
      go(1);
    }
  });
}
start();

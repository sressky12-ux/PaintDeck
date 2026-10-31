/* ═══════════════════════════════════════════════════════════════════════════
   繪匣 PaintDeck的全站工具（給 MCP 用）。

   這個檔不是 FableCut 原本的：生圖站另外加的，mcp-server.js 只多兩行把它接進去
   （tools\fablecut_mcp.py 負責插；升級 FableCut 之後重跑那支就好）。
   跟生圖站溝通全部走它自己的 HTTP API（ANIMA_URL，預設 http://127.0.0.1:8190）；
   說明文件在同資料夾的 anima-guide.md（anima_docs 讀它）。

   規則：
   - 會花錢、刪資料、改設定的 API，沒帶 confirm:true 一律不送，回一段說明讓 AI 先問使用者。
   - 零相依（跟 FableCut 一樣只用 Node 內建模組）。
   ═══════════════════════════════════════════════════════════════════════════ */
"use strict";
const fs = require("fs");
const path = require("path");
const http = require("http");

const BASE = (process.env.ANIMA_URL || "http://127.0.0.1:8190").replace(/\/+$/, "");
const GUIDE = path.join(__dirname, "anima-guide.md");

// ---------------------------------------------------------------- HTTP
function request(method, url, { body, headers = {}, timeout = 120000 } = {}) {
  return new Promise((resolve, reject) => {
    const u = new URL(url.startsWith("http") ? url : BASE + url);
    const req = http.request(u, { method, headers, timeout }, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, type: String(res.headers["content-type"] || ""), buf: Buffer.concat(chunks) }));
    });
    req.on("timeout", () => req.destroy(new Error(`生圖站 ${timeout / 1000} 秒沒有回應`)));
    req.on("error", (e) => reject(
      e.code === "ECONNREFUSED" ? new Error(`連不到生圖站（${BASE}）：請使用者先打開「繪匣 PaintDeck」`) : e));
    if (body !== undefined) req.write(body);
    req.end();
  });
}

function errorOf(r) {
  const text = r.buf.toString("utf8");
  try { const j = JSON.parse(text); if (j && j.error) return j.error; } catch { /* 不是 JSON */ }
  return text.slice(0, 500) || `HTTP ${r.status}`;
}

/** JSON 進、JSON 出；HTTP 400 以上丟出生圖站給的錯誤訊息 */
async function api(method, p, data, opts = {}) {
  const body = data === undefined ? undefined : Buffer.from(JSON.stringify(data));
  const headers = body ? { "content-type": "application/json", "content-length": body.length } : {};
  const r = await request(method, p, { body, headers, ...opts });
  if (r.status >= 400) throw new Error(errorOf(r));
  const text = r.buf.toString("utf8");
  try { return JSON.parse(text); } catch { return text; }
}

const qs = (o) => {
  const e = Object.entries(o || {}).filter(([, v]) => v !== undefined && v !== null && v !== "");
  return e.length ? "?" + e.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(typeof v === "boolean" ? (v ? 1 : 0) : v)}`).join("&") : "";
};
const json = (v) => JSON.stringify(v, null, 2);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** MCP 的圖片區塊（Claude、Codex 看得到圖） */
async function imageBlock(p) {
  const r = await request("GET", p, { timeout: 60000 });
  if (r.status >= 400) throw new Error(errorOf(r));
  const mime = r.type.split(";")[0].trim() || "image/png";
  if (!mime.startsWith("image/")) throw new Error(`不是圖片（${mime}）`);
  return { type: "image", data: r.buf.toString("base64"), mimeType: mime };
}

// ---------------------------------------------------------------- 危險操作
// 沒帶 confirm:true 不送。GET 一律放行；DELETE 一律要確認。
const DANGER = [
  [/^\/api\/diagnostics\/(clear|preferences)$/, "永久清除已結束的診斷紀錄，或調整會自動刪除舊診斷的保留設定"],
  [/^\/api\/studio\/frames\/clear$/, "永久清除拆幀圖片、聲音及修幀紀錄，無法復原"],
  [/^\/api\/cache\/clear$/, "清除可重建快取與安裝暫存，未完成的下載可能需要重新下載"],
  [/^\/api\/app\/update\/(install|preferences)$/, "安裝新版並重新開啟程式，或更改自動檢查更新設定"],
  [/^\/api\/agent\/cloud\/start$/, "雲端開機會開始計費（價格先用 GET /api/agent/cloud 看，或看生圖站畫面）"],
  [/^\/api\/agent\/cloud\/stop$/, "雲端關機＝刪機，下次開機要重下約 32GB 的模型"],
  [/^\/api\/h3\/chain\/run$/, "H3 長片開拍：雲端一段一段拍，每 1 秒影片約 25 秒雲端計費（重拍也是）"],
  // 2026-10-29 盤點：這三條助手那邊（agent_tools.rs 的 DANGER）早就有，這裡漏了
  [/^\/api\/h3\/generate$/, "H3 影片（選雲端或 RunningHub 會花錢）"],
  [/^\/api\/runninghub\/config$/, "改 RunningHub 設定"],
  [/^\/api\/skills\/(install|bind|remove)$/, "安裝、綁定或移除外掛寫法"],
  [/^\/api\/agent\/cloud\/(config|idle)$/, "改雲端設定"],
  [/^\/api\/trash\/empty$/, "把回收區永久清空，救不回來"],
  [/^\/api\/reset(\/undo)?$/, "恢復原廠／復原（整個資料搬走）"],
  [/^\/api\/gallery\/prune_missing$/, "把檔案不見的紀錄從圖庫拿掉"],
  [/\/delete$/, "刪除"],
  [/^\/api\/jobs\/clear$/, "清除任務紀錄"],
  [/^\/api\/clear_queue$/, "清空等待中的任務"],
  [/^\/api\/settings(\/import)?$/, "改生圖站設定"],
  [/^\/api\/feature_flows$/, "改內建功能要用的流程"],
  [/^\/api\/pe\/prompts$/, "改 AI 改寫的系統提示詞"],
  [/^\/api\/models\/(dirs\/(add|remove)|scan\/apply|download|fetch_listed|import|place|classify|restart_comfy)$/, "改模型資料夾、下載或搬模型檔、重開 ComfyUI"],
  [/^\/api\/civitai\/download$/, "從 Civitai 下載檔案（大檔，會寫進模型資料夾）"],
  // 2026-10-29 下載中心：助手那邊（agent_tools.rs 的 DANGER）同一天一起加
  [/^\/api\/hub\/download$/, "從 C 站或魔塔下載模型檔（大檔，會寫進模型資料夾）"],
  [/^\/api\/hub\/modelscope$/, "改魔塔金鑰"],
  [/^\/api\/hub\/rh_run$/, "在 RunningHub 跑工作流（照秒扣 RH 幣）"],
  [/^\/api\/(image|video)\/workflows/, "新增或改動匯入的工作流"],
  [/^\/api\/voices\/tts_workflow$/, "換掉或拿掉聲音庫的 TTS 工作流"],
];

function danger(method, p, body) {
  if (method === "GET") return null;
  if (method === "DELETE") return "刪除";
  if (p === "/api/images/batch") {
    const a = body && body.action;
    return a === "trash" ? "把圖移到回收區" : a === "purge" ? "永久刪除圖片，救不回來" : null;
  }
  const hit = DANGER.find(([re]) => re.test(p));
  return hit ? hit[1] : null;
}

function needConfirm(what, method, p) {
  return `沒有送出：${method} ${p} 會「${what}」。\n` +
    "請先用白話跟使用者講清楚要做什麼（花錢的要講價格），使用者同意後，再把同一個請求加上 confirm:true 重送。";
}

// ---------------------------------------------------------------- 生成的預設值
// 跟生成頁 ui\js\gen.js 的 modelDefaults()／defaultNeg() 同一套；沒給的才補
const QWEN_NEG = "artifacts, gpt-image, washed-out colors, low quality, low resolution, AI slop, deviantart, sloppy lines, rough sketch, blurry, indistinct, missing fingers, badly drawn hands, wrong number of fingers";
const QWEN_ANIME_NEG = "low quality, low resolution, blurry, jpeg artifacts, washed-out colors, sloppy lines, messy line art, extra fingers, missing fingers, badly drawn hands, deformed anatomy, 3d render, photograph, photorealistic";
const MOSS_NEG = "worst quality, bad anatomy, watermark, jpeg artifact";

let stateCache = null, stateAt = 0;
async function state(fresh) {
  if (fresh || !stateCache || Date.now() - stateAt > 30000) {
    stateCache = await api("GET", "/api/state");
    stateAt = Date.now();
  }
  return stateCache;
}

/** 生圖站看檔頭判斷不出來（other／沒有）時，照檔名猜 */
function guessFamily(model) {
  const n = String(model || "").toLowerCase();
  if (/(^|[\\/_-])anima|silvermoon/.test(n)) return "anima";
  if (/qwen[_-]?image|qwen.?2\.?1/.test(n)) return "qwen21";
  if (/moss/.test(n)) return "moss";
  return "";
}

function familyOf(st, model) {
  if (!model) return "";
  if (model.startsWith("wf:")) return "custom";
  const f = st.models && st.models.unet_family && st.models.unet_family[model];
  return f && f !== "other" ? f : guessFamily(model) || f || "";
}

/** 選模型：沒指定就挑 Anima Turbo（沒有就第一個 anima）；影片、H3 的模型不挑 */
function pickModel(st, family) {
  const all = Object.keys((st.models && st.models.unet_family) || {}).filter((n) => !/h3|ltx|wan/i.test(n));
  const names = all.filter((n) => familyOf(st, n) === (family || "anima"));
  return names.find((n) => /anima-turbo/i.test(n)) || names.find((n) => /turbo/i.test(n)) || names[0] || "";
}

async function withDefaults(params) {
  const p = { ...params };
  const filled = {};
  const set = (k, v) => { if (p[k] === undefined || p[k] === null || p[k] === "") { p[k] = v; filled[k] = v; } };
  const st = await state();
  const mode = p.mode || "txt2img";
  if (mode === "video") { set("idea", "AI（MCP）"); return { p, filled, family: "video" }; }  // 影片照設定頁選的影片模型
  if (!p.model) set("model", pickModel(st, p.family));
  if (!p.model) throw new Error("找不到可用的模型：用 anima_status 看 models_by_family，在 params.model 指定一個");
  const fam = familyOf(st, p.model);
  const turbo = /turbo/i.test(p.model || "");
  set("mode", mode);
  if (fam === "moss") {
    const fast = (p.loras || []).some((l) => /moss_speed_dmd/i.test(l.name || ""));
    set("steps", fast ? 10 : 25); set("cfg", 3); set("sampler", "euler"); set("scheduler", "sgm_uniform");
    set("negative", MOSS_NEG);
    if (mode === "txt2img") { set("width", 1024); set("height", 1536); }
  } else if (fam === "qwen21") {
    set("steps", turbo ? 8 : 25); set("cfg", turbo ? 1 : 3); set("sampler", "euler"); set("scheduler", "simple");
    set("negative", /anime/i.test(p.model) ? QWEN_ANIME_NEG : QWEN_NEG);
    if (mode === "txt2img") { set("width", 1024); set("height", 1536); }
  } else if (fam === "anima") {
    if (turbo) { set("steps", 8); set("cfg", 1); set("sampler", "euler"); set("scheduler", "simple"); }
    else { set("steps", 30); set("cfg", 4); set("sampler", "er_sde"); set("scheduler", "simple"); }
    set("negative", (st.settings && st.settings.negative) || "");
    if (mode === "txt2img") { set("width", 832); set("height", 1216); }
  } else if (mode === "txt2img" && fam !== "custom") {
    set("width", 1024); set("height", 1024); set("steps", 25); set("cfg", 4); set("sampler", "euler"); set("scheduler", "simple");
  }
  set("idea", "AI（MCP）");
  return { p, filled, family: fam || "?" };
}

// ---------------------------------------------------------------- 任務
// db.rs 的 jobs.status：queued → running → done／error／cancelled／lost（ComfyUI 那邊找不到了）
const DONE = new Set(["done", "error", "cancelled", "lost"]);

async function jobsById(ids) {
  const rows = await api("GET", "/api/jobs" + qs({ limit: 300 }));
  const list = Array.isArray(rows) ? rows : rows.jobs || rows.items || [];
  const m = new Map(list.map((r) => [String(r.id), r]));
  return ids.map((id) => m.get(String(id)) || { id, status: "unknown" });
}

const jobLine = (j) => ({
  job: j.id, status: j.status, error: j.error || undefined,
  images: (j.images || []).filter((x) => !x.deleted).map((x) => x.id),
  seed: j.summary ? j.summary.seed : undefined,
});

async function waitJobs(ids, timeoutS) {
  const end = Date.now() + Math.max(5, Math.min(timeoutS || 600, 3600)) * 1000;
  let rows = await jobsById(ids);
  while (rows.some((j) => !DONE.has(j.status)) && Date.now() < end) {
    await sleep(2000);
    rows = await jobsById(ids);
  }
  const pending = rows.filter((j) => !DONE.has(j.status)).length;
  return { done: pending === 0, pending, jobs: rows.map(jobLine) };
}

async function looks(ids, max = 4) {
  const out = [];
  for (const id of ids.slice(0, max)) {
    try { out.push({ type: "text", text: `#${id}` }, await imageBlock(`/thumb/${id}`)); }
    catch (e) { out.push({ type: "text", text: `#${id} 縮圖拿不到：${e.message}` }); }
  }
  return out;
}

// ---------------------------------------------------------------- 上傳
async function upload(file, kind) {
  const buf = fs.readFileSync(file);
  const boundary = "----anima" + Math.random().toString(36).slice(2);
  const name = path.basename(file).replace(/"/g, "");
  const head = Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="${name}"\r\nContent-Type: application/octet-stream\r\n\r\n`);
  const tail = Buffer.from(`\r\n--${boundary}--\r\n`);
  const body = Buffer.concat([head, buf, tail]);
  const r = await request("POST", "/api/upload" + qs({ kind: kind || "image" }), {
    body, headers: { "content-type": `multipart/form-data; boundary=${boundary}`, "content-length": body.length }, timeout: 300000,
  });
  if (r.status >= 400) throw new Error(errorOf(r));
  return JSON.parse(r.buf.toString("utf8"));
}

// ---------------------------------------------------------------- 說明文件
function docs(section) {
  let md;
  try { md = fs.readFileSync(GUIDE, "utf8"); } catch { return "找不到 anima-guide.md（生圖站安裝不完整）"; }
  md = md.split("{{ANIMA_URL}}").join(BASE);
  const parts = md.split(/\n(?=## )/);
  if (!section) {
    const heads = parts.slice(1).map((p) => "- " + p.split("\n")[0].replace(/^## /, ""));
    return parts[0] + "\n\n## 目錄（anima_docs 帶 section＝標題裡的字，只讀那一段；section:\"all\" 讀全部）\n" + heads.join("\n");
  }
  if (section.toLowerCase() === "all") return md;
  const s = section.toLowerCase();
  const hit = parts.slice(1).filter((p) => p.split("\n")[0].toLowerCase().includes(s));
  return hit.length ? hit.join("\n") : `沒有標題含「${section}」的段落。目錄用 anima_docs（不帶 section）看。`;
}

// ---------------------------------------------------------------- 工具
const TOOLS = [
  {
    name: "anima_status",
    description: "繪匣 PaintDeck（使用者的本機生圖網站）：先呼叫這個。回傳 ComfyUI 有沒有連上、佇列、版本、可用的模型（依家族分）、LoRA、匯入的工作流、反推與 AI 改寫狀態、預設負面詞。生圖站沒開會回錯誤，請使用者先打開它。",
    inputSchema: { type: "object", properties: { loras: { type: "boolean", description: "列出全部 LoRA 的名稱、家族、觸發詞（預設只給數量）" } } },
  },
  {
    name: "anima_docs",
    description: "全站操作說明（中文）：每一頁能做什麼、生成的參數、各台（漫畫、導演、H3 影片、助手、剪輯、拆幀…）的 API 流程、提示詞寫法、規則。不帶 section＝總覽＋目錄；帶 section 只讀標題含那幾個字的段落（例如 \"生成\"、\"漫畫\"、\"H3\"、\"提示詞\"）。動手前先讀相關段落。",
    inputSchema: { type: "object", properties: { section: { type: "string" } } },
  },
  {
    name: "anima_generate",
    description: "送生成任務（生成頁那套：txt2img、img2img、inpaint、outpaint、multiref、upscale、strip、sheet、diff、detail、rembg、video）。params 照 POST /api/generate 的格式；model、steps、cfg、sampler、scheduler、width、height、negative 沒給會照模型家族補生成頁的預設（回傳會列出補了什麼）。wait=true（預設）等到畫完再回圖庫編號；look=true 順便回縮圖讓你看。詳細參數看 anima_docs section \"生成\"。",
    inputSchema: {
      type: "object",
      properties: {
        params: { type: "object", description: "生成參數：mode、prompt、model、loras:[{name,strength}]、seed（-1＝隨機）、init_image／refs／mask（先用 anima_upload 或 /api/to_input 放進輸入資料夾）…" },
        count: { type: "integer", description: "張數 1～32，種子會 +1 遞增（預設 1）" },
        wait: { type: "boolean", description: "等畫完（預設 true）" },
        timeout_s: { type: "integer", description: "最多等幾秒（預設 600）" },
        look: { type: "boolean", description: "畫完回縮圖（最多 4 張）" },
      },
      required: ["params"],
    },
  },
  {
    name: "anima_jobs",
    description: "看任務佇列：不帶 jobs＝最近的任務；帶 jobs（任務編號陣列）＝只看那幾個，wait=true 會等到它們都結束。結果有 status 和產生的圖庫編號。",
    inputSchema: {
      type: "object",
      properties: {
        jobs: { type: "array", items: { type: "string" } },
        wait: { type: "boolean" }, timeout_s: { type: "integer" },
        limit: { type: "integer", description: "不帶 jobs 時列幾筆（預設 15）" },
      },
    },
  },
  {
    name: "anima_gallery",
    description: "搜尋圖庫（最新的在前）：q 搜提示詞、mode（txt2img／img2img／video…）、model、family、lora、fav、from／to（Unix 秒）、trash（回收區）。回傳精簡清單：編號、時間、模式、尺寸、提示詞開頭、想法、我的最愛。要看圖用 anima_image。",
    inputSchema: {
      type: "object",
      properties: {
        q: { type: "string" }, mode: { type: "string" }, model: { type: "string" }, family: { type: "string" }, lora: { type: "string" },
        fav: { type: "boolean" }, trash: { type: "boolean" }, from: { type: "number" }, to: { type: "number" },
        sort: { type: "string", description: "new（預設）／old…" }, limit: { type: "integer", description: "預設 20，最多 100" }, offset: { type: "integer" },
      },
    },
  },
  {
    name: "anima_image",
    description: "看圖庫的一張圖或影片：回傳完整資料（提示詞、負面詞、種子、模型、參數），view=\"thumb\"（預設，小張）或 \"full\"（原圖，大）會附上圖片讓你看；影片只有封面縮圖。",
    inputSchema: {
      type: "object",
      properties: { id: { type: "integer" }, view: { type: "string", enum: ["none", "thumb", "full"] }, params: { type: "boolean", description: "附上完整生成參數（預設 true）" } },
      required: ["id"],
    },
  },
  {
    name: "anima_upload",
    description: "把這台電腦上的檔案放進生圖站的輸入資料夾（img2img、局部重繪、多圖參考、H3 素材要用）。回傳的 name 填進 params 的 init_image、refs[].image、mask。圖庫裡的圖不用上傳：POST /api/to_input {id}。",
    inputSchema: {
      type: "object",
      properties: { path: { type: "string", description: "絕對路徑" }, kind: { type: "string", description: "image（預設）／mask（只收圖；影片素材走 /api/h3/media 或 /api/video/media/upload）" } },
      required: ["path"],
    },
  },
  {
    name: "anima_api",
    description: "直接呼叫生圖站任何一個 HTTP API（其他工具沒包到的都用這個：漫畫台、導演台、H3 影片台、生圖助手、提示詞庫、LoRA、Civitai、AI 改寫、反推、拆幀台、模型與工作流、雲端…）。路徑和格式看 anima_docs。會花錢、刪除、改設定的請求要先問使用者，同意後帶 confirm:true，否則不會送出。回傳圖片的路徑（/img、/thumb、拆幀的 sheet）會直接附上圖。",
    inputSchema: {
      type: "object",
      properties: {
        method: { type: "string", enum: ["GET", "POST", "PUT", "DELETE"] },
        path: { type: "string", description: "例如 /api/comic/meta、/api/director/project/<id>" },
        query: { type: "object", description: "網址參數" },
        body: { type: "object", description: "POST 的 JSON" },
        confirm: { type: "boolean", description: "使用者已經同意這個會花錢／刪除／改設定的操作" },
        timeout_s: { type: "integer" },
      },
      required: ["method", "path"],
    },
  },
];

// ---------------------------------------------------------------- 執行
async function call(name, a) {
  switch (name) {
    case "anima_status": {
      const st = await state(true);
      const fams = (st.models && st.models.unet_family) || {};
      const byFam = {};
      for (const [n, f] of Object.entries(fams)) (byFam[f] = byFam[f] || []).push(n);
      const loras = (st.models && st.models.loras) || [];
      const lcount = {};
      for (const l of loras) lcount[l.family] = (lcount[l.family] || 0) + 1;
      const out = {
        station: BASE, version: st.version, comfyui_online: st.online, queue_remaining: st.remaining, running: st.running,
        models_by_family: byFam,
        family_names: st.models && st.models.family_names,
        lora_count_by_family: lcount,
        image_workflows: ((st.models && st.models.image_workflows) || []).map((w) => ({ id: w.id, name: w.name, modes: w.modes, family: w.family })),
        tagger: st.tagger, pe: st.pe,
        default_negative_anima: st.settings && st.settings.negative,
        tips: "先讀 anima_docs；生成用 anima_generate；其他功能用 anima_api。",
      };
      if (a.loras) out.loras = loras.map((l) => ({ name: l.name, family: l.family, trigger: l.trigger || l.triggers || undefined, title: l.title || undefined }));
      if (st.models && st.models.error) out.models_error = st.models.error;
      return json(out);
    }
    case "anima_docs":
      return docs(a.section);
    case "anima_generate": {
      if (!a.params || typeof a.params !== "object") throw new Error("要給 params");
      const { p, filled, family } = await withDefaults(a.params);
      const r = await api("POST", "/api/generate", { params: p, count: a.count || 1 });
      const ids = (r.jobs || []).map((j) => j.job);
      const head = { family, model: p.model, filled_defaults: filled, jobs: r.jobs, skipped_loras: r.skipped || undefined };
      if (a.wait === false) return json({ ...head, note: "已送出；用 anima_jobs {jobs, wait:true} 等結果" });
      const w = await waitJobs(ids, a.timeout_s || 600);
      const text = json({ ...head, ...w });
      if (!a.look) return text;
      const imgs = w.jobs.flatMap((j) => j.images);
      return [{ type: "text", text }, ...(await looks(imgs))];
    }
    case "anima_jobs": {
      if (Array.isArray(a.jobs) && a.jobs.length) {
        if (a.wait) return json(await waitJobs(a.jobs, a.timeout_s || 600));
        return json((await jobsById(a.jobs)).map(jobLine));
      }
      const rows = await api("GET", "/api/jobs" + qs({ limit: Math.min(a.limit || 15, 100) }));
      const list = Array.isArray(rows) ? rows : rows.jobs || rows.items || [];
      return json(list.map((j) => ({ ...jobLine(j), mode: j.summary && j.summary.mode, model: j.summary && j.summary.model,
        prompt: j.summary && String(j.summary.prompt || "").slice(0, 80), avg_seconds: j.avg_seconds })));
    }
    case "anima_gallery": {
      const r = await api("GET", "/api/gallery" + qs({
        q: a.q, mode: a.mode, model: a.model, family: a.family, lora: a.lora, fav: a.fav ? 1 : undefined, trash: a.trash ? 1 : undefined,
        from: a.from, to: a.to, sort: a.sort, limit: Math.min(a.limit || 20, 100), offset: a.offset,
      }));
      return json({
        total: r.total,
        items: (r.items || []).map((x) => ({
          id: x.id, time: x.created ? new Date(x.created * 1000).toLocaleString("zh-TW", { hour12: false }) : undefined,
          mode: x.mode, size: x.width && `${x.width}x${x.height}`, prompt: String(x.prompt || "").slice(0, 120),
          idea: x.idea || undefined, fav: x.fav ? true : undefined, missing: x.missing || undefined,
        })),
      });
    }
    case "anima_image": {
      const info = await api("GET", `/api/image/${a.id}`);
      if (a.params === false) delete info.params;
      else if (typeof info.params === "string") { try { info.params = JSON.parse(info.params); } catch { /* 原樣 */ } }
      const text = { type: "text", text: json(info) };
      const view = a.view || "thumb";
      if (view === "none") return [text];
      const video = /\.(mp4|webm|mov|mkv)$/i.test(info.filename || "");
      try { return [text, await imageBlock(view === "full" && !video ? `/img/${a.id}` : `/thumb/${a.id}`)]; }
      catch (e) { return [text, { type: "text", text: `圖片拿不到：${e.message}` }]; }
    }
    case "anima_upload": {
      if (!a.path || !fs.existsSync(a.path)) throw new Error(`找不到檔案：${a.path}`);
      return json(await upload(a.path, a.kind));
    }
    case "anima_api": {
      const method = String(a.method || "GET").toUpperCase();
      let p = String(a.path || "");
      if (!/^\/(api|img|thumb|input|lora_preview)\//.test(p)) throw new Error("path 要以 /api/、/img/、/thumb/、/input/ 開頭");
      p = p.split("?")[0];
      const what = danger(method, p, a.body);
      if (what && a.confirm !== true) return needConfirm(what, method, p);
      let body = a.body;
      if (what && ["/api/agent/cloud/start", "/api/app/update/install", "/api/cache/clear", "/api/diagnostics/clear", "/api/studio/frames/clear"].includes(p)) body = { ...(body || {}), confirm: true };
      const url = p + qs(a.query);
      const data = body === undefined ? (method === "GET" || method === "DELETE" ? undefined : {}) : body;
      const buf = data === undefined ? undefined : Buffer.from(JSON.stringify(data));
      const r = await request(method, url, {
        body: buf, headers: buf ? { "content-type": "application/json", "content-length": buf.length } : {},
        timeout: Math.min(a.timeout_s || 300, 3600) * 1000,
      });
      if (r.status >= 400) throw new Error(`HTTP ${r.status}：${errorOf(r)}`);
      if (r.type.startsWith("image/")) return [{ type: "image", data: r.buf.toString("base64"), mimeType: r.type.split(";")[0] }];
      const text = r.buf.toString("utf8");
      if (r.type.includes("event-stream")) return text.length > 20000 ? text.slice(-20000) : text;
      if (!r.type.includes("json") && !r.type.startsWith("text/")) return `（${r.type || "二進位"}，${r.buf.length} bytes，沒有顯示內容）`;
      try { return json(JSON.parse(text)); } catch { return text.slice(0, 50000); }
    }
    default:
      throw new Error("Unknown tool: " + name);
  }
}

module.exports = { TOOLS, call, BASE, danger, docs, withDefaults };

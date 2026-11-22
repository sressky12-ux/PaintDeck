import { el, api, toast } from "./util.js";
import { app } from "./state.js";

export function kohakuStageRows(view) {
  const stages = [["uv","下載 uv"],["source","下載 KohakuMoss"],["venv","建立環境"],["torch","安裝 PyTorch（約 2GB）"],["engine","安裝引擎"],["doctor","健檢"]];
  const active = stages.findIndex(([name]) => name === view?.stage);
  return stages.map(([name,label], i) => ({
    name, label, state: view?.busy && i === active ? "running" :
      view?.stages_done?.includes(name) || view?.installed || (view?.busy && i < active) ? "done" : "todo",
  }));
}

export function checkLights(checks) {
  const rows = checks?.rows || [];
  return { rows, canStart: rows.length > 0 && checks.ok_to_start !== false && rows.every(r => ["ok","warn"].includes(r.state)) };
}

export function phaseRows(run) {
  const current = run.phase_index || ["text","latents","train"].indexOf(run.phase) + 1;
  return ["整理描述","整理圖片","訓練"].map((label,i) => {
    const state = run.state === "done" || current > i + 1 ? "done" : run.state === "running" && current === i + 1 ? "running" : "todo";
    let text = "";
    if (i === 2 && state === "running" && !run.step) text = "第一次要編譯顯卡程式，約 1～5 分鐘，之後就快了";
    else if (i === 2 && run.total) text = `${run.step || 0} / ${run.total} 步`;
    return { label, state, text };
  });
}

export function etaText(run, speed) {
  const seconds = run.sec_per_step || speed;
  if (!(seconds > 0 && run.total > run.step)) return "";
  return `剩約 ${Math.max(1,Math.round((run.total - run.step) * seconds / 60))} 分鐘`;
}

const repo = "https://github.com/KohakuBlueleaf/KohakuMossTrainer";
const installConfirm = "KohakuMoss 是 KBlueLeaf 的開源訓練器（Apache 2.0＋命名條款），會下載到繪匣的訓練器資料夾（約下載 3GB、佔 6GB，15～30 分鐘）。";
const healFix = {action:"download_heal", label:"下載 heal（5.4GB）", confirm:"要下載 heal 底模（5.4GB）到 diffusion_models 嗎？"};
const rowNames = {gpu:"顯示卡",driver:"顯卡驅動",engine:"引擎",text_encoder:"文字編碼器",vae:"VAE",base:"底模",buildtools:"C++ 編譯工具",disk:"硬碟空間"};
const marks = {done:"✓",running:"▶",todo:"○",ok:"✓",warn:"⚠",bad:"✕"};
const note = text => el("div",{class:"muted sm"},text);
const box = (title,...kids) => el("section",{class:"box"},el("label",{},title),...kids);
const link = (text,href) => el("a",{href,target:"_blank",rel:"noopener noreferrer"},text);
const post = (path,body = {}) => api(path,{json:body});
const checksFor = base => api("/api/train/kohaku/checks" + (base ? "?base=" + encodeURIComponent(base) : ""));
function button(label, action, disabled = false) {
  return el("button",{type:"button",class:"akey sm",disabled,onclick:async e => {
    const b = e.currentTarget;
    b.disabled = true;
    try { await action(); } catch (err) { toast(err.message,"err"); } finally { b.disabled = disabled; }
  }},label);
}

async function repair(fix, progress) {
  const question = fix.action === "install_engine" ? installConfirm + "\n可以取消，下次會從已完成的階段接續。要安裝嗎？" :
    fix.confirm || (fix.action === "open_hub" ? "要前往繪匣的下載中心找 MOSS 文字編碼器嗎？" : "要開啟官方下載頁嗎？");
  if (!confirm(question)) return false;
  switch (fix.action) {
    case "install_engine": await post("/api/train/kohaku/install"); break;
    case "download_heal": {
      const {id} = await post("/api/train/kohaku/download_heal",{confirm:true});
      while (true) {
        const r = await api("/api/models/downloads");
        const d = r.downloads.find(item => item.id === id);
        if (!d || d.status === "error") throw new Error(d?.error || "找不到這次下載，請重新健檢。");
        if (d.status !== "run") break;
        progress(`正在下載、核對 heal（${Math.round(100 * (d.done || 0) / (d.total || 5413698934))}%）`);
        await new Promise(resolve => setTimeout(resolve,2000));
      }
      progress("heal 底模已就位");
      break;
    }
    case "install_buildtools": {
      const r = await post("/api/train/kohaku/buildtools",{confirm:true});
      if (!r.ok && r.url) { window.open(r.url,"_blank","noopener"); progress("請在微軟下載頁安裝，完成後按「重新健檢」。"); }
      else progress("已啟動微軟安裝程式，請留意系統權限確認。");
      break;
    }
    case "open_url": window.open(fix.url,"_blank","noopener"); break;
    case "open_hub": app.showPage("civitai"); break;
  }
  return true;
}

export function defaultBase(info) {
  const heal = info.bases.find(b => b.tested);
  return (!heal?.missing ? heal?.file : info.bases.find(b => !b.missing && /base/i.test(b.file))?.file) || info.params.base;
}

export function paramPanel(p, info, view, locked, save, reload) {
  const rec = info.params;
  const par = {...rec,...p.kohaku_params};
  const patch = values => save({kohaku_params:{...par,...values}});
  const level = p.level || "std";
  const choose = (list,val,action) => el("select",{disabled:locked,onchange:e => action(e.target.value)},
    ...list.map(([v,label]) => el("option",{value:v,selected:v === val},label)));
  const bases = info.bases.map(b => [b.file,b.tested ? `heal（推薦${b.missing ? "，尚未下載" : ""}）` : b.label]);
  if (!bases.some(([v]) => v === par.base)) bases.push([par.base,par.base + "（找不到）"]);
  const base = info.bases.find(b => b.file === par.base);
  const message = note("");
  const layer = par.vram === "auto" ? info.vram_layer : par.vram;
  const speed = view?.speed?.[`${info.gpu}|${layer}`];
  const field = (key,label,min,max,step,help) => el("label",{class:"tr-f"},label,
    el("input",{type:"number",value:par[key],min,max,step,disabled:locked,onchange:e => {
      if (e.target.reportValidity() && e.target.value !== "") patch({[key]:Number(e.target.value)});
    }}),note(help + (par[key] !== rec[key] ? `（推薦：${rec[key]}）` : "")));
  const layers = [["auto",`自動（這台對應 ${info.vram_layer}）`],...[6,8,12,16,24].map(n => [`vram_${n}g`,`${n}GB 模式${n === 6 ? "（沒實測）" : ""}`])];
  return [box("推薦設定",
    el("label",{class:"tr-f"},"煉在哪顆底模",choose(bases,par.base,v => patch({base:v}))),
    note(base?.tested ? "heal 是作者修過數值的版本。煉好的 LoRA 在 SFT、Base、Slivani 上都能用（實測）。" :
      /sft/i.test(par.base) ? "SFT 是調過美感的版本，在上面煉沒實測過" : "在這顆上煉沒實測過"),
    info.bases.some(b => b.tested && b.missing) ? button(healFix.label,async () => {
      if (await repair(healFix,s => { message.textContent = s; })) await reload();
    },locked) : null,message,
    el("label",{class:"tr-f"},"強度",choose([["light","輕"],["std","標準"],["full","充分"]],level,async l => {
      const {steps,why,...keep} = par;
      await save({level:l,kohaku_params:keep});
      await reload();
    })),
    el("div",{class:"tr-why"},par.why),note(`顯存模式：${par.vram === "auto" ? "自動 → " : ""}${layer}${layer === "vram_6g" ? "（沒實測）" : ""}`),
    note(speed ? `這台上次每步 ${speed} 秒，含約 5 分鐘整理資料，預估約 ${Math.ceil((speed * par.steps + 300) / 60)} 分鐘。` :
      `第一次煉完會記住這台的速度。以 8GB 實測每步 5.8 秒粗估，約 ${Math.ceil(par.steps * 5.8 / 60)}～${Math.ceil(par.steps * 5.8 / 60) + 10} 分鐘。`)),
    el("details",{class:"fold box"},el("summary",{},"進階設定"),el("div",{class:"tr-fs"},
      field("steps","總步數",1,20000,1,"訓練總共更新幾次。"),field("dim","容量（rank）",1,128,1,"越大能學越細，也越占空間。"),
      field("alpha","縮放（alpha）",1,128,0.1,"控制 LoRA 的更新幅度。"),field("lr","學習率",0.000001,0.01,"any","每一步調整的幅度。"),
      el("label",{class:"tr-f"},"顯存模式",choose(layers,par.vram,v => patch({vram:v}))),
      el("label",{},el("input",{type:"checkbox",checked:par.flip,disabled:locked,onchange:e => patch({flip:e.target.checked})}),"左右翻轉（角色造型不對稱時關閉）")),
      note("依作者配方使用 MOSS 時間取樣（shift 1.0），跳過第 22 層，約每四分之一進度存一版。"),
      button("改回推薦設定",() => save({level,kohaku_params:{base:defaultBase(info)}}),locked))];
}

export function installProgress(view) {
  return el("div",{class:"tr-install",id:"trKInstall"},
    ...kohakuStageRows(view).map(r => note(`${marks[r.state]} ${r.label}`)),
    view?.error ? el("div",{class:"tr-warn"},view.error) : null,
    view?.log?.length ? el("details",{class:"fold"},el("summary",{},"看詳細"),el("pre",{class:"tr-log"},view.log.slice(-15).join("\n"))) : null,
    view?.busy ? button("取消安裝",async () => {
      if (confirm("要取消安裝嗎？已完成的階段會保留，下次按安裝可以接著做。")) await post("/api/train/kohaku/cancel");
    }) : null);
}

export function engineCard(p, view, locked, controls) {
  const list = el("div",{class:"tr-install"});
  const reason = note("正在健檢…");
  const progress = note("");
  const go = el("button",{type:"button",class:"akey go",disabled:true,onclick:controls.start},"開始訓練");
  const option = (value,label) => el("label",{},el("input",{type:"radio",name:"trKQueue",checked:controls.wait === value,onchange:() => controls.setWait(value)}),label);
  let checking = false;
  async function refresh() {
    if (checking) return;
    checking = true;
    go.disabled = true;
    try {
      const checks = await checksFor(p.kohaku_params?.base);
      if (!list.isConnected) return;
      controls.onChecks(checks);
      const {rows,canStart} = checkLights(checks);
      list.replaceChildren(...rows.map(r => el("div",{class:"toolbar"},
        el("span",{class:r.state === "ok" ? "muted sm" : "tr-warn"},`${marks[r.state]} ${rowNames[r.id]}：${r.text}`),
        r.fix ? button(r.fix.label,async () => {
          if (await repair(r.fix,s => { progress.textContent = s; })) {
            await controls.reloadTrainer();
            await refresh();
          }
        },locked || (r.fix.action === "install_engine" && !!view?.busy)) : null,
        r.id === "buildtools" && r.state === "warn" ? button("先不裝",() => { progress.textContent = "可以繼續訓練。沒有 C++ 工具，會比較慢（待實測）。"; }) : null)));
      const bad = rows.filter(r => r.state === "bad").map(r => rowNames[r.id]);
      reason.textContent = bad.length ? `請先處理：${bad.join("、")}` : locked ? "專案目前有工作在跑。" : "健檢通過，可以開始。";
      go.disabled = locked || !canStart;
    } catch (e) { reason.textContent = `健檢失敗：${e.message}`; }
    finally { checking = false; }
  }
  refresh();
  return box("KohakuMoss 引擎（作者 KBlueLeaf）",el("div",{class:"toolbar"},link("原專案 ↗",repo),link("授權說明 ↗",repo+"/blob/main/LICENSE")),
    list,button("重新健檢",refresh),progress,installProgress(view),
    el("div",{class:"toolbar"},option(true,"等畫完自動開始"),option(false,"現在就開始"),go),reason,
    button("改用舊引擎（ai-toolkit，需要 16GB）",async () => {
      if (confirm("改用 ai-toolkit 在本機訓練？MOSS 需要約 16GB 顯存。")) await controls.switchEngine();
    },locked));
}

export function progressRows(run, checks) {
  return el("div",{class:"tr-install",id:"trKProgress"},
    run.state === "waiting" ? el("b",{},`等 ComfyUI 畫完（還有 ${run.queue || 0} 張）…`) : null,
    ...phaseRows(run).map(r => el("div",{},`${marks[r.state]} ${r.label}　${r.text}`)),
    note([run.sec_per_step ? `每步 ${run.sec_per_step} 秒` : "",etaText(run),run.vram ? `顯存 ${run.vram}` : ""].filter(Boolean).join("　")),
    checks?.rows?.some(r => r.id === "buildtools" && r.state === "warn") ? note("沒有 C++ 工具，會比較慢（待實測）。") : null);
}

export function historyPanel(history) {
  if (!history?.length) return null;
  return el("details",{class:"fold box"},el("summary",{},"之前的輪次"),
    ...history.slice().reverse().map(r => el("div",{},el("b",{},`第 ${r.run_no} 輪`),
      note(r.ended ? new Date(r.ended * 1000).toLocaleString("zh-TW") : "結束時間未記錄"),
      ...(r.epochs?.length ? r.epochs.map(e => note(`第 ${e.n} 版${e.step ? `（${e.step} 步）` : ""}　${e.file}`)) : [note("這輪還沒存出版本")]))));
}

async function copyDiagnostics(p, run) {
  const [trainer,checks] = await Promise.all([api("/api/train/trainer").catch(() => null),checksFor(p.kohaku_params?.base).catch(() => null)]);
  const rows = checks?.rows || [];
  const none = "（沒有）";
  const text = [
    `引擎 commit：${trainer?.engines?.kohakumoss?.commit || none}`,
    `顯卡：${rows.find(r => r.id === "gpu")?.text || none}`,
    `doctor.json missing 原文：${none}`,
    `健檢列：${rows.filter(r => ["engine","buildtools"].includes(r.id)).map(r => r.text).join("\n") || none}`,
    `out\\train.py：${none}`, `out\\data.py：${none}`,
    `最後 50 行 log：\n${run.log?.slice(-50).join("\n") || none}`, `訓練錯誤：${run.error || none}`,
  ].join("\n");
  await navigator.clipboard.writeText(text);
  toast("已複製診斷資訊");
}

export function errorPanel(p, run, actions) {
  const kind = run.error_kind || "other";
  const messages = {
    oom:"顯存不夠。改用更省的模式（6GB 模式）重來？會再慢一些。",
    missing_model:"找不到模型，請到訓練前健檢查看缺少哪個檔案。",
    driver:"PyTorch 看不到顯卡，先更新 NVIDIA 驅動。",
    disk:"訓練磁碟空間不足。請在健檢查看剩餘空間，清出空間再開始。",
    other:"訓練停了，請查看下方的錯誤訊息。",
  };
  return box("這次訓練停止了",el("div",{class:"tr-warn"},messages[kind] || messages.other),note(run.error || ""),
    el("div",{class:"toolbar"},
      kind === "oom" ? button("改用省顯存模式重來",actions.economy) : null,
      ["missing_model","disk"].includes(kind) ? button("回健檢",actions.check) : null,
      kind === "driver" ? link("開 NVIDIA 驅動下載頁","https://www.nvidia.com/Download/index.aspx") : null,
      kind === "other" ? button("重試",actions.retry) : null,
      button("複製診斷資訊",() => copyDiagnostics(p,run))));
}

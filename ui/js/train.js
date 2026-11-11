// 訓練頁（煉 LoRA）：六步精靈。每一步後端都先自動做好，這裡給人看一眼、想改就改。
// 後端：/api/train/*（server\api_train.rs、train_run.rs）；設計：design\訓練台_設計.md
import { el, api, toast } from "./util.js";
import { app } from "./state.js";
import * as gen from "./gen.js";
import { pickFromGallery } from "./gallery.js";

const STEPS = ["專案", "訓練圖", "描述", "參數", "在哪煉", "訓練與驗收"];
const KINDS = [["style", "畫風", "學一種畫法：線條、上色、質感"], ["character", "角色", "學一個固定的人物"], ["concept", "概念／服裝／物件", "學一件衣服、一個道具、一種構圖"]];
const LEVELS = [["light", "輕", "學得淡一點，比較不會蓋掉底模"], ["std", "標準", "多數情況用這個"], ["full", "充分", "學得重一點，圖少或特徵難抓時用"]];
const TARGETS = [
  ["local", "本機顯卡", "用這台電腦煉，不用錢、圖不出門"],
  ["cloud", "雲端顯卡", "租一台 RunPod 的機器，或連你自己的訓練機"],
  ["civitai", "C 站", "交給 Civitai 煉，用 Buzz 付"],
  ["modelscope", "魔塔", "實驗性：包好資料集，到魔塔網頁送出"],
];

const S = { list: [], fams: [], trainer: null, vram: 0, v: null, step: 1, target: "local", cloud: "runpod", prog: null, gpus: null, quote: null, cmp: null, traits: null, auto: null, draft: { name: "", kind: "style", family: "anima", trigger: "" }, loaded: false };

// ================================================================ 不靠畫面的判斷（tests/train_ui.test.cjs）

export function liveFiles(v) {
  return ((v && v.project && v.project.images) || []).filter((i) => !i.off).map((i) => i.file);
}

// 這一步現在能不能進：能＝空字串，不能＝原因
export function blockReason(step, v) {
  if (step <= 1) return "";
  if (!v || !v.project) return "先在第一步建立專案";
  if (step === 2) return "";
  const files = liveFiles(v);
  if (!files.length) return "先放訓練圖";
  if (step === 3) return "";
  const empty = files.filter((f) => !String((v.captions || {})[f] || "").trim()).length;
  if (empty) return `還有 ${empty} 張沒有描述`;
  if (step <= 5) return "";
  return v.project.run ? "" : "還沒開始訓練";
}

export function needsConfirm(target) {
  return ["runpod", "civitai", "modelscope"].includes(target);
}

// 開始訓練要送的內容；花錢的地方沒確認過＝null（不送）
export function startBody(target, opts, confirmed) {
  if (needsConfirm(target) && !confirmed) return null;
  const b = { target };
  for (const [k, val] of Object.entries(opts || {})) if (val !== undefined && val !== null && val !== "") b[k] = val;
  if (needsConfirm(target)) b.confirm = true;
  return b;
}

// 把一則進度事件併進現在的進度（沒帶的欄位留著）
export function applyEvent(run, m) {
  const r = { state: "", step: 0, total: 0, loss: null, log: [], ...(run || {}) };
  if (m.state) r.state = m.state;
  for (const k of ["step", "total", "loss", "log"]) if (m[k] !== undefined && m[k] !== null) r[k] = m[k];
  r.msg = m.msg || "";
  return r;
}

export function percent(run) {
  if (!run || !(run.total > 0)) return 0;
  return Math.max(0, Math.min(100, Math.round((run.step / run.total) * 100)));
}

export function stateName(s) {
  const names = { starting: "準備中", running: "訓練中", done: "煉好了", error: "失敗", stopped: "停止了", interrupted: "中斷了", detached: "還在外面跑" };
  return s ? names[s] || s : "還沒開始";
}

// ================================================================ 讀寫

const F = (id) => document.getElementById(id);
const fam = (id) => S.fams.find((f) => f.id === id);
const cur = () => S.v && S.v.project;
const pid = () => cur() && cur().id;
const busyKind = () => S.v && S.v.running;

async function call(path, json, method) {
  try {
    return await api(path, json === undefined ? { method: method || "GET" } : { json, method });
  } catch (e) {
    toast(e.message, "err");
    return null;
  }
}

async function loadList() {
  const r = await call("/api/train");
  if (!r) return;
  Object.assign(S, { list: r.projects, fams: r.families, trainer: r.trainer, vram: r.vram_gb, loaded: true });
}

async function open(id, step) {
  const v = id ? await call(`/api/train/${id}`) : null;
  S.v = v;
  S.prog = v && v.project.run ? applyEvent(null, v.project.run) : null;
  S.quote = null; S.cmp = null; S.traits = null; S.auto = null;
  if (v) localStorage.setItem("trainProject", id);
  S.step = step || (v ? firstOpenStep(v) : 1);
  render();
}

// 打開專案時停在哪一步：做到哪就到哪
function firstOpenStep(v) {
  if (v.project.run) return 6;
  for (const s of [5, 4, 3, 2]) if (!blockReason(s, v)) return s === 5 && !v.project.params ? 4 : s;
  return 2;
}

function setView(v) {
  if (!v || !v.project) return;
  S.v = { ...S.v, ...v };
  render();
}

export async function show() {
  await loadList();
  if (!S.v) {
    const last = localStorage.getItem("trainProject");
    const hit = S.list.find((p) => p.id === last) || S.list[0];
    await open(hit ? hit.id : null);
  } else render();
}

// ================================================================ 畫面

function render() {
  renderList();
  renderSteps();
  const body = F("trBody");
  body.replaceChildren(...[null, step1, step2, step3, step4, step5, step6][S.step]());
  const next = S.step < 6 ? blockReason(S.step + 1, S.v) : "";
  F("trPrev").disabled = S.step <= 1;
  F("trNext").hidden = S.step >= 6 || (S.step === 1 && !cur());
  F("trNext").disabled = !!next;
  F("trHint").textContent = S.step < 6 && cur() ? next : "";
  const p = cur();
  F("trStatus").textContent = !p ? "待機" : busyKind() === "caption" ? "寫描述中" : S.prog ? `${stateName(S.prog.state)}${S.prog.state === "running" && S.prog.total ? ` ${percent(S.prog)}%` : ""}` : "待機";
  F("trDel").disabled = !p || !!busyKind();
}

function renderList() {
  const box = F("trList");
  box.replaceChildren(
    ...S.list.map((p) => {
      const f = fam(p.family);
      const run = p.running === "train" || p.state === "running" || p.state === "starting";
      return el("button", { class: `tr-proj${p.id === pid() ? " on" : ""}`, type: "button", onclick: () => open(p.id) },
        el("span", { class: `led${run ? " run" : p.state === "done" ? " ok" : p.state === "error" ? " bad" : ""}` }),
        el("b", {}, p.name),
        el("span", { class: "muted sm" }, `${f ? f.label : p.family}・${p.images} ${f && f.video ? "段" : "張"}・${stateName(p.state)}${p.adopted ? "・已採用" : ""}`));
    }),
  );
  if (!S.list.length) box.append(el("div", { class: "muted sm" }, "還沒有訓練專案。按上面的「新專案」開始。"));
}

function renderSteps() {
  F("trSteps").replaceChildren(
    ...STEPS.map((name, i) => {
      const n = i + 1;
      const why = blockReason(n, S.v);
      return el("button", { type: "button", role: "tab", class: n === S.step ? "on" : "", disabled: !!why && n !== S.step, title: why, onclick: () => { S.step = n; render(); } },
        el("span", { class: "n" }, String(n)), name);
    }),
  );
}

const box = (title, ...kids) => el("section", { class: "box" }, el("label", {}, title), ...kids);
const note = (text) => el("div", { class: "muted sm" }, text);
const keys = (items, value, on) => el("div", { class: "tr-keys" }, ...items.map(([v, name, tip]) => el("button", { type: "button", class: `akey sm${v === value ? " on" : ""}`, title: tip || "", onclick: () => on(v) }, name)));

// ---------------------------------------------------------------- 1 專案
function step1() {
  const p = cur();
  const d = p || S.draft;
  const f = fam(d.family);
  const name = el("input", { type: "text", value: d.name, maxlength: 60, placeholder: "例：我的水彩畫風" });
  const trig = el("input", { type: "text", value: d.trigger || "", maxlength: 32, spellcheck: "false", placeholder: p ? "" : "留空＝自動產生一個不會撞字的" });
  const save = async (patch) => {
    if (!p) { Object.assign(S.draft, patch); render(); return; }
    const v = await call(`/api/train/${p.id}`, patch);
    if (v) { setView(v); await loadList(); renderList(); } else render();
  };
  name.onchange = () => save({ name: name.value.trim() });
  trig.onchange = () => save({ trigger: trig.value.trim() });
  const famSel = el("select", { disabled: !!p }, ...S.fams.map((x) => el("option", { value: x.id, selected: x.id === d.family }, x.label)));
  famSel.onchange = () => save({ family: famSel.value });
  const out = [
    box("專案名字", name),
    box("煉什麼", keys(KINDS, d.kind, (k) => save({ kind: k })), note(KINDS.find((k) => k[0] === d.kind)[2])),
    box("用在哪個模型", famSel,
      f ? note(`${f.base_note}。描述用${f.caption === "tags" ? " Danbooru tag" : "英文句子"}；本機至少要 ${f.min_vram_gb}GB 顯存${S.vram ? `（這台 ${S.vram}GB）` : ""}。`) : null,
      p ? note("模型家族建好就不能改（訓練圖的整理方式和描述寫法都跟著它）。") : null),
    box("觸發詞", trig, note("生圖時打這個詞就會叫出這顆 LoRA。只能用英文、數字、底線、連字號；要夠怪，不要用常見的字（會跟模型本來認得的東西打架）。")),
  ];
  if (!p) {
    out.push(el("div", { class: "toolbar" }, el("button", { class: "akey", type: "button", onclick: async (e) => {
      S.draft.name = name.value.trim(); S.draft.trigger = trig.value.trim();
      if (!S.draft.name) return toast("先取個名字", "err");
      e.target.disabled = true;
      const v = await call("/api/train", S.draft);
      if (!v) return void (e.target.disabled = false);
      S.draft = { name: "", kind: "style", family: S.draft.family, trigger: "" };
      await loadList();
      await open(v.project.id, 2);
    } }, "建立專案")));
  }
  return out;
}

// ---------------------------------------------------------------- 2 訓練圖
const toB64 = (file) => new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(String(r.result).split(",")[1] || ""); r.onerror = () => rej(r.error); r.readAsDataURL(file); });

async function addFiles(files) {
  const id = pid();
  let added = 0; const skipped = [];
  for (const [i, f] of files.entries()) {
    F("trStatus").textContent = `加入 ${i + 1}/${files.length}`;
    const v = await call(`/api/train/${id}/images`, { file_name: f.name, data_b64: await toB64(f) });
    if (!v) break;
    added += v.added; skipped.push(...v.skipped); S.v = { ...S.v, ...v };
  }
  afterAdd(added, skipped);
}

function afterAdd(added, skipped) {
  if (skipped.length) toast(`加了 ${added} 個，${skipped.length} 個沒收：${skipped.slice(0, 3).map((s) => `${s.name}（${s.why}）`).join("；")}`, "err");
  else if (added) toast(`加了 ${added} 個`);
  loadList().then(render);
}

function step2() {
  const p = cur();
  const f = fam(p.family);
  const video = f && f.video;
  const rep = S.v.report || {};
  const lock = !!busyKind();
  const pick = el("input", { type: "file", multiple: true, hidden: true, accept: video ? "image/*,video/*" : "image/*" });
  pick.onchange = () => pick.files.length && addFiles([...pick.files]);
  const folder = el("input", { type: "text", placeholder: "貼上資料夾或檔案的路徑，例：D:\\圖\\我的畫風", spellcheck: "false" });
  const addPath = async () => {
    const t = folder.value.trim().replace(/^"|"$/g, "");
    if (!t) return;
    const v = await call(`/api/train/${p.id}/images`, { paths: [t] });
    if (v) { S.v = { ...S.v, ...v }; folder.value = ""; afterAdd(v.added, v.skipped); }
  };
  const drop = el("div", { class: "tr-drop" }, video ? "把圖片或短片拖到這裡" : "把圖片拖到這裡", el("div", { class: "muted sm" }, "會複製一份進專案、縮到約 100 萬像素（不放大）。原圖不會動。"));
  drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => { e.preventDefault(); drop.classList.remove("over"); const fs = [...e.dataTransfer.files]; if (fs.length && !lock) addFiles(fs); });
  const dupes = new Set((rep.dupes || []).flat());
  const grid = el("div", { class: "tr-grid" }, ...p.images.map((im) => {
    const tags = [im.small ? "偏小" : "", dupes.has(im.file) ? "重複" : ""].filter(Boolean);
    return el("div", { class: `tr-img${im.off ? " off" : ""}`, title: `${im.src_name || im.file}${im.src_w ? `（原圖 ${im.src_w}×${im.src_h}）` : ""}` },
      im.video ? el("div", { class: "tr-vid" }, "影片") : el("img", { src: `/api/train/${p.id}/img/${im.file}`, loading: "lazy", alt: im.file }),
      tags.length ? el("span", { class: "tr-flag" }, tags.join("・")) : null,
      el("div", { class: "tr-img-bar" },
        el("button", { type: "button", disabled: lock, title: im.off ? "勾回來" : "先不用這張（不會刪）", onclick: async () => setView(await call(`/api/train/${p.id}/images/set`, { files: [im.file], off: !im.off })) }, im.off ? "不用" : "用"),
        el("button", { type: "button", disabled: lock, title: "從專案移除（原圖不會動）", onclick: async () => { const v = await call(`/api/train/${p.id}/images/remove`, { files: [im.file] }); if (v) { setView(v); loadList().then(renderList); } } }, "✕")));
  }));
  const sug = rep.suggest || [0, 0];
  return [
    box("加訓練圖", drop, pick,
      el("div", { class: "toolbar" },
        el("button", { class: "akey sm", type: "button", disabled: lock, onclick: () => pick.click() }, "選檔案"),
        el("button", { class: "akey sm", type: "button", disabled: lock || video, onclick: () => pickFromGallery(async (rec) => { const v = await call(`/api/train/${p.id}/images`, { gallery_ids: [rec.id] }); if (v) { S.v = { ...S.v, ...v }; afterAdd(v.added, v.skipped); } }, { title: "從圖庫挑一張當訓練圖" }) }, "從圖庫挑")),
      el("div", { class: "toolbar" }, folder, el("button", { class: "akey sm", type: "button", disabled: lock, onclick: addPath }, "加入這個路徑"))),
    box(`現在 ${rep.count || 0} ${video ? "段" : "張"}（煉${KINDS.find((k) => k[0] === p.kind)[1]}建議 ${sug[0]}～${sug[1]}）`,
      ...(rep.warnings || []).map((w) => el("div", { class: "tr-warn" }, w)),
      rep.buckets && Object.keys(rep.buckets).length ? note(`長寬比：${Object.entries(rep.buckets).map(([k, n]) => `${k} × ${n}`).join("、")}`) : null,
      grid),
  ];
}

// ---------------------------------------------------------------- 3 描述
async function loadTraits() {
  const r = await call(`/api/train/${pid()}/traits`);
  S.traits = r ? r.traits : [];
  render();
}

function step3() {
  const p = cur();
  const f = fam(p.family);
  const tags = f && f.caption === "tags";
  const files = liveFiles(S.v);
  const empty = files.filter((x) => !(S.v.captions[x] || "").trim()).length;
  const running = busyKind() === "caption";
  const go = async (only_empty) => {
    if (!only_empty && !confirm("全部重寫會蓋掉你改過的描述，確定？")) return;
    const r = await call(`/api/train/${p.id}/caption`, { mode: "auto", only_empty });
    if (r && r.total) { S.v.running = "caption"; render(); } else if (r) toast(r.msg || "沒有要寫的");
  };
  const word = el("input", { type: "text", placeholder: tags ? "一個 tag，例：white background" : "一小段英文，例：plain white background", spellcheck: "false" });
  const bulk = async (body) => { const v = await call(`/api/train/${p.id}/captions`, body); if (v) { S.traits = null; setView(v); } };
  if (tags && p.kind === "character" && S.traits === null && !empty && !running) loadTraits();
  const traits = (S.traits || []).filter((t) => files.some((x) => (S.v.captions[x] || "").toLowerCase().split(",").some((c) => c.trim().replace(/_/g, " ") === t.tag)));
  const rows = el("div", { class: "tr-caps" }, ...p.images.filter((im) => !im.off).map((im) => {
    const ta = el("textarea", { rows: tags ? 3 : 4, spellcheck: "false", disabled: running }, S.v.captions[im.file] || "");
    ta.onchange = async () => { const v = await call(`/api/train/${p.id}/captions`, { file: im.file, text: ta.value }); if (v) { S.v.captions = v.captions; ta.value = v.captions[im.file]; renderSteps(); render(); } };
    return el("div", { class: "tr-cap" }, im.video ? el("div", { class: "tr-vid" }, "影片") : el("img", { src: `/api/train/${p.id}/img/${im.file}`, loading: "lazy", alt: im.file }), ta);
  }));
  return [
    box("自動寫描述",
      note(tags ? "用站內的反推（WD tagger）看圖打 tag，再自動整理：觸發詞放最前面、簽名浮水印這類雜訊拿掉。"
        + (p.kind === "style" ? "煉畫風：畫風、畫師、品質這類詞會拿掉（留著，這些詞就會把畫風分走）。" : p.kind === "character" ? "煉角色：角色名不寫，交給觸發詞。" : "")
        : "用站內的語言模型看圖寫一小段英文。第一次要載入模型，約 1～2 分鐘。" + (f && f.video ? "影片目前要自己寫：講內容、動作、鏡頭怎麼動。" : "")),
      el("div", { class: "toolbar" },
        el("button", { class: "akey", type: "button", disabled: running || !empty, onclick: () => go(true) }, empty ? `寫還沒寫的 ${empty} 張` : "每張都寫好了"),
        el("button", { class: "akey sm", type: "button", disabled: running, onclick: () => go(false) }, "全部重寫"),
        running ? el("button", { class: "akey sm warn", type: "button", onclick: () => call(`/api/train/${p.id}/stop`, {}) }, "停止") : null,
        el("span", { class: "muted sm", id: "trCapProg" }, running ? "寫描述中…" : ""))),
    traits.length ? box("這個角色每張都有的特徵",
      note("這些外觀幾乎每張都出現。煉角色時建議從描述拿掉，讓觸發詞把它們一起記住（不然以後生圖還得自己再打一次）。"),
      el("div", { class: "tr-keys" }, ...traits.map((t) => el("span", { class: "tr-chip" }, `${t.tag}（${Math.round(t.ratio * 100)}%）`))),
      el("div", { class: "toolbar" }, el("button", { class: "akey sm", type: "button", disabled: running, onclick: () => bulk({ drop: traits.map((t) => t.tag) }) }, "全部拿掉"))) : null,
    box("一次改全部", el("div", { class: "toolbar" }, word,
      el("button", { class: "akey sm", type: "button", disabled: running, onclick: () => word.value.trim() && bulk({ add: word.value.trim() }) }, "全部加上"),
      el("button", { class: "akey sm", type: "button", disabled: running, onclick: () => word.value.trim() && bulk({ drop: [word.value.trim()] }) }, "全部拿掉"))),
    box(`每一張的描述（${files.length - empty}/${files.length}）`, note("改完點別的地方就會存。觸發詞會自動留在最前面。"), rows),
  ].filter(Boolean);
}

// ---------------------------------------------------------------- 4 參數
async function loadAuto(level) {
  const r = await call(`/api/train/${pid()}/params?level=${level}`);
  if (!r) return;
  S.auto = r;
  // 還沒存過就先存自動算的，之後使用者改什麼存什麼
  if (!cur().params) setView(await call(`/api/train/${pid()}`, { level, params: {}, samples: r.samples }));
  else render();
}

function step4() {
  const p = cur();
  if (!S.auto) { loadAuto(p.level || "std"); return [note("計算中…")]; }
  const par = p.params || S.auto.params;
  const lock = !!busyKind();
  const save = async (patch) => setView(await call(`/api/train/${p.id}`, patch));
  const num = (key, label, attrs, tip) => {
    const i = el("input", { type: "number", value: par[key], disabled: lock, ...attrs });
    i.onchange = () => save({ params: { ...par, [key]: Number(i.value) } });
    return el("label", { class: "tr-f", title: tip || "" }, label, i);
  };
  const res = el("input", { type: "text", value: (par.resolution || []).join(", "), disabled: lock });
  res.onchange = () => save({ params: { ...par, resolution: res.value.split(/[,\s]+/).map(Number).filter(Boolean) } });
  const flip = el("input", { type: "checkbox", checked: !!par.flip, disabled: lock });
  flip.onchange = () => save({ params: { ...par, flip: flip.checked } });
  const samples = (p.samples && p.samples.length ? p.samples : S.auto.samples).slice(0, 4);
  const sampleBoxes = samples.map((s, i) => {
    const t = el("textarea", { rows: 2, spellcheck: "false", disabled: lock }, s);
    t.onchange = () => { const all = [...samples]; all[i] = t.value; save({ samples: all }); };
    return t;
  });
  return [
    box("煉多重",
      keys(LEVELS, p.level || "std", (l) => { S.auto = null; save({ level: l, params: {} }).then(() => loadAuto(l)); }),
      el("div", { class: "tr-why" }, par.why || ""),
      note("步數是照「每張圖看幾次」算的，不是越多越好：看太多次只會把訓練圖背起來。")),
    box("存幾版", num("epochs", "版數", { min: 1, max: 30, step: 1 }),
      note("最好的常常不是最後一版。每一版都會留下來，煉完用對照表挑。")),
    el("details", { class: "fold box" }, el("summary", {}, "進階（不確定就不用動）"),
      el("div", { class: "tr-fs" },
        num("steps", "總步數", { min: 50, max: 20000, step: 50 }),
        num("rank", "容量（rank）", { min: 1, max: 128, step: 1 }, "越大學得越細，檔案也越大；太大容易背圖"),
        num("lr", "學習率", { min: 0.000001, max: 0.01, step: 0.00001 }),
        num("caption_dropout", "描述留空機率", { min: 0, max: 0.5, step: 0.01 }, "偶爾不給描述，讓 LoRA 沒打詞也有一點效果"),
        el("label", { class: "tr-f" }, "解析度", res),
        el("label", { class: "tr-f row" }, flip, "左右翻轉（角色不對稱時要關）")),
      el("div", { class: "toolbar" }, el("button", { class: "akey sm", type: "button", disabled: lock, onclick: () => { S.auto = null; save({ level: p.level || "std", params: {} }).then(() => loadAuto(p.level || "std")); } }, "全部改回自動算的"))),
    box("訓練中途的樣張", note("每存一版就用這幾句畫一張，看學得怎麼樣。故意用訓練圖裡沒有的題材：畫得出來才是真的學會，不是背圖。"), ...sampleBoxes),
  ];
}

// ---------------------------------------------------------------- 5 在哪煉
function trainerBox() {
  const t = S.trainer || {};
  if (t.installed && !t.busy) return note("訓練器已經裝好。");
  return el("div", { class: "tr-install" },
    t.busy ? note("正在安裝訓練器…可以先去做別的，裝好這裡會變。") : note(`本機要先裝訓練器（ai-toolkit 和它自己的 PyTorch，約下載 ${t.download_gb || 8}GB，裝在繪匣的資料夾裡，不會動到 ComfyUI）。`),
    t.error ? el("div", { class: "tr-warn" }, `上次安裝失敗：${t.error}`) : null,
    el("div", { class: "toolbar" }, el("button", { class: "akey", type: "button", disabled: !!t.busy, onclick: async () => { if (await call("/api/train/trainer/install", {})) { S.trainer = { ...t, busy: true, error: "", log: [] }; render(); } } }, t.busy ? "安裝中…" : "安裝訓練器")),
    (t.log || []).length ? el("pre", { class: "tr-log", id: "trInstallLog" }, t.log.join("\n")) : el("pre", { class: "tr-log", id: "trInstallLog", hidden: true }));
}

async function startTrain(target, opts, paidText) {
  const p = cur();
  const confirmed = needsConfirm(target) ? confirm(paidText) : true;
  const body = startBody(target, opts, confirmed);
  if (!body) return;
  const r = await call(`/api/train/${p.id}/start`, body);
  if (!r) return;
  if (r.manual) { S.quote = { ms: r }; render(); return; }
  S.prog = applyEvent(null, { state: "starting", msg: "準備中…" });
  await open(p.id, 6);
  loadList().then(renderList);
}

function step5() {
  const p = cur();
  const f = fam(p.family);
  const steps = (p.params && p.params.steps) || 0;
  const running = busyKind() === "train";
  const cards = el("div", { class: "tr-targets" }, ...TARGETS.map(([id, name, tip]) => el("button", { type: "button", class: `tr-target${S.target === id ? " on" : ""}`, onclick: () => { S.target = id; S.quote = null; render(); } }, el("b", {}, name), el("span", { class: "muted sm" }, tip))));
  let detail;
  if (S.target === "local") {
    const enough = !S.vram || S.vram + 0.5 >= f.min_vram_gb;
    const sdxl = p.family === "sdxl" ? Object.entries((app.state && app.state.models && app.state.models.unet_family) || {}).filter(([, fm]) => fm === "sdxl").map(([n]) => n) : [];
    const model = p.family === "sdxl" ? el("select", {}, el("option", { value: "" }, "SDXL 官方底模（第一次會下載約 7GB）"), ...sdxl.map((n) => el("option", { value: n }, n))) : null;
    if (model && sdxl.length) model.value = sdxl[0];
    detail = box("本機顯卡",
      enough ? note(`這台 ${S.vram || "?"}GB 顯存，煉 ${f.label} 至少要 ${f.min_vram_gb}GB：可以。訓練的時候顯卡會被占滿，先不要生圖。`)
        : el("div", { class: "tr-warn" }, `這台 ${S.vram}GB 顯存不夠煉 ${f.label}（至少 ${f.min_vram_gb}GB）。改用雲端顯卡或 C 站。`),
      f.gate ? el("div", { class: "tr-warn" }, `這個底模要先到 Hugging Face 按同意才能下載：${f.gate}`) : null,
      model ? el("label", { class: "tr-f" }, "底模", model) : note(`底模：${f.base}（第一次會自動下載）`),
      trainerBox(),
      el("div", { class: "toolbar" }, el("button", { class: "akey go", type: "button", disabled: running || !enough || !(S.trainer && S.trainer.installed), onclick: () => startTrain("local", { model: model ? model.value : "" }) }, "開始訓練")));
  } else if (S.target === "cloud") {
    const sub = keys([["runpod", "租 RunPod"], ["remote", "我自己的訓練機"]], S.cloud, (c) => { S.cloud = c; render(); });
    if (S.cloud === "runpod") {
      if (!S.gpus) call(`/api/train/${p.id}/quote`, { target: "runpod" }).then((r) => { S.gpus = r ? r.gpus : []; render(); });
      const sel = el("select", {}, ...(S.gpus || []).map((g) => el("option", { value: g.key }, `${g.name}（${g.vram}GB）每小時 $${g.usd_per_hour}・這次約 ${g.hours} 小時 ≈ $${g.usd}${g.stock ? "" : "（現在沒貨）"}`)));
      detail = box("雲端顯卡", sub,
        note("繪匣會租一台 RunPod 的機器、自己裝好訓練器、煉完把每一版抓回來，然後把機器關掉。金鑰用助手頁「雲端設定」那一把。"),
        S.gpus ? (S.gpus.length ? el("label", { class: "tr-f" }, "顯卡", sel) : el("div", { class: "tr-warn" }, "拿不到顯卡清單：先到助手頁的「雲端設定」填 RunPod 金鑰")) : note("查價中…"),
        note("價錢是估的：開機裝環境那十幾分鐘也算錢，實際看排隊和顯卡。"),
        el("div", { class: "toolbar" }, el("button", { class: "akey go", type: "button", disabled: running || !(S.gpus && S.gpus.length), onclick: () => { const g = S.gpus.find((x) => x.key === sel.value); startTrain("runpod", { gpu: sel.value }, `要租 ${g.name}，每小時 $${g.usd_per_hour}，這次估計約 $${g.usd}（${g.hours} 小時，${steps} 步）。\n\n按確定就會開機、開始計費；煉完會自動關機。`); } }, "看好了，租機開始")));
    } else {
      const url = el("input", { type: "text", placeholder: "http://192.168.1.50:8000", spellcheck: "false", value: localStorage.getItem("trainRemoteUrl") || "" });
      const tok = el("input", { type: "password", placeholder: "口令（開小幫手時自己設的那一串）" });
      detail = box("雲端顯卡", sub,
        note("在你自己的訓練機上裝好 ai-toolkit，再跑繪匣附的訓練小幫手（resources\\train\\agent.py --host 0.0.0.0 --token 你的口令 --work … --toolkit …），把網址和口令填這裡。"),
        el("label", { class: "tr-f" }, "網址", url), el("label", { class: "tr-f" }, "口令", tok),
        el("div", { class: "toolbar" }, el("button", { class: "akey go", type: "button", disabled: running, onclick: () => { localStorage.setItem("trainRemoteUrl", url.value.trim()); startTrain("remote", { url: url.value.trim(), token: tok.value }); } }, "開始訓練")));
    }
  } else if (S.target === "civitai") {
    const q = S.quote && S.quote.buzz !== undefined ? S.quote : null;
    detail = box("C 站（Civitai）",
      f.civitai ? note("訓練圖和描述會上傳到 Civitai、放在你的帳號底下，由它的機器煉，用 Buzz 付。金鑰用設定頁那一把。繪匣每分鐘去看一次進度，煉好一版抓一版。")
        : el("div", { class: "tr-warn" }, "C 站還不能煉這個家族。"),
      q ? el("div", { class: "tr-why" }, `試算：${q.buzz} Buzz（${q.steps} 步、${q.images} 張）。${q.note}`) : null,
      el("div", { class: "toolbar" },
        el("button", { class: "akey", type: "button", disabled: running || !f.civitai, onclick: async (e) => {
          if (!confirm("試算要先把訓練圖和描述上傳到 Civitai（放在你的帳號底下）。試算本身不扣 Buzz。要上傳嗎？")) return;
          e.target.disabled = true; F("trStatus").textContent = "上傳、試算中…";
          const r = await call(`/api/train/${p.id}/quote`, { target: "civitai", upload_ok: true });
          if (r) S.quote = r;
          render();
        } }, q ? "重新試算" : "上傳並試算"),
        el("button", { class: "akey go", type: "button", disabled: running || !q, onclick: () => startTrain("civitai", { max_cost: q.buzz }, `要花 ${q.buzz} Buzz（${q.steps} 步、${q.images} 張）。按確定就會送出、扣 Buzz。`) }, "看好了，送出訓練")));
  } else {
    const ms = S.quote && S.quote.ms;
    detail = box("魔塔（ModelScope）・實驗性",
      note("魔塔沒有開放訓練用的 API，要在它的網頁（AIGC 專區的模型訓練）送出。繪匣先幫你把訓練圖和描述包成它收的壓縮檔。"),
      f.modelscope ? null : el("div", { class: "tr-warn" }, "魔塔的訓練頁目前沒有這個家族。"),
      ms ? el("div", { class: "tr-why" }, `包好了（${ms.files} 筆）：${ms.zip}`) : null,
      ms ? note(`到 ${ms.url} 建資料集、上傳這個壓縮檔${ms.base ? `，底模選 ${ms.base}` : ""}；步數 ${steps}、rank ${(p.params && p.params.rank) || ""} 照第四步的填。煉好的 LoRA 下載回來丟進 LoRA 資料夾就能用。`) : null,
      el("div", { class: "toolbar" },
        el("button", { class: "akey", type: "button", disabled: !f.modelscope, onclick: () => startTrain("modelscope", {}, "這一步只會在你的電腦上把資料集包成壓縮檔，不會上傳、不花錢。要包嗎？") }, ms ? "重新打包" : "打包資料集"),
        ms ? el("a", { class: "akey sm", href: ms.url, target: "_blank", rel: "noopener" }, "開魔塔訓練頁") : null));
  }
  return [box("在哪煉", cards, running ? el("div", { class: "tr-warn" }, "這個專案正在訓練，到最後一步看進度。") : null), detail];
}

// ---------------------------------------------------------------- 6 訓練與驗收
function step6() {
  const p = cur();
  const run = p.run || {};
  const prog = S.prog || applyEvent(null, run);
  const running = busyKind() === "train";
  const pc = percent(prog);
  const epochs = [...(run.epochs || [])].sort((a, b) => a.n - b.n);
  const samples = run.samples || [];
  const every = p.params && p.params.steps && p.params.epochs ? Math.max(1, Math.floor(p.params.steps / p.params.epochs)) : 0;
  const strength = el("select", {}, ...[0.6, 0.7, 0.8, 0.9, 1.0].map((s) => el("option", { value: s, selected: s === 0.8 }, `強度 ${s}`)));
  const where = { local: "本機", remote: "自己的訓練機", runpod: "RunPod", civitai: "C 站" }[run.target] || "";
  const head = box(`${where}・${stateName(prog.state)}`,
    el("div", { class: "tr-bar" }, el("div", { style: `width:${pc}%` })),
    el("div", { class: "toolbar" },
      el("span", { class: "vfd g", id: "trProgText" }, prog.total ? `${prog.step} / ${prog.total}${prog.loss != null ? `　loss ${Number(prog.loss).toFixed(4)}` : ""}` : prog.msg || ""),
      el("span", { class: "grow" }),
      running ? el("button", { class: "akey sm warn", type: "button", onclick: async () => { if (confirm("要停掉這次訓練嗎？已經存好的版本會留著。")) await call(`/api/train/${p.id}/stop`, {}); } }, "停止") : null,
      !running && run.remote && run.remote.pod ? el("button", { class: "akey sm warn", type: "button", onclick: async () => { const r = await call(`/api/train/${p.id}/stop`, {}); if (r) { toast(r.msg || "好了"); open(p.id, 6); } } }, "關掉訓練機") : null,
      !running && ["detached", "interrupted", "error", "stopped"].includes(prog.state) ? el("button", { class: "akey sm", type: "button", onclick: () => { S.step = 5; render(); } }, prog.state === "detached" ? "回去接上" : "回去重來") : null),
    prog.state === "error" && (prog.msg || run.error) ? el("div", { class: "tr-warn" }, prog.msg || run.error) : null,
    prog.state === "interrupted" ? el("div", { class: "tr-warn" }, "繪匣中途被關掉，這次訓練沒有跑完。已經存好的版本還在。") : null,
    prog.state === "detached" ? el("div", { class: "tr-warn" }, "訓練還在外面跑（或訓練機還開著、還在計費）。回上一步按開始就會接回去。") : null,
    (prog.log || []).length ? el("pre", { class: "tr-log", id: "trRunLog" }, prog.log.slice(-12).join("\n")) : null);
  const cards = el("div", { class: "tr-epochs" }, ...epochs.map((e) => {
    const mine = samples.filter((s) => every && Math.round(s.step / every) === e.n);
    const adopted = p.adopted && p.adopted.epoch === e.n;
    return el("div", { class: `tr-epoch${adopted ? " on" : ""}` },
      el("b", {}, `第 ${e.n} 版${adopted ? "（採用中）" : ""}`),
      el("div", { class: "muted sm" }, e.file),
      mine.length ? el("div", { class: "tr-samples" }, ...mine.map((s) => el("img", { src: `/api/train/${p.id}/sample/${s.file}`, loading: "lazy", alt: `第 ${e.n} 版樣張` }))) : null,
      el("button", { class: "akey sm", type: "button", onclick: async () => { const r = await call(`/api/train/${p.id}/adopt`, { epoch: e.n, strength: Number(strength.value) }); if (r) { toast(`第 ${e.n} 版已經放進 LoRA 卡匣，觸發詞 ${p.trigger}`); S.v.project = r.project; render(); loadList().then(renderList); } } }, "就用這版"));
  }));
  const prompt = el("input", { type: "text", spellcheck: "false", value: (S.cmp && S.cmp.prompt) || (p.samples && p.samples[0]) || p.trigger });
  const cmp = S.cmp && S.cmp.cells ? el("div", { class: "tr-cmp", style: `grid-template-columns: auto repeat(${S.cmp.strengths.length}, 1fr)` },
    el("span", {}), ...S.cmp.strengths.map((s) => el("b", {}, `強度 ${s}`)),
    ...S.cmp.epochs.flatMap((n) => [el("b", {}, `第 ${n} 版`), ...S.cmp.strengths.map((s) => {
      const c = S.cmp.cells.find((x) => x.epoch === n && x.strength === s);
      return el("div", { class: "tr-cell" }, c && c.image ? el("img", { src: `/thumb/${c.image}`, alt: `第 ${n} 版 強度 ${s}` }) : "…");
    })])) : null;
  return [
    head,
    box(`煉好的版本（${epochs.length}）`,
      epochs.length ? el("div", { class: "toolbar" }, el("span", { class: "muted sm" }, "採用時的建議強度"), strength) : note("每存好一版就會出現在這裡，檔案同時放進 LoRA 資料夾。"),
      cards),
    box("對照表",
      note("同一句提示詞、同一個種子，把每一版配幾個強度各畫一張，排成表來挑。用的是生成頁現在選的模型和設定：先到生成頁選好這個家族的模型。"),
      el("label", { class: "tr-f" }, "提示詞", prompt),
      el("div", { class: "toolbar" }, el("button", { class: "akey", type: "button", disabled: running || !epochs.length, onclick: async () => {
        const base = { ...gen.collectParams(), mode: "txt2img", prompt: prompt.value.trim(), loras: [] };
        for (const k of ["init_image", "mask", "control", "pose_control", "refs"]) delete base[k];
        const r = await call(`/api/train/${p.id}/compare`, { params: base, strengths: [0.6, 0.8, 1.0] });
        if (r) { S.cmp = { ...r, prompt: prompt.value.trim() }; render(); }
      } }, "出對照表")),
      cmp),
  ];
}

// ================================================================ 事件

export function onEvent(m) {
  if (m.phase === "install") {
    if (!S.trainer) return;
    if (m.state === "running") {
      S.trainer.busy = true;
      S.trainer.log = [...(S.trainer.log || []), m.msg].slice(-15);
      const pre = F("trInstallLog");
      if (pre) { pre.hidden = false; pre.textContent = S.trainer.log.join("\n"); pre.scrollTop = pre.scrollHeight; }
      return;
    }
    api("/api/train/trainer").then((t) => { S.trainer = t; if (app.page === "train") render(); }).catch(() => {});
    toast(m.state === "done" ? "訓練器裝好了" : `訓練器安裝失敗：${m.msg}`, m.state === "done" ? "" : "err");
    return;
  }
  if (m.id !== pid()) {
    if (m.state !== "running" && S.loaded) loadList().then(() => app.page === "train" && renderList());
    return;
  }
  if (m.phase === "caption") {
    if (m.state === "running") {
      const s = F("trCapProg");
      if (s) s.textContent = `寫描述中 ${m.done}/${m.total}`;
      F("trStatus").textContent = `寫描述 ${m.done}/${m.total}`;
      return;
    }
    if (m.msg) toast(m.msg, "err");
    open(pid(), S.step);
    return;
  }
  S.prog = applyEvent(S.prog, m);
  if (m.state !== "running" || m.new) {
    // 有新版本、或結束了：重讀專案（版本清單、樣張在專案檔裡）
    open(pid(), S.step).then(() => loadList().then(renderList));
    if (m.state === "done") toast("LoRA 煉好了，到「訓練與驗收」挑一版");
    if (m.state === "error") toast(`訓練失敗：${m.msg}`, "err");
    return;
  }
  if (app.page !== "train") return;
  if (S.step === 6) {
    const t = F("trProgText"), bar = document.querySelector("#trBody .tr-bar > div"), log = F("trRunLog");
    if (t) t.textContent = S.prog.total ? `${S.prog.step} / ${S.prog.total}${S.prog.loss != null ? `　loss ${Number(S.prog.loss).toFixed(4)}` : ""}` : S.prog.msg;
    if (bar) bar.style.width = `${percent(S.prog)}%`;
    if (log && S.prog.log.length) log.textContent = S.prog.log.slice(-12).join("\n");
    // 進度條還沒畫、或 log 第一次有東西（那一塊之前沒畫出來）：整頁重畫一次
    if (!t || (!log && S.prog.log.length)) render();
  }
  F("trStatus").textContent = `${stateName(S.prog.state)}${S.prog.total ? ` ${percent(S.prog)}%` : ""}`;
}

// 對照表的格子：生圖完成的事件進來，把圖填進對應的那格
export function onImage(job, rec) {
  const c = S.cmp && S.cmp.cells && S.cmp.cells.find((x) => x.job === job);
  if (!c || !rec) return;
  c.image = rec.id;
  if (app.page === "train" && S.step === 6) render();
}

export function init() {
  F("trNew").onclick = () => { S.v = null; S.prog = null; S.step = 1; render(); };
  F("trPrev").onclick = () => { if (S.step > 1) { S.step--; render(); } };
  F("trNext").onclick = () => { if (S.step < 6 && !blockReason(S.step + 1, S.v)) { S.step++; render(); } };
  F("trDel").onclick = async () => {
    const p = cur();
    if (!p || !confirm(`把訓練專案「${p.name}」搬到「待刪除」？\n\n已經煉好、放進 LoRA 資料夾的檔案不會動。`)) return;
    if (await call(`/api/train/${p.id}`, { confirm: true }, "DELETE")) { S.v = null; localStorage.removeItem("trainProject"); await show(); }
  };
}

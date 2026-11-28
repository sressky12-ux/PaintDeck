// 生成頁：表單、模式切換、送出任務、即時預覽、結果列與圖片動作
import { $, $$, el, api, toast, debounce, store, uploadFile, pickFile, makeDrop, openModal, closeModal, modeName, isVideo, IMAGE_ONLY_ACTS, VIDEO_ONLY_ACTS, fmtSecs } from "./util.js";
import { mediaSlotsControl } from "./video-slots.js";
import { app } from "./state.js";
import { attachAutocomplete, punctFixOn } from "./autocomplete.js";
import * as mask from "./mask.js";
import * as loras from "./loras.js";
import * as tagger from "./tagger.js";
import * as refs from "./refs.js";
import * as pe from "./pe.js";
import * as viewsrc from "./viewsrc.js";
import * as prompttabs from "./prompttabs.js";
import { stripOn } from "./feature.js";
import * as promptx from "./promptx.js";
import * as promptsec from "./promptsec.js";
import * as taglib from "./taglib.js";
import * as speedup from "./turbo.js";
import * as wardrobe from "./wardrobe.js";
import * as library from "./library.js";
import { pickFromGallery, openLightboxRecord } from "./gallery.js";
import * as deck from "./deck.js";
import { layoutEditor } from "./layout.js";
import * as play from "./play.js";
import * as wfpanel from "./wfpanel.js";
import * as models from "./models.js";
import * as drawer from "./assetdrawer.js";
import * as posewear from "./posewear.js";
import * as booru from "./booru.js";
import * as lk from "./lockedchips.js";
import * as imgmeta from "./imgmeta.js";
import { refreshGenNode } from "./nodes.js";

const SIZES = [
  ["直 1024×1536", 1024, 1536], ["直 832×1216", 832, 1216], ["直 896×1152", 896, 1152], ["方 1024×1024", 1024, 1024],
  ["橫 1216×832", 1216, 832], ["橫 1536×1024", 1536, 1024], ["寬 1344×768", 1344, 768],
];
const SNIPPETS = {
  "品質與評級": ["masterpiece, best quality", "safe", "sensitive", "nsfw", "explicit"],
  "光影": ["backlighting", "sunlight", "light rays", "volumetric lighting", "cinematic lighting", "lens flare", "rim lighting", "soft lighting", "dramatic lighting", "night, moonlight", "golden hour"],
  "鏡頭與構圖": ["close-up", "upper body", "cowboy shot", "full body", "wide shot", "from above", "from below", "from side", "from behind", "dutch angle", "depth of field", "looking at viewer"],
  "畫風": ["flat color", "watercolor (medium)", "sketch", "anime screencap", "official art", "chromatic aberration", "film grain"],
  "背景": ["simple background, white background", "detailed background", "outdoors", "indoors", "bedroom", "classroom", "city street", "forest", "beach", "night sky, stars"],
};
const MODE_HINT = {
  img2img: "0.3～0.5 保留構圖改細節；0.6～0.8 大幅重畫",
  inpaint: "LLLite 重繪用 1.0；一般遮罩重繪約 0.6～0.8",
};
// 四視圖「常用要求」：簡體跟工作流原文一致；全裸那句是 2026-10-18 實測過的版本＋陰部描述
export const SHEET_PRESETS = {
  cloth: "将图1角色身上的整套服装换成图2中的服装，保留图1的发型和发饰。",
  // 「上衣」太模糊（外套底下還有襯衫時兩個種子都沒改），點名「红色西装外套」才改得到
  color: "将图1中角色的红色西装外套改成黑色，保留原有款式，其他内容不变。",
  nude: "把图1中角色身上的衣服全部脱掉，变成全裸，连靴子也脱掉、光着脚。她是成年女性，胸部大小依原本衣服的轮廓延续；"
    + "原本被衣服盖住的地方画成皮肤，肤色和光影跟露出的皮肤一致；乳头和阴部依各视角自然露出，背面视角只露出背部和臀部。"
    + "看得到阴部的视角，外阴完整画出来：阴阜微微隆起，大阴唇闭合成一条清楚的缝，缝的两侧有柔和的阴影，颜色比周围皮肤略带粉红。",
  under: "把图1中角色的外衣全部脱掉，只留下胸罩和内裤，内衣的颜色和款式跟原本服装协调；原本被衣服盖住的地方画成皮肤，肤色和光影跟露出的皮肤一致。",
};
const SIZE_AUTO_TEXT = { img2img: "依主圖（第 1 張）比例自動決定", multiref: "依主圖（第 1 張）比例自動決定" };
const MODE_HINT_Q = {
  img2img: "1.0＝把輸入圖當參考、整張重畫（轉真人用這個）；0.6～0.9 從原圖像素出發，保留更多原樣",
  inpaint: "遮罩區重畫：約 0.8～1.0",
};
const MODE_HINT_MM = {
  img2img: "1.0＝MiniMax 官方 6 步參考改圖（最穩）；低於 1.0 走 6 步 PDD 切步重繪",
  inpaint: "1.0＝6 步完整重繪遮罩區；低於 1.0 走 6 步 PDD 切步",
};
// MOSS 圖生圖（2026-10-17 實測 E:\AI\moss_test\exp_replace.py）：0.7 構圖、姿勢幾乎不變但畫風改不動；0.85 整張重畫、構圖也換掉
const MODE_HINT_M = {
  img2img: "0.5～0.7 保留構圖和姿勢、改細節和光線；0.8 以上幾乎整張重畫（構圖會跑掉）",
};
// Noct Q 作者附的負面提示詞（只有 CFG > 1 才有作用）
const QWEN_NEG = "artifacts, gpt-image, washed-out colors, low quality, low resolution, AI slop, deviantart, sloppy lines, rough sketch, blurry, indistinct, missing fingers, badly drawn hands, wrong number of fingers";
// Noct Q Anime 作者附的負面詞：多了 photograph、photorealistic，免得畫成照片
const QWEN_ANIME_NEG = "low quality, low resolution, blurry, jpeg artifacts, washed-out colors, sloppy lines, messy line art, extra fingers, missing fingers, badly drawn hands, deformed anatomy, 3d render, photograph, photorealistic";

export const S = {
  mode: "txt2img", init: null, mask: null, ctrl: null,
  loras: [], openNotes: new Set(), results: [], current: null, lastSeed: null, pending: new Map(), running: null, previewUrl: null,
  locked: [],  // 鎖定角色標籤（lockedchips.js）：送出時接在提示詞最前面
};

const F = (id) => $("#" + id);
// 生成頁或它的分身「詞庫」頁（借了生成頁的面板）：貼圖、Ctrl+Enter 在這兩頁都算
const onGenPage = () => app.page === "gen" || app.page === "taglib";
const num = (id) => parseFloat(F(id).value);
// 模型家族由伺服器看檔頭判斷（anima／qwen21／moss）；LoRA 只能用在同家族的模型上
// 內建畫得出來的模型家族（families.rs 的 BUILTIN）；SDXL、Flux、SD1.5 這些靠匯入的工作流組
const BUILTIN_FAMS = ["anima", "qwen21", "moss", "krea2", "mmimage", "sdxl"];
// 設定頁匯入的生圖工作流組（值是 wf:<id>）算 custom：模型照工作流，步數、CFG 預設也照工作流（可勾「用生成頁的」）
const fam = (name = F("model").value) => (name || "").startsWith("wf:") ? "custom" : app.state?.models?.unet_family?.[name] || "anima";
/** 這台電腦上某個家族要用哪顆模型：Qwen 2.1 先官方 Turbo、再其他 Turbo（turboOnly 時到此為止）、再任何一顆；MiniMax 先 w4a8（8GB 能跑） */
function familyModel(f, turboOnly = false) {
  const ms = Object.entries(app.state?.models?.unet_family || {}).filter(([, x]) => x === f).map(([n]) => n);
  if (f === "qwen21") {
    const turbo = ms.filter((n) => /turbo/i.test(n));
    return turbo.find((n) => OFFICIAL_TURBO.test(n)) || turbo[0] || (turboOnly ? undefined : ms[0]);
  }
  return ms.find((n) => /w4a8/i.test(n)) || ms[0];
}
// 選到的工作流組（/api/state 的 models.image_workflows 其中一筆：{id, name, modes, family, options}）；不是工作流或找不到就是 null
const wfSet = (name = F("model").value) => ((name || "").startsWith("wf:") ? (app.state?.models?.image_workflows || []).find((w) => w.id === name) || null : null);
// 工作流組的模型家族（看檔頭；認不出來＝null：LoRA 就都不篩）
const wfFam = () => { const f = wfSet()?.family; return f && f !== "other" ? f : null; };
const famLabel = (f) => app.state?.models?.family_names?.[f] || f;
const famShort = (f) => famLabel(f).replace(/（.*?）/g, "").trim();  // 「SDXL（含 Pony…）」→「SDXL」
const loraFam = (l) => (app.state?.models?.loras || []).find((x) => x.name === l.name)?.family;
// LoRA 不能用在目前的模型上：內建模型要同家族；工作流組看它自己的家族（LoRA 的家族認不出來＝other 照樣可以接，跟後端 prepare 一樣）
const mismatch = (l) => {
  const lf = loraFam(l);
  if (!lf) return false;
  if (fam() !== "custom") return lf !== fam();
  const sf = wfFam();
  return !!sf && lf !== "other" && lf !== sf;
};
// 選 LoRA 的視窗只列能用的（工作流組認不出家族＝全列）；loras.js 的 openPicker 收家族名或判斷函式
const pickerFam = () => (fam() !== "custom" ? fam() : wfFam() ? (l) => !l.family || l.family === "other" || l.family === wfFam() : null);

// ---------------------------------------------------------------- 初始化
let assetDrawer = null;  // 提示詞上面的資產抽屜（assetdrawer.js）

export function init() {
  viewsrc.init(show, enableActions);
  prompttabs.init(defaultNeg);
  document.querySelector("#page-gen .dock-main")?.prepend(el("div",{id:"genTrainingNotice",class:"tr-warn",role:"status",hidden:true}));
  initialTraining = refreshTraining();
  // 資產：Anima 的圖可以當底圖或構圖控制；Qwen 2.1／MiniMax 的圖放進參考圖（自動切到圖生圖／改圖模式）
  assetDrawer = drawer.mount(F("genAssets"), {
    family: () => fam(), insert: appendPrompt,
    images: {
      base: (r) => useInput("img2img", r),
      control: (r) => setCtrl(r),
      ref: { add: (r) => { if (S.mode !== "img2img") setMode("img2img"); return refs.add(r); }, count: () => refs.get().length, get max() { return refs.maxRefs(); },
        has: (name) => refs.get().some((x) => x.name === name) },
    },
    lock: (chip) => { S.locked = lk.addChip(S.locked, chip); renderLocked(); save(); },
  });
  lay = layoutEditor({ onChange: () => { layCount(); save(); } });
  F("layMount").append(lay.el);
  $("#modeSeg").addEventListener("click", (e) => { const b = e.target.closest("button"); if (b) setMode(b.dataset.mode); });

  // 尺寸
  for (const id of ["mmRatio", "mmMp"]) F(id).onchange = () => { mmDimensions(); save(); };
  F("mmPreset8g").onclick = () => {
    F("mmMp").value = "4"; F("mmPe").value = ""; F("mmTiled").value = "512";
    mmDimensions(); save();
  };
  const chips = F("sizeChips");
  for (const [label, w, h] of SIZES) chips.append(el("button", { class: "chip", "data-w": w, "data-h": h, onclick: () => { F("width").value = w; F("height").value = h; syncSize(); save(); } }, label));
  F("swapWH").onclick = () => { [F("width").value, F("height").value] = [F("height").value, F("width").value]; syncSize(); save(); };
  ["width", "height"].forEach((id) => F(id).addEventListener("input", syncSize));
  F("autoSize").onchange = () => { syncSize(); save(); };

  // 片段（內建＋我的片段）
  const sel = F("snippetSel");
  fillSnippets();
  sel.onchange = () => {
    const v = sel.value;
    sel.value = "";
    if (v === "__save") library.saveSnippetFromSelection();
    else if (v === "__manage") library.open("snippet");
    else if (v.startsWith("lib:")) library.useById(+v.slice(4));
    else if (v) appendPrompt(v);
  };
  F("promptLib").onclick = () => library.open();
  F("promptClear").onclick = () => {
    const field = F("prompt").closest(".prompt-scr").dataset.tab === "neg" ? F("negative") : F("prompt");
    field.value = "";
    save();
    field.focus();
  };
  attachAutocomplete(F("prompt"), F("acPrompt"));
  attachAutocomplete(F("negative"), F("acNeg"));
  promptx.init({ prompt: F("prompt"), negative: F("negative"), family: () => fam(), save: () => save() });
  watchPromptValue(F("prompt"), debounce(renderTrig, 150));
  promptsec.init({ prompt: F("prompt"), wrap: F("prompt").parentElement, mount: F("promptSecs"), toggle: F("promptSecBtn"), batch: F("batchLines"), save: () => save() });
  // 全形標點自動換半形（autocomplete.js 的 fixPunct）：開關
  const pf = F("punctFixBtn");
  const pfShow = () => { pf.classList.toggle("on", punctFixOn()); pf.setAttribute("aria-pressed", String(punctFixOn())); };
  pf.onclick = () => { store.set("punctFix", !punctFixOn()); pfShow(); toast(punctFixOn() ? "英文 tag 後面的全形標點會自動換成半形" : "全形標點不再自動換"); };
  pfShow();
  taglib.init({ box: F("tagLib"), btn: F("tagLibBtn"), main: F("prompt"), watch: [F("promptSecs"), F("negative").parentElement], sheet: { anchor: F("promptBox") } });
  // 衣姿抽屜（posewear.js）：姿勢的深度圖進構圖控制（MOSS 只照深度、不照色塊）；服裝／姿勢進多圖參考時角色自動設好
  posewear.init({
    btn: F("pwBtn"), family: () => fam(),
    control: (r) => {
      if (fam() === "moss") { F("ctrlMossDepth").checked = true; F("ctrlMossColor").checked = false; if (S.mode !== "txt2img") setMode("txt2img"); }
      setCtrl(r, { type: "depth", preprocess: "none", invert: false });
    },
    ref: (r, role) => { if (S.mode !== "img2img") setMode("img2img"); return refs.add(r, true, role); },
    // 衣姿整頁試穿工作台「交給繪匣」的整套（posewear.js onBundle）
    handoff: {
      hasFamily: (f) => !!familyModel(f),
      refsNow: () => refs.get().length,
      prepare: (f) => {
        if (fam() !== f) { const m = familyModel(f); if (!m) return false; F("model").value = m; modelDefaults(); familySwitch(); }
        if (S.mode !== "img2img") setMode("img2img");
        refs.set([]); save();
        return true;
      },
      add: (r, role) => refs.add(r, false, role),
      done: () => { refs.render(); save(); },
      setPrompt: (t) => { F("prompt").value = t; F("prompt").dispatchEvent(new Event("input", { bubbles: true })); save(); },
    },
  });

  // 滑桿數值顯示
  const outs = [["denoise", "denoiseOut"], ["ipGrow", "ipGrowOut"], ["ipBlur", "ipBlurOut"], ["ipPad", "ipPadOut"], ["upDenoise", "upDenoiseOut"],
    ["hiresScale", "hiresScaleOut"], ["hiresDenoise", "hiresDenoiseOut"], ["ctrlStr", "ctrlStrOut"], ["ctrlEnd", "ctrlEndOut"], ["stripDenoise", "stripDenoiseOut"],
    ["opFeather", "opFeatherOut"], ["detailDenoise", "detailDenoiseOut"]];
  for (const [i, o] of outs) F(i).addEventListener("input", () => (F(o).textContent = F(i).value));

  // 擴圖：四邊像素、快捷比例、示意圖；生圖玩法的按鈕（play.js）
  for (const id of OP_SIDES) F(id).addEventListener("input", opPreview);
  F("opQuick").addEventListener("click", (e) => { const b = e.target.closest("[data-op]"); if (b) opQuick(b.dataset.op); });
  F("xyOpen").onclick = () => play.openXY();
  for (const id of ["sketchOpen", "initSketch", "ctrlSketch"]) F(id).onclick = () => play.openSketch();

  // 畫廊頁（booru.js）：booru 的圖送圖生圖／改圖、加進參考圖；原圖的 tag 換掉或接在提示詞後面
  booru.init({
    toInit: (r) => { if (!["img2img", "inpaint", "outpaint"].includes(S.mode)) setMode("img2img"); setInit(r, S.mode === "img2img"); },
    toRef: (r) => {
      if (!["qwen21", "mmimage"].includes(fam())) {
        const f = familyModel("qwen21") ? "qwen21" : familyModel("mmimage") ? "mmimage" : null;
        if (!f) { toast("多張參考要 Qwen 2.1 或 MiniMax H3 Image 模型：到下載中心或安裝精靈裝「Qwen 2.1 編輯」", "err"); return null; }
        F("model").value = familyModel(f); modelDefaults(); familySwitch();
      }
      if (S.mode !== "img2img") setMode("img2img");
      return refs.add(r, true);
    },
    setPrompt: (t, how) => {
      if (how === "append") appendPrompt(t);
      else { F("prompt").value = t; save(); }
      F("prompt").dispatchEvent(new Event("input", { bubbles: true }));
    },
  });

  F("model").onchange = () => { modelDefaults(); familySwitch(); save(); swapCart(); assetDrawer?.refresh(); };
  // 工作流參數面板（選了「⚙ 生圖工作流」才出現）、把內建功能匯出成 ComfyUI 工作流
  wfpanel.init({ adopt: adoptSampler });
  F("exportGraph").onclick = exportBuiltin;
  // 讀圖片參數：套用時照圖的模式（ComfyUI、WebUI 的圖是文生圖）；匯入工作流到設定頁的匯入欄
  imgmeta.init({ apply: (p) => applyParams(p, true), importWf: (f) => { app.showPage("settings"); models.importImageFile(f); } });
  F("readMeta").onclick = () => imgmeta.open();
  F("expSave").onclick = exportSave;
  F("expOpen").onclick = () => wfpanel.reveal(F("expOpen").dataset.path);
  F("expGoImport").onclick = goImport;
  F("otherModelsGo").onclick = () => { app.showPage("settings"); models.focusImport(); };
  F("basicMissingHide").onclick = () => { store.set("basicMissingHide", true); F("basicMissing").hidden = true; };
  F("dirsPendingGo").onclick = restartForDirs;
  F("comfyUpdatedGo").onclick = restartForUpdate;
  F("dirsPendingHide").onclick = () => { store.set("dirsPendingHide", dirsPendingSig(app.state?.models?.dirs_pending)); F("dirsPending").hidden = true; };
  F("turboLora").onchange = () => { modelDefaults(); save(); };
  F("mossFast").onchange = (e) => setMossFast(e.target.checked);
  F("mossDeband").onchange = (e) => setMossDeband(e.target.checked);
  F("mossEdit").onchange = (e) => setMossEdit(e.target.checked);
  F("ipMethod").onchange = () => { F("denoise").value = F("ipMethod").value === "lllite" ? 1 : 0.75; F("denoiseOut").textContent = F("denoise").value; save(); };
  F("ipArea").onchange = () => { F("ipPadRow").hidden = F("ipArea").value !== "masked"; save(); };
  F("upRefine").onchange = () => { F("upRefineRow").hidden = !F("upRefine").checked; save(); };
  F("stripRefine").onchange = () => { F("stripRefineRow").hidden = !F("stripRefine").checked; save(); };
  for (const id of ["stripEngine", "sheetEngine"]) {
    const sel = F(id);
    if (sel) sel.onchange = () => { if (sel.value !== "auto") store.set("editEngine", sel.value); save(); };
  }
  // 四視圖：做新角色設定圖（工坊）；常用要求填進框裡；服裝參考圖（圖2）
  F("csOpen").onclick = async () => (await import("./charsheet.js")).open();
  F("sheetPreset").onchange = () => {
    const t = SHEET_PRESETS[F("sheetPreset").value];
    if (t) { F("sheetReq").value = t; save(); }
    F("sheetPreset").value = "";
  };
  const setSheetRef = (r) => { S.sheetRef = r; showSheetRef(); save(); };
  const setSheetRefFile = async (f) => { try { setSheetRef(await uploadFile(f)); } catch (e) { toast(e.message, "err"); } };
  makeDrop(F("sheetRefDrop"), setSheetRefFile);
  F("sheetRefPick").onclick = async () => { const f = await pickFile(); if (f) setSheetRefFile(f); };
  F("sheetRefGallery").onclick = () => pickFromGallery(async (rec) => setSheetRef(await api("/api/to_input", { json: { id: rec.id } })));
  F("sheetRefImg").onclick = () => setSheetRef(null);
  F("ctrlType").onchange = () => {
    const t = F("ctrlType").value;
    F("ctrlPre").value = t === "depth" ? "depth" : t === "pose" ? "pose" : t === "any" ? "canny" : "none";
    F("ctrlInvert").checked = t === "any";
    save();
  };

  // 輸入圖
  const setInitFile = async (f) => { try { setInit(await uploadFile(f)); } catch (e) { toast(e.message, "err"); } };
  makeDrop(F("initDrop"), setInitFile);
  F("initPick").onclick = async () => { const f = await pickFile(); if (f) setInitFile(f); };
  F("initFromGallery").onclick = () => pickFromGallery(async (rec) => setInit(await api("/api/to_input", { json: { id: rec.id } })));
  F("maskEdit").onclick = () => S.init && mask.open(S.init, (m) => { S.mask = m; showInit(); });
  F("initTagger").onclick = () => S.init && tagger.open({ name: S.init.name }, applyTags, fam(), triggerWords);
  // 提示詞卡的「反推」（2026-11-23）：圖生圖這類有輸入圖就推輸入圖，否則推觀景窗目前那張成品，都沒有就挑一張檔案
  F("promptTagger").onclick = async () => {
    if (S.init && !F("initBox").hidden) return tagger.open({ name: S.init.name }, applyTags, fam(), triggerWords);
    if (S.current && !isVideo(S.current)) return tagger.open({ id: S.current.id }, applyTags, fam(), triggerWords);
    const f = await pickFile();
    if (!f) return;
    try { tagger.open({ name: (await uploadFile(f)).name }, applyTags, fam(), triggerWords); } catch (e) { toast(e.message, "err"); }
  };
  F("initParams").onclick = () => S.init?.params && applyParams(S.init.params, false);
  document.addEventListener("paste", async (e) => {
    if (!onGenPage() || !$("#imModal").hidden || !$("#loraPrev").hidden || !$("#taggerModal").hidden) return; // 讀圖片參數、換 LoRA 預覽圖、反推的視窗開著：貼給它們
    const files = [...(e.clipboardData?.files || [])].filter((x) => x.type.startsWith("image/"));
    if (!files.length) return;
    e.preventDefault();
    if (S.mode === "txt2img") setMode("img2img");
    if (S.mode === "img2img") return refs.addFiles(files);
    setInitFile(files[0]);
  });

  // 圖生圖／改圖統一參考匣（單一事實來源：refs[0] 單向投影到 S.init）
  refs.init({
    family: () => fam(), prompt: F("prompt"), save: () => save(),
    onChange: () => {
      syncInitFromRefs();
      familyUI();
      syncWf();
    },
  });

  // AI 改寫提示詞
  pe.init({
    prompt: F("prompt"), save: () => save(), family: () => fam(), mode: () => effectiveMode(), model: () => F("model").value,
    images: () => (S.mode === "img2img" ? refs.get().map((r) => r.name) : S.init && S.mode !== "txt2img" ? [S.init.name] : []),
    size: () => [+F("width").value, +F("height").value],
    canSize: () => !(S.mode === "inpaint" || S.mode === "upscale"),
    setSize: (w, h) => {
      if (S.mode === "img2img" && F("autoSize").checked) F("autoSize").checked = false;
      F("width").value = w; F("height").value = h; syncSize(); save();
    },
  });
  F("refSwitchModel").onclick = () => {
    const m = familyModel("qwen21", false);
    if (!m) return toast("找不到 Qwen 2.1 模型", "err");
    F("model").value = m; modelDefaults(); familySwitch(); save();
  };
  F("refSwitchMm").onclick = () => {
    const m = familyModel("mmimage");
    if (!m) return toast("找不到 MiniMax H3 Image 模型", "err");
    F("model").value = m; modelDefaults(); familySwitch(); save();
  };

  // 控制圖
  const setCtrlFile = async (f) => { try { S.ctrl = await uploadFile(f); showCtrl(); save(); } catch (e) { toast(e.message, "err"); } };
  makeDrop(F("ctrlDrop"), setCtrlFile);
  F("ctrlPick").onclick = async () => { const f = await pickFile(); if (f) setCtrlFile(f); };
  F("ctrlImg").onclick = () => { S.ctrl = null; showCtrl(); save(); };

  // LoRA
  F("addLora").onclick = () => loras.openPicker((l, triggers) => addLora(l, triggers), pickerFam(), { multi: true, have: new Set(S.loras.map((x) => x.name)) });

  // 種子、提示詞庫的整套設定
  F("seedRandom").onclick = () => { F("seed").value = ""; save(); };
  F("seedReuse").onclick = () => { if (S.lastSeed != null) F("seed").value = S.lastSeed; save(); };
  F("presetSave").onclick = () => library.saveDialog({ kind: "set" });
  F("presetLib").onclick = () => library.open("set");
  F("presetSel").onchange = () => {
    const v = F("presetSel").value;
    F("presetSel").value = "";
    if (v === "__other") library.open("set", "all");
    else if (v) library.useById(+v);
  };
  library.init({
    collect: libSnapshot, apply: applyLibSet, insert: appendPrompt, replace: (t) => { F("prompt").value = t; save(); },
    family: () => fam(), famOf: (m) => app.state?.models?.unet_family?.[m] || "", current: () => S.current, promptEl: () => F("prompt"),
    onChange: fillSnippets,
  });

  // 生成
  F("genBtn").onclick = generate;
  F("stopBtn").onclick = async () => { try { await api("/api/interrupt", { json: {} }); } catch (e) { toast("停止失敗：" + e.message, "err"); } };
  F("clearBtn").onclick = async () => {
    try { await api("/api/clear_queue", { json: {} }); } catch (e) { return toast("全停失敗：" + e.message, "err"); }
    S.pending.clear(); S.running = null; F("liveBadge").hidden = true; renderStrip();
  };
  document.addEventListener("keydown", (e) => {
    // 有浮層開著（提示詞庫、反推、影片、遮罩…）時不要在背景偷偷生成
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && onGenPage() && !document.querySelector(".modal:not([hidden])")) { e.preventDefault(); generate(); }
  });

  // 預覽圖動作
  F("viewImg").onclick = () => S.current && openLightboxRecord(S.current, S.results);
  F("viewActions").addEventListener("click", (e) => {
    const b = e.target.closest("[data-act]");
    if (b && S.current && b.dataset.act !== "download") imageAction(b.dataset.act, S.current, b.dataset.act === "play" ? F("viewMore") : b);
  });

  // 隨機頁借走生成頁面板時，面板裡的改動冒泡到隨機頁；膠囊彈出窗（#capLayer，dock.js）暫借面板時冒泡到浮層：三個都要聽（borrow.js）
  const saveSoon = debounce(save, 400), saveNow = debounce(save, 100);
  for (const pg of [F("page-gen"), F("page-taglib"), F("capLayer")].filter(Boolean)) { pg.addEventListener("input", saveSoon); pg.addEventListener("change", saveNow); }
  restore();
  seedRecent();

  // 多節點「送到」下拉：開頁先填一次，之後每 15 秒（看得到頁面時）跟按生成前各更新一次排隊數
  refreshGenNode();
  setInterval(() => { if (document.visibilityState === "visible") refreshGenNode(); }, 15000);
}

// 換模型：卡匣插槽的卡帶退出再插入（動畫在 motion.css 第 11 段）
function swapCart() {
  const w = F("modelWell");
  if (!w) return;
  w.classList.remove("swap");
  void w.offsetWidth;  // 重新觸發動畫
  w.classList.add("swap");
  w.addEventListener("animationend", () => w.classList.remove("swap"), { once: true });
}

// 開機時幻燈片匣先放圖庫最新的 6 張（檔案還在的），待機時就有東西可以點來接著改；已經開始畫了就不放舊的
async function seedRecent() {
  try {
    const r = await api("/api/gallery?offset=0&limit=12&sort=new");
    const ids = r.items.filter((x) => !x.missing).slice(0, 6).map((x) => x.id);
    const recs = (await Promise.all(ids.map((i) => api(`/api/image/${i}`).catch(() => null)))).filter(Boolean);
    if (S.results.length || S.pending.size) return;
    for (const rec of recs) { S.results.push(rec); stripSeen.add(rec.id); }
    renderStrip();
  } catch { /* 圖庫讀不到就算了，幻燈片匣維持空的 */ }
}

// ---------------------------------------------------------------- 伺服器狀態（模型清單等）
export function onState(st) {
  const m = st.models || {};
  fillModels(m);
  mmEncoders(m.text_encoders || []);
  assetDrawer?.refresh();
  if (S.family === undefined) S.family = fam();
  fillSelect(F("sampler"), m.samplers || ["euler"], store.get("form", {}).sampler || "euler");
  fillSelect(F("scheduler"), m.schedulers || ["simple"], store.get("form", {}).scheduler || "simple");
  const ts = F("turboLora");
  speedup.fillTurbo(ts, m.turbo_loras, ts.value || store.get("form", {}).turboLora || "");
  for (const id of ["hiresUpscaler", "upModel"]) fillSelect(F(id), m.upscalers || [], store.get("form", {})[id] || "4x-AnimeSharp.safetensors");
  // 選單都有選項了：之後以畫面上的值為準
  for (const k of Object.keys(pendingSel)) delete pendingSel[k];
  library.fillPickers();
  if (!F("negative").value) F("negative").value = defaultNeg();  // 工作流：空白＝照工作流自己的負面提示詞
  // 已加入的 LoRA 同步最新的上下限設定（例如滑桿的可用範圍）
  const meta = Object.fromEntries((m.loras || []).map((x) => [x.name, x]));
  for (const l of S.loras) if (meta[l.name]) Object.assign(l, { min: meta[l.name].min, max: meta[l.name].max, slider: meta[l.name].slider });
  renderLoras();
  wfFresh = true;
  try { modelHint(); } finally { wfFresh = false; }
  promptx.refresh();
  prompttabs.refresh();
}

// 模型選單依家族分組；認不出家族的模型（例如別的架構）不列出，免得選了跑不動
function fillModels(m) {
  const sel = F("model"), had = sel.value, want = had || store.get("form", {}).model || "anima-turbo-v1.1.safetensors";
  const fams = m.unet_family || {};
  sel.innerHTML = "";
  // 只列內建畫得出來的家族（families.rs 也認得 SDXL、Flux…，但 graphs.rs 畫不了，選了一定失敗）
  for (const [key, label] of Object.entries(m.family_names || { anima: "Anima" }).filter(([k]) => BUILTIN_FAMS.includes(k))) {
    const items = (m.unets || []).filter((x) => fams[x] === key);
    if (!items.length) continue;
    const og = el("optgroup", { label });
    for (const x of items) og.append(el("option", { value: x }, x.replace(/\.safetensors$/, "")));
    sel.append(og);
  }
  const flows = m.image_workflows || [];
  if (flows.length) sel.append(el("optgroup", { label: "⚙ 生圖工作流" }, ...flows.map((w) => el("option", { value: w.id, title: (w.modes || []).map((m) => modeName[m] || m).join("、") }, "⚙ " + w.name))));
  if ([...sel.options].some((o) => o.value === want)) sel.value = want;
  else if (had && sel.options.length) {
    // 畫面上選的模型從清單消失了（檔案移走、改名、外部資料夾拿掉）：選單會自己跳到第一個，
    // 但程式改 value 不會觸發 onchange，步數／CFG／負面詞還是上一顆的。照換模型的流程走一次，並講一聲
    modelDefaults(); familySwitch(); save();
    toast(`原本選的模型「${had.replace(/\.safetensors$/, "")}」不在清單裡了，改用「${sel.selectedOptions[0]?.textContent || sel.value}」，步數和 CFG 換成它的預設`, "err");
  }
  // ComfyUI 連上了、卻一個認得的模型都沒有（多半是安裝精靈跳過了下載）：給一個回到下載畫面的入口
  F("noModels").hidden = !(app.state?.online && Array.isArray(m.unets)) || sel.options.length > 0;
  // 只有 SDXL、Flux 這類內建畫不了的模型（老手）：選單裡不列，告訴他可以把自己的工作流匯進來用。
  // 後端 other_families 另外算了 checkpoints 資料夾的；已經匯入過工作流就不再提
  const other = {};
  for (const x of m.unets || []) { const f = fams[x]; if (f && f !== "other" && !BUILTIN_FAMS.includes(f)) other[f] = (other[f] || 0) + 1; }
  for (const [f, n] of Object.entries(m.other_families || {})) if (!BUILTIN_FAMS.includes(f)) other[f] = Math.max(other[f] || 0, n);
  const names = Object.entries(other).map(([f, n]) => `${(m.family_names || {})[f] || f} ${n} 個`);
  F("otherModels").hidden = !(app.state?.online && names.length && !flows.length);
  if (names.length) F("otherModelsText").textContent = `你的 ComfyUI 裡還有 ${names.join("、")}：生圖站內建的功能畫不了它們，但可以把你平常用的 ComfyUI 工作流匯進來用（之後模型選單最下面會出現「⚙ 工作流」）。`;
  paintDirsPending(m.dirs_pending);
  paintComfyUpdated(m.comfy_updated);
  checkBasic(m);
}

// 加了外部模型資料夾、ComfyUI 還沒讀到：提醒重開（後端 model_dirs::pending_restart）。「不要再提醒」只記這一組資料夾，之後又加新的會再提醒
const dirsPendingSig = (p) => (p?.list || []).map((d) => `${d.kind}|${d.path}`).sort().join("\n");
function paintDirsPending(p) {
  const box = F("dirsPending");
  box.hidden = !p?.list?.length || !app.state?.online || store.get("dirsPendingHide", "") === dirsPendingSig(p);
  if (box.hidden) return;
  F("dirsPendingList").textContent = p.list.map((d) => `${d.label}（${d.path}）`).join("、");
  F("dirsPendingGo").hidden = !p.can_restart;
  F("dirsPendingManual").hidden = !!p.can_restart;
}
function paintComfyUpdated(u) {
  const box = F("comfyUpdated");
  box.hidden = !u?.list?.length;
  if (box.hidden) return;
  F("comfyUpdatedList").textContent = u.list.join("、");
  F("comfyUpdatedGo").hidden = !u.can_restart;
  F("comfyUpdatedManual").hidden = !!u.can_restart;
}
async function restartForUpdate() {
  try {
    await api("/api/models/restart_comfy", { json: {} });
    F("comfyUpdated").hidden = true;
    toast("ComfyUI 重開中，大約半分鐘後就是新版", "ok");
  } catch (e) { toast(e.message, "err"); }
}

// 安裝清單裡的檔（MOSS 加速／編輯 LoRA）不在：問要不要下載，下載好重新整理模型清單再接著做 then（/api/models/fetch_listed）
// 清單裡的 LoRA：檔名對得上 re，或是後端說「別的資料夾已經有同一顆」時記下的名稱（store loraAlias，fetchListed 寫）
function listed(d) {
  d.is = (name) => d.re.test(name) || name === store.get("loraAlias", {})[d.rel];
  return d;
}
fetchListed.last = {};
const rememberAlias = (rel, lora) => lora && store.set("loraAlias", { ...store.get("loraAlias", {}), [rel]: lora });

async function fetchListed(rel, label, size, then) {
  // 先問有沒有：別的資料夾有大小＋SHA256 一樣的同一顆也算（後端回它的名稱 lora），就不用問要不要下載
  try {
    const c = await api("/api/models/fetch_listed", { json: { rel, check: true } });
    if (c.have) {
      // 檔在、LoRA 選單卻還沒列出來（ComfyUI 還沒讀到）：then 又會叫回來，不要一直繞
      if (Date.now() - (fetchListed.last[rel] || 0) < 10000) return toast(`${label.trim()}的檔已經在了，但 ComfyUI 的清單還沒更新：等幾秒再試，或到模型頁按「重開 ComfyUI」`, "err");
      fetchListed.last[rel] = Date.now();
      rememberAlias(rel, c.lora);
      await app.refreshState();
      return then();
    }
  } catch { /* 問不到就照舊問要不要下載 */ }
  if (!confirm(`找不到${label}（${rel}）。現在下載嗎？約 ${size}，下載好會自動套用。`)) return;
  try {
    const r = await api("/api/models/fetch_listed", { json: { rel } });
    if (r.have) { rememberAlias(rel, r.lora); await app.refreshState(); return then(); }
    toast(`開始下載${label}（約 ${size}），好了會自動套用；進度在設定頁的下載清單`, "ok");
    const t = setInterval(async () => {
      let d;
      try { d = ((await api("/api/models/downloads")).downloads || []).find((x) => x.id === r.id); } catch { return; }
      if (!d || d.status === "run") return;
      clearInterval(t);
      if (d.status !== "done") return toast(`${label}下載失敗：${d.error || "不明原因"}（再勾一次會接著下載）`, "err");
      await app.refreshState();
      toast(`${label}下載好了`, "ok");
      then();
    }, 2000);
  } catch (e) { toast(e.message, "err"); }
}

async function restartForDirs() {
  try {
    await api("/api/models/restart_comfy", { json: {} });
    F("dirsPending").hidden = true;
    toast("ComfyUI 重開中，大約半分鐘後模型選單就會多出來", "ok");
  } catch (e) { toast(e.message, "err"); }
}

// 基本版只裝了一半（例如安裝到一半關掉）：主模型在、它要的文字編碼器或 VAE 卻不在，畫圖一定失敗。
// 跟 graphs.rs 的預設檔名一致（CLIP／VAE、QWEN21_TE／QWEN21_VAE、MOSS_TE／MOSS_VAE）；只看主模型在的家族，只裝動漫的不會被提醒
const BASIC_NEEDS = {
  anima: ["text_encoders/qwen_3_06b_base", "vae/qwen_image_vae."],
  qwen21: ["text_encoders/qwen3vl_8b", "vae/qwen_image_2.1_vae"],
  moss: ["text_encoders/qwen3.5_9b", "vae/flux2-vae"],
};
let basicChecked = false;
async function checkBasic(m) {
  if (basicChecked || !app.state?.online || store.get("basicMissingHide", false)) return;
  const fams = new Set(Object.values(m.unet_family || {}));
  const have = Object.keys(BASIC_NEEDS).filter((f) => fams.has(f));
  if (!have.length) return;
  basicChecked = true;
  let v;
  try { v = await api("/api/models/inventory", { timeout: 0 }); } catch { return; }
  const miss = (v.missing?.basic || []).map((x) => x.rel).filter((rel) => have.some((f) => BASIC_NEEDS[f].some((p) => rel.startsWith(p))));
  if (!miss.length) return;
  F("basicMissingText").textContent = `有模型要用的檔不見了：${miss.join("、")}。可能是安裝到一半被關掉，現在畫圖會失敗。`;
  F("basicMissing").hidden = false;
}

const isQwenAnime = () => fam() === "qwen21" && /anime/i.test(F("model").value || "");
// MOSS-Image：官方使用說明建議的負面詞拿掉 nsfw（設定頁的預設負面詞是 Anima 用的 tag）
const MOSS_NEG = "worst quality, bad anatomy, watermark, jpeg artifact";
// SDXL 系（Illustrious、NoobAI、Pony）：NoobAI-XL 1.1 官方說明的負面詞，拿掉 nsfw 和防獸人的那幾個（跟 graphs.rs SDXL_NEG 同一句）
const SDXL_NEG = "worst quality, old, early, low quality, lowres, signature, username, logo, bad hands, mutated hands";
// SDXL 加速版（Lightning、Hyper、Turbo、DMD2、LCM）：檔名看得出來（跟 graphs.rs is_fast_sdxl 同一套）
const isFastSdxl = () => /lightning|hyper|turbo|dmd|lcm/i.test(F("model").value || "");
const sdxlSub = () => app.state?.models?.unet_sub?.[F("model").value] || "";
// 生圖工作流：負面詞空白＝照工作流自己寫的
// Krea 2：Turbo 是 CFG 1，官方範本的負面＝把正面歸零（graphs.rs neg_text），負面詞不會用到；預設留空
const defaultNeg = () => (fam() === "qwen21" ? (isQwenAnime() ? QWEN_ANIME_NEG : QWEN_NEG) : fam() === "moss" ? MOSS_NEG : fam() === "sdxl" ? SDXL_NEG : fam() === "custom" || fam() === "krea2" ? "" : (app.state?.settings?.negative || ""));

// MOSS 快速：lodestones 的 8 步 DMD 加速 LoRA 掛 0.35、步數 10（2026-10-17 實測 3070：25 步約 40 秒 → 約 16 秒，構圖畫風接近）
// 勾選狀態不另外存：清單裡有啟用這顆 LoRA 就算開著（手動移除它，勾勾也會跟著取消）
const MOSS_FAST = listed({ re: /(^|[\\/])moss_speed_dmd[^\\/]*\.safetensors$/i, rel: "loras/moss/moss_speed_dmd_lodestones.safetensors", strength: 0.35, steps: 10, normal: 25 });
const fastLora = () => (app.state?.models?.loras || []).find((x) => MOSS_FAST.is(x.name));
const fastOn = () => S.loras.some((l) => l.enabled && MOSS_FAST.is(l.name));

// MOSS 4B 文字編碼器（2026-10-31）：Qwen3.5-4B＋投影轉接器，graphs.rs MOSS_TE_4B／MOSS_4B_ADAPTER。不在 LoRA 清單裡，問後端有沒有
const MOSS_4B = [["text_encoders/Qwen3.5-4B_int8_convrot.safetensors", " MOSS 4B 文字編碼器", "4.8 GB"],
  ["moss_encoder_adapters/moss_qwen35_4b_to_9b_1600_train.safetensors", " MOSS 4B 轉接器", "21 MB"]];
// 都有＝true；缺的話一個接一個下載（每個都先問），回 false
async function have4b() {
  const miss = [];
  for (const f of MOSS_4B) {
    try { if (!(await api("/api/models/fetch_listed", { json: { rel: f[0], check: true } })).have) miss.push(f); } catch { return true; }
  }
  if (!miss.length) return true;
  const next = () => { const f = miss.shift(); if (f) fetchListed(f[0], f[1], f[2], miss.length ? next : () => toast("4B 文字編碼器都下載好了，再按一次生成", "ok")); };
  next();
  return false;
}

// MOSS 色彩／深度控制的轉接器（darask0，2026-10-31）：用一般 LoRA 載入器掛，所以放在 loras 底下；graphs.rs MOSS_CONTROL_LORA
const MOSS_CONTROL = listed({ re: /(^|[\\/])moss-color-depth-control[^\\/]*\.safetensors$/i, rel: "loras/moss/moss-color-depth-control.safetensors" });
const controlLora = () => (app.state?.models?.loras || []).find((x) => MOSS_CONTROL.is(x.name));

// MOSS 照指令改圖（2026-10-30）：圖生圖時輸入圖當參考圖（ComfyUI 的 TextEncodeMOSSImageEdit，comfy_moss 補丁移植），掛編輯 LoRA
// 預設的 chinoll Moss-Image-Edit 在 MOSS 包裡；使用者自己掛了別的編輯 LoRA（Civitai）就不加
const MOSS_EDIT = listed({ re: /(^|[\\/])moss_edit_chinoll[^\\/]*\.safetensors$/i, rel: "loras/moss/moss_edit_chinoll.safetensors", strength: 1.0 });
const editOn = () => fam() === "moss" && S.mode === "img2img" && F("mossEdit").checked;
function setMossEdit(on) {
  F("mossEdit").checked = on;
  if (on && !S.loras.some((l) => l.enabled && /edit|a2r|2real|2anime/i.test(l.name))) {
    const l = (app.state?.models?.loras || []).find((x) => MOSS_EDIT.is(x.name));
    if (!l) fetchListed("loras/moss/moss_edit_chinoll.safetensors", " MOSS 編輯 LoRA", "300 MB", () => F("mossEdit").checked && setMossEdit(true));
    else {
      const have = S.loras.find((x) => x.name === l.name);
      if (have) Object.assign(have, { enabled: true, strength: MOSS_EDIT.strength });
      else S.loras.push({ name: l.name, strength: MOSS_EDIT.strength, enabled: true, title: l.title, min: l.min, max: l.max, slider: l.slider });
    }
  }
  renderLoras();
  familyUI();
  save();
}

// MOSS 去色帶（2026-10-31）：MATLOWAI/MOSS-Image-SFT-DeBand 的 L37–41 權重差，一般 LoRA 載入器、強度 1.0，只對 SFT 模型有意義
const MOSS_DEBAND = listed({ re: /(^|[\\/])moss_image_sft_banding_fix_L37-41[^\\/]*\.safetensors$/i, rel: "loras/moss/Excellent-Loras/SFT伪影修复/moss_image_sft_banding_fix_L37-41_bf16.safetensors" });
const debandOn = () => S.loras.some((l) => l.enabled && MOSS_DEBAND.is(l.name));
function setMossDeband(on) {
  if (on) {
    const l = (app.state?.models?.loras || []).find((x) => MOSS_DEBAND.is(x.name));
    if (!l) {
      F("mossDeband").checked = false;
      return fetchListed(MOSS_DEBAND.rel, " MOSS 去色帶修正", "1.1 GB", () => { F("mossDeband").checked = true; setMossDeband(true); });
    }
    const have = S.loras.find((x) => x.name === l.name);
    if (have) Object.assign(have, { enabled: true, strength: 1.0 });
    else S.loras.push({ name: l.name, strength: 1.0, enabled: true, title: l.title, min: l.min, max: l.max, slider: l.slider });
  } else S.loras = S.loras.filter((x) => !MOSS_DEBAND.is(x.name));
  renderLoras();
  save();
}

function setMossFast(on) {
  if (on) {
    const l = fastLora();
    if (!l) {
      F("mossFast").checked = false;
      return fetchListed(MOSS_FAST.rel, " MOSS 加速 LoRA", "1.1 GB", () => { F("mossFast").checked = true; setMossFast(true); });
    }
    const have = S.loras.find((x) => x.name === l.name);
    if (have) Object.assign(have, { enabled: true, strength: MOSS_FAST.strength });
    else S.loras.push({ name: l.name, strength: MOSS_FAST.strength, enabled: true, title: l.title, min: l.min, max: l.max, slider: l.slider });
    S.fastPrevSteps = +F("steps").value;
    F("steps").value = MOSS_FAST.steps;
  } else {
    S.loras = S.loras.filter((x) => !MOSS_FAST.is(x.name));
    F("steps").value = S.fastPrevSteps && S.fastPrevSteps !== MOSS_FAST.steps ? S.fastPrevSteps : MOSS_FAST.normal;
  }
  renderLoras();
  modelHint();
  save();
}

// 換模型：負面詞若還是某個模型的預設值，就換成新模型的預設（自己改過的不動）；換家族時圖生圖重繪幅度也換成建議值
export function mmSize(ratio, mp) {
  const [a, b] = ratio.split(":").map(Number);
  const scale = Math.sqrt(mp * 1024 * 1024 / (a * b));
  return { w: Math.round(a * scale / 32) * 32, h: Math.round(b * scale / 32) * 32 };
}

function mmDimensions() {
  const { w, h } = mmSize(F("mmRatio").value, +F("mmMp").value);
  F("width").value = w;
  F("height").value = h;
  syncSize();
}

function mmEncoders(names) {
  const files = (pattern) => names.filter((n) => pattern.test(n.split(/[\\\\/]/).pop()));
  const te = files(/^qwen3vl_32b_minimax_h3_(nvfp4_awq|int8_convrot)\.safetensors$/i);
  const pe = files(/^(qwen3\.5_9b_w4a8_gptq|qwen3\.8_27b_minimax_h3_image_pe_w4a8_gptq)\.safetensors$/i);
  const prior = F("mmPe").options.length ? F("mmPe").value : store.get("form", {}).mmPe;
  fillSelect(F("mmTe"), te, pendingSel.mmTe || te.find((n) => n.includes("nvfp4")));
  F("mmPe").replaceChildren(el("option", {value:""}, "關"), ...pe.map((n) => el("option", {value:n}, n.includes("27b") ? "27B PE" : "9B")));
  F("mmPe").value = pe.includes(prior) ? prior : "";
}

function mmDefaultPe() {
  // 預設關閉提示詞擴寫器（8GB 顯卡跑思考 VLM 很慢，且可能過濾內容；使用者可手動開啟）
  const sel = F("mmPe");
  if (!sel.value) sel.selectedIndex = 0;
}

function familySwitch() {
  const f = fam();
  const neg = F("negative").value.trim();
  if (!neg || [app.state?.settings?.negative || "", QWEN_NEG, QWEN_ANIME_NEG, MOSS_NEG, SDXL_NEG].some((d) => d.trim() === neg)) F("negative").value = defaultNeg();
  // 換到另一組工作流（家族都是 custom）也要重新檢查 LoRA 合不合
  const wfChanged = f === "custom" && S.wfModel !== F("model").value;
  if (S.family && S.family !== f) {
    if (S.mode === "img2img") { F("denoise").value = f === "qwen21" || f === "mmimage" ? 1 : 0.6; F("denoiseOut").textContent = F("denoise").value; }
  }
  if ((S.family && S.family !== f) || wfChanged) {
    const off = S.loras.filter(mismatch).length;
    if (off) toast(`清單裡有 ${off} 個 LoRA 不適用這個模型，生成時會略過（劃掉的那幾個）`);
    renderLoras();
  }
  if (S.family !== f) {
    if (f === "mmimage") { mmDimensions(); mmDefaultPe(); }
    syncSize();
    if (refs.isMultiFamily(f) && refs.get().length > refs.maxRefs(f)) toast(`目前有 ${refs.get().length} 張參考圖，新家族最多 ${refs.maxRefs(f)} 張，請先移除多出的圖。`, "err");
    else if (!refs.isMultiFamily(f) && refs.get().length > 1) toast("目前模型只吃第 1 張主圖，其餘參考圖會先保留（切回 Qwen 2.1／MiniMax 可用全部）");
    refs.render();
  }
  prompttabs.refresh();
  S.family = f;
  S.wfModel = f === "custom" ? F("model").value : null;
  library.fillPickers();
}

function fillSelect(sel, items, want) {
  const cur = sel.value || want;
  sel.innerHTML = "";
  for (const it of items) sel.append(el("option", { value: it }, it.replace(/\.safetensors$|\.pth$/, "")));
  if (items.includes(cur)) sel.value = cur;
  else if (items.includes(want)) sel.value = want;
}

function isTurboModel() { return /turbo/i.test(F("model").value || ""); }
// 官方 Qwen-Image-2.1-Turbo（完整模型，或掛官方 Turbo LoRA）：後端用官方 8 步排程（graphs::is_official_qwen21_turbo）
const OFFICIAL_TURBO = /qwen[_-]image[_-]2\.1[_-]turbo/i;
function isOfficialTurbo() {
  return fam() === "qwen21" && (OFFICIAL_TURBO.test(F("model").value || "") || S.loras.some((l) => l.enabled && l.strength && OFFICIAL_TURBO.test(l.name)));
}

// 下拉選單有這個選項才換（ComfyUI 版本不同，清單可能不一樣）
function pick(id, v) { if ([...F(id).options].some((o) => o.value === v)) F(id).value = v; }

function modelDefaults() {
  // 加速 LoRA 只給 MOSS 用，換到別的模型就拿掉
  if (fam() !== "moss" && S.loras.some((l) => MOSS_FAST.is(l.name) || MOSS_DEBAND.is(l.name))) {
    S.loras = S.loras.filter((x) => !MOSS_FAST.is(x.name) && !MOSS_DEBAND.is(x.name));
    renderLoras();
  }
  if (fam() === "moss") { F("steps").value = fastOn() ? MOSS_FAST.steps : MOSS_FAST.normal; F("cfg").value = 3; pick("sampler", "euler"); pick("scheduler", "sgm_uniform"); }
  // 切過 MOSS（sgm_uniform）、SDXL（euler_ancestral）再切回來，取樣器和排程也要回家族預設（對照 api_agent_tools.rs 的 family_defaults；PR #2，SuperBird007）
  else if (fam() === "qwen21") { F("steps").value = isOfficialTurbo() ? 8 : isTurboModel() ? 6 : 25; F("cfg").value = isOfficialTurbo() || isTurboModel() ? 1 : 3; pick("sampler", "euler"); pick("scheduler", "simple"); }
  // Krea 2 Turbo：ComfyUI 內建範本 8 步、CFG 1、euler＋simple。Raw（沒蒸餾）沒有官方建議值：先給一般 DiT 的 30 步、CFG 4
  else if (fam() === "mmimage") { F("steps").value = 6; F("cfg").value = 1; pick("sampler", "euler"); pick("scheduler", "simple"); }
  else if (fam() === "krea2") { F("steps").value = isTurboModel() ? 8 : 30; F("cfg").value = isTurboModel() ? 1 : 4; pick("sampler", "euler"); pick("scheduler", "simple"); }
  // SDXL：NoobAI 官方建議 25～30 步、CFG 5～6、Euler a；加速版 8 步、CFG 1.5（各家建議不一樣，畫不好再照模型頁調）
  else if (fam() === "sdxl") { F("steps").value = isFastSdxl() ? 8 : 28; F("cfg").value = isFastSdxl() ? 1.5 : 5; pick("sampler", "euler_ancestral"); pick("scheduler", isFastSdxl() ? "sgm_uniform" : "normal"); }
  else if (fam() === "custom") { /* 步數、CFG 照工作流，這裡的不用 */ }
  else if (isTurboModel()) { F("steps").value = 8; F("cfg").value = 1; pick("sampler", "euler"); pick("scheduler", "simple"); }
  else {   // 有沒有掛加速 LoRA（turbo.js，助手頁同一套）：有＝euler，沒有＝Anima 一般版的 er_sde
    const t = speedup.turboSteps(F("turboLora").value);
    F("steps").value = t.steps; F("cfg").value = t.cfg;
    pick("sampler", F("turboLora").value ? "euler" : "er_sde"); pick("scheduler", "simple");
  }
  modelHint();
}

function modelHint() {
  const q = fam() === "qwen21";
  F("turboRow").hidden = fam() !== "anima" || isTurboModel();
  // Qwen 2.1 Turbo：graphs.sample 用 ManualSigmas（turbo_sigmas），KSampler 的步數和排程不會用到
  const official = isOfficialTurbo();
  const locked = q && (isTurboModel() || official);
  // 工作流組：步數、CFG、取樣器、排程預設照工作流（在「工作流參數」改）；勾了「用生成頁的步數、CFG、取樣器」才用這裡的旋鈕
  const wf = fam() === "custom", wfLock = wf && !wfSet()?.options?.page_sampler;
  for (const id of ["steps", "scheduler"]) {
    F(id).disabled = locked || wfLock;
    F(id).title = locked ? `這個模型固定 ${official ? 8 : 6} 步排程，改了不會生效` : wfLock ? "照工作流（要用這裡的：勾「工作流參數」裡的「用生成頁的步數、CFG、取樣器」）" : "";
  }
  for (const id of ["cfg", "sampler"]) {
    F(id).disabled = wfLock;
    F(id).title = wfLock ? "照工作流（要用這裡的：勾「工作流參數」裡的「用生成頁的步數、CFG、取樣器」）" : "";
  }
  F("samplerLock").hidden = !(locked || wfLock);
  if (locked) F("samplerLock").textContent = official
    ? "官方 Qwen 2.1 Turbo 固定用官方的 8 步排程，步數和排程改了不會生效；取樣器、CFG 仍然有作用。"
    : "Qwen 2.1 Turbo 固定用作者的 6 步排程（依尺寸自動計算），步數和排程改了不會生效；取樣器、CFG 仍然有作用。";
  else if (wfLock) F("samplerLock").textContent = "工作流的步數、CFG、取樣器、排程照工作流本身，在上面的「工作流參數」改；勾「用生成頁的步數、CFG、取樣器」才會用這裡的。";
  F("mossFastRow").hidden = fam() !== "moss";
  F("mossFast").checked = fastOn();
  F("mossDebandRow").hidden = fam() !== "moss" || !/sft/i.test(F("model").value || "");
  F("mossDeband").checked = debandOn();
  F("mossTeRow").hidden = fam() !== "moss";
  F("exportRow").hidden = wf;  // 匯入的工作流本來就是 ComfyUI 的工作流，匯出在設定頁
  F("modelHint").textContent = wf
    ? "匯入的工作流：提示詞、負面提示詞（空白＝照工作流）、種子、寬高（文生圖）、張數、輸入圖、遮罩都會套進工作流；生成頁選的 LoRA 會自動接在主模型載入節點後面。" +
      "步數、CFG、取樣器等其他欄位在下面的「工作流參數」改。要指定放哪裡，可以在節點標題或文字裡寫 {{prompt}}、{{negative}}、{{image}}、{{mask}}、{{model}}、{{sampler}} 這些標記（說明在設定 → 模型與工作流）。" +
      (wfFam() ? `模型家族：${famLabel(wfFam())}，LoRA 只會接同家族的。` : "看不出這組工作流的模型家族，LoRA 都會接上去。")
    : fam() === "moss"
    ? (fastOn()
      ? "MOSS 快速開著：加速 LoRA 0.35＋10 步（CFG 3、euler＋sgm_uniform 照舊），3070 一張約 16 秒。構圖畫風跟 25 步接近，衣服花紋、小配件等細節會換樣子；要最細的那張就取消快速、用 25 步。"
      : "MOSS-Image：25 步、CFG 3、euler＋sgm_uniform（官方 50 步；實測 25 步看不出差別、快一倍。負面提示詞有效）。3070 一張 1024 約 40 秒。") +
      "擅長排版和畫字（中英文都行），提示詞用完整句子描述；要畫的字放在引號裡。不支援 Anima 的 LoRA、構圖控制。"
    : fam() === "mmimage" ? "MiniMax H3 Image 支援文生圖、圖生圖／改圖（最多 16 張參考圖）、局部重繪、擴圖與多視圖編輯。以 <Picture 1> 指名第一張圖，建議 6 步、CFG 1。"
    : fam() === "krea2"
    ? (isTurboModel()
      ? "Krea 2 Turbo：8 步、CFG 1、euler＋simple（ComfyUI 官方範本；負面提示詞無效）。寫實照片、插畫、各種畫風都行，提示詞用英文完整句子描述畫面。可以疊 Krea 2 的 LoRA（官方畫風 LoRA 要寫觸發詞）。"
      : "Krea 2 Raw（沒蒸餾的底模，多半拿來練 LoRA）：先用 30 步、CFG 4，負面提示詞有效。日常出圖建議換 Turbo。")
    : fam() === "sdxl"
    ? (isFastSdxl()
      ? "SDXL 加速版：先用 8 步、CFG 1.5、euler_ancestral＋sgm_uniform（Lightning、DMD2 各家建議不同，畫不好照模型頁調）。"
      : "SDXL：28 步、CFG 5、euler_ancestral＋normal，負面提示詞有效。832×1216、1024×1024 這類約 1MP 的尺寸最穩。") +
      (sdxlSub() === "Pony"
        ? "Pony 系：提示詞開頭寫 score_9, score_8_up, score_7_up，負面寫 score_4, score_5, score_6。"
        : "Illustrious／NoobAI 系：用 Danbooru tag，開頭加 masterpiece, best quality, newest, absurdres, highres。") +
      "只能疊 SDXL 的 LoRA；不支援 Anima 的構圖控制。"
    : q
    ? (official
      ? "官方 Qwen 2.1 Turbo：固定 8 步、CFG 1（負面提示詞無效）。官方建議約 2K（例如 1696×2528），1024 也能畫。研究授權，不能商用。不要再疊其他加速 LoRA。"
      : isTurboModel()
      ? "Noct Q Turbo：固定 6 步、CFG 1（負面提示詞無效）。832×1248 畫人體最穩。提示詞用英文完整句子描述效果最好。"
      : isQwenAnime()
        ? "Noct Q Anime：提示詞開頭寫 An anime illustration of…（沒寫 anime 會畫成照片）。25 步、CFG 3。想快：加「Pruna 加速 8 步」LoRA，步數 8、CFG 1、取樣器 er_sde、排程 beta（畫面會偏灰，適合打草稿）。"
        : "Qwen 2.1：25 步、CFG 3（負面提示詞有效、較慢）；CFG 改 1 快一倍，但負面詞失效。想快可加「Pruna 加速 8 步」LoRA。")
    : isTurboModel()
      ? "turbo 模型：8 步、CFG 1（光影張力較強）。不要再疊 Turbo LoRA。"
      : F("turboLora").value ? "加速 LoRA 開啟：8～12 步、CFG 1。" : "未加速：約 30 步、CFG 4（品質優先、較慢）。";
  familyUI();
  syncWf();
}

// ---------------------------------------------------------------- 工作流組（模式鍵、工作流參數面板）
// 工作流組只有文生圖、圖生圖、局部重繪、放大、多圖參考（image_wf.rs 的 MODES）；擴圖、四視圖這些是內建流程獨有的
/** 選了工作流組：模式鍵只亮它有的（其他灰掉、滑鼠停上去講原因），目前的模式它沒有就切到它的第一個模式，並更新「工作流參數」面板。換回內建模型就全部恢復 */
let wfFresh = false;  // onState 呼叫的那一輪：工作流參數要重新讀（ComfyUI 剛上線、模型清單變了）
/** 把前端合併的 img2img 映射到後端／工作流真實模式（單張→img2img，多張→multiref；工作流組依它實際擁有的模式） */
export function effectiveMode() {
  if (S.mode !== "img2img") return S.mode;
  const f = fam(), n = refs.get().length;
  if (f === "custom") {
    const w = wfSet();
    const hasI = !!w?.modes?.includes("img2img"), hasM = !!w?.modes?.includes("multiref");
    if (hasM && !hasI) return "multiref";
    if (hasI && !hasM) return "img2img";
    return n >= 2 ? "multiref" : "img2img";
  }
  if (f === "qwen21" || f === "mmimage") return n >= 2 ? "multiref" : "img2img";
  return "img2img";
}

function syncWf() {
  const w = wfSet(), wf = fam() === "custom";
  const wfHas = (m) => (m === "img2img" ? (w.modes.includes("img2img") || w.modes.includes("multiref")) : w.modes.includes(m));
  for (const b of $$("#modeSeg button")) {
    if (b.dataset.t0 === undefined) b.dataset.t0 = b.title || "";
    const m = b.dataset.mode;
    const off = wf && !!w && !wfHas(m);
    b.disabled = off;
    b.title = !off ? b.dataset.t0
      : ["txt2img", "img2img", "inpaint", "upscale"].includes(m)
        ? `「${w.name}」這組工作流沒有${modeName[m]}的工作流。到「設定 → 模型與工作流」，在匯入時選「${modeName[m]}」、放進「${w.name}」就能補上。`
        : "匯入的工作流沒有這個功能（只有文生圖、圖生圖／改圖、局部重繪、放大）：想用它請換回一般模型。";
  }
  // 匯出成 ComfyUI 工作流：只有工作流組也有的那幾種模式匯得出來（匯回去才有地方放）
  const em = effectiveMode();
  const eb = F("exportGraph"), canExp = ["txt2img", "img2img", "inpaint", "upscale", "multiref"].includes(em);
  if (eb.dataset.t0 === undefined) eb.dataset.t0 = eb.title;
  eb.disabled = !canExp;
  eb.title = canExp ? eb.dataset.t0 : "這個功能沒辦法匯出成工作流（可以匯出的：文生圖、圖生圖／改圖、局部重繪、放大）";
  if (wf && w && w.modes.length && !wfHas(S.mode)) {
    const old = S.mode;
    const target = w.modes[0] === "multiref" ? "img2img" : w.modes[0];
    toast(`「${w.name}」沒有${modeName[old] || old}的工作流，已切到${modeName[target]}`);
    setMode(target);
    return;  // setMode 裡面會再同步一次
  }
  wfpanel.sync(wf ? w : null, em, wfFresh);
}

/** 打開「用生成頁的步數、CFG、取樣器」：生成頁的旋鈕先對齊工作流原本的值（不然會拿 Anima 的預設去跑別的模型） */
function adoptSampler(v) {
  if (v.steps != null) F("steps").value = v.steps;
  if (v.cfg != null) F("cfg").value = v.cfg;
  if (v.sampler_name) pick("sampler", v.sampler_name);
  if (v.scheduler) pick("scheduler", v.scheduler);
  save();
}

/** 「拿去 ComfyUI 改…」：先開小視窗說明要存什麼、怎麼改、怎麼匯回來（三步說明第一次自動打開），按「存成檔…」才存 */
function exportBuiltin() {
  const p = collectParams();
  const f = fam();
  F("expWhat").textContent = `要存的是：${app.state?.models?.family_names?.[f] || f}的「${modeName[S.mode] || S.mode}」，照生成頁現在的設定（模型、提示詞、步數…）。`;
  // 還沒放圖也能存：後端用佔位圖代替（image_wf::export_placeholders）
  const miss = [];
  if (["img2img", "inpaint", "upscale"].includes(p.mode) && !p.init_image) miss.push("輸入圖");
  if (p.mode === "inpaint" && !p.mask) miss.push("遮罩");
  if (p.mode === "multiref" && !p.refs?.length) miss.push("參考圖");
  F("expHolder").hidden = !miss.length;
  F("expHolder").textContent = miss.length ? `還沒放${miss.join("、")}：先用佔位圖代替（灰色的圖、中間一塊方形的遮罩），到 ComfyUI 再換成你要的。` : "";
  F("expDone").hidden = F("expOpen").hidden = true;
  F("expSteps").open = !store.get("expSeen", false);
  store.set("expSeen", true);
  openModal("#expModal");
  F("expSave").focus();
}

async function exportSave() {
  const b = F("expSave");
  b.disabled = true;
  try {
    const r = await wfpanel.exportGraph(collectParams());
    if (r.cancelled) return;
    F("expDone").textContent = `已存到 ${r.path}`;
    F("expOpen").dataset.path = r.path;
    F("expDone").hidden = F("expOpen").hidden = false;
  } catch (e) {
    toast("存不了：" + e.message, "err");
  } finally {
    b.disabled = false;
  }
}

/** 到「設定 → 模型與工作流」的匯入欄 */
function goImport() {
  closeModal("#expModal");
  app.showPage("settings");
  models.focusImport();
}

// ---------------------------------------------------------------- 版面框（MOSS 專用，元件在 layout.js；後端 graphs.rs 的 Layout）
let lay = null;
const layOn = () => fam() === "moss" && (S.mode === "txt2img" || S.mode === "img2img");
function layCount() {
  const n = lay?.count() || 0;
  F("layCount").hidden = !n;
  F("layCount").textContent = String(n).padStart(2, "0");
}
// 畫布比例：圖生圖自動尺寸跟輸入圖，其他照寬高
function layAspect() {
  if (!lay) return;
  const auto = S.mode === "img2img" && F("autoSize").checked && S.init?.width;
  lay.setAspect(auto ? S.init.width : +F("width").value, auto ? S.init.height : +F("height").value);
}

// LLLite 只有 Anima 有：Qwen 2.1、MOSS-Image、MiniMax 的構圖控制、LLLite 重繪都不適用
function familyUI() {
  const mm = fam() === "mmimage", q = fam() === "qwen21", noLllite = fam() !== "anima";
  const multi = refs.isMultiFamily(fam());
  const multiActive = S.mode === "img2img" && multi && refs.get().length >= 2;
  F("negDetails").hidden = mm;
  prompttabs.refresh();
  F("mmOptions").hidden = !mm;
  F("mmCanvas").hidden = !mm || (S.mode === "img2img" && F("autoSize").checked);
  if (S.mode === "img2img") F("autoSizeRow").lastChild.textContent = mm ? " 跟主圖一樣比例（自動上限約 1.5MP）" : " " + SIZE_AUTO_TEXT.img2img;
  F("refTagHint").textContent = mm ? "<Picture 1>" : "<image1>";
  F("refPixels").hidden = mm || !multi;
  F("refPixels").previousElementSibling.hidden = mm || !multi;
  const rme = F("refMultiExtra");
  if (rme) rme.hidden = !multi;
  F("refFamilyWarn").hidden = multi || S.mode !== "img2img";
  if (F("refSwitchModel")) F("refSwitchModel").hidden = !familyModel("qwen21");
  if (F("refSwitchMm")) F("refSwitchMm").hidden = !familyModel("mmimage");
  F("layBox").hidden = !layOn();
  // MOSS：文生圖用色彩／深度控制（另一套欄位，graphs.rs moss_control）
  const mossCtl = fam() === "moss" && S.mode === "txt2img";
  F("controlBox").hidden = !mossCtl && (noLllite || !(S.mode === "txt2img" || S.mode === "img2img"));
  for (const id of ["ctrlTypeWrap", "ctrlPreWrap", "ctrlInvertRow"]) F(id).hidden = mossCtl;
  F("ctrlMossRow").hidden = !mossCtl;
  F("hiresBox").hidden = mm || fam() === "custom" || !(S.mode === "txt2img" || (S.mode === "img2img" && !multiActive));
  F("ipMethodRow").hidden = noLllite;
  // 工作流組：寬高只有文生圖照生成頁；遮罩擴張／柔化、放大模型這些都在工作流裡（「工作流參數」改），生成頁這幾塊沒作用就藏起來
  const cw = fam() === "custom";
  F("sizeBox").hidden = ["inpaint", "upscale", "strip", "sheet", "outpaint"].includes(S.mode) || (cw && S.mode !== "txt2img");
  F("inpaintBox").hidden = S.mode !== "inpaint" || cw;
  F("upscaleBox").hidden = S.mode !== "upscale" || cw;
  F("denoiseBox").hidden = !(S.mode === "inpaint" || (S.mode === "img2img" && !multiActive));
  F("denoiseHint").textContent = (q ? MODE_HINT_Q : mm ? MODE_HINT_MM : fam() === "moss" ? MODE_HINT_M : MODE_HINT)[S.mode] || "";
  F("mossEditRow").hidden = !(fam() === "moss" && S.mode === "img2img");
  if (editOn()) F("denoiseHint").textContent = "照指令改圖：輸入圖只當參考，從空白重畫（重繪固定 1）。提示詞寫要怎麼改，例如「把外套改成紅色，其他保持不變」。";
  pe.refreshTask();
  renderLocked();
}

// ---------------------------------------------------------------- 模式與單一事實來源同步
let syncingRefs = false;
function syncInitFromRefs() {
  if (syncingRefs || S.mode !== "img2img") return;
  const first = refs.get()[0] || null;
  if ((S.init?.name || null) === (first?.name || null)) return;
  S.init = first ? { name: first.name, width: first.width, height: first.height, ...(first.params ? { params: first.params } : {}) } : null;
  S.mask = null;
  showInit();
  viewsrc.onInput();
}

function syncRefsFromInit() {
  if (syncingRefs) return;
  const list = refs.get();
  if (S.init && list[0]?.name !== S.init.name) {
    syncingRefs = true;
    try { refs.setPrimary(S.init, true); } finally { syncingRefs = false; }
  } else if (!S.init && list.length) {
    syncInitFromRefs();
  }
}

function setMode(mode) {
  // 舊圖重用、提示詞庫、localStorage 留著的 "multiref" 一律正規化到合併後的 "img2img"
  if (mode === "multiref") mode = "img2img";
  const modeChanged = S.mode !== mode;
  S.mode = mode;
  viewsrc.onMode(mode);
  if (fam() === "mmimage" && modeChanged) mmDefaultPe();
  $$("#modeSeg button").forEach((b) => b.classList.toggle("on", b.dataset.mode === mode));
  const needInit = mode !== "txt2img";
  F("refBox").hidden = mode !== "img2img";
  F("initBox").hidden = !needInit;
  F("maskEdit").hidden = mode !== "inpaint";
  F("sizeBox").hidden = mode === "inpaint" || mode === "upscale" || mode === "strip" || mode === "sheet" || mode === "outpaint";
  F("autoSizeRow").hidden = mode !== "img2img";
  F("autoSizeRow").lastChild.textContent = " " + (SIZE_AUTO_TEXT[mode] || SIZE_AUTO_TEXT.img2img);
  F("denoiseBox").hidden = !(mode === "img2img" || mode === "inpaint");
  F("denoiseHint").textContent = MODE_HINT[mode] || "";
  F("inpaintBox").hidden = mode !== "inpaint";
  F("upscaleBox").hidden = mode !== "upscale";
  F("outpaintBox").hidden = mode !== "outpaint";
  F("stripBox").hidden = mode !== "strip";
  F("sheetBox").hidden = mode !== "sheet";
  F("promptBox").hidden = mode === "strip" || mode === "sheet";  // 一鍵脫衣、四視圖不用提示詞框（指令寫在各自的區塊）
  F("hiresBox").hidden = !(mode === "txt2img" || mode === "img2img");
  F("controlBox").hidden = !(mode === "txt2img" || mode === "img2img");
  if (mode === "img2img") syncRefsFromInit();
  if (mode === "img2img" && app.state) {
    if (fam() === "qwen21" || fam() === "mmimage") F("denoise").value = 1;
    else if (!(num("denoise") < 1)) F("denoise").value = 0.6;
  }
  if (mode === "inpaint") F("denoise").value = fam() === "mmimage" ? 1 : fam() === "qwen21" ? 0.9 : fam() === "anima" && F("ipMethod").value === "lllite" ? 1 : 0.75;
  F("denoiseOut").textContent = F("denoise").value;
  familyUI();
  syncSize();
  showInit();
  opPreview();
  save();
  syncWf();  // 選了工作流組：這個模式它沒有就切走；更新工作流參數面板
}

// ---------------------------------------------------------------- 擴圖（graphs.rs 的 outpaint：ImagePadForOutpaint＋局部重繪那條路）
const OP_SIDES = ["opTop", "opBottom", "opLeft", "opRight"];
const opVals = () => Object.fromEntries(OP_SIDES.map((id) => [id, Math.max(0, Math.round(+F(id).value || 0))]));
function opQuick(kind) {
  const w = S.init?.width, h = S.init?.height;
  if (!w && kind !== "clear") return toast("先放一張輸入圖", "err");
  const r8 = (v) => Math.ceil(Math.max(0, v) / 8) * 8;
  const set = (t, b, l, r) => { [F("opTop").value, F("opBottom").value, F("opLeft").value, F("opRight").value] = [t, b, l, r].map(r8); };
  if (kind === "clear") set(0, 0, 0, 0);
  else if (kind === "all") set(h / 4, h / 4, w / 4, w / 4);
  else if (kind === "wide") set(0, 0, w / 4, w / 4);
  else if (kind === "tall") set(h / 4, h / 4, 0, 0);
  else if (kind === "down") set(0, h / 2, 0, 0);
  else {
    // 變成某個比例：只往短的那個方向補，兩邊平均
    const [a, b] = kind.split(":").map(Number), want = a / b;
    if (w / h < want) { const add = h * want - w; set(0, 0, add / 2, add / 2); }
    else { const add = w / want - h; set(add / 2, add / 2, 0, 0); }
  }
  opPreview();
  save();
}
function opPreview() {
  const w = S.init?.width, h = S.init?.height;
  const v = opVals();
  const W = (w || 0) + v.opLeft + v.opRight, H = (h || 0) + v.opTop + v.opBottom;
  F("opSize").textContent = w ? `${W}×${H}` : "";
  const box = F("opPreview"), o = F("opOrig");
  if (!w) { box.style.aspectRatio = "3 / 2"; o.hidden = true; F("opInfo").textContent = "先放輸入圖"; return; }
  o.hidden = false;
  box.style.aspectRatio = `${W} / ${H}`;
  Object.assign(o.style, { left: `${(100 * v.opLeft) / W}%`, top: `${(100 * v.opTop) / H}%`, width: `${(100 * w) / W}%`, height: `${(100 * h) / H}%`,
    backgroundImage: `url("/input/${encodeURIComponent(S.init.name)}")` });
  F("opInfo").textContent = W * H > 4096 * 4096 ? "太大了（上限 4096×4096）" : `${w}×${h} → ${W}×${H}`;
}

function syncSize() {
  const auto = S.mode === "img2img" && F("autoSize").checked;
  const mm = fam() === "mmimage";
  F("mmCanvas").hidden = !mm || auto;
  F("sizeInputs").hidden = mm || auto;
  F("sizeChips").hidden = mm || auto;
  const w = +F("width").value, h = +F("height").value;
  $$("#sizeChips .chip").forEach((c) => c.classList.toggle("on", +c.dataset.w === w && +c.dataset.h === h));
  F("sizeInfo").textContent = auto ? "依主圖比例" : `${((w * h) / 1e6).toFixed(2)} MP`;
  layAspect();
}

// ---------------------------------------------------------------- 輸入圖、控制圖
export function setInit(r, replaceAll = false) {
  S.init = r;
  S.mask = null;
  if (S.mode === "txt2img") setMode("img2img");
  if (S.mode === "img2img" && !syncingRefs) {
    syncingRefs = true;
    try { refs.setPrimary(r, !replaceAll); } finally { syncingRefs = false; }
  }
  showInit();
  viewsrc.onInput();
  save();
}

function showInit() {
  const d = F("initDrop"), img = F("initImg");
  d.querySelector(".mask-overlay")?.remove();
  if (S.init) {
    img.src = `/input/${encodeURIComponent(S.init.name)}`;
    d.classList.add("has");
    F("initInfo").textContent = `${S.init.width}×${S.init.height}` + (S.mode === "inpaint" ? (S.mask ? "・已畫遮罩" : "・尚未畫遮罩") : "");
    if (S.mask && S.mode === "inpaint") d.append(el("img", { class: "mask-overlay", src: `/input/${encodeURIComponent(S.mask.name)}` }));
  } else {
    img.removeAttribute("src");
    d.classList.remove("has");
    F("initInfo").textContent = "";
  }
  F("initParams").hidden = !S.init?.params;
  layAspect();
  opPreview();
}

function showSheetRef() {
  const d = F("sheetRefDrop"), img = F("sheetRefImg");
  if (S.sheetRef) { img.src = `/input/${encodeURIComponent(S.sheetRef.name)}`; img.title = "點一下移除"; d.classList.add("has"); }
  else { img.removeAttribute("src"); d.classList.remove("has"); }
}

function showCtrl() {
  const d = F("ctrlDrop");
  if (S.ctrl) { F("ctrlImg").src = `/input/${encodeURIComponent(S.ctrl.name)}`; F("ctrlImg").title = "點一下移除"; d.classList.add("has"); }
  else { F("ctrlImg").removeAttribute("src"); d.classList.remove("has"); }
}

/** 塗鴉畫布畫好的草圖當構圖控制（play.js）：開構圖控制、類型、前處理、反轉照 o 設好 */
export function setCtrl(r, o = {}) {
  S.ctrl = r;
  F("ctrlOn").checked = true;
  if (o.type) F("ctrlType").value = o.type;
  if (o.preprocess) F("ctrlPre").value = o.preprocess;
  if (o.invert !== undefined) F("ctrlInvert").checked = !!o.invert;
  F("controlBox").open = true;
  if (S.mode !== "txt2img" && S.mode !== "img2img") setMode("txt2img");
  app.showPage("gen");
  showCtrl();
  save();
}

// ---------------------------------------------------------------- LoRA 疊加
export function addLora(l, triggers) {
  if (S.loras.some((x) => x.name === l.name)) return toast("這個 LoRA 已經在清單裡了");
  S.loras.push({ name: l.name, strength: l.strength ?? (OFFICIAL_TURBO.test(l.name) ? 1 : 0.8), enabled: true, title: l.title, min: l.min, max: l.max, slider: l.slider });
  if (triggers && l.triggers?.length) insertTriggers(l.triggers);
  renderLoras();
  save();
}

const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

// 強度對照：其他設定與種子都固定，只改這個 LoRA 的強度，各生成一張
async function sweep(l) {
  if (!app.state?.online) return toast("ComfyUI 沒有連線", "err");
  if (S.mode === "upscale") return toast("放大模式不能做強度對照", "err");
  if (mismatch(l)) return toast("這個 LoRA 跟目前的模型不同家族，生成時會被略過，沒辦法比較強度", "err");
  const lo = l.min ?? -2, hi = l.max ?? 3;
  // 範圍很大的滑桿（例如 ±20 的胸部滑桿）按比例取點，否則 -1～2 根本看不出差別
  const span = Math.max(Math.abs(lo), Math.abs(hi));
  const vals = (span > 4 ? [-span / 2, -span / 4, 0, span / 4, span / 2] : lo < 0 ? [-1, 0, 1, 2] : [0, 0.5, 1, 1.5, 2])
    .filter((v) => v >= lo && v <= hi);
  const p = collectParams();
  const sourceRevision = (p.init_image || p.refs?.length) ? viewsrc.inputRevision() : null;
  if (p.mode === "multiref" ? !p.refs?.length : S.mode !== "txt2img" && !p.init_image) return toast("請先放入圖片", "err");
  p.seed = p.seed ?? Math.floor(Math.random() * 2 ** 40);
  F("seed").value = p.seed;
  const others = S.loras.filter((x) => x.enabled && x.name !== l.name && !mismatch(x)).map((x) => ({ name: x.name, strength: x.strength }));
  try {
    for (const v of vals) {
      const r = await api("/api/generate", { json: { params: { ...p, loras: v ? [...others, { name: l.name, strength: v }] : others }, count: 1 } });
      for (const j of r.jobs) S.pending.set(j.job, { seed: j.seed, sourceRevision });
    }
    S.lastSeed = p.seed;
    renderStrip();
    save();
    toast(`已送出 ${vals.length} 張：「${l.title || l.name}」強度 ${vals.join("／")}，種子固定 ${p.seed}`, "ok");
  } catch (e) {
    toast("無法開始：" + e.message, "err");
  }
}

function renderLoras() {
  // 開關官方 Turbo LoRA 會換排程：步數、CFG 跟著改（第一次只記下狀態，不蓋掉存檔裡的設定）
  const ot = isOfficialTurbo();
  if (S.officialTurbo !== undefined && ot !== S.officialTurbo) { S.officialTurbo = ot; modelDefaults(); }
  S.officialTurbo = ot;
  F("mossFast").checked = fastOn();
  F("mossDeband").checked = debandOn();
  const box = F("loraStack");
  box.innerHTML = "";
  renderTrig();
  if (!S.loras.length) return box.append(el("div", { class: "muted sm" }, "沒有使用 LoRA"));
  const meta = Object.fromEntries((app.state?.models?.loras || []).map((x) => [x.name, x]));
  S.loras.forEach((l, i) => {
    const bad = mismatch(l);
    const note = bad
      ? (fam() === "custom"
        ? `這個 LoRA 是 ${famShort(loraFam(l))} 用的，工作流「${wfSet()?.name || ""}」是 ${famShort(wfFam())} 的，用不了，生成時會略過。`
        : `這個 LoRA 是 ${famLabel(loraFam(l))} 用的，目前的模型用不了，生成時會略過。`)
      : meta[l.name]?.note || "";
    // 兩行卡（固定舞台 A 案）：第一行勾選、全名（太長換行，下面一行小字是檔名）、✕；第二行拉桿、數字、⇆
    const nm = loras.loraNames(l.title, l.name);
    const row = el("div", { class: "lora-item two" + (l.enabled && !bad ? "" : " off") + (bad ? " mismatch" : "") },
      el("input", { type: "checkbox", checked: l.enabled, title: "暫時停用", onchange: (e) => { l.enabled = e.target.checked; renderLoras(); save(); } }),
      el("div", { class: "nmw", title: (note ? note + "\n\n" : "") + l.name }, el("b", { class: "nm" }, nm.name), nm.file ? el("small", { class: "fn" }, nm.file) : null),
      el("button", { class: "icon lx", title: "移除", onclick: () => { S.loras.splice(i, 1); renderLoras(); save(); } }, "✕"),
      el("input", { type: "range", min: l.min ?? -1, max: l.max ?? 2, step: 0.05, value: l.strength, title: "左右拖曳調整強度",
        oninput: (e) => { l.strength = +e.target.value; row.querySelector("input[type=number]").value = l.strength; save(); } }),
      el("input", { type: "number", min: l.min ?? -2, max: l.max ?? 3, step: 0.05, value: l.strength,
        onchange: (e) => { l.strength = clamp(+e.target.value, l.min ?? -2, l.max ?? 3); renderLoras(); save(); } }),
      el("button", { class: "icon", title: "強度對照：同一個種子，用幾種強度各畫一張", onclick: () => sweep(l) }, "⇆"),
      note ? el("div", { class: "lora-note" + (S.openNotes.has(l.name) ? " open" : ""), title: "點一下展開／收合",
        onclick: (e) => { S.openNotes.has(l.name) ? S.openNotes.delete(l.name) : S.openNotes.add(l.name); e.currentTarget.classList.toggle("open"); } }, note) : null);
    box.append(row);
  });
}

// ---------------------------------------------------------------- 提示詞工具
function fillSnippets() {
  const sel = F("snippetSel");
  sel.innerHTML = '<option value="">＋ 插入片段</option>';
  sel.append(...library.snippetGroups());
  for (const [g, items] of Object.entries(SNIPPETS)) {
    const o = el("optgroup", { label: g });
    items.forEach((t) => o.append(el("option", { value: t }, t)));
    sel.append(o);
  }
}

// 已加入（而且有啟用）的 LoRA 的觸發詞（反推的自動清理也用它判斷有沒有掛 LoRA；漫畫台「用生成頁目前的設定」也帶這個）
export function triggerWords() {
  const meta = Object.fromEntries((app.state?.models?.loras || []).map((x) => [x.name, x]));
  const words = [];
  const seen = new Set();
  for (const l of S.loras) if (l.enabled && !mismatch(l)) for (const t of meta[l.name]?.triggers || []) {
    // C 站的觸發詞常常一整串帶逗號（「a, b, c,」）或尾巴多一個逗號：整理成「a, b, c」，空的不要、重複的不要
    const w = splitTags(t).join(", ");
    const k = splitTags(w).map(normTag).join(",");
    if (w && !seen.has(k)) { seen.add(k); words.push(w); }
  }
  return words;
}

// ---- 觸發詞標籤（2026-11-08 重寫）
// 以前的毛病：觸發詞整串帶逗號（C 站常見「a, b, c,」）或尾巴多一個逗號時，整串拿去跟提示詞的單一 tag 比，永遠對不上
// → 標籤不亮、按一次加一次、長串按了像沒反應；而且「亮不亮」是畫標籤當下記住的，提示詞被程式改掉（套用圖庫參數、AI 改寫、
// 提示詞庫…）後標籤不會更新，按下去方向就反了。現在：
//   1. 提示詞和觸發詞都先拆成單一 tag（半形／全形逗號、換行都算分隔），比對時忽略大小寫、底線／空白、跳脫括號、(tag:1.2) 權重、((多層括號))
//   2. 按下去的當下重新比對，不用舊狀態
//   3. 提示詞的 value 被任何程式改掉也會重畫標籤（watchPromptValue）
//   4. 一整組的觸發詞拆成單顆標籤，可以單顆點，也可以按「整組」一次加／移除；只有一部分在提示詞裡時「整組」顯示外框
const TAG_SEP = /[,，\n]/;
function splitTags(s) {
  return String(s ?? "").split(TAG_SEP).map((x) => x.trim()).filter(Boolean);
}

function normTag(s) {
  let t = String(s ?? "").trim();
  for (let guard = 0; guard < 8; guard++) {
    const m = t.match(/^\((.*)\)$/s) || t.match(/^\[(.*)\]$/s);
    if (!m) break;
    t = m[1].replace(/:\s*-?\d*\.?\d+\s*$/, "").trim();
  }
  return t.replace(/\\/g, "").replace(/_/g, " ").replace(/\s+/g, " ").trim().toLowerCase();
}

const promptTagSet = () => new Set(splitTags(F("prompt").value).map(normTag));

// 把提示詞裡跟 targets（normTag 過的）一樣的 tag 拿掉，連同它後面的分隔符號；其他文字、換行原樣保留
function removeTags(text, targets) {
  const tok = String(text).split(/([,，\n])/);
  const kept = [];
  let dropped = false;
  for (let i = 0; i < tok.length; i += 2) {
    let t = tok[i];
    const sep = tok[i + 1] ?? "";
    if (t.trim() && targets.has(normTag(t))) { dropped = true; continue; }
    // 拿掉的是一行的第一顆：下一顆補上來當行首，前面的空白不要留
    if (dropped && (!kept.length || kept[kept.length - 1].endsWith("\n"))) t = t.replace(/^[ \t]+/, "");
    dropped = false;
    kept.push(t + sep);
  }
  return kept.join("").replace(/^[\s,，]+/, "").replace(/[\s,，]+$/, "").replace(/([,，])(\s*[,，])+/g, "$1");
}

function trigChip(w, have) {
  const on = have.has(normTag(w));
  return el("button", { class: "chip" + (on ? " on" : ""), type: "button", "aria-pressed": on ? "true" : "false",
    title: on ? "點一下從提示詞移除" : "點一下加到提示詞", onclick: () => toggleTrigger(w) }, w);
}

function renderTrig() {
  const words = triggerWords();
  F("trigChips").hidden = !words.length;
  const box = F("trigList");
  box.innerHTML = "";
  const have = promptTagSet();
  for (const w of words) {
    const parts = splitTags(w);
    if (parts.length < 2) { box.append(trigChip(w, have)); continue; }
    const n = parts.filter((p) => have.has(normTag(p))).length;
    const all = n === parts.length;
    box.append(el("span", { class: "trig-group", role: "group" },
      el("button", { class: "chip trig-all" + (all ? " on" : n ? " part" : ""), type: "button", "aria-pressed": all ? "true" : n ? "mixed" : "false",
        title: all ? "整組從提示詞移除" : n ? "缺的補上，整組都加進提示詞" : "整組加到提示詞", onclick: () => toggleTrigger(w) },
        "整組", el("small", {}, `${n}/${parts.length}`)),
      ...parts.map((p) => trigChip(p, have))));
  }
}

// 按下去的當下重新比對：全部都在＝整組移除，否則把缺的補上
function toggleTrigger(w) {
  const t = F("prompt");
  const parts = new Set(splitTags(w).map(normTag));
  if (!parts.size) return;
  const have = promptTagSet();
  if ([...parts].every((p) => have.has(p))) t.value = removeTags(t.value, parts);
  else insertTriggers([w]);
  save();
  renderTrig();
}

// 提示詞被程式改掉（不會觸發 input 事件）也要重畫觸發詞標籤；保留別人先包過的 value（跟 deck.js 的 watchValue 同招）
function watchPromptValue(ta, cb) {
  const desc = Object.getOwnPropertyDescriptor(ta, "value") || Object.getOwnPropertyDescriptor(Object.getPrototypeOf(ta), "value");
  Object.defineProperty(ta, "value", {
    configurable: true,
    get() { return desc.get.call(this); },
    set(v) { desc.set.call(this, v); cb(); },
  });
  ta.addEventListener("input", cb);
}

// 觸發詞插在開頭的品質／評級詞後面（masterpiece, best quality, safe, jgdxstyle, …）：放越前面 LoRA 越穩；沒有品質詞就放最前面
const QUALITY = /^(masterpiece|best quality|amazing quality|high quality|very aesthetic|aesthetic|absurdres|highres|newest|recent|score[_ ]\d+([_ ]up)?|year \d{4}|safe|sensitive|nsfw|explicit)$/i;
export function insertAfterQuality(cur, words) {
  const parts = cur.trim().replace(/,\s*$/, "").split(",");
  const have = new Set(splitTags(cur).map(normTag));
  const add = [];
  for (const w of words.flatMap(splitTags)) {
    const k = normTag(w);
    if (!have.has(k)) { have.add(k); add.push(w); }
  }
  if (!add.length) return cur;
  let i = 0;
  while (i < parts.length && QUALITY.test(normTag(parts[i]))) i++;
  const rest = parts.slice(i).join(",").trim();
  return [...parts.slice(0, i).map((s) => s.trim()), ...add, ...(rest ? [rest] : [])].join(", ");
}

function insertTriggers(words) {
  F("prompt").value = insertAfterQuality(F("prompt").value, words);
  save();
}

export function appendPrompt(text) {
  const t = F("prompt");
  const cur = t.value.trim().replace(/,\s*$/, "");
  const have = new Set(cur.split(",").map((s) => s.trim().toLowerCase()));
  const add = text.split(",").map((s) => s.trim()).filter((s) => s && !have.has(s.toLowerCase()));
  if (!add.length) return;
  t.value = (cur ? cur + ", " : "") + add.join(", ");
  save();
}

function applyTags(text, replace) {
  if (replace) F("prompt").value = text;
  else appendPrompt(text);
  save();
}

// ---------------------------------------------------------------- 收集／套用參數
export function collectParams() {
  const em = effectiveMode();
  const p = {
    mode: em, model: F("model").value, turbo_lora: (!isTurboModel() && F("turboLora").value) || false,
    prompt: promptsec.forModel(F("prompt").value).trim(), negative: F("negative").value.trim(),
    steps: +F("steps").value, cfg: +F("cfg").value, sampler: F("sampler").value, scheduler: F("scheduler").value,
    seed: F("seed").value.trim() === "" ? null : +F("seed").value,
    width: +F("width").value, height: +F("height").value,
    loras: S.loras.filter((l) => l.enabled && !mismatch(l)).map((l) => ({ name: l.name, strength: l.strength })),
  };
  if (fam() === "mmimage") {
    Object.assign(p, {mm_te: F("mmTe").value, mm_pe: F("mmPe").value, vae_tiled: +F("mmTiled").value, negative:""});
    p.mm_ratio = F("mmRatio").value;
    p.mm_mp = +F("mmMp").value;
    const { w, h } = mmSize(p.mm_ratio, p.mm_mp);
    p.width = w;
    p.height = h;
  }
  Object.assign(p, pe.paramsFor(p.prompt));  // AI 改寫：原始想法與改寫結果一起存進圖庫
  if (em === "multiref") {
    p.refs = refs.get().map((r) => ({ image: r.name, role: r.role, width: r.width, height: r.height }));
    p.size_mode = F("autoSize").checked ? "auto" : "fixed"; p.pixels = 1024 * 1024; p.ref_pixels = +F("refPixels").value;
  } else if (S.mode !== "txt2img") {
    p.init_image = S.init?.name || refs.get()[0]?.name;
  }
  if (em === "img2img") { p.denoise = num("denoise"); p.size_mode = F("autoSize").checked ? "auto" : "fixed"; p.pixels = fam() === "anima" ? 1024 * 1536 : 1024 * 1024; }
  if (S.mode === "inpaint") {
    p.denoise = num("denoise"); p.mask = S.mask?.name;
    p.inpaint = { method: F("ipMethod").value, area: F("ipArea").value, grow: +F("ipGrow").value, blur: +F("ipBlur").value, padding: +F("ipPad").value };
  }
  if (S.mode === "outpaint") {
    const v = opVals();
    p.outpaint = { top: v.opTop, bottom: v.opBottom, left: v.opLeft, right: v.opRight, feather: num("opFeather") };
    p.inpaint = { grow: 8, blur: 16 };
  }
  if (S.mode === "upscale") p.upscale = { upscaler: F("upModel").value, scale: +F("upScale").value, refine: F("upRefine").checked, denoise: num("upDenoise") };
  if (S.mode === "strip") p.strip = { target: F("stripTarget").value, keep: F("stripKeep").value, refine: F("stripRefine").checked,
    refine_denoise: num("stripDenoise"), merge: F("stripMerge").checked, extra: F("stripExtra").value.trim(), tags: F("stripTags").value.trim(),
    engine: F("stripEngine")?.value || "auto" };
  if (S.mode === "sheet") p.sheet = { req: F("sheetReq").value.trim(), ref: S.sheetRef?.name || null, engine: F("sheetEngine")?.value || "auto" };
  if (em === "txt2img" || em === "img2img") {
    p.hires = { enabled: F("hiresOn").checked, scale: num("hiresScale"), denoise: num("hiresDenoise"), upscaler: F("hiresUpscaler").value };
    p.control = { enabled: F("ctrlOn").checked, type: F("ctrlType").value, preprocess: F("ctrlPre").value, invert: F("ctrlInvert").checked,
      strength: num("ctrlStr"), start: 0, end: num("ctrlEnd"), image: S.ctrl?.name || (em === "img2img" ? p.init_image : null) };
    if (fam() === "moss") {
      if (em === "txt2img" && p.control.enabled) p.moss_control = { enabled: true, image: p.control.image, depth: F("ctrlMossDepth").checked,
        color: F("ctrlMossColor").checked, strength: p.control.strength, end: p.control.end, adapter: controlLora()?.name };
      delete p.control;
    }
  }
  // 姿勢參考圖（MOSS）：有色彩／深度控制轉接器就改用它（深度圖當控制、從空白畫），沒有照舊從深度圖重繪
  if (fam() === "moss" && p.pose_control && controlLora()) p.pose_control = { ...p.pose_control, adapter: controlLora().name };
  if (layOn() && !editOn()) { const boxes = lay.usable(); if (boxes.length) p.layout = boxes; }
  if (editOn()) p.moss_edit = "auto";
  if (fam() === "moss" && F("mossTe").value === "4b") p.moss_te = "4b";
  if (fam() === "custom") {
    // 工作流組：「工作流參數」面板改過的值（鍵是「節點編號.欄位」，後端最後套上）
    const ov = wfpanel.overrides(F("model").value.slice(3), em);
    if (ov) p.wf_overrides = ov;
  }
  return p;
}

/** 導演台「用生成頁目前的設定」畫每段第一格：模型、加速 LoRA、負面詞、步數、CFG、取樣器、LoRA */
export function baseParams() {
  const p = collectParams();
  return { model: p.model, turbo_lora: p.turbo_lora, negative: p.negative, steps: p.steps, cfg: p.cfg,
    sampler: p.sampler, scheduler: p.scheduler, loras: p.loras, family: fam() };
}

const FORM_IDS = ["mmRatio", "mmMp", "mmTiled", "mmTe", "mmPe", "prompt", "negative", "width", "height", "autoSize", "denoise", "ipMethod", "ipArea", "ipGrow", "ipBlur", "ipPad", "upModel", "upScale", "upRefine", "upDenoise",
  "hiresOn", "hiresScale", "hiresDenoise", "hiresUpscaler", "ctrlOn", "ctrlType", "ctrlPre", "ctrlStr", "ctrlEnd", "ctrlInvert", "ctrlMossDepth", "ctrlMossColor", "mossTe", "model", "steps", "cfg", "sampler", "scheduler", "turboLora", "seed", "count", "refPixels",
  "stripEngine", "stripTarget", "stripKeep", "stripRefine", "stripDenoise", "stripMerge", "stripExtra", "stripTags", "sheetEngine", "sheetReq",
  "opTop", "opBottom", "opLeft", "opRight", "opFeather", "autoDetail", "detailDenoise", "batchLines"];

// 網頁剛打開時，模型、取樣器這些下拉選單還沒有選項（要等 onState 讀到清單）。
// 這時套進去的值先記在這裡，存表單時照用；不然會存成空字串，重新整理後模型被換回預設、步數卻留著
const pendingSel = {};

function collectForm() {
  const f = { mode: S.mode, loras: S.loras, init: S.init, ctrl: S.ctrl, refs: refs.get(), sheetRef: S.sheetRef, layout: lay?.get() || [], locked: S.locked };
  for (const id of FORM_IDS) { const e = F(id); f[id] = e.type === "checkbox" ? e.checked : (id in pendingSel ? pendingSel[id] : e.value); }
  return f;
}

function applyForm(f) {
  for (const id of FORM_IDS) {
    if (!(id in f)) continue;
    const e = F(id);
    if (id === "turboLora") speedup.fillTurbo(e, app.state?.models?.turbo_loras, f[id]);
    else if (e.type === "checkbox") e.checked = !!f[id];
    else if (e.tagName === "SELECT" && f[id] !== "" && f[id] != null && ![...e.options].some((o) => o.value === String(f[id]))) pendingSel[id] = f[id];
    else { e.value = f[id]; delete pendingSel[id]; }
  }
  S.loras = (f.loras || []).map((l) => ({ ...l }));
  if (f.ctrl !== undefined) S.ctrl = f.ctrl;
  if (f.init !== undefined && S.init?.name !== f.init?.name) {
    S.init = f.init;
    S.mask = null;
    viewsrc.onInput();
  }
  if (f.refs !== undefined) refs.set(f.refs);
  if (f.sheetRef !== undefined) { S.sheetRef = f.sheetRef; showSheetRef(); }
  if (f.layout !== undefined) { lay.set(f.layout); layCount(); }
  if (f.locked !== undefined) { S.locked = Array.isArray(f.locked) ? f.locked : []; renderLocked(); }
  for (const o of $$("#page-gen output, #page-taglib output, #capLayer output")) { const inp = o.closest("div, section")?.querySelector("input[type=range]"); if (inp) o.textContent = inp.value; }
  F("upRefineRow").hidden = !F("upRefine").checked;
  F("stripRefineRow").hidden = !F("stripRefine").checked;
  F("ipPadRow").hidden = F("ipArea").value !== "masked";
  if (app.state) S.family = fam();
  renderLoras(); showCtrl(); modelHint();
  setMode(f.mode || "txt2img");
}

const save = () => { store.set("form", collectForm()); promptx.refresh(); prompttabs.refresh(); };

// 鎖定角色標籤列：換模型、動參考圖時滑過去看的全文跟著變
function renderLocked() {
  lk.render(F("lkRow"), S.locked, { fam: fam(), refNames: S.mode === "img2img" ? refs.get().map((r) => r.name) : [],
    onRemove: (id) => { S.locked = S.locked.filter((c) => c.id !== id); renderLocked(); save(); } });
}

function restore() {
  const d = {
    prompt: "masterpiece, best quality, safe, 1girl, solo, ", width: 1024, height: 1536, autoSize: true, denoise: 0.6,
    ipMethod: "lllite", ipArea: "full", ipGrow: 8, ipBlur: 12, ipPad: 64, upScale: "2", upDenoise: 0.3,
    hiresScale: 1.5, hiresDenoise: 0.4, ctrlType: "any", ctrlPre: "canny", ctrlStr: 1, ctrlEnd: 1, ctrlInvert: true, ctrlMossDepth: true, ctrlMossColor: true, mossTe: "9b",
    stripTarget: "female", stripKeep: "all", stripRefine: true, stripDenoise: 0.35, stripMerge: true, stripExtra: "", stripTags: "", sheetReq: "",
    opTop: 0, opBottom: 0, opLeft: 0, opRight: 0, opFeather: 40, autoDetail: false, detailDenoise: 0.4, batchLines: false,
    steps: 8, cfg: 1, count: "4", mode: "txt2img",
  };
  applyForm({ ...d, ...store.get("form", {}) });
}

// 從圖庫或圖片讀到的參數套回表單
// 舊圖、API 送的 turbo_lora: true（＝「用預設的加速 LoRA」）→ 這台電腦上實際有的那一顆：官方的優先，沒有就清單第一顆，都沒有就空白。
// 2026-11-10 以前寫死 v0.2 的檔名，這台沒有那顆檔時選單默默落回「不使用」
function defTurbo() {
  return speedup.defTurbo(app.state?.models?.turbo_loras);
}

export function applyParams(p, withMode = true) {
  if (!p) return toast("這張圖沒有可用的生成參數", "err");
  // 選單沒有這個值（別台電腦、別的軟體的取樣器）就維持目前的，不要變成空白
  const set = (id, v) => {
    const e = F(id);
    if (v === undefined || v === null || !e) return;
    if (e.tagName === "SELECT" && ![...e.options].some((o) => o.value === String(v))) return;
    e.value = v;
  };
  set("prompt", p.prompt); set("negative", p.negative); set("steps", p.steps); set("cfg", p.cfg);
  set("sampler", p.sampler); set("scheduler", p.scheduler); set("seed", p.seed);
  if (p.width && p.height && p.mode !== "upscale") { set("width", p.width); set("height", p.height); }
  const modelMissing = p.model && ![...F("model").options].some((o) => o.value === p.model);
  if (p.model && !modelMissing) F("model").value = p.model;
  // 工作流組做的圖：「工作流參數」面板改過的值也帶回來
  if (p.wf_overrides && (p.model || "").startsWith("wf:")) wfpanel.setOverrides(p.model.slice(3), p.mode || "txt2img", p.wf_overrides);
  speedup.fillTurbo(F("turboLora"), app.state?.models?.turbo_loras, p.turbo_lora === true ? defTurbo() : p.turbo_lora);
  S.family = fam();
  S.loras = (p.loras || []).map((l) => ({ ...l, enabled: true }));
  if (p.hires) { F("hiresOn").checked = !!p.hires.enabled; set("hiresScale", p.hires.scale); set("hiresDenoise", p.hires.denoise); }
  if (p.denoise != null) set("denoise", p.denoise);
  // 局部重繪、放大、多圖參考、構圖控制的細項也照那張圖還原（以前只還原重繪幅度：遮罩擴張／柔化／內距回到表單預設，重畫範圍跟原圖不一樣）
  if (p.inpaint && typeof p.inpaint === "object") {
    set("ipMethod", p.inpaint.method); set("ipArea", p.inpaint.area); set("ipGrow", p.inpaint.grow); set("ipBlur", p.inpaint.blur); set("ipPad", p.inpaint.padding);
    F("ipPadRow").hidden = F("ipArea").value !== "masked";
  }
  if (p.upscale && typeof p.upscale === "object") {
    set("upModel", p.upscale.upscaler); set("upScale", p.upscale.scale); set("upDenoise", p.upscale.denoise);
    if (p.upscale.refine != null) F("upRefine").checked = !!p.upscale.refine;
  }
  if (p.ref_pixels) set("refPixels", p.ref_pixels);
  if (p.control && !Array.isArray(p.control) && p.control.enabled) {
    set("ctrlType", p.control.type); set("ctrlPre", p.control.preprocess); set("ctrlStr", p.control.strength); set("ctrlEnd", p.control.end);
    if (p.control.invert != null) F("ctrlInvert").checked = !!p.control.invert;
  }
  if (p.strip) {
    set("stripEngine", p.strip.engine);
    set("stripTarget", p.strip.target); set("stripKeep", p.strip.keep); set("stripDenoise", p.strip.refine_denoise);
    set("stripExtra", p.strip.extra); set("stripTags", p.strip.tags);
    F("stripRefine").checked = p.strip.refine !== false; F("stripMerge").checked = p.strip.merge !== false;
    F("stripRefineRow").hidden = !F("stripRefine").checked;
  }
  if (p.sheet) { set("sheetEngine", p.sheet.engine); set("sheetReq", p.sheet.req); }
  if (p.outpaint) { set("opTop", p.outpaint.top); set("opBottom", p.outpaint.bottom); set("opLeft", p.outpaint.left); set("opRight", p.outpaint.right); set("opFeather", p.outpaint.feather); }
  lay.set(p.layout || []); layCount();  // 重用參數＝重現那張圖：沒有框的圖會把框清掉
  if (p.layout?.length) F("layBox").open = true;
  renderLoras(); modelHint();
  for (const o of $$("#page-gen output, #page-taglib output, #capLayer output")) { const inp = o.closest("div, section")?.querySelector("input[type=range]"); if (inp) o.textContent = inp.value; }
  if (p.mode === "multiref" && p.refs) refs.set(p.refs.map((r) => ({ name: r.image, width: r.width, height: r.height, role: r.role || "person" })));
  // 文生圖、ComfyUI 匯入的、沒標模式的 → 文生圖；其餘（圖生圖、局部重繪、放大、一鍵脫衣、四視圖、多圖參考）照原模式還原（輸入圖要自己再放一次）
  if (withMode) setMode(["img2img", "inpaint", "outpaint", "upscale", "strip", "sheet", "multiref"].includes(p.mode) ? p.mode : "txt2img");
  if (fam() === "mmimage") {
    set("mmTe", p.mm_te); set("mmPe", p.mm_pe); set("mmTiled", p.vae_tiled);
    set("mmRatio", p.mm_ratio); set("mmMp", p.mm_mp);
    if (p.size_mode) F("autoSize").checked = p.size_mode === "auto";
    mmDimensions();
  }
  pe.restore(p);
  save();
  if (modelMissing) toast(`已套用參數，但模型「${String(p.model).replace(/\.safetensors$/, "")}」不在這台電腦：維持目前的模型，步數和 CFG 是那顆模型的，可能不合`, "err");
  else toast("已套用參數（種子也一併帶入，要換構圖請按「隨機」）", "ok");
}

// ---------------------------------------------------------------- 提示詞庫的整套設定
// 存：目前表單（不含輸入圖、控制圖、種子）
function libSnapshot() {
  const f = collectForm();
  delete f.init; delete f.ctrl; delete f.seed;
  return JSON.parse(JSON.stringify(f));
}

// 套用：整套換掉；這台電腦沒有的模型、取樣器之類維持目前的，回傳提醒
function applyLibSet(data) {
  const f = { ...data }, warn = [];
  for (const id of FORM_IDS) {
    const e = F(id);
    if (!(id in f) || e.tagName !== "SELECT" || [...e.options].some((o) => o.value === String(f[id]))) continue;
    if (id === "model") warn.push(`模型「${f[id]}」不在這台電腦上，模型維持目前的`);
    else if (id === "turboLora" && f[id]) warn.push(`加速 LoRA「${f[id]}」不在這台電腦上，改成不使用`);
    if (id === "turboLora") f[id] = ""; else delete f[id];
  }
  const have = new Set((app.state?.models?.loras || []).map((l) => l.name));
  const miss = have.size ? (f.loras || []).filter((l) => !have.has(l.name)).map((l) => l.name.replace(/\.safetensors$/, "")) : [];
  if (miss.length) warn.push(`找不到這些 LoRA：${miss.join("、")}`);
  applyForm(f);
  save();
  library.fillPickers();
  return warn;
}

// 圖片的生成參數 → 表單格式（「存到提示詞庫」用，不會動到目前的表單）
function paramsToForm(p) {
  const keepMode = ["multiref", "img2img", "inpaint", "outpaint", "upscale", "strip", "sheet"].includes(p.mode);
  const f = { mode: keepMode ? p.mode : "txt2img", loras: (p.loras || []).map((l) => ({ ...l, enabled: true })) };
  const put = (k, v) => { if (v !== undefined && v !== null) f[k] = v; };
  put("mmTe", p.mm_te); put("mmPe", p.mm_pe); put("mmTiled", p.vae_tiled);
  put("mmRatio", p.mm_ratio); put("mmMp", p.mm_mp);
  put("prompt", p.prompt); put("negative", p.negative); put("steps", p.steps); put("cfg", p.cfg);
  put("sampler", p.sampler); put("scheduler", p.scheduler); put("model", p.model);
  if (p.width && p.height && p.mode !== "upscale") { f.width = p.width; f.height = p.height; }
  f.turboLora = p.turbo_lora === true ? defTurbo() : (p.turbo_lora || "");
  if (p.hires) { f.hiresOn = !!p.hires.enabled; put("hiresScale", p.hires.scale); put("hiresDenoise", p.hires.denoise); }
  if (p.mode === "img2img" && p.denoise != null) f.denoise = p.denoise;
  if (p.strip) {
    put("stripTarget", p.strip.target); put("stripKeep", p.strip.keep); put("stripDenoise", p.strip.refine_denoise);
    put("stripExtra", p.strip.extra); put("stripTags", p.strip.tags);
    f.stripRefine = p.strip.refine !== false; f.stripMerge = p.strip.merge !== false;
  }
  if (p.sheet) put("sheetReq", p.sheet.req);
  if (p.outpaint) Object.assign(f, { opTop: p.outpaint.top, opBottom: p.outpaint.bottom, opLeft: p.outpaint.left, opRight: p.outpaint.right, opFeather: p.outpaint.feather });
  if (p.layout?.length) f.layout = p.layout;
  if (p.mode === "multiref" && p.refs) f.refs = p.refs.map((r) => ({ name: r.image, width: r.width, height: r.height, role: r.role || "person" }));
  return f;
}

// ---------------------------------------------------------------- 生成
let localTraining = null;
let trainingRevision = 0;
let initialTraining = null;
export function confirmGenWhileTraining(state) {
  if (state?.target !== "local" || !["starting","waiting","running"].includes(state.state)) return "";
  return "本機正在訓練 LoRA，出圖可能很慢或讓訓練失敗。仍要出圖嗎？";
}

function paintTrainingBanner() {
  const banner = F("genTrainingNotice");
  if (!banner) return;
  banner.hidden = !confirmGenWhileTraining(localTraining);
  if (!banner.hidden) banner.textContent = localTraining.total ? `本機正在煉 LoRA（第 ${localTraining.step || 0}/${localTraining.total} 步）` :
    localTraining.state === "waiting" ? "本機 LoRA 訓練正在等 ComfyUI 畫完" : "本機正在煉 LoRA（準備中）";
}

async function refreshTraining() {
  const revision = ++trainingRevision;
  try {
    const r = await api("/api/train");
    if (revision !== trainingRevision) return;
    localTraining = r.projects.find(p => p.target === "local" && p.running === "train") || null;
    paintTrainingBanner();
  } catch { /* 下次連線或訓練事件再更新。 */ }
}

export function onTraining(m) {
  if (!m.id || ["caption","install"].includes(m.phase)) return;
  if (["done","stopped","error"].includes(m.state)) {
    if (!localTraining || m.id === localTraining.id) {
      trainingRevision++;
      localTraining = null;
      paintTrainingBanner();
    }
    return;
  }
  if (m.target !== "local" && m.id !== localTraining?.id) return;
  trainingRevision++;
  localTraining = { ...(localTraining?.id === m.id ? localTraining : {}), ...m, target:"local" };
  paintTrainingBanner();
}

async function generate() {
  await initialTraining;
  const warning = confirmGenWhileTraining(localTraining);
  if (warning && !confirm(warning)) return;
  refreshGenNode();  // 排隊數隨時在變：按生成前先更新一次（不用等）
  if (F("genBtn").disabled) return;  // 連按兩下：第一下還在等狀態、等 AI 改寫
  if (!app.state) {  // 剛打開網頁、狀態還沒讀到
    F("genBtn").disabled = true;
    try { await app.refreshState(); } finally { F("genBtn").disabled = false; }
  }
  if (!app.state?.online) return toast("ComfyUI 沒有連線：點狀態列的系統狀態，按「重開 ComfyUI」", "err");
  if (F("genBtn").disabled) return;
  F("genBtn").disabled = true;
  let ok;
  // 一鍵脫衣、四視圖不用提示詞框，也就不跑 AI 改寫；「每行一句」（批次）也不跑（改寫會把整框換成一段）
  const batch = F("batchLines").checked && !F("promptBox").hidden;
  try {
    // 隨機頁的隨機產生器勾了「每次生成前重抽」：先抽一組套進提示詞，再照常跑 AI 改寫（只在隨機頁按生成時）
    if (app.page === "taglib" && S.mode !== "strip" && S.mode !== "sheet" && !batch) await wardrobe.beforeGenerate();
    ok = S.mode === "strip" || S.mode === "sheet" || batch || await pe.beforeGenerate();
  } finally { F("genBtn").disabled = false; }
  if (!ok) return;
  const p = collectParams();
  const sourceRevision = (p.init_image || p.refs?.length) ? viewsrc.inputRevision() : null;
  if (fam() === "custom") {
    const w = wfSet();
    if (!w) return toast("找不到這組工作流（可能在設定頁被刪掉了）：請在模型選單重新選一個", "err");
    if (!w.modes.includes(p.mode))
      return toast(`「${w.name}」沒有${modeName[p.mode] || p.mode}的工作流：換一個模式，或到「設定 → 模型與工作流」補匯入一個`, "err");
  }
  if (S.mode === "outpaint" && !Object.values(opVals()).some((v) => v > 0)) return toast("擴圖：至少填一邊要往外擴幾像素（或按下面的快捷鈕）", "err");
  const refNames = () => (S.mode === "img2img" ? refs.get().map((r) => r.name) : []);
  // 一鍵脫衣、四視圖沒有提示詞框，不接
  const full = (text) => (F("promptBox").hidden ? text : lk.withLocked(text, S.locked, fam(), refNames()));
  if (S.mode === "img2img") {
    if (fam() === "custom") {
      if (!refs.get().length && !p.init_image) return toast("請先加入至少一張圖片", "err");
    } else if (["qwen21", "mmimage"].includes(fam())) {
      const bad = refs.check(full(p.prompt));
      if (bad) return toast(bad, "err");
    } else if (!p.init_image) {
      return toast("請先放入一張圖片", "err");
    }
  } else if (S.mode !== "txt2img" && !p.init_image) return toast("請先放入一張圖片", "err");
  if (S.mode === "inpaint" && !p.mask) return toast("請先按「畫遮罩」塗出要重繪的區域", "err");
  if (p.control?.enabled && !p.control.image) return toast("構圖控制需要一張控制圖", "err");
  if (fam() === "moss" && S.mode === "txt2img" && F("ctrlOn").checked) {
    if (!S.ctrl?.name) return toast("構圖控制需要一張參考圖：拖進控制圖那一格", "err");
    if (!F("ctrlMossDepth").checked && !F("ctrlMossColor").checked) return toast("深度、色塊至少勾一個", "err");
    if (!controlLora()) return fetchListed(MOSS_CONTROL.rel, " MOSS 色彩／深度控制轉接器", "300 MB", () => toast("下載好了，再按一次生成", "ok"));
  }
  if (fam() === "moss" && F("mossTe").value === "4b" && !(await have4b())) return;
  if (S.mode === "sheet") {
    if (!p.sheet.req) return toast("請先寫「修改要求」，或從常用要求挑一個", "err");
    if (/[图圖]\s*2|<Picture\s*2>|<image2>/i.test(p.sheet.req) && !p.sheet.ref) return toast("要求裡提到圖2，請先放服裝參考圖", "err");
  }
  // 檢查改圖引擎對應的模型是否在本機
  if (S.mode === "strip" || S.mode === "sheet") {
    const eng = (S.mode === "strip" ? p.strip?.engine : p.sheet?.engine) || "auto";
    const fams = app.state?.models?.unet_family || {};
    const hasQwenTurbo = Object.keys(fams).some((k) => k.split(/[\\/]/).pop() === "qwen_image_2.1_turbo_int8_convrot.safetensors") || fam() === "qwen21";
    const hasMm = Object.values(fams).includes("mmimage");
    const wantMm = eng === "mmimage" || (eng === "auto" && fam() === "mmimage");
    if (wantMm && !hasMm) return toast(modeName[S.mode] + "選了 MiniMax H3 Image，但模型清單裡找不到 MiniMax 模型", "err");
    if (!wantMm && !hasQwenTurbo) return toast(modeName[S.mode] + "選了 Qwen 2.1（qwen_image_2.1_turbo_int8_convrot），模型清單裡找不到（可改選 MiniMax H3 Image）", "err");
  }
  const count = S.mode === "upscale" ? 1 : +F("count").value;
  // 批次：每一行各送一次（AI 改寫的紀錄只對整框有意義，拿掉）
  const lines = batch ? p.prompt.split("\n").map((s) => s.trim()).filter(Boolean) : [p.prompt];
  if (batch && !lines.length) return toast("提示詞是空的", "err");
  if (lines.length > 50) return toast(`一次最多 50 行（現在 ${lines.length} 行）`, "err");
  const btn = F("genBtn");
  btn.disabled = true;
  let sent = 0;
  try {
    for (const line of lines) {
      const q = batch ? { ...p, prompt: full(line), idea: undefined, pe_prompt: undefined } : { ...p, prompt: full(p.prompt) };
      // 「送到」下拉在看的節點（顯示中才有值）：沒給＝後端自動分配
      const nodeWrap = F("genNodeWrap");
      if (nodeWrap && !nodeWrap.hidden && F("genNode").value) q.node = F("genNode").value;
      const r = await api("/api/generate", { json: { params: q, count } });
      for (const j of r.jobs) S.pending.set(j.job, { seed: j.seed, sourceRevision });
      S.lastSeed = r.jobs[0]?.seed;
      sent++;
      renderStrip();
      if (r.jobs[0]?.skipped_loras?.length && sent === 1) skippedToast(r.jobs[0].skipped_loras);
      // 開了卻不會生效的設定（後端 ignored_notes）：照樣會畫，但要講
      if (r.jobs[0]?.notes?.length && sent === 1) toast(r.jobs[0].notes.join("\n"), "err");
    }
    promptx.remember(p.prompt, p.model);
    if (batch) toast(`批次：${lines.length} 句 × ${count} 張，已排進佇列`, "ok");
  } catch (e) {
    toast((sent ? `批次送到第 ${sent + 1} 句失敗（前面 ${sent} 句已排進佇列）：` : "無法開始生成：") + e.message, "err");
  } finally {
    btn.disabled = false;
  }
}

// 後端因為模型家族不合而略過的 LoRA（生成頁的清單已經先擋掉大部分，這裡是兩邊判斷不一樣的時候）
function skippedToast(names) {
  const list = names.map((n) => n.replace(/\.(safetensors|pt|ckpt)$/i, "")).join("、");
  const w = fam() === "custom" ? wfSet() : null;
  const target = w ? `工作流「${w.name}」${wfFam() ? `（${famShort(wfFam())}）` : ""}` : "目前的模型";
  toast(`這些 LoRA 的模型家族跟${target}不合，這次沒有用上：${list}`, "err");
}

export function onStart(job) {
  // 自己的任務正在跑時，別頁（助手、導演台、漫畫台，或另一台工作節點）的任務開始不要蓋掉它：
  // 蓋掉的話自己那張畫完時對不上，「生成中」收不掉。自己的任務可能比送出的回應早一步開始，所以沒在跑自己的就照記
  if (!S.pending.has(job) && S.pending.has(S.running)) return;
  S.running = job;
  if (S.pending.has(job)) { F("liveBadge").hidden = false; F("liveStep").textContent = ""; }
}

export function onProgress(m) {
  if (S.pending.has(m.job)) F("liveStep").textContent = `${m.value}/${m.max}`;
}

export function onPreview(blob) {
  if (!S.pending.has(S.running)) return;
  if (!F("viewSrc").hidden && (S.pending.get(S.running).sourceRevision !== viewsrc.inputRevision() || !F("maskModal").hidden || !F("initDrop").hidden)) return;
  if (S.previewUrl) URL.revokeObjectURL(S.previewUrl);
  S.previewUrl = URL.createObjectURL(blob);
  const img = F("viewImg");
  img.src = S.previewUrl;
  img.classList.add("on");
  F("viewEmpty").hidden = true;
}

export function onImage(job, rec) {
  if (!S.pending.has(job)) return;  // 只收從生成頁送出的圖；助手、導演台、漫畫台的圖不要混進幻燈片
  S.results.unshift(rec);
  if (S.results.length > 200) S.results.pop();
  viewsrc.onResult(rec, S.pending.get(job).sourceRevision);
  renderStrip();
}

/** ws 斷線重連後跟後端的任務清單對帳：斷線那段時間跑完的任務，finish／image 事件收不到，
 *  「等待中」的格子和「生成中」標記會永遠卡著（2026-11-10 以前只能按「全停」，那會真的清掉佇列） */
export async function reconcile() {
  initialTraining = refreshTraining();
  if (!S.pending.size) return;
  let jobs;
  try { jobs = (await api("/api/jobs?limit=300")).jobs; } catch { return; }
  const by = new Map(jobs.map((j) => [j.id, j]));
  let gone = 0;
  for (const id of [...S.pending.keys()]) {
    const j = by.get(id);
    if (j && (j.status === "queued" || j.status === "running")) continue;
    S.pending.delete(id);
    gone++;
    if (id === S.running) S.running = null;
  }
  if (!gone) return;
  if (!S.running) F("liveBadge").hidden = true;
  renderStrip();
  toast("連線中斷的那段時間有任務結束了：畫好的圖在圖庫", "ok");
}

export function onFinish(m) {
  if (!S.pending.has(m.job)) return;
  S.pending.delete(m.job);
  if (m.job === S.running) { S.running = null; F("liveBadge").hidden = true; }
  if (m.status === "error") toast("生成失敗：" + (m.error || "沒有收到原因：點狀態列的系統狀態看看，或看資料夾裡的 data\\logs\\comfyui.log 最後幾行"), "err");
  renderStrip();
}

function enableActions(enabled) {
  for (const button of $$("#viewActions button")) button.disabled = !enabled;
  const download = $("#viewActions [data-act=download]");
  download.setAttribute("aria-disabled", String(!enabled));
  if (enabled && S.current) download.href = `/img/${S.current.id}`;
  if (!enabled) {
    download.removeAttribute("href");
    F("viewMoreMenu").hidden = true;
    F("viewMore").setAttribute("aria-expanded", "false");
  }
}

function paintFavorite(rec) {
  const button = $("#viewActions [data-act=fav]");
  const label = rec.fav ? "★ 已收藏" : "☆ 收藏";
  button.querySelector(".va-name").textContent = label;
  button.title = label;
  button.setAttribute("aria-label", label);
  button.setAttribute("aria-pressed", String(!!rec.fav));
}

function show(rec) {
  S.current = rec;
  const img = F("viewImg"), vid = F("viewVid"), isVid = isVideo(rec);
  img.classList.toggle("on", !isVid);
  vid.hidden = !isVid;
  if (isVid) { img.removeAttribute("src"); vid.src = `/img/${rec.id}`; vid.play().catch(() => {}); }
  else { vid.pause(); vid.removeAttribute("src"); img.src = `/img/${rec.id}`; }
  img.classList.toggle("alpha", rec.mode === "rembg");  // 去背的透明圖墊棋盤格（play.css）
  for (const a of IMAGE_ONLY_ACTS) $$(`#viewActions [data-act="${a}"]`).forEach((b) => (b.hidden = isVid || (a === "strip" && !stripOn())));
  for (const a of VIDEO_ONLY_ACTS) $$(`#viewActions [data-act="${a}"]`).forEach((b) => (b.hidden = !isVid));
  F("viewEmpty").hidden = true;
  const ls = (rec.params?.loras || []).map((x) => `${(x.name || "").replace(/\.safetensors$/, "")} ${x.strength}`);
  F("viewMeta").textContent = `${modeName[rec.mode] || rec.mode}・${rec.width}×${rec.height}・種子 ${rec.seed ?? "-"}` + (ls.length ? `・LoRA：${ls.join("、")}` : "・未用 LoRA") + (rec.gen_seconds != null ? `・花了 ${fmtSecs(rec.gen_seconds)}` : "");
  deck.showImage(rec);
  const dl = $("#viewActions [data-act=download]");
  dl.href = `/img/${rec.id}`;
  dl.download = rec.filename;
  paintFavorite(rec);
  $$("#strip img").forEach((i) => i.classList.toggle("sel", +i.dataset.id === rec.id));
}

export function renderStrip() {
  const s = F("strip");
  s.innerHTML = "";
  for (let i = 0; i < S.pending.size; i++) s.append(el("div", { class: "pending" }, "等待中"));
  for (const r of S.results) {
    const cls = [S.current?.id === r.id && "sel", !stripSeen.has(r.id) && "new"].filter(Boolean).join(" ");
    s.append(el("img", { src: `/thumb/${r.id}`, "data-id": r.id, class: cls, onclick: () => viewsrc.select(r) }));
    stripSeen.add(r.id);
  }
}
// 幻燈片匣已經出現過的圖（只有新的一張播掉進來的動畫）
const stripSeen = new Set();

// ---------------------------------------------------------------- 圖片動作（預覽區與圖庫共用）
// r＝{name, width, height}（ComfyUI input 裡的檔名）：切到生成頁、換模式、放進輸入圖；局部重繪直接打開遮罩畫布
export function useInput(act, r) {
  app.showPage("gen");
  setMode(act);
  setInit(r, act === "img2img");
  if (act === "inpaint") mask.open(S.init, (m) => { S.mask = m; showInit(); });
}

export async function imageAction(act, rec, btn, quiet = false) {
  try {
    if (act === "reuse") {
      const full = await api(`/api/image/${rec.id}`);
      app.showPage("gen");
      applyParams(full.params);
    } else if (act === "img2img" || act === "inpaint" || act === "outpaint" || act === "upscale" || act === "strip" || act === "sheet") {
      useInput(act, await api("/api/to_input", { json: { id: rec.id } }));
    } else if (act === "charsheet") {
      (await import("./charsheet.js")).open({ gallery: rec });
    } else if (act === "play") {
      play.menu(rec, btn);  // 擴圖、變體、修臉修手、差分、去背（play.js）
    } else if (act === "studio") {
      // 圖或影片放進剪輯台的素材區（剪輯台沒開會自動開）
      if (btn) btn.disabled = true;
      try { await api("/api/studio/import", { json: { id: rec.id } }); } finally { if (btn) btn.disabled = false; }
      toast("已放進剪輯台的素材區（導演 → 剪輯台，左上 Project）", "ok");
    } else if (act === "asset") {
      (await import("./assets.js")).saveFromGallery(rec);
    } else if (act === "lorapreview") {
      (await import("./loras.js")).previewFromImage(rec);
    } else if (act === "frames") {
      const studio = await import("./studio.js");
      studio.openFrames(rec);
    } else if (act === "multiref") {
      const r = await api("/api/to_input", { json: { id: rec.id } });
      if (S.mode !== "img2img") setMode("img2img");
      const n = refs.add(r);
      if (!quiet && n != null) { app.showPage("gen"); toast(`已加進參考圖（第 ${n} 張）`, "ok"); }
    } else if (act === "video") {
      openVideo(rec);
    } else if (act === "lib") {
      const full = await api(`/api/image/${rec.id}`);
      if (!full.params || !Object.keys(full.params).length) return toast("這張圖沒有生成參數，沒辦法存成整套設定", "err");
      library.saveDialog({ kind: "set", data: paramsToForm(full.params), imageId: rec.id });
    } else if (act === "tagger") {
      tagger.open({ id: rec.id }, (text, replace) => { app.showPage("gen"); applyTags(text, replace); }, fam(), triggerWords);
    } else if (act === "fav") {
      rec.fav = rec.fav ? 0 : 1;
      await api(`/api/image/${rec.id}`, { json: { fav: !!rec.fav } });
      if (btn && !btn.closest("#viewActions")) btn.textContent = rec.fav ? "★ 已收藏" : "☆ 收藏";
      if (S.current?.id === rec.id) paintFavorite(rec);
    } else if (act === "delete") {
      if (!confirm("把這張圖移到回收區？（圖庫的「回收區」可以還原）")) return false;
      await api(`/api/image/${rec.id}`, { method: "DELETE" });
      forget([rec.id]);
      toast("已移到回收區", "ok");
      return "deleted";
    }
  } catch (e) {
    toast(e.message, "err");
  }
}

// ---------------------------------------------------------------- 讓圖動起來（內建 MiniMax H3，或設定裡匯入的 ComfyUI 影片工作流）
// 2026-10-30 LTX-2.3 退場：內建影片就是 H3（server\api_h3.rs 的 from_gen／submit_from_gen）
/** 內建影片模型的名字（寫在網頁這邊才會進英文字典） */
export const H3_NAME = "MiniMax H3（內建）";
/** 現在用的影片模型：{id, name, builtin, info, error}（info＝自訂工作流看懂的摘要，見 video_wf.rs） */
export async function videoEngine() {
  return (await videoEngines()).cur;
}
/** 全部的影片模型：{cur＝設定裡選的, builtin＝內建 H3, flows＝匯入的影片工作流（每筆跟 cur 同格式）} */
async function videoEngines() {
  const r = await api("/api/video/workflows");
  const builtin = { id: r.builtin.id, name: H3_NAME, builtin: true, info: null, error: "" };
  const flow = (w) => ({ id: w.id, name: w.name || w.id, builtin: false, info: w.info || null, pins: w.pins || [], media_slots: w.media_slots || w.info?.media_slots || [], error: w.error || "" });
  const flows = r.workflows.map(flow);
  const cur = r.engine === r.builtin.id ? builtin : flows.find((x) => x.id === r.engine) || { ...flow({ id: r.engine }), error: "找不到這個工作流" };
  return { cur, builtin, flows };
}

// vidSet＝設定裡選的影片模型；vidEng＝這次要用的（下拉可以臨時換成別的匯入工作流或內建 H3，不動設定）
let vidRec = null, vidEng = null, vidSet = null, vidAll = null, vidH3 = [];
let vidMedia = null, vidMediaEpoch = 0;
function paintVidMedia() {
  const epoch = ++vidMediaEpoch;
  F("vidMediaSlots")?.remove();
  vidMedia = null;
  if (!vidEng || vidEng.builtin || vidH3Model()) return;
  const all = store.get("vidMediaSlots", {}), id = vidEng.id;
  vidMedia = mediaSlotsControl({ engine: vidEng, values: all[id] || {},
    isCurrent: () => epoch === vidMediaEpoch && vidEng?.id === id && !vidH3Model(),
    onchange: (values) => { if (epoch !== vidMediaEpoch) return; const saved = store.get("vidMediaSlots", {}); saved[id] = values; store.set("vidMediaSlots", saved); } });
  vidMedia.element.id = "vidMediaSlots";
  F("vidParams").after(vidMedia.element);
}
/** 「動起來」指定了 H3 本機的哪顆模型：回傳模型檔名（空字串＝內建的自動挑，或設定裡的匯入工作流） */
const vidH3Model = () => (F("vidModel").value.startsWith("h3:") ? F("vidModel").value.slice(3) : "");
/** 這次走 H3（內建、或下拉指定了 H3 模型） */
const vidIsH3 = () => !!vidEng?.builtin || !!vidH3Model();

// 影片模型下拉：設定裡的（H3 自動挑或自訂工作流）＋其他匯入的影片工作流＋本機裝了的 H3 模型（GGUF 量化版排前面）。
// 2026-11-10：以前只列設定裡那一個和 H3，匯入了別的影片工作流（Wan、LTX…）在這裡看不到，要先去設定頁換才用得到
function fillVidModel() {
  const sel = F("vidModel"), keep = store.get("vidModel", "");
  const h3 = [...vidH3].sort((a, b) => b.toLowerCase().endsWith(".gguf") - a.toLowerCase().endsWith(".gguf"));
  const others = (vidAll?.flows || []).filter((w) => w.id !== vidSet?.id);
  sel.replaceChildren(el("option", { value: "" }, vidSet ? vidSet.name : H3_NAME),
    ...(vidSet && !vidSet.builtin ? [el("option", { value: "eng:h3" }, H3_NAME)] : []),
    ...others.map((w) => el("option", { value: "wf:" + w.id }, `工作流：${w.name}`)),
    ...h3.map((m) => el("option", { value: "h3:" + m }, m.toLowerCase().endsWith(".gguf") ? `MiniMax H3 本機量化版（${m}）` : `MiniMax H3 本機（${m}）`)));
  sel.value = [...sel.options].some((o) => o.value === keep) ? keep : "";
  pickVidEng();
  paintVidModel();
}

/** 照下拉決定這次用哪個影片模型（wf:＝別的匯入工作流、eng:h3＝內建 H3、其他＝設定裡的） */
function pickVidEng() {
  const v = F("vidModel").value;
  vidEng = v === "eng:h3" ? vidAll?.builtin || vidSet
    : v.startsWith("wf:") ? vidAll?.flows.find((w) => w.id === v.slice(3)) || vidSet
      : vidSet;
}

// 自訂工作流釘到「動起來」的欄位（設定 → 影片模型 → 對應與參數…）：每次可以調，記在這個瀏覽器（每個工作流一份）
const pinStore = () => store.get("vidPins", {});
function paintPins() {
  const box = F("vidParams"), pins = !vidH3Model() && vidEng?.pins?.length ? vidEng.pins : [];
  box.hidden = !pins.length;
  const mine = pinStore()[vidEng?.id] || {};
  box.replaceChildren(...pins.map((p) => {
    const v = p.key in mine ? mine[p.key] : p.value;
    const put = (x) => {
      const all = pinStore(), o = { ...(all[vidEng.id] || {}) };
      if (x === p.value) delete o[p.key]; else o[p.key] = x;
      all[vidEng.id] = o;
      store.set("vidPins", all);
    };
    let c;
    if (typeof p.value === "boolean") { c = el("input", { type: "checkbox", checked: !!v }); c.onchange = () => put(c.checked); }
    else if (typeof p.value === "number") {
      c = el("input", { type: "number", step: "any", value: v });
      c.onchange = () => { const n = c.value === "" ? p.value : +c.value; c.value = n; put(Number.isInteger(p.value) ? Math.round(n) : n); };
    } else { c = el("input", { type: "text", spellcheck: "false", value: v ?? "" }); c.onchange = () => put(c.value); }
    return el("div", { class: "wf-f" }, el("span", { class: "sub", title: `${p.title} #${p.node} · ${p.input}`, translate: "no" }, p.label), c);
  }));
}

/** 「動起來」這次調過的欄位（跟工作流設定的值不一樣的才送） */
function pinOverrides() {
  if (vidH3Model() || !vidEng?.pins?.length) return undefined;
  const mine = pinStore()[vidEng.id] || {}, keys = new Set(vidEng.pins.map((p) => p.key));
  const o = Object.fromEntries(Object.entries(mine).filter(([k]) => keys.has(k)));
  return Object.keys(o).length ? o : undefined;
}

// H3 可以選本機或雲端（記在這個瀏覽器，預設本機）；雲端走 H3 影片台同一條路（/api/h3/generate），影片做好進圖庫
const vidCloud = () => vidIsH3() && F("vidWhere").value === "cloud";
F("vidWhere").value = store.get("h3Where", "local") === "cloud" ? "cloud" : "local";
F("vidWhere").onchange = () => { store.set("h3Where", F("vidWhere").value); paintVidModel(); };

// 下拉指定了 H3 模型：說明換成那顆的
function paintVidModel() {
  paintPins();
  paintVidMedia();
  F("vidWhereBox").hidden = !vidIsH3();
  // 只有一個選項就不用顯示；選了雲端也照樣留著（不然換不回匯入的工作流），只是本機的 H3 模型檔雲端用不到（送出時不帶）
  F("vidModelBox").hidden = F("vidModel").options.length < 2;
  if (vidCloud()) {
    F("vidResBox").hidden = F("vidSecsBox").hidden = false;
    F("vidEngine").textContent = "MiniMax H3 雲端：要先在「導演 → H3 影片」開好雲端（勾 H3 影片包），做好的影片會進圖庫";
    return;
  }
  const m = vidH3Model();
  if (!m) return paintVidEngine();
  F("vidResBox").hidden = F("vidSecsBox").hidden = false;
  F("vidEngine").textContent = m.toLowerCase().endsWith(".gguf")
    ? "MiniMax H3 本機量化版：有聲音。8GB 顯卡很慢（實測 480p 3 秒約 15 分鐘），720p、長一點的可能要一兩個小時或記憶體不夠"
    : /w4a8/i.test(m)
      ? "MiniMax H3 本機 w4a8：有聲音，8GB 顯卡跑得動（實測 512×768 5 秒約 3～4 分鐘）"
      : "MiniMax H3 本機：有聲音。這個模型很大，顯卡小的話選 w4a8 版";
}

async function openVideo(rec) {
  vidRec = rec;
  vidMediaEpoch++; vidMedia = null; F("vidMediaSlots")?.remove();
  F("vidSrc").src = `/img/${rec.id}`;
  openModal("#vidModal");
  F("vidPrompt").focus();
  vidEng = vidSet = vidAll = null;
  F("vidEngine").textContent = "";
  try { vidAll = await videoEngines(); vidEng = vidSet = vidAll.cur; } catch (e) { F("vidEngine").textContent = "讀不到影片模型的設定：" + e.message; return; }
  paintVidEngine();
  // 本機有 H3 模型就列出來可以指定（讀不到就只有設定裡的）
  try { vidH3 = ((await api("/api/h3/info")).local?.models) || []; } catch { vidH3 = []; }
  fillVidModel();
}

// 設定裡的影片模型（內建 H3 或自訂工作流）的說明和選項
function paintVidEngine() {
  if (!vidEng) return;
  // 自訂工作流：解析度照工作流；讀得到每秒幾格才能選秒數
  const custom = !vidEng.builtin, i = vidEng.info;
  const notes = [];
  if (vidEng.error) notes.push("這個工作流讀不懂：" + vidEng.error);
  else if (custom) {
    if (!i.image) notes.push("這個工作流沒有 Load Image，不能從圖生影片");
    notes.push(i.seconds ? "解析度照工作流" : "長度、解析度照工作流");
  } else notes.push("本機自動挑最省顯存的 H3 模型（8GB 顯卡用 w4a8）");
  F("vidEngine").textContent = `影片模型：${vidEng.name}${notes.length ? "・" + notes.join("・") : ""}・到「設定」可以換`;
  F("vidResBox").hidden = custom;
  F("vidSecsBox").hidden = custom && !i?.seconds;
}
F("vidModel").onchange = () => { store.set("vidModel", F("vidModel").value); pickVidEng(); paintVidModel(); };
F("vidGo").onclick = async () => {
  const prompt = F("vidPrompt").value.trim();
  if (!prompt) return toast("請寫動作和聲音的描述", "err");
  const b = F("vidGo");
  b.disabled = true;
  try {
    const media = vidMedia?.read() || {};
    const r = await api("/api/to_input", { json: { id: vidRec.id } });
    // 雲端跑的是雲端機器上的模型：本機選的 H3 模型檔不帶過去（雲端沒有那個檔會報「不在清單裡」）
    const h3 = vidIsH3(), m = vidCloud() ? "" : vidH3Model(), ov = pinOverrides();
    const params = {
      mode: "video", init_image: r.name, prompt, seed: -1, video_seconds: +F("vidSecs").value, video_res: F("vidRes").value,
      idea: vidRec.idea || "", model: "", ...(h3 ? { video_engine: "h3" } : vidEng ? { video_engine: vidEng.id } : {}), ...(m ? { h3_model: m } : {}), ...(ov ? { wf_overrides: ov } : {}),
      ...(Object.keys(media).length ? { media_slots: media } : {}) };
    if (vidCloud()) {
      await api("/api/h3/generate", { json: { gen: params, where: "cloud", count: 1 } });
      closeModal("#vidModal");
      return toast("已送到雲端：H3 影片做好會進圖庫（進度在「導演 → H3 影片」）", "ok");
    }
    const res = await api("/api/generate", { json: { count: 1, params } });
    for (const j of res.jobs) S.pending.set(j.job, { seed: j.seed });
    renderStrip();
    closeModal("#vidModal");
    toast(h3 ? "H3 影片已排入：本機要跑幾分鐘，進度看右上角的任務面板" : "影片已排入", "ok");
  } catch (e) { toast(e.message, "err"); }
  finally { b.disabled = false; }
};

// 圖被移到回收區：從預覽列拿掉
export function forget(ids) {
  const s = new Set(ids);
  viewsrc.forget(s);
  S.results = S.results.filter((r) => !s.has(r.id));
  if (S.current && s.has(S.current.id)) {
    S.current = null;
    viewsrc.select(S.results[0] || null);
    if (!S.results.length) {
      F("viewImg").classList.remove("on");
      F("viewVid").pause();
      F("viewVid").hidden = true;
      F("viewEmpty").hidden = false;
      F("viewMeta").textContent = "";
    }
  }
  renderStrip();
}

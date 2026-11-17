// 混音台頁面入口：找 mixdesk → 曲子清單 → 載入一首（inspect）→ 時間軸、調音台、鋼琴捲軸、樂器庫。
// 設計 docs/superpowers/specs/2026-11-04-mixdesk-panel-design.md；後端 /api/mixdesk/*（src-tauri\src\server\api_mixdesk.rs）。
// 狀態只有一份：S.base＝最後一次 inspect 的結果，S.song＝base 套上還沒寫回譜檔的修改（S.edits），畫面一律從 S.song 畫。
import * as api from "./api.js";
import { Edits, replay, staleTracks, audibleMap, secToBeat, beatToSec, fitPpb, lineAt, reloadDecision, isConflict, stemOf } from "./core.js";
import { Engine, loadPlan } from "./audio.js";
import { Timeline, HEAD_W } from "./timeline.js";
import { Mixer } from "./mixer.js";
import { VideoSync } from "./video.js";
import { PianoRoll } from "./pianoroll.js";
import { Library } from "./library.js";
import { say, showErr, confirmBox, ask, menu } from "./ui.js";

const $ = (s) => document.querySelector(s);

const S = {
  path: null, base: null, song: null, edits: new Edits(),
  ui: { mute: {}, solo: {} }, sel: null, ppb: null,
  loopOn: false, loopRange: null, report: null, loading: new Set(),
};
const engine = new Engine();
const tl = new Timeline($("#mdArr"), {
  edit: (op) => pushEdit(op),
  select: (ti, part) => { S.sel = { track: ti, part }; tl.mark(S.sel); if (!$("#mdLib").hidden && lib.data) lib.render(); },
  openRoll: (ti, part) => openRoll(ti, part),
  seek: (sec) => engine.seek(sec),
  loop: (range) => { S.loopRange = range; S.loopOn = true; syncLoop(); renderTimeline(); },
  solo: (name) => { S.ui.solo[name] = !S.ui.solo[name]; syncEngine(); renderAll(); },
  uiMute: (name) => { S.ui.mute[name] = !S.ui.mute[name]; syncEngine(); renderAll(); },
  zoom: (f) => { S.ppb = Math.min(200, Math.max(1, (S.ppb || 4) * f)); S.ppbTouched = true; renderTimeline(); },
  addPart: (ti, beat) => addPart(ti, beat),
  dropInstrument: (key, track) => dropInstrument(key, track),
});
const mx = new Mixer($("#mdMixer"), {
  edit: (op) => pushEdit(op),
  solo: (name) => { S.ui.solo[name] = !S.ui.solo[name]; syncEngine(); renderAll(); },
  uiMute: (name) => { S.ui.mute[name] = !S.ui.mute[name]; syncEngine(); renderAll(); },
  live: (kind, name, v) => {
    if (kind === "bus") engine.setBus(name, v.volumeDb);
    else engine.setTrack(name, { volumeDb: v.volumeDb, pan: v.pan });
  },
});
const vs = new VideoSync($("#mdVideo"), engine, $("#mdPrev"));
window.mixdeskDebug = { S, engine }; // 除錯用：從開發者工具（或 AI 的 javascript 工具）直接讀狀態
vs.onDuration = (d) => {
  if (S.song && S.song.video) engine.duration = Math.max(engine.duration, (S.song.video.offset_sec || 0) + d);
  renderTimeline();
};
const roll = new PianoRoll($("#mdRoll"), {
  edit: (op) => pushEdit(op),
  audition: (key, notes) => audition(key, notes),
  convertMidi: (name) => convertMidi(name),
  playPart: (p) => engine.play(beatToSec(p.start_beat, S.song.tempo)),
});
const lib = new Library($("#mdLib"), {
  audition: (key, notes) => audition(key, notes),
  addTrack: (key) => addTrack(key),
  setInstrument: (key) => { const t = selectedTrack(); if (t) pushEdit({ op: "track.set", track: t, instrument: key }); },
  selectedTrack: () => selectedTrack(),
});

// 主題跟著繪匣（父頁面 <html data-theme>）
function syncTheme() {
  try {
    const t = parent.document.documentElement.dataset.theme;
    if (t) document.documentElement.dataset.theme = t;
    else delete document.documentElement.dataset.theme;
  } catch { /* 單獨打開 /mixdesk 時沒有父頁面 */ }
}

export function showEmpty(text) {
  const el = $("#mdEmpty");
  el.textContent = text;
  el.hidden = !text;
}

const store = {
  get: (k) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* 無痕模式 */ } },
};

// ── 畫面 ──

function view() {
  return {
    ppb: S.ppb, sel: S.sel, ui: S.ui, loopRange: S.loopRange, sec: engine.timeSec, videoDur: vs.duration,
    stale: staleTracks(S.base, S.edits.ops),
  };
}

function renderTimeline() {
  if (!S.song) return;
  if (!S.ppb) S.ppb = fitPpb($("#mdArr").clientWidth, HEAD_W, S.song.length_beats + 4);
  tl.render(S.song, view());
}

function renderAll() {
  if (!S.song) return;
  renderTimeline();
  mx.render(S.song, view());
  if (roll.ti != null) {
    roll.refresh(S.song);
    const t = roll.track, p = roll.part;
    $("#mdRollTab").textContent = t && p ? `鋼琴捲軸：${t.name}・第 ${p.index + 1} 段` : "鋼琴捲軸";
  }
  $("#mdTempo").textContent = `${S.song.tempo} BPM`;
  $("#mdSig").textContent = S.song.time || "4/4";
  $("#mdUndo").disabled = !S.edits.count;
  $("#mdRedo").disabled = !S.edits.undone.length;
  const v = S.song.video;
  vs.set(v ? `/api/mixdesk/video?song=${api.q(S.path)}&v=${encodeURIComponent(v.path)}` : null, v ? v.offset_sec : 0);
  $("#mdVideoCap").textContent = v ? `🎬 ${v.path.split(/[\\/]/).pop()}（從第 ${(v.offset_sec || 0).toFixed(2)} 秒開始）` : "沒有影片：按上面的「🎬 影片」選一支";
  showPending();
}

function showPending() {
  const n = S.edits.count;
  const el = $("#mdPending");
  el.hidden = !n;
  if (!n) return;
  const stale = staleTracks(S.base, S.edits.ops).size;
  const reread = S.edits.ops.filter((o) => (o.op === "line.set" && "text" in o) || o.op === "line.add").length;
  el.textContent = `● ${n} 處修改未套用${stale ? `・${stale} 軌待渲染` : ""}${reread ? `・重唸 ${reread} 句` : ""}`;
  el.title = "預覽是近似的：按「套用＋重新渲染」寫回譜檔，才聽得到正式版";
}

const fmtTime = (sec) => {
  const m = Math.floor(sec / 60), s = sec - m * 60;
  return `${String(m).padStart(2, "0")}:${s.toFixed(1).padStart(4, "0")}`;
};

let lastShown = "";
function frame() {
  engine.tick();
  const sec = engine.timeSec;
  if (S.song) {
    tl.tick(sec, engine.playing);
    vs.tick(sec, engine.playing);
    const beat = secToBeat(sec, S.song.tempo);
    const shown = `${fmtTime(sec)}|${Math.floor(beat / (S.song.beats_per_bar || 4)) + 1}`;
    if (shown !== lastShown) {
      lastShown = shown;
      $("#mdTime").textContent = fmtTime(sec);
      $("#mdBarNo").textContent = `第 ${shown.split("|")[1]} 小節`;
      const l = S.song.dialogue ? lineAt(S.song.dialogue.lines, sec) : null;
      $("#mdSub").textContent = l ? l.text : "";
    }
  }
  mx.tick(engine, S.stem);
  $("#mdPlay").textContent = engine.playing ? "❚❚" : "▶";
  requestAnimationFrame(frame);
}

// ── 播放引擎 ──

/** 把 S.song 的音量、聲像、匯流排、靜音獨奏套到 Engine；長曲取消靜音的軌這時才載 */
function syncEngine() {
  if (!S.song) return;
  const song = S.song;
  const stem = (S.stem = stemOf(S.edits.ops)); // 改過名的軌照舊用渲染時的分軌
  for (const t of song.tracks) { const st = stem(t.name); if (st) engine.setTrack(st, { volumeDb: t.volume_db, pan: t.pan, bus: t.bus || "main" }); }
  engine.setBus("main", ((song.buses || {}).main || {}).volume_db);
  for (const [b, cfg] of Object.entries(song.buses || {})) if (b !== "main") engine.setBus(b, cfg && cfg.volume_db);
  if (song.video) engine.setTrack("video", { volumeDb: song.video.volume_db || 0, pan: 0 });
  const aud = {};
  for (const [name, on] of Object.entries(audibleMap(song, S.ui))) { const st = stem(name); if (st) aud[st] = on; }
  for (const name of engine.tracks.keys()) if (!(name in aud)) aud[name] = false; // 刪掉的軌、沒影片時的原聲
  engine.setAudible(aud);
  const pre = (S.base.render && S.base.render.stems_pre) || {};
  for (const [name, on] of Object.entries(aud)) {
    if (!on || !pre[name] || engine.tracks.has(name) || S.loading.has(name) || !S.audioReady) continue;
    S.loading.add(name);
    engine.addTrack(song, name, api.fileUrl(pre[name]))
      .then(() => syncEngine())
      .catch((e) => say(`載入 ${name} 失敗：${e.message}`))
      .finally(() => S.loading.delete(name));
  }
}

function syncLoop() {
  engine.loop(...(S.loopOn && S.loopRange ? S.loopRange : [0, 0]));
  $("#mdLoop").classList.toggle("on", S.loopOn);
}

async function loadAudio(gen = S.gen) {
  S.audioReady = false;
  const r = S.base.render;
  if (!r || !r.stems_pre || !Object.keys(r.stems_pre).length) {
    S.audioKey = null;
    await engine.load(S.song, {}, undefined, {});
    say("這首還沒渲染過：按「套用＋重新渲染」之後才聽得到。", 0);
    S.audioReady = true;
    return;
  }
  // 渲染結果沒變（例如外面只改了音量、還沒重新渲染）：分軌不用重載，只同步推桿
  const key = `${S.path}|${r.report_json}|${r.song_hash}`;
  if (key === S.audioKey && engine.tracks.size) {
    S.audioReady = true;
    syncEngine();
    if (r.stale) say("⚠ 譜檔在上次渲染之後改過了：音量、聲像、靜音已經跟上，其他要按「套用＋重新渲染」。", 0);
    return;
  }
  S.audioKey = key;
  S.report = await api.get(api.fileUrl(r.report_json)).catch(() => null);
  const aud = audibleMap(S.song, S.ui);
  const stem = stemOf(S.edits.ops);
  const names = loadPlan(S.song.tracks, S.song.tracks.map((t) => aud[t.name])).map(stem).filter((n) => n && r.stems_pre[n]);
  if (r.stems_pre.dialogue) names.push("dialogue");
  const urls = Object.fromEntries(names.map((n) => [n, api.fileUrl(r.stems_pre[n])]));
  say(`載入分軌中（${names.length} 軌）…`, 0);
  const t0 = performance.now();
  const at = engine.timeSec, wasPlaying = engine.playing;
  const current = await engine.load(S.song, urls, undefined, { duck: S.report && S.report.duck_curve_100hz });
  if (!current || gen !== S.gen) return; // 載入途中又換了曲子：交給新的那次
  engine.setMasterGainDb(S.report ? S.report.master_gain_db : 0);
  if (S.song.video && vs.duration) engine.duration = Math.max(engine.duration, (S.song.video.offset_sec || 0) + vs.duration);
  S.audioReady = true;
  syncEngine();
  syncLoop();
  $("#mdAB").textContent = "預覽";
  $("#mdAB").classList.remove("on");
  engine.abMaster(false);
  engine.seek(Math.min(at, engine.duration));
  if (wasPlaying) engine.play(at);
  const skipped = S.song.tracks.length - names.filter((n) => n !== "dialogue").length;
  say(`${r.stale ? "⚠ 譜檔在上次渲染之後改過了：預覽聽到的是舊版，按「套用＋重新渲染」更新。　" : ""}載入 ${names.length} 軌，${((performance.now() - t0) / 1000).toFixed(1)} 秒${skipped > 0 ? `（${skipped} 軌靜音中，取消靜音時再載）` : ""}`, r.stale ? 0 : 5000);
}

// ── 曲子 ──

/** 回傳 true＝載好、false＝譜檔有錯、null＝途中又換了曲子（這次作廢） */
async function loadSong(path, { keepEdits = false, audio = true } = {}) {
  const gen = (S.gen = (S.gen || 0) + 1);
  let base;
  try {
    base = await api.get(`/api/mixdesk/song?path=${api.q(path)}`);
    if (gen !== S.gen) return null;
  } catch (e) {
    if (gen !== S.gen) return null;
    showErr(`譜檔目前有錯（畫面是上一次讀成功的樣子）：\n${e.message}`);
    return false;
  }
  showErr("");
  showEmpty("");
  const changed = path !== S.path;
  if (changed) {
    engine.stop();
    engine.seek(0);
    Object.assign(S, { path, ppb: null, ppbTouched: false, sel: null, ui: { mute: {}, solo: {} }, loopOn: false, loopRange: null });
    store.set("mixdesk.song", path);
    $("#mdSong").value = path;
  }
  if (!keepEdits) S.edits.clear();
  S.base = base;
  S.badMtime = null;
  recompute();
  renderAll();
  if (audio) await loadAudio(gen).catch((e) => say(`分軌載入失敗：${e.message}`, 0));
  return true;
}

/** S.song＝base＋還沒套用的修改；換過樂器的軌從樂器庫補上鼓組、音域（前端套用只知道 key） */
function recompute() {
  const song = replay(S.base, S.edits.ops);
  const items = lib.data ? new Map(lib.data.items.map((i) => [i.key, i])) : null;
  if (items) for (const t of song.tracks) {
    const it = items.get(t.instrument.key);
    if (it && t.instrument.name === t.instrument.key) t.instrument = { ...t.instrument, name: it.names[0], is_drums: it.is_drums, range: it.range, category: it.category };
  }
  S.song = song;
}

function pushEdit(op) {
  S.edits.push(op);
  recompute();
  syncEngine();
  renderAll();
}

function undo() { if (S.edits.undo()) { recompute(); syncEngine(); renderAll(); } }
function redo() { if (S.edits.redo()) { recompute(); syncEngine(); renderAll(); } }

function addPart(ti, beat) {
  const t = S.song.tracks[ti];
  const bpb = S.song.beats_per_bar || 4;
  const b = beat ?? Math.floor(secToBeat(engine.timeSec, S.song.tempo) / bpb) * bpb;
  pushEdit({ op: "part.add", track: t.name, at: 1 + b / bpb, notes: [], length_beats: bpb * 4 });
  S.sel = { track: ti, part: S.song.tracks[ti].parts.filter((p) => p.index >= 0).length - 1 };
  renderAll();
}

// ── 鋼琴捲軸、樂器庫、試聽 ──

function showTab(name) {
  for (const x of document.querySelectorAll(".md-tab")) x.classList.toggle("on", x.dataset.p === name);
  for (const p of document.querySelectorAll(".md-pane")) p.hidden = p.dataset.p !== name;
  if (name === "lib") lib.show().then(() => recompute());
}

function openRoll(ti, part) {
  S.sel = { track: ti, part };
  showTab("roll");
  roll.open(S.song, ti, part);
  renderAll();
  $("#mdRoll").focus();
}

const selectedTrack = () => (S.sel && S.song && S.song.tracks[S.sel.track] ? S.song.tracks[S.sel.track].name : null);

async function convertMidi(name) {
  if (!(await confirmBox(`把「${name}」轉成譜？`, "MIDI 的音符會寫進譜檔（notes），之後就能在鋼琴捲軸改。譜檔會先備份。", "轉換"))) return;
  S.edits.push({ op: "midi.convert", track: name });
  const ti = S.sel ? S.sel.track : 0;
  if (await applyEdits({ render: false })) openRoll(ti, 0);
}

function uniqueName(base) {
  const names = new Set(S.song.tracks.map((t) => t.name));
  let n = base, i = 2;
  while (names.has(n)) n = `${base}_${i++}`;
  return n;
}

function addTrack(key) {
  if (!S.song) return;
  const name = uniqueName(key);
  const bpb = S.song.beats_per_bar || 4;
  const bar = Math.floor(secToBeat(engine.timeSec, S.song.tempo) / bpb);
  const after = selectedTrack();
  S.edits.push({ op: "track.add", name, instrument: key, ...(after ? { after } : {}) });
  S.edits.push({ op: "part.add", track: name, at: 1 + bar, notes: [], length_beats: bpb * 4 });
  recompute();
  syncEngine();
  const ti = S.song.tracks.findIndex((t) => t.name === name);
  say(`新增一軌「${name}」：在鋼琴捲軸點兩下寫音符，寫完按「套用＋重新渲染」才聽得到`);
  openRoll(ti, 0);
}

function dropInstrument(key, track) {
  if (track) pushEdit({ op: "track.set", track, instrument: key });
  else addTrack(key);
}

const auditions = new Map();
let auditionSrc = null;
async function audition(key, notes) {
  const k = `${key}|${notes || ""}`;
  try {
    let buf = auditions.get(k);
    if (!buf) {
      say("試聽：合成中…", 0);
      const r = await api.post("/api/mixdesk/audition", { instrument: key, notes });
      const data = await fetch(api.fileUrl(r.path)).then((x) => x.arrayBuffer());
      buf = await engine.ctx.decodeAudioData(data);
      auditions.set(k, buf);
      say("");
    }
    if (auditionSrc) try { auditionSrc.stop(); } catch { /* 已經停了 */ }
    engine.ctx.resume?.();
    auditionSrc = engine.ctx.createBufferSource();
    auditionSrc.buffer = buf;
    auditionSrc.connect(engine.ctx.destination);
    auditionSrc.start();
  } catch (e) {
    say(`試聽失敗：${e.message}`);
  }
}

// ── 套用＋重新渲染、自動重新載入、衝突 ──

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function progress(text, frac) {
  const box = $("#mdProgress");
  box.hidden = text == null;
  if (text == null) return;
  box.querySelector("span").textContent = text;
  box.querySelector("i").style.width = Math.round(Math.max(0.02, Math.min(1, frac || 0)) * 160) + "px";
}

/** 把修改寫回譜檔（render＝再重新渲染）；渲染中再按就排隊。回傳有沒有成功寫回 */
async function applyEdits({ render = true } = {}) {
  if (!S.path) return false;
  if (S.busy) {
    if (render) { S.queued = true; $("#mdApply").textContent = "排隊中：做完再套用一次"; }
    return false;
  }
  S.busy = true;
  let ok = true;
  try {
    const sent = S.edits.count;
    if (sent) {
      progress("寫回譜檔…", 0.02);
      S.edits.seal(); // 寫回期間又改的另外記，不併進送出去的那幾筆
      let r;
      try {
        r = await api.post("/api/mixdesk/apply", { path: S.path, edits: S.edits.toApply(S.base.hash) });
      } catch (e) {
        S.edits.unseal();
        throw e;
      }
      S.edits.done(sent);
      await loadSong(S.path, { keepEdits: true, audio: !render });
      say(`已寫回譜檔${r.backup ? "（舊版備份在 mixdesk 的 .tmp\\song_backups）" : ""}`);
    }
    if (render) await runRender();
  } catch (e) {
    ok = false;
    progress(null);
    if (isConflict(e)) await conflict();
    else showErr(`套用失敗（修改都還在，可以改好再按一次）：\n${e.message}`);
  } finally {
    S.busy = false;
    progress(null);
    $("#mdApply").textContent = "套用＋重新渲染";
  }
  if (S.queued) { S.queued = false; applyEdits(); }
  return ok;
}

async function runRender() {
  const t0 = performance.now();
  const { job } = await api.post("/api/mixdesk/render", { path: S.path });
  S.job = job;
  const total = Math.max(1, S.song.tracks.length);
  let j;
  for (;;) {
    await sleep(700);
    j = await api.get(`/api/mixdesk/job/${job}`);
    if (j.state === "queued") { progress("排隊中（前面還有一首在渲染）…", 0.02); continue; }
    if (j.state !== "running") break;
    const done = j.events.filter((e) => e.event === "track").length;
    const last = j.events[j.events.length - 1];
    const what = !last ? "準備中" : last.event === "tts" ? `唸台詞「${[...last.text].slice(0, 14).join("")}…」` : last.event === "track" ? `${last.name}${last.cached ? "（快取）" : ""}` : "";
    progress(`渲染中：${done}/${total} 軌・${what}${done >= total ? "・混音、母帶" : ""}`, done / total);
  }
  S.job = null;
  progress(null);
  if (j.state === "done") {
    const warns = j.events.filter((e) => e.event === "warning").map((e) => e.text);
    await loadSong(S.path, { keepEdits: true });
    say(`渲染完成（${((performance.now() - t0) / 1000).toFixed(1)} 秒）${warns.length ? "　⚠ " + warns.join("；") : ""}`, 12000);
  } else if (j.state === "stopped") say("渲染取消了");
  else throw new Error(j.error || "渲染失敗");
}

/** 譜檔在外面被改了，面板又有未套用的修改 */
async function conflict() {
  if (S.asking) return;
  S.asking = true;
  const r = await ask("譜檔被改過了", [], {
    note: `譜檔在外面被改過了（可能是小克或另一個視窗）。你這邊還有 ${S.edits.count} 處修改沒套用。\n\n「保留」會把你的修改改套到新版譜檔上，先看一下對不對再按套用。`,
    buttons: [{ label: "重新載入（丟掉我的修改）", value: "reload", acc: true }, { label: "保留我的修改", value: "keep" }],
  });
  if (r && r.button === "reload") { S.edits.clear(); await loadSong(S.path); say("已重新載入"); }
  else { await loadSong(S.path, { keepEdits: true }); say("已改套到新版譜檔上：確認沒問題再按「套用＋重新渲染」", 0); }
  S.asking = false;
}

async function poll() {
  for (;;) {
    await sleep(2000);
    if (!S.path || !S.base) continue;
    const r = await api.get(`/api/mixdesk/mtime?path=${api.q(S.path)}`).catch(() => null);
    const mtime = r ? r.mtime : null;
    if (mtime != null && mtime === S.badMtime) continue; // 這一版讀過了而且有錯：等下一次存檔
    const d = reloadDecision({ known: S.base.mtime, mtime, pending: S.edits.count, busy: S.busy || S.asking, dialog: !$("#mdDialog").hidden });
    if (d === "reload") {
      const ok = await loadSong(S.path);
      if (ok) say("譜檔在外面改過了，已重新載入");
      else if (ok === false) S.badMtime = mtime;
    } else if (d === "ask") await conflict();
  }
}

// ── 新曲子、影片、送剪輯台 ──

async function newSong() {
  const r = await ask("新曲子", [
    { key: "title", label: "曲名（也是檔名）", value: "" },
    { key: "tempo", label: "速度 BPM", type: "number", value: 120, min: 20, max: 300 },
    { key: "time", label: "拍號", type: "select", value: "4/4", options: [["4/4", "4/4"], ["3/4", "3/4"], ["6/8", "6/8"], ["2/4", "2/4"]] },
  ], { note: "會在 mixdesk 的 songs 資料夾建一份空白譜檔。" });
  if (!r || !r.values.title.trim()) return;
  if (S.edits.count && !(await confirmBox("換曲子？", `有 ${S.edits.count} 處修改還沒套用，換曲子會丟掉。`, "丟掉並換曲子"))) return;
  try {
    const { path } = await api.post("/api/mixdesk/new", { title: r.values.title.trim(), tempo: r.values.tempo, time: r.values.time });
    await fillSongs();
    S.edits.clear();
    await loadSong(path);
    say("新曲子建好了：到「樂器庫」挑樂器（＋新軌），或請小克幫忙寫");
  } catch (e) { showErr(`建不了新曲子：${e.message}`); }
}

async function setVideo(op) {
  S.edits.push(op);
  if (await applyEdits({ render: false })) say("影片設定寫回譜檔了（影片不用重新渲染）");
}

function videoMenu(e) {
  const v = S.song && S.song.video;
  const r = e.target.getBoundingClientRect();
  const pick = (() => { try { return parent !== window && parent.mixdeskPickVideo; } catch { return null; } })();
  menu(r.left, r.bottom + 4, [
    pick && { label: "從圖庫挑一支…", fn: () => pick((rec) => setVideo({ op: "video.set", path: `gallery:${rec.id}`, offset_sec: v ? v.offset_sec || 0 : 0 })) },
    { label: "輸入影片檔路徑…", fn: async () => {
      const a = await ask("影片檔", [{ key: "path", label: "完整路徑（mp4、webm、mov…），或相對於譜檔的路徑", value: v && !String(v.path).startsWith("gallery:") ? v.path : "" }]);
      if (a && a.values.path.trim()) setVideo({ op: "video.set", path: a.values.path.trim().replace(/^"|"$/g, ""), offset_sec: v ? v.offset_sec || 0 : 0 });
    } },
    v && { label: "影片從第幾秒開始…", fn: async () => {
      const a = await ask("影片起點", [{ key: "off", label: "配樂播到第幾秒時影片開始（秒）", type: "number", value: v.offset_sec || 0, min: 0, step: 0.01 }]);
      if (a && a.values.off >= 0) pushEdit({ op: "video.set", offset_sec: a.values.off });
    } },
    v && { label: "拿掉影片", fn: () => setVideo({ op: "video.clear" }) },
  ]);
}

async function toStudio() {
  if (!S.path) return;
  if (S.edits.count && !(await confirmBox("還有修改沒套用", `送出去的是上次渲染的版本，還沒套用的 ${S.edits.count} 處修改不會在裡面。`, "照樣送"))) return;
  const r = await ask("送到剪輯台", [{ key: "what", label: "要送什麼", type: "select", value: "mix", options: [["mix", "只送配樂（mix.wav）"], ["stems", "配樂＋各軌分軌（stems）"]] }]);
  if (!r) return;
  try {
    const x = await api.post("/api/mixdesk/to_studio", { path: S.path, stems: r.values.what === "stems" });
    say(`已放進剪輯台素材：${x.imported.join("、")}${x.srt ? `\n字幕檔在 ${x.srt}（剪輯台沒有匯入字幕的入口，要自己拖進去）` : ""}`, 0);
  } catch (e) { showErr(`送不過去：${e.message}`); }
}

// ── 走帶列、分頁、鍵盤 ──

function togglePlay() {
  if (!S.song) return;
  if (engine.playing) engine.stop();
  else engine.play();
}

async function toggleAB() {
  const r = S.base && S.base.render;
  if (!r || !r.mix) return say("還沒渲染過，沒有正式母帶可以聽。");
  if (!engine.mixBuffer) {
    say("載入正式母帶（mix.wav）…", 0);
    await engine.loadMix(api.fileUrl(r.mix)).catch((e) => say(`讀不到 mix.wav：${e.message}`));
    say("");
  }
  engine.abMaster(!engine.ab);
  $("#mdAB").textContent = engine.ab ? "母帶" : "預覽";
  $("#mdAB").classList.toggle("on", engine.ab);
  if (engine.ab && S.edits.count) say("正在聽的是上次渲染的正式版，還沒套用的修改聽不到。");
}

function wire() {
  $("#mdPlay").onclick = togglePlay;
  $("#mdStop").onclick = () => { engine.stop(); engine.seek(S.loopOn && S.loopRange ? S.loopRange[0] : 0); };
  $("#mdLoop").onclick = () => {
    S.loopOn = !S.loopOn;
    if (S.loopOn && !S.loopRange && S.song) S.loopRange = [0, S.song.length_sec];
    syncLoop();
    renderTimeline();
  };
  $("#mdAB").onclick = toggleAB;
  $("#mdUndo").onclick = undo;
  $("#mdRedo").onclick = redo;
  $("#mdApply").onclick = () => applyEdits();
  $("#mdCancel").onclick = () => { if (S.job) api.post(`/api/mixdesk/job/${S.job}/stop`).catch(() => {}); };
  $("#mdNew").onclick = newSong;
  $("#mdVideoBtn").onclick = videoMenu;
  $("#mdStudio").onclick = toStudio;
  $("#mdSong").onchange = async (e) => {
    const p = e.target.value;
    if (S.edits.count && !(await confirmBox("換曲子？", `有 ${S.edits.count} 處修改還沒套用，換曲子會丟掉。`, "丟掉並換曲子"))) {
      e.target.value = S.path;
      return;
    }
    S.edits.clear();
    loadSong(p);
  };
  for (const b of document.querySelectorAll(".md-tab")) b.onclick = () => showTab(b.dataset.p);
  addEventListener("keydown", (e) => {
    if (!$("#mdDialog").hidden || /INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName)) return;
    if (e.key === " " && !e.ctrlKey) { e.preventDefault(); togglePlay(); }
    else if (e.ctrlKey && e.key.toLowerCase() === "z" && !e.shiftKey) { e.preventDefault(); undo(); }
    else if (e.ctrlKey && (e.key.toLowerCase() === "y" || (e.shiftKey && e.key.toLowerCase() === "z"))) { e.preventDefault(); redo(); }
  });
  addEventListener("resize", () => { if (S.song && !S.ppbTouched) { S.ppb = null; renderTimeline(); } });
}

async function boot() {
  syncTheme();
  try { new MutationObserver(syncTheme).observe(parent.document.documentElement, { attributes: true, attributeFilter: ["data-theme"] }); } catch { /* 同上 */ }
  wire();
  requestAnimationFrame(frame);
  const st = await api.get("/api/mixdesk/status").catch((e) => ({ ok: false, error: e.message }));
  if (!st.ok) {
    showEmpty(`混音台需要小克的 mixdesk。\n\n${st.error || "連不上繪匣"}`);
    return;
  }
  const songs = await fillSongs();
  poll();
  if (!songs.length) return showEmpty("還沒有曲子：按上面的「＋」新增一首，或請小克寫一首放在 mixdesk 的 songs 資料夾。");
  const last = store.get("mixdesk.song");
  await loadSong(songs.some((s) => s.path === last) ? last : songs[0].path);
}

async function fillSongs() {
  const { songs } = await api.get("/api/mixdesk/songs");
  const sel = $("#mdSong");
  sel.innerHTML = "";
  for (const s of songs) {
    const o = document.createElement("option");
    o.value = s.path;
    o.textContent = (s.dir === "examples" ? "示範：" : "") + s.name;
    sel.append(o);
  }
  if (S.path) sel.value = S.path;
  return songs;
}

boot();

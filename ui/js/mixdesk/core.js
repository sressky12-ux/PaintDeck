// 混音台的純邏輯（沒有 DOM，tests/mixdesk_core.test.cjs 直接測）：時間換算、展開 repeat、獨奏／靜音、等功率聲像、吸附、
// 修改堆疊、在前端先套用修改（畫面立刻更新；規則跟 mixdesk 的 apply 一樣，含連動）、哪些軌要重新渲染才聽得到。
// song 一律是 mixdesk inspect 的 JSON（格式見 mixdesk 的 docs/score-format.md「給混音台面板的 JSON 指令」）。

export const beatToSec = (beat, tempo) => (beat * 60) / tempo;
export const secToBeat = (sec, tempo) => (sec * tempo) / 60;

/** 一段展開成絕對拍數的音符（repeat 幾輪就幾份） */
export function expandPart(part) {
  const out = [];
  for (let r = 0; r < (part.repeat || 1); r++) {
    const off = part.start_beat + r * part.length_beats;
    for (const n of part.notes) out.push({ start: off + n.beat, end: off + n.beat + n.beats, pitch: n.pitch, vel: n.vel, rep: r });
  }
  return out;
}

/** 在 beat 這一刻正在響的音符（回傳索引的 Set；結束那一拍不算） */
export function activeAt(notes, beat) {
  const on = new Set();
  notes.forEach((n, i) => { if (beat >= n.start && beat < n.end) on.add(i); });
  return on;
}

/** 每軌會不會響：有任何獨奏時只有「獨奏而且沒靜音」的會響；靜音的初始值是譜檔的 mute */
export function audible(tracks, ui) {
  const muted = (t) => (ui.mute && t.name in ui.mute ? !!ui.mute[t.name] : !!t.mute);
  const anySolo = tracks.some((t) => ui.solo && ui.solo[t.name]);
  return tracks.map((t) => !muted(t) && (!anySolo || !!ui.solo[t.name]));
}

/** 等功率聲像＋音量，跟 mixdesk/mixer.py 的 pan_gain 一模一樣：回傳 [左, 右] 增益 */
export function panGains(volumeDb, pan) {
  const g = 10 ** (volumeDb / 20);
  const th = ((pan + 1) * Math.PI) / 4;
  return [Math.cos(th) * Math.SQRT2 * g, Math.sin(th) * Math.SQRT2 * g];
}

/** 吸附到格線（grid 是拍數，0＝自由） */
export function snap(beat, grid) {
  if (!grid) return beat;
  return Math.round(Math.round(beat / grid) * grid * 1e9) / 1e9;
}

const keysOf = (op) => Object.keys(op).filter((k) => k !== "op" && k !== "track").sort().join(",");

/** 還沒套用的修改：同一軌連續改同幾個欄位（拖推桿）合成一筆；可以復原、重做。
 *  寫回途中（seal 之後）送出去的那幾筆不再合併、不能復原，寫回成功用 done(n) 拿掉、失敗用 unseal() 放回來 */
export class Edits {
  constructor() { this.ops = []; this.undone = []; this.sealed = 0; }
  get count() { return this.ops.length; }
  seal() { this.sealed = this.ops.length; }
  unseal() { this.sealed = 0; }
  done(n) { this.ops.splice(0, n); this.undone = []; this.sealed = 0; }
  push(op) {
    const last = this.ops.length > this.sealed ? this.ops[this.ops.length - 1] : null;
    if (last && op.op === "track.set" && last.op === "track.set" && last.track === op.track && keysOf(last) === keysOf(op)) {
      Object.assign(last, op);
    } else {
      this.ops.push({ ...op });
    }
    this.undone = [];
  }
  undo() { if (this.ops.length <= this.sealed) return null; const op = this.ops.pop(); this.undone.push(op); return op; }
  redo() { const op = this.undone.pop() || null; if (op) this.ops.push(op); return op; }
  toApply(baseHash) { return { v: 1, base_hash: baseHash, ops: this.ops.map((o) => ({ ...o })) }; }
  clear() { this.ops = []; this.undone = []; this.sealed = 0; }
}

const trackIdx = (song, name) => song.tracks.findIndex((t) => t.name === name);

function partOf(song, op) {
  const ti = trackIdx(song, op.track);
  const part = ti >= 0 ? song.tracks[ti].parts.find((p) => p.index === op.part) : null;
  return [ti, part];
}

/** 所有連動參照 [軌, 段]：f 回傳新的參照或 null（刪掉） */
function remapLinks(song, f) {
  for (const t of song.tracks) for (const p of t.parts) {
    p.linked_with = (p.linked_with || []).map(f).filter(Boolean);
  }
}

function reindexParts(song, ti) {
  const map = new Map();
  song.tracks[ti].parts.forEach((p, i) => { if (p.index >= 0) { map.set(p.index, i); p.index = i; } });
  remapLinks(song, ([t, pi]) => (t !== ti ? [t, pi] : map.has(pi) ? [t, map.get(pi)] : null));
}

const partEnd = (notes) => notes.reduce((m, n) => Math.max(m, n.beat + n.beats), 0);

/** 在 song（會被改）上套用一筆修改，規則跟 mixdesk apply 一樣 */
export function applyLocal(song, op) {
  const bpb = song.beats_per_bar || 4;
  switch (op.op) {
    case "track.set": {
      const t = song.tracks[trackIdx(song, op.track)];
      if (!t) break;
      for (const k of ["volume_db", "pan", "bus", "mute", "fx"]) if (k in op) t[k] = op[k];
      if ("instrument" in op) t.instrument = { ...t.instrument, key: op.instrument, name: op.instrument };
      if ("name" in op) t.name = op.name;
      break;
    }
    case "track.add": {
      const t = { name: op.name, instrument: { key: op.instrument, name: op.instrument, category: "", engine: "", is_drums: false, range: null },
        volume_db: 0, pan: 0, bus: "main", mute: false, fx: [], sidechain: null, midi: null, parts: [] };
      const after = trackIdx(song, op.after);
      const at = after >= 0 ? after + 1 : song.tracks.length;
      song.tracks.splice(at, 0, t);
      remapLinks(song, ([ti, pi]) => [ti >= at ? ti + 1 : ti, pi]);
      break;
    }
    case "track.remove": {
      const at = trackIdx(song, op.track);
      if (at < 0) break;
      song.tracks.splice(at, 1);
      remapLinks(song, ([ti, pi]) => (ti === at ? null : [ti > at ? ti - 1 : ti, pi]));
      break;
    }
    case "part.notes": {
      const [ti, part] = partOf(song, op);
      if (!part) break;
      const len = op.length_beats ?? Math.max(part.length_beats, partEnd(op.notes));
      for (const [t2, p2] of [[ti, part.index], ...(part.link ? part.linked_with : [])]) {
        const p = song.tracks[t2]?.parts.find((x) => x.index === p2);
        if (p) { p.notes = op.notes.map((n) => ({ ...n })); p.length_beats = len; }
      }
      break;
    }
    case "part.unlink": {
      const [ti, part] = partOf(song, op);
      if (!part) break;
      part.link = null;
      part.linked_with = [];
      remapLinks(song, ([t2, p2]) => (t2 === ti && p2 === part.index ? null : [t2, p2]));
      break;
    }
    case "part.set": {
      const [, part] = partOf(song, op);
      if (!part) break;
      if ("at" in op) { part.at = op.at; part.start_beat = (op.at - 1) * bpb; }
      if ("repeat" in op) part.repeat = op.repeat;
      break;
    }
    case "part.add": {
      const ti = trackIdx(song, op.track);
      if (ti < 0) break;
      const notes = (op.notes || []).map((n) => ({ ...n }));
      const parts = song.tracks[ti].parts;
      parts.push({ index: parts.filter((p) => p.index >= 0).length, at: op.at, repeat: 1, start_beat: (op.at - 1) * bpb,
        length_beats: op.length_beats ?? partEnd(notes), notes, link: null, linked_with: [] });
      break;
    }
    case "part.remove": {
      const [ti, part] = partOf(song, op);
      if (!part) break;
      song.tracks[ti].parts.splice(song.tracks[ti].parts.indexOf(part), 1);
      reindexParts(song, ti);
      break;
    }
    case "bus.set": {
      song.buses = song.buses || {};
      const b = (song.buses[op.bus] = { ...(song.buses[op.bus] || {}) });
      for (const k of ["volume_db", "fx"]) if (k in op) b[k] = op[k];
      break;
    }
    case "master.set":
      for (const k of ["lufs", "ceiling_dbtp"]) if (k in op) song.master[k] = op[k];
      break;
    case "line.set": {
      const l = song.dialogue?.lines[op.line];
      if (!l) break;
      if ("at_sec" in op && l.end_sec != null) l.end_sec += op.at_sec - l.at_sec; // 搬時間：唸的長度不變
      if ("text" in op && op.text !== l.text) l.end_sec = null; // 改字：長度要重唸才知道
      for (const k of ["at_sec", "text", "emotion"]) if (k in op) l[k] = op[k];
      break;
    }
    case "line.add": {
      song.dialogue = song.dialogue || { duck: {}, lines: [] };
      song.dialogue.lines.push({ index: song.dialogue.lines.length, at_sec: op.at_sec, speaker: op.speaker, lang: op.lang,
        emotion: op.emotion ?? null, text: op.text, end_sec: null });
      break;
    }
    case "line.remove":
      if (song.dialogue) {
        song.dialogue.lines.splice(op.line, 1);
        song.dialogue.lines.forEach((l, i) => { l.index = i; });
      }
      break;
    case "video.set":
      song.video = { path: "", offset_sec: 0, volume_db: 0, mute: false, ...(song.video || {}) };
      for (const k of ["path", "offset_sec", "volume_db", "mute"]) if (k in op) song.video[k] = op[k];
      break;
    case "video.clear":
      song.video = null;
      break;
    default:
      break;
  }
  return song;
}

/** 從 inspect 的結果出發，依序套用全部修改（原本的不動） */
export function replay(base, ops) {
  const s = structuredClone(base);
  for (const op of ops) applyLocal(s, op);
  return s;
}

/** 預覽用的分軌是渲染時的軌名：改過名的軌照舊用原本的分軌，新加的（或刪掉再加同名的）還沒有分軌（null） */
export function stemOf(ops) {
  const alias = new Map();
  for (const op of ops) {
    if (op.op === "track.set" && "name" in op && op.name !== op.track) {
      alias.set(op.name, alias.has(op.track) ? alias.get(op.track) : op.track);
      alias.set(op.track, null);
    } else if (op.op === "track.add") alias.set(op.name, null);
    else if (op.op === "track.remove") alias.set(op.track, null);
  }
  return (name) => (alias.has(name) ? alias.get(name) : name);
}

/** 哪些軌改了音符、樂器或效果：預覽聽不出來，要重新渲染（只改音量、聲像、靜音不算） */
export function staleTracks(base, ops) {
  const out = new Set();
  for (const op of ops) {
    if (op.op === "part.notes") {
      out.add(op.track);
      const [, part] = partOf(base, op);
      if (part && part.link) for (const [ti] of part.linked_with) if (base.tracks[ti]) out.add(base.tracks[ti].name);
    } else if (["part.add", "part.remove", "part.set", "track.add", "midi.convert"].includes(op.op)) {
      out.add(op.track ?? op.name);
    } else if (op.op === "track.set" && ("fx" in op || "instrument" in op)) {
      out.add(op.track);
    }
  }
  return out;
}

// ── 畫面用的換算（時間軸、推桿、電平表、字幕） ──

/** 一段在時間軸上的位置與寬度（px；ppb＝每拍幾 px），寬度含 repeat */
export const clipBox = (part, ppb) => ({ left: part.start_beat * ppb, width: part.length_beats * (part.repeat || 1) * ppb });
export const playheadX = (sec, tempo, ppb) => secToBeat(sec, tempo) * ppb;
export const xToBeat = (x, ppb) => x / ppb;
/** 一開始的縮放：整首塞進畫面，太長就最少 2 px／拍 */
export const fitPpb = (viewW, headW, lengthBeats) => Math.max(2, (viewW - headW) / Math.max(1, lengthBeats));

/** 一軌用到的最低、最高音（畫迷你音符的上下範圍） */
export function pitchRange(track) {
  let lo = Infinity, hi = -Infinity;
  for (const p of track.parts) for (const n of p.notes) { if (n.pitch < lo) lo = n.pitch; if (n.pitch > hi) hi = n.pitch; }
  return lo === Infinity ? [60, 60] : [lo, hi];
}

/** 推桿位置 0～1：-12 dB 以上佔上半段（+6 到頂），-12～-60 佔下半段 */
export function dbToFader(db) {
  const f = db >= -12 ? 0.5 + ((db + 12) / 18) * 0.5 : (0.5 * (db + 60)) / 48;
  return Math.min(1, Math.max(0, f));
}
export const faderToDb = (f) => (f >= 0.5 ? -12 + (f - 0.5) * 2 * 18 : -60 + f * 2 * 48);

/** 電平表高度 0～1：-60 dBFS 到 +6 dBFS */
export function meterFrac(peak) {
  if (!(peak > 0)) return 0;
  return Math.min(1, Math.max(0, (20 * Math.log10(peak) + 60) / 66));
}

/** 每軌（含台詞、影片原聲）會不會響：{名字: bool}。台詞的靜音只在面板，影片原聲的靜音寫在譜檔 */
export function audibleMap(song, ui) {
  const list = song.tracks.map((t) => ({ name: t.name, mute: !!t.mute }));
  if (song.dialogue && song.dialogue.lines.length) list.push({ name: "dialogue", mute: false });
  if (song.video) list.push({ name: "video", mute: !!song.video.mute });
  const on = audible(list, ui);
  return Object.fromEntries(list.map((t, i) => [t.name, on[i]]));
}

/** 一句台詞佔的時間 [開始, 結束]；還沒唸過的用字數估（一個字 0.25 秒，最少 1 秒） */
export const lineSpan = (l) => [l.at_sec, l.end_sec ?? l.at_sec + Math.max(1, [...(l.text || "")].length * 0.25)];
export function lineAt(lines, sec) {
  return lines.find((l) => { const [a, b] = lineSpan(l); return sec >= a && sec < b; }) || null;
}

// ── 鋼琴捲軸（音符陣列一律回傳新的，原本的不動；拍數是段落裡的相對拍） ──

/** 吸附格線：[拍數, 顯示名]；三連音是 1/12 音符＝1/3 拍 */
export const GRIDS = [[1, "1/4"], [0.5, "1/8"], [0.25, "1/16"], [0.125, "1/32"], [1 / 3, "三連音 1/12"], [0, "自由"]];
const r9 = (x) => Math.round(x * 1e9) / 1e9;
const clampPitch = (p) => Math.max(0, Math.min(127, p));

/** 拖一組音：抓住的那顆（anchor）吸附到格線，其他照相對位置一起移；整組不會移到 0 拍前面 */
export function moveNotes(notes, sel, anchor, dBeat, dPitch, grid) {
  const a = notes[anchor];
  let delta = snap(a.beat + dBeat, grid) - a.beat;
  const first = Math.min(...[...sel].map((i) => notes[i].beat));
  if (first + delta < 0) delta = -first;
  const dp = Math.round(dPitch);
  return notes.map((n, i) => (sel.has(i) ? { ...n, beat: r9(n.beat + delta), pitch: clampPitch(n.pitch + dp) } : { ...n }));
}

/** 拉長度：吸附，最短一格（自由的話最短 1/32） */
export function resizeNotes(notes, sel, dBeats, grid) {
  const min = grid || 0.125;
  return notes.map((n, i) => (sel.has(i) ? { ...n, beats: Math.max(min, snap(n.beats + dBeats, grid)) } : { ...n }));
}

export function addNote(notes, beat, pitch, beats, vel) {
  const out = notes.map((n) => ({ ...n }));
  out.push({ beat: r9(beat), beats, pitch: clampPitch(pitch), vel });
  return out.sort((x, y) => x.beat - y.beat || x.pitch - y.pitch);
}

export const deleteNotes = (notes, sel) => notes.filter((_, i) => !sel.has(i)).map((n) => ({ ...n }));

/** 框選：拍數範圍碰到一點就算（邊界剛好接著不算），音高含頭尾 */
export function marquee(notes, b0, b1, p0, p1) {
  const [bl, bh] = [Math.min(b0, b1), Math.max(b0, b1)], [pl, ph] = [Math.min(p0, p1), Math.max(p0, p1)];
  const out = new Set();
  notes.forEach((n, i) => { if (n.beat < bh && n.beat + n.beats > bl && n.pitch >= pl && n.pitch <= ph) out.add(i); });
  return out;
}

// ── 自動重新載入（每 2 秒看譜檔的修改時間） ──

/** 譜檔的修改時間變了要怎麼辦：none／reload（自動重載）／ask（有未套用的修改，問使用者） */
export function reloadDecision({ known, mtime, pending, busy, dialog = false }) {
  if (busy || dialog || mtime == null || mtime === known) return "none"; // 對話框開著：等關掉再說（不然提示會蓋掉它）
  return pending ? "ask" : "reload";
}

/** apply 被雜湊擋下（譜檔在載入後被改過）：跟偵測到外部修改一樣處理 */
export const isConflict = (e) => /被改過/.test(String((e && e.message) || e));

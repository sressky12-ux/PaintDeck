// 混音台的播放引擎（Web Audio）：同步播放 mixdesk 渲染出來的「推桿前分軌」（stems_pre/），音量、聲像、靜音、獨奏都在這裡即時套，
// 所以拉推桿不用重新渲染。聲像公式跟 mixdesk 的 pan_gain 一樣（core.panGains）。
// 每軌：BufferSource → 開關（靜音／獨奏）→ 分左右聲道 → 左右各一個 Gain（音量×聲像）→ 電平表 → 合回立體聲 → 匯流排推桿
// →（其他匯流排再進）配樂總線推桿 → 台詞的 ducking 曲線 → 總增益（report.json 的 master_gain_db）→ 簡單限制器 → 喇叭。台詞軌不進配樂匯流排（不會被自己壓低）。
// 匯流排只套音量；匯流排的效果、真正的母帶要渲染後的 mix.wav 才有。「聽正式母帶」直接播 mix.wav，不經過總增益與限制器。
import { panGains } from "./core.js";

/** 要先載入哪些軌：不多就全部；超過 threshold 軌只載會響的，其他等取消靜音再載（長曲很吃記憶體） */
export function loadPlan(tracks, audibleFlags, threshold = 20) {
  if (tracks.length <= threshold) return tracks.map((t) => t.name);
  return tracks.filter((_, i) => audibleFlags[i]).map((t) => t.name);
}

export class Engine {
  constructor(ctx) {
    this.ctx = ctx || new AudioContext({ sampleRate: 48000 });
    this.tracks = new Map();
    this.masterGain = this.ctx.createGain();
    this.limiter = this.ctx.createDynamicsCompressor();
    for (const [k, v] of [["threshold", -1], ["knee", 0], ["ratio", 20], ["attack", 0.001], ["release", 0.1]]) this.limiter[k].value = v;
    this.music = this.ctx.createGain(); // 台詞 ducking 套在這裡
    this.music.connect(this.masterGain);
    this.mainBus = this.ctx.createGain(); // 配樂總線（buses.main）
    this.mainBus.connect(this.music);
    this.buses = new Map(); // 其他匯流排
    this.out = this.ctx.createGain(); // 喇叭前最後一站，總電平表量這裡
    this.out.connect(this.ctx.destination);
    this.outSplit = this.ctx.createChannelSplitter(2);
    this.outL = this.ctx.createAnalyser();
    this.outR = this.ctx.createAnalyser();
    this.out.connect(this.outSplit);
    this.outSplit.connect(this.outL, 0);
    this.outSplit.connect(this.outR, 1);
    this.masterGain.connect(this.limiter);
    this.limiter.connect(this.out);
    this.mixOut = this.ctx.createGain(); // 正式母帶
    this.mixOut.gain.value = 0;
    this.mixOut.connect(this.out);
    this.mixBuffer = null;
    this.duck = null;
    this.playing = false;
    this.startAt = 0;
    this.pausedAt = 0;
    this.duration = 0;
    this.sources = [];
    this.ab = false;
    this.loopRange = null;
    this.loadToken = 0; // 換曲很快的話，晚到的舊曲結果丟掉
    this.fetchBuf = (url) => fetch(url, { cache: "no-store" }).then((r) => { if (!r.ok) throw new Error(`讀不到 ${url}`); return r.arrayBuffer(); });
  }

  _bus(name) {
    if (!name || name === "main") return this.mainBus;
    if (!this.buses.has(name)) { const g = this.ctx.createGain(); g.connect(this.mainBus); this.buses.set(name, g); }
    return this.buses.get(name);
  }

  setBus(name, volumeDb) { this._bus(name).gain.value = 10 ** ((volumeDb || 0) / 20); }

  _chain(name, { dialogue = false, bus = "main" } = {}) {
    const c = this.ctx;
    const t = { name, buffer: null, split: c.createChannelSplitter(2), merge: c.createChannelMerger(2), gainL: c.createGain(), gainR: c.createGain(),
      anL: c.createAnalyser(), anR: c.createAnalyser(), on: c.createGain(), media: null, dialogue, bus };
    t.anL.fftSize = t.anR.fftSize = 2048;
    t.split.connect(t.gainL, 0);
    t.split.connect(t.gainR, 1);
    t.gainL.connect(t.anL);
    t.gainR.connect(t.anR);
    t.gainL.connect(t.merge, 0, 0);
    t.gainR.connect(t.merge, 0, 1);
    t.on.connect(t.split);
    t.merge.connect(dialogue ? this.masterGain : this._bus(bus));
    this.tracks.set(name, t);
    return t;
  }

  /** urls：{軌名: wav 網址}（台詞軌叫 dialogue）；mixUrl：正式母帶（也可以之後 loadMix）；duck：100Hz 的 ducking 曲線。影片原聲那軌留著 */
  async load(song, urls, fetchBuf = this.fetchBuf, { mixUrl = null, duck = null } = {}) {
    const token = ++this.loadToken;
    this.stop();
    for (const [name, t] of [...this.tracks]) {
      if (t.media) continue;
      t.merge.disconnect();
      this.tracks.delete(name);
    }
    for (const g of this.buses.values()) g.disconnect();
    this.buses.clear();
    for (const [name, b] of Object.entries(song.buses || {})) this.setBus(name, b && b.volume_db);
    if (!(song.buses && song.buses.main)) this.mainBus.gain.value = 1;
    this.duck = duck && duck.length ? Float32Array.from(duck) : null;
    let dur = 0;
    await Promise.all(Object.entries(urls).map(async ([name, url]) => {
      const t = await this._loadOne(song, name, url, fetchBuf);
      dur = Math.max(dur, t.buffer.duration);
    }));
    if (token !== this.loadToken) return false; // 被新的載入取代了
    this.mixBuffer = null;
    this.duration = dur;
    if (mixUrl) await this.loadMix(mixUrl, fetchBuf);
    return true;
  }

  /** 正式母帶（mix.wav）：要聽的時候才載；正在播的話從現在的位置接上 */
  async loadMix(url, fetchBuf = this.fetchBuf) {
    this.mixBuffer = await this.ctx.decodeAudioData(await fetchBuf(url));
    this.duration = Math.max(this.duration, this.mixBuffer.duration);
    if (this.playing) this.play(this.timeSec);
  }

  async _loadOne(song, name, url, fetchBuf) {
    const st = song.tracks.find((x) => x.name === name);
    const t = this._chain(name, { dialogue: name === "dialogue", bus: st ? st.bus : "main" });
    t.buffer = await this.ctx.decodeAudioData(await fetchBuf(url));
    if (st) this.setTrack(name, { volumeDb: st.volume_db, pan: st.pan });
    return t;
  }

  /** 長曲一開始沒載的軌（靜音中），取消靜音時再載；正在播的話從現在的位置接上 */
  async addTrack(song, name, url, fetchBuf = this.fetchBuf) {
    if (this.tracks.has(name)) return this.tracks.get(name);
    const token = this.loadToken;
    const t = await this._loadOne(song, name, url, fetchBuf);
    if (token !== this.loadToken || this.tracks.get(name) !== t) return null; // 載到一半換曲了
    this.duration = Math.max(this.duration, t.buffer.duration);
    if (this.playing) this.play(this.timeSec);
    return t;
  }

  /** 影片原聲：MediaElementSource 接成一軌（影片自己播，video.js 負責對時間） */
  attachMedia(name, el) {
    if (this.tracks.has(name)) return this.tracks.get(name);
    const t = this._chain(name, { dialogue: true });
    t.media = this.ctx.createMediaElementSource(el);
    t.media.connect(t.on);
    return t;
  }

  setTrack(name, { volumeDb, pan, bus }) {
    const t = this.tracks.get(name);
    if (!t) return;
    if (bus !== undefined && !t.dialogue && (bus || "main") !== (t.bus || "main")) {
      t.merge.disconnect();
      t.merge.connect(this._bus(bus));
      t.bus = bus;
    }
    const [l, r] = panGains(volumeDb ?? 0, pan ?? 0);
    t.gainL.gain.value = l;
    t.gainR.gain.value = r;
  }

  setAudible(map) {
    for (const [name, on] of Object.entries(map)) { const t = this.tracks.get(name); if (t) t.on.gain.value = on ? 1 : 0; }
  }

  setMasterGainDb(db) { this.masterGain.gain.value = 10 ** ((db || 0) / 20); }

  meter(name) {
    const t = this.tracks.get(name);
    return t ? this._read(t.anL, t.anR) : { peakL: 0, peakR: 0, rmsL: 0, rmsR: 0 };
  }

  meterMaster() { return this._read(this.outL, this.outR); }

  _read(l, r) {
    const out = {};
    for (const [k, an] of [["L", l], ["R", r]]) {
      const a = new Float32Array(an.fftSize);
      an.getFloatTimeDomainData(a);
      let peak = 0, sum = 0;
      for (const x of a) { const v = Math.abs(x); if (v > peak) peak = v; sum += x * x; }
      out["peak" + k] = peak;
      out["rms" + k] = Math.sqrt(sum / a.length);
    }
    return out;
  }

  get timeSec() { return this.playing ? this.ctx.currentTime - this.startAt : this.pausedAt; }

  play(fromSec = this.pausedAt) {
    this.stop(false);
    this.ctx.resume?.();
    const at = this.ctx.currentTime + 0.05;
    const from = Math.max(0, Math.min(fromSec, this.duration || fromSec));
    const startSrc = (buffer, dest) => {
      const s = this.ctx.createBufferSource();
      s.buffer = buffer;
      s.connect(dest);
      s.start(at, from);
      this.sources.push(s);
    };
    for (const t of this.tracks.values()) if (t.buffer) startSrc(t.buffer, t.on);
    if (this.mixBuffer) startSrc(this.mixBuffer, this.mixOut);
    if (this.duck) {
      const i = Math.floor(from * 100);
      const rest = this.duck.subarray(i);
      this.music.gain.cancelScheduledValues(0);
      if (rest.length > 1) this.music.gain.setValueCurveAtTime(rest, at, rest.length / 100);
    }
    this.startAt = at - from;
    this.playing = true;
  }

  stop(keepPos = true) {
    for (const s of this.sources) { try { s.stop(); } catch { /* 已經停了 */ } }
    this.sources = [];
    if (this.playing && keepPos) this.pausedAt = this.timeSec;
    this.playing = false;
    this.music.gain.cancelScheduledValues?.(0);
    this.music.gain.value = 1;
  }

  seek(sec) {
    if (this.playing) this.play(sec);
    else this.pausedAt = Math.max(0, sec);
  }

  /** 聽正式母帶（mix.wav）或聽各軌預覽 */
  abMaster(on) {
    this.ab = !!on && !!this.mixBuffer;
    this.mixOut.gain.value = this.ab ? 1 : 0;
    this.limiter.disconnect();
    if (!this.ab) this.limiter.connect(this.out);
  }

  loop(startSec, endSec) { this.loopRange = endSec > startSec ? [startSec, endSec] : null; }

  /** 畫面每一格呼叫：處理循環與播到底 */
  tick() {
    if (!this.playing) return;
    const t = this.timeSec;
    if (this.loopRange && t >= this.loopRange[1]) this.play(this.loopRange[0]);
    else if (this.duration && t >= this.duration) { this.stop(); this.pausedAt = 0; }
  }
}

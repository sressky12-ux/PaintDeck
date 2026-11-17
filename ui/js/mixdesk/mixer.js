// 調音台分頁：每軌一條（聲像旋鈕、效果格子、雙聲道電平表、推桿、dB、M／S、送哪個匯流排），
// 後面是影片原聲、台詞、其他匯流排、配樂總線、母帶。拖推桿／旋鈕時直接改 Engine（立刻聽得到），放開才推一筆修改。
import { dbToFader, faderToDb, meterFrac } from "./core.js";
import { el, drag, ask } from "./ui.js";
import { colorOf } from "./timeline.js";

/** 效果器與參數預設值：跟 mixdesk 的 docs/score-format.md「效果器（fx）」那張表一樣 */
export const FX = {
  eq: { label: "等化器", p: { low_db: 0, mid_db: 0, high_db: 0 } },
  compressor: { label: "壓縮器", p: { threshold_db: -20, ratio: 2, attack_ms: 10, release_ms: 120 } },
  reverb: { label: "殘響", p: { mix: 0.2, room: 0.5, damping: 0.5 } },
  delay: { label: "延遲", p: { time_beats: 0.5, feedback: 0.3, mix: 0.2 } },
  highpass: { label: "高通", p: { hz: 80 } },
  lowpass: { label: "低通", p: { hz: 12000 } },
  distortion: { label: "破音", p: { drive_db: 10 } },
  chorus: { label: "合唱", p: { rate_hz: 1.0, depth: 0.25, mix: 0.5 } },
  phaser: { label: "相位", p: { rate_hz: 0.5, depth: 0.5, mix: 0.5 } },
  bitcrush: { label: "降位元", p: { bits: 8 } },
};
const PARAM = {
  low_db: "低頻（250 Hz）dB", mid_db: "中頻（1 kHz）dB", high_db: "高頻（4 kHz）dB", threshold_db: "門檻 dB", ratio: "壓縮比",
  attack_ms: "起音 ms", release_ms: "釋放 ms", mix: "濕聲比例（0～1）", room: "房間大小（0～1）", damping: "高頻吸收（0～1）",
  time_beats: "延遲時間（拍）", feedback: "回授（0～1）", hz: "截止頻率 Hz", drive_db: "推力 dB", rate_hz: "速度 Hz", depth: "深度（0～1）", bits: "位元數",
};
const fxName = (fx) => Object.keys(fx)[0];
const fxParams = (fx) => fx[fxName(fx)] || {};
const fmtDb = (db) => (db <= -60 ? "-∞" : (db > 0 ? "+" : "") + db.toFixed(1));
const fmtPan = (p) => (Math.abs(p) < 0.005 ? "C" : (p < 0 ? "L" : "R") + Math.round(Math.abs(p) * 100));

/** 編輯一個效果的參數：回傳新的 {名稱: 參數}、"delete"、"up"，或 null（取消） */
async function editFx(fx, { isNew = false } = {}) {
  const name = fxName(fx);
  const def = FX[name] ? FX[name].p : {};
  const cur = fxParams(fx);
  const keys = [...new Set([...Object.keys(def), ...Object.keys(cur)])];
  const fields = keys.map((k) => ({ key: k, label: PARAM[k] || k, type: "number", step: "any", value: cur[k] ?? def[k] }));
  const buttons = isNew
    ? [{ label: "加上去", value: "ok", acc: true }, { label: "取消", value: null }]
    : [{ label: "儲存", value: "ok", acc: true }, { label: "往前移", value: "up" }, { label: "刪除這個效果", value: "delete" }, { label: "取消", value: null }];
  const r = await ask(`${FX[name] ? FX[name].label : name}（${name}）`, fields, { note: "參數留預設值就不會寫進譜檔。改效果要重新渲染才聽得到。", buttons });
  if (!r) return null;
  if (r.button !== "ok") return r.button;
  const p = {};
  for (const k of keys) {
    const v = r.values[k];
    if (Number.isFinite(v) && (k in cur || v !== def[k])) p[k] = v;
  }
  return { [name]: p };
}

async function pickFx() {
  const r = await ask("加效果", [{ key: "name", label: "效果", type: "select", options: Object.entries(FX).map(([k, v]) => [k, `${v.label}（${k}）`]) }]);
  return r ? { [r.values.name]: {} } : null;
}

export class Mixer {
  /** h：{edit(op), solo(name), uiMute(name), live(kind, name, {volumeDb, pan})} */
  constructor(root, h) {
    this.root = root;
    this.h = h;
    this.meters = [];
  }

  render(song, st) {
    const h = this.h;
    this.meters = [];
    const wrap = el("div", "mx-strips");
    const busNames = [...new Set([...Object.keys(song.buses || {}), ...song.tracks.map((t) => t.bus || "main")])].filter((b) => b !== "main");
    song.tracks.forEach((t, ti) => {
      wrap.append(this._strip({
        name: t.name, color: colorOf(ti), sub: t.instrument.name || t.instrument.key,
        db: t.volume_db, pan: t.pan, fx: t.fx || [], sidechain: t.sidechain, meter: t.name, stale: st.stale.has(t.name),
        muted: t.mute, solo: st.ui.solo[t.name],
        onMute: () => h.edit({ op: "track.set", track: t.name, mute: !t.mute }),
        onSolo: () => h.solo(t.name),
        live: (v) => h.live("track", t.name, v),
        setDb: (db) => h.edit({ op: "track.set", track: t.name, volume_db: db }),
        setPan: (pan) => h.edit({ op: "track.set", track: t.name, pan }),
        setFx: (fx) => h.edit({ op: "track.set", track: t.name, fx }),
        bus: t.bus || "main", buses: ["main", ...busNames],
        setBus: (bus) => h.edit({ op: "track.set", track: t.name, bus }),
      }));
    });
    if (song.video) {
      const v = song.video;
      wrap.append(this._strip({
        name: "🎬 影片原聲", color: "#7fb6e0", sub: "只在面板預覽", db: v.volume_db || 0, meter: "video", cls: "bus",
        muted: v.mute, solo: st.ui.solo.video,
        onMute: () => h.edit({ op: "video.set", mute: !v.mute }),
        onSolo: () => h.solo("video"),
        live: (x) => h.live("video", "video", x),
        setDb: (db) => h.edit({ op: "video.set", volume_db: db }),
      }));
    }
    if (song.dialogue && song.dialogue.lines.length) {
      const d = song.dialogue.duck || {};
      wrap.append(this._strip({
        name: "🎙 台詞", color: "#c9a7ff", sub: `講話時配樂壓低 ${d.amount_db ?? -10} dB`, meter: "dialogue", cls: "bus",
        muted: st.ui.mute.dialogue, solo: st.ui.solo.dialogue,
        onMute: () => h.uiMute("dialogue"), onSolo: () => h.solo("dialogue"),
      }));
    }
    for (const b of busNames) {
      const cfg = (song.buses || {})[b] || {};
      wrap.append(this._strip({
        name: `匯流排 ${b}`, color: "#9a9", sub: "只預覽音量，效果要渲染", db: cfg.volume_db || 0, fx: cfg.fx || [], cls: "bus",
        live: (x) => h.live("bus", b, x),
        setDb: (db) => h.edit({ op: "bus.set", bus: b, volume_db: db }),
        setFx: (fx) => h.edit({ op: "bus.set", bus: b, fx }),
      }));
    }
    const main = (song.buses || {}).main || {};
    wrap.append(this._strip({
      name: "配樂總線", color: "#e2a72b", sub: "buses.main", db: main.volume_db || 0, fx: main.fx || [], cls: "bus",
      live: (x) => h.live("bus", "main", x),
      setDb: (db) => h.edit({ op: "bus.set", bus: "main", volume_db: db }),
      setFx: (fx) => h.edit({ op: "bus.set", bus: "main", fx }),
    }));
    wrap.append(this._master(song));
    this.root.replaceChildren(wrap);
  }

  /** 每一格讀 Engine 的電平（播放停下來就慢慢掉回去）；stem：軌名 → 分軌名（改過名的軌） */
  tick(engine, stem = (n) => n) {
    for (const m of this.meters) {
      const v = m.name === "@master" ? engine.meterMaster() : engine.meter(stem(m.name) || "");
      for (const [side, peak] of [[0, v.peakL], [1, v.peakR]]) {
        const f = meterFrac(peak);
        m.lv[side] = f > m.lv[side] ? f : m.lv[side] * 0.92;
        m.hold[side] = f > m.hold[side] ? f : Math.max(0, m.hold[side] - 0.004);
        m.bars[side].style.height = m.lv[side] * 100 + "%";
        m.caps[side].style.bottom = m.hold[side] * 100 + "%";
        m.caps[side].hidden = m.hold[side] <= 0.01;
      }
    }
  }

  _meters(name) {
    const fm = el("div", "mx-fm");
    const bars = [], caps = [];
    for (let i = 0; i < 2; i++) {
      const mt = el("div", "mx-meter");
      mt.title = i ? "右聲道" : "左聲道";
      const bar = el("i"), cap = el("b");
      cap.hidden = true;
      mt.append(bar, cap);
      fm.append(mt);
      bars.push(bar);
      caps.push(cap);
    }
    if (name) this.meters.push({ name, bars, caps, lv: [0, 0], hold: [0, 0] });
    return fm;
  }

  _strip(o) {
    const s = el("div", "mx-strip" + (o.cls ? " " + o.cls : ""));
    s.dataset.name = o.name;
    if (o.setPan) {
      const knob = el("div", "mx-knob");
      const pt = el("div", "db", fmtPan(o.pan));
      const show = (p) => { knob.style.setProperty("--r", p * 135 + "deg"); pt.textContent = fmtPan(p); };
      show(o.pan);
      knob.title = "聲像：上下拖（往上＝右）；點兩下回中間";
      knob.onmousedown = (e) => {
        if (e.button !== 0) return;
        let pan = o.pan;
        drag(e, {
          move: (dx, dy) => { pan = Math.round(Math.max(-1, Math.min(1, o.pan - dy / 100)) * 100) / 100; show(pan); o.live({ volumeDb: o.db, pan }); },
          end: () => { if (pan !== o.pan) o.setPan(pan); },
        });
      };
      knob.ondblclick = () => { if (o.pan !== 0) { o.live({ volumeDb: o.db, pan: 0 }); o.setPan(0); } };
      s.append(knob, pt);
    }
    if (o.setFx) {
      o.fx.forEach((fx, i) => {
        const n = fxName(fx);
        const b = el("button", "mx-fx", FX[n] ? FX[n].label : n);
        b.type = "button";
        b.title = `${n} ${JSON.stringify(fxParams(fx))}（點一下改參數）`;
        b.onclick = async () => {
          const r = await editFx(fx);
          if (!r) return;
          const list = o.fx.slice();
          if (r === "delete") list.splice(i, 1);
          else if (r === "up") { if (i === 0) return; [list[i - 1], list[i]] = [list[i], list[i - 1]]; }
          else list[i] = r;
          o.setFx(list);
        };
        s.append(b);
      });
      const add = el("button", "mx-fx add", "＋效果");
      add.type = "button";
      add.onclick = async () => {
        const f = await pickFx();
        if (!f) return;
        const r = await editFx(f, { isNew: true });
        if (r && typeof r === "object") o.setFx([...o.fx, r]);
      };
      s.append(add);
    }
    if (o.sidechain) {
      const sc = el("div", "mx-fx sc", `側鏈←${o.sidechain.trigger}`);
      sc.title = `每個 ${o.sidechain.trigger} 的音符把這軌壓低 ${o.sidechain.depth_db ?? -9} dB（在譜檔的 sidechain 改）`;
      s.append(sc);
    }
    const fm = this._meters(o.meter);
    let dbEl = null;
    if (o.setDb) {
      const fader = el("div", "mx-fader");
      const thumb = el("b");
      fader.append(thumb);
      const show = (db) => { thumb.style.top = (1 - dbToFader(db)) * 100 + "%"; if (dbEl) dbEl.textContent = fmtDb(db); };
      fader.title = "音量：上下拖；點兩下回 0 dB";
      fader.onmousedown = (e) => {
        if (e.button !== 0) return;
        const hgt = fader.getBoundingClientRect().height || 1;
        const f0 = dbToFader(o.db);
        let db = o.db;
        drag(e, {
          move: (dx, dy) => { db = Math.round(faderToDb(Math.max(0, Math.min(1, f0 - dy / hgt))) * 10) / 10; show(db); o.live({ volumeDb: db, pan: o.pan ?? 0 }); },
          end: () => { if (db !== o.db) o.setDb(db); },
        });
      };
      fader.ondblclick = () => { if (o.db !== 0) { o.live({ volumeDb: 0, pan: o.pan ?? 0 }); o.setDb(0); } };
      fm.append(fader);
      dbEl = el("div", "db", fmtDb(o.db));
      dbEl.title = "點一下輸入數字";
      dbEl.onclick = async () => {
        const r = await ask(`${o.name} 音量`, [{ key: "db", label: "dB（-60～+6）", type: "number", value: o.db, min: -60, max: 6, step: 0.1 }]);
        if (r && Number.isFinite(r.values.db)) { const db = Math.max(-60, Math.min(6, r.values.db)); o.live({ volumeDb: db, pan: o.pan ?? 0 }); o.setDb(db); }
      };
      show(o.db);
    }
    s.append(fm);
    if (dbEl) s.append(dbEl);
    if (o.onMute) {
      const ms = el("div", "mx-ms");
      const m = el("button", o.muted ? "m-on" : "", "M");
      m.type = "button";
      m.title = "靜音";
      m.onclick = o.onMute;
      const so = el("button", o.solo ? "s-on" : "", "S");
      so.type = "button";
      so.title = "獨奏（只在面板預覽）";
      so.onclick = o.onSolo;
      ms.append(m, so);
      s.append(ms);
    }
    if (o.setBus) {
      const sel = el("select", "mx-bus");
      sel.title = "送到哪個匯流排";
      for (const b of o.buses) { const op = el("option", "", b === "main" ? "→ 總線" : "→ " + b); op.value = b; sel.append(op); }
      sel.value = o.bus;
      sel.onchange = () => o.setBus(sel.value);
      s.append(sel);
    }
    const nm = el("div", "nm");
    const dot = el("span", "", "● ");
    dot.style.color = o.color;
    nm.append(dot, o.name);
    if (o.stale) nm.append(el("span", "stale", " 待渲染"));
    nm.title = o.sub ? `${o.name}\n${o.sub}` : o.name;
    s.append(nm);
    if (o.sub) s.append(el("div", "sub", o.sub));
    return s;
  }

  _master(song) {
    const h = this.h;
    const s = el("div", "mx-strip master");
    const lufs = el("button", "mx-fx", `${song.master.lufs} LUFS`);
    lufs.type = "button";
    lufs.title = "母帶目標響度（點一下改）";
    lufs.onclick = async () => {
      const r = await ask("母帶響度", [{ key: "v", label: "LUFS（串流平台 -14；EDM 可以到 -11）", type: "number", value: song.master.lufs, step: 0.5, max: -5, min: -30 }]);
      if (r && Number.isFinite(r.values.v) && r.values.v !== song.master.lufs) h.edit({ op: "master.set", lufs: r.values.v });
    };
    const ceil = el("button", "mx-fx", `${song.master.ceiling_dbtp} dBTP`);
    ceil.type = "button";
    ceil.title = "真峰值上限（點一下改）";
    ceil.onclick = async () => {
      const r = await ask("真峰值上限", [{ key: "v", label: "dBTP（預設 -1.0）", type: "number", value: song.master.ceiling_dbtp, step: 0.1, max: 0, min: -6 }]);
      if (r && Number.isFinite(r.values.v) && r.values.v !== song.master.ceiling_dbtp) h.edit({ op: "master.set", ceiling_dbtp: r.values.v });
    };
    s.append(lufs, ceil, this._meters("@master"));
    const nm = el("div", "nm", "母帶");
    nm.title = "預覽的總輸出（聽正式母帶時量的是 mix.wav）";
    s.append(nm, el("div", "sub", "改了要渲染"));
    return s;
  }
}

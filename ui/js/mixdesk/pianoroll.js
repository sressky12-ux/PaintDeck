// 鋼琴捲軸分頁：編一段的音符。左邊琴鍵（點了試聽；音域外的畫灰；鼓軌改成鼓名）、中間格子、下面力度條。
// 每次放開滑鼠推一筆 part.notes 進修改堆疊；音符陣列的運算都在 core.js（moveNotes、resizeNotes、marquee…）。
import { GRIDS, moveNotes, resizeNotes, marquee, addNote, deleteNotes } from "./core.js";
import { el, drag } from "./ui.js";

const NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
export const pitchName = (p) => NAMES[p % 12] + (Math.floor(p / 12) - 1);
const isBlack = (p) => [1, 3, 6, 8, 10].includes(p % 12);
const ROW = 12; // 每列幾 px
const store = {
  get: (k, d) => { try { return localStorage.getItem(k) ?? d; } catch { return d; } },
  set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* 無痕 */ } },
};

export class PianoRoll {
  /** h：{edit(op), audition(instrumentKey, notesText), convertMidi(trackName), playPart()} */
  constructor(root, h) {
    this.root = root;
    this.h = h;
    this.grid = Number(store.get("mixdesk.grid", "0.25"));
    this.vel = 90;
    this.ppb = 48;
    this.allDrums = false;
    this.sel = new Set();
    this.ti = null;
    this.partIdx = null;
    root.tabIndex = 0;
    root.addEventListener("keydown", (e) => this._key(e));
    root.addEventListener("wheel", (e) => {
      if (!e.ctrlKey || this.ti == null) return;
      e.preventDefault();
      this.ppb = Math.min(240, Math.max(8, this.ppb * (e.deltaY < 0 ? 1.25 : 0.8)));
      this.render();
    }, { passive: false });
  }

  get track() { return this.song && this.song.tracks[this.ti]; }
  get part() { return this.track && this.track.parts.find((p) => p.index === this.partIdx); }

  /** 開一段（換段落時清掉選取、捲到音符那裡） */
  open(song, ti, partIdx) {
    const same = this.ti === ti && this.partIdx === partIdx;
    this.song = song;
    this.ti = ti;
    this.partIdx = partIdx;
    if (!same) { this.sel = new Set(); this.scrolled = false; }
    this.render();
  }

  /** 修改之後重畫（同一段：選取與捲動位置留著） */
  refresh(song) {
    this.song = song;
    if (this.ti != null && !this.part) { this.ti = null; this.partIdx = null; }
    this.render();
  }

  _rows() {
    const t = this.track, p = this.part;
    if (t.instrument.is_drums) {
      const used = new Set(t.parts.flatMap((x) => x.notes.map((n) => n.pitch)));
      const all = this.allDrums ? Object.keys(this.song.drum_names || {}).map(Number) : [];
      return [...new Set([...used, ...all])].sort((a, b) => b - a);
    }
    const ps = p.notes.map((n) => n.pitch);
    const hi = Math.max(108, ...ps), lo = Math.min(21, ...ps);
    return Array.from({ length: hi - lo + 1 }, (_, i) => hi - i);
  }

  render() {
    const root = this.root;
    if (this.ti == null || !this.part) {
      root.replaceChildren(el("div", "pr-hint", "在時間軸上點兩下一段（或右鍵 →「開鋼琴捲軸」）就會在這裡打開。"));
      return;
    }
    const t = this.track, p = this.part, song = this.song;
    if (p.midi) {
      const box = el("div", "pr-hint", `「${t.name}」是 MIDI 匯入的軌，音符不在譜檔裡。要先轉成譜（寫進譜檔的 notes）才能在這裡改。`);
      const b = el("button", "md-btn acc", "轉成譜（會先寫回譜檔）");
      b.type = "button";
      b.onclick = () => this.h.convertMidi(t.name);
      box.append(el("br"), b);
      root.replaceChildren(box);
      return;
    }
    const drums = t.instrument.is_drums, bpb = song.beats_per_bar || 4, ppb = this.ppb;
    const rows = this._rows();
    this.rows = rows;
    const rowOf = new Map(rows.map((x, i) => [x, i]));
    this.rowOf = rowOf;
    const range = t.instrument.range;
    const out = (x) => !drums && range && (x < range[0] || x > range[1]);
    const len = Math.max(p.length_beats, ...p.notes.map((n) => n.beat + n.beats));
    const width = (Math.ceil(len / bpb) + 2) * bpb * ppb;
    const height = rows.length * ROW;
    const keepScroll = this.gridEl ? [this.gridEl.scrollLeft, this.gridEl.scrollTop] : null;

    const wrap = el("div", "pr");
    if (drums) wrap.style.gridTemplateColumns = "96px 1fr"; // 鼓名比音名長
    // 工具列
    const tools = el("div", "pr-tools");
    tools.append(el("b", "", `${t.name}・第 ${p.index + 1} 段`), el("span", "pr-hint", `第 ${p.at} 小節起，${p.length_beats} 拍${(p.repeat || 1) > 1 ? `，重複 ${p.repeat} 次` : ""}`));
    const gsel = el("select");
    for (const [g, name] of GRIDS) { const o = el("option", "", name); o.value = String(g); gsel.append(o); }
    gsel.value = String(this.grid);
    gsel.title = "吸附格線";
    gsel.onchange = () => { this.grid = Number(gsel.value); store.set("mixdesk.grid", gsel.value); };
    const vin = el("input");
    vin.type = "number"; vin.min = 1; vin.max = 127; vin.value = this.vel; vin.style.width = "56px";
    vin.title = "新增音符的力度（1～127）";
    vin.onchange = () => { this.vel = Math.max(1, Math.min(127, Math.round(Number(vin.value) || 90))); };
    tools.append(el("span", "", "吸附"), gsel, el("span", "", "力度"), vin);
    const btn = (label, title, fn) => { const b = el("button", "md-btn sm", label); b.type = "button"; b.title = title; b.onclick = fn; tools.append(b); return b; };
    btn("全選", "Ctrl+A", () => { this.sel = new Set(p.notes.map((_, i) => i)); this.render(); });
    btn("刪除", "Delete", () => this._delete());
    if (!drums) { btn("↑", "移高半音（Shift：八度）", (e) => this._nudge(0, e.shiftKey ? 12 : 1)); btn("↓", "移低半音（Shift：八度）", (e) => this._nudge(0, e.shiftKey ? -12 : -1)); }
    btn("▶ 從這段播", "從這段的開頭播放", () => this.h.playPart(p));
    if (drums) {
      const all = btn(this.allDrums ? "只列用到的鼓" : "全部鼓", "鼓軌的琴鍵只列這軌用到的鼓；按這個列出全部鼓名", () => { this.allDrums = !this.allDrums; this.scrolled = false; this.render(); });
      all.classList.toggle("on", this.allDrums);
    }
    if (p.link && p.linked_with.length) {
      const lk = el("span", "pr-link", `🔗 跟 ${p.linked_with.map(([ti2, pi2]) => `${song.tracks[ti2] ? song.tracks[ti2].name : "?"} 第 ${pi2 + 1} 段`).join("、")} 連動：改這段兩邊一起變`);
      tools.append(lk);
      btn("拆開，只改這段", "把這段變成自己的一份音符", () => this.h.edit({ op: "part.unlink", track: t.name, part: p.index }));
    }
    tools.append(el("span", "pr-hint", "點兩下空白：新增　拖：移動　拉右邊：長度　框選　點兩下音符：刪除　Ctrl+滾輪：縮放"));
    wrap.append(tools);

    // 琴鍵
    const keys = el("div", "pr-keys");
    const keysIn = el("div");
    keysIn.style.cssText = `position:relative;height:${height}px`;
    rows.forEach((x, i) => {
      const k = el("div", "pr-key" + (!drums && isBlack(x) ? " blk" : "") + (out(x) ? " out" : ""),
        drums ? (song.drum_names && song.drum_names[x]) || String(x) : (x % 12 === 0 || drums ? pitchName(x) : ""));
      k.style.cssText = `top:${i * ROW}px;height:${ROW}px`;
      k.title = drums ? `${(song.drum_names && song.drum_names[x]) || x}（${x}）` : `${pitchName(x)}（${x}）${out(x) ? "：這個樂器的音域外" : ""}`;
      k.onmousedown = () => this._audition(x);
      keysIn.append(k);
    });
    keys.append(keysIn);
    if (drums) keys.classList.add("drums");

    // 格子
    const grid = el("div", "pr-grid");
    const cv = el("div", "pr-canvas");
    cv.style.cssText = `width:${width}px;height:${height}px`;
    rows.forEach((x, i) => {
      const r = el("div", "pr-row" + (!drums && isBlack(x) ? " blk" : "") + (out(x) ? " out" : ""));
      r.style.cssText = `top:${i * ROW}px;height:${ROW}px`;
      cv.append(r);
    });
    for (let b = 0; b * ppb <= width; b++) {
      const ln = el("div", "pr-beat" + (b % bpb === 0 ? " bar" : ""));
      ln.style.left = b * ppb + "px";
      cv.append(ln);
    }
    const end = el("div", "pr-end");
    end.style.left = p.length_beats * ppb + "px";
    end.title = "這段的長度";
    cv.append(end);
    this.noteEls = p.notes.map((n, i) => {
      const d = el("div", "pr-note" + (this.sel.has(i) ? " sel" : ""));
      this._place(d, n);
      if (n.beats * ppb > 22) d.textContent = drums ? (song.drum_names && song.drum_names[n.pitch]) || "" : pitchName(n.pitch);
      d.title = `${drums ? (song.drum_names && song.drum_names[n.pitch]) || n.pitch : pitchName(n.pitch)}　第 ${n.beat} 拍起 ${n.beats} 拍　力度 ${n.vel}`;
      const rs = el("i", "rs");
      d.append(rs);
      d.onmousedown = (e) => this._noteDown(e, i, e.target === rs);
      d.ondblclick = (e) => { e.stopPropagation(); this.sel = new Set([i]); this._delete(); };
      cv.append(d);
      return d;
    });
    const mq = el("div", "pr-mq");
    mq.hidden = true;
    cv.append(mq);
    cv.onmousedown = (e) => { if (e.target.closest(".pr-note")) return; this.root.focus(); this._marquee(e, cv, mq); };
    cv.ondblclick = (e) => { if (e.target.closest(".pr-note")) return; this._add(e, cv); };
    grid.append(cv);
    grid.onscroll = () => { keysIn.style.transform = `translateY(${-grid.scrollTop}px)`; vel.scrollLeft = grid.scrollLeft; };

    // 力度條
    const vel = el("div", "pr-vel");
    const velIn = el("div");
    velIn.style.cssText = `position:relative;height:100%;width:${width}px`;
    p.notes.forEach((n, i) => {
      const b = el("b", this.sel.has(i) ? "sel" : "");
      b.style.left = n.beat * ppb + "px";
      b.style.height = (n.vel / 127) * 100 + "%";
      b.title = `力度 ${n.vel}（上下拖）`;
      b.onmousedown = (e) => {
        if (e.button !== 0) return;
        const h0 = vel.getBoundingClientRect().height || 40;
        const targets = this.sel.has(i) ? [...this.sel] : [i];
        const v0 = targets.map((j) => p.notes[j].vel);
        let vs = v0;
        drag(e, {
          move: (dx, dy) => {
            vs = v0.map((v) => Math.max(1, Math.min(127, Math.round(v - (dy / h0) * 127))));
            targets.forEach((j, k) => { velIn.children[j].style.height = (vs[k] / 127) * 100 + "%"; });
          },
          end: () => {
            const notes = p.notes.map((nn, j) => ({ ...nn, vel: targets.includes(j) ? vs[targets.indexOf(j)] : nn.vel }));
            this._commit(notes);
          },
        });
      };
      velIn.append(b);
    });
    vel.append(velIn);

    wrap.append(keys, grid, el("div"), vel);
    root.replaceChildren(wrap);
    this.gridEl = grid;
    if (keepScroll && this.scrolled) [grid.scrollLeft, grid.scrollTop] = keepScroll;
    else {
      // 第一次打開：捲到音符的音高附近
      const ps = p.notes.map((n) => rowOf.get(n.pitch)).filter((x) => x != null);
      const mid = ps.length ? (Math.min(...ps) + Math.max(...ps)) / 2 : rowOf.get(60) ?? rows.length / 2;
      grid.scrollTop = Math.max(0, mid * ROW - grid.clientHeight / 2);
      this.scrolled = true;
    }
    grid.onscroll();
  }

  _place(d, n) {
    const r = this.rowOf.get(n.pitch);
    d.style.left = n.beat * this.ppb + "px";
    d.style.top = (r ?? 0) * ROW + 1 + "px";
    d.style.height = ROW - 2 + "px";
    d.style.width = Math.max(3, n.beats * this.ppb - 1) + "px";
  }

  _commit(notes) {
    this.h.edit({ op: "part.notes", track: this.track.name, part: this.partIdx, notes });
  }

  _audition(pitch) {
    const t = this.track;
    if (!t) return;
    const drums = t.instrument.is_drums;
    const name = drums ? this.song.drum_names && this.song.drum_names[pitch] : pitchName(pitch);
    if (name) this.h.audition(t.instrument.key, `${name}/1`);
  }

  /** 垂直拖幾列：鼓軌的列不是連續音高，照列表換 */
  _shiftRows(notes, sel, dRows) {
    if (!dRows) return notes;
    if (!this.track.instrument.is_drums) return notes.map((n, i) => (sel.has(i) ? { ...n, pitch: Math.max(0, Math.min(127, n.pitch + dRows)) } : n));
    return notes.map((n, i) => {
      if (!sel.has(i)) return n;
      const r = Math.max(0, Math.min(this.rows.length - 1, this.rowOf.get(n.pitch) - dRows));
      return { ...n, pitch: this.rows[r] };
    });
  }

  _noteDown(e, i, resize) {
    if (e.button !== 0) return;
    e.stopPropagation();
    this.root.focus();
    const p = this.part;
    if (e.shiftKey) { if (this.sel.has(i)) this.sel.delete(i); else this.sel.add(i); }
    else if (!this.sel.has(i)) this.sel = new Set([i]);
    this.noteEls.forEach((d, j) => d.classList.toggle("sel", this.sel.has(j)));
    let next = null;
    drag(e, {
      move: (dx, dy) => {
        if (resize) next = resizeNotes(p.notes, this.sel, dx / this.ppb, this.grid);
        else next = this._shiftRows(moveNotes(p.notes, this.sel, i, dx / this.ppb, 0, this.grid), this.sel, -Math.round(dy / ROW));
        next.forEach((n, j) => { if (this.sel.has(j)) this._place(this.noteEls[j], n); });
      },
      end: () => {
        if (!next) return;
        const moved = next.some((n, j) => n.beat !== p.notes[j].beat || n.pitch !== p.notes[j].pitch || n.beats !== p.notes[j].beats);
        if (moved) this._commit(next);
        if (!resize && next[i].pitch !== p.notes[i].pitch) this._audition(next[i].pitch);
      },
      click: () => this._audition(p.notes[i].pitch),
    });
  }

  _marquee(e, cv, mq) {
    if (e.button !== 0) return;
    const rect = cv.getBoundingClientRect();
    const at = (ev) => [ev.clientX - rect.left, ev.clientY - rect.top];
    const [x0, y0] = at(e);
    const add = e.shiftKey;
    const base = new Set(this.sel);
    drag(e, {
      move: (dx, dy, ev) => {
        const [x1, y1] = at(ev);
        Object.assign(mq.style, { left: Math.min(x0, x1) + "px", top: Math.min(y0, y1) + "px", width: Math.abs(x1 - x0) + "px", height: Math.abs(y1 - y0) + "px" });
        mq.hidden = false;
        const r0 = Math.floor(Math.min(y0, y1) / ROW), r1 = Math.floor(Math.max(y0, y1) / ROW);
        const pitches = this.rows.slice(Math.max(0, r0), r1 + 1);
        const hit = marquee(this.part.notes, x0 / this.ppb, x1 / this.ppb, -1, 128);
        this.sel = new Set([...(add ? base : []), ...[...hit].filter((j) => pitches.includes(this.part.notes[j].pitch))]);
        this.noteEls.forEach((d, j) => d.classList.toggle("sel", this.sel.has(j)));
      },
      end: () => { mq.hidden = true; this.render(); },
      click: () => { if (!add && this.sel.size) { this.sel = new Set(); this.render(); } },
    });
  }

  _add(e, cv) {
    const rect = cv.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    const pitch = this.rows[Math.floor(y / ROW)];
    if (pitch == null) return;
    const g = this.grid;
    const beat = g ? Math.floor(x / this.ppb / g) * g : x / this.ppb;
    const len = this.track.instrument.is_drums ? Math.max(g, 0.25) || 0.25 : g || 0.25;
    const notes = addNote(this.part.notes, beat, pitch, len, this.vel);
    const idx = notes.findIndex((n) => n.pitch === pitch && Math.abs(n.beat - beat) < 1e-9);
    this.sel = new Set([idx]);
    this._audition(pitch);
    this._commit(notes);
  }

  _delete() {
    if (!this.sel.size || !this.part) return;
    const notes = deleteNotes(this.part.notes, this.sel);
    this.sel = new Set();
    this._commit(notes);
  }

  _nudge(dBeat, dRows) {
    if (!this.sel.size) return;
    const p = this.part;
    const first = [...this.sel][0];
    let notes = dBeat ? moveNotes(p.notes, this.sel, first, dBeat, 0, 0) : p.notes;
    notes = this._shiftRows(notes, this.sel, dRows);
    this._commit(notes);
    if (dRows) this._audition(notes[first].pitch);
  }

  _key(e) {
    if (this.ti == null || !this.part || /INPUT|SELECT|TEXTAREA/.test(e.target.tagName)) return;
    const k = e.key;
    if (k === "Delete" || k === "Backspace") { e.preventDefault(); this._delete(); }
    else if (e.ctrlKey && k.toLowerCase() === "a") { e.preventDefault(); this.sel = new Set(this.part.notes.map((_, i) => i)); this.render(); }
    else if (k === "Escape") { this.sel = new Set(); this.render(); }
    else if (k === "ArrowUp" || k === "ArrowDown") { e.preventDefault(); this._nudge(0, (k === "ArrowUp" ? 1 : -1) * (e.shiftKey && !this.track.instrument.is_drums ? 12 : 1)); }
    else if (k === "ArrowLeft" || k === "ArrowRight") { e.preventDefault(); this._nudge((k === "ArrowRight" ? 1 : -1) * (this.grid || 0.25), 0); }
    else return;
    e.stopPropagation();
  }
}

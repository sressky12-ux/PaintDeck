// 編曲時間軸（上半部）：尺規（點一下跳過去、拖一段設循環）、影片軌、每軌的段落方塊＋迷你音符、台詞泡泡、播放線。
// 改東西一律丟回 h.edit(op)（main.js 推進修改堆疊、重畫）；播放中每一格 tick(sec) 讓正在響的音符與台詞亮起來。
import { expandPart, clipBox, pitchRange, lineSpan, secToBeat, beatToSec, snap } from "./core.js";
import { el, drag, menu, ask, confirmBox } from "./ui.js";

export const HEAD_W = 150; // 跟 mixdesk.css 的 --hd-w 一樣
const PALETTE = ["#ff5a4a", "#ffb547", "#e2a72b", "#8dffb0", "#7fd4ff", "#e6742c", "#c9a7ff", "#ff8fc8", "#9fe870", "#5fd0c8"];
export const colorOf = (i) => PALETTE[i % PALETTE.length];
const r2 = (x) => Math.round(x * 100) / 100;

export class Timeline {
  /** h：{edit(op), select(ti, part), openRoll(ti, part), seek(sec), loop([a, b]), solo(name), uiMute(name), zoom(factor), dropInstrument(key, trackName|null)} */
  constructor(root, h) {
    this.root = root;
    this.h = h;
    this.notes = [];
    this.talks = [];
    root.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      h.zoom(e.deltaY < 0 ? 1.25 : 0.8);
    }, { passive: false });
    // 從樂器庫拖過來：放在軌名上＝換樂器，放在空白處＝新增一軌
    root.addEventListener("dragover", (e) => { if (e.dataTransfer.types.includes("text/x-mixdesk-instrument")) e.preventDefault(); });
    root.addEventListener("drop", (e) => {
      const key = e.dataTransfer.getData("text/x-mixdesk-instrument");
      if (!key) return;
      e.preventDefault();
      const hd = e.target.closest(".tl-hd[data-track]");
      h.dropInstrument(key, hd ? hd.dataset.track : null);
    });
  }

  render(song, st) {
    this.song = song;
    this.ppb = st.ppb;
    const bpb = song.beats_per_bar || 4, ppb = st.ppb;
    const lines = song.dialogue ? song.dialogue.lines : [];
    const endBeat = Math.max(
      song.length_beats + secToBeat(song.tail_sec || 0, song.tempo),
      ...lines.map((l) => secToBeat(lineSpan(l)[1], song.tempo)),
      song.video ? secToBeat((song.video.offset_sec || 0) + (st.videoDur || 0), song.tempo) : 0,
    );
    this.total = Math.ceil(endBeat / bpb + 1) * bpb;
    this.notes = [];
    this.talks = [];
    this.clipEls = [];
    this.laneEls = [];
    const keep = [this.root.scrollLeft, this.root.scrollTop];
    const inner = el("div", "tl-inner");
    inner.style.width = HEAD_W + this.total * ppb + "px";
    // 軌道不多時撐高一點（34～72px），迷你音符的上下範圍跟著變
    const lanes = song.tracks.length + 1;
    this.laneH = Math.max(34, Math.min(72, Math.floor((this.root.clientHeight - 20 - (song.video ? 26 : 0)) / lanes)));
    inner.style.setProperty("--lane-h", this.laneH + "px");
    inner.append(this._ruler(bpb, st));
    if (song.video) inner.append(this._videoLane(song, st));
    song.tracks.forEach((t, ti) => inner.append(this._lane(song, t, ti, st)));
    inner.append(this._talkLane(song, st));
    this.ph = el("div", "tl-ph");
    inner.append(this.ph);
    this.root.replaceChildren(inner);
    [this.root.scrollLeft, this.root.scrollTop] = keep;
    this.tick(st.sec || 0, false);
  }

  /** 換選取只改 class（整個重畫的話，點兩下的第二下會點到新元素，雙擊就斷了） */
  mark(sel) {
    this.laneEls.forEach((l, ti) => l.classList.toggle("sel", !!sel && sel.track === ti));
    for (const c of this.clipEls) c.el.classList.toggle("picked", !!sel && sel.track === c.ti && sel.part === c.part);
  }

  /** 每一格：播放線、正在響的音符、正在講的台詞；播放中播放線跑出畫面就跟著捲 */
  tick(sec, playing) {
    if (!this.song || !this.ph) return;
    const beat = secToBeat(sec, this.song.tempo);
    const x = beat * this.ppb;
    this.ph.style.left = HEAD_W + x + "px";
    for (const n of this.notes) {
      const on = beat >= n.s && beat < n.e;
      if (on !== n.on) { n.on = on; n.el.classList.toggle("hot", on); }
    }
    for (const t of this.talks) {
      const on = sec >= t.s && sec < t.e;
      if (on !== t.on) { t.on = on; t.el.classList.toggle("hot", on); }
    }
    if (playing) {
      const view = this.root.clientWidth - HEAD_W;
      const left = this.root.scrollLeft;
      if (x < left || x > left + view - 30) this.root.scrollLeft = Math.max(0, x - 40);
    }
  }

  _ruler(bpb, st) {
    const ppb = this.ppb, tempo = this.song.tempo;
    const r = el("div", "tl-ruler");
    r.title = "點一下：跳到這裡；拖一段：設循環範圍";
    let step = 1;
    while (step * bpb * ppb < 28) step *= 2;
    for (let b = 0; b * bpb < this.total; b += step) {
      const s = el("span", "", String(b + 1));
      s.style.width = step * bpb * ppb + "px";
      r.append(s);
    }
    const loop = el("div", "tl-loop");
    const showLoop = (a, b) => {
      loop.hidden = !(b > a);
      loop.style.left = secToBeat(a, tempo) * ppb + "px";
      loop.style.width = secToBeat(b - a, tempo) * ppb + "px";
    };
    if (st.loopRange) showLoop(...st.loopRange); else loop.hidden = true;
    r.append(loop);
    r.onmousedown = (e) => {
      if (e.button !== 0) return;
      const left = r.getBoundingClientRect().left;
      const at = (cx) => beatToSec(Math.max(0, snap((cx - left) / ppb, 1)), tempo);
      const a = at(e.clientX);
      drag(e, {
        move: (dx, dy, ev) => { const b = at(ev.clientX); showLoop(Math.min(a, b), Math.max(a, b)); },
        end: (dx, dy, ev) => { const b = at(ev.clientX); if (b !== a) this.h.loop([Math.min(a, b), Math.max(a, b)]); },
        click: (ev) => this.h.seek(beatToSec(Math.max(0, (ev.clientX - left) / ppb), tempo)),
      });
    };
    return r;
  }

  _head(label, color, o) {
    const hd = el("div", "tl-hd");
    if (o.track) hd.dataset.track = o.track;
    const sw = el("span", "sw");
    sw.style.background = color;
    const nm = el("span", "nm", label);
    nm.title = o.title || label;
    hd.append(sw, nm);
    if (o.stale) { const s = el("span", "stale", "待渲染"); s.title = "改了音符、樂器或效果：預覽聽到的是舊的，要按「套用＋重新渲染」"; hd.append(s); }
    const m = el("span", "ms" + (o.muted ? " m-on" : ""), "M");
    m.title = o.muteTitle || "靜音";
    m.onclick = (e) => { e.stopPropagation(); o.onMute(); };
    const s = el("span", "ms" + (o.solo ? " s-on" : ""), "S");
    s.title = "獨奏（只在面板預覽，不寫進譜檔）";
    s.onclick = (e) => { e.stopPropagation(); o.onSolo(); };
    hd.append(m, s);
    if (o.onMenu) hd.oncontextmenu = (e) => { e.preventDefault(); o.onMenu(e); };
    if (o.onClick) hd.onclick = o.onClick;
    return hd;
  }

  _lane(song, t, ti, st) {
    const h = this.h, ppb = this.ppb, bpb = song.beats_per_bar || 4;
    const lane = el("div", "tl-lane" + (st.sel && st.sel.track === ti ? " sel" : ""));
    this.laneEls[ti] = lane;
    const col = colorOf(ti);
    lane.append(this._head(t.name, col, {
      track: t.name,
      title: `${t.name}｜${t.instrument.name || t.instrument.key}${t.midi ? "｜MIDI 匯入" : ""}`,
      muted: t.mute, solo: st.ui.solo[t.name], stale: st.stale.has(t.name),
      muteTitle: "靜音（寫進譜檔：渲染時這軌不進混音）",
      onMute: () => h.edit({ op: "track.set", track: t.name, mute: !t.mute }),
      onSolo: () => h.solo(t.name),
      onClick: () => h.select(ti, t.parts[0] ? t.parts[0].index : null),
      onMenu: (e) => menu(e.clientX, e.clientY, [
        { label: "改名…", fn: async () => {
          const r = await ask("改軌名", [{ key: "name", label: "新名字", value: t.name }]);
          if (r && r.values.name.trim() && r.values.name.trim() !== t.name) h.edit({ op: "track.set", track: t.name, name: r.values.name.trim() });
        } },
        { label: "在播放線的位置新增一段", fn: () => h.addPart(ti, null) },
        { label: "刪除這軌", fn: async () => {
          if (await confirmBox(`刪除「${t.name}」？`, "按套用之前都可以復原（Ctrl+Z）。", "刪除")) h.edit({ op: "track.remove", track: t.name });
        } },
      ]),
    }));
    const row = el("div", "tl-row");
    row.style.width = this.total * ppb + "px";
    const [lo, hi] = pitchRange(t);
    for (const p of t.parts) row.append(this._clip(song, t, ti, p, lo, hi, col, st));
    row.ondblclick = (e) => {
      if (e.target !== row || t.midi) return;
      const beat = Math.max(0, Math.floor(e.offsetX / ppb / bpb) * bpb);
      h.addPart(ti, beat);
    };
    lane.append(row);
    return lane;
  }

  _clip(song, t, ti, p, lo, hi, col, st) {
    const h = this.h, ppb = this.ppb, bpb = song.beats_per_bar || 4;
    const box = clipBox(p, ppb);
    const picked = st.sel && st.sel.track === ti && st.sel.part === p.index;
    const linked = p.link && p.linked_with.length > 0;
    const c = el("div", "tl-clip" + ((p.repeat || 1) > 1 ? " loop" : "") + (picked ? " picked" : ""));
    this.clipEls.push({ el: c, ti, part: p.index });
    c.style.cssText = `left:${box.left}px;width:${Math.max(3, box.width - 1)}px;border-color:${col};color:${col}`;
    const label = p.midi ? "MIDI" : `第 ${p.index + 1} 段${(p.repeat || 1) > 1 ? ` ×${p.repeat}` : ""}${linked ? " 🔗" : ""}`;
    c.append(el("span", "lbl", label));
    c.title = p.midi ? "MIDI 匯入的軌：點兩下開鋼琴捲軸，第一次編輯會先問要不要轉成譜"
      : `${label}（第 ${p.at} 小節起）${linked ? "\n🔗 跟別段連動：改這段的音符，連動的段落一起變" : ""}\n拖：搬位置　點兩下：開鋼琴捲軸　右鍵：更多`;
    const span = Math.max(1, hi - lo);
    for (const n of expandPart(p)) {
      const d = el("div", "tl-note");
      d.style.cssText = `left:${(n.start - p.start_beat) * ppb}px;width:${Math.max(1, (n.end - n.start) * ppb - 1)}px;`
        + `top:${hi === lo ? (this.laneH - 10) / 2 : 3 + ((hi - n.pitch) / span) * (this.laneH - 17)}px;background:${col}`;
      c.append(d);
      this.notes.push({ el: d, s: n.start, e: n.end, on: false });
    }
    c.onmousedown = (e) => {
      if (e.button !== 0) return;
      let start = p.start_beat;
      drag(e, {
        move: (dx) => {
          if (p.midi) return;
          start = Math.max(0, snap(p.start_beat + dx / ppb, 1));
          c.style.left = start * ppb + "px";
        },
        end: () => { if (!p.midi && start !== p.start_beat) h.edit({ op: "part.set", track: t.name, part: p.index, at: r2(1 + start / bpb) }); },
        click: () => h.select(ti, p.index),
      });
    };
    c.ondblclick = () => h.openRoll(ti, p.index);
    c.oncontextmenu = (e) => {
      e.preventDefault();
      if (p.midi) return menu(e.clientX, e.clientY, [{ label: "開鋼琴捲軸（會先問要不要轉成譜）", fn: () => h.openRoll(ti, p.index) }]);
      menu(e.clientX, e.clientY, [
        { label: "開鋼琴捲軸", fn: () => h.openRoll(ti, p.index) },
        linked && { label: "拆開連動，只改這段", fn: () => h.edit({ op: "part.unlink", track: t.name, part: p.index }) },
        { label: "重複次數…", fn: async () => {
          const r = await ask("重複幾次", [{ key: "n", label: "這段連續播幾輪", type: "number", value: p.repeat || 1, min: 1, step: 1 }]);
          if (r && r.values.n >= 1 && Math.round(r.values.n) !== (p.repeat || 1)) h.edit({ op: "part.set", track: t.name, part: p.index, repeat: Math.round(r.values.n) });
        } },
        { label: "搬到第幾小節…", fn: async () => {
          const r = await ask("搬到第幾小節", [{ key: "at", label: "小節（1 是開頭，可以有小數）", type: "number", value: p.at, min: 1, step: 0.25 }]);
          if (r && r.values.at >= 1 && r.values.at !== p.at) h.edit({ op: "part.set", track: t.name, part: p.index, at: r.values.at });
        } },
        { label: "刪除這段", fn: () => h.edit({ op: "part.remove", track: t.name, part: p.index }) },
      ]);
    };
    return c;
  }

  _videoLane(song, st) {
    const h = this.h, ppb = this.ppb, tempo = song.tempo, v = song.video;
    const lane = el("div", "tl-lane vid");
    lane.append(this._head("🎬 影片", "#7fb6e0", {
      title: `影片：${v.path}`,
      muted: v.mute, solo: st.ui.solo.video,
      muteTitle: "影片原聲靜音（寫進譜檔）",
      onMute: () => h.edit({ op: "video.set", mute: !v.mute }),
      onSolo: () => h.solo("video"),
    }));
    const row = el("div", "tl-row");
    row.style.width = this.total * ppb + "px";
    const bar = el("div", "tl-vid", v.path.split(/[\\/]/).pop());
    const place = (off) => {
      bar.style.left = Math.max(0, secToBeat(off, tempo)) * ppb + "px";
      bar.style.width = Math.max(20, secToBeat(st.videoDur || 10, tempo) * ppb) + "px";
    };
    place(v.offset_sec || 0);
    bar.title = "影片從這裡開始播；左右拖改起點（offset）";
    bar.onmousedown = (e) => {
      if (e.button !== 0) return;
      let off = v.offset_sec || 0;
      drag(e, {
        move: (dx) => { off = Math.max(0, r2((v.offset_sec || 0) + beatToSec(dx / ppb, tempo))); place(off); },
        end: () => { if (off !== (v.offset_sec || 0)) h.edit({ op: "video.set", offset_sec: off }); },
      });
    };
    row.append(bar);
    lane.append(row);
    return lane;
  }

  _talkLane(song, st) {
    const h = this.h, ppb = this.ppb, tempo = song.tempo;
    const lines = song.dialogue ? song.dialogue.lines : [];
    const lane = el("div", "tl-lane");
    lane.append(this._head("🎙 台詞", "#c9a7ff", {
      title: "台詞（點兩下空白處新增一句）",
      muted: st.ui.mute.dialogue, solo: st.ui.solo.dialogue,
      muteTitle: "台詞靜音（只在面板預覽）",
      onMute: () => h.uiMute("dialogue"),
      onSolo: () => h.solo("dialogue"),
    }));
    const row = el("div", "tl-row");
    row.style.width = this.total * ppb + "px";
    row.title = lines.length ? "" : "點兩下新增一句台詞";
    for (const l of lines) {
      const [a, b] = lineSpan(l);
      const d = el("div", "tl-talk", "💬 " + l.text);
      const place = (at) => {
        d.style.left = secToBeat(at, tempo) * ppb + "px";
        d.style.width = Math.max(14, secToBeat(b - a, tempo) * ppb) + "px";
      };
      place(a);
      d.title = `${l.speaker}${l.emotion ? `（${l.emotion}）` : ""}：${l.text}\n${a.toFixed(2)} 秒起${l.end_sec == null ? "（還沒唸過，長度用字數估）" : ""}\n拖：改時間　點兩下：改文字　右鍵：情緒、刪除`;
      d.onmousedown = (e) => {
        if (e.button !== 0) return;
        let at = a;
        drag(e, {
          move: (dx) => { at = Math.max(0, r2(a + beatToSec(dx / ppb, tempo))); place(at); },
          end: () => { if (at !== a) h.edit({ op: "line.set", line: l.index, at_sec: at }); },
          click: () => h.seek(a),
        });
      };
      d.ondblclick = async (e) => {
        e.stopPropagation();
        const r = await ask("改台詞", [{ key: "text", label: "要唸的字", type: "textarea", value: l.text }], { note: "改了文字要重新渲染才會重唸。" });
        if (r && r.values.text.trim() && r.values.text !== l.text) h.edit({ op: "line.set", line: l.index, text: r.values.text.trim() });
      };
      d.oncontextmenu = (e) => {
        e.preventDefault();
        menu(e.clientX, e.clientY, [
          { label: "改情緒…", fn: async () => {
            const r = await ask("情緒", [{ key: "emotion", label: "情緒名稱（要先用 mixdesk voice emotion 建好；空白＝中性）", value: l.emotion || "" }]);
            if (r) h.edit({ op: "line.set", line: l.index, emotion: r.values.emotion.trim() || null });
          } },
          { label: "改時間…", fn: async () => {
            const r = await ask("幾秒開始唸", [{ key: "at", label: "秒", type: "number", value: l.at_sec, min: 0, step: 0.05 }]);
            if (r && r.values.at >= 0) h.edit({ op: "line.set", line: l.index, at_sec: r2(r.values.at) });
          } },
          { label: "刪除這句", fn: () => h.edit({ op: "line.remove", line: l.index }) },
        ]);
      };
      row.append(d);
      this.talks.push({ el: d, s: a, e: b, on: false });
    }
    row.ondblclick = async (e) => {
      if (e.target !== row) return;
      const at = r2(beatToSec(e.offsetX / ppb, tempo));
      const last = lines[lines.length - 1];
      const r = await ask(`在 ${at.toFixed(2)} 秒新增台詞`, [
        { key: "text", label: "要唸的字", type: "textarea", value: "" },
        { key: "speaker", label: "說話者（mixdesk voice list 看有哪些）", value: last ? last.speaker : "xiaoke" },
        { key: "lang", label: "語言", value: last ? last.lang : "zh-TW" },
      ]);
      if (r && r.values.text.trim()) h.edit({ op: "line.add", at_sec: at, speaker: r.values.speaker.trim(), lang: r.values.lang.trim(), text: r.values.text.trim() });
    };
    lane.append(row);
    return lane;
  }
}

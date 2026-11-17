// node --test --test-isolation=none tests/mixdesk_core.test.cjs
// 混音台前端的純邏輯（ui/js/mixdesk/core.js）：時間換算、展開 repeat、獨奏／靜音、等功率聲像（要跟 mixdesk 的 pan_gain 一模一樣）、
// 吸附、修改堆疊（合併、復原、重做）、在前端先套用修改（含連動）、哪些軌要重新渲染。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/mixdesk/core.js'), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, Math, structuredClone, JSON, Set, Map });
  vm.runInContext(`${source}\nglobalThis.subject = { beatToSec, secToBeat, expandPart, activeAt, audible, panGains, snap, Edits, replay, applyLocal, staleTracks,
    clipBox, playheadX, xToBeat, fitPpb, pitchRange, dbToFader, faderToDb, meterFrac, audibleMap, lineAt, lineSpan,
    moveNotes, resizeNotes, marquee, addNote, deleteNotes, GRIDS, reloadDecision, isConflict, stemOf };`,
    context, { filename: 'core.js' });
  return context.subject;
}
const plain = (x) => JSON.parse(JSON.stringify(x));

function song() {
  return {
    v: 1, hash: 'h', tempo: 120, beats_per_bar: 4,
    tracks: [
      { name: 'a', instrument: { key: 'p', is_drums: false }, volume_db: 0, pan: 0, mute: false, fx: [],
        parts: [{ index: 0, at: 1, repeat: 2, start_beat: 0, length_beats: 4, notes: [{ beat: 0, beats: 1, pitch: 60, vel: 90 }], link: 'H', linked_with: [[1, 0]] },
                { index: 1, at: 3, repeat: 1, start_beat: 8, length_beats: 1, notes: [{ beat: 0, beats: 1, pitch: 64, vel: 90 }], link: null, linked_with: [] }] },
      { name: 'b', instrument: { key: 'f', is_drums: false }, volume_db: -3, pan: 0.5, mute: true, fx: [],
        parts: [{ index: 0, at: 2, repeat: 1, start_beat: 4, length_beats: 4, notes: [{ beat: 0, beats: 1, pitch: 60, vel: 90 }], link: 'H', linked_with: [[0, 0]] }] },
    ],
    dialogue: { duck: {}, lines: [{ index: 0, at_sec: 1, speaker: 'xiaoke', lang: 'zh-TW', emotion: null, text: '嗨', end_sec: 2 }] },
    video: null, buses: {}, master: { lufs: -14, ceiling_dbtp: -1 },
  };
}

test('pan_gains_match_python', () => {
  const { panGains } = load();
  const want = [
    [0, -1, 1.4142135623730951, 0.0], [0, 0, 1.0000000000000002, 1.0000000000000002], [0, 0.3, 0.7389245565417712, 1.205815284253582],
    [0, 1, 8.659560562354934e-17, 1.4142135623730951], [-6, -1, 0.7087857830839414, 0.0], [-6, 0, 0.5011872336272724, 0.5011872336272724],
    [-6, 0.3, 0.37033955435242927, 0.6043392265805357], [-6, 1, 4.340061202674495e-17, 0.7087857830839414],
  ];
  for (const [vol, pan, l, r] of want) {
    const [gl, gr] = panGains(vol, pan);
    assert.ok(Math.abs(gl - l) < 1e-9 && Math.abs(gr - r) < 1e-9, `${vol} ${pan}: ${gl} ${gr}`);
  }
});

test('time_conversion', () => {
  const { beatToSec, secToBeat } = load();
  assert.equal(beatToSec(4, 120), 2);
  assert.equal(secToBeat(1.5, 90), 2.25);
});

test('solo_mute_rules', () => {
  const { audible } = load();
  const t = [{ name: 'a', mute: false }, { name: 'b', mute: true }, { name: 'c', mute: false }];
  assert.deepEqual(plain(audible(t, { mute: {}, solo: {} })), [true, false, true]);
  assert.deepEqual(plain(audible(t, { mute: { b: false }, solo: {} })), [true, true, true]);
  assert.deepEqual(plain(audible(t, { mute: {}, solo: { c: true } })), [false, false, true]);
  assert.deepEqual(plain(audible(t, { mute: { c: true }, solo: { c: true, a: true } })), [true, false, false]);
});

test('expand_part_repeats', () => {
  const { expandPart } = load();
  const p = song().tracks[0].parts[0];
  assert.deepEqual(plain(expandPart(p)), [{ start: 0, end: 1, pitch: 60, vel: 90, rep: 0 }, { start: 4, end: 5, pitch: 60, vel: 90, rep: 1 }]);
});

test('active_notes', () => {
  const { activeAt } = load();
  const ns = [{ start: 0, end: 1 }, { start: 0.5, end: 2 }, { start: 3, end: 4 }];
  assert.deepEqual([...activeAt(ns, 0.75)], [0, 1]);
  assert.deepEqual([...activeAt(ns, 1)], [1]);
});

test('snap_grid', () => {
  const { snap } = load();
  assert.equal(snap(1.13, 0.25), 1.25);
  assert.equal(snap(1.13, 0), 1.13);
  assert.ok(Math.abs(snap(0.34, 1 / 3) - 1 / 3) < 1e-8); // 吸附會四捨五入到 1e-9，避開 0.1×3 這種浮點誤差
});

test('edits_merge_undo_redo', () => {
  const { Edits } = load();
  const e = new Edits();
  e.push({ op: 'track.set', track: 'a', volume_db: -1 });
  e.push({ op: 'track.set', track: 'a', volume_db: -2 });   // 同一軌連續拖推桿：合成一筆
  e.push({ op: 'track.set', track: 'a', pan: 0.5 });        // 不同欄位：另一筆
  assert.equal(e.count, 2);
  assert.deepEqual(plain(e.ops[0]), { op: 'track.set', track: 'a', volume_db: -2 });
  e.undo();
  assert.equal(e.count, 1);
  e.redo();
  assert.equal(e.count, 2);
  e.undo();
  e.push({ op: 'part.set', track: 'a', part: 1, at: 4 });  // 推新的修改後，重做清空
  assert.equal(e.redo(), null);
  assert.deepEqual(plain(e.toApply('abc')), { v: 1, base_hash: 'abc', ops: plain(e.ops) });
  e.clear();
  assert.equal(e.count, 0);
});

test('apply_local_linked_parts', () => {
  const { replay } = load();
  const notes = [{ beat: 0, beats: 2, pitch: 67, vel: 90 }];
  let s = replay(song(), [{ op: 'part.notes', track: 'a', part: 0, notes }]);
  assert.deepEqual(plain(s.tracks[1].parts[0].notes), notes);
  s = replay(song(), [{ op: 'part.unlink', track: 'b', part: 0 }, { op: 'part.notes', track: 'a', part: 0, notes }]);
  assert.deepEqual(plain(s.tracks[1].parts[0].notes), [{ beat: 0, beats: 1, pitch: 60, vel: 90 }]);
  assert.equal(s.tracks[0].parts[0].notes[0].pitch, 67);
  assert.equal(s.tracks[1].parts[0].link, null);
  assert.deepEqual(plain(s.tracks[0].parts[0].linked_with), []);
});

test('apply_local_track_part_line_ops', () => {
  const { replay } = load();
  const base = song();
  const s = replay(base, [
    { op: 'track.set', track: 'b', volume_db: -9, mute: false },
    { op: 'part.set', track: 'a', part: 1, at: 5, repeat: 3 },
    { op: 'part.add', track: 'b', at: 4, notes: [{ beat: 0, beats: 2, pitch: 62, vel: 90 }] },
    { op: 'track.add', name: 'c', instrument: '長笛', after: 'a' },
    { op: 'line.set', line: 0, at_sec: 3, text: '你好' },
    { op: 'part.remove', track: 'a', part: 1 },
  ]);
  assert.deepEqual(s.tracks.map((t) => t.name), ['a', 'c', 'b']);
  assert.equal(s.tracks[2].volume_db, -9);
  assert.equal(s.tracks[2].mute, false);
  assert.equal(s.tracks[0].parts.length, 1);
  assert.deepEqual(plain(s.tracks[2].parts[1]).start_beat, 12);
  assert.equal(s.tracks[2].parts[1].length_beats, 2);
  assert.deepEqual([s.dialogue.lines[0].at_sec, s.dialogue.lines[0].text], [3, '你好']);
  assert.deepEqual(plain(s.tracks[2].parts[0].linked_with), [[0, 0]]); // 加了一軌在中間，連動的軌號跟著改
  assert.equal(base.tracks[1].volume_db, -3); // 原本的不動
});

test('stale_tracks_only_for_notes_and_fx', () => {
  const { staleTracks } = load();
  const base = song();
  assert.deepEqual([...staleTracks(base, [{ op: 'track.set', track: 'a', volume_db: -2, pan: 0.1, mute: true }])], []);
  assert.deepEqual([...staleTracks(base, [{ op: 'part.notes', track: 'a', part: 0, notes: [] }])].sort(), ['a', 'b']);
  assert.deepEqual([...staleTracks(base, [{ op: 'track.set', track: 'b', fx: [{ reverb: {} }] }])], ['b']);
  assert.deepEqual([...staleTracks(base, [{ op: 'part.notes', track: 'a', part: 1, notes: [] }])], ['a']);
});

// ---------------------------------------------------------------- 播放引擎（ui/js/mixdesk/audio.js）：用假的 AudioContext 驗證接線
function loadEngine() {
  const strip = (f) => fs.readFileSync(path.join(__dirname, '../ui/js/mixdesk', f), 'utf8').replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
  const context = vm.createContext({ console, Math, structuredClone, JSON, Set, Map, Float32Array, Promise, ArrayBuffer });
  vm.runInContext(`${strip('core.js')}\n${strip('audio.js')}\nglobalThis.subject = { Engine, loadPlan };`, context, { filename: 'audio.js' });
  return context.subject;
}

function fakeCtx() {
  const param = () => ({ value: 1, calls: [], setValueCurveAtTime(...a) { this.calls.push(a); }, cancelScheduledValues() {}, setValueAtTime(v) { this.value = v; } });
  const node = (kind) => ({ kind, gain: param(), out: [], to: [], connect(n, a, b) { this.out.push([n.kind, a, b]); this.to.push(n); return n; }, disconnect() { this.out = []; this.to = []; } });
  return {
    currentTime: 0, sampleRate: 48000, destination: node('dest'),
    createGain: () => node('gain'), createChannelSplitter: () => node('split'), createChannelMerger: () => node('merge'),
    createAnalyser: () => ({ ...node('analyser'), fftSize: 2048, getFloatTimeDomainData(a) { a.fill(0.5); } }),
    createDynamicsCompressor: () => ({ ...node('comp'), threshold: param(), knee: param(), ratio: param(), attack: param(), release: param() }),
    createBufferSource: () => ({ ...node('src'), buffer: null, start() { this.started = true; }, stop() {} }),
    decodeAudioData: async () => ({ duration: 2, numberOfChannels: 2, length: 96000 }),
    resume: async () => {},
    createMediaElementSource(m) { m.sources = (m.sources || 0) + 1; return node('media'); },
  };
}

test('engine_track_graph_uses_pan_gains', async () => {
  const { Engine } = loadEngine();
  const e = new Engine(fakeCtx());
  const s = song();
  await e.load(s, { a: 'u/a.wav', b: 'u/b.wav' }, async () => new ArrayBuffer(8));
  e.setTrack('a', { volumeDb: -6, pan: 0.3 });
  const t = e.tracks.get('a');
  assert.ok(Math.abs(t.gainL.gain.value - 0.37033955435242927) < 1e-9 && Math.abs(t.gainR.gain.value - 0.6043392265805357) < 1e-9);
  e.setAudible({ a: false, b: true });
  assert.equal(e.tracks.get('a').on.gain.value, 0);
  const m = e.meter('a');
  assert.equal(m.peakL, 0.5);
  e.setMasterGainDb(6);
  assert.ok(Math.abs(e.masterGain.gain.value - 10 ** 0.3) < 1e-12);
});

test('lazy_load_plan', () => {
  const { loadPlan } = loadEngine();
  const tracks = Array.from({ length: 25 }, (_, i) => ({ name: `t${i}` }));
  const aud = tracks.map((_, i) => i % 2 === 0);
  assert.equal(loadPlan(tracks.slice(0, 5), aud.slice(0, 5)).length, 5);       // 不多的話全部載
  assert.deepEqual(plain(loadPlan(tracks, aud)), tracks.filter((_, i) => i % 2 === 0).map((t) => t.name)); // 超過 20 軌只載會響的
});

test('engine_bus_routing_and_late_load', async () => {
  const { Engine } = loadEngine();
  const e = new Engine(fakeCtx());
  const s = song();
  s.tracks[1].bus = 'drums';
  s.buses = { drums: { volume_db: -6, fx: [] }, main: { volume_db: -3, fx: [] } };
  await e.load(s, { a: 'u/a.wav' }, async () => new ArrayBuffer(8));
  assert.ok(Math.abs(e.mainBus.gain.value - 10 ** (-3 / 20)) < 1e-12);   // 配樂總線推桿
  assert.ok(Math.abs(e.buses.get('drums').gain.value - 10 ** (-6 / 20)) < 1e-12);
  assert.ok(e.buses.get('drums').to.includes(e.mainBus));               // 其他匯流排 → 配樂總線
  assert.ok(e.tracks.get('a').merge.to.includes(e.mainBus));
  assert.ok(e.tracks.get('a').on.to.includes(e.tracks.get('a').split));       // 靜音開關在電平表前面：靜音的軌電平表不動
  assert.equal(e.tracks.has('b'), false);                                // 一開始沒載
  await e.addTrack(s, 'b', 'u/b.wav', async () => new ArrayBuffer(8));   // 取消靜音後才載
  assert.ok(e.tracks.get('b').merge.to.includes(e.buses.get('drums')));
  e.setBus('drums', 0);
  assert.equal(e.buses.get('drums').gain.value, 1);
  e.setTrack('b', { volumeDb: -3, pan: 0.5, bus: 'main' });             // 換匯流排要重接
  assert.ok(e.tracks.get('b').merge.to.includes(e.mainBus) && !e.tracks.get('b').merge.to.includes(e.buses.get('drums')));
});

test('timeline_geometry', () => {
  const { clipBox, playheadX, xToBeat, fitPpb, pitchRange } = load();
  const part = { start_beat: 8, length_beats: 4, repeat: 2 };
  assert.deepEqual(plain(clipBox(part, 10)), { left: 80, width: 80 });              // 方塊寬度含 repeat
  assert.equal(playheadX(1, 120, 10), 20);                                          // 120 BPM 的 1 秒＝2 拍
  assert.equal(xToBeat(35, 10), 3.5);
  assert.equal(fitPpb(1150, 150, 100), 10);                                         // 整首塞進畫面
  assert.equal(fitPpb(400, 150, 304), 2);                                           // 太長的話最少 2 px／拍（可以捲）
  const t = { parts: [{ notes: [{ pitch: 60 }, { pitch: 72 }] }, { notes: [{ pitch: 55 }] }] };
  assert.deepEqual(plain(pitchRange(t)), [55, 72]);
  assert.deepEqual(plain(pitchRange({ parts: [] })), [60, 60]);
});

test('fader_and_meter_scale', () => {
  const { dbToFader, faderToDb, meterFrac } = load();
  assert.equal(dbToFader(6), 1);
  assert.equal(dbToFader(-60), 0);
  assert.equal(dbToFader(-12), 0.5);
  for (const db of [6, 0, -3.5, -12, -30, -60]) assert.ok(Math.abs(faderToDb(dbToFader(db)) - db) < 1e-9, `${db}`);
  assert.ok(dbToFader(0) > 0.8 && dbToFader(0) < 0.9);                             // 0 dB 在上面四分之一附近
  assert.equal(meterFrac(0), 0);
  assert.ok(Math.abs(meterFrac(1) - 60 / 66) < 1e-9);                               // 0 dBFS 留 6 dB 給爆表
  assert.equal(meterFrac(4), 1);
});

test('audible_map_includes_dialogue_and_video', () => {
  const { audibleMap } = load();
  const s = { tracks: [{ name: 'a', mute: false }, { name: 'b', mute: true }], dialogue: { lines: [{ at_sec: 1 }] }, video: { path: 'v.mp4', mute: false } };
  assert.deepEqual(plain(audibleMap(s, { mute: {}, solo: {} })), { a: true, b: false, dialogue: true, video: true });
  assert.deepEqual(plain(audibleMap(s, { mute: { dialogue: true }, solo: {} })), { a: true, b: false, dialogue: false, video: true });
  assert.deepEqual(plain(audibleMap(s, { mute: {}, solo: { video: true } })), { a: false, b: false, dialogue: false, video: true });
  s.video.mute = true;
  assert.equal(audibleMap(s, { mute: {}, solo: {} }).video, false);                // 影片原聲的靜音寫在譜檔
  assert.deepEqual(plain(audibleMap({ tracks: [{ name: 'a', mute: false }], dialogue: null, video: null }, { mute: {}, solo: {} })), { a: true });
});

test('line_span_and_subtitle', () => {
  const { lineSpan, lineAt } = load();
  const lines = [{ at_sec: 1, end_sec: 3, text: '嗨' }, { at_sec: 5, end_sec: null, text: '一二三四五六七八' }];
  assert.deepEqual(plain(lineSpan(lines[0])), [1, 3]);
  assert.deepEqual(plain(lineSpan(lines[1])), [5, 7]);                              // 還沒唸過：一個字 0.25 秒估
  assert.equal(lineAt(lines, 2).text, '嗨');
  assert.equal(lineAt(lines, 4), null);
  assert.equal(lineAt(lines, 6.5).text, '一二三四五六七八');
});

test('engine_master_meter_and_ab', () => {
  const { Engine } = loadEngine();
  const e = new Engine(fakeCtx());
  assert.equal(e.meterMaster().peakR, 0.5);                                         // 總輸出也有電平表
  e.abMaster(true);
  assert.equal(e.ab, false);                                                        // 還沒渲染過就沒有正式母帶可聽
  e.mixBuffer = { duration: 2 };
  e.abMaster(true);
  assert.equal(e.mixOut.gain.value, 1);
  assert.ok(!e.limiter.to.includes(e.out) && e.mixOut.to.includes(e.out));          // 聽正式母帶時預覽不出聲，電平表量 mix.wav
  e.abMaster(false);
  assert.ok(e.limiter.to.includes(e.out) && e.mixOut.gain.value === 0);
});

test('engine_media_track_survives_reload', async () => {
  const { Engine } = loadEngine();
  const e = new Engine(fakeCtx());
  const video = {};
  e.attachMedia('video', video);
  await e.load(song(), { a: 'u/a.wav' }, async () => new ArrayBuffer(8));
  assert.ok(e.tracks.has('video') && e.tracks.has('a'));                            // 換曲／重新渲染後影片原聲還在
  e.attachMedia('video', video);
  assert.equal(video.sources, 1);                                                    // <video> 只能接一次
  e.setTrack('video', { volumeDb: -6, pan: 0 });
  assert.ok(Math.abs(e.tracks.get('video').gainL.gain.value - 10 ** (-6 / 20)) < 1e-9);
});

test('engine_load_mix_later', async () => {
  const { Engine } = loadEngine();
  const e = new Engine(fakeCtx());
  await e.load(song(), { a: 'u/a.wav' }, async () => new ArrayBuffer(8));
  assert.equal(e.mixBuffer, null);                                                   // 正式母帶等要聽才載（省記憶體）
  await e.loadMix('u/mix.wav', async () => new ArrayBuffer(8));
  assert.ok(e.mixBuffer && e.mixBuffer.duration === 2);
});

test('pianoroll_drag_math', () => {
  const { moveNotes, resizeNotes, addNote, deleteNotes, GRIDS } = load();
  const notes = [{ beat: 0, beats: 1, pitch: 60, vel: 90 }, { beat: 1.5, beats: 0.5, pitch: 64, vel: 80 }, { beat: 3, beats: 1, pitch: 67, vel: 70 }];
  // 拖兩個音：以抓住的那顆（索引 1）吸附到 1/16 格，其他的照相對位置一起移
  const moved = moveNotes(notes, new Set([0, 1]), 1, 0.6, 2, 0.25);
  assert.deepEqual(plain(moved.map((n) => [n.beat, n.pitch])), [[0.5, 62], [2, 66], [3, 67]]);
  // 往左拖超過開頭：整組停在 0，不會變負的；音高限制在 0～127
  assert.deepEqual(plain(moveNotes(notes, new Set([0, 1]), 1, -9, 100, 0.25).map((n) => [n.beat, n.pitch])), [[0, 127], [1.5, 127], [3, 67]]);
  // 自由（grid 0）就不吸附
  assert.equal(moveNotes(notes, new Set([2]), 2, 0.37, 0, 0)[2].beat, 3.37);
  // 拉長度：吸附、最短一格
  assert.deepEqual(plain(resizeNotes(notes, new Set([0]), 0.6, 0.25).map((n) => n.beats)), [1.5, 0.5, 1]);
  assert.equal(resizeNotes(notes, new Set([1]), -5, 0.25)[1].beats, 0.25);
  // 新增排在對的位置；刪除
  const added = addNote(notes, 2, 72, 0.25, 90);
  assert.deepEqual(plain(added.map((n) => n.beat)), [0, 1.5, 2, 3]);
  assert.deepEqual(plain(deleteNotes(notes, new Set([0, 2])).map((n) => n.pitch)), [64]);
  assert.deepEqual(plain(GRIDS.map((g) => g[0])), [1, 0.5, 0.25, 0.125, 1 / 3, 0]);  // 1/4、1/8、1/16、1/32、三連音、自由
  assert.deepEqual(plain(notes[0]), { beat: 0, beats: 1, pitch: 60, vel: 90 });  // 原本的不動
});

test('marquee_select', () => {
  const { marquee } = load();
  const notes = [{ beat: 0, beats: 1, pitch: 60 }, { beat: 1.5, beats: 0.5, pitch: 64 }, { beat: 3, beats: 1, pitch: 67 }];
  assert.deepEqual(plain([...marquee(notes, 0.5, 2, 59, 64)]), [0, 1]);         // 碰到一點就算
  assert.deepEqual(plain([...marquee(notes, 2, 0.5, 64, 59)]), [0, 1]);         // 反方向拉也一樣
  assert.deepEqual(plain([...marquee(notes, 1, 1.5, 0, 127)]), []);             // 剛好在音符結束／開始的邊界不算
});

test('line_move_keeps_length_text_change_reestimates', () => {
  const { applyLocal, lineSpan } = load();
  const s = song();
  applyLocal(s, { op: 'line.set', line: 0, at_sec: 1.5 });
  assert.deepEqual(plain(lineSpan(s.dialogue.lines[0])), [1.5, 2.5]);               // 搬時間：唸的長度不變
  applyLocal(s, { op: 'line.set', line: 0, text: '一二三四五六七八' });
  assert.equal(s.dialogue.lines[0].end_sec, null);                                  // 改字：長度要重唸才知道，先用字數估
  assert.deepEqual(plain(lineSpan(s.dialogue.lines[0])), [1.5, 3.5]);
});

test('reload_decision', () => {
  const { reloadDecision, isConflict } = load();
  assert.equal(reloadDecision({ known: 10, mtime: 10, pending: 0, busy: false }), 'none');      // 沒變
  assert.equal(reloadDecision({ known: 10, mtime: 12, pending: 0, busy: false }), 'reload');    // 外面改了、面板沒有未套用的 → 自動重載
  assert.equal(reloadDecision({ known: 10, mtime: 12, pending: 3, busy: false }), 'ask');       // 有未套用的 → 問
  assert.equal(reloadDecision({ known: 10, mtime: 12, pending: 0, busy: true }), 'none');       // 自己正在套用／渲染：等做完
  assert.equal(reloadDecision({ known: 10, mtime: null, pending: 0, busy: false }), 'none');    // 讀不到（檔案暫時不在）先不動
  assert.equal(isConflict(new Error('譜檔在你載入後被改過了，請重新載入')), true);           // apply 被雜湊擋下 → 同一個提示
  assert.equal(isConflict(new Error('第 3 行：音符寫錯了')), false);
});

// ---- 最後整體審查抓到的（2026-11-04）
test('edits_sent_ops_are_not_merged', () => {
  const { Edits } = load();
  const e = new Edits();
  e.push({ op: 'track.set', track: 'a', volume_db: -3 });
  e.seal();                                                                // 已經送去寫回
  e.push({ op: 'track.set', track: 'a', volume_db: -5 });                  // 寫回期間又拖了推桿
  assert.equal(e.count, 2);                                                // 不能合併進送出去的那筆
  assert.equal(e.undo().volume_db, -5);
  assert.equal(e.undo(), null);                                            // 送出去的不能復原
  e.redo();
  e.done(1);                                                               // 寫回成功：拿掉送出去的
  assert.deepEqual(plain(e.ops), [{ op: 'track.set', track: 'a', volume_db: -5 }]);
  e.push({ op: 'track.set', track: 'a', volume_db: -7 });
  assert.equal(e.count, 1);                                                // 之後照常合併
  e.seal(); e.unseal();                                                    // 寫回失敗：照舊可以合併
  e.push({ op: 'track.set', track: 'a', volume_db: -8 });
  assert.equal(e.count, 1);
});

test('renamed_tracks_keep_their_stems', () => {
  const { stemOf } = load();
  const s = stemOf([
    { op: 'track.set', track: 'a', name: 'lead' }, { op: 'track.set', track: 'lead', name: 'lead2' },
    { op: 'track.add', name: 'x', instrument: 'p' }, { op: 'track.remove', track: 'b' }, { op: 'track.add', name: 'b', instrument: 'p' },
  ]);
  assert.equal(s('lead2'), 'a');                                           // 改過名的軌還是用渲染時的分軌
  assert.equal(s('lead'), null);
  assert.equal(s('a'), null);
  assert.equal(s('x'), null);                                              // 新軌還沒有分軌
  assert.equal(s('b'), null);                                              // 刪掉再加同名：是新的軌
  assert.equal(s('c'), 'c');
  assert.equal(s('dialogue'), 'dialogue');
});

test('reload_waits_while_a_dialog_is_open', () => {
  const { reloadDecision } = load();
  assert.equal(reloadDecision({ known: 1, mtime: 2, pending: 0, busy: false, dialog: true }), 'none');
  assert.equal(reloadDecision({ known: 1, mtime: 2, pending: 2, busy: false, dialog: true }), 'none');
  assert.equal(reloadDecision({ known: 1, mtime: 2, pending: 2, busy: false, dialog: false }), 'ask');
});

test('engine_stale_load_is_ignored', async () => {
  const { Engine } = loadEngine();
  const ctx = fakeCtx();
  ctx.decodeAudioData = async (buf) => ({ duration: buf.byteLength, numberOfChannels: 2, length: 1 });
  const e = new Engine(ctx);
  let release;
  const slow = new Promise((r) => { release = r; });
  const first = e.load(song(), { a: 'u/a.wav' }, async () => { await slow; return new ArrayBuffer(100); });
  await e.load(song(), { b: 'u/b.wav' }, async () => new ArrayBuffer(3));    // 使用者很快又換了一首
  release();
  assert.equal(await first, false);                                         // 舊的那次回報「被取代了」
  assert.deepEqual([...e.tracks.keys()], ['b']);
  assert.equal(e.duration, 3);                                              // 長度不能被晚到的舊曲蓋掉
});

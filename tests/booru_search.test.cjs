// 畫廊搜尋框（2026-11-23）：booru_search.js 的純函式——中文換 tag（評分、切段、口語對照）、搜尋紀錄、下拉選單的「搜過的」、換游標所在的詞
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const j = (x) => JSON.parse(JSON.stringify(x));   // vm 裡的陣列換成這邊的，deepEqual 才比得了

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/booru_search.js'), 'utf8').replace(/^export /gm, '');
  const context = vm.createContext({});
  vm.runInContext(source + '\nglobalThis.subject = { hasCjk, splitQuery, booruTag, pickZh, segment, translateQuery, pushHistory, matchHistory, replaceToken, toggleTag, aliasOf };', context, { filename: 'booru_search.js' });
  return context.subject;
}

// 假的 /api/tags：照 tags.rs 的中文規則（開頭相符先、再包含）回結果
const TABLE = [
  ['hatsune miku', '初音未來', 106634], ['twintails', '雙馬尾', 904712], ['low twintails', '低位雙馬尾', 112167],
  ['twintails day', '雙馬尾日', 773], ['swimsuit', '泳衣', 660909], ['one-piece swimsuit', '連體泳衣', 136279],
  ['cat ears', '貓耳', 280730], ['cat ear headphones', '貓耳耳機', 5617], ['light smile', '微笑', 73806],
  ['maid', '女僕', 300000], ['ui \\(blue archive\\)', '優香', 9000],
  // 開頭相符但很冷門 vs 包含、很常用（「星海」該選後面那個）
  ['star ocean song', '星海之歌', 120], ['starry sea', '藍色星海', 90000],
  // 只有一個八竿子打不著、又冷門的候選：寧可說找不到
  ['mars text collection', '火星文字遊戲大全集', 5],
];
const lookup = async (q) => TABLE.filter(([, zh]) => zh.includes(q))
  .sort((a, b) => (b[1].startsWith(q) - a[1].startsWith(q)) || b[2] - a[2])
  .map(([tag, zh, count]) => ({ tag, zh, count }));

test('pickZh：越常用分越高、中文越貼近分越高；完全相符的算 exact', async () => {
  const { pickZh } = load();
  const tw = pickZh('雙馬尾', await lookup('雙馬尾'));
  assert.deepEqual([tw.tag, tw.zh, tw.exact], ['twintails', '雙馬尾', true]);
  assert.ok(tw.score > 7);
  assert.equal(pickZh('初音', await lookup('初音')).tag, 'hatsune_miku');
  assert.equal(pickZh('初音', await lookup('初音')).exact, false);
  assert.equal(pickZh('泳衣', await lookup('泳衣')).tag, 'swimsuit');
  assert.equal(pickZh('星海', await lookup('星海')).tag, 'starry_sea');   // 9 萬張的「包含」贏 120 張的「開頭相符」
  assert.equal(pickZh('沒有這個', await lookup('沒有這個')), null);
});

test('pickZh：完全相符但主對照表不是這樣翻（main 對不上）的打折；沒有 main 欄位當有對上', () => {
  const { pickZh } = load();
  const rows = [{ tag: 'bathing', zh: '浴室', count: 60000, main: '洗澡' }, { tag: 'bathroom', zh: '浴室', count: 40000, main: '浴室' }];
  assert.equal(pickZh('浴室', rows).tag, 'bathroom');
  assert.equal(pickZh('浴室', rows.map(({ main, ...r }) => r)).tag, 'bathing');   // 舊版後端：只比張數
  // main 是好幾個說法（「、」隔開）其中一個對上也算
  assert.equal(pickZh('浴室', [{ tag: 'bathing', zh: '浴室', count: 60000, main: '洗澡、浴室' }, rows[1]]).tag, 'bathing');
});

test('aliasOf：口語對照、常用膠囊的中文', () => {
  const { aliasOf } = load();
  assert.equal(aliasOf('白絲'), 'white_thighhighs');
  assert.equal(aliasOf('全身'), 'full_body');   // 常用膠囊上的字
  assert.equal(aliasOf('黑長直'), 'black_hair straight_hair');
  assert.equal(aliasOf('沒有這個'), '');
});

test('segment：每種切法算成本挑最好的；一個字只收完全相符；冷門又差很遠的不收；虛字丟掉', async () => {
  const { segment } = load();
  const parts = (await segment('初音雙馬尾', lookup)).map((x) => [x.text, x.hit && x.hit.tag]);
  assert.deepEqual(j(parts), [['初音', 'hatsune_miku'], ['雙馬尾', 'twintails']]);
  const p2 = (await segment('貓耳女僕微笑', lookup)).map((x) => [x.text, x.hit && x.hit.tag]);
  assert.deepEqual(j(p2), [['貓耳', 'cat_ears'], ['女僕', 'maid'], ['微笑', 'smile']]);   // 微笑走常用膠囊的 smile
  const p3 = (await segment('火星文', lookup)).map((x) => [x.text, x.hit]);
  assert.deepEqual(j(p3), [['火星文', null]]);
  const p4 = (await segment('貓耳的女僕', lookup)).map((x) => [x.text, x.hit && x.hit.tag, !!x.drop]);
  assert.deepEqual(j(p4), [['貓耳', 'cat_ears', false], ['的', null, true], ['女僕', 'maid', false]]);
});

test('translateQuery：英文原樣、中文換 tag、排除號保留（兩個 tag 的對照兩個都排除）、找不到的不放進搜尋只列出來', async () => {
  const { translateQuery } = load();
  const r = await translateQuery('初音雙馬尾 1girl -泳衣 火星文', lookup);
  assert.equal(r.query, 'hatsune_miku twintails 1girl -swimsuit');
  assert.equal((await translateQuery('火星文', lookup)).query, '');
  assert.deepEqual(j(r.notes), [['初音', 'hatsune_miku'], ['雙馬尾', 'twintails'], ['泳衣', 'swimsuit']]);
  assert.deepEqual([...r.miss], ['火星文']);
  assert.equal((await translateQuery('優香', lookup)).query, 'ui_(blue_archive)');
  assert.equal((await translateQuery('-黑長直 貓耳的女僕', lookup)).query, '-black_hair -straight_hair cat_ears maid');
});

test('pushHistory：新的在前、不重複、有上限', () => {
  const { pushHistory } = load();
  assert.deepEqual([...pushHistory(['a', 'b', 'c'], 'b', 30)], ['b', 'a', 'c']);
  assert.deepEqual([...pushHistory(['a', 'b'], ' ', 30)], ['a', 'b']);
  assert.deepEqual([...pushHistory(['a', 'b', 'c'], 'd', 3)], ['d', 'a', 'b']);
});

test('matchHistory：收藏在前；打字後只留對得上的；跟正在打的一模一樣的不列', () => {
  const { matchHistory } = load();
  const hist = ['hatsune_miku twintails', 'cat_ears maid', 'swimsuit'];
  const favs = ['cat_ears maid'];
  assert.deepEqual(j(matchHistory(hist, favs, '', 5).map((x) => x.q)), ['cat_ears maid', 'hatsune_miku twintails', 'swimsuit']);
  assert.equal(matchHistory(hist, favs, '', 5)[0].fav, true);
  assert.deepEqual(j(matchHistory(hist, favs, 'hat twin', 5).map((x) => x.q)), ['hatsune_miku twintails']);
  assert.deepEqual(j(matchHistory(hist, favs, 'cat ears', 5).map((x) => x.q)), ['cat_ears maid']);
  assert.deepEqual(j(matchHistory(hist, favs, 'swimsuit', 5).map((x) => x.q)), []);
});

test('replaceToken：換掉游標所在的詞，保留排除號', () => {
  const { replaceToken } = load();
  assert.deepEqual({ ...replaceToken('1girl hats', 10, 'hatsune miku') }, { value: '1girl hatsune_miku ', caret: 19 });
  assert.deepEqual({ ...replaceToken('-swi 1girl', 4, 'swimsuit') }, { value: '-swimsuit 1girl', caret: 10 });
  assert.equal(replaceToken('', 0, 'solo').value, 'solo ');
});

test('toggleTag：有就拿掉、沒有就加', () => {
  const { toggleTag } = load();
  assert.equal(toggleTag('hatsune_miku', 'solo'), 'hatsune_miku solo');
  assert.equal(toggleTag('hatsune_miku solo', 'solo'), 'hatsune_miku');
});

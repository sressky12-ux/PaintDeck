// 畫廊（2026-11-23）：booru_prompt.js 的純函式——tag 轉提示詞、挑抓圖網址、檔名
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/booru_prompt.js'), 'utf8').replace(/^export /gm, '');
  const context = vm.createContext({});
  vm.runInContext(source + '\nglobalThis.subject = { fmtTag, ratingTag, buildPrompt, canSend, pickMediaUrl, fileName, MAX_BYTES };', context, { filename: 'booru_prompt.js' });
  return context.subject;
}

test('fmtTag：底線換空白、括號跳脫、顏文字不動', () => {
  const { fmtTag } = load();
  assert.equal(fmtTag('long_hair'), 'long hair');
  assert.equal(fmtTag('irem_(black_survival)'), 'irem \\(black survival\\)');
  assert.equal(fmtTag('^_^'), '^_^');
  assert.equal(fmtTag('ui_\\(blue_archive\\)'), 'ui \\(blue archive\\)');
});

test('ratingTag：各站分級對到 Anima 的評級', () => {
  const { ratingTag } = load();
  assert.equal(ratingTag('g'), 'safe');
  assert.equal(ratingTag('general'), 'safe');
  assert.equal(ratingTag('safe'), 'safe');
  assert.equal(ratingTag('s'), 'sensitive');
  assert.equal(ratingTag('q'), 'nsfw');
  assert.equal(ratingTag('questionable'), 'nsfw');
  assert.equal(ratingTag('E'), 'explicit');
  assert.equal(ratingTag(''), '');
});

test('buildPrompt：評級 → 人數 → 角色 → 作品 → 其他，meta 和畫師預設不放', () => {
  const { buildPrompt } = load();
  const tags = {
    artist: ['seasalt_flavour'], copyright: ['black_survival'], character: ['irem_(black_survival)'],
    general: ['long_hair', '1girl', 'solo', 'open_mouth'], meta: ['absurdres', 'highres'],
  };
  assert.equal(buildPrompt(tags, 'g'), 'safe, 1girl, solo, irem \\(black survival\\), black survival, long hair, open mouth');
});

test('buildPrompt：畫師打開時加 @，評級、角色可以關掉，重複的只留一個', () => {
  const { buildPrompt } = load();
  const tags = { artist: ['kyomu'], character: ['hatsune_miku'], copyright: ['vocaloid'], general: ['1girl', 'twintails', 'twintails'] };
  assert.equal(buildPrompt(tags, 'e', { artist: true }), 'explicit, 1girl, hatsune miku, vocaloid, @kyomu, twintails');
  assert.equal(buildPrompt(tags, 'e', { rating: false, character: false }), '1girl, vocaloid, twintails');
  assert.equal(buildPrompt({}, ''), '');
});

test('buildPrompt：浮水印、簽名、網址這類雜訊 tag 不放（跟 tagclean.rs 同一份）', () => {
  const { buildPrompt } = load();
  const tags = { general: ['1girl', 'artist_name', 'twitter_username', 'Signature', 'smile', 'english_text'] };
  assert.equal(buildPrompt(tags, 'g'), 'safe, 1girl, smile');
});

test('canSend／pickMediaUrl：靜態圖才送；原圖太大改抓 sample', () => {
  const { canSend, pickMediaUrl, MAX_BYTES } = load();
  const small = { fileExt: 'png', fileSize: 3e6, width: 2000, height: 3000, mediaUrl: 'https://cdn/x.png', sampleUrl: 'https://cdn/s.jpg' };
  assert.equal(canSend(small), true);
  assert.equal(pickMediaUrl(small), 'https://cdn/x.png');
  assert.equal(pickMediaUrl({ ...small, fileSize: MAX_BYTES + 1 }), 'https://cdn/s.jpg');
  // 不知道大小（Safebooru 回 0）：看像素
  assert.equal(pickMediaUrl({ ...small, fileSize: 0, width: 8000, height: 6000 }), 'https://cdn/s.jpg');
  assert.equal(pickMediaUrl({ ...small, fileSize: 0 }), 'https://cdn/x.png');
  // 沒有 sample 只好抓原圖
  assert.equal(pickMediaUrl({ ...small, fileSize: MAX_BYTES + 1, sampleUrl: '' }), 'https://cdn/x.png');
  // 影片、動圖、ugoira 不送
  for (const fileExt of ['mp4', 'webm', 'gif', 'zip']) {
    assert.equal(canSend({ ...small, fileExt }), false, fileExt);
    assert.equal(pickMediaUrl({ ...small, fileExt }), '');
  }
  // 沒寫副檔名：從網址看
  assert.equal(canSend({ mediaUrl: 'https://safebooru.org/images/1/a.jpg' }), true);
});

test('fileName：只留英數，副檔名照實際抓的網址', () => {
  const { fileName } = load();
  assert.equal(fileName({ source: 'danbooru', postId: '12609209', fileExt: 'png' }, 'https://cdn.donmai.us/sample/x/sample-abc.jpg'), 'booru_danbooru_12609209.jpg');
  assert.equal(fileName({ source: 'safebooru', postId: '7/../1', fileExt: 'jpeg' }, ''), 'booru_safebooru_71.jpg');
});

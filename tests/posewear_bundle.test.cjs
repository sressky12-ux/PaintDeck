// 試穿工作台「交出整套」：resources/share/posewear_bundle.js.txt 替換 studio.js 的 window.WB={（後端同步、搬家腳本共用）
const test = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const path = require("node:path");

const root = path.join(__dirname, "..");
const BUNDLE = fs.readFileSync(path.join(root, "resources", "share", "posewear_bundle.js.txt"), "utf8").trimEnd();

/** 假的 studio 閉包：W、compose、assetBlob、isGen、hasPerson、$ 跟官網同名 */
function studio(W, dirtyText = "") {
  const box = { window: {} };
  vm.createContext(box);
  vm.runInContext(`
    const W = ${JSON.stringify(W)};
    const isGen = () => W.mode == 'gen', hasPerson = () => W.persons.length > 0;
    const compose = () => W.pl == 'en' ? 'EN prompt image 1' : '中文 第 1 张';
    async function assetBlob(a) { if (a.bad) throw Error('素材下载失败（404）'); return 'blob:' + a.k; }
    const $ = (s) => ({ value: ${JSON.stringify(dirtyText)} });
    ${BUNDLE}async show(){}, _W: W};
  `, box);
  return box.window.WB;
}

test("bundle 的順序和角色：人物 → 服裝 → 姿勢（每個一張）→ 表情", async () => {
  const WB = studio({ mode: "tryon", pl: "zh", dirty: false,
    persons: [{ blob: "me" }], garments: [{ kind: "lib", k: "g1" }],
    poses: [{ kind: "lib", k: "p1" }, { kind: "lib", k: "p2" }], exprs: [{ kind: "lib", k: "e1" }] });
  const b = JSON.parse(JSON.stringify(await WB.bundle()));
  assert.deepEqual(b.items.map((x) => [x.role, x.blob]),
    [["person", "me"], ["outfit", "blob:g1"], ["pose", "blob:p1"], ["pose", "blob:p2"], ["expression", "blob:e1"]]);
  assert.deepEqual(b.items.map((x) => x.name), ["person1.jpg", "outfit1.jpg", "pose1.jpg", "pose2.jpg", "expression1.jpg"]);
  assert.equal(b.en, "EN prompt image 1");
  assert.equal(b.zh, "中文 第 1 张");
  assert.equal(b.pl, "zh");
  assert.equal(b.custom, "");
  assert.equal(WB._W.pl, "zh"); // 切過 W.pl 要還原
});

test("生成模式只選服裝：沒有人物也能交出", async () => {
  const WB = studio({ mode: "gen", pl: "en", dirty: true, persons: [], garments: [{ kind: "lib", k: "g" }], poses: [], exprs: [] }, " 我改過的 ");
  const b = JSON.parse(JSON.stringify(await WB.bundle()));
  assert.deepEqual(b.items.map((x) => x.role), ["outfit"]);
  assert.equal(b.custom, "我改過的");
  assert.equal(WB._W.pl, "en");
});

test("沒選服裝、試穿模式沒人物：丟出跟生成鍵一樣的提示", async () => {
  await assert.rejects(studio({ mode: "gen", pl: "en", persons: [], garments: [], poses: [], exprs: [] }).bundle(), /请先选择服装/);
  await assert.rejects(studio({ mode: "tryon", pl: "en", persons: [], garments: [{ k: "g" }], poses: [], exprs: [] }).bundle(), /请先上传人物照片/);
});

test("compose 出錯時 W.pl 照樣還原", async () => {
  const box = { window: {} };
  vm.createContext(box);
  vm.runInContext(`const W={mode:'gen',pl:'zh',persons:[],garments:[{k:'g'}],poses:[],exprs:[]};
    const isGen=()=>true,hasPerson=()=>false,compose=()=>{throw Error('x')},assetBlob=async()=>'b',$=()=>({value:''});
    ${BUNDLE}_W:W};`, box);
  await assert.rejects(box.window.WB.bundle(), /x/);
  assert.equal(box.window.WB._W.pl, "zh");
});

test("bridge.js：工作台生成鍵旁的「交給繪匣」，送 {pw:'bundle'} 給父頁", () => {
  const src = fs.readFileSync(path.join(root, "ui", "posewear", "bridge.js"), "utf8");
  assert.match(src, /pd-handoff/);
  assert.match(src, /wbGo/);
  assert.match(src, /pw: "bundle"/);
  assert.match(src, /WB\.bundle\(\)/);
  assert.match(src, /交給繪匣/);
  assert.match(src, /交给绘匣/);
});

const test = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const { readFileSync } = require("node:fs");
const read = (file) => readFileSync(require("node:path").join(__dirname, "..", file), "utf8");
const gen = read("ui/js/gen.js");

test("切換家族保留整匣參考圖，並依現在的上限檢查", () => {
  const refs = read("ui/js/refs.js").replace(/^import .*$/gm, "").replace(/^export /gm, "");
  const box = vm.createContext({ toast() {} });
  vm.runInContext(refs + "\nrefs = Array(16).fill({}); ctx = {family: () => 'mmimage'};", box);
  assert.equal(vm.runInContext("maxRefs()", box), 16);
  vm.runInContext("ctx.family = () => 'qwen21'", box);
  assert.equal(vm.runInContext("maxRefs()", box), 10);
  assert.match(vm.runInContext("check('')", box), /10/);
  assert.equal(vm.runInContext("get().length", box), 16);
});
test("MiniMax 官方比例、4MP 預設與 32 對齊", () => {
  const size = vm.runInNewContext("(" + gen.match(/function mmSize\([\s\S]*?\n\}/)[0] + ")");
  assert.deepEqual({...size("1:1", 4)}, {w:2048, h:2048});
  for (const [ratio, mp] of [["16:9",16], ["21:9",20]]) {
    const {w,h} = size(ratio,mp);
    assert.equal(w % 32 + h % 32, 0);
    assert.ok(Math.abs(w*h/(mp*1024*1024)-1)<0.03);
  }
});
test("生成頁依 MiniMax 家族收起負面詞", () => {
  assert.ok(gen.includes('F("negDetails").hidden = mm'));
});

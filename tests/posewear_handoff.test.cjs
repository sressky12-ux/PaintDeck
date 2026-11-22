// 生成頁收下衣姿工作台交出的整套（ui/js/pw_handoff.js 的純函式；設計：docs\superpowers\specs\2026-11-22-posewear-handoff-live-sync-design.md 第四節）
const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");
const fs = require("node:fs");
const { pathToFileURL } = require("node:url");

const root = path.join(__dirname, "..");
const load = () => import(pathToFileURL(path.join(root, "ui", "js", "pw_handoff.js")).href);
const items = (n, role = "pose") => Array.from({ length: n }, () => ({ role }));

test("planHandoff：引擎照目前家族；不能做多圖參考的家族看有沒有 MiniMax", async () => {
  const { planHandoff } = await load();
  assert.equal(planHandoff({ family: "qwen21", hasMmimage: true, refsNow: 0, items: items(2) }).family, "qwen21");
  assert.equal(planHandoff({ family: "mmimage", hasMmimage: true, refsNow: 0, items: items(2) }).family, "mmimage");
  assert.equal(planHandoff({ family: "anima", hasMmimage: true, refsNow: 0, items: items(2) }).family, "mmimage");
  assert.equal(planHandoff({ family: "moss", hasMmimage: false, refsNow: 0, items: items(2) }).family, "qwen21");
  assert.equal(planHandoff({ family: "custom", hasMmimage: false, refsNow: 0, items: items(2) }).family, "qwen21");
});

test("planHandoff：張數上限 Qwen 10、MiniMax 16，超過就擋並說幾張", async () => {
  const { planHandoff } = await load();
  const q = planHandoff({ family: "qwen21", hasMmimage: false, refsNow: 0, items: items(11) });
  assert.equal(q.ok, false);
  assert.equal(q.max, 10);
  assert.match(q.reason, /11 張/);
  assert.match(q.reason, /Qwen 2\.1 最多 10 張/);
  assert.match(q.reason, /少選幾個姿勢/);
  assert.equal(planHandoff({ family: "qwen21", hasMmimage: false, refsNow: 0, items: items(10) }).ok, true);
  assert.equal(planHandoff({ family: "mmimage", hasMmimage: true, refsNow: 0, items: items(16) }).ok, true);
  const m = planHandoff({ family: "mmimage", hasMmimage: true, refsNow: 0, items: items(17) });
  assert.equal(m.ok, false);
  assert.match(m.reason, /MiniMax H3 Image 最多 16 張/);
  assert.equal(planHandoff({ family: "qwen21", hasMmimage: false, refsNow: 0, items: [] }).ok, false);
});

test("planHandoff：角色對照、要不要先問換掉現有參考圖", async () => {
  const { planHandoff } = await load();
  const p = planHandoff({ family: "qwen21", hasMmimage: false, refsNow: 3,
    items: [{ role: "person" }, { role: "outfit" }, { role: "pose" }, { role: "expression" }, { role: "weird" }] });
  assert.deepEqual(p.roles, ["person", "outfit", "pose", "person", "person"]);
  assert.equal(p.replace, true);
  assert.equal(planHandoff({ family: "qwen21", hasMmimage: false, refsNow: 0, items: items(1) }).replace, false);
});

test("renumber：單張、範圍、中文寫法（簡、繁）、兩種引擎", async () => {
  const { renumber } = await load();
  const en = "Image 1 shows the person; images 2–4 show the outfit; image 5 is a pose sketch.";
  assert.equal(renumber(en, "qwen21"), "<image1> shows the person; <image2>, <image3>, <image4> show the outfit; <image5> is a pose sketch.");
  assert.equal(renumber(en, "mmimage"), "<Picture 1> shows the person; <Picture 2>, <Picture 3>, <Picture 4> show the outfit; <Picture 5> is a pose sketch.");
  assert.equal(renumber("第 1 张是人物，第 2–3 张是服装", "qwen21"), "<image1>是人物，<image2>、<image3>是服装");
  assert.equal(renumber("第 1 張是人物，第 2-3 張是服裝", "mmimage"), "<Picture 1>是人物，<Picture 2>、<Picture 3>是服裝");
  assert.equal(renumber("no numbers here", "qwen21"), "no numbers here");
});

test("handoffPrompt：MiniMax 用英文；Qwen 用工作台選的語言；手改過的優先", async () => {
  const { handoffPrompt } = await load();
  const b = { en: "image 1 EN", zh: "第 1 张 ZH", pl: "zh", custom: "" };
  assert.equal(handoffPrompt(b, "mmimage"), "<Picture 1> EN");
  assert.equal(handoffPrompt(b, "qwen21"), "<image1> ZH");
  assert.equal(handoffPrompt({ ...b, pl: "en" }, "qwen21"), "<image1> EN");
  assert.equal(handoffPrompt({ ...b, custom: "第 1 张 我改的" }, "qwen21"), "<image1> 我改的");
  // 手改的是中文、引擎要英文：MiniMax 照樣用英文版
  assert.equal(handoffPrompt({ ...b, custom: "第 1 张 我改的" }, "mmimage"), "<Picture 1> EN");
  assert.equal(handoffPrompt({ ...b, pl: "en", custom: "image 1 mine" }, "mmimage"), "<Picture 1> mine");
});

test("posewear.js 收 {pw:'bundle'}：用 pw_handoff、先問再換、逐張上傳", () => {
  const src = fs.readFileSync(path.join(root, "ui", "js", "posewear.js"), "utf8");
  assert.match(src, /from "\.\/pw_handoff\.js"/);
  assert.match(src, /m\.pw === "bundle"/);
  assert.match(src, /換掉現在的參考圖？/);
  assert.match(src, /張上傳失敗/);
  assert.match(src, /看過再按生成/);
});

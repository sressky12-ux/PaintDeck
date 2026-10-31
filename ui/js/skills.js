// 外掛寫法：使用者自備的提示詞 skill（有 SKILL.md 的 GitHub 庫）給生圖助手、AI 改寫、只寫詞用。後端 server\api_skills.rs、skills.rs
import { el, api, toast } from "./util.js";

let box = null;
let data = { skills: [], bind: {}, guides: [], dir: "" };
const q = (id) => box.querySelector("#" + id);

/** 設定頁「外掛寫法」那格：第一次打開設定頁才讀 */
export function mountSkills(b) {
  if (box) return load();
  box = b;
  q("skInstall").addEventListener("click", install);
  q("skUrl").addEventListener("keydown", (e) => { if (e.key === "Enter") install(); });
  q("skOpen").addEventListener("click", () => api("/api/skills/open", { json: {} }).catch((e) => toast(e.message, "err")));
  q("skReload").addEventListener("click", load);
  load();
}

async function load() {
  try { data = await api("/api/skills"); } catch (e) { q("skMsg").textContent = "讀不到：" + e.message; return; }
  q("skMsg").textContent = data.skills.length ? "" : `還沒有裝。資料夾在 ${data.dir}`;
  q("skPick").textContent = data.pick?.semantic
    ? `挑段方式：照意思挑（${data.pick.model}，用 CPU 算，不占顯卡）`
    : "挑段方式：照關鍵字挑。在「安裝與修復」勾「AI 改寫、生圖助手」會補下載 Qwen3-Embedding，之後就能照意思挑（換句話說也挑得到）";
  renderList();
  renderBind();
}

function license(src) {
  const id = src?.license;
  if (!src?.repo) return "自己放的";
  if (!id) return "沒標授權（只能自己看）";
  return id === "NOASSERTION" ? "授權見 LICENSE" : "授權 " + id;
}

function renderList() {
  const list = q("skList");
  list.innerHTML = "";
  for (const s of data.skills) {
    const toc = el("pre", { class: "sk-toc", hidden: true });
    const tocBtn = el("button", { class: "akey sm", type: "button", onclick: async () => {
      if (!toc.hidden) { toc.hidden = true; return; }
      try { toc.textContent = (await api(`/api/skills/${encodeURIComponent(s.id)}`)).toc; toc.hidden = false; }
      catch (e) { toast(e.message, "err"); }
    } }, "目錄");
    const rm = el("button", { class: "akey sm", type: "button", onclick: () => remove(s) }, "移除");
    const meta = [`${s.parts} 段`, `約 ${s.tokens.toLocaleString()} token`, license(s.source)];
    if (s.source?.repo) meta.push("來源 github.com/" + s.source.repo);
    list.append(el("div", { class: "sk-item" },
      el("div", {}, el("b", {}, s.name), s.id !== s.name ? el("span", { class: "muted sm" }, `（資料夾 ${s.id}）`) : null),
      s.desc ? el("div", { class: "muted sm sk-desc" }, s.desc) : null,
      el("div", { class: "muted sm" }, meta.join("・")),
      el("div", { class: "row gap" }, tocBtn, rm),
      toc));
  }
}

function renderBind() {
  const grid = q("skBind");
  grid.innerHTML = "";
  for (const g of data.guides) {
    const sel = el("select", { onchange: () => bind(g, sel.value) }, el("option", { value: "" }, "不用"));
    for (const s of data.skills) sel.append(el("option", { value: s.id }, s.name));
    const cur = data.bind?.[g.name] || "";
    if (cur && !data.skills.some((s) => s.id === cur)) sel.append(el("option", { value: cur }, `${cur}（找不到資料夾，當作不用）`));
    sel.value = cur;
    grid.append(el("div", {}, el("span", { class: "sub" }, g.zh), sel));
  }
}

async function install() {
  const url = q("skUrl").value.trim();
  if (!url) { toast("先貼 GitHub 網址", "err"); return; }
  const btn = q("skInstall");
  btn.disabled = true;
  q("skMsg").textContent = "從 GitHub 下載中…";
  try {
    const r = await api("/api/skills/install", { json: { url } });
    toast(`裝好了：${r.skill.name}（${r.skill.parts} 段）` + (r.replaced ? "，舊版搬到待刪除" : "") + "。到下面選要綁的模型", "ok");
    q("skUrl").value = "";
    await load();
  } catch (e) { q("skMsg").textContent = "安裝失敗：" + e.message; }
  finally { btn.disabled = false; }
}

async function bind(g, skill) {
  try {
    await api("/api/skills/bind", { json: { guide: g.name, skill } });
    toast(skill ? `「${g.zh}」改用外掛寫法，下一次寫提示詞就生效` : `「${g.zh}」不用外掛寫法了`, "ok");
    data.bind = { ...data.bind, [g.name]: skill };
  } catch (e) { toast(e.message, "err"); await load(); }
}

async function remove(s) {
  if (!confirm(`把「${s.name}」搬到待刪除？綁著它的模型會一起改回不用。`)) return;
  try {
    await api("/api/skills/remove", { json: { id: s.id } });
    toast(`「${s.name}」搬到待刪除了`, "ok");
    await load();
  } catch (e) { toast(e.message, "err"); }
}

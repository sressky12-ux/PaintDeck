// 表單型頁面（設定、API）左邊的段落目錄：段落依組別排好，點了跳過去，捲動時目前那段亮燈。
import { el } from "./util.js";

/** 照 order 把段落分組：組的順序照 order（沒列到的放最後），組內維持原本順序 */
export function groupSections(items, order) {
  const groups = new Map(order.map((g) => [g, []]));
  for (const it of items) {
    if (!groups.has(it.group)) groups.set(it.group, []);
    groups.get(it.group).push(it);
  }
  return [...groups].filter(([, list]) => list.length).map(([group, list]) => ({ group, items: list }));
}

const headOf = (sec) => (sec.querySelector(":scope > label, :scope > .row > label, :scope > summary, :scope > h3")?.textContent || "").replace(/\s+/g, " ").trim();

/**
 * @param {Element} scroller 會捲動的容器（目錄放在它左邊，兩個一起包進 .form-wrap）
 * @param {Element} list 段落的父元素（段落會照組別重排到這裡）
 * @param {{key: string, label: string}[]} groups 空陣列＝不分組
 */
export function mountToc(scroller, list, groups = []) {
  const secs = [...list.children].filter((s) => s.matches("section"));
  const order = groups.map((g) => g.key);
  const grouped = groups.length
    ? groupSections(secs.map((s) => ({ group: s.dataset.group || "", sec: s })), order)
    : [{ group: "", items: secs.map((sec) => ({ group: "", sec })) }];
  const nav = el("nav", { class: "form-toc", "aria-label": "段落目錄" });
  const links = new Map();
  for (const g of grouped) {
    const label = groups.find((x) => x.key === g.group)?.label;
    if (label) nav.append(el("div", { class: "toc-group" }, label));
    for (const { sec } of g.items) {
      list.append(sec);    // 依組別重排（元素原樣搬，id 不變）
      const a = el("button", { type: "button", class: "toc-item" }, headOf(sec) || "—");
      a.onclick = () => { clicked = sec; sec.scrollIntoView({ block: "start", behavior: "smooth" }); };
      nav.append(a);
      links.set(sec, a);
      sec.classList.add("toc-sec");
    }
  }
  scroller.before(nav);
  nav.parentElement.classList.add("form-wrap");
  // 段落標題會被 i18n 換字：目錄跟著換
  new MutationObserver(() => { for (const [sec, a] of links) a.textContent = headOf(sec) || "—"; })
    .observe(list, { subtree: true, characterData: true, childList: true });
  // 目前段落＝上緣已經捲到容器頂端附近的最後一段（捲到底時就是最後一段）
  let raf = 0, clicked = null;
  const light = () => {
    raf = 0;
    if (!scroller.getClientRects().length) return;
    const top = scroller.getBoundingClientRect().top + 48;
    const atEnd = scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 4;
    let cur = null;
    for (const sec of links.keys()) {
      if (!sec.getClientRects().length) continue;
      if (sec.getBoundingClientRect().top <= top) cur = sec;
    }
    if (atEnd && clicked) cur = clicked;   // 捲到底時最後幾段到不了頂端：以剛點的那一段為準
    for (const [sec, a] of links) a.classList.toggle("on", sec === cur);
  };
  scroller.addEventListener("scroll", () => { if (!raf) raf = requestAnimationFrame(light); }, { passive: true });
  scroller.addEventListener("wheel", () => { clicked = null; }, { passive: true });
  light();
  return nav;
}

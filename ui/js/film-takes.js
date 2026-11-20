// 共用拍攝版本面板：預覽、查配方與採用都是本地版本操作，不提交生成。
import { el } from "./util.js";
import { openTakeCompare } from "./take-compare.js";

export const pendingTake = (seg) => (seg?.takes || []).find((t) => t.id === seg?.pending_take) || null;

export function renderTakePanel({ seg, disabled = false, onSelect, media = (t) => t.result?.video_id ? { src: `/img/${t.result.video_id}`, poster: `/thumb/${t.result.video_id}` } : null }) {
  const takes = seg?.takes || [];
  if (!takes.length) return null;
  const selected = seg.selected_take;
  const box = el("details", { class: "dir-pr film-takes", open: !!seg.pending_take }, el("summary", {}, `拍攝版本（${takes.length}）`));
  if (seg.pending_take) box.append(el("div", { class: "pe-status amb" }, "新版本已拍好。預覽後按「採用這版」，才會用於接續與合片。"));
  if (seg.stale) box.append(el("div", { class: "pe-status amb" }, seg.stale));
  for (const take of [...takes].reverse()) {
    const active = take.id === selected, candidate = take.id === seg.pending_take;
    const result = media(take);
    const row = el("div", { class: "pe" },
      el("div", { class: "row gap wrap" }, el("b", {}, take.label || "拍攝版本"),
        el("span", { class: "muted sm" }, active ? "目前採用" : candidate ? "待審片" : "保留版本"),
        el("button", { type: "button", class: "akey sm", disabled,
          onclick: () => onSelect?.(take.id) }, active && !seg.pending_take ? "繼續用這版" : "採用這版")),
      result?.src ? el("video", { src: result.src, poster: result.poster || "", controls: true, preload: "none", playsinline: true, style: "width:100%;max-height:240px" })
        : el("span", { class: "muted sm" }, "這版沒有可預覽的影片"),
      el("details", {}, el("summary", {}, "查看生成配方"),
        el("pre", { style: "white-space:pre-wrap;overflow-wrap:anywhere;max-height:300px;overflow:auto" }, JSON.stringify(take.recipe || { legacy: true }, null, 2))));
    box.append(row);
  }
  const previewable = takes.filter((t) => media(t)?.src);
  if (takes.length >= 2 && previewable.length >= 2) {
    box.append(el("div", { class: "row gap wrap" }, el("button", { type: "button", class: "akey sm", disabled,
      onclick: () => openTakeCompare({
        title: "並排比較拍攝版本",
        items: takes.map((t) => {
          const m = media(t);
          return { id: t.id, label: t.label || "拍攝版本", src: m?.src || "", poster: m?.poster || "",
            note: t.id === selected ? "目前採用" : t.id === seg.pending_take ? "待審片" : "保留版本" };
        }),
        onAdopt: (id) => onSelect?.(id),
        disabled,
      }) }, "並排比較")));
  }
  return box;
}

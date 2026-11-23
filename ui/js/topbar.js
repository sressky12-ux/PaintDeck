import { $, $$, el, store } from "./util.js";
import { app } from "./state.js";
import * as queue from "./queue.js";

export const PAGE_ORDER = ["gen", "taglib", "gallery", "assets", "posewear", "loras", "civitai", "director", "comic", "agent", "train", "api", "booru", "jobs", "settings"];
export const DEFAULT_PINS = ["gen", "taglib", "gallery", "assets", "director"];

export function pinnedKeys(pins, current) {
  const keys = new Set((Array.isArray(pins) ? pins : DEFAULT_PINS).filter((p) => PAGE_ORDER.includes(p)));
  if (PAGE_ORDER.includes(current) && current !== "jobs" && current !== "agent") keys.add(current);
  return [...keys];
}

export function altPage(key) {
  if (key === "0") return "settings";
  return /^[1-9]$/.test(key) ? PAGE_ORDER[Number(key) - 1] : null;
}

let pins = DEFAULT_PINS, current = "gen", opened;
function closeMenu(focus = false) {
  if (!opened) return;
  const { panel, button } = opened;
  panel.hidden = true;
  button.setAttribute("aria-expanded", "false");
  $("#navPinAction").hidden = true;
  opened = null;
  if (focus) button.focus();
}

export function markPage(page) {
  current = page;
  const buttons = pinnedKeys(pins, page).map((p) => {
    const original = p === "jobs" ? $("#queueInfo") : $(`#tabs [data-page="${p}"]`);
    const label = Array.from(original.childNodes).filter((n) => n.nodeType === Node.TEXT_NODE).map((n) => n.textContent).join("").trim();
    return el("button", { type: "button", class: "tkey" + (p === page ? " on" : ""), "data-page": p,
      "aria-current": p === page ? "page" : null }, el("small", {}, $(".n", original).textContent), label);
  });
  $("#navPins").replaceChildren(...buttons);
  for (const key of $$("#tabs .pkey")) {
    const selected = key.dataset.page === page;
    key.classList.toggle("on", selected);
    key.setAttribute("aria-selected", String(selected));
  }
}

export function togglePin(page) {
  if (!PAGE_ORDER.includes(page)) return;
  pins = pins.includes(page) ? pins.filter((p) => p !== page) : [...pins, page];
  store.set("nav_pins", pins);
  markPage(current);
}

export function init() {
  pins = pinnedKeys(store.get("nav_pins", DEFAULT_PINS));
  markPage(app.page || "gen");
  for (const button of $$(".top button[aria-controls]")) {
    const panel = document.getElementById(button.getAttribute("aria-controls"));
    button.addEventListener("click", () => {
      const show = panel.hidden;
      closeMenu();
      if (show) {
        panel.hidden = false;
        button.setAttribute("aria-expanded", "true");
        opened = { button, panel };
      }
    });
  }
  document.addEventListener("pointerdown", (e) => {
    if (opened && !opened.panel.contains(e.target) && !opened.button.contains(e.target)) closeMenu();
  });
  document.addEventListener("keydown", (e) => {
    if (opened && e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeMenu(true); }
  }, true);
  $("#navPins").onclick = (e) => {
    const p = e.target.closest("[data-page]")?.dataset.page;
    if (p === "jobs") { e.stopPropagation(); queue.toggle(); }
    else if (p) app.showPage(p);
  };
  $("#qtxt").onclick = (e) => { e.stopPropagation(); queue.toggle(); };
  $("#rocker").addEventListener("click", (e) => { if (e.target.closest("[data-t]")) closeMenu(true); });
  $("#tabs").addEventListener("click", (e) => { if (e.target.closest(".pkey")) closeMenu(true); });
  $("#tabs").addEventListener("contextmenu", (e) => {
    const key = e.target.closest(".pkey");
    if (!key) return;
    e.preventDefault();
    const p = key.dataset.page || "jobs";
    const action = $("#navPinAction");
    action.textContent = pins.includes(p) ? "從頂列拿掉" : "釘到頂列";
    action.hidden = false;
    action.focus();
    action.onclick = () => { togglePin(p); action.hidden = true; key.focus(); };
  });
}

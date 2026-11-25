export function negIsCustom(value, def) {
  const normalize = (s) => s.trim().replace(/\s+/g, " ");
  return normalize(value) !== normalize(def);
}

let defaultValue;
const byId = (id) => document.getElementById(id);

function select(negative) {
  const pos = byId("promptTabPos"), neg = byId("promptTabNeg");
  pos.closest(".prompt-scr").dataset.tab = negative ? "neg" : "pos";
  for (const button of [pos, neg]) {
    const active = button === (negative ? neg : pos);
    button.setAttribute("aria-selected", String(active));
    button.tabIndex = active ? 0 : -1;
  }
}

export function refresh() {
  if (!defaultValue) return;
  const neg = byId("promptTabNeg");
  neg.hidden = byId("negDetails").hidden;
  if (neg.hidden) select(false);
  neg.querySelector(".tab-dot").hidden = !negIsCustom(byId("negative").value, defaultValue());
}

export function init(getDefault) {
  defaultValue = getDefault;
  const pos = byId("promptTabPos"), neg = byId("promptTabNeg");
  for (const button of [pos, neg]) {
    button.addEventListener("click", () => select(button === neg));
    button.addEventListener("keydown", (event) => {
      if (neg.hidden || !["ArrowLeft", "ArrowRight"].includes(event.key)) return;
      event.preventDefault();
      select(button === pos);
      (button === pos ? neg : pos).focus();
    });
  }
  byId("negative").addEventListener("input", refresh);
  refresh();
}

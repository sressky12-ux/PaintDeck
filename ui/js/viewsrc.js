const inputModes = new Set(["img2img", "inpaint", "outpaint", "upscale", "strip", "sheet"]);

export function nextView({ mode, hasInput, hasResult, event, current, fromCurrentInput, masking }) {
  if (!inputModes.has(mode)) return { view: "result", hint: false };
  if (event === "result" && (masking || !fromCurrentInput)) return { view: current, hint: true };
  if (event === "result" || event === "select-result") return { view: "result", hint: false };
  if (event === "input" || event === "select-input" || !hasInput || !hasResult) return { view: "input", hint: false };
  return { view: current, hint: false };
}

export function resultLabel(index) {
  return index == null ? "結果" : `結果 #${index}`;
}

const indices = new Map();
let revision = 0, mode = "txt2img", view = "result", selected = null, deferred = null;
let display, enableActions;

// 送出時記住輸入圖的世代。即使使用者換回同名檔案，舊工作也不屬於新的一批。
export const inputRevision = () => revision;

function transition(event, fromCurrentInput = true) {
  return nextView({ mode, event, current: view, hasInput: !!document.getElementById("initImg").getAttribute("src"),
    hasResult: indices.size > 0, fromCurrentInput, masking: !document.getElementById("maskModal").hidden });
}

function paint(state) {
  const toggle = document.getElementById("viewSrc");
  view = state.view;
  toggle.hidden = !inputModes.has(mode);
  for (const button of toggle.querySelectorAll("[data-v]")) {
    button.setAttribute("aria-pressed", String(button.dataset.v === view));
  }
  toggle.querySelector(".tab-dot").hidden = !state.hint;
  toggle.querySelector('[data-v="result"] > span').textContent = resultLabel(indices.get((view === "input" && deferred ? deferred : selected)?.id));
  document.getElementById("initDrop").hidden = view !== "input";
  if (view === "input") document.getElementById("viewVid").pause();
  enableActions(view === "result" && !!selected);
}

export function init(show, setActions) {
  display = show;
  enableActions = setActions;
  document.getElementById("viewSrc").addEventListener("click", (e) => {
    const choice = e.target.closest("[data-v]")?.dataset.v;
    if (!choice) return;
    if (choice === "result") select(deferred || selected);
    else paint(transition("select-input"));
  });
}

export function onMode(value) {
  if (mode === value) return;
  mode = value;
  if (!inputModes.has(mode) && deferred) select(deferred);
  else paint(transition("mode"));
}

export function onInput() {
  revision++;
  indices.clear();
  deferred = null;
  paint(transition("input"));
}

export function select(rec) {
  selected = rec;
  deferred = null;
  paint(transition("select-result"));
  if (rec) display(rec);
}

export function onResult(rec, sourceRevision) {
  const matches = sourceRevision === revision;
  if (matches && !indices.has(rec.id)) indices.set(rec.id, indices.size + 1);
  const state = transition("result", matches);
  if (state.hint) {
    deferred = rec;
    paint(state);
  } else select(rec);
}

export function forget(ids) {
  if (deferred && ids.has(deferred.id)) {
    deferred = null;
    paint({ view, hint: false });
  }
}

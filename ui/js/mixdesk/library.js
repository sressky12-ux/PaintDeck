// 樂器庫分頁：mixdesk 全部樂器（instruments --json），左邊分類、上面搜尋，每個都能 ▶ 試聽。
// 拖到時間軸空白處＝新增一軌，拖到軌名上＝換樂器（timeline.js 收 drop）；也可以直接按「＋新軌」「⇄ 換到選取的軌」。
import * as api from "./api.js";
import { el } from "./ui.js";

export class Library {
  /** h：{audition(key, notes|null), addTrack(key), setInstrument(key), selectedTrack()} */
  constructor(root, h) {
    this.root = root;
    this.h = h;
    this.data = null;
    this.cat = "";
    this.q = "";
  }

  async show() {
    if (!this.data) {
      this.root.replaceChildren(el("div", "pr-hint", "讀樂器清單中…"));
      try {
        this.data = await api.get("/api/mixdesk/instruments");
      } catch (e) {
        this.root.replaceChildren(el("div", "pr-hint", `讀不到樂器清單：${e.message}`));
        return;
      }
    }
    this.render();
  }

  render() {
    const { categories, items } = this.data;
    const wrap = el("div", "lib");
    const cats = el("div", "lib-cats");
    for (const [k, name] of [["", `全部（${items.length}）`], ...Object.entries(categories)]) {
      const n = k ? items.filter((i) => i.category === k).length : 0;
      const b = el("button", k === this.cat ? "on" : "", k ? `${name}（${n}）` : name);
      b.type = "button";
      b.onclick = () => { this.cat = k; this.render(); };
      cats.append(b);
    }
    const main = el("div", "lib-main");
    const q = el("input");
    q.type = "search";
    q.placeholder = "搜尋樂器（中文、英文、key 都可以）";
    q.value = this.q;
    const list = el("div", "lib-items");
    const fill = () => {
      const kw = this.q.trim().toLowerCase();
      const sel = this.h.selectedTrack();
      list.replaceChildren();
      const shown = items.filter((i) => (!this.cat || i.category === this.cat)
        && (!kw || i.key.toLowerCase().includes(kw) || i.names.some((n) => n.toLowerCase().includes(kw))));
      for (const i of shown.slice(0, 400)) {
        const it = el("div", "lib-item");
        it.draggable = true;
        it.title = `${i.names.join("、")}\nkey：${i.key}${i.range ? `\n音域 ${i.range[0]}～${i.range[1]}` : ""}\n拖到時間軸空白處新增一軌；拖到軌名上換樂器`;
        it.ondragstart = (e) => { e.dataTransfer.setData("text/x-mixdesk-instrument", i.key); e.dataTransfer.effectAllowed = "copy"; };
        const nm = el("span", "nm", i.names[0]);
        const src = el("span", "src", (i.is_drums ? "鼓組・" : "") + i.source);
        const play = el("button", "", "▶");
        play.type = "button";
        play.title = i.is_drums ? "試聽一小段鼓" : "試聽一個和弦";
        play.onclick = (e) => { e.stopPropagation(); this.h.audition(i.key, null); };
        const add = el("button", "", "＋");
        add.type = "button";
        add.title = "用這個樂器新增一軌";
        add.onclick = (e) => { e.stopPropagation(); this.h.addTrack(i.key); };
        it.append(nm, src, play, add);
        if (sel) {
          const sw = el("button", "", "⇄");
          sw.type = "button";
          sw.title = `把「${sel}」換成這個樂器`;
          sw.onclick = (e) => { e.stopPropagation(); this.h.setInstrument(i.key); };
          it.append(sw);
        }
        list.append(it);
      }
      if (shown.length > 400) list.append(el("div", "pr-hint", `還有 ${shown.length - 400} 個，打字縮小範圍`));
      if (!shown.length) list.append(el("div", "pr-hint", "找不到。換個關鍵字試試（例如：piano、鋼琴、二胡、synth）"));
    };
    q.oninput = () => { this.q = q.value; fill(); };
    main.append(q, list, el("div", "pr-hint", "▶ 試聽（第一次要合成，等一下下）　＋ 新增一軌　⇄ 換掉時間軸上選取的那軌　也可以直接拖到時間軸上"));
    wrap.append(cats, main);
    this.root.replaceChildren(wrap);
    fill();
  }
}

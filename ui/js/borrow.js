// 借面板（2026-11-17 左邊「詞庫」頁＝生成頁分身）：進隨機頁時把生成頁那幾塊真的面板搬過去，離開時原封不動放回去。
// 每塊原位留一個註解節點當書籤，還的時候放回書籤後面再拿掉書籤；dock.js 只拿外面的 dock-panel 殼，裡面的內容搬走不影響版面資料。
// 同一組元素、同一份監聽，模型、LoRA 權重、AI 改寫的版本紀錄都跟生成頁一模一樣，不用另外同步。

/** items：要借的元素（照這個順序放進 host）；mark：做書籤（測試換假的） */
export function makeBorrower(items, host, mark = () => document.createComment("借到隨機頁")) {
  const marks = new Map();
  return {
    get lent() { return marks.size > 0; },
    lend() {
      if (marks.size) return;
      for (const it of items) {
        const m = mark();
        it.before(m);
        marks.set(it, m);
        host.append(it);
      }
    },
    giveBack() {
      for (const [it, m] of marks) { m.after(it); m.remove(); }
      marks.clear();
    },
  };
}

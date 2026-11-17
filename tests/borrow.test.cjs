// node --test --test-isolation=none tests/borrow.test.cjs
// 詞庫頁借生成頁的面板（ui/js/borrow.js）：借過去、原封不動還回原位
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load() {
  const source = fs.readFileSync(path.join(__dirname, '../ui/js/borrow.js'), 'utf8').replace(/^export /gm, '');
  const context = vm.createContext({});
  vm.runInContext(source + '\nglobalThis.subject = { makeBorrower };', context, { filename: 'borrow.js' });
  return context.subject;
}

// 最小的假 DOM：只有 before／after／append／remove
class N {
  constructor(name) { this.name = name; this.parent = null; this.kids = []; }
  _detach() { if (this.parent) { this.parent.kids.splice(this.parent.kids.indexOf(this), 1); this.parent = null; } }
  _put(parent, at) { this._detach(); parent.kids.splice(at, 0, this); this.parent = parent; }
  append(n) { n._put(this, this.kids.length); }
  before(n) { n._detach(); n._put(this.parent, this.parent.kids.indexOf(this)); }
  after(n) { n._detach(); n._put(this.parent, this.parent.kids.indexOf(this) + 1); }
  remove() { this._detach(); }
}
const names = (n) => n.kids.map((k) => k.name);

function setup() {
  const zoneA = new N('zoneA'), zoneB = new N('zoneB'), host = new N('host');
  const a = new N('a'), b = new N('b'), x = new N('x'), c = new N('c');
  zoneA.append(a); zoneA.append(x); zoneA.append(b);
  zoneB.append(c);
  let i = 0;
  const mk = () => new N('mark' + i++);
  return { zoneA, zoneB, host, a, b, c, mk };
}

const { makeBorrower } = load();

test('借：照順序搬到 host，原位留書籤', () => {
  const s = setup();
  const br = makeBorrower([s.a, s.c, s.b], s.host, s.mk);
  br.lend();
  assert.equal(br.lent, true);
  assert.deepEqual(names(s.host), ['a', 'c', 'b']);
  assert.deepEqual(names(s.zoneA), ['mark0', 'x', 'mark2']);
  assert.deepEqual(names(s.zoneB), ['mark1']);
});

test('還：每塊回到原來的位置，書籤拿掉', () => {
  const s = setup();
  const br = makeBorrower([s.a, s.c, s.b], s.host, s.mk);
  br.lend();
  br.giveBack();
  assert.equal(br.lent, false);
  assert.deepEqual(names(s.zoneA), ['a', 'x', 'b']);
  assert.deepEqual(names(s.zoneB), ['c']);
  assert.deepEqual(names(s.host), []);
});

test('重複借、重複還都不會壞；來回很多次結果一樣', () => {
  const s = setup();
  const br = makeBorrower([s.a, s.c, s.b], s.host, s.mk);
  br.giveBack();
  for (let k = 0; k < 10; k++) { br.lend(); br.lend(); br.giveBack(); br.giveBack(); }
  assert.deepEqual(names(s.zoneA), ['a', 'x', 'b']);
  assert.deepEqual(names(s.zoneB), ['c']);
  assert.deepEqual(names(s.host), []);
});

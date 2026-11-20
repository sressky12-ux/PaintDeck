const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const path=require('node:path');
function element(tag,attrs={},...children){return Object.assign({tag,children:children.flat(),value:'',options:[],checked:false,disabled:false,classList:{add(){},remove(){},toggle(){}},append(...x){this.children.push(...x);},replaceChildren(...x){this.children=x;},before(){},remove(){}},attrs);}
function subject(){
 const source=fs.readFileSync(path.join(__dirname,'../ui/js/take-compare.js'),'utf8').replace(/^import .*;\r?\n/gm,'').replace(/^export /gm,'');
 const context=vm.createContext({console,document:{body:{append(){}},addEventListener(){},removeEventListener(){}},el:element});
 vm.runInContext(source+`\nglobalThis.subject={syncPlan,stepTime};`,context);
 return context.subject;
}
test('syncPlan leaves the right side alone when both sides are within tolerance',()=>{
 const s=subject();
 assert.equal(s.syncPlan(1,1.05,10),null);
 assert.equal(s.syncPlan(4,4,10),null);
});
test('syncPlan pulls the right side to the left time when they drift apart',()=>{
 const s=subject();
 assert.equal(s.syncPlan(1.5,1,10),1.5);
 assert.equal(s.syncPlan(2,1.5,10),2);
});
test('syncPlan clamps to the right duration when the left time is past its end',()=>{
 const s=subject();
 assert.equal(s.syncPlan(12,1,10),10);
 assert.equal(s.syncPlan(9.5,0,9.2),9.2);
});
test('stepTime never goes before zero or past the end',()=>{
 const s=subject();
 assert.equal(s.stepTime(0,-1,10),0);
 assert.equal(s.stepTime(10,1,10),10);
 assert.equal(s.stepTime(1,1,10),1+1/24);
 assert.equal(s.stepTime(0.01,-1,10),0);
});

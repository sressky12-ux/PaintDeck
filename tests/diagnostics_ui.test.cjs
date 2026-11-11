const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
function harness(handler=async()=>({available:true,policy:{success_days:1,error_days:3,max_mib:500},bytes:0,records:[]})) {
  const nodes=new Map(),calls=[],messages=[],listeners={},opened=[],links=[];
  function el(tag,attrs={},...children) {return Object.assign({tag,children,value:'',hidden:false,disabled:false,append(...v){this.children.push(...v);},replaceChildren(...v){this.children=v;},focus(){},click(){links.push(this);},remove(){}},attrs);}
  const $=id=>{if(!nodes.has(id))nodes.set(id,el('div'));return nodes.get(id);};
  const context=vm.createContext({$,el,api:async(url,opts={})=>{calls.push([url,opts]);return handler(url,opts);},toast:(m,k)=>messages.push([m,k]),fmtBytes:n=>`${n} B`,openModal:id=>opened.push(id),closeModal(){},
    // 2026-11-05 起本機（127.0.0.1）改成直接存進下載資料夾；這幾個測試驗的是「瀏覽器下載」那條路，當成從別台電腦開的
    location:{hostname:'remote.example'},
    document:{documentElement:{lang:'en'},body:el('body'),addEventListener:(n,fn)=>listeners[n]=fn},
    fetch:async()=>{throw Error('fixture network');},AbortController,setTimeout:()=>1,clearTimeout(){},URL:{createObjectURL:()=> 'blob:fixture',revokeObjectURL(){}},confirm:()=>false,console});
  const source=fs.readFileSync(path.join(__dirname,'../ui/js/diagnostics.js'),'utf8').replace(/^import .*;\r?\n/gm,'').replace(/^export /gm,'');
  vm.runInContext(source+'\nglobalThis.subject={init,scan,download,downloadForJob,onEvent,save,clear,report,saveReport};',context);
  return {s:context.subject,$,calls,messages,listeners,opened,links,context};
}
test('late status responses do not overwrite newer status or unsaved fields',async()=>{
  const resolvers=[];let first=true;const h=harness(()=>{if(first){first=false;return {available:true,policy:{success_days:1,error_days:3,max_mib:500},bytes:0,records:[]};}return new Promise(resolve=>resolvers.push(resolve));});h.s.init();
  const a=h.s.scan(),b=h.s.scan();
  resolvers[1]({available:true,policy:{success_days:1,error_days:3,max_mib:500},bytes:222,records:[]});await b;
  h.$('#diagError').value='2';h.$('#diagError').oninput();
  resolvers[0]({available:true,policy:{success_days:5,error_days:5,max_mib:500},bytes:111,records:[]});await a;
  assert.match(h.$('#diagSummary').textContent,/222 B/);assert.equal(h.$('#diagError').value,'2');
});
test('invalid retention is rejected and clearing completed logs requires confirmation',async()=>{
  const h=harness();h.s.init();await h.s.scan();h.$('#diagSuccess').value='0';await h.s.save();
  assert(!h.calls.some(([p])=>p.endsWith('/preferences')));assert.equal(h.messages.at(-1)[1],'err');
  await h.s.clear(true);assert(!h.calls.some(([p])=>p.endsWith('/clear')));
});
test('download errors remain visible and allow retry, successful downloads are ZIP files',async()=>{
  const h=harness();let count=0;h.context.fetch=async()=>{count++;return {ok:false,status:400,json:async()=>({error:'fixture quota'})};};
  await h.s.download('diag-fixture');await h.s.download('diag-fixture');assert.equal(count,2);assert.match(h.messages.at(-1)[0],/fixture quota/);assert.equal(h.links.length,0);
  h.context.fetch=async()=>({ok:true,headers:{get:()=> 'application/zip'},blob:async()=>({})});await h.s.download('diag-fixture');assert.equal(h.links[0].download,'paintdeck-diag-fixture.zip');
});
test('simultaneous download clicks create one request and reject invalid identifiers',async()=>{
  const h=harness();let finish,count=0;h.context.fetch=()=>{count++;return new Promise(r=>finish=r);};
  const a=h.s.download('diag-fixture');await h.s.download('diag-fixture');await h.s.download('../outside');assert.equal(count,1);
  finish({ok:false,status:500,json:async()=>({error:'fixture'})});await a;
});
test('error notice offers download and manual mismatch report; failed reports stay open',async()=>{
  const h=harness(async()=>{throw Error('fixture disk full');});h.s.init();h.s.onEvent({phase:'error',diagnostic_id:'diag-fixture'});
  assert.equal(h.$('#diagNotice').hidden,false);h.s.report('diag-fixture');assert.equal(h.opened[0],'#diagReportModal');
  h.$('#diagExpected').value='three views';h.$('#diagActual').value='one view';await h.s.saveReport();
  assert.match(h.messages.at(-1)[0],/fixture disk full/);assert.equal(h.$('#diagReportSave').disabled,false);
});

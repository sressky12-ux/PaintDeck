const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const helper = fs.readFileSync(path.join(__dirname, '../ui/js/setup-reuse.js'), 'utf8');
const source = fs.readFileSync(path.join(__dirname, '../ui/js/setup.js'), 'utf8');
const context = vm.createContext({});
vm.runInContext(helper.replace(/^export /gm, ''), context);
const scan = (data=[],comfy=[],current=null) => ({data,comfy,current});
const data = (path,images=0) => ({path,images,valid:true});

test('原設定優先，失效原路徑也不偷偷切到新圖庫', () => {
  const current={data_root:'D:\\offline',comfy_dir:'D:\\original-comfy'};
  const result=context.reuseDefaults(scan([data('D:\\other',100000)],[],current));
  assert.equal(result.data,current.data_root); assert.equal(result.comfy,current.comfy_dir);
});
test('多份圖庫不以圖片數量挑一份；單一有效圖庫才預選', () => {
  assert.equal(context.reuseDefaults(scan([data('D:\\a',1),data('D:\\b',1000)])).data,'');
  assert.equal(context.reuseDefaults(scan([data('D:\\a',0)])).data,'D:\\a');
  assert.equal(context.reuseDefaults(scan([{path:'D:\\broken',valid:false}])).data,'');
  assert.equal(context.reuseDefaults(scan([data('D:\\a'),{path:'D:\\broken',valid:false}])).data,'');
  assert.equal(context.reuseDefaults(scan()).data,'new');
});
test('多份 ComfyUI 不挑第一份；資料的已知配對優先', () => {
  const comfy=[{path:'D:\\c1'},{path:'D:\\c2'}];
  assert.equal(context.reuseDefaults(scan([data('D:\\a')],comfy)).comfy,'manual');
  assert.equal(context.reuseDefaults(scan([{...data('D:\\a'),comfy:'D:\\c2'}],comfy)).comfy,'D:\\c2');
});
test('手動資料路徑去除複製的引號，並保留沿用意圖', () => {
  const r=context.locationRequest({data:'manual',dataManual:' "D:\\old\\data" ',comfy:'manual',comfyManual:' "D:\\comfy" '});
  assert.equal(r.data_root,'D:\\old\\data'); assert.equal(r.data_mode,'reuse');
  assert.equal(r.comfy_dir,'D:\\comfy');
});
test('修改位置立即鎖住下一步，較早的檢查晚回也不能放行', async () => {
  const nodes=new Map(); const F=id=>{if(!nodes.has(id))nodes.set(id,{});return nodes.get(id);};
  const pending=[],scheduled=[];
  const c=vm.createContext({F,chk:{data:{valid:true}},jumpModels:false,
    chosen:()=>({data_root:c.path}),path:'D:\\a',
    debounce:fn=>(...args)=>scheduled.push(()=>fn(...args)),
    api:async (url,opts)=>new Promise(resolve=>pending.push({opts,resolve})),
    renderCheck(){},placesReady:()=>true,esc:s=>s,
  });
  vm.runInContext(source.slice(source.indexOf('let checkSeq = 0;'),source.indexOf('function placesReady()')),c);
  c.recheck(); assert.equal(c.chk,null); assert.equal(F('nextBtn').disabled,true);
  const old=scheduled.shift()(); c.path='D:\\b'; c.recheck();
  pending[0].resolve({old:true}); await old;
  assert.equal(c.chk,null); assert.equal(F('nextBtn').disabled,true);
  const latest=scheduled.shift()(); pending[1].resolve({latest:true}); await latest;
  assert.equal(c.chk.latest,true); assert.equal(F('nextBtn').disabled,false);
});
test('失效舊資料即使 ComfyUI 正常也不能進下一步', () => {
  const nodes={navHint:{}};
  const c=vm.createContext({F:id=>nodes[id],pick:{data:'manual',comfy:'D:\\comfy'},
    chk:{data:{valid:false,problem:'missing gallery'},comfy:{ok:true},gpu:{},variant:'nvidia'}});
  vm.runInContext(source.slice(source.indexOf('function placesReady()'),source.indexOf('// 說明另外包一層')),c);
  assert.equal(c.placesReady(),false); assert.equal(nodes.navHint.textContent,'missing gallery');
});

test('只有舊安裝紀錄時先請使用者指定舊資料，不預選空白安裝', () => {
  const r=context.reuseDefaults({...scan(),installations:[{legacy:true}]});
  assert.equal(r.data,'manual'); assert.equal(r.comfy,'manual');
});

test('第 3 步只掃指定的資料夾：加入時去引號、不重複，整台電腦不帶 roots', () => {
  let list = context.addScanDir([], ' "D:\\SD\\models" ');
  assert.deepEqual([...list], ['D:\\SD\\models']);
  assert.equal(context.addScanDir(list, 'd:/sd/models/'), list); // 大小寫、斜線不同也算同一個
  assert.equal(context.addScanDir(list, '   '), list);
  list = context.addScanDir(list, 'E:\\LoRA');
  assert.deepEqual([...list], ['D:\\SD\\models', 'E:\\LoRA']);
  assert.deepEqual(JSON.parse(JSON.stringify(context.scanRequest('all', list))), {});
  assert.deepEqual(JSON.parse(JSON.stringify(context.scanRequest('dirs', [...list, ' ']))), {roots: ['D:\\SD\\models', 'E:\\LoRA']});
});

// 衣姿 v8：动效与小功能
(()=>{const A=window.APP,$=s=>document.querySelector(s),$$=s=>document.querySelectorAll(s),RM=matchMedia('(prefers-reduced-motion: reduce)').matches,EZ='cubic-bezier(.16,1,.3,1)',grid=$('#grid');
const LS=k=>{try{return JSON.parse(localStorage[k]||'null')}catch(e){return null}},SV=(k,v)=>{try{localStorage[k]=JSON.stringify(v)}catch(e){}};
const dl=(blob,name)=>{const a=document.createElement('a');a.href=URL.createObjectURL(blob);a.download=name;a.click();setTimeout(()=>URL.revokeObjectURL(a.href),3000)};
const X=window.X={dl};
// —— 爱心 pop + 粒子
function burst(el){if(RM||!el)return;const r=el.getBoundingClientRect(),cx=r.left+r.width/2,cy=r.top+r.height/2;for(let i=0;i<8;i++){const p=document.createElement('i');p.className='pt';p.style.cssText=`left:${cx}px;top:${cy}px;background:${i%2?'#ff2442':'#ff8fa3'}`;document.body.append(p);const a=i/8*Math.PI*2,d=18+Math.random()*10;p.animate([{transform:'translate(-50%,-50%) scale(1)',opacity:1},{transform:`translate(calc(-50% + ${Math.cos(a)*d}px),calc(-50% + ${Math.sin(a)*d}px)) scale(.2)`,opacity:0}],{duration:520,easing:EZ}).finished.then(()=>p.remove())}}
function pop(el){if(!RM&&el)el.animate([{transform:'scale(1)'},{transform:'scale(1.32)'},{transform:'scale(.92)'},{transform:'scale(1)'}],{duration:460,easing:EZ})}
function fly(img,target){if(RM||!img||!target)return;const a=img.getBoundingClientRect(),b=target.getBoundingClientRect();if(!a.width)return;const c=img.cloneNode();c.className='flyimg';c.style.cssText=`left:${a.left}px;top:${a.top}px;width:${a.width}px;height:${a.height}px`;document.body.append(c);
 const dx=b.left+b.width/2-(a.left+a.width/2),dy=b.top+b.height/2-(a.top+a.height/2);c.animate([{transform:'none',opacity:1,borderRadius:'14px'},{transform:`translate(${dx*.6}px,${dy*.6-40}px) scale(.35)`,opacity:.9,offset:.6},{transform:`translate(${dx}px,${dy}px) scale(.06)`,opacity:.2,borderRadius:'50%'}],{duration:640,easing:'cubic-bezier(.5,0,.2,1)'}).finished.then(()=>{c.remove();pop(target)})}
X.fly=fly;X.pop=pop;
const favTab=()=>$('.tabs button[data-tab=fav]'),stuTab=()=>$('.tabs button[data-tab=studio]');
grid.addEventListener('click',e=>{const b=e.target.closest('[data-a]');const card=e.target.closest('.card');if(!card)return;
 if(document.body.classList.contains('batch')){e.preventDefault();e.stopPropagation();card.classList.toggle('picked');pick(card.dataset.k);return}
 if(b&&b.dataset.a=='fav'){const k=card.dataset.k,on=!A.st.fav.has(k),img=card.querySelector('img');requestAnimationFrame(()=>{const nb=grid.querySelector(`.card[data-k="${CSS.escape(k)}"] [data-a=fav]`);if(on){pop(nb);burst(nb);fly(img,favTab())}})}
 if(b&&b.dataset.a=='sel'){const k=card.dataset.k;if(!A.st.sel.includes(k))fly(card.querySelector('img'),$('#tray:not([hidden])')||stuTab())}},true);
// —— 批量模式
const B=new Set();function pick(k){B.has(k)?B.delete(k):B.add(k);bbar()}
const tb=$('.tbar'),bt=document.createElement('button');bt.className='fbtn';bt.id='batchBtn';bt.innerHTML='<svg viewBox="0 0 24 24"><rect x="4" y="4" width="7" height="7" rx="2"/><rect x="13" y="4" width="7" height="7" rx="2"/><rect x="4" y="13" width="7" height="7" rx="2"/><path d="m14 16.5 2 2 4-4"/></svg>批量';tb.querySelector('.sp').after(bt);
const bb=document.createElement('div');bb.id='bbar';bb.hidden=true;bb.innerHTML='<b id="bbN">已选 0</b><button data-b="all">全选</button><button data-b="fav">收藏</button><button data-b="unfav">取消收藏</button><button data-b="zip">打包下载</button><button data-b="stu">去试穿</button><button data-b="cp">复制</button><button data-b="x" class="pri">完成</button>';document.body.append(bb);
function batch(on){document.body.classList.toggle('batch',on);bb.querySelector('[data-b=fav]').hidden=A.st.tab=='fav';bb.querySelector('[data-b=unfav]').hidden=A.st.tab!='fav';bb.hidden=!on;bt.classList.toggle('on',on);if(!on){B.clear();$$('.card.picked').forEach(c=>c.classList.remove('picked'))}bbar()}
function bbar(){$('#bbN').textContent='已选 '+B.size;bb.querySelectorAll('[data-b]:not([data-b=x]):not([data-b=all])').forEach(b=>b.disabled=!B.size)}
bt.onclick=()=>batch(!document.body.classList.contains('batch'));
bb.onclick=async e=>{const t=e.target.closest('[data-b]');if(!t)return;const its=[...B].map(k=>A.ITEMS[k]).filter(Boolean),m=t.dataset.b;
 if(m=='x')return batch(false);if(m=='all'){$$('#grid .card').forEach(c=>{c.classList.add('picked');B.add(c.dataset.k)});return bbar()}
 if(m=='fav'){let n=0;its.forEach(it=>{if(!A.st.fav.has(it.k)){A.st.fav.add(it.k);n++}});SV('fav',[...A.st.fav]);$('#nFav').textContent=A.st.fav.size;pop(favTab());A.toast(`已收藏 ${n} 项`);batch(false);A.apply()}
 if(m=='unfav'){its.forEach(it=>A.st.fav.delete(it.k));SV('fav',[...A.st.fav]);const n=its.length,ks=its.map(i=>i.k);batch(false);A.apply();$('#nFav').textContent=A.st.fav.size||'';A.toast(`已取消 ${n} 项收藏`,()=>{ks.forEach(k=>A.st.fav.add(k));SV('fav',[...A.st.fav]);$('#nFav').textContent=A.st.fav.size;A.apply()})}
 if(m=='zip'){const local=its.filter(it=>!it.ext);if(!local.length)return A.toast(its.length?'这些图片在原站，请前往原站查看':'先选几张');if(!window.makeZip)return;t.disabled=true;t.textContent='打包中…';try{const fs=[];for(const it of local.slice(0,60)){const u=A.big(it);const r=await fetch(u);if(r.ok)fs.push({name:`posewear-${it.type}-${it.id}.webp`,blob:await r.blob()})}window.X.dl(await makeZip(fs),`posewear-${A.st.tab}-${fs.length}.zip`);A.toast(`已打包 ${fs.length} 张`+(local.length>60?'（单次最多 60 张）':'')+(local.length<its.length?'，深度姿势请到原站查看':''))}catch(err){A.toast('打包失败，请重试')}t.disabled=false;t.textContent='打包下载'}
 if(m=='cp'){A.copy(its.map(it=>A.prompt(it,A.st.lang=='en'?'en':'zh')).join('\n'),`${its.length} 条提示词`)}
 if(m=='stu'){const p=its.filter(i=>i.type=='pose'),c=its.filter(i=>i.type=='costume'||i.type=='anime'),r=its.filter(i=>i.type=='portrait');batch(false);for(const it of [...r.slice(0,3),...c.slice(0,1),...p.slice(0,6)])await window.WB.pick(it);if(!its.length)A.setTab('studio')}};
addEventListener('keydown',e=>{if(e.key=='Escape'&&document.body.classList.contains('batch'))batch(false)});
const _st=A.setTab;A.setTab=(...a)=>{if(document.body.classList.contains('batch'))batch(false);return _st(...a)};
$('.tabs').addEventListener('click',()=>document.body.classList.contains('batch')&&batch(false),true);
// —— 搜索建议
const HOT={pose:['站姿','坐姿','回头','可爱','躺卧','跪立','俯拍','蹲姿'],depth:['站姿','坐姿','上半身','全身','俯拍','侧面','可爱','跪立'],costume:['连衣裙','黑色','泳装','针织','蕾丝','冬季','牛仔','高跟鞋'],anime:['连衣裙','黑色','泳装','针织','蕾丝','冬季','牛仔','制服'],portrait:['短发','波波','狼尾','刘海','卷','奶茶色','黑发','生活照']};
const q=$('#q'),sg=document.createElement('div');sg.id='qsug';sg.hidden=true;q.parentNode.append(sg);
function sgDraw(){const h=LS('qh')||[],v=q.value.trim(),tab=['costume','anime','portrait','depth'].includes(A.st.tab)?A.st.tab:'pose',hot=(HOT[tab]||[]).filter(x=>!v||x.includes(v));
 sg.innerHTML=(h.length&&!v?`<div class="sgh"><small>最近搜索</small><button data-clr>清除</button></div><div class="sgc">${h.map(x=>`<button data-q="${A.esc(x)}">${A.esc(x)}</button>`).join('')}</div>`:'')+(hot.length?`<div class="sgh"><small>热门</small></div><div class="sgc">${hot.map((x,i)=>`<button data-q="${x}"><em>${i+1}</em>${x}</button>`).join('')}</div>`:'');sg.hidden=!sg.innerHTML}
const saveQ=()=>{const v=q.value.trim();if(v.length<1)return;SV('qh',[v,...(LS('qh')||[]).filter(x=>x!=v)].slice(0,8))};
q.addEventListener('focus',sgDraw);q.addEventListener('input',()=>{sgDraw();clearTimeout(q._h);q._h=setTimeout(saveQ,1500)});q.addEventListener('keydown',e=>{if(e.key=='Enter'){saveQ();sg.hidden=true;q.blur()}});
q.addEventListener('blur',()=>setTimeout(()=>{sg.hidden=true},150));
sg.addEventListener('mousedown',e=>e.preventDefault());sg.onclick=e=>{const b=e.target.closest('button');if(!b)return;if(b.dataset.clr!=null){SV('qh',[]);return sgDraw()}if(!['pose','costume','anime','portrait','depth'].includes(A.st.tab)&&b.dataset.q)A.setTab('pose');q.value=b.dataset.q;q.dispatchEvent(new Event('input'));saveQ();sg.hidden=true;q.blur()};
// —— 计数动画
const cnt=$('#count');new MutationObserver(()=>{if(!RM)cnt.animate([{opacity:.2,transform:'translateY(4px)'},{opacity:1,transform:'none'}],{duration:360,easing:EZ})}).observe(cnt,{childList:true,characterData:true,subtree:true});
// —— 大图：相似推荐 / 下载 / 复制图片
X.lb=it=>{const L=A.st.list,cols=new Set((it.zh||'').match(/[黑白红粉蓝灰绿黄紫金银棕米]/g)||[]);const sc=x=>it.type=='portrait'?(x.len==it.len?2:0)+(x.st&&x.st==it.st?4:0)+(x.sec==it.sec?1:0):(x.cat==it.cat?3:0)+(x.line&&x.line==it.line?1:0)+((x.zh||'').match(/[黑白红粉蓝灰绿黄紫金银棕米]/g)||[]).filter(c=>cols.has(c)).length;
 const s=L.map((x,i)=>[x,i]).filter(([x])=>x.k!=it.k&&x.type==it.type).map(([x,i])=>[x,i,sc(x)+Math.random()*.5]).sort((a,b)=>b[2]-a[2]).slice(0,8);
 const el=$('#lbSim');if(!el)return;el.innerHTML=s.length?`<small class="simh">相似推荐</small><div class="sim">${s.map(([x,i])=>`<button data-si="${i}" title="${A.esc(A.title(x))}"><img src="${A.esc(A.thumb(x))}" alt="${A.esc(A.title(x))}" loading="lazy"${x.ref?` referrerpolicy="${A.esc(x.ref)}"`:''}></button>`).join('')}</div>`:''};
async function curBlob(){const it=A.st.list[A.li()];const r=await fetch(A.big(it));return[await r.blob(),it]}
$('#lbSide').addEventListener('click',async e=>{const b=e.target.closest('button');if(!b)return;
 if(b.dataset.si){const i=+b.dataset.si;A.lbOpen(i);if(!RM)$('#lbImg').animate([{opacity:0,transform:'scale(.97)'},{opacity:1,transform:'none'}],{duration:340,easing:EZ});return}
 if(b.dataset.x=='dlx'){try{const[bl,it]=await curBlob();dl(bl,`posewear-${it.type}-${it.id}.webp`);A.toast('已开始下载')}catch(err){A.toast('下载失败')}}
 if(b.dataset.x=='cpimg'){try{const[bl]=await curBlob();const im=await createImageBitmap(bl),c=document.createElement('canvas');c.width=im.width;c.height=im.height;c.getContext('2d').drawImage(im,0,0);const png=await new Promise(r=>c.toBlob(r,'image/png'));await navigator.clipboard.write([new ClipboardItem({'image/png':png})]);A.toast('图片已复制到剪贴板')}catch(err){A.toast('当前浏览器不支持复制图片')}}});
// —— 随机：洗牌动画
const rb=$('#rand');if(rb)rb.addEventListener('click',()=>{if(RM)return;const cs=[...$$('#grid .card')].filter(c=>{const r=c.getBoundingClientRect();return r.top<innerHeight&&r.bottom>0}).slice(0,12);cs.forEach((c,i)=>c.animate([{transform:'none'},{transform:`translateY(-6px) rotate(${(i%2?1:-1)*1.2}deg)`},{transform:'none'}],{duration:420,delay:i*22,easing:EZ}))},true);
// —— 收藏/最近为空的 CTA
// —— PWA 更新提示
if('serviceWorker'in navigator&&navigator.serviceWorker.controller){navigator.serviceWorker.addEventListener('controllerchange',()=>{const u=document.createElement('button');u.id='upd';u.innerHTML='<b>●</b> 有新版本，点击刷新';u.onclick=()=>location.reload();document.body.append(u)})}
})();

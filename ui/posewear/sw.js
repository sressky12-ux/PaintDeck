const V='posewear-pd-70f48e7';self.addEventListener('install',e=>{self.skipWaiting();e.waitUntil(caches.open(V).then(c=>c.addAll(['./','index.html','style.css','app.js','studio.js','extras.js','zip.js','favicon.svg'])))});
self.addEventListener('activate',e=>e.waitUntil(caches.keys().then(ks=>Promise.all(ks.filter(k=>k!=V).map(k=>caches.delete(k))))));
self.addEventListener('fetch',e=>{const u=new URL(e.request.url);if(u.origin!=location.origin||u.pathname.startsWith('/api/')||e.request.method!='GET')return;
 const img=/\/(p|c)\/(t|i|m)\//.test(u.pathname);e.respondWith(img?caches.match(e.request).then(r=>r||fetch(e.request).then(res=>{const cl=res.clone();caches.open(V+'-img').then(c=>c.put(e.request,cl));return res}))
 :fetch(e.request).then(res=>{const cl=res.clone();caches.open(V).then(c=>c.put(e.request,cl));return res}).catch(()=>caches.match(e.request)))});

// 极简 store-only ZIP（无压缩，PNG/WebP 本身已压缩）
(()=>{const T=new Uint32Array(256);for(let n=0;n<256;n++){let c=n;for(let k=0;k<8;k++)c=c&1?0xEDB88320^(c>>>1):c>>>1;T[n]=c>>>0}
const crc=b=>{let c=~0;for(let i=0;i<b.length;i++)c=T[(c^b[i])&255]^(c>>>8);return(~c)>>>0};
window.makeZip=async files=>{const enc=new TextEncoder(),parts=[],cen=[];let off=0;const d=new Date(),dt=((d.getFullYear()-1980)<<9|(d.getMonth()+1)<<5|d.getDate()),tm=(d.getHours()<<11|d.getMinutes()<<5|d.getSeconds()>>1);
 for(const f of files){const data=new Uint8Array(await f.blob.arrayBuffer()),name=enc.encode(f.name),c=crc(data),h=new DataView(new ArrayBuffer(30));
  h.setUint32(0,0x04034b50,true);h.setUint16(4,20,true);h.setUint16(6,0x0800,true);h.setUint16(10,tm,true);h.setUint16(12,dt,true);h.setUint32(14,c,true);h.setUint32(18,data.length,true);h.setUint32(22,data.length,true);h.setUint16(26,name.length,true);
  parts.push(h,name,data);const ce=new DataView(new ArrayBuffer(46));ce.setUint32(0,0x02014b50,true);ce.setUint16(4,20,true);ce.setUint16(6,20,true);ce.setUint16(8,0x0800,true);ce.setUint16(12,tm,true);ce.setUint16(14,dt,true);ce.setUint32(16,c,true);ce.setUint32(20,data.length,true);ce.setUint32(24,data.length,true);ce.setUint16(28,name.length,true);ce.setUint32(42,off,true);cen.push(ce,name);off+=30+name.length+data.length}
 const cs=cen.reduce((s,x)=>s+x.byteLength,0),e=new DataView(new ArrayBuffer(22));e.setUint32(0,0x06054b50,true);e.setUint16(8,files.length,true);e.setUint16(10,files.length,true);e.setUint32(12,cs,true);e.setUint32(16,off,true);
 return new Blob([...parts,...cen,e],{type:'application/zip'})}})();

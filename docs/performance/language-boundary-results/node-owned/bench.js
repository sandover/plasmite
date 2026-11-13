const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const {spawnSync}=require('node:child_process');
const root=process.env.PLASMITE_BENCH_ROOT || path.resolve(__dirname, '../../../..');
async function worker(addon, mode) {
 const {Pool}=require(root+'/bindings/node'); const native=require(addon);
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'plasmite-boundary-'));
 try {
  const client=new native.Client(dir),inner=client.createPool('bench',1048576n),pool=new Pool(inner);
  const iterations=Number(process.env.PLASMITE_BENCH_ITERATIONS || 20000), textBytes=Number(process.env.PLASMITE_BENCH_TEXT_BYTES || 32);
  const value={samples:Array.from({length:32},(_,i)=>({i,text:'x'.repeat(textBytes),value:i/3}))};
  const msg=pool.append(value);const expected=pool.getLite3(msg.seq).payload;
  for(let i=0;i<1000;i++)pool.getLite3(msg.seq);
  global.gc();const start=process.hrtime.bigint(),cpu=process.cpuUsage();let verified=0;
  for(let i=0;i<iterations;i++) {
   if(mode==='raw')assert.deepEqual(pool.getLite3(msg.seq).payload,expected);
   else assert.deepEqual(pool.get(msg.seq).data,value);
   verified++;
  }
  pool.close();client.close();global.gc();await new Promise(setImmediate);global.gc();await new Promise(setImmediate);
  console.log(JSON.stringify({addon,mode,verified,textBytes,ms:Number(process.hrtime.bigint()-start)/1e6,cpu:process.cpuUsage(cpu),memory:process.memoryUsage(),node:process.version}));
 }finally{fs.rmSync(dir,{recursive:true,force:true});}
}
if(process.argv[2]==='--worker')worker(process.argv[3],process.argv[4]).catch(e=>{console.error(e);process.exitCode=1;});
else{
 const rows=[];const candidates=process.argv.slice(2);
 for(const mode of ['raw','object'])for(let r=0;r<5;r++)for(const addon of (r%2?[...candidates].reverse():candidates)){
  const p=spawnSync(process.execPath,['--expose-gc',__filename,'--worker',addon,mode],{encoding:'utf8'});
  if(p.status!==0)throw new Error(p.stderr);rows.push(JSON.parse(p.stdout));
 }
 console.log(JSON.stringify({rows},null,2));
}

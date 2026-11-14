#!/usr/bin/env node
// One fresh process per observation; compare both packages in alternating order.
const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const root = path.resolve(process.env.PLASMITE_BENCH_BINDING || path.join(__dirname,'..'));
const start = process.hrtime.bigint();
const { Client, toNative } = require(root);
const requireUs = Number(process.hrtime.bigint()-start)/1000;
async function main() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'plasmite-cold-input-'));
  const client = new Client(dir), pool = client.createPool('input',64n*1048576n);
  const fixture = process.env.PLASMITE_COLD_FIXTURE || 'small';
  const fixtures = {
    small: () => ({ metrics:{power:42}, samples:Array.from({length:4},(_,value)=>({value})) }),
    wide: () => ({ metrics:{power:42}, samples:Array.from({length:128},(_,value)=>({value})), ...Object.fromEntries(Array.from({length:256},(_,i)=>['field'+i,'x'.repeat(64)])) }),
    text64k: () => ({ metrics:{power:42}, samples:[{value:1}], text:'x'.repeat(65536) }),
    array: () => ({ metrics:{power:42}, samples:Array.from({length:4096},(_,value)=>({value})) }),
  };
  if (!fixtures[fixture]) throw new Error('unknown cold fixture');
  const input = fixtures[fixture](), messages = [];
  const before = process.memoryUsage();
  const rows = [];
  try {
    for (let call=0;call<2;call++) {
      const cpu=process.cpuUsage(), started=process.hrtime.bigint();
      const message=await pool.appendView(input);
      if (message.data.metrics.power!==42) throw new Error('payload mismatch');
      const elapsedUs=Number(process.hrtime.bigint()-started)/1000, used=process.cpuUsage(cpu);
      rows.push({call,elapsedUs,cpuUs:used.user+used.system,memory:process.memoryUsage()});
      messages.push(message);
    }
    for (const message of messages) assert.deepEqual(toNative(message.data), input);
  } finally {pool.close();client.close();fs.rmSync(dir,{recursive:true,force:true});}
  const hashes={};
  for (const name of ['index.js','input.js','portable.js','portable/plasmite_core.wasm','native/darwin-arm64/index.node','native/darwin-arm64/libplasmite.dylib']) {
    hashes[name]=crypto.createHash('sha256').update(fs.readFileSync(path.join(root,name))).digest('hex');
  }
  const diagnostics = require(path.join(root,'portable')).diagnostics();
  const report={node:process.version,fixture,requireUs,before,rows,diagnostics,hashes,limits:['First call includes lazy module/codec initialization and one consumed field; second call is not a steady-state estimate.','Two returned snapshots stay live through verification. Memory snapshots include all process allocations and do not establish peak allocation counts.']};
  const json=JSON.stringify(report,null,2)+'\n';
  if(process.argv[2])fs.writeFileSync(process.argv[2],json);else process.stdout.write(json);
}
main().catch(error=>{console.error(error);process.exitCode=1;});

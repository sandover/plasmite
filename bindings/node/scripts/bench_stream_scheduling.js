#!/usr/bin/env node
// One isolated case per process; the coordinator alternates frozen packages.
const fs = require('node:fs');
const dns = require('node:dns');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const dgram = require('node:dgram');
const { spawn, execFileSync } = require('node:child_process');
const { monitorEventLoopDelay } = require('node:perf_hooks');
const assert = require('node:assert/strict');
const root = path.resolve(process.env.PLASMITE_BENCH_BINDING || path.join(__dirname, '..'));
const { Client, UdpClient } = require(root);
const count = Number(process.argv[2] || 4);
const messages = Number(process.argv[3] || 20);
const shared = process.argv.includes('--shared-pool');
const udp = process.argv.includes('--udp');
const owned = process.argv.includes('--owned');
const rareOnly = process.argv.includes('--rare-active-only');
const sampleThreads = process.argv.includes('--sample-threads');
function threads() {
  if (!sampleThreads || process.platform === 'win32') return null;
  try {
    const args=process.platform==='darwin'?['-M','-p',String(process.pid)]:['-L','-p',String(process.pid),'-o','tid='];
    const lines=execFileSync('/bin/ps',args,{encoding:'utf8'}).trim().split('\n');
    return {count:lines.length-(process.platform==='darwin'?1:0)};
  } catch (error) { return {count:null,error:error.message}; }
}
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const now = () => process.hrtime.bigint();
const us = ns => Number(ns) / 1000;
function summary(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  return Object.fromEntries([50,95,99].map(p => [`p${p}Us`, sorted[Math.ceil(sorted.length*p/100)-1] ?? null]));
}
function delta(cpu) { const d = process.cpuUsage(cpu); return d.user + d.system; }
async function main() {
  assert.ok(Number.isInteger(count) && count > 0 && Number.isInteger(messages) && messages > 0);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-stream-bench-'));
  const client = new Client(dir);
  const pools = Array.from({ length: shared ? 1 : count }, (_, i) => client.createPool(`p${i}`, 1048576n));
  const poolAt = i => pools[shared ? 0 : i];
  let server, transport;
  if (udp) {
    const held = dgram.createSocket('udp4');
    await new Promise(resolve => held.bind(0,'127.0.0.1',resolve));
    const port = held.address().port; await new Promise(resolve => held.close(resolve));
    const ready = path.join(dir,'ready'), home = path.join(dir,'home'); fs.mkdirSync(home);
    const binary = process.env.PLASMITE_BIN || path.resolve(__dirname,'../../../target/debug/plasmite');
    server = spawn(binary,['--dir',dir,'serve','--bind','127.0.0.1:0','--remote-bind','127.0.0.1:0',`--udp=127.0.0.1:${port}`,'--udp-follow','--max-tail-concurrency=512'],{env:{...process.env,HOME:home,PLASMITE_ACCESS_HOME:home,PLASMITE_SERVE_READY_FILE:ready},stdio:['ignore','ignore','pipe']});
    let stderr=''; server.stderr.on('data',chunk=>{stderr+=chunk;});
    const deadline=Date.now()+10000;
    while (!fs.existsSync(ready)) { if (server.exitCode!==null || Date.now()>deadline) throw new Error(stderr || 'server not ready'); await sleep(10); }
    transport = new UdpClient(`udp://127.0.0.1:${port}`);
  }
  const udpPools = udp ? pools.map((_,i)=>transport.openPool(`p${i}`)) : [];
  const poolNumber = i => shared ? 0 : i;
  async function open(i, limit, since) {
    if (!udp) {
      const reader = await poolAt(i)[owned ? "followView" : "follow"]({ since: since ?? "oldest" });
      let remaining = limit, closed = false;
      return { async next(ms) {
        if (remaining === 0) return { status: 2 };
        const result = await reader.next(ms / 1000);
        if (result.done) return { status: closed ? 3 : 2 };
        if (result.value === undefined) return { status: 0 };
        if (remaining !== null) remaining--;
        return { status: 1, message: result.value };
      }, close() { closed = true; reader.close(); } };
    }
    const follow = await udpPools[poolNumber(i)][owned ? 'followView' : 'follow']({ since: since ?? 1n });
    while (await follow.nextEvent(0)) {} // discard startup progress outside timing
    return {async next(ms) {
      const deadline = now()+BigInt(ms)*1000000n;
      for (;;) {
        const left=(deadline-now()+999999n)/1000000n;
        if (left<0n) return {status:0};
        const event=await follow.nextEvent(left);
        if (!event) return {status:follow.finished?3:0};
        if (event.kind==='frame') return {status:1,message:event.message};
      }
    },close:()=>follow.close()};
  }
  async function openAll(limit,since) {
    const streams=[];
    for (let i=0;i<count;i++) streams.push(await open(i,limit,since ? lastSeq[poolNumber(i)]+1n : null));
    return streams;
  }
  const results = {};
  const lastSeq = pools.map(() => 0n);
  try {
    if (!rareOnly) {
    // Idle C waits previously occupied the same libuv workers as these probes.
    let streams = await openAll(null,false);
    const loop = monitorEventLoopDelay({ resolution: 1 }); loop.enable();
    const threadBefore = threads();
    const before = process.memoryUsage(); const cpu = process.cpuUsage(); const start = now();
    const reads = streams.map(s => s.next(5000));
    const probes = [];
    await sleep(25);
    for (const [name, operation] of [['fs', () => fs.promises.stat(__filename)], ['dns', () => dns.promises.lookup('localhost')]]) {
      const started = now(); probes.push(operation().then(() => ({ name, us: us(now()-started) })));
    }
    await sleep(225);
    const idleCpuUs = delta(cpu); const idleWallUs = us(now()-start);
    const threadPending = threads();
    const pending = process.memoryUsage(); const cancelStart = now();
    streams.forEach(s => s.close());
    const statuses = await Promise.all(reads);
    assert.ok(statuses.every(s => s.status === 3));
    results.idle = { wallUs: idleWallUs, cpuUs: idleCpuUs, threadBefore, threadPending, resourceUsage:process.resourceUsage(), cancelAllUs: us(now()-cancelStart), probes: await Promise.all(probes), eventLoopP99Us: loop.percentile(99)/1000, eventLoopMaxUs: loop.max/1000, memoryBefore: before, memoryPending: pending, memoryAfterClose: process.memoryUsage() };
    loop.disable();
    for (const [name, interval] of [['paced', 10], ['saturated', 0]]) {
      streams = await openAll(messages,true);
      const sent = pools.map(() => []); const samples = [];
      const cpu = process.cpuUsage(), started = now();
      const readers = streams.map(async (stream, i) => {
        for (let j = 0; j < messages; j++) {
          const value = await stream.next(5000);
          const arrived = now(); assert.equal(value.status, 1);
          assert.equal((udp || owned ? value.message : JSON.parse(value.message)).data.number, j);
          samples.push(us(arrived-sent[shared ? 0 : i][j]));
        }
        if (!udp) assert.equal((await stream.next(0)).status, 2);
      });
      try {
        for (let j = 0; j < messages; j++) {
          for (let i = 0; i < pools.length; i++) { sent[i][j] = now(); lastSeq[i] = (await pools[i].feed({ number: j })).seq; }
          if (interval) await sleep(interval);
        }
        await Promise.all(readers);
        results[name] = { ...summary(samples), samplesUs: samples, cpuUs: delta(cpu), wallUs: us(now()-started), delivered: samples.length };
      } finally { streams.forEach(s => s.close()); }
    }
    }
    if (rareOnly) {
      assert.equal(shared, false, 'rare-active control requires independent pools');
      const trials = [];
      for (let trial = 0; trial < messages; trial++) {
        const streams = await openAll(1,true);
        const reads = streams.map(stream=>stream.next(5000));
        let timer;
        try {
          await sleep(25);
          const cpu = process.cpuUsage(), sentAt = now();
          const target = count-1;
          lastSeq[target] = (await pools[target].feed({number:trial})).seq;
          const delivery = reads[target].then(value=>({value,arrived:now()}));
          const observed = await Promise.race([delivery,new Promise(resolve=>{timer=setTimeout(()=>resolve(null),250);})]);
          clearTimeout(timer);
          const stoppedAt = now();
          // If libuv never runs the target, release the unrelated waits. Keep
          // that result separate from observed latency: the sample is censored.
          streams.forEach((stream,i)=>{if(i!==target)stream.close();});
          const result = observed || await delivery;
          assert.equal(result.value.status,1);
          assert.equal((udp || owned ? result.value.message : JSON.parse(result.value.message)).data.number,trial);
          trials.push({deliveredBeforeRelease:observed!==null,latencyUs:observed?us(observed.arrived-sentAt):null,
            censorAfterUs:observed?null:us(stoppedAt-sentAt),deliveryAfterReleaseUs:observed?null:us(result.arrived-stoppedAt),cpuUs:delta(cpu)});
        } finally {
          clearTimeout(timer);streams.forEach(stream=>stream.close());await Promise.allSettled(reads);
        }
      }
      const samples = trials.filter(trial=>trial.deliveredBeforeRelease).map(trial=>trial.latencyUs);
      results.rareActive = {...summary(samples),trials,censored:trials.length-samples.length,targetIndex:count-1,censorBudgetMs:250};
    }
  } finally {
    udpPools.forEach(p=>p.close()); transport?.close();
    if (server) {
      const exited = new Promise(resolve=>server.once('exit',resolve)); server.kill('SIGTERM');
      let timer; await Promise.race([exited,new Promise(resolve=>{timer=setTimeout(resolve,3000);})]);clearTimeout(timer);
      if (server.exitCode===null) server.kill('SIGKILL');
    }
    pools.forEach(p => p.close()); client.close(); fs.rmSync(dir,{recursive:true,force:true});
  }
  const hashes = {};
  for (const file of ['index.js','native/darwin-arm64/index.node','native/darwin-arm64/libplasmite.dylib']) {
    hashes[file] = crypto.createHash('sha256').update(fs.readFileSync(path.join(root,file))).digest('hex');
  }
  const report = {node:process.version, platform:process.platform, count, messages, rareOnly, harnessSha256:crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex'), mode:udp ? "udp" : "local", owned, sharedPool:shared, results, hashes, limits:['Consumer process CPU excludes the UDP server process. Legacy JSON stream result interfaces stay unchanged. Separate pools default; --shared-pool isolates shared fanout.', 'Owned mode uses followViews/followViews and reads native snapshots; JSON mode retains the legacy result interface. Comparing those modes changes conversion work.', 'Optional thread probes run outside the idle CPU window; RSS remains an observation, and maxRSS is a process lifetime maximum.', 'Native wait slices cannot interrupt an operating-system file lock.', 'CPU includes this process and filesystem/DNS probes; memory samples are observations rather than allocation counts.', 'Paced timestamps start before append and end on read completion, before JSON verification; scheduling and JSON formatting remain in the path.']};
  const json = JSON.stringify(report,null,2)+'\n';
  const output = process.argv.find(v => v.startsWith('--output='));
  if (output) fs.writeFileSync(output.slice(9),json); else process.stdout.write(json);
}
main().catch(error => { console.error(error); process.exitCode=1; });

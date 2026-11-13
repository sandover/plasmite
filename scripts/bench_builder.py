#!/usr/bin/env python3
"""Repeat matched C native-value producer paths; never build while measuring.

Build: cargo build --release --lib
       cc -O3 -Iinclude scripts/bench_builder_c.c -Ltarget/release -lplasmite \
         -Wl,-rpath,$PWD/target/release -o target/release/bench_builder_c
Run: python3 scripts/bench_builder.py --output result.json
Controlled fixtures contain ASCII strings, exact i64 values and binary64 1.25.
The JSON control serializes those same C values with snprintf, then parses once.
Both paths return and fully verify retained views. Producer timings exclude the
shared verification/release; complete timings include it. Two clock reads per
producer operation add the same timing overhead to both paths. This is not a ctypes throughput test.
"""
import argparse, hashlib, json, os, platform, statistics, subprocess
from pathlib import Path
from bench_language_boundaries import pool_directory
ROOT=Path(__file__).resolve().parents[1]
def sha(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--worker',type=Path,default=ROOT/'target/release/bench_builder_c')
    p.add_argument('--library',type=Path,default=ROOT/'target/release/libplasmite.dylib')
    p.add_argument('--repeats',type=int,default=7);p.add_argument('--output',type=Path,required=True)
    a=p.parse_args();assert a.repeats>0
    env=dict(os.environ);env['DYLD_LIBRARY_PATH']=str(a.library.resolve().parent);env['LD_LIBRARY_PATH']=str(a.library.resolve().parent)
    result={'platform':platform.platform(),'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),'worker_sha256':sha(a.worker),'library_sha256':sha(a.library),'runner_sha256':sha(__file__),'worker_source_sha256':sha(ROOT/'scripts/bench_builder_c.c'),'runs':[],'summary':{}}
    for shape,n in [('small',10000),('wide',2000),('large',1000)]:
        runs=[]
        for rep in range(a.repeats):
            for mode in (['json','native'] if rep%2==0 else ['native','json']):
                with pool_directory() as directory:
                    item=json.loads(subprocess.check_output([str(a.worker),str(directory),mode,shape,str(n)],env=env,text=True))
                item['repeat']=rep;runs.append(item)
        result['runs'].extend(runs)
        med={m:statistics.median(r['ns_per_op'] for r in runs if r['mode']==m) for m in ['json','native']}
        result['summary'][shape]={'ns_per_op':med,'native_time_change_pct':100*(med['native']/med['json']-1),'complete_ns_per_op':{m:statistics.median(r['complete_ns_per_op'] for r in runs if r['mode']==m) for m in ['json','native']}}
    a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result['summary'],indent=2))
if __name__=='__main__':main()

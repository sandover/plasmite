#!/usr/bin/env python3
"""Read the same prepared immutable history through the ordinary Python tail."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--library', type=Path, required=True)
    parser.add_argument('--baseline-library', type=Path, required=True)
    parser.add_argument('--bindings', type=Path, required=True)
    parser.add_argument('--harness', type=Path, required=True)
    parser.add_argument('--messages', type=int, default=50000)
    parser.add_argument('--repeats', type=int, default=7)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--native-counts', type=Path)
    args = parser.parse_args()
    libraries = {key: value.resolve() for key, value in
                 [('baseline', args.baseline_library), ('candidate', args.library)]}
    env = dict(os.environ, PYTHONPATH=str(args.bindings.resolve()),
               PYTHONDONTWRITEBYTECODE='1', PLASMITE_LIB_DIR=str(libraries['baseline']))
    result = dict(platform=platform.platform(), python=sys.version,
                  messages=args.messages, repeats=args.repeats,
                  script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  harness_sha256=hashlib.sha256(args.harness.read_bytes()).hexdigest(),
                  library_hashes={key:hashlib.sha256((value/'libplasmite.dylib').read_bytes()).hexdigest()
                                  for key, value in libraries.items()}, runs=[])
    if args.native_counts:
        result['diagnostic_only'] = True
        result['probe_sha256'] = hashlib.sha256(args.native_counts.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix='plasmite-json-reader-') as directory:
        # Prepare one history, then let both readers inspect the same bytes.
        setup = '''
import importlib.util, json, plasmite, sys
spec=importlib.util.spec_from_file_location('bench',sys.argv[1])
bench=importlib.util.module_from_spec(spec); spec.loader.exec_module(bench)
value=bench.record(); count=int(sys.argv[3])
with plasmite.Client(sys.argv[2]) as client:
    with client.create_pool('events',max(16777216,count*max(8192,2*len(json.dumps(value).encode())+4096))) as pool:
        for i in range(count):
            bench.update(value,i)
            assert pool.append_identity(value).seq==i+1
'''
        subprocess.run([sys.executable,'-c',setup,str(args.harness.resolve()),directory,
                        str(args.messages)],env=env,check=True)
        for repeat in range(args.repeats+1):
            labels=['baseline','candidate'] if repeat%2==0 else ['candidate','baseline']
            for label in labels:
                command=[sys.executable,str(args.harness.resolve()),'--worker','--lane','plasmite',
                         '--role','reader','--directory',directory,'--bindings',str(args.bindings.resolve()),
                         '--messages',str(args.messages)]
                worker_env=dict(env, PLASMITE_LIB_DIR=str(libraries[label]))
                if args.native_counts:
                    command+=['--native-counts',str(args.native_counts.resolve())]
                    worker_env['DYLD_INSERT_LIBRARIES']=str(args.native_counts.resolve())
                worker=subprocess.run(command,input='go\n',text=True,capture_output=True,
                                      timeout=120,env=worker_env,check=True)
                lines=worker.stderr.splitlines()
                assert lines[0]=='ready'
                report=json.loads(lines[-1])
                assert report['messages']==args.messages
                assert report['checksum']==3*args.messages*(args.messages-1)//2
                report['elapsed_s']=(report['ended_ns']-report['started_ns'])/1e9
                report.update(label=label,repeat=repeat)
                if repeat: result['runs'].append(report)
                print(label,repeat,report['elapsed_s'],flush=True)
    result['medians']={label:{key:statistics.median(r[key] for r in result['runs'] if r['label']==label)
                             for key in ['elapsed_s','cpu_seconds','peak_rss_bytes']}
                       for label in libraries}
    args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result['medians'],indent=2))


if __name__=='__main__':
    main()

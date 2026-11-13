#!/usr/bin/env python3
"""Compare two isolated Rust builds against the same fully indexed history."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repeats', type=int, default=7)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[4]
    fixtures = root/'docs/performance/language-boundary-results/encoding/fixtures'
    binaries = {label: path.resolve() for label, path in
                [('baseline', args.baseline), ('candidate', args.candidate)]}
    result = dict(platform=platform.platform(), rustc=subprocess.check_output(['rustc','--version'],text=True).strip(),
                  runner_sha256=digest(Path(__file__)), helper_sha256=digest(Path(__file__).with_suffix('.rs')),
                  binary_sha256={name:digest(path) for name,path in binaries.items()}, runs=[], diagnostics=[], fixtures={})
    native = ctypes.CDLL(None)
    native.sem_unlink.argtypes = [ctypes.c_char_p]
    native.sem_unlink.restype = ctypes.c_int
    for shape, count in [('sensor',100000), ('wide',20000), ('large',5000)]:
        fixture = fixtures/f'{shape}.json'
        with tempfile.TemporaryDirectory(prefix='plasmite-selection-') as temporary:
            pool = Path(temporary)/'events.plasmite'
            prepared = json.loads(subprocess.check_output([str(binaries['baseline']),'prepare',str(pool),str(fixture),str(count)],text=True))
            result['fixtures'][shape] = dict(prepared, input_sha256=digest(fixture),
                                            payload_sha256=digest(pool.with_suffix('.fixture')))
            try:
                for mode in ['raw','tags','meta','json','get']:
                    for repeat in range(args.repeats):
                        order = list(binaries) if repeat%2 == 0 else list(reversed(binaries))
                        for label in order:
                            command = [str(binaries[label]),'consume',str(pool),str(fixture),str(count),mode]
                            run = json.loads(subprocess.check_output(command,text=True))
                            assert run['verified_messages'] == count
                            run.update(shape=shape,label=label,repeat=repeat+1)
                            result['runs'].append(run)
                    for label in binaries:
                        command = [str(binaries[label]),'consume',str(pool),str(fixture),str(count),mode,'alloc']
                        run = json.loads(subprocess.check_output(command,text=True))
                        run.update(shape=shape,label=label)
                        result['diagnostics'].append(run)
                    medians = {label:statistics.median(r['elapsed_ns'] for r in result['runs']
                               if (r['shape'],r['mode'],r['label']) == (shape,mode,label))/1e6 for label in binaries}
                    print(shape,mode,medians,flush=True)
                    args.output.write_text(json.dumps(result,indent=2)+'\n')
            finally:
                # Every helper has exited; remove only this experiment's name.
                name = '/plsm-'+hashlib.sha256(os.fsencode(pool.resolve())).hexdigest()[:24]
                assert native.sem_unlink(name.encode()) == 0


if __name__ == '__main__':
    main()

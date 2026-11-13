#!/usr/bin/env python3
"""Measure writer-handle lifetime and idle CPU on macOS; no throughput timing."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import subprocess
import sys
import tempfile
import time


def descriptors():
    return len(os.listdir('/dev/fd'))


def worker(args):
    sys.path.insert(0, str(args.bindings))
    import plasmite
    with tempfile.TemporaryDirectory(prefix='plasmite-writer-resources-') as directory:
        directory = Path(directory)
        before = descriptors()
        phases = []
        with plasmite.Client(str(directory)) as client:
            for cycle in range(3):
                pools = [(client.create_pool(f'p{i}', 65536) if cycle == 0
                          else client.open_pool(f'p{i}')) for i in range(args.pools)]
                opened = descriptors()
                for pool in pools:
                    assert pool.append({'cycle': cycle}).seq == cycle + 1
                written = descriptors()
                started_cpu = time.process_time()
                started = time.monotonic()
                time.sleep(1)
                idle_elapsed = time.monotonic() - started
                idle_cpu = time.process_time() - started_cpu
                for pool in pools:
                    pool.close()
                phases.append(dict(cycle=cycle, opened_descriptors=opened,
                                   written_descriptors=written, closed_descriptors=descriptors(),
                                   idle_elapsed_s=idle_elapsed, idle_cpu_s=idle_cpu))
                assert phases[-1]['closed_descriptors'] == before
        # Remove only this test's closed semaphore names before deleting its files.
        native = ctypes.CDLL(None)
        native.sem_unlink.argtypes = [ctypes.c_char_p]
        native.sem_unlink.restype = ctypes.c_int
        for path in directory.glob('*.plasmite'):
            digest = hashlib.sha256(os.fsencode(path.resolve())).hexdigest()[:24]
            assert native.sem_unlink(f'/plsm-{digest}'.encode()) == 0
        return dict(before_descriptors=before, pools=args.pools, phases=phases,
                    file_limit=resource.getrlimit(resource.RLIMIT_NOFILE))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bindings', type=Path, required=True)
    parser.add_argument('--baseline-library', type=Path)
    parser.add_argument('--library', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--pools', type=int, default=64)
    parser.add_argument('--worker', action='store_true')
    args = parser.parse_args()
    if args.worker:
        print(json.dumps(worker(args)))
        return
    result = dict(platform=platform.platform(), python=sys.version,
                  script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), runs=[])
    for label, library in [('baseline', args.baseline_library), ('cached', args.library)]:
        env = dict(os.environ, PLASMITE_LIB_DIR=str(library.resolve()))
        raw = subprocess.check_output([sys.executable, __file__, '--worker', '--bindings',
                                       str(args.bindings.resolve()), '--pools', str(args.pools)],
                                      env=env, text=True)
        run = json.loads(raw)
        run.update(label=label, library_sha256=hashlib.sha256((library/'libplasmite.dylib').read_bytes()).hexdigest())
        result['runs'].append(run)
    args.output.write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()

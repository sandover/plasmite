#!/usr/bin/env python3
"""Compare current local binding paths on Unix; report unavailable view jobs explicitly.

Build workers before timing. This runner never builds during a measurement.
The access workers exclude request IPC and setup from their operation timings.
Use bench_pipe_comparison.py for separate-process delivery/pipe/paced lanes.

Build Rust and the library with:
  cargo build --release --lib --example bench_transport_native
Build the C worker with:
  cc -O3 -Iinclude scripts/bench_access_c.c -Ltarget/release -lplasmite \
    -o target/release/bench_access_c
Build the Go worker from bindings/go with PKG_CONFIG=/usr/bin/true,
CGO_CFLAGS=-I<repo>/include and CGO_LDFLAGS=-L<repo>/target/release:
  go build -o ../../target/release/bench_boundary_go ../../scripts/bench_boundary_go.go
Build the Node addon through its existing package build before timing.

Example:
  python3 scripts/bench_language_boundaries.py --inventory
  python3 scripts/bench_language_boundaries.py --label candidate --output RUN.json
Use separate processes for different library builds; alternate baseline and
candidate invocations. --diagnostics makes a separate Go allocation-counter run.
Cold selected means fresh message acquisition with warm storage pages. Warm
selected means access to a retained native object, not a zero-copy view claim.
"""
import argparse
import contextlib
import ctypes
import errno
import hashlib
import json
import os
import platform
import resource
import time
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

from bench_access_comparison import CWorker, PythonPool, Worker, phase
from bench_transport_comparison import native_request
from bench_pipe_comparison import record, update

ROOT = Path(__file__).resolve().parents[1]


class GoWorker(Worker):
    def phase(self, op, items, sequences=None):
        response = native_request(self.process, {
            'op': op, 'items': items, 'expected': items, 'sequences': sequences,
        })
        self.diagnostics = response.get('diagnostics')
        return response['latencies_ns'], response['elapsed_ns'], response['result']


VIEW_JOBS = ['cold_selected', 'warm_selected', 'array_scan', 'native_conversion',
             'json_export', 'raw_forward', 'native_bytes']


@contextlib.contextmanager
def pool_directory():
    with tempfile.TemporaryDirectory(prefix='plasmite-boundary-') as directory:
        try:
            yield directory
        finally:
            # All workers/handles have closed. Remove only this fresh pool's
            # legacy named notification resource before removing its file.
            pool = Path(directory)/'events.plasmite'
            if pool.exists():
                native = ctypes.CDLL(None, use_errno=True)
                native.sem_unlink.argtypes = [ctypes.c_char_p]
                native.sem_unlink.restype = ctypes.c_int
                name = '/plsm-'+hashlib.sha256(os.fsencode(pool.resolve())).hexdigest()[:24]
                if native.sem_unlink(name.encode()) != 0 and ctypes.get_errno() != errno.ENOENT:
                    raise OSError(ctypes.get_errno(), 'sem_unlink failed')


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fixture(shape):
    value = record()
    value['samples'] = [{'value': i * 0.25} for i in range(32)]
    if shape == 'wide':
        value['fields'] = {f'field_{i}': i for i in range(200)}
    elif shape == 'large':
        value['note'] = 'x' * 65536
    return value


def inventory(args):
    base = ROOT / 'target' / args.profile
    workers = {
        'rust': base / 'examples' / 'bench_transport_native',
        'c': base / 'bench_access_c',
        'go': base / 'bench_boundary_go',
        'node': ROOT / 'bindings/node/index.node',
        'python': Path(args.library),
    }
    return {name: {'available': path.is_file(), 'artifact': str(path),
                   'sha256': sha(path) if path.is_file() else None,
                   'jobs': {**{'append': 'JSON input' if name == 'c' else 'native input',
                            'read': 'full JSON envelope' if name == 'c' else 'native message'},
                            **({'cold_selected':'full object decode then scalar', 'warm_selected':'scalar from retained native object', 'array_scan':'full object decode then scan', 'json_export':'requested complete JSON', 'raw_forward':'get and append complete encoded payload'} if name=='python' else {})},
                   'unavailable': {job: 'view lane not implemented in this worker' for job in VIEW_JOBS
                                   if name != 'python' or job in ['native_conversion', 'native_bytes']}}
            for name, path in workers.items()}


def run(args):
    available = inventory(args)
    result = {'schema_version': 1, 'benchmark': 'language-boundary-baseline',
              'platform': platform.platform(), 'python': sys.version,
              'profile': args.profile, 'label':args.label, 'diagnostic_run':args.diagnostics, 'runner_sha256': sha(__file__),
              'git_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'dirty_diff_sha256': hashlib.sha256(subprocess.check_output(['git', 'diff', '--binary'], cwd=ROOT)).hexdigest(),
              'library_sha256': sha(args.library), 'capabilities': available,
              'messages': args.messages, 'repeats': args.repeats,
              'timing': 'Native append/full get call latency; setup and worker IPC excluded. C accepts JSON input. Go full read includes decoding RawMessage. Artifacts may come from different builds; inspect hashes. No view-performance claim.',
              'source_sha256': {str(p.relative_to(ROOT)):sha(p) for p in [ROOT/'scripts/bench_access_node.cjs', ROOT/'scripts/bench_access_c.c', ROOT/'scripts/bench_boundary_go.go', ROOT/'examples/bench_transport_native.rs', ROOT/'bindings/python/plasmite/__init__.py']},
              'runs': [], 'fixture_sha256': {}}
    for repeat in range(args.repeats):
        names = args.lanes if repeat % 2 == 0 else list(reversed(args.lanes))
        for shape in args.shapes:
            value = fixture(shape)
            result['fixture_sha256'][shape] = hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()
            items = [json.loads(json.dumps(value)) for _ in range(args.messages)]
            for i,item in enumerate(items):
                update(item,i)
                item['samples'][0]['value'] = float(i)
            for name in names:
                if not available[name]['available']:
                    continue
                with pool_directory() as directory:
                    # The fresh pool holds all records; no overwrite enters this comparison.
                    client = __import__('plasmite').Client(directory)
                    pool = client.create_pool('events', max(8 * 1024 * 1024, len(json.dumps(value)) * args.messages * 8))
                    pool.close()
                    client.close()
                    base = ROOT / 'target' / args.profile
                    if name == 'python':
                        worker = PythonPool(ROOT, directory, 'events')
                        # The normal producer includes its native-object JSON conversion.
                        worker.append = lambda item, p=worker.pool: {'seq': p.feed(item).seq}
                    elif name == 'c':
                        worker = CWorker([str(base/'bench_access_c'), directory, 'events'])
                    elif name == 'rust':
                        worker = Worker([str(base/'examples/bench_transport_native'), '--local', directory, 'events'])
                    elif name == 'go':
                        worker = GoWorker([str(base/'bench_boundary_go'), directory, 'events'])
                    else:
                        worker = Worker(['node', str(ROOT/'scripts/bench_access_node.cjs'), 'local', str(ROOT), directory, 'events'])
                    try:
                        _, _, warm = phase(worker, 'append', items[:1])
                        phase(worker, 'read', items[:1], warm)
                        cpu_before = cpu_seconds()
                        timings, elapsed, sequences = phase(worker, 'append', items)
                        result['runs'].append(observation(name, shape, repeat, 'append', timings, elapsed))
                        if name=='go' and args.diagnostics: result['runs'][-1]['diagnostics']=worker.diagnostics
                        timings, elapsed, _ = phase(worker, 'read', items, sequences)
                        result['runs'].append(observation(name, shape, repeat, 'read', timings, elapsed))
                        if name=='go' and args.diagnostics: result['runs'][-1]['diagnostics']=worker.diagnostics
                        if name == 'python':
                            result['runs'].extend(python_jobs(worker.pool, sequences, items, shape, repeat))
                    finally:
                        worker.close()
                    result.setdefault('sessions', []).append({'lane': name, 'shape': shape, 'repeat': repeat+1,
                        'caller_and_worker_cpu_seconds': cpu_seconds()-cpu_before,
                        'scope': 'jobs, protocol IPC, verification and worker teardown; warmup excluded'})
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(result, indent=2)+'\n')
    if not result['runs']:
        raise RuntimeError('no requested worker artifacts available')
    return result


def cpu_seconds():
    return sum(u.ru_utime+u.ru_stime for u in
               [resource.getrusage(resource.RUSAGE_SELF), resource.getrusage(resource.RUSAGE_CHILDREN)])


def python_jobs(pool, sequences, items, shape, repeat):
    from plasmite._views import to_json

    def get(seq):
        with pool.fetch(since=seq) as reader:
            message = next(reader)
        assert message.seq == seq
        return message

    held = [get(seq) for seq in sequences]
    raw = [message.raw for message in held]
    for message in held:
        message.data
    jobs = {
        'cold_selected': lambda i: get(sequences[i]).data['metrics']['power'],
        'warm_selected': lambda i: held[i].data['metrics']['power'],
        'array_scan': lambda i: sum(x['value'] for x in get(sequences[i]).data['samples']),
        'json_export': lambda i: to_json(get(sequences[i])._view),
        'raw_forward': lambda i: pool.feed(get(sequences[i])),
    }
    rows = []
    for job, operation in jobs.items():
        timings = []
        started = time.perf_counter_ns()
        for i, item in enumerate(items):
            call = time.perf_counter_ns()
            value = operation(i)
            timings.append(time.perf_counter_ns() - call)
            if job in ['cold_selected', 'warm_selected']:
                valid = value == item['metrics']['power']
            elif job == 'array_scan':
                valid = value == sum(x['value'] for x in item['samples'])
            elif job == 'json_export':
                envelope = json.loads(value)
                valid = envelope['data'] == item and envelope['seq'] == sequences[i]
            else:
                forwarded = get(value.seq)
                valid = forwarded.seq == value.seq and forwarded.raw == raw[i]
            if not valid:
                raise RuntimeError(f'{job} verification failed at record {i}')
        rows.append(observation('python', shape, repeat, job, timings,
                                time.perf_counter_ns() - started))
    return rows


def observation(lane, shape, repeat, job, timings, elapsed):
    ordered = sorted(timings)
    return {'lane': lane, 'shape': shape, 'repeat': repeat + 1, 'job': job,
            'verified_count': len(timings), 'elapsed_ns': elapsed,
            'latencies_ns': timings, 'median_ns': statistics.median(timings),
            'p95_ns': ordered[min(len(ordered)-1, int(len(ordered)*0.95))],
            'p99_ns': ordered[min(len(ordered)-1, int(len(ordered)*0.99))]}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--profile', choices=['debug', 'release'], default='release')
    parser.add_argument('--library', type=Path)
    parser.add_argument('--label',default='current')
    parser.add_argument('--messages', type=int, default=1000)
    parser.add_argument('--repeats', type=int, default=5)
    parser.add_argument('--lanes', nargs='+', choices=['rust','c','python','node','go'], default=['rust','c','python','node','go'])
    parser.add_argument('--shapes', nargs='+', choices=['sensor','wide','large'], default=['sensor','wide','large'])
    parser.add_argument('--diagnostics',action='store_true',help='separate diagnostic run; enables Go allocation counters')
    parser.add_argument('--inventory', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    args.library = (args.library or ROOT/'target'/args.profile/('libplasmite.dylib' if sys.platform=='darwin' else 'libplasmite.so')).resolve()
    if args.messages < 1 or args.repeats < 1:
        parser.error('messages and repeats must be positive')
    if args.inventory:
        print(json.dumps(inventory(args), indent=2))
        return
    if not args.library.is_file():
        parser.error('build the requested library first')
    if not args.output:
        parser.error('--output is required for measurements')
    expected = args.library.parent/('libplasmite.dylib' if sys.platform=='darwin' else 'libplasmite.so')
    if not expected.is_file() or sha(expected)!=sha(args.library):
        parser.error('the loader requires libplasmite with its standard platform filename')
    os.environ['PLASMITE_BENCH_DIAGNOSTICS']='1' if args.diagnostics else '0'
    os.environ['PLASMITE_LIB_DIR'] = str(args.library.parent)
    for key in ['DYLD_LIBRARY_PATH','LD_LIBRARY_PATH']:
        os.environ[key] = str(args.library.parent)+os.pathsep+os.environ.get(key,'')
    sys.path.insert(0, str(ROOT/'bindings/python'))
    import plasmite  # Bind once; compare builds in separate runner invocations.
    result = run(args)
    print(json.dumps({'verified_runs':len(result['runs']), 'output':str(args.output)}))


if __name__ == '__main__':
    main()

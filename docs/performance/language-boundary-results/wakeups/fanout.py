#!/usr/bin/env python3
"""Compare independent-reader delivery and idle CPU; run without other builds."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time


def worker(args):
    sys.path.insert(0, str(args.bindings))
    import plasmite
    with plasmite.Client(args.directory) as client, client.open_pool('events') as pool:
        print('ready', flush=True)
        assert sys.stdin.readline() == 'go\n'
        started_cpu = time.process_time()
        started = time.monotonic_ns()
        latencies = []
        count = 0
        if args.role == 'writer':
            due = time.monotonic()
            for i in range(args.messages):
                delay = due - time.monotonic()
                if delay > 0:
                    time.sleep(delay)
                receipt = pool.append_identity(dict(id=i, sent_ns=time.monotonic_ns(), note='x' * 900))
                assert receipt.seq == i + 1
                due += args.interval_ms / 1000
                count += 1
        else:
            for message in pool.tail(since_seq=1, max_messages=args.messages,
                                     timeout_ms=1000 if args.idle else 10000,
                                     error_on_gap=True):
                received = time.monotonic_ns()
                assert message.seq == count + 1
                assert message.data['id'] == count
                assert message.data['note'] == 'x' * 900
                latencies.append(received - message.data['sent_ns'])
                count += 1
            assert count == (0 if args.idle else args.messages)
        elapsed = (time.monotonic_ns() - started) / 1e9
        cpu = time.process_time() - started_cpu
        report = dict(role=args.role, messages=count, elapsed_s=elapsed, cpu_s=cpu)
        if latencies:
            latencies.sort()
            report['latencies_ns'] = latencies
            for percentile in (50, 95, 99):
                report[f'p{percentile}_ms'] = latencies[(len(latencies) * percentile + 99) // 100 - 1] / 1e6
        print(json.dumps(report), flush=True)


def trial(args, writer_library, reader_library, idle=False):
    with tempfile.TemporaryDirectory(prefix='plasmite-wake-fanout-') as directory:
        environment = dict(os.environ, PLASMITE_LIB_DIR=str(writer_library),
                           PYTHONPATH=str(args.bindings), PYTHONDONTWRITEBYTECODE='1')
        subprocess.run([sys.executable, '-c',
                        f"import plasmite; c=plasmite.Client({directory!r}); p=c.create_pool('events',16777216); p.close(); c.close()"],
                       env=environment, check=True)
        processes = []
        try:
            for role in ['reader'] * args.readers + ([] if idle else ['writer']):
                command = [sys.executable, __file__, '--worker', '--role', role,
                           '--bindings', str(args.bindings), '--directory', directory,
                           '--messages', str(args.messages), '--interval-ms', str(args.interval_ms)]
                if idle:
                    command.append('--idle')
                env = dict(environment, PLASMITE_LIB_DIR=str(writer_library if role == 'writer' else reader_library))
                process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE, text=True, env=env)
                processes.append(process)
            for process in processes:
                if process.stdout.readline() != 'ready\n':
                    raise RuntimeError(process.stderr.read())
            # Let independent readers enter their first empty wait before publishing.
            for process in processes[:args.readers]:
                process.stdin.write('go\n')
                process.stdin.flush()
            if not idle:
                time.sleep(0.1)
                processes[-1].stdin.write('go\n')
                processes[-1].stdin.flush()
            reports = []
            for process in processes:
                output, error = process.communicate(timeout=30)
                if process.returncode:
                    raise RuntimeError(error)
                reports.append(json.loads(output))
            return dict(idle=idle, workers=reports)
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.wait()
            # Only this experiment's closed names; native-address runs create none.
            if sys.platform == 'darwin':
                native = ctypes.CDLL(None)
                native.sem_unlink.argtypes = [ctypes.c_char_p]
                native.sem_unlink.restype = ctypes.c_int
                for path in Path(directory).glob('*.plasmite'):
                    digest = hashlib.sha256(os.fsencode(path.resolve())).hexdigest()[:24]
                    native.sem_unlink(f'/plsm-{digest}'.encode())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bindings', type=Path, required=True)
    parser.add_argument('--library', type=Path)
    parser.add_argument('--baseline-library', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--readers', type=int, default=3)
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--messages', type=int, default=150)
    parser.add_argument('--interval-ms', type=float, default=20)
    parser.add_argument('--worker', action='store_true')
    parser.add_argument('--role')
    parser.add_argument('--directory')
    parser.add_argument('--idle', action='store_true')
    args = parser.parse_args()
    args.bindings = args.bindings.resolve()
    if args.worker:
        worker(args)
        return
    libraries = dict(baseline=args.baseline_library.resolve(), candidate=args.library.resolve())
    result = dict(platform=platform.platform(), python=sys.version, readers=args.readers,
                  messages=args.messages, interval_ms=args.interval_ms,
                  script_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  library_hashes={key: hashlib.sha256((value/'libplasmite.dylib').read_bytes()).hexdigest()
                                  for key, value in libraries.items()}, runs=[])
    for repeat in range(args.repeats):
        for label in (['baseline', 'candidate'] if repeat % 2 == 0 else ['candidate', 'baseline']):
            for idle in (False, True):
                run = trial(args, libraries[label], libraries[label], idle)
                run.update(label=label, repeat=repeat + 1)
                result['runs'].append(run)
                print(f'{label} repeat {repeat + 1} idle={idle}: complete', flush=True)
    # Old/new notification mechanisms differ. Verify both mixed directions retain delivery.
    for writer, reader in [('baseline', 'candidate'), ('candidate', 'baseline')]:
        run = trial(args, libraries[writer], libraries[reader])
        run.update(label=f'{writer}-writer-{reader}-readers', repeat=1)
        result['runs'].append(run)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    for label in libraries:
        readers = [worker for run in result['runs'] if run['label'] == label and not run['idle']
                   for worker in run['workers'] if worker['role'] == 'reader']
        print(label, {key: statistics.median(worker[key] for worker in readers)
                      for key in ['p50_ms', 'p95_ms', 'p99_ms', 'cpu_s']})


if __name__ == '__main__':
    main()

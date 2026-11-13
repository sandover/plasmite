#!/usr/bin/env python3
"""Measure direct Rust decode against matched JSON and retained native controls.

Build both examples first. Keep builds/tests and other benchmarks stopped.
Timing and Rust allocator probes run in separate processes.
"""
import argparse
import hashlib
import json
import platform
import resource
import statistics
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

def fingerprint(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repeats', type=int, default=7)
    parser.add_argument('--iterations', type=int, default=1000)
    args = parser.parse_args()
    assert args.repeats > 0 and args.iterations > 0
    timing = ROOT / 'target/release/examples/bench_owned_views'
    allocations = ROOT / 'target/release/examples/bench_owned_view_allocations'
    sources = [ROOT / p for p in ('src/view/de.rs', 'src/view.rs', 'examples/support/owned_view_workloads.rs', 'scripts/bench_rust_decode.py')]
    report = {'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(), 'platform': platform.platform(), 'sha256': {str(p.relative_to(ROOT)): fingerprint(p) for p in [timing, allocations, *sources]}, 'runs': [], 'allocations': [], 'summary': {}}
    for repeat in range(args.repeats):
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        lines = subprocess.check_output([str(timing), str(args.iterations), *(['--reverse'] if repeat % 2 else [])], text=True)
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        report['runs'].append({'repeat': repeat, 'process_cpu_seconds': (after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime), 'records': [json.loads(line) for line in lines.splitlines()]})
    # Allocator counters add work; their elapsed times intentionally stay null.
    report['allocations'] = [json.loads(line) for line in subprocess.check_output([str(allocations), '100'], text=True).splitlines()]
    grouped = {}
    for run in report['runs']:
        for record in run['records']:
            if 'lane' in record:
                grouped.setdefault(record['fixture'], {}).setdefault(record['lane'], []).append(record['ns_per_op'])
    report['summary'] = {fixture: {lane: statistics.median(values) for lane, values in lanes.items()} for fixture, lanes in grouped.items()}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({f: {k: round(v) for k, v in lanes.items() if 'typed' in k or 'full_native' in k} for f, lanes in report['summary'].items()}, indent=2))

if __name__ == '__main__':
    main()

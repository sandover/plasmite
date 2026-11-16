#!/usr/bin/env python3
"""Disposable reading generator: one receipt per sample, no retry queue."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

state = Path(sys.argv[1])
binary = sys.argv[2]
boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
run = str(uuid.uuid4())
child = subprocess.Popen([binary, '--dir', str(state / 'pools'), 'feed', 'readings',
                          '--tag', 'vm-collector', '--durability', 'flush'],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
try:
    for number in range(1000000):
        sample = dict(source='debian-vm-check', boot_id=boot, run_id=run,
                      sample=number, measured_at_ns=time.time_ns(), value=number % 100)
        child.stdin.write(json.dumps(sample) + '\n')
        child.stdin.flush()
        line = child.stdout.readline()
        if not line:
            raise RuntimeError('feed ended without a receipt: ' + str(child.poll()))
        receipt = json.loads(line)
        if receipt['data'] != sample:
            raise RuntimeError('receipt differs from reading')
        report = dict(boot_id=boot, run_id=run, pid=os.getpid(), sample=number,
                      seq=receipt['seq'], time=receipt['time'])
        temporary = state / 'collector-latest.tmp'
        temporary.write_text(json.dumps(report) + '\n')
        temporary.replace(state / 'collector-latest.json')
        print(json.dumps(report), flush=True)
        time.sleep(0.25)
finally:
    if child.poll() is None:
        child.terminate()
    child.wait()

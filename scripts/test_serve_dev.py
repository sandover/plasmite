#!/usr/bin/env python3
"""Check dev readiness and owned-process cleanup with a disposable native child."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / 'scripts/serve_dev.sh'
CHILD = r'''
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(void) {
    const char *mode = getenv("DEV_TEST_MODE");
    if (mode && mode[0] == 'f') { puts("fixture failed startup"); return 23; }
    if (!mode || mode[0] != 'n') {
        const char *delay = getenv("DEV_TEST_DELAY");
        if (delay) usleep((unsigned int)(atof(delay) * 1000000));
        FILE *f = fopen(getenv("DEV_TEST_PROOF"), "w");
        if (!f) return 24;
        fputs("bound\n", f); fclose(f);
        f = fopen(getenv("PLASMITE_SERVE_READY_FILE"), "w");
        if (!f) return 25;
        fputs("127.0.0.1:12345\n", f); fclose(f);
    }
    while (1) pause();
}
'''


class ServeDevTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix='plasmite dev test ')
        cls.root = Path(cls.temp.name)
        cls.bin = cls.root / 'fake server'
        source = cls.root / 'child.c'
        source.write_text(CHILD)
        subprocess.run(['cc', str(source), '-o', str(cls.bin)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.case = tempfile.TemporaryDirectory(dir=self.root)
        self.addCleanup(self.case.cleanup)
        self.work = Path(self.case.name)
        self.pools = self.work / 'pool directory'
        self.pools.mkdir()
        self.proof = self.work / 'bound'
        self.log = self.work / 'server.log'
        self.pidfile = self.work / 'server.pid'
        self.env = dict(os.environ, TMPDIR=str(self.work), DEV_TEST_PROOF=str(self.proof))

    def run_helper(self, action, mode='', delay='0', command=None):
        args = ['bash', str(HELPER), action, str(self.bin), str(self.pools), '127.0.0.1:9999']
        if action == 'run-with': args += [str(self.log), 'fixture', command or 'true']
        elif action == 'start-detached': args += [str(self.log), str(self.pidfile), 'fixture']
        elif action == 'stop': args += [str(self.pidfile), 'fixture']
        elif action == 'status': args += [str(self.pidfile), str(self.log)]
        result = subprocess.run(args, env=dict(self.env, DEV_TEST_MODE=mode, DEV_TEST_DELAY=delay),
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(list(self.work.glob('plasmite-dev-ready.*')), [])
        return result

    def test_immediate_and_delayed_ready_preserve_command_status(self):
        for delay in ('0', '1.2'):
            with self.subTest(delay=delay):
                self.proof.unlink(missing_ok=True)
                command = 'test -s ' + shlex.quote(str(self.proof)) + ' || exit 91; exit 17'
                result = self.run_helper('run-with', delay=delay, command=command)
                self.assertEqual(result.returncode, 17, result.stderr)

    def test_failed_and_never_ready_do_not_run_command(self):
        marker = self.work / 'command-ran'
        for mode in ('failed', 'never'):
            with self.subTest(mode=mode):
                result = self.run_helper('run-with', mode=mode,
                                         command='touch ' + shlex.quote(str(marker)))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(marker.exists())
                self.assertIn('fixture failed startup' if mode == 'failed' else '10 seconds', result.stderr)

    def test_detached_ready_status_stop_and_failed_start(self):
        result = self.run_helper('start-detached', delay='1.2')
        self.assertEqual(result.returncode, 0, result.stderr)
        try:
            self.assertTrue(self.proof.exists())
            self.assertIn('running (pid', self.run_helper('status').stdout)
        finally:
            self.assertEqual(self.run_helper('stop').returncode, 0)
        self.assertFalse(self.pidfile.exists())
        result = self.run_helper('start-detached', mode='failed')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.pidfile.exists())

    def test_foreign_pid_is_never_stopped(self):
        child = subprocess.Popen(['sleep', '60'])
        try:
            self.pidfile.write_text(str(child.pid))
            result = self.run_helper('stop')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('refusing to stop', result.stderr)
            self.assertIsNone(child.poll())
            self.assertTrue(self.pidfile.exists())
        finally:
            child.terminate()
            child.wait()


if __name__ == '__main__':
    unittest.main()

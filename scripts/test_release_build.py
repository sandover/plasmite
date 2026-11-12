#!/usr/bin/env python3
"""Exercise release selection and provenance with offline gh/git peers."""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SHA = 'a' * 40

# These peers provide only the external commands the resolver may call.
PEER = r'''
import json, os, pathlib, sys
s = json.loads(pathlib.Path(os.environ['RELEASE_FIXTURE']).read_text())
a = sys.argv[1:]
with open(os.environ['RELEASE_CALLS'], 'a') as out:
    out.write(json.dumps([pathlib.Path(sys.argv[0]).name, *a]) + '\n')
if pathlib.Path(sys.argv[0]).name == 'git':
    assert a[:3] == ['ls-remote', '--exit-code', '--tags'], a
    if s.get('git_fail'): sys.exit(1)
    print(s.get('tag_sha', 'a' * 40) + '\t' + a[-2])
elif a[:2] == ['run', 'list']:
    assert a[a.index('--limit')+1] == '40', a
    if s.get('list_fail'): sys.exit(1)
    print('\n'.join(s['runs'][:40]))
elif a[:2] == ['run', 'view']:
    if s.get('view_fail'): sys.exit(1)
    print(json.dumps(s.get('view', {'workflowName':'release','conclusion':'success'})))
elif a[:2] == ['run', 'download']:
    run = a[2]
    dest = pathlib.Path(a[a.index('--dir')+1])
    if run in s.get('unavailable', []): sys.exit(1)
    if dest.name == 'selected-run':
        if s.get('selected_fail'): sys.exit(1)
        meta = s.get('selected', s['metadata'].get(run))
    else: meta = s['metadata'].get(run)
    if meta is not None:
        dest.mkdir(parents=True, exist_ok=True)
        (dest / 'metadata.json').write_text(json.dumps(meta))
else: raise AssertionError(a)
'''


class ReleaseBuildTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='plasmite-release-selection-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        import sys
        for name in ('gh', 'git'):
            p = self.bin / name
            p.write_text('#!' + sys.executable + '\n' + PEER)
            p.chmod(0o755)
        self.meta = dict(tag='v2.0.0', version='2.0.0', source_sha=SHA,
                         source_ref='refs/tags/v2.0.0', source_run_id='102',
                         source_workflow='release')
        self.base = dict(runs=['103', '102', '101'], metadata={
            '103': dict(self.meta, tag='v1.9.0'), '102': self.meta})

    def run_resolver(self, state=None, run='', tag='v2.0.0', rehearsal='false', script=None):
        fixture = self.root / 'fixture.json'
        fixture.write_text(json.dumps(state or self.base))
        calls = self.root / 'calls.jsonl'
        calls.write_text('')
        env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                   RELEASE_FIXTURE=str(fixture), RELEASE_CALLS=str(calls))
        env.pop('GITHUB_STEP_SUMMARY', None)
        result = subprocess.run(['bash', str(script or ROOT / 'scripts/resolve_release_build.sh'),
                                 'sandover/plasmite', run, tag, rehearsal],
                                text=True, capture_output=True, env=env)
        return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_order_and_outputs(self):
        result, calls = self.run_resolver()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(dict(line.split('=', 1) for line in result.stdout.splitlines()),
                         dict(build_run_id='102', tag='v2.0.0', version='2.0.0',
                              source_sha=SHA, rehearsal='false'))
        downloads = [c[3] for c in calls if c[1:3] == ['run', 'download']]
        self.assertEqual(downloads, ['103', '102', '102'])

    def test_unavailable_or_missing_candidates_are_skipped(self):
        state = copy.deepcopy(self.base)
        state['runs'] = ['105', '104', '102']
        state['unavailable'] = ['105']
        result, _ = self.run_resolver(state)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_search_bound_and_explicit_recovery(self):
        state = copy.deepcopy(self.base)
        state['runs'] = [str(i) for i in range(200, 240)] + ['102']
        result, _ = self.run_resolver(state)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('build_run_id explicitly', result.stderr)
        result, _ = self.run_resolver(state, run='102', tag='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('incident recovery', result.stderr)

    def test_rejections(self):
        scenarios = [dict(tag_sha='b' * 40), dict(git_fail=True), dict(list_fail=True),
                     dict(view_fail=True), dict(selected_fail=True), dict(selected=None),
                     dict(view=dict(workflowName='ci', conclusion='success')),
                     dict(view=dict(workflowName='release', conclusion='failure'))]
        for field, value in [('tag', 'v1.9.0'), ('version', '1.9.0'),
                             ('source_sha', 'invalid'), ('source_sha', 'b' * 40),
                             ('source_ref', 'refs/heads/main'), ('source_run_id', '103'),
                             ('source_workflow', 'ci')]:
            scenarios.append(dict(selected=dict(self.meta, **{field: value})))
        scenarios.append(dict(unavailable=['102']))
        for change in scenarios:
            with self.subTest(change=change):
                state = dict(self.base, **change)
                result, _ = self.run_resolver(state)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertNotIn('source_sha=', result.stdout)

    def test_invalid_inputs_never_call_tools(self):
        for run, tag, rehearsal in [('0','','false'), ('-1','','false'),
                                   ('12x','','false'), ('','','false'),
                                   ('102','v2.0.0','false'), ('','v02.0.0','false'),
                                   ('','v2.0.0','yes')]:
            with self.subTest(run=run, tag=tag, rehearsal=rehearsal):
                result, calls = self.run_resolver(run=run, tag=tag, rehearsal=rehearsal)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls, [])

    def test_moved_tag_guard_mutation_and_comments(self):
        # Keep the real companion helpers so this exercises the same interface.
        for name in ('validate_release_tag.sh', 'resolve_release_tag_sha.sh'):
            shutil.copy(ROOT / 'scripts' / name, self.root / name)
        script = self.root / 'resolve_release_build.sh'
        original = (ROOT / 'scripts/resolve_release_build.sh').read_text()
        script.write_text('# harmless comment\n' + original)
        result, _ = self.run_resolver(script=script)
        self.assertEqual(result.returncode, 0, result.stderr)
        moved = dict(self.base, tag_sha='b' * 40)
        result, _ = self.run_resolver(moved, script=script)
        self.assertNotEqual(result.returncode, 0)
        script.write_text(original.replace('if [[ "$source_sha" != "$tag_source_sha" ]]; then',
                                           'if false; then'))
        result, _ = self.run_resolver(moved, script=script)
        self.assertEqual(result.returncode, 0, 'disabled guard must expose the failing expectation')


if __name__ == '__main__':
    unittest.main()

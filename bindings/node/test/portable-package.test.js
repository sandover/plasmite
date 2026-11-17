const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

test('packed remote-only package uses portable checked views without native artifacts', t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'plasmite-portable-package-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const source = path.resolve(__dirname, '..');
  const root = path.join(directory, 'package'); fs.mkdirSync(root);
  for (const name of ['index.js', 'follow.js', 'remote.js', 'views.js', 'input.js', 'message.js', 'mappings.js', 'types.d.ts',
    'portable.js', 'portable', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'bin']) {
    fs.cpSync(path.join(source, name), path.join(root, name), { recursive: true });
  }
  fs.copyFileSync(path.join(source, 'package.json'), path.join(root, 'package.json'));
  const npmEnv = { ...process.env, npm_config_cache: path.join(directory, 'npm-cache') };
  const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const packed = JSON.parse(execFileSync(npm, ['pack', '--ignore-scripts', '--json'], { cwd: root, encoding: 'utf8', env: npmEnv }))[0];
  assert.ok(packed.files.some(file => file.path === 'portable/plasmite_core.wasm'));
  assert.ok(packed.files.some(file => file.path === 'portable/COPYRIGHT.musl'));
  assert.equal(packed.files.some(file => file.path.endsWith('.node')), false);
  const consumer = path.join(directory, 'consumer'); fs.mkdirSync(consumer);
  execFileSync(npm, ['install', '--ignore-scripts', '--no-audit', '--no-fund', '--prefix', consumer,
    path.join(root, packed.filename)], { stdio: 'pipe', env: npmEnv });
  const output = execFileSync(process.execPath, ['-e', `
    const assert=require('node:assert/strict');
    const p=require('plasmite'); const portable=require('plasmite/portable');
    assert.equal(portable.diagnostics().initialized,false);
    new p.RemoteClient('http://127.0.0.1:1');
    (async()=>{
      const bytes=await portable.encodeMessage({maximum:9223372036854775807n,bytes:Buffer.from([0,255])},['tag']);
      const view=await portable.decodeMessage(bytes,3n,123456789n);
      assert.equal(view.data.maximum,9223372036854775807n);
      assert.deepEqual([...view.data.bytes],[0,255]);
      assert.equal(p.toNative(view.data).maximum,9223372036854775807n);
      assert.equal(p.toJson(view.data.bytes),'"AP8="');
      assert.equal(Object.keys(require.cache).some(name=>name.endsWith('.node')),false);
      console.log('packed-portable-only-ok');
    })().catch(error=>{console.error(error);process.exitCode=1});
  `], { cwd: consumer, encoding: 'utf8', env: { ...process.env, NODE_PATH: '' } });
  assert.match(output, /packed-portable-only-ok/);
});

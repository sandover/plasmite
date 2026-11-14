#!/usr/bin/env node
// Maintainer build/verification only. Installed packages use the checked artifact.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync, execFileSync } = require('node:child_process');
const packageRoot = path.resolve(__dirname, '..');
const root = path.resolve(packageRoot, '../..');
const artifact = path.join(packageRoot, 'portable/plasmite_core.wasm');
const manifestPath = path.join(packageRoot, 'portable/manifest.json');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const sources = [
  'bindings/node/portable-core/Cargo.toml', 'bindings/node/portable-core/Cargo.lock',
  'bindings/node/portable-core/build.rs', 'bindings/node/portable-core/src/lib.rs',
  'bindings/node/portable-core/src/core.rs', 'src/core/error.rs', 'src/abi_builder.rs',
  'src/view.rs', 'src/interface_wire.rs', 'c/lite3_shim.c', 'c/lite3_shim.h', 'c/lite3_builder.c',
  ...fs.readdirSync(path.join(root, 'src/core/lite3')).filter(name => name.endsWith('.rs')).map(name => 'src/core/lite3/' + name),
  ...fs.readdirSync(path.join(root, 'src/view')).filter(name => name.endsWith('.rs')).map(name => 'src/view/' + name),
  'vendor/lite3/include/lite3.h', 'vendor/lite3/include/lite3_context_api.h',
  ...['lite3', 'json_dec', 'json_enc', 'ctx_api', 'debug'].map(name => 'vendor/lite3/src/' + name + '.c'),
  ...['yyjson/yyjson.c', 'yyjson/yyjson.h', 'nibble_base64/base64.c', 'nibble_base64/base64.h'].map(name => 'vendor/lite3/lib/' + name),
];
function verify() {
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  const bytes = fs.readFileSync(artifact);
  if (manifest.sha256 !== sha(bytes)) throw new Error('portable Wasm checksum mismatch; rebuild the artifact');
  const module = new WebAssembly.Module(bytes);
  if (WebAssembly.Module.imports(module).length) throw new Error('portable Wasm must have no runtime imports');
  if (new WebAssembly.Instance(module, {}).exports.portable_version() !== manifest.abi) throw new Error('portable ABI mismatch');
  for (const [name, expected] of Object.entries(manifest.sources)) {
    if (sha(fs.readFileSync(path.join(root, name))) !== expected) throw new Error('portable artifact is stale: ' + name);
  }
  console.log(`Portable artifact, shared sources and import-free ABI PASS: ${bytes.length} bytes`);
}
if (!process.argv.includes('--verify')) {
  const sdk = process.env.WASI_SDK_PATH;
  if (!sdk) throw new Error('set WASI_SDK_PATH to the official WASI SDK34 directory; consumers need no compiler');
  const target = process.env.PLASMITE_PORTABLE_BUILD_DIR || path.join(root, 'target/portable');
  const build = spawnSync('cargo', ['build', '--locked', '--release', '--target', 'wasm32-unknown-unknown',
    '--manifest-path', path.join(packageRoot, 'portable-core/Cargo.toml')], {
    cwd: root, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: target },
  });
  if (build.error) throw build.error;
  if (build.status !== 0) process.exit(build.status || 1);
  fs.mkdirSync(path.dirname(artifact), { recursive: true });
  fs.copyFileSync(path.join(target, 'wasm32-unknown-unknown/release/plasmite_portable_core.wasm'), artifact);
  const bytes = fs.readFileSync(artifact);
  const manifest = { abi: 1, bytes: bytes.length, sha256: sha(bytes),
    rustc: execFileSync('rustc', ['--version'], { cwd: root, encoding: 'utf8' }).trim(),
    clang: execFileSync(path.join(sdk, 'bin/clang'), ['--version'], { encoding: 'utf8' }).split('\n')[0],
    libc_memchr_sha256: sha(execFileSync(path.join(sdk, 'bin/llvm-ar'), ['p',
      path.join(sdk, 'share/wasi-sysroot/lib/wasm32-wasip1/libc.a'), 'memchr.c.obj'])),
    sources: Object.fromEntries(sources.map(name => [name, sha(fs.readFileSync(path.join(root, name)))])),
  };
  fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
}
verify();

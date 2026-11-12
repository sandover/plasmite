const test = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const os = require("node:os");
const path = require("node:path");

test("default directories use absolute home overrides or the account home", () => {
  const key = process.platform === "win32" ? "USERPROFILE" : "HOME";
  const account = os.userInfo().homedir;
  for (const value of [undefined, "", "relative", path.join(os.tmpdir(), "home space 東京")]) {
    const env = { ...process.env };
    if (value === undefined) delete env[key]; else env[key] = value;
    const child = spawnSync(process.execPath, ["-e", 'console.log(require("./index").DEFAULT_POOL_DIR)'], { cwd: path.join(__dirname, ".."), env, encoding: "utf8" });
    assert.equal(child.status, 0, child.stderr);
    const expected = value && path.isAbsolute(value) ? value : account;
    assert.equal(child.stdout.trim(), path.join(expected, ".plasmite", "pools"));
  }
});


test("failed home lookup permits imports and reports an actionable default error", () => {
  const script = `
    const assert = require("node:assert/strict");
    const os = require("node:os");
    delete process.env.HOME;
    delete process.env.USERPROFILE;
    os.homedir = os.userInfo = () => { throw new Error("account lookup failed"); };
    const { Client, RemoteClient, DEFAULT_POOL_DIR } = require("./index");
    assert.equal(DEFAULT_POOL_DIR, undefined);
    new RemoteClient("http://localhost:9700");
    assert.throws(() => new Client(), /supply an explicit pool directory/);
  `;
  const child = spawnSync(process.execPath, ["-e", script], { cwd: path.join(__dirname, ".."), encoding: "utf8" });
  assert.equal(child.status, 0, child.stderr);
});

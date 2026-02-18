/*
Purpose: Exercise the Node cookbook snippet path in smoke automation.
Key Exports: main (script entrypoint).
Role: Validate local client.pool(), typed append return fields, and not-found handling.
Invariants: Runs against local pool directories only; no network usage.
Invariants: Produces deterministic JSON output for shell assertions.
Notes: Designed for scripts/cookbook_smoke.sh integration.
*/

const fs = require("node:fs/promises");
const { Client, ErrorKind, PlasmiteNativeError } = require("./index");

async function main() {
  if (process.argv.length !== 3) {
    throw new Error("usage: cookbook_smoke_fixture.js <pool-dir>");
  }
  const poolDir = process.argv[2];
  await fs.rm(poolDir, { recursive: true, force: true });
  await fs.mkdir(poolDir, { recursive: true });

  const client = new Client(poolDir);
  let pool = null;
  try {
    pool = client.pool("cookbook-smoke", 1024 * 1024);
    const msg = pool.append({ task: "resize", id: 1 }, ["cookbook"]);
    if (msg.seq < 1) {
      throw new Error("expected positive seq");
    }
    if (!Array.isArray(msg.tags) || msg.tags.length !== 1 || msg.tags[0] !== "cookbook") {
      throw new Error(`unexpected tags: ${JSON.stringify(msg.tags)}`);
    }
    if (!msg.data || msg.data.task !== "resize") {
      throw new Error(`unexpected data: ${JSON.stringify(msg.data)}`);
    }

    try {
      client.openPool("missing-cookbook-smoke-pool");
      throw new Error("expected not-found error");
    } catch (err) {
      if (!(err instanceof PlasmiteNativeError) || err.kind !== ErrorKind.NotFound) {
        throw err;
      }
    }

    process.stdout.write(
      `${JSON.stringify({ seq: Number(msg.seq), tags: msg.tags, data: msg.data })}\n`,
    );
  } finally {
    if (pool) {
      pool.close();
    }
    client.close();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});

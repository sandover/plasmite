/*
Purpose: JavaScript entry point for the Plasmite Node binding.
Key Exports: Client, Pool, Stream, Durability, ErrorKind, replay.
Role: Thin wrapper around the native N-API addon.
Invariants: Exports align with native symbols and v0 API semantics.
Notes: Requires libplasmite to be discoverable at runtime.
*/

const native = require("./index.node");
const { RemoteClient, RemoteError, RemotePool, RemoteTail } = require("./remote");

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function* replay(pool, options = {}) {
  const { speed = 1.0, sinceSeq, maxMessages, timeoutMs } = options;
  const stream = pool.openStream(
    sinceSeq ?? null,
    maxMessages ?? null,
    timeoutMs ?? null,
  );

  const messages = [];
  try {
    let msg;
    while ((msg = stream.nextJson()) !== null) {
      messages.push(msg);
    }
  } finally {
    stream.close();
  }

  let prevMs = null;
  for (const msg of messages) {
    const parsed = JSON.parse(msg);
    const curMs = new Date(parsed.time).getTime();

    if (prevMs !== null && speed > 0) {
      const delay = (curMs - prevMs) / speed;
      if (delay > 0) {
        await sleep(delay);
      }
    }

    prevMs = curMs;
    yield msg;
  }
}

module.exports = {
  ...native,
  RemoteClient,
  RemoteError,
  RemotePool,
  RemoteTail,
  replay,
};

/*
Purpose: Lightweight type smoke test for public Node binding declarations.
Key Exports: None.
Role: Compile-time assertion that local + remote APIs are fully typed.
Invariants: No runtime execution; must remain dependency-light.
Notes: Used by `npx tsc --noEmit -p tsconfig.json`.
*/

import {
  Client,
  Durability,
  parseMessage,
  RemoteClient,
  RemoteError,
  replay,
} from "./types";

const client = new Client("./data");
const pool = client.createPool("docs", 1024 * 1024);
const appended = pool.appendJson(Buffer.from("{}"), [], Durability.Fast);
const appendedAlias = pool.append({ kind: "note" }, ["tag"], Durability.Fast);
const frame = pool.getLite3(1n);
const got = pool.get(1n);
const parsed = parseMessage(got);
void appended;
void appendedAlias;
void frame;
void parsed;

async function smokeRemote() {
  const remote = new RemoteClient("http://127.0.0.1:9700");
  const opened = await remote.openPool("docs");
  const msg = await opened.append({ kind: "note" }, ["note"], "fast");
  const tail = await opened.tail({ sinceSeq: msg.seq, maxMessages: 1 });
  const next = await tail.next();
  if (next) {
    const seq: number = next.seq;
    void seq;
  }
  tail.cancel();
}

async function smokeReplay() {
  for await (const message of pool.tail({ maxMessages: 1, timeoutMs: 10, tags: ["tag"] })) {
    void message;
    break;
  }
  for await (const message of pool.replay({ speed: 1, tags: ["tag"] })) {
    void message;
    break;
  }
  for await (const message of replay(pool, { speed: 1 })) {
    void message;
    break;
  }
}

function handleRemoteError(err: unknown) {
  if (err instanceof RemoteError) {
    const kind: string = err.kind;
    void kind;
  }
}

void smokeRemote;
void smokeReplay;
void handleRemoteError;

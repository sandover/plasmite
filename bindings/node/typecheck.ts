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
  ErrorKind,
  Message,
  parseMessage,
  RemoteClient,
  RemoteError,
} from ".";

const client = new Client("./data");
const pool = client.createPool("docs", 1024 * 1024);
const pooled = client.openPool("docs", { create: true });
const appended = pool.feedJson(Buffer.from("{}"), { durability: Durability.Fast });
const fed = pool.feed({ kind: "note" }, { tags: ["tag"], durability: Durability.Fast });
const parsed = parseMessage(appended);
const parsedFromEnvelope = parseMessage({
  seq: 1,
  time: "2026-02-18T00:00:00Z",
  data: { ok: true },
  meta: { tags: ["tag"] },
});
const typedMessage: Message = parsed;
const retentionGap: ErrorKind = ErrorKind.RetentionGap;
void appended;
void pooled;
void fed;
void parsed;
void parsedFromEnvelope;
void typedMessage;
void retentionGap;

async function smokeRemote() {
  const remote = new RemoteClient("http://127.0.0.1:9700");
  const opened = await remote.openPool("docs");
  const msg = await opened.feed({ kind: "note" }, { tags: ["note"], durability: Durability.Fast });
  for await (const next of await opened.follow({ since: msg.seq, tags: ["note"] })) {
    const seq: bigint = next.seq;
    void seq;
    break;
  }
}

async function smokeLocalStreams() {
  for await (const message of await pool.follow({ tags: ["tag"] })) {
    void message;
    break;
  }
  const history = await pool.fetch({ since: 1n, tags: ["tag"] });
  const item = await history.next();
  if (item.value) {
    const time: bigint = item.value.time;
    void time;
  }
  await history.return();
  const binary = await pool.fetchLite3({ since: 1n });
  const frame = await binary.next();
  if (frame.value) {
    const payload: Buffer = frame.value.payload;
    void payload;
  }
  await binary.return();
}

function handleRemoteError(err: unknown) {
  if (err instanceof RemoteError) {
    const kind: ErrorKind = err.kind;
    const seq: bigint | undefined = err.seq;
    const offset: bigint | undefined = err.offset;
    void kind; void seq; void offset;
  }
}

function handleNativeError(err: unknown) {
  if (err && typeof err === "object" && "kind" in err) {
    const kind = (err as { kind?: ErrorKind }).kind;
    void kind;
  }
}

void smokeRemote;
void smokeLocalStreams;
void handleRemoteError;
void handleNativeError;

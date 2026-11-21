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
  RemoteClient,
  RemoteError,
} from ".";

const client = new Client("./data");
const pool = client.createPool("docs", {
  sizeBytes: 1024 * 1024, about: "Document events", example: { kind: "note" },
});
pool.setDescription({ about: "Draft document events" });
const pooled = client.openPool("docs", { create: true });
const appended = pool.feed(Buffer.from("{}"), { format: "json", durability: Durability.Fast });
const fed = pool.feed({ kind: "note" }, { tags: ["tag"], durability: Durability.Fast });
// @ts-expect-error Encoded input needs its format.
pool.feed(Buffer.from("{}"));
// @ts-expect-error Messages come from pools.
new Message();
const retentionGap: ErrorKind = ErrorKind.RetentionGap;
void pooled;
void fed;
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
  const parsed: Message = await appended;
  // @ts-expect-error Message tags are immutable.
  parsed.tags.push("tag");
  // @ts-expect-error Message metadata is immutable.
  parsed.meta.tags = [];
  void pool.feed(parsed);
  // @ts-expect-error Forwarded messages keep their tags.
  void pool.feed(parsed, { tags: [] });
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
  const binary = await pool.fetch({ since: 1n });
  const frame = await binary.next();
  if (frame.value) {
    const payload: Buffer = frame.value.raw;
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

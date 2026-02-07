/*
Purpose: Stable TypeScript declarations for the public Node bindings API.
Key Exports: Client, Pool, Stream, Lite3Stream, replay, and remote client types.
Role: Preserve complete JS + native type surface independently of generated files.
Invariants: Public runtime exports from index.js are represented here.
Invariants: Numeric sequence fields accept number or bigint input.
Notes: Kept separate from NAPI-generated index.d.ts to avoid regeneration loss.
*/

export const enum Durability {
  Fast = 0,
  Flush = 1,
}

export const enum ErrorKind {
  Internal = 1,
  Usage = 2,
  NotFound = 3,
  AlreadyExists = 4,
  Busy = 5,
  Permission = 6,
  Corrupt = 7,
  Io = 8,
}

export interface Lite3Frame {
  seq: bigint
  timestampNs: bigint
  flags: number
  payload: Buffer
}

export class PlasmiteNativeError extends Error {
  kind?: string
  path?: string
  seq?: number
  offset?: number
  cause?: unknown
}

export class Client {
  constructor(poolDir: string)
  createPool(poolRef: string, sizeBytes: number | bigint): Pool
  openPool(poolRef: string): Pool
  close(): void
}

export class Pool {
  appendJson(payload: Buffer, tags: string[], durability: Durability): Buffer
  appendLite3(payload: Buffer, durability: Durability): bigint
  getJson(seq: number | bigint): Buffer
  getLite3(seq: number | bigint): Lite3Frame
  openStream(
    sinceSeq?: number | bigint | null,
    maxMessages?: number | bigint | null,
    timeoutMs?: number | bigint | null,
  ): Stream
  openLite3Stream(
    sinceSeq?: number | bigint | null,
    maxMessages?: number | bigint | null,
    timeoutMs?: number | bigint | null,
  ): Lite3Stream
  close(): void
}

export class Stream {
  nextJson(): Buffer | null
  close(): void
}

export class Lite3Stream {
  next(): Lite3Frame | null
  close(): void
}

export interface ReplayOptions {
  speed?: number
  sinceSeq?: number | bigint
  maxMessages?: number | bigint
  timeoutMs?: number | bigint
}

export function replay(
  pool: Pool,
  options?: ReplayOptions,
): AsyncGenerator<Buffer, void, unknown>

export interface RemoteClientOptions {
  token?: string
}

export interface RemotePoolInfo {
  pool: string
  path: string
  file_size: number
  ring_size: number
  bounds: {
    oldest_seq: number | null
    newest_seq: number | null
  }
  write_cursor: number
  index_inline: boolean
}

export interface RemoteMessage {
  seq: number
  time: string
  data: unknown
  meta?: {
    tags?: string[]
  }
}

export interface RemoteTailOptions {
  sinceSeq?: number | bigint
  maxMessages?: number | bigint
  timeoutMs?: number
}

export class RemoteError extends Error {
  status: number
  kind: string
  hint?: string
  path?: string
  seq?: number
  offset?: number
}

export class RemoteClient {
  constructor(baseUrl: string, options?: RemoteClientOptions)
  baseUrl: URL
  token: string | null
  withToken(token: string): RemoteClient
  createPool(pool: string, sizeBytes: number | bigint): Promise<string>
  openPool(pool: string): Promise<RemotePool>
  poolInfo(pool: string): Promise<RemotePoolInfo>
  listPools(): Promise<string[]>
  deletePool(pool: string): Promise<void>
}

export class RemotePool {
  constructor(client: RemoteClient, pool: string)
  client: RemoteClient
  pool: string
  poolRef(): string
  append(
    data: unknown,
    tags?: string[],
    durability?: "fast" | "flush",
  ): Promise<RemoteMessage>
  get(seq: number | bigint): Promise<RemoteMessage>
  tail(options?: RemoteTailOptions): Promise<RemoteTail>
}

export class RemoteTail {
  next(): Promise<RemoteMessage | null>
  cancel(): void
}

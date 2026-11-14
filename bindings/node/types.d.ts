/*
Purpose: Stable TypeScript declarations for the public Node bindings API.
Key Exports: Client, Pool, Stream, Lite3Stream, parseMessage, and remote client types.
Role: Preserve complete JS + native type surface independently of generated files.
Invariants: Public runtime exports from index.js are represented here.
Invariants: Numeric sequence fields accept number or bigint input.
Notes: Canonical declaration source for package typing (`package.json#types`).
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
  RetentionGap = 9,
}
export const DEFAULT_POOL_DIR: string | undefined
export const DEFAULT_POOL_SIZE_BYTES: number
export const DEFAULT_POOL_SIZE: number

export interface Lite3Frame {
  seq: bigint
  timestampNs: bigint
  flags: number
  payload: Buffer
  time: Date
}

export interface MessageMeta {
  tags: string[]
}

export class Message {
  readonly seq: bigint
  readonly time: Date
  readonly timeRfc3339: string
  readonly data: unknown
  readonly meta: MessageMeta
  readonly raw: Buffer
  get tags(): string[]
}

export interface MessageEnvelope {
  seq: number | bigint | string
  time: string
  data: unknown
  meta?: {
    tags?: unknown[]
  }
}

export class PlasmiteNativeError extends Error {
  kind?: ErrorKind
  path?: string
  seq?: bigint
  offset?: bigint
  cause?: unknown
}

export class Client {
  constructor(poolDir?: string)
  createPool(poolRef: string, sizeBytes?: number | bigint): Pool
  openPool(poolRef: string): Pool
  pool(poolRef: string, sizeBytes?: number | bigint): Pool
  close(): void
  [Symbol.dispose](): void
}

/** Committed identity; timestampNs is unsigned Unix time in nanoseconds. */
export interface AppendReceipt {
  seq: bigint
  timestampNs: bigint
}

declare const byteViewType: unique symbol
declare const messageViewType: unique symbol

export interface BytesView extends Iterable<number> {
  readonly [byteViewType]: true
  readonly length: number
  readonly [index: number]: number
}
export type View<T> = T extends Uint8Array ? BytesView
  : T extends object ? { readonly [K in keyof T]: View<T[K]> } : T
export interface MessageView<T = unknown> {
  readonly [messageViewType]: true
  readonly seq: bigint
  readonly timestampNs: bigint
  readonly timeRfc3339: string
  readonly meta: View<{ tags: string[] }>
  readonly data: View<T>
}
export type Native<T> = T extends MessageView<infer D>
  ? { seq: bigint; time: string; meta: { tags: string[] }; data: Native<View<D>> }
  : T extends BytesView ? Uint8Array
  : T extends object ? { -readonly [K in keyof T]: Native<T[K]> } : T
/** Build independent mutable containers from a view or supported scalar. */
export function toNative<T>(value: T): Native<T>
/** Format directly from a view, preserving exact stored integer digits. */
export function toJson(value: unknown): string
/** Copy one complete message's encoded payload into an independent mutable Buffer. */
export function toLite3(message: MessageView): Buffer

export class Pool {
  /** Acquire an independent immutable snapshot without blocking JavaScript. */
  getView<T = unknown>(seq: number | bigint): Promise<MessageView<T>>
  /** Encode native input once; T asserts the resulting data shape without validation. */
  appendView<T = unknown>(payload: unknown, tags?: readonly string[], durability?: Durability): Promise<MessageView<T>>
  appendIdentity(payload: unknown, tags?: string[], durability?: Durability): AppendReceipt
  appendLite3Identity(payload: Buffer, durability?: Durability): AppendReceipt
  append(payload: unknown, tags?: string[], durability?: Durability): Message
  appendJson(payload: unknown, tags?: string[], durability?: Durability): Buffer
  appendLite3(payload: Buffer, durability?: Durability): bigint
  get(seq: number | bigint): Message
  getJson(seq: number | bigint): Buffer
  getLite3(seq: number | bigint): Lite3Frame
  tail(options?: LocalTailOptions): AsyncGenerator<Message, void, unknown>
  openStream(
    sinceSeq?: number | bigint | null,
    maxMessages?: number | bigint | null,
    timeoutMs?: number | bigint | null,
    errorOnGap?: boolean,
  ): Stream
  openLite3Stream(
    sinceSeq?: number | bigint | null,
    maxMessages?: number | bigint | null,
    timeoutMs?: number | bigint | null,
    errorOnGap?: boolean,
  ): Lite3Stream
  close(): void
  [Symbol.dispose](): void
}

export class Stream {
  [Symbol.iterator](): Iterator<Buffer>
  nextJson(): Buffer | null
  nextJsonWait(timeoutMs: number | bigint): Promise<{ status: 0 | 1 | 2 | 3; message?: Buffer | null }>
  cancel(): void
  close(): void
  [Symbol.dispose](): void
}

export class Lite3Stream {
  [Symbol.iterator](): Iterator<Lite3Frame>
  next(): Lite3Frame | null
  nextWait(timeoutMs: number | bigint): Promise<{ status: 0 | 1 | 2 | 3; frame?: Lite3Frame | null }>
  cancel(): void
  close(): void
  [Symbol.dispose](): void
}

export interface LocalTailOptions {
  sinceSeq?: number | bigint
  maxMessages?: number | bigint
  timeoutMs?: number | bigint
  tags?: string[]
  errorOnGap?: boolean
  signal?: AbortSignal
}

export type UdpStart =
  | { kind: "new" }
  | { kind: "last"; count: number | bigint }
  | { kind: "since"; timestampNs: number | bigint }
  | { kind: "sequence"; seq: number | bigint }

export interface UdpFollowOptions {
  start?: UdpStart
  live?: boolean
}

export interface UdpWaitOptions {
  signal?: AbortSignal
}

export type UdpEvent =
  | {
      kind: "frame"
      seq: bigint
      timestampNs: bigint
      payload: Buffer
      message: Message
    }
  // "gap" is the compatibility name for a server skip; "unknown" is reserved.
  | { kind: "gap"; first: bigint; last: bigint; reason: "unknown" | "oversized" | "retention" | "pressure" }
  | {
      kind: "progress"
      startSeq: bigint
      nextSeq: bigint
      endSeq: bigint | null
      terminal: boolean
      retentionGap: boolean
    }

export const UdpStart: {
  new: () => UdpStart
  last(count: number | bigint): UdpStart
  since(timestampNs: number | bigint): UdpStart
  sequence(seq: number | bigint): UdpStart
}

export class UdpClient {
  constructor(endpoint: string)
  openPool(poolName: string): UdpPool
  close(): void
  [Symbol.dispose](): void
}

export class UdpPool {
  sendJson(payload: unknown, tags?: string[]): void
  sendLite3(payload: Buffer): void
  follow(options?: UdpFollowOptions): Promise<UdpFollow>
  close(): void
  [Symbol.dispose](): void
}

export class UdpFollow {
  readonly finished: boolean
  nextEvent(timeoutMs?: number | bigint, options?: UdpWaitOptions): Promise<UdpEvent | null>
  events(options?: { pollIntervalMs?: number | bigint; signal?: AbortSignal }): AsyncGenerator<UdpEvent, void, unknown>
  cancel(): void
  close(): void
  [Symbol.dispose](): void
  [Symbol.asyncIterator](): AsyncGenerator<UdpEvent, void, unknown>
}

export function parseMessage(payload: Buffer | Message | MessageEnvelope): Message

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

export interface RemoteTailOptions {
  sinceSeq?: number | bigint
  maxMessages?: number | bigint
  timeoutMs?: number
  tags?: string[]
  errorOnGap?: boolean
  signal?: AbortSignal
}

/** One binary HTTP observation; requires current-server frame identity headers. */
export interface RemoteLite3Frame {
  seq: bigint
  timestampNs: bigint
  payload: Buffer
  time: Date
}

export class RemoteError extends Error {
  status: number
  kind: ErrorKind
  hint?: string
  path?: string
  seq?: bigint
  offset?: bigint
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
  appendView<T = unknown>(data: unknown, tags?: readonly string[], durability?: Durability): Promise<MessageView<T>>
  getView<T = unknown>(seq: number | bigint, options?: { signal?: AbortSignal }): Promise<MessageView<T>>
  tailViews<T = unknown>(options?: RemoteTailOptions): AsyncGenerator<MessageView<T>, void, unknown>
  append(
    data: unknown,
    tags?: string[],
    durability?: Durability,
  ): Promise<Message>
  get(seq: number | bigint): Promise<Message>
  appendLite3(payload: Buffer, durability?: Durability): Promise<bigint>
  getLite3(seq: number | bigint, options?: { signal?: AbortSignal }): Promise<RemoteLite3Frame>
  tailLite3(options?: RemoteTailOptions): AsyncGenerator<RemoteLite3Frame, void, unknown>
  tail(options?: RemoteTailOptions): AsyncGenerator<Message, void, unknown>
}

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
  time: bigint
  flags: number
  payload: Buffer
}

export interface MessageMeta {
  tags: string[]
}

export class Message {
  readonly seq: bigint
  readonly time: bigint
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
  missing?: bigint
  nextSeq?: bigint
  cause?: unknown
}

export interface OpenPoolOptions { create?: boolean }
export interface PoolInfo {
  name: string
  path: string
  file_size: bigint
  bounds: { oldest?: bigint; newest?: bigint }
}
export interface ReadOptions {
  since?: number | bigint | string | Instant
  last?: number | bigint
  tags?: string[]
  signal?: AbortSignal
}
export interface FeedOptions { tags?: string[]; durability?: Durability }
declare const instantType: unique symbol
export interface Instant { readonly [instantType]: true }
export function at(time: number | bigint): Instant
export function createPool(poolRef: string, sizeBytes?: number | bigint): Pool
export function openPool(poolRef: string, options?: OpenPoolOptions): Pool
export function listPools(): PoolInfo[]
export function deletePool(poolRef: string): void

export interface Reader<T = Message> extends AsyncIterableIterator<T> {
  next(): Promise<IteratorResult<T, undefined>>
  next(wait: number): Promise<IteratorResult<T | undefined, undefined>>
  return(): Promise<IteratorResult<T, undefined>>
  close(): void
}

export class Client {
  constructor(poolDir?: string)
  createPool(poolRef: string, sizeBytes?: number | bigint): Pool
  openPool(poolRef: string, options?: OpenPoolOptions): Pool
  listPools(): PoolInfo[]
  deletePool(poolRef: string): void
  close(): void
  [Symbol.dispose](): void
}

/** Committed sequence and Unix nanosecond time. */
export interface FeedReceipt {
  seq: bigint
  time: bigint
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
  readonly time: bigint
  readonly meta: View<{ tags: string[] }>
  readonly data: View<T>
}
export type Native<T> = T extends MessageView<infer D>
  ? { seq: bigint; time: bigint; meta: { tags: string[] }; data: Native<View<D>> }
  : T extends BytesView ? Uint8Array
  : T extends object ? { -readonly [K in keyof T]: Native<T[K]> } : T
/** Build independent mutable containers from a view or supported scalar. */
export function toNative<T>(value: T): Native<T>
/** Format directly from a view, preserving exact stored integer digits. */
export function toJson(value: unknown): string
/** Copy one complete message's encoded payload into an independent mutable Buffer. */
export function toLite3(message: MessageView): Buffer

export class Pool {
  feedReceipt(value: object | Message, options?: FeedOptions): Promise<FeedReceipt>
  feedJsonReceipt(payload: Buffer, options?: FeedOptions): FeedReceipt
  feedLite3Receipt(payload: Buffer, options?: Omit<FeedOptions, "tags">): FeedReceipt
  feedLite3(payload: Buffer, options?: Omit<FeedOptions, "tags">): Promise<Message>
  info(): PoolInfo
  feed(value: object | Message, options?: FeedOptions): Promise<Message>
  feedJson(payload: Buffer, options?: FeedOptions): Buffer
  feedView<T = unknown>(value: unknown, options?: FeedOptions): Promise<MessageView<T>>
  followView<T = unknown>(options?: ReadOptions): Promise<Reader<MessageView<T>>>
  fetchView<T = unknown>(options?: ReadOptions): Promise<Reader<MessageView<T>>>
  followLite3(options?: ReadOptions): Promise<Reader<Lite3Frame>>
  fetchLite3(options?: ReadOptions): Promise<Reader<Lite3Frame>>
  fetch(options?: ReadOptions): Promise<Reader>
  follow(options?: ReadOptions): Promise<Reader>
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

export interface UdpWaitOptions {
  signal?: AbortSignal
}

export type UdpEvent =
  | {
      kind: "frame"
      seq: bigint
      time: bigint
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

export class UdpClient {
  constructor(endpoint: string)
  openPool(poolName: string): UdpPool
  close(): void
  [Symbol.dispose](): void
}

export class UdpPool {
  feed(payload: object | Message, options?: FeedOptions): void
  fetch(options?: ReadOptions): Promise<UdpFollow>
  follow(options?: ReadOptions): Promise<UdpFollow>
  followView<T = unknown>(options?: ReadOptions): Promise<UdpViewFollow<T>>
  fetchView<T = unknown>(options?: ReadOptions): Promise<UdpViewFollow<T>>
  feedLite3(payload: Buffer): void
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

export type UdpViewEvent<T = unknown> =
  | { kind: "frame"; seq: bigint; time: bigint; message: MessageView<T> }
  | Exclude<UdpEvent, { kind: "frame" }>

export interface UdpViewFollow<T = unknown> {
  readonly finished: boolean
  nextEvent(timeoutMs?: number | bigint, options?: UdpWaitOptions): Promise<UdpViewEvent<T> | null>
  events(options?: { pollIntervalMs?: number | bigint; signal?: AbortSignal }): AsyncGenerator<UdpViewEvent<T>, void, unknown>
  cancel(): void
  close(): void
  [Symbol.dispose](): void
  [Symbol.asyncIterator](): AsyncGenerator<UdpViewEvent<T>, void, unknown>
}

export function parseMessage(payload: Buffer | Message | MessageEnvelope): Message

export interface RemoteClientOptions {
  token?: string
}

export type RemotePoolInfo = PoolInfo

export interface RemoteLite3Frame {
  seq: bigint
  time: bigint
  payload: Buffer
}

export class RemoteError extends Error {
  status: number
  kind: ErrorKind
  hint?: string
  path?: string
  seq?: bigint
  offset?: bigint
  missing?: bigint
  nextSeq?: bigint
}

export class RemoteClient {
  constructor(baseUrl: string, options?: RemoteClientOptions)
  baseUrl: URL
  token: string | null
  withToken(token: string): RemoteClient
  createPool(pool: string, sizeBytes?: number | bigint): Promise<RemotePool>
  openPool(pool: string, options?: OpenPoolOptions): Promise<RemotePool>
  poolInfo(pool: string): Promise<RemotePoolInfo>
  listPools(): Promise<PoolInfo[]>
  deletePool(pool: string): Promise<void>
}

export class RemotePool {
  constructor(client: RemoteClient, pool: string)
  client: RemoteClient
  pool: string
  poolRef(): string
  feedLite3(payload: Buffer, options?: FeedOptions): Promise<Message>
  feedView<T = unknown>(value: unknown, options?: FeedOptions): Promise<MessageView<T>>
  followLite3(options?: ReadOptions): Promise<Reader<RemoteLite3Frame>>
  fetchLite3(options?: ReadOptions): Promise<Reader<RemoteLite3Frame>>
  info(): Promise<PoolInfo>
  feed(data: object | Message, options?: FeedOptions): Promise<Message>
  followView<T = unknown>(options?: ReadOptions): Promise<Reader<MessageView<T>>>
  fetchView<T = unknown>(options?: ReadOptions): Promise<Reader<MessageView<T>>>
  fetch(options?: ReadOptions): Promise<Reader>
  follow(options?: ReadOptions): Promise<Reader>
}

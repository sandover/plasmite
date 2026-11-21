/*
Purpose: Stable TypeScript declarations for the public Node bindings API.
Key Exports: Client, Pool, Message, Reader, and remote client types.
Role: Preserve complete JS + native type surface independently of generated files.
Invariants: Public runtime exports from index.js are represented here.
Invariants: Numeric sequence fields accept number or bigint input.
Notes: Canonical declaration source for package typing (`package.json#types`).
*/

export const Durability: {
  readonly Fast: 0
  readonly Flush: 1
}
export type Durability = typeof Durability[keyof typeof Durability]

export const ErrorKind: {
  readonly Internal: 1
  readonly Usage: 2
  readonly NotFound: 3
  readonly AlreadyExists: 4
  readonly Busy: 5
  readonly Permission: 6
  readonly Corrupt: 7
  readonly Io: 8
  readonly RetentionGap: 9
}
export type ErrorKind = typeof ErrorKind[keyof typeof ErrorKind]
export const DEFAULT_POOL_DIR: string | undefined
export const DEFAULT_POOL_SIZE_BYTES: number

export interface MessageMeta {
  readonly tags: readonly string[]
  readonly sender?: string
}

export class Message<T = unknown> {
  private constructor()
  readonly seq: bigint
  readonly time: bigint
  readonly data: T
  readonly meta: MessageMeta
  readonly tags: readonly string[]
  readonly sender?: string
  /** Copy of the stored Lite3 bytes. */
  readonly raw: Buffer
  field<K extends keyof T & string>(name: K): View<T[K]> | undefined
  field(name: string): unknown
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
export interface PoolDescription {
  about?: string
  example?: Record<string, unknown>
}
export interface CreatePoolOptions extends PoolDescription {
  sizeBytes?: number | bigint
}
export interface PoolInfo extends PoolDescription {
  name: string
  path: string
  file_size: bigint
  bounds: { oldest?: bigint; newest?: bigint }
}
export interface ReadOptions {
  since?: number | bigint | string | Instant
  last?: number | bigint
  tags?: readonly string[]
  signal?: AbortSignal
}
export interface FeedOptions { tags?: readonly string[]; durability?: Durability }
type RemoteFeedOptions = Omit<FeedOptions, "durability"> & { durability?: Durability | "fast" | "flush" }
type ForwardOptions<Options> = Omit<Options, "tags">
type BytesOptions<Options> = (Options & { format: "json" }) | (ForwardOptions<Options> & { format: "lite3" })
declare const instantType: unique symbol
export interface Instant { readonly [instantType]: true }
export function at(time: number | bigint): Instant
export function createPool(poolRef: string, options?: number | bigint | CreatePoolOptions): Pool
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
  createPool(poolRef: string, options?: number | bigint | CreatePoolOptions): Pool
  openPool(poolRef: string, options?: OpenPoolOptions): Pool
  listPools(): PoolInfo[]
  deletePool(poolRef: string): void
  close(): void
  [Symbol.dispose](): void
}

declare const byteViewType: unique symbol

export interface BytesView extends Iterable<number> {
  readonly [byteViewType]: true
  readonly length: number
  readonly [index: number]: number
}
export type View<T> = T extends Uint8Array ? BytesView
  : T extends object ? { readonly [K in keyof T]: View<T[K]> } : T
export type Native<T> = T extends Message<infer D>
  ? { seq: bigint; time: bigint; meta: { tags: string[]; sender?: string }; data: Native<View<D>> }
  : T extends BytesView ? Uint8Array
  : T extends Uint8Array ? T
  : T extends object ? { -readonly [K in keyof T]: Native<T[K]> } : T
/** Build independent mutable containers from a message, a field, or a supported scalar. */
export function toNative<T>(value: T): Native<T>
/** Format a message, a field, or a scalar, preserving exact stored integer digits. */
export function toJson(value: unknown): string

export class Pool {
  info(): PoolInfo
  setDescription(description: PoolDescription): void
  feed<T>(message: Message<T>, options?: ForwardOptions<FeedOptions>): Promise<Message<T>>
  feed<T = unknown>(bytes: Uint8Array, options: BytesOptions<FeedOptions>): Promise<Message<T>>
  feed<T extends object>(value: T extends Uint8Array | Message ? never : T, options?: FeedOptions): Promise<Message<T>>
  fetch<T = unknown>(options?: ReadOptions): Promise<Reader<Message<T>>>
  follow<T = unknown>(options?: ReadOptions): Promise<Reader<Message<T>>>
  close(): void
  [Symbol.dispose](): void
}

export interface UdpWaitOptions {
  signal?: AbortSignal
}

export type UdpEvent<T = unknown> =
  | {
      kind: "frame"
      seq: bigint
      time: bigint
      payload: Buffer
      message: Message<T>
    }
  // "unknown" is reserved.
  | { kind: "skip"; first: bigint; last: bigint; reason: "unknown" | "oversized" | "retention" | "pressure" }
  | {
      kind: "progress"
      startSeq: bigint
      nextSeq: bigint
      endSeq: bigint | null
      terminal: boolean
    }

export class UdpClient {
  constructor(endpoint: string)
  openPool(poolName: string): UdpPool
  close(): void
  [Symbol.dispose](): void
}

export class UdpPool {
  feed(message: Message, options?: ForwardOptions<FeedOptions>): Promise<void>
  feed(bytes: Uint8Array, options: BytesOptions<FeedOptions>): Promise<void>
  feed<T extends object>(value: T extends Uint8Array | Message ? never : T, options?: FeedOptions): Promise<void>
  fetch<T = unknown>(options?: ReadOptions): Promise<UdpFollow<T>>
  follow<T = unknown>(options?: ReadOptions): Promise<UdpFollow<T>>
  close(): void
  [Symbol.dispose](): void
}

export class UdpFollow<T = unknown> implements Reader<Message<T>> {
  readonly finished: boolean
  nextEvent(timeoutMs?: number, options?: UdpWaitOptions): Promise<UdpEvent<T> | null>
  next(): Promise<IteratorResult<Message<T>, undefined>>
  next(wait?: number): Promise<IteratorResult<Message<T> | undefined, undefined>>
  return(): Promise<IteratorResult<Message<T>, undefined>>
  events(options?: { signal?: AbortSignal }): AsyncGenerator<UdpEvent<T>, void, unknown>
  cancel(): void
  close(): void
  [Symbol.dispose](): void
  [Symbol.asyncIterator](): UdpFollow<T>
}

export interface RemoteClientOptions {
  token?: string
}

export type RemotePoolInfo = PoolInfo

export class RemoteError extends Error {
  status: number
  cause?: unknown
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
  createPool(pool: string, options?: number | bigint | CreatePoolOptions): Promise<RemotePool>
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
  info(): Promise<PoolInfo>
  setDescription(description: PoolDescription): Promise<void>
  feed<T>(message: Message<T>, options?: ForwardOptions<RemoteFeedOptions>): Promise<Message<T>>
  feed<T = unknown>(bytes: Uint8Array, options: BytesOptions<RemoteFeedOptions>): Promise<Message<T>>
  feed<T extends object>(value: T extends Uint8Array | Message ? never : T, options?: RemoteFeedOptions): Promise<Message<T>>
  fetch<T = unknown>(options?: ReadOptions): Promise<Reader<Message<T>>>
  follow<T = unknown>(options?: ReadOptions): Promise<Reader<Message<T>>>
}

/*
Purpose: TypeScript definitions for the Plasmite Node binding.
Key Exports: Client, Pool, Stream, Durability, ErrorKind.
Role: Provide typed access to the N-API addon in JS/TS.
Invariants: Type signatures follow the native binding surface.
Notes: Large numeric values should be passed as bigint.
*/

export enum Durability {
  Fast = 0,
  Flush = 1,
}

export enum ErrorKind {
  Internal = 1,
  Usage = 2,
  NotFound = 3,
  AlreadyExists = 4,
  Busy = 5,
  Permission = 6,
  Corrupt = 7,
  Io = 8,
}

export class Client {
  constructor(poolDir: string)
  createPool(poolRef: string, sizeBytes: bigint | number): Pool
  openPool(poolRef: string): Pool
  close(): void
}

export class Pool {
  appendJson(payload: Buffer, descrips: string[], durability: Durability): Buffer
  getJson(seq: bigint | number): Buffer
  openStream(
    sinceSeq?: bigint | number,
    maxMessages?: bigint | number,
    timeoutMs?: bigint | number
  ): Stream
  close(): void
}

export class Stream {
  nextJson(): Buffer | null
  close(): void
}

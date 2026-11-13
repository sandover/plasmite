// Additive owned-view API usage/type contract; declarations do not imply shipped APIs.
// Check: node bindings/node/node_modules/typescript/lib/tsc.js --noEmit --strict
//   --target ES2022 --skipLibCheck examples/owned_views.ts
declare const byteViewTag: unique symbol;
interface BytesView extends Iterable<number> {
  readonly [byteViewTag]: true;
  readonly length: number;
  readonly [index: number]: number;
}
type View<T> = T extends Uint8Array ? BytesView
  : T extends object ? { readonly [K in keyof T]: View<T[K]> } : T;
type Native<T> = T extends BytesView ? Uint8Array
  : T extends object ? { -readonly [K in keyof T]: Native<T[K]> } : T;
declare function toNative<T>(value: T): Native<T>;
declare function get<T = unknown>(seq: bigint): Promise<{ readonly data: View<T> }>;
interface Reading {
  sensor: string;
  metrics: { power: number };
  samples: Array<{ value: number }>;
  optional?: { value: string } | null;
  pair: readonly [string, { readonly value: number }];
  bytes: Uint8Array;
}
async function check() {
  const m = await get<Reading>(17n);
  for (const sample of m.data.samples) { const value: number = sample.value; void value; }
  const shallow = { ...m.data }; // Child containers remain views.
  // @ts-expect-error shallow copies retain readonly children
  shallow.metrics.power = 1;
  const root: Reading = toNative(m.data);
  const metrics: { power: number } = toNative(m.data.metrics);
  const samples: Array<{ value: number }> = toNative(m.data.samples);
  const child: { value: number } = toNative(m.data.samples[0]);
  const tuple: [string, { value: number }] = toNative(m.data.pair);
  const bytes: Uint8Array = toNative(m.data.bytes);
  const scalar: number = toNative(m.data.metrics.power);
  const keys: keyof typeof m.data.metrics = 'power';
  // @ts-expect-error immutable field
  m.data.metrics.power = 1;
  // @ts-expect-error immutable array
  m.data.samples.push({ value: 1 });
  // @ts-expect-error byte view exposes no mutable typed-array methods
  m.data.bytes.fill(0);
  // @ts-expect-error byte view is immutable
  m.data.bytes[0] = 1;
  root.metrics.power = 1;
  metrics.power = 1;
  samples.push(child);
  tuple[1].value = 1;
  bytes.fill(0);
  void [scalar, keys];
}
function generic<T>(view: View<T>): Native<View<T>> {
  return toNative(view);
}
void [check, generic];

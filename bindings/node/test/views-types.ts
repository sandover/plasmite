import { Pool, toNative, toJson } from '../types';
declare const pool: Pool;
interface Reading { metrics: { power: number }; samples: { value: number }[]; bytes: Uint8Array }
async function usage() {
  const m = await pool.getView<Reading>(17n);
  const power: number = m.data.metrics.power;
  const editable: Reading = toNative(m.data);
  const child: { power: number } = toNative(m.data.metrics);
  const envelope: { seq: bigint; time: string; meta: { tags: string[] }; data: Reading } = toNative(m);
  editable.metrics.power = 1;
  child.power = 2;
  // @ts-expect-error views are immutable
  m.data.metrics.power = 2;
  // @ts-expect-error views have no mutable array methods
  m.data.samples.push({ value: 1 });
  const bytes: Uint8Array = toNative(m.data.bytes);
  bytes.fill(0);
  return [power, envelope, toJson(m)];
}
void usage;

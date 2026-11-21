import { Pool, toNative, toJson } from '../types';
declare const pool: Pool;
interface Reading { metrics: { power: number }; samples: { value: number }[]; bytes: Uint8Array }
async function usage() {
  const m = (await (await pool.fetch<Reading>({ since: 17n })).next()).value!;
  const written = await pool.feed({ metrics: { power: 42 } });
  const writtenPower: number = written.data.metrics.power;
  await pool.feed(m.data, { tags: m.meta.tags });
  await pool.feed({}, { tags: ['sensor'] as const });
  const sparse = written.field('metrics')!;
  // @ts-expect-error sparse values remain immutable
  sparse.power = 2;
  const power: number = m.data.metrics.power;
  const editable: Reading = toNative(m.data);
  const child: { power: number } = toNative(m.data.metrics);
  const envelope: { seq: bigint; time: bigint; meta: { tags: string[] }; data: Reading } = toNative(m);
  editable.metrics.power = 1;
  child.power = 2;
  // @ts-expect-error the decoded data property is read-only
  m.data = editable;
  const samples = m.field('samples')!;
  // @ts-expect-error sparse array views have no mutable methods
  samples.push({ value: 1 });
  const bytes: Uint8Array = toNative(m.data.bytes);
  bytes.fill(0);
  return [power, writtenPower, envelope, toJson(m)];
}
void usage;

async function streamTypes(pool: Pool, udp: import('../types').UdpPool) {
  for await (const message of await pool.fetch<Reading>()) {
    const metrics = message.field('metrics')!;
    const power: number = metrics.power;
    // @ts-expect-error sparse values remain immutable
    metrics.power = power;
  }
  const follow = await udp.follow<Reading>();
  for await (const message of follow) {
    const power: number = message.data.metrics.power;
    void power;
    break;
  }
  for await (const event of follow.events()) {
    if (event.kind === 'frame') {
      const power: number = event.message.data.metrics.power;
      const payload: Buffer = event.message.raw;
      void power;
      void payload;
    }
  }
  follow.close();
}
void streamTypes;

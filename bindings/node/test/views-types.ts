import { Pool, toNative, toJson } from '../types';
declare const pool: Pool;
interface Reading { metrics: { power: number }; samples: { value: number }[]; bytes: Uint8Array }
async function usage() {
  const m = (await (await pool.fetchView<Reading>({ since: 17n })).next()).value!;
  const written = await pool.feedView<Reading>({ metrics: { power: 42 } });
  const writtenPower: number = written.data.metrics.power;
  await pool.feedView(m.data, { tags: m.meta.tags });
  await pool.feedView({}, { tags: ['sensor'] as const });
  // @ts-expect-error committed results remain immutable
  written.data.metrics.power = 2;
  const power: number = m.data.metrics.power;
  const editable: Reading = toNative(m.data);
  const child: { power: number } = toNative(m.data.metrics);
  const envelope: { seq: bigint; time: bigint; meta: { tags: string[] }; data: Reading } = toNative(m);
  editable.metrics.power = 1;
  child.power = 2;
  // @ts-expect-error views are immutable
  m.data.metrics.power = 2;
  // @ts-expect-error views have no mutable array methods
  m.data.samples.push({ value: 1 });
  const bytes: Uint8Array = toNative(m.data.bytes);
  bytes.fill(0);
  return [power, writtenPower, envelope, toJson(m)];
}
void usage;

async function streamTypes(pool: Pool, udp: import('../types').UdpPool) {
  for await (const message of await pool.fetchView<Reading>()) {
    const power: number = message.data.metrics.power;
    // @ts-expect-error streamed snapshots stay immutable
    message.data.metrics.power = power;
  }
  const follow = await udp.followView<Reading>();
  for await (const event of follow) {
    if (event.kind === 'frame') {
      const power: number = event.message.data.metrics.power;
      // @ts-expect-error owned UDP data has no eagerly copied payload Buffer
      event.payload;
      void power;
    }
  }
  follow.close();
}
void streamTypes;

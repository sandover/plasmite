// Fixtures for the HTTP Lite3 message and control records.
function packet(kind, payload) {
  const header = Buffer.alloc(5);
  header[0] = kind;
  header.writeUInt32BE(payload.length, 1);
  return Buffer.concat([header, payload]);
}

function frame(seq, time, payload) {
  const identity = Buffer.alloc(16);
  identity.writeBigUInt64BE(seq, 0);
  identity.writeBigUInt64BE(time, 8);
  return packet(0x4d, Buffer.concat([identity, payload]));
}

function control(value) {
  return packet(0x43, Buffer.from(typeof value === "string" ? value : JSON.stringify(value)));
}

module.exports = { packet, frame, control };

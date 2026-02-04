/*
Purpose: JavaScript entry point for the Plasmite Node binding.
Key Exports: Client, Pool, Stream, Durability, ErrorKind.
Role: Thin wrapper around the native N-API addon.
Invariants: Exports align with native symbols and v0 API semantics.
Notes: Requires libplasmite to be discoverable at runtime.
*/

const native = require("./index.node");
const { RemoteClient, RemoteError, RemotePool, RemoteTail } = require("./remote");

module.exports = {
  ...native,
  RemoteClient,
  RemoteError,
  RemotePool,
  RemoteTail,
};

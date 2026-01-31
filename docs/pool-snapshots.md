# Pool validator snapshots

Debug-only snapshot files are written to `.scratch/` when explicitly enabled and
validator assertions fail.

Format (plain text)
- `timestamp_ms=<ms>`
- `header file_size=<...> ring_offset=<...> ring_size=<...> head_off=<...> tail_off=<...> oldest_seq=<...> newest_seq=<...>`
- `tail: state=<...> seq=<...> payload_len=<...> frame_len=<...> magic=[..]` or `decode_error=<...> magic=[..]`
- `head: state=<...> seq=<...> payload_len=<...> frame_len=<...> magic=[..]` or `decode_error=<...> magic=[..]`

Snapshots are small by design and intended for post-mortem debugging.

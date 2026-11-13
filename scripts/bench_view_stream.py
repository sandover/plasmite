"""Matched ordinary and explicit-batch view readers, with requested conversion."""


def data(plasmite, pool, messages, batch_count, batch_bytes, conversion):
    with pool.tail_views(
        since_seq=1, max_messages=messages, timeout_ms=120_000, error_on_gap=True
    ) as stream:

        def convert(message):
            if conversion == "message-json":
                # Preserve the existing full JSON/dataclass/datetime/raw consumer work.
                return plasmite.parse_message(
                    plasmite.to_json(message).encode("utf-8")
                ).data
            if conversion == "native":
                return plasmite.to_native(message.data)
            raise ValueError("unknown requested view conversion")

        if batch_count == 0:
            for message in stream:
                yield convert(message)
        else:
            while True:
                try:
                    batch = stream.next_batch(batch_count, batch_bytes)
                except StopIteration:
                    return
                for message in batch:
                    yield convert(message)

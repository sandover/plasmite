"""Matched ordinary and explicit-batch view readers, with requested conversion."""


def data(plasmite, pool, messages, batch_count, batch_bytes, conversion):
    current = hasattr(pool, "follow_view")
    if current:
        reader = pool.follow_view(since=1)
    else:
        reader = pool.follow_views(
            start=plasmite.FollowStart.sequence(1), max_messages=messages,
            timeout_ms=120_000, error_on_gap=True,
        )
    with reader as stream:

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
            for _ in range(messages):
                message = stream.next(wait=120) if current else next(stream, None)
                if message is None:
                    raise RuntimeError("view stream ended before all benchmark messages arrived")
                yield convert(message)
        else:
            while True:
                try:
                    wait = {"wait": 120} if current else {}
                    batch = stream.next_batch(batch_count, batch_bytes, **wait)
                except StopIteration:
                    return
                if not batch:
                    raise RuntimeError("view stream ended before the next benchmark batch arrived")
                for message in batch:
                    yield convert(message)

"""Matched ordinary and explicit-batch view readers, with requested conversion."""

import json


def data(plasmite, pool, messages, batch_count, batch_bytes, conversion):
    current = hasattr(plasmite.Message, "sender")
    previous = hasattr(pool, "follow_view")
    if current:
        from plasmite._views import to_json, to_native
        reader = pool.follow(since=1)._reader
    elif previous:
        to_json, to_native = plasmite.to_json, plasmite.to_native
        reader = pool.follow_view(since=1)
    else:
        to_json, to_native = plasmite.to_json, plasmite.to_native
        reader = pool.follow_views(
            start=plasmite.FollowStart.sequence(1), max_messages=messages,
            timeout_ms=120_000, error_on_gap=True,
        )
    with reader as stream:

        def convert(message):
            if conversion == "message-json":
                return json.loads(to_json(message))["data"]
            if conversion == "native":
                return to_native(message.data)
            raise ValueError("unknown requested view conversion")

        if batch_count == 0:
            for _ in range(messages):
                message = stream.next(wait=120) if current or previous else next(stream, None)
                if message is None:
                    raise RuntimeError("view stream ended before all benchmark messages arrived")
                yield convert(message)
        else:
            while True:
                try:
                    wait = {"wait": 120} if current or previous else {}
                    batch = stream.next_batch(batch_count, batch_bytes, **wait)
                except StopIteration:
                    return
                if not batch:
                    raise RuntimeError("view stream ended before the next benchmark batch arrived")
                for message in batch:
                    yield convert(message)

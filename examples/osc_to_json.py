#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["python-osc==1.10.2"]
# ///
"""Receive Open Sound Control messages and print JSON Lines for plasmite feed.

Run with uv so the OSC dependency stays outside your project environment.
Use --send-test in another terminal to send /fader/1 with the value 0.75.
"""

import argparse
import contextlib
import json
import os
import sys

from pythonosc.dispatcher import Dispatcher
from pythonosc.osc_server import BlockingOSCUDPServer
from pythonosc.udp_client import SimpleUDPClient


def emit_message(address, *args):
    """Capture JSON-compatible values; keep diagnostics out of the pipe."""
    try:
        line = json.dumps(
            {"address": address, "args": args},
            ensure_ascii=False,
            allow_nan=False,
            separators=(",", ":"),
        )
    except (TypeError, ValueError) as error:
        print(f"Skipping {address}: arguments cannot become JSON ({error})", file=sys.stderr)
        return
    print(line, flush=True)


def port_number(value):
    port = int(value)
    if not 0 <= port <= 65535:
        raise argparse.ArgumentTypeError("port must be between 0 and 65535")
    return port


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1", help="listen address or test destination (default: %(default)s)")
    parser.add_argument("--port", type=port_number, default=9000, help="UDP port (default: %(default)s; 0 picks a free listen port)")
    parser.add_argument("--send-test", action="store_true", help="send /fader/1 0.75 and exit")
    args = parser.parse_args()

    if args.send_test:
        if args.port == 0:
            parser.error("--send-test needs a nonzero destination port")
        with contextlib.closing(SimpleUDPClient(args.host, args.port)) as client:
            client.send_message("/fader/1", 0.75)
        return

    # Capture on receipt; do not wait for future bundle timetags.
    dispatcher = Dispatcher(strict_timing=False)
    dispatcher.set_default_handler(emit_message)
    try:
        with BlockingOSCUDPServer((args.host, args.port), dispatcher) as server:
            host, port = server.server_address
            print(f"Listening for OSC on {host}:{port}; stop with Ctrl+C", file=sys.stderr, flush=True)
            server.serve_forever()
    except KeyboardInterrupt:
        pass
    except BrokenPipeError:
        raise
    except OSError as error:
        parser.exit(1, f"OSC receiver: {error}\nCheck the listen address and whether another program uses this port.\n")


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        # Avoid a second broken-pipe error when Python flushes during exit.
        sys.stdout = open(os.devnull, "w")
        raise SystemExit(1)

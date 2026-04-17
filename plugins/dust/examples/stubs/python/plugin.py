#!/usr/bin/env python3
"""Minimal Python stub implementing the dust v1 handshake protocol."""

import argparse
import asyncio
import json
import os
import signal
import socket
import struct
import sys
from datetime import datetime, timezone

PLUGIN_ID = "python-stub"
PROTO_VERSION = "1.0.0"

MANIFEST = {
    "name": "Python Stub",
    "version": "0.1.0",
    "description": "Minimal Python stub implementing the dust v1 handshake.",
    "capabilities": [{"kind": "command", "prefix": "python-stub"}],
    "icon": None,
}

_seq = 0


def next_seq() -> int:
    global _seq
    _seq += 1
    return _seq


def now_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:23] + "Z"


def runtime_dir() -> str:
    xdg = os.environ.get("XDG_RUNTIME_DIR")
    if xdg:
        return os.path.join(xdg, "nanika", "plugins")
    home = os.path.expanduser("~")
    return os.path.join(home, ".alluka", "run", "plugins")


async def read_msg(reader: asyncio.StreamReader) -> dict:
    header = await reader.readexactly(4)
    n = struct.unpack(">I", header)[0]
    if n == 0:
        raise EOFError
    payload = await reader.readexactly(n)
    return json.loads(payload)


async def write_msg(writer: asyncio.StreamWriter, env: dict) -> None:
    data = json.dumps(env, separators=(",", ":")).encode()
    writer.write(struct.pack(">I", len(data)) + data)
    await writer.drain()


def dispatch(req: dict) -> dict:
    method = req.get("method", "")
    rid = req.get("id")
    if method in ("manifest", "refresh_manifest"):
        return {"kind": "response", "id": rid, "result": MANIFEST}
    if method == "render":
        return {"kind": "response", "id": rid, "result": []}
    if method == "action":
        return {"kind": "response", "id": rid, "result": {"success": True}}
    return {
        "kind": "response",
        "id": rid,
        "error": {"code": -32601, "message": f"method not found: {method}"},
    }


async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    peer = writer.get_extra_info("peername") or "conn"
    try:
        seq = next_seq()
        ready_data = {
            "manifest": MANIFEST,
            "protocol_version": PROTO_VERSION,
            "plugin_info": {"pid": os.getpid(), "started_at": now_ts()},
        }
        await write_msg(writer, {
            "kind": "event",
            "id": f"evt_{seq:016x}",
            "type": "ready",
            "ts": now_ts(),
            "sequence": seq,
            "data": ready_data,
        })

        try:
            hi = await asyncio.wait_for(read_msg(reader), timeout=5.0)
        except asyncio.TimeoutError:
            print(f"dust: {peer}: host_info timeout", file=sys.stderr)
            return
        if hi.get("kind") != "event" or hi.get("type") != "host_info":
            print(f"dust: {peer}: expected host_info, got {hi.get('kind')!r}/{hi.get('type')!r}", file=sys.stderr)
            return

        while True:
            try:
                env = await read_msg(reader)
            except (EOFError, asyncio.IncompleteReadError):
                return
            kind = env.get("kind")
            if kind == "request":
                await write_msg(writer, dispatch(env))
            elif kind == "heartbeat":
                await write_msg(writer, {"kind": "heartbeat", "ts": now_ts()})
            elif kind == "shutdown":
                return
    except (ConnectionResetError, BrokenPipeError):
        pass
    finally:
        try:
            writer.close()
            await writer.wait_closed()
        except Exception:
            pass


async def run(sock_path: str) -> None:
    os.makedirs(os.path.dirname(sock_path), exist_ok=True)
    try:
        os.remove(sock_path)
    except FileNotFoundError:
        pass

    server = await asyncio.start_unix_server(handle, path=sock_path)
    print(f"dust: {PLUGIN_ID} listening on {sock_path}", file=sys.stderr)

    loop = asyncio.get_running_loop()
    stop = loop.create_future()

    def _shutdown(signum, frame):
        if not stop.done():
            loop.call_soon_threadsafe(stop.set_result, signum)

    signal.signal(signal.SIGINT, _shutdown)
    signal.signal(signal.SIGTERM, _shutdown)

    async with server:
        await stop

    try:
        os.remove(sock_path)
    except FileNotFoundError:
        pass


def main() -> None:
    parser = argparse.ArgumentParser(
        description="python-stub: minimal dust v1 plugin stub (UDS, stdlib only)"
    )
    parser.add_argument(
        "--socket", metavar="PATH",
        help="override UDS socket path (default: $XDG_RUNTIME_DIR/nanika/plugins/python-stub.sock)",
    )
    args = parser.parse_args()

    sock_path = args.socket or os.path.join(runtime_dir(), f"{PLUGIN_ID}.sock")
    asyncio.run(run(sock_path))


if __name__ == "__main__":
    main()

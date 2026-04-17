#!/usr/bin/env python3
"""mutate-tracker.py — Send a mutation action to the tracker dust plugin via the
protocol (not the CLI), so dust-serve's event ring fires and live-tracker.py
sees it in real time.

Usage:
  python3 mutate-tracker.py create "Issue title" [--priority P1] [--description "..."]
  python3 mutate-tracker.py update TRK-42 [--status in-progress] [--priority P2] [--title "..."]
  python3 mutate-tracker.py delete TRK-42

Start `tracker dust-serve` first. This script spawns its own short-lived
connection, runs the mutation, and exits.
"""
import asyncio, json, os, struct, sys, uuid
from pathlib import Path

PLUGIN = "dust-tracker"

def sock():
    x = os.environ.get("XDG_RUNTIME_DIR", "")
    return str((Path(x)/"nanika"/"plugins" if x else Path.home()/".alluka"/"run"/"plugins")/f"{PLUGIN}.sock")

async def tx(w, obj):
    b = json.dumps(obj).encode()
    w.write(struct.pack(">I", len(b)) + b)
    await w.drain()

async def rx(r):
    n = struct.unpack(">I", await r.readexactly(4))[0]
    return await rx(r) if n == 0 else json.loads(await r.readexactly(n))

async def run(action, item_id, args):
    reader, writer = await asyncio.open_unix_connection(sock())
    # Drain the ready event from the plugin
    ready = await rx(reader)
    assert ready.get("type") == "ready", f"expected ready, got {ready}"
    # Send our host_info event (required to complete handshake)
    await tx(writer, {
        "kind": "event",
        "id": f"evt_{uuid.uuid4().hex[:16]}",
        "type": "host_info",
        "ts": "2026-04-13T00:00:00Z",
        "data": {"host_name": "mutate-tracker", "host_version": "0.1.0",
                 "protocol_version_supported": {"min": "1.0.0", "max": "1.999.999"},
                 "consumer_count": 1}
    })
    # Send the action request
    req_id = f"req_{uuid.uuid4().hex[:16]}"
    params = {"action": action, "args": args}
    if item_id:
        params["item_id"] = item_id
    await tx(writer, {"kind": "request", "id": req_id, "method": "action", "params": params})
    # Wait for the response (skip any events that arrive first)
    while True:
        msg = await rx(reader)
        if msg.get("kind") == "response" and msg.get("id") == req_id:
            if msg.get("error"):
                print(f"✘ {msg['error']}", file=sys.stderr)
                sys.exit(1)
            result = msg.get("result", {})
            print(f"✓ {result.get('message', 'ok')}")
            break
    writer.close()
    await writer.wait_closed()

def parse_args(argv):
    if len(argv) < 2 or argv[1] in ("-h", "--help"):
        print(__doc__); sys.exit(0)
    action, rest = argv[1], argv[2:]
    args, item_id, i = {}, None, 0
    # create: first positional is title; update/delete: first is item_id
    if action == "create" and rest and not rest[0].startswith("--"):
        args["title"] = rest[0]; i = 1
    elif action in ("update", "delete") and rest and not rest[0].startswith("--"):
        item_id = rest[0]; i = 1
    while i < len(rest):
        flag = rest[i].lstrip("-")
        if i+1 < len(rest) and not rest[i+1].startswith("--"):
            args[flag] = rest[i+1]; i += 2
        else:
            args[flag] = True; i += 1
    return action, item_id, args

if __name__ == "__main__":
    action, item_id, args = parse_args(sys.argv)
    try:
        asyncio.run(run(action, item_id, args))
    except KeyboardInterrupt:
        pass

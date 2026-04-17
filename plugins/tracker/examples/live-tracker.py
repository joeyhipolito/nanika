#!/usr/bin/env python3
"""live-tracker.py — Standalone dust-wire consumer proving the protocol works
without the dust SDK. Start the tracker plugin first (tracker dust-serve or the
tracker-dust binary), then run: python3 plugins/tracker/examples/live-tracker.py
It connects via UDS, handshakes, subscribes at sequence 0, prints replayed issues
as TRK-N lines, and streams live data_updated events until Ctrl-C."""
import asyncio, json, os, struct, sys
from pathlib import Path
PLUGIN = "dust-tracker"
def sock():
    x = os.environ.get("XDG_RUNTIME_DIR", "")
    return str((Path(x)/"nanika"/"plugins" if x else Path.home()/".alluka"/"run"/"plugins")/f"{PLUGIN}.sock")
async def tx(w, obj):
    b = json.dumps(obj).encode(); w.write(struct.pack(">I", len(b)) + b); await w.drain()
async def rx(r):
    n = struct.unpack(">I", await r.readexactly(4))[0]
    return await rx(r) if n == 0 else json.loads(await r.readexactly(n))
def show(d):
    items = d if isinstance(d, list) else d.get("issues", [d]) if isinstance(d, dict) else []
    for i in items:
        if isinstance(i, dict) and "title" in i:
            sid = f"TRK-{i['seq_id']}" if i.get("seq_id") else i.get("id", "?")[:8]
            print(f"{sid}\t{i['title']}\t{i.get('status', '?')}\t{i.get('priority', '-')}")
async def main():
    p = sock(); print(f"connecting {p}", file=sys.stderr)
    r, w = await asyncio.open_unix_connection(p)
    try:
        ready = await rx(r)
        m = ready["data"]["manifest"]; print(f"ready: {m['name']} v{m['version']}", file=sys.stderr)
        await tx(w, {"kind": "event", "id": "evt_host0000000001", "type": "host_info", "ts": "", "sequence": None,
            "data": {"host_name": "live-tracker.py", "host_version": "0.1.0", "protocol_version_supported": {"min": "1.0.0", "max": "1.999.999"}, "consumer_count": 1}})
        await tx(w, {"kind": "request", "id": "req_0000000000000001", "method": "events.subscribe", "params": {"since_sequence": 0}})
        resp = await rx(r)
        if resp.get("error"): print(f"subscribe error: {resp['error']}", file=sys.stderr); return
        for e in resp.get("result", {}).get("events", []):
            if e.get("type") == "data_updated": show(e.get("data", {}))
        print("-- streaming live events (ctrl-c to quit) --", file=sys.stderr)
        while True:
            env = await rx(r)
            if env.get("kind") == "heartbeat": await tx(w, {"kind": "heartbeat", "ts": ""})
            elif env.get("kind") == "event" and env.get("type") == "data_updated":
                show(env.get("data", {}))
            elif env.get("kind") == "shutdown": print("plugin shutdown", file=sys.stderr); break
    except (asyncio.IncompleteReadError, ConnectionResetError):
        print("connection closed", file=sys.stderr)
    finally: w.close()
if __name__ == "__main__":
    try: asyncio.run(main())
    except KeyboardInterrupt: print("\nbye", file=sys.stderr)

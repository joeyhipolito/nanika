# Python Stub — dust v1 protocol

Run: `python3 plugin.py` — binds to `$XDG_RUNTIME_DIR/nanika/plugins/python-stub.sock` (or `~/.alluka/run/plugins/python-stub.sock`).
Override socket: `python3 plugin.py --socket /tmp/test.sock`
Stop: send SIGINT (Ctrl-C) or SIGTERM; the socket is removed on exit.
Implements: ready event on connect, host_info handshake, manifest requests, heartbeat echo, shutdown drain.

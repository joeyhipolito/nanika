# Go Stub — dust v1 protocol

Build: `go build -o go-stub .`
Run: `./go-stub` — binds to `$XDG_RUNTIME_DIR/nanika/plugins/go-stub.sock` (or `~/.alluka/run/plugins/go-stub.sock`).
Stop: send SIGINT (Ctrl-C) or SIGTERM; the socket is removed on exit.
Implements: ready event on connect, host_info handshake, manifest requests, heartbeat echo, shutdown drain.

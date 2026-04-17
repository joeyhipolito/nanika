# bash-stub

Minimal dust v1 plugin stub written in bash.

## Requirements

`bash` ≥ 4, `socat`, `jq`, `xxd`

## Run

```bash
./plugin.sh
# or override socket path:
./plugin.sh --socket /tmp/bash-stub.sock
```

## Stop

Send SIGTERM or SIGINT (Ctrl-C). The socket file is removed on exit.

## Features

Implements the minimum conformance surface:
- `ready` event with manifest on connect
- `host_info` handshake (5-second timeout)
- `heartbeat` echo
- `manifest` / `refresh_manifest` request dispatch
- `shutdown` — closes the connection cleanly

Uses `socat UNIX-LISTEN … EXEC:` to listen, `dd`+`xxd` for binary framing, and `jq` for JSON.

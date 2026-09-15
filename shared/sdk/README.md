# Claude Code Go SDK

This module wraps a caller-installed `claude` CLI. It provides one-shot queries,
duplex streaming sessions, tool-result events, and usage records. No provider
credentials or running Claude process are needed for its unit tests. Live tests
require `-tags=integration`, explicit `RUN_CLAUDE_INTEGRATION=1`, and a
configured Claude CLI.

```sh
cd shared/sdk
GOWORK=off go test -race -count=1 -timeout=2m ./...
GOWORK=off go vet ./...
```

`AgentOptions` supports append-only system prompts, effort selection, explicit
tool/MCP disabling, and a custom CLI path. The environment
allowlist includes `CLAUDE_CONFIG_DIR`; use explicit environment options for
additional values.

Duplex callers can select `PermissionPreventive`, receive `PermissionRequest`
events, and answer them with `RespondPermission`. The historical zero-value mode
bypasses CLI permissions for `QueryText` and `NewQuery`. One-shot calls reject
preventive mode before spawning. `SubprocessTransport`
preserves CLI permission checks unless bypass is explicitly selected.

A duplex `Query` owns its subprocess group on Unix. Call `Close` to initiate
teardown and wait on `ProcessDone` for retirement. Windows retains root-process
teardown behavior. The CLI must support the requested flags; unit tests use
local fixture executables, not a live provider compatibility probe.

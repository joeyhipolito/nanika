# September 2026 OSS refresh

This refresh updates the reusable Claude Code Go SDK, engineering personas,
Hermes persona pack, and decomposition guidance from a committed source snapshot.
Existing public plugins and release-specific files are retained.

The SDK adds permission-request events for duplex sessions, process-group
retirement, per-message usage fields, append-system-prompt
and effort options, and explicit built-in-tool/MCP controls. A dedicated SDK
workflow runs tests and vet on Linux and macOS.

This is a bounded update. The newer Go orchestrator, Rust rewrite, private
interview-evidence package, and plugin updates are not included: they require
separate dependency and public-content review. The existing OSS Go orchestrator
remains the entrypoint; this refresh does not install or select the Rust pilot.

The repository's older all-skills CI workflow still lists retired module paths.
The new SDK workflow validates this update independently; it does not establish
a green repository-wide release gate.

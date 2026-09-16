# September 2026 OSS refresh

This refresh updates the reusable Claude Code Go SDK, engineering personas,
Hermes persona pack, and decomposition guidance from a committed source snapshot.
Existing public plugins and release-specific files are retained.

The SDK adds permission-request events for duplex sessions, process-group
retirement, per-message usage fields, append-system-prompt
and effort options, and explicit built-in-tool/MCP controls. A dedicated SDK
workflow runs tests and vet on Linux and macOS.

The initial SDK refresh was bounded: it retained the existing Go entrypoint and
did not include the Rust rewrite or wider plugin updates.

A subsequent Rust promotion now ships `skills/orchestrator-rs`, a Rust-first
source installer, paired process broker, explicit output/usage helpers, and
macOS/Linux pilot CI. Existing Go mission commands remain available through
compatibility routing. See the [Rust guide](../skills/orchestrator-rs/README.md)
for installation, provider version pins, and remaining rewrite limits. Wider
plugin refreshes and automatic provider Portal integration remain separate work.

The repository's older all-skills CI workflow still lists retired module paths.
The new SDK workflow validates this update independently; it does not establish
a green repository-wide release gate.

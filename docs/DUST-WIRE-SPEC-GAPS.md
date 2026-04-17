# Dust Wire Spec Gaps

Discovered during tracker plugin event ring integration (2026-04-13).

## GAP-01: `dust.binary` cannot pass subcommand arguments

**Spec rule:** HOTPLUG-11 — `dust.binary` is the path to the plugin binary.

**Gap:** The conformance runner and registry spawn the binary with `Command::new(binary_path)` and no arguments. Multi-command CLIs (like `tracker`) that require a subcommand (`dust-serve`) cannot be spawned directly. The only workaround is a separate binary entry point (e.g., `tracker-dust`) or a wrapper script.

**Recommendation:** Add an optional `dust.args` array field in `plugin.json` that the registry and conformance runner pass to `Command::new().args(...)`. This would allow `"binary": "tracker", "args": ["dust-serve"]` without a separate binary.

## GAP-02: `DustPlugin` trait does not cover subscribe or cancel

**Spec rule:** REPLAY-04/06, CANCEL-03/05/06

**Gap:** The `dust_sdk::DustPlugin` trait defines only `manifest()`, `render()`, and `action()`. Plugins that need `events.subscribe`, `events.unsubscribe`, or `cancel` must bypass `dust_sdk::run()` entirely and implement the full connection lifecycle manually (frame I/O, handshake, heartbeat, shutdown, request dispatch). This duplicates ~200 lines of protocol boilerplate per plugin.

**Recommendation:** Either extend `DustPlugin` with optional default methods for subscribe/cancel/refresh, or provide a `dust_sdk::run_with_events()` variant that accepts an `EventRing` and handles subscribe/cancel/live-push in the SDK loop.

## GAP-03: No `events.subscribe` section in dust-conform CLI

**Spec rule:** REPLAY-04/05/06/08

**Gap:** The `dust-conform --plugin-manifest` CLI validates four sections: `handshake`, `methods`, `heartbeat`, `shutdown`. There is no `events` or `replay` section. The replay conformance tests exist only as integration tests against the fixture binary (`dust-conformance/tests/replay.rs`), not as a section in the conformance CLI.

**Impact:** A plugin can pass all `dust-conform` sections without implementing `events.subscribe` at all. Conformance for event support is not validated by the standard tool.

**Recommendation:** Add a `replay` section to the conformance CLI that sends `events.subscribe` with `since_sequence: 0` and validates the response shape (REPLAY-04).

## GAP-04: Refresh event data contract is unspecified

**Spec rule:** ENVELOPE-07 — `Refresh` event type defined, but no data contract.

**Gap:** The spec defines `EventType::Refresh` as "host requests the plugin re-render or re-fetch" but does not specify what the plugin should do beyond re-rendering. For event-ring plugins, the natural response is to re-query the data source and emit a `data_updated` event, but this behavior is not specified. The data payload schema for the resulting `data_updated` event (e.g., should it be a full snapshot or a diff?) is left to the plugin.

**Recommendation:** Document the expected plugin behavior on `Refresh` for event-capable plugins: re-query → emit `data_updated` with full snapshot → push to active subscribers.

## GAP-05: No bootstrap path for initial state via events.subscribe

**Spec rule:** REPLAY-04 — `events.subscribe` returns retained events from the ring.

**Gap:** On a fresh plugin start with no mutations, `events.subscribe` with `since_sequence: 0` returns an empty events array. A consumer wanting the current domain state (e.g., all open issues) gets nothing. The protocol does not document a bootstrap flow — the consumer must independently know to call `render` or send a `refresh` event to seed the ring. This means `events.subscribe` alone is insufficient for initial state acquisition.

**Workaround (live-tracker.py):** Accept the empty initial state; live events arrive once mutations occur.

**Recommendation:** Either (a) document that plugins SHOULD emit a `data_updated` snapshot into the ring at startup so that `since_sequence: 0` always returns current state, or (b) add a protocol-level `snapshot` method that returns current domain data without requiring a subscription.

## GAP-06: data_updated event data schema is plugin-defined and trigger-dependent

**Spec rule:** ENVELOPE-07 — `DataUpdated` event type, no data contract.

**Gap:** The `data` field in `data_updated` events has no schema contract. For the tracker plugin alone, the shape differs by trigger: action mutations produce a single serialized issue object (`{"id": "...", "title": "..."}`), while refresh produces a wrapper (`{"source": "refresh", "issues": [...]}`). A generic consumer cannot parse events without plugin-specific knowledge. Non-Rust consumers must reverse-engineer the data shape from the plugin source.

**Impact:** Every new consumer (Python, JS, Go) must implement plugin-specific data parsing. No generic event viewer is possible.

**Recommendation:** Define a `data_updated` envelope convention: top-level `source` field (action name or "refresh"), and a `records` array of domain objects. Plugins that follow this convention can be consumed generically.

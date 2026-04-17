# Dust Protocol Plan v5.1

**Goal.** Produce a normative wire specification for a UDS-based, length-prefixed JSON, bidirectional, hot-pluggable plugin protocol, and implement it under that spec so the protocol replaces both the Wails dashboard's JS-bundle injection contract and the `<binary> query --json` CLI contract as the canonical way a nanika plugin exposes itself to any consumer. Dust's Tauri UI is **out of scope**. This is protocol-only.

## Revision history

- **v2** — first pass. Codex returned 14 blockers / 18 warnings / 12 suggestions. Review: `~/.alluka/observability-reports/dust-protocol-plan-codex-review.md`
- **v3** — restructured to spec-first ordering, added state transition tables, explicit framing edge cases, error code registry, observability backpressure, security posture, hot-plug identity rules. Codex returned: 4/14 v2 blockers fully CLOSED, 10/14 PARTIAL; 13/18 warnings CLOSED; 4 new blockers (§5 reopens §2 decisions, host_info inconsistent, false same-user mitigation claim, Phase F/G mis-gated); 7 new warnings. Review: `~/.alluka/observability-reports/dust-protocol-plan-v3-codex-review.md`
- **v4** — closed every remaining v2 partial + every new v3 finding. Three codex critiques pushed back on: (a) malformed-JSON correlation deleted outright rather than pre-parser specified, (b) Phase E "gates F/G" reworded as "conformance companion to C/D", (c) same-user peer identity explicitly accepts same-user trust as v1 posture. Codex review of v4: validated (a) and (c), flagged (b) as half-correct — the companion framing is right but the specific exit criteria in v4 created a literal E/F circular dependency. Review: `~/.alluka/observability-reports/dust-protocol-plan-v4-codex-review.md`
- **v5** — targeted fix pass on v4. Closed the 3 remaining blockers and 5 warnings: Phase E/F circularity broken by introducing a harness-internal conformance fixture; `events.subscribe` pagination semantics fixed by dropping `max` entirely; `subscription_id` removed from live event routing (kept only as unsubscribe handle); first-connection-wins canonicalized in §2.5 and removed from §5; `log_overflow` demoted to a writer-only meta log record (not an event over the wire); `ready` manifest example completed with required fields; `Capability::Scheduler` kept as reserved/advisory after codex grep caught 5 additional workspace references I missed in v4; state machine table clarified with explicit direction semantics. Codex v5 review: **executable**; implementation can start. Flagged 3 non-blocking cleanups to apply before Phase A.
- **v5.1 (this document)** — non-blocking cleanup pass on v5. Applied the four codex v5 findings: (1) removed stale `log_overflow`-as-event references from §2.3 envelope type string, §2.16 threat table, Phase D scope, and R1/R2; (2) fixed the Scheduler schema contradiction by adding `scheduler` to §2.15 allowed capabilities as reserved/advisory; (3) added byte-bounded ring buffer retention (`min(1000 events, 512 KiB)`) to §2.9 + §2.11 so single-frame replay cannot exceed the 1 MiB frame cap; (4) editorial "six resource limits" → "seven" in Phase D scope. No new review pass needed — codex already validated v5 as executable, and these are the exact cleanups it flagged.

## Changes vs. v4 (what codex v4 review asked for)

1. **Phase E/F circularity broken.** v4 said E's C/D assertions pass against the "hello reference plugin once Phase F ships" while F depended on those E assertions being green — a literal cycle. v5 introduces a **harness-internal conformance fixture** (`dust-conformance/fixtures/minimal/`) that E drives during C and D implementation. Phase F's public `hello-plugin` becomes a separate deliverable that F must itself pass against `dust-conform` as part of its own exit criteria.
2. **`events.subscribe` pagination semantics fixed.** v4 said "events newest-first trimmed to `max`" with `max: 1000` hard cap. With 500 retained events and `max: 100`, the older 400 were silently dropped without `replay_gap`. v5 **drops `max` entirely**. Subscribe returns all retained events matching `since_sequence` up to the ring-buffer bound (1000 events). Simpler and correct. R17 revised accordingly.
3. **`subscription_id` removed from live event routing.** v4 said live events were "tagged with subscription_id" but the event envelope had no such field and multiple subscriptions per connection were unimplementable. v5 drops the tagging idea: live events are identical to normal events; each active subscription receives every event after its `since_sequence`. `subscription_id` exists only as an unsubscribe handle.
4. **Multi-consumer topology canonicalized.** v4 §2.5 said first-connection-wins but §5 Q1 reopened the question and `role_hint` was referenced without a normative shape. v5 deletes Q1 from §5 (first-connection-wins is **the rule**; v1.1 may add registry-election if it bites in practice) and removes `role_hint` entirely (never defined, never needed).
5. **`log_overflow` demoted to writer meta record.** v4 had it as a `registry → subscribers` event but subscribers `accept()` the plugin socket — there's no such channel. v5 makes `log_overflow` a **writer-only meta record** in the observability log: when the writer detects queue overflow or disk-full, it writes a `{kind: "meta", type: "log_overflow", ...}` line in-band in `events.jsonl`. Consumers see it by tailing the log. Removed from the §2.13 event vocabulary.
6. **`ready` manifest example completed.** v4 §2.12 example omitted `binary` and `protocol_version` which §2.15 marks required. v5 includes them.
7. **`Capability::Scheduler` kept as reserved.** v4 scoped removal to `dust-core` + `dust-registry` + one test. Codex grep caught 5 more workspace references I missed: `plugins/dust/src-tauri/src/lib.rs:161`, `plugins/dust/dust-dashboard/src/ui.rs:231,321`, `plugins/dust/dust-dashboard/src/app.rs:266`, `plugins/dust/PROTOCOL.md:101`. Removing it would break the workspace build unless `src-tauri` and `dust-dashboard` were deleted, but the user explicitly said "ignore building Tauri, not delete it." v5 **keeps `Capability::Scheduler`** as a reserved advisory capability (which matches what the code already does — it's a fuzzy-search keyword with no dispatch semantics). Documented in §2.13. Phase C removal scope dropped.
8. **State machine table clarified.** v4's table label said "registry connection" but the `active` row mixed plugin→registry and registry→plugin frames without explicit direction. v5 adds a `Triggers` column for registry-initiated transitions (like `shutdown`) and relabels "Accepted inbound" as "Accepted from plugin" to make the perspective unambiguous.

(v4 changes vs v3, for historical context, are retained in the revision history above. The v3→v4 delta is available in git log and the v4 review report.)

---

## 1. Scope

### Out of scope
Same as v2/v3: all Tauri UI work, missions/chat/system tabs, tray affordance, replacement dashboard, `GetPluginUIBundle` dynamic injection, removal of `query --json` from non-UI consumers, Windows support.

### In scope
- **Normative wire spec** for the protocol — framing, envelope, error model, lifecycle, security, observability, schema evolution
- **Implementation** of the spec in `plugins/dust/dust-{core,sdk,registry}/`
- **Conformance harness** that asserts every spec clause, running alongside C/D as they ship
- **Reference plugin + language stubs** (Rust, Go, Python, bash) proving the protocol is genuinely language-agnostic
- **Proof-of-concept** against one real nanika plugin (`tracker`)
- **Wails cleanup** on a separate, independent track — not gated, not gating

### Protocol principles
1. **Bidirectional** — host ↔ plugin is full-duplex. Plugins push without being polled
2. **Observable by construction** — every **non-heartbeat** protocol message on every socket is auditable via an append-only log with bounded queues + redaction
3. **Hot-pluggable** — plugin binary drops in, live within seconds; binary removed, torn down gracefully; no host restart, ever
4. **Language-agnostic** — wire spec carries zero Rust assumptions; implementable in Go, Python, bash with equivalent effort
5. **Decoupled** — the registry is one reference consumer. Any process that speaks the read-only subset of the protocol is a valid subscriber. Dust is the reference implementation of the registry; other registries are not forbidden but v1 only ships dust

---

## 2. Normative spec draft

This section is the v4 wire spec in seed form. Phase A hardens this into `docs/DUST-WIRE-SPEC.md` as the single source of truth. Everything below is normative — implementation phases B/C/D must match it, and the conformance harness E asserts every clause.

### 2.1 Transport

**Socket path convention:**
```
$XDG_RUNTIME_DIR/nanika/plugins/<plugin-id>.sock   (if XDG_RUNTIME_DIR set)
~/.alluka/run/plugins/<plugin-id>.sock             (fallback)
```
**Never** `/tmp` or any shared namespace.

**Permissions:**
- Runtime directory: `0700`
- Socket file: `0600`
- Both owned by the user running the host

**Peer credential verification:**
- Linux: `SO_PEERCRED` — verify connecting uid matches host uid
- macOS: `LOCAL_PEERCRED` / `getpeereid()` — same check
- BSD: `LOCAL_PEERCRED`
- Any OS without peer credential support: host refuses to start. Documented v1 OS matrix: macOS + Linux

**Same-user trust posture (v1).** The peer credential check proves the peer has the same uid as the host. It does **not** prove the peer is the binary resolved from the manifest. v1 accepts same-user trust: any process running as the user is trusted to speak the protocol. An adversary who already has the user's uid can `ptrace` the legitimate plugin, read its memory, or replace its binary — dust cannot defend against that and does not claim to. See §2.16 threat model for the explicit scope.

**Stale socket cleanup:**
- Host on startup removes sockets whose plugin_id is not in the current manifest set
- Plugin on startup: if socket exists, try to connect; if connect succeeds, another instance is live — exit 1; if connect fails, `unlink()` and bind

**Plugin ID grammar:**
```
plugin_id := ^[a-z][a-z0-9_-]{1,63}$
```
Invalid IDs are rejected at manifest parse time.

### 2.2 Framing

**Wire format:**
```
┌─────────────────────────┬──────────────────────┐
│ 4-byte length (u32 BE)  │  UTF-8 JSON payload  │
└─────────────────────────┴──────────────────────┘
```

`length` counts the UTF-8 payload bytes, after encoding, before any transport-level escaping.

**Normative rules:**

| Case | Behavior |
|---|---|
| `length == 0` | Accepted in all states after `handshake_wait`; no effect; does **not** reset heartbeat counter |
| `length > 1 MiB` (`0x100000`) | Close connection, log `frame_oversized`, plugin marked `dead` |
| EOF in length prefix | Clean disconnect, transition to `dead` |
| EOF mid-payload | Close connection, log `frame_truncated`, `dead` |
| Payload is not valid UTF-8 | Close connection, log `frame_utf8_error`, `dead` |
| Payload is valid UTF-8 but not parseable JSON | **Close connection** — no correlation attempt, no fishing for an `id` in broken bytes |
| Payload is valid JSON but not a JSON object | Close connection |
| Payload is a JSON object with no `kind` field | Close connection |
| Payload has unknown `kind` | Close connection |
| Partial read: fewer bytes arrived than `length` requires | Buffer and retry. Per-frame read deadline: **500ms** from the first buffered byte. Exceeded → close, log `frame_read_timeout`, `dead` |
| Partial write: socket returned fewer bytes than written | Buffer and retry. Per-frame write deadline: **1s** from first byte. Exceeded → close, log `frame_write_timeout`, `dead` |
| Slowloris: peer drip-feeds bytes slower than the per-frame deadline | Detected by the read timeout above; connection closed |

**Malformed payloads always close, never correlate.** v3 tried to be clever about extracting an `id` from broken JSON to send a correlated error response. That's unimplementable without a hand-rolled pre-parser. v4 treats any malformed payload as an unrecoverable protocol error and closes the connection. The peer reconnects and tries again.

### 2.3 Envelope

Every framed payload is a JSON object with a discriminated `kind`. Five kinds exist in v1:

```jsonc
// REQUEST — either side may initiate (see §2.5 for who can send what)
{
  "kind": "request",
  "id": "req_<16hex>",
  "method": "manifest|render|action|events.subscribe|events.unsubscribe|cancel|refresh_manifest|...",
  "params": {...}                 // method-specific; may be omitted
}

// RESPONSE — correlated to a request by id
{
  "kind": "response",
  "id": "req_<16hex>",            // mirrors the request id
  "result": {...}                 // present iff successful
  // OR
  "error": { "code": -32601, "message": "method not found", "data": {...}? }
                                  // present iff failed
  // exactly one of result | error
}

// EVENT — either side pushes, no response expected
{
  "kind": "event",
  "id": "evt_<16hex>",            // unique within (connection, direction)
  "type": "ready|host_info|status_changed|progress|log|error|data_updated|refresh|visibility_changed",
  "ts": "2026-04-12T09:30:00.123Z",
  "sequence": 42,                 // plugin-originated events only; see §2.9
  "data": {...}
}

// HEARTBEAT — mutual liveness
{
  "kind": "heartbeat",
  "ts": "2026-04-12T09:30:00.123Z"
}

// SHUTDOWN — registry → plugin, graceful teardown request
{
  "kind": "shutdown",
  "reason": "host_exit|plugin_disable|version_mismatch|watcher_delete|binary_deleted|watcher_error|consumer_error|timeout"
}
```

**ID uniqueness:** per-connection per-direction. The registry and plugin each maintain independent request-ID namespaces. A duplicate request ID in the same direction receives an error response `-32600 invalid_request` and the duplicate is ignored.

**`result` and `error` are mutually exclusive** on a response. Exactly one must be present. Responses with neither or both are protocol errors and the receiver closes the connection.

**Responses may arrive out of order.** Receivers correlate by `id` only.

**Late responses:**
- After the per-request timeout: receiver drops the response, logs `late_response`. No retroactive state update
- During `draining`: accepted and forwarded if for an in-flight request; otherwise dropped
- After `dead`: dropped

### 2.4 Error code registry

Closed enum for v1:

| Code | Name | Semantics |
|---|---|---|
| `-32700` | `parse_error` | (reserved; see §2.2 — v4 closes on parse failure, does not send this) |
| `-32600` | `invalid_request` | Envelope malformed, duplicate ID, missing required field |
| `-32601` | `method_not_found` | Unknown method on a request |
| `-32602` | `invalid_params` | Params fail method-specific validation, including unknown required fields |
| `-32603` | `internal_error` | Receiver-side unexpected error; should be treated as a bug |
| `-33001` | `timeout` | Request exceeded its deadline (per-request, not heartbeat) |
| `-33002` | `canceled` | Operation was canceled via `method: cancel` |
| `-33003` | `unsupported_version` | Handshake advertised a protocol version outside the peer's supported range |
| `-33004` | `unauthorized` | Peer credential check failed, or caller role denied (see §2.5 consumer privileges) |
| `-33005` | `busy` | Backpressure: in-flight request limit or queue limit reached |
| `-33006` | `shutting_down` | Receiver is in `draining` state; new requests rejected |
| `-33007` | `replay_gap` | `events.subscribe` cursor older than ring buffer retention |
| `-33008` | `plugin_dead` | Registry-side synthetic error on requests to a plugin in `dead` state |
| `-33009` | `frame_oversized` | Reserved for logging only; normally produces connection close, not an error response |

**Plugin-specific error codes** (inside `error.data`, not as the top-level `code`) are unconstrained. Plugins that need to surface domain-specific errors do so via `error.data.plugin_code` and the registry's closed code set stays stable.

**Evolution:** see §2.17. Adding codes to this registry is a **minor** version bump. Removing or renumbering codes is **major**. Plugins never invent codes at the top-level `code` field.

### 2.5 Lifecycle state machine and consumer topology

#### 2.5.1 Registry vs. subscriber

A plugin has **at most one active registry** (typically dust) and **zero or more read-only subscribers**. The registry is the privileged consumer that:
- Spawned the plugin binary
- Owns lifecycle (shutdown, respawn, termination)
- Performs the `ready` / `host_info` handshake
- Receives heartbeats and enforces heartbeat timeouts
- Sends `shutdown`

Subscribers are other processes (an ad-hoc Python consumer, a CLI tool, an MCP wrapper) that connect to the plugin socket via a separate `accept()`. Subscribers MAY call:
- `manifest` (read the plugin's current manifest)
- `render` (read the current UI state — read-only, idempotent)
- `events.subscribe` / `events.unsubscribe` (consume the event stream)

Subscribers MAY NOT call:
- `action` (state mutation)
- `refresh_manifest`
- `shutdown` (envelope kind)
- any method with `role: "registry_only"` declared in the plugin manifest

Mutating calls from a subscriber receive `-33004 unauthorized`.

**First-connection-wins is the v1 rule.** The plugin tracks connections in accept order. The first accepted connection is the registry — it's the one that spawned the plugin binary and speaks the `ready`/`host_info` handshake. Every subsequent connection is a subscriber and may only call the read-only subset. There is no promotion path for subscribers to become registry within the same plugin lifetime. If the registry connection drops, the plugin enters `draining` (because the registry-side observes the close) and eventually `dead`; subscribers are then disconnected. If v1.1 needs a registry-election handshake (e.g. dust and an MCP wrapper racing to connect), it is additive via `events.subscribe` extensions and does not require a major bump.

**Heartbeat tracking applies to the registry connection only.** Subscribers are not heartbeated; their dropping is observed via socket close.

#### 2.5.2 State machine

Six states per plugin: `spawned` → `connected` → `handshake_wait` → `active` ⇄ `draining` → `dead`. This table models the **plugin lifecycle as observed and driven by the registry**. Subscriber connections have a simpler `connected` → `subscribing` → `closed` lifecycle (no handshake, no heartbeat tracking) and are not modeled here.

**State transition table (plugin lifecycle as seen by the registry):**

| State | Accepted from plugin | Rejected from plugin | Registry triggers | Timeout behavior | Process-exit behavior | Transitions out |
|---|---|---|---|---|---|---|
| `spawned` | (none — plugin has not bound) | — | — | 5s wait for socket file to appear → `dead`, reason `socket_never_appeared` | → `dead`, reason `plugin_exited_before_bind` | → `connected` when registry `connect()` succeeds |
| `connected` | (none — registry has not started read loop) | — | — | 5s wait for first inbound frame → `dead`, reason `handshake_timeout` | → `dead`, reason `plugin_exited_before_handshake` | → `handshake_wait` when registry starts read loop |
| `handshake_wait` | only `event` with `type: ready` | anything else → close + `dead`, reason `premature_traffic` | Registry sends `host_info` event after validating `ready` | 5s → `dead`, reason `handshake_timeout` | → `dead`, reason `plugin_exited_during_handshake` | → `active` on valid `ready` within version range; → `dead` reason `unsupported_version` on mismatch |
| `active` | `request`, `response`, `event`, `heartbeat` | duplicate-ID requests → `-32600`; unknown method → `-32601` | Registry may send `request`, `response`, `event` (e.g. `refresh`, `visibility_changed`), `heartbeat`, or `shutdown` at any time | per-request timeouts (configurable, default 30s); 3 missed heartbeats → `dead`, reason `heartbeat_timeout` — with pause override below | → `dead`, reason `process_exited` | → `draining` when registry sends `shutdown`; → `dead` on peer close |
| `draining` | `response` (for in-flight requests only), `event` (for ring replay only), `heartbeat` (silently accepted, not tracked) | new `request` → `-33006` | Registry waits for drain to complete; does not send new requests | 2s drain deadline → `dead`, reason `drain_timeout`; all in-flight requests respond with `-33002 canceled` | → `dead`, reason `process_exited_during_drain` | → `dead` when drain completes or deadline hits |
| `dead` | all → dropped | — | — | — | — | terminal; registry may respawn per `restart` policy |

**Handshake integration.** Within `handshake_wait`, the plugin sends its `ready` event; the registry validates `ready.data.protocol_version` against its supported range. If valid, the registry replies with a `host_info` event (see §2.12) and transitions the plugin to `active`. If the version is out of range, the registry sends `{kind: shutdown, reason: version_mismatch}` and waits for drain or peer close. The plugin is free to close its own end if it receives a `host_info` whose `host_version` is out of *its* supported range; this causes a `connected` → `dead` transition on the registry side via `process_exited`.

**Heartbeat pause override during `active`.** The missed-heartbeat counter for a connection pauses while any in-flight operation identified by an `op_id` is actively progressing — defined as receiving at least one `progress` event or the final `response` within the last `heartbeat_interval_ms`. This is keyed by `op_id`, not by request ID, because a single request may spawn multiple `progress` events and the final response is singular. Implementations track `last_progress_ts_per_op` and compare against the heartbeat clock.

**Host dies mid-handshake.** Plugin observes peer close during `handshake_wait`; it writes any queued events to disk if persistent, cleans up its socket, and exits. Registry on restart observes the stale socket, detects no live peer (connect fails), unlinks, and respawns from the manifest.

**`ready` arriving after shutdown timer started.** Dropped, logged, no state change. This can happen if a slow plugin finally handshakes after the 5s handshake timeout expired.

### 2.6 Heartbeat rules

- Both sides emit one `heartbeat` per `heartbeat_interval_ms` (default `10000`, negotiable via manifest)
- Miss threshold is `3` by default
- **Heartbeats pause during `draining`** on both sides
- **Heartbeats pause for long-running operations keyed by `op_id`**: see §2.5 active-state override
- Heartbeat envelope carries only `kind` and `ts` — no `sequence`, no `id`, and is **not logged** to the observability stream (rate too high for durable log)

### 2.7 Shutdown semantics

- Only the registry initiates shutdown. A plugin that wants to shut itself down exits its process instead
- `shutdown` envelope carries a `reason` from the enum in §2.3:
  - `host_exit` — registry is exiting
  - `plugin_disable` — user disabled the plugin
  - `version_mismatch` — protocol version out of supported range
  - `watcher_delete` — `plugin.json` removed or renamed
  - `binary_deleted` — plugin binary deleted from disk
  - `watcher_error` — fsnotify watcher failed, forcing a teardown
  - `consumer_error` — the plugin crashed a subscriber's socket and the registry wants a fresh instance
  - `timeout` — some registry-side operation (e.g. handshake) timed out
- Plugin enters `draining` on receipt
- Plugin has `shutdown_drain_ms` (default `2000`) to:
  - Stop accepting new requests (respond `-33006 shutting_down`)
  - Flush in-flight responses, each cancelable with `-33002 canceled` if the plugin chooses
  - Flush queued events
- After the drain deadline, registry `SIGKILL`s the plugin process and transitions to `dead`
- In-flight requests on the registry side get synthesized `-33002 canceled` responses for any that did not receive a response before `dead`

### 2.8 Version negotiation

- Handshake carries a single semver string: `ready.data.protocol_version = "1.0.0"`
- Registry config declares a supported range: `{min: "1.0.0", max: "1.999.999"}`
- Registry replies with a `host_info` event carrying its own `protocol_version_supported` range
- If the plugin's version is outside the registry's range: registry sends `{kind: shutdown, reason: version_mismatch}`, drains, `dead`
- If the registry's range is outside the plugin's supported range: plugin closes its end of the connection after receiving `host_info`; registry observes peer close and transitions `dead`

**Version is advertised via events, not a connect preamble.** v3 mentioned "connect preamble" in one place and "event" in another; v4 uses `ready` (plugin→registry) and `host_info` (registry→plugin) events, in that order, within the `handshake_wait` state.

**Semver bump rules:**
- **Patch** — bug fixes; wire-format unchanged; no new methods; no new event types; no new error codes
- **Minor** — additive only: new optional envelope fields, new event types, new methods, new capability names, **new error codes in the registry**
- **Major** — breaking: field removals, required-field additions, method signature changes, state machine changes, error code removals or renumbering

**Unknown field policy:**
- Unknown optional fields in envelopes and `params`: silently ignored
- Unknown required fields in `params`: `-32602 invalid_params`
- Unknown event `type`: logged as `event_type_unknown`, dropped
- Unknown method: `-32601 method_not_found`
- Unknown error code in response: treated as opaque error, logged

### 2.9 Reconnect, replay, and sequence ownership

**There is no connection-level reconnect.** A dropped connection enters `dead` (registry) or closed (subscriber). A new connection from the same plugin is a fresh lifecycle. A subscriber that wants persistent event consumption must reconnect and re-subscribe from its last-known sequence.

**Event replay is an explicit RPC:**

```jsonc
// Subscriber → plugin
{ "kind": "request", "id": "req_<16hex>", "method": "events.subscribe",
  "params": {
    "since_sequence": 41   // inclusive: receive events with sequence >= 41
  }
}

// Plugin → subscriber (case 1: cursor in range)
{ "kind": "response", "id": "req_<16hex>",
  "result": {
    "subscription_id": "sub_<16hex>",   // handle for unsubscribe; NOT used for event routing
    "events": [<event>, ...],            // every retained event with sequence >= since_sequence,
                                          //   in ascending sequence order, bounded by ring size (1000)
    "next_sequence": 142                 // the sequence that the next live event will carry
  }
}

// Plugin → subscriber (case 2: cursor too old)
{ "kind": "response", "id": "req_<16hex>",
  "error": { "code": -33007, "message": "replay_gap",
             "data": { "oldest_available": 100, "requested": 41 } }
}
```

After a successful subscribe, the plugin **begins pushing new events** on this connection as they are emitted. Pushed events use the standard event envelope (§2.3) with no additional routing fields — they are regular `event` kinds on the connection, identical to any event the plugin emits. The subscriber correlates them by `sequence`, not by `subscription_id`.

```jsonc
// Unsubscribe ends the live push for this subscription
{ "kind": "request", "id": "req_<16hex>", "method": "events.unsubscribe",
  "params": { "subscription_id": "sub_<16hex>" }
}

// Plugin → subscriber
{ "kind": "response", "id": "req_<16hex>",
  "result": { "ok": true } }
```

**`since_sequence` semantics:**
- Inclusive: `since_sequence: 41` returns events with `sequence >= 41`
- `since_sequence: 0` returns all retained events (ring size ≤ 1000)
- `since_sequence > latest`: returns empty `events` array with `next_sequence` = `latest + 1`. Live push delivers new events from that point
- `since_sequence < oldest_available`: returns `-33007 replay_gap` with `oldest_available` in `data`

**No `max` / no pagination.** The response always returns every retained event matching `since_sequence` in a single call, bounded by the ring buffer size (1000 events). A subscriber that wants a smaller window filters client-side. Pagination via `since_sequence` iteration is available but only if the subscriber tears down and re-subscribes — v1 does not support multi-call pagination on a single subscription, and in the common case the 1000-event bound makes it unnecessary.

**`subscription_id` is an unsubscribe handle, not a routing key.** A connection may have at most one active subscription on a plugin (plugins reject a second `events.subscribe` on the same connection with `-33005 busy`). `subscription_id` exists so that if the subscriber disconnects and reconnects, it has a handle to clean up — though in practice, disconnect terminates the subscription and there's nothing to clean up. The field is there for forward compatibility with multi-subscription v1.1 (which would need its own routing semantics and is out of scope for v1).

**Snapshot vs. live.** `events.subscribe` is a hybrid: it returns a snapshot (all retained events from `since_sequence`) AND establishes a live push stream from `next_sequence` forward. There is no snapshot-only or live-only mode in v1.

**Duplicate event detection:** the subscriber is responsible for deduplication via `sequence`. Sequences are monotonic per plugin process; a subscriber that sees `sequence` decrease MUST interpret it as a plugin restart and discard its cursor.

**Sequence ownership:**
- **Plugin-originated events** get a plugin-assigned monotonic `u64`, starting at `1` at process start. Does not persist across process restart
- **Registry-originated events** (`host_info`, `refresh`, `visibility_changed`) carry no `sequence` field. They are not replayable. If a subscriber wants to know the current visibility state, it calls a dedicated `manifest`-like request, not subscribe
- **Subscriber-originated** events: N/A. Subscribers don't originate events, only consume them

**Ring buffer (byte-bounded):**
- In-memory ring of plugin-originated events per plugin
- Retention is `min(1000 events, 512 KiB total serialized size)` — whichever bound is hit first. A new event that would push the total serialized size over 512 KiB evicts the oldest events until the new event fits
- This guarantees a full `events.subscribe` response fits within the 1 MiB frame cap (§2.2) with ~500 KiB headroom for envelope overhead, regardless of individual event sizes
- `events.subscribe` walks the ring and returns every retained event matching `since_sequence` in a single response (§2.9 wire shape)
- Events older than the oldest retained event return `-33007 replay_gap`
- The ring is scoped to the plugin process lifetime. On plugin restart, the ring starts empty and sequence starts at 1. Subscribers detect this and re-subscribe from `since_sequence: 0`

### 2.10 Observability & redaction

**Log location:** `~/.alluka/dust/events.jsonl`

**Writer architecture (non-blocking, bounded):**
```
      protocol dispatch (hot path)
                │
                ▼ (non-blocking send; drop-oldest on overflow)
      ┌────────────────────┐
      │ bounded channel    │  cap 10_000 per plugin
      └────────────────────┘
                │
                ▼
      ┌────────────────────┐
      │ background writer  │  batches every 100ms or 4 KiB, fsync per batch
      └────────────────────┘
                │
                ▼
      ~/.alluka/dust/events.jsonl
```

**Protocol path never blocks on disk.** If the bounded channel is full, the protocol path drops the oldest queued item (not the new one) and increments the plugin's `events_dropped` counter. Dropped items are not themselves logged through the normal channel (recursion hazard). Instead, the writer emits a **meta record** directly into `events.jsonl` at most once per 60s per plugin when `events_dropped` is non-zero:

```jsonc
// Meta record written directly by the writer, not a protocol event
{ "ts": "2026-04-12T09:30:00.123Z",
  "plugin_id": "tracker",
  "kind": "meta",                                  // discriminator: NOT "in"/"out"
  "type": "log_overflow",
  "data": {
    "events_dropped_since_last_notice": 4217,
    "reason": "queue_full" | "disk_full" | "writer_error"
  } }
```

Meta records are written directly by the writer goroutine and are not framed protocol events — they never appear on any socket. Consumers observing `log_overflow` must tail the log file. The `kind: "meta"` discriminator lets log readers filter meta records from in-band protocol messages.

**Log schema per line:**
```json
{ "ts": "2026-04-12T09:30:00.123Z",
  "plugin_id": "tracker",
  "direction": "in",
  "sequence": 42,
  "message": <the framed envelope, after redaction> }
```

**Rotation:** 100 MB or 7 days, whichever first. Rotated file renamed to `events.jsonl.<YYYYMMDD-HHMMSS>`. Retention is a **bounded audit history**, not "everything the plugin ever said."

**Heartbeats are not logged.** `kind: heartbeat` is filtered out at the writer's input. Everything else (request, response, event, shutdown) is logged.

**Redaction:** each plugin manifest declares a `log_redact` array of simplified-JSONPath strings. Before writing to the log, the writer strips matching paths from the envelope.

```json
{
  "dust": {
    "binary": "bin/tracker-dust",
    "log_redact": [
      "$.params.auth_token",
      "$.params.content",
      "$.data.secret",
      "$.data.events[*].content"
    ]
  }
}
```

**Simplified JSONPath grammar (v1):**
```
path    := '$' segment+
segment := '.' identifier                 // child by name
         | '[*]'                          // all elements of an array
identifier := [a-zA-Z_][a-zA-Z0-9_]*
```

No filters, no slices, no recursion. Invalid paths are logged at manifest parse time with `invalid_redact_path` and skipped (the plugin still loads).

**Default redaction policy:** nothing is redacted by default. Plugins opt in to protection via `log_redact`. The spec does not maintain a list of "sensitive field names" because every plugin has different data shapes. See R13 for the residual risk.

**Per-message opt-out:** plugins that want to mark a single message as "do not log" set `$.meta.log_redact_all: true` in the envelope. The writer honors this and writes a placeholder record with only `{ts, plugin_id, direction, sequence, redacted: true}`.

**Log permissions:** file `0600`, directory `0700`. Same ownership as the runtime directory.

### 2.11 Backpressure

| Resource | Limit | Overflow behavior |
|---|---|---|
| In-flight requests per connection per direction | 100 | 101st → error `-33005 busy` |
| Queued events in ring (per plugin) | `min(1000 events, 512 KiB total)` | Oldest evicted (whichever bound hits first); late subscribers get `-33007 replay_gap` |
| Log writer bounded channel (per plugin) | 10000 | Drop oldest; increment `events_dropped`; writer emits `log_overflow` meta record (§2.10) at most once per 60s |
| Concurrent registry connections per plugin | 1 | Second registry connection → close with `-33004 unauthorized` |
| Concurrent subscriber connections per plugin | 16 | 17th → close with `-33005 busy` |
| Active subscriptions per connection | 1 | Second `events.subscribe` on the same connection → `-33005 busy` |
| Disk full on log write | — | Writer enters degraded mode; drops batches; writes `log_overflow` meta record at most once per 60s; protocol path unaffected |

**Slow consumer policy:** subscribers that don't drain fast enough have their push queue evicted from the ring. They detect the gap via `-33007 replay_gap` on the next subscribe.

### 2.12 Handshake shape (`ready` and `host_info`)

The handshake is bidirectional and uses events in the `handshake_wait` state.

**Plugin → registry (`ready`):**

```jsonc
{
  "kind": "event",
  "id": "evt_<16hex>",
  "type": "ready",
  "ts": "2026-04-12T09:30:00.123Z",
  "sequence": 1,
  "data": {
    "manifest": {
      "name": "tracker",
      "version": "0.1.0",
      "dust": {
        "binary": "bin/tracker-dust",
        "protocol_version": "1.0.0",
        "capabilities": ["widget", "command"],
        "restart": "on_failure",
        "heartbeat_interval_ms": 10000,
        "shutdown_drain_ms": 2000,
        "spawn_timeout_ms": 5000,
        "log_redact": []
      }
    },
    "protocol_version": "1.0.0",
    "plugin_info": {
      "pid": 12345,
      "started_at": "2026-04-12T09:29:59.000Z"
    }
  }
}
```

The `manifest.dust` object in the `ready` event is the plugin's authoritative copy of what the registry parsed from `plugin.json`. It must include every required field from §2.15 (`binary`, `protocol_version`). The registry validates that the declared shape matches its own parsed copy and rejects mismatches as `-32602 invalid_params` on the handshake.

**Registry → plugin (`host_info`):**

```jsonc
{
  "kind": "event",
  "id": "evt_<16hex>",
  "type": "host_info",
  "ts": "2026-04-12T09:30:00.125Z",
  "data": {
    "host_name": "dust",
    "host_version": "0.1.0",
    "protocol_version_supported": { "min": "1.0.0", "max": "1.999.999" },
    "consumer_count": 1
  }
}
```

**`ts` is required on every event envelope, including `host_info`.** v3's example omitted it; v4 fixes that.

`host_info` is a registry-originated event. It carries no `sequence` field (see §2.9 — registry events are unsequenced).

After the registry sends `host_info`, the plugin transitions to `active` on the registry side and responds to subsequent requests normally.

### 2.13 Event vocabulary v1 (normative payloads)

| Type | Direction | Payload | Sequenced? | Logged? |
|---|---|---|---|---|
| `ready` | plugin → registry | `{manifest, protocol_version, plugin_info}` | yes (seq = 1) | yes |
| `host_info` | registry → plugin | `{host_name, host_version, protocol_version_supported, consumer_count}` | no | yes |
| `status_changed` | plugin → registry | `{status: "ok"\|"degraded"\|"error", detail?: string}` | yes | yes |
| `progress` | plugin → registry | `{op_id: string, percent: 0-100, message?: string}` | yes | yes (subject to redaction) |
| `log` | plugin → registry | `{level: "debug"\|"info"\|"warn"\|"error", message: string, fields?: {...}}` | yes | yes (subject to redaction) |
| `error` | plugin → registry | `{plugin_code: string\|int, message: string, fatal: bool, data?: {...}}` | yes | yes |
| `data_updated` | plugin → registry | `{resource: string, version: string\|int}` | yes | yes |
| `refresh` | registry → plugin | `{reason?: string}` | no | yes |
| `visibility_changed` | registry → plugin | `{visible: bool}` | no | yes |

**`log_overflow` is not in this table.** It is a writer meta record (§2.10) written directly into `events.jsonl` by the observability writer when the bounded channel overflows or disk writes fail. It is not a wire event, not pushed over any socket, not subject to redaction. Consumers observe it by tailing the log file.

**`Capability::Scheduler` is reserved in v1.** Plugins MAY declare `"scheduler"` in their `dust.capabilities` array. The registry treats it as an advisory tag (it affects fuzzy-search ranking in the palette at `dust-registry/src/lib.rs:514`) but there is no protocol-level dispatch for scheduled jobs. Plugins that need scheduled execution must continue to use the nanika scheduler CLI (`scheduler jobs add ...`) for now. A dispatch mechanism for `scheduler`-capability plugins is reserved for v1.1+ if a real use case surfaces.

**`error` events carry plugin-specific codes.** Unlike response errors (which use the closed `-327xx`/`-330xx` registry in §2.4), `error` events surface plugin-internal errors to the registry. Their `plugin_code` field is free-form; the registry surfaces it to consumers as an opaque identifier.

**Unknown event types** are logged with level `warn` and dropped. They never cause state transitions.

**`cancel` is not an event.** v3 had a vocabulary row for `cancel` and §2.9 called it host-originated; both were wrong. v4 treats `cancel` as a request method only (see §2.14).

### 2.14 Cancellation contract

`cancel` is a **request method**, not an event. This matters because cancel wants an acknowledgment.

```jsonc
// Registry → plugin
{ "kind": "request", "id": "req_<16hex>", "method": "cancel",
  "params": { "op_id": "<op_id from the original action params>" } }

// Plugin → registry (immediate ack)
{ "kind": "response", "id": "req_<16hex>",
  "result": { "ok": true, "already_complete": false } }
```

**Semantics:**
- Cancel is best-effort. The plugin may ignore it (past the point of no return) but must still respond
- If the operation had already completed before cancel arrived, response is `{ok: true, already_complete: true}`
- The canceled operation's eventual response carries `error: {code: -33002, message: "canceled"}` if cancel took effect
- `op_id` is an opaque string the caller chose when dispatching the original request, typically via `params.op_id`

### 2.15 Hot-plug identity, watcher, and manifest schema

**Watch target:** `~/nanika/plugins/*/plugin.json`

**Watcher rules:**
- Filesystem event: debounce 200ms, dedup by canonical path (resolves symlinks)
- **Atomic rename only** — plugins must write `plugin.json.tmp` and rename. In-place truncate+write is observed partway through and logged as `manifest_parse_failure`; retried on the next event
- Plugin ID must match `plugin.json.name` and the grammar in §2.1; invalid IDs are rejected at parse time
- `plugin.name` change inside the same file: graceful teardown of the old identity, spawn of the new
- Symlinks resolved to canonical path
- Manifest parse failures are retried on the next event; plugin not spawned until a complete parse succeeds

**Binary path validation:**
- Resolved relative to the plugin directory
- Polled every 5s while the plugin is `active` or `draining` for existence + executable bit
- Binary deletion mid-operation → registry sends `{kind: shutdown, reason: binary_deleted}`

**Plugin instance collision:**
- Registry refuses to spawn a second instance of a plugin whose ID already has an `active` handle
- Stale-socket detection: on spawn, if the socket exists, try to connect; if connect succeeds, another instance is live — spawn aborted; if connect fails, `unlink()` stale socket and proceed

**`dust` manifest block schema:**

```json
{
  "dust": {
    "binary": "bin/tracker-dust",                 // REQUIRED: path relative to plugin dir
    "protocol_version": "1.0.0",                   // REQUIRED: semver
    "capabilities": ["widget", "command"],         // OPTIONAL: default []
    "restart": "on_failure",                       // OPTIONAL: "never" | "on_failure" | "always", default "on_failure"
    "heartbeat_interval_ms": 10000,                // OPTIONAL: default 10000
    "shutdown_drain_ms": 2000,                     // OPTIONAL: default 2000
    "spawn_timeout_ms": 5000,                      // OPTIONAL: default 5000, overrides §2.5 spawn timeout
    "log_redact": []                               // OPTIONAL: default []
  }
}
```

**Field defaults:**
- `capabilities`: empty list
- `restart`: `"on_failure"` — respawn up to 3 times on non-clean exit; backoff 1s → 2s → 4s → 8s (capped)
- `heartbeat_interval_ms`: 10000
- `shutdown_drain_ms`: 2000
- `spawn_timeout_ms`: 5000
- `log_redact`: `[]`

**Field validation:**
- `binary`: must resolve to an executable within the plugin directory (no `..`, no absolute paths outside the plugin dir)
- `protocol_version`: valid semver string
- `capabilities`: subset of the capability set recognized by the registry (v1: `widget`, `command`, `scheduler`). `scheduler` is **reserved/advisory** in v1 — declarable but with no protocol-level dispatch semantics (see §2.13)
- `heartbeat_interval_ms`: ≥ 1000, ≤ 300000
- `shutdown_drain_ms`: ≥ 100, ≤ 60000
- `spawn_timeout_ms`: ≥ 1000, ≤ 60000

Invalid manifests are logged at parse time with `manifest_validation_failed` and the plugin is not spawned.

**`refresh_manifest` method** — normative:

```jsonc
// Registry → plugin
{ "kind": "request", "id": "req_<16hex>", "method": "refresh_manifest" }

// Plugin → registry
{ "kind": "response", "id": "req_<16hex>",
  "result": {
    "manifest": { ... }                   // updated manifest, same shape as ready.data.manifest
  }
}
```

The registry calls `refresh_manifest` when `plugin.json` is modified in place (not replaced via atomic rename). The plugin re-reads its own manifest and returns it. Registry validates, swaps its cached copy, and proceeds. In-flight requests complete against the old manifest; new requests use the new manifest.

### 2.16 Threat model

v1 scope: **defend against other local users, malformed or buggy peer processes, and stale-socket spoofing.** Out of scope: malicious code running under the user's own uid.

| Threat | Mitigation |
|---|---|
| Other local user tries to read/write plugin socket | Runtime directory `0700` owned by user; socket `0600`; peer credential check rejects foreign uids |
| Malicious peer sends oversized frame to DoS registry | 1 MiB payload cap (§2.2); oversized → close + `dead` |
| Malicious peer floods events to DoS observability writer | Bounded channel + drop-oldest (§2.10); `log_overflow` meta record surfaces via the observability log |
| Malformed or buggy peer sends invalid JSON mid-stream | Connection closes on parse failure (§2.2); no state corruption; peer may reconnect |
| Stale socket from a crashed plugin gets reused by a different binary | Canonical path resolution, stale-socket cleanup on startup, collision detection at spawn |
| Slowloris — peer drip-feeds bytes to starve the read loop | Per-frame read deadline (500ms), per-frame write deadline (1s) (§2.2) |
| Log exposure via cloud backup / home sync | Log mode `0600`, directory `0700`, documented recommendation to exclude `~/.alluka/dust/` from home-sync |
| Compromised plugin leaks user data into the observability log | `log_redact` policy (§2.10); per-message opt-out; residual risk documented as R13 |
| Subscriber calls mutating methods | Role enforcement in §2.5 — mutating methods from a subscriber return `-33004 unauthorized` |
| Second registry connection attempts to take over an active plugin | First-connection-wins rule (§2.5); second registry connection closes with `-33004` |

**Explicitly out of scope:**
- Malicious code running as the same uid. Such an adversary already has sufficient privilege to `ptrace` the legitimate plugin, read its memory, replace its binary, or bind the plugin's socket directly. Adding nonce handshakes or binary-hash verification provides theater, not security. If you are defending against arbitrary same-user code execution, you need OS-level sandboxing (AppArmor, SELinux, sandbox_exec), not a user-space protocol
- Network adversaries. UDS is local only. Cross-machine remoting is v2+
- Supply-chain attacks against the plugin binary itself. v2+ may add manifest signing

### 2.17 Schema evolution policy

| Change | Semver | Handling |
|---|---|---|
| Bug fix, no wire change | patch | — |
| Add optional envelope field | minor | Older peers ignore |
| Add event type | minor | Older peers log + drop |
| Add method | minor | Older peers respond `-32601 method_not_found` |
| Add capability name | minor | Older peers ignore capability |
| **Add error code to the registry** | **minor** | Older peers treat as opaque error (consistent with §2.4) |
| Add required envelope field | major | Breaks older peers |
| Remove or rename method/type/field | major | — |
| Remove or renumber error code | major | — |
| Change state machine | major | — |

**v3 contradiction fixed.** v3 said adding an error code was major in §2.8 but minor in §2.17. v4 canonicalizes as minor — adding codes is additive; older peers treat unknown codes as opaque.

**Unknown-field tolerance** is the core of schema evolution — producers add fields freely, consumers ignore unknowns; breakage surfaces only on required-field additions, method signature changes, and state machine changes.

---

## 3. Phase plan

### 3.1 Dependency DAG

```
   (separate, parallel) Wails cleanup ─── can run any time, not gated
   
   Phase A (normative spec draft) ──┐
                                    │
                                    ├── Phase B (envelope + framing + error impl)
                                    │            │
                                    │            ├── Phase C (lifecycle + hot-plug)
                                    │            │       │
                                    │            ├── Phase D (streaming + observability + backpressure)
                                    │            │       │
                                    │            └── Phase E (conformance harness, runs alongside C/D from day one)
                                    │                    │
                                    │                    └── (assertions grow as C and D ship)
                                    │                    
                                    │                    C+D exit criteria passed + E assertions green for C+D scope
                                    │                                 │
                                    │                                 ▼
                                    │                     Phase F (reference plugin + Go/Python/bash stubs)
                                    │                                 │
                                    │                                 ▼
                                    │                     Phase G (tracker POC)
                                    │                                 │
                                    │                                 ▼
                                    │                     Phase H (final spec freeze, v1.0.0)
                                    │                                 │
                                    └────── spec updates feed back throughout; final text locked in H ────────┘
```

Notes on dependency shape:
- **B gates everything.** Without the envelope/framing/error-model in dust-core, nothing C/D/E/F/G does can compile
- **C and D run in parallel** after B. They touch different modules (C: `dust-registry/watcher.rs` + `dust-core/state.rs`; D: `dust-core/events.rs` + `dust-registry/observability.rs`) and their implementations are independent
- **E runs alongside C and D from day one.** E's scope is "every clause in §2 of the spec becomes a conformance assertion". The harness is built incrementally — B clauses first, C clauses as C ships, D clauses as D ships. E is never "done" until H; its exit state is "all §2 clauses covered and green against the reference plugin"
- **F gates on completed C + completed D + E assertions green for C and D scope.** Without C, F has no hot-plug identity or lifecycle to test against. Without D, F has no streaming or redaction to validate. F's exit criteria explicitly require both
- **G gates on F.** G is the real-plugin test; it needs the reference plugin + language stubs as prior art and conformance baseline
- **H gates on G.** Final spec freeze incorporates design gaps surfaced by G against real data

### 3.2 Phase A — Normative spec draft

**Depends on:** —

**Touches:** `docs/DUST-WIRE-SPEC.md` (new)

**Scope:** Take §2 of this plan and harden it into a standalone normative document. All sections present; no Rust types; every message shape is JSON with example bytes; every state transition is a table row.

**Exit criteria:**
- `docs/DUST-WIRE-SPEC.md` exists, every §2 section present
- No internal contradictions (editorial pass cross-references socket path, manifest key names, error codes, state names, event vocabulary across all sections)
- All open questions from v3 §5 are resolved in the spec body; §5 of this plan holds only genuinely-still-open items
- One wire trace example per message kind (request, response, event, heartbeat, shutdown) as JSON with example byte counts
- Every "MUST" clause corresponds to a named test case in the Phase E conformance harness coverage matrix

### 3.3 Phase B — Envelope + framing + error implementation

**Depends on:** Phase A

**Touches:** `plugins/dust/dust-core/src/envelope.rs` (new), `plugins/dust/dust-core/src/error.rs` (new), `plugins/dust/dust-core/src/framing.rs` (new), `plugins/dust/dust-core/src/lib.rs` (glue), `plugins/dust/dust-registry/src/ipc.rs` (delete — G1)

**Closes gaps:** G1, G2, G4, G6, G12

**Scope:**
- Delete `plugins/dust/dust-registry/src/ipc.rs`
- Implement framing per §2.2: 1 MiB cap, 500ms per-frame read deadline, 1s per-frame write deadline, slowloris defense, every edge case in the framing table
- Implement envelope types per §2.3 with ID uniqueness per connection per direction, `result`/`error` exclusivity, late-response handling
- Implement error registry per §2.4 as a closed enum
- Implement typed `action` params `{op_id?, item_id?, args?: map<string, value>}` (G4)
- No correlation attempts on malformed payloads — connection closes (G6 resolution)

**Exit criteria:**
- `cargo test -p dust-core` green with a named test per row of the §2.2 framing table and per row of the §2.4 error table
- `plugins/dust/dust-registry/src/ipc.rs` does not exist
- Phase E harness can drive envelope + framing assertions end-to-end before C and D exist

### 3.4 Phase C — Lifecycle + hot-plug

**Depends on:** Phase B

**Touches:** `plugins/dust/dust-registry/src/lib.rs`, `plugins/dust/dust-registry/src/watcher.rs` (new or rewritten), `plugins/dust/dust-core/src/state.rs` (new), `plugins/plugin.schema.json`

**Closes gaps:** G3, G5, G8, G9, G11 partial (push direction — completed by D). **G10 deferred to v1.1+** — `Capability::Scheduler` is kept as a reserved advisory capability in v1 (see §2.13). Removal would require deleting `dust-dashboard` and `src-tauri` from the Cargo workspace (references at `plugins/dust/src-tauri/src/lib.rs:161`, `dust-dashboard/src/ui.rs:231,321`, `dust-dashboard/src/app.rs:266`), which contradicts the "do not touch Tauri" constraint for this plan.

**Scope:**
- State machine per §2.5 table (registry connection lifecycle)
- Subscriber connection lifecycle per §2.5.1 (connected → subscribing → closed, no heartbeat tracking)
- Hot-plug watcher per §2.15: `~/nanika/plugins/*/plugin.json`, debounce 200ms, dedup by canonical path, atomic-rename-only, plugin ID validation, binary path poll, collision detection, stale-socket cleanup
- Peer credential verification per §2.1 (Linux + macOS)
- Runtime directory creation at `$XDG_RUNTIME_DIR/nanika/plugins/` or `~/.alluka/run/plugins/`, `0700`
- `ready`/`host_info` handshake per §2.12, including `ts` on `host_info`
- Heartbeat per §2.6 with the op_id-keyed pause override in §2.5
- Graceful shutdown per §2.7 with expanded reason enum
- Extend `plugins/plugin.schema.json` with the `dust:` manifest block per §2.15
- `Capability::Scheduler` **stays** in `plugins/dust/dust-core/src/lib.rs:199-213` and its match arm in `plugins/dust/dust-registry/src/lib.rs:506-516`. Documented in §2.13 as reserved/advisory. No code changes in Phase C for this capability
- `refresh_manifest` request method per §2.15 (G3)
- `Widget.refresh_secs` host-side timer (G5) — registry emits `refresh` event to plugin at the declared interval

**Exit criteria:**
- Phase E harness passes every state-transition test case from §2.5
- A `plugin.json` dropped into `~/nanika/plugins/` reaches `active` within 2s
- A plugin whose binary is deleted transitions to `dead` within 3s
- A plugin missing heartbeats is declared `dead` at `heartbeat_interval_ms × 3` against the **negotiated** interval
- A plugin sending `ready` after shutdown has started logs and drops the event with no state change
- Peer credential verification rejects foreign-uid connections on Linux and macOS
- Stale socket cleanup on startup verified
- `Capability::Scheduler` is documented in `docs/DUST-WIRE-SPEC.md` as a reserved/advisory capability and its existing code paths (fuzzy-search keyword) are preserved unchanged

### 3.5 Phase D — Streaming + observability + backpressure

**Depends on:** Phase B (parallel with Phase C)

**Touches:** `plugins/dust/dust-core/src/events.rs` (new), `plugins/dust/dust-registry/src/observability.rs` (new), `plugins/dust/dust-registry/src/backpressure.rs` (new), `plugins/dust/dust-sdk/src/lib.rs` (event emit + subscribe methods)

**Closes gaps:** G11 (push direction)

**Scope:**
- Event envelope per §2.3 and §2.13
- Per-plugin in-memory event ring (1000) with `events.subscribe` / `events.unsubscribe` handlers per §2.9
- Sequence assignment per §2.9 (plugin-originated events only)
- Observability writer per §2.10: bounded channel, background batch writer, fsync every 100ms or 4 KiB, rotation at 100 MB / 7 days, heartbeat filtering
- Redaction pipeline per §2.10 with JSONPath subset grammar + invalid-path handling
- Per-message opt-out via `$.meta.log_redact_all`
- Backpressure per §2.11 — all seven resource limits
- Disk-full degraded mode with `log_overflow` meta record emission
- `dust logs --plugin <id> [--follow]` CLI

**Exit criteria:**
- A plugin emitting `progress` shows up in `~/.alluka/dust/events.jsonl` within 100ms under idle load
- Redaction test: `log_redact: ["$.params.auth_token"]` + a request carrying `params.auth_token = "secret"` → log shows `"<redacted>"`
- Backpressure test: 20000 events in a tight loop does not block the protocol path; `events_dropped` increments; plugin stays `active`
- Disk-full simulation: writer enters degraded mode, writes `log_overflow` meta records at most once per 60s, protocol path unaffected
- Replay test: subscribe at `since_sequence: 500` returns events 500..latest; subscribe at `since_sequence: 0` when ring oldest is 500 returns `-33007 replay_gap`; subscribe at `since_sequence > latest` returns empty with live push
- Plugin restart test: consumer's subscribe at old sequence returns `-33007 replay_gap` with `oldest_available: 1`
- Heartbeat test: heartbeats NOT appended to the observability log

### 3.6 Phase E — Conformance harness (companion to C and D)

**Depends on:** Phase B

**Runs alongside:** Phase C and Phase D from day one. Phase E is a **companion process**, not a gate that blocks C/D. Its assertions grow incrementally as C and D ship.

**Touches:** `plugins/dust/dust-conformance/` (new Rust crate), `plugins/dust/dust-conformance/fixtures/minimal/` (new — the harness-internal conformance fixture)

**Scope:**
- `dust-conform` binary: connects to a plugin socket (or spawns a plugin via manifest), drives assertions derived directly from §2 of the spec
- **Harness-internal conformance fixture**: a minimal plugin shipped inside the `dust-conformance` crate that implements exactly the v1 protocol surface — no domain logic, no UI. This fixture is what E drives during C and D implementation, **not** Phase F's public hello plugin. The fixture exists so that E has something to assert against before F ships; it is private to the harness and not part of the public reference plugin set
- Assertion set built incrementally: B clauses first (framing, envelope, error), then C clauses (lifecycle, hot-plug) as C ships, then D clauses (events, observability, backpressure, redaction) as D ships
- Every "MUST" clause in §2 → one or more assertions
- Table-driven: `(state, input frame, expected response, expected state, expected side effect)`
- Every error code in §2.4 produced in its named situation
- Every state transition in §2.5, including edge cases (host dies mid-handshake, ready after shutdown, heartbeat during long request)
- Every backpressure limit in §2.11
- Redaction behavior with valid and invalid JSONPath paths
- Hot-plug identity (watcher tests require a scratch `~/nanika/plugins/` mount via env override)
- CLI flags: `--plugin-manifest <path>`, `--section <A-H>`, `--json`, `--verbose`
- CI integration: runs against every plugin with a `dust:` block

**Exit criteria (no hard "complete" until H):**
- After B ships: harness covers §2.2 + §2.3 + §2.4 clauses, passes against the harness-internal fixture
- After C ships: harness adds §2.5 + §2.6 + §2.7 + §2.8 + §2.12 + §2.15, passes against the harness-internal fixture
- After D ships: harness adds §2.9 + §2.10 + §2.11 + §2.13 + §2.14, passes against the harness-internal fixture
- Phase F's hello plugin, when it ships, is independently driven by the harness and must pass everything — but this is F's exit criterion, not E's
- After G ships: every §2 "MUST" is covered in `docs/DUST-CONFORM-COVERAGE.md`
- H freezes the coverage matrix

**The circularity v4 had is resolved:** E's pre-F assertions run against the `dust-conformance/fixtures/minimal/` fixture (built and maintained by the conformance crate itself), not against F's public `hello-plugin`. F then builds its own public reference plugin and validates it against the same harness as a Phase F exit criterion. E does not depend on F.

### 3.7 Phase F — Public reference plugin + language stubs

**Depends on:** Phase C AND Phase D AND Phase E (C+D exit criteria passed against the harness-internal fixture; E assertions for §2.2–§2.15 implemented and green)

**Touches:** `plugins/dust/dust-sdk/examples/hello/` (new Rust reference), `plugins/dust/examples/stubs/{go,python,bash}/` (new)

**Scope:**
- **Rust reference plugin (public):** clean `hello-plugin` using `dust-sdk` directly — handshake, event emit, `refresh` handling, graceful shutdown, cancellation, redaction demo. This is the **public** reference (copy-as-a-template for new plugin authors), distinct from the harness-internal fixture at `dust-conformance/fixtures/minimal/`
- **Go stub** — ~150 lines, `encoding/json` + `net` + the new envelope shape
- **Python stub** — ~150 lines, `asyncio` + `json` + the new envelope shape
- **Bash stub** — `jq` + `socat` + a while-read loop. Proves the protocol is implementable without structured client libraries
- All four (Rust hello + 3 stubs) pass `dust-conform` end-to-end against the full §2 coverage built by Phase E

**Exit criteria:**
- `dust-conform --plugin-manifest <each of the four>` passes 100% against every section E covers
- Each non-Rust stub is under 200 lines
- Each stub has a "Writing a plugin in <language>" section in `docs/DUST-WIRE-SPEC.md` with the stub as the worked example
- `hello-plugin` is genuinely copyable: a new plugin author can `cp -r plugins/dust/dust-sdk/examples/hello plugins/mything` and have a working dust plugin after editing `plugin.json.name` and the domain logic

### 3.8 Phase G — Tracker POC

**Depends on:** Phase F

**Touches:** `plugins/tracker/src/dust_serve.rs` (new) or a new `bin/tracker-dust`, `plugins/tracker/plugin.json` (add `dust:` block)

**Scope:**
- Implement the protocol against real tracker state: `manifest`, `render` (items list), `action` (next/ready/tree + update mutations), `events.subscribe`, emit `data_updated` on issue create/edit/close
- Handle `refresh` from registry by re-emitting current state
- Keep the existing `tracker query ...` CLI — it stays for terminal use and orchestrator preflight
- `log_redact: []` (tracker has no sensitive fields)
- Pass `dust-conform`

**Exit criteria:**
- Tracker plugin reaches `active` within 2s of dropping its `plugin.json`
- Creating an issue via `tracker create` produces a `data_updated` event in `~/.alluka/dust/events.jsonl` within 1s
- `dust-conform` passes
- A 50-line Python subscriber connects, calls `events.subscribe`, and renders a live-updating terminal view of tracker issues — proving decoupled consumers work
- Design gaps surfaced during G are logged to `docs/DUST-WIRE-SPEC-GAPS.md` for Phase H

### 3.9 Phase H — Final spec freeze (v1.0.0)

**Depends on:** Phase G

**Touches:** `docs/DUST-WIRE-SPEC.md` (final), `docs/PLUGIN-PROTOCOL.md` (rewritten with deprecation pointer), `docs/DUST-CONFORM-COVERAGE.md` (finalized)

**Scope:**
- Apply every gap from `docs/DUST-WIRE-SPEC-GAPS.md` to the spec text
- Tag protocol as `v1.0.0` in spec frontmatter
- Rewrite `docs/PLUGIN-PROTOCOL.md` with a top-of-file pointer to `DUST-WIRE-SPEC.md` and a legacy-CLI-contract appendix
- Run full conformance suite against reference plugin, all 3 language stubs, and tracker
- `CHANGELOG.md` entry for dust crates noting v1 tag

**Exit criteria:**
- `docs/DUST-WIRE-SPEC.md` has `version: 1.0.0` in frontmatter
- `docs/PLUGIN-PROTOCOL.md` points at the new spec
- Full conformance suite green
- `docs/DUST-CONFORM-COVERAGE.md` at 100% "MUST" coverage
- Residual deferred clauses documented as §4 risks

### 3.10 Separate track — Wails + orphan cleanup

**Depends on:** — (independent of protocol phases)

**Touches:** `plugins/dashboard/` (delete), `plugins/dust/plugins/` (delete), orphaned `plugins/*/ui/` dirs (delete — list below), `scripts/nanika-update.sh`, `CLAUDE.md`, Claude `settings.json` MCP bridge entries

**Runs:** any time; not gated by any protocol phase. Can land before Phase A or after Phase H.

**Scope:**
- **Pre-cleanup audit** (hard gate): `rg 'plugins/dashboard'` across `~/.claude`, `~/.alluka`, mission files (`~/.alluka/missions/**`), persona memory (`personas/**/MEMORY.md`), scheduled jobs (`scheduler jobs`). Artifact committed with the cleanup branch. Nothing is deleted until the audit is complete
- **Rollback posture**: cleanup happens on a named branch `wails-retirement`; branch survives 7 days from the last deletion before merge
- `rm -rf plugins/dashboard/`
- `rm -rf plugins/dust/plugins/` — nested prototype plugins subdir
- **Orphaned `ui/` deletions, enumerated** (verify exact set via `find plugins -type d -name ui` before running):
  - `plugins/discord/ui/`
  - `plugins/elevenlabs/ui/`
  - `plugins/gmail/ui/`
  - `plugins/linkedin/ui/`
  - `plugins/nen/ui/` (if present)
  - `plugins/obsidian/ui/`
  - `plugins/reddit/ui/`
  - `plugins/substack/ui/` (including `substack/ui/shims/`)
  - `plugins/telegram/ui/`
  - `plugins/ynab/ui/`
  - `plugins/youtube/ui/`
  - `plugins/engage/ui/`
  - `plugins/scheduler/ui/`
  - `plugins/scout/ui/`
  - `plugins/tracker/ui/`
- `plugins/dashboard/frontend/src/generated/plugin-ui-map.ts`
- Remove `plugins/dashboard` entry from `scripts/nanika-update.sh`
- Remove the three MCP bridge tools (`navigate`, `notify`, `reply`) from any Claude `settings.json`
- Update `CLAUDE.md` to drop dashboard references
- `docs/PLUGIN-PROTOCOL.md` gets a deprecation note pointing at `DUST-WIRE-SPEC.md` once A ships

**Exit criteria:**
- Pre-cleanup audit artifact committed
- `rg '\bplugins/dashboard\b'` returns zero hits in `scripts/`, `plugins/`, `docs/`, `CLAUDE.md`
- `plugins/dust/plugins/` does not exist
- `find plugins -type d -name ui` matches the deletion list
- `scripts/nanika-update.sh --dry-run` clean
- Branch survived 7 days from last deletion
- MCP bridge tools removed from all `settings.json`

---

## 4. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | Event vocabulary chosen in Phase A is too narrow and needs breaking changes after plugins adopt it | Medium | High | Vocabulary intentionally minimal (9 wire event types). Unknown types logged + dropped. Semver rules allow minor-bump additions |
| R2 | Observability writer queue overflow loses data users expected to see | Medium | Medium | Drop-oldest semantics + `events_dropped` counter + `log_overflow` meta record written directly to `events.jsonl`. Spec wording is "bounded audit history"; users are told up front |
| R3 | Hot-plug debounce miscalibrated and a plugin drops in gets spawned twice | Medium | High | Canonical-path dedup + spawn mutex per plugin_id + stale-socket collision detection. Harness test: drop same manifest 50 times in 100ms, assert exactly one spawn |
| R4 | Phase G surfaces a design flaw that invalidates code written in B/C/D | Medium | High | Design flaws found in G feed into H spec freeze. B/C/D not "frozen" until H. Cost measured in H effort, not phase abandonment |
| R5 | Conformance harness is too strict and rejects legitimate implementations | Low | Medium | Assertions are observable-behavior only (wire bytes, state transitions, timings). Timing assertions have ±20% tolerance to survive slow CI |
| R6 | Language-agnostic claim is aspirational — Go/Python/bash stubs don't actually work | Medium | Medium | Stubs are Phase F exit criteria. Bash stub is the hardest test; if `jq+socat` can implement the protocol, any structured language can |
| R7 | Preflight hook still depends on `query --json`; plugins shipping only the new protocol break preflight | Low | High | Spec is explicit: CLI `query --json` is not removed, only deprecated for UI consumers. Plugins adopting the new protocol may additionally keep their CLI |
| R8 | Spawn/handshake timeout (5s) too tight for slow-starting plugins | Low | Low | Manifest override via `dust.spawn_timeout_ms` (§2.15) |
| R9 | Observability writer fsync cadence is too slow and events lost on process crash | Low | Medium | fsync per batch (100ms or 4 KiB). Up to 100ms of events may be lost on crash. Documented. Durability-critical consumers use `events.subscribe` replay, not write-level durability |
| R10 | Wails cleanup accidentally removes something a user workflow depends on | Medium | High | Pre-cleanup audit (hard gate in §3.10) + named branch + 7-day soak + `git revert` rollback |
| R11 | Design gap surfaced after H freeze requires a protocol bump | Medium | Low | v1 is good, not perfect. Residual gaps in `docs/DUST-WIRE-SPEC-GAPS.md` for v1.1. Semver rules keep the compat ladder short |
| R12 | Spec drift between Phase A draft and B/C/D implementation | Medium | High | Phase E runs alongside C/D with assertions derived from spec text directly. Divergence surfaces as failing assertions immediately, not at integration time |
| R13 | Redaction policy too narrow — manifest misses a sensitive field, secrets leak into log | Medium | High | Phase F audit pass against reference plugin catches common field names. Phase G exercise catches real-data cases. Redaction is a writer-side policy, addable post-v1 without a protocol bump |
| R14 | Peer credential check not portable across OSes; host refuses to start on unsupported OS | Low | Medium | v1 OS matrix documented: macOS + Linux. Clear error on unsupported OS. Windows out of scope |
| R15 | Plugin restart loses event sequence continuity; subscribers can't distinguish restart from packet loss | Medium | Medium | §2.9 explicit: plugin restart resets sequence to 1. Subscriber detects `sequence < last_known` as restart signal, re-subscribes from `since_sequence: 0` |
| R16 | `dust-conform` harness itself has bugs | Low | Medium | Harness is table-driven from spec text. Deliberately-broken reference plugin test proves the negative case |
| R17 | `events.subscribe` returns the entire retained ring in a single response | Low | Low | Ring is byte-bounded at 512 KiB serialized (§2.9), leaving ~500 KiB headroom under the 1 MiB frame cap. Event-size pathologies cannot exceed the frame bound. Subscribers wanting smaller batches filter client-side or subscribe at a higher `since_sequence` |
| R18 | `refresh_manifest` causes a race with in-flight requests against the old manifest | Low | Low | Manifest refresh is request-scoped: new requests use the new manifest, in-flight requests complete against the old. Pointer swap is trivially safe under a mutex |
| R19 | Same-user trust posture is wrong for users who share their uid with untrusted code (e.g. shared dev machines with lax sandboxing) | Low | Medium | Documented in §2.16. Users requiring stronger isolation must use OS-level sandboxing. Dust does not claim to defend against same-uid adversaries |
| R20 | `Capability::Scheduler` kept in v1 but with no dispatch semantics surprises plugin authors who declare it expecting scheduled execution | Low | Low | Documented in §2.13 as reserved/advisory. A dispatch mechanism is reserved for v1.1+. Authors needing scheduled execution continue to use the nanika scheduler CLI |
| R21 | Harness-internal conformance fixture drifts from the public hello reference plugin, so F's exit assertions diverge from E's pre-F assertions | Low | Medium | Both fixtures live in the dust workspace and are grep-able. A CI check compares their `ready.data.manifest.dust.capabilities` and ensures the fixture covers every capability the hello plugin declares |

---

## 5. Open questions

Most v2/v3/v4 questions are resolved in §2 of this plan. Remaining:

1. **Phase G POC plugin — `tracker` (Rust) vs. a non-Rust plugin to stress the language-agnostic claim more aggressively?** — touches **Phase G**. Recommended default: **tracker** for v1, paired with the Phase F language stubs. Residual risk is R6 (stubs prove the bytes, not ergonomics). Flagged, not blocking.

Everything else previously held in §5 is now resolved in §2:
- v2 Q1 / v3 Q1 (observability log path): `~/.alluka/dust/events.jsonl` (§2.10)
- v2 Q2 / v3 — (socket path): `$XDG_RUNTIME_DIR/nanika/plugins/` or `~/.alluka/run/plugins/` (§2.1)
- v2 Q3 (manifest nesting): nested `dust:` block (§2.15)
- v2 Q4 / v3 — (auth): peer credential check + same-user trust posture explicitly scoped (§2.16)
- v2 Q5 (binary format): JSON only for v1 (§2.2)
- v2 Q6 (orchestrator coupling): plugins independent (§2.9)
- v2 Q7 (retire `PLUGIN-PROTOCOL.md`): rewrite with legacy appendix (Phase H)
- v3 Q3 (`refresh_manifest` shape): request method (§2.15)
- v3 Q4 (writer layout): single file, single writer goroutine, multi-producer channel (§2.10)
- v3 Q5 (`Capability::Scheduler`): kept as reserved/advisory in v1 (§2.13). Removal deferred to v1.1+
- v4 Q1 (multi-consumer topology): canonicalized as first-connection-wins in §2.5. v1.1 may add registry-election if required

---

## 6. Not included in this plan

- Any Tauri UI work (missions tab, command palette, chat, system tab, tracker UI, metrics tab)
- Replacement for the Wails macOS tray / global hotkey
- Reviving `GetPluginUIBundle` dynamic JS injection
- Migration of any plugin beyond `tracker` in Phase G
- Removal of `<binary> query --json` from plugins that still use it for terminal or preflight
- Windows support (OS matrix: macOS + Linux)
- Cross-machine / remote consumer support (UDS is local-only; TCP or stream-over-SSH is v2+)
- Manifest signing or supply-chain hardening (v2+)
- A visual dashboard consumer of any kind
- Defense against same-uid malicious code (explicitly out of threat model scope — see §2.16)

When you're ready to build a UI consumer, v1.0.0 of the protocol is what it connects to. Until then, `dust-conform`, the four language stubs, and the observability log + `dust logs` CLI are enough to develop and validate against.

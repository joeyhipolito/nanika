---
title: "Dust Wire Protocol Specification"
version: 1.0.0
status: stable
created: "2026-04-12"
frozen_at: "2026-04-13"
source: "docs/dust-protocol-plan.md §2.1–§2.17"
---

# Dust Wire Protocol Specification

**Version 1.0.0 — stable, frozen 2026-04-13**

## Notation

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD",
"SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be
interpreted as described in RFC 2119.

Every normative requirement carries a unique identifier (e.g., **TRANSPORT-01**)
for conformance-harness traceability. A conformant implementation MUST satisfy
every requirement in this document.

---

## §1  Transport

### 1.1  Socket Path Convention

**TRANSPORT-01.**  The host MUST resolve the socket path for a given plugin as:

```
$XDG_RUNTIME_DIR/nanika/plugins/<plugin-id>.sock
```

when the environment variable `XDG_RUNTIME_DIR` is set and non-empty.

**TRANSPORT-02.**  When `XDG_RUNTIME_DIR` is unset or empty, the host MUST use
the fallback path:

```
~/.alluka/run/plugins/<plugin-id>.sock
```

where `~` expands to the home directory of the user running the host process.

**TRANSPORT-03.**  The host MUST NOT place sockets under `/tmp`, `/var/run`, or
any other shared namespace.

### 1.2  Permissions

**TRANSPORT-04.**  The host MUST create the runtime directory
(`nanika/plugins/` or `.alluka/run/plugins/`) with mode `0700` and all ancestor
directories it creates with mode `0700`.

**TRANSPORT-05.**  The host MUST create the Unix domain socket file with mode
`0600`.

**TRANSPORT-06.**  Both the runtime directory and the socket file MUST be owned
by the uid of the user running the host process.

### 1.3  Peer Credential Verification

**TRANSPORT-07.**  On every accepted connection, the host MUST verify that the
connecting peer's effective uid matches the host's own uid, using the
platform-appropriate mechanism:

| Platform | Mechanism |
|---|---|
| Linux | `SO_PEERCRED` on the accepted socket fd |
| macOS | `LOCAL_PEERCRED` or `getpeereid()` on the accepted socket fd |
| BSD | `LOCAL_PEERCRED` on the accepted socket fd |

**TRANSPORT-08.**  If the peer uid does not match the host uid, the host MUST
close the connection immediately without sending any data. The host MUST log
the rejection, including the mismatched uid.

**TRANSPORT-09.**  On any operating system that does not provide a peer
credential mechanism listed in **TRANSPORT-07**, the host MUST refuse to start
and MUST emit a diagnostic identifying the unsupported OS.

**TRANSPORT-10.**  The v1 supported OS matrix is: **macOS** and **Linux**. An
implementation MAY support additional platforms if they satisfy
**TRANSPORT-07**, but is not required to.

### 1.4  Same-User Trust Posture

**TRANSPORT-11.**  The peer credential check proves only that the connecting
process has the same uid as the host. It does NOT prove the peer is the binary
resolved from the plugin manifest. The v1 protocol accepts same-user trust: any
process running as the host's uid is considered a valid protocol peer.

**TRANSPORT-12.**  Implementations MUST NOT claim defense against an adversary
who already holds the host user's uid. Such an adversary can `ptrace` the
legitimate plugin, read its memory, or replace its binary — these attacks are
out of scope for v1. See the threat model (§17) for
the explicit security boundary.

### 1.5  Stale Socket Cleanup

**TRANSPORT-13.**  On startup, the host MUST enumerate all `.sock` files in the
runtime directory and remove any socket whose `<plugin-id>` portion is not
present in the current manifest set. Sockets whose plugin-id IS in the manifest
set MUST NOT be removed by this cleanup — they may belong to a live instance.

**TRANSPORT-14.**  When a plugin process starts and finds an existing socket at
its path, it MUST attempt to `connect()` to that socket:

- If `connect()` succeeds, another instance of this plugin is already live.
  The new process MUST exit with code `1`.
- If `connect()` fails (e.g., `ECONNREFUSED`, `ENOENT` after race), the new
  process MUST `unlink()` the stale socket and then `bind()` its own.

### 1.6  Plugin ID Grammar

**TRANSPORT-15.**  A plugin ID MUST match the following POSIX extended regular
expression:

```
^[a-z][a-z0-9_-]{1,63}$
```

This means:
- First character: lowercase ASCII letter (`a`–`z`).
- Subsequent characters: lowercase ASCII letters, digits (`0`–`9`), underscore
  (`_`), or hyphen (`-`).
- Minimum total length: 2 characters.
- Maximum total length: 64 characters.

**TRANSPORT-16.**  Plugin IDs that do not match **TRANSPORT-15** MUST be
rejected at manifest parse time. The host MUST NOT create a socket for an
invalid plugin ID.

---

## §2  Framing

### 2.1  Wire Format

**FRAME-01.**  Every protocol frame on the wire MUST consist of exactly two
contiguous fields:

```
┌─────────────────────────┬──────────────────────┐
│ 4-byte length (u32 BE)  │  UTF-8 JSON payload  │
└─────────────────────────┴──────────────────────┘
```

- The **length prefix** is a 4-byte unsigned integer in **big-endian** byte
  order.
- The **payload** is the UTF-8-encoded JSON bytes.

**FRAME-02.**  The `length` field MUST contain the exact count of UTF-8 bytes
in the payload, measured after JSON serialization and before any
transport-level processing. The length does NOT include the 4-byte prefix
itself.

### 2.2  Frame Size Limit

**FRAME-03.**  The maximum permitted value of the `length` field is **1,048,576
bytes** (1 MiB, `0x00100000`). If a receiver reads a `length` value strictly
greater than `0x00100000`, it MUST close the connection, log `frame_oversized`
with the received length value, and transition the plugin to the `dead` state.

### 2.3  Normative Edge Cases

The following table defines the required behavior for every framing edge case.
Each row is a testable assertion.

| ID | Condition | Required Behavior |
|---|---|---|
| **FRAME-04** | `length == 0` | Accepted in all connection states after `handshake_wait`. The zero-length frame has no effect. It MUST NOT reset the heartbeat miss counter. |
| **FRAME-05** | `length > 1 MiB` (`0x100000`) | Close connection. Log `frame_oversized`. Transition plugin to `dead`. |
| **FRAME-06** | EOF received while reading the 4-byte length prefix | Treat as a clean disconnect. Transition plugin to `dead`. No error logged. |
| **FRAME-07** | EOF received after the length prefix but before `length` payload bytes are read | Close connection. Log `frame_truncated` with the number of bytes received vs. expected. Transition plugin to `dead`. |
| **FRAME-08** | Payload bytes are not valid UTF-8 | Close connection. Log `frame_utf8_error`. Transition plugin to `dead`. |
| **FRAME-09** | Payload is valid UTF-8 but is not parseable as JSON | Close connection. No attempt to correlate an `id` field. No error response sent. |
| **FRAME-10** | Payload is valid JSON but the top-level value is not a JSON object (e.g., array, string, number, `null`) | Close connection. |
| **FRAME-11** | Payload is a JSON object with no `kind` field | Close connection. |
| **FRAME-12** | Payload is a JSON object whose `kind` value is not one of the five defined kinds (`request`, `response`, `event`, `heartbeat`, `shutdown`) | Close connection. |
| **FRAME-13** | Partial read: the underlying socket returns fewer bytes than `length` requires before EOF | Buffer received bytes and retry the read. A per-frame read deadline of **500 ms** starts from the moment the first byte of the payload is buffered. If the deadline elapses before `length` bytes are collected, close the connection. Log `frame_read_timeout`. Transition plugin to `dead`. |
| **FRAME-14** | Partial write: the underlying socket accepts fewer bytes than the frame being written | Buffer remaining bytes and retry the write. A per-frame write deadline of **1,000 ms** (1 s) starts from the moment the first byte of the frame is written. If the deadline elapses before all bytes are flushed, close the connection. Log `frame_write_timeout`. Transition plugin to `dead`. |
| **FRAME-15** | Slowloris: the peer drip-feeds bytes at a rate that would keep the connection alive indefinitely | Detected by **FRAME-13** — the 500 ms per-frame read deadline applies to the aggregate read. Any frame whose payload cannot be fully received within 500 ms of its first buffered byte is terminated. |

### 2.4  Malformed Payload Policy

**FRAME-16.**  When a received payload fails any validation step
(**FRAME-08** through **FRAME-12**), the receiver MUST close the connection
immediately. The receiver MUST NOT attempt to extract an `id` from the broken
payload to send a correlated error response. Rationale: extracting fields from
malformed or partially-parsed data requires a hand-rolled pre-parser and is
unimplementable reliably. The peer reconnects and retries.

---

## §3  Envelope

### 3.1  Discriminated Kinds

**ENVELOPE-01.**  Every framed JSON payload MUST be a JSON object containing a
`kind` field whose value is a string. The `kind` field discriminates the
envelope into exactly one of five kinds. No other kinds are permitted in v1.

| Kind | Direction | Description |
|---|---|---|
| `request` | Either side may initiate | Asks the peer to perform a method |
| `response` | Sent by the method handler to the requester | Carries the result or error for a prior request |
| `event` | Either side may push | Fire-and-forget notification; no response expected |
| `heartbeat` | Either side | Mutual liveness signal |
| `shutdown` | Registry → plugin only | Graceful teardown request |

### 3.2  Kind: `request`

**ENVELOPE-02.**  A `request` envelope MUST contain the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | string | MUST | Literal `"request"` |
| `id` | string | MUST | Unique identifier matching the pattern `req_<16hex>` (see **ENVELOPE-14**) |
| `method` | string | MUST | The method name to invoke |
| `params` | object | MAY | Method-specific parameters. If absent, the receiver MUST treat it as equivalent to `{}` |

#### Wire Trace Example

Compact JSON (no whitespace):
```json
{"kind":"request","id":"req_a1b2c3d4e5f67890","method":"manifest","params":{}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 4E` (78 in decimal) |
| Payload | 78 bytes UTF-8 |
| **Total frame** | **82 bytes** |

### 3.3  Kind: `response`

**ENVELOPE-03.**  A `response` envelope MUST contain the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | string | MUST | Literal `"response"` |
| `id` | string | MUST | Mirrors the `id` of the originating request |
| `result` | object | Conditional | Present if and only if the method succeeded |
| `error` | object | Conditional | Present if and only if the method failed |

**ENVELOPE-04.**  The `result` and `error` fields are **mutually exclusive**.
Exactly one of the two MUST be present in every response. A response containing
both `result` and `error` is a protocol error — the receiver MUST close the
connection. A response containing neither `result` nor `error` is equally a
protocol error — the receiver MUST close the connection.

**ENVELOPE-05.**  When present, the `error` object MUST contain:

| Field | Type | Required | Description |
|---|---|---|---|
| `code` | integer | MUST | An error code from the registry (§4) |
| `message` | string | MUST | Human-readable description |
| `data` | object | MAY | Additional structured context |

#### Wire Trace Example (success)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","result":{"name":"tracker","version":"1.2.0"}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 5D` (93 in decimal) |
| Payload | 93 bytes UTF-8 |
| **Total frame** | **97 bytes** |

#### Wire Trace Example (error)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","error":{"code":-32601,"message":"method not found"}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 64` (100 in decimal) |
| Payload | 100 bytes UTF-8 |
| **Total frame** | **104 bytes** |

### 3.4  Kind: `event`

**ENVELOPE-06.**  An `event` envelope MUST contain the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | string | MUST | Literal `"event"` |
| `id` | string | MUST | Unique identifier matching the pattern `evt_<16hex>` (see **ENVELOPE-14**) |
| `type` | string | MUST | One of the defined event types (see below) |
| `ts` | string | MUST | ISO 8601 timestamp with millisecond precision (e.g., `"2026-04-12T09:30:00.123Z"`) |
| `sequence` | integer | Conditional | MUST be present on plugin-originated events. Monotonically increasing per plugin connection. MAY be absent on host-originated events |
| `data` | object | MUST | Event-specific payload |

**ENVELOPE-07.**  The v1 event type vocabulary is:

| Type | Originator | Description |
|---|---|---|
| `ready` | plugin → host | Plugin has completed initialization |
| `host_info` | host → plugin | Host configuration pushed after handshake |
| `status_changed` | plugin → host | Plugin lifecycle state change |
| `progress` | plugin → host | Long-running operation progress |
| `log` | plugin → host | Structured log record |
| `error` | plugin → host | Non-fatal error notification |
| `data_updated` | plugin → host | Domain data changed |
| `refresh` | host → plugin | Host requests the plugin re-render or re-fetch |
| `visibility_changed` | host → plugin | Consumer visibility state change |

#### Wire Trace Example

```json
{"kind":"event","id":"evt_f0e1d2c3b4a59687","type":"status_changed","ts":"2026-04-12T09:30:00.123Z","sequence":42,"data":{"status":"ready"}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 8C` (140 in decimal) |
| Payload | 140 bytes UTF-8 |
| **Total frame** | **144 bytes** |

### 3.5  Kind: `heartbeat`

**ENVELOPE-08.**  A `heartbeat` envelope MUST contain the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | string | MUST | Literal `"heartbeat"` |
| `ts` | string | MUST | ISO 8601 timestamp with millisecond precision |

**ENVELOPE-09.**  Heartbeats carry no `id` field. They MUST NOT be logged by
the observability layer (protocol principle 2 explicitly exempts heartbeats).

#### Wire Trace Example

```json
{"kind":"heartbeat","ts":"2026-04-12T09:30:00.123Z"}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 34` (52 in decimal) |
| Payload | 52 bytes UTF-8 |
| **Total frame** | **56 bytes** |

### 3.6  Kind: `shutdown`

**ENVELOPE-10.**  A `shutdown` envelope MUST contain the following fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `kind` | string | MUST | Literal `"shutdown"` |
| `reason` | string | MUST | One of the defined reason codes (see below) |

**ENVELOPE-11.**  The `shutdown` kind is unidirectional: it is sent by the
registry (host) to the plugin ONLY. A plugin MUST NOT send a `shutdown`
envelope. If the registry receives a `shutdown` from a plugin, it MUST close
the connection.

**ENVELOPE-12.**  The v1 shutdown reason vocabulary is:

| Reason | Semantics |
|---|---|
| `host_exit` | The host process is shutting down |
| `plugin_disable` | The plugin was disabled by the user or configuration |
| `version_mismatch` | Handshake protocol version is outside the host's supported range |
| `watcher_delete` | The plugin's manifest file was deleted from disk |
| `binary_deleted` | The plugin binary was removed or is no longer executable |
| `watcher_error` | The file watcher encountered an unrecoverable error for this plugin |
| `consumer_error` | A consumer reported an unrecoverable error for this plugin |
| `timeout` | The plugin failed to respond within the host's deadline (e.g., handshake timeout) |

#### Wire Trace Example

```json
{"kind":"shutdown","reason":"host_exit"}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 28` (40 in decimal) |
| Payload | 40 bytes UTF-8 |
| **Total frame** | **44 bytes** |

### 3.7  ID Uniqueness

**ENVELOPE-13.**  Request and event IDs MUST be unique within the scope of
**(connection, direction)**. That is, the registry and plugin each maintain
independent ID namespaces. A request ID generated by the registry has no
relation to a request ID generated by the plugin, even if they share the same
string value.

**ENVELOPE-14.**  Request IDs MUST match the pattern `req_<16hex>` where
`<16hex>` is exactly 16 lowercase hexadecimal characters (`[0-9a-f]{16}`).
Event IDs MUST match the pattern `evt_<16hex>` with the same hex constraint.

**ENVELOPE-15.**  If a receiver encounters a request whose `id` duplicates an
already-in-flight request ID in the same direction on the same connection, the
receiver MUST respond with error code `-32600` (`invalid_request`) and MUST
ignore the duplicate request (i.e., MUST NOT invoke the method). The original
in-flight request is unaffected.

### 3.8  Response Ordering

**ENVELOPE-16.**  Responses MAY arrive in any order relative to the requests
that generated them. Receivers MUST correlate responses to requests by the `id`
field only and MUST NOT assume FIFO ordering.

### 3.9  Late Responses

**ENVELOPE-17.**  If a response arrives after the per-request timeout for its
corresponding request has elapsed, the receiver MUST drop the response and MUST
log `late_response` with the `id` and the elapsed time. No retroactive state
update is permitted.

**ENVELOPE-18.**  During the `draining` connection state, a response MUST be
accepted and forwarded if its `id` matches an in-flight request. A response
whose `id` does not match any in-flight request during `draining` MUST be
dropped.

**ENVELOPE-19.**  After a connection transitions to the `dead` state, all
received responses MUST be dropped.

---

## §4  Error Code Registry

### 4.1  Registry Table

**ERROR-01.**  The error code registry is a **closed enum** in v1. The `code`
field in a response `error` object MUST contain one of the codes listed below.
Implementations MUST NOT invent new top-level error codes outside this table.

| Code | Name | Semantics | Produces Response? |
|---|---|---|---|
| `-32700` | `parse_error` | **Reserved.** Per §2 **FRAME-09**, the receiver closes the connection on parse failure rather than sending this code. Implementations MUST NOT send `-32700` in a response. It exists in the registry for JSON-RPC lineage documentation only. | No (connection closed) |
| `-32600` | `invalid_request` | Envelope is malformed, contains a duplicate `id` (see **ENVELOPE-15**), or is missing a required field. | Yes |
| `-32601` | `method_not_found` | The `method` field in a `request` names a method that the receiver does not implement. | Yes |
| `-32602` | `invalid_params` | The `params` object fails method-specific validation, including the presence of unknown required fields or type mismatches. | Yes |
| `-32603` | `internal_error` | An unexpected error occurred on the receiver side. Receivers SHOULD treat occurrences of this code as bugs warranting investigation. | Yes |
| `-33001` | `timeout` | The request exceeded its per-request deadline. This is distinct from heartbeat timeouts and per-frame deadlines (**FRAME-13**, **FRAME-14**). | Yes |
| `-33002` | `canceled` | The operation was canceled via a `cancel` request (method `"cancel"`). | Yes |
| `-33003` | `unsupported_version` | During handshake, the peer advertised a protocol version outside the receiver's supported range. | Yes |
| `-33004` | `unauthorized` | The peer credential check failed (see **TRANSPORT-08**), or the caller's role is denied for the requested operation. | Yes |
| `-33005` | `busy` | Backpressure signal: the receiver's in-flight request limit or internal queue limit has been reached. The sender SHOULD retry with exponential backoff. | Yes |
| `-33006` | `shutting_down` | The receiver is in the `draining` state and is rejecting new requests. The sender MUST NOT retry until reconnection. | Yes |
| `-33007` | `replay_gap` | An `events.subscribe` request specified a `since_sequence` cursor that is older than the ring buffer's retention window. The subscriber missed events. | Yes |
| `-33008` | `plugin_dead` | Registry-side synthetic error returned on any request targeting a plugin that is in the `dead` state. | Yes |
| `-33009` | `frame_oversized` | **Reserved for logging only.** When **FRAME-05** fires, the implementation logs `frame_oversized` but closes the connection rather than sending this code in a response. Implementations MUST NOT send `-33009` in a response. | No (connection closed) |

### 4.2  Plugin-Specific Error Codes

**ERROR-02.**  Plugins that need to surface domain-specific errors MUST do so
via the `error.data.plugin_code` field inside the standard error object. The
top-level `code` field MUST always be one of the codes in the registry table
above (typically `-32603` for internal plugin errors or `-32602` for validation
failures).

**ERROR-03.**  The structure of `error.data` beyond `plugin_code` is
unconstrained. Plugins MAY include additional diagnostic fields. Consumers MUST
NOT rely on the shape of `error.data` beyond the `plugin_code` field being
present when the plugin documents it.

### 4.3  Error Code Evolution

**ERROR-04.**  Adding a new code to this registry constitutes a **minor**
protocol version bump (e.g., 1.0 → 1.1).

**ERROR-05.**  Removing a code from this registry, or changing the integer
value assigned to an existing code name, constitutes a **major** protocol
version bump (e.g., 1.x → 2.0).

**ERROR-06.**  Plugins MUST NOT invent codes at the top-level `code` field.
Any integer value in the `code` field that does not appear in the registry
table is a protocol violation. The receiver SHOULD log the unknown code and
MAY close the connection.

---

## §5  Lifecycle State Machine

### 5.1  States

**LIFECYCLE-01.**  The plugin lifecycle as observed by the registry comprises
exactly six states: `spawned`, `connected`, `handshake_wait`, `active`,
`draining`, and `dead`. The `dead` state is terminal. No other states are
defined in v1.

### 5.2  State Transition Table

**LIFECYCLE-02.**  The following table is the normative definition of all
permitted state transitions. Every row is a testable assertion. The table
models the **plugin lifecycle as driven by the registry**. Subscriber
connections have a simpler `connected` → `subscribing` → `closed` lifecycle
and are not modeled here (see §6).

| State | Accepted from plugin | Rejected from plugin | Registry triggers | Timeout behavior | Process-exit behavior | Transitions out |
|---|---|---|---|---|---|---|
| `spawned` | (none — plugin has not bound) | — | — | 5 s wait for socket file to appear → `dead`, reason `socket_never_appeared` | → `dead`, reason `plugin_exited_before_bind` | → `connected` when registry `connect()` succeeds |
| `connected` | (none — registry has not started read loop) | — | — | 5 s wait for first inbound frame → `dead`, reason `handshake_timeout` | → `dead`, reason `plugin_exited_before_handshake` | → `handshake_wait` when registry starts read loop |
| `handshake_wait` | only `event` with `type: ready` | anything else → close + `dead`, reason `premature_traffic` | Registry sends `host_info` event after validating `ready` | 5 s → `dead`, reason `handshake_timeout` | → `dead`, reason `plugin_exited_during_handshake` | → `active` on valid `ready` within version range; → `draining` via shutdown `version_mismatch` on version mismatch (see **LIFECYCLE-03**) |
| `active` | `request`, `response`, `event`, `heartbeat` | duplicate-ID requests → `-32600`; unknown method → `-32601` | Registry MAY send `request`, `response`, `event` (e.g., `refresh`, `visibility_changed`), `heartbeat`, or `shutdown` at any time | per-request timeouts (configurable, default 30 s); 3 missed heartbeats → `dead`, reason `heartbeat_timeout` — with pause override (see **LIFECYCLE-05**) | → `dead`, reason `process_exited` | → `draining` when registry sends `shutdown`; → `dead` on peer close |
| `draining` | `response` (for in-flight requests only), `event` (for ring replay only), `heartbeat` (silently accepted, not tracked) | new `request` → `-33006` | Registry waits for drain to complete; MUST NOT send new requests | 2 s drain deadline → `dead`, reason `drain_timeout`; all in-flight requests respond with `-33002 canceled` | → `dead`, reason `process_exited_during_drain` | → `dead` when drain completes or deadline hits |
| `dead` | all → dropped | — | — | — | — | terminal; registry MAY respawn per `restart` policy in the manifest |

### 5.3  Handshake Integration

**LIFECYCLE-03.**  Within `handshake_wait`, the plugin sends its `ready` event
(see §13). The registry MUST validate `ready.data.protocol_version` against its
supported range. If valid, the registry MUST reply with a `host_info` event
(see §13) and transition the plugin to `active`. If the version is outside the
supported range, the registry MUST send `{kind: "shutdown", reason:
"version_mismatch"}` and transition to `draining`.

**LIFECYCLE-04.**  The plugin MAY close its own end of the connection if it
receives a `host_info` whose `protocol_version_supported` range does not
overlap with its own requirements. This causes a → `dead` transition on the
registry side via process exit observation.

### 5.4  Heartbeat Pause Override

**LIFECYCLE-05.**  During `active`, the missed-heartbeat counter for a
connection MUST pause while any in-flight operation identified by an `op_id` is
actively progressing — defined as receiving at least one `progress` event or
the final `response` within the last `heartbeat_interval_ms`. This override is
keyed by `op_id`, not by request ID, because a single request MAY spawn
multiple `progress` events. Implementations MUST track
`last_progress_ts_per_op` and compare against the heartbeat clock.

### 5.5  Edge Cases

**LIFECYCLE-06.**  If the host process dies during `handshake_wait`, the plugin
MUST observe the peer close, write any queued events to disk if persistent,
clean up its socket, and exit. The registry, on restart, observes the stale
socket, detects no live peer (`connect()` fails), unlinks, and respawns from
the manifest.

**LIFECYCLE-07.**  If a `ready` event arrives after the 5 s handshake timeout
has already fired (i.e., the plugin is now `dead`), the registry MUST drop the
event and log `late_ready`. No state change occurs.

**LIFECYCLE-08.**  The `socket_never_appeared`, `plugin_exited_before_bind`,
`handshake_timeout`, `premature_traffic`, `plugin_exited_before_handshake`,
`plugin_exited_during_handshake`, `heartbeat_timeout`, `process_exited`,
`process_exited_during_drain`, and `drain_timeout` strings in the transition
table are diagnostic reason values logged by the registry. They are NOT part
of the shutdown reason enum (**ENVELOPE-12**) and MUST NOT appear in a
`shutdown` envelope.

---

## §6  Consumer Topology

### 6.1  Registry vs. Subscriber

**CONSUMER-01.**  A plugin has at most **one active registry connection** and
**zero or more read-only subscriber connections**. The registry is the
privileged consumer that:

- Spawned the plugin binary
- Owns lifecycle (shutdown, respawn, termination)
- Performs the `ready` / `host_info` handshake (§13)
- Receives heartbeats and enforces heartbeat timeouts (§7)
- Sends `shutdown` (§8)

**CONSUMER-02.**  Subscribers are processes that connect to the plugin socket
via a separate `accept()`. A subscriber MAY call only the following methods:

- `manifest` — read the plugin's current manifest
- `render` — read the current UI state (read-only, idempotent)
- `events.subscribe` — consume the event stream (§10)
- `events.unsubscribe` — end a live push subscription (§10)

**CONSUMER-03.**  A subscriber MUST NOT call any of the following. A plugin
that receives any of these from a subscriber MUST respond with `-33004
unauthorized`:

- `action` (state mutation)
- `refresh_manifest`
- `shutdown` (envelope kind — see **ENVELOPE-11**)
- Any method with `role: "registry_only"` declared in the plugin manifest

### 6.2  First-Connection-Wins

**CONSUMER-04.**  The plugin MUST track connections in `accept()` order. The
first accepted connection is the registry. Every subsequent connection is a
subscriber and is restricted to the read-only method subset
(**CONSUMER-02**). There is no promotion path: a subscriber MUST NOT become
a registry within the same plugin lifetime.

**CONSUMER-05.**  If a second process attempts to connect as registry (i.e., a
second connection arrives while the first is still active), it is treated as a
subscriber. It receives `-33004 unauthorized` on any mutating call.

**CONSUMER-06.**  If the registry connection drops, the plugin enters
`draining` on the registry side (see §5). Subscribers MUST be disconnected
after the drain completes and the plugin transitions to `dead`.

### 6.3  Heartbeat Scope

**CONSUMER-07.**  Heartbeat tracking applies to the **registry connection
only**. Subscribers are NOT heartbeated. Subscriber disconnection is observed
solely via socket close.

---

## §7  Heartbeat Rules

**HEARTBEAT-01.**  Both the registry and the plugin MUST emit one `heartbeat`
envelope per `heartbeat_interval_ms`. The default interval is **10,000 ms**
(10 s). The interval is negotiable via the `manifest.dust.heartbeat_interval_ms`
field in the `ready` event (§13).

**HEARTBEAT-02.**  The default miss threshold is **3**. If a side does not
receive a heartbeat from its peer within `3 × heartbeat_interval_ms`, it MUST
consider the peer dead. On the registry side, this transitions the plugin to
`dead` with reason `heartbeat_timeout`.

**HEARTBEAT-03.**  Heartbeats MUST pause during the `draining` state on both
sides. Neither side SHALL send heartbeats while draining, and neither side
SHALL count missed heartbeats while draining.

**HEARTBEAT-04.**  Heartbeats MUST pause for long-running operations keyed by
`op_id` (see **LIFECYCLE-05**). The missed-heartbeat counter resumes when no
in-flight `op_id` is actively progressing.

**HEARTBEAT-05.**  The heartbeat envelope carries only `kind` and `ts` — no
`sequence`, no `id`. Heartbeats MUST NOT be logged to the observability stream
(see §11). The rate is too high for durable logging.

---

## §8  Shutdown Semantics

### 8.1  Initiation

**SHUTDOWN-01.**  Only the registry initiates shutdown. A plugin that wants to
shut itself down MUST exit its process instead. The registry observes the
process exit and transitions the plugin to `dead`.

**SHUTDOWN-02.**  The shutdown reason enum is defined in **ENVELOPE-12** and
comprises exactly eight values: `host_exit`, `plugin_disable`,
`version_mismatch`, `watcher_delete`, `binary_deleted`, `watcher_error`,
`consumer_error`, `timeout`.

### 8.2  Drain Behavior

**SHUTDOWN-03.**  On receipt of a `shutdown` envelope, the plugin MUST enter
the `draining` state and MUST complete the drain within `shutdown_drain_ms`
(default **2,000 ms**, configurable via manifest).

**SHUTDOWN-04.**  During drain, the plugin MUST stop accepting new requests and
MUST respond to any incoming request with `-33006 shutting_down`.

**SHUTDOWN-05.**  During drain, the plugin SHOULD flush in-flight responses.
For any in-flight operation that cannot complete before the drain deadline, the
plugin MAY respond with `-33002 canceled`.

**SHUTDOWN-06.**  During drain, the plugin SHOULD flush queued events.

### 8.3  Termination

**SHUTDOWN-07.**  After the drain deadline elapses, the registry MUST send
`SIGKILL` to the plugin process and transition the plugin to `dead`.

**SHUTDOWN-08.**  For any in-flight requests on the registry side that did not
receive a response before the `dead` transition, the registry MUST synthesize
`-33002 canceled` responses and deliver them to the original callers.

---

## §9  Version Negotiation

### 9.1  Handshake Version Exchange

**VERSION-01.**  The plugin advertises its protocol version as a single semver
string in `ready.data.protocol_version` (e.g., `"1.0.0"`). Versions are
advertised via events, not a connect preamble.

**VERSION-02.**  The registry config declares a supported range as `{min:
"<semver>", max: "<semver>"}`. The registry communicates this range to the
plugin via `host_info.data.protocol_version_supported` (see §13).

### 9.2  Mismatch Handling

**VERSION-03.**  If the plugin's advertised version is outside the registry's
supported range, the registry MUST send `{kind: "shutdown", reason:
"version_mismatch"}`, drain, and transition to `dead`.

**VERSION-04.**  If the registry's supported range is outside the plugin's own
supported range, the plugin MUST close its end of the connection after
receiving `host_info`. The registry observes the peer close and transitions to
`dead`.

### 9.3  Semver Bump Rules

**VERSION-05.**  The following bump rules define what constitutes each semver
component change:

| Bump | Scope |
|---|---|
| **Patch** | Bug fixes. Wire format unchanged. No new methods, event types, or error codes. |
| **Minor** | Additive only: new optional envelope fields, new event types, new methods, new capability names, new error codes in the registry. |
| **Major** | Breaking: field removals, required-field additions, method signature changes, state machine changes, error code removals or renumbering. |

### 9.4  Unknown Field Policy

**VERSION-06.**  The following table defines required behavior when a receiver
encounters unknown fields, types, methods, or codes:

| Condition | Required behavior |
|---|---|
| Unknown optional fields in envelopes and `params` | Silently ignored |
| Unknown required fields in `params` | `-32602 invalid_params` |
| Unknown event `type` | Logged as `event_type_unknown`, dropped |
| Unknown method | `-32601 method_not_found` |
| Unknown error code in response | Treated as opaque error, logged |

---

## §10  Reconnect, Replay, and Sequence Ownership

### 10.1  Connection-Level Reconnect

**REPLAY-01.**  There is no connection-level reconnect in v1. A dropped
connection enters `dead` (registry) or closed (subscriber). A new connection
from the same plugin is a fresh lifecycle. A subscriber that wants persistent
event consumption MUST reconnect and re-subscribe from its last-known sequence.

### 10.2  Event Replay via `events.subscribe`

**REPLAY-02.**  The `events.subscribe` method is a hybrid RPC: it returns a
snapshot (all retained events from `since_sequence`) AND establishes a live
push stream from `next_sequence` forward. There is no snapshot-only or
live-only mode in v1.

**REPLAY-03.**  The `events.subscribe` request MUST contain the following
`params`:

| Field | Type | Required | Description |
|---|---|---|---|
| `since_sequence` | integer | MUST | Inclusive cursor: receive events with `sequence >= since_sequence` |

**REPLAY-04.**  The success response MUST contain the following `result`
fields:

| Field | Type | Required | Description |
|---|---|---|---|
| `subscription_id` | string | MUST | Handle for `events.unsubscribe`; NOT used for event routing |
| `events` | array | MUST | Every retained event with `sequence >= since_sequence`, ascending order, bounded by ring size |
| `next_sequence` | integer | MUST | The sequence that the next live event will carry |

#### Wire Trace Example (request)

Compact JSON (no whitespace):
```json
{"kind":"request","id":"req_a1b2c3d4e5f67890","method":"events.subscribe","params":{"since_sequence":41}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 69` (105 in decimal) |
| Payload | 105 bytes UTF-8 |
| **Total frame** | **109 bytes** |

#### Wire Trace Example (success response)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","result":{"subscription_id":"sub_a1b2c3d4e5f67890","events":[],"next_sequence":142}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 83` (131 in decimal) |
| Payload | 131 bytes UTF-8 |
| **Total frame** | **135 bytes** |

#### Wire Trace Example (replay gap)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","error":{"code":-33007,"message":"replay_gap","data":{"oldest_available":100,"requested":41}}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 8D` (141 in decimal) |
| Payload | 141 bytes UTF-8 |
| **Total frame** | **145 bytes** |

### 10.3  `since_sequence` Semantics

**REPLAY-05.**  The `since_sequence` parameter MUST be interpreted as follows:

| Value | Behavior |
|---|---|
| `since_sequence: N` (N > 0) | Return events with `sequence >= N` |
| `since_sequence: 0` | Return all retained events (ring size ≤ 1000) |
| `since_sequence > latest` | Return empty `events` array with `next_sequence = latest + 1`. Live push delivers new events from that point |
| `since_sequence < oldest_available` | Return `-33007 replay_gap` with `oldest_available` in `error.data` |

### 10.4  Live Event Push

**REPLAY-06.**  After a successful subscribe, the plugin MUST begin pushing new
events on the subscriber connection as they are emitted. Pushed events use the
standard event envelope (§3.4) with no additional routing fields — they are
regular `event` kinds on the connection. The subscriber correlates them by
`sequence`, not by `subscription_id`.

### 10.5  `events.unsubscribe`

**REPLAY-07.**  The `events.unsubscribe` method ends the live push for a
subscription. The request MUST contain `params.subscription_id` matching a
prior `events.subscribe` response.

#### Wire Trace Example (request)

```json
{"kind":"request","id":"req_a1b2c3d4e5f67890","method":"events.unsubscribe","params":{"subscription_id":"sub_a1b2c3d4e5f67890"}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 80` (128 in decimal) |
| Payload | 128 bytes UTF-8 |
| **Total frame** | **132 bytes** |

#### Wire Trace Example (response)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","result":{"ok":true}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 44` (68 in decimal) |
| Payload | 68 bytes UTF-8 |
| **Total frame** | **72 bytes** |

### 10.6  Subscription Constraints

**REPLAY-08.**  `subscription_id` is an unsubscribe handle, not a routing key.
A connection MAY have at most **one** active subscription on a plugin. A
second `events.subscribe` on the same connection MUST be rejected with
`-33005 busy`.

**REPLAY-09.**  Disconnect terminates the subscription implicitly. There is
nothing to clean up via `events.unsubscribe` after disconnect.

**REPLAY-10.**  No pagination. The `events.subscribe` response always returns
every retained event matching `since_sequence` in a single call, bounded by
the ring buffer size. A subscriber that wants a smaller window MUST filter
client-side.

### 10.7  Sequence Ownership

**REPLAY-11.**  **Plugin-originated events** get a plugin-assigned monotonic
`u64` in the `sequence` field, starting at `1` at process start. The sequence
does NOT persist across process restart.

**REPLAY-12.**  **Registry-originated events** (`host_info`, `refresh`,
`visibility_changed`) carry no `sequence` field. They are not replayable.

**REPLAY-13.**  Subscribers MUST NOT originate events. Subscribers only consume
them.

**REPLAY-14.**  A subscriber that observes `sequence` decrease below its
last-known value MUST interpret this as a plugin restart, discard its cursor,
and re-subscribe from `since_sequence: 0`.

### 10.8  Ring Buffer

**REPLAY-15.**  Each plugin MUST maintain an in-memory ring of
plugin-originated events. Retention is `min(1000 events, 512 KiB total
serialized size)` — whichever bound is hit first.

**REPLAY-16.**  A new event that would push the total serialized size over
512 KiB MUST evict the oldest events until the new event fits.

**REPLAY-17.**  The 512 KiB byte bound guarantees that a full
`events.subscribe` response fits within the 1 MiB frame cap (**FRAME-03**)
with ~500 KiB headroom for envelope overhead.

**REPLAY-18.**  The ring is scoped to the plugin process lifetime. On plugin
restart, the ring starts empty and sequence starts at `1`.

### 10.9  Startup Snapshot Convention

**REPLAY-19.**  Event-capable plugins SHOULD emit a `data_updated` event into
their ring buffer during startup (after the `ready` handshake, before accepting
subscribers) carrying a full snapshot of current domain state. This lets a
subscriber that calls `events.subscribe` with `since_sequence: 0` receive the
plugin's current state as the first retained event, without needing a separate
bootstrap call.

This is SHOULD (not MUST) so that existing plugins which rely on live mutations
alone remain conformant. Plugins that follow this convention are bootstrap-safe
for subscribers arriving on a cold ring (no prior mutations).

A subscriber that receives an empty `events` array from `since_sequence: 0` MUST
treat it as "no retained snapshot" rather than "no data" — the plugin may
simply not implement this convention. See §14.6 for the recommended
`data_updated` payload shape.

---

## §11  Observability and Redaction

### 11.1  Log Location

**OBSERVE-01.**  The observability log MUST be written to:

```
~/.alluka/dust/events.jsonl
```

where `~` expands to the home directory of the user running the host process.

### 11.2  Writer Architecture

**OBSERVE-02.**  The protocol dispatch path MUST NOT block on disk I/O. All
log writes MUST flow through a bounded channel to a background writer
goroutine. The architecture is:

```
protocol dispatch (hot path)
          │
          ▼ (non-blocking send; drop-oldest on overflow)
┌────────────────────┐
│ bounded channel    │  cap 10,000 per plugin
└────────────────────┘
          │
          ▼
┌────────────────────┐
│ background writer  │  batches every 100 ms or 4 KiB, fsync per batch
└────────────────────┘
          │
          ▼
~/.alluka/dust/events.jsonl
```

**OBSERVE-03.**  The bounded channel MUST have a capacity of **10,000** per
plugin.

**OBSERVE-04.**  When the bounded channel is full, the protocol path MUST drop
the **oldest** queued item (not the new one) and MUST increment the plugin's
`events_dropped` counter.

### 11.3  `log_overflow` Meta Record

**OBSERVE-05.**  `log_overflow` is a **writer meta record**, NOT a wire event.
It MUST NOT appear on any socket. It MUST NOT appear in the event vocabulary
(§14). When the writer detects `events_dropped > 0`, it MUST write a meta
record directly into `events.jsonl` at most once per **60 s** per plugin:

```json
{"ts":"2026-04-12T09:30:00.123Z","plugin_id":"tracker","kind":"meta","type":"log_overflow","data":{"events_dropped_since_last_notice":4217,"reason":"queue_full"}}
```

The `reason` field MUST be one of: `queue_full`, `disk_full`, `writer_error`.

**OBSERVE-06.**  Meta records carry `kind: "meta"` as a discriminator. Regular
log records carry a `direction` field (`"in"` or `"out"`) instead. Log readers
MUST use this distinction to filter meta records from protocol messages.

### 11.4  Log Schema

**OBSERVE-07.**  Each non-meta line in the log file MUST conform to the
following schema:

```json
{"ts":"2026-04-12T09:30:00.123Z","plugin_id":"tracker","direction":"in","sequence":42,"message":{}}
```

| Field | Type | Description |
|---|---|---|
| `ts` | string | ISO 8601 timestamp with millisecond precision |
| `plugin_id` | string | The plugin that produced or received the message |
| `direction` | string | `"in"` (plugin → host) or `"out"` (host → plugin) |
| `sequence` | integer | Plugin-assigned sequence, if present on the envelope |
| `message` | object | The framed envelope after redaction has been applied |

### 11.5  Rotation

**OBSERVE-08.**  The log file MUST be rotated at **100 MB** or **7 days**,
whichever comes first. The rotated file MUST be renamed to
`events.jsonl.<YYYYMMDD-HHMMSS>`. Retention is a bounded audit history, not
an append-forever log.

### 11.6  Heartbeat Filtering

**OBSERVE-09.**  `kind: "heartbeat"` MUST be filtered out at the writer's
input. Heartbeats MUST NOT be logged. All other envelope kinds (`request`,
`response`, `event`, `shutdown`) MUST be logged.

### 11.7  Redaction

**OBSERVE-10.**  Each plugin manifest MAY declare a `log_redact` array of
simplified-JSONPath strings under `dust.log_redact`. Before writing to the
log, the writer MUST strip matching paths from the envelope.

Example manifest fragment:

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

### 11.8  JSONPath Subset Grammar

**OBSERVE-11.**  The v1 JSONPath grammar is a strict subset:

```
path       := '$' segment+
segment    := '.' identifier
            | '[*]'
identifier := [a-zA-Z_][a-zA-Z0-9_]*
```

No filters, no slices, no recursion.

**OBSERVE-12.**  Invalid paths MUST be logged at manifest parse time with
`invalid_redact_path` and MUST be skipped. The plugin MUST still load.

### 11.9  Default and Per-Message Redaction

**OBSERVE-13.**  Nothing is redacted by default. Plugins opt in to protection
via `log_redact`. The spec does not maintain a list of "sensitive field names"
because every plugin has different data shapes.

**OBSERVE-14.**  Plugins that want to mark a single message as "do not log"
MUST set `$.meta.log_redact_all: true` in the envelope. The writer MUST honor
this and MUST write a placeholder record with only `{ts, plugin_id, direction,
sequence, redacted: true}`.

### 11.10  Log Permissions

**OBSERVE-15.**  The log file MUST be created with mode `0600`. The log
directory MUST be created with mode `0700`. Ownership MUST match the runtime
directory (see **TRANSPORT-06**).

---

## §12  Backpressure

### 12.1  Resource Limit Table

**PRESSURE-01.**  The following table defines all backpressure limits in v1.
Every row is a testable assertion.

| Resource | Limit | Overflow behavior |
|---|---|---|
| In-flight requests per connection per direction | 100 | 101st request → error `-33005 busy` |
| Queued events in ring (per plugin) | `min(1000 events, 512 KiB total)` | Oldest evicted (whichever bound hits first); late subscribers get `-33007 replay_gap` |
| Log writer bounded channel (per plugin) | 10,000 | Drop oldest; increment `events_dropped`; writer emits `log_overflow` meta record (**OBSERVE-05**) at most once per 60 s |
| Concurrent registry connections per plugin | 1 | Second registry connection → close with `-33004 unauthorized` |
| Concurrent subscriber connections per plugin | 16 | 17th connection → close with `-33005 busy` |
| Active subscriptions per connection | 1 | Second `events.subscribe` on the same connection → `-33005 busy` |
| Disk full on log write | — | Writer enters degraded mode; drops batches; writes `log_overflow` meta record at most once per 60 s; protocol path unaffected |

### 12.2  Slow Consumer Policy

**PRESSURE-02.**  Subscribers that do not drain fast enough have their push
queue evicted from the ring. They detect the gap via `-33007 replay_gap` on
the next `events.subscribe`.

---

## §13  Handshake

### 13.1  Protocol Flow

**HANDSHAKE-01.**  The handshake is bidirectional and uses events in the
`handshake_wait` state (**LIFECYCLE-02**). The plugin sends `ready` first; the
registry validates and replies with `host_info`.

### 13.2  Plugin → Registry (`ready`)

**HANDSHAKE-02.**  The plugin MUST send a `ready` event as its first frame
after entering `handshake_wait`. The `ready` event MUST use `sequence: 1`.

**HANDSHAKE-03.**  The `ready` event `data` field MUST contain:

| Field | Type | Required | Description |
|---|---|---|---|
| `manifest` | object | MUST | The plugin's authoritative copy of the parsed manifest, including the full `dust` block |
| `protocol_version` | string | MUST | The plugin's protocol version as a semver string |
| `plugin_info` | object | MUST | Process metadata: `pid` (integer), `started_at` (ISO 8601 string) |

**HANDSHAKE-04.**  The `manifest.dust` object MUST include every required
field: `binary` and `protocol_version`. The registry MUST validate that the
declared manifest matches its own parsed copy from `plugin.json` and MUST
reject mismatches as `-32602 invalid_params`.

#### Wire Trace Example

Compact JSON (no whitespace):
```json
{"kind":"event","id":"evt_a1b2c3d4e5f67890","type":"ready","ts":"2026-04-12T09:30:00.123Z","sequence":1,"data":{"manifest":{"name":"tracker","version":"0.1.0","dust":{"binary":"bin/tracker-dust","protocol_version":"1.0.0","capabilities":["widget","command"],"restart":"on_failure","heartbeat_interval_ms":10000,"shutdown_drain_ms":2000,"spawn_timeout_ms":5000,"log_redact":[]}},"protocol_version":"1.0.0","plugin_info":{"pid":12345,"started_at":"2026-04-12T09:29:59.000Z"}}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 01 DA` (474 in decimal) |
| Payload | 474 bytes UTF-8 |
| **Total frame** | **478 bytes** |

### 13.3  Registry → Plugin (`host_info`)

**HANDSHAKE-05.**  After validating the `ready` event, the registry MUST send a
`host_info` event. The `host_info` event `data` field MUST contain:

| Field | Type | Required | Description |
|---|---|---|---|
| `host_name` | string | MUST | Registry identifier (e.g., `"dust"`) |
| `host_version` | string | MUST | Registry software version |
| `protocol_version_supported` | object | MUST | `{min: "<semver>", max: "<semver>"}` |
| `consumer_count` | integer | MUST | Current number of connected consumers |

**HANDSHAKE-06.**  `host_info` is a registry-originated event. It MUST NOT
carry a `sequence` field (see **REPLAY-12** — registry events are
unsequenced).

#### Wire Trace Example

```json
{"kind":"event","id":"evt_b2c3d4e5f6789012","type":"host_info","ts":"2026-04-12T09:30:00.125Z","data":{"host_name":"dust","host_version":"0.1.0","protocol_version_supported":{"min":"1.0.0","max":"1.999.999"},"consumer_count":1}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 E4` (228 in decimal) |
| Payload | 228 bytes UTF-8 |
| **Total frame** | **232 bytes** |

### 13.4  Post-Handshake Transition

**HANDSHAKE-07.**  After the registry sends `host_info`, the plugin transitions
to `active` on the registry side and MUST respond to subsequent requests
normally.

### 13.5  Refresh Event Behavior (Event-Capable Plugins)

**HANDSHAKE-08.**  The `refresh` event (registry → plugin, see §14.1) signals
that the registry wants the plugin to re-acquire its domain state. For plugins
that implement `events.subscribe` (event-capable plugins), handling the
`refresh` event SHOULD follow this pattern:

1. Re-query the underlying data source for current state.
2. Emit a `data_updated` event carrying a full snapshot of that state (see
   §14.6 for the recommended payload shape). The event MUST be assigned the
   next plugin-owned `sequence` (**REPLAY-11**) and MUST be appended to the
   ring buffer.
3. Push the same event to every active subscriber on the live push stream
   (**REPLAY-06**).

This behavior lets consumers force a full refresh on demand — a generic viewer
or a subscriber that detects drift can send `refresh` and expect the next
`data_updated` event on its connection to carry current state.

This is SHOULD (not MUST) so plugins that only re-render visually (no data
source to re-query) remain conformant. Non-event-capable plugins MAY ignore
`refresh` beyond re-rendering. Plugins that follow this convention are
interactively consistent.

---

## §14  Event Vocabulary

### 14.1  Normative Payload Table

**VOCAB-01.**  The following table defines all nine wire event types in v1,
their direction, normative payload shape, sequencing, and logging behavior.
Every row is a testable assertion.

| Type | Direction | Payload | Sequenced? | Logged? |
|---|---|---|---|---|
| `ready` | plugin → registry | `{manifest, protocol_version, plugin_info}` | yes (`sequence = 1`) | yes |
| `host_info` | registry → plugin | `{host_name, host_version, protocol_version_supported, consumer_count}` | no | yes |
| `status_changed` | plugin → registry | `{status: "ok"\|"degraded"\|"error", detail?: string}` | yes | yes |
| `progress` | plugin → registry | `{op_id: string, percent: 0–100, message?: string}` | yes | yes (subject to redaction) |
| `log` | plugin → registry | `{level: "debug"\|"info"\|"warn"\|"error", message: string, fields?: {...}}` | yes | yes (subject to redaction) |
| `error` | plugin → registry | `{plugin_code: string\|int, message: string, fatal: bool, data?: {...}}` | yes | yes |
| `data_updated` | plugin → registry | `{resource: string, version: string\|int}` | yes | yes |
| `refresh` | registry → plugin | `{reason?: string}` | no | yes |
| `visibility_changed` | registry → plugin | `{visible: bool}` | no | yes |

### 14.2  Exclusions

**VOCAB-02.**  `log_overflow` is NOT a wire event. It is a writer meta record
(see **OBSERVE-05**) written directly into `events.jsonl` by the observability
writer when the bounded channel overflows or disk writes fail. It is not
pushed over any socket, not subject to redaction. Consumers observe it by
tailing the log file.

**VOCAB-03.**  `cancel` is NOT an event. It is a request method (see
**ENVELOPE-02** for the request envelope shape and §15 for the cancellation
contract).

### 14.3  Error Event Codes

**VOCAB-04.**  `error` events carry plugin-specific codes in the `plugin_code`
field. Unlike response errors (which use the closed `-327xx`/`-330xx` registry
in §4), `error` events surface plugin-internal errors. The `plugin_code` field
is free-form; the registry surfaces it to consumers as an opaque identifier.

### 14.4  Scheduler Capability

**VOCAB-05.**  `Capability::Scheduler` is **reserved/advisory** in v1. Plugins
MAY declare `"scheduler"` in their `dust.capabilities` array. The registry
treats it as an advisory tag but there is no protocol-level dispatch for
scheduled jobs. Plugins that need scheduled execution MUST continue to use the
nanika scheduler CLI. A dispatch mechanism for `scheduler`-capability plugins
is reserved for v1.1+.

### 14.5  Unknown Event Types

**VOCAB-06.**  An event whose `type` is not in the table above MUST be logged
at level `warn` with diagnostic identifier `event_type_unknown` and MUST be
dropped. Unknown event types MUST NOT cause state transitions.


### 14.6  `data_updated` Envelope Convention

**VOCAB-07.**  The `data` field of a `data_updated` event (§14.1) carries a
plugin-defined payload. To support generic consumers (event viewers,
cross-language subscribers, dashboards), plugins SHOULD use the following
envelope convention:

| Field | Type | Required | Description |
|---|---|---|---|
| `source` | string | SHOULD | Origin of the update — an action method name (e.g., `"create"`, `"update"`) or the literal string `"refresh"` when the event was produced by the §13.5 refresh flow |
| `records` | array | SHOULD | Zero or more domain objects reflecting the post-update state. For action-triggered events, the array typically contains the single mutated record. For refresh/startup snapshots, the array contains the full current state |

Example (action mutation):
```json
{"source":"create","records":[{"id":"TRK-1","title":"Example","status":"open"}]}
```

Example (refresh snapshot):
```json
{"source":"refresh","records":[{"id":"TRK-1","title":"Example","status":"open"},{"id":"TRK-2","title":"Another","status":"closed"}]}
```

Plugins that follow this convention are generically consumable: a viewer can
decode every `data_updated` event without plugin-specific schema knowledge.

This is SHOULD (not MUST) so existing plugins (notably `tracker`) that emit
ad-hoc shapes — a bare domain object from action mutations, a custom wrapper
from refresh — remain conformant. New plugins SHOULD adopt the convention;
existing plugins MAY migrate on a compatibility break. Generic consumers that
cannot parse a `data_updated` payload against this schema MUST fall back to
treating `data` as opaque and MUST NOT raise a protocol error.

---

## §15  Cancellation Contract

### 15.1  Cancel as Request Method

**CANCEL-01.**  `cancel` is a **request method**, not an event. The registry
sends cancel to a plugin using the standard request envelope (**ENVELOPE-02**)
with `method: "cancel"`. Because cancel requires an acknowledgment, it MUST
be sent as a `request`, not as an `event`.

### 15.2  Request Shape

**CANCEL-02.**  The `cancel` request `params` MUST contain:

| Field | Type | Required | Description |
|---|---|---|---|
| `op_id` | string | MUST | The `op_id` from the original operation's `params` |

#### Wire Trace Example (request)

Compact JSON (no whitespace):
```json
{"kind":"request","id":"req_a1b2c3d4e5f67890","method":"cancel","params":{"op_id":"op_deadbeef12345678"}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 69` (105 in decimal) |
| Payload | 105 bytes UTF-8 |
| **Total frame** | **109 bytes** |

### 15.3  Response Shape

**CANCEL-03.**  The `cancel` response `result` MUST contain:

| Field | Type | Required | Description |
|---|---|---|---|
| `ok` | boolean | MUST | Always `true` — cancel was received |
| `already_complete` | boolean | MUST | `true` if the operation completed before cancel arrived |

#### Wire Trace Example (response — cancel accepted)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","result":{"ok":true,"already_complete":false}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 5D` (93 in decimal) |
| Payload | 93 bytes UTF-8 |
| **Total frame** | **97 bytes** |

### 15.4  Semantics

**CANCEL-04.**  Cancel is **best-effort**. The plugin MAY ignore the cancel
request (e.g., past the point of no return) but MUST still send a response.

**CANCEL-05.**  If the operation had already completed before the cancel
request arrived, the cancel response MUST carry `already_complete: true`.
The operation's own response is unaffected.

**CANCEL-06.**  If the cancel takes effect, the canceled operation's eventual
response MUST carry `error: {code: -33002, message: "canceled"}` (see
**ERROR-01**). The cancel response itself carries
`{ok: true, already_complete: false}`.

---

## §16  Hot-Plug Identity, Watcher, and Manifest Schema

### 16.1  Watch Target

**HOTPLUG-01.**  The registry MUST watch the path:

```
~/nanika/plugins/*/plugin.json
```

where `~` expands to the home directory of the user running the registry
process. Every file matching this glob is a candidate manifest.

### 16.2  Watcher Rules

**HOTPLUG-02.**  Filesystem events MUST be debounced by **200 ms** and
deduplicated by canonical path (symlinks resolved to their target). Multiple
events for the same canonical path within the debounce window MUST be
collapsed into a single manifest parse attempt.

**HOTPLUG-03.**  Plugins MUST write manifest changes via atomic rename:
write `plugin.json.tmp`, then rename to `plugin.json`. An in-place
truncate-and-write is observable as a partial file and MUST be logged as
`manifest_parse_failure`. The registry MUST retry the parse on the next
filesystem event; the plugin is not spawned until a complete parse succeeds.

### 16.3  Identity Validation

**HOTPLUG-04.**  The plugin ID MUST match the `name` field in `plugin.json`
AND MUST conform to the plugin ID grammar (**TRANSPORT-15**). If the `name`
field does not match the grammar, the registry MUST reject the manifest at
parse time and log `manifest_validation_failed`.

**HOTPLUG-05.**  If the `name` field changes inside an existing
`plugin.json`, the registry MUST perform a graceful teardown of the old
plugin identity (§8) and spawn a new identity from the updated manifest.

### 16.4  Binary Path Validation

**HOTPLUG-06.**  The `dust.binary` field MUST be resolved relative to the
plugin directory (the directory containing `plugin.json`). The path MUST NOT
contain `..` segments and MUST NOT resolve to a location outside the plugin
directory.

**HOTPLUG-07.**  While the plugin is in the `active` or `draining` state,
the registry MUST poll the resolved binary path every **5 s** for existence
and the executable bit.

**HOTPLUG-08.**  If the binary is deleted or loses its executable bit while
the plugin is `active` or `draining`, the registry MUST send
`{kind: "shutdown", reason: "binary_deleted"}` and transition to `draining`
(§8).

### 16.5  Instance Collision

**HOTPLUG-09.**  The registry MUST refuse to spawn a second instance of a
plugin whose plugin ID already has an `active` connection handle. Only one
instance per plugin ID is permitted at any time.

**HOTPLUG-10.**  On spawn, if a socket file already exists at the plugin's
path, the registry MUST attempt to `connect()` to it. If `connect()` succeeds,
another instance is live — spawn MUST be aborted. If `connect()` fails, the
registry MUST `unlink()` the stale socket and proceed with the spawn.

### 16.6  `dust` Manifest Block Schema

**HOTPLUG-11.**  The `dust` block inside `plugin.json` MUST conform to the
following schema. Required fields MUST be present; optional fields default to
the listed values when absent.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `binary` | string | MUST | — | Path to plugin binary, relative to plugin directory |
| `args` | array of strings | MAY | `[]` | Extra arguments passed to the binary on spawn (see **HOTPLUG-11a**) |
| `protocol_version` | string | MUST | — | Semver string (e.g., `"1.0.0"`) |
| `capabilities` | array of strings | MAY | `[]` | Subset of recognized capability names |
| `restart` | string | MAY | `"on_failure"` | One of: `"never"`, `"on_failure"`, `"always"` |
| `heartbeat_interval_ms` | integer | MAY | `10000` | Heartbeat interval in milliseconds |
| `shutdown_drain_ms` | integer | MAY | `2000` | Drain deadline in milliseconds |
| `spawn_timeout_ms` | integer | MAY | `5000` | Time from process spawn to socket appearance; overrides the 5 s default in **LIFECYCLE-02** |
| `log_redact` | array of strings | MAY | `[]` | JSONPath subset strings for redaction (§11) |

**HOTPLUG-11a.**  When `dust.args` is present, the registry MUST pass the
listed strings as positional arguments to the spawned binary, in order, after
the binary path. This enables multi-command CLIs to expose dust-serve as a
subcommand without a separate wrapper binary:

```json
{
  "dust": {
    "binary": "tracker",
    "args": ["dust-serve"],
    "protocol_version": "1.0.0"
  }
}
```

The registry invokes `tracker dust-serve` instead of `tracker` alone. The
`args` field MUST NOT be used to pass environment variables or shell
metacharacters; each element is treated as a literal argument string.

**HOTPLUG-12.**  The `restart` field controls respawn behavior on non-clean
exit. When set to `"on_failure"`, the registry MUST respawn the plugin up to
**3 times** with exponential backoff: **1 s → 2 s → 4 s** (capped at 8 s).
When set to `"never"`, the registry MUST NOT respawn. When set to `"always"`,
the registry MUST respawn on any exit, including clean exit, using the same
backoff schedule.

### 16.7  Field Validation

**HOTPLUG-13.**  The registry MUST validate manifest fields against the
following constraints:

| Field | Constraint |
|---|---|
| `binary` | Resolves to an executable within the plugin directory; no `..`, no absolute paths outside the plugin dir |
| `protocol_version` | Valid semver string |
| `capabilities` | Subset of `{"widget", "command", "scheduler"}`. `scheduler` is reserved/advisory (see **VOCAB-05**) |
| `heartbeat_interval_ms` | ≥ 1,000 ms, ≤ 300,000 ms |
| `shutdown_drain_ms` | ≥ 100 ms, ≤ 60,000 ms |
| `spawn_timeout_ms` | ≥ 1,000 ms, ≤ 60,000 ms |

**HOTPLUG-14.**  A manifest that fails any validation check MUST be logged at
parse time with diagnostic identifier `manifest_validation_failed` and the
plugin MUST NOT be spawned.

### 16.8  `refresh_manifest` Method

**HOTPLUG-15.**  The registry MAY call the `refresh_manifest` request method
when `plugin.json` is modified in place (not replaced via atomic rename). The
plugin MUST re-read its own manifest and return it in the response.

**HOTPLUG-16.**  The response `result` MUST contain:

| Field | Type | Required | Description |
|---|---|---|---|
| `manifest` | object | MUST | Updated manifest, same shape as `ready.data.manifest` (§13) |

The registry MUST validate the returned manifest, swap its cached copy, and
proceed. In-flight requests MUST complete against the old manifest; new
requests MUST use the new manifest.

#### Wire Trace Example (request)

```json
{"kind":"request","id":"req_a1b2c3d4e5f67890","method":"refresh_manifest"}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 00 4A` (74 in decimal) |
| Payload | 74 bytes UTF-8 |
| **Total frame** | **78 bytes** |

#### Wire Trace Example (response)

```json
{"kind":"response","id":"req_a1b2c3d4e5f67890","result":{"manifest":{"name":"tracker","version":"0.1.0","dust":{"binary":"bin/tracker-dust","protocol_version":"1.0.0","capabilities":["widget","command"],"restart":"on_failure","heartbeat_interval_ms":10000,"shutdown_drain_ms":2000,"spawn_timeout_ms":5000,"log_redact":[]}}}}
```

| Component | Bytes |
|---|---|
| Length prefix | `00 00 01 44` (324 in decimal) |
| Payload | 324 bytes UTF-8 |
| **Total frame** | **328 bytes** |

---

## §17  Threat Model

### 17.1  v1 Scope

**THREAT-01.**  The v1 threat model defends against: **other local users**,
**malformed or buggy peer processes**, and **stale-socket spoofing**. The v1
threat model does NOT defend against malicious code running under the host
user's own uid.

### 17.2  Threat Mitigation Table

**THREAT-02.**  The following table maps each identified threat to its
mitigation. Each mitigation cross-references the normative requirement that
implements it.

| Threat | Mitigation |
|---|---|
| Other local user reads/writes plugin socket | Runtime directory `0700`, socket `0600`, peer credential check rejects foreign uids (**TRANSPORT-04**, **TRANSPORT-05**, **TRANSPORT-07**) |
| Oversized frame DoS against registry | 1 MiB payload cap (**FRAME-03**); oversized → close + `dead` |
| Event flood DoS against observability writer | Bounded channel + drop-oldest (**OBSERVE-03**, **OBSERVE-04**); `log_overflow` meta record surfaces the condition (**OBSERVE-05**) |
| Invalid JSON mid-stream | Connection closes on parse failure (**FRAME-09**); no state corruption; peer may reconnect |
| Stale socket reused by different binary | Canonical path resolution (**HOTPLUG-02**), stale-socket cleanup on startup (**TRANSPORT-13**), collision detection at spawn (**HOTPLUG-10**) |
| Slowloris byte-drip attack | Per-frame read deadline 500 ms (**FRAME-13**), per-frame write deadline 1 s (**FRAME-14**) |
| Log exposure via cloud backup or home sync | Log mode `0600`, directory `0700` (**OBSERVE-15**); documented recommendation to exclude `~/.alluka/dust/` from home-sync |
| Plugin leaks user data into observability log | `log_redact` policy (**OBSERVE-10**); per-message opt-out (**OBSERVE-14**) |
| Subscriber calls mutating methods | Role enforcement — mutating methods from a subscriber return `-33004 unauthorized` (**CONSUMER-03**) |
| Second registry connection hijacks plugin | First-connection-wins rule (**CONSUMER-04**); second connection treated as subscriber |

### 17.3  Explicitly Out of Scope

**THREAT-03.**  Malicious code running as the same uid is explicitly out of
scope. Such an adversary already has sufficient privilege to `ptrace` the
legitimate plugin, read its memory, replace its binary, or bind the plugin's
socket directly. Nonce handshakes or binary-hash verification would provide
theater, not security. Defense against same-uid code execution requires
OS-level sandboxing (AppArmor, SELinux, `sandbox_exec`), not a user-space
protocol.

**THREAT-04.**  Network adversaries are out of scope. Unix domain sockets are
local only. Cross-machine remoting is reserved for v2+.

**THREAT-05.**  Supply-chain attacks against the plugin binary are out of
scope. Manifest signing is reserved for v2+.

---

## §18  Schema Evolution Policy

### 18.1  Change Classification

**EVOLUTION-01.**  The following table classifies protocol changes by their
semver impact. Each row is normative.

| Change | Semver bump | Handling |
|---|---|---|
| Bug fix, no wire change | patch | — |
| Add optional envelope field | minor | Older peers silently ignore |
| Add event type | minor | Older peers log `event_type_unknown` + drop (**VOCAB-06**) |
| Add method | minor | Older peers respond `-32601 method_not_found` |
| Add capability name | minor | Older peers ignore the capability |
| **Add error code to registry** | **minor** | Older peers treat as opaque error (consistent with **ERROR-04**) |
| Add required envelope field | major | Breaks older peers |
| Remove or rename method, type, or field | major | — |
| Remove or renumber error code | major | — |
| Change state machine | major | — |

### 18.2  Error Code Evolution

**EVOLUTION-02.**  Adding an error code to the registry (§4) is explicitly a
**minor** version bump. Older peers that receive an unknown code MUST treat it
as an opaque error and MUST log the unknown code. They MUST NOT close the
connection solely because the code is unrecognized.

### 18.3  Unknown-Field Tolerance

**EVOLUTION-03.**  Unknown-field tolerance is the core of schema evolution.
Producers add fields freely; consumers MUST ignore unknown fields. Breakage
surfaces only on required-field additions, method signature changes, and state
machine changes. This principle is normatively defined in **VERSION-06**.

---

## §19  Language Stubs

This section provides a minimal handshake example in each of the four
reference languages. Each example demonstrates the core protocol operation:
bind a Unix domain socket, accept the registry connection, send a
length-prefixed `ready` event, and read the length-prefixed `host_info`
response. Full reference implementations are produced in Phase F.

**STUB-00 Conformance scope.**  The four language stubs (Rust SDK, Go, Python,
Bash) are validated against the **handshake / methods / heartbeat / shutdown**
sections of `dust-conform` only. They are NOT expected to pass the `replay`
section: `events.subscribe` requires a retained event ring (§10) which is out
of scope for a minimal handshake stub. A stub responding with `-32601 method
not found` to `events.subscribe` is conformant for §19 purposes. Full plugins
(e.g., `dust-fixture-minimal`, `hello`, `tracker`) MUST pass all five sections
including `replay`.

### 19.1  Rust

**STUB-01.**  A Rust plugin uses `std::os::unix::net::UnixListener` for the
socket and manual `u32::to_be_bytes()` / `u32::from_be_bytes()` for framing.
The `dust-sdk` crate wraps this into a `DustPlugin` trait with automatic
handshake, heartbeat echo, and graceful shutdown. A minimal plugin is
approximately 20 lines:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use dust_sdk::{run, ActionParams, ActionResult, Component, DustPlugin, PluginManifest};

struct MyPlugin;

#[async_trait]
impl DustPlugin for MyPlugin {
    fn plugin_id(&self) -> &str { "my-plugin" }
    async fn manifest(&self) -> PluginManifest { /* ... */ }
    async fn render(&self) -> Vec<Component> { vec![] }
    async fn action(&self, _: ActionParams) -> ActionResult { ActionResult::ok() }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    run(Arc::new(MyPlugin)).await
}
```

**STUB-01a.**  Four optional `DustPlugin` methods have default implementations
that provide safe stub behaviour. Override them to participate in the
corresponding protocol flows:

| Method | Default behaviour | Wire method |
|--------|-------------------|-------------|
| `subscribe(since_sequence)` | `-33007 replay_gap` | `events.subscribe` |
| `unsubscribe(subscription_id)` | `Ok(())` | `events.unsubscribe` |
| `cancel(op_id)` | `already_complete: true` | `cancel` |
| `refresh(reason)` | `Ok(())` | `refresh` |

Example override — serve replay from an in-memory ring:

```rust
async fn subscribe(
    &self,
    since_sequence: u64,
) -> Result<Vec<EventEnvelope>, SubscribeError> {
    let ring = self.events.lock().unwrap();
    ring.subscribe(since_sequence).map_err(|gap| SubscribeError {
        oldest_available: gap.oldest_available,
        requested: gap.requested,
    })
}
```

The raw handshake without the SDK is approximately 25 lines:

```rust
use std::os::unix::net::UnixListener;
use std::io::{Read, Write};

fn send_frame(s: &mut impl Write, payload: &[u8]) {
    s.write_all(&(payload.len() as u32).to_be_bytes()).unwrap();
    s.write_all(payload).unwrap();
}

fn recv_frame(s: &mut impl Read) -> Vec<u8> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).unwrap();
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut buf).unwrap();
    buf
}

fn main() {
    let ln = UnixListener::bind(&socket_path).unwrap();
    let (mut conn, _) = ln.accept().unwrap();
    send_frame(&mut conn, ready_json.as_bytes());  // ready event
    let host_info = recv_frame(&mut conn);          // host_info event
    // -> active: heartbeat loop + request handling
}
```

### 19.2  Go

**STUB-02.**  A Go plugin uses `net.Listen("unix", path)` and
`encoding/binary` for the 4-byte big-endian length prefix. The entire
handshake fits in ~20 lines with `encoding/json` for payload serialization:

```go
ln, _ := net.Listen("unix", socketPath)
conn, _ := ln.Accept()

// Send ready event
ready, _ := json.Marshal(readyPayload)
binary.Write(conn, binary.BigEndian, uint32(len(ready)))
conn.Write(ready)

// Read host_info event
var length uint32
binary.Read(conn, binary.BigEndian, &length)
buf := make([]byte, length)
io.ReadFull(conn, buf)
var hostInfo map[string]any
json.Unmarshal(buf, &hostInfo)
// -> active: heartbeat ticker + request dispatch
```

### 19.3  Python

**STUB-03.**  A Python plugin uses `asyncio.start_unix_server` and
`struct.pack(">I", n)` for the length prefix. The async model maps naturally
to the heartbeat timer and concurrent request handling:

```python
import asyncio, json, struct

async def handle(reader, writer):
    # Send ready event
    ready = json.dumps(ready_payload).encode()
    writer.write(struct.pack(">I", len(ready)) + ready)
    await writer.drain()

    # Read host_info event
    length = struct.unpack(">I", await reader.readexactly(4))[0]
    host_info = json.loads(await reader.readexactly(length))
    # -> active: heartbeat task + request loop

asyncio.run(asyncio.start_unix_server(handle, path=socket_path))
```

### 19.4  Bash

**STUB-04.**  A Bash plugin uses `socat` for the Unix socket listener and
`jq` for JSON construction. Binary framing requires `printf` with hex escapes
for the 4-byte length prefix. This stub proves the protocol is implementable
without structured client libraries, though production plugins should use a
compiled language:

```bash
#!/usr/bin/env bash
set -euo pipefail
SOCK="${XDG_RUNTIME_DIR}/nanika/plugins/my-plugin.sock"

send_frame() {
  local p="$1" len=${#1}
  printf "\\x%02x\\x%02x\\x%02x\\x%02x" \
    $(( (len>>24)&0xFF )) $(( (len>>16)&0xFF )) \
    $(( (len>>8)&0xFF ))  $(( len&0xFF ))
  printf '%s' "$p"
}

READY=$(jq -nc '{kind:"event",id:"evt_0000000000000001",
  type:"ready",ts:(now|todate),sequence:1,
  data:{manifest:{name:"my-plugin",version:"0.1.0",
  dust:{binary:"bin/my-plugin",protocol_version:"1.0.0"}},
  protocol_version:"1.0.0",
  plugin_info:{pid:'$$',started_at:(now|todate)}}}')

# Listen, send ready, read host_info, enter heartbeat loop
socat UNIX-LISTEN:"$SOCK",unlink-early - <<< "$(send_frame "$READY")"
```

---

## Appendix A  Conformance Assertion Index

Every normative requirement in this specification maps to one or more named
conformance assertions below. A conformant implementation MUST pass every
assertion. Assertion names follow the pattern `section.specific_behavior` for
use in the Phase E conformance harness (`dust-conform`).

Requirements that use only SHOULD, MAY, or are purely informative are omitted.

### A.1  §1 Transport

| Req ID | Assertion | MUST Clause |
|---|---|---|
| TRANSPORT-01 | `transport.socket_path_xdg` | MUST use `$XDG_RUNTIME_DIR/nanika/plugins/<id>.sock` when set |
| TRANSPORT-02 | `transport.socket_path_fallback` | MUST use `~/.alluka/run/plugins/<id>.sock` when `$XDG_RUNTIME_DIR` unset |
| TRANSPORT-03 | `transport.no_shared_namespace` | MUST NOT place sockets under `/tmp`, `/var/run`, or shared paths |
| TRANSPORT-04 | `transport.runtime_dir_mode_0700` | MUST create runtime directory with mode `0700` |
| TRANSPORT-05 | `transport.socket_mode_0600` | MUST create socket file with mode `0600` |
| TRANSPORT-06 | `transport.ownership_matches_uid` | Runtime directory and socket MUST be owned by host uid |
| TRANSPORT-07 | `transport.peer_credential_check` | MUST verify peer uid via platform mechanism |
| TRANSPORT-08 | `transport.foreign_uid_rejected` | MUST close immediately; MUST log rejection with mismatched uid |
| TRANSPORT-09 | `transport.unsupported_os_refuses_start` | MUST refuse to start on OS without peer credential mechanism |
| TRANSPORT-12 | `transport.no_same_uid_defense_claim` | MUST NOT claim defense against same-uid adversary |
| TRANSPORT-13 | `transport.stale_socket_cleanup` | MUST remove absent-plugin sockets; MUST NOT remove live-plugin sockets |
| TRANSPORT-14 | `transport.existing_socket_connect_test` | MUST `connect()` existing socket; MUST exit if live; MUST `unlink()` if stale |
| TRANSPORT-15 | `transport.plugin_id_grammar` | MUST match `^[a-z][a-z0-9_-]{1,63}$` |
| TRANSPORT-16 | `transport.invalid_id_rejected` | MUST reject at parse time; MUST NOT create socket |

### A.2  §2 Framing

| Req ID | Assertion | MUST Clause |
|---|---|---|
| FRAME-01 | `framing.length_prefixed_json` | Frame MUST be 4-byte BE length + UTF-8 JSON payload |
| FRAME-02 | `framing.length_is_payload_bytes` | Length MUST be exact UTF-8 byte count of payload |
| FRAME-03 | `framing.oversized_payload_closes_connection` | Length > 1 MiB MUST close connection and transition to `dead` |
| FRAME-04 | `framing.zero_length_no_heartbeat_reset` | Zero-length frame MUST NOT reset heartbeat miss counter |
| FRAME-05 | `framing.oversized_logs_frame_oversized` | MUST log `frame_oversized` with received length |
| FRAME-06 | `framing.eof_length_clean_disconnect` | EOF during length prefix treated as clean disconnect |
| FRAME-07 | `framing.eof_payload_logs_truncated` | MUST log `frame_truncated` with received vs expected bytes |
| FRAME-08 | `framing.invalid_utf8_closes` | MUST close and log `frame_utf8_error` |
| FRAME-09 | `framing.invalid_json_closes_no_correlate` | MUST close; MUST NOT attempt `id` extraction |
| FRAME-10 | `framing.non_object_json_closes` | Non-object top-level JSON MUST close connection |
| FRAME-11 | `framing.missing_kind_closes` | Missing `kind` field MUST close connection |
| FRAME-12 | `framing.unknown_kind_closes` | Unknown `kind` value MUST close connection |
| FRAME-13 | `framing.partial_read_500ms_deadline` | MUST close if payload not received within 500 ms |
| FRAME-14 | `framing.partial_write_1s_deadline` | MUST close if frame not flushed within 1 s |
| FRAME-15 | `framing.slowloris_detected_by_read_deadline` | Slowloris attack detected by **FRAME-13** read deadline |
| FRAME-16 | `framing.malformed_no_id_extraction` | MUST NOT extract `id` from broken payload |

### A.3  §3 Envelope

| Req ID | Assertion | MUST Clause |
|---|---|---|
| ENVELOPE-01 | `envelope.kind_field_required` | Payload MUST contain `kind` string; exactly five kinds in v1 |
| ENVELOPE-02 | `envelope.request_shape` | Request MUST have `kind`, `id`, `method` |
| ENVELOPE-03 | `envelope.response_shape` | Response MUST have `kind`, `id` |
| ENVELOPE-04 | `envelope.result_error_mutually_exclusive` | Exactly one of `result`/`error` MUST be present |
| ENVELOPE-05 | `envelope.error_has_code_and_message` | Error MUST have `code` (integer) and `message` (string) |
| ENVELOPE-06 | `envelope.event_shape` | Event MUST have `kind`, `id`, `type`, `ts`, `data` |
| ENVELOPE-07 | `envelope.event_type_vocabulary` | Event `type` MUST be one of nine defined types |
| ENVELOPE-08 | `envelope.heartbeat_shape` | Heartbeat MUST have `kind` and `ts` |
| ENVELOPE-09 | `envelope.heartbeat_no_id_no_log` | Heartbeats MUST NOT carry `id`; MUST NOT be logged |
| ENVELOPE-10 | `envelope.shutdown_shape` | Shutdown MUST have `kind` and `reason` |
| ENVELOPE-11 | `envelope.shutdown_registry_only` | Plugin MUST NOT send shutdown; registry MUST close on receipt from plugin |
| ENVELOPE-12 | `envelope.shutdown_reason_vocabulary` | Reason MUST be one of eight defined values |
| ENVELOPE-13 | `envelope.id_unique_per_connection_direction` | IDs MUST be unique per (connection, direction) |
| ENVELOPE-14 | `envelope.id_format_hex` | Request `req_<16hex>`, event `evt_<16hex>` |
| ENVELOPE-15 | `envelope.duplicate_id_returns_32600` | Duplicate in-flight ID MUST get `-32600`; method MUST NOT invoke |
| ENVELOPE-16 | `envelope.response_order_not_assumed` | MUST NOT assume FIFO response ordering |
| ENVELOPE-17 | `envelope.late_response_dropped` | MUST drop late response; MUST log `late_response` |
| ENVELOPE-18 | `envelope.draining_response_forwarded` | In-flight response MUST be accepted during drain |
| ENVELOPE-19 | `envelope.dead_responses_dropped` | All responses MUST be dropped after `dead` |

### A.4  §4 Error Code Registry

| Req ID | Assertion | MUST Clause |
|---|---|---|
| ERROR-01 | `error.closed_enum_v1` | `code` MUST be from the registry table; MUST NOT invent new codes |
| ERROR-02 | `error.plugin_code_in_data` | Plugin domain errors MUST use `error.data.plugin_code` |
| ERROR-03 | `error.data_shape_unconstrained` | Consumers MUST NOT rely on `error.data` shape beyond `plugin_code` |
| ERROR-04 | `error.add_code_is_minor` | Adding a code is a minor version bump |
| ERROR-05 | `error.remove_code_is_major` | Removing or renumbering a code is a major version bump |
| ERROR-06 | `error.no_invented_top_level_codes` | Unknown integer in `code` is a protocol violation |

### A.5  §5 Lifecycle State Machine

| Req ID | Assertion | MUST Clause |
|---|---|---|
| LIFECYCLE-01 | `lifecycle.exactly_six_states` | Exactly six states; `dead` is terminal |
| LIFECYCLE-02 | `lifecycle.transition_table_exhaustive` | Every transition per table row is testable |
| LIFECYCLE-03 | `lifecycle.handshake_version_check` | MUST validate `ready.data.protocol_version`; MUST shutdown on mismatch |
| LIFECYCLE-05 | `lifecycle.heartbeat_pause_on_op_id` | MUST pause heartbeat counter during active `op_id` progress |
| LIFECYCLE-06 | `lifecycle.host_death_plugin_cleanup` | Plugin MUST clean up socket on host death |
| LIFECYCLE-07 | `lifecycle.late_ready_dropped` | MUST drop `ready` after timeout; MUST log `late_ready` |
| LIFECYCLE-08 | `lifecycle.diagnostic_not_wire_reason` | Diagnostic reasons MUST NOT appear in shutdown envelope |

### A.6  §6 Consumer Topology

| Req ID | Assertion | MUST Clause |
|---|---|---|
| CONSUMER-02 | `consumer.subscriber_allowed_methods` | Subscribers limited to `manifest`, `render`, `events.subscribe`, `events.unsubscribe` |
| CONSUMER-03 | `consumer.subscriber_mutating_returns_33004` | MUST respond `-33004 unauthorized` to subscriber mutating calls |
| CONSUMER-04 | `consumer.first_connection_is_registry` | MUST track `accept()` order; first connection is registry |
| CONSUMER-05 | `consumer.no_promotion` | Subscriber MUST NOT become registry within same lifetime |
| CONSUMER-06 | `consumer.registry_drop_disconnects_subscribers` | Subscribers MUST be disconnected after registry drop + drain |
| CONSUMER-07 | `consumer.heartbeat_registry_only` | Heartbeat tracking applies to registry connection ONLY |

### A.7  §7 Heartbeat Rules

| Req ID | Assertion | MUST Clause |
|---|---|---|
| HEARTBEAT-01 | `heartbeat.both_sides_emit` | MUST emit one heartbeat per `heartbeat_interval_ms` |
| HEARTBEAT-02 | `heartbeat.three_miss_dead` | MUST consider peer dead after 3 missed heartbeats |
| HEARTBEAT-03 | `heartbeat.pause_during_drain` | MUST pause heartbeats during `draining` |
| HEARTBEAT-04 | `heartbeat.pause_on_op_id` | MUST pause counter for long-running `op_id` |
| HEARTBEAT-05 | `heartbeat.no_log_no_sequence` | MUST NOT log heartbeats to observability stream |

### A.8  §8 Shutdown Semantics

| Req ID | Assertion | MUST Clause |
|---|---|---|
| SHUTDOWN-01 | `shutdown.registry_only_initiates` | Plugin MUST exit its process instead of sending shutdown |
| SHUTDOWN-02 | `shutdown.reason_from_envelope_12` | Reason MUST be from **ENVELOPE-12** vocabulary |
| SHUTDOWN-03 | `shutdown.drain_within_deadline` | MUST drain within `shutdown_drain_ms` |
| SHUTDOWN-04 | `shutdown.reject_new_requests_33006` | MUST respond `-33006 shutting_down` to new requests during drain |
| SHUTDOWN-07 | `shutdown.sigkill_after_deadline` | MUST send SIGKILL after drain deadline |
| SHUTDOWN-08 | `shutdown.synthesize_canceled_responses` | MUST synthesize `-33002 canceled` for unanswered in-flight requests |

### A.9  §9 Version Negotiation

| Req ID | Assertion | MUST Clause |
|---|---|---|
| VERSION-01 | `version.advertise_in_ready` | MUST advertise semver in `ready.data.protocol_version` |
| VERSION-03 | `version.mismatch_sends_shutdown` | MUST send shutdown `version_mismatch` when outside range |
| VERSION-04 | `version.plugin_closes_on_incompatible` | Plugin MUST close on incompatible range |

### A.10  §10 Reconnect, Replay, and Sequence Ownership

| Req ID | Assertion | MUST Clause |
|---|---|---|
| REPLAY-02 | `replay.subscribe_snapshot_and_live` | MUST return snapshot AND establish live push |
| REPLAY-03 | `replay.subscribe_requires_since_sequence` | `params.since_sequence` MUST be present |
| REPLAY-04 | `replay.subscribe_result_shape` | `result` MUST have `subscription_id`, `events`, `next_sequence` |
| REPLAY-05 | `replay.since_sequence_gte_n` | `since_sequence: N` (N>0) MUST return events with `sequence >= N` |
| REPLAY-05 | `replay.since_sequence_zero_returns_all` | `since_sequence: 0` MUST return all retained events |
| REPLAY-05 | `replay.since_sequence_future_returns_empty` | `since_sequence > latest` MUST return empty `events` |
| REPLAY-05 | `replay.since_sequence_gap_returns_33007` | `since_sequence < oldest` MUST return `-33007 replay_gap` |
| REPLAY-06 | `replay.live_push_after_subscribe` | MUST push new events after successful subscribe |
| REPLAY-07 | `replay.unsubscribe_requires_subscription_id` | `params.subscription_id` MUST match prior subscribe |
| REPLAY-08 | `replay.one_subscription_per_connection` | Second subscribe MUST be rejected with `-33005 busy` |
| REPLAY-11 | `replay.plugin_sequence_monotonic_u64` | Plugin events MUST have monotonic `u64` `sequence` starting at 1 |
| REPLAY-13 | `replay.subscribers_no_originate` | Subscribers MUST NOT originate events |
| REPLAY-14 | `replay.sequence_decrease_is_restart` | MUST interpret sequence decrease as plugin restart |
| REPLAY-15 | `replay.ring_retention_min_1000_512kib` | MUST retain `min(1000 events, 512 KiB total serialized)` |
| REPLAY-16 | `replay.ring_evicts_oldest_on_byte_overflow` | MUST evict oldest when 512 KiB exceeded |
| REPLAY-18 | `replay.ring_empty_on_restart` | Ring MUST start empty on plugin restart; sequence MUST start at 1 |

### A.11  §11 Observability and Redaction

| Req ID | Assertion | MUST Clause |
|---|---|---|
| OBSERVE-01 | `observe.log_path_events_jsonl` | MUST write to `~/.alluka/dust/events.jsonl` |
| OBSERVE-02 | `observe.dispatch_does_not_block_on_io` | Protocol path MUST NOT block on disk I/O |
| OBSERVE-03 | `observe.channel_capacity_10000` | Bounded channel MUST have capacity 10,000 per plugin |
| OBSERVE-04 | `observe.overflow_drops_oldest` | MUST drop oldest on overflow; MUST increment `events_dropped` |
| OBSERVE-05 | `observe.log_overflow_meta_record_60s` | MUST write `log_overflow` at most once per 60 s; MUST NOT appear on socket |
| OBSERVE-06 | `observe.meta_kind_discriminator` | Meta records MUST use `kind: "meta"` |
| OBSERVE-07 | `observe.log_record_schema` | Non-meta lines MUST have `ts`, `plugin_id`, `direction`, `sequence`, `message` |
| OBSERVE-08 | `observe.rotation_100mb_7days` | MUST rotate at 100 MB or 7 days |
| OBSERVE-09 | `observe.heartbeat_filtered` | `heartbeat` MUST be filtered out; MUST NOT be logged |
| OBSERVE-10 | `observe.redaction_strips_matching_paths` | MUST strip paths from `log_redact` before writing |
| OBSERVE-12 | `observe.invalid_redact_path_logged_skipped` | Invalid paths MUST be logged; MUST be skipped; plugin MUST still load |
| OBSERVE-14 | `observe.per_message_redact_all` | `log_redact_all: true` MUST produce placeholder record |
| OBSERVE-15 | `observe.log_file_mode_0600` | Log file MUST be `0600`; directory MUST be `0700` |

### A.12  §12 Backpressure

| Req ID | Assertion | MUST Clause |
|---|---|---|
| PRESSURE-01 | `backpressure.inflight_request_limit_100` | 101st in-flight request per direction MUST get `-33005 busy` |
| PRESSURE-01 | `backpressure.subscriber_connection_limit_16` | 17th subscriber connection MUST be closed with `-33005 busy` |
| PRESSURE-01 | `backpressure.one_subscription_per_connection` | Second `events.subscribe` on same connection MUST get `-33005 busy` |
| PRESSURE-01 | `backpressure.second_registry_closes_33004` | Second registry connection MUST be closed with `-33004 unauthorized` |

### A.13  §13 Handshake

| Req ID | Assertion | MUST Clause |
|---|---|---|
| HANDSHAKE-01 | `handshake.events_in_handshake_wait` | Handshake uses events in `handshake_wait` state |
| HANDSHAKE-02 | `handshake.ready_first_frame_sequence_1` | MUST send `ready` as first frame with `sequence: 1` |
| HANDSHAKE-03 | `handshake.ready_data_has_manifest_version_info` | `ready.data` MUST have `manifest`, `protocol_version`, `plugin_info` |
| HANDSHAKE-04 | `handshake.manifest_dust_binary_version_required` | `manifest.dust` MUST include `binary` and `protocol_version`; registry MUST validate match |
| HANDSHAKE-05 | `handshake.host_info_data_shape` | `host_info.data` MUST have `host_name`, `host_version`, `protocol_version_supported`, `consumer_count` |
| HANDSHAKE-06 | `handshake.host_info_no_sequence` | `host_info` MUST NOT carry `sequence` |
| HANDSHAKE-07 | `handshake.post_handshake_active` | After `host_info`, plugin MUST respond to requests normally |

### A.14  §14 Event Vocabulary

| Req ID | Assertion | MUST Clause |
|---|---|---|
| VOCAB-02 | `vocabulary.log_overflow_not_wire_event` | `log_overflow` MUST NOT appear on any socket |
| VOCAB-05 | `vocabulary.scheduler_reserved_use_cli` | Plugins needing scheduling MUST use nanika scheduler CLI |
| VOCAB-06 | `vocabulary.unknown_type_logged_warn_dropped` | Unknown type MUST be logged `event_type_unknown`; MUST be dropped; MUST NOT cause state change |

### A.15  §15 Cancellation Contract

| Req ID | Assertion | MUST Clause |
|---|---|---|
| CANCEL-01 | `cancel.is_request_not_event` | MUST be sent as `request` kind, not `event` |
| CANCEL-02 | `cancel.params_has_op_id` | `params` MUST contain `op_id` |
| CANCEL-03 | `cancel.response_has_ok_and_already_complete` | `result` MUST contain `ok` (boolean) and `already_complete` (boolean) |
| CANCEL-04 | `cancel.must_respond_even_if_ignored` | Plugin MUST respond to cancel even if it ignores the cancellation |
| CANCEL-05 | `cancel.already_complete_when_op_finished` | MUST carry `already_complete: true` if operation already completed |
| CANCEL-06 | `cancel.canceled_op_returns_33002` | Canceled operation MUST respond with `-33002 canceled` |

### A.16  §16 Hot-Plug Identity, Watcher, and Manifest Schema

| Req ID | Assertion | MUST Clause |
|---|---|---|
| HOTPLUG-01 | `hotplug.watch_plugins_plugin_json` | MUST watch `~/nanika/plugins/*/plugin.json` |
| HOTPLUG-02 | `hotplug.debounce_200ms_dedup_canonical` | MUST debounce 200 ms; MUST dedup by canonical path |
| HOTPLUG-03 | `hotplug.atomic_rename_only` | In-place write MUST be logged `manifest_parse_failure`; MUST retry |
| HOTPLUG-04 | `hotplug.name_matches_id_grammar` | Plugin ID MUST match `name` field AND **TRANSPORT-15** grammar |
| HOTPLUG-05 | `hotplug.name_change_teardown_respawn` | MUST teardown old identity; MUST spawn new |
| HOTPLUG-06 | `hotplug.binary_relative_no_escape` | MUST NOT contain `..`; MUST NOT resolve outside plugin dir |
| HOTPLUG-07 | `hotplug.binary_poll_5s_active_draining` | MUST poll binary every 5 s while `active` or `draining` |
| HOTPLUG-08 | `hotplug.binary_deleted_sends_shutdown` | MUST send shutdown `binary_deleted` on deletion |
| HOTPLUG-09 | `hotplug.no_duplicate_active_instance` | MUST refuse second instance of same plugin ID |
| HOTPLUG-10 | `hotplug.stale_socket_connect_test` | MUST `connect()` existing socket; MUST abort if live; MUST `unlink()` if stale |
| HOTPLUG-11 | `hotplug.binary_and_protocol_version_required` | `binary` and `protocol_version` MUST be present |
| HOTPLUG-12 | `hotplug.restart_backoff_3_retries` | `on_failure` MUST respawn ≤ 3 times with 1s→2s→4s backoff |
| HOTPLUG-13 | `hotplug.capabilities_validated_subset` | MUST be subset of `{widget, command, scheduler}` |
| HOTPLUG-14 | `hotplug.invalid_manifest_not_spawned` | MUST log `manifest_validation_failed`; MUST NOT spawn |
| HOTPLUG-15 | `hotplug.refresh_manifest_reread_return` | Plugin MUST re-read manifest and return it |
| HOTPLUG-16 | `hotplug.refresh_inflight_old_new_requests_new` | In-flight MUST use old manifest; new requests MUST use new |

### A.17  §17 Threat Model

| Req ID | Assertion | MUST Clause |
|---|---|---|
| THREAT-03 | `threat.same_uid_out_of_scope` | Protocol MUST NOT claim defense against same-uid adversary |

### A.18  §18 Schema Evolution Policy

| Req ID | Assertion | MUST Clause |
|---|---|---|
| EVOLUTION-02 | `evolution.unknown_code_opaque_no_close` | MUST treat unknown code as opaque error; MUST NOT close connection |
| EVOLUTION-03 | `evolution.consumers_ignore_unknown_fields` | Consumers MUST ignore unknown fields |

### A.19  §19 Language Stubs

(No MUST clauses. This section contains illustrative examples, not normative
requirements. The stubs are validated by the Phase E conformance harness
during Phase F exit testing.)


---

## §20  Changelog

### v1.0.0 — 2026-04-13 (stable, frozen)

First stable release. Protocol frozen at 1.0.0 on 2026-04-13.

**Gaps closed during the 0.9.0-draft → 1.0.0 cycle:**

- **GAP-01 — Multi-command plugin binaries.** Added optional `dust.args` array
  to `plugin.json` so plugins that share a binary across the CLI and
  `dust-serve` modes (e.g., `tracker dust-serve`) can declare their spawn
  arguments. `dust-conform` now forwards these to the child process. See
  §16 and the tracker manifest for the canonical example.
- **GAP-02 — Optional `DustPlugin` trait methods.** `subscribe`,
  `unsubscribe`, `cancel`, and `refresh` now have safe default
  implementations on the Rust SDK's `DustPlugin` trait. A plugin that does
  not need replay, cancellation, or refresh no longer has to write the
  boilerplate; the defaults emit the spec-mandated responses
  (`-33007 replay_gap`, `Ok(())`, `already_complete: true`). See §19.1
  STUB-01a.
- **GAP-03 — `replay` conformance section.** `dust-conform` now includes
  a fifth section, `replay`, that exercises `events.subscribe` with
  `since_sequence: 0` and validates the response shape against REPLAY-04
  (`subscription_id`, `events`, `next_sequence`). The SDK's default
  `events.subscribe` dispatcher was also corrected to wrap trait-level
  `Ok(events)` returns into the REPLAY-04 result envelope instead of
  serializing the raw event array.
- **Conformance scope clarified (§19).** Added STUB-00 spelling out that the
  four language stubs are validated against handshake/methods/heartbeat/shutdown
  only; the replay section is reserved for full plugins with a retained event
  ring.

**Wire-level compatibility:** No breaking changes to any MUST clause from
0.9.0-draft. A plugin that passed the 0.9.0-draft conformance harness
continues to pass the handshake/methods/heartbeat/shutdown sections of the
1.0.0 harness unchanged.

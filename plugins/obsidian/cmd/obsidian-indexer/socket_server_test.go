package main

import "testing"

// T3.4 — §10.4 Phase 3
// Asserts: malformed JSON, unknown fields, and oversized queries sent over the
// Unix socket each produce a clean error response; the daemon remains stable
// after each bad request and continues serving subsequent valid requests.
func TestRecall_SocketProtocol(t *testing.T) {
	t.Skip("RED — T3.4 not yet implemented (blocks on TRK-528 Phase 3)")
}

// T3.9 — §10.4 Phase 3
// Asserts: 50 concurrent obsidian recall requests sent over the socket all
// return valid responses with no data races (race detector on) and the daemon
// RSS does not grow across the run.
func TestRecall_Concurrency(t *testing.T) {
	t.Skip("RED — T3.9 not yet implemented (blocks on TRK-528 Phase 3)")
}

// T3.4 (fuzz) — §10.4 Phase 3
// Fuzz target for the Unix-socket request parser: arbitrary byte sequences must
// not crash the daemon or cause it to emit a corrupt response.
func FuzzSocketRequest(f *testing.F) {
	f.Skip("RED — T3.4 (fuzz) not yet implemented (blocks on TRK-528 Phase 3)")
}

// T3.10 — §10.4 Phase 3
// Asserts: the inject-context socket command returns vault-sourced context
// blocks that are scoped to the active vault and respect the byte-budget
// parameter passed in the request.
func TestInjectContextVault(t *testing.T) {
	t.Skip("RED — T3.10 not yet implemented (blocks on TRK-528 Phase 3)")
}

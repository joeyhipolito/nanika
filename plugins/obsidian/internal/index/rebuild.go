// Package index implements RFC §7 Phase 0 (TRK-524): RebuildEmpty for new vault setup.
package index

import (
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
)

// RebuildEmpty initialises a fresh cache directory for an empty vault.
// It writes three files under cachePath:
//   - index.db  — empty SQLite index (schema applied via Open)
//   - graph.bin — zero-node zero-edge CSR with 4-byte "CSR1" magic header
//   - preflight.md — fallback content (≤ 1024 bytes, non-empty)
func RebuildEmpty(vaultPath, cachePath string) error {
	if err := os.MkdirAll(cachePath, 0755); err != nil {
		return fmt.Errorf("creating cache directory: %w", err)
	}

	// 1. index.db — reuse Open to apply the schema (not hand-written SQL).
	dbPath := filepath.Join(cachePath, "index.db")
	store, err := Open(dbPath)
	if err != nil {
		return fmt.Errorf("creating index: %w", err)
	}
	store.Close()

	// 2. graph.bin — "CSR1" magic (4 bytes) + uint32 node count (4) + uint32 edge count (4) = 12 bytes.
	graphPath := filepath.Join(cachePath, "graph.bin")
	graphData := make([]byte, 12)
	copy(graphData[0:4], "CSR1")
	binary.LittleEndian.PutUint32(graphData[4:8], 0)
	binary.LittleEndian.PutUint32(graphData[8:12], 0)
	if err := os.WriteFile(graphPath, graphData, 0644); err != nil {
		return fmt.Errorf("writing graph.bin: %w", err)
	}

	// 3. preflight.md — fallback content shown when the daemon hasn't run yet.
	preflightPath := filepath.Join(cachePath, "preflight.md")
	preflightContent := "# Vault Index\n\nIndex not yet built. Run `obsidian index` to populate.\n"
	if err := os.WriteFile(preflightPath, []byte(preflightContent), 0644); err != nil {
		return fmt.Errorf("writing preflight.md: %w", err)
	}

	_ = vaultPath // reserved for future incremental rebuild
	return nil
}

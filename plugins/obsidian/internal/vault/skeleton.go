// Package vault implements RFC §7 Phase 0 (TRK-524): vault skeleton initialization.
package vault

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

// VaultKind selects which folder schema InitSkeleton creates.
type VaultKind int

const (
	// KindNanika creates the 9-folder Nanika agent vault schema.
	KindNanika VaultKind = iota
	// KindSecondBrain creates the 7-folder second-brain schema.
	KindSecondBrain
)

// InitSkeleton creates the vault directory structure and a root index.md.
// It is idempotent: existing folders are left untouched and an existing
// index.md is never overwritten.
func InitSkeleton(path string, kind VaultKind) error {
	if err := os.MkdirAll(path, 0755); err != nil {
		return fmt.Errorf("creating vault directory: %w", err)
	}

	schema := SchemaFor(kind)

	for _, d := range schema.Dirs {
		if err := os.MkdirAll(filepath.Join(path, d), 0755); err != nil {
			return fmt.Errorf("creating directory %s: %w", d, err)
		}
	}

	indexPath := filepath.Join(path, IndexFile)
	f, err := os.OpenFile(indexPath, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0644)
	if err != nil {
		if errors.Is(err, os.ErrExist) {
			return nil
		}
		return fmt.Errorf("creating index.md: %w", err)
	}
	defer f.Close()

	if _, err := f.WriteString(schema.MOCTemplate); err != nil {
		return fmt.Errorf("writing index.md: %w", err)
	}
	return nil
}

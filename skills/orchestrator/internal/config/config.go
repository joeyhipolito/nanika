// Package config resolves the orchestrator's base config directory.
// All other packages derive their paths from Dir() so that a single env var
// can relocate the entire config tree at test time or in multi-instance setups.
package config

import (
	"fmt"
	"os"
	"path/filepath"
)

const (
	// DirName is the default config directory name under $HOME.
	DirName = ".alluka"
	// DirNameLegacy is the pre-Nanika config directory name.
	DirNameLegacy = ".via"
	// EnvVar is the env var that overrides the default config directory.
	// Highest priority — takes precedence over all others.
	EnvVar = "ORCHESTRATOR_CONFIG_DIR"
	// EnvVarAllukaHome is the unified Nanika config home env var.
	EnvVarAllukaHome = "ALLUKA_HOME"
	// EnvVarViaHome is the legacy cross-tool base directory env var.
	EnvVarViaHome = "VIA_HOME"
)

// Dir returns the orchestrator base config directory.
//
// Priority (highest to lowest):
//  1. ORCHESTRATOR_CONFIG_DIR — explicit orchestrator override
//  2. ALLUKA_HOME             — unified Nanika config home
//  3. VIA_HOME                — legacy cross-tool base → $VIA_HOME/orchestrator/
//  4. ~/.alluka               — new default (if exists)
//  5. ~/.via                  — legacy fallback
func Dir() (string, error) {
	if d := os.Getenv(EnvVar); d != "" {
		return d, nil
	}
	if d := os.Getenv(EnvVarAllukaHome); d != "" {
		return d, nil
	}
	if d := os.Getenv(EnvVarViaHome); d != "" {
		return filepath.Join(d, "orchestrator"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	// Prefer ~/.alluka if it exists, fall back to ~/.via
	alluka := filepath.Join(home, DirName)
	if _, err := os.Stat(alluka); err == nil {
		return alluka, nil
	}
	return filepath.Join(home, DirNameLegacy), nil
}

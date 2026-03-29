package scan

import (
	"fmt"
	"os"
	"path/filepath"
)

// Dir returns the Alluka base config directory using the same priority chain
// as the orchestrator's config.Dir():
//  1. ORCHESTRATOR_CONFIG_DIR — explicit orchestrator override
//  2. ALLUKA_HOME             — unified Nanika config home
//  3. VIA_HOME               — legacy cross-tool base → $VIA_HOME/orchestrator/
//  4. ~/.alluka              — new default (if exists)
//  5. ~/.via                 — legacy fallback
func Dir() (string, error) {
	if d := os.Getenv("ORCHESTRATOR_CONFIG_DIR"); d != "" {
		return d, nil
	}
	if d := os.Getenv("ALLUKA_HOME"); d != "" {
		return d, nil
	}
	if d := os.Getenv("VIA_HOME"); d != "" {
		return filepath.Join(d, "orchestrator"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	alluka := filepath.Join(home, ".alluka")
	if _, err := os.Stat(alluka); err == nil {
		return alluka, nil
	}
	return filepath.Join(home, ".via"), nil
}

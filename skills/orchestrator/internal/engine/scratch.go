package engine

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/scratch"
)

// maxScratchBytes is the maximum size of scratch notes injected into a
// worker's CLAUDE.md. Notes are truncated to this limit to prevent context
// bloat across long dependency chains.
const maxScratchBytes = 4096

// ExtractScratchBlock returns the concatenated content of all
// <!-- scratch --> ... <!-- /scratch --> blocks found in output.
// Returns "" when no blocks are present.
// Delegates to the scratch leaf package so worker tests can import the same
// implementation without creating an engine→worker→engine import cycle.
func ExtractScratchBlock(output string) string {
	return scratch.ExtractBlock(output)
}

// extractScratch extracts scratch blocks from phase output and writes them
// to the phase's scratch directory. Non-fatal: errors are logged when verbose.
func (e *Engine) extractScratch(phase *core.Phase, output string) {
	content := ExtractScratchBlock(output)
	if content == "" {
		return
	}
	phaseID := phaseRuntimeID(phase)
	dir := core.ScratchDir(e.workspace.Path, phaseID)
	if err := os.MkdirAll(dir, 0700); err != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] scratch dir creation failed for phase %s: %v\n", phase.Name, err)
		}
		return
	}
	if err := os.WriteFile(filepath.Join(dir, "notes.md"), []byte(content), 0600); err != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] scratch write failed for phase %s: %v\n", phase.Name, err)
		}
	}
}

// collectPriorScratch reads scratch notes from all completed dependency
// phases. Returns nil when no scratch notes exist.
func (e *Engine) collectPriorScratch(phase *core.Phase) map[string]string {
	if len(phase.Dependencies) == 0 {
		return nil
	}
	result := make(map[string]string)
	for _, depID := range phase.Dependencies {
		dep, ok := e.phases[depID]
		if !ok || dep.Status != core.StatusCompleted {
			continue
		}
		depPhaseID := phaseRuntimeID(dep)
		scratchPath := filepath.Join(core.ScratchDir(e.workspace.Path, depPhaseID), "notes.md")
		data, err := os.ReadFile(scratchPath)
		if err != nil {
			continue
		}
		content := strings.TrimSpace(string(data))
		if content != "" {
			result[dep.Name] = content
		}
	}
	if len(result) == 0 {
		return nil
	}
	return result
}

// truncateScratch truncates scratch content to the given limit, appending a
// truncation marker when content is clipped.
func truncateScratch(content string, limit int) string {
	if len(content) <= limit {
		return content
	}
	return content[:limit] + "\n\n[truncated — exceeded 4KB scratchpad limit]"
}

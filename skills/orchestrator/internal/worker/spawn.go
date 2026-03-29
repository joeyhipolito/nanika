package worker

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
	"github.com/joeyhipolito/orchestrator-cli/internal/router"
)

// Spawn creates a worker directory with CLAUDE.md and hooks.
func Spawn(wsPath string, phase *core.Phase, bundle core.ContextBundle) (*core.WorkerConfig, error) {
	workerName := fmt.Sprintf("%s-%s", phase.Persona, phase.ID)

	workerDir, err := core.CreateWorkerDir(wsPath, workerName)
	if err != nil {
		return nil, fmt.Errorf("create worker dir: %w", err)
	}

	// Resolve persona prompt from ~/nanika/personas/*.md
	personaPrompt := persona.GetPrompt(phase.Persona)
	if personaPrompt == "" {
		// Fallback to backend-engineer if persona not found
		personaPrompt = persona.GetPrompt("senior-backend-engineer")
	}
	bundle.Persona = personaPrompt
	bundle.PersonaName = phase.Persona

	// Load skills
	if len(phase.Skills) > 0 {
		bundle.Skills = LoadSkills(phase.Skills)
	}

	// Resolve model (runtime-aware: Codex gets OpenAI model names)
	tier := router.ModelTier(phase.ModelTier)
	model := router.ResolveForRuntime(tier, phase.Runtime)
	effortLevel := router.ResolveEffortForRuntime(tier, phase.Persona, phase.Runtime)

	// WorkerDir is the artifact output directory; populate it before BuildCLAUDEmd
	// so the output instruction can reference the explicit path when TargetDir is set.
	bundle.WorkerDir = workerDir

	// Generate CLAUDE.md
	claudemd := BuildCLAUDEmd(bundle)
	claudemdPath := filepath.Join(workerDir, "CLAUDE.md")
	if err := os.WriteFile(claudemdPath, []byte(claudemd), 0600); err != nil {
		return nil, fmt.Errorf("write CLAUDE.md: %w", err)
	}

	// Generate stop hook for learning capture
	learningsPath := filepath.Join(core.LearningsDir(wsPath), workerName+".json")
	hookScript := learning.GenerateHookScript(workerName, bundle.Domain, wsPath, learningsPath)

	hookPath := filepath.Join(workerDir, ".claude", "hooks", "stop.sh")
	if err := os.WriteFile(hookPath, []byte(hookScript), 0700); err != nil {
		return nil, fmt.Errorf("write hook: %w", err)
	}

	return &core.WorkerConfig{
		Name:        workerName,
		WorkerDir:   workerDir,
		TargetDir:   bundle.TargetDir,
		Model:       model,
		EffortLevel: effortLevel,
		Bundle:      bundle,
		HookScript:  hookScript,
	}, nil
}

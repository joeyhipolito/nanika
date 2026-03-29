// gen-decomposer-prompt writes the live decomposer system prompt (baseline, no
// target context) to stdout. Pipe it into evals/prompts/decomposer.txt to keep
// the promptfoo eval in sync with the real orchestrator prompt builder.
//
// Usage (from repo root):
//
//	go run ./cmd/gen-decomposer-prompt > ~/nanika/evals/prompts/decomposer.txt
//
// Or via make:
//
//	make gen-eval-prompt
package main

import (
	"fmt"

	"github.com/joeyhipolito/orchestrator-cli/internal/decompose"
)

func main() {
	// Placeholder task sentinel — promptfoo replaces the full prompt with
	// its own {{task}} variable substitution, so this value is never sent to
	// the model. It exists only so the "## Task to Decompose" section is
	// visible in the generated file for human review.
	fmt.Print(decompose.BuildBaselinePrompt("{{task}}"))
}

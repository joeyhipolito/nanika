// gen-persona-selector-prompt writes the live persona-selector prompt (the
// exact prompt used by llmMatch in internal/persona) to stdout. Pipe it into
// evals/prompts/persona-selector.txt to keep the promptfoo eval in sync with
// the real orchestrator routing call.
//
// Usage (from repo root):
//
//	go run ./cmd/gen-persona-selector-prompt > ~/nanika/evals/prompts/persona-selector.txt
//
// Or via make:
//
//	make gen-eval-persona-selector
package main

import (
	"fmt"

	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

func main() {
	summary := persona.FormatForDecomposer()
	fmt.Printf(`Pick the single best persona for this task. Reply with ONLY the persona name, nothing else.

## Available Personas
%s

## Task
{{task}}

Reply with just the persona name (e.g., "senior-backend-engineer"):`, summary)
}

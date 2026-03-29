# Ko — Eval Engine for Promptfoo YAML

Ko is a Go evaluation engine for running prompt tests defined in promptfoo YAML format. It parses test configurations, renders prompts with variables, queries an LLM, and runs assertions on the output.

## Features

- **Promptfoo YAML parsing**: Load standard promptfoo eval configurations
- **Variable rendering**: Render prompts using Go's `text/template` syntax (`{{variable}}`)
- **Parallel execution**: Run tests concurrently with configurable concurrency level
- **Rich assertions**: Support for contains, regex, JSON validation, string equality, and more
- **Default assertions**: Apply assertions to all tests without repetition
- **Skip support**: Skip individual tests without removing them from config
- **Clean error handling**: Detailed error messages for assertion failures

## Installation

Ko is part of the `nen` Go module. No additional installation needed beyond standard Go module import.

```bash
go get github.com/joeyhipolito/nen/ko
```

## Usage

### Basic Example

```go
package main

import (
	"context"
	"log"

	"github.com/joeyhipolito/nen/ko"
)

func main() {
	// Load eval config from YAML file
	cfg, err := ko.LoadEvalConfig(context.Background(), "eval.yaml")
	if err != nil {
		log.Fatalf("Load config: %v", err)
	}

	// Create runner with options
	opts := ko.RunnerOptions{
		Concurrency: 4,
		Model:       "claude-3-5-sonnet-20241022",
		Verbose:     true,
	}
	runner := ko.NewRunner(cfg, opts)

	// Define how to query the LLM
	queryFn := func(ctx context.Context, prompt string) (string, error) {
		// Call your LLM here (e.g., Claude API, local model, etc.)
		// For now, this is a placeholder
		return callYourLLM(ctx, prompt)
	}

	// Run tests
	results, err := runner.Run(context.Background(), queryFn)
	if err != nil {
		log.Fatalf("Run tests: %v", err)
	}

	// Process results
	log.Printf("Passed: %d/%d", results.Passed, results.Total)
	for _, test := range results.Tests {
		if !test.Passed {
			log.Printf("FAIL: %s", test.Description)
			for _, assertion := range test.Assertions {
				if !assertion.Passed {
					log.Printf("  - %s: %s", assertion.Type, assertion.Message)
				}
			}
		}
	}
}
```

### YAML Configuration Format

Ko uses the promptfoo YAML format:

```yaml
description: "Code review eval"
sharing: false

providers:
  - file://providers/claude-cli.mjs

prompts:
  - file://prompts/review.txt

defaultTest:
  assert:
    - type: contains
      value: "### Blockers"
    - type: contains
      value: "### Warnings"

tests:
  - description: "Basic code review"
    vars:
      code: |
        func Process(input string) {
          return "output"
        }
    assert:
      - type: regex
        value: "Process"

  - description: "Security vulnerability detection"
    vars:
      code: |
        func Query(id string) string {
          return "SELECT * FROM users WHERE id = " + id
        }
    assert:
      - type: llm-rubric
        value: "The response identifies SQL injection vulnerability"

  - description: "Skipped test"
    skip: true
    vars:
      code: "test code"
```

### Prompt Template Format

Prompts use Go's `text/template` syntax with `{{.variableName}}` for variable substitution:

```
You are a code reviewer.

## Code to Review

{{.code}}

## Review Objective

{{.objective}}
```

Variables from the test's `vars` field are passed to the template.

## Assertion Types

### contains
Check if output contains a substring (case-sensitive).

```yaml
assert:
  - type: contains
    value: "error found"
```

### not-contains
Check if output does NOT contain a substring.

```yaml
assert:
  - type: not-contains
    value: "success"
```

### equals
Check if output exactly equals a value (after trimming whitespace).

```yaml
assert:
  - type: equals
    value: "OK"
```

### regex / matches
Check if output matches a regex pattern.

```yaml
assert:
  - type: regex
    value: '^\d{3}-\d{4}$'
```

### is-json
Check if output is valid JSON.

```yaml
assert:
  - type: is-json
    value: ""  # value is ignored for this type
```

### json-schema
Validate JSON output against a schema (currently just validates JSON validity).

```yaml
assert:
  - type: json-schema
    value: '{"type": "object"}'  # schema URL or inline schema
```

### llm-rubric
Use an LLM to evaluate output against a rubric (placeholder, currently always passes if output is non-empty).

```yaml
assert:
  - type: llm-rubric
    value: "The response contains a clear explanation"
```

### length
Check output length with min, max, range, or equals criteria.

```yaml
assert:
  - type: length
    value: "min:10"      # at least 10 characters
  - type: length
    value: "max:1000"    # at most 1000 characters
  - type: length
    value: "range:10,100"  # between 10 and 100 characters
  - type: length
    value: "equals:42"   # exactly 42 characters
```

## Configurable Options

### RunnerOptions

```go
type RunnerOptions struct {
	Concurrency int    // Number of parallel workers (default: 1)
	Model       string // LLM model to use (default: claude-3-5-sonnet-20241022)
	Verbose     bool   // Print progress to stderr (default: false)
}
```

## Test Results

The `Run` method returns an `EvalResults` struct with comprehensive test outcome data:

```go
type EvalResults struct {
	ConfigPath  string        // Path to the eval config
	Tests       []TestResult  // Results for each test
	Passed      int           // Number of passing tests
	Failed      int           // Number of failing tests
	Total       int           // Total number of tests
	FailedTests []string      // Descriptions of failed tests
}

type TestResult struct {
	Description string           // Test description
	Passed      bool             // Did the test pass?
	Output      string           // LLM output
	Assertions  []AssertionResult // Individual assertion results
	Error       string           // Error message if test failed
	DurationMs  int64            // Test duration in milliseconds
}
```

## Parallel Execution

Set `Concurrency` > 1 to run tests in parallel:

```go
opts := ko.RunnerOptions{
	Concurrency: 4,  // Run up to 4 tests simultaneously
}
runner := ko.NewRunner(cfg, opts)
```

Tests are distributed evenly across worker goroutines. Results are collected and returned in the original test order.

## Integration with SDK

To integrate with an LLM provider (e.g., Claude SDK):

```go
import "github.com/joeyhipolito/orchestrator-cli/internal/sdk"

queryFn := func(ctx context.Context, prompt string) (string, error) {
	return sdk.QueryText(ctx, prompt, &sdk.AgentOptions{
		Model: "claude-3-5-sonnet-20241022",
	})
}

results, err := runner.Run(ctx, queryFn)
```

## Testing

Run the test suite:

```bash
go test ./ko -v
```

Test coverage includes:
- All assertion types (contains, equals, regex, JSON, length)
- Prompt rendering with variables
- Config loading and parsing
- Parallel test execution
- Default assertions inheritance
- Test skipping
- Error handling

## API Reference

### LoadEvalConfig

```go
func LoadEvalConfig(ctx context.Context, configPath string) (*EvalConfig, error)
```

Load and parse a promptfoo YAML config file. Resolves all `file://` references and stores prompt content.

### NewRunner

```go
func NewRunner(cfg *EvalConfig, opts RunnerOptions) *Runner
```

Create a new test runner with the given config and options.

### Run

```go
func (r *Runner) Run(ctx context.Context, queryFn QueryFunc) (*EvalResults, error)
```

Execute all tests, calling `queryFn` for each test's LLM query. Returns aggregated results.

### RenderPrompt

```go
func (c *EvalConfig) RenderPrompt(promptPath string, vars map[string]interface{}) (string, error)
```

Render a prompt template with variables. Separate method if you need to test rendering independently.

### Assertion Functions

All assertion logic is available as standalone functions in `assertions.go`:

```go
AssertContains(output, value string) (bool, string)
AssertNotContains(output, value string) (bool, string)
AssertEquals(output, value string) (bool, string)
AssertMatches(output, pattern string) (bool, string)
AssertIsJSON(output string) (bool, string)
AssertLength(output, criteria string) (bool, string)
```

Each returns `(passed bool, message string)`.

## Limitations & Future Work

1. **LLM-rubric assertion**: Currently a placeholder. Full implementation would require a secondary LLM call.
2. **JSON schema validation**: Currently validates JSON validity only. Could be extended with JSON Schema validation.
3. **Single prompt per config**: Each eval runs all tests against the first prompt in the config. Multi-prompt evals would require design changes.
4. **No result persistence**: Results are returned but not saved. Consider adding logging/metrics integration.

## Contributing

When extending Ko:

- Add new assertion types to `assertions.go` and update `RunAssertion` dispatcher
- Add corresponding unit tests to `assertions_test.go`
- Update this README with new assertion type documentation
- Maintain `QueryFunc` interface for testability (always allow dependency injection)

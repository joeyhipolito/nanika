package ko

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"text/template"
	"time"

	"gopkg.in/yaml.v3"
)

// promptEntry is a single prompt — either a file:// reference or inline raw text.
type promptEntry struct {
	fileRef string // non-empty when file://
	label   string // display name (raw form)
	raw     string // inline content (raw form)
}

func (p *promptEntry) UnmarshalYAML(value *yaml.Node) error {
	if value.Kind == yaml.ScalarNode {
		p.fileRef = value.Value
		return nil
	}
	// Object form: {label: "...", raw: "..."}
	type rawForm struct {
		Label string `yaml:"label"`
		Raw   string `yaml:"raw"`
	}
	var r rawForm
	if err := value.Decode(&r); err != nil {
		return fmt.Errorf("decode prompt entry: %w", err)
	}
	p.label = r.Label
	p.raw = r.Raw
	return nil
}

// EvalConfig represents a promptfoo-style eval configuration
type EvalConfig struct {
	Description string        `yaml:"description"`
	Sharing     bool          `yaml:"sharing"`
	Providers   []string      `yaml:"providers"`
	Prompts     []promptEntry `yaml:"prompts"`
	DefaultTest DefaultTest   `yaml:"defaultTest"`
	Tests       []TestCase    `yaml:"tests"`

	// Resolved during Load
	promptKeys     []string          `yaml:"-"` // ordered keys into promptContents
	promptContents map[string]string `yaml:"-"` // key -> content
	basePath       string            `yaml:"-"` // directory for resolving relative paths
}

// bareVarRe matches {{varname}} (no dot) so they can be promoted to {{.varname}}.
var bareVarRe = regexp.MustCompile(`\{\{(\s*[a-zA-Z_][a-zA-Z0-9_]*\s*)\}\}`)

// DefaultTest specifies default assertions for all tests
type DefaultTest struct {
	Assert []AssertionConfig `yaml:"assert"`
}

// TestCase represents a single test
type TestCase struct {
	Description string               `yaml:"description"`
	Vars        map[string]string    `yaml:"vars"`
	Assert      []AssertionConfig    `yaml:"assert"`
	Skip        bool                 `yaml:"skip"`
}

// AssertionConfig specifies an assertion to run
type AssertionConfig struct {
	Type        string            `yaml:"type"`
	Value       string            `yaml:"value"`
	Description string            `yaml:"description"` // optional human-readable label
	Threshold   float64           `yaml:"threshold"`   // numeric threshold for cost, latency, weighted, similar
	Assert      []AssertionConfig `yaml:"assert"`      // nested assertions for not, assert-all, assert-any, weighted
	Weight      float64           `yaml:"weight"`      // per-assertion weight used by a weighted parent
	Dual        bool              `yaml:"dual"`        // enable dual-judge (primary + codex); disagreements flagged as REVIEW
}

// AssertionMeta carries per-call metadata available to cost and latency assertions.
type AssertionMeta struct {
	CostUSD   float64
	LatencyMs int64
}

// TestResult represents the result of a single test
type TestResult struct {
	Description  string            `json:"description"`
	Passed       bool              `json:"passed"`
	Output       string            `json:"output"`
	Assertions   []AssertionResult `json:"assertions"`
	Error        string            `json:"error,omitempty"`
	DurationMs   int64             `json:"duration_ms"`
	InputTokens  int               `json:"input_tokens,omitempty"`
	OutputTokens int               `json:"output_tokens,omitempty"`
	CostUSD      float64           `json:"cost_usd,omitempty"`
	CacheHit     bool              `json:"cache_hit,omitempty"`
}

// AssertionResult represents the result of a single assertion
type AssertionResult struct {
	Type        string `json:"type"`
	Value       string `json:"value"`
	Description string `json:"description,omitempty"`
	Passed      bool   `json:"passed"`
	Message     string `json:"message,omitempty"`
	Reasoning   string `json:"reasoning,omitempty"` // judge explanation (LLM assertion types)
	Review      bool   `json:"review,omitempty"`    // true when dual judges disagree
}

// EvalResults contains results from running all tests
type EvalResults struct {
	ConfigPath   string       `json:"config_path"`
	Tests        []TestResult `json:"tests"`
	Passed       int          `json:"passed"`
	Failed       int          `json:"failed"`
	Total        int          `json:"total"`
	FailedTests  []string     `json:"failed_tests"`
	InputTokens  int          `json:"input_tokens,omitempty"`
	OutputTokens int          `json:"output_tokens,omitempty"`
	CostUSD      float64      `json:"cost_usd,omitempty"`
}

// RunnerOptions configures eval runner behavior
type RunnerOptions struct {
	Concurrency int              // Number of parallel tests (default 1)
	Model       string           // LLM model to use (default: claude-3-5-sonnet)
	Verbose     bool             // Print progress to stderr
	UseCliMode  bool             // Use claude CLI instead of Anthropic API
	OnResult    func(*TestResult) // Called immediately after each test completes (serial, safe to use from one goroutine)
}

// LoadEvalConfig loads and parses a promptfoo-style YAML config file.
// Prompts may be file:// references or inline {label, raw} objects.
func LoadEvalConfig(ctx context.Context, configPath string) (*EvalConfig, error) {
	data, err := os.ReadFile(configPath)
	if err != nil {
		return nil, fmt.Errorf("read config: %w", err)
	}

	var cfg EvalConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parse config: %w", err)
	}

	cfg.promptContents = make(map[string]string)
	configDir := filepath.Dir(configPath)
	cfg.basePath = configDir

	for i, p := range cfg.Prompts {
		if p.fileRef != "" {
			if strings.HasPrefix(p.fileRef, "file://") {
				absPath := filepath.Join(configDir, strings.TrimPrefix(p.fileRef, "file://"))
				content, err := os.ReadFile(absPath)
				if err != nil {
					return nil, fmt.Errorf("read prompt %s: %w", absPath, err)
				}
				cfg.promptContents[absPath] = string(content)
				cfg.promptKeys = append(cfg.promptKeys, absPath)
			}
		} else {
			// Inline raw prompt
			key := fmt.Sprintf("raw:%d:%s", i, p.label)
			cfg.promptContents[key] = p.raw
			cfg.promptKeys = append(cfg.promptKeys, key)
		}
	}

	return &cfg, nil
}

// RenderPrompt renders a prompt template with variables.
// Supports both {{.varname}} (Go template) and {{varname}} (promptfoo-style).
func (c *EvalConfig) RenderPrompt(promptKey string, vars map[string]interface{}) (string, error) {
	content, ok := c.promptContents[promptKey]
	if !ok {
		return "", fmt.Errorf("prompt not found: %s", promptKey)
	}

	// Promote bare {{varname}} → {{.varname}} so Go templates work with promptfoo-style vars.
	// This does not affect already-dotted {{.varname}} or actions like {{range}}.
	content = bareVarRe.ReplaceAllString(content, "{{.$1}}")

	tmpl, err := template.New("prompt").Option("missingkey=zero").Parse(content)
	if err != nil {
		return "", fmt.Errorf("parse template: %w", err)
	}

	var buf strings.Builder
	if err := tmpl.Execute(&buf, vars); err != nil {
		return "", fmt.Errorf("render template: %w", err)
	}

	return buf.String(), nil
}

// Runner executes tests against LLM prompts
type Runner struct {
	config  *EvalConfig
	opts    RunnerOptions
	queryFn QueryFunc // For testing, allows injection
}

// QueryFunc is the function type for querying LLM (allows testing without actual SDK)
type QueryFunc func(ctx context.Context, prompt string) (string, error)

// NewRunner creates a new test runner
func NewRunner(cfg *EvalConfig, opts RunnerOptions) *Runner {
	if opts.Concurrency <= 0 {
		opts.Concurrency = 1
	}
	if opts.Model == "" {
		opts.Model = "claude-3-5-sonnet-20241022"
	}

	return &Runner{
		config: cfg,
		opts:   opts,
	}
}

// Run executes all tests with the given QueryFunc
func (r *Runner) Run(ctx context.Context, queryFn QueryFunc) (*EvalResults, error) {
	if queryFn == nil {
		return nil, fmt.Errorf("queryFn is required")
	}
	r.queryFn = queryFn

	results := &EvalResults{
		ConfigPath: r.config.Description,
		Tests:      make([]TestResult, 0, len(r.config.Tests)),
		FailedTests: make([]string, 0),
	}

	// Use worker pool for parallel execution
	workChan := make(chan *TestCase, len(r.config.Tests))
	resultChan := make(chan *TestResult, len(r.config.Tests))
	errChan := make(chan error, 1)

	var wg sync.WaitGroup

	// Spawn workers
	for i := 0; i < r.opts.Concurrency; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for tc := range workChan {
				if tc.Skip {
					if r.opts.Verbose {
						fmt.Fprintf(os.Stderr, "[SKIP] %s\n", tc.Description)
					}
					resultChan <- &TestResult{
						Description: tc.Description,
						Passed:      true, // skipped tests don't fail
						Output:      "(skipped)",
					}
					continue
				}

				result := r.runTest(ctx, tc)
				resultChan <- result
			}
		}()
	}

	// Send work
	go func() {
		for _, tc := range r.config.Tests {
			workChan <- &tc
		}
		close(workChan)
	}()

	// Collect results
	go func() {
		wg.Wait()
		close(resultChan)
	}()

	for result := range resultChan {
		if r.opts.OnResult != nil {
			r.opts.OnResult(result)
		}
		results.Tests = append(results.Tests, *result)
		results.InputTokens += result.InputTokens
		results.OutputTokens += result.OutputTokens
		results.CostUSD += result.CostUSD
		if result.Passed {
			results.Passed++
		} else {
			results.Failed++
			results.FailedTests = append(results.FailedTests, result.Description)
		}
	}

	results.Total = len(results.Tests)

	// Check for errors
	select {
	case err := <-errChan:
		return results, err
	default:
	}

	return results, nil
}

// runTest executes a single test case
func (r *Runner) runTest(ctx context.Context, tc *TestCase) *TestResult {
	start := time.Now()
	result := &TestResult{
		Description: tc.Description,
		Assertions:  make([]AssertionResult, 0),
	}

	if r.opts.Verbose {
		fmt.Fprintf(os.Stderr, "[RUN] %s\n", tc.Description)
	}

	// Get the first prompt (for now, single prompt per config)
	if len(r.config.promptKeys) == 0 {
		result.Error = "no prompts configured"
		result.Passed = false
		return result
	}

	promptPath := r.config.promptKeys[0]

	// Render prompt
	vars := make(map[string]interface{})
	for k, v := range tc.Vars {
		vars[k] = v
	}

	prompt, err := r.config.RenderPrompt(promptPath, vars)
	if err != nil {
		result.Error = fmt.Sprintf("render prompt: %v", err)
		result.Passed = false
		return result
	}

	// Attach a per-call usage recorder so the query function can report tokens.
	queryCtx, usagePtr := WithUsageRecorder(ctx)

	// Query LLM
	output, err := r.queryFn(queryCtx, prompt)
	if err != nil {
		result.Error = fmt.Sprintf("query LLM: %v", err)
		result.Passed = false
		return result
	}

	result.Output = output
	result.DurationMs = time.Since(start).Milliseconds()
	result.CacheHit = usagePtr.CacheHit
	result.InputTokens = usagePtr.InputTokens
	result.OutputTokens = usagePtr.OutputTokens
	result.CostUSD = CostUSD(r.opts.Model, *usagePtr)

	meta := AssertionMeta{
		CostUSD:   result.CostUSD,
		LatencyMs: result.DurationMs,
	}

	// Combine assertions from defaultTest and this test
	assertions := append(
		append([]AssertionConfig{}, r.config.DefaultTest.Assert...),
		tc.Assert...,
	)

	// Run assertions
	allPassed := true
	for _, assertion := range assertions {
		ar := RunAssertion(ctx, assertion, output, meta)
		result.Assertions = append(result.Assertions, ar)
		if !ar.Passed {
			allPassed = false
		}
	}

	result.Passed = allPassed

	if r.opts.Verbose {
		status := "PASS"
		if !result.Passed {
			status = "FAIL"
		}
		fmt.Fprintf(os.Stderr, "[%s] %s\n", status, tc.Description)
	}

	return result
}

// execCmdInterface defines the minimal interface for exec.Cmd to make it testable.
type execCmdInterface interface {
	Output() ([]byte, error)
}

// execCommand is a package variable that can be mocked in tests.
// It defaults to exec.CommandContext.
var execCommand func(ctx context.Context, name string, arg ...string) execCmdInterface = func(ctx context.Context, name string, arg ...string) execCmdInterface {
	return exec.CommandContext(ctx, name, arg...)
}

// CallLLM executes the claude CLI with the given model and prompt, returning stdout.
// Command: claude --model MODEL --print --output-format text --max-turns 1 --dangerously-skip-permissions -p PROMPT
func CallLLM(ctx context.Context, model, prompt string) (string, error) {
	if model == "" {
		return "", fmt.Errorf("model is required")
	}
	if prompt == "" {
		return "", fmt.Errorf("prompt is required")
	}

	cmd := execCommand(ctx, "claude",
		"--model", model,
		"--print",
		"--bare",
		"--output-format", "text",
		"--max-turns", "1",
		"--dangerously-skip-permissions",
		"-p", prompt,
	)

	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("claude exec: %w", err)
	}

	return strings.TrimSpace(string(out)), nil
}

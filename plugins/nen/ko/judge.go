package ko

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"strings"
)

const judgeModel = "claude-haiku-4-5-20251001"

// judgeVerdict is the normalized outcome from any judge backend.
type judgeVerdict struct {
	passed    bool
	reasoning string
}

// judgeJSON is the structured response expected from every judge.
type judgeJSON struct {
	Pass      bool   `json:"pass"`
	Reasoning string `json:"reasoning"`
}

// judgeSystemPrompt is prepended to every judge call to enforce the output contract.
const judgeSystemPrompt = `You are an impartial LLM evaluator. Respond ONLY with a JSON object, no markdown fences, no extra text.
Format: {"pass": <true|false>, "reasoning": "<one or two sentence explanation>"}`

// judgeCallAPI calls claude-haiku via the Anthropic Messages API.
func judgeCallAPI(ctx context.Context, userPrompt string) (*judgeVerdict, error) {
	apiKey := os.Getenv("ANTHROPIC_API_KEY")
	if apiKey == "" {
		return nil, fmt.Errorf("ANTHROPIC_API_KEY not set")
	}

	baseURL := os.Getenv("ANTHROPIC_BASE_URL")
	if baseURL == "" {
		baseURL = "https://api.anthropic.com"
	}

	type msg struct {
		Role    string `json:"role"`
		Content string `json:"content"`
	}
	type req struct {
		Model     string `json:"model"`
		MaxTokens int    `json:"max_tokens"`
		System    string `json:"system"`
		Messages  []msg  `json:"messages"`
	}
	body, _ := json.Marshal(req{
		Model:     judgeModel,
		MaxTokens: 256,
		System:    judgeSystemPrompt,
		Messages:  []msg{{Role: "user", Content: userPrompt}},
	})

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost,
		baseURL+"/v1/messages", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("x-api-key", apiKey)
	httpReq.Header.Set("anthropic-version", "2023-06-01")

	resp, err := http.DefaultClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("http: %w", err)
	}
	defer resp.Body.Close()

	type apiResp struct {
		Content []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"content"`
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	var ar apiResp
	if err := json.NewDecoder(resp.Body).Decode(&ar); err != nil {
		return nil, fmt.Errorf("decode response: %w", err)
	}
	if ar.Error != nil {
		return nil, fmt.Errorf("api error: %s", ar.Error.Message)
	}
	if len(ar.Content) == 0 {
		return nil, fmt.Errorf("empty response (status %d)", resp.StatusCode)
	}

	return parseJudgeJSON(ar.Content[0].Text)
}

// judgeCallCodex calls the `codex exec` CLI as a secondary judge.
// The prompt is passed via stdin; the CLI must write a judgeJSON object to stdout.
func judgeCallCodex(ctx context.Context, userPrompt string) (*judgeVerdict, error) {
	cmd := exec.CommandContext(ctx, "codex", "exec", "--json")
	cmd.Stdin = strings.NewReader(judgeSystemPrompt + "\n\n" + userPrompt)
	out, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("codex exec: %w", err)
	}
	return parseJudgeJSON(string(out))
}

// parseJudgeJSON extracts a judgeVerdict from raw text that should contain a
// judgeJSON object, tolerating a leading/trailing code fence.
func parseJudgeJSON(raw string) (*judgeVerdict, error) {
	raw = strings.TrimSpace(raw)
	// Strip ```json ... ``` or ``` ... ``` fences if present.
	if strings.HasPrefix(raw, "```") {
		if end := strings.LastIndex(raw, "```"); end > 3 {
			inner := raw[3:end]
			if nl := strings.IndexByte(inner, '\n'); nl >= 0 {
				inner = inner[nl+1:]
			}
			raw = strings.TrimSpace(inner)
		}
	}
	// Find the first { … } block if the model added commentary.
	if start := strings.IndexByte(raw, '{'); start > 0 {
		raw = raw[start:]
	}

	var v judgeJSON
	if err := json.Unmarshal([]byte(raw), &v); err != nil {
		return nil, fmt.Errorf("parse judge JSON: %w (raw: %.120s)", err, raw)
	}
	return &judgeVerdict{passed: v.Pass, reasoning: v.Reasoning}, nil
}

// runJudge runs the primary judge and, when dual is true, also the Codex judge.
// Agreement → (passed, review=false). Disagreement → (false, review=true).
func runJudge(ctx context.Context, prompt string, dual bool) (passed, review bool, reasoning string, err error) {
	primary, err := judgeCallAPI(ctx, prompt)
	if err != nil {
		return false, false, "", fmt.Errorf("primary judge: %w", err)
	}

	if !dual {
		return primary.passed, false, primary.reasoning, nil
	}

	secondary, err := judgeCallCodex(ctx, prompt)
	if err != nil {
		// Codex not available → fall back to primary only and note it.
		return primary.passed, false,
			primary.reasoning + " (codex judge unavailable: " + err.Error() + ")",
			nil
	}

	if primary.passed == secondary.passed {
		combined := primary.reasoning
		if secondary.reasoning != "" {
			combined += " | codex: " + secondary.reasoning
		}
		return primary.passed, false, combined, nil
	}

	// Judges disagree → flag for human review.
	reasoning = fmt.Sprintf("DISAGREE — primary(%v): %s | codex(%v): %s",
		primary.passed, primary.reasoning,
		secondary.passed, secondary.reasoning)
	return false, true, reasoning, nil
}

// ── Prompt builders ──────────────────────────────────────────────────────────

func rubricPrompt(output, rubric string) string {
	return fmt.Sprintf(`Evaluate the following output against the rubric.

Rubric: %s

Output:
%s`, rubric, output)
}

func similarPrompt(output, expected string, threshold float64) string {
	return fmt.Sprintf(`Determine whether the two texts are semantically similar enough to be considered equivalent.
Similarity threshold (0-1): %.2f — pass only if similarity meets or exceeds this value.

Expected:
%s

Actual:
%s`, threshold, expected, output)
}

func factualityPrompt(output, context string) string {
	return fmt.Sprintf(`Evaluate whether the output is factually consistent with the provided context.
Every factual claim in the output should be supported by the context; do not apply external knowledge.

Context:
%s

Output to evaluate:
%s`, context, output)
}

func answerRelevancePrompt(output, question string) string {
	return fmt.Sprintf(`Evaluate whether the output directly and adequately answers the question.

Question: %s

Output:
%s`, question, output)
}

// ── Public assertion helpers (called from RunAssertion) ───────────────────────

// JudgeLLMRubric evaluates output against a rubric using the LLM judge.
func JudgeLLMRubric(ctx context.Context, output, rubric string, dual bool) (passed, review bool, reasoning string, err error) {
	return runJudge(ctx, rubricPrompt(output, rubric), dual)
}

// JudgeSimilar evaluates semantic similarity between output and expected text.
func JudgeSimilar(ctx context.Context, output, expected string, threshold float64, dual bool) (passed, review bool, reasoning string, err error) {
	if threshold <= 0 {
		threshold = 0.7
	}
	return runJudge(ctx, similarPrompt(output, expected, threshold), dual)
}

// JudgeFactuality evaluates factual consistency of output against a context string.
func JudgeFactuality(ctx context.Context, output, factContext string, dual bool) (passed, review bool, reasoning string, err error) {
	return runJudge(ctx, factualityPrompt(output, factContext), dual)
}

// JudgeAnswerRelevance evaluates whether output adequately answers the question.
func JudgeAnswerRelevance(ctx context.Context, output, question string, dual bool) (passed, review bool, reasoning string, err error) {
	return runJudge(ctx, answerRelevancePrompt(output, question), dual)
}

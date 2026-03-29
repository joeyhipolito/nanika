package ko

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"strings"

	"github.com/dop251/goja"
)

// AssertContains checks if output contains the given substring
func AssertContains(output, value string) (bool, string) {
	passed := strings.Contains(output, value)
	if !passed {
		return false, fmt.Sprintf("output does not contain %q", value)
	}
	return true, ""
}

// AssertNotContains checks if output does not contain the given substring
func AssertNotContains(output, value string) (bool, string) {
	passed := !strings.Contains(output, value)
	if !passed {
		return false, fmt.Sprintf("output should not contain %q", value)
	}
	return true, ""
}

// AssertEquals checks if output exactly equals the given value (after trimming whitespace)
func AssertEquals(output, value string) (bool, string) {
	passed := strings.TrimSpace(output) == strings.TrimSpace(value)
	if !passed {
		return false, fmt.Sprintf("output does not equal %q", value)
	}
	return true, ""
}

// AssertMatches checks if output matches the given regex pattern
func AssertMatches(output, pattern string) (bool, string) {
	re, err := regexp.Compile(pattern)
	if err != nil {
		return false, fmt.Sprintf("invalid regex: %v", err)
	}

	passed := re.MatchString(output)
	if !passed {
		return false, fmt.Sprintf("output does not match pattern %q", pattern)
	}
	return true, ""
}

// AssertIsJSON checks if output is valid JSON
func AssertIsJSON(output string) (bool, string) {
	output = strings.TrimSpace(output)
	if output == "" {
		return false, "output is empty (not valid JSON)"
	}

	var v interface{}
	if err := json.Unmarshal([]byte(output), &v); err != nil {
		return false, fmt.Sprintf("output is not valid JSON: %v", err)
	}

	return true, ""
}

// AssertIsJSONWithSchema checks if output is valid JSON that can be unmarshaled to a specific type
// For now, we only verify it's valid JSON. More sophisticated schema validation can be added later.
func AssertIsJSONWithSchema(output string, schemaJSON string) (bool, string) {
	output = strings.TrimSpace(output)
	if output == "" {
		return false, "output is empty (not valid JSON)"
	}

	var v interface{}
	if err := json.Unmarshal([]byte(output), &v); err != nil {
		return false, fmt.Sprintf("output is not valid JSON: %v", err)
	}

	return true, ""
}


// AssertLength checks if output length (in characters) meets the criteria
// value should be in format like "min:10", "max:100", or "range:10,100"
func AssertLength(output, criteria string) (bool, string) {
	length := len(output)

	parts := strings.Split(criteria, ":")
	if len(parts) != 2 {
		return false, fmt.Sprintf("invalid length criteria format: %q", criteria)
	}

	op := strings.TrimSpace(parts[0])
	val := strings.TrimSpace(parts[1])

	switch op {
	case "min":
		var minLen int
		if _, err := fmt.Sscanf(val, "%d", &minLen); err != nil {
			return false, fmt.Sprintf("invalid min length: %s", val)
		}
		if length < minLen {
			return false, fmt.Sprintf("output length %d is less than minimum %d", length, minLen)
		}

	case "max":
		var maxLen int
		if _, err := fmt.Sscanf(val, "%d", &maxLen); err != nil {
			return false, fmt.Sprintf("invalid max length: %s", val)
		}
		if length > maxLen {
			return false, fmt.Sprintf("output length %d exceeds maximum %d", length, maxLen)
		}

	case "range":
		rangeParts := strings.Split(val, ",")
		if len(rangeParts) != 2 {
			return false, fmt.Sprintf("invalid range format: %q", val)
		}

		var minLen, maxLen int
		if _, err := fmt.Sscanf(strings.TrimSpace(rangeParts[0]), "%d", &minLen); err != nil {
			return false, fmt.Sprintf("invalid min in range: %s", rangeParts[0])
		}
		if _, err := fmt.Sscanf(strings.TrimSpace(rangeParts[1]), "%d", &maxLen); err != nil {
			return false, fmt.Sprintf("invalid max in range: %s", rangeParts[1])
		}

		if length < minLen || length > maxLen {
			return false, fmt.Sprintf("output length %d is not in range [%d, %d]", length, minLen, maxLen)
		}

	case "equals", "eq":
		var expectedLen int
		if _, err := fmt.Sscanf(val, "%d", &expectedLen); err != nil {
			return false, fmt.Sprintf("invalid length: %s", val)
		}
		if length != expectedLen {
			return false, fmt.Sprintf("output length %d does not equal %d", length, expectedLen)
		}

	default:
		return false, fmt.Sprintf("unknown length operator: %s", op)
	}

	return true, ""
}

// AssertJavaScript evaluates a JavaScript snippet with `output` in scope.
// The snippet should either return true (pass) or throw an Error (fail).
func AssertJavaScript(output, code string) (bool, string) {
	vm := goja.New()
	_ = vm.Set("output", output)
	val, err := vm.RunString(code)
	if err != nil {
		// JS threw an Error — extract its message
		if jsErr, ok := err.(*goja.Exception); ok {
			return false, jsErr.Error()
		}
		return false, err.Error()
	}
	if val != nil && val.ToBoolean() {
		return true, ""
	}
	return false, "assertion returned false"
}

// AssertStartsWith checks if output starts with the given prefix
func AssertStartsWith(output, prefix string) (bool, string) {
	if !strings.HasPrefix(output, prefix) {
		return false, fmt.Sprintf("output does not start with %q", prefix)
	}
	return true, ""
}

// AssertEndsWith checks if output ends with the given suffix
func AssertEndsWith(output, suffix string) (bool, string) {
	if !strings.HasSuffix(output, suffix) {
		return false, fmt.Sprintf("output does not end with %q", suffix)
	}
	return true, ""
}

// AssertContainsAll checks if output contains every substring in the JSON array value
func AssertContainsAll(output, valueJSON string) (bool, string) {
	var items []string
	if err := json.Unmarshal([]byte(valueJSON), &items); err != nil {
		return false, fmt.Sprintf("value must be a JSON string array: %v", err)
	}
	for _, item := range items {
		if !strings.Contains(output, item) {
			return false, fmt.Sprintf("output does not contain %q", item)
		}
	}
	return true, ""
}

// AssertContainsAny checks if output contains at least one substring from the JSON array value
func AssertContainsAny(output, valueJSON string) (bool, string) {
	var items []string
	if err := json.Unmarshal([]byte(valueJSON), &items); err != nil {
		return false, fmt.Sprintf("value must be a JSON string array: %v", err)
	}
	for _, item := range items {
		if strings.Contains(output, item) {
			return true, ""
		}
	}
	return false, "output does not contain any of the required substrings"
}

// AssertMaxLength checks if output character count is at most maxLen
func AssertMaxLength(output string, maxLen int) (bool, string) {
	if len(output) > maxLen {
		return false, fmt.Sprintf("output length %d exceeds maximum %d", len(output), maxLen)
	}
	return true, ""
}

// AssertMinLength checks if output character count is at least minLen
func AssertMinLength(output string, minLen int) (bool, string) {
	if len(output) < minLen {
		return false, fmt.Sprintf("output length %d is less than minimum %d", len(output), minLen)
	}
	return true, ""
}

// AssertCost checks that the reported cost does not exceed the threshold (in USD)
func AssertCost(costUSD, threshold float64) (bool, string) {
	if costUSD > threshold {
		return false, fmt.Sprintf("cost $%.6f exceeds threshold $%.6f", costUSD, threshold)
	}
	return true, ""
}

// AssertLatency checks that the reported latency does not exceed the threshold (in ms)
func AssertLatency(latencyMs int64, thresholdMs float64) (bool, string) {
	if float64(latencyMs) > thresholdMs {
		return false, fmt.Sprintf("latency %dms exceeds threshold %.0fms", latencyMs, thresholdMs)
	}
	return true, ""
}

// AssertNot inverts the result of a single nested assertion
func AssertNot(ctx context.Context, assertion AssertionConfig, output string, meta AssertionMeta) (bool, string) {
	if len(assertion.Assert) == 0 {
		return false, "not assertion requires exactly one sub-assertion in assert"
	}
	sub := RunAssertion(ctx, assertion.Assert[0], output, meta)
	if sub.Passed {
		return false, fmt.Sprintf("expected assertion %q to fail but it passed", sub.Type)
	}
	return true, ""
}

// AssertAll passes only when every nested assertion passes
func AssertAll(ctx context.Context, assertion AssertionConfig, output string, meta AssertionMeta) (bool, string) {
	if len(assertion.Assert) == 0 {
		return false, "assert-all requires at least one sub-assertion in assert"
	}
	for _, sub := range assertion.Assert {
		result := RunAssertion(ctx, sub, output, meta)
		if !result.Passed {
			return false, fmt.Sprintf("assertion %q failed: %s", sub.Type, result.Message)
		}
	}
	return true, ""
}

// AssertAny passes when at least one nested assertion passes
func AssertAny(ctx context.Context, assertion AssertionConfig, output string, meta AssertionMeta) (bool, string) {
	if len(assertion.Assert) == 0 {
		return false, "assert-any requires at least one sub-assertion in assert"
	}
	for _, sub := range assertion.Assert {
		result := RunAssertion(ctx, sub, output, meta)
		if result.Passed {
			return true, ""
		}
	}
	return false, "none of the sub-assertions passed"
}

// AssertWeighted passes when the weighted pass rate meets or exceeds the threshold.
// Each sub-assertion uses its Weight field (default 1.0). Threshold defaults to 0.5.
func AssertWeighted(ctx context.Context, assertion AssertionConfig, output string, meta AssertionMeta) (bool, string) {
	if len(assertion.Assert) == 0 {
		return false, "weighted assertion requires at least one sub-assertion in assert"
	}

	threshold := assertion.Threshold
	if threshold == 0 {
		threshold = 0.5
	}

	var totalWeight, passedWeight float64
	for _, sub := range assertion.Assert {
		w := sub.Weight
		if w == 0 {
			w = 1.0
		}
		result := RunAssertion(ctx, sub, output, meta)
		totalWeight += w
		if result.Passed {
			passedWeight += w
		}
	}

	score := passedWeight / totalWeight
	if score < threshold {
		return false, fmt.Sprintf("weighted score %.2f is below threshold %.2f", score, threshold)
	}
	return true, ""
}

// RunAssertion dispatches to the appropriate assertion function based on type.
// ctx is required for LLM-backed assertion types (llm-rubric, similar, factuality, answer-relevance).
func RunAssertion(ctx context.Context, assertion AssertionConfig, output string, meta AssertionMeta) AssertionResult {
	result := AssertionResult{
		Type:        assertion.Type,
		Value:       assertion.Value,
		Description: assertion.Description,
	}

	switch assertion.Type {
	case "contains":
		result.Passed, result.Message = AssertContains(output, assertion.Value)

	case "not-contains":
		result.Passed, result.Message = AssertNotContains(output, assertion.Value)

	case "equals":
		result.Passed, result.Message = AssertEquals(output, assertion.Value)

	case "regex", "matches":
		result.Passed, result.Message = AssertMatches(output, assertion.Value)

	case "is-json":
		result.Passed, result.Message = AssertIsJSON(output)

	case "json-schema":
		result.Passed, result.Message = AssertIsJSONWithSchema(output, assertion.Value)

	case "javascript":
		result.Passed, result.Message = AssertJavaScript(output, assertion.Value)

	case "llm-rubric":
		passed, review, reasoning, err := JudgeLLMRubric(ctx, output, assertion.Value, assertion.Dual)
		if err != nil {
			result.Passed = false
			result.Message = fmt.Sprintf("judge error: %v", err)
		} else {
			result.Passed = passed
			result.Review = review
			result.Reasoning = reasoning
			if review {
				result.Message = "judges disagreed — flagged for review"
			}
		}

	case "similar":
		passed, review, reasoning, err := JudgeSimilar(ctx, output, assertion.Value, assertion.Threshold, assertion.Dual)
		if err != nil {
			result.Passed = false
			result.Message = fmt.Sprintf("judge error: %v", err)
		} else {
			result.Passed = passed
			result.Review = review
			result.Reasoning = reasoning
			if review {
				result.Message = "judges disagreed — flagged for review"
			}
		}

	case "factuality":
		passed, review, reasoning, err := JudgeFactuality(ctx, output, assertion.Value, assertion.Dual)
		if err != nil {
			result.Passed = false
			result.Message = fmt.Sprintf("judge error: %v", err)
		} else {
			result.Passed = passed
			result.Review = review
			result.Reasoning = reasoning
			if review {
				result.Message = "judges disagreed — flagged for review"
			}
		}

	case "answer-relevance":
		passed, review, reasoning, err := JudgeAnswerRelevance(ctx, output, assertion.Value, assertion.Dual)
		if err != nil {
			result.Passed = false
			result.Message = fmt.Sprintf("judge error: %v", err)
		} else {
			result.Passed = passed
			result.Review = review
			result.Reasoning = reasoning
			if review {
				result.Message = "judges disagreed — flagged for review"
			}
		}

	case "length":
		result.Passed, result.Message = AssertLength(output, assertion.Value)

	case "starts-with":
		result.Passed, result.Message = AssertStartsWith(output, assertion.Value)

	case "ends-with":
		result.Passed, result.Message = AssertEndsWith(output, assertion.Value)

	case "contains-all":
		result.Passed, result.Message = AssertContainsAll(output, assertion.Value)

	case "contains-any":
		result.Passed, result.Message = AssertContainsAny(output, assertion.Value)

	case "max-length":
		var maxLen int
		if _, err := fmt.Sscanf(assertion.Value, "%d", &maxLen); err != nil {
			result.Message = fmt.Sprintf("invalid max-length value: %s", assertion.Value)
			result.Passed = false
		} else {
			result.Passed, result.Message = AssertMaxLength(output, maxLen)
		}

	case "min-length":
		var minLen int
		if _, err := fmt.Sscanf(assertion.Value, "%d", &minLen); err != nil {
			result.Message = fmt.Sprintf("invalid min-length value: %s", assertion.Value)
			result.Passed = false
		} else {
			result.Passed, result.Message = AssertMinLength(output, minLen)
		}

	case "cost":
		result.Passed, result.Message = AssertCost(meta.CostUSD, assertion.Threshold)

	case "latency":
		result.Passed, result.Message = AssertLatency(meta.LatencyMs, assertion.Threshold)

	case "not":
		result.Passed, result.Message = AssertNot(ctx, assertion, output, meta)

	case "assert-all":
		result.Passed, result.Message = AssertAll(ctx, assertion, output, meta)

	case "assert-any":
		result.Passed, result.Message = AssertAny(ctx, assertion, output, meta)

	case "weighted":
		result.Passed, result.Message = AssertWeighted(ctx, assertion, output, meta)

	default:
		result.Message = fmt.Sprintf("unknown assertion type: %s", assertion.Type)
		result.Passed = false
	}

	return result
}

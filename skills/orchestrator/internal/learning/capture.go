package learning

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"regexp"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// maxContentLen caps the length of a captured learning to prevent storing
// stack traces, JSON blobs, or other unbounded output.
const maxContentLen = 500

// ansiPattern matches ANSI escape sequences (colors, cursor movement, etc.)
var ansiPattern = regexp.MustCompile(`\x1b\[[0-9;]*[a-zA-Z]`)

// CaptureFromText extracts learnings from worker output text.
func CaptureFromText(text, workerName, domain, workspaceID string) []Learning {
	var result []Learning
	seen := make(map[string]bool)

	scanner := bufio.NewScanner(strings.NewReader(text))
	for scanner.Scan() {
		line := scanner.Text()
		upperLine := strings.ToUpper(line)

	markerLoop:
		for _, marker := range DefaultMarkers {
			upperMarker := strings.ToUpper(marker.Marker)
			variants := []string{
				upperMarker,
				"`" + upperMarker[:len(upperMarker)-1] + "`:",
				"**" + upperMarker,
			}

			for _, variant := range variants {
				idx := strings.Index(upperLine, variant)
				if idx < 0 {
					continue
				}

				// Word-boundary check: marker must be at start of line or
				// preceded by whitespace to avoid matching "PRELEARNING:" etc.
				if idx > 0 {
					prev := line[idx-1]
					if prev != ' ' && prev != '\t' && prev != '-' && prev != '*' {
						continue
					}
				}

				content := strings.TrimSpace(line[idx+len(variant):])
				content = cleanupContent(content)
				content = stripLeakedMarkers(content)

				if !isValidLearning(content) {
					continue // try next variant or marker, not break
				}

				hash := contentHash(content)
				if seen[hash] {
					break markerLoop
				}
				seen[hash] = true

				l := Learning{
					ID:           generateID(),
					Type:         marker.Type,
					Marker:       marker.Marker,
					Content:      content,
					Domain:       domain,
					WorkerName:   workerName,
					WorkspaceID:  workspaceID,
					CreatedAt:    time.Now(),
					QualityScore: HeuristicScore(marker.Type),
				}
				result = append(result, l)

				getEmitter().Emit(context.Background(), event.New(
					event.LearningExtracted,
					workspaceID, "", workerName,
					map[string]any{
						"learning_type": string(marker.Type),
						"content":       content,
						"worker_name":   workerName,
						"domain":        domain,
						"marker":        marker.Marker,
					},
				))
				break markerLoop // one match per line
			}
		}
	}

	return result
}

// isValidLearning checks that content meets minimum quality thresholds.
func isValidLearning(content string) bool {
	if len(content) < 20 {
		return false
	}
	// Must end with terminal punctuation (., !, ?, or closing paren/bracket after punctuation)
	trimmed := strings.TrimRight(content, " )")
	if trimmed == "" {
		return false
	}
	last := trimmed[len(trimmed)-1]
	return last == '.' || last == '!' || last == '?' || last == ')' || last == ']'
}

// allMarkerPrefixes includes both active and formerly-active marker prefixes
// so that leaked markers in content are always stripped regardless of whether
// the marker is still in DefaultMarkers.
var allMarkerPrefixes = []string{
	"LEARNING:", "TIL:", "INSIGHT:", "FINDING:",
	"PATTERN:", "APPROACH:",
	"GOTCHA:", "ERROR:", "FIX:",
	"SOURCE:",
	"DECISION:", "TRADEOFF:",
}

// stripLeakedMarkers removes marker prefixes that leaked into the content body.
// e.g., "LEARNING: The actual content" → "The actual content"
func stripLeakedMarkers(content string) string {
	upper := strings.ToUpper(content)
	for _, prefix := range allMarkerPrefixes {
		if strings.HasPrefix(upper, prefix) {
			content = strings.TrimSpace(content[len(prefix):])
			upper = strings.ToUpper(content)
		}
	}
	return content
}

// CaptureWithFocus uses an LLM to extract learnings aligned with the persona's focus areas.
// Supplements marker-based capture with semantically-guided extraction.
func CaptureWithFocus(ctx context.Context, output string, focusAreas []string, workerName, domain, workspaceID string) []Learning {
	if len(focusAreas) == 0 || len(output) < 200 {
		return nil
	}

	// Truncate output to avoid token overflow (keep first 8K chars)
	text := output
	if len(text) > 8000 {
		text = text[:8000]
	}

	var focusList strings.Builder
	for _, area := range focusAreas {
		focusList.WriteString("- ")
		focusList.WriteString(area)
		focusList.WriteString("\n")
	}

	prompt := fmt.Sprintf(`Extract learnings from this worker output that are relevant to these focus areas:

%s
For each learning, output one line in this exact format:
TYPE: content

Where TYPE is one of: insight, pattern, error, source, decision

Rules:
- Only extract specific, actionable learnings (not vague observations)
- Minimum 50 characters per learning
- Maximum 10 learnings
- Skip anything already marked with LEARNING:, GOTCHA:, etc.

Worker output:
%s`, focusList.String(), text)

	llmOutput, err := sdk.QueryText(ctx, prompt, &sdk.AgentOptions{
		Model:    "haiku",
		MaxTurns: 1,
	})
	if err != nil {
		return nil
	}

	var result []Learning
	seen := make(map[string]bool)

	scanner := bufio.NewScanner(strings.NewReader(llmOutput))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())

		for _, ltype := range []LearningType{TypeInsight, TypePattern, TypeError, TypeSource, TypeDecision} {
			prefix := string(ltype) + ":"
			if !strings.HasPrefix(strings.ToLower(line), prefix) {
				continue
			}
			content := strings.TrimSpace(line[len(prefix):])
			content = cleanupContent(content)
			if len(content) < 50 {
				break
			}
			hash := contentHash(content)
			if seen[hash] {
				break
			}
			seen[hash] = true

			result = append(result, Learning{
				ID:           generateID(),
				Type:         ltype,
				Content:      content,
				Context:      "focus-captured",
				Domain:       domain,
				WorkerName:   workerName,
				WorkspaceID:  workspaceID,
				CreatedAt:    time.Now(),
				QualityScore: HeuristicScore(ltype),
			})
			break
		}
	}

	return result
}

// CaptureFromConversation extracts durable learnings from a multi-turn
// conversation chunk (e.g., a Claude Code session transcript window).
//
// Unlike CaptureWithFocus — which targets a persona's declared focus areas
// from single-phase output — this function is consolidation-tuned: it targets
// decisions, insights, features discovered, and gotchas from human-Claude
// dialogue. It is called by the dream subsystem during background transcript
// mining.
//
// Prompt contract: Haiku model, max 5 learnings per call, TYPE:content format
// matching the grammar used by CaptureWithFocus. Empty output is valid.
// QualityScore starts at 0.4 (lower than live-captured learnings because
// conversation extraction is less curated than persona-focused extraction).
func CaptureFromConversation(ctx context.Context, conversationText, workerName, domain, workspaceID string) []Learning {
	if len(conversationText) < 200 {
		return nil
	}

	// Cap at 12K chars — conversation chunks are larger than single phase output
	// (CaptureWithFocus caps at 8K) but still need to stay within haiku limits.
	text := conversationText
	if len(text) > 12000 {
		text = text[:12000]
	}

	prompt := `You are reviewing a conversation between a human and an AI assistant. Extract durable learnings worth remembering across future sessions.

Focus only on:
- DECISION: architectural choices, tradeoffs accepted, approaches confirmed or rejected
- INSIGHT: non-obvious discoveries about tools, APIs, system behaviour, or domain knowledge
- PATTERN: approaches that worked or failed, reusable techniques, conventions established
- ERROR: gotchas, pitfalls, bugs discovered, misassumptions corrected, things to avoid

Output format — one learning per line:
TYPE: content

Rules:
- TYPE must be one of: insight, pattern, error, decision
- Content must be 50–400 characters, specific and actionable (not vague)
- Maximum 5 learnings total
- Skip pleasantries, summaries, obvious facts, or anything not worth remembering long-term
- If no durable learnings exist in this chunk, output nothing (empty response is valid)

Conversation:
` + text

	llmOutput, err := sdk.QueryText(ctx, prompt, &sdk.AgentOptions{
		Model:    "haiku",
		MaxTurns: 1,
	})
	if err != nil {
		return nil
	}

	var result []Learning
	seen := make(map[string]bool)

	scanner := bufio.NewScanner(strings.NewReader(llmOutput))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		for _, ltype := range []LearningType{TypeInsight, TypePattern, TypeError, TypeDecision} {
			prefix := string(ltype) + ":"
			if !strings.HasPrefix(strings.ToLower(line), prefix) {
				continue
			}
			content := strings.TrimSpace(line[len(prefix):])
			content = cleanupContent(content)
			if len(content) < 50 {
				break
			}
			hash := contentHash(content)
			if seen[hash] {
				break
			}
			seen[hash] = true

			result = append(result, Learning{
				ID:           generateID(),
				Type:         ltype,
				Content:      content,
				Domain:       domain,
				WorkerName:   workerName,
				WorkspaceID:  workspaceID,
				QualityScore: 0.4, // reduced: conversation extraction is less curated than focus capture
				CreatedAt:    time.Now(),
			})
			break
		}

		if len(result) >= 5 {
			break
		}
	}

	return result
}

// GenerateHookScript creates a shell script for learning capture.
func GenerateHookScript(workerName, domain, workspacePath, outputPath string) string {
	script := fmt.Sprintf(`#!/bin/bash
# Learning capture hook for %s
# Domain: %s

OUTPUT_FILE="%s"
mkdir -p "$(dirname "$OUTPUT_FILE")"

while IFS= read -r line; do
`, workerName, domain, outputPath)

	for _, marker := range DefaultMarkers {
		script += fmt.Sprintf(`    if [[ "$line" == *"%s"* ]]; then
        content="${line#*%s}"
        echo '{"marker":"%s","type":"%s","content":"'"$content"'","timestamp":"'$(date -u +%%Y-%%m-%%dT%%H:%%M:%%SZ)'"}' >> "$OUTPUT_FILE"
    fi
`, marker.Marker, marker.Marker, marker.Marker, marker.Type)
	}

	script += "done\n"
	return script
}

func cleanupContent(content string) string {
	content = strings.TrimSpace(content)

	// Strip ANSI escape sequences
	content = ansiPattern.ReplaceAllString(content, "")

	// Strip markdown formatting characters: bold (**), italic (*), backticks
	var result strings.Builder
	i := 0
	runes := []rune(content)
	for i < len(runes) {
		if i+1 < len(runes) && runes[i] == '*' && runes[i+1] == '*' {
			i += 2 // skip **
			continue
		}
		if runes[i] == '`' {
			i++ // skip `
			continue
		}
		result.WriteRune(runes[i])
		i++
	}

	cleaned := strings.TrimSpace(result.String())

	// Cap content length to prevent storing stack traces, JSON blobs, etc.
	if len(cleaned) > maxContentLen {
		// Truncate at the last sentence boundary within the cap, if possible.
		truncated := cleaned[:maxContentLen]
		if lastDot := strings.LastIndexAny(truncated, ".!?"); lastDot > maxContentLen/2 {
			truncated = truncated[:lastDot+1]
		}
		cleaned = truncated
	}

	return cleaned
}

func contentHash(content string) string {
	h := sha256.Sum256([]byte(strings.ToLower(content)))
	return hex.EncodeToString(h[:])
}

func generateID() string {
	return fmt.Sprintf("learn_%d", time.Now().UnixNano())
}

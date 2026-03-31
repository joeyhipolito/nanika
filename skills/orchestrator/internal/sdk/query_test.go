package sdk

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestExtractEvents_TextBlock(t *testing.T) {
	msg := &AssistantMessage{
		Content: []ContentBlock{
			{Type: BlockTypeText, Text: "Hello world"},
		},
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	ev := events[0]
	if ev.Kind != KindText {
		t.Errorf("want KindText, got %q", ev.Kind)
	}
	if ev.Text != "Hello world" {
		t.Errorf("want text %q, got %q", "Hello world", ev.Text)
	}
}

func TestExtractEvents_ToolUseBlock(t *testing.T) {
	input := json.RawMessage(`{"file_path":"/foo/bar.go"}`)
	msg := &AssistantMessage{
		Content: []ContentBlock{
			{Type: BlockTypeToolUse, ID: "toolu_01", Name: "Read", Input: input},
		},
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	ev := events[0]
	if ev.Kind != KindToolUse {
		t.Errorf("want KindToolUse, got %q", ev.Kind)
	}
	if ev.ToolName != "Read" {
		t.Errorf("want tool name %q, got %q", "Read", ev.ToolName)
	}
	if ev.ToolID != "toolu_01" {
		t.Errorf("want tool id %q, got %q", "toolu_01", ev.ToolID)
	}
}

func TestExtractEvents_MixedBlocks(t *testing.T) {
	msg := &AssistantMessage{
		Content: []ContentBlock{
			{Type: BlockTypeText, Text: "Let me read that file."},
			{Type: BlockTypeToolUse, ID: "toolu_02", Name: "Read", Input: json.RawMessage(`{}`)},
			{Type: BlockTypeText, Text: "Done."},
		},
	}
	events := extractEvents(msg)
	if len(events) != 3 {
		t.Fatalf("want 3 events, got %d", len(events))
	}
	if events[0].Kind != KindText {
		t.Errorf("events[0]: want KindText, got %q", events[0].Kind)
	}
	if events[1].Kind != KindToolUse {
		t.Errorf("events[1]: want KindToolUse, got %q", events[1].Kind)
	}
	if events[2].Kind != KindText {
		t.Errorf("events[2]: want KindText, got %q", events[2].Kind)
	}
}

func TestExtractEvents_EmptyTextBlockSkipped(t *testing.T) {
	msg := &AssistantMessage{
		Content: []ContentBlock{
			{Type: BlockTypeText, Text: ""},
			{Type: BlockTypeText, Text: "non-empty"},
		},
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event (empty block skipped), got %d", len(events))
	}
	if events[0].Text != "non-empty" {
		t.Errorf("want %q, got %q", "non-empty", events[0].Text)
	}
}

func TestExtractEvents_ResultMessage(t *testing.T) {
	msg := &ResultMessage{
		Type:       MessageTypeResult,
		Subtype:    "success",
		NumTurns:   5,
		DurationMs: 12345,
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	ev := events[0]
	if ev.Kind != KindTurnEnd {
		t.Errorf("want KindTurnEnd, got %q", ev.Kind)
	}
	if ev.NumTurns != 5 {
		t.Errorf("want NumTurns 5, got %d", ev.NumTurns)
	}
	if ev.DurationMs != 12345 {
		t.Errorf("want DurationMs 12345, got %d", ev.DurationMs)
	}
	if ev.IsError {
		t.Error("want IsError=false for success subtype")
	}
}

func TestExtractEvents_ErrorResult(t *testing.T) {
	msg := &ResultMessage{
		Type:         MessageTypeResult,
		Subtype:      "error",
		ErrorMessage: "context canceled",
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	ev := events[0]
	if !ev.IsError {
		t.Error("want IsError=true for error subtype")
	}
	if ev.ErrorMsg != "context canceled" {
		t.Errorf("want error msg %q, got %q", "context canceled", ev.ErrorMsg)
	}
}

func TestExtractEvents_StreamEventWithDelta(t *testing.T) {
	msg := &StreamEvent{
		Type: MessageTypeStream,
		Event: &DeltaEvent{
			Type: "content_block_delta",
			Delta: &DeltaBody{
				Type: "text_delta",
				Text: "partial",
			},
		},
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	if events[0].Kind != KindText || events[0].Text != "partial" {
		t.Errorf("want KindText %q, got kind=%q text=%q", "partial", events[0].Kind, events[0].Text)
	}
}

func TestExtractEvents_StreamEventNoDelta(t *testing.T) {
	msg := &StreamEvent{Type: MessageTypeStream}
	events := extractEvents(msg)
	if len(events) != 0 {
		t.Errorf("want 0 events for empty stream event, got %d", len(events))
	}
}

func TestExtractEvents_GenericMessage(t *testing.T) {
	msg := &GenericMessage{Type: "system"}
	events := extractEvents(msg)
	if len(events) != 0 {
		t.Errorf("want 0 events for generic message, got %d", len(events))
	}
}

// TestExtractEvents_ResultMessage_CostFromCLIWireFormat verifies that cost and
// token counts are populated from the actual Claude CLI output format, which emits
// total_cost_usd as a top-level field and token counts inside a "usage" object —
// not as a nested "cost" sub-object.
func TestExtractEvents_ResultMessage_CostFromCLIWireFormat(t *testing.T) {
	// Simulate the actual JSON emitted by the CLI (confirmed from live output).
	raw := `{"type":"result","subtype":"success","session_id":"abc","num_turns":1,"duration_ms":3671,"total_cost_usd":0.083861,"usage":{"input_tokens":3,"cache_creation_input_tokens":12498,"cache_read_input_tokens":11217,"output_tokens":5}}`
	msg := parseMessageBestEffort([]byte(raw))
	result, ok := msg.(*ResultMessage)
	if !ok {
		t.Fatalf("want *ResultMessage, got %T", msg)
	}
	if result.TotalCostUSD == 0 {
		t.Error("TotalCostUSD should be non-zero after parsing")
	}
	if result.Usage == nil {
		t.Fatal("Usage should be non-nil after parsing")
	}

	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	ev := events[0]
	if ev.Kind != KindTurnEnd {
		t.Errorf("want KindTurnEnd, got %q", ev.Kind)
	}
	if ev.Cost == nil {
		t.Fatal("Cost should be populated from CLI wire format fields")
	}
	if ev.Cost.TotalCostUSD != 0.083861 {
		t.Errorf("TotalCostUSD: want 0.083861, got %f", ev.Cost.TotalCostUSD)
	}
	// tokens_in = input_tokens(3) + cache_creation(12498) + cache_read(11217) = 23718
	wantTokensIn := 3 + 12498 + 11217
	if ev.Cost.InputTokens != wantTokensIn {
		t.Errorf("InputTokens: want %d, got %d", wantTokensIn, ev.Cost.InputTokens)
	}
	if ev.Cost.OutputTokens != 5 {
		t.Errorf("OutputTokens: want 5, got %d", ev.Cost.OutputTokens)
	}
}

// TestExtractEvents_ResultMessage_LegacyCostField verifies that the legacy nested
// cost field still works if a future CLI version emits it.
func TestExtractEvents_ResultMessage_LegacyCostField(t *testing.T) {
	msg := &ResultMessage{
		Type:    MessageTypeResult,
		Subtype: "success",
		Cost:    &CostInfo{InputTokens: 100, OutputTokens: 50, TotalCostUSD: 0.01},
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event, got %d", len(events))
	}
	ev := events[0]
	if ev.Cost == nil {
		t.Fatal("Cost should be populated from legacy Cost field")
	}
	if ev.Cost.TotalCostUSD != 0.01 {
		t.Errorf("TotalCostUSD: want 0.01, got %f", ev.Cost.TotalCostUSD)
	}
}

func TestExtractEvents_NestedAssistantMessage(t *testing.T) {
	// Test the nested message.content path (as opposed to direct content).
	msg := &AssistantMessage{
		Message: &AssistantMessageInner{
			Content: []ContentBlock{
				{Type: BlockTypeText, Text: "nested text"},
			},
		},
	}
	events := extractEvents(msg)
	if len(events) != 1 {
		t.Fatalf("want 1 event from nested message, got %d", len(events))
	}
	if events[0].Text != "nested text" {
		t.Errorf("want %q, got %q", "nested text", events[0].Text)
	}
}

// TestQueryText_TurnBoundarySeparator verifies that QueryText inserts "\n\n"
// between turns when a result message (KindTurnEnd) is received while text
// has been accumulated. This ensures multi-turn outputs are readable when
// used as prior context in subsequent phases.
func TestQueryText_TurnBoundarySeparator(t *testing.T) {
	dir := t.TempDir()
	script := filepath.Join(dir, "fake-claude.sh")
	// Emit two assistant turns separated by a result message.
	content := `#!/bin/sh
printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"turn one"}]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"s1","num_turns":1,"duration_ms":1}'
printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"turn two"}]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"s2","num_turns":2,"duration_ms":2}'
exit 0
`
	if err := os.WriteFile(script, []byte(content), 0700); err != nil {
		t.Fatalf("write fake cli: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	out, err := QueryText(ctx, "ignored", &AgentOptions{CLIPath: script})
	if err != nil {
		t.Fatalf("QueryText returned error: %v", err)
	}
	want := "turn one\n\nturn two\n\n"
	if out != want {
		t.Errorf("QueryText output = %q; want %q", out, want)
	}
}

func TestQueryText_ReturnsWhenProcessExitsButStdoutPipeStaysOpen(t *testing.T) {
	dir := t.TempDir()
	script := filepath.Join(dir, "fake-claude.sh")
	content := `#!/bin/sh
printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"review ok"}]}'
(sleep 30) &
exit 0
`
	if err := os.WriteFile(script, []byte(content), 0700); err != nil {
		t.Fatalf("write fake cli: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	start := time.Now()
	out, err := QueryText(ctx, "ignored", &AgentOptions{
		CLIPath: script,
	})
	if err != nil {
		t.Fatalf("QueryText returned error: %v", err)
	}
	if out != "review ok" {
		t.Fatalf("QueryText output = %q, want %q", out, "review ok")
	}
	if time.Since(start) > 4*time.Second {
		t.Fatalf("QueryText took too long to return after process exit: %s", time.Since(start))
	}
}

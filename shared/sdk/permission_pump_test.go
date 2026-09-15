package sdk

import (
	"bufio"
	"encoding/json"
	"io"
	"strings"
	"testing"
	"time"
)

// goldenCanUseToolAllow and goldenCanUseToolDeny are the verbatim recv-direction
// control_request lines captured from claude 2.1.207 under
// --permission-prompt-tool stdio. Source of truth:
// cmd/nanika/internal/tui/shell/testdata/claude-control-protocol/{claude-allow,claude-deny}.jsonl
// (the `raw` field of the recv can_use_tool frame). Replaying the real bytes
// keeps this test honest against protocol drift.
const (
	goldenCanUseToolAllow = `{"type":"control_request","request_id":"ff824a16-afb9-46fd-a880-0906152e2ec8","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"curl -s https://example.com","description":"Fetch https://example.com with curl"},"description":"Fetch https://example.com with curl","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"curl -s https://example.com"}],"behavior":"allow","destination":"localSettings"}],"decision_reason":"This command requires approval","decision_reason_type":"other","tool_use_id":"toolu_01KWQx8CUQWMyG8Pen9KsYJ8"}}`
	goldenCanUseToolDeny  = `{"type":"control_request","request_id":"eb68f1e6-126e-4f5e-83f5-23804b0ce81b","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"curl -s https://example.com","description":"Fetch content from example.com"},"description":"Fetch content from example.com","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"curl -s https://example.com"}],"behavior":"allow","destination":"localSettings"}],"decision_reason":"This command requires approval","decision_reason_type":"other","tool_use_id":"toolu_01LpEWVYWnWzNN5WBnFEtrwU"}}`
)

// TestPump_CanUseToolEmitsPermissionRequest golden-replays the captured claude
// can_use_tool control exchange through the real pump and asserts it surfaces a
// KindPermissionRequest StreamedEvent carrying the correlation id, tool name,
// tool_use_id, verbatim input, and decision reason — the approval request the
// executor seam forwards to the gateway.
func TestPump_CanUseToolEmitsPermissionRequest(t *testing.T) {
	t.Parallel()

	r := io.NopCloser(strings.NewReader(goldenCanUseToolAllow + "\n"))
	q := &Query{
		messages: make(chan *StreamedEvent, 4),
		pending:  make(map[string]chan queryCtrlRespBody),
		done:     make(chan struct{}),
	}
	defer close(q.done)
	go q.pump(r)

	select {
	case ev := <-q.messages:
		if ev.Kind != KindPermissionRequest {
			t.Fatalf("kind = %q, want %q", ev.Kind, KindPermissionRequest)
		}
		if ev.Permission == nil {
			t.Fatalf("Permission payload is nil")
		}
		p := ev.Permission
		if p.RequestID != "ff824a16-afb9-46fd-a880-0906152e2ec8" {
			t.Errorf("RequestID = %q", p.RequestID)
		}
		if p.ToolName != "Bash" {
			t.Errorf("ToolName = %q, want Bash", p.ToolName)
		}
		if p.ToolUseID != "toolu_01KWQx8CUQWMyG8Pen9KsYJ8" {
			t.Errorf("ToolUseID = %q", p.ToolUseID)
		}
		if p.Reason != "This command requires approval" {
			t.Errorf("Reason = %q", p.Reason)
		}
		var in struct {
			Command string `json:"command"`
		}
		if err := json.Unmarshal(p.Input, &in); err != nil {
			t.Fatalf("Input not valid JSON: %v", err)
		}
		if in.Command != "curl -s https://example.com" {
			t.Errorf("Input.command = %q", in.Command)
		}
	case <-time.After(time.Second):
		t.Fatal("no permission_request event within 1s")
	}
}

// TestRespondPermission_AllowResumesTurn asserts the allow control_response the
// SDK writes matches the captured wire shape: behavior:allow wrapped in
// success/request_id, correlated on the request id — the frame that resumes the
// gated turn. It also asserts updatedInput is omitted when the caller does not
// mutate the input.
func TestRespondPermission_AllowResumesTurn(t *testing.T) {
	t.Parallel()

	pr, pw := io.Pipe()
	q := &Query{stdin: pw}
	reader := bufio.NewReader(pr)

	go func() {
		_ = q.RespondPermission("ff824a16-afb9-46fd-a880-0906152e2ec8", PermissionDecision{Allow: true})
	}()

	line, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read response line: %v", err)
	}
	inner := decodeResponseInner(t, line, "ff824a16-afb9-46fd-a880-0906152e2ec8")
	if inner["behavior"] != "allow" {
		t.Fatalf("behavior = %v, want allow", inner["behavior"])
	}
	if _, ok := inner["updatedInput"]; ok {
		t.Fatalf("updatedInput must be omitted when nil, got %v", inner["updatedInput"])
	}
}

// TestRespondPermission_DenySurfacesMessage asserts the deny control_response
// carries behavior:deny + the message surfaced to the model — matching the
// captured claude-deny frame that blocks the tool with an is_error result.
func TestRespondPermission_DenySurfacesMessage(t *testing.T) {
	t.Parallel()

	pr, pw := io.Pipe()
	q := &Query{stdin: pw}
	reader := bufio.NewReader(pr)

	const msg = "Denied by probe harness (scripted deny)."
	go func() {
		_ = q.RespondPermission("eb68f1e6-126e-4f5e-83f5-23804b0ce81b", PermissionDecision{Allow: false, Message: msg})
	}()

	line, err := reader.ReadString('\n')
	if err != nil {
		t.Fatalf("read response line: %v", err)
	}
	inner := decodeResponseInner(t, line, "eb68f1e6-126e-4f5e-83f5-23804b0ce81b")
	if inner["behavior"] != "deny" {
		t.Fatalf("behavior = %v, want deny", inner["behavior"])
	}
	if inner["message"] != msg {
		t.Fatalf("message = %v, want %q", inner["message"], msg)
	}
}

// decodeResponseInner unwraps the control_response envelope and returns the
// inner response object, asserting the envelope framing (type, subtype:success,
// correlated request_id) along the way.
func decodeResponseInner(t *testing.T, line, wantReqID string) map[string]any {
	t.Helper()
	var env struct {
		Type     string `json:"type"`
		Response struct {
			Subtype   string         `json:"subtype"`
			RequestID string         `json:"request_id"`
			Response  map[string]any `json:"response"`
		} `json:"response"`
	}
	if err := json.Unmarshal([]byte(strings.TrimSpace(line)), &env); err != nil {
		t.Fatalf("unmarshal control_response: %v (line=%q)", err, line)
	}
	if env.Type != "control_response" {
		t.Fatalf("type = %q, want control_response", env.Type)
	}
	if env.Response.Subtype != "success" {
		t.Fatalf("subtype = %q, want success", env.Response.Subtype)
	}
	if env.Response.RequestID != wantReqID {
		t.Fatalf("request_id = %q, want %q", env.Response.RequestID, wantReqID)
	}
	return env.Response.Response
}

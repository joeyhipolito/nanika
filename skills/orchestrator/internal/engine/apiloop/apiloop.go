package apiloop

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// Run drives the agentic loop for one phase against the configured Provider.
// It returns the final assistant text, an empty session ID (HTTP-API providers
// don't model server-side resume), the aggregated cost, and any error.
//
// The loop performs at most cfg.MaxTurns iterations. On each turn it:
//  1. Streams a single message request.
//  2. If the stop reason is tool_use, dispatches all tool_use blocks in
//     parallel and appends their tool_result blocks as the next user message.
//  3. Otherwise breaks and returns.
//
// Errors with status 401 trigger one OAuth refresh + retry (only when
// Provider.RefreshOn401 says so). Errors with status 429 or 529 are retried
// with exponential backoff (honouring Retry-After when present), capped at
// six attempts per request.
func Run(
	ctx context.Context,
	cfg Config,
	wc *core.WorkerConfig,
	em event.Emitter,
	verbose bool,
) (string, string, *sdk.CostInfo, error) {
	cfg = normalizeConfig(cfg)
	if cfg.Provider == nil {
		return "", "", nil, errors.New("apiloop: nil Provider")
	}

	pc := PhaseCtx{
		MissionID:  wc.Bundle.WorkspaceID,
		PhaseID:    wc.Bundle.PhaseID,
		WorkerName: wc.Name,
	}
	prov := cfg.Provider

	em.Emit(ctx, event.New(event.WorkerSpawned, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
		"model":   wc.Model,
		"runtime": prov.Name(),
		"dir":     wc.WorkerDir,
	}))

	start := cfg.Now()

	maxTurns := wc.MaxTurns
	if maxTurns <= 0 {
		maxTurns = cfg.MaxTurns
	}
	if maxTurns <= 0 {
		maxTurns = 50
	}

	cred, err := cfg.LoadCredential(prov.CredentialProvider())
	if err != nil {
		em.Emit(ctx, event.New(event.WorkerFailed, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
			"error": err.Error(),
		}))
		return "", "", nil, fmt.Errorf("%s: load credential: %w", prov.Name(), err)
	}

	systemPrompt := BuildSystemPrompt(wc)
	messages := []Message{
		{Role: "user", Blocks: []Block{{Type: "text", Text: BuildUserPrompt(wc)}}},
	}

	registry := cfg.Tools
	toolDefs := buildToolDefs(registry)

	var (
		assistantText strings.Builder
		aggCost       sdk.CostInfo
	)

	for turn := 0; turn < maxTurns; turn++ {
		req := Request{
			Model:     prov.ResolveModel(wc.Model),
			MaxTokens: 8192,
			System:    systemPrompt,
			Messages:  messages,
			Tools:     toolDefs,
			Stream:    true,
		}

		result, streamErr := streamWithRetry(ctx, cfg, cred, req, em, pc, &assistantText)
		if streamErr != nil {
			em.Emit(ctx, event.New(event.WorkerFailed, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
				"error": streamErr.Error(),
			}))
			return assistantText.String(), "", &aggCost, fmt.Errorf("%s: stream: %w", prov.Name(), streamErr)
		}

		if result.Usage != nil {
			aggCost.InputTokens += result.Usage.InputTokens
			aggCost.OutputTokens += result.Usage.OutputTokens
			aggCost.CacheCreationTokens += result.Usage.CacheCreationTokens
			aggCost.CacheReadTokens += result.Usage.CacheReadTokens
			aggCost.TotalCostUSD += prov.ComputeCostUSD(result.Model, result.Usage)
		}

		messages = append(messages, Message{Role: "assistant", Blocks: result.Blocks})

		if result.StopReason != "tool_use" {
			break
		}

		toolUses := filterToolUses(result.Blocks)
		if len(toolUses) == 0 {
			break
		}

		results := runToolsParallel(ctx, registry, toolUses, em, pc)
		messages = append(messages, Message{Role: "user", Blocks: results})
	}

	output := assistantText.String()
	duration := cfg.Now().Sub(start)

	if output != "" && wc.WorkerDir != "" {
		outPath := filepath.Join(wc.WorkerDir, "output.md")
		if writeErr := os.WriteFile(outPath, []byte(output), 0600); writeErr != nil && verbose {
			fmt.Printf("[%s] warning: could not write output.md: %v\n", wc.Name, writeErr)
		}
	}

	em.Emit(ctx, event.New(event.WorkerOutput, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))
	em.Emit(ctx, event.New(event.WorkerCompleted, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))

	return output, "", &aggCost, nil
}

// streamWithRetry wraps a single Provider.ParseStream call with the retry loop
// for 401/429/529 status codes. Any other error is returned to the caller.
func streamWithRetry(
	ctx context.Context,
	cfg Config,
	cred *auth.Credential,
	req Request,
	em event.Emitter,
	pc PhaseCtx,
	assistantText *strings.Builder,
) (StreamResult, error) {
	const maxRetries = 6
	refreshed := false

	for attempt := 0; ; attempt++ {
		result, err := doStream(ctx, cfg, cred, req, em, pc, assistantText)
		if err == nil {
			return result, nil
		}
		if errors.Is(err, ctx.Err()) || ctx.Err() != nil {
			return StreamResult{}, ctx.Err()
		}

		var apiErr *APIError
		if !errors.As(err, &apiErr) {
			return StreamResult{}, err
		}

		switch {
		case apiErr.Status == http.StatusUnauthorized && !refreshed && cfg.Provider.RefreshOn401(cred):
			if rerr := cfg.RefreshCredential(ctx, cred); rerr != nil {
				return StreamResult{}, fmt.Errorf("refresh after 401: %w", rerr)
			}
			refreshed = true
			continue

		case apiErr.Status == http.StatusTooManyRequests || apiErr.Status == 529:
			if attempt >= maxRetries {
				return StreamResult{}, err
			}
			delay := apiErr.RetryAfter
			if delay <= 0 {
				delay = cfg.Backoff(attempt)
			}
			select {
			case <-ctx.Done():
				return StreamResult{}, ctx.Err()
			case <-time.After(delay):
			}
			continue

		default:
			return StreamResult{}, err
		}
	}
}

// doStream issues one HTTP request and dispatches the response body to
// Provider.ParseStream. The text-delta callback throttles emission to ~1 Hz.
func doStream(
	ctx context.Context,
	cfg Config,
	cred *auth.Credential,
	req Request,
	em event.Emitter,
	pc PhaseCtx,
	assistantText *strings.Builder,
) (StreamResult, error) {
	prov := cfg.Provider
	body, err := prov.BuildRequest(req)
	if err != nil {
		return StreamResult{}, fmt.Errorf("marshal request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, cfg.BaseURL+prov.Endpoint(), bytes.NewReader(body))
	if err != nil {
		return StreamResult{}, fmt.Errorf("build request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "text/event-stream")
	prov.SetAuthHeaders(httpReq.Header, cred)

	resp, err := cfg.HTTPClient.Do(httpReq)
	if err != nil {
		return StreamResult{}, fmt.Errorf("http: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		raw, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		ae := &APIError{Status: resp.StatusCode, Body: string(raw)}
		if ra := resp.Header.Get("Retry-After"); ra != "" {
			if secs, perr := strconv.Atoi(ra); perr == nil {
				ae.RetryAfter = time.Duration(secs) * time.Second
			}
		}
		return StreamResult{}, ae
	}

	var (
		pendingText strings.Builder
		lastEmit    = time.Now()
	)
	flushText := func() {
		if pendingText.Len() == 0 {
			return
		}
		chunk := pendingText.String()
		pendingText.Reset()
		em.Emit(ctx, event.New(event.WorkerOutput, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
			"chunk":      chunk,
			"event_kind": "text",
			"streaming":  true,
		}))
		lastEmit = time.Now()
	}
	// onText runs synchronously inside Provider.ParseStream's single
	// scanner goroutine — no locking required.
	onText := func(s string) {
		assistantText.WriteString(s)
		pendingText.WriteString(s)
		if time.Since(lastEmit) >= time.Second {
			flushText()
		}
	}

	result, parseErr := prov.ParseStream(ctx, resp.Body, onText)
	flushText()
	if parseErr != nil {
		return StreamResult{}, parseErr
	}
	// Emit a synthetic tool_use marker for each tool block so workers see
	// when the model decides to act.
	for _, b := range result.Blocks {
		if b.Type == "tool_use" {
			em.Emit(ctx, event.New(event.WorkerOutput, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
				"chunk":      fmt.Sprintf("[tool: %s]\n", b.ToolName),
				"event_kind": "tool_use",
				"tool_name":  b.ToolName,
				"streaming":  true,
			}))
		}
	}
	return result, nil
}

// runToolsParallel executes each tool_use block concurrently and gathers the
// results in input order so the assistant's tool_use IDs line up with the
// next user message's tool_result blocks.
func runToolsParallel(
	ctx context.Context,
	reg *tools.Registry,
	uses []Block,
	em event.Emitter,
	pc PhaseCtx,
) []Block {
	results := make([]Block, len(uses))
	var wg sync.WaitGroup
	for i, use := range uses {
		i, use := i, use
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[i] = executeOneTool(ctx, reg, use)
			em.Emit(ctx, event.New(event.WorkerOutput, pc.MissionID, pc.PhaseID, pc.WorkerName, map[string]any{
				"chunk":      fmt.Sprintf("[tool result: %s]\n", use.ToolName),
				"event_kind": "tool_result",
				"tool_name":  use.ToolName,
				"is_error":   results[i].IsError,
			}))
		}()
	}
	wg.Wait()
	return results
}

func executeOneTool(ctx context.Context, reg *tools.Registry, use Block) Block {
	res := Block{Type: "tool_result", ToolUseID: use.ToolUseID}
	if reg == nil {
		res.IsError = true
		res.Output = fmt.Sprintf("no tool registry configured (tool: %s)", use.ToolName)
		return res
	}
	tool := reg.Get(use.ToolName)
	if tool == nil {
		res.IsError = true
		res.Output = fmt.Sprintf("unknown tool: %s", use.ToolName)
		return res
	}
	var args map[string]any
	if len(use.Input) > 0 {
		if err := json.Unmarshal(use.Input, &args); err != nil {
			res.IsError = true
			res.Output = fmt.Sprintf("invalid tool input: %v", err)
			return res
		}
	}
	out, err := tool.Execute(ctx, args)
	if err != nil {
		res.IsError = true
		res.Output = err.Error()
		return res
	}
	res.IsError = out.IsError
	res.Output = out.Content
	return res
}

func filterToolUses(blocks []Block) []Block {
	out := make([]Block, 0, len(blocks))
	for _, b := range blocks {
		if b.Type == "tool_use" {
			out = append(out, b)
		}
	}
	return out
}

func buildToolDefs(reg *tools.Registry) []ToolDef {
	if reg == nil {
		return nil
	}
	all := reg.All()
	defs := make([]ToolDef, 0, len(all))
	for _, t := range all {
		defs = append(defs, ToolDef{
			Name:        t.Name(),
			Description: t.Description(),
			Schema:      t.InputSchema(),
		})
	}
	return defs
}

// BuildSystemPrompt and BuildUserPrompt are exported because the executors'
// parity tests verify prompt threading and several callers compose them
// independently.

// BuildSystemPrompt assembles the system prompt from the persona text, skills
// index, and Nanika preamble. The shape mirrors the Claude Code CLAUDE.md
// injection so parity tests can compare prompts apples-to-apples.
func BuildSystemPrompt(wc *core.WorkerConfig) string {
	var b strings.Builder
	b.WriteString("You are a Nanika orchestrator worker. ")
	b.WriteString("Produce concrete, verifiable artifacts; prefer editing existing files; ")
	b.WriteString("never push, branch, or commit — the orchestrator handles git.\n\n")
	if wc.Bundle.Persona != "" {
		b.WriteString("# Persona\n")
		b.WriteString(wc.Bundle.Persona)
		b.WriteString("\n\n")
	}
	if len(wc.Bundle.Skills) > 0 {
		b.WriteString("# Skills available\n")
		for _, s := range wc.Bundle.Skills {
			fmt.Fprintf(&b, "- %s\n", s.Name)
		}
		b.WriteString("\n")
	}
	if wc.WorkerDir != "" {
		fmt.Fprintf(&b, "# Workspace\nWorker directory: %s\n", wc.WorkerDir)
	}
	return b.String()
}

// BuildUserPrompt frames the phase objective with any dependency outputs.
func BuildUserPrompt(wc *core.WorkerConfig) string {
	const maxPriorLen = 8000
	var b strings.Builder
	b.WriteString("You will be given source material from prior phase output below, followed by a task.\n\n")
	if pc := wc.Bundle.PriorContext; pc != "" {
		if len(pc) > maxPriorLen {
			pc = pc[:maxPriorLen] + fmt.Sprintf("\n[Note: Prior context truncated; original was %d characters]", len(wc.Bundle.PriorContext))
		}
		b.WriteString("<prior_phase_output>\n")
		b.WriteString(pc)
		b.WriteString("\n</prior_phase_output>\n\n")
	}
	b.WriteString("Task: ")
	b.WriteString(wc.Bundle.Objective)
	return b.String()
}

// DefaultBackoff is the production exponential-backoff schedule: 1s, 2s, 4s,
// …, capped at 30s. Tests typically inject a tighter schedule via Config.
func DefaultBackoff(attempt int) time.Duration {
	d := time.Duration(1<<attempt) * time.Second
	if d > 30*time.Second {
		d = 30 * time.Second
	}
	return d
}

// SharedRegistry returns a process-wide tool registry that lazy-loads on first
// use. All API executors registered by defaultRegistry() share this cache so
// plugin discovery (subprocess --help-json fan-out) runs once per process,
// not once per executor.
var (
	sharedToolsOnce sync.Once
	sharedToolsReg  *tools.Registry
)

func SharedRegistry(ctx context.Context) *tools.Registry {
	sharedToolsOnce.Do(func() {
		sharedToolsReg = tools.Load(ctx)
	})
	return sharedToolsReg
}

func normalizeConfig(cfg Config) Config {
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Timeout: 0}
	}
	if cfg.LoadCredential == nil {
		cfg.LoadCredential = auth.LoadCredential
	}
	if cfg.RefreshCredential == nil {
		cfg.RefreshCredential = auth.RefreshOAuth
	}
	if cfg.Backoff == nil {
		cfg.Backoff = DefaultBackoff
	}
	if cfg.Now == nil {
		cfg.Now = time.Now
	}
	return cfg
}

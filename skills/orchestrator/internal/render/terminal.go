// Package render provides terminal-oriented rendering of orchestrator events.
package render

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// color constants — empty when the output is not a TTY.
type palette struct {
	Reset   string
	Bold    string
	Dim     string
	Cyan    string
	Green   string
	Yellow  string
	Red     string
	Blue    string
	Magenta string
}

func detectPalette(w *os.File) palette {
	fi, err := w.Stat()
	if err != nil || fi.Mode()&os.ModeCharDevice == 0 {
		return palette{} // not a TTY — no colors
	}
	return palette{
		Reset:   "\033[0m",
		Bold:    "\033[1m",
		Dim:     "\033[2m",
		Cyan:    "\033[36m",
		Green:   "\033[32m",
		Yellow:  "\033[33m",
		Red:     "\033[31m",
		Blue:    "\033[34m",
		Magenta: "\033[35m",
	}
}

// TerminalRenderer is an event.Emitter that prints formatted mission progress
// to stderr. It is designed to be composed into a MultiEmitter alongside the
// file/UDS emitters so every event is both persisted and displayed.
//
// When Verbose is true, each event is also printed in the raw format used by
// `orchestrator events replay` (timestamp + type + data).
type TerminalRenderer struct {
	w       io.Writer
	c       palette
	verbose bool

	mu          sync.Mutex
	phaseStarts map[string]time.Time // phaseID → start time
	phaseMeta   map[string]phaseMeta // phaseID → metadata

	// phase outcome counters for mission summary
	completed int
	failed    int
	skipped   int
}

type phaseMeta struct {
	name    string
	persona string
}

// NewTerminalRenderer creates a renderer writing to stderr.
// Pass verbose=true to additionally print the full event stream.
func NewTerminalRenderer(verbose bool) *TerminalRenderer {
	stderr := os.Stderr
	return &TerminalRenderer{
		w:           stderr,
		c:           detectPalette(stderr),
		verbose:     verbose,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}
}

// Emit handles a single event and prints the appropriate terminal output.
func (r *TerminalRenderer) Emit(_ context.Context, ev event.Event) {
	r.mu.Lock()
	defer r.mu.Unlock()

	if r.verbose {
		r.printRawEvent(ev)
	}

	switch ev.Type {
	case event.DecomposeStarted:
		r.printDecomposeStarted(ev)
	case event.DecomposeCompleted:
		r.printDecomposeCompleted(ev)
	case event.DecomposeFallback:
		r.printDecomposeFallback(ev)

	case event.MissionStarted:
		r.printMissionStarted(ev)
	case event.MissionCompleted:
		r.printMissionCompleted(ev)
	case event.MissionFailed:
		r.printMissionFailed(ev)
	case event.MissionCancelled:
		r.printMissionCancelled(ev)

	case event.PhaseStarted:
		r.printPhaseStarted(ev)
	case event.PhaseCompleted:
		r.printPhaseCompleted(ev)
	case event.PhaseFailed:
		r.printPhaseFailed(ev)
	case event.PhaseSkipped:
		r.printPhaseSkipped(ev)
	case event.PhaseRetrying:
		r.printPhaseRetrying(ev)

	case event.WorkerOutput:
		r.printWorkerOutput(ev)

	case event.GitWorktreeCreated:
		r.printGitWorktreeCreated(ev)
	case event.GitCommitted:
		r.printGitCommitted(ev)
	case event.GitPRCreated:
		r.printGitPRCreated(ev)

	case event.FileOverlapDetected:
		r.printFileOverlapDetected(ev)

	case event.SystemError:
		r.printSystemError(ev)
	}
}

// Close is a no-op; TerminalRenderer does not own the writer.
func (r *TerminalRenderer) Close() error { return nil }

// --- decomposition --------------------------------------------------------

func (r *TerminalRenderer) printDecomposeStarted(ev event.Event) {
	summary := dataStr(ev.Data, "task_summary")
	if summary == "" {
		summary = "task"
	}
	if len(summary) > 80 {
		summary = summary[:77] + "..."
	}
	fmt.Fprintf(r.w, "%s%s▸ decomposing:%s %s\n", r.c.Bold, r.c.Cyan, r.c.Reset, summary)
}

func (r *TerminalRenderer) printDecomposeCompleted(ev event.Event) {
	count := dataInt(ev.Data, "phase_count")
	mode := dataStr(ev.Data, "execution_mode")

	fmt.Fprintf(r.w, "%s%s▸ plan:%s %d phase(s), %s\n", r.c.Bold, r.c.Cyan, r.c.Reset, count, mode)

	// Print phase table
	if phases, ok := ev.Data["phases"].([]any); ok {
		for i, raw := range phases {
			pm, ok := raw.(map[string]any)
			if !ok {
				continue
			}
			name := strVal(pm, "name")
			persona := strVal(pm, "persona")
			idx := fmt.Sprintf("%d", i+1)
			fmt.Fprintf(r.w, "  %s%s%s. %s %s(%s)%s\n",
				r.c.Dim, idx, r.c.Reset,
				name,
				r.c.Dim, persona, r.c.Reset)
		}
		fmt.Fprintln(r.w)
	}
}

func (r *TerminalRenderer) printDecomposeFallback(ev event.Event) {
	reason := dataStr(ev.Data, "reason")
	fmt.Fprintf(r.w, "%s%s▸ decompose fallback:%s %s\n", r.c.Bold, r.c.Yellow, r.c.Reset, reason)
}

// --- mission lifecycle ----------------------------------------------------

func (r *TerminalRenderer) printMissionStarted(ev event.Event) {
	phases := dataInt(ev.Data, "phases")
	mode := dataStr(ev.Data, "execution_mode")
	fmt.Fprintf(r.w, "%s%s▸ mission started:%s %d phase(s), %s\n\n",
		r.c.Bold, r.c.Cyan, r.c.Reset, phases, mode)
}

func (r *TerminalRenderer) printMissionCompleted(ev event.Event) {
	dur := dataStr(ev.Data, "duration")
	artifacts := dataInt(ev.Data, "artifacts")
	fmt.Fprintf(r.w, "\n%s%s✔ mission completed%s in %s", r.c.Bold, r.c.Green, r.c.Reset, dur)
	if artifacts > 0 {
		fmt.Fprintf(r.w, " (%d artifact(s))", artifacts)
	}
	fmt.Fprintln(r.w)
	r.printPhaseSummary()
}

func (r *TerminalRenderer) printMissionFailed(ev event.Event) {
	dur := dataStr(ev.Data, "duration")
	errMsg := dataStr(ev.Data, "error")
	fmt.Fprintf(r.w, "\n%s%s✘ mission failed%s in %s: %s\n", r.c.Bold, r.c.Red, r.c.Reset, dur, errMsg)
	r.printPhaseSummary()
}

func (r *TerminalRenderer) printMissionCancelled(ev event.Event) {
	dur := dataStr(ev.Data, "duration")
	fmt.Fprintf(r.w, "\n%s%s⊘ mission cancelled%s after %s\n", r.c.Bold, r.c.Yellow, r.c.Reset, dur)
	r.printPhaseSummary()
}

func (r *TerminalRenderer) printPhaseSummary() {
	if r.completed+r.failed+r.skipped == 0 {
		return
	}
	fmt.Fprintf(r.w, "  phases: %d completed, %d failed, %d skipped\n", r.completed, r.failed, r.skipped)
}

// --- phase lifecycle ------------------------------------------------------

func (r *TerminalRenderer) printPhaseStarted(ev event.Event) {
	name := dataStr(ev.Data, "name")
	persona := dataStr(ev.Data, "persona")

	r.phaseStarts[ev.PhaseID] = ev.Timestamp
	r.phaseMeta[ev.PhaseID] = phaseMeta{name: name, persona: persona}

	fmt.Fprintf(r.w, "%s%s▶ %s%s %s(%s)%s\n",
		r.c.Bold, r.c.Blue, name, r.c.Reset,
		r.c.Dim, persona, r.c.Reset)
}

func (r *TerminalRenderer) printPhaseCompleted(ev event.Event) {
	r.completed++
	meta := r.phaseMeta[ev.PhaseID]
	dur := r.phaseDuration(ev)
	retries := dataInt(ev.Data, "retries")

	suffix := ""
	if retries > 0 {
		suffix = fmt.Sprintf(" (%d retries)", retries)
	}

	fmt.Fprintf(r.w, "%s%s✔ %s%s completed in %s%s\n",
		r.c.Bold, r.c.Green, meta.name, r.c.Reset, dur, suffix)
}

func (r *TerminalRenderer) printPhaseFailed(ev event.Event) {
	r.failed++
	meta := r.phaseMeta[ev.PhaseID]
	dur := r.phaseDuration(ev)
	errMsg := dataStr(ev.Data, "error")

	name := meta.name
	if name == "" {
		name = ev.PhaseID
	}

	fmt.Fprintf(r.w, "%s%s✘ %s%s failed after %s: %s\n",
		r.c.Bold, r.c.Red, name, r.c.Reset, dur, errMsg)
}

func (r *TerminalRenderer) printPhaseSkipped(ev event.Event) {
	r.skipped++
	name := dataStr(ev.Data, "name")
	reason := dataStr(ev.Data, "reason")
	if name == "" {
		name = ev.PhaseID
	}
	fmt.Fprintf(r.w, "%s%s⊘ %s%s skipped", r.c.Bold, r.c.Yellow, name, r.c.Reset)
	if reason != "" {
		fmt.Fprintf(r.w, ": %s", reason)
	}
	fmt.Fprintln(r.w)
}

func (r *TerminalRenderer) printPhaseRetrying(ev event.Event) {
	meta := r.phaseMeta[ev.PhaseID]
	attempt := dataInt(ev.Data, "attempt")
	backoff := dataStr(ev.Data, "backoff")
	errMsg := dataStr(ev.Data, "error")

	name := meta.name
	if name == "" {
		name = ev.PhaseID
	}

	fmt.Fprintf(r.w, "%s%s↻ %s%s attempt %d failed (%s), retrying in %s\n",
		r.c.Bold, r.c.Yellow, name, r.c.Reset, attempt, errMsg, backoff)
}

// --- worker output --------------------------------------------------------

func (r *TerminalRenderer) printWorkerOutput(ev event.Event) {
	chunk := dataStr(ev.Data, "chunk")
	if chunk == "" {
		return
	}
	kind := dataStr(ev.Data, "event_kind")
	if kind == "tool_use" || kind == "tool_result" {
		// Tools are noisy — show a compact indicator
		tool := dataStr(ev.Data, "tool_name")
		if tool != "" {
			fmt.Fprintf(r.w, "  %s⚙ %s%s\n", r.c.Dim, tool, r.c.Reset)
		}
		return
	}
	// Stream text chunks — indent and dim to distinguish from orchestrator output
	for _, line := range strings.Split(chunk, "\n") {
		if line == "" {
			continue
		}
		fmt.Fprintf(r.w, "  %s%s%s\n", r.c.Dim, line, r.c.Reset)
	}
}

// --- git events -----------------------------------------------------------

func (r *TerminalRenderer) printGitWorktreeCreated(ev event.Event) {
	branch := dataStr(ev.Data, "branch")
	fmt.Fprintf(r.w, "%s%s▸ git:%s worktree created (branch: %s)\n",
		r.c.Bold, r.c.Magenta, r.c.Reset, branch)
}

func (r *TerminalRenderer) printGitCommitted(ev event.Event) {
	sha := dataStr(ev.Data, "sha")
	if len(sha) > 8 {
		sha = sha[:8]
	}
	fmt.Fprintf(r.w, "%s%s▸ git:%s committed %s\n",
		r.c.Bold, r.c.Magenta, r.c.Reset, sha)
}

func (r *TerminalRenderer) printGitPRCreated(ev event.Event) {
	url := dataStr(ev.Data, "pr_url")
	fmt.Fprintf(r.w, "%s%s▸ git:%s PR created → %s\n",
		r.c.Bold, r.c.Magenta, r.c.Reset, url)
}

// --- system ---------------------------------------------------------------

func (r *TerminalRenderer) printFileOverlapDetected(ev event.Event) {
	file := dataStr(ev.Data, "file")
	severity := dataStr(ev.Data, "severity")
	var phases []string
	if raw, ok := ev.Data["phases"].([]any); ok {
		for _, p := range raw {
			if s, ok := p.(string); ok {
				phases = append(phases, s)
			}
		}
	}
	fmt.Fprintf(r.w, "%s%s⚠ file_overlap [%s]:%s %s — phases: %s\n",
		r.c.Bold, r.c.Yellow, severity, r.c.Reset, file, strings.Join(phases, ", "))
}

func (r *TerminalRenderer) printSystemError(ev event.Event) {
	errMsg := dataStr(ev.Data, "error")
	fmt.Fprintf(r.w, "%s%s✘ system error:%s %s\n", r.c.Bold, r.c.Red, r.c.Reset, errMsg)
}

// --- verbose raw event output ---------------------------------------------

func (r *TerminalRenderer) printRawEvent(ev event.Event) {
	ts := ev.Timestamp.Local().Format("15:04:05.000")
	seq := fmt.Sprintf("%4d", ev.Sequence)

	var parts []string
	if ev.PhaseID != "" {
		parts = append(parts, "phase="+ev.PhaseID)
	}
	if ev.WorkerID != "" {
		parts = append(parts, "worker="+ev.WorkerID)
	}
	for k, v := range ev.Data {
		parts = append(parts, fmt.Sprintf("%s=%v", k, v))
	}

	ctx := ""
	if len(parts) > 0 {
		ctx = "  " + strings.Join(parts, " ")
	}

	fmt.Fprintf(r.w, "%s%s %s %-30s%s%s\n", r.c.Dim, ts, seq, ev.Type, ctx, r.c.Reset)
}

// --- helpers --------------------------------------------------------------

func (r *TerminalRenderer) phaseDuration(ev event.Event) string {
	if start, ok := r.phaseStarts[ev.PhaseID]; ok {
		return ev.Timestamp.Sub(start).Round(time.Second).String()
	}
	return "?"
}

func dataStr(data map[string]any, key string) string {
	if data == nil {
		return ""
	}
	v, ok := data[key]
	if !ok {
		return ""
	}
	s, ok := v.(string)
	if ok {
		return s
	}
	return fmt.Sprintf("%v", v)
}

func dataInt(data map[string]any, key string) int {
	if data == nil {
		return 0
	}
	v, ok := data[key]
	if !ok {
		return 0
	}
	switch n := v.(type) {
	case int:
		return n
	case int64:
		return int(n)
	case float64:
		return int(n)
	default:
		return 0
	}
}

func strVal(m map[string]any, key string) string {
	v, ok := m[key]
	if !ok {
		return ""
	}
	s, ok := v.(string)
	if ok {
		return s
	}
	return fmt.Sprintf("%v", v)
}

package main

import (
	"bufio"
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/joeyhipolito/nanika-dashboard/internal"
	_ "modernc.org/sqlite"
)

// App holds the application state and exposes methods bound to the Wails frontend.
// Accessible in the browser as window.go.main.App.
type App struct {
	ctx context.Context
}

func NewApp() *App {
	return &App{}
}

func (a *App) startup(ctx context.Context) {
	a.ctx = ctx
	a.startEventBridge(ctx)
	a.startLiveBridge(ctx)
}

// PluginManifest mirrors the shape of plugin.json.
type PluginManifest struct {
	Name         string                 `json:"name"`
	Version      string                 `json:"version"`
	APIVersion   int                    `json:"api_version"`
	Description  string                 `json:"description"`
	Binary       string                 `json:"binary"`
	Provides     []string               `json:"provides"`
	Actions      map[string]interface{} `json:"actions"`
	Tags         []string               `json:"tags"`
	Icon         string                 `json:"icon,omitempty"`
	UI           bool                   `json:"ui,omitempty"`
	Capabilities json.RawMessage        `json:"capabilities,omitempty"`
}

// EnableInteraction sets the interactive region of the full-screen overlay in
// AppKit screen coordinates (origin at bottom-left of primary display, points).
// Called from the frontend when the command palette or a module panel becomes
// visible so that mouse events in that region reach the overlay.
func (a *App) EnableInteraction(x, y, w, h float64) {
	internal.EnableInteraction(x, y, w, h)
}

// DisableInteraction re-enables full click-through so every mouse event passes
// to the app beneath the overlay. Called when the palette and all panels close.
func (a *App) DisableInteraction() {
	internal.DisableInteraction()
}

// ListPlugins scans ~/nanika/plugins/*/plugin.json and returns all plugins with api_version >= 1.
func (a *App) ListPlugins() ([]PluginManifest, error) {
	home, _ := os.UserHomeDir()
	pluginsDir := filepath.Join(home, "nanika", "plugins")
	entries, err := os.ReadDir(pluginsDir)
	if err != nil {
		return nil, fmt.Errorf("read plugins dir: %w", err)
	}

	var plugins []PluginManifest
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		pj := filepath.Join(pluginsDir, e.Name(), "plugin.json")
		data, err := os.ReadFile(pj)
		if err != nil {
			continue
		}
		var m PluginManifest
		if err := json.Unmarshal(data, &m); err != nil {
			continue
		}
		if m.APIVersion < 1 {
			continue
		}
		if m.Name == "" {
			m.Name = e.Name()
		}
		plugins = append(plugins, m)
	}
	return plugins, nil
}

// GetPluginUIBundle reads ~/nanika/plugins/<name>/ui/dist/index.js and returns
// the JS source so the frontend can load it via a blob URL + dynamic import().
func (a *App) GetPluginUIBundle(name string) (string, error) {
	home, _ := os.UserHomeDir()
	bundlePath := filepath.Join(home, "nanika", "plugins", name, "ui", "dist", "index.js")
	data, err := os.ReadFile(bundlePath)
	if err != nil {
		return "", fmt.Errorf("plugin ui bundle not found for %s: %w", name, err)
	}
	return string(data), nil
}

// QueryPluginStatus execs `<binary> query status --json` and returns the raw JSON string.
func (a *App) QueryPluginStatus(name string) (string, error) {
	return execPluginQuery(name, "status")
}

// QueryPluginItems execs `<binary> query items --json` and returns the raw JSON string.
func (a *App) QueryPluginItems(name string) (string, error) {
	return execPluginQuery(name, "items")
}

// PluginAction execs `<binary> query action <verb> [<id>] --json` and returns the raw JSON string.
func (a *App) PluginAction(name, verb, id string) (string, error) {
	binary := resolvePluginBinary(name)
	if binary == "" {
		return "", fmt.Errorf("plugin binary not found: %s", name)
	}

	args := []string{"query", "action", verb}
	if id != "" {
		args = append(args, id)
	}
	args = append(args, "--json")

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, binary, args...)
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("plugin action %s %s: %w", name, verb, err)
	}
	return string(out), nil
}

func execPluginQuery(name, queryType string) (string, error) {
	binary := resolvePluginBinary(name)
	if binary == "" {
		return "", fmt.Errorf("plugin binary not found: %s", name)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, binary, "query", queryType, "--json")
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("plugin query %s %s: %w", name, queryType, err)
	}
	return string(out), nil
}

func resolvePluginBinary(name string) string {
	home, _ := os.UserHomeDir()

	// Try plugin.json binary field first.
	pj := filepath.Join(home, "nanika", "plugins", name, "plugin.json")
	if data, err := os.ReadFile(pj); err == nil {
		var m struct {
			Binary string `json:"binary"`
		}
		if json.Unmarshal(data, &m) == nil && m.Binary != "" {
			if path, err := exec.LookPath(m.Binary); err == nil {
				return path
			}
		}
	}

	// Fallback: ~/nanika/bin/<name>
	binPath := filepath.Join(home, "nanika", "bin", name)
	if _, err := os.Stat(binPath); err == nil {
		return binPath
	}
	return ""
}

// ── Helpers ────────────────────────────────────────────────────────────────────

// configDir returns the base config directory (~/.alluka), respecting
// ALLUKA_HOME and ORCHESTRATOR_CONFIG_DIR overrides.
func configDir() string {
	for _, env := range []string{"ORCHESTRATOR_CONFIG_DIR", "ALLUKA_HOME"} {
		if d := os.Getenv(env); d != "" {
			return d
		}
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka")
}

// execOrchestrator runs an orchestrator CLI command and returns its stdout.
func execOrchestrator(timeout time.Duration, args ...string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, "orchestrator", args...)
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return "", fmt.Errorf("orchestrator %s: %s", strings.Join(args, " "), strings.TrimSpace(string(ee.Stderr)))
		}
		return "", fmt.Errorf("orchestrator %s: %w", strings.Join(args, " "), err)
	}
	return string(out), nil
}

// enrichedEnv returns os.Environ() augmented with common user bin directories
// so that helper binaries (orchestrator, plugin CLIs) are reachable when the
// app is launched as a macOS .app bundle and doesn't inherit the shell PATH.
func enrichedEnv() []string {
	home, _ := os.UserHomeDir()
	extraPaths := []string{
		filepath.Join(home, "bin"),
		filepath.Join(home, ".local", "bin"),
		filepath.Join(home, "go", "bin"),
		"/opt/homebrew/bin",
		"/usr/local/bin",
	}

	env := os.Environ()
	// Find and augment the existing PATH entry.
	for i, e := range env {
		if strings.HasPrefix(e, "PATH=") {
			current := e[5:]
			env[i] = "PATH=" + strings.Join(extraPaths, ":") + ":" + current
			return env
		}
	}
	// No PATH in env (unusual) — set one from scratch.
	env = append(env, "PATH="+strings.Join(extraPaths, ":")+":"+os.Getenv("PATH"))
	return env
}

// sanitizeID rejects path-traversal characters in IDs from the frontend.
func sanitizeID(id string) error {
	if id == "" {
		return fmt.Errorf("id is required")
	}
	if strings.ContainsAny(id, "/\\") || strings.Contains(id, "..") {
		return fmt.Errorf("invalid id: %s", id)
	}
	return nil
}

// ── Local plan/event types (mirrors orchestrator internal types) ───────────────
// These types replicate the shapes from skills/orchestrator/internal/core and
// skills/orchestrator/internal/event so we can parse their files without
// importing those packages (the dashboard is a standalone module).

type orchEvent struct {
	ID        string         `json:"id"`
	Type      string         `json:"type"`
	Timestamp time.Time      `json:"timestamp"`
	Sequence  int64          `json:"sequence"`
	MissionID string         `json:"mission_id"`
	PhaseID   string         `json:"phase_id,omitempty"`
	WorkerID  string         `json:"worker_id,omitempty"`
	Data      map[string]any `json:"data,omitempty"`
}

type orchPhase struct {
	ID           string   `json:"id"`
	Name         string   `json:"name"`
	Persona      string   `json:"persona"`
	Skills       []string `json:"skills"`
	Status       string   `json:"status"`
	Dependencies []string `json:"dependencies"`
}

type orchPlan struct {
	ID     string       `json:"id"`
	Task   string       `json:"task"`
	Phases []*orchPhase `json:"phases"`
}

// orchCheckpoint is the minimal subset of checkpoint.json we need.
type orchCheckpoint struct {
	WorkspaceID string    `json:"workspace_id"`
	Domain      string    `json:"domain"`
	Plan        *orchPlan `json:"plan"`
	Status      string    `json:"status"`
	StartedAt   time.Time `json:"started_at"`
}

// orchCheckpointEnvelope wraps checkpoint in an envelope (v2 format).
type orchCheckpointEnvelope struct {
	Version int            `json:"version"`
	Payload orchCheckpoint `json:"payload"`
}

// loadLocalCheckpoint reads ~/.alluka/workspaces/<id>/checkpoint.json.
// Supports both the v2 envelope format and the legacy direct format.
func loadLocalCheckpoint(id string) *orchCheckpoint {
	data, err := os.ReadFile(filepath.Join(configDir(), "workspaces", id, "checkpoint.json"))
	if err != nil {
		return nil
	}
	// Try legacy direct format first.
	var cp orchCheckpoint
	if json.Unmarshal(data, &cp) == nil && cp.WorkspaceID != "" {
		return &cp
	}
	// Try envelope format.
	var env orchCheckpointEnvelope
	if json.Unmarshal(data, &env) == nil && env.Payload.WorkspaceID != "" {
		return &env.Payload
	}
	return nil
}

// ── Event sanitization (mirrors event.Sanitize from the orchestrator) ─────────

var (
	reAPIKey      = regexp.MustCompile(`(?:sk-ant-|ghp_|gho_|glpat-|AKIA|xoxb-|xoxp-)[A-Za-z0-9_\-]+`)
	rePrivateKey  = regexp.MustCompile(`-----BEGIN[^\r\n]*`)
	reBearerBasic = regexp.MustCompile(`(?i)(?:Bearer|Basic)\s+[A-Za-z0-9+/=._\-]{8,}`)

	sanitizeStripKeys = map[string]bool{
		"dir":   true,
		"error": true,
	}
	sanitizeSafeKeys = map[string]bool{
		"task": true,
	}
)

func isSensitiveKey(k string) bool {
	if sanitizeStripKeys[k] {
		return true
	}
	lower := strings.ToLower(k)
	for _, pat := range []string{"password", "secret", "token", "api_key", "apikey", "credential", "auth"} {
		if strings.Contains(lower, pat) {
			return true
		}
	}
	return false
}

func shannonEntropy(s string) float64 {
	if len(s) == 0 {
		return 0
	}
	runes := []rune(s)
	n := float64(len(runes))
	freq := make(map[rune]float64, len(runes))
	for _, r := range runes {
		freq[r]++
	}
	var h float64
	for _, count := range freq {
		p := count / n
		h -= p * math.Log2(p)
	}
	return h
}

func isHighEntropy(s string) bool {
	return len(s) > 20 && shannonEntropy(s) > 4.5
}

func sanitizeStringValue(s string) string {
	s = reAPIKey.ReplaceAllString(s, "[REDACTED]")
	s = rePrivateKey.ReplaceAllString(s, "[REDACTED]")
	s = reBearerBasic.ReplaceAllString(s, "[REDACTED]")
	if !strings.Contains(s, "[REDACTED]") && isHighEntropy(s) {
		return "[REDACTED]"
	}
	for _, tok := range strings.Fields(s) {
		if !strings.Contains(tok, "[REDACTED]") && isHighEntropy(tok) {
			return "[REDACTED]"
		}
	}
	return s
}

func sanitizeStringSafe(s string) string {
	s = reAPIKey.ReplaceAllString(s, "[REDACTED]")
	s = rePrivateKey.ReplaceAllString(s, "[REDACTED]")
	s = reBearerBasic.ReplaceAllString(s, "[REDACTED]")
	return s
}

func sanitizeAnyValue(k string, v any) any {
	switch val := v.(type) {
	case string:
		if sanitizeSafeKeys[k] {
			return sanitizeStringSafe(val)
		}
		return sanitizeStringValue(val)
	case map[string]any:
		return sanitizeDataMap(val)
	case []any:
		result := make([]any, len(val))
		for i, elem := range val {
			result[i] = sanitizeAnyValue("", elem)
		}
		return result
	default:
		return v
	}
}

func sanitizeDataMap(data map[string]any) map[string]any {
	if data == nil {
		return nil
	}
	out := make(map[string]any, len(data))
	for k, v := range data {
		out[k] = sanitizeAnyValue(k, v)
	}
	return out
}

func sanitizeEvent(ev orchEvent) orchEvent {
	if len(ev.Data) == 0 {
		return ev
	}
	filtered := make(map[string]any, len(ev.Data))
	for k, v := range ev.Data {
		if !isSensitiveKey(k) {
			filtered[k] = sanitizeAnyValue(k, v)
		}
	}
	if len(filtered) == 0 {
		filtered = nil
	}
	ev.Data = filtered
	return ev
}

// ── Missions ──────────────────────────────────────────────────────────────────

// ListMissions reads ~/.alluka/events/*.jsonl and enriches each entry with
// status, task, and phase count from the workspace checkpoint.
func (a *App) ListMissions() (string, error) {
	dir := filepath.Join(configDir(), "events")
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return "[]", nil
		}
		return "", fmt.Errorf("reading events dir: %w", err)
	}

	type entry struct {
		MissionID  string    `json:"mission_id"`
		Status     string    `json:"status,omitempty"`
		Task       string    `json:"task,omitempty"`
		Phases     int       `json:"phases"`
		EventCount int       `json:"event_count"`
		SizeBytes  int64     `json:"size_bytes"`
		ModifiedAt time.Time `json:"modified_at"`
	}
	missions := make([]entry, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		mid := strings.TrimSuffix(e.Name(), ".jsonl")
		fpath := filepath.Join(dir, e.Name())
		ent := entry{
			MissionID:  mid,
			EventCount: countJSONLLines(fpath),
			SizeBytes:  info.Size(),
			ModifiedAt: info.ModTime(),
		}
		// Enrich from checkpoint if available.
		if cp := loadLocalCheckpoint(mid); cp != nil {
			ent.Status = cp.Status
			if cp.Plan != nil {
				ent.Task = cp.Plan.Task
				ent.Phases = len(cp.Plan.Phases)
			}
		}
		missions = append(missions, ent)
	}
	data, err := json.Marshal(missions)
	if err != nil {
		return "", fmt.Errorf("marshalling missions: %w", err)
	}
	return string(data), nil
}

// GetMission reads a specific JSONL event log and returns its sanitized events as a JSON array.
// Events are sanitized to strip sensitive fields (tokens, paths, credentials) before
// sending them to the frontend, matching the daemon's handleReplayMission behavior.
func (a *App) GetMission(id string) (string, error) {
	if err := sanitizeID(id); err != nil {
		return "", err
	}
	f, err := os.Open(filepath.Join(configDir(), "events", id+".jsonl"))
	if err != nil {
		return "", fmt.Errorf("opening mission %s: %w", id, err)
	}
	defer f.Close()

	events := make([]orchEvent, 0)
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" {
			continue
		}
		var ev orchEvent
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue // skip malformed lines
		}
		events = append(events, sanitizeEvent(ev))
	}
	if err := sc.Err(); err != nil {
		return "", fmt.Errorf("reading mission %s: %w", id, err)
	}
	data, err := json.Marshal(events)
	if err != nil {
		return "", fmt.Errorf("marshalling events: %w", err)
	}
	return string(data), nil
}

// runMissionOptions mirrors the frontend RunMissionOptions shape.
type runMissionOptions struct {
	Domain string           `json:"domain"`
	Flags  *runMissionFlags `json:"flags"`
}

type runMissionFlags struct {
	NoReview   bool   `json:"no_review"`
	NoGit      bool   `json:"no_git"`
	Sequential bool   `json:"sequential"`
	Model      string `json:"model"`
}

// RunMission starts `orchestrator run <task>` in the background and returns
// a response matching the daemon's shape: {request_id, status: "accepted", task}.
// opts is a JSON-encoded RunMissionOptions (may be empty "{}").
func (a *App) RunMission(task string, optsJSON string) (string, error) {
	if task == "" {
		return "", fmt.Errorf("task is required")
	}

	var opts runMissionOptions
	if optsJSON != "" && optsJSON != "{}" {
		_ = json.Unmarshal([]byte(optsJSON), &opts)
	}

	// Build: orchestrator [--domain d] [--sequential] [--model m] run [--no-review] [--no-git] <task>
	var args []string
	if opts.Domain != "" {
		args = append(args, "--domain", opts.Domain)
	}
	if opts.Flags != nil {
		if opts.Flags.Sequential {
			args = append(args, "--sequential")
		}
		if opts.Flags.Model != "" {
			args = append(args, "--model", opts.Flags.Model)
		}
	}
	args = append(args, "run")
	if opts.Flags != nil {
		if opts.Flags.NoReview {
			args = append(args, "--no-review")
		}
		if opts.Flags.NoGit {
			args = append(args, "--no-git")
		}
	}
	args = append(args, task)

	cmd := exec.Command("orchestrator", args...)
	cmd.Env = enrichedEnv()
	if err := cmd.Start(); err != nil {
		return "", fmt.Errorf("starting mission: %w", err)
	}
	go cmd.Wait() // reap child process

	requestID := generateRequestID()
	result, _ := json.Marshal(map[string]string{
		"request_id": requestID,
		"status":     "accepted",
		"task":       task,
	})
	return string(result), nil
}

// generateRequestID returns an opaque correlation ID in YYYYMMDD-hex format.
func generateRequestID() string {
	ts := time.Now().Format("20060102")
	b := make([]byte, 4)
	rand.Read(b) //nolint:errcheck
	return fmt.Sprintf("%s-%s", ts, hex.EncodeToString(b))
}

// CancelMission execs `orchestrator cancel <workspaceID>` and returns a
// structured response matching the daemon's cancel shape.
func (a *App) CancelMission(workspaceID string) (string, error) {
	if workspaceID == "" {
		return "", fmt.Errorf("workspace ID is required")
	}
	_, err := execOrchestrator(30*time.Second, "cancel", workspaceID)
	if err != nil {
		return "", err
	}
	result, _ := json.Marshal(map[string]string{
		"mission_id": workspaceID,
		"action":     "cancelled",
	})
	return string(result), nil
}

// dagNode mirrors the daemon's DAGNode shape.
type dagNode struct {
	ID           string   `json:"id"`
	Name         string   `json:"name"`
	Persona      string   `json:"persona"`
	Skills       []string `json:"skills"`
	Status       string   `json:"status"`
	Dependencies []string `json:"dependencies"`
}

// dagEdge mirrors the daemon's DAGEdge shape.
type dagEdge struct {
	From string `json:"from"`
	To   string `json:"to"`
}

// dagResponse mirrors the daemon's DAGResponse shape expected by the frontend.
type dagResponse struct {
	MissionID string    `json:"mission_id"`
	Nodes     []dagNode `json:"nodes"`
	Edges     []dagEdge `json:"edges"`
}

// GetMissionDAG reads the checkpoint for a mission workspace and builds a
// proper DAGResponse (nodes + edges) matching the daemon's handleMissionDAG shape.
func (a *App) GetMissionDAG(workspaceID string) (string, error) {
	if err := sanitizeID(workspaceID); err != nil {
		return "", err
	}

	cp := loadLocalCheckpoint(workspaceID)
	if cp == nil || cp.Plan == nil {
		return "", fmt.Errorf("checkpoint not found for workspace %s", workspaceID)
	}

	nodes := make([]dagNode, 0, len(cp.Plan.Phases))
	var edges []dagEdge

	for _, p := range cp.Plan.Phases {
		deps := p.Dependencies
		if deps == nil {
			deps = []string{}
		}
		skills := p.Skills
		if skills == nil {
			skills = []string{}
		}
		nodes = append(nodes, dagNode{
			ID:           p.ID,
			Name:         p.Name,
			Persona:      p.Persona,
			Skills:       skills,
			Status:       p.Status,
			Dependencies: deps,
		})
		for _, depID := range p.Dependencies {
			edges = append(edges, dagEdge{From: depID, To: p.ID})
		}
	}
	if edges == nil {
		edges = []dagEdge{}
	}

	resp := dagResponse{
		MissionID: workspaceID,
		Nodes:     nodes,
		Edges:     edges,
	}
	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling DAG: %w", err)
	}
	return string(data), nil
}

// GetMissionDetail returns checkpoint metadata for a specific mission workspace,
// matching the MissionDetail shape expected by the frontend.
func (a *App) GetMissionDetail(id string) (string, error) {
	if err := sanitizeID(id); err != nil {
		return "", err
	}

	// Stat the event log for size and modified time.
	evPath := filepath.Join(configDir(), "events", id+".jsonl")
	evInfo, err := os.Stat(evPath)
	if err != nil {
		return "", fmt.Errorf("event log not found for mission %s: %w", id, err)
	}

	type missionDetail struct {
		MissionID  string    `json:"mission_id"`
		Status     string    `json:"status,omitempty"`
		Task       string    `json:"task,omitempty"`
		Phases     int       `json:"phases,omitempty"`
		EventCount int       `json:"event_count"`
		SizeBytes  int64     `json:"size_bytes"`
		ModifiedAt time.Time `json:"modified_at"`
	}
	detail := missionDetail{
		MissionID:  id,
		EventCount: countJSONLLines(evPath),
		SizeBytes:  evInfo.Size(),
		ModifiedAt: evInfo.ModTime(),
	}
	if cp := loadLocalCheckpoint(id); cp != nil {
		detail.Status = cp.Status
		if cp.Plan != nil {
			detail.Task = cp.Plan.Task
			detail.Phases = len(cp.Plan.Phases)
		}
	}
	data, err := json.Marshal(detail)
	if err != nil {
		return "", fmt.Errorf("marshalling mission detail: %w", err)
	}
	return string(data), nil
}

// ── Personas ──────────────────────────────────────────────────────────────────

type personaEntry struct {
	Name               string   `json:"name"`
	WhenToUse          []string `json:"when_to_use"`
	Expertise          []string `json:"expertise"`
	Color              string   `json:"color"`
	MissionsAssigned   int      `json:"missions_assigned"`
	SuccessRate        float64  `json:"success_rate"`
	AvgDurationSeconds float64  `json:"avg_duration_seconds"`
	CurrentlyActive    bool     `json:"currently_active"`
}

// ListPersonas reads ~/nanika/personas/*.md (and <dir>/<dir>.md) and returns metadata as JSON.
func (a *App) ListPersonas() (string, error) {
	home, _ := os.UserHomeDir()
	dir := filepath.Join(home, "nanika", "personas")
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "", fmt.Errorf("reading personas dir: %w", err)
	}

	logsDir := filepath.Join(configDir(), "events")
	stats, _ := aggregatePersonaStats(logsDir)

	personas := make([]personaEntry, 0, len(entries))
	for _, e := range entries {
		var name, fpath string
		if e.IsDir() {
			name = e.Name()
			fpath = filepath.Join(dir, name, name+".md")
		} else if strings.HasSuffix(e.Name(), ".md") {
			name = strings.TrimSuffix(e.Name(), ".md")
			fpath = filepath.Join(dir, e.Name())
		} else {
			continue
		}
		raw, err := os.ReadFile(fpath)
		if err != nil {
			continue
		}
		pe := parsePersona(name, string(raw))
		if u := stats[name]; u != nil {
			pe.MissionsAssigned = u.missionsAssigned
			if u.total > 0 {
				pe.SuccessRate = float64(u.succeeded) / float64(u.total)
			}
			if len(u.durations) > 0 {
				var sum float64
				for _, d := range u.durations {
					sum += d.Seconds()
				}
				pe.AvgDurationSeconds = sum / float64(len(u.durations))
			}
			pe.CurrentlyActive = u.active
		}
		personas = append(personas, pe)
	}

	data, err := json.Marshal(personas)
	if err != nil {
		return "", fmt.Errorf("marshalling personas: %w", err)
	}
	return string(data), nil
}

// ReloadPersonas re-reads persona files from disk.
// Cannot call persona.Load() directly (different module, internal package),
// so this provides a fresh read from the filesystem.
func (a *App) ReloadPersonas() (string, error) {
	return a.ListPersonas()
}

// personaDetailEntry is the full detail response for a single persona.
type personaDetailEntry struct {
	personaEntry
	RecentMissions []personaRecentMission     `json:"recent_missions"`
	SuccessTrend   []personaSuccessTrendPoint `json:"success_trend"`
}

type personaRecentMission struct {
	WorkspaceID string  `json:"workspace_id"`
	Domain      string  `json:"domain"`
	Task        string  `json:"task"`
	Status      string  `json:"status"`
	StartedAt   string  `json:"started_at"`
	DurationS   float64 `json:"duration_s"`
}

type personaSuccessTrendPoint struct {
	Week        string  `json:"week"`
	Total       int     `json:"total"`
	Succeeded   int     `json:"succeeded"`
	SuccessRate float64 `json:"success_rate"`
}

// GetPersonaDetail returns the full detail for one persona including recent missions and success trend.
func (a *App) GetPersonaDetail(name string) (string, error) {
	home, _ := os.UserHomeDir()
	personasDir := filepath.Join(home, "nanika", "personas")

	// Try both foo.md and foo/foo.md layouts.
	var fpath string
	if raw, err := os.ReadFile(filepath.Join(personasDir, name+".md")); err == nil {
		fpath = filepath.Join(personasDir, name+".md")
		_ = raw
	} else if raw, err := os.ReadFile(filepath.Join(personasDir, name, name+".md")); err == nil {
		fpath = filepath.Join(personasDir, name, name+".md")
		_ = raw
	}
	if fpath == "" {
		return "", fmt.Errorf("persona %q not found", name)
	}
	raw, err := os.ReadFile(fpath)
	if err != nil {
		return "", fmt.Errorf("reading persona file: %w", err)
	}

	base := parsePersona(name, string(raw))

	logsDir := filepath.Join(configDir(), "events")
	stats, _ := aggregatePersonaStats(logsDir)
	if u := stats[name]; u != nil {
		base.MissionsAssigned = u.missionsAssigned
		if u.total > 0 {
			base.SuccessRate = float64(u.succeeded) / float64(u.total)
		}
		if len(u.durations) > 0 {
			var sum float64
			for _, d := range u.durations {
				sum += d.Seconds()
			}
			base.AvgDurationSeconds = sum / float64(len(u.durations))
		}
		base.CurrentlyActive = u.active
	}

	recent, trend := personaDetailStats(logsDir, name)

	detail := personaDetailEntry{
		personaEntry:   base,
		RecentMissions: recent,
		SuccessTrend:   trend,
	}
	data, err := json.Marshal(detail)
	if err != nil {
		return "", fmt.Errorf("marshalling persona detail: %w", err)
	}
	return string(data), nil
}

// personaDetailStats scans event logs and returns recent missions and weekly trend for one persona.
func personaDetailStats(logsDir, personaName string) ([]personaRecentMission, []personaSuccessTrendPoint) {
	entries, err := os.ReadDir(logsDir)
	if err != nil {
		return nil, nil
	}

	type missionRecord struct {
		workspaceID string
		task        string
		status      string
		startedAt   string
		durationS   float64
	}

	weekBuckets := make(map[string]*personaSuccessTrendPoint) // key: "YYYY-WW"
	var missions []missionRecord

	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
			continue
		}
		rec := scanPersonaMissionLog(filepath.Join(logsDir, e.Name()), personaName)
		if rec == nil {
			continue
		}
		missions = append(missions, *rec)

		// Parse week bucket from startedAt.
		if rec.startedAt != "" {
			t, err := time.Parse(time.RFC3339Nano, rec.startedAt)
			if err != nil {
				t, err = time.Parse("2006-01-02 15:04:05", rec.startedAt)
			}
			if err == nil {
				yr, wk := t.ISOWeek()
				key := fmt.Sprintf("%d-W%02d", yr, wk)
				if weekBuckets[key] == nil {
					weekBuckets[key] = &personaSuccessTrendPoint{Week: key}
				}
				weekBuckets[key].Total++
				if rec.status == "success" {
					weekBuckets[key].Succeeded++
				}
			}
		}
	}

	// Sort missions newest-first (JSONL filenames are date-prefixed).
	for i, j := 0, len(missions)-1; i < j; i, j = i+1, j-1 {
		missions[i], missions[j] = missions[j], missions[i]
	}
	if len(missions) > 10 {
		missions = missions[:10]
	}

	recent := make([]personaRecentMission, 0, len(missions))
	for _, m := range missions {
		recent = append(recent, personaRecentMission{
			WorkspaceID: m.workspaceID,
			Task:        m.task,
			Status:      m.status,
			StartedAt:   m.startedAt,
			DurationS:   m.durationS,
		})
	}

	// Build sorted trend slice (last 12 weeks).
	type weekEntry struct {
		key string
		pt  *personaSuccessTrendPoint
	}
	var weeks []weekEntry
	for k, v := range weekBuckets {
		weeks = append(weeks, weekEntry{k, v})
	}
	// Sort by week key ascending.
	for i := 1; i < len(weeks); i++ {
		for j := i; j > 0 && weeks[j].key < weeks[j-1].key; j-- {
			weeks[j], weeks[j-1] = weeks[j-1], weeks[j]
		}
	}
	if len(weeks) > 12 {
		weeks = weeks[len(weeks)-12:]
	}
	trend := make([]personaSuccessTrendPoint, 0, len(weeks))
	for _, w := range weeks {
		pt := *w.pt
		if pt.Total > 0 {
			pt.SuccessRate = float64(pt.Succeeded) / float64(pt.Total)
		}
		trend = append(trend, pt)
	}

	return recent, trend
}

// scanPersonaMissionLog reads one JSONL and returns a mission record if the persona participated.
func scanPersonaMissionLog(logPath, personaName string) *struct {
	workspaceID string
	task        string
	status      string
	startedAt   string
	durationS   float64
} {
	f, err := os.Open(logPath)
	if err != nil {
		return nil
	}
	defer f.Close()

	type eventRecord struct {
		Type      string         `json:"type"`
		Timestamp string         `json:"timestamp"`
		MissionID string         `json:"mission_id"`
		Data      map[string]any `json:"data"`
	}

	var usesPersona bool
	var task, startedAt, missionID string
	var status string
	var durationS float64

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		var ev eventRecord
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue
		}
		if missionID == "" && ev.MissionID != "" {
			missionID = ev.MissionID
		}
		switch ev.Type {
		case "phase.started", "dag.phase_dispatched":
			if p, ok := ev.Data["persona"].(string); ok && p == personaName {
				usesPersona = true
			}
		case "mission.started":
			if ts := ev.Timestamp; ts != "" {
				startedAt = ts
			}
			if t, ok := ev.Data["task"].(string); ok {
				// Extract a short task title: first non-empty non-frontmatter line.
				task = extractTaskTitle(t)
			}
		case "mission.completed":
			status = "success"
			if d, ok := ev.Data["duration"].(string); ok {
				if dur, err := time.ParseDuration(d); err == nil {
					durationS = dur.Seconds()
				}
			}
		case "mission.failed":
			status = "failed"
		}
	}

	if !usesPersona {
		return nil
	}
	if status == "" {
		status = "in_progress"
	}
	return &struct {
		workspaceID string
		task        string
		status      string
		startedAt   string
		durationS   float64
	}{missionID, task, status, startedAt, durationS}
}

// extractTaskTitle returns a short one-line summary from a mission task string.
// Skips YAML frontmatter and returns the first non-empty heading or line.
func extractTaskTitle(task string) string {
	// Strip YAML frontmatter.
	body := task
	if strings.HasPrefix(task, "---\n") {
		if end := strings.Index(task[4:], "\n---"); end >= 0 {
			body = strings.TrimSpace(task[4+end+4:])
		}
	}
	for _, line := range strings.Split(body, "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		// Return heading text without # prefix.
		line = strings.TrimLeft(line, "#")
		line = strings.TrimSpace(line)
		if line != "" {
			return line
		}
	}
	// Fallback: first 80 chars.
	if len(task) > 80 {
		return task[:80] + "…"
	}
	return task
}

// parsePersona extracts name, expertise, when_to_use bullet points, and color from persona markdown.
func parsePersona(name, content string) personaEntry {
	p := personaEntry{Name: name, WhenToUse: []string{}, Expertise: []string{}, Color: personaColor(name)}

	body := content
	if strings.HasPrefix(content, "---\n") {
		if end := strings.Index(content[4:], "\n---"); end >= 0 {
			body = content[4+end+4:]
		}
	}

	// Extract bullet points under "## Expertise" and "## When to Use" sections.
	lines := strings.Split(body, "\n")
	inExpertise := false
	inWhenToUse := false
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if strings.EqualFold(trimmed, "## expertise") {
			inExpertise = true
			inWhenToUse = false
			continue
		}
		if strings.EqualFold(trimmed, "## when to use") {
			inWhenToUse = true
			inExpertise = false
			continue
		}
		if strings.HasPrefix(trimmed, "## ") {
			inExpertise = false
			inWhenToUse = false
			continue
		}
		if inExpertise && strings.HasPrefix(trimmed, "- ") {
			p.Expertise = append(p.Expertise, strings.TrimPrefix(trimmed, "- "))
		}
		if inWhenToUse && strings.HasPrefix(trimmed, "- ") {
			p.WhenToUse = append(p.WhenToUse, strings.TrimPrefix(trimmed, "- "))
		}
	}
	return p
}

// personaUsage accumulates mission stats for one persona across all event logs.
type personaUsage struct {
	missionsAssigned int
	succeeded        int
	total            int
	durations        []time.Duration
	active           bool
}

// aggregatePersonaStats reads all JSONL files in logsDir and returns per-persona stats.
func aggregatePersonaStats(logsDir string) (map[string]*personaUsage, error) {
	entries, err := os.ReadDir(logsDir)
	if os.IsNotExist(err) {
		return make(map[string]*personaUsage), nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading events dir: %w", err)
	}
	stats := make(map[string]*personaUsage)
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
			continue
		}
		_ = processPersonaMissionLog(filepath.Join(logsDir, e.Name()), stats)
	}
	return stats, nil
}

// processPersonaMissionLog reads one mission JSONL and updates stats for each persona that appeared.
func processPersonaMissionLog(logPath string, stats map[string]*personaUsage) error {
	f, err := os.Open(logPath)
	if err != nil {
		return err
	}
	defer f.Close()

	type eventRecord struct {
		Type string         `json:"type"`
		Data map[string]any `json:"data"`
	}

	personasInMission := make(map[string]bool)
	var completed, failed bool
	var duration time.Duration

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		var ev eventRecord
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue
		}
		switch ev.Type {
		case "phase.started", "dag.phase_dispatched":
			if p, ok := ev.Data["persona"].(string); ok && p != "" {
				personasInMission[p] = true
			}
		case "mission.completed":
			completed = true
			if d, ok := ev.Data["duration"].(string); ok {
				if dur, err := time.ParseDuration(d); err == nil {
					duration = dur
				}
			}
		case "mission.failed":
			failed = true
		}
	}

	if len(personasInMission) == 0 {
		return nil
	}

	isActive := !completed && !failed
	for p := range personasInMission {
		u := stats[p]
		if u == nil {
			u = &personaUsage{}
			stats[p] = u
		}
		u.missionsAssigned++
		if completed || failed {
			u.total++
		}
		if completed {
			u.succeeded++
			if duration > 0 {
				u.durations = append(u.durations, duration)
			}
		}
		if isActive {
			u.active = true
		}
	}
	return nil
}

// personaColor returns a stable display color for a named persona.
// Unknown personas fall back to gray. Mirrors daemon's personaColor function.
func personaColor(name string) string {
	colors := map[string]string{
		// Live catalog
		"academic-researcher":      "#0ea5e9",
		"architect":                "#6366f1",
		"data-analyst":             "#3b82f6",
		"devops-engineer":          "#8b5cf6",
		"qa-engineer":              "#ef4444",
		"security-auditor":         "#dc2626",
		"senior-backend-engineer":  "#10b981",
		"senior-frontend-engineer": "#06b6d4",
		"staff-code-reviewer":      "#f59e0b",
		"technical-writer":         "#64748b",
		// Legacy names
		"academic-reviewer":          "#0284c7",
		"academic-writer":            "#2563eb",
		"artist":                     "#ec4899",
		"backend-engineer":           "#10b981",
		"cartographer":               "#14b8a6",
		"cinematographer":            "#7c3aed",
		"code-reviewer":              "#f59e0b",
		"conversion-writer":          "#c026d3",
		"frontend-engineer":          "#06b6d4",
		"indie-coach":                "#f97316",
		"journaler":                  "#84cc16",
		"methodologist":              "#a855f7",
		"narrator":                   "#059669",
		"principal-systems-reviewer": "#6366f1",
		"researcher":                 "#0ea5e9",
		"senior-golang-engineer":     "#34d399",
		"senior-rust-engineer":       "#ea580c",
		"storyteller":                "#d946ef",
	}
	if c, ok := colors[name]; ok {
		return c
	}
	return "#6b7280" // gray
}

// ── Metrics ───────────────────────────────────────────────────────────────────

// GetMetrics reads ~/.alluka/metrics.jsonl and returns aggregated stats as MetricsResponse JSON.
func (a *App) GetMetrics() (string, error) {
	type metricsRec struct {
		WorkspaceID string    `json:"workspace_id"`
		Domain      string    `json:"domain"`
		Task        string    `json:"task"`
		StartedAt   time.Time `json:"started_at"`
		DurationSec int       `json:"duration_s"`
		Status      string    `json:"status"` // "success", "failure", "partial"
		Phases      []struct {
			Persona string `json:"persona"`
			Status  string `json:"status"`
		} `json:"phases,omitempty"`
	}
	type domainStats struct {
		Total     int `json:"total"`
		Completed int `json:"completed"`
		Failed    int `json:"failed"`
		Cancelled int `json:"cancelled"`
	}
	type personaStats struct {
		Phases    int `json:"phases"`
		Completed int `json:"completed"`
		Failed    int `json:"failed"`
	}
	type recentMission struct {
		WorkspaceID string    `json:"workspace_id"`
		Domain      string    `json:"domain"`
		Task        string    `json:"task,omitempty"`
		Status      string    `json:"status"`
		StartedAt   time.Time `json:"started_at,omitempty"`
		DurationS   int       `json:"duration_s,omitempty"`
	}
	type metricsResp struct {
		Total        int                      `json:"total"`
		Completed    int                      `json:"completed"`
		Failed       int                      `json:"failed"`
		Cancelled    int                      `json:"cancelled"`
		AvgDurationS int                      `json:"avg_duration_s"`
		ByDomain     map[string]*domainStats  `json:"by_domain"`
		ByPersona    map[string]*personaStats `json:"by_persona"`
		Recent       []recentMission          `json:"recent"`
	}

	resp := &metricsResp{
		ByDomain:  make(map[string]*domainStats),
		ByPersona: make(map[string]*personaStats),
		Recent:    []recentMission{},
	}

	metricsPath := filepath.Join(configDir(), "metrics.jsonl")
	f, err := os.Open(metricsPath)
	if err != nil {
		if os.IsNotExist(err) {
			data, _ := json.Marshal(resp)
			return string(data), nil
		}
		return "", fmt.Errorf("opening metrics: %w", err)
	}
	defer f.Close()

	var records []metricsRec
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" {
			continue
		}
		var rec metricsRec
		if err := json.Unmarshal([]byte(line), &rec); err != nil {
			continue
		}
		// Filter test/synthetic workspaces and zero-duration records.
		if strings.HasPrefix(rec.WorkspaceID, "ws-") || strings.HasPrefix(rec.WorkspaceID, "test-") || rec.DurationSec == 0 {
			continue
		}
		records = append(records, rec)
	}
	if err := sc.Err(); err != nil {
		return "", fmt.Errorf("reading metrics: %w", err)
	}

	var totalDur, durationCount int
	for _, rec := range records {
		resp.Total++
		apiStatus := "failed"
		if rec.Status == "success" {
			apiStatus = "completed"
			resp.Completed++
		} else {
			resp.Failed++
		}
		// Only count missions with actual timing data (matches daemon's durationCount logic).
		if rec.DurationSec > 0 {
			totalDur += rec.DurationSec
			durationCount++
		}

		if rec.Domain != "" {
			ds := resp.ByDomain[rec.Domain]
			if ds == nil {
				ds = &domainStats{}
				resp.ByDomain[rec.Domain] = ds
			}
			ds.Total++
			if apiStatus == "completed" {
				ds.Completed++
			} else {
				ds.Failed++
			}
		}

		for _, ph := range rec.Phases {
			if ph.Persona == "" {
				continue
			}
			ps := resp.ByPersona[ph.Persona]
			if ps == nil {
				ps = &personaStats{}
				resp.ByPersona[ph.Persona] = ps
			}
			ps.Phases++
			if ph.Status == "completed" {
				ps.Completed++
			} else if ph.Status == "failed" {
				ps.Failed++
			}
		}
	}

	if durationCount > 0 {
		resp.AvgDurationS = totalDur / durationCount
	}

	// Scan workspace directories for cancelled missions not in metrics.jsonl.
	seenIDs := make(map[string]bool, len(records))
	for _, rec := range records {
		seenIDs[rec.WorkspaceID] = true
	}
	wsBase := filepath.Join(configDir(), "workspaces")
	evBase := filepath.Join(configDir(), "events")
	if wsEntries, werr := os.ReadDir(wsBase); werr == nil {
		for _, we := range wsEntries {
			if !we.IsDir() || seenIDs[we.Name()] {
				continue
			}
			wsID := we.Name()
			// Skip test/synthetic workspace directories.
			if strings.HasPrefix(wsID, "ws-") || strings.HasPrefix(wsID, "test-") {
				continue
			}
			// Only count workspaces that have a checkpoint and a cancelled event.
			cp := loadLocalCheckpoint(wsID)
			if cp == nil {
				continue
			}
			logPath := filepath.Join(evBase, wsID+".jsonl")
			if eventLogHasCancelledEvent(logPath) {
				resp.Total++
				resp.Cancelled++
				if cp.Domain != "" {
					ds := resp.ByDomain[cp.Domain]
					if ds == nil {
						ds = &domainStats{}
						resp.ByDomain[cp.Domain] = ds
					}
					ds.Total++
					ds.Cancelled++
				}
			}
		}
	}

	// Include up to the 10 most recent missions (newest first).
	start := len(records) - 10
	if start < 0 {
		start = 0
	}
	for i := len(records) - 1; i >= start; i-- {
		rec := records[i]
		apiStatus := "failed"
		if rec.Status == "success" {
			apiStatus = "completed"
		}
		resp.Recent = append(resp.Recent, recentMission{
			WorkspaceID: rec.WorkspaceID,
			Domain:      rec.Domain,
			Task:        rec.Task,
			Status:      apiStatus,
			StartedAt:   rec.StartedAt,
			DurationS:   rec.DurationSec,
		})
	}

	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling metrics: %w", err)
	}
	return string(data), nil
}

// ── Findings ──────────────────────────────────────────────────────────────────

// eventLogHasCancelledEvent reports whether a mission JSONL file contains a
// mission.cancelled event, using a fast string pre-check before full JSON parsing.
func eventLogHasCancelledEvent(path string) bool {
	f, err := os.Open(path)
	if err != nil {
		return false
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 16*1024), 16*1024)
	for sc.Scan() {
		line := sc.Text()
		if !strings.Contains(line, "mission.cancelled") {
			continue
		}
		var ev struct {
			Type string `json:"type"`
		}
		if json.Unmarshal([]byte(line), &ev) == nil && ev.Type == "mission.cancelled" {
			return true
		}
	}
	return false
}

// ── Scanners ──────────────────────────────────────────────────────────────────

// ListScanners reads ~/.alluka/nen/scanners/ and returns metadata for each executable.
func (a *App) ListScanners() (string, error) {
	dir := filepath.Join(configDir(), "nen", "scanners")
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return "[]", nil
		}
		return "", fmt.Errorf("reading scanners dir: %w", err)
	}

	type entry struct {
		Name      string    `json:"name"`
		Path      string    `json:"path"`
		SizeBytes int64     `json:"size_bytes"`
		ModTime   time.Time `json:"mod_time"`
	}
	scanners := make([]entry, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		if info.Mode()&0111 == 0 {
			continue
		}
		scanners = append(scanners, entry{
			Name:      e.Name(),
			Path:      filepath.Join(dir, e.Name()),
			SizeBytes: info.Size(),
			ModTime:   info.ModTime(),
		})
	}
	data, err := json.Marshal(scanners)
	if err != nil {
		return "", fmt.Errorf("marshalling scanners: %w", err)
	}
	return string(data), nil
}

// ── Channels ──────────────────────────────────────────────────────────────────

// GetChannelStatus reads ~/.alluka/channels/*.json and checks the daemon socket.
func (a *App) GetChannelStatus() (string, error) {
	cfgDir := configDir()

	// Check daemon in parallel with file reads (up to 2s timeout).
	daemonCh := make(chan bool, 1)
	go func() { daemonCh <- isDaemonReachable(filepath.Join(cfgDir, "daemon.sock")) }()

	channelsDir := filepath.Join(cfgDir, "channels")
	entries, err := os.ReadDir(channelsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return "[]", nil
		}
		return "", fmt.Errorf("reading channels dir: %w", err)
	}

	daemonUp := <-daemonCh

	type chanStatus struct {
		Name       string `json:"name"`
		Platform   string `json:"platform"`
		Configured bool   `json:"configured"`
		Active     bool   `json:"active"`
		ErrorCount int    `json:"error_count"`
	}
	_ = daemonUp // retained for daemon socket check side-effect
	channels := make([]chanStatus, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".json") {
			continue
		}
		name := strings.TrimSuffix(e.Name(), ".json")
		cs := chanStatus{
			Name:     name,
			Platform: name, // filename is the platform (e.g. "telegram", "discord")
		}

		raw, err := os.ReadFile(filepath.Join(channelsDir, e.Name()))
		if err == nil {
			var m map[string]interface{}
			if json.Unmarshal(raw, &m) == nil {
				hasToken := false
				for _, k := range []string{"bot_token", "token", "webhook_url"} {
					if v, ok := m[k].(string); ok && v != "" {
						hasToken = true
						break
					}
				}
				cs.Configured = hasToken
				cs.Active = hasToken
				if p, ok := m["platform"].(string); ok && p != "" {
					cs.Platform = p
				}
			}
		}
		channels = append(channels, cs)
	}

	data, err := json.Marshal(channels)
	if err != nil {
		return "", fmt.Errorf("marshalling channel status: %w", err)
	}
	return string(data), nil
}

// countJSONLLines counts non-empty lines in a JSONL file (proxy for event count).
func countJSONLLines(path string) int {
	f, err := os.Open(path)
	if err != nil {
		return 0
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 1<<20)
	n := 0
	for sc.Scan() {
		if strings.TrimSpace(sc.Text()) != "" {
			n++
		}
	}
	return n
}

// isDaemonReachable tries a quick Unix socket dial to check if the daemon is up.
func isDaemonReachable(sockPath string) bool {
	conn, err := net.DialTimeout("unix", sockPath, 2*time.Second)
	if err != nil {
		return false
	}
	conn.Close()
	return true
}

// ── Actions ───────────────────────────────────────────────────────────────────

// NenScan execs `shu evaluate` via the nen plugin binary and returns the output as JSON.
func (a *App) NenScan() (string, error) {
	binary := resolvePluginBinary("nen")
	if binary == "" {
		return "", fmt.Errorf("nen plugin binary not found")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, binary, "evaluate")
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("shu evaluate: %w", err)
	}
	result, _ := json.Marshal(map[string]string{"output": strings.TrimSpace(string(out))})
	return string(result), nil
}

// Cleanup execs `orchestrator cleanup` and returns the output as JSON.
func (a *App) Cleanup() (string, error) {
	out, err := execOrchestrator(30*time.Second, "cleanup")
	if err != nil {
		return "", err
	}
	result, _ := json.Marshal(map[string]string{"output": strings.TrimSpace(out)})
	return string(result), nil
}

// ── Health ────────────────────────────────────────────────────────────────────

// daemonStatus describes a running/stopped daemon process.
type daemonStatus struct {
	Name   string `json:"name"`
	Status string `json:"status"` // "running" | "stopped"
	PID    int    `json:"pid"`
}

// orchestratorHealthResp is the response shape for OrchestratorHealth.
type orchestratorHealthResp struct {
	Daemons   []daemonStatus `json:"daemons"`
	Timestamp time.Time      `json:"timestamp"`
}

// OrchestratorHealth returns the status of the orchestrator, scheduler, and
// nen-daemon by reading their PID files and verifying the processes are alive.
func (a *App) OrchestratorHealth() (string, error) {
	cfgDir := configDir()
	pidFiles := []struct {
		name string
		path string
	}{
		{"orchestrator", filepath.Join(cfgDir, "daemon.pid")},
		{"scheduler", filepath.Join(cfgDir, "scheduler", "daemon.pid")},
		{"nen-daemon", filepath.Join(cfgDir, "nen-daemon.pid")},
	}

	daemons := make([]daemonStatus, 0, len(pidFiles))
	for _, pf := range pidFiles {
		ds := daemonStatus{Name: pf.name, Status: "stopped"}
		if data, err := os.ReadFile(pf.path); err == nil {
			var pid int
			if _, err := fmt.Sscan(strings.TrimSpace(string(data)), &pid); err == nil && pid > 0 {
				if pidIsAlive(pid) {
					ds.Status = "running"
					ds.PID = pid
				}
			}
		}
		daemons = append(daemons, ds)
	}

	resp := orchestratorHealthResp{
		Daemons:   daemons,
		Timestamp: time.Now().UTC(),
	}
	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling health: %w", err)
	}
	return string(data), nil
}

// pidIsAlive sends signal 0 to a PID — returns true if the process exists.
func pidIsAlive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}

// NenHealth proxies `shu query status --json` (via the nen plugin binary)
// and returns the raw JSON output.
func (a *App) NenHealth() (string, error) {
	binary := resolvePluginBinary("nen")
	if binary == "" {
		result, _ := json.Marshal(map[string]string{"error": "nen plugin binary not found"})
		return string(result), nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, binary, "query", "status", "--json")
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return "", fmt.Errorf("shu query status: %s", strings.TrimSpace(string(ee.Stderr)))
		}
		return "", fmt.Errorf("shu query status: %w", err)
	}
	return strings.TrimSpace(string(out)), nil
}

// SchedulerHealth proxies `scheduler query status --json` and returns the raw JSON output.
func (a *App) SchedulerHealth() (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "scheduler", "query", "status", "--json")
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return "", fmt.Errorf("scheduler query status: %s", strings.TrimSpace(string(ee.Stderr)))
		}
		return "", fmt.Errorf("scheduler query status: %w", err)
	}
	return strings.TrimSpace(string(out)), nil
}

// pluginHealthCache caches plugin doctor results to avoid hammering all plugin
// binaries on every request. Refreshed after pluginHealthTTL.
var pluginHealthCache struct {
	sync.Mutex
	data     string
	cachedAt time.Time
}

const pluginHealthTTL = 5 * time.Minute

// pluginDoctorResult holds the outcome of running `<binary> doctor --json` for one plugin.
type pluginDoctorResult struct {
	Name   string          `json:"name"`
	Status string          `json:"status"` // "ok" | "error" | "unavailable"
	Output json.RawMessage `json:"output,omitempty"`
	Error  string          `json:"error,omitempty"`
}

// pluginHealthResp is the response shape for GetPluginHealth.
type pluginHealthResp struct {
	Plugins  []pluginDoctorResult `json:"plugins"`
	CachedAt time.Time            `json:"cached_at"`
}

// GetPluginHealth runs `<binary> doctor --json` for every installed plugin and
// returns the aggregated results. Results are cached for 5 minutes.
func (a *App) GetPluginHealth() (string, error) {
	pluginHealthCache.Lock()
	defer pluginHealthCache.Unlock()

	if pluginHealthCache.data != "" && time.Since(pluginHealthCache.cachedAt) < pluginHealthTTL {
		return pluginHealthCache.data, nil
	}

	plugins, err := a.ListPlugins()
	if err != nil {
		return "", fmt.Errorf("listing plugins: %w", err)
	}

	results := make([]pluginDoctorResult, 0, len(plugins))
	for _, p := range plugins {
		r := pluginDoctorResult{Name: p.Name, Status: "unavailable"}
		binary := resolvePluginBinary(p.Name)
		if binary == "" {
			results = append(results, r)
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		cmd := exec.CommandContext(ctx, binary, "doctor", "--json")
		cmd.Env = enrichedEnv()
		out, execErr := cmd.Output()
		cancel()
		if execErr != nil {
			r.Status = "error"
			if ee, ok := execErr.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
				r.Error = strings.TrimSpace(string(ee.Stderr))
			} else {
				r.Error = execErr.Error()
			}
			results = append(results, r)
			continue
		}
		r.Status = "ok"
		r.Output = json.RawMessage(strings.TrimSpace(string(out)))
		results = append(results, r)
	}

	resp := pluginHealthResp{
		Plugins:  results,
		CachedAt: time.Now().UTC(),
	}
	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling plugin health: %w", err)
	}
	pluginHealthCache.data = string(data)
	pluginHealthCache.cachedAt = time.Now().UTC()
	return pluginHealthCache.data, nil
}

// pluginCapabilitiesEntry is one entry in the GetPluginCapabilities response.
type pluginCapabilitiesEntry struct {
	Name         string          `json:"name"`
	Capabilities json.RawMessage `json:"capabilities"`
}

// pluginCapabilitiesCache caches capabilities to avoid re-reading plugin.json
// files on every autocomplete keystroke. Refreshed after pluginCapabilitiesTTL.
var pluginCapabilitiesCache struct {
	sync.Mutex
	data     string
	cachedAt time.Time
}

const pluginCapabilitiesTTL = 5 * time.Minute

// GetPluginCapabilities returns the capabilities object for every installed
// plugin in a single call. Plugins without a capabilities field return null.
func (a *App) GetPluginCapabilities() (string, error) {
	pluginCapabilitiesCache.Lock()
	defer pluginCapabilitiesCache.Unlock()

	if pluginCapabilitiesCache.data != "" && time.Since(pluginCapabilitiesCache.cachedAt) < pluginCapabilitiesTTL {
		return pluginCapabilitiesCache.data, nil
	}

	plugins, err := a.ListPlugins()
	if err != nil {
		return "", fmt.Errorf("listing plugins: %w", err)
	}

	entries := make([]pluginCapabilitiesEntry, 0, len(plugins))
	for _, p := range plugins {
		entries = append(entries, pluginCapabilitiesEntry{
			Name:         p.Name,
			Capabilities: p.Capabilities,
		})
	}

	data, err := json.Marshal(entries)
	if err != nil {
		return "", fmt.Errorf("marshalling plugin capabilities: %w", err)
	}
	pluginCapabilitiesCache.data = string(data)
	pluginCapabilitiesCache.cachedAt = time.Now().UTC()
	return pluginCapabilitiesCache.data, nil
}

// ── Orchestrator cost/eval endpoints ─────────────────────────────────────────

// GetRyuReport queries metrics.db for cost data: today's spend, week's spend,
// and top 5 missions by cost. Returns structured JSON.
func (a *App) GetRyuReport() (string, error) {
	type topMission struct {
		ID      string  `json:"id"`
		Task    string  `json:"task"`
		CostUSD float64 `json:"cost_usd"`
		Status  string  `json:"status"`
	}
	type ryuReportResp struct {
		TodaySpend  float64      `json:"today_spend"`
		WeekSpend   float64      `json:"week_spend"`
		TopMissions []topMission `json:"top_missions"`
		GeneratedAt time.Time    `json:"generated_at"`
	}

	dbPath := filepath.Join(configDir(), "metrics.db")
	if _, err := os.Stat(dbPath); os.IsNotExist(err) {
		resp := ryuReportResp{TopMissions: []topMission{}, GeneratedAt: time.Now().UTC()}
		data, _ := json.Marshal(resp)
		return string(data), nil
	}

	db, err := sql.Open("sqlite", "file:"+dbPath+"?mode=ro&_busy_timeout=5000")
	if err != nil {
		return "", fmt.Errorf("open metrics.db: %w", err)
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	now := time.Now().UTC()
	todaySince := now.Add(-24 * time.Hour).Format(time.RFC3339)
	weekSince := now.Add(-7 * 24 * time.Hour).Format(time.RFC3339)

	var todaySpend, weekSpend float64
	if err := db.QueryRowContext(ctx,
		`SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?`,
		todaySince,
	).Scan(&todaySpend); err != nil {
		return "", fmt.Errorf("querying today spend: %w", err)
	}
	if err := db.QueryRowContext(ctx,
		`SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?`,
		weekSince,
	).Scan(&weekSpend); err != nil {
		return "", fmt.Errorf("querying week spend: %w", err)
	}

	rows, err := db.QueryContext(ctx,
		`SELECT id, COALESCE(task,''), cost_usd_total, COALESCE(status,'')
		 FROM missions
		 ORDER BY cost_usd_total DESC
		 LIMIT 5`)
	if err != nil {
		return "", fmt.Errorf("querying top missions: %w", err)
	}
	defer rows.Close()

	missions := []topMission{}
	for rows.Next() {
		var m topMission
		if err := rows.Scan(&m.ID, &m.Task, &m.CostUSD, &m.Status); err != nil {
			return "", fmt.Errorf("scanning top mission: %w", err)
		}
		missions = append(missions, m)
	}
	if err := rows.Err(); err != nil {
		return "", fmt.Errorf("iterating top missions: %w", err)
	}

	resp := ryuReportResp{
		TodaySpend:  todaySpend,
		WeekSpend:   weekSpend,
		TopMissions: missions,
		GeneratedAt: now,
	}
	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling ryu report: %w", err)
	}
	return string(data), nil
}

// GetKoResults queries ko-history.db for eval suite results: pass rate per suite
// (grouped by config path) and last run date. Returns structured JSON.
func (a *App) GetKoResults() (string, error) {
	type suiteResult struct {
		ConfigPath  string  `json:"config_path"`
		Description string  `json:"description"`
		Total       int     `json:"total"`
		Passed      int     `json:"passed"`
		Failed      int     `json:"failed"`
		PassRate    float64 `json:"pass_rate"`
		LastRunAt   string  `json:"last_run_at"`
	}
	type koResultsResp struct {
		Suites      []suiteResult `json:"suites"`
		GeneratedAt time.Time     `json:"generated_at"`
	}

	dbPath := filepath.Join(configDir(), "ko-history.db")
	if _, err := os.Stat(dbPath); os.IsNotExist(err) {
		resp := koResultsResp{Suites: []suiteResult{}, GeneratedAt: time.Now().UTC()}
		data, _ := json.Marshal(resp)
		return string(data), nil
	}

	db, err := sql.Open("sqlite", "file:"+dbPath+"?mode=ro&_busy_timeout=5000")
	if err != nil {
		return "", fmt.Errorf("open ko-history.db: %w", err)
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	// Aggregate per config_path: sum totals across all runs, take most recent description and run date.
	rows, err := db.QueryContext(ctx, `
		SELECT config_path,
		       COALESCE(MAX(description), '') AS description,
		       COALESCE(SUM(total), 0)        AS total,
		       COALESCE(SUM(passed), 0)       AS passed,
		       COALESCE(SUM(failed), 0)       AS failed,
		       MAX(COALESCE(finished_at, started_at)) AS last_run_at
		FROM eval_runs
		WHERE finished_at IS NOT NULL
		GROUP BY config_path
		ORDER BY last_run_at DESC
	`)
	if err != nil {
		return "", fmt.Errorf("querying ko suites: %w", err)
	}
	defer rows.Close()

	suites := []suiteResult{}
	for rows.Next() {
		var s suiteResult
		if err := rows.Scan(&s.ConfigPath, &s.Description, &s.Total, &s.Passed, &s.Failed, &s.LastRunAt); err != nil {
			return "", fmt.Errorf("scanning suite row: %w", err)
		}
		if s.Total > 0 {
			s.PassRate = float64(s.Passed) / float64(s.Total)
		}
		suites = append(suites, s)
	}
	if err := rows.Err(); err != nil {
		return "", fmt.Errorf("iterating ko suites: %w", err)
	}

	resp := koResultsResp{Suites: suites, GeneratedAt: time.Now().UTC()}
	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling ko results: %w", err)
	}
	return string(data), nil
}

// ── Tracker endpoints ─────────────────────────────────────────────────────────

// execTracker runs a tracker CLI command and returns its stdout.
func execTracker(timeout time.Duration, args ...string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, "tracker", args...)
	cmd.Env = enrichedEnv()
	out, err := cmd.Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return "", fmt.Errorf("tracker %s: %s", strings.Join(args, " "), strings.TrimSpace(string(ee.Stderr)))
		}
		return "", fmt.Errorf("tracker %s: %w", strings.Join(args, " "), err)
	}
	return string(out), nil
}

// TrackerItems proxies `tracker query items --json` and returns the items array as JSON.
func (a *App) TrackerItems() (string, error) {
	raw, err := execTracker(15*time.Second, "query", "items", "--json")
	if err != nil {
		return "", fmt.Errorf("tracker items: %w", err)
	}
	// The CLI returns {"items":[...]}; unwrap to the array.
	var envelope struct {
		Items json.RawMessage `json:"items"`
	}
	if err := json.Unmarshal([]byte(raw), &envelope); err != nil {
		return "", fmt.Errorf("parsing tracker items response: %w", err)
	}
	if envelope.Items == nil {
		return "[]", nil
	}
	return string(envelope.Items), nil
}

// trackerUpdateRequest mirrors the POST /api/orchestrator/tracker-update body.
type trackerUpdateRequest struct {
	ID       string `json:"id"`
	Status   string `json:"status,omitempty"`
	Priority string `json:"priority,omitempty"`
	Labels   string `json:"labels,omitempty"`
}

// TrackerUpdate runs `tracker update <id> [--status ...] [--priority ...] [--labels ...]`
// and returns {"ok":true} on success or an error.
func (a *App) TrackerUpdate(reqJSON string) (string, error) {
	var req trackerUpdateRequest
	if err := json.Unmarshal([]byte(reqJSON), &req); err != nil {
		return "", fmt.Errorf("parsing tracker update request: %w", err)
	}
	if req.ID == "" {
		return "", fmt.Errorf("id is required")
	}
	// Reject path-traversal characters in the issue ID.
	if strings.ContainsAny(req.ID, "/\\") || strings.Contains(req.ID, "..") {
		return "", fmt.Errorf("invalid id: %s", req.ID)
	}

	args := []string{"update", req.ID}
	if req.Status != "" {
		args = append(args, "--status", req.Status)
	}
	if req.Priority != "" {
		args = append(args, "--priority", req.Priority)
	}
	if req.Labels != "" {
		args = append(args, "--labels", req.Labels)
	}
	if len(args) == 2 {
		return "", fmt.Errorf("at least one field (status, priority, labels) is required")
	}

	if _, err := execTracker(15*time.Second, args...); err != nil {
		return "", fmt.Errorf("tracker update: %w", err)
	}
	return `{"ok":true}`, nil
}

// TrackerStats returns counts grouped by status and priority for the filter sidebar badges.
func (a *App) TrackerStats() (string, error) {
	raw, err := execTracker(15*time.Second, "query", "items", "--json")
	if err != nil {
		return "", fmt.Errorf("tracker stats: %w", err)
	}

	var envelope struct {
		Items []struct {
			Status   string `json:"status"`
			Priority string `json:"priority"`
		} `json:"items"`
	}
	if err := json.Unmarshal([]byte(raw), &envelope); err != nil {
		return "", fmt.Errorf("parsing tracker stats response: %w", err)
	}

	byStatus := map[string]int{}
	byPriority := map[string]int{}
	for _, item := range envelope.Items {
		if item.Status != "" {
			byStatus[item.Status]++
		}
		if item.Priority != "" {
			byPriority[item.Priority]++
		}
	}

	type statsResp struct {
		Total      int            `json:"total"`
		ByStatus   map[string]int `json:"by_status"`
		ByPriority map[string]int `json:"by_priority"`
	}
	resp := statsResp{
		Total:      len(envelope.Items),
		ByStatus:   byStatus,
		ByPriority: byPriority,
	}
	data, err := json.Marshal(resp)
	if err != nil {
		return "", fmt.Errorf("marshalling tracker stats: %w", err)
	}
	return string(data), nil
}

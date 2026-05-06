package worker

import (
	"context"
	"encoding/json"
	"fmt"
	"math/rand"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika/shared/sdk"
)

const (
	maxProfileListLen = 8
	maxProfileItemLen = 200

	nudgeSampleRate    = 0.1
	nudgeMaxTranscript = 8000
	nudgeMinTranscript = 200
)

// nudgeRand is the sampler. Default: rand.Float64. Tests pin it.
var nudgeRand = rand.Float64

// nudgeQueryText is the LLM seam. Default: sdk.QueryText. Tests override it.
var nudgeQueryText = func(ctx context.Context, prompt string, opts *sdk.AgentOptions) (string, error) {
	return sdk.QueryText(ctx, prompt, opts)
}

// nudgePatch mirrors the JSON the model is asked to return. All fields are
// optional; the merger ignores zero values.
type nudgePatch struct {
	Role               string   `json:"role"`
	Preferences        []string `json:"preferences"`
	CommunicationStyle []string `json:"communication_style"`
	OngoingProjects    []string `json:"ongoing_projects"`
}

// PeriodicallyNudgeUserProfile is a best-effort post-run hook that asks
// Haiku to extract durable user-profile signals from the just-finished
// mission's worker outputs and merges them into USER.md.
//
// The function is gated three ways: (1) NANIKA_USER_PROFILE_NUDGE env var
// must be truthy (default off), (2) nudgeRand() must be <= nudgeSampleRate
// (default 1-in-10), (3) the loaded UserProfile must have OptedInForAutoUpdate
// set. Failures at any later step return a wrapped error but never corrupt
// the on-disk profile — the caller logs the error and moves on.
func PeriodicallyNudgeUserProfile(ctx context.Context, projectDir, workspacePath, domain string) error {
	if !nudgeEnabled() {
		return nil
	}
	if nudgeRand() > nudgeSampleRate {
		return nil
	}
	if projectDir == "" {
		return nil
	}

	profile, err := LoadUserProfile(projectDir)
	if err != nil {
		return fmt.Errorf("load USER.md: %w", err)
	}
	if !profile.OptedInForAutoUpdate {
		return nil
	}

	transcript := readWorkerOutputs(workspacePath)
	if len(transcript) < nudgeMinTranscript {
		return nil
	}

	prompt := buildNudgePrompt(profile, transcript)
	raw, err := nudgeQueryText(ctx, prompt, &sdk.AgentOptions{
		Model:    "haiku",
		MaxTurns: 1,
	})
	if err != nil {
		return fmt.Errorf("nudge: llm: %w", err)
	}

	patch, perr := parseNudgePatch(raw)
	if perr != nil || patch == nil {
		// Malformed response: best-effort no-op. USER.md untouched.
		return nil
	}

	merged := MergeUserProfile(profile, patchToProfile(patch))
	if merged.UpdatedAt.Equal(profile.UpdatedAt) {
		// No-op merge: nothing to save.
		return nil
	}
	if err := SaveUserProfile(projectDir, merged); err != nil {
		return fmt.Errorf("save USER.md: %w", err)
	}
	return nil
}

func nudgeEnabled() bool {
	return os.Getenv("NANIKA_USER_PROFILE_NUDGE") == "1"
}

// readWorkerOutputs concatenates the workers' output.md files in mtime order
// (oldest first), capped at nudgeMaxTranscript runes. Returns "" when the
// workspace path is empty or no worker output is found.
func readWorkerOutputs(workspacePath string) string {
	if workspacePath == "" {
		return ""
	}
	workersDir := filepath.Join(workspacePath, "workers")
	entries, err := os.ReadDir(workersDir)
	if err != nil {
		return ""
	}

	type entry struct {
		path  string
		mtime time.Time
	}
	var files []entry
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		p := filepath.Join(workersDir, e.Name(), "output.md")
		info, err := os.Stat(p)
		if err != nil {
			continue
		}
		files = append(files, entry{path: p, mtime: info.ModTime()})
	}
	sort.Slice(files, func(i, j int) bool {
		return files[i].mtime.Before(files[j].mtime)
	})

	var sb strings.Builder
	for _, f := range files {
		data, err := os.ReadFile(f.path)
		if err != nil {
			continue
		}
		if sb.Len() > 0 {
			sb.WriteString("\n\n")
		}
		sb.Write(data)
		if sb.Len() >= nudgeMaxTranscript {
			break
		}
	}
	out := sb.String()
	if len(out) > nudgeMaxTranscript {
		out = out[:nudgeMaxTranscript]
	}
	return out
}

// renderProfileSummary renders the durable-profile fields of a UserProfile
// as a compact bullet list suitable for anchoring the model.
func renderProfileSummary(profile *UserProfile) string {
	var sb strings.Builder
	if profile == nil {
		sb.WriteString("(none)")
		return sb.String()
	}
	if profile.Role != "" {
		sb.WriteString(fmt.Sprintf("- Role: %s\n", profile.Role))
	}
	if len(profile.Preferences) > 0 {
		sb.WriteString(fmt.Sprintf("- Preferences: %v\n", profile.Preferences))
	}
	if len(profile.CommunicationStyle) > 0 {
		sb.WriteString(fmt.Sprintf("- Communication style: %v\n", profile.CommunicationStyle))
	}
	if len(profile.OngoingProjects) > 0 {
		sb.WriteString(fmt.Sprintf("- Ongoing projects: %v\n", profile.OngoingProjects))
	}
	if sb.Len() == 0 {
		sb.WriteString("(empty)")
	}
	return sb.String()
}

func buildNudgePrompt(profile *UserProfile, transcript string) string {
	system := `You are extracting durable USER PROFILE signals from a software-engineering session transcript.

Output ONLY a single JSON object — no prose, no code fences, no preface, no trailing text. The object must conform exactly to the schema in the user message. If the session contains no new durable signals, output {}.

Rules:
- A "durable signal" is a stable trait of the user (role, ongoing project, communication preference, working-style preference). NOT a one-off task detail, NOT a fact about the codebase, NOT something that could change next session.
- Never invent or generalize. If a field is not clearly evidenced in the transcript, omit it.
- For list fields, propose at most 5 items per field, each <= 120 characters.
- Never restate items already present in the existing profile shown to you — only new or refined signals.
`

	user := fmt.Sprintf(`Existing user profile (do not restate items already here):
%s

Schema (omit any field with no new signal):
{
  "role": "string — one short phrase, e.g. 'fullstack engineer at AKQA'",
  "preferences": ["string", "..."],
  "communication_style": ["string", "..."],
  "ongoing_projects": ["string", "..."]
}

Session transcript (worker outputs from the just-finished mission, truncated):
%s

Reply with the JSON object only.`, renderProfileSummary(profile), transcript)

	return system + "\n\n" + user
}

// parseNudgePatch is tolerant: it locates the first balanced JSON object in
// the raw response (handling code fences and surrounding prose) and decodes
// it. Unknown fields are ignored. Anything else returns an error.
func parseNudgePatch(raw string) (*nudgePatch, error) {
	if strings.TrimSpace(raw) == "" {
		return nil, fmt.Errorf("nudge: empty response")
	}
	start := strings.IndexByte(raw, '{')
	end := strings.LastIndexByte(raw, '}')
	if start < 0 || end <= start {
		return nil, fmt.Errorf("nudge: no JSON object found")
	}
	var p nudgePatch
	if err := json.Unmarshal([]byte(raw[start:end+1]), &p); err != nil {
		return nil, fmt.Errorf("nudge: invalid JSON: %w", err)
	}
	return &p, nil
}

func patchToProfile(p *nudgePatch) *UserProfile {
	if p == nil {
		return nil
	}
	return &UserProfile{
		Role:               p.Role,
		Preferences:        p.Preferences,
		CommunicationStyle: p.CommunicationStyle,
		OngoingProjects:    p.OngoingProjects,
	}
}

// MergeUserProfile returns a new *UserProfile that combines the persisted
// profile (existing) with a model-proposed patch. The merge is intentionally
// conservative: the patch can only add or refine, never delete.
//
//  1. Nil-safety: a nil existing is treated as an empty profile; a nil patch
//     is a no-op (returns a clone of existing).
//  2. Empty fields in the patch never clobber existing values.
//  3. Scalar fields (Role) are replaced only when non-empty and different.
//     List fields are unioned (case-insensitive on TrimSpace), preserving
//     existing order, then capped at maxProfileListLen with FIFO eviction.
//  4. Each list item is trimmed and capped at maxProfileItemLen runes; items
//     that become empty after trimming are dropped.
//  5. Name and OptedInForAutoUpdate are NEVER touched by the merge.
//  6. UpdatedAt is bumped only when something actually changed.
func MergeUserProfile(existing, patch *UserProfile) *UserProfile {
	if existing == nil {
		existing = &UserProfile{}
	}
	if patch == nil {
		clone := *existing
		clone.Preferences = append([]string(nil), existing.Preferences...)
		clone.CommunicationStyle = append([]string(nil), existing.CommunicationStyle...)
		clone.OngoingProjects = append([]string(nil), existing.OngoingProjects...)
		return &clone
	}

	out := *existing
	out.Preferences = append([]string(nil), existing.Preferences...)
	out.CommunicationStyle = append([]string(nil), existing.CommunicationStyle...)
	out.OngoingProjects = append([]string(nil), existing.OngoingProjects...)

	changed := false

	if v := strings.TrimSpace(patch.Role); v != "" && v != out.Role {
		out.Role = capItem(v)
		changed = true
	}

	if merged, added := unionCapped(out.Preferences, patch.Preferences); added {
		out.Preferences = merged
		changed = true
	}
	if merged, added := unionCapped(out.CommunicationStyle, patch.CommunicationStyle); added {
		out.CommunicationStyle = merged
		changed = true
	}
	if merged, added := unionCapped(out.OngoingProjects, patch.OngoingProjects); added {
		out.OngoingProjects = merged
		changed = true
	}

	if changed {
		out.UpdatedAt = time.Now()
	}
	return &out
}

// unionCapped returns the merged list and a bool indicating whether any new
// items were added. Empty/whitespace-only items are ignored. Comparison is
// case-insensitive on TrimSpace. The result is FIFO-capped at maxProfileListLen.
func unionCapped(existing, patch []string) ([]string, bool) {
	if len(patch) == 0 {
		return existing, false
	}
	seen := make(map[string]struct{}, len(existing)+len(patch))
	out := make([]string, 0, len(existing)+len(patch))
	for _, s := range existing {
		v := capItem(s)
		k := strings.ToLower(strings.TrimSpace(v))
		if k == "" {
			continue
		}
		if _, dup := seen[k]; dup {
			continue
		}
		seen[k] = struct{}{}
		out = append(out, v)
	}
	added := false
	for _, s := range patch {
		v := capItem(s)
		k := strings.ToLower(strings.TrimSpace(v))
		if k == "" {
			continue
		}
		if _, dup := seen[k]; dup {
			continue
		}
		seen[k] = struct{}{}
		out = append(out, v)
		added = true
	}
	if len(out) > maxProfileListLen {
		drop := len(out) - maxProfileListLen
		out = out[drop:]
	}
	return out, added
}

func capItem(s string) string {
	s = strings.TrimSpace(s)
	r := []rune(s)
	if len(r) <= maxProfileItemLen {
		return s
	}
	return strings.TrimSpace(string(r[:maxProfileItemLen]))
}

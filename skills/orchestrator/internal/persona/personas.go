// Package persona loads and matches persona files from ~/nanika/personas/*.md.
package persona

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/fsnotify/fsnotify"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

// Persona holds a loaded persona definition.
type Persona struct {
	Name          string   // filename without .md (e.g., "architect")
	Title         string   // first line heading (e.g., "System Architect — Designs systems worth building twice")
	Content       string   // raw file content (used for change detection on reload)
	PromptBody    string   // content with frontmatter stripped (injected into CLAUDE.md)
	Role          string   // frontmatter role (e.g., "implementer"); empty if no frontmatter
	Capabilities  []string // frontmatter capabilities; nil if no frontmatter
	Triggers      []string // frontmatter triggers; nil if no frontmatter
	Handoffs      []string // frontmatter handoffs (raw names); nil if no frontmatter
	InferredRole  string   // inferred legacy role when frontmatter is absent
	Expertise     []string // legacy capabilities extracted from ## Expertise
	WhenToUse     []string // keyword-routing hints: prefer ## When to Use, else frontmatter triggers
	WhenNotUse    []string // extracted "When NOT to Use" items
	LearningFocus []string // extracted "Learning Focus" areas
	HandsOffTo     []string // persona names to hand off to (from frontmatter handoffs or WhenNotUse)
	OutputRequires []string // frontmatter output_requires patterns for contract validation
}

// mu protects catalog for concurrent hot-reload access.
var mu sync.RWMutex

// catalog holds all loaded personas, keyed by name.
var catalog map[string]*Persona

// overrideDir allows configuring the personas directory at runtime.
var overrideDir string

// SetDir overrides the default personas directory (~/nanika/personas/).
// Call before Load().
func SetDir(dir string) {
	overrideDir = dir
}

// readCatalog reads all persona files from the personas directory and returns a
// new catalog map. It holds no locks; callers are responsible for thread safety.
func readCatalog() (map[string]*Persona, error) {
	dir := personasDir()
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}

	cat := make(map[string]*Persona)
	for _, e := range entries {
		var name, content string

		if e.IsDir() {
			// Convention: <dirname>/<dirname>.md — e.g. lifekeeper/lifekeeper.md
			candidate := filepath.Join(dir, e.Name(), e.Name()+".md")
			data, err := os.ReadFile(candidate)
			if err != nil {
				continue // no <dirname>/<dirname>.md, skip
			}
			name = e.Name()
			content = string(data)
		} else {
			if !strings.HasSuffix(e.Name(), ".md") {
				continue
			}
			data, err := os.ReadFile(filepath.Join(dir, e.Name()))
			if err != nil {
				continue
			}
			name = strings.TrimSuffix(e.Name(), ".md")
			content = string(data)
		}

		p := &Persona{
			Name:    name,
			Content: content,
		}

		// Parse YAML frontmatter; body is the prompt content sans frontmatter.
		fm, body := parseFrontmatter(content)
		if body == "" {
			body = content // no frontmatter: body is the whole file
		}
		p.PromptBody = body
		p.Role = fm.Role
		p.Capabilities = fm.Capabilities
		p.Triggers = fm.Triggers
		p.Handoffs = fm.Handoffs
		p.OutputRequires = fm.OutputRequires

		// Extract title from the first line of the body (already has leading newlines trimmed).
		lines := strings.SplitN(body, "\n", 2)
		if len(lines) > 0 {
			p.Title = strings.TrimPrefix(strings.TrimSpace(lines[0]), "# ")
		}

		// WhenToUse powers keyword scoring, so keep the richer markdown bullets
		// when they exist. Frontmatter triggers remain available separately for
		// metadata-driven routing and as a fallback for minimal hot-plug personas.
		p.WhenToUse = extractSection(body, "## When to Use")
		if len(p.WhenToUse) == 0 {
			p.WhenToUse = fm.Triggers
		}

		p.Expertise = extractSection(body, "## Expertise")
		p.WhenNotUse = extractSection(body, "## When NOT to Use")
		p.LearningFocus = extractSection(body, "## Learning Focus")
		p.InferredRole = inferLegacyRole(name, p.Title, p.WhenToUse)

		cat[name] = p
	}

	// Second pass: resolve HandsOffTo now that all persona names are known.
	// Prefer frontmatter handoffs (explicit list); fall back to parsing
	// "hand off to X" patterns from the ## When NOT to Use section.
	for _, p := range cat {
		if len(p.Handoffs) > 0 {
			p.HandsOffTo = filterHandoffs(p.Handoffs, cat)
		} else {
			p.HandsOffTo = extractHandoffs(p.WhenNotUse, cat)
		}
	}

	return cat, nil
}

// Load reads all persona files from the personas directory and replaces the
// in-memory catalog atomically. Safe to call at any time for an initial or forced load.
func Load() error {
	newCat, err := readCatalog()
	if err != nil {
		return err
	}
	mu.Lock()
	catalog = newCat
	mu.Unlock()
	return nil
}

// Reload reloads the persona catalog from disk, logs any added/removed/modified
// personas, and replaces the in-memory catalog atomically.
func Reload() error {
	// Snapshot old catalog (read lock only — non-blocking for readers).
	mu.RLock()
	prev := catalog
	mu.RUnlock()

	newCat, err := readCatalog()
	if err != nil {
		return fmt.Errorf("persona reload: %w", err)
	}

	logCatalogChanges(prev, newCat)

	mu.Lock()
	catalog = newCat
	mu.Unlock()
	return nil
}

// logCatalogChanges prints added/removed/modified persona names to stderr.
func logCatalogChanges(prev, next map[string]*Persona) {
	for name := range next {
		if _, ok := prev[name]; !ok {
			fmt.Fprintf(os.Stderr, "persona: added %q\n", name)
		} else if prev[name].Content != next[name].Content {
			fmt.Fprintf(os.Stderr, "persona: modified %q\n", name)
		}
	}
	for name := range prev {
		if _, ok := next[name]; !ok {
			fmt.Fprintf(os.Stderr, "persona: removed %q\n", name)
		}
	}
}

// StartWatcher watches the personas directory for .md file changes and reloads
// the catalog on create/modify/delete/rename events. Falls back to 30-second
// polling if fsnotify cannot watch the directory. Runs until ctx is cancelled.
func StartWatcher(ctx context.Context) {
	dir := personasDir()

	watcher, err := fsnotify.NewWatcher()
	if err != nil {
		fmt.Fprintf(os.Stderr, "persona: fsnotify unavailable (%v), falling back to 30s polling\n", err)
		go pollReload(ctx, 30*time.Second)
		return
	}

	if err := addPersonaWatchPaths(watcher, dir); err != nil {
		watcher.Close()
		fmt.Fprintf(os.Stderr, "persona: cannot watch %s (%v), falling back to 30s polling\n", dir, err)
		go pollReload(ctx, 30*time.Second)
		return
	}

	go func() {
		defer watcher.Close()
		// Debounce: collect events for a short window before reloading,
		// so a rapid sequence of writes (e.g. editor save) triggers one reload.
		var debounce <-chan time.Time
		for {
			select {
			case <-ctx.Done():
				return
			case ev, ok := <-watcher.Events:
				if !ok {
					return
				}
				if ev.Has(fsnotify.Create) {
					if info, err := os.Stat(ev.Name); err == nil && info.IsDir() && isDirectPersonaChild(dir, ev.Name) {
						if err := watcher.Add(ev.Name); err != nil {
							fmt.Fprintf(os.Stderr, "persona: cannot watch %s (%v)\n", ev.Name, err)
						}
					}
				}
				if !shouldReloadForEvent(dir, ev.Name) {
					continue
				}
				if ev.Has(fsnotify.Create) || ev.Has(fsnotify.Write) ||
					ev.Has(fsnotify.Remove) || ev.Has(fsnotify.Rename) {
					debounce = time.After(50 * time.Millisecond)
				}
			case _, ok := <-watcher.Errors:
				if !ok {
					return
				}
				// Watcher errors are non-fatal; keep watching.
			case <-debounce:
				debounce = nil
				if err := Reload(); err != nil {
					fmt.Fprintf(os.Stderr, "persona: reload error: %v\n", err)
				}
			}
		}
	}()
}

// pollReload reloads the catalog every interval until ctx is cancelled.
// Used as a fallback when fsnotify is unavailable.
func pollReload(ctx context.Context, interval time.Duration) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			if err := Reload(); err != nil {
				fmt.Fprintf(os.Stderr, "persona: poll reload error: %v\n", err)
			}
		}
	}
}

// ensureLoaded guarantees the catalog is populated at least once.
// Uses double-checked locking to be safe under concurrent access.
func ensureLoaded() {
	mu.RLock()
	loaded := catalog != nil
	mu.RUnlock()
	if loaded {
		return
	}
	// Slow path: acquire write lock and load if still nil.
	mu.Lock()
	if catalog == nil {
		c, _ := readCatalog()
		catalog = c
	}
	mu.Unlock()
}

// Get returns a persona by name, or nil if not found.
func Get(name string) *Persona {
	ensureLoaded()
	mu.RLock()
	defer mu.RUnlock()
	return catalog[name]
}

// GetPrompt returns the persona prompt body (frontmatter stripped) for injection
// into CLAUDE.md. Falls back to raw Content for personas without frontmatter.
func GetPrompt(name string) string {
	p := Get(name)
	if p == nil {
		return ""
	}
	if p.PromptBody != "" {
		return p.PromptBody
	}
	return p.Content
}

// GetLearningFocus returns the learning focus areas for a persona.
// Returns nil if persona not found or has no learning focus section.
func GetLearningFocus(name string) []string {
	p := Get(name)
	if p == nil {
		return nil
	}
	return p.LearningFocus
}

// Names returns all loaded persona names.
func Names() []string {
	ensureLoaded()
	mu.RLock()
	defer mu.RUnlock()
	names := make([]string, 0, len(catalog))
	for name := range catalog {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

// All returns a snapshot of all loaded personas.
func All() map[string]*Persona {
	ensureLoaded()
	mu.RLock()
	defer mu.RUnlock()
	snapshot := make(map[string]*Persona, len(catalog))
	for k, v := range catalog {
		snapshot[k] = v
	}
	return snapshot
}

// HasRole reports whether the named persona has the given effective role.
// Frontmatter is authoritative; legacy personas fall back to inferred role.
func HasRole(name, role string) bool {
	p := Get(name)
	if p == nil {
		return false
	}
	effective := p.Role
	if effective == "" {
		effective = p.InferredRole
	}
	if effective == "" {
		return false
	}
	return strings.EqualFold(effective, role)
}

// NamesWithRole returns all persona names whose effective role matches role.
// Falls back to the provided fallbackNames only when no catalog persona matches.
func NamesWithRole(role string, fallbackNames []string) []string {
	ensureLoaded()
	mu.RLock()
	defer mu.RUnlock()

	seen := make(map[string]bool)
	var names []string
	for name, p := range catalog {
		effective := p.Role
		if effective == "" {
			effective = p.InferredRole
		}
		if effective != "" && strings.EqualFold(effective, role) && !seen[name] {
			seen[name] = true
			names = append(names, name)
		}
	}
	if len(names) == 0 {
		for _, name := range fallbackNames {
			if !seen[name] {
				seen[name] = true
				names = append(names, name)
			}
		}
	}
	sort.Strings(names)
	return names
}

// HasCapability reports whether the named persona exposes capability
// (case-insensitive) through frontmatter capabilities or legacy expertise.
func HasCapability(name, capability string) bool {
	p := Get(name)
	if p == nil {
		return false
	}
	capLower := strings.ToLower(capability)
	for _, c := range p.Capabilities {
		if normalizeCapability(c) == capLower {
			return true
		}
	}
	for _, c := range p.Expertise {
		if normalizeCapability(c) == capLower {
			return true
		}
	}
	return false
}

func normalizeCapability(capability string) string {
	trimmed := strings.TrimSpace(capability)
	if idx := strings.Index(trimmed, " ("); idx > 0 {
		trimmed = trimmed[:idx]
	}
	return strings.ToLower(trimmed)
}

func inferLegacyRole(name, title string, whenToUse []string) string {
	text := strings.ToLower(strings.Join(append([]string{name, title}, whenToUse...), " "))
	switch {
	case containsAnyFold(text, "reviewer", "review", "auditor", "audit", "qa", "verify", "validation"):
		return "reviewer"
	case containsAnyFold(text, "architect", "architecture", "researcher", "research", "writer", "documentation", "docs", "design", "plan", "analyst", "analysis"):
		return "planner"
	case text != "":
		return "implementer"
	default:
		return ""
	}
}

func containsAnyFold(text string, needles ...string) bool {
	for _, needle := range needles {
		if strings.Contains(text, strings.ToLower(needle)) {
			return true
		}
	}
	return false
}

func addPersonaWatchPaths(watcher *fsnotify.Watcher, dir string) error {
	if err := watcher.Add(dir); err != nil {
		return err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		path := filepath.Join(dir, e.Name())
		if err := watcher.Add(path); err != nil {
			fmt.Fprintf(os.Stderr, "persona: cannot watch %s (%v)\n", path, err)
		}
	}
	return nil
}

func shouldReloadForEvent(root, path string) bool {
	if strings.HasSuffix(path, ".md") {
		return true
	}
	return isDirectPersonaChild(root, path)
}

func isDirectPersonaChild(root, path string) bool {
	return filepath.Dir(filepath.Clean(path)) == filepath.Clean(root)
}

// SelectionMethod indicates how a persona was selected.
type SelectionMethod string

const (
	// SelectionExplicit means the persona was named directly by the caller
	// (e.g., hand-written PHASE line or API parameter).
	SelectionExplicit SelectionMethod = "explicit"
	// SelectionCorrection means the persona was chosen from an explicit routing
	// correction recorded for this target/task shape.
	SelectionCorrection SelectionMethod = "correction"
	// SelectionTargetProfile means the persona was chosen from stored target-profile
	// facts about the task's target (e.g., "orchestrator → Go → senior-golang-engineer").
	SelectionTargetProfile SelectionMethod = "target_profile"
	// SelectionRoutingPattern means the persona was chosen from a learned routing
	// pattern recorded for a task/target combination.
	SelectionRoutingPattern SelectionMethod = "routing_pattern"
	// SelectionLLM means the LLM picked the persona during decomposition or matching.
	SelectionLLM SelectionMethod = "llm"
	// SelectionKeyword means offline keyword scoring picked the persona.
	SelectionKeyword SelectionMethod = "keyword"
	// SelectionFallback means no positive-scoring match was found and the persona
	// was assigned as an explicit default (e.g., senior-backend-engineer for
	// implementation tasks when no language-specific persona exists).
	SelectionFallback SelectionMethod = "fallback"
)

// Match finds the best persona for a task description.
// Uses LLM (Haiku) for intelligent classification, falls back to keyword matching.
func Match(task string) string {
	name, _ := MatchWithMethod(task)
	return name
}

// MatchWithMethod is like Match but also returns the selection method used.
func MatchWithMethod(task string) (string, SelectionMethod) {
	ensureLoaded()

	// Try LLM-based matching first
	name, err := llmMatch(context.Background(), task)
	if err == nil && Get(name) != nil {
		return name, SelectionLLM
	}

	// Fallback: keyword matching from WhenToUse/WhenNotUse triggers.
	// Returns SelectionKeyword when a positive match is found, SelectionFallback
	// when keyword scoring finds nothing and the alphabetical default fires.
	return keywordMatchWithMethod(task)
}

// llmMatchModel is the model used for LLM-based persona matching.
// Override with PERSONA_MATCH_MODEL env var (e.g. "sonnet", "opus").
// Default is "haiku" for speed and cost efficiency.
var llmMatchModel = func() string {
	if m := os.Getenv("PERSONA_MATCH_MODEL"); m != "" {
		return m
	}
	return "haiku"
}()

// llmMatchTimeout is the maximum time allowed for an LLM persona match call.
// Silent failures from rate limits or stalls fall back to keyword matching.
const llmMatchTimeout = 15 * time.Second

// llmMatch asks an LLM to pick the best persona for a task.
// Model defaults to haiku; override with PERSONA_MATCH_MODEL env var.
func llmMatch(ctx context.Context, task string) (string, error) {
	summary := FormatForDecomposer()

	prompt := fmt.Sprintf(`Pick the single best persona for this task. Reply with ONLY the persona name, nothing else.

## Available Personas
%s

## Task
%s

Reply with just the persona name (e.g., "senior-backend-engineer"):`, summary, task)

	matchCtx, cancel := context.WithTimeout(ctx, llmMatchTimeout)
	defer cancel()

	output, err := sdk.QueryText(matchCtx, prompt, &sdk.AgentOptions{
		Model:    llmMatchModel,
		MaxTurns: 1,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "[persona] llmMatch failed (model=%s): %v — falling back to keyword\n", llmMatchModel, err)
		return "", err
	}

	// Extract persona name from response
	name := strings.TrimSpace(strings.ToLower(output))
	// Clean up markdown formatting if present
	name = strings.Trim(name, "`\"'")
	// Take first line only
	if idx := strings.IndexByte(name, '\n'); idx > 0 {
		name = name[:idx]
	}
	name = strings.TrimSpace(name)

	return name, nil
}

// wordMatchesTask checks if a trigger word matches anywhere in the task text.
// Uses prefix matching to handle verb stems: "implement" matches "implementing",
// "write" matches "writing", etc. Minimum shared prefix of 4 chars.
func wordMatchesTask(word string, taskWords []string) bool {
	for _, tw := range taskWords {
		if word == tw {
			return true
		}
		// Prefix match: shorter must be prefix of longer, min 4 chars shared
		short, long := word, tw
		if len(short) > len(long) {
			short, long = long, short
		}
		if len(short) >= 4 && strings.HasPrefix(long, short) {
			return true
		}
	}
	return false
}

// buildTaskWords splits and cleans a lowercased task string into words
// suitable for prefix matching. Words shorter than 4 chars are dropped.
// Callers should compute this once per task before scoring multiple personas.
func buildTaskWords(lower string) []string {
	fields := strings.Fields(lower)
	words := make([]string, 0, len(fields))
	for _, w := range fields {
		w = strings.Trim(w, "(),./")
		if len(w) >= 4 {
			words = append(words, w)
		}
	}
	return words
}

// scoreKeywords computes a net keyword score for a persona against a task.
// Positive from WhenToUse triggers (+1 per word), negative from WhenNotUse (-1 per word ≥6 chars),
// plus a name-stem bonus (+3).
// taskWords must be pre-computed via buildTaskWords(lower) by the caller.
func scoreKeywords(name string, p *Persona, lower string, taskWords []string) int {
	score := 0

	// Positive: WhenToUse triggers
	for _, trigger := range p.WhenToUse {
		for _, word := range strings.Fields(strings.ToLower(trigger)) {
			word = strings.Trim(word, "(),.")
			if len(word) < 4 {
				continue
			}
			if wordMatchesTask(word, taskWords) {
				score++
			}
		}
	}

	// Negative: WhenNotUse triggers penalize mismatches.
	// Only penalize words ≥6 chars to avoid common words firing false penalties.
	for _, trigger := range p.WhenNotUse {
		for _, word := range strings.Fields(strings.ToLower(trigger)) {
			word = strings.Trim(word, "(),.")
			if len(word) < 6 {
				continue
			}
			if wordMatchesTask(word, taskWords) {
				score--
			}
		}
	}

	// Persona name stem match
	nameStem := strings.TrimSuffix(strings.TrimSuffix(name, "-engineer"), "er")
	if len(nameStem) >= 4 && strings.Contains(lower, nameStem) {
		score += 3
	}

	return score
}

// alphabeticalFallback returns the alphabetically first persona name from the catalog.
// Used as a deterministic fallback when no persona scores above 0.
// Caller must hold at least a read lock.
func alphabeticalFallback() string {
	names := make([]string, 0, len(catalog))
	for name := range catalog {
		names = append(names, name)
	}
	sort.Strings(names)
	return names[0]
}

// keywordMatch is the offline fallback when LLM is unavailable.
// Scores personas using WhenToUse triggers (+1), WhenNotUse penalties (-2),
// and name-stem bonuses (+3).
func keywordMatch(task string) string {
	name, _ := keywordMatchWithMethod(task)
	return name
}

// keywordMatchWithMethod is like keywordMatch but also returns the selection method.
// Returns SelectionKeyword when a persona scores above 0, SelectionFallback when
// no positive match exists and the alphabetical fallback is used.
func keywordMatchWithMethod(task string) (string, SelectionMethod) {
	name, method := MatchWithMethodCandidates(task, Names())
	if method == SelectionKeyword {
		return name, SelectionKeyword
	}
	// No positive-scoring match: use alphabetical fallback with explicit label.
	mu.RLock()
	fb := alphabeticalFallback()
	mu.RUnlock()
	return fb, SelectionFallback
}

// MatchWithMethodCandidates runs keyword scoring over only the listed candidate
// persona names. Returns (name, SelectionKeyword) when a candidate scores above 0,
// or ("", SelectionFallback) when no candidate matches the task intent.
// Unknown names in candidates are silently skipped.
func MatchWithMethodCandidates(task string, candidates []string) (string, SelectionMethod) {
	ensureLoaded()
	lower := strings.ToLower(task)
	taskWords := buildTaskWords(lower)
	bestName := ""
	bestScore := 0

	mu.RLock()
	for _, name := range candidates {
		p, ok := catalog[name]
		if !ok {
			continue
		}
		score := scoreKeywords(name, p, lower, taskWords)
		if score > bestScore || (score > 0 && bestName == "") {
			bestScore = score
			bestName = name
		} else if score == bestScore && bestScore > 0 && name < bestName {
			// Tiebreak: alphabetically first for determinism.
			bestName = name
		}
	}
	mu.RUnlock()

	if bestName != "" {
		return bestName, SelectionKeyword
	}
	return "", SelectionFallback
}

// MatchTop returns the top N matching personas for a task.
// Uses keyword scoring only (LLM returns a single pick, not a ranking).
func MatchTop(task string, n int) []string {
	ensureLoaded()

	lower := strings.ToLower(task)
	taskWords := buildTaskWords(lower)

	type scored struct {
		name  string
		score int
	}
	var scores []scored

	mu.RLock()
	for name, p := range catalog {
		score := scoreKeywords(name, p, lower, taskWords)
		if score > 0 {
			scores = append(scores, scored{name, score})
		}
	}
	fb := alphabeticalFallback()
	mu.RUnlock()

	// Sort by score descending, then alphabetically for stability
	sort.Slice(scores, func(i, j int) bool {
		if scores[i].score != scores[j].score {
			return scores[i].score > scores[j].score
		}
		return scores[i].name < scores[j].name
	})

	result := make([]string, 0, n)
	for i := 0; i < len(scores) && i < n; i++ {
		result = append(result, scores[i].name)
	}

	// Deterministic fallback: alphabetically first persona
	if len(result) == 0 {
		result = append(result, fb)
	}

	return result
}

// FormatForDecomposer produces a summary of all personas for the LLM decomposer.
// Includes role and capabilities from frontmatter when available.
func FormatForDecomposer() string {
	ensureLoaded()

	mu.RLock()
	// Sort persona names for deterministic LLM context
	names := make([]string, 0, len(catalog))
	for name := range catalog {
		names = append(names, name)
	}
	sort.Strings(names)

	var b strings.Builder
	for _, name := range names {
		p := catalog[name]
		b.WriteString("- **")
		b.WriteString(name)
		b.WriteString("**")
		if p.Role != "" {
			b.WriteString(" (role: ")
			b.WriteString(p.Role)
			b.WriteString(")")
		}
		b.WriteString(": ")
		b.WriteString(p.Title)
		b.WriteString("\n")
		if len(p.Capabilities) > 0 {
			b.WriteString("  Capabilities: ")
			b.WriteString(strings.Join(p.Capabilities, ", "))
			b.WriteString("\n")
		}
		for _, trigger := range p.WhenToUse {
			b.WriteString("  - ")
			b.WriteString(trigger)
			b.WriteString("\n")
		}
		if len(p.HandsOffTo) > 0 {
			b.WriteString("  Hands off to: ")
			b.WriteString(strings.Join(p.HandsOffTo, ", "))
			b.WriteString("\n")
		}
	}
	mu.RUnlock()
	return b.String()
}

// handoffPattern matches "hand off to <persona-name>" in WhenNotUse entries.
var handoffPattern = regexp.MustCompile(`hand off to ([\w][\w-]*)`)

// extractHandoffs parses WhenNotUse entries for "hand off to <name>" patterns.
// Only returns names that exist in the catalog.
func extractHandoffs(items []string, cat map[string]*Persona) []string {
	seen := make(map[string]bool)
	var targets []string

	for _, item := range items {
		for _, match := range handoffPattern.FindAllStringSubmatch(item, -1) {
			name := match[1]
			if _, ok := cat[name]; ok && !seen[name] {
				seen[name] = true
				targets = append(targets, name)
			}
		}
	}

	sort.Strings(targets)
	return targets
}

// frontmatter holds the structured fields parsed from YAML frontmatter.
type frontmatter struct {
	Role           string
	Capabilities   []string
	Triggers       []string
	Handoffs       []string
	OutputRequires []string
}

// parseFrontmatter splits a persona file into its YAML frontmatter and body.
// Returns zero-value frontmatter and the original content unchanged when no
// valid frontmatter block (opening and closing ---) is found.
func parseFrontmatter(content string) (frontmatter, string) {
	if !strings.HasPrefix(content, "---\n") {
		return frontmatter{}, content
	}
	rest := content[4:] // skip opening "---\n"

	// Locate the closing --- at the start of a line.
	closeIdx := strings.Index(rest, "\n---")
	if closeIdx < 0 {
		return frontmatter{}, content
	}
	// The character after "---" must be '\n' or end-of-string.
	afterClose := closeIdx + 4
	if afterClose < len(rest) && rest[afterClose] != '\n' {
		return frontmatter{}, content
	}

	yamlBlock := rest[:closeIdx]
	body := ""
	if afterClose < len(rest) {
		body = rest[afterClose+1:] // skip the '\n' after "---"
	}
	// Trim leading blank lines so the returned body starts at the first content line.
	body = strings.TrimLeft(body, "\n")

	return parseSimpleYAML(yamlBlock), body
}

// parseSimpleYAML parses the simple YAML used in persona frontmatter.
// Handles scalar strings (key: value) and string lists (key:\n  - item).
// Does not support nested mappings or other YAML types.
func parseSimpleYAML(block string) frontmatter {
	var fm frontmatter
	currentKey := ""
	for _, line := range strings.Split(block, "\n") {
		if strings.HasPrefix(line, "  - ") {
			item := strings.TrimSpace(line[4:])
			// Strip surrounding quotes (YAML scalar quoting).
			if len(item) >= 2 && ((item[0] == '"' && item[len(item)-1] == '"') || (item[0] == '\'' && item[len(item)-1] == '\'')) {
				item = item[1 : len(item)-1]
			}
			switch currentKey {
			case "capabilities":
				fm.Capabilities = append(fm.Capabilities, item)
			case "triggers":
				fm.Triggers = append(fm.Triggers, item)
			case "handoffs":
				fm.Handoffs = append(fm.Handoffs, item)
			case "output_requires":
				fm.OutputRequires = append(fm.OutputRequires, item)
			}
			continue
		}
		if idx := strings.Index(line, ": "); idx > 0 {
			key := line[:idx]
			val := line[idx+2:]
			currentKey = key
			if key == "role" {
				fm.Role = val
			}
		} else if strings.HasSuffix(line, ":") {
			currentKey = strings.TrimSuffix(line, ":")
		}
	}
	return fm
}

// filterHandoffs returns the subset of names that exist in cat, sorted and
// deduplicated. Used when frontmatter handoffs are present.
func filterHandoffs(names []string, cat map[string]*Persona) []string {
	seen := make(map[string]bool)
	var result []string
	for _, name := range names {
		if _, ok := cat[name]; ok && !seen[name] {
			seen[name] = true
			result = append(result, name)
		}
	}
	sort.Strings(result)
	return result
}

func extractSection(content, heading string) []string {
	lines := strings.Split(content, "\n")
	var items []string
	inSection := false

	for _, line := range lines {
		if strings.HasPrefix(line, heading) {
			inSection = true
			continue
		}
		if inSection && strings.HasPrefix(line, "## ") {
			break // next section
		}
		if inSection && strings.HasPrefix(strings.TrimSpace(line), "- ") {
			item := strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(line), "- "))
			items = append(items, item)
		}
	}

	return items
}

func personasDir() string {
	if overrideDir != "" {
		return overrideDir
	}
	// Check ORCHESTRATOR_PERSONAS_DIR env
	if dir := os.Getenv("ORCHESTRATOR_PERSONAS_DIR"); dir != "" {
		return dir
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "personas"
	}
	return filepath.Join(home, "nanika", "personas")
}

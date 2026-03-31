package cmd

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// NoteType is the classification of a vault note.
type NoteType string

const (
	NoteTypeFleeting  NoteType = "fleeting"
	NoteTypeTask      NoteType = "task"
	NoteTypeReference NoteType = "reference"
	NoteTypeIdea      NoteType = "idea"
	NoteTypeNote      NoteType = "note"
)

// isValidNoteType reports whether t is a known NoteType.
func isValidNoteType(t NoteType) bool {
	switch t {
	case NoteTypeFleeting, NoteTypeTask, NoteTypeReference, NoteTypeIdea, NoteTypeNote:
		return true
	}
	return false
}

// llmConfidenceThreshold is the minimum confidence for an LLM classification
// to override the regex fallback.
const llmConfidenceThreshold = 0.75

// LLMClassifyResult holds the structured output from an LLM classifier.
type LLMClassifyResult struct {
	Type       NoteType `json:"type"`
	Confidence float64  `json:"confidence"`
	Entities   []string `json:"entities"`
}

// LLMClassifier classifies a note's text content.
// Implementations may be real API clients or mocks for testing.
type LLMClassifier interface {
	Classify(ctx context.Context, content string) (LLMClassifyResult, error)
}

// HaikuClassifier classifies notes using Claude Haiku via the Anthropic Messages API.
type HaikuClassifier struct {
	apiKey     string
	httpClient *http.Client
}

// NewHaikuClassifier returns a classifier backed by Claude Haiku.
// Returns nil when apiKey is empty so callers can use nil as a "no LLM" sentinel.
func NewHaikuClassifier(apiKey string) *HaikuClassifier {
	if apiKey == "" {
		return nil
	}
	return &HaikuClassifier{
		apiKey:     apiKey,
		httpClient: &http.Client{Timeout: 15 * time.Second},
	}
}

// anthropicMessagesResponse is the JSON shape returned by the Anthropic Messages API.
type anthropicMessagesResponse struct {
	Content []struct {
		Type string `json:"type"`
		Text string `json:"text"`
	} `json:"content"`
	Error *struct {
		Type    string `json:"type"`
		Message string `json:"message"`
	} `json:"error,omitempty"`
}

// callAnthropicMessages posts a request to the Anthropic Messages API and returns the parsed response.
// Shared by HaikuClassifier and ClaudeSummarizer to avoid duplicate HTTP boilerplate.
func callAnthropicMessages(ctx context.Context, httpClient *http.Client, apiKey, model string, maxTokens int, messages []map[string]string) (anthropicMessagesResponse, error) {
	reqBody := map[string]any{
		"model":      model,
		"max_tokens": maxTokens,
		"messages":   messages,
	}
	jsonBody, err := json.Marshal(reqBody)
	if err != nil {
		return anthropicMessagesResponse{}, fmt.Errorf("marshal request: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST",
		"https://api.anthropic.com/v1/messages", bytes.NewReader(jsonBody))
	if err != nil {
		return anthropicMessagesResponse{}, fmt.Errorf("create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("x-api-key", apiKey)
	req.Header.Set("anthropic-version", "2023-06-01")

	resp, err := httpClient.Do(req)
	if err != nil {
		return anthropicMessagesResponse{}, fmt.Errorf("http request: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return anthropicMessagesResponse{}, fmt.Errorf("read response: %w", err)
	}

	var apiResp anthropicMessagesResponse
	if err := json.Unmarshal(body, &apiResp); err != nil {
		return anthropicMessagesResponse{}, fmt.Errorf("decode response: %w", err)
	}
	if apiResp.Error != nil {
		return anthropicMessagesResponse{}, fmt.Errorf("API error: %s", apiResp.Error.Message)
	}
	return apiResp, nil
}

const classifySystemPrompt = `Classify the note below. Respond with JSON only — no prose, no code fences.

Schema: {"type":"task"|"reference"|"idea"|"note"|"fleeting","confidence":0.0–1.0,"entities":["…up to 5 key topics…"]}

Rules:
- task: actionable items, checkboxes, TODOs, follow-ups
- reference: URLs, citations, bookmarks, external sources
- note: structured content with multiple headings or sections
- idea: single concept, insight, or brainstorm
- fleeting: unclear, fragmentary, or unclassifiable capture
- confidence: how sure you are (1.0 = certain)
- entities: key named topics that may correspond to existing vault notes`

// Classify sends the note content to Claude Haiku and returns a structured result.
func (h *HaikuClassifier) Classify(ctx context.Context, content string) (LLMClassifyResult, error) {
	prompt := classifySystemPrompt + "\n\nNote:\n" + content

	apiResp, err := callAnthropicMessages(ctx, h.httpClient, h.apiKey,
		"claude-haiku-4-5-20251001", 256,
		[]map[string]string{{"role": "user", "content": prompt}})
	if err != nil {
		return LLMClassifyResult{}, err
	}
	if len(apiResp.Content) == 0 || apiResp.Content[0].Type != "text" {
		return LLMClassifyResult{}, fmt.Errorf("unexpected response format")
	}

	var result LLMClassifyResult
	if err := json.Unmarshal([]byte(apiResp.Content[0].Text), &result); err != nil {
		return LLMClassifyResult{}, fmt.Errorf("parse JSON in response: %w", err)
	}
	return result, nil
}

// TriageOptions holds flags for the triage command.
type TriageOptions struct {
	List       bool
	Auto       bool
	Older      string // duration string like "7d", "24h" — parsed by parseSinceDuration
	DryRun     bool
	JSONOutput bool
	Quiet      bool // suppress all output when nothing was processed (cron-friendly)
}

// PendingNote represents a note in the inbox awaiting triage.
type PendingNote struct {
	Path     string `json:"path"`
	Type     string `json:"type"`
	AgeDays  int    `json:"age_days"`
	AgeLabel string `json:"age_label"`
	Created  string `json:"created"`
}

// ProcessedNote represents a note that was triaged by --auto.
type ProcessedNote struct {
	FromPath   string   `json:"from_path"`
	ToPath     string   `json:"to_path"`
	NoteType   string   `json:"note_type"`
	LinksAdded []string `json:"links_added,omitempty"`
	DryRun     bool     `json:"dry_run,omitempty"`
	Appended   bool     `json:"appended,omitempty"` // true when content was appended to canonical
}

// TriageSummary holds aggregate counts for the triage run.
type TriageSummary struct {
	Total     int `json:"total"`
	Processed int `json:"processed"`
	Skipped   int `json:"skipped"`
	Errors    int `json:"errors"`
}

// TriageOutput is the full JSON output for the triage command.
type TriageOutput struct {
	Pending   []PendingNote   `json:"pending"`
	Processed []ProcessedNote `json:"processed"`
	Errors    []string        `json:"errors"`
	Summary   TriageSummary   `json:"summary"`
}

// TriageCmd triages notes in the Inbox/ folder.
// --list shows pending notes with age; --auto classifies, enriches, and moves them.
// Default mode (neither flag) is equivalent to --list.
func TriageCmd(vaultPath string, opts TriageOptions) error {
	// Default to --list when neither mode is specified.
	if !opts.List && !opts.Auto {
		opts.List = true
	}

	// Parse --older duration (empty string → 0, meaning no filter).
	olderDuration, err := parseSinceDuration(opts.Older)
	if err != nil {
		return fmt.Errorf("invalid --older value %q: %w", opts.Older, err)
	}

	// Guard: Inbox must exist.
	inboxPath := filepath.Join(vaultPath, "Inbox")
	if _, err := os.Stat(inboxPath); os.IsNotExist(err) {
		if opts.JSONOutput {
			return output.JSON(TriageOutput{})
		}
		fmt.Println("No inbox found (Inbox/ does not exist in vault)")
		return nil
	}

	notes, err := vault.ListNotes(vaultPath, "Inbox")
	if err != nil {
		return fmt.Errorf("listing inbox: %w", err)
	}

	result := TriageOutput{}
	now := time.Now()

	// Collect pending notes (skip already-processed ones; apply --older filter).
	for _, info := range notes {
		fullPath := filepath.Join(vaultPath, info.Path)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Sprintf("%s: %v", info.Path, err))
			continue
		}

		parsed := vault.ParseNote(string(data))
		status := frontmatterString(parsed.Frontmatter, "status")

		// Notes with status: processed have already been handled; skip them.
		if status == "processed" {
			continue
		}

		noteType := frontmatterString(parsed.Frontmatter, "type")
		created := frontmatterString(parsed.Frontmatter, "created")
		ageDays, createdLabel := computeAge(created, info.ModTime, now)

		// Apply --older filter: skip notes younger than the threshold.
		if olderDuration > 0 {
			thresholdDays := int(olderDuration.Hours() / 24)
			if ageDays < thresholdDays {
				continue
			}
		}

		result.Pending = append(result.Pending, PendingNote{
			Path:     info.Path,
			Type:     noteType,
			AgeDays:  ageDays,
			AgeLabel: formatAgeLabel(ageDays),
			Created:  createdLabel,
		})
	}

	result.Summary.Total = len(result.Pending)

	// --auto: classify, enrich, rewrite frontmatter, move each pending note.
	if opts.Auto {
		// Open index for wikilink enrichment (best-effort; skipped if not built).
		var store *index.Store
		dbPath := index.IndexDBPath(vaultPath)
		if _, err := os.Stat(dbPath); err == nil {
			if s, err := index.Open(dbPath); err == nil {
				store = s
				defer store.Close()
			}
		}

		// Create Haiku classifier if ANTHROPIC_API_KEY is set; nil → regex fallback.
		var llm LLMClassifier
		if apiKey := os.Getenv("ANTHROPIC_API_KEY"); apiKey != "" {
			llm = NewHaikuClassifier(apiKey)
		}

		for _, pending := range result.Pending {
			processed, err := triageNote(vaultPath, pending, store, llm, opts.DryRun, now)
			if err != nil {
				result.Errors = append(result.Errors, fmt.Sprintf("%s: %v", pending.Path, err))
				result.Summary.Errors++
				continue
			}
			result.Processed = append(result.Processed, processed)
			result.Summary.Processed++
		}
		result.Summary.Skipped = result.Summary.Total - result.Summary.Processed - result.Summary.Errors
	}

	if opts.JSONOutput {
		return output.JSON(result)
	}

	// --quiet: skip all output when the run produced nothing worth reporting.
	// Errors still surface so cron email catches failures.
	if opts.Quiet && result.Summary.Processed == 0 && len(result.Errors) == 0 {
		return nil
	}

	if opts.Auto {
		printTriageAutoReport(result, opts.DryRun)
	} else {
		printTriageListReport(result)
	}
	return nil
}

// triageNote classifies, enriches, rewrites frontmatter, and moves a single note.
// When llm is non-nil and returns a confident result, it overrides the regex classifier.
func triageNote(vaultPath string, pending PendingNote, store *index.Store, llm LLMClassifier, dryRun bool, now time.Time) (ProcessedNote, error) {
	fullPath := filepath.Join(vaultPath, pending.Path)
	data, err := os.ReadFile(fullPath)
	if err != nil {
		return ProcessedNote{}, fmt.Errorf("reading note: %w", err)
	}

	parsed := vault.ParseNote(string(data))

	// Step 1: Classify note type — LLM when available, regex fallback otherwise.
	ctx := context.Background()
	noteType, llmEntities := classifyWithLLM(ctx, parsed, llm)

	// Step 2: Find wikilink suggestions.
	// Entity-based matches (from LLM) take priority; cosine-similarity fills the rest.
	var linksAdded []string
	entityLinks := matchEntitiesAgainstVault(vaultPath, llmEntities)
	linksAdded = append(linksAdded, entityLinks...)

	if store != nil {
		for _, s := range enrichSingleNote(store, pending.Path) {
			toName := strings.TrimSuffix(filepath.Base(s.To), ".md")
			// Avoid duplicates from entity matching.
			if !containsStr(linksAdded, toName) {
				linksAdded = append(linksAdded, toName)
			}
		}
	}

	// Step 3: Compute destination path.
	toPath := triageDestination(pending.Path, noteType, parsed)

	// Step 4: Build updated note content.
	newContent := buildTriagedContent(parsed, noteType, linksAdded, now)

	if dryRun {
		return ProcessedNote{
			FromPath:   pending.Path,
			ToPath:     toPath,
			NoteType:   noteType,
			LinksAdded: linksAdded,
			DryRun:     true,
		}, nil
	}

	// Step 5: Write to destination.
	// If a canonical note already exists at the destination, append to it instead
	// of creating a duplicate or deconflicting with a timestamp suffix.
	targetFull := filepath.Join(vaultPath, toPath)
	appended := false
	if _, err := os.Stat(targetFull); err == nil {
		// Canonical exists — append the new body to it.
		if err := appendToCanonical(targetFull, parsed.Body, now); err != nil {
			return ProcessedNote{}, fmt.Errorf("appending to canonical: %w", err)
		}
		appended = true
	} else {
		if err := os.MkdirAll(filepath.Dir(targetFull), 0755); err != nil {
			return ProcessedNote{}, fmt.Errorf("creating target directory: %w", err)
		}
		if err := os.WriteFile(targetFull, []byte(newContent), 0644); err != nil {
			return ProcessedNote{}, fmt.Errorf("writing triaged note: %w", err)
		}
	}

	// Step 6: Remove original.
	if err := os.Remove(fullPath); err != nil {
		return ProcessedNote{}, fmt.Errorf("removing original note: %w", err)
	}

	return ProcessedNote{
		FromPath:   pending.Path,
		ToPath:     toPath,
		NoteType:   noteType,
		LinksAdded: linksAdded,
		Appended:   appended,
	}, nil
}

// classifyWithLLM classifies a note using the LLM when available and confident.
// Falls back to regex (classifyNoteType) when llm is nil, returns an error, or
// has confidence below llmConfidenceThreshold. Also returns extracted entities
// from the LLM for vault matching (nil on regex fallback).
func classifyWithLLM(ctx context.Context, parsed *vault.Note, llm LLMClassifier) (noteType string, entities []string) {
	if llm != nil {
		content := buildLLMContent(parsed)
		result, err := llm.Classify(ctx, content)
		if err == nil &&
			result.Confidence >= llmConfidenceThreshold &&
			isValidNoteType(result.Type) &&
			result.Type != NoteTypeFleeting {
			return string(result.Type), result.Entities
		}
	}
	return classifyNoteType(parsed), nil
}

// buildLLMContent assembles the text sent to the LLM for classification.
// Includes salient frontmatter fields followed by the note body.
func buildLLMContent(parsed *vault.Note) string {
	var sb strings.Builder
	if title := frontmatterString(parsed.Frontmatter, "title"); title != "" {
		fmt.Fprintf(&sb, "title: %s\n", title)
	}
	if source := frontmatterString(parsed.Frontmatter, "source"); source != "" {
		fmt.Fprintf(&sb, "source: %s\n", source)
	}
	if sb.Len() > 0 {
		sb.WriteByte('\n')
	}
	sb.WriteString(parsed.Body)
	return sb.String()
}

// matchEntitiesAgainstVault finds vault notes whose slugified filenames match
// any of the given entity names. Returns note display names (without .md extension).
func matchEntitiesAgainstVault(vaultPath string, entities []string) []string {
	if len(entities) == 0 {
		return nil
	}

	notes, err := vault.ListNotes(vaultPath, "")
	if err != nil {
		return nil
	}

	// Build a set of slugified entity names for O(1) lookup.
	entitySlugs := make(map[string]bool, len(entities))
	for _, e := range entities {
		entitySlugs[slugify(e)] = true
	}

	var matches []string
	seen := make(map[string]bool)
	for _, n := range notes {
		name := strings.TrimSuffix(filepath.Base(n.Path), ".md")
		if entitySlugs[slugify(name)] && !seen[name] {
			matches = append(matches, name)
			seen[name] = true
		}
	}
	return matches
}

// appendToCanonical appends the new body to an existing canonical note, separated
// by a dated divider so the provenance of each append is clear.
func appendToCanonical(destFull, newBody string, now time.Time) error {
	existing, err := os.ReadFile(destFull)
	if err != nil {
		return err
	}

	var entry strings.Builder
	if len(existing) > 0 && existing[len(existing)-1] != '\n' {
		entry.WriteByte('\n')
	}
	fmt.Fprintf(&entry, "\n---\n*Appended %s*\n\n%s\n",
		now.Format("2006-01-02"),
		strings.TrimSpace(newBody),
	)

	return os.WriteFile(destFull, append(existing, []byte(entry.String())...), 0644)
}

// containsStr reports whether s is in the slice.
func containsStr(slice []string, s string) bool {
	for _, v := range slice {
		if v == s {
			return true
		}
	}
	return false
}

// classifyNoteType determines the best note type for a parsed note using regex rules.
// Priority order:
//  1. Existing non-fleeting type in frontmatter → preserve it.
//  2. Task signals (checkbox, TODO) → "task".
//  3. Source URL or inline URL → "reference".
//  4. Multiple headings (structured content) → "note".
//  5. Default → "idea".
func classifyNoteType(parsed *vault.Note) string {
	existing := frontmatterString(parsed.Frontmatter, "type")
	if existing != "" && existing != "fleeting" {
		return existing
	}

	body := strings.ToLower(parsed.Body)

	// Task signals: checkbox items or explicit TODO/action markers.
	taskRe := regexp.MustCompile(`(?m)^[-*]\s+\[[ xX]\]|^\s*todo:|\bfollowup\b|^action:`)
	if taskRe.MatchString(body) {
		return "task"
	}

	// Reference signals: URL in source frontmatter or body.
	source := frontmatterString(parsed.Frontmatter, "source")
	if strings.HasPrefix(source, "http://") || strings.HasPrefix(source, "https://") {
		return "reference"
	}
	urlRe := regexp.MustCompile(`https?://\S+`)
	if urlRe.MatchString(body) {
		return "reference"
	}

	// Note signals: structured content with multiple headings.
	if len(parsed.Headings) >= 2 {
		return "note"
	}

	return "idea"
}

// typeFolder maps a note type to its target vault folder.
func typeFolder(noteType string) string {
	switch noteType {
	case "task":
		return "Tasks"
	case "reference":
		return "References"
	case "idea":
		return "Ideas"
	case "note":
		return "Notes"
	default:
		return "Notes"
	}
}

// triageDestination computes the vault-relative target path for a note.
// Format: {Folder}/{slug}.md
func triageDestination(fromPath, noteType string, parsed *vault.Note) string {
	folder := typeFolder(noteType)
	slug := noteSlug(fromPath, parsed)
	return filepath.Join(folder, slug+".md")
}

// noteSlug derives a filename slug from the note's frontmatter title,
// first H1 heading, or — as a fallback — the original filename.
func noteSlug(fromPath string, parsed *vault.Note) string {
	if title := frontmatterString(parsed.Frontmatter, "title"); title != "" {
		return slugify(title)
	}
	for _, h := range parsed.Headings {
		if h.Level == 1 {
			return slugify(h.Text)
		}
	}
	// Fall back to the timestamp filename (e.g. "20260221-143022").
	return strings.TrimSuffix(filepath.Base(fromPath), ".md")
}

var (
	slugNonAlnum = regexp.MustCompile(`[^a-z0-9]+`)
)

// slugify converts a title to a lowercase, hyphen-separated slug.
func slugify(s string) string {
	s = strings.ToLower(s)
	s = slugNonAlnum.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")
	if s == "" {
		return "untitled"
	}
	return s
}

// deconflictPath appends a short timestamp suffix to avoid path collisions.
func deconflictPath(path string) string {
	ext := filepath.Ext(path)
	base := strings.TrimSuffix(path, ext)
	return fmt.Sprintf("%s-%d%s", base, time.Now().UnixMilli()%100000, ext)
}

// buildTriagedContent rewrites a note's frontmatter with the classified type,
// sets status: processed and triaged date, preserves all other existing fields,
// and appends a ## Related Notes section for any wikilink suggestions.
func buildTriagedContent(parsed *vault.Note, noteType string, linksAdded []string, now time.Time) string {
	// Merge: copy all existing frontmatter, then overlay triage-specific fields.
	merged := make(map[string]any, len(parsed.Frontmatter)+3)
	for k, v := range parsed.Frontmatter {
		merged[k] = v
	}
	merged["type"] = noteType
	merged["status"] = "processed"
	merged["triaged"] = now.Format("2006-01-02")

	var b strings.Builder
	b.WriteString("---\n")

	// Emit well-known keys in deterministic order first.
	knownKeys := []string{"title", "created", "type", "status", "triaged", "source", "tags"}
	written := make(map[string]bool, len(merged))
	for _, key := range knownKeys {
		v, ok := merged[key]
		if !ok {
			continue
		}
		writeFrontmatterField(&b, key, v)
		written[key] = true
	}

	// Emit remaining keys in sorted order so output is deterministic.
	extras := make([]string, 0, len(merged))
	for k := range merged {
		if !written[k] {
			extras = append(extras, k)
		}
	}
	sort.Strings(extras)
	for _, key := range extras {
		writeFrontmatterField(&b, key, merged[key])
	}

	b.WriteString("---\n")

	// Append body (ensure leading newline between frontmatter and body).
	body := parsed.Body
	if body != "" && !strings.HasPrefix(body, "\n") {
		b.WriteByte('\n')
	}
	b.WriteString(body)

	// Append suggested wikilinks as a Related Notes section.
	if len(linksAdded) > 0 && !strings.Contains(body, "## Related Notes") {
		if !strings.HasSuffix(b.String(), "\n") {
			b.WriteByte('\n')
		}
		b.WriteString("\n## Related Notes\n")
		for _, link := range linksAdded {
			fmt.Fprintf(&b, "- [[%s]]\n", link)
		}
	}

	return b.String()
}

// writeFrontmatterField emits one YAML frontmatter line (or block for lists) into b.
// Empty strings and empty lists are skipped to avoid noise.
func writeFrontmatterField(b *strings.Builder, key string, value any) {
	switch v := value.(type) {
	case []string:
		if len(v) > 0 {
			fmt.Fprintf(b, "%s:\n", key)
			for _, item := range v {
				fmt.Fprintf(b, "  - %s\n", item)
			}
		}
	case string:
		if v != "" {
			if strings.ContainsAny(v, ":{}[]#&*!|>'\"%@`") {
				fmt.Fprintf(b, "%s: \"%s\"\n", key, strings.ReplaceAll(v, "\"", "\\\""))
			} else {
				fmt.Fprintf(b, "%s: %s\n", key, v)
			}
		}
	default:
		fmt.Fprintf(b, "%s: %v\n", key, v)
	}
}

// enrichSingleNote finds wikilink suggestions for a single note from the index.
// Returns up to 5 suggestions above a 0.7 cosine similarity threshold, sorted
// by similarity descending (highest-similarity notes first).
func enrichSingleNote(store *index.Store, notePath string) []LinkSuggestion {
	notes, err := store.GetAllNoteRows()
	if err != nil {
		return nil
	}
	return enrichSingleNoteFromRows(notes, notePath)
}

// enrichSingleNoteFromRows is the testable core of enrichSingleNote.
func enrichSingleNoteFromRows(notes []index.NoteRow, notePath string) []LinkSuggestion {
	const threshold = 0.7
	const maxLinks = 5

	// Locate the target note's embedding and its existing wikilinks.
	var targetEmb []float32
	existingLinks := make(map[string]bool)
	for _, n := range notes {
		if n.Path != notePath {
			continue
		}
		targetEmb = n.Embedding
		if n.Wikilinks != "" {
			for _, link := range strings.Split(n.Wikilinks, ", ") {
				existingLinks[strings.ToLower(strings.TrimSpace(link))] = true
			}
		}
		break
	}
	if targetEmb == nil {
		return nil // note not yet indexed
	}

	targetName := strings.ToLower(strings.TrimSuffix(filepath.Base(notePath), ".md"))

	// Collect all candidates above the threshold first, then sort.
	var suggestions []LinkSuggestion
	for _, n := range notes {
		if n.Path == notePath || n.Embedding == nil {
			continue
		}

		sim := float64(index.CosineSimilarity(targetEmb, n.Embedding))
		if sim < threshold {
			continue
		}

		toName := strings.ToLower(strings.TrimSuffix(filepath.Base(n.Path), ".md"))
		if existingLinks[toName] || toName == targetName {
			continue
		}

		suggestions = append(suggestions, LinkSuggestion{
			From:       notePath,
			To:         n.Path,
			Similarity: sim,
		})
	}

	// Sort by similarity descending so the highest-scoring notes come first.
	for i := 1; i < len(suggestions); i++ {
		for j := i; j > 0 && suggestions[j].Similarity > suggestions[j-1].Similarity; j-- {
			suggestions[j], suggestions[j-1] = suggestions[j-1], suggestions[j]
		}
	}

	// Truncate to top maxLinks.
	if len(suggestions) > maxLinks {
		suggestions = suggestions[:maxLinks]
	}

	return suggestions
}

// computeAge returns (days old, created-label) from a frontmatter date string or file mod time.
func computeAge(created string, modTime int64, now time.Time) (int, string) {
	if created != "" {
		t, err := time.Parse("2006-01-02", created)
		if err == nil {
			days := int(now.Sub(t).Hours() / 24)
			if days < 0 {
				days = 0
			}
			return days, created
		}
	}
	t := time.Unix(modTime, 0)
	days := int(now.Sub(t).Hours() / 24)
	if days < 0 {
		days = 0
	}
	return days, t.Format("2006-01-02")
}

// formatAgeLabel converts a day count to a human-readable string.
func formatAgeLabel(days int) string {
	switch {
	case days == 0:
		return "today"
	case days == 1:
		return "1 day ago"
	case days < 7:
		return fmt.Sprintf("%d days ago", days)
	case days < 14:
		return "1 week ago"
	case days < 30:
		return fmt.Sprintf("%d weeks ago", days/7)
	case days < 60:
		return "1 month ago"
	default:
		return fmt.Sprintf("%d months ago", days/30)
	}
}

// frontmatterString extracts a string value from a parsed frontmatter map.
func frontmatterString(fm map[string]any, key string) string {
	v, ok := fm[key]
	if !ok {
		return ""
	}
	s, _ := v.(string)
	return s
}

func printTriageListReport(result TriageOutput) {
	header := fmt.Sprintf("Inbox Triage (pending: %d)", result.Summary.Total)
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))

	if len(result.Pending) == 0 {
		fmt.Println("\nNo pending notes in inbox.")
		return
	}

	fmt.Println()
	for _, p := range result.Pending {
		noteType := p.Type
		if noteType == "" {
			noteType = "untyped"
		}
		fmt.Printf("  %-40s  %-12s  %s\n",
			filepath.Base(p.Path),
			noteType,
			p.AgeLabel,
		)
	}

	if len(result.Errors) > 0 {
		fmt.Printf("\nErrors (%d):\n", len(result.Errors))
		for _, e := range result.Errors {
			fmt.Printf("  ! %s\n", e)
		}
	}
}

func printTriageAutoReport(result TriageOutput, dryRun bool) {
	header := "Triage"
	if dryRun {
		header += " (dry run)"
	}
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))

	if len(result.Processed) > 0 {
		fmt.Println()
		for _, p := range result.Processed {
			prefix := "✓"
			if p.DryRun {
				prefix = "→"
			} else if p.Appended {
				prefix = "+"
			}
			line := fmt.Sprintf("  %s %s → %s (type: %s",
				prefix,
				filepath.Base(p.FromPath),
				p.ToPath,
				p.NoteType,
			)
			if len(p.LinksAdded) > 0 {
				line += ", links: " + strings.Join(p.LinksAdded, ", ")
			}
			if p.Appended {
				line += ", appended to canonical"
			}
			line += ")"
			fmt.Println(line)
		}
	}

	if len(result.Errors) > 0 {
		fmt.Printf("\nErrors (%d):\n", len(result.Errors))
		for _, e := range result.Errors {
			fmt.Printf("  ! %s\n", e)
		}
	}

	fmt.Printf("\nSummary: %d processed, %d errors (of %d total)\n",
		result.Summary.Processed, result.Summary.Errors, result.Summary.Total)
}

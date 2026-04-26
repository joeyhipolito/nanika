package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

const (
	canonicalizeDuplicateThreshold = 0.95
	canonicalizeFolder             = "Notes"
)

// CanonicalizeOptions holds flags for the canonicalize command.
type CanonicalizeOptions struct {
	// Mode is "batch" (process all stored clusters) or "per-capture" (clusters for one note).
	Mode        string
	CapturePath string // vault-relative path; required for per-capture mode
	DryRun      bool
	JSONOutput  bool
}

// CanonicalNote describes a canonical note that was (or would be) created.
type CanonicalNote struct {
	ClusterID     string   `json:"cluster_id"`
	CanonicalPath string   `json:"canonical_path,omitempty"`
	MemberPaths   []string `json:"member_paths"`
}

// SkippedCluster describes a cluster that was not canonicalized.
type SkippedCluster struct {
	ClusterID   string `json:"cluster_id"`
	Reason      string `json:"reason"`
	DuplicateOf string `json:"duplicate_of,omitempty"`
}

// CanonicalizeSummary holds aggregate counts for the result.
type CanonicalizeSummary struct {
	ClustersProcessed int  `json:"clusters_processed"`
	Created           int  `json:"created"`
	Skipped           int  `json:"skipped"`
	DryRun            bool `json:"dry_run,omitempty"`
}

// CanonicalizeResult is the full JSON output for the canonicalize command.
type CanonicalizeResult struct {
	Created []CanonicalNote    `json:"created"`
	Skipped []SkippedCluster   `json:"skipped"`
	Summary CanonicalizeSummary `json:"summary"`
}

// ClaudeSummarizer generates synthesized summaries using Claude Haiku.
type ClaudeSummarizer struct {
	apiKey     string
	httpClient *http.Client
}

// NewClaudeSummarizer returns a new ClaudeSummarizer, or nil if apiKey is empty.
func NewClaudeSummarizer(apiKey string) *ClaudeSummarizer {
	if apiKey == "" {
		return nil
	}
	return &ClaudeSummarizer{
		apiKey:     apiKey,
		httpClient: &http.Client{Timeout: 60 * time.Second},
	}
}

// Summarize sends cluster member contents to Claude Haiku and returns a synthesized summary.
func (s *ClaudeSummarizer) Summarize(ctx context.Context, title string, contents []string) (string, error) {
	joined := strings.Join(contents, "\n\n---\n\n")
	if len(joined) > 12000 {
		joined = joined[:12000]
	}

	prompt := fmt.Sprintf(`You are synthesizing a set of related notes into a single canonical summary.

Notes to synthesize:
%s

Write a clear, concise synthesis (2-4 paragraphs) that:
1. Captures the core idea and key insights from all notes
2. Identifies patterns and connections between the notes
3. Preserves important details and examples
4. Avoids redundancy

Respond with only the synthesis text, no preamble.`, joined)

	apiResp, err := callAnthropicMessages(ctx, s.httpClient, s.apiKey,
		"claude-haiku-4-5-20251001", 1024,
		[]map[string]string{{"role": "user", "content": prompt}})
	if err != nil {
		return "", err
	}
	if len(apiResp.Content) == 0 || apiResp.Content[0].Type != "text" {
		return "", fmt.Errorf("unexpected response format")
	}
	return strings.TrimSpace(apiResp.Content[0].Text), nil
}

// CanonicalizeCmd creates canonical notes from detected clusters.
// Modes:
//   - "batch" (default): processes all stored clusters from the DB.
//   - "per-capture": detects clusters that include CapturePath and canonicalizes those.
func CanonicalizeCmd(vaultPath string, opts CanonicalizeOptions) error {
	if opts.Mode == "per-capture" && opts.CapturePath == "" {
		return fmt.Errorf("per-capture mode requires --capture <path>")
	}

	dbPath := index.IndexDBPath(vaultPath)
	store, err := index.Open(dbPath)
	if err != nil {
		return fmt.Errorf("opening index: %w", err)
	}
	defer store.Close()

	var clusters []DetectedCluster
	switch opts.Mode {
	case "per-capture":
		clusters, err = clustersForCapture(vaultPath, store, opts.CapturePath)
		if err != nil {
			return err
		}
	default: // "batch" or "weekly"
		clusters, err = loadStoredClusters(store)
		if err != nil {
			return err
		}
	}

	var summarizer *ClaudeSummarizer
	if apiKey := os.Getenv("ANTHROPIC_API_KEY"); apiKey != "" {
		summarizer = NewClaudeSummarizer(apiKey)
	}

	embClient := index.NewEmbeddingClient(config.ResolveAPIKey())

	result, err := canonicalizeClusters(vaultPath, store, clusters, summarizer, embClient, opts)
	if err != nil {
		return err
	}

	if opts.JSONOutput {
		return output.JSON(result)
	}
	printCanonicalizeReport(result, opts.DryRun)
	return nil
}

// loadStoredClusters returns DetectedClusters from the clusters table for batch processing.
func loadStoredClusters(store *index.Store) ([]DetectedCluster, error) {
	rows, err := store.GetAllClusters()
	if err != nil {
		return nil, fmt.Errorf("loading clusters: %w", err)
	}
	clusters := make([]DetectedCluster, 0, len(rows))
	for _, r := range rows {
		var paths []string
		if err := json.Unmarshal([]byte(r.MemberPaths), &paths); err != nil {
			return nil, fmt.Errorf("decoding member paths for cluster %s: %w", r.ClusterID, err)
		}
		clusters = append(clusters, DetectedCluster{
			ClusterID:     r.ClusterID,
			MemberPaths:   paths,
			AvgSimilarity: r.AvgSimilarity,
			Size:          r.Size,
		})
	}
	return clusters, nil
}

// clustersForCapture runs live cluster detection and returns only clusters containing capturePath.
func clustersForCapture(vaultPath string, store *index.Store, capturePath string) ([]DetectedCluster, error) {
	captures, err := loadUntaggedCaptures(vaultPath)
	if err != nil {
		return nil, fmt.Errorf("loading captures: %w", err)
	}
	loadEmbeddingsInto(store, captures)

	var withEmb []*promoteNoteInfo
	for _, c := range captures {
		if c.Embedding != nil {
			withEmb = append(withEmb, c)
		}
	}

	all := detectSemanticClusters(withEmb, clusterSemanticThreshold, clusterMinSize)

	var filtered []DetectedCluster
	for _, c := range all {
		for _, p := range c.MemberPaths {
			if p == capturePath {
				filtered = append(filtered, c)
				break
			}
		}
	}
	return filtered, nil
}

// canonicalizeClusters processes clusters and creates canonical notes.
func canonicalizeClusters(
	vaultPath string,
	store *index.Store,
	clusters []DetectedCluster,
	summarizer *ClaudeSummarizer,
	embClient *index.EmbeddingClient,
	opts CanonicalizeOptions,
) (CanonicalizeResult, error) {
	ctx := context.Background()

	// Load all existing note rows once for duplicate detection.
	var noteRows []index.NoteRow
	if embClient.IsAvailable() {
		var err error
		noteRows, err = store.GetAllNoteRows()
		if err != nil {
			return CanonicalizeResult{}, fmt.Errorf("loading note rows: %w", err)
		}
	}

	result := CanonicalizeResult{}
	now := time.Now()

	for _, c := range clusters {
		// Skip clusters already canonicalized.
		if _, exists, err := store.GetCanonicalByClusterID(c.ClusterID); err != nil {
			return result, fmt.Errorf("checking canonical for cluster %s: %w", c.ClusterID, err)
		} else if exists {
			result.Skipped = append(result.Skipped, SkippedCluster{
				ClusterID: c.ClusterID,
				Reason:    "already canonicalized",
			})
			continue
		}

		members, err := loadMemberNotes(vaultPath, c.MemberPaths)
		if err != nil {
			return result, fmt.Errorf("loading members for cluster %s: %w", c.ClusterID, err)
		}
		if len(members) < clusterMinSize {
			result.Skipped = append(result.Skipped, SkippedCluster{
				ClusterID: c.ClusterID,
				Reason:    fmt.Sprintf("only %d of %d members readable", len(members), len(c.MemberPaths)),
			})
			continue
		}

		title := deriveClusterTitle(members)
		summary := buildFallbackSummary(members)
		if summarizer != nil {
			var contents []string
			for _, m := range members {
				contents = append(contents, m.Body)
			}
			if llmSummary, llmErr := summarizer.Summarize(ctx, title, contents); llmErr == nil {
				summary = llmSummary
			} else {
				fmt.Fprintf(os.Stderr, "warning: LLM summary failed for cluster %s: %v (using fallback)\n", c.ClusterID, llmErr)
			}
		}

		// Duplicate detection: skip if a near-identical note already exists.
		dupPath, dupErr := detectNearDuplicate(ctx, embClient, noteRows, summary)
		if dupErr != nil {
			fmt.Fprintf(os.Stderr, "warning: duplicate check failed for cluster %s: %v\n", c.ClusterID, dupErr)
		} else if dupPath != "" {
			result.Skipped = append(result.Skipped, SkippedCluster{
				ClusterID:   c.ClusterID,
				Reason:      "near-duplicate exists",
				DuplicateOf: dupPath,
			})
			continue
		}

		if opts.DryRun {
			result.Created = append(result.Created, CanonicalNote{
				ClusterID:   c.ClusterID,
				MemberPaths: c.MemberPaths,
			})
			continue
		}

		canonicalPath, err := createCanonicalNote(vaultPath, members, summary, title, c.ClusterID, now)
		if err != nil {
			return result, fmt.Errorf("creating canonical note for cluster %s: %w", c.ClusterID, err)
		}

		// Insert bidirectional wikilinks into source notes.
		canonicalName := strings.TrimSuffix(filepath.Base(canonicalPath), ".md")
		if err := insertBacklinks(vaultPath, c.MemberPaths, canonicalName); err != nil {
			fmt.Fprintf(os.Stderr, "warning: inserting backlinks for %s: %v\n", canonicalPath, err)
		}

		if err := store.UpsertCanonical(index.CanonicalRow{
			ClusterID:     c.ClusterID,
			CanonicalPath: canonicalPath,
			CreatedAt:     now.Unix(),
		}); err != nil {
			return result, fmt.Errorf("recording canonical note for cluster %s: %w", c.ClusterID, err)
		}

		result.Created = append(result.Created, CanonicalNote{
			ClusterID:     c.ClusterID,
			CanonicalPath: canonicalPath,
			MemberPaths:   c.MemberPaths,
		})
	}

	result.Summary = CanonicalizeSummary{
		ClustersProcessed: len(clusters),
		Created:           len(result.Created),
		Skipped:           len(result.Skipped),
		DryRun:            opts.DryRun,
	}
	return result, nil
}

// loadMemberNotes reads note files for the given vault-relative paths.
// Unreadable files are skipped; the caller checks the minimum size.
func loadMemberNotes(vaultPath string, paths []string) ([]*promoteNoteInfo, error) {
	var members []*promoteNoteInfo
	for _, p := range paths {
		data, err := os.ReadFile(filepath.Join(vaultPath, p))
		if err != nil {
			continue
		}
		parsed := vault.ParseNote(string(data))
		title := frontmatterString(parsed.Frontmatter, "title")
		if title == "" {
			title = strings.TrimSuffix(filepath.Base(p), ".md")
		}
		members = append(members, &promoteNoteInfo{
			Path:        p,
			Title:       title,
			Tags:        extractTagsList(parsed.Frontmatter),
			Body:        parsed.Body,
			Frontmatter: parsed.Frontmatter,
		})
	}
	return members, nil
}

// buildFallbackSummary concatenates member bodies for use when LLM is unavailable.
func buildFallbackSummary(members []*promoteNoteInfo) string {
	var parts []string
	for _, m := range members {
		if body := strings.TrimSpace(m.Body); body != "" {
			parts = append(parts, body)
		}
	}
	return strings.Join(parts, "\n\n")
}

// detectNearDuplicate checks whether the summary is semantically near-identical
// to any existing indexed note. Returns the path of the near-duplicate if found,
// or "" if none. Returns ("", nil) when embedding is unavailable.
func detectNearDuplicate(ctx context.Context, embClient *index.EmbeddingClient, noteRows []index.NoteRow, summary string) (string, error) {
	if !embClient.IsAvailable() || len(noteRows) == 0 {
		return "", nil
	}
	summaryEmb, err := embClient.Embed(ctx, summary)
	if err != nil {
		return "", fmt.Errorf("embedding summary: %w", err)
	}
	for _, n := range noteRows {
		if n.Embedding == nil {
			continue
		}
		if float64(index.CosineSimilarity(summaryEmb, n.Embedding)) >= canonicalizeDuplicateThreshold {
			return n.Path, nil
		}
	}
	return "", nil
}

// createCanonicalNote writes the canonical note file and returns its vault-relative path.
func createCanonicalNote(vaultPath string, members []*promoteNoteInfo, summary, title, clusterID string, now time.Time) (string, error) {
	allTags := mergeUniqueTags(members)

	var b strings.Builder
	b.WriteString("---\n")
	fmt.Fprintf(&b, "title: %s\n", title)
	b.WriteString("type: note\n")
	b.WriteString("status: active\n")
	fmt.Fprintf(&b, "created: %s\n", now.Format("2006-01-02"))
	fmt.Fprintf(&b, "cluster_id: %s\n", clusterID)
	b.WriteString("canonicalized-from:\n")
	for _, m := range members {
		fmt.Fprintf(&b, "  - '[[%s]]'\n", strings.TrimSuffix(filepath.Base(m.Path), ".md"))
	}
	if len(allTags) > 0 {
		b.WriteString("tags:\n")
		for _, t := range allTags {
			fmt.Fprintf(&b, "  - %s\n", t)
		}
	}
	b.WriteString("---\n\n")
	fmt.Fprintf(&b, "# %s\n\n", title)
	b.WriteString(summary)
	b.WriteString("\n\n## Sources\n\n")
	for _, m := range members {
		fmt.Fprintf(&b, "- [[%s]]\n", strings.TrimSuffix(filepath.Base(m.Path), ".md"))
	}
	b.WriteByte('\n')

	slug := slugify(title)
	canonicalPath := canonicalizeFolder + "/" + slug + ".md"
	fullPath := filepath.Join(vaultPath, canonicalPath)

	// Deconflict if path already exists.
	if _, err := os.Stat(fullPath); err == nil {
		ext := filepath.Ext(canonicalPath)
		base := strings.TrimSuffix(canonicalPath, ext)
		canonicalPath = fmt.Sprintf("%s-%d%s", base, now.UnixMilli()%100000, ext)
		fullPath = filepath.Join(vaultPath, canonicalPath)
	}

	if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
		return "", fmt.Errorf("creating directory: %w", err)
	}
	if err := os.WriteFile(fullPath, []byte(b.String()), 0644); err != nil {
		return "", fmt.Errorf("writing canonical note: %w", err)
	}
	return canonicalPath, nil
}

// insertBacklinks adds a canonicalized-to wikilink to the frontmatter of each source note.
// Errors on individual notes are printed as warnings and do not abort the operation.
func insertBacklinks(vaultPath string, memberPaths []string, canonicalName string) error {
	for _, p := range memberPaths {
		if err := insertBacklink(vaultPath, p, canonicalName); err != nil {
			fmt.Fprintf(os.Stderr, "  warning: could not update %s: %v\n", p, err)
		}
	}
	return nil
}

// insertBacklink rewrites a single source note's frontmatter to add a canonicalized-to link.
// A no-op if the field already exists.
func insertBacklink(vaultPath, notePath, canonicalName string) error {
	fullPath := filepath.Join(vaultPath, notePath)
	data, err := os.ReadFile(fullPath)
	if err != nil {
		return fmt.Errorf("reading note: %w", err)
	}
	parsed := vault.ParseNote(string(data))
	if _, exists := parsed.Frontmatter["canonicalized-to"]; exists {
		return nil
	}
	updated := buildSourceWithBacklink(parsed, canonicalName)
	if err := os.WriteFile(fullPath, []byte(updated), 0644); err != nil {
		return fmt.Errorf("writing note: %w", err)
	}
	return nil
}

// buildSourceWithBacklink rebuilds a note's content with a canonicalized-to field added.
func buildSourceWithBacklink(parsed *vault.Note, canonicalName string) string {
	var b strings.Builder
	b.WriteString("---\n")
	for _, key := range []string{"title", "created", "type", "status", "source"} {
		if v := frontmatterString(parsed.Frontmatter, key); v != "" {
			fmt.Fprintf(&b, "%s: %s\n", key, v)
		}
	}
	if tags, ok := parsed.Frontmatter["tags"]; ok {
		switch v := tags.(type) {
		case []string:
			if len(v) > 0 {
				b.WriteString("tags:\n")
				for _, t := range v {
					fmt.Fprintf(&b, "  - %s\n", t)
				}
			}
		case string:
			if v != "" {
				fmt.Fprintf(&b, "tags: %s\n", v)
			}
		}
	}
	fmt.Fprintf(&b, "canonicalized-to: '[[%s]]'\n", canonicalName)
	b.WriteString("---\n")
	body := parsed.Body
	if body != "" && !strings.HasPrefix(body, "\n") {
		b.WriteByte('\n')
	}
	b.WriteString(body)
	return b.String()
}

// printCanonicalizeReport prints a human-readable report of canonicalization results.
func printCanonicalizeReport(result CanonicalizeResult, dryRun bool) {
	header := "Canonicalize"
	if dryRun {
		header += " (dry run)"
	}
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))
	fmt.Printf("\nProcessed %d cluster(s): %d created, %d skipped.\n",
		result.Summary.ClustersProcessed, result.Summary.Created, result.Summary.Skipped)

	for _, c := range result.Created {
		if dryRun {
			fmt.Printf("\n  + Would create canonical note for cluster %s (%d members)\n",
				c.ClusterID, len(c.MemberPaths))
		} else {
			fmt.Printf("\n  + Created: %s\n", c.CanonicalPath)
			for _, p := range c.MemberPaths {
				fmt.Printf("    ← [[%s]]\n", strings.TrimSuffix(filepath.Base(p), ".md"))
			}
		}
	}

	for _, s := range result.Skipped {
		if s.DuplicateOf != "" {
			fmt.Printf("\n  ~ Skipped cluster %s: %s (near-duplicate of %s)\n",
				s.ClusterID, s.Reason, s.DuplicateOf)
		} else {
			fmt.Printf("\n  ~ Skipped cluster %s: %s\n", s.ClusterID, s.Reason)
		}
	}
}

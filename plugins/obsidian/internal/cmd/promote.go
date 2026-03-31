package cmd

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

const (
	promoteMinClusterSize      = 3
	promoteTagJaccardThreshold = 0.25
	promoteSemanticThreshold   = 0.80
	promoteArchiveFolder       = "Archive"
)

// PromoteOptions holds flags for the promote command.
type PromoteOptions struct {
	DryRun     bool
	JSONOutput bool
}

// promoteNoteInfo holds metadata for a note used in clustering.
type promoteNoteInfo struct {
	Path        string
	Title       string
	Tags        []string
	Embedding   []float32
	Body        string
	Frontmatter map[string]any
}

// ClusterNoteInfo is the JSON-friendly representation of a note in a cluster.
type ClusterNoteInfo struct {
	Path  string   `json:"path"`
	Title string   `json:"title"`
	Tags  []string `json:"tags"`
}

// Cluster holds a set of related notes and their overlap score.
type Cluster struct {
	Notes      []ClusterNoteInfo `json:"notes"`
	Score      float64           `json:"score"`       // average pairwise similarity
	CommonTags []string          `json:"common_tags"` // tags shared by all notes in the cluster
}

// PromotedCluster describes a cluster that was promoted to a canonical note.
type PromotedCluster struct {
	CanonicalPath string   `json:"canonical_path"`
	SourcePaths   []string `json:"source_paths"`
	DryRun        bool     `json:"dry_run,omitempty"`
}

// PromoteOutput is the full JSON output for the promote command.
type PromoteOutput struct {
	Clusters []Cluster        `json:"clusters"`
	Promoted []PromotedCluster `json:"promoted,omitempty"`
	Summary  PromoteSummary   `json:"summary"`
}

// PromoteSummary holds aggregate counts.
type PromoteSummary struct {
	ClustersFound    int `json:"clusters_found"`
	ClustersPromoted int `json:"clusters_promoted"`
}

// PromoteCmd runs cluster detection and optionally promotes clusters into canonical notes.
func PromoteCmd(vaultPath string, opts PromoteOptions) error {
	notes, err := collectNotesForClustering(vaultPath)
	if err != nil {
		return fmt.Errorf("loading notes: %w", err)
	}

	// Optionally load embeddings from index (best-effort).
	dbPath := index.IndexDBPath(vaultPath)
	if _, statErr := os.Stat(dbPath); statErr == nil {
		if store, openErr := index.Open(dbPath); openErr == nil {
			defer store.Close()
			loadEmbeddingsInto(store, notes)
		}
	}

	clusters, clusterNotes := detectClusters(notes, promoteTagJaccardThreshold, promoteSemanticThreshold, promoteMinClusterSize)

	result := PromoteOutput{
		Clusters: clusters,
		Summary:  PromoteSummary{ClustersFound: len(clusters)},
	}

	if opts.JSONOutput {
		return output.JSON(result)
	}

	if opts.DryRun {
		printPromoteDryRun(clusters)
		return nil
	}

	if len(clusters) == 0 {
		fmt.Println("No clusters found (need 3+ related notes by tag overlap or semantic similarity).")
		return nil
	}

	promoted, err := interactivePromote(vaultPath, clusters, clusterNotes, time.Now())
	if err != nil {
		return err
	}

	result.Promoted = promoted
	result.Summary.ClustersPromoted = len(promoted)
	printPromoteReport(promoted)
	return nil
}

// collectNotesForClustering loads all vault notes, excluding already-promoted ones.
func collectNotesForClustering(vaultPath string) ([]*promoteNoteInfo, error) {
	allNotes, err := vault.ListNotes(vaultPath, "")
	if err != nil {
		return nil, err
	}

	var result []*promoteNoteInfo
	for _, info := range allNotes {
		fullPath := filepath.Join(vaultPath, info.Path)
		data, readErr := os.ReadFile(fullPath)
		if readErr != nil {
			continue
		}

		parsed := vault.ParseNote(string(data))

		// Skip notes that were already promoted. Use key-existence check rather than
		// string value, because the [[wikilink]] value parses as a list in the YAML parser.
		if _, hasPT := parsed.Frontmatter["promoted-to"]; hasPT {
			continue
		}

		title := frontmatterString(parsed.Frontmatter, "title")
		if title == "" {
			title = strings.TrimSuffix(filepath.Base(info.Path), ".md")
		}

		result = append(result, &promoteNoteInfo{
			Path:        info.Path,
			Title:       title,
			Tags:        extractTagsList(parsed.Frontmatter),
			Body:        parsed.Body,
			Frontmatter: parsed.Frontmatter,
		})
	}
	return result, nil
}

// extractTagsList returns the tags list from frontmatter as a string slice.
func extractTagsList(fm map[string]any) []string {
	v, ok := fm["tags"]
	if !ok {
		return nil
	}
	switch t := v.(type) {
	case []string:
		return t
	case string:
		if t == "" {
			return nil
		}
		return []string{t}
	}
	return nil
}

// loadEmbeddingsInto populates embeddings on notes from the index store.
func loadEmbeddingsInto(store *index.Store, notes []*promoteNoteInfo) {
	rows, err := store.GetAllNoteRows()
	if err != nil {
		return
	}
	byPath := make(map[string][]float32, len(rows))
	for _, r := range rows {
		if r.Embedding != nil {
			byPath[r.Path] = r.Embedding
		}
	}
	for _, n := range notes {
		if emb, ok := byPath[n.Path]; ok {
			n.Embedding = emb
		}
	}
}

// detectClusters finds groups of 3+ related notes by tag Jaccard and semantic similarity.
// Returns the Cluster slice (for output) and a parallel slice of raw note groups (for promotion).
func detectClusters(notes []*promoteNoteInfo, jaccardThreshold, semanticThreshold float64, minSize int) ([]Cluster, [][]*promoteNoteInfo) {
	n := len(notes)
	adj := make([][]int, n)

	for i := 0; i < n; i++ {
		for j := i + 1; j < n; j++ {
			related := false

			// Tag Jaccard similarity (only when at least one note has tags).
			if len(notes[i].Tags) > 0 || len(notes[j].Tags) > 0 {
				if tagJaccard(notes[i].Tags, notes[j].Tags) >= jaccardThreshold {
					related = true
				}
			}

			// Semantic similarity (only when both notes have embeddings).
			if !related && notes[i].Embedding != nil && notes[j].Embedding != nil {
				sim := float64(index.CosineSimilarity(notes[i].Embedding, notes[j].Embedding))
				if sim >= semanticThreshold {
					related = true
				}
			}

			if related {
				adj[i] = append(adj[i], j)
				adj[j] = append(adj[j], i)
			}
		}
	}

	// BFS to find connected components.
	visited := make([]bool, n)
	var clusters []Cluster
	var clusterNotes [][]*promoteNoteInfo

	for start := 0; start < n; start++ {
		if visited[start] {
			continue
		}
		var component []int
		queue := []int{start}
		visited[start] = true
		for len(queue) > 0 {
			curr := queue[0]
			queue = queue[1:]
			component = append(component, curr)
			for _, neighbor := range adj[curr] {
				if !visited[neighbor] {
					visited[neighbor] = true
					queue = append(queue, neighbor)
				}
			}
		}

		if len(component) < minSize {
			continue
		}

		noteList := make([]*promoteNoteInfo, len(component))
		for i, idx := range component {
			noteList[i] = notes[idx]
		}
		clusters = append(clusters, buildClusterInfo(noteList))
		clusterNotes = append(clusterNotes, noteList)
	}

	return clusters, clusterNotes
}

// buildClusterInfo creates a Cluster descriptor from a list of notes.
func buildClusterInfo(notes []*promoteNoteInfo) Cluster {
	noteInfos := make([]ClusterNoteInfo, len(notes))
	for i, n := range notes {
		noteInfos[i] = ClusterNoteInfo{
			Path:  n.Path,
			Title: n.Title,
			Tags:  n.Tags,
		}
	}
	return Cluster{
		Notes:      noteInfos,
		Score:      computeClusterScore(notes),
		CommonTags: findCommonTags(notes),
	}
}

// computeClusterScore returns the average pairwise similarity across the cluster.
// Uses tag Jaccard alone when embeddings are unavailable, combined average otherwise.
func computeClusterScore(notes []*promoteNoteInfo) float64 {
	if len(notes) < 2 {
		return 0
	}
	var total float64
	count := 0
	for i := 0; i < len(notes); i++ {
		for j := i + 1; j < len(notes); j++ {
			sim := tagJaccard(notes[i].Tags, notes[j].Tags)
			if notes[i].Embedding != nil && notes[j].Embedding != nil {
				cos := float64(index.CosineSimilarity(notes[i].Embedding, notes[j].Embedding))
				sim = (sim + cos) / 2
			}
			total += sim
			count++
		}
	}
	if count == 0 {
		return 0
	}
	return total / float64(count)
}

// findCommonTags returns tags present in every note of the cluster.
func findCommonTags(notes []*promoteNoteInfo) []string {
	if len(notes) == 0 {
		return nil
	}
	common := make(map[string]bool)
	for _, t := range notes[0].Tags {
		common[strings.ToLower(t)] = true
	}
	for _, n := range notes[1:] {
		noteSet := make(map[string]bool)
		for _, t := range n.Tags {
			noteSet[strings.ToLower(t)] = true
		}
		for t := range common {
			if !noteSet[t] {
				delete(common, t)
			}
		}
	}
	var result []string
	for t := range common {
		result = append(result, t)
	}
	sort.Strings(result)
	return result
}

// tagJaccard computes the Jaccard similarity between two tag sets.
// Returns 0 when both sets are empty so empty-tag notes don't cluster on tags alone.
func tagJaccard(a, b []string) float64 {
	if len(a) == 0 && len(b) == 0 {
		return 0
	}
	setA := make(map[string]bool, len(a))
	for _, t := range a {
		setA[strings.ToLower(strings.TrimSpace(t))] = true
	}
	union := make(map[string]bool, len(a)+len(b))
	for t := range setA {
		union[t] = true
	}
	intersection := 0
	for _, t := range b {
		t = strings.ToLower(strings.TrimSpace(t))
		union[t] = true
		if setA[t] {
			intersection++
		}
	}
	if len(union) == 0 {
		return 0
	}
	return float64(intersection) / float64(len(union))
}

// interactivePromote displays clusters and prompts the user to select which to promote.
func interactivePromote(vaultPath string, clusters []Cluster, clusterNotes [][]*promoteNoteInfo, now time.Time) ([]PromotedCluster, error) {
	printClusters(clusters)

	fmt.Printf("\nFound %d cluster(s). Enter cluster numbers to promote (e.g. \"1 2\"), \"all\", or \"none\": ", len(clusters))
	reader := bufio.NewReader(os.Stdin)
	line, err := reader.ReadString('\n')
	if err != nil {
		return nil, fmt.Errorf("reading input: %w", err)
	}
	line = strings.TrimSpace(line)

	var toPromote []int
	switch strings.ToLower(line) {
	case "all":
		for i := range clusters {
			toPromote = append(toPromote, i)
		}
	case "", "none":
		fmt.Println("No clusters promoted.")
		return nil, nil
	default:
		for _, part := range strings.Fields(line) {
			n, parseErr := strconv.Atoi(part)
			if parseErr != nil || n < 1 || n > len(clusters) {
				fmt.Printf("  Skipping invalid cluster number: %s\n", part)
				continue
			}
			toPromote = append(toPromote, n-1)
		}
	}

	var promoted []PromotedCluster
	for _, idx := range toPromote {
		p, promErr := promoteCluster(vaultPath, clusterNotes[idx], now)
		if promErr != nil {
			fmt.Printf("  Error promoting cluster %d: %v\n", idx+1, promErr)
			continue
		}
		promoted = append(promoted, p)
	}
	return promoted, nil
}

// promoteCluster merges a cluster of notes into a single canonical note and archives the sources.
func promoteCluster(vaultPath string, notes []*promoteNoteInfo, now time.Time) (PromotedCluster, error) {
	canonicalPath, content := buildCanonicalNote(notes, now)

	// Deconflict if the canonical path already exists.
	fullCanonical := filepath.Join(vaultPath, canonicalPath)
	if _, err := os.Stat(fullCanonical); err == nil {
		ext := filepath.Ext(canonicalPath)
		base := strings.TrimSuffix(canonicalPath, ext)
		canonicalPath = fmt.Sprintf("%s-%d%s", base, now.UnixMilli()%100000, ext)
		fullCanonical = filepath.Join(vaultPath, canonicalPath)
	}

	if err := os.MkdirAll(filepath.Dir(fullCanonical), 0755); err != nil {
		return PromotedCluster{}, fmt.Errorf("creating canonical dir: %w", err)
	}
	if err := os.WriteFile(fullCanonical, []byte(content), 0644); err != nil {
		return PromotedCluster{}, fmt.Errorf("writing canonical note: %w", err)
	}

	canonicalName := strings.TrimSuffix(filepath.Base(canonicalPath), ".md")

	var sourcePaths []string
	for _, n := range notes {
		archivePath, err := archiveSourceNote(vaultPath, n, canonicalName, now)
		if err != nil {
			fmt.Printf("  Warning: could not archive %s: %v\n", n.Path, err)
			continue
		}
		sourcePaths = append(sourcePaths, archivePath)
	}

	return PromotedCluster{
		CanonicalPath: canonicalPath,
		SourcePaths:   sourcePaths,
	}, nil
}

// buildCanonicalNote creates the merged note content and returns (vault-relative path, content).
func buildCanonicalNote(notes []*promoteNoteInfo, now time.Time) (string, string) {
	title := deriveClusterTitle(notes)
	allTags := mergeUniqueTags(notes)

	var b strings.Builder
	b.WriteString("---\n")
	fmt.Fprintf(&b, "title: %s\n", title)
	b.WriteString("type: note\n")
	b.WriteString("status: active\n")
	fmt.Fprintf(&b, "created: %s\n", now.Format("2006-01-02"))
	b.WriteString("promoted-from:\n")
	for _, n := range notes {
		name := strings.TrimSuffix(filepath.Base(n.Path), ".md")
		fmt.Fprintf(&b, "  - '[[%s]]'\n", name)
	}
	if len(allTags) > 0 {
		b.WriteString("tags:\n")
		for _, t := range allTags {
			fmt.Fprintf(&b, "  - %s\n", t)
		}
	}
	b.WriteString("---\n\n")
	fmt.Fprintf(&b, "# %s\n\n", title)

	for i, n := range notes {
		fmt.Fprintf(&b, "## Source %d: %s\n\n", i+1, n.Title)
		body := strings.TrimSpace(n.Body)
		if body != "" {
			b.WriteString(body)
			b.WriteString("\n")
		}
		b.WriteString("\n---\n\n")
	}

	slug := slugify(title)
	return "Notes/" + slug + ".md", b.String()
}

// deriveClusterTitle picks a title for the canonical note.
// Uses the most-common tag, falling back to the first note's title.
func deriveClusterTitle(notes []*promoteNoteInfo) string {
	tagCount := make(map[string]int)
	for _, n := range notes {
		for _, t := range n.Tags {
			tagCount[strings.ToLower(t)]++
		}
	}
	bestTag, bestCount := "", 0
	for t, c := range tagCount {
		if c > bestCount || (c == bestCount && t < bestTag) {
			bestTag, bestCount = t, c
		}
	}
	if bestTag != "" {
		return strings.ToUpper(bestTag[:1]) + bestTag[1:]
	}
	if len(notes) > 0 && notes[0].Title != "" {
		return notes[0].Title
	}
	return "Promoted Cluster"
}

// mergeUniqueTags collects all unique tags from a set of notes, sorted.
func mergeUniqueTags(notes []*promoteNoteInfo) []string {
	seen := make(map[string]bool)
	var tags []string
	for _, n := range notes {
		for _, t := range n.Tags {
			tl := strings.ToLower(strings.TrimSpace(t))
			if tl != "" && !seen[tl] {
				seen[tl] = true
				tags = append(tags, tl)
			}
		}
	}
	sort.Strings(tags)
	return tags
}

// archiveSourceNote rewrites a source note with a promoted-to link and moves it to Archive/.
func archiveSourceNote(vaultPath string, n *promoteNoteInfo, canonicalName string, now time.Time) (string, error) {
	updatedContent := buildPromotedSourceContent(n, canonicalName, now)

	archivePath := filepath.Join(promoteArchiveFolder, n.Path)
	fullArchive := filepath.Join(vaultPath, archivePath)

	if err := os.MkdirAll(filepath.Dir(fullArchive), 0755); err != nil {
		return "", fmt.Errorf("creating archive dir: %w", err)
	}

	// Deconflict if archive path already exists.
	if _, err := os.Stat(fullArchive); err == nil {
		ext := filepath.Ext(archivePath)
		base := strings.TrimSuffix(archivePath, ext)
		archivePath = fmt.Sprintf("%s-%d%s", base, now.UnixMilli()%100000, ext)
		fullArchive = filepath.Join(vaultPath, archivePath)
	}

	if err := os.WriteFile(fullArchive, []byte(updatedContent), 0644); err != nil {
		return "", fmt.Errorf("writing archive note: %w", err)
	}
	if err := os.Remove(filepath.Join(vaultPath, n.Path)); err != nil {
		return "", fmt.Errorf("removing original: %w", err)
	}
	return archivePath, nil
}

// buildPromotedSourceContent rewrites note content with a promoted-to frontmatter link.
func buildPromotedSourceContent(n *promoteNoteInfo, canonicalName string, now time.Time) string {
	var b strings.Builder
	b.WriteString("---\n")
	if title := frontmatterString(n.Frontmatter, "title"); title != "" {
		fmt.Fprintf(&b, "title: %s\n", title)
	}
	if created := frontmatterString(n.Frontmatter, "created"); created != "" {
		fmt.Fprintf(&b, "created: %s\n", created)
	}
	if noteType := frontmatterString(n.Frontmatter, "type"); noteType != "" {
		fmt.Fprintf(&b, "type: %s\n", noteType)
	}
	if status := frontmatterString(n.Frontmatter, "status"); status != "" {
		fmt.Fprintf(&b, "status: %s\n", status)
	}
	fmt.Fprintf(&b, "promoted-to: '[[%s]]'\n", canonicalName)
	fmt.Fprintf(&b, "archived: %s\n", now.Format("2006-01-02"))
	if source := frontmatterString(n.Frontmatter, "source"); source != "" {
		fmt.Fprintf(&b, "source: %s\n", source)
	}
	if tags, ok := n.Frontmatter["tags"]; ok {
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
	b.WriteString("---\n")
	body := n.Body
	if body != "" && !strings.HasPrefix(body, "\n") {
		b.WriteByte('\n')
	}
	b.WriteString(body)
	return b.String()
}

// printPromoteDryRun displays discovered clusters without modifying anything.
func printPromoteDryRun(clusters []Cluster) {
	header := "Promote (dry run)"
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))
	if len(clusters) == 0 {
		fmt.Println("\nNo clusters found (need 3+ related notes by tag overlap or semantic similarity).")
		return
	}
	fmt.Printf("\nFound %d cluster(s):\n", len(clusters))
	printClusters(clusters)
}

// printClusters displays all discovered clusters.
func printClusters(clusters []Cluster) {
	for i, c := range clusters {
		fmt.Printf("\nCluster %d  (score: %.2f", i+1, c.Score)
		if len(c.CommonTags) > 0 {
			fmt.Printf(", shared tags: %s", strings.Join(c.CommonTags, ", "))
		}
		fmt.Println(")")
		for _, n := range c.Notes {
			tagStr := ""
			if len(n.Tags) > 0 {
				tagStr = "  [" + strings.Join(n.Tags, ", ") + "]"
			}
			fmt.Printf("    - %s%s\n", n.Title, tagStr)
			fmt.Printf("      %s\n", n.Path)
		}
	}
}

// printPromoteReport displays the results of the promotion.
func printPromoteReport(promoted []PromotedCluster) {
	if len(promoted) == 0 {
		fmt.Println("No clusters promoted.")
		return
	}
	header := "Promote"
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))
	fmt.Println()
	for _, p := range promoted {
		fmt.Printf("  + Created: %s\n", p.CanonicalPath)
		for _, src := range p.SourcePaths {
			fmt.Printf("    → archived: %s\n", src)
		}
	}
	fmt.Printf("\nPromoted %d cluster(s).\n", len(promoted))
}

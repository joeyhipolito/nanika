package cmd

import (
	"encoding/json"
	"fmt"
	"hash/fnv"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

const (
	clusterMinSize           = 3
	clusterSemanticThreshold = 0.70
)

// ClusterOptions holds flags for the cluster command.
type ClusterOptions struct {
	DryRun     bool
	JSONOutput bool
}

// DetectedCluster describes a single detected cluster.
type DetectedCluster struct {
	ClusterID     string   `json:"cluster_id"`
	MemberPaths   []string `json:"member_paths"`
	AvgSimilarity float64  `json:"avg_similarity"`
	Size          int      `json:"size"`
}

// ClusterResult is the full JSON output for the cluster command.
type ClusterResult struct {
	Clusters []DetectedCluster `json:"clusters"`
	Summary  ClusterSummary    `json:"summary"`
}

// ClusterSummary holds aggregate counts.
type ClusterSummary struct {
	CapturesScanned int  `json:"captures_scanned"`
	WithEmbeddings  int  `json:"with_embeddings"`
	ClustersFound   int  `json:"clusters_found"`
	DryRun          bool `json:"dry_run,omitempty"`
}

// ClusterCmd detects semantic clusters among untagged captures and persists them to the index DB.
func ClusterCmd(vaultPath string, opts ClusterOptions) error {
	captures, err := loadUntaggedCaptures(vaultPath)
	if err != nil {
		return fmt.Errorf("loading captures: %w", err)
	}

	dbPath := index.IndexDBPath(vaultPath)
	store, openErr := index.Open(dbPath)
	if openErr != nil {
		return fmt.Errorf("opening index: %w", openErr)
	}
	defer store.Close()

	loadEmbeddingsInto(store, captures)

	var withEmb []*promoteNoteInfo
	for _, c := range captures {
		if c.Embedding != nil {
			withEmb = append(withEmb, c)
		}
	}

	clusters := detectSemanticClusters(withEmb, clusterSemanticThreshold, clusterMinSize)

	result := ClusterResult{
		Clusters: clusters,
		Summary: ClusterSummary{
			CapturesScanned: len(captures),
			WithEmbeddings:  len(withEmb),
			ClustersFound:   len(clusters),
			DryRun:          opts.DryRun,
		},
	}

	if !opts.DryRun {
		if err := persistClusters(store, clusters); err != nil {
			return fmt.Errorf("storing clusters: %w", err)
		}
	}

	if opts.JSONOutput {
		return output.JSON(result)
	}

	printClusterReport(result, opts.DryRun)
	return nil
}

// loadUntaggedCaptures loads fleeting notes without tags from the vault.
// Captures without tags represent "unnamed concepts" — topics the user has written
// about but not yet categorised.
func loadUntaggedCaptures(vaultPath string) ([]*promoteNoteInfo, error) {
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

		isCapture := strings.HasPrefix(info.Path, "Inbox/") ||
			frontmatterString(parsed.Frontmatter, "type") == "fleeting"
		if !isCapture {
			continue
		}

		// Skip already-promoted notes.
		if _, hasPT := parsed.Frontmatter["promoted-to"]; hasPT {
			continue
		}

		// Only include untagged captures — tags indicate a named concept.
		tags := extractTagsList(parsed.Frontmatter)
		if len(tags) > 0 {
			continue
		}

		title := frontmatterString(parsed.Frontmatter, "title")
		if title == "" {
			title = strings.TrimSuffix(filepath.Base(info.Path), ".md")
		}

		result = append(result, &promoteNoteInfo{
			Path:        info.Path,
			Title:       title,
			Tags:        nil,
			Body:        parsed.Body,
			Frontmatter: parsed.Frontmatter,
		})
	}
	return result, nil
}

// detectSemanticClusters groups notes into connected components using cosine similarity.
func detectSemanticClusters(notes []*promoteNoteInfo, threshold float64, minSize int) []DetectedCluster {
	n := len(notes)
	adj := make([][]int, n)

	// Precompute all pairwise similarities once; reuse for AvgSimilarity below.
	// Key: i*n+j for the pair (i,j) where i<j.
	simCache := make(map[int]float64, n*(n-1)/2)
	for i := 0; i < n; i++ {
		for j := i + 1; j < n; j++ {
			sim := float64(index.CosineSimilarity(notes[i].Embedding, notes[j].Embedding))
			simCache[i*n+j] = sim
			if sim >= threshold {
				adj[i] = append(adj[i], j)
				adj[j] = append(adj[j], i)
			}
		}
	}

	visited := make([]bool, n)
	var clusters []DetectedCluster

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

		members := make([]*promoteNoteInfo, len(component))
		for i, idx := range component {
			members[i] = notes[idx]
		}

		paths := make([]string, len(members))
		for i, m := range members {
			paths[i] = m.Path
		}

		clusters = append(clusters, DetectedCluster{
			ClusterID:     clusterID(paths),
			MemberPaths:   paths,
			AvgSimilarity: avgCachedSim(component, n, simCache),
			Size:          len(members),
		})
	}

	return clusters
}

// avgCachedSim computes the average pairwise similarity for a set of note indices
// using a precomputed cache keyed by i*n+j (i<j).
func avgCachedSim(indices []int, n int, cache map[int]float64) float64 {
	if len(indices) < 2 {
		return 0
	}
	var total float64
	count := 0
	for a := 0; a < len(indices); a++ {
		for b := a + 1; b < len(indices); b++ {
			i, j := indices[a], indices[b]
			if i > j {
				i, j = j, i
			}
			total += cache[i*n+j]
			count++
		}
	}
	return total / float64(count)
}

// clusterID returns a stable hex identifier derived from the sorted set of member paths.
// Re-running cluster detection on the same notes produces the same ID.
func clusterID(paths []string) string {
	sorted := make([]string, len(paths))
	copy(sorted, paths)
	sort.Strings(sorted)
	h := fnv.New64a()
	for _, p := range sorted {
		_, _ = h.Write([]byte(p))
		_, _ = h.Write([]byte{0}) // delimiter to avoid collisions across path pairs
	}
	return fmt.Sprintf("%016x", h.Sum64())
}

// avgPairwiseSimilarity computes the average cosine similarity across all pairs in the cluster.
func avgPairwiseSimilarity(notes []*promoteNoteInfo) float64 {
	if len(notes) < 2 {
		return 0
	}
	var total float64
	count := 0
	for i := 0; i < len(notes); i++ {
		for j := i + 1; j < len(notes); j++ {
			total += float64(index.CosineSimilarity(notes[i].Embedding, notes[j].Embedding))
			count++
		}
	}
	if count == 0 {
		return 0
	}
	return total / float64(count)
}

// persistClusters replaces the stored cluster set with the newly detected one.
func persistClusters(store *index.Store, clusters []DetectedCluster) error {
	if err := store.DeleteAllClusters(); err != nil {
		return fmt.Errorf("clearing old clusters: %w", err)
	}
	now := time.Now().Unix()
	for _, c := range clusters {
		pathsJSON, err := json.Marshal(c.MemberPaths)
		if err != nil {
			return fmt.Errorf("encoding member paths for cluster %s: %w", c.ClusterID, err)
		}
		row := index.ClusterRow{
			ClusterID:     c.ClusterID,
			MemberPaths:   string(pathsJSON),
			AvgSimilarity: c.AvgSimilarity,
			Size:          c.Size,
			DetectedAt:    now,
		}
		if err := store.UpsertCluster(row); err != nil {
			return fmt.Errorf("storing cluster %s: %w", c.ClusterID, err)
		}
	}
	return nil
}

func printClusterReport(result ClusterResult, dryRun bool) {
	header := "Cluster Detection"
	if dryRun {
		header += " (dry run)"
	}
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))
	fmt.Printf("\nScanned %d untagged captures (%d with embeddings)",
		result.Summary.CapturesScanned, result.Summary.WithEmbeddings)

	if result.Summary.ClustersFound == 0 {
		fmt.Printf("; no clusters found (need %d+ captures with similar embeddings).\n", clusterMinSize)
		fmt.Println("\nTip: run 'obsidian index' to build embeddings before clustering.")
		return
	}

	fmt.Printf("; found %d cluster(s):\n", result.Summary.ClustersFound)
	for i, c := range result.Clusters {
		fmt.Printf("\nCluster %d  (id: %s, avg_similarity: %.3f, size: %d)\n",
			i+1, c.ClusterID, c.AvgSimilarity, c.Size)
		for _, p := range c.MemberPaths {
			fmt.Printf("    - %s\n", p)
		}
	}

	if !dryRun {
		fmt.Printf("\nClusters stored in index database.\n")
	}
}

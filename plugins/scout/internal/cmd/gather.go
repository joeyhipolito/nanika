package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/daemon"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
	"github.com/joeyhipolito/nanika-scout/internal/health"
)

// gatherResult represents the outcome of gathering from a single source for a topic.
type gatherResult struct {
	Topic  string `json:"topic"`
	Source string `json:"source"`
	Items  int    `json:"items"`
	File   string `json:"file,omitempty"`
	Error  string `json:"error,omitempty"`
}

// GatherCmd gathers intel for one or all topics.
func GatherCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Parse flags
	var topicName string
	var fullGather bool
	var filteredArgs []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`Usage: scout gather [<topic>] [options]

Options:
  --full              Force full re-gather, ignoring incremental state
  --since <duration>  (reserved) filter by time window, e.g. 24h, 7d
  --json              JSON output
  --help, -h          Show this help

Examples:
  scout gather                  Gather all topics (incremental by default)
  scout gather --full           Force full re-gather for all topics
  scout gather "ai-models"      Gather one topic incrementally
`)
			return nil
		case "--full":
			fullGather = true
		case "--since":
			if i+1 >= len(args) {
				return fmt.Errorf("--since requires a value (e.g. 24h, 7d)")
			}
			i++ // skip value — reserved for future use
		default:
			filteredArgs = append(filteredArgs, args[i])
		}
	}

	if len(filteredArgs) > 0 {
		topicName = filteredArgs[0]
	}

	// Determine which topics to gather
	var topics []gather.TopicConfig

	if topicName != "" {
		topic, err := LoadTopicByName(topicName)
		if err != nil {
			return fmt.Errorf("topic %q not found: %w", topicName, err)
		}
		topics = append(topics, *topic)
	} else {
		var err error
		topics, err = loadAllTopics()
		if err != nil {
			return err
		}
		if len(topics) == 0 {
			fmt.Println("No topics configured. Add one first:")
			fmt.Println("  scout topics preset ai-all")
			return nil
		}
	}

	if !jsonOutput {
		fmt.Printf("Gathering intel for %d topic(s)...\n", len(topics))
		if fullGather {
			fmt.Println("  (full re-gather: ignoring incremental state)")
		}
		fmt.Println()
	}

	// Load health store (best-effort: errors here are non-fatal)
	hs, err := health.Load(config.HealthFile())
	if err != nil {
		hs, _ = health.Load("") // empty in-memory store
	}

	// Load gather state for incremental mode (best-effort: missing/corrupt treated as full gather)
	stateFile := config.GatherStateFile()
	state := gather.LoadState(stateFile)
	if fullGather {
		state = &gather.GatherState{Topics: make(map[string]map[string]time.Time)}
	}

	// Two-level parallel gathering:
	// Level 1: Topics in parallel
	// Level 2: Sources within each topic in parallel
	var (
		mu      sync.Mutex
		results []gatherResult
	)

	gatherStarted := time.Now().UTC()

	var wg sync.WaitGroup
	for _, topic := range topics {
		wg.Add(1)
		go func(t gather.TopicConfig) {
			defer wg.Done()
			topicResults := gatherTopic(t, hs, state, jsonOutput)
			mu.Lock()
			results = append(results, topicResults...)
			mu.Unlock()
		}(topic)
	}
	wg.Wait()

	// Update gather state: record successful-source timestamps.
	for _, r := range results {
		if r.Error == "" {
			state.SetCutoff(r.Topic, r.Source, gatherStarted)
		}
	}
	// Persist state (best-effort)
	_ = gather.SaveState(stateFile, state)

	// Persist health data (best-effort)
	_ = hs.Save()

	// Emit scout.intel_gathered events to the daemon (best-effort: no-op if daemon not running).
	emitGatherEvents(results)

	if jsonOutput {
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(results)
	}

	totalItems := 0
	for _, r := range results {
		totalItems += r.Items
	}

	fmt.Println()
	fmt.Printf("Gathering complete: %d items across %d topic(s)\n", totalItems, len(topics))
	return nil
}

// gatherTopic gathers all sources for a single topic in parallel,
// then deduplicates items across sources by URL.
func gatherTopic(topic gather.TopicConfig, hs *health.Store, state *gather.GatherState, jsonOutput bool) []gatherResult {
	type sourceResult struct {
		result gatherResult
		items  []gather.IntelItem
	}

	var (
		mu         sync.Mutex
		srcResults []sourceResult
	)

	var wg sync.WaitGroup
	for _, source := range topic.Sources {
		wg.Add(1)
		go func(src string) {
			defer wg.Done()
			cutoff := state.Cutoff(topic.Name, src)
			start := time.Now()
			r, items := gatherSourceItems(topic, src, cutoff, jsonOutput)
			latency := time.Since(start)
			if r.Error != "" {
				hs.RecordFailure(src, latency)
			} else {
				hs.RecordSuccess(src, latency)
			}
			mu.Lock()
			srcResults = append(srcResults, sourceResult{result: r, items: items})
			mu.Unlock()
		}(source)
	}
	wg.Wait()

	// Cross-source URL dedup: same article from different sources kept once
	seenURLs := make(map[string]gather.IntelItem)
	var results []gatherResult
	for i := range srcResults {
		sr := &srcResults[i]
		if sr.items == nil {
			results = append(results, sr.result)
			continue
		}

		before := len(sr.items)
		sr.items = gather.DedupeByURL(sr.items, seenURLs)
		after := len(sr.items)

		// Save the (possibly reduced) items
		if len(sr.items) > 0 {
			sr.result = saveSourceItems(topic, sr.result.Source, sr.items, jsonOutput)
		}
		if before != after && !jsonOutput {
			fmt.Printf("  [%s/%s] cross-source dedup: %d -> %d items\n", topic.Name, sr.result.Source, before, after)
		}
		results = append(results, sr.result)
	}

	return results
}

// gatherSourceItems gathers, scores, dedupes, and caps items from a single source.
// cutoff filters items to only those newer than the given time (zero = no filter).
// Returns the gatherResult and the processed items (nil on error).
func gatherSourceItems(topic gather.TopicConfig, source string, cutoff time.Time, jsonOutput bool) (gatherResult, []gather.IntelItem) {
	src := strings.ToLower(source)

	if !jsonOutput {
		if !cutoff.IsZero() {
			fmt.Printf("  [%s/%s] Gathering (incremental since %s)...\n", topic.Name, src, cutoff.Format(time.RFC3339))
		} else {
			fmt.Printf("  [%s/%s] Gathering...\n", topic.Name, src)
		}
	}

	var items []gather.IntelItem
	var err error

	factory, ok := gather.Registry[src]
	if !ok {
		return gatherResult{
			Topic:  topic.Name,
			Source: src,
			Error:  fmt.Sprintf("unknown source: %s", src),
		}, nil
	}
	g, err := factory(topic)
	if err != nil {
		return gatherResult{
			Topic:  topic.Name,
			Source: src,
			Error:  err.Error(),
		}, nil
	}
	items, err = g.Gather(context.Background(), topic.SearchTerms)

	if err != nil {
		if !jsonOutput {
			fmt.Printf("  [%s/%s] Error: %v\n", topic.Name, src, err)
		}
		return gatherResult{
			Topic:  topic.Name,
			Source: src,
			Error:  err.Error(),
		}, nil
	}

	// Filter to items newer than the incremental cutoff.
	if !cutoff.IsZero() {
		before := len(items)
		items = gather.FilterAfter(items, cutoff)
		if !jsonOutput && before != len(items) {
			fmt.Printf("  [%s/%s] incremental filter: %d -> %d items\n", topic.Name, src, before, len(items))
		}
	}

	// Score, deduplicate, and cap items
	if len(items) > 0 {
		items = gather.ScoreItems(items, topic.SearchTerms, time.Now().UTC())
		items = gather.DedupeItems(items)
		items = gather.CapItems(items, topic.EffectiveMaxItems())
	}

	if !jsonOutput {
		fmt.Printf("  [%s/%s] %d items (capped at %d)\n", topic.Name, src, len(items), topic.EffectiveMaxItems())
	}

	return gatherResult{
		Topic:  topic.Name,
		Source: src,
		Items:  len(items),
	}, items
}

// saveSourceItems persists gathered items to disk and returns the final gatherResult.
func saveSourceItems(topic gather.TopicConfig, source string, items []gather.IntelItem, jsonOutput bool) gatherResult {
	now := time.Now().UTC()
	intelFile := gather.IntelFile{
		Topic:      topic.Name,
		GatheredAt: now,
		Source:     source,
		Items:      items,
	}

	filePath, err := saveIntel(topic.Name, source, now, &intelFile)
	if err != nil {
		if !jsonOutput {
			fmt.Printf("  [%s/%s] Error saving: %v\n", topic.Name, source, err)
		}
		return gatherResult{
			Topic:  topic.Name,
			Source: source,
			Items:  len(items),
			Error:  fmt.Sprintf("save error: %v", err),
		}
	}

	if !jsonOutput {
		fmt.Printf("  [%s/%s] %d items -> %s\n", topic.Name, source, len(items), filePath)
	}

	return gatherResult{
		Topic:  topic.Name,
		Source: source,
		Items:  len(items),
		File:   filePath,
	}
}

// emitGatherEvents sends a scout.intel_gathered event per topic to the daemon
// (if running). Items are aggregated across all sources for the topic.
// This is best-effort: errors are logged to stderr, never returned.
func emitGatherEvents(results []gatherResult) {
	// Aggregate successful item counts per topic.
	topicCounts := make(map[string]int)
	for _, r := range results {
		if r.Error == "" && r.Items > 0 {
			topicCounts[r.Topic] += r.Items
		}
	}
	if len(topicCounts) == 0 {
		return
	}

	emitter, err := daemon.NewEmitter()
	if err != nil {
		fmt.Fprintf(os.Stderr, "scout: daemon emitter: %v\n", err)
		return
	}
	defer emitter.Close()

	for topic, count := range topicCounts {
		emitter.EmitIntelGathered(topic, count)
	}
}

// saveIntel writes an intel file to ~/.scout/intel/{topic}/{date}.json.
func saveIntel(topicName string, source string, ts time.Time, intelFile *gather.IntelFile) (string, error) {
	topicDir := filepath.Join(config.IntelDir(), topicName)
	if err := os.MkdirAll(topicDir, 0700); err != nil {
		return "", fmt.Errorf("failed to create intel directory: %w", err)
	}

	// Filename: {date}_{source}.json to allow multiple sources per day
	fileName := fmt.Sprintf("%s_%s.json", ts.Format("2006-01-02T150405"), source)
	filePath := filepath.Join(topicDir, fileName)

	data, err := json.MarshalIndent(intelFile, "", "  ")
	if err != nil {
		return "", fmt.Errorf("failed to marshal intel: %w", err)
	}

	if err := os.WriteFile(filePath, data, 0600); err != nil {
		return "", fmt.Errorf("failed to write intel file: %w", err)
	}

	return filePath, nil
}

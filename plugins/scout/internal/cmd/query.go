package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
	"github.com/joeyhipolito/nanika-scout/internal/health"
)

// QueryCmd handles the query subcommand tree.
func QueryCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		fmt.Print(`Usage: scout query <subcommand> [--json]

Subcommands:
  status          Topic count, total items, last gather, source health
  items           List topics with item counts and last gathered time
  action gather   Trigger gather-all and return item count

Options:
  --json          JSON output (required for machine use)
  --help, -h      Show this help
`)
		return nil
	}

	switch args[0] {
	case "status":
		return queryStatusCmd(jsonOutput)
	case "items":
		return queryItemsCmd(jsonOutput)
	case "actions":
		return queryActionsCmd(jsonOutput)
	case "action":
		if len(args) < 2 {
			return fmt.Errorf("query action requires a subcommand (e.g. gather)")
		}
		switch args[1] {
		case "gather":
			return queryActionGather(jsonOutput)
		case "intel":
			if len(args) < 3 {
				return fmt.Errorf("query action intel requires a topic name")
			}
			return queryActionIntel(args[2], jsonOutput)
		default:
			return fmt.Errorf("unknown action %q (supported: gather, intel)", args[1])
		}
	default:
		return fmt.Errorf("unknown query subcommand %q\n\nRun 'scout query --help' for usage", args[0])
	}
}

// querySourceSummary summarises health for one gather source.
type querySourceSummary struct {
	Name         string `json:"name"`
	Status       string `json:"status"`
	SuccessCount int    `json:"success_count"`
	FailureCount int    `json:"failure_count"`
}

// queryStatusResponse is the JSON shape for `scout query status --json`.
type queryStatusResponse struct {
	OK         bool                 `json:"ok"`
	TopicCount int                  `json:"topic_count"`
	TotalItems int                  `json:"total_items"`
	LastGather string               `json:"last_gather,omitempty"`
	Sources    []querySourceSummary `json:"sources"`
}

func queryStatusCmd(jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	topics, err := loadAllTopics()
	if err != nil {
		return fmt.Errorf("loading topics: %w", err)
	}

	// Walk all topic intel dirs to count items and find last gather time.
	totalItems := 0
	var lastGather time.Time

	intelDir := config.IntelDir()
	entries, _ := os.ReadDir(intelDir)
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		items, t := countIntelItems(filepath.Join(intelDir, entry.Name()))
		totalItems += items
		if t.After(lastGather) {
			lastGather = t
		}
	}

	// Load health store (best-effort).
	hs, _ := health.Load(config.HealthFile())

	srcNames := make([]string, 0, len(hs.Sources))
	for name := range hs.Sources {
		srcNames = append(srcNames, name)
	}
	sort.Strings(srcNames)

	sources := make([]querySourceSummary, 0, len(srcNames))
	for _, name := range srcNames {
		sh := hs.Sources[name]
		sources = append(sources, querySourceSummary{
			Name:         name,
			Status:       sh.Status(),
			SuccessCount: sh.SuccessCount,
			FailureCount: sh.FailureCount,
		})
	}

	var lastGatherStr string
	if !lastGather.IsZero() {
		lastGatherStr = lastGather.UTC().Format(time.RFC3339)
	}

	resp := queryStatusResponse{
		OK:         true,
		TopicCount: len(topics),
		TotalItems: totalItems,
		LastGather: lastGatherStr,
		Sources:    sources,
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(resp)
	}

	fmt.Printf("Topics:      %d\n", resp.TopicCount)
	fmt.Printf("Total items: %d\n", resp.TotalItems)
	if resp.LastGather != "" {
		fmt.Printf("Last gather: %s\n", resp.LastGather)
	}
	if len(resp.Sources) > 0 {
		fmt.Printf("Sources (%d):\n", len(resp.Sources))
		for _, s := range resp.Sources {
			fmt.Printf("  %-14s  %-8s  ok: %d  fail: %d\n",
				s.Name, s.Status, s.SuccessCount, s.FailureCount)
		}
	}
	return nil
}

// queryTopicItem is one entry in the `scout query items --json` response.
type queryTopicItem struct {
	Name         string `json:"name"`
	Description  string `json:"description,omitempty"`
	SourceCount  int    `json:"source_count"`
	ItemCount    int    `json:"item_count"`
	LastGathered string `json:"last_gathered,omitempty"`
}

type queryItemsOutput struct {
	Items []queryTopicItem `json:"items"`
	Count int              `json:"count"`
}

type queryScoutActionItem struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type queryScoutActionsOutput struct {
	Actions []queryScoutActionItem `json:"actions"`
}

func queryActionsCmd(jsonOutput bool) error {
	actions := []queryScoutActionItem{
		{Name: "gather", Command: "scout gather", Description: "Gather intel from all configured topics"},
		{Name: "gather-topic", Command: "scout gather <topic>", Description: "Gather intel for a specific topic"},
		{Name: "topics", Command: "scout topics", Description: "List configured topics"},
		{Name: "topics-add", Command: "scout topics add <name>", Description: "Add a new topic"},
		{Name: "topics-remove", Command: "scout topics remove <name>", Description: "Remove a topic"},
		{Name: "intel", Command: "scout intel <topic>", Description: "Show gathered intel for a topic"},
	}
	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(queryScoutActionsOutput{Actions: actions})
	}
	for _, a := range actions {
		fmt.Printf("%-16s  %s\n              command: %s\n", a.Name, a.Description, a.Command)
	}
	return nil
}

func queryItemsCmd(jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	topics, err := loadAllTopics()
	if err != nil {
		return fmt.Errorf("loading topics: %w", err)
	}

	intelDir := config.IntelDir()
	result := make([]queryTopicItem, 0, len(topics))

	for _, t := range topics {
		itemCount, lastGather := countIntelItems(filepath.Join(intelDir, t.Name))
		var lastGatheredStr string
		if !lastGather.IsZero() {
			lastGatheredStr = lastGather.UTC().Format(time.RFC3339)
		}
		result = append(result, queryTopicItem{
			Name:         t.Name,
			Description:  t.Description,
			SourceCount:  len(t.Sources),
			ItemCount:    itemCount,
			LastGathered: lastGatheredStr,
		})
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(queryItemsOutput{Items: result, Count: len(result)})
	}

	if len(result) == 0 {
		fmt.Println("No topics configured.")
		return nil
	}
	fmt.Printf("Topics (%d):\n", len(result))
	for _, item := range result {
		last := "never"
		if item.LastGathered != "" {
			last = item.LastGathered
		}
		fmt.Printf("  %-20s  %4d items  %d sources  last: %s\n",
			item.Name, item.ItemCount, item.SourceCount, last)
	}
	return nil
}

// queryActionResponse is the JSON shape for `scout query action gather --json`.
type queryActionResponse struct {
	OK            bool `json:"ok"`
	ItemsGathered int  `json:"items_gathered"`
}

func queryActionGather(jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	topics, err := loadAllTopics()
	if err != nil {
		return fmt.Errorf("loading topics: %w", err)
	}

	if len(topics) == 0 {
		if jsonOutput {
			enc := json.NewEncoder(os.Stdout)
			enc.SetIndent("", "  ")
			return enc.Encode(queryActionResponse{OK: true, ItemsGathered: 0})
		}
		fmt.Println("No topics configured.")
		return nil
	}

	hs, err := health.Load(config.HealthFile())
	if err != nil {
		hs, _ = health.Load("")
	}

	stateFile := config.GatherStateFile()
	state := gather.LoadState(stateFile)

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

	for _, r := range results {
		if r.Error == "" {
			state.SetCutoff(r.Topic, r.Source, gatherStarted)
		}
	}
	_ = gather.SaveState(stateFile, state)
	_ = hs.Save()
	emitGatherEvents(results)

	totalItems := 0
	for _, r := range results {
		totalItems += r.Items
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(queryActionResponse{OK: true, ItemsGathered: totalItems})
	}

	fmt.Printf("Gathered %d items across %d topic(s)\n", totalItems, len(topics))
	return nil
}

// queryActionIntel returns scored IntelItem slice for a specific topic as JSON.
// Called via `scout query action intel <topic> --json`.
func queryActionIntel(topicName string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	topicDir := filepath.Join(config.IntelDir(), topicName)
	entries, err := os.ReadDir(topicDir)
	if err != nil {
		if os.IsNotExist(err) {
			if jsonOutput {
				fmt.Println("[]")
				return nil
			}
			fmt.Printf("No intel found for topic %q\n", topicName)
			return nil
		}
		return fmt.Errorf("reading intel dir: %w", err)
	}

	seen := make(map[string]bool)
	var allItems []gather.IntelItem

	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(topicDir, entry.Name()))
		if err != nil {
			continue
		}
		var f gather.IntelFile
		if err := json.Unmarshal(data, &f); err != nil {
			continue
		}
		for _, item := range f.Items {
			if !seen[item.ID] {
				seen[item.ID] = true
				allItems = append(allItems, item)
			}
		}
	}

	sort.Slice(allItems, func(i, j int) bool {
		if allItems[i].Score != allItems[j].Score {
			return allItems[i].Score > allItems[j].Score
		}
		return allItems[i].Timestamp.After(allItems[j].Timestamp)
	})

	// Cap at 50 items for UI performance.
	if len(allItems) > 50 {
		allItems = allItems[:50]
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(allItems)
	}

	fmt.Printf("Intel: %s (%d items)\n", topicName, len(allItems))
	return nil
}

// countIntelItems counts unique intel items in topicDir and returns the
// latest GatheredAt timestamp. Returns 0, zero-time on error.
func countIntelItems(topicDir string) (itemCount int, lastGather time.Time) {
	entries, err := os.ReadDir(topicDir)
	if err != nil {
		return 0, time.Time{}
	}
	seen := make(map[string]bool)
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(topicDir, entry.Name()))
		if err != nil {
			continue
		}
		var f gather.IntelFile
		if err := json.Unmarshal(data, &f); err != nil {
			continue
		}
		for _, item := range f.Items {
			if !seen[item.ID] {
				seen[item.ID] = true
				itemCount++
			}
		}
		if f.GatheredAt.After(lastGather) {
			lastGather = f.GatheredAt
		}
	}
	return itemCount, lastGather
}

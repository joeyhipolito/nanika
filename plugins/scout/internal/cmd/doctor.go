package cmd

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
)

// DoctorCheck represents a single doctor check result.
type DoctorCheck struct {
	Name    string `json:"name"`
	Status  string `json:"status"` // "ok", "warn", "fail"
	Message string `json:"message"`
}

// DoctorOutput represents the JSON output of the doctor command.
type DoctorOutput struct {
	Checks  []DoctorCheck `json:"checks"`
	Summary string        `json:"summary"`
	AllOK   bool          `json:"all_ok"`
}

// DoctorCmd validates the Scout CLI installation and configuration.
func DoctorCmd(jsonOutput bool) error {
	var checks []DoctorCheck
	allOK := true

	// 1. Check binary location
	binaryPath, err := exec.LookPath("scout")
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Binary",
			Status:  "warn",
			Message: "scout not found in PATH (running from local build?)",
		})
	} else {
		checks = append(checks, DoctorCheck{
			Name:    "Binary",
			Status:  "ok",
			Message: binaryPath,
		})
	}

	// 2. Check config file exists
	configPath := config.Path()
	if !config.Exists() {
		checks = append(checks, DoctorCheck{
			Name:    "Config file",
			Status:  "warn",
			Message: fmt.Sprintf("%s not found. Run 'scout configure' (optional)", configPath),
		})
	} else {
		checks = append(checks, DoctorCheck{
			Name:    "Config file",
			Status:  "ok",
			Message: configPath,
		})

		// 3. Check config permissions
		perms, err := config.Permissions()
		if err != nil {
			checks = append(checks, DoctorCheck{
				Name:    "Config permissions",
				Status:  "fail",
				Message: fmt.Sprintf("Cannot read permissions: %v", err),
			})
			allOK = false
		} else if perms != 0600 {
			checks = append(checks, DoctorCheck{
				Name:    "Config permissions",
				Status:  "warn",
				Message: fmt.Sprintf("%o (should be 600). Fix: chmod 600 %s", perms, configPath),
			})
		} else {
			checks = append(checks, DoctorCheck{
				Name:    "Config permissions",
				Status:  "ok",
				Message: "600 (secure)",
			})
		}

		// 4. Check config format
		_, err = config.Load()
		if err != nil {
			checks = append(checks, DoctorCheck{
				Name:    "Config format",
				Status:  "fail",
				Message: fmt.Sprintf("Failed to parse config: %v", err),
			})
			allOK = false
		} else {
			checks = append(checks, DoctorCheck{
				Name:    "Config format",
				Status:  "ok",
				Message: "Valid",
			})
		}
	}

	// 5. Check RSS feed reachability
	client := &http.Client{Timeout: 10 * time.Second}
	testFeedURL := "https://blog.google/technology/ai/rss/"
	resp, err := client.Head(testFeedURL)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "RSS feeds",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach test feed: %v", err),
		})
	} else {
		resp.Body.Close()
		checks = append(checks, DoctorCheck{
			Name:    "RSS feeds",
			Status:  "ok",
			Message: fmt.Sprintf("Reachable (tested %s)", testFeedURL),
		})
	}

	// 6. Check GitHub API reachability
	req, _ := http.NewRequest("HEAD", "https://api.github.com", nil)
	req.Header.Set("User-Agent", "scout-cli/0.3")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "GitHub API",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach GitHub API: %v", err),
		})
	} else {
		resp.Body.Close()
		msg := fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode)
		if os.Getenv("GITHUB_TOKEN") != "" {
			msg += ", GITHUB_TOKEN set"
		} else {
			msg += ", no GITHUB_TOKEN (rate limited to 10 req/min)"
		}
		checks = append(checks, DoctorCheck{
			Name:    "GitHub API",
			Status:  "ok",
			Message: msg,
		})
	}

	// 7. Check Reddit API reachability (use GET, not HEAD — Reddit may return 405 for HEAD)
	req, _ = http.NewRequest("GET", "https://www.reddit.com/search.json?q=test&limit=1", nil)
	req.Header.Set("User-Agent", "scout-cli/0.3 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Reddit API",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach Reddit API: %v", err),
		})
	} else {
		resp.Body.Close()
		status := "ok"
		msg := fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode)
		if resp.StatusCode == 429 {
			status = "warn"
			msg = "Rate limited (HTTP 429). Wait and retry."
		}
		checks = append(checks, DoctorCheck{
			Name:    "Reddit API",
			Status:  status,
			Message: msg,
		})
	}

	// 8. Check Substack feed reachability
	req, _ = http.NewRequest("GET", "https://oneusefulthing.substack.com/feed", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Substack feeds",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach Substack: %v", err),
		})
	} else {
		resp.Body.Close()
		checks = append(checks, DoctorCheck{
			Name:    "Substack feeds",
			Status:  "ok",
			Message: fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode),
		})
	}

	// 9. Check Medium feed reachability
	req, _ = http.NewRequest("GET", "https://medium.com/feed/tag/artificial-intelligence", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Medium feeds",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach Medium: %v", err),
		})
	} else {
		resp.Body.Close()
		status := "ok"
		msg := fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode)
		if resp.StatusCode == 429 {
			status = "warn"
			msg = "Rate limited (HTTP 429). Medium may throttle requests."
		}
		checks = append(checks, DoctorCheck{
			Name:    "Medium feeds",
			Status:  status,
			Message: msg,
		})
	}

	// 10. Check Hacker News Algolia API
	req, _ = http.NewRequest("HEAD", "https://hn.algolia.com/api/v1/search?query=test&hitsPerPage=1", nil)
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "HN Algolia API",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach HN API: %v", err),
		})
	} else {
		resp.Body.Close()
		checks = append(checks, DoctorCheck{
			Name:    "HN Algolia API",
			Status:  "ok",
			Message: fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode),
		})
	}

	// 11. Check Dev.to API
	req, _ = http.NewRequest("HEAD", "https://dev.to/api/articles?per_page=1", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Dev.to API",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach Dev.to: %v", err),
		})
	} else {
		resp.Body.Close()
		msg := fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode)
		if os.Getenv("DEVTO_API_KEY") != "" {
			msg += ", DEVTO_API_KEY set"
		}
		checks = append(checks, DoctorCheck{
			Name:    "Dev.to API",
			Status:  "ok",
			Message: msg,
		})
	}

	// 12. Check Lobste.rs RSS
	req, _ = http.NewRequest("HEAD", "https://lobste.rs/t/programming.rss", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Lobsters RSS",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach Lobsters: %v", err),
		})
	} else {
		resp.Body.Close()
		checks = append(checks, DoctorCheck{
			Name:    "Lobsters RSS",
			Status:  "ok",
			Message: fmt.Sprintf("Reachable (HTTP %d)", resp.StatusCode),
		})
	}

	// 13. Check Bird CLI for X/Twitter
	if birdPath, err := exec.LookPath("bird"); err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Bird CLI (X)",
			Status:  "warn",
			Message: "bird not found in PATH (X/Twitter gathering unavailable)",
		})
	} else {
		checks = append(checks, DoctorCheck{
			Name:    "Bird CLI (X)",
			Status:  "ok",
			Message: birdPath,
		})
	}

	// 14. Check YouTube RSS reachability (no API key required)
	req, _ = http.NewRequest("HEAD", "https://www.youtube.com/feeds/videos.xml?channel_id=UCH_at8rAFuYuQB6k8WeJiZA", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "YouTube RSS",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach YouTube RSS: %v", err),
		})
	} else {
		resp.Body.Close()
		checks = append(checks, DoctorCheck{
			Name:    "YouTube RSS",
			Status:  "ok",
			Message: fmt.Sprintf("Reachable (HTTP %d, no API key needed)", resp.StatusCode),
		})
	}

	// 15. Check ArXiv API reachability
	req, _ = http.NewRequest("HEAD", "https://export.arxiv.org/api/query?search_query=cat:cs.AI&max_results=1", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "ArXiv API",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach ArXiv API: %v", err),
		})
	} else {
		resp.Body.Close()
		checks = append(checks, DoctorCheck{
			Name:    "ArXiv API",
			Status:  "ok",
			Message: fmt.Sprintf("Reachable (HTTP %d, no API key needed)", resp.StatusCode),
		})
	}

	// 16. Check Bluesky public API reachability
	req, _ = http.NewRequest("HEAD", "https://api.bsky.app/xrpc/app.bsky.feed.searchPosts?q=test&limit=1", nil)
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")
	resp, err = client.Do(req)
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Bluesky API",
			Status:  "warn",
			Message: fmt.Sprintf("Cannot reach Bluesky API: %v", err),
		})
	} else {
		resp.Body.Close()
		status := "ok"
		msg := fmt.Sprintf("Reachable (HTTP %d, no API key needed)", resp.StatusCode)
		if resp.StatusCode == 429 {
			status = "warn"
			msg = "Rate limited (HTTP 429). Wait and retry."
		}
		checks = append(checks, DoctorCheck{
			Name:    "Bluesky API",
			Status:  status,
			Message: msg,
		})
	}

	// 18. Check topics directory
	topicsDir := config.TopicsDir()
	if info, err := os.Stat(topicsDir); err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Topics dir",
			Status:  "warn",
			Message: fmt.Sprintf("%s not found. Run 'scout topics preset ai-all'", topicsDir),
		})
	} else if !info.IsDir() {
		checks = append(checks, DoctorCheck{
			Name:    "Topics dir",
			Status:  "fail",
			Message: fmt.Sprintf("%s exists but is not a directory", topicsDir),
		})
		allOK = false
	} else {
		entries, _ := os.ReadDir(topicsDir)
		topicCount := 0
		for _, e := range entries {
			if !e.IsDir() {
				topicCount++
			}
		}
		checks = append(checks, DoctorCheck{
			Name:    "Topics dir",
			Status:  "ok",
			Message: fmt.Sprintf("%s (%d topic(s))", topicsDir, topicCount),
		})
	}

	// 19. Check intel directory
	intelDir := config.IntelDir()
	if info, err := os.Stat(intelDir); err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Intel dir",
			Status:  "warn",
			Message: fmt.Sprintf("%s not found. Will be created on first gather.", intelDir),
		})
	} else if !info.IsDir() {
		checks = append(checks, DoctorCheck{
			Name:    "Intel dir",
			Status:  "fail",
			Message: fmt.Sprintf("%s exists but is not a directory", intelDir),
		})
		allOK = false
	} else {
		entries, _ := os.ReadDir(intelDir)
		checks = append(checks, DoctorCheck{
			Name:    "Intel dir",
			Status:  "ok",
			Message: fmt.Sprintf("%s (%d topic(s) with intel)", intelDir, len(entries)),
		})
	}

	// Determine summary
	summary := "All checks passed!"
	if !allOK {
		failCount := 0
		for _, c := range checks {
			if c.Status == "fail" {
				failCount++
			}
		}
		summary = fmt.Sprintf("%d check(s) failed. Run 'scout configure' to fix.", failCount)
	}

	// JSON output
	if jsonOutput {
		output := DoctorOutput{
			Checks:  checks,
			Summary: summary,
			AllOK:   allOK,
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	// Human-readable output
	fmt.Println("Scout CLI Doctor")
	fmt.Println("================")
	fmt.Println()

	for _, c := range checks {
		var icon string
		switch c.Status {
		case "ok":
			icon = "OK"
		case "warn":
			icon = "WARN"
		case "fail":
			icon = "FAIL"
		}
		fmt.Printf("  [%4s] %-20s %s\n", icon, c.Name+":", c.Message)
	}

	fmt.Println()
	if allOK {
		fmt.Println(summary)
	} else {
		fmt.Println(summary)
		return fmt.Errorf("doctor checks failed")
	}

	return nil
}

package gather

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

// GitHub search API response structures.
type githubSearchResponse struct {
	TotalCount int          `json:"total_count"`
	Items      []githubRepo `json:"items"`
}

type githubRepo struct {
	FullName    string      `json:"full_name"`
	Description string      `json:"description"`
	HTMLURL     string      `json:"html_url"`
	Language    string      `json:"language"`
	Stars       int         `json:"stargazers_count"`
	CreatedAt   string      `json:"created_at"`
	UpdatedAt   string      `json:"updated_at"`
	Topics      []string    `json:"topics"`
	Owner       githubOwner `json:"owner"`
}

type githubOwner struct {
	Login string `json:"login"`
}

// GitHubGatherer searches GitHub for repositories matching queries.
type GitHubGatherer struct {
	Queries []string
	Client  *http.Client
}

// NewGitHubGatherer creates a new GitHub gatherer with the given queries.
func NewGitHubGatherer(queries []string) *GitHubGatherer {
	return &GitHubGatherer{Queries: queries, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *GitHubGatherer) Name() string { return "github" }

// Gather searches GitHub for repos matching the configured queries or search terms.
func (g *GitHubGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	queries := g.Queries
	if len(queries) == 0 && len(searchTerms) > 0 {
		queries = []string{strings.Join(searchTerms, " ")}
	}
	if len(queries) == 0 {
		return nil, nil
	}
	since := time.Now().AddDate(0, 0, -7).Format("2006-01-02")
	return collectFromList(queries, "github", func(q string) ([]IntelItem, error) {
		return g.searchRepos(ctx, q, since)
	}, searchTerms)
}

func (g *GitHubGatherer) searchRepos(ctx context.Context, query string, since string) ([]IntelItem, error) {
	q := fmt.Sprintf("%s created:>%s", query, since)
	apiURL := fmt.Sprintf("https://api.github.com/search/repositories?q=%s&sort=stars&order=desc&per_page=30",
		url.QueryEscape(q))

	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("Accept", "application/vnd.github.v3+json")

	if token := os.Getenv("GITHUB_TOKEN"); token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch GitHub API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("github: rate limited (403). Try again in a few minutes. For higher limits, set GITHUB_TOKEN env var")
	}

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("github: HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var searchResp githubSearchResponse
	if err := json.Unmarshal(body, &searchResp); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	var items []IntelItem
	for _, repo := range searchResp.Items {
		items = append(items, githubRepoToIntel(repo))
	}

	return items, nil
}

func githubRepoToIntel(repo githubRepo) IntelItem {
	var tags []string
	if repo.Language != "" {
		tags = append(tags, repo.Language)
	}
	tags = append(tags, fmt.Sprintf("%d stars", repo.Stars))
	tags = append(tags, repo.Topics...)

	return IntelItem{
		ID:         generateID(repo.HTMLURL),
		Title:      repo.FullName,
		Content:    repo.Description,
		SourceURL:  repo.HTMLURL,
		Author:     repo.Owner.Login,
		Timestamp:  parseDate(repo.CreatedAt),
		Tags:       tags,
		Engagement: repo.Stars,
	}
}

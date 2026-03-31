package gather

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"
)

// Product Hunt GraphQL API request/response structures.
type phGraphQLRequest struct {
	Query string `json:"query"`
}

type phGraphQLResponse struct {
	Data struct {
		Posts struct {
			Edges []struct {
				Node phPost `json:"node"`
			} `json:"edges"`
		} `json:"posts"`
	} `json:"data"`
	Errors []struct {
		Message string `json:"message"`
	} `json:"errors,omitempty"`
}

type phPost struct {
	ID            string `json:"id"`
	Name          string `json:"name"`
	Tagline       string `json:"tagline"`
	URL           string `json:"url"`
	VotesCount    int    `json:"votesCount"`
	CommentsCount int    `json:"commentsCount"`
	CreatedAt     string `json:"createdAt"`
	Topics        struct {
		Edges []struct {
			Node struct {
				Name string `json:"name"`
			} `json:"node"`
		} `json:"edges"`
	} `json:"topics"`
	Makers []struct {
		Name string `json:"name"`
	} `json:"makers"`
}

// ProductHuntGatherer fetches product launches from Product Hunt.
type ProductHuntGatherer struct {
	Client *http.Client
}

// NewProductHuntGatherer creates a new Product Hunt gatherer.
func NewProductHuntGatherer() *ProductHuntGatherer {
	return &ProductHuntGatherer{Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *ProductHuntGatherer) Name() string { return "producthunt" }

// Gather fetches recent Product Hunt launches matching the search terms.
// Requires PRODUCTHUNT_TOKEN env var. Returns nil (not error) if token is missing.
func (g *ProductHuntGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	token := os.Getenv("PRODUCTHUNT_TOKEN")
	if token == "" {
		fmt.Fprintf(os.Stderr, "    Warning: producthunt: PRODUCTHUNT_TOKEN not set, skipping\n")
		return nil, nil
	}

	posts, err := g.fetchPosts(ctx, token)
	if err != nil {
		return nil, err
	}

	var items []IntelItem
	for _, post := range posts {
		items = append(items, phPostToIntel(post))
	}

	if len(searchTerms) > 0 {
		items = filterByTerms(items, searchTerms)
	}

	return items, nil
}

const phGraphQLEndpoint = "https://api.producthunt.com/v2/api/graphql"

// fetchPosts queries the Product Hunt GraphQL API for posts from the last 7 days.
func (g *ProductHuntGatherer) fetchPosts(ctx context.Context, token string) ([]phPost, error) {
	since := time.Now().AddDate(0, 0, -7).Format(time.RFC3339)
	query := fmt.Sprintf(`{
		posts(first: 50, order: VOTES, postedAfter: "%s") {
			edges {
				node {
					id
					name
					tagline
					url
					votesCount
					commentsCount
					createdAt
					topics {
						edges {
							node {
								name
							}
						}
					}
					makers {
						name
					}
				}
			}
		}
	}`, since)

	reqBody, err := json.Marshal(phGraphQLRequest{Query: query})
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST", phGraphQLEndpoint, bytes.NewReader(reqBody))
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch Product Hunt API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("rate limited (429)")
	}
	if resp.StatusCode == http.StatusUnauthorized {
		return nil, fmt.Errorf("invalid PRODUCTHUNT_TOKEN (401)")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var gqlResp phGraphQLResponse
	if err := json.Unmarshal(body, &gqlResp); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	if len(gqlResp.Errors) > 0 {
		return nil, fmt.Errorf("GraphQL error: %s", gqlResp.Errors[0].Message)
	}

	var posts []phPost
	for _, edge := range gqlResp.Data.Posts.Edges {
		posts = append(posts, edge.Node)
	}

	return posts, nil
}

// phPostToIntel converts a Product Hunt post to an IntelItem.
func phPostToIntel(post phPost) IntelItem {
	content := cleanText(post.Tagline)
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	var tags []string
	for _, edge := range post.Topics.Edges {
		tags = append(tags, strings.ToLower(edge.Node.Name))
	}
	tags = append(tags, "producthunt")

	var maker string
	if len(post.Makers) > 0 {
		maker = post.Makers[0].Name
	}

	return IntelItem{
		ID:         generateID("producthunt-" + post.ID),
		Title:      cleanText(post.Name),
		Content:    content,
		SourceURL:  post.URL,
		Author:     maker,
		Timestamp:  parseDate(post.CreatedAt),
		Tags:       tags,
		Engagement: post.VotesCount + post.CommentsCount,
	}
}

// ParseProductHuntResponse parses raw Product Hunt GraphQL JSON into IntelItems.
// Exported for testing.
func ParseProductHuntResponse(data []byte) ([]IntelItem, error) {
	var gqlResp phGraphQLResponse
	if err := json.Unmarshal(data, &gqlResp); err != nil {
		return nil, fmt.Errorf("failed to parse Product Hunt response: %w", err)
	}

	var items []IntelItem
	for _, edge := range gqlResp.Data.Posts.Edges {
		items = append(items, phPostToIntel(edge.Node))
	}
	return items, nil
}

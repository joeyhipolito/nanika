package api

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"time"
)

const (
	officialBaseURL = "https://api.linkedin.com/rest"
	userinfoURL     = "https://api.linkedin.com/v2/userinfo"
	linkedInVersion = "202602"
)

// OAuthClient handles requests to the Official LinkedIn REST API.
type OAuthClient struct {
	BaseURL    string
	Token      string
	PersonURN  string
	HTTPClient *http.Client
}

// NewOAuthClient creates a new OAuth API client.
func NewOAuthClient(token, personURN string) *OAuthClient {
	return &OAuthClient{
		BaseURL:    officialBaseURL,
		Token:      token,
		PersonURN:  personURN,
		HTTPClient: &http.Client{Timeout: 30 * time.Second},
	}
}

// do executes an HTTP request against the Official LinkedIn REST API.
func (c *OAuthClient) do(method, path string, body interface{}) ([]byte, http.Header, error) {
	var bodyReader io.Reader
	if body != nil {
		data, err := json.Marshal(body)
		if err != nil {
			return nil, nil, fmt.Errorf("marshaling request body: %w", err)
		}
		bodyReader = bytes.NewReader(data)
	}

	reqURL := c.BaseURL + path
	if path == "/v2/userinfo" {
		reqURL = userinfoURL
	}

	req, err := http.NewRequest(method, reqURL, bodyReader)
	if err != nil {
		return nil, nil, fmt.Errorf("creating request: %w", err)
	}

	req.Header.Set("Authorization", "Bearer "+c.Token)
	req.Header.Set("LinkedIn-Version", linkedInVersion)
	req.Header.Set("X-Restli-Protocol-Version", "2.0.0")
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return nil, nil, fmt.Errorf("executing request: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, nil, fmt.Errorf("reading response body: %w", err)
	}

	if resp.StatusCode == http.StatusUnauthorized {
		return nil, nil, fmt.Errorf("OAuth token expired or invalid. Run 'linkedin configure' to re-authorize")
	}
	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, nil, fmt.Errorf("rate limited by LinkedIn. Try again in a few minutes")
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		snippet := string(respBody)
		if len(snippet) > 200 {
			snippet = snippet[:200] + "..."
		}
		return nil, nil, fmt.Errorf("API error (HTTP %d): %s", resp.StatusCode, snippet)
	}

	return respBody, resp.Header, nil
}

// GetUserInfo fetches the authenticated user's profile via OpenID Connect.
func (c *OAuthClient) GetUserInfo() (*UserInfo, error) {
	data, _, err := c.do("GET", "/v2/userinfo", nil)
	if err != nil {
		return nil, err
	}

	var info UserInfo
	if err := json.Unmarshal(data, &info); err != nil {
		return nil, fmt.Errorf("parsing userinfo response: %w", err)
	}
	return &info, nil
}

// CreatePost creates a new LinkedIn post. Returns the post ID from the x-restli-id header.
func (c *OAuthClient) CreatePost(req *CreatePostRequest) (string, error) {
	_, headers, err := c.do("POST", "/posts", req)
	if err != nil {
		return "", err
	}
	postID := headers.Get("X-Restli-Id")
	if postID == "" {
		return "", fmt.Errorf("no post ID returned in response headers")
	}
	return postID, nil
}

// InitializeImageUpload starts the image upload flow, returning an upload URL and image URN.
func (c *OAuthClient) InitializeImageUpload() (*ImageUploadValue, error) {
	req := &InitializeImageUploadRequest{
		InitializeUploadRequest: ImageUploadInit{
			Owner: c.PersonURN,
		},
	}
	data, _, err := c.do("POST", "/images?action=initializeUpload", req)
	if err != nil {
		return nil, err
	}
	var resp InitializeImageUploadResponse
	if err := json.Unmarshal(data, &resp); err != nil {
		return nil, fmt.Errorf("parsing image upload response: %w", err)
	}
	return &resp.Value, nil
}

// UploadImage uploads image bytes to the provided upload URL.
func (c *OAuthClient) UploadImage(uploadURL string, imageData []byte) error {
	req, err := http.NewRequest("PUT", uploadURL, bytes.NewReader(imageData))
	if err != nil {
		return fmt.Errorf("creating upload request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.Token)
	req.Header.Set("Content-Type", "application/octet-stream")

	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return fmt.Errorf("uploading image: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := io.ReadAll(resp.Body)
		snippet := string(body)
		if len(snippet) > 200 {
			snippet = snippet[:200] + "..."
		}
		return fmt.Errorf("image upload failed (HTTP %d): %s", resp.StatusCode, snippet)
	}
	return nil
}

// ListPosts fetches the authenticated user's posts, sorted by last modified.
func (c *OAuthClient) ListPosts(count int) ([]Post, error) {
	path := fmt.Sprintf("/posts?author=%s&q=author&count=%d&sortBy=LAST_MODIFIED",
		url.QueryEscape(c.PersonURN), count)

	data, _, err := c.doWithHeaders("GET", path, nil, map[string]string{
		"X-RestLi-Method": "FINDER",
	})
	if err != nil {
		return nil, err
	}

	var resp PostsResponse
	if err := json.Unmarshal(data, &resp); err != nil {
		return nil, fmt.Errorf("parsing posts response: %w", err)
	}
	return resp.Elements, nil
}

// doWithHeaders is like do but allows extra headers.
func (c *OAuthClient) doWithHeaders(method, path string, body interface{}, extraHeaders map[string]string) ([]byte, http.Header, error) {
	var bodyReader io.Reader
	if body != nil {
		data, err := json.Marshal(body)
		if err != nil {
			return nil, nil, fmt.Errorf("marshaling request body: %w", err)
		}
		bodyReader = bytes.NewReader(data)
	}

	reqURL := c.BaseURL + path
	if path == "/v2/userinfo" {
		reqURL = userinfoURL
	}

	req, err := http.NewRequest(method, reqURL, bodyReader)
	if err != nil {
		return nil, nil, fmt.Errorf("creating request: %w", err)
	}

	req.Header.Set("Authorization", "Bearer "+c.Token)
	req.Header.Set("LinkedIn-Version", linkedInVersion)
	req.Header.Set("X-Restli-Protocol-Version", "2.0.0")
	req.Header.Set("Content-Type", "application/json")
	for k, v := range extraHeaders {
		req.Header.Set(k, v)
	}

	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return nil, nil, fmt.Errorf("executing request: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, nil, fmt.Errorf("reading response body: %w", err)
	}

	if resp.StatusCode == http.StatusUnauthorized {
		return nil, nil, fmt.Errorf("OAuth token expired or invalid. Run 'linkedin configure' to re-authorize")
	}
	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, nil, fmt.Errorf("rate limited by LinkedIn. Try again in a few minutes")
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		snippet := string(respBody)
		if len(snippet) > 200 {
			snippet = snippet[:200] + "..."
		}
		return nil, nil, fmt.Errorf("API error (HTTP %d): %s", resp.StatusCode, snippet)
	}

	return respBody, resp.Header, nil
}

package api

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// ImageResponse represents the response from Substack's image upload endpoint.
type ImageResponse struct {
	URL string `json:"url"`
}

// UploadImage uploads a local image file to Substack's CDN and returns the hosted URL.
// The endpoint is POST {publication_url}/api/v1/image with form data {"image": "data:mime;base64,..."}.
func (c *Client) UploadImage(filePath string) (string, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return "", fmt.Errorf("reading image file: %w", err)
	}

	// Determine MIME type from extension
	mime := "image/png"
	ext := strings.ToLower(filepath.Ext(filePath))
	switch ext {
	case ".jpg", ".jpeg":
		mime = "image/jpeg"
	case ".gif":
		mime = "image/gif"
	case ".webp":
		mime = "image/webp"
	case ".svg":
		mime = "image/svg+xml"
	}

	// Encode as data URI
	encoded := "data:" + mime + ";base64," + base64.StdEncoding.EncodeToString(data)

	// POST as form data
	form := url.Values{}
	form.Set("image", encoded)

	reqURL := c.BaseURL + "/api/v1/image"
	req, err := http.NewRequest("POST", reqURL, strings.NewReader(form.Encode()))
	if err != nil {
		return "", fmt.Errorf("creating upload request: %w", err)
	}
	req.Header.Set("Cookie", c.Cookie)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko)")

	// Use a generous timeout for large image uploads
	uploadClient := &http.Client{Timeout: 120 * time.Second}
	resp, err := uploadClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("uploading image: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		respBody, _ := io.ReadAll(resp.Body)
		return "", fmt.Errorf("image upload failed: HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(respBody)))
	}

	var result ImageResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return "", fmt.Errorf("decoding image response: %w", err)
	}

	if result.URL == "" {
		return "", fmt.Errorf("image upload returned empty URL")
	}

	return result.URL, nil
}

// UploadImageGlobal uploads a local image file to Substack's global CDN endpoint
// (https://substack.com/api/v1/image) used by notes/comments, and returns the hosted URL.
func (c *Client) UploadImageGlobal(filePath string) (string, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return "", fmt.Errorf("reading image file: %w", err)
	}

	mime := "image/png"
	ext := strings.ToLower(filepath.Ext(filePath))
	switch ext {
	case ".jpg", ".jpeg":
		mime = "image/jpeg"
	case ".gif":
		mime = "image/gif"
	case ".webp":
		mime = "image/webp"
	case ".svg":
		mime = "image/svg+xml"
	}

	encoded := "data:" + mime + ";base64," + base64.StdEncoding.EncodeToString(data)

	form := url.Values{}
	form.Set("image", encoded)

	reqURL := "https://substack.com/api/v1/image"
	req, err := http.NewRequest("POST", reqURL, strings.NewReader(form.Encode()))
	if err != nil {
		return "", fmt.Errorf("creating upload request: %w", err)
	}
	req.Header.Set("Cookie", c.Cookie)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko)")

	uploadClient := &http.Client{Timeout: 120 * time.Second}
	resp, err := uploadClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("uploading image: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		respBody, _ := io.ReadAll(resp.Body)
		return "", fmt.Errorf("image upload failed: HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(respBody)))
	}

	var result ImageResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return "", fmt.Errorf("decoding image response: %w", err)
	}

	if result.URL == "" {
		return "", fmt.Errorf("image upload returned empty URL")
	}

	return result.URL, nil
}

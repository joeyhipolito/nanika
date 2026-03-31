package browser

import (
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
)

const (
	DefaultCDPPort   = "9222"
	DefaultRemoteURL = "http://localhost:9222"
)

// CDPClient connects to a running Chrome instance via agent-browser CLI.
type CDPClient struct {
	CDPPort string
}

// NewCDPClient creates a new CDP client. If port is empty, defaults to 9222.
func NewCDPClient(remoteURL string) *CDPClient {
	port := DefaultCDPPort
	if remoteURL != "" {
		// Extract port from URL like "http://localhost:9222"
		parts := strings.Split(remoteURL, ":")
		if len(parts) > 2 {
			port = strings.TrimRight(parts[2], "/")
		}
	}
	return &CDPClient{CDPPort: port}
}

// run executes an agent-browser command and returns stdout.
// The connect command is special — it doesn't take --cdp flag.
func (c *CDPClient) run(args ...string) (string, error) {
	var fullArgs []string
	if len(args) > 0 && args[0] == "connect" {
		fullArgs = args
	} else {
		fullArgs = append([]string{"--cdp", c.CDPPort}, args...)
	}
	cmd := exec.Command("agent-browser", fullArgs...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("agent-browser %s: %s", strings.Join(args, " "), strings.TrimSpace(string(out)))
	}
	return strings.TrimSpace(string(out)), nil
}

// runJSON executes an agent-browser command with --json flag and returns stdout.
func (c *CDPClient) runJSON(args ...string) (string, error) {
	fullArgs := append(args, "--json")
	return c.run(fullArgs...)
}

// TestConnection verifies Chrome is reachable via agent-browser.
// If Chrome has no open pages, creates one.
func (c *CDPClient) TestConnection() error {
	// First check if Chrome is listening at all
	if err := c.ensurePage(); err != nil {
		return fmt.Errorf("cannot connect to Chrome on port %s: %w\n\nStart Chrome with remote debugging:\n  linkedin chrome --launch", c.CDPPort, err)
	}
	_, err := c.run("get", "title")
	if err != nil {
		return fmt.Errorf("cannot communicate with Chrome: %w", err)
	}
	return nil
}

// ensurePage checks if Chrome has any open pages, creating one if needed.
func (c *CDPClient) ensurePage() error {
	// Try connecting — if it fails with "No page found", create a blank page
	_, err := c.run("connect", c.CDPPort)
	if err != nil {
		if strings.Contains(err.Error(), "No page found") {
			// Create a new tab via CDP HTTP API
			createURL := fmt.Sprintf("http://localhost:%s/json/new?about:blank", c.CDPPort)
			cmd := exec.Command("curl", "-s", "-X", "PUT", createURL)
			if _, createErr := cmd.Output(); createErr != nil {
				return fmt.Errorf("Chrome has no open pages and could not create one: %w", createErr)
			}
			// Retry connect
			_, err = c.run("connect", c.CDPPort)
		}
	}
	return err
}

// LoginStatus holds the result of a LinkedIn login check.
type LoginStatus struct {
	LoggedIn bool   `json:"loggedIn"`
	Name     string `json:"name"`
}

// IsLoggedIn checks if the Chrome session is logged into LinkedIn.
func (c *CDPClient) IsLoggedIn() (*LoginStatus, error) {
	// Connect and navigate to feed
	if _, err := c.run("connect", c.CDPPort); err != nil {
		return nil, err
	}

	out, err := c.run("get", "url")
	if err != nil {
		return nil, err
	}

	// If not already on LinkedIn, navigate there
	if !strings.Contains(out, "linkedin.com") {
		if _, err := c.run("open", "https://www.linkedin.com/feed/"); err != nil {
			return nil, err
		}
		if _, err := c.run("wait", "--load", "networkidle"); err != nil {
			return nil, err
		}
	}

	// Check URL — if redirected to login, not logged in
	out, err = c.run("get", "url")
	if err != nil {
		return nil, err
	}

	if strings.Contains(out, "/login") || strings.Contains(out, "/authwall") || strings.Contains(out, "/uas/") {
		return &LoginStatus{LoggedIn: false}, nil
	}

	return &LoginStatus{LoggedIn: true, Name: ""}, nil
}

// Eval runs JavaScript in the browser and returns the result as a string.
func (c *CDPClient) Eval(js string) (string, error) {
	return c.run("eval", js)
}

// EvalJSON runs JavaScript in the browser and unmarshals the JSON result.
func (c *CDPClient) EvalJSON(js string, v interface{}) error {
	out, err := c.runJSON("eval", js)
	if err != nil {
		return err
	}

	// agent-browser --json wraps output; try to extract the result
	// The output may be a JSON object with a "result" field, or the raw JSON
	if err := json.Unmarshal([]byte(out), v); err != nil {
		return fmt.Errorf("parsing eval result: %w (raw: %s)", err, out[:min(len(out), 200)])
	}
	return nil
}

// Navigate opens a URL in the browser.
func (c *CDPClient) Navigate(url string) error {
	_, err := c.run("open", url)
	return err
}

// WaitNetworkIdle waits for network to settle.
func (c *CDPClient) WaitNetworkIdle() error {
	_, err := c.run("wait", "--load", "networkidle")
	return err
}

// WaitMs waits for a specified number of milliseconds.
func (c *CDPClient) WaitMs(ms int) error {
	_, err := c.run("wait", fmt.Sprintf("%d", ms))
	return err
}

// Scroll scrolls the page down by the given number of pixels.
func (c *CDPClient) Scroll(pixels int) error {
	_, err := c.run("scroll", "down", fmt.Sprintf("%d", pixels))
	return err
}

// Snapshot returns the accessibility tree of the page.
func (c *CDPClient) Snapshot(selector string) (string, error) {
	if selector != "" {
		return c.run("snapshot", "-s", selector)
	}
	return c.run("snapshot")
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

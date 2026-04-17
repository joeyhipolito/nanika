// Package usage probes the Anthropic OAuth usage endpoint to retrieve real
// plan-utilization figures for the authenticated Max-plan account.
package usage

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

// Sentinel errors returned by Probe.
var (
	ErrNoToken          = errors.New("usage: no OAuth token found in credentials file")
	ErrUnauthorized     = errors.New("usage: request unauthorized (401)")
	ErrUnexpectedStatus = errors.New("usage: unexpected HTTP status from usage endpoint")
	ErrParse            = errors.New("usage: failed to parse usage response")
)

// usageEndpoint is the URL probed by Probe. Overridable in tests.
var usageEndpoint = "https://api.anthropic.com/api/oauth/usage"

// Snapshot holds the parsed plan-utilization figures returned by the endpoint.
type Snapshot struct {
	FiveHourUtil           float64
	FiveHourResetsAt       *time.Time
	SevenDayUtil           float64
	SevenDayResetsAt       *time.Time
	SevenDaySonnetUtil     float64
	SevenDaySonnetResetsAt *time.Time
	RawJSON                string
}

// credentialsFile returns the path to the Claude credentials JSON file,
// using $CLAUDE_CREDENTIALS_FILE if set.
func credentialsFile() string {
	if v := os.Getenv("CLAUDE_CREDENTIALS_FILE"); v != "" {
		return v
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".claude", ".credentials.json")
}

// readToken loads the OAuth access token. On macOS, Claude Code stores
// credentials in Keychain under service "Claude Code-credentials" — checked
// first. Otherwise (or if Keychain lookup fails), falls back to the
// credentials file at ~/.claude/.credentials.json (overridable via
// CLAUDE_CREDENTIALS_FILE). Returns ErrNoToken if neither source has a
// non-empty token.
func readToken() (string, error) {
	// Keychain lookup is skipped when CLAUDE_USAGE_NO_KEYCHAIN=1. Tests
	// set this to force the file-path branch even on macOS where Keychain
	// would otherwise return the live developer token.
	if runtime.GOOS == "darwin" && os.Getenv("CLAUDE_USAGE_NO_KEYCHAIN") != "1" {
		if tok, err := readTokenFromKeychain(); err == nil && tok != "" {
			return tok, nil
		}
		// Keychain miss is non-fatal — try the file path next.
	}
	return readTokenFromFile()
}

// readTokenFromKeychain shells out to /usr/bin/security to retrieve the
// JSON blob stored under service "Claude Code-credentials" and extracts
// the OAuth accessToken. Returns ErrNoToken if the entry is missing or
// the JSON does not contain a non-empty token.
func readTokenFromKeychain() (string, error) {
	cmd := exec.Command("/usr/bin/security", "find-generic-password", "-s", "Claude Code-credentials", "-w")
	out, err := cmd.Output()
	if err != nil {
		return "", ErrNoToken
	}
	return parseAccessTokenJSON(out)
}

func readTokenFromFile() (string, error) {
	path := credentialsFile()
	if path == "" {
		return "", ErrNoToken
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return "", ErrNoToken
		}
		return "", fmt.Errorf("usage: reading credentials file: %w", err)
	}
	return parseAccessTokenJSON(data)
}

func parseAccessTokenJSON(data []byte) (string, error) {
	var creds struct {
		ClaudeAiOauth struct {
			AccessToken string `json:"accessToken"`
		} `json:"claudeAiOauth"`
	}
	if err := json.Unmarshal(data, &creds); err != nil {
		return "", fmt.Errorf("usage: parsing credentials JSON: %w", err)
	}
	if creds.ClaudeAiOauth.AccessToken == "" {
		return "", ErrNoToken
	}
	return creds.ClaudeAiOauth.AccessToken, nil
}

// apiUsageWindow is a single utilization/reset pair from the endpoint.
// Utilization is reported as a percent (e.g. 80.0 = 80%); we convert to a
// 0.0–1.0 fraction at the boundary so the rest of the codebase is
// unit-consistent with quota_snapshots.estimated_5h_utilization.
// ResetsAt is an RFC 3339 timestamp string, not a Unix int.
type apiUsageWindow struct {
	Utilization float64 `json:"utilization"`
	ResetsAt    string  `json:"resets_at"`
}

// apiUsageResponse mirrors the JSON shape returned by the endpoint.
// Additional fields (seven_day_oauth_apps, seven_day_opus, extra_usage,
// etc.) are intentionally not parsed; the verbatim body is preserved in
// Snapshot.RawJSON so downstream tooling can read them later.
type apiUsageResponse struct {
	FiveHour       *apiUsageWindow `json:"five_hour"`
	SevenDay       *apiUsageWindow `json:"seven_day"`
	SevenDaySonnet *apiUsageWindow `json:"seven_day_sonnet"`
}

// parseResetsAt converts an RFC 3339 timestamp string to *time.Time.
// Returns nil for empty input or unparseable values.
func parseResetsAt(s string) *time.Time {
	if s == "" {
		return nil
	}
	t, err := time.Parse(time.RFC3339Nano, s)
	if err != nil {
		t, err = time.Parse(time.RFC3339, s)
		if err != nil {
			return nil
		}
	}
	t = t.UTC()
	return &t
}

// percentToFraction converts the endpoint's percent value (0.0–100.0) to
// a 0.0–1.0 fraction. Negative values are clamped to 0.
func percentToFraction(p float64) float64 {
	f := p / 100.0
	if f < 0 {
		return 0
	}
	return f
}

// Probe fetches real plan-utilization from the Anthropic OAuth usage endpoint.
// It enforces a 3-second timeout unless ctx already has a shorter deadline.
// On any error the returned error message never contains the bearer token.
func Probe(ctx context.Context) (*Snapshot, error) {
	token, err := readToken()
	if err != nil {
		return nil, err
	}

	// Enforce 3-second hard cap; respect a shorter caller deadline.
	const hardTimeout = 3 * time.Second
	probeCtx, cancel := context.WithTimeout(ctx, hardTimeout)
	if deadline, ok := ctx.Deadline(); ok {
		if time.Until(deadline) < hardTimeout {
			cancel()
			probeCtx, cancel = context.WithDeadline(ctx, deadline)
		}
	}
	defer cancel()

	req, err := http.NewRequestWithContext(probeCtx, http.MethodGet, usageEndpoint, nil)
	if err != nil {
		return nil, fmt.Errorf("usage: building request: %w", err)
	}
	// Set headers. The Authorization value is assigned directly to avoid it
	// appearing in any format string passed to fmt functions.
	req.Header.Set("anthropic-beta", "oauth-2025-04-20")
	req.Header.Set("Content-Type", "application/json")
	req.Header["Authorization"] = []string{"Bearer " + token}

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("usage: HTTP request failed: %w", scrubToken(err, token))
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("usage: reading response body: %w", scrubToken(err, token))
	}

	if resp.StatusCode == http.StatusUnauthorized {
		return nil, ErrUnauthorized
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("%w: %d", ErrUnexpectedStatus, resp.StatusCode)
	}

	var raw apiUsageResponse
	if err := json.Unmarshal(body, &raw); err != nil {
		return nil, fmt.Errorf("%w: %v", ErrParse, err)
	}

	snap := &Snapshot{RawJSON: string(body)}
	if raw.FiveHour != nil {
		snap.FiveHourUtil = percentToFraction(raw.FiveHour.Utilization)
		snap.FiveHourResetsAt = parseResetsAt(raw.FiveHour.ResetsAt)
	}
	if raw.SevenDay != nil {
		snap.SevenDayUtil = percentToFraction(raw.SevenDay.Utilization)
		snap.SevenDayResetsAt = parseResetsAt(raw.SevenDay.ResetsAt)
	}
	if raw.SevenDaySonnet != nil {
		snap.SevenDaySonnetUtil = percentToFraction(raw.SevenDaySonnet.Utilization)
		snap.SevenDaySonnetResetsAt = parseResetsAt(raw.SevenDaySonnet.ResetsAt)
	}
	return snap, nil
}

// scrubToken replaces any occurrence of token in err's message with [REDACTED].
// This is a safety net for net/http errors that may embed the request URL.
func scrubToken(err error, token string) error {
	if err == nil || token == "" {
		return err
	}
	safe := strings.ReplaceAll(err.Error(), token, "[REDACTED]")
	if safe != err.Error() {
		return errors.New(safe)
	}
	return err
}

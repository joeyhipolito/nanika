package usage

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const testToken = "test-secret-token-VALUE-12345"

// writeCredsFile writes a credentials JSON file with the given access token.
func writeCredsFile(t *testing.T, dir, token string) string {
	t.Helper()
	type claudeAiOauth struct {
		AccessToken string `json:"accessToken"`
	}
	type creds struct {
		ClaudeAiOauth claudeAiOauth `json:"claudeAiOauth"`
	}
	data, _ := json.Marshal(creds{ClaudeAiOauth: claudeAiOauth{AccessToken: token}})
	path := filepath.Join(dir, ".credentials.json")
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatalf("writeCredsFile: %v", err)
	}
	return path
}

// canonicalResponse returns a well-formed usage response body matching the
// real wire format: utilization is a percent (0–100) and resets_at is an
// RFC 3339 timestamp string.
func canonicalResponse() []byte {
	type bucket struct {
		Utilization float64 `json:"utilization"`
		ResetsAt    string  `json:"resets_at"`
	}
	type resp struct {
		FiveHour       bucket `json:"five_hour"`
		SevenDay       bucket `json:"seven_day"`
		SevenDaySonnet bucket `json:"seven_day_sonnet"`
	}
	data, _ := json.Marshal(resp{
		FiveHour:       bucket{Utilization: 25.0, ResetsAt: "2026-04-15T00:00:00.000000+00:00"},
		SevenDay:       bucket{Utilization: 50.0, ResetsAt: "2026-04-17T04:00:00.000000+00:00"},
		SevenDaySonnet: bucket{Utilization: 75.0, ResetsAt: "2026-04-17T04:00:00.000000+00:00"},
	})
	return data
}

func TestProbe(t *testing.T) {
	// Save and restore the package-level endpoint var.
	origEndpoint := usageEndpoint
	t.Cleanup(func() { usageEndpoint = origEndpoint })

	// Save and restore the credentials env var.
	origCreds := os.Getenv("CLAUDE_CREDENTIALS_FILE")
	t.Cleanup(func() { os.Setenv("CLAUDE_CREDENTIALS_FILE", origCreds) })

	// Force the file-path branch on macOS so tests don't pick up the
	// developer's real Keychain token.
	origNoKC := os.Getenv("CLAUDE_USAGE_NO_KEYCHAIN")
	os.Setenv("CLAUDE_USAGE_NO_KEYCHAIN", "1")
	t.Cleanup(func() { os.Setenv("CLAUDE_USAGE_NO_KEYCHAIN", origNoKC) })

	t.Run("canonical 200 parses all 6 fields", func(t *testing.T) {
		dir := t.TempDir()
		os.Setenv("CLAUDE_CREDENTIALS_FILE", writeCredsFile(t, dir, testToken))

		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusOK)
			w.Write(canonicalResponse())
		}))
		t.Cleanup(srv.Close)
		usageEndpoint = srv.URL

		snap, err := Probe(context.Background())
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if snap.FiveHourUtil != 0.25 {
			t.Errorf("FiveHourUtil: want 0.25, got %v", snap.FiveHourUtil)
		}
		if snap.SevenDayUtil != 0.50 {
			t.Errorf("SevenDayUtil: want 0.50, got %v", snap.SevenDayUtil)
		}
		if snap.SevenDaySonnetUtil != 0.75 {
			t.Errorf("SevenDaySonnetUtil: want 0.75, got %v", snap.SevenDaySonnetUtil)
		}
		if snap.FiveHourResetsAt == nil {
			t.Error("FiveHourResetsAt: want non-nil")
		} else if snap.FiveHourResetsAt.Format(time.RFC3339) != "2026-04-15T00:00:00Z" {
			t.Errorf("FiveHourResetsAt: want 2026-04-15T00:00:00Z, got %v", snap.FiveHourResetsAt.Format(time.RFC3339))
		}
		if snap.SevenDayResetsAt == nil {
			t.Error("SevenDayResetsAt: want non-nil")
		} else if snap.SevenDayResetsAt.Format(time.RFC3339) != "2026-04-17T04:00:00Z" {
			t.Errorf("SevenDayResetsAt: want 2026-04-17T04:00:00Z, got %v", snap.SevenDayResetsAt.Format(time.RFC3339))
		}
		if snap.SevenDaySonnetResetsAt == nil {
			t.Error("SevenDaySonnetResetsAt: want non-nil")
		} else if snap.SevenDaySonnetResetsAt.Format(time.RFC3339) != "2026-04-17T04:00:00Z" {
			t.Errorf("SevenDaySonnetResetsAt: want 2026-04-17T04:00:00Z, got %v", snap.SevenDaySonnetResetsAt.Format(time.RFC3339))
		}
		if snap.RawJSON == "" {
			t.Error("RawJSON: want non-empty")
		}
	})

	t.Run("401 returns ErrUnauthorized", func(t *testing.T) {
		dir := t.TempDir()
		os.Setenv("CLAUDE_CREDENTIALS_FILE", writeCredsFile(t, dir, testToken))

		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusUnauthorized)
		}))
		t.Cleanup(srv.Close)
		usageEndpoint = srv.URL

		_, err := Probe(context.Background())
		if !errors.Is(err, ErrUnauthorized) {
			t.Fatalf("want ErrUnauthorized, got %v", err)
		}
		assertNoToken(t, err)
	})

	t.Run("malformed JSON returns ErrParse", func(t *testing.T) {
		dir := t.TempDir()
		os.Setenv("CLAUDE_CREDENTIALS_FILE", writeCredsFile(t, dir, testToken))

		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusOK)
			w.Write([]byte(`{not valid json`))
		}))
		t.Cleanup(srv.Close)
		usageEndpoint = srv.URL

		_, err := Probe(context.Background())
		if !errors.Is(err, ErrParse) {
			t.Fatalf("want ErrParse, got %v", err)
		}
		assertNoToken(t, err)
	})

	t.Run("slow server returns deadline error within 3s", func(t *testing.T) {
		dir := t.TempDir()
		os.Setenv("CLAUDE_CREDENTIALS_FILE", writeCredsFile(t, dir, testToken))

		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			// Delay longer than Probe's 3s hard cap.
			select {
			case <-time.After(5 * time.Second):
			case <-r.Context().Done():
			}
			w.WriteHeader(http.StatusOK)
		}))
		t.Cleanup(srv.Close)
		usageEndpoint = srv.URL

		start := time.Now()
		_, err := Probe(context.Background())
		elapsed := time.Since(start)

		if err == nil {
			t.Fatal("expected timeout error, got nil")
		}
		if elapsed >= 4*time.Second {
			t.Errorf("Probe did not honour 3s cap: elapsed %v", elapsed)
		}
		// The error must be context-related (deadline exceeded or context canceled).
		if !errors.Is(err, context.DeadlineExceeded) && !strings.Contains(err.Error(), "deadline") && !strings.Contains(err.Error(), "context") {
			t.Errorf("expected context/deadline error, got: %v", err)
		}
		assertNoToken(t, err)
	})

	t.Run("credentials file missing returns ErrNoToken", func(t *testing.T) {
		dir := t.TempDir()
		// Point to a file that does not exist.
		os.Setenv("CLAUDE_CREDENTIALS_FILE", filepath.Join(dir, "nonexistent.json"))

		_, err := Probe(context.Background())
		if !errors.Is(err, ErrNoToken) {
			t.Fatalf("want ErrNoToken, got %v", err)
		}
		// No token was loaded, so nothing to scrub — but the error must not
		// accidentally contain the constant either.
		if strings.Contains(err.Error(), testToken) {
			t.Errorf("error string contains test token: %v", err)
		}
	})

	t.Run("empty accessToken returns ErrNoToken", func(t *testing.T) {
		dir := t.TempDir()
		os.Setenv("CLAUDE_CREDENTIALS_FILE", writeCredsFile(t, dir, "" /* empty token */))

		_, err := Probe(context.Background())
		if !errors.Is(err, ErrNoToken) {
			t.Fatalf("want ErrNoToken, got %v", err)
		}
		if strings.Contains(err.Error(), testToken) {
			t.Errorf("error string contains test token: %v", err)
		}
	})
}

// assertNoToken fails t if err's message contains testToken.
// Only called for cases where a token was actually loaded.
func assertNoToken(t *testing.T, err error) {
	t.Helper()
	if err != nil && strings.Contains(err.Error(), testToken) {
		t.Errorf("error string must not contain bearer token, but got: %v", err)
	}
}

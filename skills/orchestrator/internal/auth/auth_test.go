package auth

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

// writeFakeCredFile writes ~/.claude/.credentials.json fixture to dir.
func writeFakeCredFile(t *testing.T, dir, accessToken, refreshToken string, expiresAtMs int64) string {
	t.Helper()
	type oauthFields struct {
		AccessToken  string `json:"accessToken"`
		RefreshToken string `json:"refreshToken"`
		ExpiresAt    int64  `json:"expiresAt,omitempty"`
	}
	type creds struct {
		ClaudeAiOauth oauthFields `json:"claudeAiOauth"`
	}
	data, _ := json.Marshal(creds{ClaudeAiOauth: oauthFields{
		AccessToken:  accessToken,
		RefreshToken: refreshToken,
		ExpiresAt:    expiresAtMs,
	}})
	path := filepath.Join(dir, ".credentials.json")
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatalf("writeFakeCredFile: %v", err)
	}
	return path
}

// writeAuthJSON writes ~/.alluka/auth.json with a single provider entry.
func writeAuthJSON(t *testing.T, path string, entries map[string]authJSONEntry) {
	t.Helper()
	data, _ := json.MarshalIndent(entries, "", "  ")
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		t.Fatalf("writeAuthJSON mkdir: %v", err)
	}
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatalf("writeAuthJSON: %v", err)
	}
}

func TestLoadCredential_EnvVarPrecedence(t *testing.T) {
	dir := t.TempDir()
	authFile := filepath.Join(dir, "auth.json")
	credDir := filepath.Join(dir, "claude")
	if err := os.MkdirAll(credDir, 0700); err != nil {
		t.Fatal(err)
	}

	// auth.json has an OAuth entry; env var should win.
	writeAuthJSON(t, authFile, map[string]authJSONEntry{
		"anthropic": {AuthType: AuthTypeOAuth, AccessToken: "oauth-token", RefreshToken: "rt"},
	})
	writeFakeCredFile(t, credDir, "cred-file-token", "cred-rt", 0)

	t.Setenv("ANTHROPIC_API_KEY", "sk-env-key")
	t.Setenv("ALLUKA_AUTH_FILE", authFile)
	t.Setenv("CLAUDE_CREDENTIALS_DIR", credDir)

	cred, err := LoadCredential("anthropic")
	if err != nil {
		t.Fatalf("LoadCredential: %v", err)
	}
	if cred.Source != SourceEnv {
		t.Errorf("Source: want %q, got %q", SourceEnv, cred.Source)
	}
	if cred.APIKey != "sk-env-key" {
		t.Errorf("APIKey: want %q, got %q", "sk-env-key", cred.APIKey)
	}
	if cred.AuthType != AuthTypeAPIKey {
		t.Errorf("AuthType: want %q, got %q", AuthTypeAPIKey, cred.AuthType)
	}
	if cred.Provider != "anthropic" {
		t.Errorf("Provider: want %q, got %q", "anthropic", cred.Provider)
	}
}

func TestLoadCredential_AuthJSONAPIKey(t *testing.T) {
	dir := t.TempDir()
	authFile := filepath.Join(dir, "auth.json")

	writeAuthJSON(t, authFile, map[string]authJSONEntry{
		"anthropic": {AuthType: AuthTypeAPIKey, APIKey: "sk-from-file"},
	})

	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("ALLUKA_AUTH_FILE", authFile)

	cred, err := LoadCredential("anthropic")
	if err != nil {
		t.Fatalf("LoadCredential: %v", err)
	}
	if cred.Source != SourceAuthJSON {
		t.Errorf("Source: want %q, got %q", SourceAuthJSON, cred.Source)
	}
	if cred.APIKey != "sk-from-file" {
		t.Errorf("APIKey: want %q, got %q", "sk-from-file", cred.APIKey)
	}
}

func TestLoadCredential_OAuthImportFromCredFile(t *testing.T) {
	dir := t.TempDir()
	credDir := filepath.Join(dir, "claude")
	if err := os.MkdirAll(credDir, 0700); err != nil {
		t.Fatal(err)
	}

	expMs := time.Now().Add(1 * time.Hour).UnixMilli()
	writeFakeCredFile(t, credDir, "access-tok", "refresh-tok", expMs)

	// No env key, no auth.json.
	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("ALLUKA_AUTH_FILE", filepath.Join(dir, "nonexistent.json"))
	t.Setenv("CLAUDE_CREDENTIALS_DIR", credDir)

	cred, err := LoadCredential("anthropic")
	if err != nil {
		t.Fatalf("LoadCredential: %v", err)
	}
	if cred.Source != SourceCredFile {
		t.Errorf("Source: want %q, got %q", SourceCredFile, cred.Source)
	}
	if cred.AccessToken != "access-tok" {
		t.Errorf("AccessToken: want %q, got %q", "access-tok", cred.AccessToken)
	}
	if cred.RefreshToken != "refresh-tok" {
		t.Errorf("RefreshToken: want %q, got %q", "refresh-tok", cred.RefreshToken)
	}
	if cred.AuthType != AuthTypeOAuth {
		t.Errorf("AuthType: want %q, got %q", AuthTypeOAuth, cred.AuthType)
	}
	// ExpiresAt should be within 1 second of the source ms timestamp.
	wantExp := time.UnixMilli(expMs).UTC()
	if diff := cred.ExpiresAt.Sub(wantExp); diff < -time.Second || diff > time.Second {
		t.Errorf("ExpiresAt: want ~%v, got %v", wantExp, cred.ExpiresAt)
	}
}

func TestLoadCredential_NoCredential(t *testing.T) {
	dir := t.TempDir()

	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("ALLUKA_AUTH_FILE", filepath.Join(dir, "nonexistent.json"))
	t.Setenv("CLAUDE_CREDENTIALS_DIR", filepath.Join(dir, "no-claude"))
	t.Setenv("CLAUDE_AUTH_NO_KEYCHAIN", "1")

	_, err := LoadCredential("anthropic")
	if !errors.Is(err, ErrNoCredential) {
		t.Errorf("want ErrNoCredential, got %v", err)
	}
}

func TestLoadCredential_AuthJSONOAuthWithExpiry(t *testing.T) {
	dir := t.TempDir()
	authFile := filepath.Join(dir, "auth.json")
	exp := time.Now().Add(2 * time.Hour).UTC().Truncate(time.Second)

	writeAuthJSON(t, authFile, map[string]authJSONEntry{
		"anthropic": {
			AuthType:     AuthTypeOAuth,
			AccessToken:  "at",
			RefreshToken: "rt",
			ExpiresAt:    exp.Format(time.RFC3339),
		},
	})

	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("ALLUKA_AUTH_FILE", authFile)

	cred, err := LoadCredential("anthropic")
	if err != nil {
		t.Fatalf("LoadCredential: %v", err)
	}
	if !cred.ExpiresAt.Equal(exp) {
		t.Errorf("ExpiresAt: want %v, got %v", exp, cred.ExpiresAt)
	}
	if cred.IsExpired() {
		t.Error("IsExpired: want false for future token")
	}
}

func TestRefreshOAuth_FixtureFlow(t *testing.T) {
	dir := t.TempDir()
	authFile := filepath.Join(dir, "auth.json")

	t.Setenv("ALLUKA_AUTH_FILE", authFile)

	// Save and restore package-level endpoint.
	orig := refreshEndpoint
	t.Cleanup(func() { refreshEndpoint = orig })

	newAccess := "new-access-token-xyz"
	newRefresh := "new-refresh-token-abc"

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "want POST", http.StatusMethodNotAllowed)
			return
		}
		var req refreshRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, "bad body", http.StatusBadRequest)
			return
		}
		if req.GrantType != "refresh_token" || req.RefreshToken != "old-refresh" {
			http.Error(w, "wrong body", http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(refreshResponse{
			AccessToken:  newAccess,
			RefreshToken: newRefresh,
			ExpiresIn:    3600,
		})
	}))
	t.Cleanup(srv.Close)
	refreshEndpoint = srv.URL

	cred := &Credential{
		Provider:     "anthropic",
		AuthType:     AuthTypeOAuth,
		AccessToken:  "old-access",
		RefreshToken: "old-refresh",
		Source:       SourceCredFile,
	}

	if err := RefreshOAuth(context.Background(), cred); err != nil {
		t.Fatalf("RefreshOAuth: %v", err)
	}

	if cred.AccessToken != newAccess {
		t.Errorf("AccessToken: want %q, got %q", newAccess, cred.AccessToken)
	}
	if cred.RefreshToken != newRefresh {
		t.Errorf("RefreshToken: want %q, got %q", newRefresh, cred.RefreshToken)
	}
	if cred.ExpiresAt.IsZero() {
		t.Error("ExpiresAt: want non-zero after refresh")
	}

	// Verify persisted to auth.json.
	data, err := os.ReadFile(authFile)
	if err != nil {
		t.Fatalf("reading auth.json: %v", err)
	}
	var store map[string]authJSONEntry
	if err := json.Unmarshal(data, &store); err != nil {
		t.Fatalf("parsing persisted auth.json: %v", err)
	}
	entry, ok := store["anthropic"]
	if !ok {
		t.Fatal("persisted auth.json missing anthropic entry")
	}
	if entry.AccessToken != newAccess {
		t.Errorf("persisted AccessToken: want %q, got %q", newAccess, entry.AccessToken)
	}
	if entry.RefreshToken != newRefresh {
		t.Errorf("persisted RefreshToken: want %q, got %q", newRefresh, entry.RefreshToken)
	}
}

func TestRefreshOAuth_HTTPError(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_AUTH_FILE", filepath.Join(dir, "auth.json"))

	orig := refreshEndpoint
	t.Cleanup(func() { refreshEndpoint = orig })

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
	}))
	t.Cleanup(srv.Close)
	refreshEndpoint = srv.URL

	cred := &Credential{
		Provider:     "anthropic",
		AuthType:     AuthTypeOAuth,
		RefreshToken: "rt",
	}
	if err := RefreshOAuth(context.Background(), cred); err == nil {
		t.Error("want error on 401, got nil")
	}
}

func TestRefreshOAuth_NonOAuthCredential(t *testing.T) {
	cred := &Credential{AuthType: AuthTypeAPIKey, APIKey: "sk-key"}
	err := RefreshOAuth(context.Background(), cred)
	if err == nil {
		t.Error("want error for API key credential")
	}
}

func TestAtomicWriteUnderContention(t *testing.T) {
	dir := t.TempDir()
	authFile := filepath.Join(dir, "auth.json")

	orig := refreshEndpoint
	t.Cleanup(func() { refreshEndpoint = orig })

	// Serve a valid refresh response for all callers.
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(refreshResponse{
			AccessToken:  "shared-access",
			RefreshToken: "shared-refresh",
			ExpiresIn:    3600,
		})
	}))
	t.Cleanup(srv.Close)
	refreshEndpoint = srv.URL

	t.Setenv("ALLUKA_AUTH_FILE", authFile)

	const n = 20
	errs := make([]error, n)
	var wg sync.WaitGroup
	wg.Add(n)
	for i := range n {
		go func(i int) {
			defer wg.Done()
			cred := &Credential{
				Provider:     "anthropic",
				AuthType:     AuthTypeOAuth,
				AccessToken:  "old",
				RefreshToken: "rt",
				Source:       SourceCredFile,
			}
			errs[i] = RefreshOAuth(context.Background(), cred)
		}(i)
	}
	wg.Wait()

	for i, err := range errs {
		if err != nil {
			t.Errorf("goroutine %d: RefreshOAuth error: %v", i, err)
		}
	}

	// File must be valid JSON after all concurrent writes.
	data, err := os.ReadFile(authFile)
	if err != nil {
		t.Fatalf("reading auth.json after contention: %v", err)
	}
	var store map[string]authJSONEntry
	if err := json.Unmarshal(data, &store); err != nil {
		t.Errorf("auth.json corrupted after concurrent writes: %v\ncontent: %s", err, data)
	}
}

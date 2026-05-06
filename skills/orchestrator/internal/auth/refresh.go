package auth

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"syscall"
	"time"
)

// refreshEndpoint is the Anthropic OAuth token refresh URL. Overridable in tests.
var refreshEndpoint = "https://api.anthropic.com/api/auth/oauth/token"

type refreshRequest struct {
	GrantType    string `json:"grant_type"`
	RefreshToken string `json:"refresh_token"`
}

type refreshResponse struct {
	AccessToken  string  `json:"access_token"`
	RefreshToken string  `json:"refresh_token,omitempty"`
	ExpiresIn    float64 `json:"expires_in,omitempty"` // seconds
	TokenType    string  `json:"token_type,omitempty"`
}

// RefreshOAuth hits the Anthropic token refresh endpoint, updates cred in
// place with the new tokens, and atomically persists the result to
// ~/.alluka/auth.json. Thread-safe: uses an in-process mutex and an advisory
// file lock to coordinate with other processes.
func RefreshOAuth(ctx context.Context, cred *Credential) error {
	if cred.AuthType != AuthTypeOAuth {
		return fmt.Errorf("auth: RefreshOAuth called on non-OAuth credential (type=%s)", cred.AuthType)
	}
	if cred.RefreshToken == "" {
		return fmt.Errorf("auth: credential has no refresh token")
	}

	body, err := json.Marshal(refreshRequest{
		GrantType:    "refresh_token",
		RefreshToken: cred.RefreshToken,
	})
	if err != nil {
		return fmt.Errorf("auth: marshalling refresh request: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, refreshEndpoint, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("auth: building refresh request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("anthropic-beta", "oauth-2025-04-20")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("auth: refresh HTTP request: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("auth: reading refresh response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("auth: refresh endpoint returned %d: %s", resp.StatusCode, respBody)
	}

	var result refreshResponse
	if err := json.Unmarshal(respBody, &result); err != nil {
		return fmt.Errorf("auth: parsing refresh response: %w", err)
	}
	if result.AccessToken == "" {
		return fmt.Errorf("auth: refresh response missing access_token")
	}

	mu.Lock()
	defer mu.Unlock()

	cred.AccessToken = result.AccessToken
	if result.RefreshToken != "" {
		cred.RefreshToken = result.RefreshToken
	}
	if result.ExpiresIn > 0 {
		cred.ExpiresAt = time.Now().Add(time.Duration(result.ExpiresIn) * time.Second).UTC()
	}

	return persistCredential(cred)
}

// persistCredential writes cred to ~/.alluka/auth.json atomically:
// write to a temp file in the same dir, acquire an advisory flock on a
// sidecar lock file, then rename into place.
func persistCredential(cred *Credential) error {
	path := allukaAuthFileFn()
	if path == "" {
		return fmt.Errorf("auth: cannot determine auth.json path")
	}

	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("auth: ensuring auth dir exists: %w", err)
	}

	// Load existing store; start fresh when the file is absent or malformed.
	store := make(map[string]authJSONEntry)
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &store)
	}

	entry := authJSONEntry{
		AuthType:     cred.AuthType,
		APIKey:       cred.APIKey,
		AccessToken:  cred.AccessToken,
		RefreshToken: cred.RefreshToken,
	}
	if !cred.ExpiresAt.IsZero() {
		entry.ExpiresAt = cred.ExpiresAt.UTC().Format(time.RFC3339)
	}
	store[cred.Provider] = entry

	data, err := json.MarshalIndent(store, "", "  ")
	if err != nil {
		return fmt.Errorf("auth: marshalling auth.json: %w", err)
	}

	// Advisory file lock coordinates concurrent processes; mu handles goroutines.
	lockPath := path + ".lock"
	lockFile, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return fmt.Errorf("auth: opening lock file: %w", err)
	}
	defer lockFile.Close()

	if err := syscall.Flock(int(lockFile.Fd()), syscall.LOCK_EX); err != nil {
		return fmt.Errorf("auth: acquiring file lock: %w", err)
	}
	defer syscall.Flock(int(lockFile.Fd()), syscall.LOCK_UN) //nolint:errcheck

	tmp, err := os.CreateTemp(dir, "auth.json.tmp-*")
	if err != nil {
		return fmt.Errorf("auth: creating temp file: %w", err)
	}
	tmpPath := tmp.Name()
	defer os.Remove(tmpPath) // no-op when rename succeeded

	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("auth: writing temp file: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("auth: syncing temp file: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("auth: closing temp file: %w", err)
	}

	if err := os.Rename(tmpPath, path); err != nil {
		return fmt.Errorf("auth: atomic rename to %s: %w", path, err)
	}
	return nil
}

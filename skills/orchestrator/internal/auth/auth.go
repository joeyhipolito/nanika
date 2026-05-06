// Package auth resolves and refreshes credentials for API providers.
// Resolution order for LoadCredential: env var → ~/.alluka/auth.json →
// ~/.claude/.credentials.json → macOS Keychain.
package auth

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"
)

// AuthType is the authentication mechanism for a credential.
type AuthType string

const (
	AuthTypeAPIKey AuthType = "api_key"
	AuthTypeOAuth  AuthType = "oauth"
)

// Source records where a credential was loaded from.
type Source string

const (
	SourceEnv      Source = "env"
	SourceAuthJSON Source = "auth_json"
	SourceCredFile Source = "cred_file"
	SourceKeychain Source = "keychain"
)

// ErrNoCredential is returned when no credential is found for the requested provider.
var ErrNoCredential = errors.New("auth: no credential found for provider")

// Credential holds authentication material for a provider.
type Credential struct {
	Provider     string    `json:"provider"`
	AuthType     AuthType  `json:"auth_type"`
	AccessToken  string    `json:"access_token,omitempty"`
	RefreshToken string    `json:"refresh_token,omitempty"`
	ExpiresAt    time.Time `json:"expires_at,omitempty"`
	APIKey       string    `json:"api_key,omitempty"`
	Source       Source    `json:"source"`
}

// IsExpired reports whether the OAuth access token has expired.
// Returns false for API key credentials or when ExpiresAt is the zero time.
func (c *Credential) IsExpired() bool {
	if c.AuthType != AuthTypeOAuth || c.ExpiresAt.IsZero() {
		return false
	}
	return time.Now().After(c.ExpiresAt)
}

// mu guards in-process concurrent credential operations.
var mu sync.Mutex

// claudeCredDirFn returns the path to the Claude config directory.
// Points to an overridable var so tests can redirect to a temp dir.
var claudeCredDirFn = func() string {
	if v := os.Getenv("CLAUDE_CREDENTIALS_DIR"); v != "" {
		return v
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".claude")
}

// allukaAuthFileFn returns the path to ~/.alluka/auth.json.
// Points to an overridable var so tests can redirect to a temp file.
var allukaAuthFileFn = func() string {
	if v := os.Getenv("ALLUKA_AUTH_FILE"); v != "" {
		return v
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".alluka", "auth.json")
}

// LoadCredential returns a credential for provider (e.g. "anthropic"),
// resolving in priority order:
//  1. Environment variable <UPPER_PROVIDER>_API_KEY (e.g. ANTHROPIC_API_KEY)
//  2. ~/.alluka/auth.json
//  3. ~/.claude/.credentials.json (OAuth import)
//  4. macOS Keychain entry "Claude Code-credentials" (darwin only)
func LoadCredential(provider string) (*Credential, error) {
	if cred, ok := loadFromEnv(provider); ok {
		return cred, nil
	}
	if cred, ok := loadFromAuthJSON(provider); ok {
		return cred, nil
	}
	if cred, ok := loadOAuthFromCredFile(provider); ok {
		return cred, nil
	}
	// CLAUDE_AUTH_NO_KEYCHAIN=1 lets tests skip Keychain even on macOS,
	// mirroring the CLAUDE_USAGE_NO_KEYCHAIN pattern in the usage package.
	if runtime.GOOS == "darwin" && os.Getenv("CLAUDE_AUTH_NO_KEYCHAIN") != "1" {
		if cred, ok := loadOAuthFromKeychain(provider); ok {
			return cred, nil
		}
	}
	return nil, fmt.Errorf("%w: %s", ErrNoCredential, provider)
}

func loadFromEnv(provider string) (*Credential, bool) {
	key := strings.ToUpper(provider) + "_API_KEY"
	val := os.Getenv(key)
	if val == "" {
		return nil, false
	}
	return &Credential{
		Provider: provider,
		AuthType: AuthTypeAPIKey,
		APIKey:   val,
		Source:   SourceEnv,
	}, true
}

// authJSONEntry is a single provider entry in ~/.alluka/auth.json.
type authJSONEntry struct {
	AuthType     AuthType `json:"auth_type"`
	APIKey       string   `json:"api_key,omitempty"`
	AccessToken  string   `json:"access_token,omitempty"`
	RefreshToken string   `json:"refresh_token,omitempty"`
	ExpiresAt    string   `json:"expires_at,omitempty"` // RFC 3339
}

func loadFromAuthJSON(provider string) (*Credential, bool) {
	path := allukaAuthFileFn()
	if path == "" {
		return nil, false
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, false
	}
	var store map[string]authJSONEntry
	if err := json.Unmarshal(data, &store); err != nil {
		return nil, false
	}
	entry, ok := store[provider]
	if !ok {
		return nil, false
	}
	cred := &Credential{
		Provider:     provider,
		AuthType:     entry.AuthType,
		APIKey:       entry.APIKey,
		AccessToken:  entry.AccessToken,
		RefreshToken: entry.RefreshToken,
		Source:       SourceAuthJSON,
	}
	if entry.ExpiresAt != "" {
		if t, err := time.Parse(time.RFC3339, entry.ExpiresAt); err == nil {
			cred.ExpiresAt = t.UTC()
		}
	}
	return cred, true
}

// claudeCredentials matches the JSON shape of ~/.claude/.credentials.json.
type claudeCredentials struct {
	ClaudeAiOauth struct {
		AccessToken  string      `json:"accessToken"`
		RefreshToken string      `json:"refreshToken"`
		ExpiresAt    interface{} `json:"expiresAt"` // ms timestamp or RFC 3339 string
	} `json:"claudeAiOauth"`
}

func loadOAuthFromCredFile(provider string) (*Credential, bool) {
	if provider != "anthropic" {
		return nil, false
	}
	dir := claudeCredDirFn()
	if dir == "" {
		return nil, false
	}
	data, err := os.ReadFile(filepath.Join(dir, ".credentials.json"))
	if err != nil {
		return nil, false
	}
	return parseClaudeCredentials(data, SourceCredFile)
}

func loadOAuthFromKeychain(provider string) (*Credential, bool) {
	if provider != "anthropic" {
		return nil, false
	}
	cmd := exec.Command("/usr/bin/security", "find-generic-password",
		"-s", "Claude Code-credentials", "-w")
	out, err := cmd.Output()
	if err != nil {
		return nil, false
	}
	return parseClaudeCredentials(out, SourceKeychain)
}

func parseClaudeCredentials(data []byte, src Source) (*Credential, bool) {
	var raw claudeCredentials
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, false
	}
	if raw.ClaudeAiOauth.AccessToken == "" {
		return nil, false
	}
	return &Credential{
		Provider:     "anthropic",
		AuthType:     AuthTypeOAuth,
		AccessToken:  raw.ClaudeAiOauth.AccessToken,
		RefreshToken: raw.ClaudeAiOauth.RefreshToken,
		ExpiresAt:    parseExpiresAt(raw.ClaudeAiOauth.ExpiresAt),
		Source:       src,
	}, true
}

// parseExpiresAt handles both Unix millisecond timestamps (float64 from JSON)
// and RFC 3339 strings, as Claude Code uses different formats across versions.
func parseExpiresAt(v interface{}) time.Time {
	switch t := v.(type) {
	case float64:
		if t > 0 {
			return time.UnixMilli(int64(t)).UTC()
		}
	case string:
		if t != "" {
			if parsed, err := time.Parse(time.RFC3339, t); err == nil {
				return parsed.UTC()
			}
		}
	}
	return time.Time{}
}

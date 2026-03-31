package auth

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"golang.org/x/oauth2"
)

// LoadToken reads an OAuth2 token from the given path.
func LoadToken(path string) (*oauth2.Token, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read token file: %w", err)
	}

	var token oauth2.Token
	if err := json.Unmarshal(data, &token); err != nil {
		return nil, fmt.Errorf("failed to parse token file: %w", err)
	}

	return &token, nil
}

// SaveToken writes an OAuth2 token to the given path with 0600 permissions.
// Parent directories are created with 0700 permissions if they don't exist.
func SaveToken(path string, token *oauth2.Token) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("failed to create token directory: %w", err)
	}

	data, err := json.MarshalIndent(token, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal token: %w", err)
	}

	if err := os.WriteFile(path, data, 0600); err != nil {
		return fmt.Errorf("failed to write token file: %w", err)
	}

	return nil
}

// SavingTokenSource wraps an oauth2.TokenSource and persists refreshed tokens
// to disk automatically. This ensures that when a token is refreshed in the
// background, the new token is saved without requiring explicit caller action.
type SavingTokenSource struct {
	Base     oauth2.TokenSource
	Path     string
	lastSave *oauth2.Token
}

// Token returns a valid token, saving it to disk if it was refreshed.
func (s *SavingTokenSource) Token() (*oauth2.Token, error) {
	token, err := s.Base.Token()
	if err != nil {
		return nil, err
	}

	// Save if this is a new or refreshed token (different access token)
	if s.lastSave == nil || s.lastSave.AccessToken != token.AccessToken {
		if saveErr := SaveToken(s.Path, token); saveErr != nil {
			return token, fmt.Errorf("token valid but failed to save: %w", saveErr)
		}
		s.lastSave = token
	}

	return token, nil
}

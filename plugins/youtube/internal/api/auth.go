package api

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"time"
)

const (
	ytTokenURL    = "https://oauth2.googleapis.com/token"
	ytAuthBaseURL = "https://accounts.google.com/o/oauth2/v2/auth"
	ytScope       = "https://www.googleapis.com/auth/youtube.force-ssl"
	ytRedirectURI = "urn:ietf:wg:oauth:2.0:oob"
)

// baseDir returns the ~/.alluka/ directory, respecting ALLUKA_HOME override.
func baseDir() (string, error) {
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return v, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, ".alluka"), nil
}

// tokenPath returns the path to youtube-oauth.json in the alluka base directory.
func tokenPath() (string, error) {
	base, err := baseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "youtube-oauth.json"), nil
}

// configPath returns the path to youtube-config.json in the alluka base directory.
func configPath() (string, error) {
	base, err := baseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "youtube-config.json"), nil
}

// LoadConfig reads ~/.alluka/youtube-config.json.
func LoadConfig() (Config, error) {
	path, err := configPath()
	if err != nil {
		return Config{}, err
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return Config{}, fmt.Errorf("reading %s: %w", path, err)
	}
	var cfg Config
	if err := json.Unmarshal(data, &cfg); err != nil {
		return Config{}, fmt.Errorf("parsing youtube-config.json: %w", err)
	}
	if cfg.Budget <= 0 {
		cfg.Budget = 10000
	}
	return cfg, nil
}

// SaveConfig writes the Config to ~/.alluka/youtube-config.json (0600).
func SaveConfig(cfg Config) error {
	path, err := configPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("creating ~/.alluka: %w", err)
	}
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling config: %w", err)
	}
	if err := os.WriteFile(path, data, 0600); err != nil {
		return fmt.Errorf("writing youtube-config.json: %w", err)
	}
	return nil
}

// ConfigExists returns true if ~/.alluka/youtube-config.json exists.
func ConfigExists() bool {
	path, err := configPath()
	if err != nil {
		return false
	}
	_, err = os.Stat(path)
	return err == nil
}

// ConfigFilePath returns the resolved path to youtube-config.json.
func ConfigFilePath() (string, error) {
	return configPath()
}

// LoadToken reads the OAuth2 token from ~/.alluka/youtube-oauth.json.
func LoadToken() (*Token, error) {
	path, err := tokenPath()
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading youtube token at %s: %w", path, err)
	}
	var t Token
	if err := json.Unmarshal(data, &t); err != nil {
		return nil, fmt.Errorf("parsing youtube token: %w", err)
	}
	return &t, nil
}

// SaveToken writes the OAuth2 token to ~/.alluka/youtube-oauth.json (0600).
func SaveToken(t *Token) error {
	path, err := tokenPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("creating ~/.alluka: %w", err)
	}
	data, err := json.MarshalIndent(t, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling youtube token: %w", err)
	}
	if err := os.WriteFile(path, data, 0600); err != nil {
		return fmt.Errorf("writing youtube token: %w", err)
	}
	return nil
}

// RefreshToken exchanges a refresh token for a new access token.
func RefreshToken(clientID, clientSecret, refreshToken string) (*Token, error) {
	resp, err := http.PostForm(ytTokenURL, url.Values{
		"client_id":     {clientID},
		"client_secret": {clientSecret},
		"refresh_token": {refreshToken},
		"grant_type":    {"refresh_token"},
	})
	if err != nil {
		return nil, fmt.Errorf("refreshing youtube token: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading refresh response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("refresh failed (%d): %s", resp.StatusCode, body)
	}

	var result struct {
		AccessToken string `json:"access_token"`
		ExpiresIn   int    `json:"expires_in"`
		TokenType   string `json:"token_type"`
		Error       string `json:"error"`
		ErrorDesc   string `json:"error_description"`
	}
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("parsing refresh response: %w", err)
	}
	if result.Error != "" {
		return nil, fmt.Errorf("refresh error %q: %s", result.Error, result.ErrorDesc)
	}

	return &Token{
		AccessToken:  result.AccessToken,
		RefreshToken: refreshToken, // refresh token is unchanged on refresh
		Expiry:       time.Now().Add(time.Duration(result.ExpiresIn) * time.Second),
		TokenType:    result.TokenType,
	}, nil
}

// AuthURL returns the OAuth2 authorization URL for the user to visit.
func AuthURL(clientID string) string {
	params := url.Values{
		"client_id":     {clientID},
		"redirect_uri":  {ytRedirectURI},
		"response_type": {"code"},
		"scope":         {ytScope},
		"access_type":   {"offline"},
		"prompt":        {"consent"},
	}
	return ytAuthBaseURL + "?" + params.Encode()
}

// ExchangeCode exchanges an authorization code for OAuth2 tokens.
func ExchangeCode(clientID, clientSecret, code string) (*Token, error) {
	resp, err := http.PostForm(ytTokenURL, url.Values{
		"client_id":     {clientID},
		"client_secret": {clientSecret},
		"code":          {code},
		"grant_type":    {"authorization_code"},
		"redirect_uri":  {ytRedirectURI},
	})
	if err != nil {
		return nil, fmt.Errorf("exchanging auth code: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading token response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("token exchange failed (%d): %s", resp.StatusCode, body)
	}

	var result struct {
		AccessToken  string `json:"access_token"`
		RefreshToken string `json:"refresh_token"`
		ExpiresIn    int    `json:"expires_in"`
		TokenType    string `json:"token_type"`
		Error        string `json:"error"`
		ErrorDesc    string `json:"error_description"`
	}
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("parsing token response: %w", err)
	}
	if result.Error != "" {
		return nil, fmt.Errorf("token error %q: %s", result.Error, result.ErrorDesc)
	}
	if result.RefreshToken == "" {
		return nil, fmt.Errorf("no refresh_token in response — ensure access_type=offline and prompt=consent")
	}

	return &Token{
		AccessToken:  result.AccessToken,
		RefreshToken: result.RefreshToken,
		Expiry:       time.Now().Add(time.Duration(result.ExpiresIn) * time.Second),
		TokenType:    result.TokenType,
	}, nil
}

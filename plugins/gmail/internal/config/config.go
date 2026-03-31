// Package config handles reading and writing the Gmail CLI configuration.
// Configuration is stored in ~/.gmail/config in INI-style format.
// Account management uses ~/.gmail/accounts.json (JSON array).
// Per-account OAuth tokens are stored in ~/.gmail/tokens/<alias>.json.
package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const (
	// ConfigDir is the directory name for Gmail configuration.
	ConfigDir = ".gmail"
	// ConfigFile is the configuration file name.
	ConfigFile = "config"
	// AccountsFile is the accounts registry file name.
	AccountsFile = "accounts.json"
	// TokensDir is the subdirectory for per-account OAuth tokens.
	TokensDir = "tokens"

	// EnvClientID is the environment variable that overrides client_id from config.
	EnvClientID = "GMAIL_CLIENT_ID"
	// EnvClientSecret is the environment variable that overrides client_secret from config.
	EnvClientSecret = "GMAIL_CLIENT_SECRET"
)

const EnvConfigDir = "GMAIL_CONFIG_DIR"

var (
	cfgMgr  = newWithEnv(ConfigDir, ConfigFile, EnvConfigDir)
	acctMgr = newWithEnv(ConfigDir, AccountsFile, EnvConfigDir)
)

// Config represents the Gmail CLI configuration.
type Config struct {
	ClientID     string
	ClientSecret string
}

// Account represents a registered Gmail account.
type Account struct {
	Alias   string `json:"alias"`
	Email   string `json:"email"`
	AddedAt string `json:"added_at"`
}

// Path returns the full path to the config file (~/.gmail/config).
func Path() string {
	p, err := cfgMgr.Path()
	if err != nil {
		return ""
	}
	return p
}

// Dir returns the full path to the config directory (~/.gmail/).
func Dir() string {
	d, err := cfgMgr.Dir()
	if err != nil {
		return ""
	}
	return d
}

// TokenPath returns the full path to a token file for the given account alias.
func TokenPath(alias string) string {
	return filepath.Join(Dir(), TokensDir, alias+".json")
}

// Load reads the configuration from ~/.gmail/config.
// Returns an empty Config (not an error) if the file doesn't exist.
// Environment variables GMAIL_CLIENT_ID and GMAIL_CLIENT_SECRET are applied
// as fallbacks when the config file is absent or does not set a value.
// Priority: config file > env var.
func Load() (*Config, error) {
	values, err := cfgMgr.LoadINI()
	if err != nil {
		return nil, fmt.Errorf("failed to open config file: %w", err)
	}

	cfg := &Config{
		ClientID:     values["client_id"],
		ClientSecret: values["client_secret"],
	}

	// Apply environment variable fallbacks; file takes priority over env vars.
	if cfg.ClientID == "" {
		cfg.ClientID = os.Getenv(EnvClientID)
	}
	if cfg.ClientSecret == "" {
		cfg.ClientSecret = os.Getenv(EnvClientSecret)
	}

	return cfg, nil
}

// Save writes the configuration to ~/.gmail/config with proper permissions.
func Save(cfg *Config) error {
	if err := cfgMgr.Ensure(); err != nil {
		return fmt.Errorf("failed to create config directory: %w", err)
	}
	path := Path()

	var b strings.Builder
	b.WriteString("# Gmail CLI Configuration\n")
	b.WriteString("# Created by: gmail configure\n")
	b.WriteString("\n")
	b.WriteString("# OAuth2 Client ID\n")
	b.WriteString("# Get from: https://console.cloud.google.com/apis/credentials\n")
	fmt.Fprintf(&b, "client_id=%s\n", cfg.ClientID)
	b.WriteString("\n")
	b.WriteString("# OAuth2 Client Secret\n")
	fmt.Fprintf(&b, "client_secret=%s\n", cfg.ClientSecret)

	if err := os.WriteFile(path, []byte(b.String()), 0600); err != nil {
		return fmt.Errorf("failed to write config file: %w", err)
	}
	return nil
}

// Exists returns true if the config file exists.
func Exists() bool {
	return Path() != "" && func() bool {
		_, err := os.Stat(Path())
		return err == nil
	}()
}

// Permissions returns the file permissions of the config file, or an error.
func Permissions() (os.FileMode, error) {
	path := Path()
	if path == "" {
		return 0, fmt.Errorf("cannot determine config path")
	}
	info, err := os.Stat(path)
	if err != nil {
		return 0, err
	}
	return info.Mode().Perm(), nil
}

// LoadAccounts reads the accounts registry from ~/.gmail/accounts.json.
// Returns an empty slice (not an error) if the file doesn't exist.
func LoadAccounts() ([]Account, error) {
	var accounts []Account
	if err := acctMgr.LoadJSON(&accounts); err != nil {
		return nil, fmt.Errorf("failed to read accounts file: %w", err)
	}
	if accounts == nil {
		accounts = []Account{}
	}
	return accounts, nil
}

// SaveAccounts writes the accounts registry to ~/.gmail/accounts.json.
func SaveAccounts(accounts []Account) error {
	if err := acctMgr.SaveJSON(accounts); err != nil {
		return fmt.Errorf("failed to write accounts file: %w", err)
	}
	return nil
}

// AddAccount registers a new account with the given alias and email.
// Returns an error if an account with that alias already exists.
func AddAccount(alias, email string) error {
	accounts, err := LoadAccounts()
	if err != nil {
		return err
	}

	for _, a := range accounts {
		if a.Alias == alias {
			return fmt.Errorf("account with alias %q already exists", alias)
		}
	}

	accounts = append(accounts, Account{
		Alias:   alias,
		Email:   email,
		AddedAt: time.Now().UTC().Format(time.RFC3339),
	})

	return SaveAccounts(accounts)
}

// RemoveAccount removes an account by alias and deletes its token file.
// Returns an error if the account does not exist.
func RemoveAccount(alias string) error {
	accounts, err := LoadAccounts()
	if err != nil {
		return err
	}

	found := false
	filtered := make([]Account, 0, len(accounts))
	for _, a := range accounts {
		if a.Alias == alias {
			found = true
			continue
		}
		filtered = append(filtered, a)
	}

	if !found {
		return fmt.Errorf("account with alias %q not found", alias)
	}

	// Remove the token file if it exists.
	if tp := TokenPath(alias); tp != "" {
		_ = os.Remove(tp)
	}

	return SaveAccounts(filtered)
}

// GetAccount returns the account with the given alias, or an error if not found.
func GetAccount(alias string) (*Account, error) {
	accounts, err := LoadAccounts()
	if err != nil {
		return nil, err
	}

	for _, a := range accounts {
		if a.Alias == alias {
			return &a, nil
		}
	}

	return nil, fmt.Errorf("account with alias %q not found", alias)
}

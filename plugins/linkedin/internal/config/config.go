package config

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	configDirName  = ".linkedin"
	configEnvVar   = "LINKEDIN_CONFIG_DIR"
	configFileName = "config"
)

// Config holds the LinkedIn CLI configuration.
type Config struct {
	// Official API (OAuth 2.0)
	ClientID     string
	ClientSecret string
	AccessToken  string
	TokenExpiry  string // RFC3339 format
	PersonURN    string // urn:li:person:XXXX

	// Chrome CDP (for feed/comment reading)
	ChromeDebugURL string // default: http://localhost:9222
}

// configDir returns the path to ~/.linkedin/ (or $LINKEDIN_CONFIG_DIR if set).
func configDir() (string, error) {
	if d := os.Getenv(configEnvVar); d != "" {
		return d, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	return filepath.Join(home, configDirName), nil
}

// BaseDir returns the path to ~/.linkedin/.
func BaseDir() (string, error) {
	return configDir()
}

// Path returns the path to ~/.linkedin/config.
func Path() (string, error) {
	dir, err := configDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, configFileName), nil
}

// Exists checks if the config file exists.
func Exists() bool {
	p, err := Path()
	if err != nil {
		return false
	}
	_, err = os.Stat(p)
	return err == nil
}

// Permissions returns the file mode of the config file.
func Permissions() (os.FileMode, error) {
	p, err := Path()
	if err != nil {
		return 0, err
	}
	info, err := os.Stat(p)
	if err != nil {
		return 0, fmt.Errorf("checking config permissions: %w", err)
	}
	return info.Mode().Perm(), nil
}

// Load reads ~/.linkedin/config and returns the parsed Config.
// Returns an empty Config (not error) if the file doesn't exist.
func Load() (*Config, error) {
	p, err := Path()
	if err != nil {
		return nil, err
	}

	f, err := os.Open(p)
	if err != nil {
		if os.IsNotExist(err) {
			return &Config{}, nil
		}
		return nil, fmt.Errorf("opening config: %w", err)
	}
	defer f.Close()

	values := make(map[string]string)
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 {
			continue
		}
		values[strings.TrimSpace(parts[0])] = strings.TrimSpace(parts[1])
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}

	return &Config{
		ClientID:       values["client_id"],
		ClientSecret:   values["client_secret"],
		AccessToken:    values["access_token"],
		TokenExpiry:    values["token_expiry"],
		PersonURN:      values["person_urn"],
		ChromeDebugURL: values["chrome_debug_url"],
	}, nil
}

// Save writes the Config to ~/.linkedin/config.
func Save(cfg *Config) error {
	dir, err := configDir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}

	p := filepath.Join(dir, configFileName)

	header := "# LinkedIn CLI Configuration\n# Created by: linkedin configure\n"
	keyOrder := []string{
		"client_id",
		"client_secret",
		"access_token",
		"token_expiry",
		"person_urn",
		"chrome_debug_url",
	}
	values := map[string]string{
		"client_id":        cfg.ClientID,
		"client_secret":    cfg.ClientSecret,
		"access_token":     cfg.AccessToken,
		"token_expiry":     cfg.TokenExpiry,
		"person_urn":       cfg.PersonURN,
		"chrome_debug_url": cfg.ChromeDebugURL,
	}

	var sb strings.Builder
	sb.WriteString(header)
	sb.WriteByte('\n')
	for _, key := range keyOrder {
		if val, ok := values[key]; ok {
			fmt.Fprintf(&sb, "%s=%s\n", key, val)
		}
	}

	if err := os.WriteFile(p, []byte(sb.String()), 0600); err != nil {
		return fmt.Errorf("writing config: %w", err)
	}
	return nil
}

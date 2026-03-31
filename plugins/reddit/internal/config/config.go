package config

import "os"

const (
	ConfigDirName  = ".reddit"
	ConfigFileName = "config"
)

var cfgStore = newStoreWithEnv(ConfigDirName, "REDDIT_CONFIG_DIR")

// Config holds the Reddit CLI configuration.
type Config struct {
	RedditSession string
	CSRFToken     string
	Username      string
}

// BaseDir returns the path to ~/.reddit/.
func BaseDir() (string, error) { return cfgStore.baseDir() }

// Path returns the path to ~/.reddit/config.
func Path() (string, error) { return cfgStore.path() }

// Exists checks if the config file exists.
func Exists() bool { return cfgStore.exists() }

// Permissions returns the file mode of the config file.
func Permissions() (os.FileMode, error) { return cfgStore.permissions() }

// Load reads ~/.reddit/config and returns the parsed Config.
func Load() (*Config, error) {
	values, err := cfgStore.load()
	if err != nil {
		return nil, err
	}
	return &Config{
		RedditSession: values["reddit_session"],
		CSRFToken:     values["csrf_token"],
		Username:      values["username"],
	}, nil
}

// Save writes the Config to ~/.reddit/config.
func Save(cfg *Config) error {
	values := map[string]string{
		"reddit_session": cfg.RedditSession,
		"csrf_token":     cfg.CSRFToken,
		"username":       cfg.Username,
	}
	header := "# Reddit CLI Configuration\n# Created by: reddit configure cookies"
	keyOrder := []string{"reddit_session", "csrf_token", "username"}
	return cfgStore.save(values, header, keyOrder)
}

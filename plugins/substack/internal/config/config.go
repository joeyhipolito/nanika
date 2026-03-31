package config

import "os"

const (
	ConfigDirName  = ".substack"
	ConfigFileName = "config"
)

var cfgStore = newStoreWithEnv(ConfigDirName, "SUBSTACK_CONFIG_DIR")

type Config struct {
	Cookie         string
	PublicationURL string
	Subdomain      string
	SiteURL        string
}

// BaseDir returns the config directory path (~/.substack).
func BaseDir() (string, error) {
	return cfgStore.baseDir()
}

// Path returns the full path to the config file.
func Path() (string, error) {
	return cfgStore.path()
}

// Exists returns true if the config file exists.
func Exists() bool {
	return cfgStore.exists()
}

// Permissions returns the file mode of the config file.
func Permissions() (os.FileMode, error) {
	return cfgStore.permissions()
}

// Load reads the config file and returns a Config.
func Load() (*Config, error) {
	values, err := cfgStore.load()
	if err != nil {
		return nil, err
	}
	return &Config{
		Cookie:         values["cookie"],
		PublicationURL: values["publication_url"],
		Subdomain:      values["subdomain"],
		SiteURL:        values["site_url"],
	}, nil
}

// Save writes the config to disk with secure permissions.
func Save(cfg *Config) error {
	values := map[string]string{
		"cookie":          cfg.Cookie,
		"publication_url": cfg.PublicationURL,
		"subdomain":       cfg.Subdomain,
	}
	if cfg.SiteURL != "" {
		values["site_url"] = cfg.SiteURL
	}

	header := "# Substack CLI Configuration\n# Created by: substack configure"
	keyOrder := []string{"cookie", "publication_url", "subdomain", "site_url"}
	return cfgStore.save(values, header, keyOrder)
}

package config

import "os"

const (
	ConfigDirName  = ".sobre"
	ConfigFileName = "config"
)

var cfgStore = newStoreWithEnv(ConfigDirName, "SOBRE_CONFIG_DIR")

type Config struct {
	DBPath string // Path to SQLite database
}

// BaseDir returns the config directory path (~/.sobre).
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
	cfg := &Config{}
	if dbPath, ok := values["db_path"]; ok {
		cfg.DBPath = dbPath
	}
	return cfg, nil
}

// Save writes the config to disk with secure permissions.
func Save(cfg *Config) error {
	if cfg.DBPath == "" {
		// Set default database path if not provided
		cfg.DBPath = os.ExpandEnv("$HOME/.sobre/db.sqlite")
	}

	values := map[string]string{
		"db_path": cfg.DBPath,
	}

	header := "# Sobre CLI Configuration\n# Created by: sobre configure"
	keyOrder := []string{"db_path"}
	return cfgStore.save(values, header, keyOrder)
}

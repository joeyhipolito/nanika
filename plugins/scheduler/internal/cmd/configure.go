package cmd

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
)

func newConfigureCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "configure",
		Short: "Interactive configuration setup",
		Long: `Walk through scheduler settings and write them to ~/.alluka/scheduler/config.

Existing values are shown as defaults; press Enter to keep them.`,
		RunE: func(cmd *cobra.Command, args []string) error {
			return runConfigure()
		},
	}

	showCmd := &cobra.Command{
		Use:   "show",
		Short: "Print the current configuration",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runConfigureShow()
		},
	}

	cmd.AddCommand(showCmd)
	return cmd
}

func runConfigure() error {
	reader := bufio.NewReader(os.Stdin)

	fmt.Println("scheduler Configuration")
	fmt.Println("===========================")
	fmt.Println()

	if config.Exists() {
		fmt.Printf("Existing configuration found at %s\n", config.Path())
		fmt.Print("Overwrite? [y/N] ")
		reply, _ := reader.ReadString('\n')
		if !strings.EqualFold(strings.TrimSpace(reply), "y") {
			fmt.Println("Configuration cancelled.")
			return nil
		}
		fmt.Println()
	}

	existing, _ := config.Load()

	// DB path
	fmt.Printf("Database path [%s]: ", existing.DBPath)
	dbPath, _ := reader.ReadString('\n')
	dbPath = strings.TrimSpace(dbPath)
	if dbPath == "" {
		dbPath = existing.DBPath
	}

	// Shell
	fmt.Printf("Shell [%s]: ", existing.Shell)
	shell, _ := reader.ReadString('\n')
	shell = strings.TrimSpace(shell)
	if shell == "" {
		shell = existing.Shell
	}

	// Log level
	fmt.Printf("Log level (debug/info/warn/error) [%s]: ", existing.LogLevel)
	logLevel, _ := reader.ReadString('\n')
	logLevel = strings.TrimSpace(logLevel)
	if logLevel == "" {
		logLevel = existing.LogLevel
	}
	switch logLevel {
	case "debug", "info", "warn", "error":
		// valid
	default:
		return fmt.Errorf("invalid log level %q: must be debug, info, warn, or error", logLevel)
	}

	// Max concurrent
	fmt.Printf("Max concurrent jobs [%d]: ", existing.MaxConcurrent)
	maxStr, _ := reader.ReadString('\n')
	maxStr = strings.TrimSpace(maxStr)
	maxConcurrent := existing.MaxConcurrent
	if maxStr != "" {
		n, err := strconv.Atoi(maxStr)
		if err != nil || n < 0 {
			return fmt.Errorf("max_concurrent must be a non-negative integer, got %q", maxStr)
		}
		maxConcurrent = n
	}

	cfg := &config.Config{
		DBPath:        dbPath,
		Shell:         shell,
		LogLevel:      logLevel,
		MaxConcurrent: maxConcurrent,
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("saving configuration: %w", err)
	}

	fmt.Println()
	fmt.Printf("Configuration saved to %s\n", config.Path())
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  scheduler doctor        # validate setup")
	return nil
}

func runConfigureShow() error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	if !config.Exists() {
		fmt.Printf("No configuration file found at %s\n", config.Path())
		fmt.Println("Run 'scheduler configure' to create one.")
		return nil
	}

	fmt.Printf("Config file:      %s\n", config.Path())
	fmt.Printf("Database path:    %s\n", cfg.DBPath)
	fmt.Printf("Shell:            %s\n", cfg.Shell)
	fmt.Printf("Log level:        %s\n", cfg.LogLevel)
	fmt.Printf("Max concurrent:   %d\n", cfg.MaxConcurrent)
	return nil
}

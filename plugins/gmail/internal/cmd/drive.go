package cmd

import (
	"fmt"
	"strings"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// DriveListCmd handles "gmail drive list [--limit N] --account <alias> [--json]".
func DriveListCmd(cfg *config.Config, account string, limit int, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	files, err := client.ListDriveFiles(limit)
	if err != nil {
		return fmt.Errorf("list drive files: %w", err)
	}

	if len(files) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No files found.")
		return nil
	}

	if jsonOutput {
		return printJSON(files)
	}
	printDriveFiles(files)
	return nil
}

// DriveSearchCmd handles "gmail drive search <query> [--limit N] --account <alias> [--json]".
func DriveSearchCmd(cfg *config.Config, account, query string, limit int, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	files, err := client.SearchDriveFiles(query, limit)
	if err != nil {
		return fmt.Errorf("search drive files: %w", err)
	}

	if len(files) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Printf("No files matching %q.\n", query)
		return nil
	}

	if jsonOutput {
		return printJSON(files)
	}
	printDriveFiles(files)
	return nil
}

// DriveDownloadCmd handles "gmail drive download <file-id> [--output <path>] --account <alias> [--json]".
func DriveDownloadCmd(cfg *config.Config, account, fileID, outputPath string, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	saved, err := client.DownloadDriveFile(fileID, outputPath)
	if err != nil {
		return fmt.Errorf("download file: %w", err)
	}

	if jsonOutput {
		return printJSON(struct {
			FileID  string `json:"file_id"`
			SavedTo string `json:"saved_to"`
		}{FileID: fileID, SavedTo: saved})
	}
	fmt.Printf("Saved to %s\n", saved)
	return nil
}

func printDriveFiles(files []api.DriveFile) {
	for _, f := range files {
		size := formatDriveSize(f.Size)
		fmt.Printf("[%s] %s", f.ModifiedTime, f.Name)
		if size != "" {
			fmt.Printf("  %s", size)
		}
		if len(f.Owners) > 0 {
			fmt.Printf("  (%s)", strings.Join(f.Owners, ", "))
		}
		fmt.Println()
	}
}

func formatDriveSize(bytes int64) string {
	switch {
	case bytes == 0:
		return ""
	case bytes < 1024:
		return fmt.Sprintf("%d B", bytes)
	case bytes < 1024*1024:
		return fmt.Sprintf("%.1f KB", float64(bytes)/1024)
	case bytes < 1024*1024*1024:
		return fmt.Sprintf("%.1f MB", float64(bytes)/(1024*1024))
	default:
		return fmt.Sprintf("%.1f GB", float64(bytes)/(1024*1024*1024))
	}
}

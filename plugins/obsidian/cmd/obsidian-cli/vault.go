// vault.go — RFC §7 Phase 0 (TRK-524): `vault init` + `vault doctor` subcommands.
package main

import (
	"errors"
	"flag"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// errUsage is returned by subcommand handlers when arguments are invalid.
// main() detects it via errors.Is and exits with code 2.
var errUsage = errors.New("usage error")

// handleVaultCommand dispatches vault subcommands.
func handleVaultCommand(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("%w: vault requires a subcommand (init|doctor)", errUsage)
	}
	switch args[0] {
	case "init":
		return handleVaultInit(args[1:])
	case "doctor":
		return handleVaultDoctor(args[1:])
	default:
		return fmt.Errorf("%w: unknown vault subcommand: %s", errUsage, args[0])
	}
}

func handleVaultInit(args []string) error {
	fs := flag.NewFlagSet("vault init", flag.ContinueOnError)
	typeFlag := fs.String("type", "nanika", "vault type (nanika|second-brain)")
	pathFlag := fs.String("path", "", "vault path")

	if err := fs.Parse(args); err != nil {
		return fmt.Errorf("%w: %v", errUsage, err)
	}

	vaultPath := *pathFlag
	if vaultPath == "" {
		vaultPath = config.ResolveVaultPath()
	}
	if vaultPath == "" {
		return fmt.Errorf("%w: --path is required (or set vault_path in config)", errUsage)
	}

	var kind vault.VaultKind
	switch *typeFlag {
	case "nanika":
		kind = vault.KindNanika
	case "second-brain":
		kind = vault.KindSecondBrain
	default:
		return fmt.Errorf("%w: unknown --type %q (valid: nanika, second-brain)", errUsage, *typeFlag)
	}

	if err := vault.InitSkeleton(vaultPath, kind); err != nil {
		return fmt.Errorf("vault init: %w", err)
	}

	fmt.Fprintf(os.Stdout, "Vault initialized at %s\n", vaultPath)
	return nil
}

func handleVaultDoctor(args []string) error {
	fs := flag.NewFlagSet("vault doctor", flag.ContinueOnError)
	pathFlag := fs.String("path", "", "vault path")
	formatFlag := fs.String("format", "text", "output format (text|json)")
	allFlag := fs.Bool("all", false, "run all checks: missing frontmatter, orphans, oversized files, duplicate IDs")

	if err := fs.Parse(args); err != nil {
		return fmt.Errorf("%w: %v", errUsage, err)
	}

	vaultPath := *pathFlag
	// Accept positional path when --path is omitted (e.g. vault doctor --all <path>).
	if vaultPath == "" && len(fs.Args()) > 0 {
		vaultPath = fs.Args()[0]
	}
	if vaultPath == "" {
		return fmt.Errorf("%w: vault path is required (--path <path> or positional argument)", errUsage)
	}

	var report vault.Report
	var err error
	if *allFlag {
		report, err = vault.DoctorAll(vaultPath)
	} else {
		report, err = vault.Doctor(vaultPath)
	}
	if err != nil {
		return fmt.Errorf("vault doctor: %w", err)
	}

	total := report.OrphanCount + report.DanglingCount + len(report.InvariantViolations) + len(report.Issues)

	if *formatFlag == "json" {
		if err := output.JSON(report); err != nil {
			return err
		}
		if total > 0 {
			return fmt.Errorf("vault doctor: %d issue(s) found", total)
		}
		return nil
	}

	if total == 0 {
		fmt.Fprintln(os.Stdout, "Vault OK: no issues found")
	} else {
		for _, iv := range report.InvariantViolations {
			fmt.Fprintf(os.Stdout, "INVARIANT: %s\n", iv)
		}
		for _, issue := range report.Issues {
			fmt.Fprintf(os.Stdout, "ISSUE: %s\n", issue)
		}
		if report.DanglingCount > 0 {
			fmt.Fprintf(os.Stdout, "Dangling links: %d\n", report.DanglingCount)
		}
		if report.OrphanCount > 0 {
			fmt.Fprintf(os.Stdout, "Orphan notes: %d\n", report.OrphanCount)
		}
	}

	if total > 0 {
		return fmt.Errorf("vault doctor: %d issue(s) found", total)
	}
	return nil
}

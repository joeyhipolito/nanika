package cmd

import (
	"flag"
	"fmt"
	"os"
	"sort"

	engageclaude "github.com/joeyhipolito/nanika-engage/internal/claude"
	"github.com/joeyhipolito/nanika-engage/internal/post"
)

// DoctorCmd checks that all required platform CLIs and the claude CLI are available.
func DoctorCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("doctor", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: engage doctor\n\nCheck that all required platform CLIs are available.\n")
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	allOK := true

	// Check platform posting CLIs in alphabetical order for consistent output.
	platforms := make([]string, 0, len(post.PlatformCLIs))
	for p := range post.PlatformCLIs {
		platforms = append(platforms, p)
	}
	sort.Strings(platforms)

	for _, platform := range platforms {
		cli := post.PlatformCLIs[platform]
		if err := post.CheckCLI(cli); err != nil {
			fmt.Printf("  [missing] %s (%s)\n", platform, cli)
			allOK = false
		} else {
			fmt.Printf("  [ok]      %s (%s)\n", platform, cli)
		}
	}

	// Check claude CLI separately — required for drafting, not posting.
	if engageclaude.Available() {
		fmt.Printf("  [ok]      claude (drafting)\n")
	} else {
		fmt.Printf("  [missing] claude — required for 'engage draft'\n")
		allOK = false
	}

	fmt.Println()
	if !allOK {
		return fmt.Errorf("one or more CLIs are missing — install them and re-run 'engage doctor'")
	}
	fmt.Println("All CLIs available.")
	return nil
}

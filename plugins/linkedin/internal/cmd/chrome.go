package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
)

// ChromeCmd prints or launches the Chrome command for remote debugging.
func ChromeCmd(args []string) error {
	launch := false
	for _, arg := range args {
		if arg == "--launch" {
			launch = true
		}
	}

	chromePath := findChromePath()

	if !launch {
		fmt.Println("To use feed reading, launch Chrome with remote debugging:")
		fmt.Println()
		fmt.Printf("  %s \\\n", chromePath)
		fmt.Println("    --remote-debugging-port=9222 \\")
		fmt.Println("    --user-data-dir=~/.chrome-linkedin")
		fmt.Println()
		fmt.Println("Then log into LinkedIn in that browser window.")
		fmt.Println("Your session persists across restarts (stored in ~/.chrome-linkedin).")
		fmt.Println()
		fmt.Println("Or launch automatically with: linkedin chrome --launch")
		return nil
	}

	// Launch Chrome
	fmt.Printf("Launching Chrome with remote debugging on port 9222...\n")

	home, _ := os.UserHomeDir()
	userDataDir := home + "/.chrome-linkedin"

	cmd := exec.Command(chromePath,
		"--remote-debugging-port=9222",
		"--user-data-dir="+userDataDir,
	)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("failed to launch Chrome: %w\n\nIs Chrome installed at %s?", err, chromePath)
	}

	fmt.Printf("Chrome launched (PID %d)\n", cmd.Process.Pid)
	fmt.Println("Log into LinkedIn in the browser window, then run: linkedin doctor")

	// Detach — don't wait for Chrome to exit
	cmd.Process.Release()
	return nil
}

func findChromePath() string {
	switch runtime.GOOS {
	case "darwin":
		return "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
	default:
		// Try common Linux paths
		if path, err := exec.LookPath("google-chrome"); err == nil {
			return path
		}
		if path, err := exec.LookPath("google-chrome-stable"); err == nil {
			return path
		}
		if path, err := exec.LookPath("chromium-browser"); err == nil {
			return path
		}
		return "google-chrome"
	}
}

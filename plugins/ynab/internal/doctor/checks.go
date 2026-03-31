package doctor

import (
	"fmt"
	"os"
	"os/exec"
)

// CheckFunc is a function that performs a single diagnostic check.
// Return a Check with Status == "" to indicate the check should be skipped.
type CheckFunc func() Check

// BinaryInPath returns a CheckFunc that verifies `name` is found in PATH.
func BinaryInPath(name string) CheckFunc {
	return func() Check {
		path, err := exec.LookPath(name)
		if err != nil {
			return Check{
				Name:    "Binary",
				Status:  "warn",
				Message: name + " not found in PATH (running from local build?)",
			}
		}
		return Check{Name: "Binary", Status: "ok", Message: path}
	}
}

// ConfigExists returns a CheckFunc that verifies the config file exists at configPath.
func ConfigExists(configPath, configureCmd string) CheckFunc {
	return func() Check {
		if _, err := os.Stat(configPath); os.IsNotExist(err) {
			return Check{
				Name:    "Config file",
				Status:  "fail",
				Message: fmt.Sprintf("%s not found. Run '%s'", configPath, configureCmd),
			}
		}
		return Check{Name: "Config file", Status: "ok", Message: configPath}
	}
}

// ConfigPermissions returns a CheckFunc that verifies configPath has 0600 permissions.
func ConfigPermissions(configPath string) CheckFunc {
	return func() Check {
		info, err := os.Stat(configPath)
		if err != nil {
			if os.IsNotExist(err) {
				return Check{} // skip — ConfigExists already reported this
			}
			return Check{
				Name:    "Config permissions",
				Status:  "fail",
				Message: fmt.Sprintf("cannot read permissions: %v", err),
			}
		}
		perms := info.Mode().Perm()
		if perms != 0600 {
			return Check{
				Name:    "Config permissions",
				Status:  "warn",
				Message: fmt.Sprintf("%o (should be 600). Fix: chmod 600 %s", perms, configPath),
			}
		}
		return Check{Name: "Config permissions", Status: "ok", Message: "600 (secure)"}
	}
}

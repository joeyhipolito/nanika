package worker

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// sharedDenyRules are enforced for every worker role. They prevent actions
// that the orchestrator must exclusively control: git branch lifecycle,
// GitHub PR operations, and destructive filesystem commands.
var sharedDenyRules = []string{
	// Git operations — orchestrator owns the branch lifecycle
	"Bash(git push)",
	"Bash(git checkout -b)",
	"Bash(git branch -D)",
	"Bash(git branch -d)",
	"Bash(git reset --hard)",
	"Bash(git merge)",
	"Bash(git rebase)",
	"Bash(git stash drop)",
	// GitHub CLI — orchestrator owns PR lifecycle
	"Bash(gh pr create)",
	"Bash(gh pr merge)",
	"Bash(gh pr close)",
	"Bash(gh issue)",
	// Destructive
	"Bash(rm -rf /)",
	"Bash(rm -rf ~)",
}

// DenyRulesForRole returns the permissions.deny rules for the given worker role.
// Planner and reviewer roles get the shared rules plus Edit (they produce new
// artifacts via Write but must not modify existing code). Implementer and
// unclassified roles get only the shared rules.
func DenyRulesForRole(role core.Role) []string {
	switch role {
	case core.RolePlanner, core.RoleReviewer:
		rules := make([]string, len(sharedDenyRules), len(sharedDenyRules)+1)
		copy(rules, sharedDenyRules)
		return append(rules, "Edit")
	default:
		rules := make([]string, len(sharedDenyRules))
		copy(rules, sharedDenyRules)
		return rules
	}
}

// settingsLocal is the top-level structure for settings.local.json.
type settingsLocal struct {
	Permissions settingsPermissions `json:"permissions"`
}

// settingsPermissions holds the allow and deny rule arrays.
// Allow is omitted from the JSON when empty so existing callers that pass nil
// get the same output as before this field was added.
type settingsPermissions struct {
	Allow []string `json:"allow,omitempty"`
	Deny  []string `json:"deny"`
}

// buildSettingsLocal serializes deny and allow rules into a settings.local.json payload.
// Pass nil for allowRules to omit the allow list from the output.
func buildSettingsLocal(denyRules, allowRules []string) ([]byte, error) {
	s := settingsLocal{
		Permissions: settingsPermissions{Allow: allowRules, Deny: denyRules},
	}
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("marshal settings.local.json: %w", err)
	}
	return data, nil
}

// writeSettingsFile writes deny-rule settings.local.json into the given .claude directory.
// The directory must already exist.
func writeSettingsFile(claudeDir string, settingsJSON []byte) error {
	settingsPath := filepath.Join(claudeDir, "settings.local.json")
	if err := os.WriteFile(settingsPath, settingsJSON, 0600); err != nil {
		return fmt.Errorf("write settings.local.json to %s: %w", claudeDir, err)
	}
	return nil
}

// backupSuffix is appended to settings.local.json when backing up an existing
// file in a target repo before overwriting with deny rules.
const backupSuffix = ".orchestrator-backup"

// writeTargetSettings writes settings.local.json into {targetDir}/.claude/,
// backing up any existing file so it can be restored after execution.
func writeTargetSettings(targetDir string, settingsJSON []byte) error {
	claudeDir := filepath.Join(targetDir, ".claude")
	if err := os.MkdirAll(claudeDir, 0700); err != nil {
		return fmt.Errorf("create .claude dir in target: %w", err)
	}

	settingsPath := filepath.Join(claudeDir, "settings.local.json")

	// Back up existing settings.local.json if present
	if data, err := os.ReadFile(settingsPath); err == nil {
		backupPath := settingsPath + backupSuffix
		if err := os.WriteFile(backupPath, data, 0600); err != nil {
			return fmt.Errorf("backup existing settings.local.json: %w", err)
		}
	}

	return writeSettingsFile(claudeDir, settingsJSON)
}

// cleanupTargetSettings removes the orchestrator-written settings.local.json
// from a target repo and restores any backup that was created before execution.
func cleanupTargetSettings(targetDir string) {
	settingsPath := filepath.Join(targetDir, ".claude", "settings.local.json")
	backupPath := settingsPath + backupSuffix
	os.Remove(settingsPath)
	// Restore backup if it existed before the orchestrator wrote deny rules.
	if _, err := os.Stat(backupPath); err == nil {
		os.Rename(backupPath, settingsPath)
	}
}

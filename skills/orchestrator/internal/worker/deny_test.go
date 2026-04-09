package worker

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// ---------------------------------------------------------------------------
// DenyRulesForRole: per-role deny rule sets
// ---------------------------------------------------------------------------

func TestDenyRulesForRole_SharedRulesPresent(t *testing.T) {
	// Every role must include all shared deny rules.
	roles := []core.Role{core.RolePlanner, core.RoleImplementer, core.RoleReviewer, ""}
	for _, role := range roles {
		t.Run(string(role), func(t *testing.T) {
			rules := DenyRulesForRole(role)
			ruleSet := toSet(rules)
			for _, shared := range sharedDenyRules {
				if !ruleSet[shared] {
					t.Errorf("role %q missing shared rule %q", role, shared)
				}
			}
		})
	}
}

func TestDenyRulesForRole_Planner(t *testing.T) {
	rules := DenyRulesForRole(core.RolePlanner)
	ruleSet := toSet(rules)
	if !ruleSet["Edit"] {
		t.Error("planner must deny Edit")
	}
	if len(rules) != len(sharedDenyRules)+1 {
		t.Errorf("planner rule count = %d; want %d (shared + Edit)", len(rules), len(sharedDenyRules)+1)
	}
}

func TestDenyRulesForRole_Reviewer(t *testing.T) {
	rules := DenyRulesForRole(core.RoleReviewer)
	ruleSet := toSet(rules)
	if !ruleSet["Edit"] {
		t.Error("reviewer must deny Edit")
	}
	if len(rules) != len(sharedDenyRules)+1 {
		t.Errorf("reviewer rule count = %d; want %d (shared + Edit)", len(rules), len(sharedDenyRules)+1)
	}
}

func TestDenyRulesForRole_Implementer(t *testing.T) {
	rules := DenyRulesForRole(core.RoleImplementer)
	ruleSet := toSet(rules)
	if ruleSet["Edit"] {
		t.Error("implementer must NOT deny Edit")
	}
	if len(rules) != len(sharedDenyRules) {
		t.Errorf("implementer rule count = %d; want %d (shared only)", len(rules), len(sharedDenyRules))
	}
}

func TestDenyRulesForRole_Default(t *testing.T) {
	rules := DenyRulesForRole("")
	ruleSet := toSet(rules)
	if ruleSet["Edit"] {
		t.Error("default role must NOT deny Edit")
	}
	if len(rules) != len(sharedDenyRules) {
		t.Errorf("default rule count = %d; want %d (shared only)", len(rules), len(sharedDenyRules))
	}
}

func TestDenyRulesForRole_NoCrossTalk(t *testing.T) {
	// Calling DenyRulesForRole for one role must not mutate the result for another.
	// This guards against the append-to-shared-slice bug (GOTCHA from design doc).
	_ = DenyRulesForRole(core.RolePlanner)
	implRules := DenyRulesForRole(core.RoleImplementer)
	ruleSet := toSet(implRules)
	if ruleSet["Edit"] {
		t.Error("implementer rules contaminated by prior planner call — shared slice mutation detected")
	}
}

// ---------------------------------------------------------------------------
// buildSettingsLocal: JSON structure
// ---------------------------------------------------------------------------

func TestBuildSettingsLocal_Structure(t *testing.T) {
	rules := []string{"Bash(git push)", "Edit"}
	data, err := buildSettingsLocal(rules, nil)
	if err != nil {
		t.Fatalf("buildSettingsLocal: %v", err)
	}

	// Unmarshal into map[string]any to catch struct tag typos — if json tags
	// were wrong, the struct round-trip would still pass but the raw keys would differ.
	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal raw: %v", err)
	}
	perms, ok := raw["permissions"].(map[string]any)
	if !ok {
		t.Fatalf("missing top-level 'permissions' key; got keys: %v", mapKeys(raw))
	}
	deny, ok := perms["deny"].([]any)
	if !ok {
		t.Fatalf("missing 'deny' key under permissions; got keys: %v", mapKeys(perms))
	}
	if len(deny) != 2 {
		t.Fatalf("deny count = %d; want 2", len(deny))
	}
}

func TestBuildSettingsLocal_ValidJSON(t *testing.T) {
	// Verify each role produces valid JSON.
	for _, role := range []core.Role{core.RolePlanner, core.RoleImplementer, core.RoleReviewer, ""} {
		t.Run(string(role), func(t *testing.T) {
			rules := DenyRulesForRole(role)
			data, err := buildSettingsLocal(rules, nil)
			if err != nil {
				t.Fatalf("buildSettingsLocal: %v", err)
			}
			if !json.Valid(data) {
				t.Error("produced invalid JSON")
			}
		})
	}
}

func TestBuildSettingsLocal_AllowListPresent(t *testing.T) {
	deny := []string{"Bash(git push)"}
	allow := []string{"Read", "Glob"}
	data, err := buildSettingsLocal(deny, allow)
	if err != nil {
		t.Fatalf("buildSettingsLocal: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	perms, ok := raw["permissions"].(map[string]any)
	if !ok {
		t.Fatalf("missing 'permissions' key")
	}
	allowList, ok := perms["allow"].([]any)
	if !ok {
		t.Fatalf("missing 'allow' key under permissions; got keys: %v", mapKeys(perms))
	}
	if len(allowList) != 2 {
		t.Errorf("allow count = %d; want 2", len(allowList))
	}
}

func TestBuildSettingsLocal_AllowListOmittedWhenNil(t *testing.T) {
	data, err := buildSettingsLocal([]string{"Edit"}, nil)
	if err != nil {
		t.Fatalf("buildSettingsLocal: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	perms := raw["permissions"].(map[string]any)
	if _, exists := perms["allow"]; exists {
		t.Error("'allow' key must be absent when allowRules is nil")
	}
}

func TestBuildSettingsLocal_LowRiskToolsIncluded(t *testing.T) {
	// Verify that the integration path (deny + LowRiskTools) produces valid JSON
	// with all expected allow entries.
	deny := DenyRulesForRole(core.RoleImplementer)
	data, err := buildSettingsLocal(deny, LowRiskTools())
	if err != nil {
		t.Fatalf("buildSettingsLocal: %v", err)
	}
	if !json.Valid(data) {
		t.Fatal("produced invalid JSON")
	}

	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	perms := raw["permissions"].(map[string]any)
	allowList, ok := perms["allow"].([]any)
	if !ok {
		t.Fatal("'allow' key missing when LowRiskTools passed")
	}
	if len(allowList) != len(LowRiskTools()) {
		t.Errorf("allow count = %d; want %d", len(allowList), len(LowRiskTools()))
	}
}

// ---------------------------------------------------------------------------
// writeSettingsFile: writes to .claude/settings.local.json
// ---------------------------------------------------------------------------

func TestWriteSettingsFile(t *testing.T) {
	claudeDir := filepath.Join(t.TempDir(), ".claude")
	if err := os.MkdirAll(claudeDir, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	payload := []byte(`{"permissions":{"deny":["Edit"]}}`)
	if err := writeSettingsFile(claudeDir, payload); err != nil {
		t.Fatalf("writeSettingsFile: %v", err)
	}

	got, err := os.ReadFile(filepath.Join(claudeDir, "settings.local.json"))
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if string(got) != string(payload) {
		t.Errorf("file content = %q; want %q", got, payload)
	}
}

// ---------------------------------------------------------------------------
// writeTargetSettings / cleanupTargetSettings: backup and restore
// ---------------------------------------------------------------------------

func TestWriteTargetSettings_NoExisting(t *testing.T) {
	targetDir := t.TempDir()
	payload := []byte(`{"permissions":{"deny":["Bash(git push)"]}}`)

	if err := writeTargetSettings(targetDir, payload); err != nil {
		t.Fatalf("writeTargetSettings: %v", err)
	}

	settingsPath := filepath.Join(targetDir, ".claude", "settings.local.json")
	got, err := os.ReadFile(settingsPath)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if string(got) != string(payload) {
		t.Errorf("content = %q; want %q", got, payload)
	}

	// No backup should exist.
	backupPath := settingsPath + backupSuffix
	if _, err := os.Stat(backupPath); err == nil {
		t.Error("backup file should not exist when there was no pre-existing settings")
	}
}

func TestWriteTargetSettings_BackupsExisting(t *testing.T) {
	targetDir := t.TempDir()
	claudeDir := filepath.Join(targetDir, ".claude")
	if err := os.MkdirAll(claudeDir, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	original := []byte(`{"permissions":{"allow":["*"]}}`)
	settingsPath := filepath.Join(claudeDir, "settings.local.json")
	if err := os.WriteFile(settingsPath, original, 0600); err != nil {
		t.Fatalf("write original: %v", err)
	}

	payload := []byte(`{"permissions":{"deny":["Edit"]}}`)
	if err := writeTargetSettings(targetDir, payload); err != nil {
		t.Fatalf("writeTargetSettings: %v", err)
	}

	// Settings should now contain deny rules.
	got, err := os.ReadFile(settingsPath)
	if err != nil {
		t.Fatalf("read settings: %v", err)
	}
	if string(got) != string(payload) {
		t.Errorf("settings = %q; want deny payload", got)
	}

	// Backup should contain original content.
	backupPath := settingsPath + backupSuffix
	backup, err := os.ReadFile(backupPath)
	if err != nil {
		t.Fatalf("read backup: %v", err)
	}
	if string(backup) != string(original) {
		t.Errorf("backup = %q; want original content", backup)
	}
}

func TestCleanupTargetSettings_RestoresBackup(t *testing.T) {
	targetDir := t.TempDir()
	claudeDir := filepath.Join(targetDir, ".claude")
	if err := os.MkdirAll(claudeDir, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	original := []byte(`{"permissions":{"allow":["*"]}}`)
	settingsPath := filepath.Join(claudeDir, "settings.local.json")
	if err := os.WriteFile(settingsPath, original, 0600); err != nil {
		t.Fatalf("write original: %v", err)
	}

	// Simulate writeTargetSettings then cleanupTargetSettings.
	payload := []byte(`{"permissions":{"deny":["Edit"]}}`)
	if err := writeTargetSettings(targetDir, payload); err != nil {
		t.Fatalf("writeTargetSettings: %v", err)
	}

	cleanupTargetSettings(targetDir)

	// Settings should be restored to original.
	got, err := os.ReadFile(settingsPath)
	if err != nil {
		t.Fatalf("read after cleanup: %v", err)
	}
	if string(got) != string(original) {
		t.Errorf("after cleanup = %q; want original content %q", got, original)
	}

	// Backup file should be gone.
	backupPath := settingsPath + backupSuffix
	if _, err := os.Stat(backupPath); err == nil {
		t.Error("backup file should be removed after cleanup")
	}
}

func TestCleanupTargetSettings_RemovesWhenNoBackup(t *testing.T) {
	targetDir := t.TempDir()
	claudeDir := filepath.Join(targetDir, ".claude")
	if err := os.MkdirAll(claudeDir, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	settingsPath := filepath.Join(claudeDir, "settings.local.json")
	if err := os.WriteFile(settingsPath, []byte(`{"permissions":{"deny":["Edit"]}}`), 0600); err != nil {
		t.Fatalf("write: %v", err)
	}

	cleanupTargetSettings(targetDir)

	if _, err := os.Stat(settingsPath); err == nil {
		t.Error("settings.local.json should be removed when no backup exists")
	}
}

// ---------------------------------------------------------------------------
// Shared deny rules content checks
// ---------------------------------------------------------------------------

func TestSharedDenyRules_ContainsExpectedRules(t *testing.T) {
	expected := []string{
		"Bash(git push)",
		"Bash(git checkout -b)",
		"Bash(git branch -D)",
		"Bash(git branch -d)",
		"Bash(git reset --hard)",
		"Bash(git merge)",
		"Bash(git rebase)",
		"Bash(git stash drop)",
		"Bash(gh pr create)",
		"Bash(gh pr merge)",
		"Bash(gh pr close)",
		"Bash(gh issue)",
		"Bash(rm -rf /)",
		"Bash(rm -rf ~)",
	}

	ruleSet := toSet(sharedDenyRules)
	for _, rule := range expected {
		if !ruleSet[rule] {
			t.Errorf("shared rules missing %q", rule)
		}
	}
	if len(sharedDenyRules) != len(expected) {
		t.Errorf("shared rules count = %d; want %d", len(sharedDenyRules), len(expected))
	}
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

func toSet(ss []string) map[string]bool {
	m := make(map[string]bool, len(ss))
	for _, s := range ss {
		m[s] = true
	}
	return m
}

func mapKeys(m map[string]any) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	return keys
}

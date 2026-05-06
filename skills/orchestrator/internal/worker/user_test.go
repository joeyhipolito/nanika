package worker

import (
	"os"
	"testing"
	"time"
)

func TestUserProfileParseAndFormat(t *testing.T) {
	// Create a test profile
	now := time.Now()
	profile := &UserProfile{
		Name:                 "Joey Hipolito",
		Role:                 "Full-stack engineer",
		Preferences:          []string{"async communication", "written documentation"},
		CommunicationStyle:   []string{"direct", "practical"},
		OngoingProjects:      []string{"nanika orchestrator", "hermes parity"},
		UpdatedAt:            now,
		OptedInForAutoUpdate: true,
	}

	// Format to markdown
	content := formatUserProfile(profile)

	// Parse it back
	parsed := &UserProfile{}
	if err := parseUserProfile(content, parsed); err != nil {
		t.Fatalf("parseUserProfile: %v", err)
	}

	// Verify roundtrip
	if parsed.Name != profile.Name {
		t.Errorf("Name: got %q, want %q", parsed.Name, profile.Name)
	}
	if parsed.Role != profile.Role {
		t.Errorf("Role: got %q, want %q", parsed.Role, profile.Role)
	}
	if len(parsed.Preferences) != len(profile.Preferences) {
		t.Errorf("Preferences length: got %d, want %d", len(parsed.Preferences), len(profile.Preferences))
	}
	for i, pref := range parsed.Preferences {
		if pref != profile.Preferences[i] {
			t.Errorf("Preferences[%d]: got %q, want %q", i, pref, profile.Preferences[i])
		}
	}
	if len(parsed.CommunicationStyle) != len(profile.CommunicationStyle) {
		t.Errorf("CommunicationStyle length: got %d, want %d",
			len(parsed.CommunicationStyle), len(profile.CommunicationStyle))
	}
	if len(parsed.OngoingProjects) != len(profile.OngoingProjects) {
		t.Errorf("OngoingProjects length: got %d, want %d",
			len(parsed.OngoingProjects), len(profile.OngoingProjects))
	}
	if parsed.OptedInForAutoUpdate != profile.OptedInForAutoUpdate {
		t.Errorf("OptedInForAutoUpdate: got %v, want %v",
			parsed.OptedInForAutoUpdate, profile.OptedInForAutoUpdate)
	}
}

func TestUserProfileSaveAndLoad(t *testing.T) {
	tmpDir := t.TempDir()

	// Create a test profile
	profile := &UserProfile{
		Name:                 "Test User",
		Role:                 "Tester",
		Preferences:          []string{"test preference 1", "test preference 2"},
		CommunicationStyle:   []string{"test style 1"},
		OngoingProjects:      []string{"test project 1", "test project 2", "test project 3"},
		UpdatedAt:            time.Now(),
		OptedInForAutoUpdate: true,
	}

	// Save to temp directory
	if err := SaveUserProfile(tmpDir, profile); err != nil {
		t.Fatalf("SaveUserProfile: %v", err)
	}

	// Verify file was created
	path, err := userProfilePath(tmpDir)
	if err != nil {
		t.Fatalf("userProfilePath: %v", err)
	}

	if _, err := os.Stat(path); err != nil {
		t.Fatalf("USER.md file not created: %v", err)
	}

	// Load it back
	loaded, err := LoadUserProfile(tmpDir)
	if err != nil {
		t.Fatalf("LoadUserProfile: %v", err)
	}

	// Verify
	if loaded.Name != profile.Name {
		t.Errorf("Name: got %q, want %q", loaded.Name, profile.Name)
	}
	if loaded.Role != profile.Role {
		t.Errorf("Role: got %q, want %q", loaded.Role, profile.Role)
	}
	if len(loaded.Preferences) != len(profile.Preferences) {
		t.Errorf("Preferences length: got %d, want %d", len(loaded.Preferences), len(profile.Preferences))
	}
	if loaded.OptedInForAutoUpdate != profile.OptedInForAutoUpdate {
		t.Errorf("OptedInForAutoUpdate: got %v, want %v", loaded.OptedInForAutoUpdate, profile.OptedInForAutoUpdate)
	}
}

func TestUserProfileEmptyProfile(t *testing.T) {
	tmpDir := t.TempDir()

	// Load from non-existent directory should return default profile
	profile, err := LoadUserProfile(tmpDir)
	if err != nil {
		t.Fatalf("LoadUserProfile on non-existent: %v", err)
	}

	if profile == nil {
		t.Fatal("got nil profile, want default")
	}
	if profile.OptedInForAutoUpdate {
		t.Errorf("default OptedInForAutoUpdate should be false, got true")
	}
}

func TestNudgeUserProfile(t *testing.T) {
	profile := &UserProfile{
		Name: "Test User",
		Role: "Engineer",
	}

	prompt := NudgeUserProfile(profile, 24)
	if len(prompt) == 0 {
		t.Fatalf("NudgeUserProfile returned empty string")
	}

	// Verify the prompt includes key sections
	if !contains(prompt, "User Profile Update Nudge") {
		t.Errorf("prompt missing 'User Profile Update Nudge'")
	}
	if !contains(prompt, "Current profile") {
		t.Errorf("prompt missing 'Current profile'")
	}
	if !contains(prompt, "Name") {
		t.Errorf("prompt missing 'Name'")
	}
}

func TestUserProfileFormatEmpty(t *testing.T) {
	profile := &UserProfile{
		UpdatedAt: time.Now(),
	}

	content := formatUserProfile(profile)
	if len(content) == 0 {
		t.Fatal("formatUserProfile returned empty string")
	}

	// Should still have frontmatter
	if !contains(content, "---") {
		t.Errorf("formatted profile missing frontmatter")
	}
}

func contains(s, substr string) bool {
	return len(s) > 0 && len(substr) > 0 && (s == substr || len(s) > len(substr) && (s[:len(substr)] == substr || s[len(s)-len(substr):] == substr || findInString(s, substr)))
}

func findInString(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}

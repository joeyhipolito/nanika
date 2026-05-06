package worker

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// UserProfile represents the USER.md persistent profile.
type UserProfile struct {
	Name                 string    `json:"-"` // For the frontmatter name: field
	Role                 string    `json:"role"`
	Preferences          []string  `json:"preferences"`
	CommunicationStyle   []string  `json:"communication_style"`
	OngoingProjects      []string  `json:"ongoing_projects"`
	UpdatedAt            time.Time `json:"updated_at"`
	OptedInForAutoUpdate bool      `json:"opted_in_for_auto_update"`
}

// userProfilePath returns ~/.claude/projects/<encoded-dir>/memory/USER.md
// Uses the same encoding as workerMemoryPath for consistency.
func userProfilePath(projectDir string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}

	key := encodeProjectKey(projectDir)
	return filepath.Join(home, ".claude", "projects", key, "memory", "USER.md"), nil
}

// LoadUserProfile reads and parses the USER.md file from the AutoMemory subsystem location.
func LoadUserProfile(projectDir string) (*UserProfile, error) {
	path, err := userProfilePath(projectDir)
	if err != nil {
		return nil, err
	}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			// Return a default/empty profile if the file doesn't exist
			return &UserProfile{
				OptedInForAutoUpdate: false,
			}, nil
		}
		return nil, fmt.Errorf("reading USER.md: %w", err)
	}

	profile := &UserProfile{}
	if err := parseUserProfile(string(data), profile); err != nil {
		return nil, err
	}

	return profile, nil
}

// SaveUserProfile writes the user profile to USER.md in the AutoMemory subsystem location.
func SaveUserProfile(projectDir string, profile *UserProfile) error {
	path, err := userProfilePath(projectDir)
	if err != nil {
		return err
	}

	// Ensure directory exists
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("creating directory: %w", err)
	}

	content := formatUserProfile(profile)

	// Atomic write: temp file then rename
	tmpPath := path + ".tmp"
	if err := os.WriteFile(tmpPath, []byte(content), 0600); err != nil {
		return fmt.Errorf("writing temp file: %w", err)
	}
	return os.Rename(tmpPath, path)
}

// parseUserProfile parses USER.md content into a UserProfile struct.
// Handles both YAML frontmatter and bullet-point sections.
func parseUserProfile(content string, profile *UserProfile) error {
	lines := strings.Split(content, "\n")
	inFrontmatter := false
	currentSection := ""
	frontmatterCount := 0

	for i := 0; i < len(lines); i++ {
		line := strings.TrimSpace(lines[i])

		// Handle frontmatter
		if line == "---" {
			frontmatterCount++
			if frontmatterCount == 1 {
				inFrontmatter = true
				continue
			} else if frontmatterCount == 2 {
				inFrontmatter = false
				continue
			}
		}

		if inFrontmatter {
			if strings.Contains(line, ":") {
				parts := strings.SplitN(line, ":", 2)
				key := strings.TrimSpace(parts[0])
				val := strings.TrimSpace(parts[1])

				switch key {
				case "name":
					profile.Name = val
				case "opted_in_for_auto_update":
					profile.OptedInForAutoUpdate = val == "true"
				}
			}
			continue
		}

		// Markdown headings for sections
		if strings.HasPrefix(line, "# ") {
			heading := strings.TrimPrefix(line, "# ")
			heading = strings.TrimSpace(heading)
			currentSection = strings.ToLower(heading)
			continue
		}
		if strings.HasPrefix(line, "## ") {
			currentSection = strings.ToLower(strings.TrimPrefix(line, "## "))
			continue
		}

		// Bullet points under each section
		if strings.HasPrefix(line, "- ") {
			item := strings.TrimPrefix(line, "- ")
			item = strings.TrimSpace(item)
			if item == "" {
				continue
			}

			switch currentSection {
			case "role":
				profile.Role = item
			case "preferences", "preferences and working style":
				profile.Preferences = append(profile.Preferences, item)
			case "communication style":
				profile.CommunicationStyle = append(profile.CommunicationStyle, item)
			case "ongoing projects", "current work":
				profile.OngoingProjects = append(profile.OngoingProjects, item)
			}
		}
	}

	return nil
}

// formatUserProfile renders a UserProfile as USER.md content with YAML frontmatter.
func formatUserProfile(profile *UserProfile) string {
	var sb strings.Builder

	// YAML frontmatter
	sb.WriteString("---\n")
	sb.WriteString(fmt.Sprintf("name: %s\n", profile.Name))
	sb.WriteString(fmt.Sprintf("updated_at: \"%s\"\n", profile.UpdatedAt.Format(time.RFC3339)))
	sb.WriteString(fmt.Sprintf("opted_in_for_auto_update: %v\n", profile.OptedInForAutoUpdate))
	sb.WriteString("---\n\n")

	// Name section
	if profile.Name != "" {
		sb.WriteString(fmt.Sprintf("# %s\n\n", profile.Name))
	}

	// Role section
	if profile.Role != "" {
		sb.WriteString("## Role\n\n")
		sb.WriteString(fmt.Sprintf("- %s\n\n", profile.Role))
	}

	// Preferences section
	if len(profile.Preferences) > 0 {
		sb.WriteString("## Preferences\n\n")
		for _, pref := range profile.Preferences {
			sb.WriteString(fmt.Sprintf("- %s\n", pref))
		}
		sb.WriteString("\n")
	}

	// Communication style section
	if len(profile.CommunicationStyle) > 0 {
		sb.WriteString("## Communication Style\n\n")
		for _, style := range profile.CommunicationStyle {
			sb.WriteString(fmt.Sprintf("- %s\n", style))
		}
		sb.WriteString("\n")
	}

	// Ongoing projects section
	if len(profile.OngoingProjects) > 0 {
		sb.WriteString("## Ongoing Projects\n\n")
		for _, proj := range profile.OngoingProjects {
			sb.WriteString(fmt.Sprintf("- %s\n", proj))
		}
		sb.WriteString("\n")
	}

	return sb.String()
}

// NudgeUserProfile generates a prompt for the periodic nudge to update USER.md.
// Returns the prompt as a string, suitable for injection into the learning pipeline.
func NudgeUserProfile(profile *UserProfile, lastUpdateHours int) string {
	prompt := "### User Profile Update Nudge\n\n"
	prompt += "Optionally update your USER.md profile based on what you've learned about yourself in this session. "
	prompt += "This helps personalize future AI assistance.\n\n"

	if lastUpdateHours == 0 {
		prompt += "This is your first profile update.\n\n"
	} else {
		prompt += fmt.Sprintf("Your profile was last updated %d hours ago.\n\n", lastUpdateHours)
	}

	prompt += "**Current profile:**\n"
	if profile.Name != "" {
		prompt += fmt.Sprintf("- Name: %s\n", profile.Name)
	}
	if profile.Role != "" {
		prompt += fmt.Sprintf("- Role: %s\n", profile.Role)
	}
	if len(profile.Preferences) > 0 {
		prompt += fmt.Sprintf("- Preferences: %v\n", profile.Preferences)
	}
	if len(profile.CommunicationStyle) > 0 {
		prompt += fmt.Sprintf("- Communication style: %v\n", profile.CommunicationStyle)
	}
	if len(profile.OngoingProjects) > 0 {
		prompt += fmt.Sprintf("- Ongoing projects: %v\n", profile.OngoingProjects)
	}

	prompt += "\n**To update your profile, provide:**\n"
	prompt += "- Name: Your name or identifier\n"
	prompt += "- Role: Your job title or primary function\n"
	prompt += "- Preferences: How you like to work (e.g., 'async communication', 'detailed explanations')\n"
	prompt += "- Communication style: How you prefer to communicate (e.g., 'direct', 'formal')\n"
	prompt += "- Ongoing projects: What you're currently working on\n"

	return prompt
}

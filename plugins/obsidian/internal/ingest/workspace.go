package ingest

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// WorkspaceMission holds metadata from a workspace's mission.json file.
type WorkspaceMission struct {
	ID        string   `json:"id"`
	Title     string   `json:"title"`
	Status    string   `json:"status"`
	Type      string   `json:"type"`
	Summary   string   `json:"summary"`
	Tags      []string `json:"tags"`
	CreatedAt string   `json:"created_at"`
}

// WorkspaceArtifact represents a Via workspace to be captured into the vault.
type WorkspaceArtifact struct {
	ID        string
	Title     string
	Status    string
	Type      string
	Summary   string
	Tags      []string
	CreatedAt time.Time
	Files     []string // artifact file names found in the workspace dir
}

// WorkspaceIngestOptions controls the workspace ingest operation.
type WorkspaceIngestOptions struct {
	Since  time.Duration // 0 means all
	DryRun bool
}

// WorkspaceIngestResult holds the results of a workspace ingest.
type WorkspaceIngestResult struct {
	Created []string `json:"created"`
	Skipped []string `json:"skipped"`
	Errors  []string `json:"errors"`
	Source  string   `json:"source"`
}

// workspacesBasePath returns the path to the Alluka workspaces directory.
func workspacesBasePath() (string, error) {
	base, err := allukaHome()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "workspaces"), nil
}

// IngestWorkspaces scans ~/.alluka/workspaces/ and creates capture notes for each workspace.
// Missing or empty directories are handled gracefully (returns empty result).
func IngestWorkspaces(vaultPath string, opts WorkspaceIngestOptions, state *State) (*WorkspaceIngestResult, error) {
	wsBase, err := workspacesBasePath()
	if err != nil {
		return nil, err
	}

	result := &WorkspaceIngestResult{Source: "workspaces"}

	if _, err := os.Stat(wsBase); err != nil {
		if os.IsNotExist(err) {
			// No workspaces directory yet — not an error
			return result, nil
		}
		return nil, fmt.Errorf("cannot access workspaces directory: %w", err)
	}

	entries, err := os.ReadDir(wsBase)
	if err != nil {
		return nil, fmt.Errorf("cannot read workspaces directory: %w", err)
	}

	var cutoff time.Time
	if opts.Since > 0 {
		cutoff = time.Now().Add(-opts.Since)
	}

	for _, entry := range entries {
		if !entry.IsDir() || strings.HasPrefix(entry.Name(), ".") {
			continue
		}

		wsDir := filepath.Join(wsBase, entry.Name())
		artifact, err := readWorkspaceDir(wsDir, entry.Name())
		if err != nil {
			result.Errors = append(result.Errors, fmt.Sprintf("cannot read workspace %s: %v", entry.Name(), err))
			continue
		}

		// Apply --since filter using workspace creation time
		if !cutoff.IsZero() && !artifact.CreatedAt.IsZero() && artifact.CreatedAt.Before(cutoff) {
			continue
		}

		if state.HasWorkspace(artifact.ID) {
			result.Skipped = append(result.Skipped, artifact.ID)
			continue
		}

		notePath := workspaceNotePath(artifact)
		content := buildWorkspaceNote(artifact)

		if opts.DryRun {
			result.Created = append(result.Created, notePath)
			continue
		}

		if err := writeNote(vaultPath, notePath, content); err != nil {
			if strings.Contains(err.Error(), "already exists") {
				result.Skipped = append(result.Skipped, notePath)
				state.MarkWorkspace(artifact.ID)
				continue
			}
			result.Errors = append(result.Errors, fmt.Sprintf("cannot write %s: %v", notePath, err))
			continue
		}

		result.Created = append(result.Created, notePath)
		state.MarkWorkspace(artifact.ID)
	}

	return result, nil
}

// readWorkspaceDir reads a workspace directory and returns its artifact metadata.
// Falls back gracefully when mission.json is absent or malformed.
func readWorkspaceDir(wsDir, dirName string) (*WorkspaceArtifact, error) {
	a := &WorkspaceArtifact{
		ID:    dirName,
		Title: dirName,
	}

	// Try to read mission.json for rich metadata
	missionPath := filepath.Join(wsDir, "mission.json")
	if data, err := os.ReadFile(missionPath); err == nil {
		var m WorkspaceMission
		if json.Unmarshal(data, &m) == nil {
			if m.Title != "" {
				a.Title = m.Title
			}
			if m.ID != "" {
				a.ID = m.ID
			}
			a.Status = m.Status
			a.Type = m.Type
			a.Summary = m.Summary
			a.Tags = m.Tags
			if m.CreatedAt != "" {
				if t, err := time.Parse(time.RFC3339, m.CreatedAt); err == nil {
					a.CreatedAt = t
				}
			}
		}
	}

	// Fall back to directory mtime if no created_at was found
	if a.CreatedAt.IsZero() {
		if info, err := os.Stat(wsDir); err == nil {
			a.CreatedAt = info.ModTime()
		}
	}

	// Collect artifact files (non-recursive, relevant extensions only)
	if entries, err := os.ReadDir(wsDir); err == nil {
		for _, e := range entries {
			if e.IsDir() {
				continue
			}
			name := e.Name()
			if name == "mission.json" {
				continue
			}
			ext := strings.ToLower(filepath.Ext(name))
			if ext == ".md" || ext == ".json" || ext == ".txt" {
				a.Files = append(a.Files, name)
			}
		}
	}

	return a, nil
}

// workspaceNotePath returns the vault-relative path for a workspace capture note.
// Format: Captures/Workspaces/{slug}.md
func workspaceNotePath(a *WorkspaceArtifact) string {
	slug := titleToSlug(a.ID)
	if slug == "" {
		slug = "workspace"
	}
	return filepath.Join("Captures", "Workspaces", slug+".md")
}

// buildWorkspaceNote generates markdown content for a workspace capture note.
func buildWorkspaceNote(a *WorkspaceArtifact) string {
	var b strings.Builder

	date := time.Now().Format("2006-01-02")
	if !a.CreatedAt.IsZero() {
		date = a.CreatedAt.Format("2006-01-02")
	}

	// Build tags list
	tags := []string{"capture", "workspace", "via"}
	if a.Type != "" {
		tags = append(tags, strings.ReplaceAll(a.Type, " ", "-"))
	}
	for _, t := range a.Tags {
		if t != "" {
			tags = append(tags, t)
		}
	}

	// Frontmatter
	b.WriteString("---\n")
	b.WriteString("type: capture\n")
	b.WriteString("source: via-workspace\n")
	fmt.Fprintf(&b, "workspace-id: %s\n", a.ID)
	if a.Status != "" {
		fmt.Fprintf(&b, "status: %s\n", a.Status)
	}
	fmt.Fprintf(&b, "date: %s\n", date)
	fmt.Fprintf(&b, "tags: [%s]\n", strings.Join(tags, ", "))
	fmt.Fprintf(&b, "captured: %s\n", time.Now().Format("2006-01-02"))
	b.WriteString("---\n\n")

	// Title
	fmt.Fprintf(&b, "# %s\n\n", a.Title)

	// Summary if present
	if strings.TrimSpace(a.Summary) != "" {
		b.WriteString(strings.TrimSpace(a.Summary))
		b.WriteString("\n\n")
	}

	// Workspace metadata section
	b.WriteString("## Workspace Info\n\n")
	fmt.Fprintf(&b, "- **ID**: `%s`\n", a.ID)
	if a.Type != "" {
		fmt.Fprintf(&b, "- **Type**: %s\n", a.Type)
	}
	if a.Status != "" {
		fmt.Fprintf(&b, "- **Status**: %s\n", a.Status)
	}
	fmt.Fprintf(&b, "- **Date**: %s\n", date)

	// Artifact file list
	if len(a.Files) > 0 {
		b.WriteString("\n## Artifacts\n\n")
		for _, f := range a.Files {
			fmt.Fprintf(&b, "- `%s`\n", f)
		}
	}

	b.WriteString("\n## Notes\n\n")
	b.WriteString("<!-- Add your notes here -->\n")

	return b.String()
}

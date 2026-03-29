package git

import (
	"regexp"
	"strings"
)

const maxSlugLen = 40

var (
	// nonAlphaNum matches any character that is not a letter, digit, or hyphen.
	nonAlphaNum = regexp.MustCompile(`[^a-z0-9-]+`)
	// multiHyphen collapses consecutive hyphens.
	multiHyphen = regexp.MustCompile(`-{2,}`)
)

// BranchName returns a git branch name in the form via/<missionID>/<slug>
// where slug is derived from task (lowercase, hyphens, max 40 chars).
func BranchName(missionID, task string) string {
	slug := slugify(task)
	return "via/" + missionID + "/" + slug
}

// slugify converts an arbitrary string into a URL/git-safe slug:
//   - lowercase
//   - spaces and special characters replaced by hyphens
//   - consecutive hyphens collapsed
//   - leading/trailing hyphens trimmed
//   - truncated to maxSlugLen characters (trimming trailing hyphens after truncation)
func slugify(s string) string {
	s = strings.ToLower(s)
	s = nonAlphaNum.ReplaceAllString(s, "-")
	s = multiHyphen.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")
	if len(s) > maxSlugLen {
		s = s[:maxSlugLen]
		s = strings.TrimRight(s, "-")
	}
	if s == "" {
		s = "task"
	}
	return s
}

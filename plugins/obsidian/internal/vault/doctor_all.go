package vault

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

const oversizedThreshold = 500 * 1024

func DoctorAll(path string) (Report, error) {
	report := Report{Path: path}

	info, err := os.Stat(path)
	if err != nil {
		return report, fmt.Errorf("accessing vault: %w", err)
	}
	if !info.IsDir() {
		return report, fmt.Errorf("not a directory: %s", path)
	}

	type entry struct {
		abs  string
		rel  string
		base string
		size int64
	}

	var entries []entry
	err = filepath.WalkDir(path, func(p string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return nil
		}
		if d.IsDir() && strings.HasPrefix(d.Name(), ".") {
			return filepath.SkipDir
		}
		if !d.IsDir() && strings.HasSuffix(d.Name(), ".md") {
			fi, ferr := d.Info()
			if ferr != nil {
				return nil
			}
			rel, _ := filepath.Rel(path, p)
			entries = append(entries, entry{
				abs:  p,
				rel:  rel,
				base: strings.TrimSuffix(d.Name(), ".md"),
				size: fi.Size(),
			})
		}
		return nil
	})
	if err != nil {
		return report, fmt.Errorf("walking vault: %w", err)
	}

	noteNames := make(map[string]bool, len(entries))
	inbound := make(map[string]int, len(entries))
	for _, e := range entries {
		noteNames[e.base] = true
		inbound[e.base] = 0
	}

	idMap := make(map[string][]string)

	for _, e := range entries {
		data, rerr := os.ReadFile(e.abs)
		if rerr != nil {
			continue
		}
		content := string(data)

		if !strings.HasPrefix(content, "---\n") && !strings.HasPrefix(content, "---\r\n") {
			report.Issues = append(report.Issues, fmt.Sprintf("missing frontmatter: %s", e.rel))
		}

		if e.size > oversizedThreshold {
			report.Issues = append(report.Issues, fmt.Sprintf("oversized file (%d bytes): %s", e.size, e.rel))
		}

		note := ParseNote(content)

		if id, ok := note.Frontmatter["id"]; ok {
			idStr := fmt.Sprintf("%v", id)
			if idStr != "" {
				idMap[idStr] = append(idMap[idStr], e.rel)
			}
		}

		for _, link := range note.Wikilinks {
			if noteNames[link] {
				inbound[link]++
			} else {
				report.DanglingCount++
			}
		}
	}

	for id, paths := range idMap {
		if len(paths) > 1 {
			report.InvariantViolations = append(report.InvariantViolations,
				fmt.Sprintf("duplicate id %q: %s", id, strings.Join(paths, ", ")))
		}
	}

	for _, e := range entries {
		if inbound[e.base] == 0 {
			report.OrphanCount++
		}
	}

	return report, nil
}

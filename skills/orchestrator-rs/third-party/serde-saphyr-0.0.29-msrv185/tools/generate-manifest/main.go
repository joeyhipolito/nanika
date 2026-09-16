// Command generate-manifest records the exact upstream and patched source
// delta for the reviewed serde-saphyr MSRV patch.
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
)

type fileRecord struct {
	Path   string `json:"path"`
	Mode   string `json:"mode"`
	SHA256 string `json:"sha256"`
}

type changeRecord struct {
	Path         string `json:"path"`
	BeforeSHA256 string `json:"before_sha256"`
	AfterSHA256  string `json:"after_sha256"`
}

type manifest struct {
	SchemaVersion   int            `json:"schema_version"`
	Crate           string         `json:"crate"`
	Version         string         `json:"version"`
	UpstreamArchive string         `json:"upstream_archive"`
	UpstreamSHA256  string         `json:"upstream_sha256"`
	SourceRoot      string         `json:"source_root"`
	OriginalFiles   []fileRecord   `json:"original_files"`
	Changes         []changeRecord `json:"changes"`
	UnchangedCount  int            `json:"unchanged_count"`
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	root, err := filepath.Abs("third-party/serde-saphyr-0.0.29-msrv185")
	if err != nil {
		return err
	}
	archive := filepath.Join(root, "upstream/serde-saphyr-0.0.29.crate")
	temporary, err := os.MkdirTemp("", "serde-saphyr-manifest-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(temporary)
	cmd := exec.Command("/usr/bin/tar", "-xzf", archive, "-C", temporary, "--strip-components=1")
	cmd.Env = nil
	if output, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("extract upstream: %w\n%s", err, output)
	}
	original, err := files(temporary)
	if err != nil {
		return err
	}
	patched, err := files(filepath.Join(root, "source"))
	if err != nil {
		return err
	}
	patchedByPath := make(map[string]fileRecord, len(patched))
	for _, file := range patched {
		patchedByPath[file.Path] = file
	}
	result := manifest{
		SchemaVersion:   1,
		Crate:           "serde-saphyr",
		Version:         "0.0.29",
		UpstreamArchive: "upstream/serde-saphyr-0.0.29.crate",
		SourceRoot:      "source",
		OriginalFiles:   original,
	}
	archiveBytes, err := os.ReadFile(archive)
	if err != nil {
		return err
	}
	result.UpstreamSHA256 = digest(archiveBytes)
	for _, before := range original {
		after, ok := patchedByPath[before.Path]
		if !ok {
			return fmt.Errorf("patched tree is missing %s", before.Path)
		}
		delete(patchedByPath, before.Path)
		if before.Mode != after.Mode {
			return fmt.Errorf("patched mode changed for %s", before.Path)
		}
		if before.SHA256 == after.SHA256 {
			result.UnchangedCount++
		} else {
			result.Changes = append(result.Changes, changeRecord{
				Path: before.Path, BeforeSHA256: before.SHA256, AfterSHA256: after.SHA256,
			})
		}
	}
	if len(patchedByPath) != 0 {
		return fmt.Errorf("patched tree has %d extra files", len(patchedByPath))
	}
	encoded, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return err
	}
	encoded = append(encoded, '\n')
	return os.WriteFile(filepath.Join(root, "patch-manifest.json"), encoded, 0o644)
}

func files(root string) ([]fileRecord, error) {
	var result []fileRecord
	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("unsupported source entry %s", path)
		}
		contents, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		relative, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		result = append(result, fileRecord{
			Path: filepath.ToSlash(relative), Mode: fmt.Sprintf("%04o", info.Mode().Perm()), SHA256: digest(contents),
		})
		return nil
	})
	sort.Slice(result, func(i, j int) bool { return result[i].Path < result[j].Path })
	return result, err
}

func digest(contents []byte) string {
	sum := sha256.Sum256(contents)
	return hex.EncodeToString(sum[:])
}

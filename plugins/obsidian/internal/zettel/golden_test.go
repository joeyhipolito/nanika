package zettel

import (
	"flag"
	"os"
	"path/filepath"
	"testing"
)

var updateGolden = flag.Bool("update-golden", false, "overwrite golden files with current output")

func goldenPath(name string) string {
	return filepath.Join("..", "..", "testdata", "golden", name)
}

// checkGolden compares got against the named golden file.
// Pass -update-golden to regenerate.
func checkGolden(t *testing.T, name, got string) {
	t.Helper()
	path := goldenPath(name)
	if *updateGolden {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatalf("cannot create golden dir: %v", err)
		}
		if err := os.WriteFile(path, []byte(got), 0644); err != nil {
			t.Fatalf("cannot write golden file %s: %v", path, err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("cannot read golden file %s: %v\n(run with -update-golden to create it)", path, err)
	}
	if string(want) != got {
		t.Errorf("golden mismatch for %s\n--- want ---\n%s\n--- got ---\n%s", name, want, got)
	}
}

package sdk

import "testing"

// setSessionsRoot overrides sessionsRoot for the duration of a test and
// restores it via t.Cleanup.
func setSessionsRoot(t *testing.T, dir string) {
	t.Helper()
	orig := sessionsRoot
	sessionsRoot = dir
	t.Cleanup(func() { sessionsRoot = orig })
}

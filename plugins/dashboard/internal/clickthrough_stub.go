//go:build !darwin

package internal

// PrimaryScreenSize returns a fallback size on non-macOS platforms.
func PrimaryScreenSize() (int, int) { return 1920, 1080 }

// StartClickthrough is a no-op on non-macOS platforms.
func StartClickthrough() {}

// EnableInteraction is a no-op on non-macOS platforms.
func EnableInteraction(x, y, w, h float64) {}

// DisableInteraction is a no-op on non-macOS platforms.
func DisableInteraction() {}

// RegisterHotkey is a no-op on non-macOS platforms.
func RegisterHotkey() {}

// FocusWindowNative is a no-op on non-macOS platforms.
func FocusWindowNative() {}

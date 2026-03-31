//go:build !darwin

package internal

import "context"

// SetupTray is a no-op on non-macOS platforms.
func SetupTray(ctx context.Context) {}

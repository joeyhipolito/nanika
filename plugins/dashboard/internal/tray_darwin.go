//go:build darwin

package internal

/*
#cgo CFLAGS: -x objective-c -fobjc-arc
#cgo LDFLAGS: -framework Cocoa

// Implemented in tray_darwin.m
void nanikaTraySetup(void);
*/
import "C"

import "context"

// SetupTray initialises the macOS menu-bar status item.
// Must be called after SetContext so the callbacks can reach the Wails runtime.
func SetupTray(ctx context.Context) {
	SetContext(ctx)
	C.nanikaTraySetup()
}

//export goTrayShow
func goTrayShow() {
	OpenPalette()
}

//export goTrayQuit
func goTrayQuit() {
	QuitApp()
}

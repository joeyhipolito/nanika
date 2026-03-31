//go:build darwin

package internal

/*
#cgo CFLAGS: -x objective-c
#cgo LDFLAGS: -framework Cocoa -framework CoreGraphics

typedef struct { int w; int h; } ScreenDims;

// Implemented in clickthrough_darwin.m
ScreenDims primaryScreenDims(void);
void clickthrough_start(void);
void clickthrough_enable_interaction(float x, float y, float w, float h);
void clickthrough_disable_interaction(void);
void clickthrough_register_hotkey(void);
void focus_window(void);
*/
import "C"

// PrimaryScreenSize returns the logical point dimensions of the primary
// display. Safe to call before wails.Run.
func PrimaryScreenSize() (int, int) {
	d := C.primaryScreenDims()
	return int(d.w), int(d.h)
}

// StartClickthrough installs the NSEvent monitors and puts the overlay into
// full pass-through mode. Call once after the Wails window is created.
func StartClickthrough() {
	C.clickthrough_start()
}

// EnableInteraction sets the active interactive region (AppKit screen coords).
func EnableInteraction(x, y, w, h float64) {
	C.clickthrough_enable_interaction(C.float(x), C.float(y), C.float(w), C.float(h))
}

// DisableInteraction restores full click-through.
func DisableInteraction() {
	C.clickthrough_disable_interaction()
}

// RegisterHotkey installs global and local Option double-tap key monitors.
func RegisterHotkey() {
	C.clickthrough_register_hotkey()
}

// FocusWindowNative activates the app and gives the window keyboard focus via
// NSApp activateIgnoringOtherApps + makeKeyAndOrderFront.
func FocusWindowNative() {
	C.focus_window()
}

//export goHotkeyTriggered
func goHotkeyTriggered() {
	OpenPalette()
}

//export goClickOutside
func goClickOutside() {
	DismissPalette()
}

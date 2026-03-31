package internal

import (
	"context"
	"sync"

	wailsruntime "github.com/wailsapp/wails/v2/pkg/runtime"
)

var (
	mu     sync.Mutex
	appCtx context.Context
)

// SetContext stores the Wails runtime context for use by tray callbacks.
func SetContext(ctx context.Context) {
	mu.Lock()
	appCtx = ctx
	mu.Unlock()
}

// InitClickthrough installs the NSEvent monitors and puts the overlay into
// full pass-through mode. Call once from OnStartup after the window is ready.
func InitClickthrough() {
	StartClickthrough()
}

// OpenPalette emits a toggle event and focuses the window so the webview
// receives keyboard input immediately.
func OpenPalette() {
	mu.Lock()
	c := appCtx
	mu.Unlock()
	if c == nil {
		return
	}
	go func() {
		wailsruntime.EventsEmit(c, "nanika:toggle-palette")
		FocusWindow()
	}()
}

// FocusWindow brings the overlay window to front and gives it keyboard focus.
// Calls both wailsruntime.WindowShow (ensures Wails state is consistent) and
// FocusWindowNative (NSApp activateIgnoringOtherApps + makeKeyAndOrderFront)
// so the webview receives keyboard events immediately.
func FocusWindow() {
	mu.Lock()
	c := appCtx
	mu.Unlock()
	if c == nil {
		return
	}
	go func() {
		wailsruntime.WindowShow(c)
		FocusWindowNative()
	}()
}

// DismissPalette emits a dismiss event so the frontend closes the palette
// without toggling — used by click-outside detection in the ObjC layer.
func DismissPalette() {
	mu.Lock()
	c := appCtx
	mu.Unlock()
	if c == nil {
		return
	}
	go func() {
		wailsruntime.EventsEmit(c, "nanika:dismiss-palette")
	}()
}

// UnfocusWindow removes keyboard focus from the overlay so key events
// pass to the app beneath. Called when the palette is dismissed.
func UnfocusWindow() {
	mu.Lock()
	c := appCtx
	mu.Unlock()
	if c == nil {
		return
	}
	go func() {
		wailsruntime.WindowHide(c)
		wailsruntime.WindowShow(c)
	}()
}

// QuitApp terminates the application.
func QuitApp() {
	mu.Lock()
	c := appCtx
	mu.Unlock()
	if c == nil {
		return
	}
	go func() {
		wailsruntime.Quit(c)
	}()
}

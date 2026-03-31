#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>

typedef struct { int w; int h; } ScreenDims;

ScreenDims primaryScreenDims(void) {
    CGRect bounds = CGDisplayBounds(CGMainDisplayID());
    ScreenDims d;
    d.w = (int)bounds.size.width;
    d.h = (int)bounds.size.height;
    return d;
}

// --- click-through overlay ---

static id    gGlobalMonitor        = nil;
static id    gLocalMonitor         = nil;
static id    gClickMonitor         = nil;
static BOOL  gInteractiveEnabled   = NO;
static CGFloat gInteractX = 0, gInteractY = 0, gInteractW = 0, gInteractH = 0;

static NSWindow* appWindow(void) {
    for (NSWindow *w in [[NSApplication sharedApplication] windows]) {
        if (w.visible && !w.miniaturized) return w;
    }
    return nil;
}

static void checkMouse(void) {
    NSWindow *win = appWindow();
    if (!win) return;
    if (!gInteractiveEnabled) {
        [win setIgnoresMouseEvents:YES];
        return;
    }
    NSPoint mouse  = [NSEvent mouseLocation];
    NSRect  rect   = NSMakeRect(gInteractX, gInteractY, gInteractW, gInteractH);
    NSRect  padded = NSInsetRect(rect, -6.0, -6.0);
    BOOL    over   = NSPointInRect(mouse, padded);
    [win setIgnoresMouseEvents:!over];
}

extern void goClickOutside(void);

void clickthrough_start(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gGlobalMonitor != nil) return;

        NSWindow *win = appWindow();
        if (win) [win setIgnoresMouseEvents:YES];

        gGlobalMonitor = [NSEvent
            addGlobalMonitorForEventsMatchingMask:
                (NSEventMaskMouseMoved |
                 NSEventMaskLeftMouseDragged |
                 NSEventMaskRightMouseDragged)
            handler:^(NSEvent *event) {
                dispatch_async(dispatch_get_main_queue(), ^{ checkMouse(); });
            }];

        gLocalMonitor = [NSEvent
            addLocalMonitorForEventsMatchingMask:
                (NSEventMaskMouseMoved |
                 NSEventMaskLeftMouseDragged |
                 NSEventMaskRightMouseDragged)
            handler:^NSEvent *(NSEvent *event) {
                checkMouse();
                return event;
            }];

        // Click-outside monitor: fires for left-mouse-down events that pass
        // through the overlay (i.e. clicks outside the palette bounds where
        // ignoresMouseEvents is YES). When the palette is open, any click that
        // reaches another app means the user clicked outside — dismiss the palette.
        gClickMonitor = [NSEvent
            addGlobalMonitorForEventsMatchingMask:NSEventMaskLeftMouseDown
            handler:^(NSEvent *event) {
                if (!gInteractiveEnabled) return;
                NSPoint mouse  = [NSEvent mouseLocation];
                NSRect  rect   = NSMakeRect(gInteractX, gInteractY, gInteractW, gInteractH);
                NSRect  padded = NSInsetRect(rect, -6.0, -6.0);
                if (!NSPointInRect(mouse, padded)) {
                    dispatch_async(dispatch_get_main_queue(), ^{ goClickOutside(); });
                }
            }];

        checkMouse();
    });
}

void clickthrough_enable_interaction(float x, float y, float w, float h) {
    gInteractX = (CGFloat)x;
    gInteractY = (CGFloat)y;
    gInteractW = (CGFloat)w;
    gInteractH = (CGFloat)h;
    gInteractiveEnabled = YES;
    dispatch_async(dispatch_get_main_queue(), ^{ checkMouse(); });
}

void clickthrough_disable_interaction(void) {
    gInteractiveEnabled = NO;
    dispatch_async(dispatch_get_main_queue(), ^{
        NSWindow *win = appWindow();
        if (win) [win setIgnoresMouseEvents:YES];
    });
}

// --- focus: bring window to foreground with keyboard input ---
// Both activateIgnoringOtherApps and makeKeyAndOrderFront are required:
// activateIgnoringOtherApps makes the process the active app (menu bar, etc.),
// while makeKeyAndOrderFront makes the specific window the key window so it
// receives keyboard events.

void focus_window(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        NSWindow *win = appWindow();
        if (!win) return;
        [NSApp activateIgnoringOtherApps:YES];
        [win makeKeyAndOrderFront:nil];
    });
}

// --- global hotkey: double-tap Option ---
// Triggers when Option is pressed twice within 350ms.
// Needs BOTH a global monitor (fires when another app is active) AND a local
// monitor (fires when our app is active, i.e. palette is open). Without the
// local monitor the second tap while the palette is open is invisible to us.

extern void goHotkeyTriggered(void);

static id             gKeyMonitor      = nil;
static id             gLocalKeyMonitor = nil;
static NSTimeInterval gLastOptionUp    = 0;

static void handleOptionKey(NSEvent *event) {
    // keyCode 58 = Left Option, 61 = Right Option
    if (event.keyCode != 58 && event.keyCode != 61) return;

    // We care about key-up (Option released)
    BOOL optionDown = (event.modifierFlags & NSEventModifierFlagOption) != 0;
    if (optionDown) return; // key pressed, wait for release

    NSTimeInterval now   = [NSDate timeIntervalSinceReferenceDate];
    NSTimeInterval delta = now - gLastOptionUp;
    gLastOptionUp = now;

    if (delta < 0.35 && delta > 0.05) {
        // Double-tap detected — reset so a third tap doesn't re-trigger
        gLastOptionUp = 0;
        goHotkeyTriggered();
    }
}

void clickthrough_register_hotkey(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gKeyMonitor != nil) return;

        // Global: fires when another app is in front (palette is closed/hidden).
        gKeyMonitor = [NSEvent
            addGlobalMonitorForEventsMatchingMask:NSEventMaskFlagsChanged
            handler:^(NSEvent *event) {
                handleOptionKey(event);
            }];

        // Local: fires when our app is the active app (palette is open).
        // Without this the hotkey cannot dismiss an open palette.
        gLocalKeyMonitor = [NSEvent
            addLocalMonitorForEventsMatchingMask:NSEventMaskFlagsChanged
            handler:^NSEvent *(NSEvent *event) {
                handleOptionKey(event);
                return event;
            }];
    });
}

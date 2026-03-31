#import <Cocoa/Cocoa.h>

// Forward declarations for Go-exported callbacks.
extern void goTrayShow(void);
extern void goTrayQuit(void);

@interface _NanikaTrayDelegate : NSObject
- (IBAction)doShow:(id)sender;
- (IBAction)doQuit:(id)sender;
@end

@implementation _NanikaTrayDelegate
- (IBAction)doShow:(id)sender { goTrayShow(); }
- (IBAction)doQuit:(id)sender { goTrayQuit(); }
@end

static _NanikaTrayDelegate *_trayDelegate;
static NSStatusItem *_statusItem;

void nanikaTraySetup(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        _trayDelegate = [[_NanikaTrayDelegate alloc] init];

        NSStatusBar *bar = [NSStatusBar systemStatusBar];
        _statusItem = [bar statusItemWithLength:NSSquareStatusItemLength];

        if (@available(macOS 11.0, *)) {
            NSImage *icon = [NSImage imageWithSystemSymbolName:@"n.circle.fill"
                                        accessibilityDescription:@"Nanika"];
            icon.template = YES;
            _statusItem.button.image = icon;
        } else {
            _statusItem.button.title = @"N";
        }
        _statusItem.button.toolTip = @"Nanika";

        NSMenu *menu = [[NSMenu alloc] init];

        NSMenuItem *openItem = [[NSMenuItem alloc]
            initWithTitle:@"Open"
                   action:@selector(doShow:)
            keyEquivalent:@""];
        openItem.target = _trayDelegate;
        [menu addItem:openItem];

        [menu addItem:[NSMenuItem separatorItem]];

        NSMenuItem *quitItem = [[NSMenuItem alloc]
            initWithTitle:@"Quit"
                   action:@selector(doQuit:)
            keyEquivalent:@"q"];
        quitItem.target = _trayDelegate;
        [menu addItem:quitItem];

        _statusItem.menu = menu;
    });
}

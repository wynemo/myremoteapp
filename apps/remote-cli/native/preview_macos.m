#import <Cocoa/Cocoa.h>
#import <CoreGraphics/CoreGraphics.h>
#import <QuartzCore/QuartzCore.h>
#include <stdbool.h>

// The image layer is separate from minifb's nearest-neighbour Metal renderer.
// The original content view keeps handling all input; this overlay never hits.
@interface RemotePreviewImageView : NSView
@property(nonatomic, strong) CALayer *picture;
@property(nonatomic) size_t imageWidth;
@property(nonatomic) size_t imageHeight;
@property(nonatomic, strong) id keyboardMonitor;
@property(nonatomic, strong) id keyboardFocusObserver;
@property(nonatomic, strong) NSMutableIndexSet *modifierKeys;
@property(nonatomic) BOOL lastCapsLock;
@end

@implementation RemotePreviewImageView
- (BOOL)isFlipped { return YES; }
- (NSView *)hitTest:(NSPoint)point { return nil; }
- (void)viewDidMoveToWindow { [super viewDidMoveToWindow]; [self updatePictureLayout]; }
- (void)viewDidChangeBackingProperties {
    [super viewDidChangeBackingProperties];
    [self updatePictureLayout];
}
- (void)resizeSubviewsWithOldSize:(NSSize)size {
    [super resizeSubviewsWithOldSize:size];
    [self updatePictureLayout];
}
- (void)updatePictureLayout {
    if (!self.picture || !self.imageWidth || !self.imageHeight) return;
    // Match the integer aspect-fit viewport used by mouse::normalize.
    double width = floor(self.bounds.size.width), height = floor(self.bounds.size.height);
    double aspect = (double)self.imageWidth / self.imageHeight;
    double fitWidth = width, fitHeight = height;
    if (aspect > width / MAX(1, height)) fitHeight = floor(width / aspect);
    else fitWidth = floor(height * aspect);
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    self.picture.frame = CGRectMake(floor((width - fitWidth) / 2),
                                    floor((height - fitHeight) / 2), fitWidth, fitHeight);
    self.picture.contentsScale = self.window.backingScaleFactor ?: 1;
    [CATransaction commit];
}
@end

void *remote_preview_create(void *handle) {
    NSWindow *window = (__bridge NSWindow *)handle;
    if (!window.contentView) return NULL;
    // minifb wraps the public contentView in its input-handling frame view.
    // Fit to that same viewport, not the smaller nested Metal container.
    NSView *host = window.contentView.superview ?: window.contentView;
    RemotePreviewImageView *view = [[RemotePreviewImageView alloc] initWithFrame:host.bounds];
    view.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    view.wantsLayer = YES;
    view.layer.backgroundColor = NSColor.blackColor.CGColor;
    view.picture = [CALayer layer];
    view.picture.contentsGravity = kCAGravityResize;
    view.picture.minificationFilter = kCAFilterTrilinear;
    view.picture.magnificationFilter = kCAFilterLinear;
    [view.layer addSublayer:view.picture];
    [host addSubview:view positioned:NSWindowAbove relativeTo:nil];
    return (__bridge_retained void *)view;
}

bool remote_preview_present(void *handle, const uint32_t *pixels, size_t width, size_t height) {
    RemotePreviewImageView *view = (__bridge RemotePreviewImageView *)handle;
    if (!view || !pixels || !width || !height || width > 2560 || height > 2560) return false;
    CFDataRef data = CFDataCreate(NULL, (const UInt8 *)pixels, width * height * 4);
    if (!data) return false;
    CGDataProviderRef provider = CGDataProviderCreateWithCFData(data);
    CFRelease(data);
    if (!provider) return false;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGImageRef image = CGImageCreate(width, height, 8, 32, width * 4, space,
        kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst, provider, NULL, true,
        kCGRenderingIntentDefault);
    CGColorSpaceRelease(space);
    CGDataProviderRelease(provider);
    if (!image) return false;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    view.imageWidth = width;
    view.imageHeight = height;
    view.picture.contents = (__bridge id)image;
    [view updatePictureLayout];
    [CATransaction commit];
    CGImageRelease(image);
    return true;
}

// Read position and size from the exact view that displays the image. minifb's
// cached size updates only at the end of live resize and its mouse origin is
// reconstructed from the window frame rather than this view's coordinate space.
static bool preview_input_geometry(RemotePreviewImageView *view, NSPoint windowPoint,
                                   float *x, float *y, size_t *width, size_t *height) {
    if (!view || !view.window) return false;
    NSRect bounds = view.bounds;
    NSPoint local = [view convertPoint:windowPoint fromView:nil];
    *x = local.x - bounds.origin.x;
    *y = local.y - bounds.origin.y;
    *width = (size_t)MAX(0, floor(bounds.size.width));
    *height = (size_t)MAX(0, floor(bounds.size.height));
    return *width > 0 && *height > 0;
}

bool remote_preview_mouse(void *handle, float *x, float *y, size_t *width, size_t *height) {
    RemotePreviewImageView *view = (__bridge RemotePreviewImageView *)handle;
    if (!view || !view.window) return false;
    return preview_input_geometry(view, view.window.mouseLocationOutsideOfEventStream,
                                  x, y, width, height);
}

static void preview_key_transition(NSWindow *window, NSEvent *source, unsigned short key, BOOL down) {
    NSEvent *translated = [NSEvent keyEventWithType:down ? NSEventTypeKeyDown : NSEventTypeKeyUp
        location:source.locationInWindow modifierFlags:source.modifierFlags timestamp:source.timestamp
        windowNumber:window.windowNumber context:nil characters:@"" charactersIgnoringModifiers:@""
        isARepeat:NO keyCode:key];
    if (down) [window keyDown:translated];
    else [window keyUp:translated];
}

void remote_preview_keyboard_enabled(void *handle, bool enabled) {
    RemotePreviewImageView *view = (__bridge RemotePreviewImageView *)handle;
    if (view.keyboardMonitor) {
        [NSEvent removeMonitor:view.keyboardMonitor];
        view.keyboardMonitor = nil;
    }
    if (view.keyboardFocusObserver) {
        [NSNotificationCenter.defaultCenter removeObserver:view.keyboardFocusObserver];
        view.keyboardFocusObserver = nil;
    }
    if (enabled) {
        view.modifierKeys = [NSMutableIndexSet indexSet];
        view.lastCapsLock = (NSEvent.modifierFlags & NSEventModifierFlagCapsLock) != 0;
        __weak RemotePreviewImageView *weakView = view;
        view.keyboardFocusObserver = [NSNotificationCenter.defaultCenter
            addObserverForName:nil object:view.window queue:nil
            usingBlock:^(NSNotification *note) {
                if ([note.name isEqualToString:NSWindowDidResignKeyNotification]) {
                    [weakView.modifierKeys removeAllIndexes];
                } else if ([note.name isEqualToString:NSWindowDidBecomeKeyNotification]) {
                    weakView.lastCapsLock = (NSEvent.modifierFlags & NSEventModifierFlagCapsLock) != 0;
                }
            }];
        // Local monitor only: no global keyboard hook or monitoring permission.
        // Route Command combinations past local Quit/Hide menu shortcuts, and
        // retain the actual modifier side (minifb's aggregate fallback sets both).
        view.keyboardMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:
            NSEventMaskKeyDown | NSEventMaskFlagsChanged handler:^NSEvent *(NSEvent *event) {
                RemotePreviewImageView *current = weakView;
                NSWindow *target = current.window;
                if (!target || !target.isKeyWindow || event.window != target) return event;
                if (event.type == NSEventTypeFlagsChanged) {
                    unsigned short code = event.keyCode;
                    if (code == 57) {
                        BOOL caps = (event.modifierFlags & NSEventModifierFlagCapsLock) != 0;
                        if (caps != current.lastCapsLock) {
                            current.lastCapsLock = caps;
                            preview_key_transition(target, event, code, YES);
                            preview_key_transition(target, event, code, NO);
                        }
                        return nil;
                    }
                    NSEventModifierFlags mask = 0;
                    switch (code) {
                        case 54: case 55: mask = NSEventModifierFlagCommand; break;
                        case 56: case 60: mask = NSEventModifierFlagShift; break;
                        case 58: case 61: mask = NSEventModifierFlagOption; break;
                        case 59: case 62: mask = NSEventModifierFlagControl; break;
                        default: return nil;
                    }
                    BOOL down = (event.modifierFlags & mask) && ![current.modifierKeys containsIndex:code];
                    if (down) [current.modifierKeys addIndex:code];
                    else [current.modifierKeys removeIndex:code];
                    preview_key_transition(target, event, code, down);
                    return nil;
                }
                if (event.modifierFlags & NSEventModifierFlagCommand) {
                    [target sendEvent:event];
                    return nil;
                }
                return event;
            }];
    }
}

// Physical keyboard state straight from the HID system. Key releases that
// happen while the viewer is unfocused never reach the local monitor, so the
// client uses this to decide which blocked modifiers to press again.
bool remote_physical_key_down(unsigned short keyCode) {
    return CGEventSourceKeyState(kCGEventSourceStateHIDSystemState, keyCode);
}

void remote_preview_destroy(void *handle) {
    remote_preview_keyboard_enabled(handle, false);
    RemotePreviewImageView *view = (__bridge_transfer RemotePreviewImageView *)handle;
    [view removeFromSuperview];
}

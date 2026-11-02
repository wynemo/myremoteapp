// Native coordinate regression without screen capture or synthetic input.
#import "../apps/remote-cli/native/preview_macos.m"
#include <assert.h>
#include <math.h>

static void check_point(RemotePreviewImageView *view, double x, double y,
                        size_t expectedWidth, size_t expectedHeight) {
    // The non-flipped parent starts at window (0, 0); this view is flipped.
    // Calculate the window coordinate independently of convertPoint:fromView:.
    NSPoint point = NSMakePoint(view.frame.origin.x + x,
                               NSMaxY(view.frame) - y);
    float actualX = 0, actualY = 0;
    size_t width = 0, height = 0;
    assert(preview_input_geometry(view, point, &actualX, &actualY, &width, &height));
    assert(fabs(actualX - x) < 0.001 && fabs(actualY - y) < 0.001);
    assert(width == expectedWidth && height == expectedHeight);
}

int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        NSWindow *window = [[NSWindow alloc]
            initWithContentRect:NSMakeRect(0, 0, 1200, 900)
            styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskResizable
            backing:NSBackingStoreBuffered defer:NO];
        RemotePreviewImageView *view = [[RemotePreviewImageView alloc]
            initWithFrame:NSMakeRect(37, 29, 1000, 800)];
        view.wantsLayer = YES;
        view.picture = [CALayer layer];
        [view.layer addSublayer:view.picture];
        [window.contentView addSubview:view];
        view.imageWidth = 1920;
        view.imageHeight = 1080;
        [view updatePictureLayout];
        assert(CGRectEqualToRect(view.picture.frame, CGRectMake(0, 119, 1000, 562)));
        for (int row = 0; row <= 4; ++row) {
            for (int column = 0; column <= 4; ++column) {
                check_point(view, column * 249.75, 119 + row * 140.25, 1000, 800);
            }
        }
        // Live resize: no minifb resize-ended callback or cached size update.
        view.frame = NSMakeRect(53, 41, 800, 600);
        [view updatePictureLayout];
        assert(CGRectEqualToRect(view.picture.frame, CGRectMake(0, 75, 800, 450)));
        check_point(view, 400, 300, 800, 600);
        check_point(view, 799, 524, 800, 600);
        view.imageWidth = 1080;
        view.imageHeight = 1920;
        [view updatePictureLayout];
        assert(CGRectEqualToRect(view.picture.frame, CGRectMake(231, 0, 337, 600)));
        check_point(view, 231, 0, 800, 600);
        check_point(view, 567, 599, 800, 600);
        // Outside points must remain outside for Rust to reject letterboxing.
        check_point(view, -1, -1, 800, 600);
        check_point(view, 801, 601, 800, 600);
        // Keep a readable logical window and fit the work area.
        NSRect logical = NSMakeRect(0, 0, 1920, 1080);
        size_t pixelWidth = 0, pixelHeight = 0;
        remote_preview_pixel_size((__bridge void *)view, &pixelWidth, &pixelHeight);
        NSRect backing = [view convertRectToBacking:view.bounds];
        assert(pixelWidth == (size_t)llround(backing.size.width));
        assert(pixelHeight == (size_t)llround(backing.size.height));
        uint32_t scalePercent = 0;
        remote_preview_display_metrics((__bridge void *)view, &pixelWidth, &pixelHeight, &scalePercent);
        assert(scalePercent == (uint32_t)llround(window.backingScaleFactor * 100));
        NSRect screenBacking = [window convertRectToBacking:NSMakeRect(0, 0, window.screen.frame.size.width, window.screen.frame.size.height)];
        assert(pixelWidth == (size_t)llround(screenBacking.size.width));
        assert(pixelHeight == (size_t)llround(screenBacking.size.height));
        remote_preview_configure_window((__bridge void *)window, 1920, 1080);
        NSRect content = [window contentRectForFrameRect:window.frame];
        assert(content.size.width <= logical.size.width + 1);
        assert(content.size.height <= logical.size.height + 1);
        NSRect work = window.screen.visibleFrame;
        assert(window.frame.size.width <= work.size.width + 1);
        assert(window.frame.size.height <= work.size.height + 1);
        assert(fabs(content.size.width / content.size.height - 1920.0 / 1080.0) < 0.01);
        puts("Native preview coordinates: offset origin, flipped Y, landscape/portrait, live resize and outside points passed.");
    }
    return 0;
}

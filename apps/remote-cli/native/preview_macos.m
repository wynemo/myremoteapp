#import <Cocoa/Cocoa.h>
#import <QuartzCore/QuartzCore.h>
#include <stdbool.h>

// The image layer is separate from minifb's nearest-neighbour Metal renderer.
// The original content view keeps handling all input; this overlay never hits.
@interface RemotePreviewImageView : NSView
@property(nonatomic, strong) CALayer *picture;
@property(nonatomic) size_t imageWidth;
@property(nonatomic) size_t imageHeight;
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

void remote_preview_destroy(void *handle) {
    RemotePreviewImageView *view = (__bridge_transfer RemotePreviewImageView *)handle;
    [view removeFromSuperview];
}

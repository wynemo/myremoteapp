#import <Cocoa/Cocoa.h>
#import <ApplicationServices/ApplicationServices.h>
#include <stdbool.h>
#include <unistd.h>

// Runtime-checked CoreGraphics SPI. No changes to physical display modes.
@interface NSObject (RemoteVirtualDisplayAPI)
- (id)initWithDescriptor:(id)descriptor;
- (id)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)rate;
- (BOOL)applySettings:(id)settings;
- (unsigned int)displayID;
@end

static bool windowBounds(CGWindowID windowID, pid_t pid, CGRect *rect) {
    CFArrayRef list = CGWindowListCopyWindowInfo(kCGWindowListOptionIncludingWindow, windowID);
    if (!list) return false;
    bool ok = false;
    if (CFArrayGetCount(list) == 1) {
        CFDictionaryRef entry = CFArrayGetValueAtIndex(list, 0);
        int owner = 0;
        CFNumberRef ownerValue = CFDictionaryGetValue(entry, kCGWindowOwnerPID);
        CFDictionaryRef bounds = CFDictionaryGetValue(entry, kCGWindowBounds);
        ok = ownerValue && CFNumberGetValue(ownerValue, kCFNumberIntType, &owner) && owner == pid &&
             bounds && CGRectMakeWithDictionaryRepresentation(bounds, rect);
    }
    CFRelease(list);
    return ok;
}

static AXUIElementRef findWindow(pid_t pid, CGRect wanted) {
    AXUIElementRef app = AXUIElementCreateApplication(pid);
    if (!app) return NULL;
    AXUIElementSetMessagingTimeout(app, 0.3);
    CFArrayRef windows = NULL;
    AXError result = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute, (CFTypeRef *)&windows);
    CFRelease(app);
    if (result != kAXErrorSuccess || !windows) { if (windows) CFRelease(windows); return NULL; }
    if (CFGetTypeID(windows) != CFArrayGetTypeID()) { CFRelease(windows); return NULL; }
    if (CFArrayGetCount(windows) > 128) { CFRelease(windows); return NULL; }
    AXUIElementRef found = NULL;
    CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + 3.0;
    for (CFIndex i = 0; i < CFArrayGetCount(windows); i++) {
        if (CFAbsoluteTimeGetCurrent() >= deadline) {
            if (found) CFRelease(found);
            found = NULL;
            break;
        }
        AXUIElementRef window = (AXUIElementRef)CFArrayGetValueAtIndex(windows, i);
        if (!window || CFGetTypeID(window) != AXUIElementGetTypeID()) continue;
        AXUIElementSetMessagingTimeout(window, 0.3);
        AXValueRef p = NULL, s = NULL;
        CGPoint position = CGPointZero; CGSize size = CGSizeZero;
        AXUIElementCopyAttributeValue(window, kAXPositionAttribute, (CFTypeRef *)&p);
        AXUIElementCopyAttributeValue(window, kAXSizeAttribute, (CFTypeRef *)&s);
        bool matches = p && s && CFGetTypeID(p) == AXValueGetTypeID() && CFGetTypeID(s) == AXValueGetTypeID() &&
            AXValueGetValue(p, kAXValueCGPointType, &position) && AXValueGetValue(s, kAXValueCGSizeType, &size) &&
            fabs(position.x - wanted.origin.x) < 3 && fabs(position.y - wanted.origin.y) < 3 &&
            fabs(size.width - wanted.size.width) < 3 && fabs(size.height - wanted.size.height) < 3;
        if (p) CFRelease(p); if (s) CFRelease(s);
        if (matches) {
            if (found) { CFRelease(found); found = NULL; break; }
            found = (AXUIElementRef)CFRetain(window);
        }
    }
    CFRelease(windows);
    return found;
}

static bool setPosition(AXUIElementRef window, CGPoint point) {
    AXValueRef value = AXValueCreate(kAXValueCGPointType, &point);
    if (!value) return false;
    AXError result = AXUIElementSetAttributeValue(window, kAXPositionAttribute, value);
    CFRelease(value);
    return result == kAXErrorSuccess;
}
static bool setSize(AXUIElementRef window, CGSize size) {
    AXValueRef value = AXValueCreate(kAXValueCGSizeType, &size);
    if (!value) return false;
    AXError result = AXUIElementSetAttributeValue(window, kAXSizeAttribute, value);
    CFRelease(value);
    return result == kAXErrorSuccess;
}

@interface RemoteWorkspace : NSObject {
@public
    AXUIElementRef target;
    CGRect original;
    CGWindowID windowID;
    pid_t pid;
    BOOL moved;
}
@property(nonatomic, strong) id display;
- (void)restore;
@end
@implementation RemoteWorkspace
- (void)restore {
    CGRect current;
    if (moved && target && windowBounds(windowID, pid, &current)) {
        setSize(target, original.size);
        setPosition(target, original.origin);
    }
    moved = NO;
    self.display = nil;
}
- (void)dealloc { [self restore]; if (target) CFRelease(target); }
@end

// Error codes: 1 permission, 2 target, 3 unavailable, 4 display creation,
// 5 layout, 6 non-HiDPI, 7 app refused move/resize.
void *remote_workspace_create(uint32_t windowID, int32_t pid, int32_t *error) {
    *error = 0;
    @autoreleasepool {
    RemoteWorkspace *workspace = [RemoteWorkspace new];
    @try {
        if (!AXIsProcessTrusted()) { *error = 1; return NULL; }
        workspace->windowID = windowID; workspace->pid = pid;
        if (!windowBounds(windowID, pid, &workspace->original) || workspace->original.size.width < 1 ||
            workspace->original.size.height < 1 || !(workspace->target = findWindow(pid, workspace->original))) {
            *error = 2; return NULL;
        }
        for (NSString *name in @[@"CGVirtualDisplay", @"CGVirtualDisplayDescriptor", @"CGVirtualDisplaySettings", @"CGVirtualDisplayMode"])
            if (!NSClassFromString(name)) { *error = 3; return NULL; }
        CGDirectDisplayID displays[32]; uint32_t count = 0;
        if (CGGetActiveDisplayList(32, displays, &count) != kCGErrorSuccess || !count) { *error = 5; return NULL; }
        double right = 0;
        for (uint32_t i = 0; i < count; i++) right = MAX(right, CGRectGetMaxX(CGDisplayBounds(displays[i])));
        if (!isfinite(right) || right > INT32_MAX - 1440) { *error = 5; return NULL; }
        id descriptor = [NSClassFromString(@"CGVirtualDisplayDescriptor") new];
        [descriptor setValue:@"MyRemoteApp HiDPI" forKey:@"name"];
        [descriptor setValue:@2880 forKey:@"maxPixelsWide"];
        [descriptor setValue:@2160 forKey:@"maxPixelsHigh"];
        [descriptor setValue:@0x4d52 forKey:@"vendorID"];
        [descriptor setValue:@1 forKey:@"productID"];
        [descriptor setValue:@(getpid()) forKey:@"serialNum"];
        if ([descriptor respondsToSelector:NSSelectorFromString(@"setSerialNumber:")])
            [descriptor setValue:@(getpid()) forKey:@"serialNumber"];
        [descriptor setValue:dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0) forKey:@"queue"];
        [descriptor setValue:[NSValue valueWithSize:NSMakeSize(330, 247.5)] forKey:@"sizeInMillimeters"];
        workspace.display = [[NSClassFromString(@"CGVirtualDisplay") alloc] initWithDescriptor:descriptor];
        id mode = [[NSClassFromString(@"CGVirtualDisplayMode") alloc] initWithWidth:1440 height:1080 refreshRate:60];
        id settings = [NSClassFromString(@"CGVirtualDisplaySettings") new];
        [settings setValue:@1 forKey:@"hiDPI"];
        [settings setValue:@[mode] forKey:@"modes"];
        if (!workspace.display || ![workspace.display applySettings:settings]) { *error = 4; return NULL; }
        CGDirectDisplayID displayID = [workspace.display displayID];
        for (int i = 0; i < 60 && !CGDisplayIsOnline(displayID); i++) usleep(50000);
        CGDisplayConfigRef config;
        if (CGBeginDisplayConfiguration(&config) != kCGErrorSuccess) { *error = 5; return NULL; }
        if (CGConfigureDisplayMirrorOfDisplay(config, displayID, kCGNullDirectDisplay) != kCGErrorSuccess ||
            CGConfigureDisplayOrigin(config, displayID, (int32_t)right, 0) != kCGErrorSuccess) {
            CGCancelDisplayConfiguration(config); *error = 5; return NULL;
        }
        if (CGCompleteDisplayConfiguration(config, kCGConfigureForSession) != kCGErrorSuccess) { *error = 5; return NULL; }
        CGDisplayModeRef actualMode = CGDisplayCopyDisplayMode(displayID);
        bool retina = actualMode && CGDisplayModeGetPixelWidth(actualMode) >= 2 * CGDisplayModeGetWidth(actualMode) &&
            CGDisplayModeGetPixelHeight(actualMode) >= 2 * CGDisplayModeGetHeight(actualMode) &&
            CGDisplayModeGetWidth(actualMode) >= 1400 && CGDisplayModeGetHeight(actualMode) >= 1080;
        if (actualMode) CGDisplayModeRelease(actualMode);
        if (!retina) { *error = 6; return NULL; }
        CGRect screen = CGDisplayBounds(displayID);
        double fit = MIN(1280.0 / workspace->original.size.width, 960.0 / workspace->original.size.height);
        CGSize size = CGSizeMake(round(workspace->original.size.width * fit), round(workspace->original.size.height * fit));
        CGPoint point = CGPointMake(screen.origin.x + 80, screen.origin.y + 60);
        workspace->moved = YES;
        if (!setPosition(workspace->target, point) || !setSize(workspace->target, size) || !setPosition(workspace->target, point)) {
            *error = 7; return NULL;
        }
        bool placed = false;
        for (int i = 0; i < 60; i++) {
            CGRect current;
            if (windowBounds(windowID, pid, &current) && CGRectContainsRect(screen, current) &&
                current.size.width >= size.width * 0.8 && current.size.height >= size.height * 0.8) {
                placed = true; break;
            }
            usleep(50000);
        }
        if (!placed) { *error = 7; return NULL; }
        return (__bridge_retained void *)workspace;
    } @catch (NSException *exception) {
        *error = 4;
        return NULL;
    }
    }
}
bool remote_workspace_ready(void *handle) {
    RemoteWorkspace *workspace = (__bridge RemoteWorkspace *)handle;
    if (!workspace || !workspace.display) return false;
    CGDirectDisplayID displayID = [workspace.display displayID];
    CGRect rect;
    return CGDisplayIsOnline(displayID) &&
        windowBounds(workspace->windowID, workspace->pid, &rect) &&
        CGRectContainsRect(CGDisplayBounds(displayID), rect);
}

void remote_workspace_destroy(void *handle) {
    RemoteWorkspace *workspace = (__bridge_transfer RemoteWorkspace *)handle;
    [workspace restore];
}


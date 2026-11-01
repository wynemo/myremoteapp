#import <AppKit/AppKit.h>
#include <stdint.h>
uint64_t remote_clipboard_version(void) {
    @autoreleasepool { return (uint64_t)[NSPasteboard generalPasteboard].changeCount; }
}

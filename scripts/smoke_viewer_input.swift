// Post input only into the synthetic viewer window owned by the supplied PID.
// Optional local regression check; requires Accessibility permission.
import AppKit
import ApplicationServices
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
guard AXIsProcessTrusted(), let app = NSRunningApplication(processIdentifier: pid) else {
    fputs("Synthetic input test requires Accessibility permission.\n", stderr)
    exit(1)
}
var rect: CGRect?
for _ in 0..<100 {
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    for window in windows {
        guard window[kCGWindowOwnerPID as String] as? Int == Int(pid),
              window[kCGWindowLayer as String] as? Int == 0,
              let bounds = window[kCGWindowBounds as String] as? [String: Any],
              let candidate = CGRect(dictionaryRepresentation: bounds as CFDictionary),
              candidate.width >= 100, candidate.height >= 100 else { continue }
        rect = candidate
    }
    if rect != nil { break }
    Thread.sleep(forTimeInterval: 0.05)
}
guard let rect = rect else { fatalError("Synthetic viewer window did not appear") }
app.activate(options: [])
Thread.sleep(forTimeInterval: 0.3)
let point = CGPoint(x: rect.midX, y: rect.midY + 10)
CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: point,
        mouseButton: .left)?.post(tap: .cghidEventTap)
for (down, up, button): (CGEventType, CGEventType, CGMouseButton) in [
    (.leftMouseDown, .leftMouseUp, .left),
    (.rightMouseDown, .rightMouseUp, .right),
] {
    CGEvent(mouseEventSource: nil, mouseType: down, mouseCursorPosition: point,
            mouseButton: button)?.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.1)
    CGEvent(mouseEventSource: nil, mouseType: up, mouseCursorPosition: point,
            mouseButton: button)?.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.1)
}

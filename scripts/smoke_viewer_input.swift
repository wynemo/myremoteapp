// Post input only into the synthetic viewer window owned by the supplied PID.
// Optional local regression check; requires Accessibility permission.
import AppKit
import ApplicationServices
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
let testKeyboard = CommandLine.arguments.contains("--keyboard")
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

if testKeyboard {
    func key(_ code: CGKeyCode, _ down: Bool, _ flags: CGEventFlags = [], repeatKey: Bool = false) {
        guard let event = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down) else {
            fatalError("Cannot create test key event")
        }
        event.flags = flags
        event.setIntegerValueField(.keyboardEventAutorepeat, value: repeatKey ? 1 : 0)
        event.post(tap: .cghidEventTap)
        Thread.sleep(forTimeInterval: 0.05)
    }
    key(0x3b, true, .maskControl)
    key(0x00, true, .maskControl)
    key(0x00, true, .maskControl, repeatKey: true)
    key(0x00, false, .maskControl)
    key(0x3b, false)
    key(0x35, true) // Escape must reach the server rather than close the viewer.
    key(0x35, false)
    key(0x37, true, .maskCommand)
    key(0x00, true, .maskCommand)
    key(0x00, false, .maskCommand)
    key(0x37, false)
    key(0x37, true, .maskCommand)
    key(0x0c, true, .maskCommand) // Command-Q must go remote, not quit locally.
    key(0x0c, false, .maskCommand)
    key(0x37, false)
    key(0x3b, true, .maskControl)
    // Only hide the synthetic viewer; its focus loss must release remote Ctrl.
    app.hide()
    Thread.sleep(forTimeInterval: 0.2)
    key(0x3b, false)
}

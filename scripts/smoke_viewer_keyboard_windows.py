"""Post key messages only to the synthetic viewer process created by the smoke test."""
import ctypes
from ctypes import wintypes
import time


def send_test_keys(pid):
    user = ctypes.WinDLL("user32", use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
    user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user.IsWindowVisible.argtypes = [wintypes.HWND]
    user.SetForegroundWindow.argtypes = [wintypes.HWND]
    user.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
    user.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    found = []

    @callback_type
    def visit(window, _):
        owner = wintypes.DWORD()
        user.GetWindowThreadProcessId(window, ctypes.byref(owner))
        if owner.value == pid and user.IsWindowVisible(window):
            found.append(window)
        return True

    deadline = time.monotonic() + 10
    while not found and time.monotonic() < deadline:
        user.EnumWindows(visit, 0)
        time.sleep(0.05)
    if not found:
        raise RuntimeError("Synthetic viewer window did not appear")
    window = found[0]
    user.SetForegroundWindow(window)
    time.sleep(0.2)

    def key(vk, scan, down, extended=False, repeat=False, system=False):
        bits = 1 | (scan << 16) | (int(extended) << 24)
        if repeat:
            bits |= 1 << 30
        if not down:
            bits |= (1 << 30) | (1 << 31)
        message = (0x104 if down else 0x105) if system else (0x100 if down else 0x101)
        if not user.PostMessageW(window, message, vk, bits):
            raise ctypes.WinError(ctypes.get_last_error())
        time.sleep(0.05)

    key(0xa2, 0x1d, True)
    key(0x41, 0x1e, True)
    key(0x41, 0x1e, True, repeat=True)
    key(0x41, 0x1e, False)
    key(0xa2, 0x1d, False)
    key(0x1b, 0x01, True)
    key(0x1b, 0x01, False)
    key(0x5b, 0x5b, True, extended=True)
    key(0x41, 0x1e, True)
    key(0x41, 0x1e, False)
    key(0x5b, 0x5b, False, extended=True)
    key(0x5b, 0x5b, True, extended=True)
    key(0x51, 0x10, True)
    key(0x51, 0x10, False)
    key(0x5b, 0x5b, False, extended=True)
    key(0x12, 0x38, True, system=True)
    key(0x25, 0x4b, True, extended=True, system=True)
    key(0x25, 0x4b, False, extended=True, system=True)
    key(0x12, 0x38, False, system=True)
    key(0x12, 0x38, True, extended=True, system=True)
    key(0x12, 0x38, False, extended=True, system=True)
    # Leave Ctrl held, then hide only this fixture to exercise focus release.
    key(0xa2, 0x1d, True)
    user.ShowWindow(window, 0)

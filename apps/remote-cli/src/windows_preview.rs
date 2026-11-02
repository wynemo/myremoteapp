//! Keep rendering/input in physical pixels and desktop sizing in logical DIPs.
use anyhow::{Result, ensure};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    UI::{
        HiDpi::{
            AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
            SetThreadDpiAwarenessContext,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

pub struct DpiContext(DPI_AWARENESS_CONTEXT);
impl DpiContext {
    pub fn enter() -> Result<Self> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        ensure!(!previous.is_null(), "无法启用客户端 DPI 缩放");
        Ok(Self(previous))
    }
}
impl Drop for DpiContext {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

pub fn logical_client_size(window: &minifb::Window) -> (usize, usize) {
    let dpi = unsafe { GetDpiForWindow(window.get_window_handle()) };
    crate::display_resize::logical_size(window.get_size(), dpi)
}

pub fn display_metrics(window: &minifb::Window) -> Result<((usize, usize), u32)> {
    let handle = window.get_window_handle();
    let dpi = unsafe { GetDpiForWindow(handle) }.max(96);
    let mut monitor = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe {
            GetMonitorInfoW(
                MonitorFromWindow(handle, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
        } != 0,
        "无法读取客户端显示器尺寸"
    );
    Ok((
        (
            (monitor.rcMonitor.right - monitor.rcMonitor.left) as usize,
            (monitor.rcMonitor.bottom - monitor.rcMonitor.top) as usize,
        ),
        (dpi * 100 + 48) / 96,
    ))
}

pub fn configure(window: &minifb::Window, pixels: (usize, usize)) -> Result<()> {
    let handle = window.get_window_handle();
    let dpi = unsafe { GetDpiForWindow(handle) }.max(96);
    let mut monitor = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe {
            GetMonitorInfoW(
                MonitorFromWindow(handle, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
        } != 0,
        "无法读取客户端屏幕工作区"
    );
    let style = unsafe { GetWindowLongW(handle, GWL_STYLE) } as u32;
    let ex_style = unsafe { GetWindowLongW(handle, GWL_EXSTYLE) } as u32;
    let mut chrome = RECT::default();
    ensure!(
        unsafe { AdjustWindowRectExForDpi(&mut chrome, style, 0, ex_style, dpi) } != 0,
        "无法读取客户端窗口边框"
    );
    let border = (chrome.right - chrome.left, chrome.bottom - chrome.top);
    let work = monitor.rcWork;
    let physical = (pixels.0 * dpi as usize / 96, pixels.1 * dpi as usize / 96);
    let size = crate::display_resize::fit_size(
        physical,
        (
            (work.right - work.left - border.0).max(1) as usize,
            (work.bottom - work.top - border.1).max(1) as usize,
        ),
    );
    let outer = (size.0 as i32 + border.0, size.1 as i32 + border.1);
    ensure!(
        unsafe {
            SetWindowPos(
                handle,
                std::ptr::null_mut(),
                work.left + (work.right - work.left - outer.0) / 2,
                work.top + (work.bottom - work.top - outer.1) / 2,
                outer.0,
                outer.1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        } != 0,
        "无法适配客户端窗口大小"
    );
    ensure!(
        unsafe { SetWindowSubclass(handle, Some(dpi_changed), 0x445049, 0) } != 0,
        "无法启用客户端跨屏 DPI 适配"
    );
    Ok(())
}

unsafe extern "system" fn dpi_changed(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_DPICHANGED && lparam != 0 {
        // Windows supplies the new physical rectangle preserving logical size.
        let rect = unsafe { &*(lparam as *const RECT) };
        unsafe {
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        return 0;
    }
    if message == WM_NCDESTROY {
        unsafe {
            RemoveWindowSubclass(window, Some(dpi_changed), id);
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

//! A virtual display is owned by the sharing session, including capture restarts.
//! This module is main-thread only: Rc keeps the display alive across lock/unlock.
use anyhow::{Result, bail, ensure};
use std::{ffi::c_void, ptr::NonNull};

unsafe extern "C" {
    fn remote_workspace_create(window_id: u32, pid: i32, error: *mut i32) -> *mut c_void;
    fn remote_workspace_ready(workspace: *mut c_void) -> bool;
    fn remote_workspace_destroy(workspace: *mut c_void);
}

pub struct VirtualWorkspace(NonNull<c_void>);

impl VirtualWorkspace {
    pub fn create(window_id: u32, pid: i32) -> Result<Self> {
        let mut error = 0;
        // Native code validates the window owner and unique AX window before
        // moving it; failed setup restores the window and releases the display.
        let handle = unsafe { remote_workspace_create(window_id, pid, &mut error) };
        let Some(handle) = NonNull::new(handle) else {
            let reason = match error {
                1 => "需要辅助功能权限才能移动共享窗口",
                2 => "无法唯一定位目标窗口，窗口可能已关闭或不支持辅助功能",
                3 => "当前 macOS 不支持创建虚拟显示器",
                4 => "创建虚拟显示器失败",
                5 => "无法配置虚拟显示器位置",
                6 => "系统未提供所需的 2 倍 HiDPI 模式",
                7 => "目标窗口未能移入虚拟显示器，具体位置与设置结果见上方日志",
                _ => "虚拟显示器初始化失败",
            };
            bail!("{reason}；未启动虚拟屏共享");
        };
        Ok(Self(handle))
    }

    pub fn ensure_ready(&self) -> Result<()> {
        ensure!(
            unsafe { remote_workspace_ready(self.0.as_ptr()) },
            "虚拟 HiDPI 显示器或目标窗口不可用；请确认目标窗口仍在虚拟屏上"
        );
        Ok(())
    }
}

impl Drop for VirtualWorkspace {
    fn drop(&mut self) {
        // The last CaptureSession restores the target window before disposing
        // the virtual display. Never changes a physical display's mode.
        unsafe { remote_workspace_destroy(self.0.as_ptr()) };
    }
}

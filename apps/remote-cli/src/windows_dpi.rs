//! Windows has no documented per-monitor DPI setter. These runtime-checked
//! DisplayConfig packets (-3/-4) are also used by Windows DPI helper projects.
//! Unsupported systems fail explicitly; never substitute bitmap enlargement.
use anyhow::{Result, bail, ensure};
use std::mem::size_of;
use windows_sys::Win32::{Devices::Display::*, Foundation::ERROR_INSUFFICIENT_BUFFER};

#[repr(C)]
#[derive(Default)]
struct DpiGet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    minimum: i32,
    current: i32,
    maximum: i32,
}
#[repr(C)]
struct DpiSet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    relative: i32,
}
const _: () = assert!(size_of::<DpiGet>() == 32 && size_of::<DpiSet>() == 24);

fn source(device: &[u16; 32]) -> Result<DISPLAYCONFIG_DEVICE_INFO_HEADER> {
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0, 0);
        let result = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        ensure!(
            result == 0 && path_count > 0 && path_count <= 64 && mode_count <= 256,
            "Windows 显示驱动不支持 DPI 协商（查询显示路径返回 {result}）"
        );
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        ensure!(result == 0, "无法读取 Windows DPI 显示路径：{result}");
        for path in paths.iter().take(path_count as usize) {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } == 0
                && name.viewGdiDeviceName == *device
            {
                return Ok(name.header);
            }
        }
        bail!("无法匹配 Windows 主显示器的 DPI 配置");
    }
    bail!("Windows 显示路径正在变化，请重试连接")
}

fn read(device: &[u16; 32]) -> Result<DpiGet> {
    let mut packet = DpiGet {
        header: source(device)?,
        ..Default::default()
    };
    packet.header.r#type = -3;
    packet.header.size = size_of::<DpiGet>() as u32;
    let result = unsafe { DisplayConfigGetDeviceInfo(&mut packet.header) };
    ensure!(
        result == 0,
        "Windows 不支持读取桌面 DPI（系统返回 {result}）"
    );
    crate::display_resize::dpi_percent(packet.minimum, packet.current, packet.maximum)?;
    Ok(packet)
}

fn percent(packet: &DpiGet) -> Result<u32> {
    crate::display_resize::dpi_percent(packet.minimum, packet.current, packet.maximum)
}

pub fn current_scale(device: &[u16; 32]) -> Result<u32> {
    percent(&read(device)?)
}

fn set(device: &[u16; 32], scale: u32) -> Result<u32> {
    let active = read(device)?;
    if percent(&active)? == scale {
        return Ok(scale);
    }
    let relative = crate::display_resize::dpi_relative(active.minimum, active.maximum, scale)?;
    let mut packet = DpiSet {
        header: active.header,
        relative,
    };
    packet.header.r#type = -4;
    packet.header.size = size_of::<DpiSet>() as u32;
    let result = unsafe { DisplayConfigSetDeviceInfo(&packet.header) };
    ensure!(
        result == 0,
        "Windows 拒绝设置桌面缩放 {scale}%（系统返回 {result}）"
    );
    let actual = percent(&read(device)?)?;
    ensure!(
        actual == scale,
        "Windows DPI 未生效：请求 {scale}%，实际 {actual}%"
    );
    Ok(actual)
}

#[derive(Default)]
pub struct DesktopDpi {
    original: Option<([u16; 32], u32)>,
    applied: Option<u32>,
}
impl DesktopDpi {
    pub fn remember(&mut self, device: &[u16; 32]) -> Result<()> {
        if let Some((saved, _)) = self.original {
            ensure!(saved == *device, "控制期间主显示器已变化，请重新连接");
        } else {
            self.original = Some((*device, percent(&read(device)?)?));
        }
        Ok(())
    }
    pub fn apply(&mut self, device: &[u16; 32], scale: u32) -> Result<u32> {
        self.remember(device)?;
        // Save expected state before writing, so even failed verification can restore.
        self.applied = Some(scale);
        set(device, scale)
    }
}
impl Drop for DesktopDpi {
    fn drop(&mut self) {
        if let Some((device, original)) = self.original.take()
            && let Some(applied) = self.applied
        {
            match read(&device).and_then(|active| percent(&active)) {
                Ok(current) if current == applied => {
                    if let Err(error) = set(&device, original) {
                        eprintln!("恢复 Windows 原 DPI 失败：{error:#}");
                    }
                }
                Ok(_) => {} // Preserve a subsequent local display change.
                Err(error) => eprintln!("无法确认 Windows 原 DPI 恢复条件：{error:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "changes desktop DPI; disposable Windows runner only"]
    fn native_dpi_two_hundred_percent_and_restore() {
        let _context = crate::windows_preview::DpiContext::enter().unwrap();
        let device = crate::windows_display::primary().unwrap();
        let original = percent(&read(&device).unwrap()).unwrap();
        let mut dpi = DesktopDpi::default();
        dpi.remember(&device).unwrap();
        let mut resolution = crate::windows_display::DesktopResolution::default();
        // Raise the physical resolution first so 200% is a supported scale.
        resolution.resize(2560, 1440).unwrap();
        {
            dpi.apply(&device, 200).unwrap();
            assert_eq!(percent(&read(&device).unwrap()).unwrap(), 200);
            let window =
                minifb::Window::new("MyRemoteApp DPI fixture", 640, 480, Default::default())
                    .unwrap();
            assert_eq!(
                unsafe {
                    windows_sys::Win32::UI::HiDpi::GetDpiForWindow(window.get_window_handle())
                },
                192,
                "a new native window must actually receive 192 DPI"
            );
            dpi.apply(&device, 100).unwrap();
            dpi.apply(&device, 200).unwrap();
        }
        drop(resolution);
        drop(dpi);
        assert_eq!(percent(&read(&device).unwrap()).unwrap(), original);
    }
}

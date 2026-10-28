use std::collections::VecDeque;

/// Physical-pixel bounds of the Windows virtual screen, including negative origins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl DesktopRect {
    pub fn frame_size(self) -> (u32, u32) {
        let width = f64::from(self.width.max(1));
        let height = f64::from(self.height.max(1));
        let scale = (2560.0 / width.max(height))
            .min(1440.0 / width.min(height))
            .min(1.0);
        // Floor avoids exceeding the protocol's pixel budget after rounding.
        (
            (width * scale).max(1.0) as u32,
            (height * scale).max(1.0) as u32,
        )
    }
}

/// Frame coordinates are valid only for the layout that was actually captured.
#[derive(Default)]
pub struct DesktopFrames(VecDeque<(u64, DesktopRect)>);
impl DesktopFrames {
    pub fn layout_changed(&self, rect: DesktopRect) -> bool {
        self.0.back().is_some_and(|(_, previous)| *previous != rect)
    }
    pub fn record(&mut self, sequence: u64, rect: DesktopRect) {
        if self.layout_changed(rect) {
            self.clear();
        }
        self.0.push_back((sequence, rect));
        while self.0.len() > 32 {
            self.0.pop_front();
        }
    }
    pub fn get(&self, sequence: u64) -> Option<DesktopRect> {
        self.0
            .iter()
            .find(|(id, _)| *id == sequence)
            .map(|(_, rect)| *rect)
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_frames_are_invalidated_by_layout_changes_and_pauses() {
        let left = DesktopRect {
            x: -1920,
            y: 0,
            width: 3840,
            height: 1080,
        };
        let right = DesktopRect { x: 0, ..left };
        let mut frames = DesktopFrames::default();
        frames.record(0, left);
        assert_eq!(frames.get(0), Some(left));
        assert!(!frames.layout_changed(left));
        assert!(frames.layout_changed(right));
        frames.record(1, right);
        assert_eq!(frames.get(0), None);
        assert_eq!(frames.get(1), Some(right));
        for sequence in 2..34 {
            frames.record(sequence, right);
        }
        assert_eq!(frames.get(1), None);
        assert_eq!(frames.get(33), Some(right));
        frames.clear();
        assert_eq!(frames.get(33), None);
    }

    #[test]
    fn full_desktop_fits_protocol_in_all_layouts() {
        for (w, h, expected) in [
            (1920, 1080, (1920, 1080)),
            (3840, 2160, (2560, 1440)),
            (2160, 3840, (1440, 2560)),
            (7680, 2160, (2560, 720)),
            (2000, 2000, (1440, 1440)),
        ] {
            let desktop = DesktopRect {
                x: -1920,
                y: -1080,
                width: w,
                height: h,
            };
            let size = desktop.frame_size();
            assert_eq!(size, expected);
            remote_protocol::validate_frame_size(size.0, size.1).unwrap();
        }
    }
}

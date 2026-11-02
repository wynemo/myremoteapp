use std::time::{Duration, Instant};

/// Windows reports physical client pixels under Per-Monitor V2 awareness.
/// macOS reports view bounds in points and must not be divided a second time.
#[cfg(any(target_os = "windows", test))]
pub fn logical_size(pixels: (usize, usize), dpi: u32) -> (usize, usize) {
    let dpi = u64::from(dpi.max(96));
    let convert = |value: usize| ((value as u64 * 96 + dpi / 2) / dpi) as usize;
    (convert(pixels.0), convert(pixels.1))
}

/// Fit received pixel dimensions into the local work area without changing aspect.
pub fn fit_size(size: (usize, usize), available: (usize, usize)) -> (usize, usize) {
    let scale = (available.0 as f64 / size.0.max(1) as f64)
        .min(available.1 as f64 / size.1.max(1) as f64)
        .min(1.0);
    (
        (size.0 as f64 * scale).floor().max(1.0) as usize,
        (size.1 as f64 * scale).floor().max(1.0) as usize,
    )
}

/// Wait for live resize to settle; never feed received frame sizes back to the host.
#[derive(Default)]
pub struct ResizeTracker {
    observed: Option<((usize, usize), Instant)>,
    sent: Option<(usize, usize)>,
}

impl ResizeTracker {
    pub fn update(&mut self, size: (usize, usize), now: Instant) -> Option<(u32, u32)> {
        if size.0 < 320 || size.1 < 200 || size.0 > 8192 || size.1 > 8192 {
            self.observed = None;
            return None;
        }
        let (_, since) = *self.observed.get_or_insert((size, now));
        if self.observed.unwrap().0 != size {
            self.observed = Some((size, now));
            return None;
        }
        if self.sent == Some(size) || now.duration_since(since) < Duration::from_millis(500) {
            return None;
        }
        self.sent = Some(size);
        Some((size.0 as u32, size.1 as u32))
    }
}

/// Compare relative width and height, so aspect ratio and readable scale both matter.
#[cfg(any(target_os = "windows", test))]
pub fn mode_distance(size: (u32, u32), target: (u32, u32)) -> f64 {
    (f64::from(size.0) / f64::from(target.0)).ln().powi(2)
        + (f64::from(size.1) / f64::from(target.1)).ln().powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_k_at_two_hundred_percent_is_full_hd_logical_space() {
        assert_eq!(logical_size((3840, 2160), 192), (1920, 1080));
        assert_eq!(logical_size((3840, 2160), 144), (2560, 1440));
        assert_eq!(logical_size((1920, 1080), 96), (1920, 1080));
        assert_eq!(fit_size((3840, 2160), (1920, 1000)), (1777, 1000));
        assert_eq!(fit_size((1080, 1920), (1920, 1000)), (562, 1000));
    }

    #[test]
    fn sends_only_settled_sizes_and_ignores_minimize() {
        let mut tracker = ResizeTracker::default();
        let now = Instant::now();
        assert_eq!(tracker.update((1280, 720), now), None);
        assert_eq!(
            tracker.update((1280, 720), now + Duration::from_millis(499)),
            None
        );
        assert_eq!(
            tracker.update((1280, 720), now + Duration::from_millis(500)),
            Some((1280, 720))
        );
        assert_eq!(
            tracker.update((1280, 720), now + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            tracker.update((900, 700), now + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            tracker.update((960, 720), now + Duration::from_millis(2400)),
            None
        );
        assert_eq!(
            tracker.update((960, 720), now + Duration::from_millis(2900)),
            Some((960, 720))
        );
        assert_eq!(tracker.update((0, 0), now + Duration::from_secs(3)), None);
    }

    #[test]
    fn selects_exact_mode_or_nearest_supported_dimensions() {
        let modes = [(800, 600), (1280, 720), (1280, 800), (1920, 1080)];
        let select = |target| {
            *modes
                .iter()
                .min_by(|a, b| mode_distance(**a, target).total_cmp(&mode_distance(**b, target)))
                .unwrap()
        };
        assert_eq!(select((1280, 720)), (1280, 720));
        assert_eq!(select((1300, 810)), (1280, 800));
        assert_eq!(select((1800, 1000)), (1920, 1080));
    }
}

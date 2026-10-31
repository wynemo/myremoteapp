use anyhow::{Result, ensure};
use std::borrow::Cow;

// Used only when the display driver cannot perform the requested GDI stretch.
// Equal-sized frames borrow their original DIB without an extra pixel copy.
pub fn resize(
    pixels: &[u8],
    width: usize,
    height: usize,
    output_width: usize,
    output_height: usize,
) -> Result<Cow<'_, [u8]>> {
    ensure!(
        width > 0 && height > 0 && output_width > 0 && output_height > 0,
        "Invalid capture dimensions"
    );
    let length = width.checked_mul(height).and_then(|n| n.checked_mul(4));
    ensure!(length == Some(pixels.len()), "Invalid BGRA capture buffer");
    ensure!(
        output_width <= width && output_height <= height,
        "Capture resize cannot upscale"
    );
    if (width, height) == (output_width, output_height) {
        return Ok(Cow::Borrowed(pixels));
    }
    let mut output = vec![0; output_width * output_height * 4];
    for y in 0..output_height {
        let sy = ((y as f64 + 0.5) * height as f64 / output_height as f64 - 0.5).max(0.0);
        let y0 = (sy as usize).min(height - 1);
        let y1 = (y0 + 1).min(height - 1);
        let fy = sy - y0 as f64;
        for x in 0..output_width {
            let sx = ((x as f64 + 0.5) * width as f64 / output_width as f64 - 0.5).max(0.0);
            let x0 = (sx as usize).min(width - 1);
            let x1 = (x0 + 1).min(width - 1);
            let fx = sx - x0 as f64;
            for channel in 0..4 {
                let a = pixels[(y0 * width + x0) * 4 + channel] as f64;
                let b = pixels[(y0 * width + x1) * 4 + channel] as f64;
                let c = pixels[(y1 * width + x0) * 4 + channel] as f64;
                let d = pixels[(y1 * width + x1) * 4 + channel] as f64;
                output[(y * output_width + x) * 4 + channel] =
                    ((a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy)
                        .round() as u8;
            }
        }
    }
    Ok(Cow::Owned(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_size_borrows_and_downscale_preserves_bgra_channels() {
        let pixels = [
            0, 10, 20, 255, 40, 50, 60, 255, 80, 90, 100, 255, 120, 130, 140, 255,
        ];
        assert!(matches!(
            resize(&pixels, 2, 2, 2, 2).unwrap(),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            resize(&pixels, 2, 2, 1, 1).unwrap().as_ref(),
            &[60, 70, 80, 255]
        );
    }
    #[test]
    fn single_axis_and_invalid_buffers_are_handled() {
        assert_eq!(
            resize(&[0, 10, 20, 255, 40, 50, 60, 255], 1, 2, 1, 1)
                .unwrap()
                .as_ref(),
            &[20, 30, 40, 255]
        );
        assert!(resize(&[], 1, 1, 1, 1).is_err());
        assert!(resize(&[0; 4], 1, 1, 0, 1).is_err());
        assert!(resize(&[0; 4], 1, 1, 2, 1).is_err());
    }
}

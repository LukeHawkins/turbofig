//! Screenshot PNG handling: a cheap dimension peek, and the expensive
//! decode/resize/encode path, kept separate so callers can skip the
//! expensive path whenever no resize is actually needed.

/// Read a PNG's width/height from its header only, without decoding pixel
/// data. Returns `None` when the bytes are not a readable image.
pub(crate) fn probe_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Decode, Lanczos3-resize so the longest edge is at most `max_dim`, and
/// re-encode as PNG.
///
/// This does a full pixel decode: callers on an async runtime should run it
/// inside `spawn_blocking`. Call this only when a resize is actually needed
/// (`probe_dims` has already shown the longest edge exceeds `max_dim`).
/// Falls back to the original bytes (dims 0,0) if decode or encode fails.
pub(crate) fn resize_png(bytes: &[u8], max_dim: u32) -> (Vec<u8>, u32, u32) {
    let max_dim = max_dim.max(1);
    let img = match image::load_from_memory(bytes) {
        Ok(img) => img,
        Err(_) => return (bytes.to_vec(), 0, 0),
    };
    let resized = img.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3);
    let new_w = resized.width();
    let new_h = resized.height();
    let mut out: Vec<u8> = Vec::new();
    if resized
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .is_err()
    {
        return (bytes.to_vec(), img.width(), img.height());
    }
    (out, new_w, new_h)
}

/// Test-only convenience that mirrors the old monolithic `downscale_png`:
/// probe then resize only if needed. Production code (`ops/screenshot.rs`)
/// calls `probe_dims` and `resize_png` directly so it can skip the decode
/// entirely, and run the resize on a blocking thread.
#[cfg(test)]
pub(crate) fn downscale_png(bytes: &[u8], max_dim: u32, full_res: bool) -> (Vec<u8>, u32, u32) {
    let max_dim = max_dim.max(1);
    match probe_dims(bytes) {
        None => (bytes.to_vec(), 0, 0),
        Some((w, h)) if full_res || w.max(h) <= max_dim => (bytes.to_vec(), w, h),
        Some(_) => resize_png(bytes, max_dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = ::image::RgbaImage::new(width, height);
        let mut buf: Vec<u8> = Vec::new();
        ::image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut buf),
                ::image::ImageFormat::Png,
            )
            .expect("encode test PNG");
        buf
    }

    #[test]
    fn downscale_png_max_dim_zero_does_not_panic_and_produces_valid_output() {
        let png = make_png(100, 50);
        let (out_bytes, w, h) = downscale_png(&png, 0, false);
        assert!(
            w >= 1,
            "width must be >= 1 after clamping max_dim=0, got {w}"
        );
        assert!(
            h >= 1,
            "height must be >= 1 after clamping max_dim=0, got {h}"
        );
        ::image::load_from_memory(&out_bytes).expect("downscaled result must be a valid PNG");
    }

    #[test]
    fn downscale_png_reduces_large_image_to_max_dim() {
        let png = make_png(2000, 1000);
        let (out_bytes, w, h) = downscale_png(&png, 1200, false);
        assert_eq!(w, 1200, "width must equal max_dim");
        assert_eq!(h, 600, "height must be halved proportionally");
        let decoded = ::image::load_from_memory(&out_bytes).expect("decode result");
        assert_eq!(decoded.width(), 1200);
        assert_eq!(decoded.height(), 600);
    }

    #[test]
    fn downscale_png_reduces_portrait_image_to_max_dim() {
        let png = make_png(1000, 2000);
        let (out_bytes, w, h) = downscale_png(&png, 1200, false);
        assert_eq!(w, 600, "width must be halved proportionally");
        assert_eq!(h, 1200, "height must equal max_dim");
        let decoded = ::image::load_from_memory(&out_bytes).expect("decode result");
        assert_eq!(decoded.width(), 600);
        assert_eq!(decoded.height(), 1200);
    }

    #[test]
    fn downscale_png_full_res_returns_input_unchanged() {
        let png = make_png(2000, 1000);
        let (out_bytes, w, h) = downscale_png(&png, 1200, true);
        assert_eq!(out_bytes, png, "bytes must be unchanged for full_res=true");
        assert_eq!(w, 2000);
        assert_eq!(h, 1000);
    }

    #[test]
    fn downscale_png_small_image_is_not_upscaled() {
        let png = make_png(100, 50);
        let (out_bytes, w, h) = downscale_png(&png, 1200, false);
        assert_eq!(
            out_bytes, png,
            "bytes must be unchanged when image fits inside max_dim"
        );
        assert_eq!(w, 100);
        assert_eq!(h, 50);
    }

    #[test]
    fn downscale_png_invalid_bytes_pass_through_with_zero_dims() {
        let bad = b"hello";
        let (out_bytes, w, h) = downscale_png(bad, 1200, false);
        assert_eq!(out_bytes, bad, "bytes must be unchanged for invalid PNG");
        assert_eq!(w, 0);
        assert_eq!(h, 0);
    }

    #[test]
    fn probe_dims_reads_header_only() {
        let png = make_png(42, 7);
        assert_eq!(probe_dims(&png), Some((42, 7)));
    }

    #[test]
    fn probe_dims_returns_none_for_invalid_bytes() {
        assert_eq!(probe_dims(b"not a png"), None);
    }
}

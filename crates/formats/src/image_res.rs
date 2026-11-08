//! Decoding imported image resources (fills, alphas, stencils).

use crate::{FormatError, MAX_IMAGE_SIZE, Result};

/// A decoded image: RGBA as 0..1 floats in the file's own encoding (8/16-bit files stay
/// sRGB-encoded; `srgb` says so), row-major, top row first.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[f32; 4]>,
    /// True for 8/16-bit images (sRGB-encoded); false for float images (linear).
    pub srgb: bool,
}

/// Most texels [`decode_image`] accepts: 8192 × 8192, the largest texture set resolution
/// (`texturecraft_core::MAX_RESOLUTION`). Decoded as RGBA f32 that is 1 GiB; a larger
/// resource adds no detail to any texture set.
pub const MAX_RESOURCE_TEXELS: u64 = 1 << 26;

/// Decode PNG, JPEG, TGA, TIFF, BMP, WEBP, EXR or HDR bytes of at most
/// [`MAX_RESOURCE_TEXELS`] texels (refused from the header, before decoding).
pub fn decode_image(bytes: &[u8]) -> Result<DecodedImage> {
    decode_image_within(bytes, MAX_RESOURCE_TEXELS)
}

/// [`decode_image`] for an image of at most `max_texels` (width × height), checked from the
/// header, before any pixel is decoded or allocated.
pub fn decode_image_within(bytes: &[u8], max_texels: u64) -> Result<DecodedImage> {
    header_dimensions_within(bytes, max_texels)?;
    let img = crate::guard("image", || image::load_from_memory(bytes).map_err(|e| FormatError::Malformed { kind: "image", msg: e.to_string() }))?;
    let srgb = !matches!(img, image::DynamicImage::ImageRgb32F(_) | image::DynamicImage::ImageRgba32F(_));
    let f = img.into_rgba32f();
    let (width, height) = f.dimensions();
    Ok(DecodedImage { width, height, rgba: pixels(f.into_raw()), srgb })
}

/// RGBA samples as pixels, reusing the decoder's buffer rather than copying it: at the texel cap
/// that buffer is 1 GiB, and a copy would double the peak.
fn pixels(samples: Vec<f32>) -> Vec<[f32; 4]> {
    bytemuck::try_cast_vec(samples).unwrap_or_else(|(_, samples)| {
        // Only if the allocation can't be reused as-is (never for RGBA f32 from `image`).
        samples.as_chunks::<4>().0.to_vec()
    })
}

/// The image's dimensions read from its header, refused if either edge exceeds
/// [`MAX_IMAGE_SIZE`] or width × height exceeds `max_texels`. Decodes no pixels.
fn header_dimensions_within(bytes: &[u8], max_texels: u64) -> Result<(u32, u32)> {
    let reader =
        image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| FormatError::Malformed { kind: "image", msg: e.to_string() })?;
    let (w, h) = reader.into_dimensions().map_err(|e| FormatError::Malformed { kind: "image", msg: e.to_string() })?;
    if w == 0 || h == 0 || w > MAX_IMAGE_SIZE || h > MAX_IMAGE_SIZE {
        return Err(FormatError::TooLarge(format!("image is {w}x{h} (limit {MAX_IMAGE_SIZE})")));
    }
    if u64::from(w) * u64::from(h) > max_texels {
        return Err(FormatError::TooLarge(format!("image is {w}x{h}, more than {max_texels} texels; scale it down and import it again")));
    }
    Ok((w, h))
}

/// PNG bytes of an `rgb` (sRGB) image whose alpha is `coverage` (`width × height`, top row first):
/// drawn text as an image resource.
pub fn coverage_png(width: u32, height: u32, coverage: &[u8], rgb: [u8; 3]) -> Result<Vec<u8>> {
    let rgba: Vec<u8> = coverage.iter().flat_map(|a| [rgb[0], rgb[1], rgb[2], *a]).collect();
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| FormatError::Invalid(format!("{} coverage values for {width}x{height}", coverage.len())))?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| FormatError::Invalid(e.to_string()))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_image_refuses_resources_over_the_texel_cap_from_the_header() {
        // 8193 × 8192 greyscale: one column over 2^26 texels. Encoded as a PNG this is small;
        // decoding it would take over 1 GiB.
        let png = {
            let mut b = Vec::new();
            image::GrayImage::new(8193, 8192).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
            b
        };
        assert!(png.len() < 1 << 20, "the test image compresses well ({} bytes)", png.len());
        assert!(matches!(decode_image(&png), Err(FormatError::TooLarge(_))));
    }

    #[test]
    fn a_resource_at_the_cap_edge_passes_the_header_check() {
        // 2^26 texels exactly, as a 16384 × 4096 strip: the check is on texels, not edge length.
        let png = {
            let mut b = Vec::new();
            image::GrayImage::new(16_384, 4096).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
            b
        };
        // The header check is all that stands between this image and decoding, so test it
        // directly: decoding it here would hold two 1 GiB buffers at once.
        assert_eq!(header_dimensions_within(&png, MAX_RESOURCE_TEXELS).unwrap(), (16_384, 4096));
    }

    #[test]
    fn samples_become_pixels_in_order_with_or_without_a_copy() {
        let samples: Vec<f32> = (0..8u8).map(f32::from).collect();
        let ptr = samples.as_ptr().cast::<u8>();
        let px = pixels(samples);
        assert_eq!(px, [[0.0, 1.0, 2.0, 3.0], [4.0, 5.0, 6.0, 7.0]]);
        assert_eq!(px.as_ptr().cast::<u8>(), ptr, "the decoder's buffer is reused, not copied");
        // A length that isn't whole pixels can't be reused: copied, the stray sample dropped.
        assert_eq!(pixels((0..5u8).map(f32::from).collect()), [[0.0, 1.0, 2.0, 3.0]]);
    }

    #[test]
    fn decodes_png_and_rejects_garbage() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_fn(4, 2, |x, _| image::Rgba([x as u8 * 60, 0, 0, 255]));
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let d = decode_image(&png).unwrap();
        assert_eq!((d.width, d.height), (4, 2));
        assert!(d.srgb);
        assert!((d.rgba[1][0] - 60.0 / 255.0).abs() < 1e-3);
        assert!(decode_image(b"not an image").is_err());
        assert!(decode_image(&png[..png.len() / 2]).is_err());
        let text = decode_image(&coverage_png(2, 1, &[0, 255], [255; 3]).unwrap()).unwrap();
        assert_eq!(text.rgba, vec![[1.0, 1.0, 1.0, 0.0], [1.0; 4]]);
        assert!(coverage_png(2, 2, &[0], [255; 3]).is_err());
    }

    #[test]
    fn decoding_within_a_texel_limit_refuses_big_images_from_the_header() {
        let png = {
            let mut b = Vec::new();
            image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3])).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
            b
        };
        assert_eq!(decode_image_within(&png, 64).unwrap().width, 8);
        assert!(matches!(decode_image_within(&png, 63), Err(FormatError::TooLarge(_))));
        assert!(decode_image(&png).is_ok());
    }
}

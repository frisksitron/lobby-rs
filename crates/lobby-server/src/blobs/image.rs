//! Chat previews and Member avatars and Lobby icons. Pure CPU work: callers run it under
//! `cpu::on_cpu`.

use std::io::{BufRead, Seek};

use ::image::{
    DynamicImage, ExtendedColorType, ImageDecoder as _, ImageEncoder as _, ImageReader, Limits,
    Rgb, RgbImage, codecs::jpeg::JpegEncoder, codecs::png::PngEncoder, imageops::FilterType,
    metadata::Orientation,
};
use lobby_protocol::{
    Preview,
    limits::{IMAGE_MAX_ALLOC as DECODE_MAX_ALLOC, IMAGE_MAX_EDGE as DECODE_MAX_SIDE},
};

use super::sniff::Sniffed;

pub const PREVIEW_MAX_EDGE: u32 = 480;
pub const PREVIEW_JPEG_QUALITY: u8 = 80;
pub const SQUARE_MAX_EDGE: u32 = 256;
pub const SQUARE_JPEG_QUALITY: u8 = 82;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("decode: {0}")]
    Decode(#[from] ::image::ImageError),
    #[error("read: {0}")]
    Read(#[from] std::io::Error),
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(DECODE_MAX_SIDE);
    l.max_image_height = Some(DECODE_MAX_SIDE);
    l.max_alloc = Some(DECODE_MAX_ALLOC);
    l
}

pub fn fit(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= max_edge {
        return (width.max(1), height.max(1));
    }
    let scale = |side: u32| {
        let scaled =
            (u64::from(side) * u64::from(max_edge) + u64::from(longest) / 2) / u64::from(longest);
        (scaled as u32).max(1)
    };
    (scale(width), scale(height))
}

/// Alpha composited on white, since JPEG has none.
fn flatten(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (src, dst) in rgba.pixels().zip(out.pixels_mut()) {
        let a = u32::from(src[3]);
        let mix = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        *dst = Rgb([mix(src[0]), mix(src[1]), mix(src[2])]);
    }
    out
}

/// The picture inside the decode limits, and which way up it is meant to be seen.
fn decode(input: impl BufRead + Seek) -> Result<(DynamicImage, Orientation), PreviewError> {
    let mut reader = ImageReader::new(input).with_guessed_format()?;
    let mut limits = limits();
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    limits.reserve(decoder.total_bytes())?;
    Ok((DynamicImage::from_decoder(decoder)?, orientation))
}

/// An avatar or icon: the middle square of the picture, at most [`SQUARE_MAX_EDGE`] wide, upright.
/// JPEG when nothing in it is see-through, PNG otherwise.
pub fn square(input: impl BufRead + Seek) -> Result<(Vec<u8>, Sniffed), PreviewError> {
    let (img, orientation) = decode(input)?;
    let side = img.width().min(img.height());
    let mut img = img.crop_imm(
        (img.width() - side) / 2,
        (img.height() - side) / 2,
        side,
        side,
    );
    if side > SQUARE_MAX_EDGE {
        img = img.resize_exact(SQUARE_MAX_EDGE, SQUARE_MAX_EDGE, FilterType::CatmullRom);
    }
    img.apply_orientation(orientation);
    let mut out = Vec::new();
    let see_through = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < u8::MAX);
    if see_through {
        let rgba = img.to_rgba8();
        PngEncoder::new(&mut out).write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )?;
        Ok((out, Sniffed::PNG))
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, SQUARE_JPEG_QUALITY).encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )?;
        Ok((out, Sniffed::JPEG))
    }
}

pub fn chat_preview(input: impl BufRead + Seek) -> Result<(Vec<u8>, Preview), PreviewError> {
    let (mut img, orientation) = decode(input)?;
    // Scaled before it is turned, so a quarter turn copies the preview and not the full decode.
    // `fit` treats both sides alike, so the upright size comes out the same.
    // An area average, not CatmullRom: as good for a shrink this big, and a 12 MP photo's
    // preview takes about half the time on the Lobby's one CPU.
    let (w, h) = fit(img.width(), img.height(), PREVIEW_MAX_EDGE);
    if (w, h) != (img.width(), img.height()) {
        img = img.thumbnail_exact(w, h);
    }
    img.apply_orientation(orientation);
    let rgb = flatten(&img);
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, PREVIEW_JPEG_QUALITY).encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok((
        out,
        Preview {
            width: rgb.width(),
            height: rgb.height(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ::image::ImageFormat;

    use super::*;

    fn preview(bytes: &[u8]) -> Result<(Vec<u8>, Preview), PreviewError> {
        chat_preview(Cursor::new(bytes))
    }

    fn encode(img: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn png_header_only(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in [(&b"IHDR"[..], ihdr), (b"IEND", Vec::new())] {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(&data);
            png.extend_from_slice(&body);
            png.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        png
    }

    #[test]
    fn bombs_and_garbage_are_refused_without_a_panic() {
        {
            let too_wide =
                DynamicImage::ImageLuma8(::image::GrayImage::new(DECODE_MAX_SIDE + 1, 1));
            assert!(preview(&encode(&too_wide, ImageFormat::Png)).is_err());
            // Each side is inside the limit, but 16384 x 16384 RGB is 768 MiB of pixels.
            assert!(preview(&png_header_only(DECODE_MAX_SIDE, DECODE_MAX_SIDE)).is_err());
            // The same header with a harmless size passes the limits (and fails only for lack of
            // data).
            assert!(matches!(
                preview(&png_header_only(8, 8)),
                Err(PreviewError::Decode(e)) if !matches!(e, ::image::ImageError::Limits(_))
            ));
        }
        {
            assert!(preview(b"not an image at all").is_err());
            assert!(preview(b"").is_err());
        }
    }

    /// A JPEG of `img` that says, in EXIF, that it is meant to be turned by `orientation`.
    fn jpeg_with_orientation(img: &DynamicImage, orientation: u8) -> Vec<u8> {
        let jpeg = encode(img, ImageFormat::Jpeg);
        let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&(exif.len() as u16 + 2).to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn a_square_is_the_middle_of_the_picture_the_way_up_it_is_meant_to_be_seen() {
        // Red on the left, blue on the right: 40 x 20, whose middle square is 10 px of each.
        let wide = DynamicImage::ImageRgb8(RgbImage::from_fn(40, 20, |x, _| {
            if x < 20 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        }));
        let (upright, mime) = square(Cursor::new(encode(&wide, ImageFormat::Jpeg))).unwrap();
        assert_eq!(mime, Sniffed::JPEG);
        let upright = ::image::load_from_memory(&upright).unwrap().to_rgb8();
        assert_eq!(upright.dimensions(), (20, 20));
        assert!(upright.get_pixel(2, 10)[0] > 200 && upright.get_pixel(17, 10)[2] > 200);

        // Orientation 6 turns the picture a quarter clockwise: red ends up on top.
        let (turned, _) = square(Cursor::new(jpeg_with_orientation(&wide, 6))).unwrap();
        let turned = ::image::load_from_memory(&turned).unwrap().to_rgb8();
        assert_eq!(turned.dimensions(), (20, 20));
        assert!(turned.get_pixel(10, 2)[0] > 200 && turned.get_pixel(10, 17)[2] > 200);
    }
}

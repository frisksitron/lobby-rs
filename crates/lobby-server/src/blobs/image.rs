//! Chat previews and Member avatars and Lobby icons. Pure CPU work: callers run it under
//! `cpu::on_cpu`.

use std::io::{self, BufRead, Read, Seek, SeekFrom};

use ::image::{
    DynamicImage, ExtendedColorType, ImageDecoder as _, ImageEncoder as _, ImageFormat,
    ImageReader, Limits, Rgb, RgbImage, codecs::jpeg::JpegEncoder, codecs::png::PngEncoder,
    metadata::Orientation,
};
use lobby_protocol::{Preview, limits::IMAGE_MAX_EDGE as DECODE_MAX_SIDE};

use super::sniff::Sniffed;

/// Bytes one decode may hold at its peak, counted as [`decode`] does. Decodes run one at a time
/// (`cpu::CPU_PERMITS`), so this plus the server's own use stays well under its 512 MiB
/// container. It takes a baseline JPEG of about 80 MP, its file included, and an 8K PNG.
const DECODE_MAX_ALLOC: u64 = 256 * 1024 * 1024;
/// What a progressive JPEG's coefficients hold besides the output, in thirds of the output.
const PROGRESSIVE_EXTRA_THIRDS: u64 = 6;
/// What a lossless WebP's RGBA decode holds besides an RGB output, in thirds of the output.
const WEBP_EXTRA_THIRDS: u64 = 4;

pub const PREVIEW_MAX_EDGE: u32 = 480;
pub const PREVIEW_JPEG_QUALITY: u8 = 80;
pub const SQUARE_MAX_EDGE: u32 = 256;
pub const SQUARE_JPEG_QUALITY: u8 = 82;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("decode: {0}")]
    Decode(#[from] ::image::ImageError),
    #[error("read: {0}")]
    Read(#[from] io::Error),
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

/// The picture inside the decode limits, and which way up it is meant to be seen. The limits
/// count what the decoder holds besides its output: the JPEG decoder reads the whole file first,
/// a progressive JPEG keeps a 16-bit coefficient per sample until its last scan (twice the
/// output), and a lossless WebP without alpha decodes into RGBA before it drops the alpha.
fn decode(mut input: impl BufRead + Seek) -> Result<(DynamicImage, Orientation), PreviewError> {
    let mut limits = limits();
    let format = ImageReader::new(&mut input).with_guessed_format()?.format();
    let extra_thirds = match format {
        Some(ImageFormat::Jpeg) => jpeg_extra_thirds(&mut input, &mut limits)?,
        Some(ImageFormat::WebP) => WEBP_EXTRA_THIRDS,
        _ => 0,
    };
    let mut reader = ImageReader::new(input).with_guessed_format()?;
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let output = decoder.total_bytes();
    limits.reserve(output)?;
    limits.reserve(output.saturating_mul(extra_thirds) / 3)?;
    Ok((DynamicImage::from_decoder(decoder)?, orientation))
}

/// Reserves the file, which the JPEG decoder reads whole, and returns what the frame adds. A
/// progressive frame is any `FF C2` in the file: the decoder acts on a marker wherever it finds
/// one, and a baseline scan stuffs its `FF`s, so only metadata can match by chance.
fn jpeg_extra_thirds(
    input: &mut (impl Read + Seek),
    limits: &mut Limits,
) -> Result<u64, PreviewError> {
    let start = input.stream_position()?;
    limits.reserve(input.seek(SeekFrom::End(0))? - start)?;
    input.seek(SeekFrom::Start(start))?;
    let mut file = Vec::new();
    input.read_to_end(&mut file)?;
    input.seek(SeekFrom::Start(start))?;
    let progressive = file.windows(2).any(|pair| pair == [0xFF, 0xC2]);
    Ok(if progressive {
        PROGRESSIVE_EXTRA_THIRDS
    } else {
        0
    })
}

/// The middle square, at most [`SQUARE_MAX_EDGE`] wide: scaled first, then cropped, so the
/// full-size picture is never copied.
fn middle_square(img: &DynamicImage) -> DynamicImage {
    let (width, height) = (u64::from(img.width()), u64::from(img.height()));
    let side = width.min(height);
    let edge = side.min(u64::from(SQUARE_MAX_EDGE));
    let (w, h) = ((width * edge / side) as u32, (height * edge / side) as u32);
    let edge = edge as u32;
    img.thumbnail_exact(w, h)
        .crop_imm((w - edge) / 2, (h - edge) / 2, edge, edge)
}

/// An avatar or icon: the middle square of the picture, at most [`SQUARE_MAX_EDGE`] wide, upright.
/// JPEG when nothing in it is see-through, PNG otherwise.
pub fn square(input: impl BufRead + Seek) -> Result<(Vec<u8>, Sniffed), PreviewError> {
    let (decoded, orientation) = decode(input)?;
    let mut img = middle_square(&decoded);
    drop(decoded);
    img.apply_orientation(orientation);
    let mut out = Vec::new();
    let rgba = img.to_rgba8();
    let see_through = rgba.pixels().any(|p| p[3] < u8::MAX);
    if see_through {
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

    /// An RGB PNG of `width` x `height` whose image data is empty: the decoder reads its headers
    /// and sizes its output before it finds the data missing.
    fn png_header_only(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let chunks = [
            (&b"IHDR"[..], ihdr),
            (b"IDAT", Vec::new()),
            (b"IEND", Vec::new()),
        ];
        for (kind, data) in chunks {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(&data);
            png.extend_from_slice(&body);
            png.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        png
    }

    /// A JPEG of `width` x `height` RGB with headers and no data, baseline or progressive.
    fn jpeg_header_only(width: u16, height: u16, progressive: bool) -> Vec<u8> {
        let mut jpeg = vec![
            0xFF,
            0xD8,
            0xFF,
            if progressive { 0xC2 } else { 0xC0 },
            0,
            17,
            8,
        ];
        jpeg.extend_from_slice(&height.to_be_bytes());
        jpeg.extend_from_slice(&width.to_be_bytes());
        jpeg.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
        let spectral_end = if progressive { 0 } else { 63 };
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0, 12, 3, 1, 0, 2, 0, 3, 0, 0, spectral_end, 0]);
        jpeg
    }

    fn over_the_limits(result: Result<(Vec<u8>, Preview), PreviewError>) -> bool {
        matches!(
            result,
            Err(PreviewError::Decode(::image::ImageError::Limits(_)))
        )
    }

    #[test]
    fn bombs_and_garbage_are_refused_without_a_panic() {
        {
            let too_wide =
                DynamicImage::ImageLuma8(::image::GrayImage::new(DECODE_MAX_SIDE + 1, 1));
            assert!(over_the_limits(preview(&encode(
                &too_wide,
                ImageFormat::Png
            ))));
            // Each side is inside the limit, but 16384 x 16384 RGB is 768 MiB of pixels.
            let square = png_header_only(DECODE_MAX_SIDE, DECODE_MAX_SIDE);
            assert!(over_the_limits(preview(&square)));
            // 10000 x 10000 RGB is 286 MiB.
            assert!(over_the_limits(preview(&png_header_only(10_000, 10_000))));
            // Headers with a size the budget takes pass the limits, and fail only for lack of
            // data: an 8K screenshot, and an 85 MP photo.
            assert!(!over_the_limits(preview(&png_header_only(7680, 4320))));
            assert!(!over_the_limits(preview(&jpeg_header_only(
                9216, 9216, false
            ))));
            // A progressive JPEG holds three times its pixels, so 48 MP is too many.
            assert!(over_the_limits(preview(&jpeg_header_only(
                8000, 6000, true
            ))));
            assert!(!over_the_limits(preview(&jpeg_header_only(
                8000, 6000, false
            ))));
            // The decoder skips fill bytes before a marker, so a progressive frame can hide
            // behind one.
            let mut filled = jpeg_header_only(8000, 6000, true);
            filled.insert(2, 0xFF);
            assert!(over_the_limits(preview(&filled)));
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

        // A picture wider than the avatar edge is scaled down from its middle.
        let big = DynamicImage::ImageRgb8(RgbImage::from_fn(900, 600, |x, _| {
            if x < 450 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        }));
        let (scaled, _) = square(Cursor::new(encode(&big, ImageFormat::Png))).unwrap();
        let scaled = ::image::load_from_memory(&scaled).unwrap().to_rgb8();
        assert_eq!(scaled.dimensions(), (SQUARE_MAX_EDGE, SQUARE_MAX_EDGE));
        assert!(scaled.get_pixel(10, 128)[0] > 200 && scaled.get_pixel(245, 128)[2] > 200);
    }
}

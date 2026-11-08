//! MIME sniffing over the first bytes: a hand-written subset of the WHATWG MIME sniffing algorithm.
//! The client's `Content-Type` and the file extension are never read.

use super::{BlobError, BlobKind};

pub const SNIFF_LEN: usize = 512;

/// A sniffed type that may be stored. Executables and denylisted types are errors, never values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniffed(&'static str);

impl Sniffed {
    /// What the avatar and icon encoders produce.
    pub const JPEG: Sniffed = Sniffed("image/jpeg");
    pub const PNG: Sniffed = Sniffed("image/png");

    pub fn mime(&self) -> &'static str {
        self.0
    }

    pub fn is_image(&self) -> bool {
        self.0.starts_with("image/")
    }
}

const MACHO_MAGICS: [[u8; 4]; 5] = [
    [0xFE, 0xED, 0xFA, 0xCE],
    [0xFE, 0xED, 0xFA, 0xCF],
    [0xCE, 0xFA, 0xED, 0xFE],
    [0xCF, 0xFA, 0xED, 0xFE],
    // Mach-O fat binaries (also Java classes, which no Member needs to send).
    [0xCA, 0xFE, 0xBA, 0xBE],
];

fn is_executable(head: &[u8]) -> bool {
    head.starts_with(b"MZ")
        || head.starts_with(b"\x7fELF")
        || head.starts_with(b"#!")
        || MACHO_MAGICS.iter().any(|m| head.starts_with(m))
}

/// Active content a browser would run; the text sniff refuses it, whatever the extension says.
fn is_denied_markup(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = head
        .iter()
        .position(|b| !matches!(b, b'\t' | b'\n' | 0x0C | b'\r' | b' '))
        .unwrap_or(head.len());
    let rest = &head[start..];
    [
        &b"<svg"[..],
        b"<!doctype html",
        b"<html",
        b"<script",
        b"<?php",
    ]
    .iter()
    .any(|p| rest.len() >= p.len() && rest[..p.len()].eq_ignore_ascii_case(p))
}

fn is_mp3_frame(head: &[u8]) -> bool {
    matches!(head, [0xFF, b1, ..] if b1 & 0xE0 == 0xE0 && b1 & 0x18 != 0x08 && b1 & 0x06 != 0)
}

fn is_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

fn binary(head: &[u8]) -> Option<&'static str> {
    let riff = |form: &[u8]| head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == form;
    Some(match head {
        h if h.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        h if h.starts_with(b"GIF87a") || h.starts_with(b"GIF89a") => "image/gif",
        _ if riff(b"WEBP") => "image/webp",
        h if h.len() >= 14 && h.starts_with(b"BM") && h[6..10] == [0, 0, 0, 0] => "image/bmp",
        [0, 0, 1, 0, ..] => "image/x-icon",
        h if h.starts_with(b"%PDF-") => "application/pdf",
        h if h.starts_with(b"PK\x03\x04") => "application/zip",
        [0x1F, 0x8B, 0x08, ..] => "application/gzip",
        h if h.starts_with(b"OggS") => "audio/ogg",
        h if h.starts_with(b"fLaC") => "audio/flac",
        h if h.starts_with(b"ID3") || is_mp3_frame(h) => "audio/mpeg",
        _ if riff(b"WAVE") => "audio/wav",
        h if h.len() >= 12 && &h[4..8] == b"ftyp" => match &h[8..12] {
            b"qt  " => "video/quicktime",
            b"M4A " | b"M4B " => "audio/mp4",
            _ => "video/mp4",
        },
        h if h.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) => {
            if h.windows(4).any(|w| w == b"webm") {
                "video/webm"
            } else {
                "video/x-matroska"
            }
        }
        _ => return None,
    })
}

/// Executables first (all kinds), then the binary signatures, then text, else
/// `application/octet-stream`. Avatars and icons must be images.
pub fn classify(head: &[u8], kind: BlobKind) -> Result<Sniffed, BlobError> {
    let head = &head[..head.len().min(SNIFF_LEN)];
    if head.is_empty() {
        return Err(BlobError::Empty);
    }
    if is_executable(head) {
        return Err(BlobError::Executable);
    }
    let mime = match binary(head) {
        Some(m) => m,
        None if is_text(head) => {
            if is_denied_markup(head) {
                return Err(BlobError::Unsupported);
            }
            "text/plain"
        }
        None => "application/octet-stream",
    };
    if kind != BlobKind::ChatAttachment && !mime.starts_with("image/") {
        return Err(BlobError::Unsupported);
    }
    Ok(Sniffed(mime))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mime(head: &[u8]) -> Result<&'static str, BlobError> {
        classify(head, BlobKind::ChatAttachment).map(|s| s.mime())
    }

    #[test]
    fn signatures_map_to_their_types() {
        let mut ftyp = b"\0\0\0\x18ftyp".to_vec();
        ftyp.extend_from_slice(b"isom");
        let mut mov = b"\0\0\0\x14ftyp".to_vec();
        mov.extend_from_slice(b"qt  ");
        let mut m4a = b"\0\0\0\x14ftyp".to_vec();
        m4a.extend_from_slice(b"M4A ");
        let mut webp = b"RIFF\x24\0\0\0WEBPVP8 ".to_vec();
        webp.extend_from_slice(&[0; 8]);
        let mut wav = b"RIFF\x24\0\0\0WAVEfmt ".to_vec();
        wav.extend_from_slice(&[0; 8]);
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0x36, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x28, 0]);
        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x42, 0x82, 0x84];
        webm.extend_from_slice(b"webm");
        for (head, expected) in [
            (&b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"[..], "image/png"),
            (&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10], "image/jpeg"),
            (b"GIF89a\x01\0\x01\0", "image/gif"),
            (b"GIF87a\x01\0\x01\0", "image/gif"),
            (&webp, "image/webp"),
            (&bmp, "image/bmp"),
            (&[0, 0, 1, 0, 1, 0, 16, 16], "image/x-icon"),
            (b"%PDF-1.7\n", "application/pdf"),
            (b"PK\x03\x04\x14\0", "application/zip"),
            (&[0x1F, 0x8B, 0x08, 0, 0], "application/gzip"),
            (b"OggS\0\x02", "audio/ogg"),
            (b"fLaC\0\0\0\"", "audio/flac"),
            (b"ID3\x04\0\0", "audio/mpeg"),
            (&[0xFF, 0xFB, 0x90, 0x64], "audio/mpeg"),
            (&wav, "audio/wav"),
            (&ftyp, "video/mp4"),
            (&mov, "video/quicktime"),
            (&m4a, "audio/mp4"),
            (&webm, "video/webm"),
            (&[0x1A, 0x45, 0xDF, 0xA3, 0x42, 0x82], "video/x-matroska"),
            (b"hello, world\n", "text/plain"),
            ("naïve café".as_bytes(), "text/plain"),
            (&[0x01, 0x02, 0xFE, 0x03], "application/octet-stream"),
            (b"RIFF\x24\0\0\0AVI LIST", "application/octet-stream"),
            (b"BM is a text file", "text/plain"),
        ] {
            assert_eq!(mime(head).unwrap(), expected, "{head:?}");
        }
    }

    #[test]
    fn executables_and_active_content_are_refused() {
        {
            for head in [
                &b"MZ\x90\0\x03"[..],
                b"\x7fELF\x02\x01",
                b"#!/bin/sh\nrm -rf /\n",
                &[0xFE, 0xED, 0xFA, 0xCE, 0],
                &[0xFE, 0xED, 0xFA, 0xCF, 0],
                &[0xCE, 0xFA, 0xED, 0xFE, 0],
                &[0xCF, 0xFA, 0xED, 0xFE, 0],
                &[0xCA, 0xFE, 0xBA, 0xBE, 0],
            ] {
                for kind in [
                    BlobKind::ChatAttachment,
                    BlobKind::Avatar,
                    BlobKind::LobbyIcon,
                ] {
                    assert!(
                        matches!(classify(head, kind), Err(BlobError::Executable)),
                        "{head:?} {kind:?}"
                    );
                }
            }
        }
        {
            for head in [
                &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
                b"  \n<SVG/>",
                b"\xEF\xBB\xBF<!DOCTYPE html><title>x</title>",
                b"\t<html><body>",
                b"<script>alert(1)</script>",
                b"<?php echo 1;",
                b"\r\n <!doctype HTML>",
            ] {
                assert!(
                    matches!(mime(head), Err(BlobError::Unsupported)),
                    "{}",
                    String::from_utf8_lossy(head)
                );
            }
            assert_eq!(mime(b"a <svg> in prose").unwrap(), "text/plain");
        }
    }
}

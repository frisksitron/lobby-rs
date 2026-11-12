//! The save and open policy for attachments: a server-provided name is never
//! joined onto a directory as it came, Open is offered only for a small allowlist whose
//! extension and sniffed MIME type agree, and every saved file carries the
//! Mark-of-the-Web.

use std::{io, path::Path};

use lobby_protocol::content::is_bidi_control;

/// Windows counts a file name in UTF-16 units.
const NAME_MAX_UTF16: usize = 255;

/// What a Save dialog is prefilled with, and the name an Open copy gets: the basename after
/// splitting on both `/` and `\`, without `: < > " | ? *`, control characters and bidi controls,
/// trailing dots and spaces trimmed, a Windows reserved device name prefixed with `_`, empty ⇒
/// `download`, at most 255 UTF-16 units.
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let kept: String = base
        .chars()
        .filter(|&c| {
            !c.is_control()
                && !is_bidi_control(c)
                && !matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
        .collect();
    let mut out = kept
        .trim_end_matches(['.', ' '])
        .trim_start_matches(' ')
        .to_owned();
    if out.is_empty() {
        return "download".to_owned();
    }
    if is_reserved(&out) {
        out.insert(0, '_');
    }
    let mut units = 0;
    let too_long = out.char_indices().find(|(_, c)| {
        units += c.len_utf16();
        units > NAME_MAX_UTF16
    });
    if let Some((end, _)) = too_long {
        out.truncate(end);
    }
    out
}

/// Windows refuses `CON`, `CON.txt` and friends in any case.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default();
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
}

const OPEN_ALLOWLIST: &[(&str, &[&str])] = &[
    ("pdf", &["application/pdf"]),
    ("txt", &["text/plain"]),
    ("png", &["image/png"]),
    ("jpg", &["image/jpeg"]),
    ("jpeg", &["image/jpeg"]),
    ("gif", &["image/gif"]),
    ("webp", &["image/webp"]),
    ("mp3", &["audio/mpeg", "audio/mp3"]),
    ("mp4", &["video/mp4"]),
    (
        "wav",
        &["audio/wav", "audio/x-wav", "audio/wave", "audio/vnd.wave"],
    ),
    ("ogg", &["audio/ogg", "video/ogg", "application/ogg"]),
];

/// Whether Open may be offered: the sanitized name's extension is on the allowlist and the
/// server's sniffed MIME type agrees with it. A `.pdf` sniffed as `application/octet-stream`
/// gets Save… only.
pub fn open_allowed(name: &str, mime: &str) -> bool {
    let name = sanitize_file_name(name);
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    OPEN_ALLOWLIST
        .iter()
        .find(|(e, _)| *e == ext)
        .is_some_and(|(_, mimes)| mimes.contains(&mime.as_str()))
}

/// Marks `path` as downloaded from the internet (`Zone.Identifier` alternate data stream), so
/// SmartScreen and Office treat it as such. Fails on file systems without streams (FAT, exFAT);
/// the caller logs that and keeps the file.
#[cfg(windows)]
pub fn mark_downloaded(path: &Path) -> io::Result<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(stream, b"[ZoneTransfer]\r\nZoneId=3\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_reduced_to_a_safe_basename() {
        let cases = [
            ("photo.jpg", "photo.jpg"),
            ("..\\..\\x.bat", "x.bat"),
            (
                r"..\..\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\x.bat",
                "x.bat",
            ),
            ("../../etc/passwd", "passwd"),
            ("a/b\\c.txt", "c.txt"),
            ("C:report.pdf", "Creport.pdf"),
            ("what?.png", "what.png"),
            ("trailing... ", "trailing"),
            ("  spaced  ", "spaced"),
            ("tab\there\u{7f}.txt", "tabhere.txt"),
            ("report\u{202E}fdp.bat", "reportfdp.bat"),
            (
                "\u{200E}a\u{200F}b\u{202A}c\u{202D}d\u{2066}e\u{2069}.txt",
                "abcde.txt",
            ),
            ("\u{202E}", "download"),
            ("", "download"),
            ("...", "download"),
            ("/", "download"),
            ("CON", "_CON"),
            ("con.txt", "_con.txt"),
            ("LPT1.log", "_LPT1.log"),
            ("COM².txt", "_COM².txt"),
            ("COM0", "COM0"),
            ("com10.txt", "com10.txt"),
            ("console.txt", "console.txt"),
            ("naïve ☃.png", "naïve ☃.png"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_file_name(input), expected, "{input:?}");
        }
        let long = "x".repeat(300);
        assert_eq!(sanitize_file_name(&long).chars().count(), 255);
        let wide = "😀".repeat(200);
        assert_eq!(sanitize_file_name(&wide).encode_utf16().count(), 254);
    }

    #[test]
    fn open_needs_an_allowlisted_extension_whose_mime_agrees() {
        let allowed = [
            ("doc.pdf", "application/pdf"),
            ("notes.txt", "text/plain"),
            ("notes.TXT", "text/plain; charset=utf-8"),
            ("cat.png", "image/png"),
            ("cat.jpg", "image/jpeg"),
            ("cat.jpeg", "image/jpeg"),
            ("cat.gif", "image/gif"),
            ("cat.webp", "image/webp"),
            ("song.mp3", "audio/mpeg"),
            ("clip.mp4", "video/mp4"),
            ("clip.wav", "audio/wav"),
            ("clip.ogg", "audio/ogg"),
            ("..\\..\\cat.png", "image/png"),
        ];
        for (name, mime) in allowed {
            assert!(open_allowed(name, mime), "{name} {mime}");
        }
        let refused = [
            ("x.bat", "text/plain"),
            ("x.exe", "application/octet-stream"),
            ("doc.pdf", "application/octet-stream"),
            ("cat.png", "image/jpeg"),
            ("page.html", "text/html"),
            ("noext", "text/plain"),
            ("archive.zip", "application/zip"),
            ("script.js", "text/javascript"),
            ("", "text/plain"),
        ];
        for (name, mime) in refused {
            assert!(!open_allowed(name, mime), "{name} {mime}");
        }
    }
}

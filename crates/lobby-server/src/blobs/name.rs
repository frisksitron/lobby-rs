//! Upload name sanitizing. The server runs on Linux where `\` is not a path separator, but the
//! saved file lands on a Windows client, so both separators split.

use lobby_protocol::content::is_bidi_control;

pub const NAME_MAX_CHARS: usize = 255;
pub const DEFAULT_NAME: &str = "upload.bin";

const DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn is_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    DEVICE_NAMES.iter().any(|d| stem.eq_ignore_ascii_case(d))
}

fn trim(s: &str) -> &str {
    s.trim_start().trim_end_matches(['.', ' '])
}

pub fn sanitize(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|&c| !c.is_control() && !is_bidi_control(c))
        .collect();
    let mut name = trim(&cleaned).to_owned();
    if is_device(&name) {
        name.insert(0, '_');
    }
    if let Some((end, _)) = name.char_indices().nth(NAME_MAX_CHARS) {
        name.truncate(end);
    }
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    if name.is_empty() {
        DEFAULT_NAME.to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_table() {
        {
            let long = "a".repeat(300);
            let multibyte = "é".repeat(300);
            for (raw, expected) in [
                ("cat.png", "cat.png"),
                (
                    r"..\..\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\x.bat",
                    "x.bat",
                ),
                ("../../etc/passwd", "passwd"),
                ("a/b\\c/d.txt", "d.txt"),
                ("C:\\Users\\me\\report.pdf", "report.pdf"),
                ("dir/", DEFAULT_NAME),
                ("", DEFAULT_NAME),
                ("..", DEFAULT_NAME),
                (".", DEFAULT_NAME),
                ("   ", DEFAULT_NAME),
                ("notes.txt. . ", "notes.txt"),
                ("  lead.txt", "lead.txt"),
                ("tab\tand\u{0}nul\u{7f}.txt", "tabandnul.txt"),
                ("report\u{202E}fdp.bat", "reportfdp.bat"),
                (
                    "\u{200E}a\u{200F}b\u{202A}c\u{202D}d\u{2066}e\u{2069}.txt",
                    "abcde.txt",
                ),
                ("\u{202E}", DEFAULT_NAME),
                ("CON", "_CON"),
                ("con.txt", "_con.txt"),
                ("Nul.tar.gz", "_Nul.tar.gz"),
                ("COM1", "_COM1"),
                ("lpt9.log", "_lpt9.log"),
                ("AUX ", "_AUX"),
                ("console.txt", "console.txt"),
                ("COM10", "COM10"),
                ("COM0", "COM0"),
                ("x\\CON", "_CON"),
                (".bashrc", ".bashrc"),
                ("résumé 日本語.pdf", "résumé 日本語.pdf"),
            ] {
                assert_eq!(sanitize(raw), expected, "{raw:?}");
            }
            assert_eq!(sanitize(&long).chars().count(), NAME_MAX_CHARS);
            let truncated = sanitize(&multibyte);
            assert_eq!(truncated.chars().count(), NAME_MAX_CHARS);
            assert!(truncated.chars().all(|c| c == 'é'));
        }
        {
            let raw = format!("{}.{}", "a".repeat(254), "txt");
            assert_eq!(sanitize(&raw), "a".repeat(254));
        }
    }
}

//! External links: what a click on a link in a Message may open.
//! Only an absolute `http`, `https` or `mailto` URL, parsed with `url`, ever reaches the system
//! browser; a relative, scheme-relative (`//host`), UNC (`\\host\share`) or any other scheme is
//! refused. Messages render links only for these, and a click checks again.

use url::Url;

pub fn external_url(href: &str) -> Option<Url> {
    let href = href.trim();
    // `url` would read "\\host\share" as a relative path; it never gets that far.
    if href.starts_with(['\\', '/']) {
        return None;
    }
    let url = Url::parse(href).ok()?;
    match url.scheme() {
        "http" | "https" => url.host_str().is_some_and(|h| !h.is_empty()).then_some(url),
        "mailto" => (!url.path().trim().is_empty()).then_some(url),
        _ => None,
    }
}

pub fn is_external_link(href: &str) -> bool {
    external_url(href).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_http_https_and_mailto_open() {
        for ok in [
            "https://example.com",
            "http://127.0.0.1:8080/x?y#z",
            "HTTPS://EXAMPLE.COM/A",
            "mailto:someone@example.com",
            " https://padded.example ",
        ] {
            assert!(is_external_link(ok), "{ok}");
        }
        for refused in [
            "",
            "/api/v1/messages",
            "relative/path",
            "//evil.example/x",
            "\\\\attacker\\share\\x",
            "javascript:alert(1)",
            "file:///C:/Windows/System32/calc.exe",
            "data:text/html,hi",
            "ms-settings:privacy",
            "http://",
            "mailto:",
            "example.com",
        ] {
            assert!(!is_external_link(refused), "{refused:?}");
        }
    }
}

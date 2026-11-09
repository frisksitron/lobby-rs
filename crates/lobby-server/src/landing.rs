//! `GET /`: the page a Lobby's address opens in a browser. It names the Lobby, links to the app
//! with `lobby://<host>` and to the installer. It shows nothing about what is inside.

use axum::{
    extract::State,
    http::{HeaderName, HeaderValue, header},
    response::IntoResponse,
};

use crate::server::App;

/// Not in `lobby_protocol::routes`: no client calls it.
pub(crate) const PATH: &str = "/";

const DOWNLOAD_URL: &str =
    "https://github.com/frisksitron/lobby-rs/releases/latest/download/LobbySetup.exe";

const PAGE: &str = include_str!("landing.html");

/// The page has no external request of any kind, so nothing else is allowed.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; \
     script-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub(crate) async fn page(State(app): State<App>) -> impl IntoResponse {
    let body = PAGE
        .replace("{{download}}", DOWNLOAD_URL)
        .replace("{{name}}", &escape(&app.lobby.info().name));
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                HeaderName::from_static("content-security-policy"),
                HeaderValue::from_static(CONTENT_SECURITY_POLICY),
            ),
        ],
        body,
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

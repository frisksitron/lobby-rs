//! Tracing setup: JSON lines with `LOBBY_LOG_FORMAT=json` (the Docker image's default),
//! human-readable otherwise; `RUST_LOG` filters.

use tracing_subscriber::{
    EnvFilter, Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

const DEFAULT_FILTER: &str = "info,lobby_server=info";

pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let json = std::env::var("LOBBY_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    let fmt = if json {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().boxed()
    };
    // `try_init`: a test harness may already have installed a subscriber.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt)
        .try_init();
}

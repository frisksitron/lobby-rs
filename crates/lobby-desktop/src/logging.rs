//! File logging: a daily file under `<data_dir>/logs` (7 kept), plus stderr in debug builds.
//! `RUST_LOG` filters.
//!
//! The appender's `WorkerGuard` lives in [`LOG_GUARD`] rather than on `main`'s stack, because
//! GPUI's OS-logoff path calls `std::process::exit` and `main` may never return. Whoever ends the
//! process last calls [`flush_and_drop`].

use std::{fmt, path::Path, sync::Mutex};

use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{InitError, RollingFileAppender, Rotation},
};
use tracing_subscriber::{
    EnvFilter, Layer,
    layer::{Context, SubscriberExt as _},
    util::SubscriberInitExt as _,
    util::TryInitError,
};

pub static LOG_GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);

const DEFAULT_FILTER: &str = "info";
const KEEP_FILES: usize = 7;

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("cannot create the log directory: {0}")]
    Dir(#[source] std::io::Error),
    #[error("cannot open the log file: {0}")]
    File(#[source] InitError),
    #[error("a global logger is already installed: {0}")]
    Subscriber(#[source] TryInitError),
}

pub fn init(dir: &Path) -> Result<(), LoggingError> {
    std::fs::create_dir_all(dir).map_err(LoggingError::Dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("lobby")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(dir)
        .map_err(LoggingError::File)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer);
    let stderr = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .boxed()
    });
    tracing_subscriber::registry()
        .with(GpuiNoise)
        .with(filter)
        .with(file)
        .with(stderr)
        .try_init()
        .map_err(LoggingError::Subscriber)?;
    *LOG_GUARD.lock().unwrap_or_else(|e| e.into_inner()) = Some(guard);
    Ok(())
}

pub fn flush_and_drop() {
    let guard = LOG_GUARD.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(guard);
}

/// Drops a known-harmless GPUI record that would make ERROR lines meaningless: debug builds probe
/// the DXGI debug layer at every start and, without the Graphics Tools optional feature, log
/// `0x887A002D` at ERROR. The WARN right after it already states the outcome.
struct GpuiNoise;

impl<S: Subscriber> Layer<S> for GpuiNoise {
    fn event_enabled(&self, event: &Event<'_>, _cx: Context<'_, S>) -> bool {
        if *event.metadata().level() != Level::ERROR {
            return true;
        }
        let mut fields = LogFields::default();
        event.record(&mut fields);
        !is_gpui_noise(&fields.file, &fields.message)
    }
}

#[derive(Default)]
struct LogFields {
    file: String,
    message: String,
}

impl Visit for LogFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "log.file" => self.file = value.to_owned(),
            "message" => self.message = value.to_owned(),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "log.file" => self.file = format!("{value:?}"),
            "message" => self.message = format!("{value:?}"),
            _ => {}
        }
    }
}

fn is_gpui_noise(file: &str, message: &str) -> bool {
    let file = file.replace('\\', "/");
    file.ends_with("/directx_devices.rs") && message.contains("0x887A002D")
}

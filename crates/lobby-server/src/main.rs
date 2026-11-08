//! `lobby-server [--config <path>]`: serve until Ctrl+C or SIGTERM.
//! Exit codes: 0 clean, 1 unclean shutdown or startup failure, 64 usage, 70 after a fatal service
//! failure, 78 invalid configuration or a foreign database.

use std::{path::PathBuf, process::ExitCode};

use lobby_server::{Config, ConfigSource, Sockets, StartError, telemetry};
use tracing::{error, info, warn};

const USAGE: &str = "usage: lobby-server [--config <path>]

Reads ./lobby.toml (if present), then LOBBY_* environment variables
(nested keys with __, e.g. LOBBY_SMTP__HOST).";

const EX_USAGE: u8 = 64;
const EX_SOFTWARE: u8 = 70;
const EX_CONFIG: u8 = 78;

enum Command {
    Serve(ConfigSource),
    Help,
    Version,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut source = ConfigSource::Default;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                let path = args.next().ok_or("--config needs a path")?;
                source = ConfigSource::File(PathBuf::from(path));
            }
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    Ok(Command::Serve(source))
}

#[tokio::main]
async fn main() -> ExitCode {
    let source = match parse_args(std::env::args().skip(1)) {
        Ok(Command::Serve(source)) => source,
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("lobby-server {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(EX_USAGE);
        }
    };
    telemetry::init();
    let cfg = match Config::load(&source) {
        Ok(cfg) => cfg,
        Err(e) => {
            error!("{e}");
            return ExitCode::from(EX_CONFIG);
        }
    };
    for warning in cfg.warnings() {
        warn!("{warning}");
    }
    serve(cfg).await
}

async fn serve(cfg: Config) -> ExitCode {
    let tcp = match tokio::net::TcpListener::bind(cfg.http.bind).await {
        Ok(l) => l,
        Err(e) => {
            error!(bind = %cfg.http.bind, error = %e, "cannot bind http.bind");
            return ExitCode::FAILURE;
        }
    };
    let udp = match lobby_server::bind_udp(cfg.sfu.udp_bind) {
        Ok(s) => s,
        Err(e) => {
            error!(bind = %cfg.sfu.udp_bind, error = %e, "cannot bind sfu.udp_bind");
            return ExitCode::FAILURE;
        }
    };
    let sfu_tcp = match lobby_server::bind_tcp(cfg.sfu.tcp_bind) {
        Ok(s) => s,
        Err(e) => {
            error!(bind = %cfg.sfu.tcp_bind, error = %e, "cannot bind sfu.tcp_bind");
            return ExitCode::FAILURE;
        }
    };
    let sockets = Sockets { tcp, udp, sfu_tcp };
    let server = match lobby_server::start(cfg, sockets).await {
        Ok(s) => s,
        Err(e @ StartError::Db(lobby_server::DbOpenError::Foreign { .. })) => {
            error!("{e}");
            return ExitCode::from(EX_CONFIG);
        }
        Err(e) => {
            error!(error = %e, "startup failed");
            return ExitCode::FAILURE;
        }
    };
    let fatal = server.fatal();
    let fatal_hit = tokio::select! {
        () = shutdown_signal() => false,
        () = fatal.cancelled() => true,
    };
    info!(fatal = fatal_hit, "shutting down");
    let report = server.shutdown().await;
    info!(?report, clean = report.is_clean(), "shutdown report");
    if fatal_hit {
        ExitCode::from(EX_SOFTWARE)
    } else if report.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Ctrl+C everywhere; SIGTERM on unix (Docker); Ctrl+Break and console close on Windows.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!(error = %e, "cannot listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let other = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(windows)]
    let other = async {
        use tokio::signal::windows::{ctrl_break, ctrl_close};
        match (ctrl_break(), ctrl_close()) {
            (Ok(mut brk), Ok(mut close)) => {
                tokio::select! {
                    _ = brk.recv() => {}
                    _ = close.recv() => {}
                }
            }
            _ => {
                error!("cannot listen for Ctrl+Break / console close");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = other => {}
    }
}

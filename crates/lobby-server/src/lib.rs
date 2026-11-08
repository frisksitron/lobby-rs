//! The Lobby backend. One composition root, [`start`], builds every service from explicit
//! dependencies; [`RunningServer::shutdown`] stops them tier by tier.

mod access;
mod account;
mod auth;
mod blobs;
mod chat;
mod client_ip;
pub mod config;
mod cpu;
mod db;
mod domain;
pub mod error;
mod jobs;
mod limits;
mod linger;
mod lobby;
pub mod mail;
mod media;
mod owner;
mod profile_images;
mod router;
mod server;
mod sfu;
pub mod telemetry;
mod tiers;
mod uploads;
mod voice;
mod ws;

#[cfg(feature = "test-support")]
pub mod testing;

pub use config::{Config, ConfigError, ConfigSource};
pub use db::{DbError, DbOpenError};
#[cfg(feature = "test-support")]
pub use domain::HeldWrites;
#[cfg(feature = "test-support")]
pub use jobs::CleanupReport;
#[cfg(feature = "test-support")]
pub use lobby::Counts;
pub use server::{App, RunningServer, ShutdownReport, Sockets, StartError, start};
#[cfg(feature = "test-support")]
pub use sfu::MediaPath;
pub use sfu::{bind_tcp, bind_udp};

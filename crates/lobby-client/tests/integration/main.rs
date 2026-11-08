//! The client's integration tests, one binary: each module is a flow run against a real
//! in-process `lobby-server`, or against the Session driver's fakes. One binary because every
//! test crate links the whole client.

mod admission;
mod attachments;
mod chat;
mod common;
mod connect;
mod entry;
mod history;
mod resilience;
mod session_driver;
mod share;
mod update;
mod voice;
mod watch;

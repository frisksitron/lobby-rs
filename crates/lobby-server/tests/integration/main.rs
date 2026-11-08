//! The server's integration tests, one binary: each module drives an in-process server over
//! real loopback sockets. One binary because every test crate links the whole server.

mod admission;
mod auth;
mod common;
mod mail;
mod owner;
mod realtime;
mod server;
mod uploads;
mod voice;

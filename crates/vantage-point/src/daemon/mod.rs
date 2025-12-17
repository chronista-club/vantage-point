//! Daemon module - HTTP server + WebSocket hub

mod hub;
mod server;

pub use server::run;

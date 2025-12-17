//! Daemon module - HTTP server + WebSocket hub

mod server;
mod hub;

pub use server::run;

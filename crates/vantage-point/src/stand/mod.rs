//! Stand module - AI Agent server (HTTP + WebSocket hub)
//!
//! "Stand" is named after JoJo's Bizarre Adventure - an entity that stands by
//! the user's side and wields unique capabilities.

mod hub;
mod server;

pub use server::run;

//! Authoritative multiplayer server for Terminal Chess.
//!
//! [`hub`] holds every rule about rooms, seats, clocks, and persistence and
//! never touches a socket. [`server`] is the WebSocket transport around it.

pub mod hub;
pub mod server;

pub use server::{serve, Config};

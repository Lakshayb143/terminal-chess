//! Authoritative multiplayer server for Terminal Chess.
//!
//! [`hub`] holds every rule about rooms, seats, clocks, and persistence and
//! never touches a socket. [`store`] keeps accounts and finished games in
//! SQLite, and [`accounts`] adds password hashing and session tokens on top.
//! [`server`] is the WebSocket transport around them, and [`ssh`] lets
//! people play over plain `ssh` by running the terminal client for them.

pub mod accounts;
pub mod hub;
pub mod server;
pub mod ssh;
pub mod store;

pub use server::{serve, serve_with, Config};

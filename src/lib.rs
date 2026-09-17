//! Shared chess rules, game state, persistence, and presentation support.
//!
//! The terminal client and the multiplayer server both depend on this crate so
//! positions, clocks, outcomes, and wire messages have one implementation.

pub mod board;
pub mod eval;
pub mod game;
pub mod input;
pub mod movegen;
pub mod online;
pub mod protocol;
pub mod san;
pub mod search;
pub mod sound;
pub mod storage;
pub mod ui;

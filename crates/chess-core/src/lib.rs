//! Chess rules, notation, evaluation, engine search, and UI-independent game
//! state. It has no I/O dependencies, so the terminal client and the
//! multiplayer server share one implementation of every rule.

pub mod board;
pub mod eval;
pub mod game;
pub mod movegen;
pub mod san;
pub mod search;

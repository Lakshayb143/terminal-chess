//! Terminal client support: rendering, input, sound, local storage, and the
//! network transport. Chess rules live in `chess-core` and wire messages in
//! `chess-protocol`, which the multiplayer server shares.

pub mod client;
pub mod input;
pub mod sound;
pub mod storage;
pub mod ui;

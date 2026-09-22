//! The interactive terminal application: option parsing, the screen, and the
//! local and online game loops. Chess rules live in `chess-core`; this module
//! only presents them and turns input into game actions.

pub(crate) mod account;
pub(crate) mod actions;
pub(crate) mod cli;
pub(crate) mod format;
pub(crate) mod home;
pub(crate) mod local;
pub(crate) mod online;
pub(crate) mod pages;
pub(crate) mod parse;
pub(crate) mod prompt;
pub(crate) mod saves;
pub(crate) mod screen;
pub(crate) mod settings;

#[cfg(test)]
mod tests;

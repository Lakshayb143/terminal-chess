//! A chess game for the terminal: draw a board, read a move, answer with one.

mod app;

use app::cli::Options;
use app::local::play;
use chess::storage;

fn main() {
    let loaded = storage::load_preferences();
    let options = match Options::parse(std::env::args().skip(1), &loaded.preferences) {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(message) => {
            eprintln!("chess: {}", message);
            eprintln!("Try `chess --help`.");
            std::process::exit(2);
        }
    };
    if let Err(message) = play(options, loaded) {
        eprintln!("chess: {}", message);
        std::process::exit(1);
    }
}

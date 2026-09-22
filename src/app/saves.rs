//! Local saves and preferences, adapted to the on-disk format.

use std::path::Path;
use std::time::Duration;

use chess::{storage, ui};
use chess_core::board::{Color, Position};
use chess_core::game::{outcome, Game};
use chess_core::san::parse_move;

use crate::app::cli::Mode;
use crate::app::screen::Screen;

// Local saves adapt the shared game model to the existing on-disk format.

pub(crate) fn saved_game(game: &mut Game, mode: Mode) -> storage::SavedGame {
    let mut saved = storage::SavedGame::new();
    saved.start_fen = game.start.to_fen();
    saved.moves = game.undos.iter().map(|undo| undo.mv.to_uci()).collect();
    saved.mode = mode.name().to_string();
    saved.initial_clock_ms = game
        .clock
        .initial
        .map(|time| time.as_millis().min(u64::MAX as u128) as u64);
    saved.increment_ms = game.clock.increment.as_millis().min(u64::MAX as u128) as u64;
    saved.remaining_ms = game.clock.snapshot();
    saved.paused = game.paused;
    saved.resigned = game.resigned.map(|color| color.name().to_ascii_lowercase());
    saved.draw_offer = game
        .draw_offer
        .map(|color| color.name().to_ascii_lowercase());
    saved.agreed_draw = game.agreed_draw;
    saved
}

pub(crate) fn restore_game(saved: storage::SavedGame) -> Result<(Game, Mode), String> {
    let mode = Mode::named(&saved.mode)
        .ok_or_else(|| format!("saved game has unknown mode `{}`", saved.mode))?;
    let start = Position::from_fen(&saved.start_fen)
        .map_err(|error| format!("saved starting position is invalid: {error}"))?;
    let initial = saved.initial_clock_ms.map(Duration::from_millis);
    let increment = Duration::from_millis(saved.increment_ms);
    let mut game = Game::with_clock(start, initial, increment);
    for (index, notation) in saved.moves.iter().enumerate() {
        let movement = parse_move(&game.pos, notation).map_err(|_| {
            format!(
                "saved move {} (`{}`) is not legal in its position",
                index + 1,
                notation
            )
        })?;
        game.play(movement);
    }
    game.resigned = saved.resigned.as_deref().and_then(color_named);
    game.draw_offer = saved.draw_offer.as_deref().and_then(color_named);
    game.agreed_draw = saved.agreed_draw;
    game.paused = saved.paused;
    let running = !game.paused && outcome(&game).is_none();
    game.clock
        .restore(saved.remaining_ms, game.pos.side, running);
    game.revision = 0;
    Ok((game, mode))
}

pub(crate) fn color_named(name: &str) -> Option<Color> {
    match name.to_ascii_lowercase().as_str() {
        "white" => Some(Color::White),
        "black" => Some(Color::Black),
        _ => None,
    }
}

pub(crate) fn runtime_preferences(screen: &Screen, game: &Game) -> storage::Preferences {
    storage::Preferences {
        version: 1,
        white_name: screen.player_names[Color::White.index()].clone(),
        black_name: screen.player_names[Color::Black.index()].clone(),
        theme: ui::palette_name(screen.theme.palette).to_string(),
        pieces: screen.pieces.name().to_string(),
        compact: screen.compact,
        sound: screen.sound.mode().name().to_string(),
        flipped: screen.flipped,
        clock_enabled: game.clock.initial.is_some(),
        clock_minutes: game
            .clock
            .initial
            .map(|time| time.as_secs_f64() / 60.0)
            .unwrap_or(10.0),
        increment_seconds: game.clock.increment.as_secs_f64(),
        onboarding_complete: true,
        // The caller knows the engine; this default is replaced.
        engine_level: "casual".to_string(),
    }
}

pub(crate) fn save_current_game(path: &Path, game: &mut Game, mode: Mode) -> Result<(), String> {
    storage::save_game(path, &saved_game(game, mode))
}

//! UI-independent game state shared by local and network play.

use std::time::{Duration, Instant};

use crate::board::{Color, Move, Piece, Position, Undo};
use crate::movegen::{generate_legal, in_check};
use crate::san::to_san;
use crate::search;

/// A chess clock driven by a monotonic local clock.
///
/// For online play this object lives on the authoritative server. Clients only
/// receive snapshots and may animate the displayed time between snapshots.
pub struct GameClock {
    pub initial: Option<Duration>,
    pub increment: Duration,
    pub remaining: [Duration; 2],
    pub running: Option<(Color, Instant)>,
    pub flagged: Option<Color>,
    shown_seconds: [u64; 2],
}

impl GameClock {
    pub fn new(initial: Option<Duration>, increment: Duration, side: Color) -> GameClock {
        let time = initial.unwrap_or(Duration::ZERO);
        let seconds = shown_seconds(time);
        GameClock {
            initial,
            increment,
            remaining: [time; 2],
            running: initial.map(|_| (side, Instant::now())),
            flagged: None,
            shown_seconds: [seconds; 2],
        }
    }

    /// Synchronize elapsed time and report whether the displayed clock changed.
    pub fn tick(&mut self) -> bool {
        let flagged_before = self.flagged;
        self.sync();
        let shown = [
            shown_seconds(self.remaining[Color::White.index()]),
            shown_seconds(self.remaining[Color::Black.index()]),
        ];
        let changed = shown != self.shown_seconds || self.flagged != flagged_before;
        self.shown_seconds = shown;
        changed
    }

    pub fn complete_move(&mut self, mover: Color, next: Color) {
        self.sync();
        if self.flagged.is_some() || self.initial.is_none() {
            return;
        }
        self.remaining[mover.index()] += self.increment;
        self.running = Some((next, Instant::now()));
        self.remember_shown();
    }

    pub fn pause(&mut self) {
        self.sync();
        self.running = None;
        self.remember_shown();
    }

    pub fn resume(&mut self, side: Color) {
        if self.initial.is_none() {
            return;
        }
        self.flagged = None;
        self.running = Some((side, Instant::now()));
        self.remember_shown();
    }

    pub fn reset(&mut self, side: Color) {
        let time = self.initial.unwrap_or(Duration::ZERO);
        self.remaining = [time; 2];
        self.flagged = None;
        self.running = self.initial.map(|_| (side, Instant::now()));
        self.remember_shown();
    }

    pub fn remaining(&self, color: Color) -> Option<Duration> {
        self.initial?;
        let stored = self.remaining[color.index()];
        match self.running {
            Some((running, since)) if running == color => {
                Some(stored.saturating_sub(since.elapsed()))
            }
            _ => Some(stored),
        }
    }

    pub fn format(&self, color: Color) -> String {
        let Some(remaining) = self.remaining(color) else {
            return "UNTIMED".to_string();
        };
        let total = shown_seconds(remaining);
        if total >= 60 {
            format!("{}:{:02}", total / 60, total % 60)
        } else {
            format!("0:{:02}", total)
        }
    }

    pub fn low(&self, color: Color) -> bool {
        self.remaining(color)
            .is_some_and(|time| time <= Duration::from_secs(30))
    }

    fn sync(&mut self) {
        let Some((color, since)) = self.running else {
            return;
        };
        let remaining = &mut self.remaining[color.index()];
        *remaining = remaining.saturating_sub(since.elapsed());
        if remaining.is_zero() {
            self.flagged = Some(color);
            self.running = None;
        } else {
            self.running = Some((color, Instant::now()));
        }
    }

    fn remember_shown(&mut self) {
        self.shown_seconds = [
            shown_seconds(self.remaining[Color::White.index()]),
            shown_seconds(self.remaining[Color::Black.index()]),
        ];
    }

    pub fn snapshot(&mut self) -> [u64; 2] {
        self.sync();
        [
            self.remaining[Color::White.index()]
                .as_millis()
                .min(u64::MAX as u128) as u64,
            self.remaining[Color::Black.index()]
                .as_millis()
                .min(u64::MAX as u128) as u64,
        ]
    }

    pub fn restore(&mut self, remaining_ms: [u64; 2], side: Color, running: bool) {
        self.remaining = remaining_ms.map(Duration::from_millis);
        self.flagged = None;
        self.running = (self.initial.is_some() && running).then_some((side, Instant::now()));
        self.remember_shown();
    }
}

fn shown_seconds(time: Duration) -> u64 {
    time.as_millis().div_ceil(1000).min(u64::MAX as u128) as u64
}

/// The complete state of one chess game, independent of any user interface.
pub struct Game {
    pub pos: Position,
    /// The position every restart returns to.
    pub start: Position,
    pub undos: Vec<Undo>,
    /// SAN of each move played, for move lists and PGN export.
    pub sans: Vec<String>,
    /// Zobrist key of every position reached, starting with `start`.
    pub hashes: Vec<u64>,
    pub resigned: Option<Color>,
    /// A draw offer remains live until the opponent accepts or plays a move.
    pub draw_offer: Option<Color>,
    pub agreed_draw: bool,
    /// Side that lost because it did not return within the online reconnect window.
    pub abandoned: Option<Color>,
    pub paused: bool,
    /// Monotonically changes whenever persistent or visible game state changes.
    pub revision: u64,
    pub clock: GameClock,
}

impl Game {
    pub fn new(start: Position) -> Game {
        Game::with_clock(start, None, Duration::ZERO)
    }

    pub fn with_clock(start: Position, initial: Option<Duration>, increment: Duration) -> Game {
        let side = start.side;
        Game {
            pos: start.clone(),
            hashes: vec![start.hash],
            start,
            undos: Vec::new(),
            sans: Vec::new(),
            resigned: None,
            draw_offer: None,
            agreed_draw: false,
            abandoned: None,
            paused: false,
            revision: 0,
            clock: GameClock::new(initial, increment, side),
        }
    }

    pub fn play(&mut self, mv: Move) -> String {
        let mover = self.pos.side;
        if self.draw_offer == Some(mover.flip()) {
            self.draw_offer = None;
        }
        let text = to_san(&self.pos, mv);
        let undo = self.pos.make_move(mv);
        self.undos.push(undo);
        self.sans.push(text.clone());
        self.hashes.push(self.pos.hash);
        self.clock.complete_move(mover, self.pos.side);
        self.revision = self.revision.wrapping_add(1);
        text
    }

    pub fn take_back(&mut self) -> Option<String> {
        let undo = self.undos.pop()?;
        self.pos.unmake_move(undo);
        self.hashes.pop();
        self.resigned = None;
        self.draw_offer = None;
        self.agreed_draw = false;
        self.abandoned = None;
        if !self.paused {
            self.clock.resume(self.pos.side);
        }
        self.revision = self.revision.wrapping_add(1);
        self.sans.pop()
    }

    pub fn restart(&mut self) {
        self.pos = self.start.clone();
        self.undos.clear();
        self.sans.clear();
        self.hashes = vec![self.start.hash];
        self.resigned = None;
        self.draw_offer = None;
        self.agreed_draw = false;
        self.abandoned = None;
        self.paused = false;
        self.clock.reset(self.pos.side);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn last_move(&self) -> Option<Move> {
        self.undos.last().map(|undo| undo.mv)
    }

    /// The pieces `color` has taken, which are the enemy pieces in the undo log.
    pub fn captured_by(&self, color: Color) -> Vec<Piece> {
        let mut taken: Vec<Piece> = self
            .undos
            .iter()
            .filter_map(|undo| undo.captured)
            .filter(|piece| piece.color != color)
            .collect();
        taken.sort_by_key(|piece| std::cmp::Reverse(piece_value_in_pawns(piece.kind.index())));
        taken
    }

    /// Positions before the current one, used to seed engine repetition state.
    pub fn prior_positions(&self) -> &[u64] {
        &self.hashes[..self.hashes.len() - 1]
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if self.paused {
            self.clock.pause();
        } else {
            self.clock.resume(self.pos.side);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

fn piece_value_in_pawns(index: usize) -> i32 {
    [1, 3, 3, 5, 9, 0][index]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Carries the side that delivered mate.
    Checkmate(Color),
    /// Carries the side that gave up.
    Resignation(Color),
    /// Carries the side that ran out of time.
    Timeout(Color),
    /// Carries the side that did not reconnect in time.
    Abandonment(Color),
    DrawAgreement,
    Stalemate,
    FiftyMove,
    Threefold,
    Insufficient,
}

pub fn outcome(game: &Game) -> Option<Outcome> {
    if let Some(color) = game.resigned {
        return Some(Outcome::Resignation(color));
    }
    if game.agreed_draw {
        return Some(Outcome::DrawAgreement);
    }
    if let Some(color) = game.clock.flagged {
        return Some(Outcome::Timeout(color));
    }
    if let Some(color) = game.abandoned {
        return Some(Outcome::Abandonment(color));
    }
    let pos = &game.pos;
    if generate_legal(pos).is_empty() {
        return Some(if in_check(pos, pos.side) {
            Outcome::Checkmate(pos.side.flip())
        } else {
            Outcome::Stalemate
        });
    }
    if pos.halfmove >= 100 {
        return Some(Outcome::FiftyMove);
    }
    if game.hashes.iter().filter(|&&hash| hash == pos.hash).count() >= 3 {
        return Some(Outcome::Threefold);
    }
    if search::is_insufficient_material(pos) {
        return Some(Outcome::Insufficient);
    }
    None
}

pub fn describe(result: &Outcome) -> String {
    match result {
        Outcome::Checkmate(winner) => format!("Checkmate - {} wins", winner.name()),
        Outcome::Resignation(loser) => {
            format!("{} resigns - {} wins", loser.name(), loser.flip().name())
        }
        Outcome::Timeout(loser) => {
            format!(
                "{} runs out of time - {} wins",
                loser.name(),
                loser.flip().name()
            )
        }
        Outcome::Abandonment(loser) => format!(
            "{} did not reconnect - {} wins",
            loser.name(),
            loser.flip().name()
        ),
        Outcome::DrawAgreement => "Draw by agreement".to_string(),
        Outcome::Stalemate => "Stalemate - the game is drawn".to_string(),
        Outcome::FiftyMove => "Drawn by the fifty-move rule".to_string(),
        Outcome::Threefold => "Drawn by threefold repetition".to_string(),
        Outcome::Insufficient => "Drawn - neither side has enough material to mate".to_string(),
    }
}

/// A compact result explanation for fixed-width interfaces.
pub fn outcome_detail(result: &Outcome) -> String {
    match result {
        Outcome::Checkmate(winner) => format!("{} wins", winner.name()),
        Outcome::Resignation(loser) => format!("{} resigned", loser.name()),
        Outcome::Timeout(loser) => format!("{} lost on time", loser.name()),
        Outcome::Abandonment(loser) => format!("{} did not reconnect", loser.name()),
        Outcome::DrawAgreement => "By agreement".to_string(),
        Outcome::Stalemate => "Stalemate".to_string(),
        Outcome::FiftyMove => "Fifty-move rule".to_string(),
        Outcome::Threefold => "Threefold repetition".to_string(),
        Outcome::Insufficient => "Insufficient material".to_string(),
    }
}

/// The PGN result tag for the current game.
pub fn score_tag(game: &Game) -> &'static str {
    match outcome(game) {
        Some(Outcome::Checkmate(Color::White))
        | Some(Outcome::Resignation(Color::Black))
        | Some(Outcome::Timeout(Color::Black))
        | Some(Outcome::Abandonment(Color::Black)) => "1-0",
        Some(Outcome::Checkmate(Color::Black))
        | Some(Outcome::Resignation(Color::White))
        | Some(Outcome::Timeout(Color::White))
        | Some(Outcome::Abandonment(Color::White)) => "0-1",
        Some(_) => "1/2-1/2",
        None => "*",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Position;
    use crate::san::parse_move;

    #[test]
    fn game_records_moves_for_shared_clients() {
        let mut game = Game::new(Position::startpos());
        let movement = parse_move(&game.pos, "e4").ok().unwrap();

        assert_eq!(game.play(movement), "e4");
        assert_eq!(game.sans, ["e4"]);
        assert_eq!(game.revision, 1);
        assert_eq!(game.hashes.len(), 2);
    }

    #[test]
    fn detects_checkmate_outside_the_terminal_client() {
        let mut game = Game::new(Position::startpos());
        for notation in ["f3", "e5", "g4", "Qh4#"] {
            let movement = parse_move(&game.pos, notation).ok().unwrap();
            game.play(movement);
        }

        assert_eq!(outcome(&game), Some(Outcome::Checkmate(Color::Black)));
        assert_eq!(score_tag(&game), "0-1");
    }
}

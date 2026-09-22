//! The engine finds what it should, and plays below its strength only by as
//! much as it is told to.

use std::collections::HashSet;

use chess_core::board::Position;
use chess_core::san::to_san;
use chess_core::search::{Limits, Search, MATE_THRESHOLD};

fn search(fen: &str, depth: u32, randomness: i32, seed: u64) -> (String, i32) {
    let pos = Position::from_fen(fen).unwrap();
    let mut engine = Search::new();
    engine.seed(seed);
    let limits = Limits {
        depth,
        movetime: None,
        randomness,
    };
    let result = engine.think(&pos, &limits, &mut |_| {});
    (to_san(&pos, result.best.unwrap()), result.score)
}

#[test]
fn finds_a_mate_in_one() {
    // Scholar's mate is on the board.
    let fen = "r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5Q2/PPPP1PPP/RNB1K1NR w KQkq - 4 4";
    let (best, score) = search(fen, 3, 0, 1);
    assert_eq!(best, "Qxf7#");
    assert!(score >= MATE_THRESHOLD);
}

#[test]
fn a_random_choice_is_never_worse_than_the_randomness_allows() {
    let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    let (_, best) = search(start, 2, 0, 1);
    for seed in 1..=40 {
        let (_, chosen) = search(start, 2, 60, seed);
        assert!(best - chosen <= 60, "seed {seed}: {chosen} against {best}");
    }
}

#[test]
fn randomness_varies_the_moves_played() {
    let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    let quiet: HashSet<String> = (1..=20).map(|seed| search(start, 2, 0, seed).0).collect();
    assert_eq!(quiet.len(), 1, "without randomness the choice is fixed");
    let varied: HashSet<String> = (1..=20).map(|seed| search(start, 2, 80, seed).0).collect();
    assert!(varied.len() > 3, "only {varied:?}");
}

#[test]
fn even_a_weak_engine_takes_a_queen_left_for_it() {
    // Black's queen on d4 is undefended and the knight on f3 attacks it.
    let fen = "rnb1kbnr/pppp1ppp/8/4p3/3q4/5N2/PPPPPPPP/RNBQKB1R w KQkq - 0 3";
    for seed in 1..=20 {
        assert_eq!(search(fen, 1, 300, seed).0, "Nxd4", "seed {seed}");
    }
}

//! Correctness tests for the rules the online server trusts: move generation,
//! incremental hashing, make/unmake symmetry, and notation round trips.

use chess_core::board::{Move, Position, START_FEN};
use chess_core::movegen::{generate_legal, perft};
use chess_core::san::{parse_move, to_san_with};

const KIWIPETE: &str = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
const ENDGAME: &str = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
const PROMOTIONS: &str = "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1";
const TALKCHESS: &str = "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8";
const MIDDLEGAME: &str = "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10";

/// Reference counts from the Chess Programming Wiki perft results page.
fn assert_perft(fen: &str, expected: &[u64]) {
    let mut pos = Position::from_fen(fen).unwrap();
    for (depth, &nodes) in expected.iter().enumerate() {
        let depth = depth as u32 + 1;
        assert_eq!(perft(&mut pos, depth), nodes, "perft({depth}) of {fen}");
    }
    assert_eq!(pos.to_fen(), Position::from_fen(fen).unwrap().to_fen());
}

#[test]
fn perft_start_position() {
    assert_perft(START_FEN, &[20, 400, 8_902, 197_281]);
}

#[test]
fn perft_kiwipete_castling_en_passant_and_pins() {
    assert_perft(KIWIPETE, &[48, 2_039, 97_862]);
}

#[test]
fn perft_rook_endgame_with_en_passant_discovered_checks() {
    assert_perft(ENDGAME, &[14, 191, 2_812, 43_238]);
}

#[test]
fn perft_promotions_and_castling_through_check() {
    assert_perft(PROMOTIONS, &[6, 264, 9_467]);
}

#[test]
fn perft_underpromotion_captures() {
    assert_perft(TALKCHESS, &[44, 1_486, 62_379]);
}

#[test]
fn perft_quiet_middlegame() {
    assert_perft(MIDDLEGAME, &[46, 2_079, 89_890]);
}

/// Visit every node to `depth`, handing each position to `check` before its
/// moves are expanded.
fn walk(pos: &mut Position, depth: u32, check: &mut impl FnMut(&Position, &[Move])) {
    let moves = generate_legal(pos);
    check(pos, &moves);
    if depth == 0 {
        return;
    }
    for mv in moves {
        let before = pos.to_fen();
        let hash = pos.hash;
        let undo = pos.make_move(mv);
        walk(pos, depth - 1, check);
        pos.unmake_move(undo);
        assert_eq!(
            pos.to_fen(),
            before,
            "unmake of {} changed the position",
            mv.to_uci()
        );
        assert_eq!(pos.hash, hash, "unmake of {} changed the hash", mv.to_uci());
    }
}

#[test]
fn incremental_hash_matches_a_full_rehash_everywhere() {
    for fen in [START_FEN, KIWIPETE, ENDGAME, PROMOTIONS, TALKCHESS] {
        let mut pos = Position::from_fen(fen).unwrap();
        walk(&mut pos, 3, &mut |pos, _| {
            assert_eq!(
                pos.hash,
                pos.compute_hash(),
                "stale hash at {}",
                pos.to_fen()
            );
        });
    }
}

#[test]
fn fen_round_trips_at_every_node() {
    for fen in [START_FEN, KIWIPETE, PROMOTIONS] {
        let mut pos = Position::from_fen(fen).unwrap();
        walk(&mut pos, 2, &mut |pos, _| {
            let fen = pos.to_fen();
            let reparsed = Position::from_fen(&fen).unwrap();
            assert_eq!(reparsed.to_fen(), fen);
            assert_eq!(
                reparsed.hash, pos.hash,
                "hash differs after reparsing {fen}"
            );
        });
    }
}

#[test]
fn san_and_uci_parse_back_to_the_same_move() {
    for fen in [START_FEN, KIWIPETE, ENDGAME, PROMOTIONS, TALKCHESS] {
        let mut pos = Position::from_fen(fen).unwrap();
        walk(&mut pos, 1, &mut |pos, moves| {
            for &mv in moves {
                let san = to_san_with(pos, mv, moves);
                let uci = mv.to_uci();
                assert!(
                    matches!(parse_move(pos, &san), Ok(parsed) if parsed == mv),
                    "SAN `{san}` did not parse back at {}",
                    pos.to_fen()
                );
                assert!(
                    matches!(parse_move(pos, &uci), Ok(parsed) if parsed == mv),
                    "UCI `{uci}` did not parse back at {}",
                    pos.to_fen()
                );
            }
        });
    }
}

#[test]
fn from_fen_rejects_positions_without_one_king_each() {
    assert!(Position::from_fen("8/8/8/8/8/8/8/8 w - - 0 1").is_err());
    assert!(Position::from_fen("4k3/8/8/8/8/8/8/3KK3 w - - 0 1").is_err());
}

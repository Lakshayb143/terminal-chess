//! Negamax search: alpha-beta with iterative deepening, a transposition table,
//! a quiescence pass, and the move ordering that makes the pruning pay off.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::board::*;
use crate::eval::{evaluate, piece_value};
use crate::movegen::{generate_legal, generate_legal_captures, in_check};

/// Score of being mated at the root. A mate `n` plies away scores `MATE - n`,
/// so a quicker mate always outranks a slower one and the game loop can read
/// the distance back out of the score.
pub const MATE: i32 = 30_000;
/// Anything at or above this is a forced mate rather than an ordinary score.
pub const MATE_THRESHOLD: i32 = MATE - MAX_PLY as i32;
/// Deepest iteration `think` will start.
pub const MAX_DEPTH: u32 = 64;

const INFINITY: i32 = 31_000;
const DRAW: i32 = 0;
const MAX_PLY: usize = 64;
/// Power of two, so the table index is a mask of the Zobrist key.
const TT_SIZE: usize = 1 << 19;
/// Nodes between deadline checks; reading the clock is not free.
const CLOCK_INTERVAL: u64 = 2048;
/// Keeps history scores below the killer bonuses.
const HISTORY_MAX: i32 = 200_000;
/// How far a capture may fall short of alpha before quiescence skips it.
const DELTA_MARGIN: i32 = 200;

/// What stops the search, whichever of depth and time runs out first, and
/// how carefully the move is then chosen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub depth: u32,
    pub movetime: Option<Duration>,
    /// Plays below full strength: each root move's score gets a random bonus
    /// of up to this many centipawns before the best is chosen, so a move up
    /// to this much worse than the best is sometimes played, and nothing
    /// worse ever is. Zero always plays the best move found.
    pub randomness: i32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            depth: MAX_DEPTH,
            movetime: Some(Duration::from_secs(3)),
            randomness: 0,
        }
    }
}

/// The outcome of one `think` call.
pub struct SearchResult {
    /// `None` only when the side to move has no legal move at all.
    pub best: Option<Move>,
    /// Centipawns from the moving side's point of view.
    pub score: i32,
    /// Deepest iteration that ran to completion.
    pub depth: u32,
    pub nodes: u64,
    pub pv: Vec<Move>,
    pub elapsed: Duration,
}

// ---------------------------------------------------------------------------
// Transposition table
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// The score is the true value of the position.
    Exact,
    /// The search failed high: the true value is at least `score`.
    Lower,
    /// The search failed low: the true value is at most `score`.
    Upper,
}

#[derive(Clone, Copy)]
struct TtEntry {
    key: u64,
    mv: Option<Move>,
    score: i32,
    depth: i16,
    bound: Bound,
    age: u8,
}

/// `depth < 0` marks a slot that has never been written.
const TT_EMPTY: TtEntry = TtEntry {
    key: 0,
    mv: None,
    score: 0,
    depth: -1,
    bound: Bound::Exact,
    age: 0,
};

/// Mate scores are stored relative to the node, not the root, so that an entry
/// stays true when the same position turns up at a different distance.
#[inline]
fn to_tt_score(score: i32, ply: usize) -> i32 {
    if score >= MATE_THRESHOLD {
        score + ply as i32
    } else if score <= -MATE_THRESHOLD {
        score - ply as i32
    } else {
        score
    }
}

#[inline]
fn from_tt_score(score: i32, ply: usize) -> i32 {
    if score >= MATE_THRESHOLD {
        score - ply as i32
    } else if score <= -MATE_THRESHOLD {
        score + ply as i32
    } else {
        score
    }
}

// ---------------------------------------------------------------------------
// Draws that the search has to score as draws
// ---------------------------------------------------------------------------

/// True when neither side could mate even with the other side's help, which
/// makes the position dead drawn under the FIDE rules. Shared with the game
/// loop, which needs the same judgement to end a game.
pub fn is_insufficient_material(pos: &Position) -> bool {
    let mut minors = 0;
    // Bishops per square colour: two bishops on one colour can never mate.
    let mut bishops = [0u32; 2];

    for s in all_squares() {
        let piece = match pos.at(s) {
            Some(p) => p,
            None => continue,
        };
        match piece.kind {
            PieceKind::King => {}
            PieceKind::Knight => minors += 1,
            PieceKind::Bishop => {
                minors += 1;
                bishops[((file_of(s) + rank_of(s)) & 1) as usize] += 1;
            }
            // A pawn promotes, and a rook or queen mates on its own.
            _ => return false,
        }
        if minors > 2 {
            return false;
        }
    }

    match minors {
        0 | 1 => true,
        2 => bishops[0] == 2 || bishops[1] == 2,
        _ => false,
    }
}

fn has_non_pawn_material(pos: &Position, color: Color) -> bool {
    all_squares().any(|s| match pos.at(s) {
        Some(p) => p.color == color && !matches!(p.kind, PieceKind::Pawn | PieceKind::King),
        None => false,
    })
}

// ---------------------------------------------------------------------------
// The searcher
// ---------------------------------------------------------------------------

pub struct Search {
    tt: Vec<TtEntry>,
    /// Two quiet moves per ply that last caused a cutoff there.
    killers: Vec<[Option<Move>; 2]>,
    /// Cutoff counts per from/to square, indexed by raw 0x88 squares.
    history: Vec<[i32; 128]>,
    /// Triangular principal-variation table.
    pv: Vec<Vec<Move>>,
    /// Zobrist keys of every position before the one being searched, so that
    /// repetitions in the game and inside the search both read as draws.
    path: Vec<u64>,
    nodes: u64,
    deadline: Option<Instant>,
    stopped: bool,
    /// Set once depth 1 is complete: before that there is no move to fall
    /// back on, so the clock must not cut the search short.
    can_stop: bool,
    age: u8,
    /// xorshift state for [`Limits::randomness`]; never zero.
    random: u64,
}

impl Default for Search {
    fn default() -> Search {
        Search::new()
    }
}

impl Search {
    pub fn new() -> Search {
        Search {
            tt: vec![TT_EMPTY; TT_SIZE],
            killers: vec![[None; 2]; MAX_PLY],
            history: vec![[0; 128]; 128],
            pv: vec![Vec::new(); MAX_PLY],
            path: Vec::new(),
            nodes: 0,
            deadline: None,
            stopped: false,
            can_stop: false,
            age: 0,
            random: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0x9e37_79b9_7f4a_7c15, |time| time.as_nanos() as u64)
                | 1,
        }
    }

    /// Make [`Limits::randomness`] repeatable, for tests and matches.
    pub fn seed(&mut self, seed: u64) {
        self.random = seed | 1;
    }

    /// Seed the repetition list with the positions the game has already
    /// visited, oldest first, *excluding* the one about to be searched.
    pub fn set_history(&mut self, hashes: &[u64]) {
        self.path.clear();
        self.path.extend_from_slice(hashes);
    }

    /// Search `pos` and return the move to play, calling `report` after every
    /// completed iteration so a caller can show the search getting deeper
    /// while it waits.
    pub fn think(
        &mut self,
        pos: &Position,
        limits: &Limits,
        report: &mut dyn FnMut(&SearchResult),
    ) -> SearchResult {
        let start = Instant::now();
        self.deadline = limits.movetime.map(|budget| start + budget);
        self.stopped = false;
        self.can_stop = false;
        self.nodes = 0;
        self.age = self.age.wrapping_add(1);
        for slot in self.killers.iter_mut() {
            *slot = [None; 2];
        }
        // Keep history between moves but let it fade, so a cutoff from ten
        // moves ago does not outvote what is working now.
        for row in self.history.iter_mut() {
            for count in row.iter_mut() {
                *count /= 2;
            }
        }

        let mut result = SearchResult {
            best: None,
            score: 0,
            depth: 0,
            nodes: 0,
            pv: Vec::new(),
            elapsed: Duration::ZERO,
        };

        let legal = generate_legal(pos);
        if legal.is_empty() {
            result.elapsed = start.elapsed();
            return result;
        }
        // Never hand back `None` for a position that has moves, whatever the
        // clock does to the first iteration.
        result.best = Some(legal[0]);

        let mut work = pos.clone();
        let root_path = self.path.len();
        for depth in 1..=limits.depth.clamp(1, MAX_DEPTH) {
            let score = self.negamax(&mut work, depth as i32, 0, -INFINITY, INFINITY);
            // A cut-short iteration has searched only part of the root moves,
            // so its best move is not comparable; keep the previous one.
            if self.stopped {
                self.path.truncate(root_path);
                break;
            }
            if let Some(&mv) = self.pv[0].first() {
                result.best = Some(mv);
                result.pv = self.pv[0].clone();
                result.score = score;
                result.depth = depth;
                result.nodes = self.nodes;
                result.elapsed = start.elapsed();
                report(&result);
            }
            self.can_stop = true;

            // A forced mate is as good as the score gets.
            if score.abs() >= MATE_THRESHOLD {
                break;
            }
            // Starting an iteration we cannot finish only wastes the clock.
            if let Some(budget) = limits.movetime {
                if start.elapsed() * 2 > budget {
                    break;
                }
            }
        }

        if limits.randomness > 0 && legal.len() > 1 {
            self.choose_imperfectly(
                &mut work,
                &legal,
                result.depth.max(1),
                limits.randomness,
                &mut result,
            );
        }
        result.nodes = self.nodes;
        result.elapsed = start.elapsed();
        result
    }

    /// Score every root move exactly at `depth`, add up to `randomness` to
    /// each, and play the highest. Iterative deepening only proves the best
    /// move; the others need true scores for the bonus to mean anything.
    fn choose_imperfectly(
        &mut self,
        pos: &mut Position,
        legal: &[Move],
        depth: u32,
        randomness: i32,
        result: &mut SearchResult,
    ) {
        // The levels that ask for this search a few plies at most, so it
        // finishes quickly; a deadline would leave some moves unscored.
        self.deadline = None;
        self.stopped = false;
        let mut chosen: Option<(i32, i32, Vec<Move>)> = None;
        for &mv in legal {
            let undo = pos.make_move(mv);
            self.path.push(undo.hash);
            let score = -self.negamax(pos, depth as i32 - 1, 1, -INFINITY, INFINITY);
            self.path.pop();
            pos.unmake_move(undo);
            let bonus = (self.next_random() % (randomness as u64 + 1)) as i32;
            if chosen
                .as_ref()
                .is_none_or(|(best, _, _)| score + bonus > *best)
            {
                let mut line = vec![mv];
                line.extend_from_slice(&self.pv[1]);
                chosen = Some((score + bonus, score, line));
            }
        }
        if let Some((_, score, line)) = chosen {
            result.best = line.first().copied();
            result.score = score;
            result.pv = line;
        }
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*: plenty for choosing between chess moves.
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        self.random.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    // -- the recursion ------------------------------------------------------

    fn negamax(
        &mut self,
        pos: &mut Position,
        mut depth: i32,
        ply: usize,
        mut alpha: i32,
        mut beta: i32,
    ) -> i32 {
        self.pv[ply].clear();
        if self.stopped {
            return DRAW;
        }
        self.nodes += 1;
        if self.nodes.is_multiple_of(CLOCK_INTERVAL) && self.past_deadline() {
            self.stopped = true;
            return DRAW;
        }

        let root = ply == 0;
        if !root {
            if pos.halfmove >= 100 || self.is_repetition(pos) || is_insufficient_material(pos) {
                return DRAW;
            }
            // Mate-distance pruning: a mate found already is closer than
            // anything this subtree can still produce.
            alpha = alpha.max(-MATE + ply as i32);
            beta = beta.min(MATE - ply as i32 - 1);
            if alpha >= beta {
                return alpha;
            }
        }
        if ply >= MAX_PLY - 1 {
            return evaluate(pos);
        }

        // Being in check is never a quiet position, so look one move further.
        let checked = in_check(pos, pos.side);
        if checked {
            depth += 1;
        }
        if depth <= 0 {
            return self.quiesce(pos, ply, alpha, beta);
        }

        let is_pv = beta - alpha > 1;

        let mut tt_move = None;
        if let Some(entry) = self.probe(pos.hash) {
            tt_move = entry.mv;
            if !root && !is_pv && entry.depth as i32 >= depth {
                let score = from_tt_score(entry.score, ply);
                let usable = match entry.bound {
                    Bound::Exact => true,
                    Bound::Lower => score >= beta,
                    Bound::Upper => score <= alpha,
                };
                if usable {
                    return score;
                }
            }
        }

        // Null move: hand the opponent a free move, and if the position is
        // still good enough to fail high it is not worth searching properly.
        // Skipped in check and in pawn endings, where zugzwang makes passing
        // better than moving and the assumption breaks down.
        if !is_pv && !root && !checked && depth >= 3 && has_non_pawn_material(pos, pos.side) {
            let reduction = 2 + depth / 6;
            let undo = pos.make_null();
            let score = -self.negamax(pos, depth - 1 - reduction, ply + 1, -beta, -beta + 1);
            pos.unmake_null(undo);
            if self.stopped {
                return DRAW;
            }
            if score >= beta {
                // A mate score proved by a null move is not trustworthy.
                return if score >= MATE_THRESHOLD { beta } else { score };
            }
        }

        let mut moves = generate_legal(pos);
        if moves.is_empty() {
            return if checked { -MATE + ply as i32 } else { DRAW };
        }
        self.order(pos, &mut moves, tt_move, ply);

        let original_alpha = alpha;
        let mut best_score = -INFINITY;
        let mut best_move = None;

        for (searched, mv) in moves.into_iter().enumerate() {
            let quiet = is_quiet(pos, mv);
            let undo = pos.make_move(mv);
            // `undo.hash` is the position we just left, which is what a
            // repetition further down has to match against.
            self.path.push(undo.hash);

            let score = if searched == 0 {
                -self.negamax(pos, depth - 1, ply + 1, -beta, -alpha)
            } else {
                // Late quiet moves rarely beat the first one; prove it with a
                // shallow null-window search and only re-search if it does.
                let reduction = if depth >= 3 && quiet && !checked && searched >= 4 {
                    1 + (searched as i32 / 8).min(2)
                } else {
                    0
                };
                let mut score =
                    -self.negamax(pos, depth - 1 - reduction, ply + 1, -alpha - 1, -alpha);
                if score > alpha && reduction > 0 {
                    score = -self.negamax(pos, depth - 1, ply + 1, -alpha - 1, -alpha);
                }
                if score > alpha && score < beta {
                    score = -self.negamax(pos, depth - 1, ply + 1, -beta, -alpha);
                }
                score
            };

            self.path.pop();
            pos.unmake_move(undo);
            if self.stopped {
                return DRAW;
            }

            if score > best_score {
                best_score = score;
                best_move = Some(mv);
            }
            if score > alpha {
                alpha = score;
                let (head, tail) = self.pv.split_at_mut(ply + 1);
                let line = &mut head[ply];
                line.clear();
                line.push(mv);
                line.extend_from_slice(&tail[0]);
            }
            if alpha >= beta {
                // A quiet move good enough to cut off here is worth trying
                // early in sibling nodes too.
                if quiet {
                    if self.killers[ply][0] != Some(mv) {
                        self.killers[ply][1] = self.killers[ply][0];
                        self.killers[ply][0] = Some(mv);
                    }
                    let slot = &mut self.history[mv.from as usize][mv.to as usize];
                    *slot = (*slot + depth * depth).min(HISTORY_MAX);
                }
                break;
            }
        }

        let bound = if best_score >= beta {
            Bound::Lower
        } else if best_score > original_alpha {
            Bound::Exact
        } else {
            Bound::Upper
        };
        self.store(pos.hash, best_move, best_score, depth, bound, ply);
        best_score
    }

    /// Play out the captures so that the evaluation never lands in the middle
    /// of an exchange and mistakes a hanging piece for material won.
    fn quiesce(&mut self, pos: &mut Position, ply: usize, mut alpha: i32, beta: i32) -> i32 {
        if self.stopped {
            return DRAW;
        }
        self.nodes += 1;
        if self.nodes.is_multiple_of(CLOCK_INTERVAL) && self.past_deadline() {
            self.stopped = true;
            return DRAW;
        }
        if ply >= MAX_PLY - 1 {
            return evaluate(pos);
        }

        // In check there is no standing pat: every evasion has to be looked at.
        let checked = in_check(pos, pos.side);
        let (stand, mut moves) = if checked {
            (-INFINITY, generate_legal(pos))
        } else {
            let stand = evaluate(pos);
            if stand >= beta {
                return stand;
            }
            if stand > alpha {
                alpha = stand;
            }
            (stand, generate_legal_captures(pos))
        };

        if moves.is_empty() {
            return if checked { -MATE + ply as i32 } else { stand };
        }
        self.order(pos, &mut moves, None, ply);

        let mut best = stand;
        for mv in moves {
            // Delta pruning: a capture that cannot come close to alpha even
            // when it wins its target for free is not worth the nodes.
            if !checked {
                let gain = victim_of(pos, mv).map(piece_value).unwrap_or(0)
                    + mv.promo.map(piece_value).unwrap_or(0);
                if stand + gain + DELTA_MARGIN < alpha {
                    continue;
                }
            }

            let undo = pos.make_move(mv);
            self.path.push(undo.hash);
            let score = -self.quiesce(pos, ply + 1, -beta, -alpha);
            self.path.pop();
            pos.unmake_move(undo);
            if self.stopped {
                return DRAW;
            }

            if score > best {
                best = score;
                if score > alpha {
                    alpha = score;
                }
                if alpha >= beta {
                    break;
                }
            }
        }
        best
    }

    // -- move ordering ------------------------------------------------------

    fn order(&self, pos: &Position, moves: &mut [Move], tt_move: Option<Move>, ply: usize) {
        let mut scored: Vec<(i32, Move)> = moves
            .iter()
            .map(|&mv| (self.move_score(pos, mv, tt_move, ply), mv))
            .collect();
        scored.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        for (slot, (_, mv)) in moves.iter_mut().zip(scored) {
            *slot = mv;
        }
    }

    fn move_score(&self, pos: &Position, mv: Move, tt_move: Option<Move>, ply: usize) -> i32 {
        if tt_move == Some(mv) {
            return 1_000_000;
        }
        if let Some(victim) = victim_of(pos, mv) {
            // MVV-LVA: take the most valuable piece with the least valuable one.
            let attacker = pos.at(mv.from).map(|p| p.kind).unwrap_or(PieceKind::King);
            return 500_000 + piece_value(victim) * 16 - piece_value(attacker);
        }
        if let Some(promo) = mv.promo {
            return 400_000 + piece_value(promo);
        }
        if self.killers[ply][0] == Some(mv) {
            return 300_000;
        }
        if self.killers[ply][1] == Some(mv) {
            return 290_000;
        }
        self.history[mv.from as usize][mv.to as usize]
    }

    // -- bookkeeping --------------------------------------------------------

    fn past_deadline(&self) -> bool {
        match self.deadline {
            Some(deadline) => self.can_stop && Instant::now() >= deadline,
            None => false,
        }
    }

    /// Equal Zobrist keys mean the same position, side to move included, so
    /// scanning the whole path would be correct; `halfmove` is how far back a
    /// repeat can possibly live, and only bounds the work.
    fn is_repetition(&self, pos: &Position) -> bool {
        let window = (pos.halfmove as usize).min(self.path.len());
        self.path[self.path.len() - window..].contains(&pos.hash)
    }

    fn probe(&self, key: u64) -> Option<&TtEntry> {
        let entry = &self.tt[(key as usize) & (TT_SIZE - 1)];
        if entry.depth >= 0 && entry.key == key {
            Some(entry)
        } else {
            None
        }
    }

    fn store(
        &mut self,
        key: u64,
        mv: Option<Move>,
        score: i32,
        depth: i32,
        bound: Bound,
        ply: usize,
    ) {
        let slot = &mut self.tt[(key as usize) & (TT_SIZE - 1)];
        // Prefer deeper entries, but never let an old search hold a slot
        // hostage for the rest of the game.
        let replace =
            slot.depth < 0 || slot.key != key || slot.age != self.age || depth as i16 >= slot.depth;
        if !replace {
            return;
        }
        *slot = TtEntry {
            key,
            mv,
            score: to_tt_score(score, ply),
            depth: depth as i16,
            bound,
            age: self.age,
        };
    }
}

/// The piece `mv` captures, if any. En passant takes the pawn beside the
/// destination, so the target square is empty in that case.
#[inline]
fn victim_of(pos: &Position, mv: Move) -> Option<PieceKind> {
    if mv.kind == MoveKind::EnPassant {
        Some(PieceKind::Pawn)
    } else {
        pos.at(mv.to).map(|p| p.kind)
    }
}

/// A move that neither captures nor promotes, and so is eligible for the
/// killer, history and reduction heuristics.
#[inline]
fn is_quiet(pos: &Position, mv: Move) -> bool {
    victim_of(pos, mv).is_none() && mv.promo.is_none()
}

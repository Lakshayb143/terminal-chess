//! SQLite storage for accounts, sessions, linked SSH keys, and finished games.
//!
//! Everything here is synchronous and quick; the slow parts of signing in
//! (password hashing) happen in [`crate::accounts`] before the store is locked.

use std::fs;
use std::path::Path;

use chess_protocol::{FinishReason, GameRecord, GameResult, RecordedPlayer, TimeControl};
use rusqlite::{params, Connection, OptionalExtension};

pub type UserId = i64;

/// Each entry upgrades the schema by one version; never edit a released one.
const MIGRATIONS: &[&str] = &[r#"
    CREATE TABLE users (
        id            INTEGER PRIMARY KEY,
        username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
        password_hash TEXT    NOT NULL,
        created_at_ms INTEGER NOT NULL
    );
    CREATE TABLE sessions (
        token_hash      BLOB    PRIMARY KEY,
        user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at_ms   INTEGER NOT NULL,
        last_used_at_ms INTEGER NOT NULL
    );
    CREATE INDEX sessions_by_user ON sessions(user_id);
    CREATE TABLE ssh_keys (
        fingerprint TEXT    PRIMARY KEY,
        user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        added_at_ms INTEGER NOT NULL
    );
    CREATE TABLE games (
        id            TEXT    PRIMARY KEY,
        white_user    INTEGER REFERENCES users(id) ON DELETE SET NULL,
        black_user    INTEGER REFERENCES users(id) ON DELETE SET NULL,
        white_name    TEXT    NOT NULL,
        black_name    TEXT    NOT NULL,
        initial_ms    INTEGER NOT NULL,
        increment_ms  INTEGER NOT NULL,
        moves         TEXT    NOT NULL,
        result        TEXT    NOT NULL,
        reason        TEXT    NOT NULL,
        started_at_ms INTEGER NOT NULL,
        ended_at_ms   INTEGER NOT NULL
    );
    CREATE INDEX games_by_white ON games(white_user, ended_at_ms);
    CREATE INDEX games_by_black ON games(black_user, ended_at_ms);
"#];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub created_at_ms: u64,
}

/// One side of a finished game, as the hub hands it over for storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedPlayer {
    pub name: String,
    pub user: Option<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedGame {
    pub game_id: String,
    pub white: FinishedPlayer,
    pub black: FinishedPlayer,
    pub time_control: TimeControl,
    pub moves: Vec<String>,
    pub result: GameResult,
    pub reason: FinishReason,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        // The database holds password hashes, so only the server may read it.
        // SQLite gives its journal files the database file's permissions.
        #[cfg(unix)]
        if !path.exists() {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| format!("could not create {}: {error}", path.display()))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        Store::prepare(connection)
    }

    pub fn open_in_memory() -> Result<Store, String> {
        Store::prepare(Connection::open_in_memory().map_err(storage_error)?)
    }

    fn prepare(connection: Connection) -> Result<Store, String> {
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;",
            )
            .map_err(storage_error)?;
        let mut store = Store { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), String> {
        let version: i64 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(storage_error)?;
        let version = usize::try_from(version).unwrap_or(usize::MAX);
        if version > MIGRATIONS.len() {
            return Err(format!(
                "the database uses schema version {version}, newer than this server knows ({})",
                MIGRATIONS.len()
            ));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
            let transaction = self.connection.transaction().map_err(storage_error)?;
            transaction
                .execute_batch(migration)
                .map_err(storage_error)?;
            transaction
                .pragma_update(None, "user_version", (index + 1) as i64)
                .map_err(storage_error)?;
            transaction.commit().map_err(storage_error)?;
        }
        Ok(())
    }

    /// `Ok(None)` when the username is already taken, in any letter case.
    pub fn create_user(
        &mut self,
        username: &str,
        password_hash: &str,
        now_ms: u64,
    ) -> Result<Option<User>, String> {
        let inserted = self.connection.execute(
            "INSERT INTO users (username, password_hash, created_at_ms) VALUES (?1, ?2, ?3)",
            params![username, password_hash, now_ms as i64],
        );
        match inserted {
            Ok(_) => Ok(Some(User {
                id: self.connection.last_insert_rowid(),
                username: username.to_string(),
                created_at_ms: now_ms,
            })),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Ok(None)
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    /// The name a deleted account's games are kept under, in the histories of
    /// the people it played.
    pub const DELETED_PLAYER: &'static str = "deleted player";

    /// The user and their stored password hash, found case-insensitively.
    pub fn user_with_password(&self, username: &str) -> Result<Option<(User, String)>, String> {
        self.connection
            .query_row(
                "SELECT id, username, created_at_ms, password_hash FROM users WHERE username = ?1",
                [username],
                |row| Ok((user_from_row(row)?, row.get(3)?)),
            )
            .optional()
            .map_err(storage_error)
    }

    pub fn password_hash(&self, user: UserId) -> Result<Option<String>, String> {
        self.connection
            .query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                [user],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)
    }

    /// Replace the password and sign out every session but `keep`.
    pub fn set_password(
        &mut self,
        user: UserId,
        password_hash: &str,
        keep: Option<&[u8]>,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        transaction
            .execute(
                "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                params![user, password_hash],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "DELETE FROM sessions WHERE user_id = ?1 AND token_hash IS NOT ?2",
                params![user, keep],
            )
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)
    }

    /// Delete the account, its sessions and its SSH keys. Its finished games
    /// stay in its opponents' histories under [`Store::DELETED_PLAYER`].
    pub fn delete_user(&mut self, user: UserId) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(storage_error)?;
        for side in ["white", "black"] {
            transaction
                .execute(
                    &format!("UPDATE games SET {side}_name = ?2 WHERE {side}_user = ?1"),
                    params![user, Store::DELETED_PLAYER],
                )
                .map_err(storage_error)?;
        }
        // Sessions and keys go with the account; games keep a null player.
        transaction
            .execute("DELETE FROM users WHERE id = ?1", [user])
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)
    }

    pub fn create_session(
        &mut self,
        user: UserId,
        token_hash: &[u8],
        now_ms: u64,
    ) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT INTO sessions (token_hash, user_id, created_at_ms, last_used_at_ms)
                 VALUES (?1, ?2, ?3, ?3)",
                params![token_hash, user, now_ms as i64],
            )
            .map(|_| ())
            .map_err(storage_error)
    }

    /// The session's user, if the session was used more recently than
    /// `idle_limit_ms` ago. Using it keeps it alive.
    pub fn use_session(
        &mut self,
        token_hash: &[u8],
        now_ms: u64,
        idle_limit_ms: u64,
    ) -> Result<Option<User>, String> {
        let oldest = now_ms.saturating_sub(idle_limit_ms) as i64;
        let user = self
            .connection
            .query_row(
                "SELECT users.id, users.username, users.created_at_ms
                 FROM sessions JOIN users ON users.id = sessions.user_id
                 WHERE sessions.token_hash = ?1 AND sessions.last_used_at_ms >= ?2",
                params![token_hash, oldest],
                user_from_row,
            )
            .optional()
            .map_err(storage_error)?;
        if user.is_some() {
            self.connection
                .execute(
                    "UPDATE sessions SET last_used_at_ms = ?2 WHERE token_hash = ?1",
                    params![token_hash, now_ms as i64],
                )
                .map_err(storage_error)?;
        }
        Ok(user)
    }

    pub fn delete_session(&mut self, token_hash: &[u8]) -> Result<(), String> {
        self.connection
            .execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])
            .map(|_| ())
            .map_err(storage_error)
    }

    /// Remove sessions nobody has used within `idle_limit_ms`.
    pub fn delete_idle_sessions(
        &mut self,
        now_ms: u64,
        idle_limit_ms: u64,
    ) -> Result<usize, String> {
        let oldest = now_ms.saturating_sub(idle_limit_ms) as i64;
        self.connection
            .execute("DELETE FROM sessions WHERE last_used_at_ms < ?1", [oldest])
            .map_err(storage_error)
    }

    /// Link `fingerprint` to `user`, moving it if another account had it.
    pub fn link_ssh_key(
        &mut self,
        user: UserId,
        fingerprint: &str,
        now_ms: u64,
    ) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT INTO ssh_keys (fingerprint, user_id, added_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT (fingerprint) DO UPDATE SET user_id = ?2, added_at_ms = ?3",
                params![fingerprint, user, now_ms as i64],
            )
            .map(|_| ())
            .map_err(storage_error)
    }

    /// The account's keys as `(fingerprint, added_at_ms)`, oldest first.
    pub fn ssh_keys_for(&self, user: UserId) -> Result<Vec<(String, u64)>, String> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT fingerprint, added_at_ms FROM ssh_keys WHERE user_id = ?1
                 ORDER BY added_at_ms, fingerprint",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([user], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(storage_error)?;
        rows.collect::<Result<_, _>>().map_err(storage_error)
    }

    /// Returns whether the key was linked to this account.
    pub fn unlink_ssh_key(&mut self, user: UserId, fingerprint: &str) -> Result<bool, String> {
        self.connection
            .execute(
                "DELETE FROM ssh_keys WHERE user_id = ?1 AND fingerprint = ?2",
                params![user, fingerprint],
            )
            .map(|removed| removed > 0)
            .map_err(storage_error)
    }

    pub fn user_for_ssh_key(&self, fingerprint: &str) -> Result<Option<User>, String> {
        self.connection
            .query_row(
                "SELECT users.id, users.username, users.created_at_ms
                 FROM ssh_keys JOIN users ON users.id = ssh_keys.user_id
                 WHERE ssh_keys.fingerprint = ?1",
                [fingerprint],
                user_from_row,
            )
            .optional()
            .map_err(storage_error)
    }

    /// Store a finished game. Recording the same game twice keeps the first.
    /// A player whose account was deleted during the game is kept as a guest.
    pub fn record_game(&mut self, game: &FinishedGame) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO games (
                    id, white_user, black_user, white_name, black_name, initial_ms,
                    increment_ms, moves, result, reason, started_at_ms, ended_at_ms
                 ) VALUES (
                    ?1, (SELECT id FROM users WHERE id = ?2), (SELECT id FROM users WHERE id = ?3),
                    ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
                 )",
                params![
                    game.game_id,
                    game.white.user,
                    game.black.user,
                    game.white.name,
                    game.black.name,
                    game.time_control.initial_ms as i64,
                    game.time_control.increment_ms as i64,
                    game.moves.join(" "),
                    enum_text(&game.result),
                    enum_text(&game.reason),
                    game.started_at_ms as i64,
                    game.ended_at_ms as i64,
                ],
            )
            .map(|_| ())
            .map_err(storage_error)
    }

    /// The user's most recent finished games, newest first.
    pub fn games_for(&self, user: UserId, limit: u32) -> Result<Vec<GameRecord>, String> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT id, white_name, white_user IS NOT NULL, black_name, black_user IS NOT NULL,
                        initial_ms, increment_ms, moves, result, reason, started_at_ms, ended_at_ms
                 FROM games WHERE white_user = ?1 OR black_user = ?1
                 ORDER BY ended_at_ms DESC, rowid DESC LIMIT ?2",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map(params![user, limit], |row| {
                let moves: String = row.get(7)?;
                Ok(GameRecord {
                    game_id: row.get(0)?,
                    white: RecordedPlayer {
                        name: row.get(1)?,
                        registered: row.get(2)?,
                    },
                    black: RecordedPlayer {
                        name: row.get(3)?,
                        registered: row.get(4)?,
                    },
                    time_control: TimeControl {
                        initial_ms: row.get::<_, i64>(5)? as u64,
                        increment_ms: row.get::<_, i64>(6)? as u64,
                    },
                    moves: moves.split_whitespace().map(str::to_string).collect(),
                    result: text_enum(row.get(8)?, 8)?,
                    reason: text_enum(row.get(9)?, 9)?,
                    started_at_ms: row.get::<_, i64>(10)? as u64,
                    ended_at_ms: row.get::<_, i64>(11)? as u64,
                })
            })
            .map_err(storage_error)?;
        rows.collect::<Result<_, _>>().map_err(storage_error)
    }
}

fn user_from_row(row: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        username: row.get(1)?,
        created_at_ms: row.get::<_, i64>(2)? as u64,
    })
}

/// Store enums as their wire names so the database reads like the protocol.
fn enum_text<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => unreachable!("wire enums serialize to strings"),
    }
}

fn text_enum<T: serde::de::DeserializeOwned>(text: String, column: usize) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(text)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, error.into())
    })
}

fn storage_error(error: rusqlite::Error) -> String {
    format!("database error: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(
        game_id: &str,
        white: Option<UserId>,
        black: Option<UserId>,
        ended: u64,
    ) -> FinishedGame {
        FinishedGame {
            game_id: game_id.to_string(),
            white: FinishedPlayer {
                name: "white".to_string(),
                user: white,
            },
            black: FinishedPlayer {
                name: "black".to_string(),
                user: black,
            },
            time_control: TimeControl {
                initial_ms: 300_000,
                increment_ms: 0,
            },
            moves: vec!["f2f3".to_string(), "e7e5".to_string()],
            result: GameResult::BlackWins,
            reason: FinishReason::Resignation,
            started_at_ms: ended - 1_000,
            ended_at_ms: ended,
        }
    }

    #[test]
    fn usernames_are_unique_ignoring_case() {
        let mut store = Store::open_in_memory().unwrap();
        let user = store.create_user("Magnus", "hash", 1).unwrap().unwrap();
        assert_eq!(store.create_user("magnus", "other", 2).unwrap(), None);
        let (found, hash) = store.user_with_password("MAGNUS").unwrap().unwrap();
        assert_eq!(found, user);
        assert_eq!(found.username, "Magnus");
        assert_eq!(hash, "hash");
    }

    #[test]
    fn sessions_expire_when_idle_and_can_be_revoked() {
        let mut store = Store::open_in_memory().unwrap();
        let user = store.create_user("anna", "hash", 0).unwrap().unwrap();
        store.create_session(user.id, b"token", 1_000).unwrap();
        assert_eq!(
            store.use_session(b"token", 1_500, 1_000).unwrap(),
            Some(user.clone())
        );
        // Using it at 1 500 keeps it alive until 2 500.
        assert!(store.use_session(b"token", 2_400, 1_000).unwrap().is_some());
        assert!(store.use_session(b"token", 3_500, 1_000).unwrap().is_none());
        assert_eq!(store.delete_idle_sessions(3_500, 1_000).unwrap(), 1);

        store.create_session(user.id, b"second", 4_000).unwrap();
        store.delete_session(b"second").unwrap();
        assert!(store
            .use_session(b"second", 4_000, 1_000)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_linked_ssh_key_moves_to_the_latest_account() {
        let mut store = Store::open_in_memory().unwrap();
        let first = store.create_user("first", "hash", 0).unwrap().unwrap();
        let second = store.create_user("second", "hash", 0).unwrap().unwrap();
        assert_eq!(store.user_for_ssh_key("SHA256:abc").unwrap(), None);
        store.link_ssh_key(first.id, "SHA256:abc", 1).unwrap();
        assert_eq!(store.user_for_ssh_key("SHA256:abc").unwrap(), Some(first));
        store.link_ssh_key(second.id, "SHA256:abc", 2).unwrap();
        assert_eq!(store.user_for_ssh_key("SHA256:abc").unwrap(), Some(second));
    }

    #[test]
    fn game_history_is_per_player_newest_first_and_recorded_once() {
        let mut store = Store::open_in_memory().unwrap();
        let anna = store.create_user("anna", "hash", 0).unwrap().unwrap();
        let bob = store.create_user("bob", "hash", 0).unwrap().unwrap();
        store
            .record_game(&finished("one", Some(anna.id), None, 10_000))
            .unwrap();
        store
            .record_game(&finished("two", Some(bob.id), Some(anna.id), 20_000))
            .unwrap();
        store
            .record_game(&finished("two", None, None, 30_000))
            .unwrap();
        store
            .record_game(&finished("three", Some(bob.id), None, 40_000))
            .unwrap();

        let games = store.games_for(anna.id, 10).unwrap();
        let ids: Vec<&str> = games.iter().map(|game| game.game_id.as_str()).collect();
        assert_eq!(ids, ["two", "one"]);
        assert!(games[0].white.registered && games[0].black.registered);
        assert!(!games[1].black.registered);
        assert_eq!(games[1].moves, ["f2f3", "e7e5"]);
        assert_eq!(games[1].result, GameResult::BlackWins);
        assert_eq!(games[1].reason, FinishReason::Resignation);
        assert_eq!(store.games_for(anna.id, 1).unwrap().len(), 1);
    }

    #[test]
    fn a_new_password_signs_out_every_other_session() {
        let mut store = Store::open_in_memory().unwrap();
        let anna = store.create_user("anna", "old", 0).unwrap().unwrap();
        let bob = store.create_user("bob", "hash", 0).unwrap().unwrap();
        store.create_session(anna.id, b"here", 0).unwrap();
        store.create_session(anna.id, b"laptop", 0).unwrap();
        store.create_session(bob.id, b"bob's", 0).unwrap();
        store.set_password(anna.id, "new", Some(b"here")).unwrap();
        assert_eq!(
            store.password_hash(anna.id).unwrap().as_deref(),
            Some("new")
        );
        assert!(store.use_session(b"here", 1, 1_000).unwrap().is_some());
        assert!(store.use_session(b"laptop", 1, 1_000).unwrap().is_none());
        assert!(store.use_session(b"bob's", 1, 1_000).unwrap().is_some());
    }

    #[test]
    fn ssh_keys_are_listed_and_unlinked_per_account() {
        let mut store = Store::open_in_memory().unwrap();
        let anna = store.create_user("anna", "hash", 0).unwrap().unwrap();
        let bob = store.create_user("bob", "hash", 0).unwrap().unwrap();
        store.link_ssh_key(anna.id, "SHA256:laptop", 2).unwrap();
        store.link_ssh_key(anna.id, "SHA256:desktop", 1).unwrap();
        store.link_ssh_key(bob.id, "SHA256:bob", 3).unwrap();
        assert_eq!(
            store.ssh_keys_for(anna.id).unwrap(),
            [
                ("SHA256:desktop".to_string(), 1),
                ("SHA256:laptop".to_string(), 2)
            ]
        );
        // Nobody can unlink someone else's key.
        assert!(!store.unlink_ssh_key(anna.id, "SHA256:bob").unwrap());
        assert!(store.unlink_ssh_key(anna.id, "SHA256:laptop").unwrap());
        assert_eq!(store.user_for_ssh_key("SHA256:laptop").unwrap(), None);
        assert_eq!(store.ssh_keys_for(anna.id).unwrap().len(), 1);
    }

    #[test]
    fn a_deleted_account_leaves_its_games_nameless_in_others_histories() {
        let mut store = Store::open_in_memory().unwrap();
        let anna = store.create_user("anna", "hash", 0).unwrap().unwrap();
        let bob = store.create_user("bob", "hash", 0).unwrap().unwrap();
        store.create_session(anna.id, b"token", 0).unwrap();
        store.link_ssh_key(anna.id, "SHA256:key", 0).unwrap();
        let mut game = finished("one", Some(anna.id), Some(bob.id), 10_000);
        game.white.name = "anna".to_string();
        store.record_game(&game).unwrap();

        store.delete_user(anna.id).unwrap();
        assert!(store.user_with_password("anna").unwrap().is_none());
        assert!(store.use_session(b"token", 1, 1_000).unwrap().is_none());
        assert_eq!(store.user_for_ssh_key("SHA256:key").unwrap(), None);
        let games = store.games_for(bob.id, 10).unwrap();
        assert_eq!(games[0].white.name, Store::DELETED_PLAYER);
        assert!(!games[0].white.registered);
        // The name is free again.
        assert!(store.create_user("anna", "hash", 1).unwrap().is_some());

        // A game that ends after its player's account went is still kept.
        store
            .record_game(&finished("two", Some(anna.id), Some(bob.id), 20_000))
            .unwrap();
        assert_eq!(store.games_for(bob.id, 10).unwrap().len(), 2);
    }

    #[test]
    fn a_file_database_keeps_its_data_and_schema_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/chess.db");
        {
            let mut store = Store::open(&path).unwrap();
            store.create_user("anna", "hash", 0).unwrap().unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.user_with_password("anna").unwrap().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "password hashes must not be readable by others"
            );
        }
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
    }
}

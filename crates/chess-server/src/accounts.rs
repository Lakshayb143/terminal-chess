//! Accounts: usernames, passwords, session tokens, and SSH key links.
//!
//! Passwords are hashed with Argon2id on the blocking pool, a few at a time,
//! so sign-ins neither stall the async runtime nor exhaust memory. Session
//! tokens are random and only their SHA-256 digest is stored, so a copy of
//! the database cannot be used to sign in.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chess_protocol::{Account, ErrorCode, GameRecord, SshKey};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::store::{FinishedGame, Store, User, UserId};

/// A session nobody uses for this long is signed out.
pub const SESSION_IDLE_LIMIT: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// How long an SSH connection's link ticket stays valid.
const LINK_TICKET_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
const MIN_USERNAME_CHARS: usize = 3;
const MAX_USERNAME_CHARS: usize = 20;
const MIN_PASSWORD_CHARS: usize = 8;
/// Argon2 accepts any length; a cap keeps a single request cheap to hash.
const MAX_PASSWORD_BYTES: usize = 256;
/// Each Argon2id hash with the default parameters uses 19 MiB.
const CONCURRENT_HASHES: usize = 4;
const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "anonymous",
    "chess",
    "guest",
    "help",
    "mod",
    "moderator",
    "root",
    "server",
    "staff",
    "support",
    "system",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountError {
    InvalidUsername(String),
    InvalidPassword(String),
    UsernameTaken,
    InvalidCredentials,
    /// The password given to confirm a change was not the account's.
    WrongPassword,
    InvalidSession,
    InvalidTicket,
    UnknownKey,
    /// The database failed. The detail is logged, not shown to players.
    Storage(String),
}

impl AccountError {
    pub fn code(&self) -> ErrorCode {
        match self {
            AccountError::InvalidUsername(_)
            | AccountError::InvalidPassword(_)
            | AccountError::InvalidTicket
            | AccountError::UnknownKey => ErrorCode::InvalidRequest,
            AccountError::UsernameTaken => ErrorCode::UsernameTaken,
            AccountError::InvalidCredentials | AccountError::WrongPassword => {
                ErrorCode::InvalidCredentials
            }
            AccountError::InvalidSession => ErrorCode::InvalidSession,
            AccountError::Storage(_) => ErrorCode::Internal,
        }
    }

    /// Text meant for the player.
    pub fn message(&self) -> String {
        match self {
            AccountError::InvalidUsername(why) | AccountError::InvalidPassword(why) => why.clone(),
            AccountError::UsernameTaken => "that username is taken; try another".to_string(),
            AccountError::InvalidCredentials => "wrong username or password".to_string(),
            AccountError::WrongPassword => "that is not your password".to_string(),
            AccountError::UnknownKey => "that key is not linked to your account".to_string(),
            AccountError::InvalidSession => "you have been signed out; sign in again".to_string(),
            AccountError::InvalidTicket => "this SSH key can no longer be linked".to_string(),
            AccountError::Storage(_) => "the server could not reach its database".to_string(),
        }
    }
}

/// A signed-in account and the new session token that proves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignIn {
    pub user: User,
    pub token: String,
}

struct LinkTicket {
    fingerprint: String,
    issued: Instant,
}

/// The account service shared by every connection. Cloning is cheap.
#[derive(Clone)]
pub struct Accounts {
    store: Arc<Mutex<Store>>,
    hashing: Arc<Semaphore>,
    tickets: Arc<Mutex<HashMap<String, LinkTicket>>>,
}

impl Accounts {
    pub fn new(store: Store) -> Accounts {
        Accounts {
            store: Arc::new(Mutex::new(store)),
            hashing: Arc::new(Semaphore::new(CONCURRENT_HASHES)),
            tickets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register(&self, username: &str, password: &str) -> Result<SignIn, AccountError> {
        let username = validate_username(username)?;
        validate_password(password, &username)?;
        let hash = self.hash(password.to_string()).await?;
        let created = self
            .with_store(move |store| store.create_user(&username, &hash, now_ms()))
            .await?;
        let user = created.ok_or(AccountError::UsernameTaken)?;
        let token = self.start_session(user.id).await?;
        Ok(SignIn { user, token })
    }

    pub async fn log_in(&self, username: &str, password: &str) -> Result<SignIn, AccountError> {
        let username = username.trim().to_string();
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(AccountError::InvalidCredentials);
        }
        let found = self
            .with_store(move |store| store.user_with_password(&username))
            .await?;
        // An unknown username still costs a full hash, so response times do
        // not reveal which usernames exist.
        let (user, hash) = match found {
            Some((user, hash)) => (Some(user), hash),
            None => (None, decoy_hash().to_string()),
        };
        let matched = self.verify(password.to_string(), hash).await?;
        let user = user
            .filter(|_| matched)
            .ok_or(AccountError::InvalidCredentials)?;
        let token = self.start_session(user.id).await?;
        Ok(SignIn { user, token })
    }

    pub async fn authenticate(&self, token: &str) -> Result<User, AccountError> {
        let digest = token_digest(token);
        self.with_store(move |store| {
            store.use_session(&digest, now_ms(), duration_ms(SESSION_IDLE_LIMIT))
        })
        .await?
        .ok_or(AccountError::InvalidSession)
    }

    pub async fn log_out(&self, token: &str) -> Result<(), AccountError> {
        let digest = token_digest(token);
        self.with_store(move |store| store.delete_session(&digest))
            .await
    }

    /// A new session for an account that has already proved who it is.
    pub async fn start_session(&self, user: UserId) -> Result<String, AccountError> {
        let token = random_token();
        let digest = token_digest(&token);
        self.with_store(move |store| store.create_session(user, &digest, now_ms()))
            .await?;
        Ok(token)
    }

    pub async fn user_for_ssh_key(&self, fingerprint: &str) -> Result<Option<User>, AccountError> {
        let fingerprint = fingerprint.to_string();
        self.with_store(move |store| store.user_for_ssh_key(&fingerprint))
            .await
    }

    /// A one-time ticket that lets whoever holds it link `fingerprint` to the
    /// account they sign in to. The SSH gateway hands it to the program it
    /// starts, so the key itself never has to cross the WebSocket.
    pub fn issue_link_ticket(&self, fingerprint: &str) -> String {
        let ticket = random_token();
        let mut tickets = self.tickets.lock().expect("ticket lock poisoned");
        tickets.retain(|_, ticket| ticket.issued.elapsed() < LINK_TICKET_LIFETIME);
        tickets.insert(
            ticket.clone(),
            LinkTicket {
                fingerprint: fingerprint.to_string(),
                issued: Instant::now(),
            },
        );
        ticket
    }

    pub fn revoke_link_ticket(&self, ticket: &str) {
        self.tickets
            .lock()
            .expect("ticket lock poisoned")
            .remove(ticket);
    }

    pub async fn link_ssh_key(&self, user: UserId, ticket: &str) -> Result<(), AccountError> {
        let fingerprint = {
            let mut tickets = self.tickets.lock().expect("ticket lock poisoned");
            match tickets.remove(ticket) {
                Some(ticket) if ticket.issued.elapsed() < LINK_TICKET_LIFETIME => {
                    ticket.fingerprint
                }
                _ => return Err(AccountError::InvalidTicket),
            }
        };
        self.with_store(move |store| store.link_ssh_key(user, &fingerprint, now_ms()))
            .await
    }

    /// Check `current`, then replace it with `new`. Every session but `keep`,
    /// the one asking, is signed out.
    pub async fn change_password(
        &self,
        user: UserId,
        username: &str,
        current: &str,
        new: &str,
        keep: Option<&str>,
    ) -> Result<(), AccountError> {
        self.confirm_password(user, current).await?;
        validate_password(new, username)?;
        let hash = self.hash(new.to_string()).await?;
        let keep = keep.map(token_digest);
        self.with_store(move |store| store.set_password(user, &hash, keep.as_deref()))
            .await
    }

    /// Check `password`, then delete the account for good.
    pub async fn delete_account(&self, user: UserId, password: &str) -> Result<(), AccountError> {
        self.confirm_password(user, password).await?;
        self.with_store(move |store| store.delete_user(user)).await
    }

    pub async fn ssh_keys(&self, user: UserId) -> Result<Vec<SshKey>, AccountError> {
        let keys = self
            .with_store(move |store| store.ssh_keys_for(user))
            .await?;
        Ok(keys
            .into_iter()
            .map(|(fingerprint, added_at_ms)| SshKey {
                fingerprint,
                added_at_ms,
            })
            .collect())
    }

    pub async fn unlink_ssh_key(
        &self,
        user: UserId,
        fingerprint: &str,
    ) -> Result<(), AccountError> {
        let fingerprint = fingerprint.to_string();
        let removed = self
            .with_store(move |store| store.unlink_ssh_key(user, &fingerprint))
            .await?;
        if removed {
            Ok(())
        } else {
            Err(AccountError::UnknownKey)
        }
    }

    /// `Err(WrongPassword)` unless `password` is the account's.
    async fn confirm_password(&self, user: UserId, password: &str) -> Result<(), AccountError> {
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(AccountError::WrongPassword);
        }
        let hash = self
            .with_store(move |store| store.password_hash(user))
            .await?
            .ok_or(AccountError::InvalidSession)?;
        if self.verify(password.to_string(), hash).await? {
            Ok(())
        } else {
            Err(AccountError::WrongPassword)
        }
    }

    pub async fn record_games(&self, games: Vec<FinishedGame>) -> Result<(), AccountError> {
        if games.is_empty() {
            return Ok(());
        }
        self.with_store(move |store| games.iter().try_for_each(|game| store.record_game(game)))
            .await
    }

    pub async fn games_for(
        &self,
        user: UserId,
        limit: u32,
    ) -> Result<Vec<GameRecord>, AccountError> {
        self.with_store(move |store| store.games_for(user, limit))
            .await
    }

    pub async fn delete_idle_sessions(&self) -> Result<usize, AccountError> {
        self.with_store(|store| {
            store.delete_idle_sessions(now_ms(), duration_ms(SESSION_IDLE_LIMIT))
        })
        .await
    }

    async fn with_store<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Store) -> Result<T, String> + Send + 'static,
    ) -> Result<T, AccountError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let mut store = store
                .lock()
                .map_err(|_| "database lock poisoned".to_string())?;
            work(&mut store)
        })
        .await
        .map_err(|error| AccountError::Storage(format!("database task failed: {error}")))?
        .map_err(AccountError::Storage)
    }

    async fn hash(&self, password: String) -> Result<String, AccountError> {
        let _permit = self
            .hashing
            .acquire()
            .await
            .expect("hashing semaphore is never closed");
        tokio::task::spawn_blocking(move || hash_password(&password))
            .await
            .map_err(|error| AccountError::Storage(format!("hashing task failed: {error}")))?
    }

    async fn verify(&self, password: String, hash: String) -> Result<bool, AccountError> {
        let _permit = self
            .hashing
            .acquire()
            .await
            .expect("hashing semaphore is never closed");
        tokio::task::spawn_blocking(move || {
            Argon2::default()
                .verify_password(password.as_bytes(), hash.as_str())
                .is_ok()
        })
        .await
        .map_err(|error| AccountError::Storage(format!("hashing task failed: {error}")))
    }
}

impl From<&User> for Account {
    fn from(user: &User) -> Account {
        Account {
            username: user.username.clone(),
            created_at_ms: user.created_at_ms,
        }
    }
}

/// The username as it will be stored, or why it cannot be one.
pub fn validate_username(username: &str) -> Result<String, AccountError> {
    let username = username.trim();
    let invalid = |why: &str| Err(AccountError::InvalidUsername(why.to_string()));
    let length = username.chars().count();
    if length < MIN_USERNAME_CHARS {
        return invalid("a username needs at least 3 characters");
    }
    if length > MAX_USERNAME_CHARS {
        return invalid("a username can have at most 20 characters");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return invalid("use only letters, digits, - and _ in a username");
    }
    if !username.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return invalid("a username must start with a letter or digit");
    }
    let lower = username.to_ascii_lowercase();
    if RESERVED_USERNAMES.contains(&lower.as_str()) {
        return invalid("that username is reserved; try another");
    }
    Ok(username.to_string())
}

pub fn validate_password(password: &str, username: &str) -> Result<(), AccountError> {
    let invalid = |why: &str| Err(AccountError::InvalidPassword(why.to_string()));
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return invalid("a password needs at least 8 characters");
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return invalid("that password is too long");
    }
    if password.eq_ignore_ascii_case(username) {
        return invalid("your password cannot be your username");
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, AccountError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| AccountError::Storage(format!("could not hash password: {error}")))
}

/// A real hash of a password nobody knows, checked against when a username
/// does not exist.
fn decoy_hash() -> &'static str {
    static DECOY: OnceLock<String> = OnceLock::new();
    DECOY.get_or_init(|| hash_password(&random_token()).expect("hashing a random password"))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system has no random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn token_digest(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_ms)
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts() -> Accounts {
        Accounts::new(Store::open_in_memory().unwrap())
    }

    #[test]
    fn usernames_are_checked_before_anything_is_stored() {
        assert_eq!(validate_username("  Magnus_C  ").unwrap(), "Magnus_C");
        assert_eq!(validate_username("a-1").unwrap(), "a-1");
        for bad in [
            "ab",
            "has space",
            "émile",
            "_under",
            "-dash",
            "Guest",
            "ADMIN",
        ] {
            assert!(validate_username(bad).is_err(), "{bad} should be rejected");
        }
        assert!(validate_username(&"x".repeat(21)).is_err());
    }

    #[test]
    fn passwords_need_eight_characters_and_are_not_the_username() {
        assert!(validate_password("correct horse", "anna").is_ok());
        assert!(validate_password("short", "anna").is_err());
        assert!(validate_password("Annabelle", "annabelle").is_err());
        assert!(validate_password(&"p".repeat(300), "anna").is_err());
    }

    #[tokio::test]
    async fn register_log_in_authenticate_and_log_out() {
        let accounts = accounts();
        let registered = accounts.register("Anna", "correct horse").await.unwrap();
        assert_eq!(registered.user.username, "Anna");
        assert_eq!(
            accounts.register("anna", "another pass").await,
            Err(AccountError::UsernameTaken)
        );

        assert_eq!(
            accounts.log_in("ANNA", "wrong horse").await,
            Err(AccountError::InvalidCredentials)
        );
        assert_eq!(
            accounts.log_in("nobody", "correct horse").await,
            Err(AccountError::InvalidCredentials)
        );
        let second = accounts.log_in("ANNA", "correct horse").await.unwrap();
        assert_ne!(second.token, registered.token);

        assert_eq!(
            accounts.authenticate(&registered.token).await.unwrap(),
            registered.user
        );
        accounts.log_out(&registered.token).await.unwrap();
        assert_eq!(
            accounts.authenticate(&registered.token).await,
            Err(AccountError::InvalidSession)
        );
        // Signing out on one computer leaves the others signed in.
        assert!(accounts.authenticate(&second.token).await.is_ok());
    }

    #[tokio::test]
    async fn changing_the_password_needs_the_old_one_and_signs_out_elsewhere() {
        let accounts = accounts();
        let here = accounts.register("anna", "correct horse").await.unwrap();
        let elsewhere = accounts.log_in("anna", "correct horse").await.unwrap();
        let change = |current: &'static str, new: &'static str| {
            accounts.change_password(here.user.id, "anna", current, new, Some(&here.token))
        };
        assert_eq!(
            change("wrong horse", "battery staple").await,
            Err(AccountError::WrongPassword)
        );
        assert!(matches!(
            change("correct horse", "short").await,
            Err(AccountError::InvalidPassword(_))
        ));
        change("correct horse", "battery staple").await.unwrap();
        assert!(accounts.authenticate(&here.token).await.is_ok());
        assert_eq!(
            accounts.authenticate(&elsewhere.token).await,
            Err(AccountError::InvalidSession)
        );
        assert_eq!(
            accounts.log_in("anna", "correct horse").await,
            Err(AccountError::InvalidCredentials)
        );
        assert!(accounts.log_in("anna", "battery staple").await.is_ok());
    }

    #[tokio::test]
    async fn an_account_is_deleted_only_with_its_password() {
        let accounts = accounts();
        let anna = accounts.register("anna", "correct horse").await.unwrap();
        assert_eq!(
            accounts.delete_account(anna.user.id, "wrong horse").await,
            Err(AccountError::WrongPassword)
        );
        accounts
            .delete_account(anna.user.id, "correct horse")
            .await
            .unwrap();
        assert_eq!(
            accounts.authenticate(&anna.token).await,
            Err(AccountError::InvalidSession)
        );
        assert_eq!(
            accounts.log_in("anna", "correct horse").await,
            Err(AccountError::InvalidCredentials)
        );
    }

    #[tokio::test]
    async fn keys_are_listed_and_unlinked() {
        let accounts = accounts();
        let anna = accounts
            .register("anna", "correct horse")
            .await
            .unwrap()
            .user;
        let ticket = accounts.issue_link_ticket("SHA256:key");
        accounts.link_ssh_key(anna.id, &ticket).await.unwrap();
        let keys = accounts.ssh_keys(anna.id).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].fingerprint, "SHA256:key");
        accounts
            .unlink_ssh_key(anna.id, "SHA256:key")
            .await
            .unwrap();
        assert_eq!(
            accounts.unlink_ssh_key(anna.id, "SHA256:key").await,
            Err(AccountError::UnknownKey)
        );
        assert_eq!(accounts.user_for_ssh_key("SHA256:key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_link_ticket_links_its_key_once() {
        let accounts = accounts();
        let anna = accounts
            .register("anna", "correct horse")
            .await
            .unwrap()
            .user;
        let ticket = accounts.issue_link_ticket("SHA256:key");
        accounts.link_ssh_key(anna.id, &ticket).await.unwrap();
        assert_eq!(
            accounts.user_for_ssh_key("SHA256:key").await.unwrap(),
            Some(anna.clone())
        );
        assert_eq!(
            accounts.link_ssh_key(anna.id, &ticket).await,
            Err(AccountError::InvalidTicket)
        );

        let revoked = accounts.issue_link_ticket("SHA256:other");
        accounts.revoke_link_ticket(&revoked);
        assert_eq!(
            accounts.link_ssh_key(anna.id, &revoked).await,
            Err(AccountError::InvalidTicket)
        );
    }
}

//! D52: password accounts (`docs/ACCOUNTS.md`). Argon2id with the crate's defaults (OWASP:
//! m = 19 MiB, t = 2, p = 1), stored as a PHC string on the account log. Hashing and checking
//! run on blocking threads (principle VI): each is tens of milliseconds of CPU.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// NIST SP 800-63B: at least 8 characters, long passphrases allowed, no composition rules.
pub fn check_new(password: &str) -> Result<(), String> {
    let n = password.chars().count();
    if n < 8 {
        return Err("a password is at least 8 characters".into());
    }
    if n > 256 {
        return Err("a password is at most 256 characters".into());
    }
    Ok(())
}

/// The PHC string of a new password (fresh random salt).
pub fn hash_blocking(password: &str) -> Result<String, String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).map_err(|e| e.to_string())?;
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub async fn hash(password: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || hash_blocking(&password))
        .await
        .map_err(|e| e.to_string())?
}

/// Whether `password` matches `phc`. An empty `phc` (an unknown user, or one without a
/// password) still costs one Argon2 run, so timing doesn't say which accounts exist.
pub async fn verify(password: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let target = if phc.is_empty() {
            dummy()
        } else {
            phc.as_str()
        };
        let ok = PasswordHash::new(target).is_ok_and(|h| {
            argon2::Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        });
        ok && !phc.is_empty()
    })
    .await
    .unwrap_or(false)
}

/// A real hash of nothing anyone knows, for the unknown-user path.
fn dummy() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| hash_blocking(&uuid::Uuid::new_v4().to_string()).unwrap_or_default())
}

/// Failed sign-ins per login: after `MAX_FAILURES` in `WINDOW`, refuse until the window ends.
// ponytail: per instance and in memory (a cache: losing it loses nothing but the count). An
// attacker spreading guesses over many instances gets MAX_FAILURES per instance per window;
// a bucket-backed counter is the upgrade if that ever matters.
#[derive(Default)]
pub struct Throttle {
    failures: parking_lot::Mutex<HashMap<String, (u32, Instant)>>,
}

const MAX_FAILURES: u32 = 10;
const WINDOW: Duration = Duration::from_mins(15);

impl Throttle {
    /// `None` = may try; `Some(wait)` = locked out for `wait`.
    pub fn locked(&self, login: &str) -> Option<Duration> {
        let mut map = self.failures.lock();
        let (n, since) = *map.get(&login.to_ascii_lowercase())?;
        let left = WINDOW.checked_sub(since.elapsed());
        match left {
            None => {
                map.remove(&login.to_ascii_lowercase());
                None
            }
            Some(left) => (n >= MAX_FAILURES).then_some(left),
        }
    }

    pub fn failed(&self, login: &str) {
        let mut map = self.failures.lock();
        let e = map
            .entry(login.to_ascii_lowercase())
            .or_insert((0, Instant::now()));
        if e.1.elapsed() >= WINDOW {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }

    pub fn succeeded(&self, login: &str) {
        self.failures.lock().remove(&login.to_ascii_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_verify_and_throttle() {
        let phc = hash("correct horse battery".into()).await.unwrap();
        assert!(phc.starts_with("$argon2id$"));
        assert!(verify("correct horse battery".into(), phc.clone()).await);
        assert!(!verify("wrong".into(), phc).await);
        assert!(
            !verify("anything".into(), String::new()).await,
            "no hash, no entry"
        );
        assert!(check_new("short").is_err() && check_new("long enough").is_ok());

        let t = Throttle::default();
        for _ in 0..MAX_FAILURES - 1 {
            t.failed("Alice");
        }
        assert!(t.locked("alice").is_none());
        t.failed("alice");
        assert!(t.locked("ALICE").is_some(), "locked after the limit");
        t.succeeded("alice");
        assert!(t.locked("alice").is_none());
    }
}

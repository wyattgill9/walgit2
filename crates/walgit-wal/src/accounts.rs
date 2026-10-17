//! D51: the user directory, a fold of the account log (`docs/ACCOUNTS.md`).
//!
//! The account log is a WAL of its own (`system/accounts/`, `RepoId::system("accounts")`), with
//! the standard manifest/log/checkpoint layout. `AccountsState::apply` is the only
//! interpretation of its events; every index (by login, by identity) is derived from them, so
//! a checkpoint's `AccountHistory` plus the log tail rebuilds the directory. Uniqueness of
//! logins and identities is a publisher precondition, re-verified on every CAS attempt.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use prost::Message;
use serde::Serialize;
use walgit_proto::v1::{
    AccountEvent, AccountGenesis, AccountHistory, AccountRecord, EntryKind, LogEntry,
    NamespaceReserved, account_event::Body,
};
use walgit_store::ObjectStoreExt;

use crate::error::WalError;
use crate::handle::RepoHandle;
use crate::registry::Registry;

/// The account log's id: never routable, never listed.
pub fn log_id() -> walgit_git::RepoId {
    walgit_git::RepoId::system("accounts")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub user_id: String,
    pub login: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub avatar_url: String,
    /// D52: the Argon2id PHC string of a password account (empty = no password). Never serialized.
    #[serde(skip)]
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Org {
    pub org_id: String,
    pub login: String,
    /// `user_id`s; each administers every repository in the org's namespace.
    pub owners: BTreeSet<String>,
}

/// Who a namespace (`<owner>`) belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace<'a> {
    User(&'a User),
    Org(&'a Org),
    /// Held repositories before it had an account: only a site admin's org can take it.
    Reserved,
    Free,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Holder {
    User(String),
    Org(String),
    Reserved,
}

/// The folded account log at `seq`.
#[derive(Debug, Clone, Default)]
pub struct AccountsState {
    pub seq: u64,
    /// The genesis entry landed (every other event needs it, so reservations come first).
    pub genesis: bool,
    pub users: BTreeMap<String, User>,
    pub orgs: BTreeMap<String, Org>,
    /// Lowercased login -> holder. Logins are unique across users, orgs and reservations.
    by_login: BTreeMap<String, Holder>,
    /// `(issuer, subject)` -> `user_id`.
    by_identity: BTreeMap<(String, String), String>,
    // ponytail: every record is kept and rewritten whole at each checkpoint, like forge.pb;
    // sign-ups are rare, so this grows slowly. Shard if accounts.pb ever gets large.
    records: Vec<AccountRecord>,
}

/// Names a user can't take: the path segments walgit itself serves at the root.
const RESERVED_LOGINS: &[&str] = &[
    "_auth",
    "_ui",
    "_events",
    "api",
    "api-browser",
    "services",
    "healthz",
    "readyz",
    "metrics",
    "repos.js",
    "repos.mjs",
    "admin",
    "system",
    "settings",
    "login",
    "logout",
    "new",
    "anonymous",
    "anon",
];

/// A login a user may choose: 1–39 of `[a-z0-9-]`, no leading, trailing or double hyphen,
/// not one of walgit's own root paths (GitHub's rules, lowercased).
pub fn valid_user_login(login: &str) -> Result<(), String> {
    let ok = !login.is_empty()
        && login.len() <= 39
        && login
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--");
    if !ok {
        return Err(format!(
            "{login:?} is not a valid username: 1 to 39 lowercase letters, digits or single hyphens, not at either end"
        ));
    }
    if RESERVED_LOGINS.contains(&login) {
        return Err(format!("{login:?} is reserved"));
    }
    Ok(())
}

impl AccountsState {
    pub fn from_history(h: AccountHistory) -> Self {
        let mut s = AccountsState::default();
        for r in h.records {
            s.apply_record(r);
        }
        s.seq = h.seq;
        s
    }

    pub fn history_upto(&self, seq: u64) -> AccountHistory {
        AccountHistory {
            seq,
            records: self
                .records
                .iter()
                .filter(|r| r.seq <= seq)
                .cloned()
                .collect(),
        }
    }

    /// Who holds `login` (case-insensitive).
    pub fn namespace(&self, login: &str) -> Namespace<'_> {
        match self.by_login.get(&login.to_ascii_lowercase()) {
            Some(Holder::User(id)) => self.users.get(id).map_or(Namespace::Free, Namespace::User),
            Some(Holder::Org(id)) => self.orgs.get(id).map_or(Namespace::Free, Namespace::Org),
            Some(Holder::Reserved) => Namespace::Reserved,
            None => Namespace::Free,
        }
    }

    pub fn user_by_login(&self, login: &str) -> Option<&User> {
        match self.namespace(login) {
            Namespace::User(u) => Some(u),
            _ => None,
        }
    }

    pub fn user_by_identity(&self, issuer: &str, subject: &str) -> Option<&User> {
        let id = self
            .by_identity
            .get(&(issuer.to_string(), subject.to_string()))?;
        self.users.get(id)
    }

    /// Users whose login or name starts with `prefix` (case-insensitive), login order.
    pub fn search(&self, prefix: &str, limit: usize) -> Vec<&User> {
        let p = prefix.to_ascii_lowercase();
        self.users
            .values()
            .filter(|u| {
                u.login.starts_with(&p)
                    || u.name.to_lowercase().starts_with(&p)
                    || u.name
                        .to_lowercase()
                        .split_whitespace()
                        .any(|w| w.starts_with(&p))
            })
            .take(limit)
            .collect()
    }

    /// Advances over committed entries in (`after`, last] (the forge's rule: committed seqs
    /// can skip, so completeness is the caller's range). A state behind `after` stays put.
    pub(crate) fn fold_entries<'a>(
        &mut self,
        after: u64,
        entries: impl IntoIterator<Item = &'a LogEntry>,
    ) {
        if self.seq < after {
            return;
        }
        for e in entries {
            if e.seq <= self.seq {
                continue;
            }
            if !e.account.is_empty() {
                self.apply_record(AccountRecord {
                    seq: e.seq,
                    at: e.created_at,
                    events: e.account.clone(),
                });
            }
            self.seq = e.seq;
        }
    }

    fn apply_record(&mut self, r: AccountRecord) {
        for ev in &r.events {
            self.apply(ev);
        }
        self.records.push(r);
    }

    fn apply(&mut self, ev: &AccountEvent) {
        let Some(body) = &ev.body else { return };
        match body {
            Body::Genesis(g) => {
                self.genesis = true;
                for login in &g.reserved {
                    self.by_login
                        .entry(login.to_ascii_lowercase())
                        .or_insert(Holder::Reserved);
                }
            }
            Body::NamespaceReserved(n) => {
                self.by_login
                    .entry(n.login.to_ascii_lowercase())
                    .or_insert(Holder::Reserved);
            }
            Body::UserCreated(u) => {
                self.by_login.insert(
                    u.login.to_ascii_lowercase(),
                    Holder::User(u.user_id.clone()),
                );
                self.users.insert(
                    u.user_id.clone(),
                    User {
                        user_id: u.user_id.clone(),
                        login: u.login.clone(),
                        email: u.email.clone(),
                        name: u.name.clone(),
                        avatar_url: u.avatar_url.clone(),
                        password: String::new(),
                    },
                );
            }
            Body::IdentityLinked(l) => {
                self.by_identity
                    .insert((l.issuer.clone(), l.subject.clone()), l.user_id.clone());
            }
            Body::ProfileChanged(p) => {
                if let Some(u) = self.users.get_mut(&p.user_id) {
                    for (field, v) in [
                        (&mut u.email, &p.email),
                        (&mut u.name, &p.name),
                        (&mut u.avatar_url, &p.avatar_url),
                    ] {
                        if !v.is_empty() {
                            field.clone_from(v);
                        }
                    }
                }
            }
            Body::OrgCreated(o) => {
                self.by_login
                    .insert(o.login.to_ascii_lowercase(), Holder::Org(o.org_id.clone()));
                self.orgs.insert(
                    o.org_id.clone(),
                    Org {
                        org_id: o.org_id.clone(),
                        login: o.login.clone(),
                        owners: o.owners.iter().cloned().collect(),
                    },
                );
            }
            Body::OrgOwnersChanged(o) => {
                if let Some(org) = self.orgs.get_mut(&o.org_id) {
                    org.owners = o.owners.iter().cloned().collect();
                }
            }
            Body::PasswordSet(p) => {
                if let Some(u) = self.users.get_mut(&p.user_id) {
                    u.password.clone_from(&p.phc);
                }
            }
        }
    }

    /// Preconditions of one entry against this state, in event order (a later event sees what
    /// earlier ones of the same entry create). Checked by the publisher on every CAS attempt.
    pub(crate) fn verify(&self, events: &[AccountEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Err("no account events".into());
        }
        let mut logins: HashSet<String> = HashSet::new();
        let mut users: HashSet<&str> = HashSet::new();
        let mut identities: HashSet<(&str, &str)> = HashSet::new();
        let user_exists =
            |id: &str, users: &HashSet<&str>| self.users.contains_key(id) || users.contains(id);
        for ev in events {
            let Some(body) = &ev.body else {
                return Err("empty account event".into());
            };
            if !self.genesis && !matches!(body, Body::Genesis(_)) {
                return Err("the account log has no genesis entry yet".into());
            }
            match body {
                Body::Genesis(_) => {
                    if self.genesis || events.len() != 1 {
                        return Err("genesis is the log's first entry, alone".into());
                    }
                }
                Body::NamespaceReserved(n) => match self.namespace(&n.login) {
                    Namespace::Free | Namespace::Reserved => {}
                    Namespace::User(_) | Namespace::Org(_) => {
                        return Err(format!("{:?} belongs to an account", n.login));
                    }
                },
                Body::UserCreated(u) => {
                    if u.user_id.is_empty()
                        || self.users.contains_key(&u.user_id)
                        || !users.insert(&u.user_id)
                    {
                        return Err("a new user needs a new id".into());
                    }
                    valid_user_login(&u.login)?;
                    let key = u.login.to_ascii_lowercase();
                    if self.namespace(&key) != Namespace::Free || !logins.insert(key) {
                        return Err(format!("the username {:?} is taken", u.login));
                    }
                }
                Body::IdentityLinked(l) => {
                    if l.issuer.is_empty() || l.subject.is_empty() {
                        return Err("an identity needs an issuer and a subject".into());
                    }
                    if !user_exists(&l.user_id, &users) {
                        return Err(format!("no user {:?}", l.user_id));
                    }
                    let key = (l.issuer.as_str(), l.subject.as_str());
                    if self
                        .by_identity
                        .contains_key(&(l.issuer.clone(), l.subject.clone()))
                        || !identities.insert(key)
                    {
                        return Err("this sign-in is already linked to an account".into());
                    }
                }
                Body::ProfileChanged(p) => {
                    if !user_exists(&p.user_id, &users) {
                        return Err(format!("no user {:?}", p.user_id));
                    }
                }
                Body::OrgCreated(o) => {
                    if o.org_id.is_empty()
                        || self.orgs.contains_key(&o.org_id)
                        || self.users.contains_key(&o.org_id)
                    {
                        return Err("a new org needs a new id".into());
                    }
                    walgit_git::RepoId::new(&o.login, "x").map_err(|e| e.to_string())?;
                    let key = o.login.to_ascii_lowercase();
                    let open =
                        matches!(self.namespace(&key), Namespace::Free | Namespace::Reserved);
                    if !open || !logins.insert(key) {
                        return Err(format!("{:?} is taken", o.login));
                    }
                    verify_owners(&o.owners, |id| user_exists(id, &users))?;
                }
                Body::OrgOwnersChanged(o) => {
                    if !self.orgs.contains_key(&o.org_id) {
                        return Err(format!("no org {:?}", o.org_id));
                    }
                    verify_owners(&o.owners, |id| user_exists(id, &users))?;
                }
                Body::PasswordSet(p) => {
                    if !user_exists(&p.user_id, &users) {
                        return Err(format!("no user {:?}", p.user_id));
                    }
                    if !p.phc.is_empty() && !p.phc.starts_with("$argon2id$") {
                        return Err("a password is stored as an Argon2id PHC string".into());
                    }
                }
            }
        }
        Ok(())
    }
}

fn verify_owners(owners: &[String], exists: impl Fn(&str) -> bool) -> Result<(), String> {
    if owners.is_empty() {
        return Err("an org needs at least one owner".into());
    }
    match owners.iter().find(|id| !exists(id)) {
        Some(id) => Err(format!("no user {id:?}")),
        None => Ok(()),
    }
}

impl RepoHandle {
    /// Brings the account fold to the handle's manifest head. Call after a refs-level sync.
    pub async fn accounts_ensure(&self) -> Result<(), WalError> {
        let manifest = self.manifest();
        let head = manifest.head_seq;
        let have = self.accounts.lock().as_ref().map(|s| s.seq);
        if have.is_some_and(|s| s >= head) {
            return Ok(());
        }
        let mut fresh = None;
        let from = match have {
            Some(seq) if seq + 1 >= manifest.min_seq.max(1) => seq,
            _ => {
                let cp = manifest.checkpoint.as_ref();
                let state = match cp
                    .map(|c| c.accounts_key.as_str())
                    .filter(|k| !k.is_empty())
                {
                    Some(key) => {
                        let (_, bytes) = self.store.get_bytes(key).await?.ok_or_else(|| {
                            WalError::Corrupt(format!("account history missing: {key}"))
                        })?;
                        let h = AccountHistory::decode(bytes.as_ref()).map_err(|e| {
                            WalError::Corrupt(format!("account history decode: {e}"))
                        })?;
                        AccountsState::from_history(h)
                    }
                    None => AccountsState {
                        seq: cp.map_or(0, |c| c.seq),
                        ..AccountsState::default()
                    },
                };
                let seq = state.seq;
                fresh = Some(state);
                seq
            }
        };
        let entries =
            crate::log_reader::read_log_manifest(self, &manifest, from + 1, Some(head)).await?;
        let mut guard = self.accounts.lock();
        if let Some(state) = fresh
            && guard.as_ref().is_none_or(|g| g.seq < state.seq)
        {
            *guard = Some(state);
        }
        if let Some(state) = guard.as_mut() {
            state.fold_entries(from, &entries);
        }
        Ok(())
    }

    /// Reads the account fold (empty until `accounts_ensure`).
    pub fn accounts_read<R>(&self, f: impl FnOnce(&AccountsState) -> R) -> R {
        match self.accounts.lock().as_ref() {
            Some(s) => f(s),
            None => f(&AccountsState::default()),
        }
    }

    /// Folds entries the sync or a publish just applied (no-op until loaded).
    pub(crate) fn accounts_fold(&self, after: u64, entries: &[&LogEntry]) {
        if let Some(state) = self.accounts.lock().as_mut() {
            state.fold_entries(after, entries.iter().copied());
        }
    }

    /// Publishes account events as one ACCOUNT entry, verified against the fold at each CAS
    /// attempt. `Invalid` carries the refused precondition. Returns the entry's seq.
    pub async fn publish_accounts(
        &self,
        events: Vec<AccountEvent>,
        author: &str,
    ) -> Result<u64, WalError> {
        // Loaded here, the fold follows every sync `publish_one` makes (it catches up before
        // each attempt), so `verify` always sees the attempt's basis.
        self.accounts_ensure().await?;
        let (seq, _) = crate::publish::publish_one(self, "accounts", |manifest| {
            self.accounts_read(|s| {
                if s.seq < manifest.head_seq {
                    return Err(WalError::Corrupt(format!(
                        "account fold at {} behind the basis {}",
                        s.seq, manifest.head_seq
                    )));
                }
                s.verify(&events).map_err(WalError::Invalid)
            })?;
            let entry = LogEntry {
                kind: EntryKind::Account as i32,
                meta: std::collections::HashMap::from([("author".to_string(), author.to_string())]),
                account: events.clone(),
                ..Default::default()
            };
            Ok((entry, manifest.clone()))
        })
        .await?;
        Ok(seq)
    }
}

/// The account log, opened (and created with its genesis entry) on first use.
pub struct Accounts {
    registry: Arc<Registry>,
    handle: tokio::sync::OnceCell<Arc<RepoHandle>>,
}

impl Accounts {
    pub fn new(registry: Arc<Registry>) -> Arc<Self> {
        Arc::new(Accounts {
            registry,
            handle: tokio::sync::OnceCell::new(),
        })
    }

    /// The log's handle, if this process opened it already (no I/O).
    pub fn opened(&self) -> Option<&Arc<RepoHandle>> {
        self.handle.get()
    }

    /// The account log, synced: one conditional GET (principle IV). A bucket that never had
    /// one gets it here, with its genesis entry reserving every namespace that holds
    /// repositories (one LIST, once in the bucket's lifetime).
    pub async fn synced(&self) -> Result<Arc<RepoHandle>, WalError> {
        let handle = self
            .handle
            .get_or_try_init(|| async {
                let id = log_id();
                match self.registry.open(&id).await {
                    Ok(h) => Ok(h),
                    Err(WalError::NotFound) => {
                        self.registry
                            .open_or_create(&id, walgit_git::ObjectFormat::Sha1)
                            .await
                    }
                    Err(e) => Err(e),
                }
            })
            .await?
            .clone();
        drop(handle.sync_refs().await?);
        handle.accounts_ensure().await?;
        if !handle.accounts_read(|s| s.genesis) {
            let mut reserved: Vec<String> = self
                .registry
                .list()
                .await?
                .iter()
                .map(|id| id.owner().to_string())
                .collect();
            reserved.sort();
            reserved.dedup();
            let genesis = AccountEvent {
                body: Some(Body::Genesis(AccountGenesis { reserved })),
            };
            match handle.publish_accounts(vec![genesis], "walgit").await {
                // Another instance wrote it first: the fold has it after the next sync.
                Ok(_) | Err(WalError::Invalid(_)) => {
                    drop(handle.sync_refs().await?);
                    handle.accounts_ensure().await?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(handle)
    }

    /// Reads the fold as of the last sync, without revalidating (for facts that never change
    /// once written: an identity link, a user's id). Opens and syncs on first use.
    pub async fn cached<R>(&self, f: impl FnOnce(&AccountsState) -> R) -> Result<R, WalError> {
        let handle = match self.opened() {
            Some(h) => h.clone(),
            None => self.synced().await?,
        };
        Ok(handle.accounts_read(f))
    }

    /// Reserves `owner` for a repository about to be created in a namespace with no account
    /// (security 1: sign-up can never take a namespace that holds repositories). Idempotent;
    /// refused when the namespace belongs to an account.
    pub async fn reserve(&self, owner: &str, author: &str) -> Result<(), WalError> {
        let handle = self.synced().await?;
        if handle.accounts_read(|s| s.namespace(owner) == Namespace::Reserved) {
            return Ok(());
        }
        let ev = AccountEvent {
            body: Some(Body::NamespaceReserved(NamespaceReserved {
                login: owner.to_string(),
            })),
        };
        handle.publish_accounts(vec![ev], author).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use walgit_proto::v1::{IdentityLinked, OrgCreated, UserCreated};

    fn ev(body: Body) -> AccountEvent {
        AccountEvent { body: Some(body) }
    }

    fn entry(seq: u64, account: Vec<AccountEvent>) -> LogEntry {
        LogEntry {
            seq,
            account,
            ..Default::default()
        }
    }

    fn user(id: &str, login: &str) -> AccountEvent {
        ev(Body::UserCreated(UserCreated {
            user_id: id.into(),
            login: login.into(),
            ..Default::default()
        }))
    }

    fn link(id: &str, sub: &str) -> AccountEvent {
        ev(Body::IdentityLinked(IdentityLinked {
            user_id: id.into(),
            issuer: "https://id".into(),
            subject: sub.into(),
        }))
    }

    #[test]
    fn fold_verify_and_history_round_trip() {
        let mut s = AccountsState::default();
        // Nothing before genesis.
        assert!(s.verify(&[user("u1", "alice")]).is_err());
        let genesis = ev(Body::Genesis(AccountGenesis {
            reserved: vec!["examples".into(), "Test".into()],
        }));
        assert!(s.verify(std::slice::from_ref(&genesis)).is_ok());
        s.fold_entries(0, &[entry(1, vec![genesis.clone()])]);
        assert!(s.verify(&[genesis]).is_err(), "genesis is once");

        // Sign-up: a user and their identity in one entry.
        let alice = [user("u1", "alice"), link("u1", "sub-a")];
        assert!(s.verify(&alice).is_ok());
        s.fold_entries(1, &[entry(2, alice.to_vec())]);
        assert_eq!(
            s.user_by_login("Alice").map(|u| u.user_id.as_str()),
            Some("u1")
        );
        assert_eq!(
            s.user_by_identity("https://id", "sub-a")
                .map(|u| u.login.as_str()),
            Some("alice")
        );

        // Security 1: reserved namespaces and taken logins can't be signed up for.
        for (login, why) in [
            ("examples", "reserved by genesis"),
            ("test", "reserved, case-insensitively"),
            ("alice", "taken"),
            ("api", "a walgit path"),
            ("Bob", "uppercase"),
            ("-bob", "leading hyphen"),
        ] {
            assert!(s.verify(&[user("u9", login)]).is_err(), "{login}: {why}");
        }
        // The same login twice in one entry, or one identity twice, is refused.
        assert!(s.verify(&[user("u2", "bob"), user("u3", "bob")]).is_err());
        assert!(s.verify(&[user("u2", "bob"), link("u2", "sub-a")]).is_err());
        // A reservation can't take an account's namespace; reserving twice is fine.
        let reserve = |l: &str| {
            ev(Body::NamespaceReserved(NamespaceReserved {
                login: l.into(),
            }))
        };
        assert!(s.verify(&[reserve("alice")]).is_err());
        assert!(s.verify(&[reserve("examples")]).is_ok());
        s.fold_entries(2, &[entry(3, vec![reserve("newns")])]);
        assert!(s.verify(&[user("u2", "newns")]).is_err());

        // Only an org claims a reserved namespace, owned by existing users.
        let org = |owners: Vec<String>| {
            ev(Body::OrgCreated(OrgCreated {
                org_id: "o1".into(),
                login: "examples".into(),
                owners,
            }))
        };
        assert!(s.verify(&[org(vec![])]).is_err());
        assert!(s.verify(&[org(vec!["nobody".into()])]).is_err());
        assert!(s.verify(&[org(vec!["u1".into()])]).is_ok());
        s.fold_entries(3, &[entry(4, vec![org(vec!["u1".into()])])]);
        assert!(matches!(s.namespace("examples"), Namespace::Org(o) if o.owners.contains("u1")));
        assert!(s.verify(&[org(vec!["u1".into()])]).is_err(), "taken now");

        // The checkpoint history folds back to the same directory.
        let again = AccountsState::from_history(s.history_upto(s.seq));
        assert_eq!(again.seq, s.seq);
        assert_eq!(again.users.len(), 1);
        assert_eq!(again.namespace("newns"), Namespace::Reserved);
        assert!(matches!(again.namespace("examples"), Namespace::Org(_)));
        assert_eq!(
            again
                .user_by_identity("https://id", "sub-a")
                .map(|u| u.login.as_str()),
            Some("alice")
        );
        assert_eq!(s.search("al", 10).len(), 1);
    }
}

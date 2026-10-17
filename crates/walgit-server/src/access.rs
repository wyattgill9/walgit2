//! D51: one permission model for every path (`docs/ACCOUNTS.md`).
//!
//! Every repository-scoped handler goes through here and nowhere else:
//! 1. [`authenticate`]: the request's credential, resolved to its account and host flags.
//! 2. [`open`] the repository: a missing one reads exactly like one you may not see.
//! 3. [`authorize`] the role the operation needs. It revalidates the manifest itself (the
//!    request's next sync of the repository reuses that round trip), so a decision is never
//!    older than the request (principle IV), and refuses before any pack work starts.
//!
//! Writes are decided again by the publisher at their CAS basis (`walgit_wal::access`), so
//! access revoked while a push or a forge write is in flight refuses it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use walgit_git::RepoId;
use walgit_wal::RepoHandle;
use walgit_wal::accounts::{AccountsState, Namespace, User};

pub use walgit_wal::access::Role;

use crate::AppState;
use crate::auth::{AuthError, Credential, EmailFlags};
use crate::error::ApiError;

/// The `user_id` of `mode = none`'s one implicit local user. A sentinel: never in the log.
pub const LOCAL_USER_ID: &str = "local";

/// Who a request is, as authorization sees it.
#[derive(Debug, Clone)]
pub struct Subject {
    /// The login (what is shown, logged and matched by `policy.json`); `anonymous` for
    /// nobody, `anon` for `mode = none`'s local user.
    pub name: String,
    /// The account. `None` = anonymous.
    pub user_id: Option<String>,
    pub email: String,
    /// May create repositories under its own login (host `write`).
    pub can_create: bool,
    /// Admin on every repository (host `admin`).
    pub site_admin: bool,
}

impl Subject {
    pub fn anonymous() -> Self {
        Subject {
            name: "anonymous".into(),
            user_id: None,
            email: String::new(),
            can_create: false,
            site_admin: false,
        }
    }

    pub fn is_anonymous(&self) -> bool {
        self.user_id.is_none()
    }

    /// What `policy.json` principals match: the login and the account's email.
    pub fn names(&self) -> Vec<&str> {
        let mut v = vec![self.name.as_str()];
        if !self.email.is_empty() {
            v.push(self.email.as_str());
        }
        v
    }

    fn of(user: User, flags: EmailFlags) -> Self {
        Subject {
            name: user.login,
            user_id: Some(user.user_id),
            email: user.email,
            can_create: flags.can_create,
            site_admin: flags.site_admin,
        }
    }
}

/// The request's subject: its credential (a real 401 for an invalid one) resolved to its
/// account. A valid credential without an account is `AuthError::NoAccount`.
pub async fn authenticate(st: &AppState, headers: &HeaderMap) -> Result<Subject, AuthError> {
    let cred = st.auth.credential(headers).await?;
    let subject = match cred {
        Credential::Anonymous => Subject::anonymous(),
        Credential::Local => Subject {
            name: "anon".into(),
            user_id: Some(LOCAL_USER_ID.into()),
            email: String::new(),
            can_create: true,
            site_admin: true,
        },
        Credential::Token { user, write, admin } => {
            let account = lookup(st, |s| s.user_by_login(&user).cloned())
                .await?
                .ok_or_else(|| {
                    AuthError::NoAccount(format!(
                        "this token acts as {user:?}, which has no account: create it with `walgit user create {user}`"
                    ))
                })?;
            Subject::of(
                account,
                EmailFlags {
                    can_create: write,
                    site_admin: admin,
                },
            )
        }
        Credential::Account { user_id } => {
            // A session or token for an account this bucket doesn't have: a dead credential.
            let account = lookup(st, |s| s.users.get(&user_id).cloned())
                .await?
                .ok_or(AuthError::Invalid)?;
            let flags = account_flags(st, &account)?;
            Subject::of(account, flags)
        }
        Credential::Identity(id) => {
            let account = lookup(st, |s| s.user_by_identity(&id.issuer, &id.subject).cloned())
                .await?
                .ok_or_else(|| {
                    AuthError::NoAccount(format!(
                        "{} has no walgit account yet: sign in once in a browser at {}/_auth/login to choose a username",
                        id.email,
                        crate::smart::request_base_url(st, headers)
                    ))
                })?;
            let mut flags = st.auth.email_flags(&id.email)?;
            let tokens = st.auth.token_flags(&account.login);
            flags.can_create |= tokens.can_create;
            flags.site_admin |= tokens.site_admin;
            Subject::of(account, flags)
        }
    };
    tracing::Span::current().record("principal", subject.name.as_str());
    Ok(subject)
}

/// An account's host flags: every static token bound to its login, plus (oidc) its email's
/// policy, re-applied on every request — an email that left the allowlist is `Forbidden`.
fn account_flags(st: &AppState, account: &User) -> Result<EmailFlags, AuthError> {
    let mut flags = st.auth.token_flags(&account.login);
    // D52: where people make their own accounts, every account may create under its own login.
    flags.can_create |= st.auth.passwords() != walgit_config::PasswordAccounts::Off;
    if st.auth.mode() == walgit_config::AuthMode::Oidc && !account.email.is_empty() {
        let e = st.auth.email_flags(&account.email)?;
        flags.can_create |= e.can_create;
        flags.site_admin |= e.site_admin;
    }
    Ok(flags)
}

/// A directory fact that never changes once written (an id, an identity link, a login until
/// renames exist): from the fold as last synced; a miss revalidates once before answering.
async fn lookup<R>(
    st: &AppState,
    f: impl Fn(&AccountsState) -> Option<R>,
) -> Result<Option<R>, AuthError> {
    let hit = st.accounts.cached(&f).await.map_err(unavailable)?;
    if hit.is_some() {
        return Ok(hit);
    }
    let handle = st.accounts.synced().await.map_err(unavailable)?;
    Ok(handle.accounts_read(&f))
}

fn unavailable(e: walgit_wal::WalError) -> AuthError {
    tracing::warn!(error = %e, "account log unavailable");
    AuthError::Unavailable
}

/// An authentication failure as an HTTP answer: invalid or missing → 401 (git erases a dead
/// credential and asks again); not allowed / no account → 403 with the reason.
pub fn auth_err(e: AuthError) -> ApiError {
    match e {
        AuthError::Invalid | AuthError::Unauthorized => ApiError::Unauthorized,
        AuthError::Forbidden => ApiError::Forbidden,
        AuthError::NoAccount(why) => ApiError::Denied(why),
        AuthError::Unavailable => ApiError::ServiceUnavailable("auth provider unavailable".into()),
    }
}

/// The answer to a request that may not see a repository: anonymous → 401 (git asks for a
/// credential, the UI signs in), signed in → 404. Missing and private read the same.
pub fn no_access(subject: &Subject) -> ApiError {
    if subject.is_anonymous() {
        ApiError::Unauthorized
    } else {
        ApiError::NotFound("repository".into())
    }
}

/// Open `id` for `subject`; a repository that doesn't exist is refused like one it can't see.
pub async fn open(
    st: &AppState,
    id: &RepoId,
    subject: &Subject,
) -> Result<Arc<RepoHandle>, ApiError> {
    match st.registry.open(id).await {
        Ok(h) => Ok(h),
        Err(walgit_wal::WalError::NotFound) => Err(no_access(subject)),
        Err(e) => Err(crate::smart::wal_err(e)),
    }
}

/// `Cache-Control` of immutable bytes nobody without a credential may read.
pub const PRIVATE_IMMUTABLE: &str = "private, max-age=31536000, immutable";

/// Whether anyone at all may read this repository: public, on a host that lets anonymous
/// callers read. Only then are its immutable bytes cacheable by shared caches, or delivered
/// by URL to clients that send no credential (D46). As of the request's sync.
pub fn world_readable(st: &AppState, handle: &RepoHandle) -> bool {
    let anyone =
        st.auth.mode() == walgit_config::AuthMode::None || st.auth.anonymous_read_allowed();
    anyone
        && handle
            .access()
            .is_none_or(|a| a.visibility() == walgit_proto::v1::Visibility::Public)
}

/// The `Cache-Control` override for one repository's immutable bytes (`None` = public).
pub fn immutable_cache_control(st: &AppState, handle: &RepoHandle) -> Option<&'static str> {
    (!world_readable(st, handle)).then_some(PRIVATE_IMMUTABLE)
}

/// [`open`] then [`authorize`]: the repository and what `subject` may do there.
pub async fn repo(
    st: &AppState,
    id: &RepoId,
    subject: &Subject,
    need: Role,
) -> Result<(Arc<RepoHandle>, Grant), ApiError> {
    let handle = open(st, id, subject).await?;
    let grant = authorize(st, &handle, subject, need).await?;
    Ok((handle, grant))
}

/// [`authenticate`] then [`repo`], for handlers with nothing to do in between.
pub async fn request_repo(
    st: &AppState,
    headers: &HeaderMap,
    id: &RepoId,
    need: Role,
) -> Result<(Arc<RepoHandle>, Grant), ApiError> {
    let subject = authenticate(st, headers).await.map_err(auth_err)?;
    repo(st, id, &subject, need).await
}

/// What `subject` may do on one repository.
#[derive(Debug, Clone)]
pub struct Grant {
    pub subject: Subject,
    pub role: Role,
    /// Site admin, or owner of the repository's namespace (resolved now; the publisher
    /// re-derives collaborator roles at its CAS basis).
    pub owner_admin: bool,
}

impl Grant {
    /// The actor for forge events and pushes: the publisher judges its role again at each
    /// CAS attempt.
    pub fn actor(&self) -> walgit_wal::forge::Actor {
        walgit_wal::forge::Actor {
            login: self.subject.name.clone(),
            user_id: self.subject.user_id.clone().unwrap_or_default(),
            owner_admin: self.owner_admin,
        }
    }
}

/// Revalidate the repository (one conditional GET, reused by the request's next sync) and
/// require `need`. The one gate of every repository operation.
pub async fn authorize(
    st: &AppState,
    handle: &RepoHandle,
    subject: &Subject,
    need: Role,
) -> Result<Grant, ApiError> {
    handle
        .sync_refs_for_request()
        .await
        .map_err(crate::smart::wal_err)?;
    let access = handle.access();
    let mut owner_admin = subject.site_admin;
    let asker = |owner_admin| walgit_wal::access::Asker {
        user_id: subject.user_id.as_deref(),
        owner_admin,
    };
    let anonymous_read = st.auth.anonymous_read_allowed();
    let mut role = walgit_wal::access::role(access.as_ref(), asker(owner_admin), anonymous_read);
    if role < need && !owner_admin && !subject.is_anonymous() {
        owner_admin = owns(st, handle.id().owner(), subject).await?;
        role = walgit_wal::access::role(access.as_ref(), asker(owner_admin), anonymous_read);
    }
    if role >= need {
        return Ok(Grant {
            subject: subject.clone(),
            role,
            owner_admin,
        });
    }
    // Nobody signed in: ask for a credential (git prompts, the UI signs in) whatever the
    // operation.
    if role == Role::None || subject.is_anonymous() {
        return Err(no_access(subject));
    }
    Err(ApiError::Denied(format!(
        "{} needs {} access to {}; {} has {}",
        operation(need),
        need.as_str(),
        handle.id(),
        subject.name,
        role.as_str()
    )))
}

fn operation(need: Role) -> &'static str {
    match need {
        Role::None | Role::Read => "reading",
        Role::Triage => "this",
        Role::Write => "writing",
        Role::Maintain => "changing settings",
        Role::Admin => "administering",
    }
}

/// Whether `subject` owns the namespace `owner`: it is their login, or an org they own.
/// A user's login is read from the fold as last synced (it never changes); an org's owners
/// are revalidated (an owner removed a moment ago must not pass).
async fn owns(st: &AppState, owner: &str, subject: &Subject) -> Result<bool, ApiError> {
    let Some(me) = subject.user_id.as_deref() else {
        return Ok(false);
    };
    let mine = |s: &AccountsState| match s.namespace(owner) {
        Namespace::User(u) => Some(u.user_id == me),
        Namespace::Org(o) => o.owners.contains(me).then_some(true),
        Namespace::Reserved | Namespace::Free => None,
    };
    if st.accounts.cached(mine).await.map_err(accounts_err)? == Some(true) {
        return Ok(true);
    }
    let handle = st.accounts.synced().await.map_err(accounts_err)?;
    Ok(handle.accounts_read(mine) == Some(true))
}

fn accounts_err(e: walgit_wal::WalError) -> ApiError {
    ApiError::ServiceUnavailable(format!("account log: {e}"))
}

/// May `subject` create the repository `id`? Under their own login or an org they own, with
/// host `write`; a site admin anywhere. A namespace with no account is reserved first, so a
/// later sign-up can never take it (security 1). Returns what the creation publishes.
pub async fn authorize_create(
    st: &AppState,
    id: &RepoId,
    subject: &Subject,
) -> Result<(), ApiError> {
    if subject.is_anonymous() {
        return Err(ApiError::Unauthorized);
    }
    let handle = st.accounts.synced().await.map_err(accounts_err)?;
    let me = subject.user_id.as_deref().unwrap_or_default();
    let mine = handle.accounts_read(|s| match s.namespace(id.owner()) {
        Namespace::User(u) => Some(u.user_id == me),
        Namespace::Org(o) => Some(o.owners.contains(me)),
        Namespace::Reserved | Namespace::Free => None,
    });
    match mine {
        _ if subject.site_admin => {
            if mine.is_none() {
                st.accounts
                    .reserve(id.owner(), &subject.name)
                    .await
                    .map_err(|e| match e {
                        walgit_wal::WalError::Invalid(m) => ApiError::Conflict(m),
                        e => accounts_err(e),
                    })?;
            }
            Ok(())
        }
        Some(true) if subject.can_create => Ok(()),
        Some(true) => Err(ApiError::Denied(format!(
            "{} may not create repositories on this host",
            subject.name
        ))),
        _ => Err(ApiError::Denied(format!(
            "{} can only create repositories under {}/ or an org they own, not {}/",
            subject.name,
            subject.name,
            id.owner()
        ))),
    }
}

/// The repositories of `repos` that `subject` may read. A site admin sees all; otherwise
/// each is revalidated and decided like any read.
// ponytail: one manifest round trip per repository; a listing index replaces this when
// owner pages get large (docs/ROUNDTRIPS.md).
pub async fn readable(st: &AppState, subject: &Subject, repos: Vec<RepoId>) -> Vec<RepoId> {
    use futures::StreamExt;
    if subject.site_admin {
        return repos;
    }
    futures::stream::iter(repos)
        .map(|id| async move {
            let handle = st.registry.open(&id).await.ok()?;
            authorize(st, &handle, subject, Role::Read).await.ok()?;
            Some(id)
        })
        .buffered(16)
        .filter_map(|x| async move { x })
        .collect()
        .await
}

/// Non-repository routes that show content (the SPA, owner listings, the directory): any
/// account, or anyone when `anonymous_read` is on.
pub async fn signed_in(st: &AppState, headers: &HeaderMap) -> Result<Subject, AuthError> {
    let s = authenticate(st, headers).await?;
    if s.is_anonymous() && !st.auth.anonymous_read_allowed() {
        return Err(AuthError::Unauthorized);
    }
    Ok(s)
}

/// D51 security 3: in `mode = none` (no credentials, everyone a site admin) serve only
/// requests made directly from this machine — a loopback peer, a loopback `Host` (no DNS
/// rebinding), and no header a proxy or another walgit host would add. Layered outermost.
pub async fn local_only(
    State(st): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if st.auth.mode() != walgit_config::AuthMode::None || is_direct_local(&req) {
        return next.run(req).await;
    }
    (
        StatusCode::FORBIDDEN,
        "walgit: server.auth.mode = none serves only direct requests from this machine; use token or oidc to serve anyone else\n",
    )
        .into_response()
}

fn is_direct_local(req: &Request<Body>) -> bool {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    if !peer.is_some_and(|ip| ip.is_loopback()) {
        return false;
    }
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().host())
        .unwrap_or_default();
    if !loopback_host(host) {
        return false;
    }
    !req.headers().keys().any(|k| {
        let k = k.as_str();
        k == "forwarded"
            || k == "x-real-ip"
            || k.starts_with("x-forwarded-")
            || k.starts_with("x-walgit-")
    })
}

/// `localhost`, `*.localhost`, `127.0.0.1` or `[::1]`, with any port.
fn loopback_host(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']').map_or(rest, |(h, _)| h)
    } else {
        host.rsplit_once(':').map_or(host, |(h, p)| {
            if p.bytes().all(|b| b.is_ascii_digit()) {
                h
            } else {
                host
            }
        })
    };
    let name = name.to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost") || name == "127.0.0.1" || name == "::1"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(peer: &str, host: &str, extra: &[(&str, &str)]) -> Request<Body> {
        let mut r = Request::builder().uri("/x").header(header::HOST, host);
        for (k, v) in extra {
            r = r.header(*k, *v);
        }
        let mut r = r.body(Body::empty()).unwrap();
        r.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
        r
    }

    #[test]
    fn mode_none_serves_only_direct_local_requests() {
        assert!(is_direct_local(&req(
            "127.0.0.1:5000",
            "127.0.0.1:8080",
            &[]
        )));
        assert!(is_direct_local(&req(
            "[::1]:5000",
            "walgit.localhost:8080",
            &[]
        )));
        assert!(is_direct_local(&req("127.0.0.1:5000", "[::1]:8080", &[])));
        assert!(is_direct_local(&req("127.0.0.1:5000", "localhost", &[])));
        for (r, why) in [
            (req("10.0.0.2:5000", "127.0.0.1:8080", &[]), "remote peer"),
            (req("127.0.0.1:5000", "evil.example", &[]), "DNS rebinding"),
            (
                req("127.0.0.1:5000", "evil.example:8080", &[]),
                "rebinding, port",
            ),
            (
                req("127.0.0.1:5000", "localhost.evil.example", &[]),
                "suffix trick",
            ),
            (
                req(
                    "127.0.0.1:5000",
                    "127.0.0.1:8080",
                    &[("x-forwarded-for", "1.2.3.4")],
                ),
                "a local proxy",
            ),
            (
                req(
                    "127.0.0.1:5000",
                    "127.0.0.1:8080",
                    &[("forwarded", "for=1.2.3.4")],
                ),
                "a local proxy (RFC 7239)",
            ),
            (
                req(
                    "127.0.0.1:5000",
                    "127.0.0.1:8080",
                    &[("x-real-ip", "1.2.3.4")],
                ),
                "nginx",
            ),
            (
                req(
                    "127.0.0.1:5000",
                    "127.0.0.1:8080",
                    &[("x-walgit-principal", "x")],
                ),
                "another walgit host",
            ),
        ] {
            assert!(!is_direct_local(&r), "{why}");
        }
        // No peer information at all fails closed.
        let mut r = Request::builder()
            .uri("/x")
            .header(header::HOST, "127.0.0.1")
            .body(Body::empty())
            .unwrap();
        assert!(!is_direct_local(&r));
        r.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>("127.0.0.1:1".parse().unwrap()));
        assert!(is_direct_local(&r));
    }
}

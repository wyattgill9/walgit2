//! D51: people on the API (`docs/ACCOUNTS.md`, `web/API.md`).
//!
//! * `GET|PUT /{o}/{r}/api[-browser]/access` — a repository's visibility and collaborators
//!   (Write reads, Admin replaces the whole document).
//! * `GET /api[-browser]/v1/users?q=` — the user directory, for pickers (any account).
//! * `GET /api/v1/orgs/{login}`, `POST /api/v1/orgs`, `PUT /api/v1/orgs/{login}/owners` —
//!   minimal orgs: a namespace and its owners (site admins create them; owners manage them).

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use walgit_proto::v1::{AccountEvent, OrgCreated, OrgOwnersChanged, RepoAccess, account_event};
use walgit_wal::accounts::{AccountsState, Namespace, User};

use crate::AppState;
use crate::access::{Role, auth_err};
use crate::error::ApiError;

#[derive(Serialize)]
struct Person {
    login: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    avatar_url: String,
}

impl From<&User> for Person {
    fn from(u: &User) -> Self {
        Person {
            login: u.login.clone(),
            name: u.name.clone(),
            avatar_url: u.avatar_url.clone(),
        }
    }
}

#[derive(Serialize)]
struct Collaborator {
    #[serde(flatten)]
    person: Person,
    role: &'static str,
}

#[derive(Serialize)]
struct Owner {
    /// `user` | `org` | `none` (a namespace no account holds: only site admins administer it).
    kind: &'static str,
    login: String,
    /// Org owners (each is admin on every repository of the org).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    owners: Vec<Person>,
}

#[derive(Serialize)]
struct AccessDoc {
    visibility: &'static str,
    /// 0 = never set (public, no collaborators).
    revision: u64,
    owner: Owner,
    collaborators: Vec<Collaborator>,
    /// The caller's role.
    role: &'static str,
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}

fn repo_id(owner: &str, name: &str) -> Result<walgit_git::RepoId, ApiError> {
    walgit_git::RepoId::new(owner, name).map_err(|_| ApiError::NotFound("repository".into()))
}

async fn directory(st: &AppState) -> Result<Arc<walgit_wal::RepoHandle>, ApiError> {
    st.accounts
        .synced()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("account log: {e}")))
}

fn owner_of(s: &AccountsState, namespace: &str) -> Owner {
    let people = |ids: &std::collections::BTreeSet<String>| {
        ids.iter()
            .filter_map(|id| s.users.get(id))
            .map(Person::from)
            .collect()
    };
    match s.namespace(namespace) {
        Namespace::User(u) => Owner {
            kind: "user",
            login: u.login.clone(),
            owners: Vec::new(),
        },
        Namespace::Org(o) => Owner {
            kind: "org",
            login: o.login.clone(),
            owners: people(&o.owners),
        },
        Namespace::Reserved | Namespace::Free => Owner {
            kind: "none",
            login: namespace.to_string(),
            owners: Vec::new(),
        },
    }
}

/// `GET /{o}/{r}/api/access` — visibility, owner and collaborators (Write).
pub async fn get_access(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let id = repo_id(&owner, &name)?;
    let (handle, grant) = crate::access::request_repo(&st, &headers, &id, Role::Write).await?;
    let access = handle.access();
    let dir = directory(&st).await?;
    let doc = dir.accounts_read(|s| {
        let mut collaborators: Vec<Collaborator> = access
            .iter()
            .flat_map(|a| a.collaborators.iter())
            .filter_map(|(user_id, role)| {
                let user = s.users.get(user_id)?;
                let role = walgit_proto::v1::AccessRole::try_from(*role).ok()?;
                Some(Collaborator {
                    person: Person::from(user),
                    role: Role::from_proto(role).as_str(),
                })
            })
            .collect();
        collaborators.sort_by(|a, b| a.person.login.cmp(&b.person.login));
        AccessDoc {
            visibility: walgit_wal::access::visibility_str(
                access
                    .as_ref()
                    .map_or(walgit_proto::v1::Visibility::Public, RepoAccess::visibility),
            ),
            revision: access.as_ref().map_or(0, |a| a.revision),
            owner: owner_of(s, id.owner()),
            collaborators,
            role: grant.role.as_str(),
        }
    });
    Ok(no_store(Json(doc).into_response()))
}

#[derive(Deserialize)]
struct CollaboratorIn {
    login: String,
    role: String,
}

#[derive(Deserialize)]
pub struct AccessIn {
    visibility: String,
    #[serde(default)]
    collaborators: Vec<CollaboratorIn>,
    /// The revision the caller edited: refused (409) when another change landed first.
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    message: String,
}

/// `PUT /{o}/{r}/api/access` — replace visibility and collaborators (Admin). Every login must
/// be an account; `role` is `read` | `triage` | `write` | `maintain` | `admin`.
pub async fn put_access(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<AccessIn>,
) -> Result<Response, ApiError> {
    let id = repo_id(&owner, &name)?;
    let (handle, grant) = crate::access::request_repo(&st, &headers, &id, Role::Admin).await?;
    let visibility = walgit_wal::access::parse_visibility(&body.visibility).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "visibility {:?} is not public, internal or private",
            body.visibility
        ))
    })?;
    let dir = directory(&st).await?;
    let mut collaborators = std::collections::HashMap::new();
    for c in &body.collaborators {
        let role = Role::parse(&c.role).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "role {:?} is not read, triage, write, maintain or admin",
                c.role
            ))
        })?;
        let user = dir
            .accounts_read(|s| s.user_by_login(&c.login).cloned())
            .ok_or_else(|| ApiError::BadRequest(format!("no user {:?}", c.login)))?;
        collaborators.insert(user.user_id, role.to_proto() as i32);
    }
    let doc = RepoAccess {
        visibility: visibility as i32,
        collaborators,
        author: grant.subject.user_id.clone().unwrap_or_default(),
        message: body.message,
        ..Default::default()
    };
    let revision = handle
        .publish_access(doc, body.revision)
        .await
        .map_err(|e| match e {
            walgit_wal::WalError::Invalid(m) => ApiError::Conflict(m),
            e => crate::smart::wal_err(e),
        })?;
    tracing::info!(repo = %id, by = %grant.subject.name, revision, "access published");
    Ok(no_store(
        Json(serde_json::json!({ "revision": revision })).into_response(),
    ))
}

#[derive(Deserialize)]
pub struct UsersQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    n: Option<usize>,
}

/// `GET /api/v1/users?q=&n=` — directory search by login or name prefix (any account).
pub async fn users(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<UsersQuery>,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(&st, &headers)
        .await
        .map_err(auth_err)?;
    if subject.is_anonymous() {
        return Err(ApiError::Unauthorized);
    }
    let dir = directory(&st).await?;
    let n = q.n.unwrap_or(20).clamp(1, 100);
    let people: Vec<Person> = dir.accounts_read(|s| {
        s.search(q.q.trim(), n)
            .into_iter()
            .map(Person::from)
            .collect()
    });
    Ok(no_store(Json(people).into_response()))
}

#[derive(Serialize)]
struct OrgDoc {
    login: String,
    owners: Vec<Person>,
}

/// `GET /api/v1/orgs/{login}` (any account).
pub async fn get_org(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(&st, &headers)
        .await
        .map_err(auth_err)?;
    if subject.is_anonymous() {
        return Err(ApiError::Unauthorized);
    }
    let dir = directory(&st).await?;
    let doc = dir
        .accounts_read(|s| match owner_of(s, &login) {
            Owner {
                kind: "org",
                login,
                owners,
            } => Some(OrgDoc { login, owners }),
            _ => None,
        })
        .ok_or_else(|| ApiError::NotFound(format!("org {login}")))?;
    Ok(no_store(Json(doc).into_response()))
}

#[derive(Deserialize)]
pub struct OrgIn {
    login: String,
    owners: Vec<String>,
}

#[derive(Deserialize)]
pub struct OwnersIn {
    owners: Vec<String>,
}

fn owner_ids(s: &AccountsState, logins: &[String]) -> Result<Vec<String>, ApiError> {
    logins
        .iter()
        .map(|l| {
            s.user_by_login(l)
                .map(|u| u.user_id.clone())
                .ok_or_else(|| ApiError::BadRequest(format!("no user {l:?}")))
        })
        .collect()
}

/// `POST /api/v1/orgs` `{login, owners: [login]}` — a site admin creates an org, claiming a
/// namespace that has repositories but no account, or a free one.
pub async fn create_org(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<OrgIn>,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(&st, &headers)
        .await
        .map_err(auth_err)?;
    if !subject.site_admin {
        return Err(ApiError::Denied("only a site admin creates orgs".into()));
    }
    let dir = directory(&st).await?;
    let owners = dir.accounts_read(|s| owner_ids(s, &body.owners))?;
    let ev = AccountEvent {
        body: Some(account_event::Body::OrgCreated(OrgCreated {
            org_id: uuid::Uuid::new_v4().to_string(),
            login: body.login.clone(),
            owners,
        })),
    };
    publish(&dir, ev, &subject.name).await?;
    Ok((StatusCode::CREATED, "created").into_response())
}

/// `PUT /api/v1/orgs/{login}/owners` `{owners: [login]}` — a site admin or an owner of the
/// org replaces its owners.
pub async fn put_org_owners(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(login): Path<String>,
    Json(body): Json<OwnersIn>,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(&st, &headers)
        .await
        .map_err(auth_err)?;
    let dir = directory(&st).await?;
    let me = subject.user_id.clone().unwrap_or_default();
    let (org_id, owners) = dir.accounts_read(|s| {
        let Namespace::Org(org) = s.namespace(&login) else {
            return Err(ApiError::NotFound(format!("org {login}")));
        };
        if !subject.site_admin && !org.owners.contains(&me) {
            return Err(ApiError::Denied(format!(
                "only a site admin or an owner of {login} changes its owners"
            )));
        }
        Ok((org.org_id.clone(), owner_ids(s, &body.owners)?))
    })?;
    let ev = AccountEvent {
        body: Some(account_event::Body::OrgOwnersChanged(OrgOwnersChanged {
            org_id,
            owners,
        })),
    };
    publish(&dir, ev, &subject.name).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

async fn publish(
    dir: &walgit_wal::RepoHandle,
    ev: AccountEvent,
    author: &str,
) -> Result<(), ApiError> {
    dir.publish_accounts(vec![ev], author)
        .await
        .map(|_| ())
        .map_err(|e| match e {
            walgit_wal::WalError::Invalid(m) => ApiError::Conflict(m),
            e => ApiError::ServiceUnavailable(format!("account log: {e}")),
        })
}

#[derive(Deserialize)]
pub struct NewUser {
    login: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
    /// D52: a password to sign in with (password accounts on); none = a robot for tokens.
    #[serde(default)]
    password: Option<String>,
}

/// `POST /api/v1/users` — a site admin creates an account (`passwords = "admin"` is how people
/// get one; robots for static tokens need no password).
pub async fn create_user(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<NewUser>,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(&st, &headers)
        .await
        .map_err(auth_err)?;
    if !subject.site_admin {
        return Err(ApiError::Denied(
            "only a site admin creates accounts".into(),
        ));
    }
    let login = body.login.trim().to_ascii_lowercase();
    walgit_wal::accounts::valid_user_login(&login).map_err(ApiError::BadRequest)?;
    if body.password.is_some() && st.auth.passwords() == walgit_config::PasswordAccounts::Off {
        return Err(ApiError::BadRequest(
            "password accounts are off on this host (server.auth.passwords)".into(),
        ));
    }
    crate::web::login::create_user(
        &st,
        &login,
        body.email.trim(),
        body.name.trim(),
        body.password,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "login": login })),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct PasswordIn {
    password: String,
}

/// `PUT /api/v1/users/{login}/password` — a site admin sets someone's password (a reset).
pub async fn put_password(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(login): Path<String>,
    Json(body): Json<PasswordIn>,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(&st, &headers)
        .await
        .map_err(auth_err)?;
    if !subject.site_admin {
        return Err(ApiError::Denied(
            "only a site admin resets passwords".into(),
        ));
    }
    if st.auth.passwords() == walgit_config::PasswordAccounts::Off {
        return Err(ApiError::BadRequest(
            "password accounts are off on this host (server.auth.passwords)".into(),
        ));
    }
    let dir = directory(&st).await?;
    let user = dir
        .accounts_read(|s| s.user_by_login(&login).cloned())
        .ok_or_else(|| ApiError::NotFound(format!("user {login}")))?;
    crate::web::login::set_password(&st, &user.user_id, &body.password).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

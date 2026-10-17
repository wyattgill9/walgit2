//! D50: the issues and pull-requests API (`docs/FORGE.md`, `web/API.md`). Reads fold the WAL's
//! forge events at refs level (no objects); writes are one forge entry each through the
//! publisher. Bodies are JSON only (`Json` refuses other content types with 415), which keeps a
//! cross-site form from riding the session cookie. Handlers are shared: `PULL` picks the kind.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use walgit_proto::v1::{
    ForgeComment, ForgeDraft, ForgeEdit, ForgeEvent, ForgeKind, ForgeLabel, ForgeLineComment,
    ForgeOpened, ForgeReview, ForgeSetChange, ForgeStateChange, forge_edit, forge_event::Body,
};
use walgit_wal::RepoHandle;
use walgit_wal::forge::{Item, SearchPage, TimelineItem};

use crate::AppState;
use crate::access::{Role, auth_err};
use crate::error::ApiError;
use crate::web::api::{REPO_API_BASES, json_swr};

pub fn router(state: Arc<AppState>) -> Router {
    let mut r = Router::new();
    for base in REPO_API_BASES {
        r = routes::<false>(r, &format!("{base}/issues"))
            .route(&format!("{base}/issues"), post(create_issue))
            .route(&format!("{base}/forge/summary"), get(summary))
            .route(&format!("{base}/labels"), get(labels))
            .route(
                &format!("{base}/labels/{{name}}"),
                axum::routing::put(put_label).delete(delete_label),
            );
        r = routes::<true>(r, &format!("{base}/pulls"))
            .route(&format!("{base}/pulls"), post(create_pull))
            .route(&format!("{base}/pulls/{{number}}/reviews"), post(review))
            .route(
                &format!("{base}/pulls/{{number}}/merge"),
                get(super::forge_merge::mergeability).post(super::forge_merge::merge),
            )
            .route(
                &format!("{base}/pulls/{{number}}/branch"),
                axum::routing::delete(super::forge_merge::delete_branch),
            );
    }
    r.with_state(state)
}

fn routes<const PULL: bool>(r: Router<Arc<AppState>>, p: &str) -> Router<Arc<AppState>> {
    r.route(p, get(list::<PULL>))
        .route(
            &format!("{p}/{{number}}"),
            get(detail::<PULL>).patch(update::<PULL>),
        )
        .route(&format!("{p}/{{number}}/comments"), post(comment::<PULL>))
        .route(
            &format!("{p}/{{number}}/comments/{{id}}"),
            patch(edit_comment::<PULL>).delete(delete_comment::<PULL>),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    page: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct Detail {
    #[serde(flatten)]
    item: Item,
    timeline: Vec<TimelineItem>,
    /// Pull requests: the tips to compare — the branches' current tips while open, the tips
    /// when it was closed after that.
    #[serde(skip_serializing_if = "Option::is_none")]
    base_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_sha: Option<String>,
}

#[derive(Deserialize)]
struct NewIssue {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignees: Vec<String>,
}

#[derive(Deserialize)]
struct NewPull {
    title: String,
    #[serde(default)]
    body: String,
    /// Branch names: merge `head` into `base`.
    base: String,
    head: String,
    /// Open as a draft (not mergeable until ready for review).
    #[serde(default)]
    draft: bool,
}

/// Every field optional; the present ones commit together as one entry.
#[derive(Deserialize, Default)]
struct ItemPatch {
    title: Option<String>,
    body: Option<String>,
    /// "open" | "closed"
    state: Option<String>,
    /// Closing: `completed` (default) | `not_planned`
    state_reason: Option<String>,
    #[serde(default)]
    add_labels: Vec<String>,
    #[serde(default)]
    remove_labels: Vec<String>,
    #[serde(default)]
    add_assignees: Vec<String>,
    #[serde(default)]
    remove_assignees: Vec<String>,
    /// Pull requests: `true` = convert to draft, `false` = ready for review.
    draft: Option<bool>,
}

#[derive(Deserialize)]
struct NewComment {
    body: String,
    /// GitHub's "Close with comment": also close, with this reason.
    close: Option<String>,
}

#[derive(Deserialize)]
struct CommentPatch {
    body: String,
}

#[derive(Deserialize)]
struct NewReview {
    /// `approved` | `changes_requested` | `commented`
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    comments: Vec<NewLineComment>,
}

#[derive(Deserialize)]
struct NewLineComment {
    path: String,
    /// `RIGHT` (the head) | `LEFT` (the merge base)
    side: String,
    line: u32,
    body: String,
}

const fn kind_name(pull: bool) -> &'static str {
    if pull { "pull" } else { "issue" }
}

/// The repository, revalidated and authorized for `need` (D51).
pub(super) async fn open(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    need: Role,
) -> Result<(Arc<RepoHandle>, crate::access::Grant), ApiError> {
    let id = walgit_git::RepoId::new(owner, name)
        .map_err(|_| ApiError::NotFound("repository".into()))?;
    crate::access::request_repo(st, headers, &id, need).await
}

/// Refs-level sync + the forge fold: every read sees every acknowledged write (principle IV).
pub(super) async fn synced(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<Arc<RepoHandle>, ApiError> {
    let (handle, _) = open(st, headers, owner, name, Role::Read).await?;
    drop(handle.sync_refs().await.map_err(crate::smart::wal_err)?);
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    Ok(handle)
}

/// A branch's tip from the synced ref state.
pub(super) fn branch_tip(handle: &RepoHandle, branch: &str) -> Option<String> {
    let refs = handle.local().ref_view().ok()?;
    refs.get(&format!("refs/heads/{branch}"))
        .filter(|oid| !oid.is_empty())
}

/// (base, head) a pull request shows: live while open (the recorded tip once a branch is gone).
pub(super) fn tips(handle: &RepoHandle, i: &Item) -> (String, String) {
    if i.state == "open" {
        (
            branch_tip(handle, &i.base_ref).unwrap_or_else(|| i.base_oid.clone()),
            branch_tip(handle, &i.head_ref).unwrap_or_else(|| i.head_oid.clone()),
        )
    } else {
        (i.base_oid.clone(), i.head_oid.clone())
    }
}

pub(super) fn detail_of(handle: &RepoHandle, number: u64, pull: bool) -> Result<Detail, ApiError> {
    let item = handle
        .forge_read(|s| {
            s.items
                .get(&number)
                // The issues routes answer for pull requests too (GitHub's API does): an `#n` link
                // needs no kind.
                .filter(|i| !pull || i.kind == "pull")
                .cloned()
        })
        .ok_or_else(|| ApiError::NotFound(format!("{} #{number}", kind_name(pull))))?;
    let (base_sha, head_sha) = if pull {
        let (b, h) = tips(handle, &item);
        (Some(b), Some(h))
    } else {
        (None, None)
    };
    Ok(Detail {
        timeline: item.timeline.clone(),
        item,
        base_sha,
        head_sha,
    })
}

async fn list<const PULL: bool>(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    let kind = if PULL { "is:pr" } else { "is:issue" };
    let query = format!("{kind} {}", q.q.as_deref().unwrap_or("is:open"));
    let page: SearchPage = handle.forge_read(|s| s.search(&query, q.page.unwrap_or(1)));
    Ok(json_swr(&page, None).into_response(&headers))
}

async fn detail<const PULL: bool>(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    Ok(json_swr(&detail_of(&handle, number, PULL)?, None).into_response(&headers))
}

/// Publishes `events` as the signed-in account (D51 security 2: reading a public repository
/// anonymously is not taking part). The publisher re-derives the role at its CAS basis and
/// applies the forge rules to it (`ForgeState::verify`).
async fn publish(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    events: Vec<ForgeEvent>,
) -> Result<(Arc<RepoHandle>, u64), ApiError> {
    let subject = crate::access::authenticate(st, headers)
        .await
        .map_err(auth_err)?;
    if subject.is_anonymous() {
        return Err(ApiError::Unauthorized);
    }
    let id = walgit_git::RepoId::new(owner, name)
        .map_err(|_| ApiError::NotFound("repository".into()))?;
    let (handle, grant) = crate::access::repo(st, &id, &subject, Role::Read).await?;
    let assigned: Vec<String> = events
        .iter()
        .flat_map(|ev| match &ev.body {
            Some(Body::Opened(o)) => o.assignees.clone(),
            Some(Body::Assignees(c)) => c.add.clone(),
            _ => Vec::new(),
        })
        .collect();
    require_users(st, &assigned).await?;
    let seq = handle
        .publish_forge(grant.actor(), events)
        .await
        .map_err(|e| match e {
            walgit_wal::WalError::Invalid(msg) => ApiError::BadRequest(msg),
            e => crate::smart::wal_err(e),
        })?;
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    Ok((handle, seq))
}

pub(super) fn event(number: u64, body: Body) -> ForgeEvent {
    ForgeEvent {
        number,
        actor: String::new(),
        actor_id: String::new(),
        body: Some(body),
    }
}

/// Closing an issue (no tips to freeze).
pub(super) fn closing_event(number: u64, reason: &str) -> ForgeEvent {
    event(
        number,
        Body::State(ForgeStateChange {
            closed: true,
            reason: reason.into(),
            ..Default::default()
        }),
    )
}

fn set_change(add: Vec<String>, remove: Vec<String>) -> Option<ForgeSetChange> {
    (!add.is_empty() || !remove.is_empty()).then_some(ForgeSetChange { add, remove })
}

/// The close event; a pull request freezes the tips it shows from now on.
async fn closing<const PULL: bool>(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    number: u64,
    reason: Option<String>,
) -> Result<ForgeEvent, ApiError> {
    let (base_oid, head_oid) = if PULL {
        let handle = synced(st, headers, owner, repo).await?;
        let item = handle
            .forge_read(|s| s.items.get(&number).cloned())
            .ok_or_else(|| ApiError::NotFound(format!("pull #{number}")))?;
        tips(&handle, &item)
    } else {
        Default::default()
    };
    Ok(event(
        number,
        Body::State(ForgeStateChange {
            closed: true,
            reason: reason.unwrap_or_else(|| "completed".into()),
            base_oid,
            head_oid,
        }),
    ))
}

/// D51: assignees are accounts — every login must name one (the directory as last synced,
/// revalidated once before refusing).
async fn require_users(st: &AppState, logins: &[String]) -> Result<(), ApiError> {
    let missing = |s: &walgit_wal::accounts::AccountsState| {
        logins
            .iter()
            .find(|l| s.user_by_login(l).is_none())
            .cloned()
    };
    let unavailable =
        |e: walgit_wal::WalError| ApiError::ServiceUnavailable(format!("account log: {e}"));
    if logins.is_empty()
        || st
            .accounts
            .cached(missing)
            .await
            .map_err(unavailable)?
            .is_none()
    {
        return Ok(());
    }
    let dir = st.accounts.synced().await.map_err(unavailable)?;
    match dir.accounts_read(missing) {
        Some(login) => Err(ApiError::BadRequest(format!("no user {login:?} to assign"))),
        None => Ok(()),
    }
}

async fn create_issue(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Json(new): Json<NewIssue>,
) -> Result<Response, ApiError> {
    let opened = ForgeOpened {
        kind: ForgeKind::Issue as i32,
        title: new.title,
        body: new.body,
        labels: new.labels,
        assignees: new.assignees,
        ..Default::default()
    };
    created(&st, &headers, &owner, &repo, opened, false).await
}

async fn create_pull(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Json(new): Json<NewPull>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    let tip = |b: &str| {
        branch_tip(&handle, b).ok_or_else(|| ApiError::BadRequest(format!("no branch {b:?}")))
    };
    let (base_oid, head_oid) = (tip(&new.base)?, tip(&new.head)?);
    if base_oid == head_oid {
        return Err(ApiError::BadRequest(format!(
            "{} and {} are identical",
            new.base, new.head
        )));
    }
    let opened = ForgeOpened {
        kind: ForgeKind::Pull as i32,
        title: new.title,
        body: new.body,
        base_ref: new.base,
        head_ref: new.head,
        base_oid,
        head_oid,
        draft: new.draft,
        ..Default::default()
    };
    created(&st, &headers, &owner, &repo, opened, true).await
}

async fn created(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    opened: ForgeOpened,
    pull: bool,
) -> Result<Response, ApiError> {
    let (handle, seq) = publish(
        st,
        headers,
        owner,
        repo,
        vec![event(0, Body::Opened(opened))],
    )
    .await?;
    let number = handle
        .forge_read(|s| s.opened_at(seq))
        .ok_or_else(|| ApiError::Internal(format!("item opened at seq {seq} not found")))?;
    Ok((StatusCode::CREATED, Json(detail_of(&handle, number, pull)?)).into_response())
}

async fn update<const PULL: bool>(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    Json(p): Json<ItemPatch>,
) -> Result<Response, ApiError> {
    let mut events = Vec::new();
    let edit = |change| {
        event(
            number,
            Body::Edit(ForgeEdit {
                comment_id: 0,
                change: Some(change),
            }),
        )
    };
    if let Some(t) = p.title {
        events.push(edit(forge_edit::Change::Title(t)));
    }
    if let Some(b) = p.body {
        events.push(edit(forge_edit::Change::Body(b)));
    }
    if let Some(c) = set_change(p.add_labels, p.remove_labels) {
        events.push(event(number, Body::Labels(c)));
    }
    if let Some(draft) = p.draft {
        events.push(event(number, Body::Draft(ForgeDraft { draft })));
    }
    if let Some(c) = set_change(p.add_assignees, p.remove_assignees) {
        events.push(event(number, Body::Assignees(c)));
    }
    match p.state.as_deref() {
        Some("closed") => events
            .push(closing::<PULL>(&st, &headers, &owner, &repo, number, p.state_reason).await?),
        Some("open") => events.push(event(number, Body::State(ForgeStateChange::default()))),
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "state is \"open\" or \"closed\", not {other:?}"
            )));
        }
        None => {}
    }
    if events.is_empty() {
        return Err(ApiError::BadRequest("nothing to change".into()));
    }
    let (handle, _) = publish(&st, &headers, &owner, &repo, events).await?;
    Ok(Json(detail_of(&handle, number, PULL)?).into_response())
}

async fn comment<const PULL: bool>(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    Json(c): Json<NewComment>,
) -> Result<Response, ApiError> {
    let mut events = vec![event(number, Body::Comment(ForgeComment { body: c.body }))];
    if let Some(reason) = c.close {
        events.push(closing::<PULL>(&st, &headers, &owner, &repo, number, Some(reason)).await?);
    }
    let (handle, _) = publish(&st, &headers, &owner, &repo, events).await?;
    Ok((StatusCode::CREATED, Json(detail_of(&handle, number, PULL)?)).into_response())
}

async fn edit_comment<const PULL: bool>(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number, id)): Path<(String, String, u64, u64)>,
    Json(c): Json<CommentPatch>,
) -> Result<Response, ApiError> {
    let change = forge_edit::Change::Body(c.body);
    let ev = event(
        number,
        Body::Edit(ForgeEdit {
            comment_id: id,
            change: Some(change),
        }),
    );
    let (handle, _) = publish(&st, &headers, &owner, &repo, vec![ev]).await?;
    Ok(Json(detail_of(&handle, number, PULL)?).into_response())
}

async fn delete_comment<const PULL: bool>(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number, id)): Path<(String, String, u64, u64)>,
) -> Result<Response, ApiError> {
    let change = forge_edit::Change::Deleted(true);
    let ev = event(
        number,
        Body::Edit(ForgeEdit {
            comment_id: id,
            change: Some(change),
        }),
    );
    let (handle, _) = publish(&st, &headers, &owner, &repo, vec![ev]).await?;
    Ok(Json(detail_of(&handle, number, PULL)?).into_response())
}

/// A review of the pull request's current head (its line comments anchor there).
async fn review(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    Json(r): Json<NewReview>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    let item = handle
        .forge_read(|s| s.items.get(&number).cloned())
        .ok_or_else(|| ApiError::NotFound(format!("pull #{number}")))?;
    let (_, commit_oid) = tips(&handle, &item);
    let comments = r
        .comments
        .into_iter()
        .map(|c| ForgeLineComment {
            path: c.path,
            side: c.side,
            line: c.line,
            body: c.body,
        })
        .collect();
    let ev = event(
        number,
        Body::Review(ForgeReview {
            state: r.state,
            body: r.body,
            commit_oid,
            comments,
        }),
    );
    let (handle, _) = publish(&st, &headers, &owner, &repo, vec![ev]).await?;
    Ok((StatusCode::CREATED, Json(detail_of(&handle, number, true)?)).into_response())
}

#[derive(Serialize)]
struct Summary {
    open_issues: usize,
    open_pulls: usize,
}

/// `GET …/forge/summary`: the open counts the repository tabs show.
async fn summary(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    let s = handle.forge_read(|s| {
        let open = |kind: &str| {
            s.items
                .values()
                .filter(|i| i.kind == kind && i.state == "open")
                .count()
        };
        Summary {
            open_issues: open("issue"),
            open_pulls: open("pull"),
        }
    });
    Ok(json_swr(&s, None).into_response(&headers))
}

#[derive(Serialize)]
struct LabelRow {
    #[serde(flatten)]
    def: walgit_wal::forge::LabelDef,
    /// Open issues and pull requests carrying it.
    open: usize,
}

/// `GET …/labels`: the catalog (plus labels items carry without a definition, gray), with how
/// many open items carry each.
async fn labels(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    let rows: Vec<LabelRow> = handle.forge_read(|s| {
        let mut defs = s.labels.clone();
        for i in s.items.values() {
            for l in &i.labels {
                defs.entry(l.clone())
                    .or_insert_with(|| walgit_wal::forge::LabelDef {
                        name: l.clone(),
                        color: "#777777".into(),
                        description: String::new(),
                    });
            }
        }
        defs.into_values()
            .map(|def| LabelRow {
                open: s
                    .items
                    .values()
                    .filter(|i| i.state == "open" && i.labels.contains(&def.name))
                    .count(),
                def,
            })
            .collect()
    });
    Ok(json_swr(&rows, None).into_response(&headers))
}

#[derive(Deserialize)]
struct LabelBody {
    /// `#rrggbb`
    color: String,
    #[serde(default)]
    description: String,
}

/// `PUT …/labels/{name}`: create or update a label (write).
async fn put_label(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Json(b): Json<LabelBody>,
) -> Result<Response, ApiError> {
    let ev = event(
        0,
        Body::Label(ForgeLabel {
            name,
            color: b.color,
            description: b.description,
            deleted: false,
        }),
    );
    publish(&st, &headers, &owner, &repo, vec![ev]).await?;
    labels(State(st), headers, Path((owner, repo))).await
}

/// `DELETE …/labels/{name}`: drop it from the catalog and from every item (write).
async fn delete_label(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let ev = event(
        0,
        Body::Label(ForgeLabel {
            name,
            deleted: true,
            ..Default::default()
        }),
    );
    publish(&st, &headers, &owner, &repo, vec![ev]).await?;
    labels(State(st), headers, Path((owner, repo))).await
}

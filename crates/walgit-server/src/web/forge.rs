//! D50: the issues API (`docs/FORGE.md`, `web/API.md`). Reads fold the WAL's forge events at
//! refs level (no objects); writes are one forge entry each through the publisher. Bodies are
//! JSON only (`Json` refuses other content types with 415), which keeps a cross-site form from
//! riding the session cookie.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use serde::{Deserialize, Serialize};
use walgit_proto::v1::{
    ForgeComment, ForgeEdit, ForgeEvent, ForgeKind, ForgeOpened, ForgeSetChange, ForgeStateChange,
    forge_edit, forge_event::Body,
};
use walgit_wal::RepoHandle;
use walgit_wal::forge::{Actor, Item, SearchPage, TimelineItem};

use crate::AppState;
use crate::error::ApiError;
use crate::web::api::{REPO_API_BASES, auth_err, json_swr};

pub fn router(state: Arc<AppState>) -> Router {
    let mut r = Router::new();
    for base in REPO_API_BASES {
        r = r
            .route(&format!("{base}/issues"), get(list).post(create))
            .route(
                &format!("{base}/issues/{{number}}"),
                get(detail).patch(update),
            )
            .route(
                &format!("{base}/issues/{{number}}/comments"),
                axum::routing::post(comment),
            )
            .route(
                &format!("{base}/issues/{{number}}/comments/{{id}}"),
                patch(edit_comment).delete(delete_comment),
            );
    }
    r.with_state(state)
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    page: Option<usize>,
}

#[derive(Serialize)]
struct Detail {
    #[serde(flatten)]
    item: Item,
    timeline: Vec<TimelineItem>,
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

/// Every field optional; the present ones commit together as one entry.
#[derive(Deserialize, Default)]
struct IssuePatch {
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

async fn open(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<Arc<RepoHandle>, ApiError> {
    st.auth.require_read(headers).await.map_err(auth_err)?;
    let id = walgit_git::RepoId::new(owner, name)
        .map_err(|_| ApiError::NotFound("repository".into()))?;
    st.registry.open(&id).await.map_err(|e| match e {
        walgit_wal::WalError::NotFound => ApiError::NotFound("repository".into()),
        e => crate::smart::wal_err(e),
    })
}

/// Refs-level sync + the forge fold: every read sees every acknowledged write (principle IV).
async fn synced(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<Arc<RepoHandle>, ApiError> {
    let handle = open(st, headers, owner, name).await?;
    drop(handle.sync_refs().await.map_err(crate::smart::wal_err)?);
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    Ok(handle)
}

fn detail_of(handle: &RepoHandle, number: u64) -> Result<Detail, ApiError> {
    handle
        .forge_read(|s| {
            s.items
                .get(&number)
                .filter(|i| i.kind == "issue")
                .map(|i| Detail {
                    item: i.clone(),
                    timeline: i.timeline.clone(),
                })
        })
        .ok_or_else(|| ApiError::NotFound(format!("issue #{number}")))
}

async fn list(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    let query = format!("is:issue {}", q.q.as_deref().unwrap_or("is:open"));
    let page: SearchPage = handle.forge_read(|s| s.search(&query, q.page.unwrap_or(1)));
    Ok(json_swr(&page, None).into_response(&headers))
}

async fn detail(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
) -> Result<Response, ApiError> {
    let handle = synced(&st, &headers, &owner, &repo).await?;
    Ok(json_swr(&detail_of(&handle, number)?, None).into_response(&headers))
}

/// Publishes `events` as the signed-in principal; anonymous readers can't write.
async fn publish(
    st: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    events: Vec<ForgeEvent>,
) -> Result<(Arc<RepoHandle>, u64), ApiError> {
    let principal = st.auth.require_read(headers).await.map_err(auth_err)?;
    if principal.anonymous && !principal.write {
        return Err(ApiError::Unauthorized);
    }
    let handle = open(st, headers, owner, name).await?;
    let actor = Actor {
        name: principal.name,
        write: principal.write,
    };
    let seq = handle
        .publish_forge(actor, events)
        .await
        .map_err(|e| match e {
            walgit_wal::WalError::Invalid(msg) => ApiError::BadRequest(msg),
            e => crate::smart::wal_err(e),
        })?;
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    Ok((handle, seq))
}

fn event(number: u64, body: Body) -> ForgeEvent {
    ForgeEvent {
        number,
        actor: String::new(),
        body: Some(body),
    }
}

fn set_change(add: Vec<String>, remove: Vec<String>) -> Option<ForgeSetChange> {
    (!add.is_empty() || !remove.is_empty()).then_some(ForgeSetChange { add, remove })
}

fn closing(reason: Option<String>) -> Body {
    Body::State(ForgeStateChange {
        closed: true,
        reason: reason.unwrap_or_else(|| "completed".into()),
    })
}

async fn create(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Json(new): Json<NewIssue>,
) -> Result<Response, ApiError> {
    let opened = Body::Opened(ForgeOpened {
        kind: ForgeKind::Issue as i32,
        title: new.title,
        body: new.body,
        labels: new.labels,
        assignees: new.assignees,
    });
    let (handle, seq) = publish(&st, &headers, &owner, &repo, vec![event(0, opened)]).await?;
    let number = handle
        .forge_read(|s| s.opened_at(seq))
        .ok_or_else(|| ApiError::Internal(format!("issue opened at seq {seq} not found")))?;
    Ok((StatusCode::CREATED, Json(detail_of(&handle, number)?)).into_response())
}

async fn update(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    Json(p): Json<IssuePatch>,
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
    if let Some(c) = set_change(p.add_assignees, p.remove_assignees) {
        events.push(event(number, Body::Assignees(c)));
    }
    match p.state.as_deref() {
        Some("closed") => events.push(event(number, closing(p.state_reason))),
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
    Ok(Json(detail_of(&handle, number)?).into_response())
}

async fn comment(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    Json(c): Json<NewComment>,
) -> Result<Response, ApiError> {
    let mut events = vec![event(number, Body::Comment(ForgeComment { body: c.body }))];
    if let Some(reason) = c.close {
        events.push(event(number, closing(Some(reason))));
    }
    let (handle, _) = publish(&st, &headers, &owner, &repo, events).await?;
    Ok((StatusCode::CREATED, Json(detail_of(&handle, number)?)).into_response())
}

async fn edit_comment(
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
    Ok(Json(detail_of(&handle, number)?).into_response())
}

async fn delete_comment(
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
    Ok(Json(detail_of(&handle, number)?).into_response())
}

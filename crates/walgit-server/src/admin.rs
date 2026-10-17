//! Admin endpoints: `PUT /{owner}/{repo}` (create), `DELETE /{owner}/{repo}`
//! (delete manifest + prefix objects), `GET /` (list repos, text/plain).

use crate::access::auth_err;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use walgit_git::ObjectFormat;

use crate::AppState;
use crate::access::Role;
use crate::error::ApiError;
use crate::repo::RepoRoute;

/// `PUT /{owner}/{repo}[?object_format=&visibility=public|internal|private]` — create repo.
/// 201 on new, 409 if it exists. D51: under your own login or an org you own (host `write`),
/// anywhere for a site admin; the visibility is in the creation's CAS.
pub async fn create(
    st: &AppState,
    route: &RepoRoute,
    headers: &HeaderMap,
    query: &str,
) -> Result<Response, ApiError> {
    let subject = crate::access::authenticate(st, headers)
        .await
        .map_err(auth_err)?;
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
    };
    let format = match param("object_format") {
        Some("sha256") => ObjectFormat::Sha256,
        Some("sha1") => ObjectFormat::Sha1,
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "unsupported object format: {other}"
            )));
        }
        None => ObjectFormat::from(st.cfg.git.object_format),
    };
    let access = match param("visibility") {
        None => None,
        Some(v) => Some(walgit_proto::v1::RepoAccess {
            visibility: walgit_wal::access::parse_visibility(v).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "visibility {v:?} is not public, internal or private"
                ))
            })? as i32,
            revision: 1,
            author: subject.user_id.clone().unwrap_or_default(),
            updated_at: Some(walgit_proto::time::now()),
            message: "created".into(),
            ..Default::default()
        }),
    };
    crate::access::authorize_create(st, &route.id, &subject).await?;
    match st
        .registry
        .create_with_access(&route.id, format, access)
        .await
    {
        Ok(_h) => Ok((StatusCode::CREATED, "created").into_response()),
        Err(walgit_wal::WalError::AlreadyExists) => {
            Ok((StatusCode::CONFLICT, "already exists").into_response())
        }
        Err(e) => Err(wal_err(e)),
    }
}

/// `DELETE /{owner}/{repo}` — deletion of the manifest and every object under the repo
/// prefix. Needs Admin on the repository.
pub async fn delete(
    st: &AppState,
    route: &RepoRoute,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    crate::access::request_repo(st, headers, &route.id, Role::Admin).await?;
    st.registry.delete(&route.id).await.map_err(wal_err)?;
    Ok((StatusCode::NO_CONTENT, "").into_response())
}

/// `GET /` — list the repos the caller may read as text/plain, one `owner/name` per line.
pub async fn list_repos(st: &AppState, headers: &HeaderMap) -> Result<Response, ApiError> {
    let subject = crate::access::signed_in(st, headers)
        .await
        .map_err(auth_err)?;
    let repos = st.registry.list().await.map_err(wal_err)?;
    let body = crate::access::readable(st, &subject, repos)
        .await
        .into_iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        body,
    )
        .into_response())
}

fn wal_err(e: walgit_wal::WalError) -> ApiError {
    match &e {
        walgit_wal::WalError::NotFound => ApiError::NotFound(e.to_string()),
        _ => ApiError::Internal(format!("wal: {e}")),
    }
}

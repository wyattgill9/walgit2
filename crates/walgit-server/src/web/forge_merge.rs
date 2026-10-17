//! D50: merging pull requests (`docs/FORGE.md`). Ported from rgodha24/walgithub's facade: every
//! object a merge creates is built in a scratch object directory that reads the serving copy
//! through `GIT_ALTERNATE_OBJECT_DIRECTORIES`, so a refused merge leaves nothing behind. The
//! merge is `git merge-tree --write-tree` (no worktree); the result is packed, indexed into the
//! serving copy, proved connected, and published as one PUSH entry that moves the base branch,
//! marks the pull request merged and closes the issues it fixes — all or nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as UrlPath, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use walgit_proto::v1::{ForgeMerged, RefTransaction, RefUpdate, forge_event::Body};
use walgit_wal::forge::Item;

use super::forge::{closing_event, detail_of, event, open, tips};
use crate::AppState;
use crate::access::Role;
use crate::error::ApiError;
use crate::web::api::json_swr;

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(e.to_string())
}

/// A scratch object directory over the bare serving copy (`GIT_WORK_TREE` is an empty
/// directory: `merge-tree` and `commit-tree` need none, but git wants one set).
struct Scratch {
    dir: tempfile::TempDir,
    git_dir: PathBuf,
}

impl Scratch {
    async fn new(git_dir: &Path) -> Result<Self, ApiError> {
        let git_dir = git_dir.to_path_buf();
        let dir = tokio::task::spawn_blocking(|| -> std::io::Result<tempfile::TempDir> {
            let dir = tempfile::Builder::new().prefix("walgit-merge-").tempdir()?;
            std::fs::create_dir_all(dir.path().join("objects"))?;
            std::fs::create_dir_all(dir.path().join("worktree"))?;
            Ok(dir)
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        Ok(Self { dir, git_dir })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(self.dir.path())
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_OBJECT_DIRECTORY", self.dir.path().join("objects"))
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                self.git_dir.join("objects"),
            )
            .env("GIT_INDEX_FILE", self.dir.path().join("index"))
            .env("GIT_WORK_TREE", self.dir.path().join("worktree"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd
    }

    async fn run(&self, mut cmd: Command, stdin: &[u8]) -> Result<std::process::Output, ApiError> {
        use tokio::io::AsyncWriteExt;
        let mut child = cmd.spawn().map_err(internal)?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(stdin).await.map_err(internal)?;
            pipe.shutdown().await.map_err(internal)?;
        }
        child.wait_with_output().await.map_err(internal)
    }

    async fn text(&self, args: &[&str]) -> Result<String, ApiError> {
        let out = self.run(self.command(args), &[]).await?;
        if !out.status.success() {
            return Err(internal(format!(
                "git {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// `merge-tree --write-tree`: `Ok(tree)` or `Err(conflicted paths)`. `merge_base` overrides
    /// the common ancestor (a rebase replays one commit at a time that way).
    async fn merge_tree(
        &self,
        ours: &str,
        theirs: &str,
        merge_base: Option<&str>,
    ) -> Result<Result<String, Vec<String>>, ApiError> {
        let base_arg = merge_base.map(|b| format!("--merge-base={b}"));
        let mut args = vec!["merge-tree", "--write-tree", "--name-only", "--no-messages"];
        if let Some(b) = &base_arg {
            args.push(b);
        }
        args.extend([ours, theirs]);
        let out = self.run(self.command(&args), &[]).await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        match out.status.code() {
            Some(0) => Ok(Ok(tree)),
            Some(1) => Ok(Err(lines
                .take_while(|l| !l.is_empty())
                .map(str::to_string)
                .collect())),
            _ => Err(internal(format!(
                "merge-tree: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    async fn commit_tree(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
        author: (&str, &str),
        committer: (&str, &str),
    ) -> Result<String, ApiError> {
        let mut args = vec!["commit-tree", tree];
        for p in parents {
            args.extend(["-p", p]);
        }
        args.extend(["-m", message]);
        let mut cmd = self.command(&args);
        cmd.env("GIT_AUTHOR_NAME", author.0)
            .env("GIT_AUTHOR_EMAIL", author.1)
            .env("GIT_COMMITTER_NAME", committer.0)
            .env("GIT_COMMITTER_EMAIL", committer.1);
        let out = self.run(cmd, &[]).await?;
        if !out.status.success() {
            return Err(internal(String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Replays `base..head` onto `base` one commit at a time (each step merges the commit
    /// against the growing tip with its parent as the base — what `git rebase` does), keeping
    /// each commit's author and message. `Err(paths)` on the first conflict.
    async fn rebase(
        &self,
        base: &str,
        head: &str,
        committer: (&str, &str),
    ) -> Result<Result<String, Vec<String>>, ApiError> {
        let range = format!("{base}..{head}");
        let list = self
            .text(&["rev-list", "--reverse", "--no-merges", &range])
            .await?;
        let mut onto = base.to_string();
        for sha in list.lines() {
            let parent = format!("{sha}^");
            let tree = match self.merge_tree(&onto, sha, Some(&parent)).await? {
                Ok(tree) => tree,
                Err(paths) => return Ok(Err(paths)),
            };
            let info = self
                .text(&["log", "-1", "--format=%an%x00%ae%x00%B", sha])
                .await?;
            let mut f = info.splitn(3, '\0');
            let (name, email, msg) = (
                f.next().unwrap_or_default(),
                f.next().unwrap_or_default(),
                f.next().unwrap_or_default(),
            );
            onto = self
                .commit_tree(&tree, &[&onto], msg, (name, email), committer)
                .await?;
        }
        Ok(Ok(onto))
    }

    /// A self-contained pack of everything `commit` adds over `haves`.
    async fn pack(&self, commit: &str, haves: &[&str]) -> Result<Vec<u8>, ApiError> {
        let mut revs = format!("{commit}\n");
        for have in haves {
            revs.push('^');
            revs.push_str(have);
            revs.push('\n');
        }
        let out = self
            .run(
                self.command(&[
                    "pack-objects",
                    "--revs",
                    "--stdout",
                    "--delta-base-offset",
                    "-q",
                ]),
                revs.as_bytes(),
            )
            .await?;
        if !out.status.success() {
            return Err(internal(String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(out.stdout)
    }
}

/// Issue numbers a pull request body closes: GitHub's keywords (`close[sd]`, `fix(e[sd])`,
/// `resolve[sd]`) followed by `#n`.
fn closes(body: &str) -> Vec<u64> {
    const KEYWORDS: [&str; 9] = [
        "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
    ];
    let words: Vec<&str> = body.split_whitespace().collect();
    let mut out: Vec<u64> = words
        .windows(2)
        .filter(|w| KEYWORDS.contains(&w[0].trim_end_matches(':').to_ascii_lowercase().as_str()))
        .filter_map(|w| {
            let n = w[1].strip_prefix('#')?;
            n.trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// An email for a principal that is not one (static tokens are names).
fn email_of(name: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{name}@users.walgit.invalid")
    }
}

/// The open pull request and its live tips, from a synced handle.
fn open_pull(
    handle: &walgit_wal::RepoHandle,
    number: u64,
) -> Result<(Item, String, String), ApiError> {
    let item = handle
        .forge_read(|s| s.items.get(&number).filter(|i| i.kind == "pull").cloned())
        .ok_or_else(|| ApiError::NotFound(format!("pull #{number}")))?;
    if item.state != "open" {
        return Err(ApiError::BadRequest(format!("#{number} is not open")));
    }
    let (base, head) = tips(handle, &item);
    Ok((item, base, head))
}

#[derive(Serialize)]
struct Mergeability {
    /// `head` merges into `base` without conflicts.
    mergeable: bool,
    /// Its commits also replay onto `base` one by one ("Rebase and merge").
    rebaseable: bool,
    /// The base already contains the head: nothing to merge.
    up_to_date: bool,
    conflicts: Vec<String>,
    base_sha: String,
    head_sha: String,
}

/// `GET …/pulls/{n}/merge`: can it merge cleanly, and if not, where it conflicts.
pub(super) async fn mergeability(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath((owner, repo, number)): UrlPath<(String, String, u64)>,
) -> Result<Response, ApiError> {
    let (handle, _) = open(&st, &headers, &owner, &repo, Role::Read).await?;
    let (guard, access) = handle.sync_objects().await.map_err(crate::smart::wal_err)?;
    if access.is_remote() {
        return Err(ApiError::ServiceUnavailable(
            "merging needs this repository's packs on this host".into(),
        ));
    }
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    let (_, base, head) = open_pull(&handle, number)?;
    let local = handle.local().clone();
    let up_to_date = local.is_ancestor(&head, &base).await.map_err(internal)?;
    let (conflicts, rebaseable) = if up_to_date {
        (Vec::new(), false)
    } else {
        let scratch = Scratch::new(local.path()).await?;
        let conflicts = scratch
            .merge_tree(&base, &head, None)
            .await?
            .err()
            .unwrap_or_default();
        // A clean merge can still fail to rebase: replayed one by one, a commit may conflict
        // that the branch's own merge of the base resolved (GitHub's `rebaseable`).
        let rebaseable = conflicts.is_empty()
            && scratch
                .rebase(&base, &head, ("walgit", "walgit@users.walgit.invalid"))
                .await?
                .is_ok();
        (conflicts, rebaseable)
    };
    drop(guard);
    let m = Mergeability {
        mergeable: !up_to_date && conflicts.is_empty(),
        rebaseable,
        up_to_date,
        conflicts,
        base_sha: base,
        head_sha: head,
    };
    Ok(json_swr(&m, None).into_response(&headers))
}

#[derive(Deserialize)]
pub(super) struct MergeRequest {
    /// `merge` (default) | `squash` | `rebase`
    #[serde(default)]
    method: Option<String>,
    /// The head the client looked at; a head that moved since is a 409.
    sha: Option<String>,
    /// Merge and squash commit message (first line = title); GitHub's default when absent.
    message: Option<String>,
}

/// `POST …/pulls/{n}/merge`: build the merge in a scratch object directory, then publish the
/// base branch move, the PR's merge and the issues it closes as one entry.
pub(super) async fn merge(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath((owner, repo, number)): UrlPath<(String, String, u64)>,
    Json(req): Json<MergeRequest>,
) -> Result<Response, ApiError> {
    let method = req.method.unwrap_or_else(|| "merge".into());
    if !matches!(method.as_str(), "merge" | "squash" | "rebase") {
        return Err(ApiError::BadRequest(format!(
            "{method:?} is not merge, squash or rebase"
        )));
    }
    let (handle, grant) = open(&st, &headers, &owner, &repo, Role::Write).await?;
    let (guard, access) = handle.sync_objects().await.map_err(crate::smart::wal_err)?;
    if access.is_remote() {
        return Err(ApiError::ServiceUnavailable(
            "merging needs this repository's packs on this host".into(),
        ));
    }
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    let (item, base, head) = open_pull(&handle, number)?;
    // The publisher refuses a draft too; saying so first spares building the merge.
    if item.draft {
        return Err(ApiError::BadRequest(format!(
            "#{number} is a draft; mark it ready for review first"
        )));
    }
    if req.sha.as_ref().is_some_and(|sha| *sha != head) {
        return Err(ApiError::Conflict(
            "Head branch was modified. Review and try the merge again.".into(),
        ));
    }
    let local = handle.local().clone();
    if local.is_ancestor(&head, &base).await.map_err(internal)? {
        return Err(ApiError::BadRequest(format!(
            "{} already contains {}: nothing to merge",
            item.base_ref, item.head_ref
        )));
    }

    let subject = &grant.subject;
    let email = if subject.email.is_empty() {
        email_of(&subject.name)
    } else {
        subject.email.clone()
    };
    let me = (subject.name.as_str(), email);
    let me = (me.0, me.1.as_str());
    let message = req.message.unwrap_or_else(|| match method.as_str() {
        "squash" => format!("{} (#{number})\n\n{}", item.title, item.body),
        _ => format!(
            "Merge pull request #{number} from {}\n\n{}",
            item.head_ref, item.title
        ),
    });
    let scratch = Scratch::new(local.path()).await?;
    let built = match method.as_str() {
        "rebase" => scratch.rebase(&base, &head, me).await?,
        _ => match scratch.merge_tree(&base, &head, None).await? {
            Ok(tree) => {
                let parents: &[&str] = if method == "squash" {
                    &[&base]
                } else {
                    &[&base, &head]
                };
                Ok(scratch
                    .commit_tree(&tree, parents, message.trim(), me, me)
                    .await?)
            }
            Err(paths) => Err(paths),
        },
    };
    let built = built.map_err(|paths| {
        ApiError::Conflict(format!(
            "This branch has conflicts that must be resolved: {}",
            paths.join(", ")
        ))
    })?;

    let pack = scratch.pack(&built, &[&base, &head]).await?;
    let ingested = local
        .ingest_pack(
            std::io::Cursor::new(pack),
            walgit_git::IngestOptions {
                fsck: st.cfg.wal.fsck_objects,
                max_bytes: None,
                thin: false,
            },
        )
        .await
        .map_err(|e| internal(format!("index the merge: {e}")))?;
    let tip = gix_hash::ObjectId::from_hex(built.as_bytes()).map_err(internal)?;
    local
        .check_connectivity_async(&[tip], true)
        .await
        .map_err(|e| internal(format!("connectivity: {e}")))?;
    drop(guard);

    // The merge is a push to the base branch: the push policy decides as for any push.
    let mut txn = RefTransaction {
        updates: vec![RefUpdate {
            name: format!("refs/heads/{}", item.base_ref),
            old_oid: base.clone(),
            new_oid: built.clone(),
            ..Default::default()
        }],
        atomic: true,
        ..Default::default()
    };
    local.fill_peeled(&mut txn);
    let policy = crate::policy::load(&st.store, handle.id())
        .await
        .map_err(|e| internal(format!("load policy: {e}")))?;
    let ev = crate::policy::evaluate(&policy, &subject.names(), &txn, |_| false);
    if let Some((_, Err(why))) = ev.per_ref.iter().find(|(_, r)| r.is_err()) {
        return Err(ApiError::BadRequest(format!("push policy: {why}")));
    }

    let mut events = vec![event(
        number,
        Body::Merged(ForgeMerged {
            method,
            merge_commit_oid: built,
            base_oid: base,
            head_oid: head,
        }),
    )];
    let open_issues: Vec<u64> = handle.forge_read(|s| {
        closes(&item.body)
            .into_iter()
            .filter(|k| {
                s.items
                    .get(k)
                    .is_some_and(|i| i.kind == "issue" && i.state == "open")
            })
            .collect()
    });
    events.extend(
        open_issues
            .into_iter()
            .map(|k| closing_event(k, "completed")),
    );
    let res = handle
        .publish_forge_push(grant.actor(), events, ingested, txn)
        .await
        .map_err(crate::smart::wal_err)?;
    if let Some(msg) = res.forge_error {
        return Err(ApiError::BadRequest(msg));
    }
    if res.per_ref.iter().any(|(_, r)| r.is_err()) {
        return Err(ApiError::Conflict(
            "Base branch was modified. Review and try the merge again.".into(),
        ));
    }
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    Ok(Json(detail_of(&handle, number, true)?).into_response())
}

/// `DELETE …/pulls/{n}/branch`: GitHub's "Delete branch" once a pull request is closed.
pub(super) async fn delete_branch(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath((owner, repo, number)): UrlPath<(String, String, u64)>,
) -> Result<Response, ApiError> {
    let (handle, grant) = open(&st, &headers, &owner, &repo, Role::Write).await?;
    handle.forge_ensure().await.map_err(crate::smart::wal_err)?;
    let item = handle
        .forge_read(|s| s.items.get(&number).filter(|i| i.kind == "pull").cloned())
        .ok_or_else(|| ApiError::NotFound(format!("pull #{number}")))?;
    if item.state == "open" {
        return Err(ApiError::BadRequest(
            "close or merge the pull request first".into(),
        ));
    }
    let name = format!("refs/heads/{}", item.head_ref);
    let old = super::forge::branch_tip(&handle, &item.head_ref)
        .ok_or_else(|| ApiError::NotFound(format!("branch {}", item.head_ref)))?;
    let txn = RefTransaction {
        updates: vec![RefUpdate {
            name: name.clone(),
            old_oid: old,
            new_oid: String::new(),
            ..Default::default()
        }],
        atomic: true,
        ..Default::default()
    };
    let policy = crate::policy::load(&st.store, handle.id())
        .await
        .map_err(|e| internal(format!("load policy: {e}")))?;
    let ev = crate::policy::evaluate(&policy, &grant.subject.names(), &txn, |_| false);
    if let Some((_, Err(why))) = ev.per_ref.iter().find(|(_, r)| r.is_err()) {
        return Err(ApiError::BadRequest(format!("push policy: {why}")));
    }
    let meta =
        std::collections::HashMap::from([("principal".to_string(), grant.subject.name.clone())]);
    let res = handle
        .publish_ref_update(txn, meta)
        .await
        .map_err(crate::smart::wal_err)?;
    if res.per_ref.iter().any(|(_, r)| r.is_err()) {
        return Err(ApiError::Conflict(format!(
            "{name} moved; reload and try again"
        )));
    }
    Ok(Json(detail_of(&handle, number, true)?).into_response())
}

#[cfg(test)]
mod tests {
    use super::closes;

    #[test]
    fn closing_keywords() {
        assert_eq!(
            closes("Fixes #12, closes #3.\nResolved: #12 and see #7"),
            [3, 12]
        );
        assert!(closes("fixes 12, prefix#4").is_empty());
    }
}

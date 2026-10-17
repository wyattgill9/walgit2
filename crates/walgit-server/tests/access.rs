// Test fixtures use panics to fail the test, including shared helper functions.
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]

//! D51: one permission model on every path (`docs/ACCOUNTS.md`). A token-mode host with
//! anonymous read; alice owns `alice/secret` (private) and `alice/open` (public); bob has
//! write, carol read, tina triage and mona maintain on both; dave has no role; root is a site
//! admin.

mod harness;

use std::path::Path;
use std::process::Command;

use harness::{Server, TestRepo, git_in};
use reqwest::{Method as M, StatusCode};
use serde_json::{Value, json};
use sha2::Digest;

type TestResult = anyhow::Result<()>;

const USERS: [&str; 7] = ["alice", "bob", "carol", "tina", "mona", "dave", "root"];

fn tokens() -> Vec<walgit_config::StaticToken> {
    USERS
        .iter()
        .map(|u| walgit_config::StaticToken {
            user: (*u).into(),
            token: format!("{u}-token"),
            token_env: None,
            // Everyone may create under their own login, except dave.
            write: *u != "dave",
            admin: *u == "root",
        })
        .collect()
}

fn token_mode(c: &mut walgit_config::Config) {
    c.server.auth.mode = walgit_config::AuthMode::Token;
    c.server.auth.anonymous_read = true;
    c.server.auth.tokens = tokens();
}

/// `(status, body)` of one request as `who` (`None` = anonymous).
async fn call(
    server: &Server,
    method: M,
    path: &str,
    who: Option<&str>,
    body: Option<Value>,
) -> anyhow::Result<(StatusCode, String)> {
    let mut req = reqwest::Client::new()
        .request(method, format!("{}{path}", server.base_url))
        .header("Accept", "application/json");
    if let Some(who) = who {
        req = req.bearer_auth(format!("{who}-token"));
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await?;
    Ok((resp.status(), resp.text().await?))
}

async fn status(server: &Server, method: M, path: &str, who: Option<&str>) -> StatusCode {
    call(server, method, path, who, None).await.unwrap().0
}

/// `git` with `who`'s token and no prompt; `(succeeded, stderr)`.
fn git_as(dir: &Path, who: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "credential.helper=",
            "-c",
            &format!("http.extraHeader=Authorization: Bearer {who}-token"),
        ])
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

struct World {
    server: Server,
    source: TestRepo,
    head: String,
    file: String,
    lfs_oid: String,
    pack: String,
}

async fn put_access(server: &Server, repo: &str, visibility: &str) -> TestResult {
    let collaborators: Vec<Value> = [
        ("bob", "write"),
        ("carol", "read"),
        ("tina", "triage"),
        ("mona", "maintain"),
    ]
    .iter()
    .map(|(l, r)| json!({"login": l, "role": r}))
    .collect();
    let (st, text) = call(
        server,
        M::PUT,
        &format!("/{repo}/api/access"),
        Some("alice"),
        Some(json!({"visibility": visibility, "collaborators": collaborators})),
    )
    .await?;
    anyhow::ensure!(st == 200, "access {repo}: {st} {text}");
    Ok(())
}

async fn world() -> anyhow::Result<World> {
    world_with(token_mode).await
}

async fn world_with(tweak: impl FnOnce(&mut walgit_config::Config)) -> anyhow::Result<World> {
    let server = Server::start_with_tweak(tweak).await?;
    server.create_users(&USERS).await?;
    for (repo, vis) in [("secret", "private"), ("open", "public")] {
        let (st, text) = call(
            &server,
            M::PUT,
            &format!("/alice/{repo}?visibility={vis}"),
            Some("alice"),
            None,
        )
        .await?;
        anyhow::ensure!(st == 201, "create {repo}: {st} {text}");
    }
    let source = TestRepo::synthetic(2, 1)?;
    for repo in ["secret", "open"] {
        let (ok, err) = git_as(
            &source.dir,
            "alice",
            &[
                "push",
                "-q",
                &server.repo_url("alice", repo),
                "HEAD:refs/heads/main",
            ],
        );
        anyhow::ensure!(ok, "alice pushes {repo}: {err}");
        put_access(
            &server,
            &format!("alice/{repo}"),
            if repo == "secret" {
                "private"
            } else {
                "public"
            },
        )
        .await?;
        let (st, text) = call(
            &server,
            M::POST,
            &format!("/alice/{repo}/api/issues"),
            Some("alice"),
            Some(json!({"title": "tracked"})),
        )
        .await?;
        anyhow::ensure!(st.is_success(), "issue {repo}: {st} {text}");
    }
    let head = git_in(&source.dir, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let file = git_in(&source.dir, &["ls-tree", "--name-only", "HEAD"])?
        .lines()
        .next()
        .unwrap()
        .to_string();
    let body = b"lfs bytes".to_vec();
    let lfs_oid = hex::encode(sha2::Sha256::digest(&body));
    for repo in ["secret", "open"] {
        let r = reqwest::Client::new()
            .put(format!(
                "{}/alice/{repo}.git/info/lfs/objects/{lfs_oid}",
                server.base_url
            ))
            .bearer_auth("alice-token")
            .body(body.clone())
            .send()
            .await?;
        anyhow::ensure!(r.status().is_success(), "lfs put: {}", r.status());
    }
    // A topic branch and pull request #2 on the public repository.
    git_in(
        &source.dir,
        &["commit", "-q", "--allow-empty", "-m", "topic"],
    )?;
    let (ok, err) = git_as(
        &source.dir,
        "alice",
        &[
            "push",
            "-q",
            &server.repo_url("alice", "open"),
            "HEAD:refs/heads/topic",
        ],
    );
    anyhow::ensure!(ok, "topic: {err}");
    git_in(&source.dir, &["reset", "-q", "--hard", "HEAD~1"])?;
    let (st, text) = call(
        &server,
        M::POST,
        "/alice/open/api/pulls",
        Some("alice"),
        Some(json!({"title": "topic", "base": "main", "head": "topic"})),
    )
    .await?;
    anyhow::ensure!(st.is_success(), "pull: {st} {text}");
    let id = walgit_git::RepoId::new("alice", "secret")?;
    let pack = server.state.registry.open(&id).await?.manifest().packs[0]
        .checksum
        .clone();
    Ok(World {
        server,
        source,
        head,
        file,
        lfs_oid,
        pack,
    })
}

/// Every read route of a repository.
fn read_routes(w: &World, repo: &str) -> Vec<(M, String)> {
    let mut v = Vec::new();
    for lane in ["api", "api-browser"] {
        for sub in [
            String::new(),
            "/refs".into(),
            "/refs/branches".into(),
            "/branches".into(),
            "/resolve/main".into(),
            "/tree/main".into(),
            format!("/blob/main/{}", w.file),
            format!("/blob/{}/{}", w.head, w.file),
            "/commits".into(),
            format!("/commit/{}", w.head),
            "/compare/main...main".into(),
            "/overview".into(),
            "/ops".into(),
            "/tasks".into(),
            "/settings".into(),
            "/settings/effective".into(),
            "/settings/history".into(),
            "/settings/describe".into(),
            "/policy".into(),
            "/issues".into(),
            "/issues/1".into(),
            "/pulls".into(),
            "/labels".into(),
            "/forge/summary".into(),
        ] {
            v.push((M::GET, format!("/{repo}/{lane}{sub}")));
        }
    }
    for suffix in [".git", ""] {
        v.push((
            M::GET,
            format!("/{repo}{suffix}/info/refs?service=git-upload-pack"),
        ));
        v.push((
            M::GET,
            format!("/{repo}{suffix}/info/lfs/objects/{}", w.lfs_oid),
        ));
        v.push((
            M::HEAD,
            format!("/{repo}{suffix}/info/lfs/objects/{}", w.lfs_oid),
        ));
        v.push((M::GET, format!("/{repo}{suffix}/packfiles/{}.pack", w.pack)));
    }
    v
}

/// Private means private: on every read route, anonymous gets 401 and dave 404 — exactly
/// what a repository that doesn't exist answers — and everyone with a role reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_private_repository_is_private_on_every_route() -> TestResult {
    let w = world().await?;
    let s = &w.server;
    for (method, path) in read_routes(&w, "alice/secret") {
        let alice = status(s, method.clone(), &path, Some("alice")).await;
        assert!(
            alice.is_success(),
            "{method} {path}: the owner reads ({alice})"
        );
        assert_eq!(
            status(s, method.clone(), &path, None).await,
            StatusCode::UNAUTHORIZED,
            "{method} {path}: anonymous"
        );
        assert_eq!(
            status(s, method.clone(), &path, Some("dave")).await,
            StatusCode::NOT_FOUND,
            "{method} {path}: dave"
        );
        for who in ["bob", "carol", "tina", "mona", "root"] {
            assert_eq!(
                status(s, method.clone(), &path, Some(who)).await,
                alice,
                "{method} {path}: {who}"
            );
        }
        // Indistinguishable from a repository that doesn't exist.
        let missing = path.replacen("alice/secret", "alice/nothing", 1);
        assert_eq!(
            status(s, method.clone(), &missing, None).await,
            StatusCode::UNAUTHORIZED,
            "{method} {missing}"
        );
        assert_eq!(
            status(s, method.clone(), &missing, Some("dave")).await,
            StatusCode::NOT_FOUND,
            "{method} {missing}"
        );
    }
    // The public twin reads for everyone, anonymous included.
    for (method, path) in read_routes(&w, "alice/open") {
        for who in [None, Some("dave")] {
            assert!(
                status(s, method.clone(), &path, who).await.is_success(),
                "{method} {path}: {who:?}"
            );
        }
    }
    Ok(())
}

/// An immutable answer cached by an earlier reader is never served to someone who may not
/// read it; listings show only what the caller may read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caches_and_listings_do_not_leak() -> TestResult {
    let w = world().await?;
    let s = &w.server;
    let blob = format!("/alice/secret/api/blob/{}/{}", w.head, w.file);
    for _ in 0..2 {
        assert_eq!(status(s, M::GET, &blob, Some("alice")).await, 200);
    }
    assert_eq!(status(s, M::GET, &blob, Some("dave")).await, 404);
    assert_eq!(status(s, M::GET, &blob, None).await, 401);
    // Private bytes are never cacheable by a shared cache.
    let r = reqwest::Client::new()
        .get(format!(
            "{}/alice/secret.git/info/lfs/objects/{}",
            s.base_url, w.lfs_oid
        ))
        .bearer_auth("alice-token")
        .send()
        .await?;
    assert!(
        r.headers()["cache-control"]
            .to_str()?
            .starts_with("private"),
        "{:?}",
        r.headers()["cache-control"]
    );

    let repos = |who: Option<&'static str>| async move {
        let (st, text) = call(s, M::GET, "/api/v1/owners/alice/repos", who, None)
            .await
            .unwrap();
        assert_eq!(st, 200);
        serde_json::from_str::<Value>(&text).unwrap()
    };
    assert_eq!(repos(None).await, json!(["open"]));
    assert_eq!(repos(Some("dave")).await, json!(["open"]));
    assert_eq!(repos(Some("carol")).await, json!(["open", "secret"]));
    assert_eq!(repos(Some("root")).await, json!(["open", "secret"]));
    // The repository summary says what the caller may do.
    let (_, text) = call(s, M::GET, "/alice/secret/api", Some("tina"), None).await?;
    let summary: Value = serde_json::from_str(&text)?;
    assert_eq!(
        (summary["visibility"].as_str(), summary["role"].as_str()),
        (Some("private"), Some("triage"))
    );
    Ok(())
}

/// Each write needs its role, whatever the route.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_need_their_roles() -> TestResult {
    let w = world().await?;
    let s = &w.server;
    // Push.
    let (ok, _) = git_as(
        &w.source.dir,
        "bob",
        &[
            "push",
            "-q",
            &s.repo_url("alice", "secret"),
            "HEAD:refs/heads/bob",
        ],
    );
    assert!(ok, "bob (write) pushes");
    for who in ["carol", "tina"] {
        let (ok, err) = git_as(
            &w.source.dir,
            who,
            &[
                "push",
                "-q",
                &s.repo_url("alice", "secret"),
                &format!("HEAD:refs/heads/{who}"),
            ],
        );
        assert!(!ok && err.contains("needs write access"), "{who}: {err}");
    }
    let (ok, err) = git_as(
        &w.source.dir,
        "dave",
        &[
            "push",
            "-q",
            &s.repo_url("alice", "secret"),
            "HEAD:refs/heads/dave",
        ],
    );
    assert!(
        !ok && err.to_lowercase().contains("not found"),
        "dave: {err}"
    );
    // Clone.
    let tmp = tempfile::tempdir()?;
    let (ok, err) = git_as(
        tmp.path(),
        "carol",
        &["clone", "-q", &s.repo_url("alice", "secret"), "c"],
    );
    assert!(ok, "carol (read) clones: {err}");
    let (ok, err) = git_as(
        tmp.path(),
        "dave",
        &["clone", "-q", &s.repo_url("alice", "secret"), "d"],
    );
    assert!(
        !ok && err.to_lowercase().contains("not found"),
        "dave: {err}"
    );

    // LFS upload.
    let body = b"more".to_vec();
    let oid = hex::encode(sha2::Sha256::digest(&body));
    for (who, want) in [("carol", 403), ("bob", 200)] {
        let r = reqwest::Client::new()
            .put(format!(
                "{}/alice/secret.git/info/lfs/objects/{oid}",
                s.base_url
            ))
            .bearer_auth(format!("{who}-token"))
            .body(body.clone())
            .send()
            .await?;
        assert_eq!(r.status().as_u16(), want, "lfs put as {who}");
    }

    // Settings need Maintain, policy and access Admin, maintenance ops Maintain.
    let settings = "[packs]\nfold_when_fresh_packs_reach = 3\n";
    for (who, want) in [("bob", 403), ("mona", 200)] {
        let r = reqwest::Client::new()
            .put(format!("{}/alice/secret/api/settings", s.base_url))
            .bearer_auth(format!("{who}-token"))
            .body(settings)
            .send()
            .await?;
        assert_eq!(r.status().as_u16(), want, "settings as {who}");
    }
    let policy = r#"{"version":1,"groups":[],"rules":[]}"#;
    for (who, want) in [("mona", 403), ("alice", 204)] {
        let r = reqwest::Client::new()
            .put(format!("{}/alice/secret/api/policy", s.base_url))
            .bearer_auth(format!("{who}-token"))
            .body(policy)
            .send()
            .await?;
        assert_eq!(r.status().as_u16(), want, "policy as {who}");
    }
    let (st, _) = call(
        s,
        M::PUT,
        "/alice/secret/api/access",
        Some("mona"),
        Some(json!({"visibility": "public"})),
    )
    .await?;
    assert_eq!(st, 403, "access needs Admin");
    assert_eq!(
        status(s, M::GET, "/alice/secret/api/access", Some("carol")).await,
        403
    );
    assert_eq!(
        status(s, M::GET, "/alice/secret/api/access", Some("bob")).await,
        200
    );
    assert_eq!(
        status(s, M::POST, "/alice/secret/api/ops/checkpoint", Some("bob")).await,
        403
    );
    assert_eq!(
        status(s, M::DELETE, "/alice/secret", Some("mona")).await,
        403
    );
    assert_eq!(
        status(s, M::DELETE, "/alice/secret/api", Some("bob")).await,
        403
    );

    // The forge at its boundaries.
    let labels = json!({"add_labels": ["bug"]});
    let (st, _) = call(
        s,
        M::PATCH,
        "/alice/secret/api/issues/1",
        Some("carol"),
        Some(labels.clone()),
    )
    .await?;
    assert_eq!(st, 400, "labels need Triage");
    let (st, text) = call(
        s,
        M::PATCH,
        "/alice/secret/api/issues/1",
        Some("tina"),
        Some(labels),
    )
    .await?;
    assert!(st.is_success(), "tina (triage) labels: {st} {text}");
    // Assignees are accounts.
    let (st, _) = call(
        s,
        M::PATCH,
        "/alice/secret/api/issues/1",
        Some("tina"),
        Some(json!({"add_assignees": ["ghost"]})),
    )
    .await?;
    assert_eq!(st, 400, "an unknown assignee");
    let (st, _) = call(
        s,
        M::PATCH,
        "/alice/secret/api/issues/1",
        Some("tina"),
        Some(json!({"add_assignees": ["bob"]})),
    )
    .await?;
    assert!(st.is_success(), "bob is assignable");
    let (st, _) = call(
        s,
        M::POST,
        "/alice/secret/api/issues/1/comments",
        Some("carol"),
        Some(json!({"body": "+1"})),
    )
    .await?;
    assert!(st.is_success(), "carol (read) comments");
    let (st, _) = call(
        s,
        M::POST,
        "/alice/secret/api/issues/1/comments",
        Some("dave"),
        Some(json!({"body": "hi"})),
    )
    .await?;
    assert_eq!(st, 404, "dave can't take part in a private repository");

    // Creating: under your own login (with host write), never in someone else's namespace.
    assert_eq!(status(s, M::PUT, "/bob/mine", Some("bob")).await, 201);
    assert_eq!(status(s, M::PUT, "/alice/bobs", Some("bob")).await, 403);
    assert_eq!(status(s, M::PUT, "/dave/mine", Some("dave")).await, 403);
    assert_eq!(status(s, M::PUT, "/elsewhere/x", Some("bob")).await, 403);
    let (ok, err) = git_as(
        &w.source.dir,
        "bob",
        &[
            "push",
            "-q",
            &s.repo_url("alice", "pushed"),
            "HEAD:refs/heads/main",
        ],
    );
    assert!(!ok, "auto-create on push follows the same rule: {err}");
    Ok(())
}

/// Security 1: a namespace that holds repositories but no account is reserved; only a site
/// admin's org claims it, and its owners then administer its repositories.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reserved_namespaces_and_minimal_orgs() -> TestResult {
    use walgit_proto::v1::{AccountEvent, UserCreated, account_event::Body};
    let w = world().await?;
    let s = &w.server;
    assert_eq!(status(s, M::PUT, "/team/app", Some("root")).await, 201);
    let log = s.state.accounts.synced().await?;
    let signup = |login: &str| AccountEvent {
        body: Some(Body::UserCreated(UserCreated {
            user_id: format!("new-{login}"),
            login: login.into(),
            ..Default::default()
        })),
    };
    let r = log.publish_accounts(vec![signup("team")], "test").await;
    assert!(matches!(r, Err(walgit_wal::WalError::Invalid(_))), "{r:?}");
    // Only a site admin creates orgs; then alice owns `team`.
    let org = json!({"login": "team", "owners": ["alice"]});
    let (st, _) = call(s, M::POST, "/api/v1/orgs", Some("alice"), Some(org.clone())).await?;
    assert_eq!(st, 403);
    let (st, text) = call(s, M::POST, "/api/v1/orgs", Some("root"), Some(org)).await?;
    assert_eq!(st, 201, "{text}");
    assert_eq!(
        status(s, M::GET, "/team/app/api/access", Some("alice")).await,
        200
    );
    assert_eq!(
        status(s, M::GET, "/team/app/api/access", Some("bob")).await,
        403
    );
    assert_eq!(
        status(s, M::PUT, "/team/second", Some("alice")).await,
        201,
        "an org owner creates in it"
    );
    // Owners change: bob replaces alice; alice loses Admin at once.
    let (st, _) = call(
        s,
        M::PUT,
        "/api/v1/orgs/team/owners",
        Some("alice"),
        Some(json!({"owners": ["bob"]})),
    )
    .await?;
    assert_eq!(st, 204);
    assert_eq!(
        status(s, M::GET, "/team/app/api/access", Some("alice")).await,
        403
    );
    assert_eq!(
        status(s, M::GET, "/team/app/api/access", Some("bob")).await,
        200
    );
    Ok(())
}

/// Security 2: reading a public repository anonymously is not taking part.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_readers_never_write_the_forge() -> TestResult {
    let w = world().await?;
    let s = &w.server;
    let id = walgit_git::RepoId::new("alice", "open")?;
    let handle = s.state.registry.open(&id).await?;
    drop(handle.sync_refs().await?);
    let before = handle.manifest().head_seq;
    for lane in ["api", "api-browser"] {
        assert_eq!(
            status(s, M::GET, &format!("/alice/open/{lane}/issues/1"), None).await,
            200
        );
        for (method, path, body) in [
            (M::POST, "issues", json!({"title": "x"})),
            (M::POST, "issues/1/comments", json!({"body": "x"})),
            (M::PATCH, "issues/1", json!({"title": "y"})),
            (
                M::POST,
                "pulls",
                json!({"title": "x", "base": "main", "head": "topic"}),
            ),
            (
                M::POST,
                "pulls/2/reviews",
                json!({"state": "commented", "body": "x"}),
            ),
            (M::PUT, "labels/bug", json!({"color": "#ff0000"})),
            (M::POST, "pulls/2/merge", json!({})),
            (M::DELETE, "pulls/2/branch", json!({})),
        ] {
            let (st, text) = call(
                s,
                method.clone(),
                &format!("/alice/open/{lane}/{path}"),
                None,
                Some(body),
            )
            .await?;
            assert_eq!(st, 401, "anonymous {method} {lane}/{path}: {text}");
        }
    }
    drop(handle.sync_refs().await?);
    assert_eq!(handle.manifest().head_seq, before, "nothing was published");
    // dave (an account, no role) takes part in a public repository, but doesn't triage.
    let (st, _) = call(
        s,
        M::POST,
        "/alice/open/api/issues",
        Some("dave"),
        Some(json!({"title": "from dave"})),
    )
    .await?;
    assert!(st.is_success());
    let (st, _) = call(
        s,
        M::POST,
        "/alice/open/api/issues/1/comments",
        Some("dave"),
        Some(json!({"body": "+1"})),
    )
    .await?;
    assert!(st.is_success());
    let (st, _) = call(
        s,
        M::PATCH,
        "/alice/open/api/issues/1",
        Some("dave"),
        Some(json!({"add_labels": ["bug"]})),
    )
    .await?;
    assert_eq!(st, 400);
    Ok(())
}

/// Security 3: `mode = none` (everyone a site admin) answers only direct local requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_none_serves_only_direct_local_requests() -> TestResult {
    let server = Server::start().await?;
    let get = |headers: &'static [(&'static str, &'static str)]| {
        let mut r = reqwest::Client::new().get(format!("{}/api/v1/me", server.base_url));
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        async move { r.send().await.unwrap().status().as_u16() }
    };
    assert_eq!(get(&[]).await, 200);
    assert_eq!(get(&[("Host", "walgit.localhost:8080")]).await, 200);
    for headers in [
        &[("Host", "evil.example")][..],
        &[("X-Forwarded-For", "1.2.3.4")],
        &[("Forwarded", "for=1.2.3.4")],
        &[("X-Real-IP", "1.2.3.4")],
        &[("X-Walgit-Principal", "someone")],
        &[("X-Walgit-Capabilities", "accel-redirect")],
    ] {
        assert_eq!(get(headers).await, 403, "{headers:?}");
    }
    let (st, text) = call(&server, M::GET, "/api/v1/me", None, None).await?;
    assert_eq!(st, 200);
    let me: Value = serde_json::from_str(&text)?;
    assert_eq!(
        (me["auth_mode"].as_str(), me["site_admin"].as_bool()),
        (Some("none"), Some(true))
    );
    Ok(())
}

/// Security 4 and principle IV: access changed on one instance applies to the next request
/// on every other one; a write judged at its CAS basis is covered by the WAL tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_applies_to_the_next_request_everywhere() -> TestResult {
    let w = world().await?;
    let a = &w.server;
    let b = a.start_sibling_with(token_mode).await?;
    let tmp = tempfile::tempdir()?;
    let (ok, err) = git_as(
        tmp.path(),
        "bob",
        &["clone", "-q", &a.repo_url("alice", "secret"), "c"],
    );
    assert!(ok, "{err}");
    let (ok, _) = git_as(
        &w.source.dir,
        "bob",
        &[
            "push",
            "-q",
            &a.repo_url("alice", "secret"),
            "HEAD:refs/heads/b1",
        ],
    );
    assert!(ok);
    // alice removes bob on instance b.
    let (st, text) = call(
        &b,
        M::PUT,
        "/alice/secret/api/access",
        Some("alice"),
        Some(json!({"visibility": "private", "collaborators": []})),
    )
    .await?;
    assert_eq!(st, 200, "{text}");
    // On instance a, bob's very next requests are refused.
    assert_eq!(
        status(a, M::GET, "/alice/secret/api/refs", Some("bob")).await,
        404
    );
    let (ok, err) = git_as(
        &w.source.dir,
        "bob",
        &[
            "push",
            "-q",
            &a.repo_url("alice", "secret"),
            "HEAD:refs/heads/b2",
        ],
    );
    assert!(!ok, "{err}");
    let (ok, _) = git_as(
        tmp.path().join("c").as_path(),
        "bob",
        &["fetch", "-q", "origin"],
    );
    assert!(!ok);

    // public → private: an anonymous reader holding an ETag is refused at once.
    let r = reqwest::Client::new()
        .get(format!("{}/alice/open/api/refs", a.base_url))
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let etag = r.headers()["etag"].to_str()?.to_string();
    put_access(&b, "alice/open", "private").await?;
    let r = reqwest::Client::new()
        .get(format!("{}/alice/open/api/refs", a.base_url))
        .header("If-None-Match", etag)
        .send()
        .await?;
    assert_eq!(r.status(), 401);
    Ok(())
}

// ---- D52: password accounts ----

const SECRET: &str = "0123456789abcdef0123456789abcdef-session-secret";

fn password_mode(mode: walgit_config::PasswordAccounts) -> impl FnOnce(&mut walgit_config::Config) {
    move |c| {
        token_mode(c);
        c.server.auth.passwords = mode;
        c.server.auth.session_secret = Some(SECRET.into());
    }
}

/// A browser: no redirects followed, the session cookie kept by hand.
async fn form(
    server: &Server,
    path: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
    site: &str,
) -> anyhow::Result<(StatusCode, Option<String>, String)> {
    let mut req = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .post(format!("{}{path}", server.base_url))
        .header("Sec-Fetch-Site", site)
        .form(fields);
    if let Some(c) = cookie {
        req = req.header("Cookie", c);
    }
    let r = req.send().await?;
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("walgit_session=") && !v.starts_with("walgit_session=;"))
        .map(|v| v.split(';').next().unwrap().to_string());
    Ok((r.status(), cookie, r.text().await?))
}

async fn me_with(server: &Server, cookie: &str) -> Value {
    let r = reqwest::Client::new()
        .get(format!("{}/api/v1/me", server.base_url))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    serde_json::from_str(&r.text().await.unwrap()).unwrap()
}

/// D52 with open sign-up: a browser creates an account, is signed in, creates a repository,
/// mints a git token; wrong passwords fail and lock out; names that are taken or reserved stay so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_accounts_sign_up_sign_in_and_lock_out() -> TestResult {
    let w = world_with(password_mode(walgit_config::PasswordAccounts::Open)).await?;
    let s = &w.server;
    let page = reqwest::Client::new()
        .get(format!("{}/_auth/login?next=/new", s.base_url))
        .send()
        .await?;
    assert_eq!(page.status(), 200);
    let html = page.text().await?;
    assert!(
        html.contains(r#"action="/_auth/login""#) && html.contains("/_auth/signup"),
        "{html}"
    );

    let signup = |login: &'static str, pw: &'static str, confirm: &'static str| async move {
        form(
            s,
            "/_auth/signup",
            &[
                ("login", login),
                ("password", pw),
                ("confirm", confirm),
                ("next", "/new"),
            ],
            None,
            "same-origin",
        )
        .await
        .unwrap()
    };
    // Taken, reserved, malformed, mismatched or short: refused, nothing signed in.
    assert_eq!(
        status(s, M::PUT, "/team/app", Some("root")).await,
        201,
        "reserves team"
    );
    for (login, pw, confirm, want) in [
        ("alice", "long enough pw", "long enough pw", 409),
        ("team", "long enough pw", "long enough pw", 409),
        ("Bad Name", "long enough pw", "long enough pw", 400),
        ("newbie", "long enough pw", "different pw!!", 400),
        ("newbie", "short", "short", 400),
    ] {
        let (st, cookie, text) = signup(login, pw, confirm).await;
        assert_eq!(
            (st.as_u16(), cookie.is_none()),
            (want, true),
            "{login}: {text}"
        );
    }
    // Another site can't post the form.
    let (st, _, _) = form(
        s,
        "/_auth/signup",
        &[
            ("login", "newbie"),
            ("password", "long enough pw"),
            ("confirm", "long enough pw"),
        ],
        None,
        "cross-site",
    )
    .await?;
    assert_eq!(st, 403);

    let (st, cookie, _) = signup("newbie", "long enough pw", "long enough pw").await;
    assert!(st.is_redirection(), "{st}");
    let cookie = cookie.expect("signed in");
    let me = me_with(s, &cookie).await;
    assert_eq!(
        (me["login"].as_str(), me["can_create"].as_bool()),
        (Some("newbie"), Some(true))
    );
    assert_eq!(me["sign_in"]["passwords"], "open");

    // The session works for the API: create a private repository, read it; others can't.
    let r = reqwest::Client::new()
        .put(format!("{}/newbie/project?visibility=private", s.base_url))
        .header("Cookie", &cookie)
        .send()
        .await?;
    assert_eq!(r.status(), 201);
    let r = reqwest::Client::new()
        .get(format!("{}/newbie/project/api", s.base_url))
        .header("Cookie", &cookie)
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    assert_eq!(
        status(s, M::GET, "/newbie/project/api", Some("dave")).await,
        404
    );

    // A git token from the session (token mode too, D52).
    let r = reqwest::Client::new()
        .post(format!("{}/_auth/tokens", s.base_url))
        .header("Cookie", &cookie)
        .header("Sec-Fetch-Site", "same-origin")
        .send()
        .await?;
    assert_eq!(r.status(), 200);
    let tok: Value = serde_json::from_str(&r.text().await?)?;
    let token = tok["token"].as_str().unwrap();
    let r = reqwest::Client::new()
        .get(format!("{}/newbie/project/api", s.base_url))
        .bearer_auth(token)
        .send()
        .await?;
    assert_eq!(r.status(), 200, "the minted token is newbie");

    // Changing the password: the current one first.
    let (st, _, _) = form(
        s,
        "/_auth/password",
        &[
            ("current", "wrong current"),
            ("password", "a new passphrase"),
        ],
        Some(&cookie),
        "same-origin",
    )
    .await?;
    assert_eq!(st, 401);
    let (st, _, text) = form(
        s,
        "/_auth/password",
        &[
            ("current", "long enough pw"),
            ("password", "a new passphrase"),
        ],
        Some(&cookie),
        "same-origin",
    )
    .await?;
    assert_eq!(st, 200, "{text}");
    // Signing in: a wrong password fails; ten failures lock even the right one out.
    let login = |pw: &'static str| async move {
        form(
            s,
            "/_auth/login",
            &[("login", "newbie"), ("password", pw)],
            None,
            "same-origin",
        )
        .await
        .unwrap()
    };
    let (st, cookie2, _) = login("a new passphrase").await;
    assert!(st.is_redirection() && cookie2.is_some());
    let (st, cookie2, _) = login("nope").await;
    assert_eq!((st.as_u16(), cookie2.is_none()), (401, true));
    let (st, _, _) = form(
        s,
        "/_auth/login",
        &[("login", "ghost"), ("password", "x")],
        None,
        "same-origin",
    )
    .await?;
    assert_eq!(st, 401, "an unknown user reads like a wrong password");
    for _ in 0..9 {
        login("nope").await;
    }
    let (st, cookie2, _) = login("a new passphrase").await;
    assert_eq!((st.as_u16(), cookie2.is_none()), (429, true), "locked out");

    Ok(())
}

/// D52 with `passwords = "admin"`: no sign-up; a site admin creates the account and sets the
/// password; the person signs in with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_created_password_accounts() -> TestResult {
    let w = world_with(password_mode(walgit_config::PasswordAccounts::Admin)).await?;
    let s = &w.server;
    assert_eq!(
        reqwest::Client::new()
            .get(format!("{}/_auth/signup", s.base_url))
            .send()
            .await?
            .status(),
        404
    );
    let user = json!({"login": "eve", "password": "eve's passphrase"});
    let (st, _) = call(
        s,
        M::POST,
        "/api/v1/users",
        Some("alice"),
        Some(user.clone()),
    )
    .await?;
    assert_eq!(st, 403, "only site admins create accounts");
    let (st, text) = call(s, M::POST, "/api/v1/users", Some("root"), Some(user)).await?;
    assert_eq!(st, 201, "{text}");
    let (st, cookie, _) = form(
        s,
        "/_auth/login",
        &[("login", "eve"), ("password", "eve's passphrase")],
        None,
        "same-origin",
    )
    .await?;
    assert!(st.is_redirection(), "{st}");
    assert_eq!(me_with(s, &cookie.unwrap()).await["login"], "eve");
    // A reset by the site admin replaces it.
    let (st, _) = call(
        s,
        M::PUT,
        "/api/v1/users/eve/password",
        Some("root"),
        Some(json!({"password": "reset passphrase"})),
    )
    .await?;
    assert_eq!(st, 204);
    let (st, _, _) = form(
        s,
        "/_auth/login",
        &[("login", "eve"), ("password", "eve's passphrase")],
        None,
        "same-origin",
    )
    .await?;
    assert_eq!(st, 401);
    let (st, _, _) = form(
        s,
        "/_auth/login",
        &[("login", "eve"), ("password", "reset passphrase")],
        None,
        "same-origin",
    )
    .await?;
    assert!(st.is_redirection());
    // The directory never shows a password.
    let (_, text) = call(s, M::GET, "/api/v1/users?q=eve", Some("alice"), None).await?;
    assert!(!text.contains("argon2"), "{text}");
    Ok(())
}

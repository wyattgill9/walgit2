// Test fixtures use panics to fail the test, including shared helper functions.
#![allow(clippy::indexing_slicing, clippy::string_slice, clippy::unwrap_used)]

//! web/API.md §6 conformance for the read-only JSON API.

mod harness;

use harness::{Server, git_in};
use serde_json::Value;

type TestResult = anyhow::Result<()>;

async fn get(
    server: &Server,
    path: &str,
) -> anyhow::Result<(reqwest::StatusCode, String, Option<String>)> {
    let resp = reqwest::Client::new()
        .get(format!("{}{path}", server.base_url))
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = resp.status();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);
    let text = resp.text().await?;
    Ok((status, text, ct))
}

async fn get_h(
    server: &Server,
    path: &str,
    extra: &[(&str, &str)],
) -> anyhow::Result<(reqwest::StatusCode, String, reqwest::header::HeaderMap)> {
    let mut req = reqwest::Client::new()
        .get(format!("{}{path}", server.base_url))
        .header("Accept", "application/json");
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = resp.text().await?;
    Ok((status, text, headers))
}
fn hdr(h: &reqwest::header::HeaderMap, k: &str) -> String {
    h.get(k)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn json(server: &Server, path: &str) -> anyhow::Result<Value> {
    let (status, text, ct) = get(server, path).await?;
    anyhow::ensure!(status.is_success(), "GET {path} -> {status}: {text}");
    anyhow::ensure!(
        ct.as_deref().unwrap_or("").starts_with("application/json"),
        "content-type {ct:?}"
    );
    Ok(serde_json::from_str(&text)?)
}

/// Build a source repo with the shapes the UI cares about and push it.
fn fixture(server: &Server) -> anyhow::Result<std::path::PathBuf> {
    let dir = tempfile::tempdir()?.keep(); // TODO(hermetic): keep TempDir in fixture
    git_in(&dir, &["init", "-q", "-b", "main"])?;
    git_in(&dir, &["config", "user.email", "t@t"])?;
    git_in(&dir, &["config", "user.name", "Tester"])?;
    std::fs::write(dir.join("README.md"), "# Title\n\nhello\n")?;
    std::fs::create_dir_all(dir.join("src/inner"))?;
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n")?;
    std::fs::write(dir.join("src/inner/x.txt"), "x\n")?;
    std::fs::write(dir.join("bin.dat"), [0u8, 159, 146, 150, 0, 1, 2])?;
    std::fs::write(dir.join("big.txt"), vec![b'a'; 2 * 1024 * 1024 + 1])?;
    git_in(&dir, &["add", "."])?;
    git_in(&dir, &["commit", "-q", "-m", "initial\n\nbody line"])?;
    // feature/x branch with a nested dir named after a path segment, plus rename.
    git_in(&dir, &["checkout", "-q", "-b", "feature/x"])?;
    std::fs::create_dir_all(dir.join("dir"))?;
    std::fs::write(dir.join("dir/f.txt"), "f\n")?;
    git_in(&dir, &["mv", "src/main.rs", "src/app.rs"])?;
    git_in(&dir, &["add", "."])?;
    git_in(&dir, &["commit", "-q", "-m", "feature work"])?;
    git_in(&dir, &["checkout", "-q", "main"])?;
    std::fs::write(dir.join("src/inner/x.txt"), "xx\n")?;
    git_in(&dir, &["commit", "-qam", "second on main"])?;
    git_in(
        &dir,
        &["merge", "-q", "--no-ff", "-m", "merge feature", "feature/x"],
    )?;
    git_in(&dir, &["tag", "-a", "v1.0", "-m", "release"])?;
    git_in(
        &dir,
        &[
            "-c",
            "tag.forceSignAnnotated=false",
            "-c",
            "tag.gpgsign=false",
            "tag",
            "light",
        ],
    )?;
    for _ in 0..40 {
        git_in(&dir, &["commit", "-q", "--allow-empty", "-m", "filler"])?;
    }
    git_in(
        &dir,
        &["push", "-q", "--mirror", &server.repo_url("o", "r")],
    )?;
    Ok(dir)
}

/// web/API.md §6 against one server (called for the local-packs instance and
/// for a sibling that serves the same repo remotely).
#[allow(clippy::many_single_char_names)]
async fn conformance(
    server: &Server,
    src: &std::path::Path,
    head: &str,
    feature: &str,
    v1_peeled: &str,
) -> TestResult {
    // owners
    assert_eq!(
        json(server, "/services/api/owners").await?,
        serde_json::json!(["o"])
    );
    assert_eq!(
        json(server, "/services/api/owners/o").await?,
        serde_json::json!(["r"])
    );

    // refs: O(1) head only, ETag + 304
    let (st, text, h) = get_h(server, "/o/r/api/refs", &[]).await?;
    assert_eq!(st, 200);
    let refs: Value = serde_json::from_str(&text)?;
    assert_eq!(refs["head"]["name"], "main");
    assert_eq!(refs["head"]["sha"], head);
    let etag = hdr(&h, "etag");
    assert_eq!(etag, format!("\"{head}\""));
    assert!(hdr(&h, "cache-control").contains("stale-while-revalidate"));
    let (st, _, _) = get_h(server, "/o/r/api/refs", &[("If-None-Match", &etag)]).await?;
    assert_eq!(st, 304);
    // ref lists: paged, sorted, filtered
    let p = json(server, "/o/r/api/refs/branches?n=1").await?;
    assert_eq!(p["refs"][0]["name"], "feature/x");
    assert_eq!(p["more"], true);
    let p = json(server, "/o/r/api/refs/branches?after=feature/x&n=5").await?;
    assert_eq!(p["refs"][0]["name"], "main");
    assert_eq!(p["refs"][0]["sha"], head);
    assert_eq!(p["more"], false);
    let p = json(server, "/o/r/api/refs/branches?q=AIN").await?;
    assert_eq!(p["refs"].as_array().unwrap().len(), 1);
    let p = json(server, "/o/r/api/refs/branches?prefix=feature").await?;
    assert_eq!(p["refs"][0]["name"], "feature/x");
    let p = json(server, "/o/r/api/refs/tags").await?;
    let tags = p["refs"].as_array().unwrap();
    assert_eq!(tags.len(), 2);
    let v1 = tags.iter().find(|t| t["name"] == "v1.0").unwrap();
    assert_eq!(v1["sha"], v1_peeled, "annotated tag sha must be peeled");
    assert_eq!(get(server, "/o/r/api/refs/nope").await?.0, 404);
    // SSE form
    let (st, body, h) = get_h(
        server,
        "/o/r/api/refs/tags",
        &[("Accept", "text/event-stream")],
    )
    .await?;
    assert_eq!(st, 200);
    assert!(hdr(&h, "content-type").starts_with("text/event-stream"));
    assert!(body.contains("event: ref\n") && body.contains("event: done\ndata: {\"more\":false}"));

    // resolve
    let (st, text, h) = get_h(server, "/o/r/api/resolve/feature/x/dir", &[]).await?;
    assert_eq!(st, 200);
    let r: Value = serde_json::from_str(&text)?;
    assert_eq!(r["ref"], "feature/x");
    assert_eq!(r["sha"], feature);
    assert_eq!(r["path"], "dir");
    assert_eq!(r["kind"], "branch");
    assert_eq!(hdr(&h, "etag"), format!("\"{feature}\""));
    let r = json(server, "/o/r/api/resolve/v1.0").await?;
    assert_eq!(r["kind"], "tag");
    assert_eq!(r["sha"], v1_peeled);
    let r = json(server, &format!("/o/r/api/resolve/{}/src", &head[..8])).await?;
    assert_eq!(r["kind"], "commit");
    assert_eq!(r["sha"], head);
    assert_eq!(r["path"], "src");
    let r = json(server, "/o/r/api/resolve/").await?;
    assert_eq!(r["ref"], "main");
    let (st, _, ct) = get(server, "/o/r/api/resolve/nope/x").await?;
    assert_eq!(st, 404);
    assert!(!ct.unwrap_or_default().contains("json"));

    // tree root
    let (st, text, h) = get_h(server, "/o/r/api/tree/main", &[]).await?;
    assert_eq!(st, 200);
    let tree: Value = serde_json::from_str(&text)?;
    assert_eq!(tree["ref"], "main");
    assert_eq!(tree["sha"], head);
    assert_eq!(tree["path"], "");
    assert!(hdr(&h, "cache-control").contains("stale-while-revalidate"));
    assert_eq!(hdr(&h, "etag"), format!("\"{head}\""));
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["dir", "src", "README.md", "big.txt", "bin.dat"],
        "dirs first, then byte order"
    );
    let src_entry = &tree["entries"][1];
    assert_eq!(src_entry["type"], "tree");
    assert_eq!(src_entry["mode"], "040000");
    assert_eq!(src_entry["size"], -1);
    let readme_entry = &tree["entries"][2];
    assert_eq!(readme_entry["type"], "blob");
    assert_eq!(readme_entry["mode"], "100644");
    assert_eq!(readme_entry["size"], "# Title\n\nhello\n".len());
    assert_eq!(tree["readme"]["name"], "README.md");
    assert!(
        tree["readme"]["contents"]
            .as_str()
            .unwrap()
            .starts_with("# Title")
    );
    assert_eq!(tree["commit"]["sha"].as_str().unwrap().len(), 40);

    // longest-ref rule: feature/x + path dir
    let t = json(server, "/o/r/api/tree/feature/x/dir").await?;
    assert_eq!(t["ref"], "feature/x");
    assert_eq!(t["path"], "dir");
    assert_eq!(t["entries"][0]["name"], "f.txt");
    // subtree commit = newest commit touching the path
    let t = json(server, "/o/r/api/tree/main/src/inner").await?;
    assert_eq!(t["entries"][0]["name"], "x.txt");
    assert_eq!(t["commit"]["subject"], "second on main");
    // blob path as tree -> 404 plain text
    let (st, body, ct) = get(server, "/o/r/api/tree/main/README.md").await?;
    assert_eq!(st, 404);
    assert!(
        !ct.unwrap_or_default().contains("json"),
        "404 must be plain text: {body}"
    );
    // sha as ref -> immutable
    let (st, text, h) = get_h(server, &format!("/o/r/api/tree/{feature}"), &[]).await?;
    assert_eq!(st, 200);
    let t: Value = serde_json::from_str(&text)?;
    assert_eq!(t["ref"], feature);
    assert_eq!(t["sha"], feature);
    assert!(hdr(&h, "cache-control").contains("immutable"));
    // second hit served from the immutable LRU
    let (st, text2, _) = get_h(server, &format!("/o/r/api/tree/{feature}"), &[]).await?;
    assert_eq!(st, 200);
    assert_eq!(text, text2);

    // blob
    let b = json(server, "/o/r/api/blob/main/README.md").await?;
    assert_eq!(b["name"], "README.md");
    assert_eq!(b["path"], "README.md");
    assert_eq!(b["contents"], "# Title\n\nhello\n");
    let (st, raw, ct) = get(server, "/o/r/api/blob/main/README.md?raw").await?;
    assert_eq!(st, 200);
    assert!(ct.unwrap_or_default().starts_with("text/plain"));
    assert_eq!(raw, "# Title\n\nhello\n");
    let b = json(server, "/o/r/api/blob/main/bin.dat").await?;
    assert_eq!(b["binary"], true);
    assert!(b.get("contents").is_none());
    let b = json(server, "/o/r/api/blob/main/big.txt").await?;
    assert_eq!(b["too_large"], true);
    assert_eq!(b["size"], 2 * 1024 * 1024 + 1);
    assert_eq!(get(server, "/o/r/api/blob/main/nope.txt").await?.0, 404);

    // commits + pagination
    let c = json(server, "/o/r/api/commits?ref=main&path=&skip=0").await?;
    assert_eq!(c["ref"], "main");
    assert_eq!(c["sha"], head);
    let (_, _, h) = get_h(
        server,
        &format!("/o/r/api/commits?ref={head}&path=&skip=0"),
        &[],
    )
    .await?;
    assert!(hdr(&h, "cache-control").contains("immutable"));
    let commits = c["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 35);
    assert_eq!(c["more"], true);
    assert_eq!(commits[0]["sha"], head);
    assert!(commits[0]["parents"].is_array());
    let c2 = json(server, "/o/r/api/commits?ref=main&skip=35&n=50").await?;
    assert_eq!(c2["more"], false);
    let total = 35 + c2["commits"].as_array().unwrap().len();
    let expected: usize = git_in(src, &["rev-list", "--count", "main"])?
        .trim()
        .parse()?;
    assert_eq!(total, expected);
    let c = json(server, "/o/r/api/commits?ref=main&path=src/inner/x.txt").await?;
    let subjects: Vec<&str> = c["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, vec!["second on main", "initial"]);
    let first = &c["commits"][1];
    assert_eq!(first["body"], "body line");
    assert_eq!(first["parents"], serde_json::json!([]));
    assert!(first["author_date"].as_str().unwrap().contains('T'));
    assert_eq!(get(server, "/o/r/api/commits?ref=nope").await?.0, 404);

    // commit detail: rename + merge (first-parent)
    let d = json(server, &format!("/o/r/api/commit/{feature}")).await?;
    assert_eq!(d["commit"]["sha"], feature);
    let paths: Vec<&str> = d["stats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"src/app.rs"),
        "renamed file appears once with new path: {paths:?}"
    );
    assert!(!paths.contains(&"src/main.rs"));
    assert!(d["patch"].as_str().unwrap().contains("diff --git a/"));
    let m = json(server, &format!("/o/r/api/commit/{head}")).await?;
    // HEAD is a filler empty commit; find the merge commit instead.
    assert_eq!(m["stats"], serde_json::json!([]));
    let merge = git_in(src, &["rev-parse", "main~40"])?.trim().to_string();
    let m = json(server, &format!("/o/r/api/commit/{merge}")).await?;
    assert_eq!(m["commit"]["parents"].as_array().unwrap().len(), 2);
    assert!(
        !m["stats"].as_array().unwrap().is_empty(),
        "merge diffed against first parent must have stats"
    );
    assert!(m["patch"].as_str().unwrap().contains("diff --git"));
    assert!(!m["patch"].as_str().unwrap().contains("diff --cc"));
    // short sha and 404
    let d = json(server, &format!("/o/r/api/commit/{}", &feature[..10])).await?;
    assert_eq!(d["commit"]["sha"], feature);
    let (_, _, h) = get_h(server, &format!("/o/r/api/commit/{}", &feature[..10]), &[]).await?;
    assert_eq!(hdr(&h, "etag"), format!("\"{feature}\""));
    let (_, _, h) = get_h(server, &format!("/o/r/api/commit/{feature}"), &[]).await?;
    assert!(hdr(&h, "cache-control").contains("immutable"));
    let (st, _, ct) = get(server, "/o/r/api/commit/deadbeef").await?;
    assert_eq!(st, 404);
    assert!(!ct.unwrap_or_default().contains("json"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn api_md_conformance() -> TestResult {
    let server = Server::start().await?;
    // Empty instance.
    assert_eq!(
        json(&server, "/services/api/owners").await?,
        serde_json::json!([])
    );
    assert_eq!(
        json(&server, "/services/api/owners/nobody").await?,
        serde_json::json!([])
    );

    server.put_repo("o", "r").await?;
    let src = fixture(&server)?;
    let head = git_in(&src, &["rev-parse", "HEAD"])?.trim().to_string();
    let feature = git_in(&src, &["rev-parse", "feature/x"])?
        .trim()
        .to_string();
    let v1_peeled = git_in(&src, &["rev-parse", "v1.0^{commit}"])?
        .trim()
        .to_string();
    conformance(&server, &src, &head, &feature, &v1_peeled).await?;

    // Local repos date every entry (`updated`, optional in API.md, so not in `conformance`):
    // one bounded log walk, newest commit under each entry. Attribution order is unit-tested in
    // api.rs (`attribute_dates`); the fixture's commits share one second.
    let tree = json(&server, "/o/r/api/tree/main").await?;
    for e in tree["entries"].as_array().unwrap() {
        assert!(e["updated"].is_string(), "undated entry: {e}");
    }
    let inner = json(&server, "/o/r/api/tree/main/src/inner").await?;
    let src_entry = &tree["entries"][1];
    assert_eq!(src_entry["name"], "src");
    assert_eq!(src_entry["updated"], inner["commit"]["commit_date"]);

    // branches screen: default on its own, the rest newest first with ahead/behind of it.
    let b = json(&server, "/o/r/api/branches?view=all").await?;
    assert_eq!(b["default"]["name"], "main");
    assert_eq!(b["default"]["sha"], head);
    assert!(b["default"]["ahead"].is_null() && b["default"]["behind"].is_null());
    assert_eq!(b["branches"].as_array().unwrap().len(), 1);
    let fx = &b["branches"][0];
    assert_eq!(fx["name"], "feature/x");
    assert_eq!(fx["sha"], feature);
    // whatever identity git recorded (.cargo/config.toml pins one for tests)
    let who = git_in(&src, &["log", "-1", "--format=%an%x00%ae", "feature/x"])?;
    let (name, email) = who.trim().split_once('\0').unwrap();
    assert_eq!(
        (fx["author"].as_str(), fx["author_email"].as_str()),
        (Some(name), Some(email))
    );
    assert!(fx["updated"].is_string());
    // merged into main: nothing ahead; behind by "second on main", the merge, 40 fillers
    assert_eq!(
        (fx["ahead"].as_u64(), fx["behind"].as_u64()),
        (Some(0), Some(42))
    );
    assert_eq!(b["more"], false);
    // overview = fresh branches; stale is empty for a just-made fixture; q filters
    let b = json(&server, "/o/r/api/branches").await?;
    assert_eq!(b["branches"][0]["name"], "feature/x");
    let b = json(&server, "/o/r/api/branches?view=stale").await?;
    assert_eq!(b["branches"].as_array().unwrap().len(), 0);
    let b = json(&server, "/o/r/api/branches?view=all&q=nope").await?;
    assert_eq!(b["branches"].as_array().unwrap().len(), 0);
    assert_eq!(get(&server, "/o/r/api/branches?view=bogus").await?.0, 404);

    // unknown repo
    assert_eq!(get(&server, "/o/nope/api/refs").await?.0, 404);
    // page route -> index.html
    let (st, html, ct) = get(&server, "/o/r/tree/main/anything").await?;
    assert_eq!(st, 200);
    assert!(ct.unwrap_or_default().starts_with("text/html"));
    assert!(html.contains("<html"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_repo_refs() -> TestResult {
    let server = Server::start().await?;
    server.put_repo("o", "empty").await?;
    let refs = json(&server, "/o/empty/api/refs").await?;
    assert!(refs["head"].is_null());
    let p = json(&server, "/o/empty/api/refs/branches").await?;
    assert_eq!(p["refs"], serde_json::json!([]));
    assert_eq!(p["more"], false);
    assert_eq!(get(&server, "/o/empty/api/resolve/").await?.0, 404);
    Ok(())
}

/// The same contract on an instance whose `cache.max_bytes` is too small for
/// the repo's packs: objects are read from the store by range (indexes local),
/// nothing is materialized, and long answers stream the SSE envelope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_objects_conformance() -> TestResult {
    let big = Server::start().await?;
    big.put_repo("o", "r").await?;
    let src = fixture(&big)?;
    let head = git_in(&src, &["rev-parse", "HEAD"])?.trim().to_string();
    let feature = git_in(&src, &["rev-parse", "feature/x"])?
        .trim()
        .to_string();
    let v1_peeled = git_in(&src, &["rev-parse", "v1.0^{commit}"])?
        .trim()
        .to_string();

    let small = big
        .start_sibling_with(|cfg| {
            cfg.cache.max_bytes = bytesize::ByteSize::b(1);
        })
        .await?;

    // First object request with SSE accept: envelope with task/notice packets and a result.
    let resp = reqwest::Client::new()
        .get(format!("{}/o/r/api/tree/{head}", small.base_url))
        .header("Accept", "application/json, text/event-stream")
        .send()
        .await?;
    assert_eq!(resp.status(), 200);
    assert!(
        hdr(resp.headers(), "content-type").starts_with("text/event-stream"),
        "first remote render streams"
    );
    let text = resp.text().await?;
    assert!(
        text.contains("event: notice\n"),
        "narrates what it does: {text}"
    );
    assert!(
        text.contains("event: task\n"),
        "remote-index task announced: {text}"
    );
    let result_line = text
        .split("\n\n")
        .find(|p| p.starts_with("event: result"))
        .expect("result packet");
    let body: Value =
        serde_json::from_str(result_line.trim_start_matches("event: result\ndata: "))?;
    assert_eq!(body["sha"], head);
    assert!(body["entries"].as_array().unwrap().len() >= 5);

    // Second time: served from the immutable cache as plain JSON even with SSE accept.
    let resp = reqwest::Client::new()
        .get(format!("{}/o/r/api/tree/{head}", small.base_url))
        .header("Accept", "application/json, text/event-stream")
        .send()
        .await?;
    assert!(hdr(resp.headers(), "content-type").starts_with("application/json"));
    assert!(hdr(resp.headers(), "cache-control").contains("immutable"));

    // Full contract, plain JSON.
    conformance(&small, &src, &head, &feature, &v1_peeled).await?;
    assert!(
        !small.registry_has_packs("o", "r").await,
        "small front must not materialize packs"
    );

    // Tasks are discoverable: the remote-index task ran here and finished ok.
    let t = json(&small, "/o/r/api/tasks").await?;
    let recent = t["recent"].as_array().unwrap();
    let ri = recent
        .iter()
        .find(|r| r["kind"] == "remote-index")
        .expect("remote-index task");
    assert_eq!(ri["ok"], true);
    assert!(t["running"].as_array().unwrap().is_empty());
    // Attach to the finished task: replay + result.
    let (st, text, h) = get_h(
        &small,
        &format!("/o/r/api/tasks/{}", ri["id"].as_str().unwrap()),
        &[("Accept", "text/event-stream")],
    )
    .await?;
    assert_eq!(st, 200);
    assert!(hdr(&h, "content-type").starts_with("text/event-stream"));
    assert!(text.contains("event: result\n"), "{text}");

    // Overview (WAL tab) renders without packs and says so.
    let o = json(&small, "/o/r/api/overview").await?;
    assert_eq!(o["local"]["objects"], "remote");
    assert!(
        o["health"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.as_str().unwrap().contains("exceeds"))
    );
    Ok(())
}

/// D15/D27: the repo-scoped API is `/{owner}/{repo}/api/…` (and `…/api-browser/…`);
/// the pre-D15 `/services/api/{owner}/{repo}/…` shape is gone (no aliases —
/// AGENTS banner).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_api_prefix_is_gone() -> TestResult {
    let server = Server::start().await?;
    server.put_repo("o", "r").await?;
    let src = fixture(&server)?;
    let _ = git_in(&src, &["rev-parse", "HEAD"])?;
    for path in ["refs", "resolve/main", "tree/main", "tasks", "overview"] {
        let (sa, a, _) = get_h(&server, &format!("/o/r/api/{path}"), &[]).await?;
        assert_eq!(sa, 200, "{path}: {a}");
        let (sb, _, _) = get_h(&server, &format!("/o/r/api-browser/{path}"), &[]).await?;
        assert_eq!(sb, 200, "{path} on the browser lane");
        let (sc, _, _) = get_h(&server, &format!("/services/api/o/r/{path}"), &[]).await?;
        assert_eq!(sc, 404, "{path}: /services/api/o/r must be gone");
    }
    Ok(())
}

/// Pushes are `/<area>/<repository>.git` only — no `.git` is a pkt-line ERR.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_requires_area_repository_git() -> TestResult {
    let server = Server::start().await?;
    server.put_repo("area", "repository").await?;
    let (st, body, _) = get_h(
        &server,
        "/area/repository/info/refs?service=git-receive-pack",
        &[],
    )
    .await?;
    assert_eq!(st, 200, "{body}");
    assert!(
        body.contains("<area>/<repository>.git"),
        "refusal must name the required URL shape: {body:?}"
    );
    let (ok, ad, _) = get_h(
        &server,
        "/area/repository.git/info/refs?service=git-receive-pack",
        &[],
    )
    .await?;
    assert_eq!(ok, 200, "{ad}");
    assert!(!ad.contains("push URL must be"), "{ad:?}");
    Ok(())
}

/// A browser on localhost is sent to walgit.localhost (same port). Git is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_localhost_redirects_to_walgit_localhost() -> TestResult {
    let server = Server::start().await?;
    let url = format!("{}/", server.base_url);
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .get(&url)
        .header("Accept", "text/html")
        .send()
        .await?;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FOUND,
        "{}",
        resp.status()
    );
    let loc = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        loc.contains("walgit.localhost"),
        "browser should be sent to walgit.localhost, got {loc}"
    );
    let git = reqwest::Client::new()
        .get(format!(
            "{}/area/repository.git/info/refs?service=git-upload-pack",
            server.base_url
        ))
        .header("User-Agent", "git/2.46.0")
        .send()
        .await?;
    assert_ne!(git.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    Ok(())
}

/// D50 issues: who may do what, JSON-only writes, search counts and the timeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issues_api() -> TestResult {
    use reqwest::Method as M;
    use serde_json::json;
    // D51: `maint` is a site admin (Admin on `o/r`, an unowned namespace); alice and bob
    // read the public repository.
    let token = |user: &str, admin: bool| walgit_config::StaticToken {
        user: user.into(),
        token: format!("{user}-token"),
        token_env: None,
        write: admin,
        admin,
    };
    let server = Server::start_with_tweak(|c| {
        c.server.auth.mode = walgit_config::AuthMode::Token;
        c.server.auth.anonymous_read = true;
        c.server.auth.tokens = vec![
            token("alice", false),
            token("bob", false),
            token("maint", true),
        ];
    })
    .await?;
    server.create_users(&["alice", "bob", "maint"]).await?;
    let call = |method: reqwest::Method, path: &str, who: Option<&str>, body: Option<Value>| {
        let mut req = reqwest::Client::new().request(method, format!("{}{path}", server.base_url));
        if let Some(who) = who {
            req = req.bearer_auth(format!("{who}-token"));
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        async move {
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            let text = resp.text().await?;
            anyhow::Ok((
                status,
                serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)),
            ))
        }
    };
    assert_eq!(call(M::PUT, "/o/r/api", Some("maint"), None).await?.0, 201);

    let (status, _) = call(
        M::POST,
        "/o/r/api/issues",
        None,
        Some(json!({"title": "x"})),
    )
    .await?;
    assert_eq!(status, 401, "anonymous readers can't open issues");
    let form = reqwest::Client::new()
        .post(format!("{}/o/r/api/issues", server.base_url))
        .bearer_auth("alice-token")
        .header("content-type", "text/plain")
        .body(r#"{"title":"x"}"#)
        .send()
        .await?;
    assert_eq!(form.status().as_u16(), 415, "writes are JSON only");

    let (status, issue) = call(
        M::POST,
        "/o/r/api/issues",
        Some("alice"),
        Some(json!({"title": "Crash on push", "body": "steps"})),
    )
    .await?;
    assert_eq!(
        (status, issue["number"].as_u64(), issue["state"].as_str()),
        (201, Some(1), Some("open"))
    );
    let (status, issue) = call(
        M::POST,
        "/o/r/api/issues/1/comments",
        Some("bob"),
        Some(json!({"body": "same here"})),
    )
    .await?;
    assert_eq!((status, issue["comments"].as_u64()), (201, Some(1)));

    let (status, refused) = call(
        M::PATCH,
        "/o/r/api/issues/1",
        Some("bob"),
        Some(json!({"state": "closed"})),
    )
    .await?;
    assert_eq!(status, 400, "{refused}");
    let (status, _) = call(
        M::PATCH,
        "/o/r/api/issues/1",
        Some("alice"),
        Some(json!({"add_labels": ["bug"]})),
    )
    .await?;
    assert_eq!(status, 400, "labels need write");
    let (status, _) = call(
        M::PATCH,
        "/o/r/api/issues/1",
        Some("maint"),
        Some(json!({"add_labels": ["bug"]})),
    )
    .await?;
    assert_eq!(status, 200);
    let (status, issue) = call(
        M::POST,
        "/o/r/api/issues/1/comments",
        Some("alice"),
        Some(json!({"body": "fixed in main", "close": "completed"})),
    )
    .await?;
    assert_eq!(
        (
            status,
            issue["state"].as_str(),
            issue["state_reason"].as_str()
        ),
        (201, Some("closed"), Some("completed"))
    );
    call(
        M::POST,
        "/o/r/api/issues",
        Some("bob"),
        Some(json!({"title": "Docs typo"})),
    )
    .await?;

    let open = json(&server, "/o/r/api/issues").await?;
    assert_eq!(
        (open["open_count"].as_u64(), open["closed_count"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(open["items"][0]["number"], 2);
    let closed = json(&server, "/o/r/api/issues?q=is:closed%20label:bug").await?;
    assert_eq!(closed["items"][0]["labels"], json!(["bug"]));

    let detail = json(&server, "/o/r/api/issues/1").await?;
    let kinds: Vec<&str> = detail["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["comment", "labeled", "comment", "closed"]);
    let bobs = detail["timeline"][0]["id"].as_u64().unwrap();
    let (status, _) = call(
        M::PATCH,
        &format!("/o/r/api/issues/1/comments/{bobs}"),
        Some("alice"),
        Some(json!({"body": "edited"})),
    )
    .await?;
    assert_eq!(
        status, 400,
        "only the author or a collaborator edits a comment"
    );

    // The label catalog: writers define colors; deleting a label takes it off every issue.
    let label = |who: &'static str, color: &str| {
        call(
            M::PUT,
            "/o/r/api/labels/bug",
            Some(who),
            Some(json!({"color": color, "description": "Something isn't working"})),
        )
    };
    assert_eq!(label("alice", "#d73a4a").await?.0, 400, "labels need write");
    assert_eq!(label("maint", "red").await?.0, 400, "colors are #rrggbb");
    let (status, catalog) = label("maint", "#d73a4a").await?;
    assert_eq!(status, 200, "{catalog}");
    assert_eq!(
        (
            catalog[0]["name"].as_str(),
            catalog[0]["color"].as_str(),
            catalog[0]["open"].as_u64()
        ),
        (Some("bug"), Some("#d73a4a"), Some(0))
    );
    let (status, _) = call(M::DELETE, "/o/r/api/labels/bug", Some("maint"), None).await?;
    assert_eq!(status, 200);
    assert_eq!(
        json(&server, "/o/r/api/issues/1").await?["labels"],
        json!([]),
        "deleted from the issue too"
    );
    let (status, issue) = call(
        M::DELETE,
        &format!("/o/r/api/issues/1/comments/{bobs}"),
        Some("bob"),
        None,
    )
    .await?;
    assert_eq!((status, issue["comments"].as_u64()), (200, Some(1)));
    Ok(())
}

/// D50 pull requests: open from a pushed branch, compare (commits + diff), refusals, close
/// freezes the tips it shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pulls_api() -> TestResult {
    use serde_json::json;
    let server = Server::start().await?;
    server.put_repo("o", "r").await?;
    let src = fixture(&server)?;
    git_in(&src, &["checkout", "-q", "-b", "topic", "main"])?;
    std::fs::write(src.join("topic.txt"), "new file\n")?;
    git_in(&src, &["add", "topic.txt"])?;
    git_in(&src, &["commit", "-q", "-m", "add topic"])?;
    git_in(&src, &["push", "-q", &server.repo_url("o", "r"), "topic"])?;
    let topic = git_in(&src, &["rev-parse", "topic"])?.trim().to_string();
    let post = |path: &str, body: Value| {
        let req = reqwest::Client::new()
            .post(format!("{}{path}", server.base_url))
            .json(&body);
        async move {
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            anyhow::Ok((status, resp.json::<Value>().await.unwrap_or(Value::Null)))
        }
    };

    let cmp = json(&server, "/o/r/api/compare/main...topic").await?;
    assert_eq!(
        (cmp["ahead"].as_u64(), cmp["behind"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(cmp["commits"][0]["subject"], "add topic");
    assert_eq!(cmp["stats"][0]["path"], "topic.txt");
    assert!(cmp["patch"].as_str().unwrap().contains("+new file"));

    let (status, pr) = post(
        "/o/r/api/pulls",
        json!({"title": "Add topic", "base": "main", "head": "topic"}),
    )
    .await?;
    assert_eq!(status, 201, "{pr}");
    assert_eq!(
        (pr["number"].as_u64(), pr["kind"].as_str()),
        (Some(1), Some("pull"))
    );
    assert_eq!(
        (pr["base_ref"].as_str(), pr["head_ref"].as_str()),
        (Some("main"), Some("topic"))
    );
    assert_eq!(pr["head_sha"], topic);
    let (status, dup) = post(
        "/o/r/api/pulls",
        json!({"title": "again", "base": "main", "head": "topic"}),
    )
    .await?;
    assert_eq!(status, 400, "one open PR per head/base: {dup}");
    let (status, _) = post(
        "/o/r/api/pulls",
        json!({"title": "x", "base": "main", "head": "nope"}),
    )
    .await?;
    assert_eq!(status, 400, "the head branch must exist");
    let (status, issue) = post("/o/r/api/issues", json!({"title": "an issue"})).await?;
    assert_eq!(
        (status, issue["number"].as_u64()),
        (201, Some(2)),
        "issues and PRs share numbers"
    );

    // Reviews: the author can't approve their own PR; a comment review anchors its line
    // comments to the head it saw.
    let (status, own) = post("/o/r/api/pulls/1/reviews", json!({"state": "approved"})).await?;
    assert_eq!(status, 400, "{own}");
    let (status, _) = post("/o/r/api/pulls/1/reviews", json!({"state": "commented"})).await?;
    assert_eq!(status, 400, "an empty comment review says nothing");
    let line =
        json!({"path": "topic.txt", "side": "RIGHT", "line": 1, "body": "trailing newline?"});
    let (status, pr) = post(
        "/o/r/api/pulls/1/reviews",
        json!({"state": "commented", "body": "one nit", "comments": [line]}),
    )
    .await?;
    assert_eq!(status, 201, "{pr}");
    let review = pr["timeline"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        (review["type"].as_str(), review["commit_oid"].as_str()),
        (Some("review"), Some(topic.as_str()))
    );
    assert_eq!(review["comments"][0]["path"], "topic.txt");
    assert!(
        pr.get("review_decision").is_none(),
        "a comment review decides nothing"
    );

    let pulls = json(&server, "/o/r/api/pulls").await?;
    assert_eq!(
        (
            pulls["open_count"].as_u64(),
            pulls["items"][0]["number"].as_u64()
        ),
        (Some(1), Some(1))
    );
    let issues = json(&server, "/o/r/api/issues").await?;
    assert_eq!(
        issues["items"][0]["number"], 2,
        "the issue list holds issues only"
    );
    assert_eq!(
        issues["authors"],
        json!(["anon"]),
        "the filter menus' values"
    );
    let counts = json(&server, "/o/r/api/forge/summary").await?;
    assert_eq!(
        (
            counts["open_issues"].as_u64(),
            counts["open_pulls"].as_u64()
        ),
        (Some(1), Some(1))
    );
    let via_issues = json(&server, "/o/r/api/issues/1").await?;
    assert_eq!(
        via_issues["kind"], "pull",
        "an #1 link resolves through the issues route"
    );
    let (status, _) = post(
        "/o/r/api/issues/2/comments",
        json!({"body": "blocked by #1"}),
    )
    .await?;
    assert_eq!(status, 201);
    let pr1 = json(&server, "/o/r/api/pulls/1").await?;
    let mentioned = pr1["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["type"] == "referenced" && t["source"] == 2);
    assert!(
        mentioned,
        "#1 in issue 2 shows on the pull request's timeline: {pr1}"
    );

    // Close, then move the branch: the closed PR keeps showing what it had.
    let (status, closed) = post(
        "/o/r/api/pulls/1/comments",
        json!({"body": "not now", "close": "completed"}),
    )
    .await?;
    assert_eq!((status, closed["state"].as_str()), (201, Some("closed")));
    git_in(&src, &["commit", "-q", "--allow-empty", "-m", "later"])?;
    git_in(&src, &["push", "-q", &server.repo_url("o", "r"), "topic"])?;
    let pr = json(&server, "/o/r/api/pulls/1").await?;
    assert_eq!(pr["head_sha"], topic);
    Ok(())
}

/// D50 merges: merge / squash / rebase move main as git would, one entry closes the PR and
/// the issues it fixes, conflicts are reported and refused, and what landed fsck-checks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merge_api() -> TestResult {
    use reqwest::Method as M;
    use serde_json::json;
    let server = Server::start().await?;
    server.put_repo("o", "r").await?;
    let src = fixture(&server)?;
    let url = server.repo_url("o", "r");
    let branch = |name: &str, files: &[(&str, &str)]| -> anyhow::Result<()> {
        git_in(&src, &["checkout", "-q", "-B", name, "main"])?;
        for (i, (path, text)) in files.iter().enumerate() {
            std::fs::write(src.join(path), text)?;
            git_in(&src, &["add", path])?;
            git_in(&src, &["commit", "-q", "-m", &format!("{name} {i}")])?;
        }
        git_in(&src, &["push", "-q", &url, name])?;
        Ok(())
    };
    let call = |method: reqwest::Method, path: &str, body: Option<Value>| {
        let mut req = reqwest::Client::new().request(method, format!("{}{path}", server.base_url));
        if let Some(body) = body {
            req = req.json(&body);
        }
        async move {
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            let text = resp.text().await?;
            anyhow::Ok((
                status,
                serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)),
            ))
        }
    };
    let open = |head: &str, body: &str| {
        call(
            M::POST,
            "/o/r/api/pulls",
            Some(json!({"title": head, "body": body, "base": "main", "head": head})),
        )
    };
    let main = || {
        git_in(&src, &["ls-remote", &url, "refs/heads/main"])
            .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
    };

    let (_, issue) = call(
        M::POST,
        "/o/r/api/issues",
        Some(json!({"title": "a.txt is missing"})),
    )
    .await?;
    let issue = issue["number"].as_u64().unwrap();
    branch("m", &[("a.txt", "a\n")])?;
    branch("s", &[("b.txt", "b\n"), ("b.txt", "b2\n")])?;
    branch("r", &[("c.txt", "c\n"), ("d.txt", "d\n")])?;
    let (_, pm) = open("m", &format!("Fixes #{issue}")).await?;
    let (_, ps) = open("s", "").await?;
    let (_, pr) = open("r", "").await?;
    let [pm, ps, pr] = [pm, ps, pr].map(|p| p["number"].as_u64().unwrap());

    let m = json(&server, &format!("/o/r/api/pulls/{pm}/merge")).await?;
    assert_eq!(
        (m["mergeable"].as_bool(), m["up_to_date"].as_bool()),
        (Some(true), Some(false))
    );
    let before = main()?;
    let (status, merged) = call(
        M::POST,
        &format!("/o/r/api/pulls/{pm}/merge"),
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, 200, "{merged}");
    assert_eq!(
        (merged["state"].as_str(), merged["merged"].as_bool()),
        (Some("closed"), Some(true))
    );
    let tip = main()?;
    assert_eq!(merged["merge_commit"], tip);
    git_in(&src, &["fetch", "-q", &url, "main"])?;
    let parents = git_in(&src, &["log", "-1", "--format=%P", "FETCH_HEAD"])?;
    assert_eq!(
        parents.split_whitespace().next(),
        Some(before.as_str()),
        "a merge commit's first parent is the old main"
    );
    assert_eq!(parents.split_whitespace().count(), 2);
    let closed = json(&server, &format!("/o/r/api/issues/{issue}")).await?;
    assert_eq!(
        closed["state"], "closed",
        "Fixes #n closes the issue with the merge"
    );

    let (status, _) = call(
        M::POST,
        &format!("/o/r/api/pulls/{ps}/merge"),
        Some(json!({"method": "squash", "sha": "0000"})),
    )
    .await?;
    assert_eq!(
        status, 409,
        "a head that moved since the client looked is refused"
    );
    let (status, _) = call(
        M::POST,
        &format!("/o/r/api/pulls/{ps}/merge"),
        Some(json!({"method": "squash"})),
    )
    .await?;
    assert_eq!(status, 200);
    git_in(&src, &["fetch", "-q", &url, "main"])?;
    assert_eq!(
        git_in(&src, &["log", "-1", "--format=%P", "FETCH_HEAD"])?
            .split_whitespace()
            .count(),
        1,
        "squash: one parent"
    );
    assert_eq!(git_in(&src, &["show", "FETCH_HEAD:b.txt"])?, "b2\n");

    let (status, _) = call(
        M::POST,
        &format!("/o/r/api/pulls/{pr}/merge"),
        Some(json!({"method": "rebase"})),
    )
    .await?;
    assert_eq!(status, 200);
    git_in(&src, &["fetch", "-q", &url, "main"])?;
    assert_eq!(
        git_in(&src, &["log", "-2", "--format=%s", "FETCH_HEAD"])?,
        "r 1\nr 0\n",
        "rebase replays each commit"
    );
    git_in(&src, &["fsck", "--no-progress"])?;

    // Two branches editing one line: the second conflicts after the first lands.
    git_in(&src, &["branch", "-f", "main", "FETCH_HEAD"])?;
    branch("x", &[("a.txt", "x\n")])?;
    branch("y", &[("a.txt", "y\n")])?;
    let (_, px) = open("x", "").await?;
    let (_, py) = open("y", "").await?;
    assert_eq!(
        call(
            M::POST,
            &format!("/o/r/api/pulls/{}/merge", px["number"]),
            Some(json!({}))
        )
        .await?
        .0,
        200
    );
    let m = json(&server, &format!("/o/r/api/pulls/{}/merge", py["number"])).await?;
    assert_eq!(
        (m["mergeable"].as_bool(), m["conflicts"].clone()),
        (Some(false), json!(["a.txt"]))
    );
    let (status, why) = call(
        M::POST,
        &format!("/o/r/api/pulls/{}/merge", py["number"]),
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, 409, "{why}");

    let (status, _) = call(
        M::PATCH,
        &format!("/o/r/api/pulls/{pm}"),
        Some(json!({"state": "open"})),
    )
    .await?;
    assert_eq!(status, 400, "a merged pull request stays merged");
    let (status, _) = call(M::DELETE, &format!("/o/r/api/pulls/{pm}/branch"), None).await?;
    assert_eq!(status, 200);
    assert!(
        git_in(&src, &["ls-remote", &url, "refs/heads/m"])?.is_empty(),
        "the head branch is gone"
    );
    let merged_list = json(&server, "/o/r/api/pulls?q=is:merged").await?;
    assert_eq!(merged_list["items"].as_array().unwrap().len(), 4);

    // A draft can't merge until it is ready for review.
    git_in(&src, &["branch", "-f", "main", "FETCH_HEAD"])?;
    branch("d", &[("e.txt", "e\n")])?;
    let (_, pd) = call(
        M::POST,
        "/o/r/api/pulls",
        Some(json!({"title": "wip", "base": "main", "head": "d", "draft": true})),
    )
    .await?;
    let pd = pd["number"].as_u64().unwrap();
    assert_eq!(
        json(&server, "/o/r/api/pulls?q=is:draft").await?["items"][0]["number"],
        pd
    );
    let (status, why) = call(
        M::POST,
        &format!("/o/r/api/pulls/{pd}/merge"),
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, 400, "{why}");
    let (status, ready) = call(
        M::PATCH,
        &format!("/o/r/api/pulls/{pd}"),
        Some(json!({"draft": false})),
    )
    .await?;
    assert_eq!((status, ready["draft"].as_bool()), (200, Some(false)));
    assert_eq!(
        call(
            M::POST,
            &format!("/o/r/api/pulls/{pd}/merge"),
            Some(json!({}))
        )
        .await?
        .0,
        200
    );
    Ok(())
}

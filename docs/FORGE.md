# Forge: issues and pull requests

Context: anyone touching issues or pull requests. Decision of record: **D50** (`AGENTS.md` §4).

## Model

Forge state is a fold of events in the WAL. There is no other store.

- **Commit:** a forge write is a log entry: `ENTRY_KIND_FORGE`, or `forge` events on a PUSH. It is
  committed by the manifest CAS like a push. A merge is one entry: pack + base ref update + `Merged` +
  `StateChanged(closed)`.
- **Numbers:** issue and PR numbers share one sequence, `Manifest.forge_next_number`, assigned by the
  publisher on each CAS attempt.
- **Read:** `ForgeState` is loaded on the first forge request and advanced on every sync. Refs-only
  requests never load it. Each checkpoint writes the folded state to `checkpoints/<seq>/forge.pb`. Cold
  start reads that, then the log tail.
- **PR head:** read live from `refs/heads/<head_ref>` while the PR is open, and frozen into the
  closing or merging event. `ForgeOpened` records both tips, so a PR whose branch was deleted
  still shows what it had. Commits and Files changed are the three-dot compare of the tips
  shown (`compare/{base}...{head}`). There are no `refs/pull/*` refs.

## Status

| Slice | State |
|---|---|
| 0 Groundwork (Garage CAS emulation, S3 checksums, D50) | done |
| 1 Issues: API, SDK, list/detail/new pages, labels, assignees; checkpointed `forge.pb` | done |
| 2 Checkpointed state, recovery sims, request budgets, benchmarks | done |
| 3 Pull requests: compare, open, list, Conversation / Commits / Files changed, close and reopen | done |
| 4 Reviews: approve / request changes / comment, line comments from the diff, review decision | done |
| 5 Merge (merge / squash / rebase), mergeability, closing keywords, delete branch | done |
| Deferred, batch A: tab and PR tab counts, Author/Label/Assignee menus, `#n` links and "mentioned in" entries | done |
| Deferred, batch B1: draft pull requests, the label catalog (colors, Labels page) | done |

## Local rig: Garage

Garage accepts `If-Match` and `If-None-Match` and ignores them. The store contract fails with
`Create: exactly one of 32 should win, got 32`. With `store.s3.emulate_conditional_writes = true`, the
S3 store serializes conditional PUTs per key in-process and checks with a HEAD first. The contract
passes, but it is correct for **one instance only**. Run cross-instance concurrency on rustfs
(`just dev-store`) or in the sim.

```
WALGIT_TEST_S3_EMULATE_CAS=1 WALGIT_TEST_S3_ENDPOINT=http://127.0.0.1:3900 \
  cargo test -p walgit-store --features s3 --test contract s3_contract
```

## Behavior worth knowing

- **Permissions** come from the existing principal.
  - Anyone signed in may open issues, comment, and edit or close what they authored.
  - `write` may do everything, including labels and assignees.
  - Anonymous readers can't write.
  - The publisher checks every rule again on each CAS attempt. A refused request publishes
    nothing and answers 400 with the reason.
- **Reviews** anchor their line comments to the head they saw. A comment on another head shows
  as outdated in the conversation and is left out of the diff; there is no line remapping yet.
  A pending review lives in the page until it is submitted, and is lost on reload; GitHub keeps
  it on the server.
- **Merging** builds objects in a scratch object directory over the serving copy (ported from
  rgodha24/walgithub): `merge-tree --write-tree`, `commit-tree`, `pack-objects`, then
  `ingest_pack` and a connectivity check. A refused merge leaves nothing behind. The publish is
  one PUSH entry carrying the pack, the base ref move, `ForgeMerged`, and the closes of fixed
  issues. If the push policy, a forge rule or the base's old value refuses any part, nothing
  lands. Mergeability is computed on each request; it needs the packs on the host.
- **One change, one entry.** "Close with comment" and a multi-field `PATCH` are each one entry.
- **Direct import** refuses repositories that have issues. Its folded log would drop them.
- **Deployment order.** A checkpoint written by a binary older than D50 would carry no
  `forge_key`. Stop older writers before using the forge (D43's rule).

## Recovery

`cargo test -p walgit-server --test sim forge` (seeds as for every sim: `WALGIT_SIM_SEED[S]`):

- **`sim_forge_under_faults`.** Three instances open issues and comment while every store link
  injects faults at 8%: errors before and after, CAS failures, stale reads and truncation.
  Checkpoints race the writes. After healing, a cold instance has to show:
  - every acknowledged issue and comment exactly once, and nothing twice;
  - issue numbers `1..=N` with no hole or repeat;
  - the bucket's `forge_next_number` equal to N + 1;
  - a `forge.pb` + tail fold byte-identical to a long-lived instance's incremental fold.
- **`liveness_forge_after_a_lost_cas_response`.** A write whose CAS reply is lost lands at most
  once, and the writer and a cold reader agree.
- **`test_forge_push_is_all_or_nothing`** (WAL). A refused event keeps the ref unchanged, and a
  refused ref keeps the event out.

Found by the fault sim: committed seqs can skip, because a crash between the log PUT and the CAS
leaves an orphaned slot that the next writer steps over. The fold used to require
`seq == state + 1`, and stopped for good at the first hole. It now trusts the caller's range
(every committed entry in `(after, last]`) instead.

## Measurements

**Store requests** (`forge_request_round_trip_budgets`, asserted): a warm write takes 3
requests (freshness GET, log PUT, manifest CAS); a warm read takes 1; a checkpoint of a forge
repository takes 5 (4 + `forge.pb`).

**Scale**, in-process: `cargo test -p walgit-wal --release --lib forge::tests::bench_scale --
--ignored --nocapture`. Each issue has 5 comments, and a third have a label; the log is about
75 B per event.

| Issues (events) | Fold | `is:open` | Text search | `forge.pb` | Encode | Cold rebuild |
|---|---|---|---|---|---|---|
| 1k (6k) | 2 ms | 0.05 ms | 0.1 ms | 0.5 MiB | 2 ms | 3 ms |
| 10k (63k) | 23 ms | 0.4 ms | 1.5 ms | 4.7 MiB | 18 ms | 35 ms |
| 100k (633k) | 228 ms | 4.5 ms | 16 ms | 48 MiB | 173 ms | 371 ms |

Linear-scan search holds at 100k, so no index is needed yet. The cost that grows is storage:
every checkpoint rewrites the whole `forge.pb` (48 MiB at 100k issues, about once per 256
entries). Sharding `forge.pb` (rewrite only the shards that changed) is the next step if a
repository gets near that size.

**HTTP** (`tests/forge-bench.sh <repo-url>`): Garage on the same machine, one instance with
`emulate_conditional_writes`, release build, 2026-10-16:

| Load | Throughput | p50 | p99 |
|---|---|---|---|
| Sequential comments | — | 3.2 ms | 5.0 ms |
| 1 writer | 147 writes/s | 2.8 ms | 3.5 ms |
| 8 writers | 546 writes/s | 10.9 ms | 13.1 ms |
| 32 writers | 1,280 writes/s | 14.2 ms | 21.7 ms |
| List issues | — | 1.2 ms | 1.5 ms |

Group commit is what scales throughput: concurrent writes share one CAS. All 338 comments landed
exactly once. On real S3, add about 60–80 ms per round trip (a write is 3 rounds). Earlier on
Garage, 32 concurrent comments plus a `git push`, then `kill -9` and a cold start on an empty
cache, reproduced the same issues and timelines.

## Fuzz

`tests/forge-fuzz.mjs` plays users doing everything people do with a repository, starting from an
empty one:
- **Git:** the maintainer commits on main; developers branch, commit and push, update their PR
  branches, merge main into a conflicting branch, and delete branches.
- **Pull requests:** open, sometimes as drafts and with `Fixes #n` and `#n` references; review
  (approve, request changes, line comments); merge by every method; close and reopen.
- **Issues:** open, comment, close with a comment, close as completed or not planned, reopen,
  label, assign, edit, and edit or delete comments.
- **Label catalog:** churn on colors and deletions.
- **Permission probes:** what a read-only user, an author or an anonymous visitor must be refused.

It keeps a model of every item. Every 25 steps and at the end, the server must equal it:
- state, merged, draft, title, comment count, labels, assignees, close reason, review decision;
- the list and tab counts;
- every `#n` mention.

At the end, a fresh clone must pass `git fsck`, every merge commit must be on main with the right
parent count, and deleted branches must be gone. `MODE=snapshot` dumps every item, to compare
across a cold restart.

It needs token users `alice`/`bob` (write), `carol` (read only) and `maint` (write, admin);
see the header of the script. Use a fresh repository or `store.prefix`.

Results on Garage, 2026-10-16:
- Eight seeds, 2,150 steps, four of them running in parallel on separate repositories. The
  model matched every time.
- Seed 7 at 400 steps: 72 items, 9 merges, 11 branches deleted, 78 cross-references, and 111
  correctly refused actions.
- `kill -9`, then a cold start on an empty cache: every item byte-identical, rebuilt from 11
  forge checkpoints plus the tail.
- All 84 UI pages of the result render without an error in headless Chrome.

Found and fixed along the way:
- A clean merge is not always a clean rebase. A branch that merged main to resolve a conflict
  can't be replayed commit by commit. Mergeability now reports `rebaseable`, and the UI offers
  "Rebase and merge" only then.
- A draft with conflicts answered 409 (conflict) instead of 400 (draft). The draft check now
  comes first.

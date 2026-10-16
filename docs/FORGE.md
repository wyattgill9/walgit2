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
| 2 Checkpointed state, recovery, benchmarks | — |
| 3 Pull requests: compare, open, list, Conversation / Commits / Files changed, close and reopen | done |
| 4 Reviews: approve / request changes / comment, line comments from the diff, review decision | done |
| 5 Merge (merge / squash / rebase) | — |

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
- **One change, one entry.** "Close with comment" and a multi-field `PATCH` are each one entry.
- **Direct import** refuses repositories that have issues. Its folded log would drop them.
- **Deployment order.** A checkpoint written by a binary older than D50 would carry no
  `forge_key`. Stop older writers before using the forge (D43's rule).

## Measurements

Benchmarks are not in yet; they come with slice 2. Checked so far on Garage, one instance with
`emulate_conditional_writes`, and checkpoints every 8 entries:
- 32 concurrent comments plus a concurrent `git push`: all 32 present exactly once, and the push
  landed.
- `kill -9`, then a cold start on an empty cache directory: identical issue list and timeline,
  read from `forge.pb` plus the log tail.

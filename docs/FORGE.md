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
  closing or merging event.

## Status

| Slice | State |
|---|---|
| 0 Groundwork (Garage CAS emulation, S3 checksums, D50) | done |
| 1 Issues | — |
| 2 Checkpointed state, recovery, benchmarks | — |
| 3 Pull requests | — |
| 4 Reviews | — |
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

## Measurements

None yet. They arrive with slice 2: write latency, contention, list/search latency, and storage growth.

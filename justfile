# walgit justfile — local dev and test targets.

# `timeout` is GNU coreutils: absent on macOS, where every wrapped recipe otherwise dies with
# `sh: timeout: command not found` (exit 127) before a single test runs. Prefer timeout, then
# gtimeout (brew install coreutils), else run unwrapped: no watchdog is better than no tests.
t5 := `if command -v timeout >/dev/null 2>&1; then echo "timeout 300"; elif command -v gtimeout >/dev/null 2>&1; then echo "gtimeout 300"; else echo ""; fi`
t10 := `if command -v timeout >/dev/null 2>&1; then echo "timeout 600"; elif command -v gtimeout >/dev/null 2>&1; then echo "gtimeout 600"; else echo ""; fi`
t15 := `if command -v timeout >/dev/null 2>&1; then echo "timeout 900"; elif command -v gtimeout >/dev/null 2>&1; then echo "gtimeout 900"; else echo ""; fi`

# Default: show available targets.
default:
    @just --list

# Build the Vite SPA assets embedded by walgit-server.
web-build:
    cd web && pnpm install --frozen-lockfile && pnpm run build

# --- tests -------------------------------------------------------------------
# All hermetic: in-memory store, tempdir caches, real `git` binary.

# Fast tier (default, < 1 min): unit tests + the quick integration suites.
# Never run `cargo test --workspace --no-fail-fast` interactively: a single
# hung test blocks for the whole timeout. Use `just e2e` / `just ci` below.
test:
    {{t5}} cargo test --workspace --lib --bins
    {{t10}} cargo test -p walgit-store -p walgit-git -p walgit-wal --tests
    {{t10}} cargo test -p walgit-server --test web_api --test web_ui --test api_v1 --test static_http --test packfile_uri --test forward --test maintain --test routing_prefix --test lfs_upstream --test drain --test events --test follow --test policy

# Smart-HTTP end-to-end against real git (≈ 20 s) — run when touching smart.rs/receive/upload-pack/wal.
e2e *ARGS:
    {{t10}} cargo test -p walgit-server --test e2e {{ARGS}}

# Zero rustc warnings, workspace-wide, all targets (tests, benches, examples).
warnings:
    #!/usr/bin/env bash
    set -uo pipefail
    if ! out="$({{t15}} cargo build --workspace --all-targets 2>&1)"; then
        printf '%s\n' "$out"
        echo; echo "cargo build failed — fix the errors above"; exit 1
    fi
    # Strip ANSI escapes (CARGO_TERM_COLOR=always) so the anchored `^warning:` still matches.
    plain="$(printf '%s\n' "$out" | sed $'s/\x1b\\[[0-9;]*m//g')"
    if printf '%s\n' "$plain" | grep -qE '^warning: (unused|function|variable|field|method|struct|enum|never|dead|irrefutable|unreachable|value assigned|deprecated|trait|type|constant|static|associated)'; then
        printf '%s\n' "$plain" | grep -E '^warning' -A4 | grep -vE '^warning: `walgit-[a-z]+`'
        echo; echo "rustc warnings present — fix them"; exit 1
    fi
    echo "no rustc warnings"

# Clippy, workspace-wide, all targets, warnings are errors. The lint set lives in
# [workspace.lints] in Cargo.toml; test code is exempt from the panic-path restriction
# lints via clippy.toml (allow-unwrap-in-tests etc.).
clippy:
    {{t15}} cargo clippy --workspace --all-targets -- -D warnings

# Everything that must be green before a merge.
ci: warnings clippy test e2e sim smoke

# Fault injection and recovery share process-wide test hooks; run serially.
sim:
    {{t15}} cargo test -p walgit-server --test sim -- --test-threads=1

# Standalone CLI/server against memory; add WALGIT_TEST_S3_ENDPOINT for the local rig.
smoke:
    {{t15}} cargo build -p walgit-cli
    WALGIT="$(realpath "${CARGO_TARGET_DIR:-target}/debug/walgit")" {{t15}} tests/e2e.sh

# Slow tier: #[ignore]d benches/soaks (20k-ref push, 466k-ref render, ...).
test-slow:
    cargo test --workspace -- --ignored --nocapture

# walgit-store contract against the in-memory backend.
store-test:
    cargo test -p walgit-store --test contract -- memory_contract

# Bounded contract checks and exact negative controls (Java 11+).
spec:
    scripts/run-spec.sh fast

spec-fragments:
    scripts/run-spec.sh fragments

spec-full:
    scripts/run-spec.sh full

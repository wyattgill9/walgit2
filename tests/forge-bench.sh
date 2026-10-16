#!/usr/bin/env bash
# Forge write latency and contention against a running walgit (docs/FORGE.md "Measurements").
#
#   tests/forge-bench.sh https://walgit.localhost:8080/test/demo [curl options…]
#
# The repository must exist and accept writes from this client (e.g. `server.auth.mode = "none"`
# on loopback, or pass `-H Authorization:Bearer…`; options are word-split, so no spaces inside
# one). It opens one issue, then:
#   1. 50 sequential comments                        → write latency p50 / p99
#   2. 96 comments at 1, 8 and 32 concurrent writers → throughput and latency
#   3. 30 list reads                                 → read latency p50 / p99
# and checks that every comment landed exactly once. Requires: bash, curl, jq, awk, xargs.
set -euo pipefail

REPO="${1:?usage: $0 <repo-url> [curl options…]}"
shift
export API="$REPO/api" CURL_ARGS="$*"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# One comment; prints its seconds.
comment() {
    # shellcheck disable=SC2086 # CURL_ARGS is a word list
    curl -sS $CURL_ARGS -o /dev/null -w '%{time_total}\n' -X POST "$API/issues/$1/comments" \
        -H 'content-type: application/json' -d "{\"body\":\"$2\"}"
}
export -f comment

# p50 / p99 of the seconds in a file, as milliseconds.
pct() {
    sort -n "$1" | awk '{ v[NR] = $1 } END {
        printf "p50 %.1f ms  p99 %.1f ms  (n=%d)", v[int((NR - 1) * 0.50) + 1] * 1000, v[int((NR - 1) * 0.99) + 1] * 1000, NR }'
}

# shellcheck disable=SC2086
ISSUE="$(curl -sS $CURL_ARGS -X POST "$API/issues" -H 'content-type: application/json' \
    -d "{\"title\":\"forge-bench $(date +%s)\"}" | jq -r .number)"
[[ "$ISSUE" =~ ^[0-9]+$ ]] || { echo "could not open an issue at $API" >&2; exit 1; }
export ISSUE
echo "issue #$ISSUE"

for i in $(seq 1 50); do comment "$ISSUE" "seq $i"; done > "$TMP/seq"
echo "sequential comment:        $(pct "$TMP/seq")"

for c in 1 8 32; do
    start=$(date +%s%N)
    seq 1 96 | xargs -P "$c" -I{} bash -c "comment \"\$ISSUE\" \"c$c {}\"" > "$TMP/c$c"
    ms=$(( ($(date +%s%N) - start) / 1000000 ))
    printf 'concurrency %-2s 96 writes: %6.1f writes/s  %s\n' "$c" "$(awk "BEGIN { print 96000 / $ms }")" "$(pct "$TMP/c$c")"
done

# shellcheck disable=SC2086
for i in $(seq 1 30); do curl -sS $CURL_ARGS -o /dev/null -w '%{time_total}\n' "$API/issues?q=is:open"; done > "$TMP/list"
echo "list issues:               $(pct "$TMP/list")"

# shellcheck disable=SC2086
got=$(curl -sS $CURL_ARGS "$API/issues/$ISSUE" | jq .comments)
want=$((50 + 3 * 96))
echo "comments on #$ISSUE: $got of $want"
[[ "$got" == "$want" ]]

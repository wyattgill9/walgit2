#!/usr/bin/env bash
# Git push throughput against a running walgit: real `git push` over smart HTTP.
#
#   tests/push-bench.sh https://walgit.localhost:8080/test/push-bench
#
# The repository is created (PUT …/api) and must not exist. Env:
#   WALGIT_CA     CA file for a self-signed server (git `http.sslCAInfo`)
#   WALGIT_TOKEN  bearer for a server with auth (omit for `mode = "none"`)
#   SERVER_LOG    the server's JSON log: adds how many pushes shared each manifest CAS
#   LEVELS        concurrent pushers (default "1 8 32 64"); PUSHES per level (default 128)
#
# 1. Independent branches: each pusher commits and pushes to its own branch; pushes don't
#    conflict, so concurrent ones can share one log PUT + manifest CAS (group commit).
# 2. Everyone on main: every push targets main; a rejected push (main moved) fetches, rebases
#    and pushes again, which is what developers on a hot branch do. Retries are reported.
# Commits are made before timing; a sample is the `git push` alone (or the push-retry loop).
# Requires: bash, git, curl, xargs, awk, sort.
set -euo pipefail

URL="${1:?usage: $0 <repo-url>}"
REPO="${URL#*://*/}" # owner/name, as the server logs it
LEVELS="${LEVELS:-1 8 32 64}"
PUSHES="${PUSHES:-128}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
GITC=(-c "user.name=bench" -c "user.email=bench@example.com")
[[ -n "${WALGIT_CA:-}" ]] && GITC+=(-c "http.sslCAInfo=$WALGIT_CA")
[[ -n "${WALGIT_TOKEN:-}" ]] && GITC+=(-c "http.extraHeader=Authorization: Bearer $WALGIT_TOKEN")
export GITC_STR="${GITC[*]@Q}" URL TMP

curl_auth=()
[[ -n "${WALGIT_TOKEN:-}" ]] && curl_auth=(-H "Authorization: Bearer $WALGIT_TOKEN")
status=$(curl -sS ${WALGIT_CA:+--cacert "$WALGIT_CA"} "${curl_auth[@]}" -o /dev/null -w '%{http_code}' -X PUT "$URL/api")
[[ "$status" == 201 ]] || { echo "create $URL: HTTP $status (it must not exist)" >&2; exit 1; }

g() { eval "git $GITC_STR \"\$@\""; }
export -f g

seed="$TMP/seed"
git init -q -b main "$seed"
echo "push bench" > "$seed/README"
g -C "$seed" add README
g -C "$seed" commit -qm init
g -C "$seed" push -q "$URL.git" main
g -C "$seed" remote add origin "$URL.git"

# p50 / p99 of the milliseconds in a file.
pct() {
    sort -n "$1" | awk '{ v[NR] = $1 } END { printf "p50 %5.0f ms  p99 %5.0f ms", v[int((NR - 1) * 0.50) + 1], v[int((NR - 1) * 0.99) + 1] }'
}
# Pushes per manifest CAS in the server log since line $1.
batches() {
    [[ -n "${SERVER_LOG:-}" ]] || return 0
    tail -n +"$(($1 + 1))" "$SERVER_LOG" | grep '"message":"wal.publish"' | grep -F "\"repo\":\"$REPO\"" |
        grep -v '"seq":0[,}]' |
        grep -o '"batch_size":[0-9]*' | cut -d: -f2 |
        awk '{ n++; s += $1; if ($1 > m) m = $1 } END { if (n) printf "  %d commits to the bucket, %.1f pushes each (max %d)", n, s / n, m }'
}
log_mark() { [[ -n "${SERVER_LOG:-}" ]] && wc -l < "$SERVER_LOG" || echo 0; }

# One pusher of scenario 1: K commits ready, then push them one at a time to its branch.
branch_worker() {
    local i=$1 k=$2
    local d="$TMP/w$i"
    for n in $(seq 1 "$k"); do
        echo "level $LEVEL worker $i push $n" > "$d/f$n"
        g -C "$d" add "f$n"
        g -C "$d" commit -qm "w$i $n"
        g -C "$d" branch -q -f "c$n"
    done
    for n in $(seq 1 "$k"); do
        local s e
        s=$(date +%s%N)
        g -C "$d" push -q origin "c$n:refs/heads/l$LEVEL-w$i" 2> "$d/err" || cat "$d/err" >> "$TMP/errors"
        e=$(date +%s%N)
        echo $(((e - s) / 1000000))
    done
}
# One pusher of scenario 2: commit, push to main; on a rejection, rebase and push again.
main_worker() {
    local i=$1 k=$2
    local d="$TMP/w$i"
    for n in $(seq 1 "$k"); do
        echo "level $LEVEL worker $i push $n" > "$d/m$LEVEL-$i-$n"
        g -C "$d" add "m$LEVEL-$i-$n"
        g -C "$d" commit -qm "main l$LEVEL w$i $n"
        local s e
        s=$(date +%s%N)
        until g -C "$d" push -q origin HEAD:main 2> /dev/null; do
            echo retry >> "$TMP/retries"
            g -C "$d" pull -q --rebase origin main > /dev/null 2>&1
        done
        e=$(date +%s%N)
        echo $(((e - s) / 1000000))
    done
}
export -f branch_worker main_worker

run() { # scenario, pushers
    local scenario=$1 c=$2
    local k=$((PUSHES / c))
    rm -rf "$TMP"/w* "$TMP/retries"
    for i in $(seq 1 "$c"); do
        cp -r "$seed" "$TMP/w$i"
        g -C "$TMP/w$i" pull -q origin main
    done
    local mark start ms
    mark=$(log_mark)
    start=$(date +%s%N)
    seq 1 "$c" | LEVEL=$c xargs -P "$c" -I{} bash -c "${scenario}_worker {} $k" > "$TMP/lat"
    ms=$((($(date +%s%N) - start) / 1000000))
    local total retries
    total=$(wc -l < "$TMP/lat")
    retries=$( [[ -f "$TMP/retries" ]] && wc -l < "$TMP/retries" || echo 0)
    [[ "$total" -eq $((k * c)) ]] || echo "warning: $total timings for $((k * c)) pushes" >&2
    printf '%-8s %3d pushers: %4d pushes in %6.1f s  %6.1f pushes/s  %s' "$scenario" "$c" "$total" \
        "$(awk "BEGIN { print $ms / 1000 }")" "$(awk "BEGIN { print $total * 1000 / $ms }")" "$(pct "$TMP/lat")"
    [[ "$scenario" == main ]] && printf '  %d retries' "$retries"
    batches "$mark"
    echo
}

echo "push bench: $URL, $PUSHES pushes per level"
for c in $LEVELS; do run branch "$c"; done
for c in $LEVELS; do run main "$c"; done
if [[ -s "$TMP/errors" ]]; then
    echo "push errors:" >&2
    sort "$TMP/errors" | uniq -c >&2
    exit 1
fi

# Every push landed: a fresh clone has every commit of main and every branch.
fresh="$TMP/fresh"
g clone -q --mirror "$URL.git" "$fresh"
git -C "$fresh" fsck --no-progress
main_commits=$(git -C "$fresh" rev-list --count main)
want=$((1 + $(echo $LEVELS | wc -w) * PUSHES))
echo "main has $main_commits commits (expected $want), $(git -C "$fresh" for-each-ref refs/heads | wc -l) branches; fsck clean"
[[ "$main_commits" -eq "$want" ]] || { echo "pushes went missing from main" >&2; exit 1; }

#!/usr/bin/env bash
# 02-parallel-fan-out.sh — Two parallel sessions, one fan-in that waits for both
# auth and db run concurrently; review depends on both.
set -euo pipefail

TENDER="${TENDER:-./target/debug/tender}"
NS="spike-fanout-$$"
TMPDIR="/tmp/tender-spike-$$"

cleanup() {
    echo "--- cleanup ---"
    "$TENDER" kill auth   --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill db     --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill review --namespace "$NS" 2>/dev/null || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

mkdir -p "$TMPDIR"

echo "=== 02-parallel-fan-out (namespace: $NS) ==="

# Two parallel sessions (no --after, so they start immediately)
"$TENDER" start auth --namespace "$NS" -- \
    sh -c "sleep 1 && echo 'auth refactored' > $TMPDIR/auth.txt && echo 'auth done'"

"$TENDER" start db --namespace "$NS" -- \
    sh -c "sleep 1 && echo 'db layer refactored' > $TMPDIR/db.txt && echo 'db done'"

# Fan-in: review depends on BOTH auth and db
"$TENDER" start review --namespace "$NS" --after auth --after db -- \
    sh -c "cat $TMPDIR/auth.txt $TMPDIR/db.txt && echo 'review: all changes consistent'"

echo "--- waiting for all sessions ---"
"$TENDER" wait auth db review --namespace "$NS" -t 30

echo ""
echo "--- session statuses ---"
for s in auth db review; do
    echo "[$s]"
    "$TENDER" status "$s" --namespace "$NS"
    echo ""
done

echo "--- logs ---"
for s in auth db review; do
    echo "=== $s ==="
    "$TENDER" log "$s" --namespace "$NS" -r
    echo ""
done

# Verify
REVIEW_LOG=$("$TENDER" log review --namespace "$NS" -r 2>&1)

PASS=true

if echo "$REVIEW_LOG" | grep -q "auth refactored"; then
    echo "PASS: review saw auth output"
else
    echo "FAIL: review did not see auth output"
    PASS=false
fi

if echo "$REVIEW_LOG" | grep -q "db layer refactored"; then
    echo "PASS: review saw db output"
else
    echo "FAIL: review did not see db output"
    PASS=false
fi

if echo "$REVIEW_LOG" | grep -q "all changes consistent"; then
    echo "PASS: review completed"
else
    echo "FAIL: review did not produce expected output"
    PASS=false
fi

echo ""
if [ "$PASS" = true ]; then
    echo "RESULT: ALL PASSED"
else
    echo "RESULT: SOME TESTS FAILED"
    exit 1
fi

#!/usr/bin/env bash
# 01-basic-chain.sh — Test basic --after dependency chain with 3 sessions
# Research -> Impl -> Review, each waiting for the previous to complete.
set -euo pipefail

TENDER="${TENDER:-./target/debug/tender}"
NS="spike-chain-$$"
TMPDIR="/tmp/tender-spike-$$"

cleanup() {
    echo "--- cleanup ---"
    "$TENDER" kill research --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill impl    --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill review  --namespace "$NS" 2>/dev/null || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

mkdir -p "$TMPDIR"

echo "=== 01-basic-chain (namespace: $NS) ==="

# Session 1: research — writes a findings file
"$TENDER" start research --namespace "$NS" -- \
    sh -c "echo 'finding: auth is in src/auth.ts' > $TMPDIR/findings.txt && echo 'research done'"

# Session 2: impl — depends on research, reads findings
"$TENDER" start impl --namespace "$NS" --after research -- \
    sh -c "cat $TMPDIR/findings.txt && echo 'implemented fixes based on findings'"

# Session 3: review — depends on impl
"$TENDER" start review --namespace "$NS" --after impl -- \
    sh -c "echo 'review: changes look good'"

echo "--- waiting for all sessions ---"
"$TENDER" wait research impl review --namespace "$NS" -t 30

echo ""
echo "--- session statuses ---"
for s in research impl review; do
    echo "[$s]"
    "$TENDER" status "$s" --namespace "$NS"
    echo ""
done

echo "--- logs ---"
for s in research impl review; do
    echo "=== $s ==="
    "$TENDER" log "$s" --namespace "$NS" -r
    echo ""
done

# Verify
RESEARCH_LOG=$("$TENDER" log research --namespace "$NS" -r 2>&1)
IMPL_LOG=$("$TENDER" log impl --namespace "$NS" -r 2>&1)
REVIEW_LOG=$("$TENDER" log review --namespace "$NS" -r 2>&1)

PASS=true

if echo "$RESEARCH_LOG" | grep -q "research done"; then
    echo "PASS: research session completed"
else
    echo "FAIL: research session did not produce expected output"
    PASS=false
fi

if echo "$IMPL_LOG" | grep -q "finding: auth is in src/auth.ts"; then
    echo "PASS: impl read research findings"
else
    echo "FAIL: impl did not see research findings"
    PASS=false
fi

if echo "$IMPL_LOG" | grep -q "implemented fixes"; then
    echo "PASS: impl produced output"
else
    echo "FAIL: impl did not produce expected output"
    PASS=false
fi

if echo "$REVIEW_LOG" | grep -q "review: changes look good"; then
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

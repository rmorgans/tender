#!/usr/bin/env bash
# 03-failure-propagation.sh — Middle session fails, test what happens to downstream
# research (ok) -> impl (FAILS) -> review (should it start?)
set -euo pipefail

TENDER="${TENDER:-./target/debug/tender}"
NS="spike-fail-$$"
TMPDIR="/tmp/tender-spike-$$"

cleanup() {
    echo "--- cleanup ---"
    "$TENDER" kill research --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill impl    --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill review  --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill review2 --namespace "$NS" 2>/dev/null || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

mkdir -p "$TMPDIR"

echo "=== 03-failure-propagation (namespace: $NS) ==="

# Session 1: research succeeds
"$TENDER" start research --namespace "$NS" -- \
    sh -c "echo 'findings written' > $TMPDIR/findings.txt"

# Session 2: impl FAILS (exit 1)
"$TENDER" start impl --namespace "$NS" --after research -- \
    sh -c "echo 'starting impl' && exit 1"

# Session 3: review depends on impl (should this block forever? fail? skip?)
"$TENDER" start review --namespace "$NS" --after impl -- \
    sh -c "echo 'review should NOT run if failure blocks'"

echo "--- waiting for research and impl ---"
"$TENDER" wait research impl --namespace "$NS" -t 30 || true

echo ""
echo "--- checking impl status (should show non-zero exit) ---"
"$TENDER" status impl --namespace "$NS"

echo ""
echo "--- waiting for review (expect timeout or skip) ---"
# Give review a short timeout — if --after blocks on failure, it won't complete
"$TENDER" wait review --namespace "$NS" -t 10 2>&1 && REVIEW_COMPLETED=true || REVIEW_COMPLETED=false

echo ""
echo "--- review status ---"
"$TENDER" status review --namespace "$NS" 2>&1 || true

echo ""
echo "=== PART 2: --any-exit flag ==="
echo "Testing whether --any-exit allows downstream to proceed despite failure"

# Session with --any-exit: should proceed even if dependency failed
"$TENDER" start review2 --namespace "$NS" --after impl --any-exit -- \
    sh -c "echo 'review2 ran despite upstream failure'" 2>&1 || true

"$TENDER" wait review2 --namespace "$NS" -t 10 2>&1 && REVIEW2_COMPLETED=true || REVIEW2_COMPLETED=false

echo ""
echo "--- review2 status ---"
"$TENDER" status review2 --namespace "$NS" 2>&1 || true
if [ "$REVIEW2_COMPLETED" = true ]; then
    echo "--- review2 log ---"
    "$TENDER" log review2 --namespace "$NS" -r 2>&1 || true
fi

echo ""
echo "=== OBSERVATIONS ==="
if [ "$REVIEW_COMPLETED" = true ]; then
    echo "NOTE: review DID complete — --after does NOT block on upstream failure"
    REVIEW_LOG=$("$TENDER" log review --namespace "$NS" -r 2>&1 || true)
    echo "  review log: $REVIEW_LOG"
else
    echo "NOTE: review did NOT complete — --after BLOCKS when upstream fails (exit non-zero)"
fi

if [ "$REVIEW2_COMPLETED" = true ]; then
    echo "NOTE: review2 with --any-exit DID complete — flag works as expected"
else
    echo "NOTE: review2 with --any-exit did NOT complete — flag may not work as expected"
fi

echo ""
echo "RESULT: See observations above (this test documents behavior, not pass/fail)"

#!/usr/bin/env bash
# 04-real-agent-chain.sh — Template for real claude -p / codex exec chains
#
# By default, uses echo stand-ins so the script is runnable without API keys.
# To test with real agents, set USE_REAL_AGENTS=1 and ensure claude/codex are on PATH.
#
# Usage:
#   ./04-real-agent-chain.sh                     # echo stand-ins
#   USE_REAL_AGENTS=1 ./04-real-agent-chain.sh   # real agents (needs API keys)
set -euo pipefail

TENDER="${TENDER:-./target/debug/tender}"
NS="spike-agent-$$"
WORKDIR="/tmp/tender-spike-agent-$$"
USE_REAL_AGENTS="${USE_REAL_AGENTS:-0}"

cleanup() {
    echo "--- cleanup ---"
    "$TENDER" kill research  --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill implement --namespace "$NS" 2>/dev/null || true
    "$TENDER" kill review    --namespace "$NS" 2>/dev/null || true
    # Uncomment to clean up workdir:
    # rm -rf "$WORKDIR"
    echo "Workdir preserved at: $WORKDIR"
}
trap cleanup EXIT

mkdir -p "$WORKDIR"

echo "=== 04-real-agent-chain (namespace: $NS) ==="
echo "Mode: $([ "$USE_REAL_AGENTS" = "1" ] && echo "REAL AGENTS" || echo "ECHO STAND-INS")"
echo "Workdir: $WORKDIR"
echo ""

if [ "$USE_REAL_AGENTS" = "1" ]; then
    # ----------------------------------------------------------------
    # REAL AGENT MODE
    # Replace prompts with your actual task descriptions.
    # File-based communication: agents write to $WORKDIR, next agent reads.
    # ----------------------------------------------------------------

    "$TENDER" start research --namespace "$NS" --timeout 300 --cwd "$WORKDIR" -- \
        claude -p "Analyze the current directory structure. Write a summary of findings to findings.md. Be concise."

    "$TENDER" start implement --namespace "$NS" --after research --timeout 300 --cwd "$WORKDIR" -- \
        claude -p "Read findings.md. Create a simple TODO list based on the findings. Write it to TODO.md."
        # Alternative with codex:
        # codex exec --full-auto "Read findings.md and create TODO.md based on the findings"

    "$TENDER" start review --namespace "$NS" --after implement --timeout 300 --cwd "$WORKDIR" -- \
        claude -p "Read findings.md and TODO.md. Write a brief review to review.md noting any issues."
else
    # ----------------------------------------------------------------
    # STAND-IN MODE (default) — runnable without API keys
    # Simulates what agents would do with echo/cat/sleep
    # ----------------------------------------------------------------

    "$TENDER" start research --namespace "$NS" --timeout 30 --cwd "$WORKDIR" -- \
        sh -c "echo '# Findings' > findings.md && echo '- Auth module needs refactoring' >> findings.md && echo '- Database layer has N+1 queries' >> findings.md && echo 'Research complete' && sleep 1"

    "$TENDER" start implement --namespace "$NS" --after research --timeout 30 --cwd "$WORKDIR" -- \
        sh -c "echo 'Reading findings...' && cat findings.md && echo '' && echo '# TODO' > TODO.md && echo '- [ ] Refactor auth module' >> TODO.md && echo '- [ ] Fix N+1 queries' >> TODO.md && echo 'Implementation plan written' && sleep 1"

    "$TENDER" start review --namespace "$NS" --after implement --timeout 30 --cwd "$WORKDIR" -- \
        sh -c "echo 'Reviewing...' && cat findings.md && cat TODO.md && echo '' && echo '# Review' > review.md && echo 'TODO items align with findings. LGTM.' >> review.md && echo 'Review complete'"
fi

echo "--- waiting for pipeline ---"
"$TENDER" wait research implement review --namespace "$NS" -t 120

echo ""
echo "--- session statuses ---"
for s in research implement review; do
    echo "[$s]"
    "$TENDER" status "$s" --namespace "$NS"
    echo ""
done

echo "--- logs ---"
for s in research implement review; do
    echo "=== $s ==="
    "$TENDER" log "$s" --namespace "$NS" -r
    echo ""
done

echo "--- workdir contents ---"
ls -la "$WORKDIR"/
echo ""
for f in "$WORKDIR"/*.md; do
    if [ -f "$f" ]; then
        echo "=== $(basename "$f") ==="
        cat "$f"
        echo ""
    fi
done

echo "RESULT: Pipeline complete. Check logs and workdir above."

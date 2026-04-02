---
id: agent-exec-spike
depends_on: []
links: []
---

# Agent Orchestration Spike — Tender as Agent Pipeline

Spike to test whether tender's existing `--after` dependency chains work for orchestrating one-shot coding agents. No new exec target needed — both Claude Code and Codex are one-shot (run task, exit).

## Key Finding (Pre-Spike)

Both agents are one-shot:
- `claude -p "task"` → runs, exits
- `codex exec "task"` → runs, exits

Neither stays alive as a persistent session. The exec model doesn't apply. The orchestration model is dependency chaining — each agent is a separate tender session with `exec_target: None`.

## Hypothesis

Tender's existing primitives (sessions, `--after`, timeouts, namespaces, `wait`, `watch`) are sufficient to orchestrate multi-agent pipelines without any Rust changes.

## What to test

### 1. Basic `--after` chain with Claude Code

```bash
tender start research -- claude -p "find all auth usages in src/, write findings to findings.txt"
tender start impl --after research -- claude -p "read findings.txt and refactor the old auth calls"
tender start review --after impl -- claude -p "review the git diff for security issues"
```

Does the chain execute in order? Does each agent see the previous agent's file output?

### 2. Mixed-provider pipeline

```bash
tender start analyze -- claude -p "analyze the codebase, write analysis.md"
tender start implement --after analyze -- codex exec --full-auto "read analysis.md, implement the recommendations"
tender start verify --after implement -- claude -p "run tests and verify the changes"
```

Different agents, same working directory, chained via tender.

### 3. Parallel fan-out with fan-in review

```bash
tender start auth -- claude -p "refactor auth module"
tender start db -- codex exec --full-auto "refactor database layer"
tender start review --after auth --after db -- claude -p "review all changes for consistency"
```

Two agents work in parallel, a third waits for both.

### 4. Failure handling

What happens when a middle agent fails? Does `--after` propagate the failure? Does `--any-exit` allow the chain to continue?

### 5. Output sharing

Agents share a working directory, so file-based communication works. But can a downstream agent also read the upstream's tender output log?

```bash
tender log research  # Does this give useful context to pass to the next agent?
```

## Spike scope

- 2-4 hours, no Rust changes
- Shell scripts only
- Test with `claude -p` and `codex exec --full-auto`
- Document: what works, what breaks, what's missing

## Questions the spike should answer

1. Does `--after` chaining work reliably with one-shot agents?
2. Is file-based communication between agents sufficient, or do agents need access to upstream session logs?
3. What timeout values are practical for agent sessions?
4. Does parallel fan-out + fan-in review work?
5. What's missing from tender's current primitives for this use case?

## Not In Scope

- New exec targets or Rust changes
- Prompt engineering or agent behavior tuning
- Multi-model routing or cost tracking
- Building an AI framework

---
name: ralph-implement
description: >-
  Drive one iteration of the Ralph implementation loop for avalanche-rs: study
  specs/*.md and plan/*.md, pick the next batch of plan items that can be
  implemented in parallel, build each in its own git worktree via subagents,
  verify, and merge the finished work back into the current tree — folding any
  spec simplifications/extensions and plan progress updates discovered during
  implementation. Use this whenever the user says "ralph", "run ralph",
  "ralph-implement", "do the next ralph iteration", "implement the next plan
  items", "find the next parallel work and build it", or otherwise wants you to
  autonomously advance the milestone plan by fanning out worktree subagents.
  Trigger even when the user just says "keep going on the plan" or "what's next
  and build it" without naming Ralph explicitly.
---

# ralph-implement

Advance the avalanche-rs milestone plan by one iteration: read the current
state, choose the next set of **independently implementable** plan items, build
them in parallel in isolated worktrees via subagents, verify each, and merge the
finished work back into the working tree. Along the way, keep `specs/` and
`plan/` honest — fold in what you learn.

This is the executable, repeatable form of `prompts/ralph-implement.md`. The
plan files (`plan/M*.md`) and conventions (`specs/00-overview-and-conventions.md`,
`AGENTS.md`, `CLAUDE.md`) are the source of truth; when this skill and those
files disagree, **the files win** — read them first.

## Why this shape

The plan is large and most milestone tasks are independent of one another. The
fastest *and* safest way to advance it is to fan out: each task gets its own
worktree so subagents can build and test without clobbering each other's
`target/` or each other's edits, and only verified work merges back. Doing the
items serially in the main tree wastes the parallelism the plan was structured
for; doing them in parallel *without* worktrees produces merge chaos and
cross-contaminated test runs. Worktrees + per-task subagents + verify-before-merge
is the combination that keeps throughput high and the main tree always-green.

## The loop (one iteration)

Work through these in order. Each step has a supporting skill — invoke it rather
than improvising.

### 1. Orient — read the current state

Study `specs/*.md` and `plan/*.md`. You don't need every file, but you need
enough to know **what is done, what is next, and what blocks what**:

- Skim the milestone plans (`plan/M*.md`) for the first tasks that are not yet
  marked complete. The auto-memory `MEMORY.md` index and `m*-progress` memories
  record the current convergence frontier — start there to avoid re-reading the
  whole plan.
- For each candidate task, read the spec section(s) it implements so the
  subagent you dispatch has the real requirements, not a paraphrase.

### 2. Select the next parallel batch

Pick the next set of items that can be implemented **at the same time without
shared state or sequential dependencies**. The litmus test: two tasks are
parallelizable if neither needs the other's output and they touch disjoint
crates/files (or disjoint enough that a clean merge is trivial).

- Prefer one task per crate — the proven pattern in this repo (see the
  `m*-progress` and `m1-storage-parallel-waves` memories).
- If a task depends on another in the same batch, drop it to the next wave.
- Keep the batch sized to what you can verify and merge confidently in one
  iteration — a handful, not the whole milestone.

When the choice of *which* batch isn't obvious from the plan (e.g. several
equally-ready waves, or a sequencing tradeoff), surface it briefly and pick a
recommendation rather than silently guessing.

### 3. Build each item in its own worktree, via subagents

Use **`superpowers:using-git-worktrees`** to create an isolated workspace per
task, **`superpowers:subagent-driven-development`** and
**`superpowers:dispatching-parallel-agents`** to dispatch one subagent per
worktree, and **`superpowers:test-driven-development`** for the implementation
discipline inside each.

Repo-specific worktree notes (learned the hard way — see the `m2`/`m7`/`m8`
progress memories):

- `isolation: "worktree"` on the Agent tool works and creates in-tree
  worktrees under `.claude/worktrees/`. Sibling-directory worktrees are denied
  by the sandbox.
- Subagents should **not** edit `specs/`, `plan/`, or the root `Cargo.toml` —
  those are merge-conflict magnets and are the orchestrator's job (steps 5–6).
  Keep each subagent scoped to its crate.
- A shared `CARGO_TARGET_DIR` across worktrees can clobber the main tree's test
  binaries. End the wave with a single full-workspace `nextest` run from the
  main tree, and `cargo clean -p <crate>` if a stale-binary `NotFound` appears.

Give each subagent: the task ID, the spec section(s) it implements, the target
crate, and the instruction to follow `AGENTS.md`/`CLAUDE.md` conventions
(license headers, lint passes, `lint-saevm` for SAE crates, `taulint`, etc.).

### 4. Verify before merging

Use **`superpowers:verification-before-completion`**. Nothing merges on the
strength of a subagent's say-so. For each finished worktree, confirm the
verification actually ran and passed — typically:

```sh
./scripts/run_task.sh lint        # or lint-saevm for ava-saevm* crates
./scripts/run_task.sh test-unit   # the touched crate(s)
```

If a task involves live/oracle gates, honor the **mandatory** oracle-binary
check first: `./scripts/check_oracle_binary.sh` must print `OK` (rebuild
`~/avalanchego` on FAIL) — see `CLAUDE.md`. Report failures with their output;
do not paper over a red run.

### 5. Merge finished work into the current tree

Merge each verified worktree back into the working tree. After merging the
batch, run one **full-workspace** `test-unit` from the main tree to catch
cross-task interactions that per-worktree runs can't see. Resolve any
root-`Cargo.toml`/workspace-member additions here, in the main tree, where you
have the whole picture.

### 6. Fold findings back into specs/ and plan/

This is what keeps the plan trustworthy across iterations:

- **specs/** — if implementation revealed something that **simplifies** the spec
  or a concrete **choice** worth recording (a decided-upon encoding, an
  edge-case resolution, a dropped ambiguity), update the spec. For drift coming
  *from upstream avalanchego*, don't rewrite prose — use the
  **`avalanchego-sync`** skill's "Upstream delta" callout convention instead.
- **plan/** — mark each task complete **only after** its verification passed in
  step 4/5. Record what landed (test counts, follow-ups deferred) the way the
  existing `plan/M*.md` entries and progress memories do.
- Consider updating the relevant `m*-progress` auto-memory so the next iteration
  starts from an accurate frontier.

## When to stop

One invocation = one iteration (orient → select → build → verify → merge →
fold). Stop after a batch merges green and the plan/specs/memories reflect it,
then report what landed and what the next ready batch looks like. If the user
asked for a continuous loop, the `ralph-loop` plugin (`/ralph-loop`) drives the
repetition; this skill is the body of a single pass.

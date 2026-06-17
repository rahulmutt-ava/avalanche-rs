---
name: avalanchego-sync
description: >-
  Sync the avalanche-rs specs/ and plan/ with the upstream avalanchego Go repo
  by reading the HEAD commit of the local ~/avalanchego checkout, computing the
  drift since the recorded spec pin, and folding each upstream change in as an
  "Upstream delta" callout (never rewriting spec prose) plus any plan tasks, then
  bumping the pin in specs/README.md. Use this whenever the user wants to re-sync
  / catch up the specs against newer avalanchego, asks about upstream drift, says
  the ~/avalanchego checkout advanced or was pulled, wants to bump or update the
  spec pin / upstream-provenance, fold in upstream commits, or mentions an
  "Upstream delta". Trigger even when the user just says "sync the specs" or
  "check what changed upstream" without naming the exact mechanism.
---

# avalanchego-sync

Keep the `avalanche-rs` specs (`specs/`) and milestone plans (`plan/`) current
with the upstream Go node by **folding** changes from the local `~/avalanchego`
checkout, not by rewriting the specs. The specs are a frozen, derived snapshot;
upstream changes are layered on top as dated callouts so the provenance trail
stays auditable and the original prose stays intact.

This is the executable version of the convention in `specs/README.md`
("Upstream provenance") and `specs/00-overview-and-conventions.md`. When this
skill and those files disagree, **the files win** — read them first.

## The core rule

> **Never rewrite spec prose to match upstream. Fold each upstream change in as
> an "Upstream delta" blockquote callout in the affected file(s), and add/extend
> plan tasks for anything that becomes implementation work.** Then bump the
> reviewed-through pin in `specs/README.md`.

Why: the specs were *generated from* a specific commit and reviewed as a whole.
Editing the body silently would destroy the "what did this look like at the pin"
baseline and make it impossible to tell derived-from-snapshot text apart from
later patches. Callouts keep both the original and the delta visible, each with
its own commit SHA and fold-date.

## Workflow

### 1. Peek — diagnose the drift (read-only)

Run the bundled script. It reads the pin from `specs/README.md`, the HEAD of the
checkout, and prints the commit range you must review plus the SAE/C-Chain/EVM
subset (the active stricter area) with per-commit file lists:

```sh
.claude/skills/avalanchego-sync/scripts/peek.sh            # uses ~/avalanchego
.claude/skills/avalanchego-sync/scripts/peek.sh /path/to/checkout
```

It writes nothing. If it reports **0 commits**, the specs are already in sync —
stop and tell the user. Do **not** trust `AVALANCHEGO_PATH` as the checkout —
that env var conventionally points at a built *binary*, not the source tree.

> **First, sanity-check the checkout.** A `git pull` advances HEAD without
> rebuilding, and a shallow clone may not contain the pin commit. If peek warns
> the pin "is not a commit in <SRC>", the checkout is too shallow or divergent —
> `git -C ~/avalanchego fetch --unshallow` (or fetch the pin) before continuing.
> This skill only edits docs, so the *binary* staleness that
> `scripts/check_oracle_binary.sh` guards is not your concern here (see §Scope).

### 2. Triage — map each commit to spec + plan files

For every commit in the range (section 3 of peek output), decide: does it change
something the specs assert? Categorize each as:

- **Spec-relevant** — changes a protocol constant, wire/codec format, formula,
  API shape, metric name, flag, invariant, lifecycle/ordering, or recovery
  behavior the specs document. These get a callout.
- **Implementation work** — a spec-relevant change that also needs Rust code.
  These additionally get a plan task (and the callout should point to it).
- **Non-gating** — behind an unscheduled fork (e.g. **Helicon**, the SAE fork —
  currently year-9999 on all networks), or CI/tooling/test-infra only. Still fold
  a callout if it touches documented behavior (so the trail is complete), and say
  it's dormant/non-gating; skip the plan task unless the user wants it staged.
- **Irrelevant** — repo chores, docs, unrelated subsystems with no spec surface.
  Note them in your summary so the user sees you considered them; no callout.

Use `git -C ~/avalanchego show <sha>` to read the actual diff before deciding —
the commit title alone is not enough. Pull the **PR number** from the title
(`(#5424)`) for the callout.

Map by subsystem — the file's own §/heading is where the callout goes:

| Touches… | Spec file(s) | Plan |
|----------|--------------|------|
| storage / DB / firewood | `04` | `M1` |
| networking / p2p / wire | `05`, `15` | `M2` |
| consensus / proposervm | `06` | `M3` |
| P-Chain / staking / ACP-77 / ACP-236 | `08` | `M4` |
| X-Chain | `09` | `M5` |
| C-Chain / EVM / coreth | `10` | `M6` |
| SAE / saevm / gas-as-time | `11`, `21` | `M7` |
| node / config / flags / API | `12`, `13`, `14` | `M8` |
| metrics / logging | `18` | (often no code; metric-name parity) |
| fee / economics math | `21` | matching VM milestone |
| determinism / clock | `24` | — |
| recovery / crash-consistency | `27` | — |
| interop / compat | `26` | `M9` |

Cross-cutting items that don't fit a milestone go in `plan/X-cross-cutting.md`.

### 3. Fold each spec-relevant commit as an "Upstream delta" callout

Insert a blockquote at the most relevant point in the affected spec file (next to
the prose it qualifies). Match the house style exactly — study existing examples
before writing, e.g.:

- `specs/11-saevm.md:360` (param/EVM-enforcement delta, Helicon-gated)
- `specs/11-saevm.md:288` (a refactor that changes a public signature)
- `specs/10-cchain-evm-reth.md:793` (a new verification step)
- `specs/18-metrics-and-logging.md:349` (added metrics)

Template:

```markdown
> **Upstream delta (avalanchego `<short-sha>`, #<PR> — folded <YYYY-MM-DD>).**
> <What changed in Go, in terms of the spec's own vocabulary. State the seam:
> which Go file/function, and the Rust analog (crate/type) where one is named.
> Call out anything that's a no-op in Rust, dormant behind an unscheduled fork,
> or a knock-on signature change. Keep it tight; this annotates, it doesn't
> re-document.>
```

Notes on style:
- Use the **short SHA** as it appears upstream (peek prints 10 chars; 7–10 is fine).
- Date is today's date (the fold date), not the commit date.
- For a multi-commit sweep of one area you may use a range header,
  e.g. `(avalanchego \`fb174e8\` → \`cc3b103b9\`, folded <date>)` — see
  `specs/11-saevm.md:763`.
- When a whole new subsection is warranted (not just an inline qualifier), add a
  `### N.x Upstream delta — <topic>` heading instead of a blockquote — see
  `specs/21-fee-economics-math.md:847`.
- Don't delete or reflow the surrounding original prose.

### 4. Add or extend plan tasks for implementation work

When a delta implies Rust work, add a task to the right `plan/M*.md` (or
`plan/X-cross-cutting.md`), tagged so it's findable, mirroring the existing
in-file convention — see `plan/M7-saevm.md:423` (M7.35–M7.37):

```markdown
### Task M7.NN: <imperative title> **[UPSTREAM DELTA — added <YYYY-MM-DD>]** ⬜ TODO
```

Use the next free task number in that milestone. Status markers follow the file's
existing vocabulary (`⬜ TODO`, `✅ DONE (<sha>)`, `🟡`, `BLOCKED on …`). Link the
task from the spec delta and vice-versa (task number ↔ spec §) so the two stay
discoverable from each other, and add the new task to that milestone's
spec-coverage / traceability table if it has one (see the table near
`plan/M7-saevm.md:507`). If a delta is non-gating (Helicon/unscheduled), say so in
the task and don't imply it blocks a milestone.

### 5. Bump the pin in `specs/README.md`

Update the "Upstream provenance" block:
- Set **reviewed-through** to the checkout HEAD short-SHA + its date.
- Add a one-paragraph summary of what this sync folded: the commits (SHA + #PR),
  which files/tasks they landed in, and which were judged non-gating/irrelevant.
- Keep the `generated-from` commit unchanged — that's the original snapshot and
  never moves.
- Leave the standing instruction ("When re-syncing against newer avalanchego,
  start the review from …") pointing at the **new** reviewed-through SHA.

Append, don't overwrite, the existing provenance history — each sync is a new
sentence/paragraph in the trail (see how the `cc3b103b91 → 0b0b57143c` sync is
recorded). Also update the file list in the provenance block if this sync touched
spec files not already listed there.

### 6. Report

Summarize for the user: the old→new pin, a one-line-per-commit table of where
each went (spec §, plan task, or "non-gating: reason" / "irrelevant: reason"),
and any judgment calls worth a second opinion (especially: is a fork really
unscheduled? does a "refactor" change wire bytes?). Don't run lint/tests — this
is a docs-only change; `taulint`/`lint-determinism` etc. don't apply unless you
also wrote code in a follow-up.

## Scope — what this skill does NOT do

- **It does not rebuild or move the Go oracle binary.** The binary-vs-checkout
  staleness guard (`scripts/check_oracle_binary.sh`) and the golden-vector corpus
  pin (`tests/vectors/manifest.json:avalanchego_revision`) are a *separate* sync
  concern — they gate live differential / recorded-oracle tests, not spec prose.
  Mention them only if the user is also re-extracting vectors or running live
  gates; bumping the spec pin does not require touching either.
- **It does not implement the Rust code** for a delta — it stages the work as a
  plan task. Actually building it is a normal TDD task afterward.
- **It does not rewrite or "modernize" spec text.** Folding only.

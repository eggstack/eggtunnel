---
name: plan
description: Navigate or update Eggtunnel's plans/ control surface (registry, roadmap, milestones, ADRs, closure evidence)
---

## What I do

Keep `plans/` honest. Eggtunnel plans in two separate horizons with strict
status and evidence rules, and the most common failure is describing planned
work as if it shipped.

## When to use me

Use me when asked to add/update a milestone plan, change a milestone status,
register or unregister work, assess whether a milestone can start, or write
closure evidence. Also use me before answering "what is the current milestone?"
or "is M0xx done?" — re-derive the answer, never recall it.

## Authority order (`plans/003-planning-process.md` §9)

Resolve conflicts in this order; later items never override earlier ones:

1. canonical specification + terminology (`plans/000`, `plans/001`)
2. accepted ADRs (`plans/adrs/`)
3. subsystem roadmap (`plans/subsystems/`)
4. milestone implementation plan (`plans/implementation/`)
5. current repository evidence

Repository evidence is **lowest**. If a plan disagrees with shipped code, the
plan is stale — not the code.

## Hard rules

- `plans/registry.md` is the control surface. It carries only canonical refs,
  status vocabulary, active roadmaps, dependency-ready plans, blockers, and
  recent closure records. Do **not** duplicate historical plan detail there.
- `plans/closure/` records are **immutable historical evidence**. Never rewrite
  a closed milestone's record to reflect later reality — add a new record. This
  is the single most-repeated rule in this repo's AGENTS.md and it still holds.
- Status vocabulary is fixed (`003` §17 / `registry.md`): proposed, ready,
  active, blocked, closing, closed, conditionally closed, superseded, archived.
  A milestone is **not** closed merely because implementation landed — that is
  `closing`, until a closure record is accepted.
- Dependency classes (`003` §7): hard, interface, soft, operational. A milestone
  is `ready` only when all **hard** dependencies are closed and interface
  dependencies have a stable written contract.
- The subsystem roadmap status table and `registry.md` duplicate each other.
  **Flip both in the same change** or the control surface lies.
- Never claim hosted CI evidence when only local commands were run (`003` §12).
  Distinguish: source inspection, unit/integration tests, repeated local tests,
  benchmarks, fuzzing, hosted CI, external interop, downstream integration.
- Canonical docs (`000`-`003`) and accepted ADRs are amendable only on direction
  change, discovered contradiction, an accepted ADR, or explicit user
  direction. Corrective implementation work alone is not justification.
- Long-term and interim horizons must stay separate. Implementation difficulty
  never silently weakens the long-term contract.

## Planning an unimplemented capability

Write the plan against what the code does **today**, and prove it:

- Name the exact current behavior being replaced (e.g. the CLI installing
  `RuntimePolicy::default()` and collapsing `BindPolicy` to one boolean).
- List the invariants that cannot regress, and an explicit out-of-scope list.
- Prefer lowering into existing validated library types over inventing
  CLI-side semantic validation. Do not create a second policy engine.
- Stop conditions: say when the work must escalate to a new plan or ADR.
- Require ordered work packages, focused tests, broad verification,
  documentation updates, acceptance criteria, and closure evidence.

## Writing a closure record

Include (`003` §11): milestone + plan, baseline, implementation commits, final
reviewed head, requirement-to-evidence matrix, exact commands run and outcomes,
feature/dependency evidence, security/resource/lifecycle evidence,
documentation evidence, known limitations, unresolved findings by severity, and
a disposition (closed / conditionally closed / corrective pass required /
blocked). Claim closure from evidence, never from prose.

## Traps

- A `blocked` plan's contents are **not** implemented. Do not document blocked
  work in `docs/` or `AGENTS.md`; those describe shipped behavior only.
- A milestone may reuse an earlier plan file (M013's publication event is
  recorded in `closure/.../013-status.md` against plan `012`). There is no
  `013-*.md` plan file; that gap is intentional, not a missing document.
- Cross-repo gates (e.g. M019 needs a published Eggress bounded-WebSocket API)
  cannot be satisfied by swapping in a git/path dependency.
- The archived C001 corrective addendum stays archived; new corrective work gets
  a fresh plan.

## Finish by re-deriving

After editing plans, confirm: registry status matches the plan file's own status
line, the roadmap table agrees, referenced plan/closure/ADR paths exist on disk,
and no living doc still calls a closed milestone open.

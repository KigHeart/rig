# AJENTIR*

**A cost-aware, durable, model-agnostic agent runtime — implemented in Rust.**

AJENTIR is a runtime substrate for executing agent loops and graph workflows with one durable super-step kernel. It treats the super-step as the unit of progress, state atomicity, recovery, context admission, model routing and cost accounting.

The asterisk marks an evolving engineering hypothesis. The v0.2 architecture is a design baseline, not a claim that its invariants or performance hypotheses have already been implemented or verified.

## Architecture baseline

**The normative design baseline is [AJENTIR v0.2](docs/architecture/ajentir_v02.pdf).** Its repository companion is [AJENTIR v0.2 — Architecture Baseline](docs/architecture/AJENTIR-V0.2-BASELINE.md).

The core architectural decisions are:

- One bulk-synchronous-parallel (BSP) kernel. Simple agent/tool loops and graph workflows are front-ends to the same kernel, not separate execution engines.
- Typed state channels with reducer laws, immutable per-node admitted views, and deterministic merge by node ID.
- Task-level write records and journaled model responses/effects, in addition to checkpoints.
- Compare-and-swap checkpoint commits with lease epochs and fencing.
- Explicit effect classes, idempotency keys and durable intent-before-execution for non-Pure effects.
- A cache-aware cost ledger, deterministic context admission, model routing, deferred tool loading, compaction and refinement budgets.
- Durable park/resume, single-use approval tokens, schema-versioned state and typed observability.
- Deterministic simulation, fault injection, model checking, store conformance and comparative evaluation as required verification work.

AJENTIR can execute multi-agent workflows: nodes may be model agents, tools, deterministic computations, verifiers or adapters to externally operated agents. It is not defined as an agent-output voting service. Its responsibility is to govern execution, state, recovery, costs and effects around that work.

## Target crate topology

The v0.2 target tiers are:

| Tier | Crates |
|---|---|
| T0 | `aj-core` |
| T1 | `aj-state`, `aj-store`, `aj-model` |
| T2 | `aj-kernel`, `aj-context`, `aj-tools`, `aj-router` |
| T3 | `aj-control`, `aj-telemetry` |
| T4 | `aj-sim`, `aj-eval` |

This is a target architecture, not a statement that these packages all exist today. The repository currently has a Rig-derived provider/tool foundation and an early AJENTIR state/checkpoint package. Package names, paths and dependency edges must be migrated deliberately, with workspace-wide compile and test evidence; do not mechanically rename the workspace to match the target diagram.

## Implementation sequence

1. **P0 — Kernel foundation:** core contracts, typed state, in-memory kernel and deterministic simulator. Exit: reducer laws and merge determinism under permuted completion order.
2. **P1 — Durable store:** store traits/conformance, SQLite, task records, CAS and leases. Exit: crash-at-every-await and peer-regression corpus pass.
3. **P2 — Model and ledger:** provider trait, manifests, request builder, cache planner, cost ledger and two providers. Exit: usage reconciliation and stable prefix across resume.
4. **P3 — Control and effects:** durable hooks, park/resume, budgets, Postgres and effect journal. Exit: simulator approval/park and state-write recovery hypotheses pass.
5. **P4 — Cost/context/tools:** router, context admission, deferred discovery and typed plan IR. Exit: admission, tool deferral and cache-aware routing experiments runnable against real providers.
6. **P5 — Evaluation:** adapters for peer runtimes and a comparative report scoring every hypothesis, including refutations.

Phases are ordered by dependency and testable exit criteria, not by feature count.

## Verification and claims

The seven kernel invariants (frontier completeness, interrupt progress, atomic visibility/fencing, exactly-once state application, deterministic merge, effect discipline, and admission purity/ledger monotonicity) are design obligations. They must be supported by TLA+ model checking, reducer property tests, crash-at-every-await simulation, deterministic seeds, store conformance, fuzzing and the evaluation harness.

Exactly-once **state application** is not exactly-once arbitrary external side effects. Unknown outcomes for irreversible operations park for reconciliation. No durability mode may silently downgrade when storage is unavailable.

Performance claims about lower cost per successful task, lower crash overhead, cache-aware routing or quality preservation remain hypotheses until measured under the v0.2 protocol.

## Current repository status

AJENTIR began as a fork of [Rig](https://github.com/0xPlaygrounds/rig). Rig-derived provider and tool integrations are implementation lineage and candidate adapters, not the public architecture itself. Some physical crate paths and internal Rust identifiers remain Rig-derived during the staged migration.

The early AJENTIR state/checkpoint implementation is useful groundwork, but it does not by itself implement the v0.2 BSP kernel, task-write journal, fenced distributed commit, model/effect journal, cost ledger, durable control plane or evaluation harness.

Keep this README and the v0.2 PDF synchronized through explicit architecture amendments. Do not describe target crates, invariants or hypotheses as shipped or verified until their acceptance gates pass.

## License

MIT. See [LICENSE](LICENSE) and inherited source notices.

# AJENTIR* v0.2 — Architecture Baseline

**Status:** normative design baseline for implementation planning  
**Source:** Kiprop Yego, *AJENTIR*: Runtime Architecture and Comparative Runtime Analysis, Version 0.2, 21 November 2026  
**Source document:** [ajentir_v02.pdf](./ajentir_v02.pdf)

This document translates the v0.2 design into repository-level implementation boundaries. It does not claim that the design is implemented or experimentally validated. The source document remains authoritative where this summary is abbreviated.

## 1. Product definition

AJENTIR is a cost-aware, durable, model-agnostic **agent runtime substrate**. It is not a role/crew framework, prompt library, ingestion platform, UI, retriever collection, or general-purpose coding-agent harness.

Its execution abstraction supports a simple model/tool loop and graph-shaped workflows through **one bulk-synchronous-parallel (BSP) super-step kernel**. A workflow node may represent a model agent, a tool, a deterministic computation, a verifier, or an adapter to an independently operated agent. AJENTIR may coordinate multiple agents, but its defining responsibility is the runtime semantics around their work—not merely collecting or voting on agent responses.

The central thesis is that the **super-step** is the unit of:
- progress and scheduling;
- state atomicity and reducer merge;
- checkpointing and recovery;
- model/context admission and routing;
- cost accounting and budget enforcement.

## 2. Design goals, in priority order

1. **Durability:** committed work survives process death, redeployment and scale-in; committed state transitions are not applied twice.
2. **Cost transparency:** decisions that affect token, tool, infrastructure or human-review cost are observable and configurable per super-step.
3. **Model agnosticism:** providers are abstracted; provider/model selection is a runtime concern and can vary by turn.
4. **Quality preservation:** cost optimizations are explicit and measured; no silent quality degradation.
5. **Inspectability:** typed, serializable, forkable, content-addressed state can be listed, diffed, edited and replayed.

## 3. Runtime model

A super-step executes these phases:

1. **Plan:** freeze the active frontier.
2. **Admit:** deterministically compile an immutable input view for each active node from the committed state, admission policy and step.
3. **Execute:** run nodes concurrently against their immutable views. Nodes produce write sets, effect records and optional interrupt/park requests; they cannot observe sibling writes from the same step.
4. **Barrier:** wait for completion, park or failure.
5. **Merge:** apply writes by canonical node ID order using the channel reducer.
6. **Route:** calculate the next frontier from the merged state and node results.
7. **Commit:** atomically persist the new state, advanced frontier and ledger.

Loop execution is a front-end compiled to the two-node cyclic agent↔tools graph. Graph execution is another front-end. There must not be separate durability implementations for loop and graph modes.

## 4. State and reducer contract

A channel is defined by its value type, reducer, identity, scope and context-admission class.

### Channel scopes
- `Step`: cleared at each barrier.
- `Run`: persisted in run checkpoints.
- `Thread`: shared across runs in a conversation/thread.
- `Principal`: stored separately under principal identity and referenced by checkpoints, not copied into them.

### Reducer laws
Every reducer must satisfy:
- identity: `e ⊕ x = x = x ⊕ e`;
- associativity: `(x ⊕ y) ⊕ z = x ⊕ (y ⊕ z)`.

Commutativity is not required because the kernel fixes fold order by node ID. Property-based tests must check reducer laws and split-fold equivalence. Erasure is permitted at the persistence boundary; the public channel and reducer APIs remain typed.

### Admission classes
`always`, `window(w)`, `on_reference`, `summary`, `retrieve(k)`, and `never`. Admission must be a pure deterministic function of committed state, policy and step. The ledger is not placed in the model prompt; it is queryable through runtime interfaces.

## 5. Checkpoint, task-write and commit protocol

A checkpoint records its identity and parent, thread/run, super-step, state-schema version, snapshot or delta, advanced frontier, interrupt marker and ledger snapshot. Its ID is content-addressed and checkpoints form a verifiable hash chain.

Each completed node execution appends a task record containing its run, step, node, attempt, write set, model responses, effects and ledger delta. Recovery uses existing task records rather than rerunning completed nodes.

Commit is a compare-and-swap against the expected thread head and current lease epoch. A stale epoch is rejected as a fencing failure. Only the committed head is externally visible.

### Seven runtime invariants

- **I1 Frontier completeness:** the committed frontier is exactly the work scheduled but not yet applied; it is empty iff terminal.
- **I2 Interrupt progress:** before/after interrupts have distinct checkpoint semantics; a consumed marker cannot re-raise on every resume.
- **I3 Atomic visibility and fencing:** state is visible only after commit; stale lease epochs cannot commit.
- **I4 Exactly-once state application:** at most one task record for each `(run, step, node)` contributes to committed state.
- **I5 Deterministic merge:** next state and frontier depend only on the previous checkpoint and task records, not task completion order.
- **I6 Effect discipline:** every non-Pure effect has a stable idempotency key; irreversible effects have durable intent before execution.
- **I7 Admission purity and ledger monotonicity:** admission is deterministic; each cost-ledger component is non-decreasing.

The crash-consistency proposition is conditional on these invariants and the stated node/journal assumptions. It does **not** promise bit-for-bit reproduction of an interrupted run when a task had no durable result record: such work may be sampled/executed again.

## 6. Effects and side-effect semantics

Tools declare one effect class:
- `Pure`: no external side effect.
- `Idempotent`: repeat-safe with the same idempotency key.
- `Compensable`: has a registered inverse/compensation.
- `Irreversible`: cannot safely be repeated or automatically undone.

Undeclared tools default to `Irreversible`.

For every non-Pure effect, persist an intent synchronously before invoking the external operation, then persist the result. A journaled completed result is returned on resume. An intent without a result for an irreversible operation parks the run for reconciliation; it must never be blindly repeated. This is not a claim of exactly-once effects against arbitrary external systems.

Completed model responses are also journaled by canonical request digest so a crash after provider completion does not automatically incur another inference charge.

## 7. Cost-aware model and context architecture

The cost ledger covers model input/output, cache write/read, tools, infrastructure and human review. Model prices and capabilities come from provider/model manifests, not hard-coded constants.

The following decisions are coupled and must be designed together:
- context admission and compaction;
- stable prompt-prefix construction and cache lifetime;
- deferred tool discovery/loading;
- model routing and provider-switch penalties;
- refinement/verifier decisions;
- budgets and parked-run resume costs.

Stable prompt segments are ordered before volatile input. The stable prefix is frozen per prefix epoch; changes explicitly advance the epoch and are recorded. Deferred tools are appended outside the stable prefix where possible. Compaction is scheduled using an explicit cost rule, not performed indiscriminately each turn.

The router selects a model satisfying the phase quality threshold while minimizing expected cost plus cache-aware model-switch cost. Refinement continues only while estimated marginal quality gain per marginal cost meets the task-class threshold, subject to hard token, money and iteration limits. Outcomes are explicit: `Accepted`, `BestEffort`, or `Escalated`.

Budgets are ledger channels and gates. Concurrent worst-case reservations are allocated deterministically by node ID, then reconciled to actual usage.

## 8. Tools and programmatic plans

Tool manifests include schema, schema token size, effect class, embedding and deferral hint. Discovery must not mutate the frozen stable prefix. Tool plans are declarative dataflow IR (call, map, filter, project, bounded reduce), type-checked before execution and interpreted by the runtime. Intermediate values are content-addressed; only declared projections are returned to the model. Irreversible calls within a plan pass the approval hook.

A `ProgramExecutor` trait is the boundary for provider-hosted or user-supplied execution environments. A general-purpose sandbox is not part of the core product.

## 9. Durable control, approvals and observability

Durable hooks exist at `BeforeModel`, `AfterModel`, `BeforeTool`, `AfterTool`, `BeforeCommit` and `OnBudget`. A hook returns `Allow`, `Deny`, `Rewrite`, `Park` or `Escalate`. Hook decisions are journaled results. `Park` is a durable run state, not a sleeping process.

An approval record binds run, step, node, canonical call digest, required roles, expiry and single-use nonce. Resume validates the actor, consumes the nonce, appends the decision to the audit chain, acquires a lease and continues. Expiry defaults to deny.

Checkpoints carry channel schema versions. Resume applies explicit version migrators or refuses to load; it must not silently reinterpret persisted state.

Typed runtime events include step/node lifecycle, model request and token usage, tool intent/result, write application, checkpoint commit, interrupt/park, budget tick and route decision. Events map to OpenTelemetry-compatible spans. The durable cost ledger is the crash-surviving counterpart to telemetry.

## 10. Failure model and deployment

Required handling includes provider 429/5xx with bounded retry and accounting; truncated streams discarded until terminal completion; poison steps quarantined; duplicate resume fenced; store failure stalls rather than silently downgrading durability; schema drift migrates or refuses; hash-chain corruption falls back to a verifiable ancestor with an explicit report; unknown irreversible outcomes park for reconciliation; hostile tool output never enters the stable prefix unfiltered.

Deployment targets:
- **Embedded:** in-process runtime with memory/SQLite and local leases.
- **Worker fleet:** stateless workers share Postgres/object storage; lease expiry permits another worker to resume.
- **Scale-to-zero:** parked/interrupted runs consume no compute and resume on any host after approval/webhook/timer.

Durability modes:
- **Sync (default):** task records and checkpoints durable before the next step; loss bounded to in-flight work.
- **Async:** persistence overlaps next step; loss bounded to one committed step.
- **Exit:** persist on exit/error/park; loss may span the run and is forbidden when irreversible tools are present.

Irreversible effect intents are synchronous in every mode.

## 11. Crate topology

The v0.2 tiered topology is the architectural target. Crates depend only on lower tiers; kernel crates use `#![forbid(unsafe_code)]`. Clock, randomness, network, store and model-provider I/O are trait boundaries so the runtime can execute under a deterministic simulator.

| Tier | Crate | Responsibility |
|---|---|---|
| T0 | `aj-core` | Shared identities, IDs, errors, primitive contracts |
| T1 | `aj-state` | Typed channels, reducers, derive macros |
| T1 | `aj-store` | Checkpoint/task/effect storage traits, conformance suite, backends |
| T1 | `aj-model` | Provider trait, manifests, request builder, cache planner |
| T2 | `aj-kernel` | Single BSP kernel, commit protocol, loop/graph front-ends |
| T2 | `aj-context` | Admission, prompt compilation, compaction |
| T2 | `aj-tools` | Registry, discovery, effect metadata, plan IR and dispatch |
| T2 | `aj-router` | Cache/switch-cost-aware model selection |
| T3 | `aj-control` | Durable hooks, park/resume, approvals and budgets |
| T3 | `aj-telemetry` | Typed events, traces and exported observability |
| T4 | `aj-sim` | Deterministic runtime simulator and fault injection |
| T4 | `aj-eval` | Workload harness, peer adapters, metrics and comparative reports |

Storage backends and model providers are feature-gated sub-crates beneath `aj-store` and `aj-model`, not dependencies of the kernel. The table is the v0.2 target topology, not a statement that these crates already exist.

## 12. Verification strategy

No invariant is claimed implemented merely because a type or test exists. Required verification:
1. TLA+ model of workers, append, CAS, lease expiry, crash/restart and duplicate resume; check I3–I5 and liveness under stated fairness assumptions.
2. Property-based reducer identity, associativity and split-fold tests.
3. Crash-at-every-await simulation checked against a failure-free oracle.
4. Seed-driven deterministic scheduling over simulated clock, network, store and provider.
5. Regression corpus for stale frontier, streaming checkpoint omissions, repeated interrupt markers and pending task writes.
6. One store conformance suite for CAS, fencing, ordering and retention.
7. Fuzz checkpoint decoding, plan IR and manifest parsing.

## 13. Evaluation baseline

Compare runtimes, not models. Fix model/version, temperature, prompt, tools and retry limits; report adapter fidelity; test both idiomatic defaults and documented tuned configurations; run at least 30 repetitions per cell; use Wilson intervals for success and bootstrap intervals for cost. Failed runs and retries count in cost per success.

Workloads:
- W1 single-agent tool loop (~50 tools, 10 turns);
- W2 hundreds of tools and deferred discovery;
- W3 parallel fan-out/fan-in reducer;
- W4 human approval with redeploy before decision;
- W5 random SIGKILL;
- W6 hundreds-turn long horizon.

Measure success, tokens/cost per success, admission ratio, prefix hit ratio, refinement share, crash overhead, recovery-point loss, time to resume, parked cost, routing regret, orchestration overhead and inspectability.

H1–H6 from v0.2 remain hypotheses, not results. Each must be measured and can be refuted. In particular, H6's randomized crash schedules test exactly-once **state write application**, not exactly-once arbitrary external effects.

## 14. Implementation phases and exit criteria

| Phase | Deliverables | Exit criterion |
|---|---|---|
| P0 | `aj-core`, `aj-state`, in-memory `aj-kernel`, `aj-sim` | Reducer laws and deterministic merge pass under permuted completion order |
| P1 | `aj-store` traits/conformance, SQLite, task records, CAS, leases | Crash-at-every-await and peer-bug corpus pass |
| P2 | `aj-model`, manifests/request builder/cache planner/ledger, two providers | Ledger reconciles provider usage; prefix remains frozen across resume |
| P3 | `aj-control`, hooks, park/resume, budgets, Postgres, effect journal | H5 and H6 pass in simulator |
| P4 | `aj-router`, `aj-context`, `aj-tools`, deferred loading and plan IR | H1, H3 and H4 runnable against real providers |
| P5 | `aj-eval` adapters and comparative benchmark | Baseline report scores every hypothesis, including failures |

Implementation is ordered by dependency and exit criteria, not by calendar date or feature count.

## 15. Open problems retained from v0.2

Online quality estimation; estimating remaining turns and prefix reuse; validating admission policies against task-specific information needs; cold-cache pricing after park; cryptographic erasure versus hash-chain integrity; cross-thread transactions; plan-IR expressiveness; and integration-specific reconciliation of unknown-outcome effects remain open. Do not silently present these as solved.

## 16. Repository mapping and migration constraints

The repository currently contains a Rig-derived provider/tool workspace and an early AJENTIR state/checkpoint crate. A Cargo package-identity migration is in progress; physical paths and Rust identifiers may remain Rig-derived temporarily. Do not rename or split packages mechanically to match the target table before inspecting dependency edges and proving the migration compiles.

Current implementation evidence must be recorded separately from this target architecture:
- Existing state/checkpoint code is a starting point for P0/P1, not proof that task-write journaling, lease fencing, effect journaling or the complete commit protocol exists.
- Existing provider/tool crates are candidates for the `aj-model` and `aj-tools` adapters, not automatically the final API.
- Agent manifests/Foundry are not a substitute for the BSP execution kernel; decide whether and how they map to node declarations after inspecting their actual consumers.

The uploaded PDF is retained as the full baseline. This repository document is a navigational and implementation-oriented companion; changes to the baseline require an explicit architecture decision and versioned amendment.

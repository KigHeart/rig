<div align="center">

# AJENTIR*

**A Rust-native agentic systems framework.**

*Typed state · Durable execution · Reducer-driven coordination · Explicit control*

[![Rust](https://img.shields.io/badge/language-Rust-orange?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Repository](https://img.shields.io/badge/GitHub-KigHeart%2Fajentir-181717?logo=github)](https://github.com/KigHeart/ajentir)

</div>

AJENTIR is being engineered as a Rust-native framework for building agents whose state, execution progress, and authority boundaries can be inspected and controlled.

The design combines a deliberately small agent loop with typed, reducer-driven state and durable execution. The goal is not another wrapper around model APIs. It is an execution substrate for agents that must survive interruption, resume from validated state, support human approval, and make their control flow explicit.

The asterisk in **AJENTIR*** marks an evolving engineering hypothesis. Capabilities described as planned or in progress are not represented as shipped functionality.

## Design principles

- **Rust-native contracts.** Use the type system to make invalid states harder to represent and interfaces explicit.
- **Small agent loop.** Keep the default execution model understandable: construct a model request, inspect its response, invoke authorized tools, incorporate results, and continue.
- **Reducer-driven state.** State changes are explicit operations with defined conflict and ordering semantics.
- **Durable execution.** Persist validated state and execution frontiers so interrupted work can be resumed deliberately.
- **Control is not capability.** A model proposal is not authority. Approval and policy checks must gate privileged operations.
- **Determinism where it matters.** Reducer application and state transitions must not depend on task completion order.
- **Honest recovery semantics.** Checkpointing does not make arbitrary external side effects exactly-once.

## Current architecture

The workspace is in transition from its Rig-derived foundation to the AJENTIR public architecture.

- `crates/ajentir` contains the first AJENTIR state and checkpoint substrate: typed channels, versioned schemas, reducers, deterministic update batches, frontier-aware checkpoints, fork-with-edit provenance, and in-memory/SQLite checkpointers.
- `rig-core`, `rig-agent`, and related provider/integration crates currently supply inherited model, tool, and integration capabilities. They are implementation foundations, not the intended public identity of the framework.
- `rig-foundry` remains a separate manifest/agent-identity component.

**Transition status:** the repository and product identity are now AJENTIR, but the root compatibility facade and many inherited crate names/APIs still carry Rig identifiers. They are retained during the migration to avoid silently breaking the existing workspace. They will be replaced or isolated behind AJENTIR-facing interfaces in deliberate, tested steps. This is not yet a claim that every crate has been renamed.

## Execution roadmap

1. Complete the typed state and checkpoint substrate.
2. Build the default loop runtime on top of that substrate.
3. Add durable interruption, approval, and resume semantics.
4. Introduce optional graph execution without making a graph mandatory for simple agents.
5. Add execution-ledger and policy-gated tool dispatch, with explicit limits around external side effects.
6. Migrate inherited provider and integration APIs behind AJENTIR-facing crates and documentation.

## What is not claimed yet

The current state/checkpoint slice does not implement a complete agent runtime, durable approval workflow, distributed leases or fencing, an execution ledger, or exactly-once execution of external side effects. The SQLite checkpointer is durable storage; the in-memory checkpointer is not. These boundaries are intentional and documented.

## Development

This is a Rust workspace. Start with the AJENTIR crate:

```bash
cargo test -p ajentir
cargo clippy -p ajentir --all-targets --all-features -- -D warnings
cargo fmt -- --check
```

The commands above are the intended local verification gates; their success is not implied by this README.

## Lineage and attribution

AJENTIR began from a fork of [Rig](https://github.com/0xPlaygrounds/rig), a Rust LLM application framework. The repository retains the upstream MIT licensing and attribution applicable to inherited code. AJENTIR's architecture, runtime semantics, and public identity are being developed as a distinct framework. See [LICENSE](LICENSE) and the relevant source notices.

## License

MIT. See [LICENSE](LICENSE).

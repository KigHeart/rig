# AJENTIR* substrate on Rig

AJENTIR* is the working design name for a durable agent-execution model being
prototyped inside Rig's `rig-foundry` crate. The asterisk is intentional:
this is an evolving design hypothesis, not a released framework.

The implementation keeps Rig responsible for model and tool interaction while
adding explicit state and recovery semantics around agent execution.

## Current implementation

- **Channel schemas:** versioned channel definitions select a reducer.
- **Reducers:** `LastWriteWins`, `AppendList`, `AppendMessages`, `MergeMap`,
  and `MaxByTimestamp`.
- **Atomic updates:** a malformed update batch is rejected without partially
  mutating the state.
- **Deterministic parallel reduction:** node results are applied in node-name
  order rather than task-completion order.
- **Frontier-aware checkpoints:** `Next` records the next super-step frontier;
  `Executing` records interrupted nodes that must be rerun; `Complete` is
  terminal and resumes no work.
- **Fork with edit:** a new run can branch from a checkpoint while preserving
  parent-checkpoint provenance.
- **Persistence:** `SqliteCheckpointer` stores complete validated snapshots.
  `MemoryCheckpointer` is for tests and ephemeral development only.

## Important semantics

A checkpoint is accepted only when its identifiers, state schema version,
channel values, and frontier are valid. Persisted data is validated again when
loaded; deserialization alone is not trusted.

An interrupted node is rerun because its state updates were not committed.
This does not make arbitrary external side effects exactly-once. That requires
the separate execution ledger, idempotency contracts, and reconciliation of
outcomes that become unknown after a crash.

Reducer application order is deterministic for node updates supplied to one
super-step. The graph scheduler must provide a complete, stable set of node
updates for that step before committing the resulting state.

## Not implemented yet

- Execution ledger and tool invocation identities.
- Durable approval/interrupt decisions and resume API.
- Distributed worker leases and fencing tokens.
- Graph scheduler and the default Rig-backed loop strategy.
- Exactly-once guarantees for arbitrary external tools.
- Multi-process concurrency policy and production database operations.

The SQLite backend is a durable single-database checkpoint store, not yet a
distributed coordination service. Do not treat it as a production-grade
multi-host worker lock.

## Minimal use

```rust
use rig_foundry::{
    ChannelDefinition, ChannelReducer, ChannelSchema, ChannelState,
    Checkpointer, Frontier, SqliteCheckpointer,
};
use std::collections::BTreeMap;

let mut channels = BTreeMap::new();
channels.insert(
    "messages".to_owned(),
    ChannelDefinition {
        reducer: ChannelReducer::AppendMessages,
    },
);
let schema = ChannelSchema::new(1, channels)?;
let mut state = ChannelState::new(schema);
let checkpoint = rig_foundry::Checkpoint {
    checkpoint_id: "checkpoint-0001".to_owned(),
    run_id: "run-0001".to_owned(),
    parent_checkpoint_id: None,
    state_schema_version: 1,
    super_step: 0,
    state,
    frontier: Frontier::next(vec!["analyst".to_owned()])?,
};
let store = SqliteCheckpointer::open("ajentir.sqlite")?;
store.save(checkpoint)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

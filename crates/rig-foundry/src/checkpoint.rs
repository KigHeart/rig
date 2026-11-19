//! Durable checkpoint records and frontier-aware resume semantics.

use crate::state::{ChannelState, StateError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

/// Work frontier persisted with a checkpoint.
///
/// `Next` is the work scheduled after a completed super-step. `Executing`
/// records the nodes that were in flight when an interrupt occurred; those
/// nodes must be re-run on resume because their updates were not committed.
/// `Complete` is terminal and must never restart the graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "nodes", rename_all = "snake_case")]
pub enum Frontier {
    Next(Vec<String>),
    Executing(Vec<String>),
    Complete,
}

impl Frontier {
    /// Build a canonical next frontier.
    pub fn next(nodes: impl IntoIterator<Item = String>) -> Result<Self, CheckpointError> {
        Self::from_nodes(nodes, true)
    }

    /// Build a canonical interrupted/executing frontier.
    pub fn executing(
        nodes: impl IntoIterator<Item = String>,
    ) -> Result<Self, CheckpointError> {
        Self::from_nodes(nodes, false)
    }

    fn from_nodes(
        nodes: impl IntoIterator<Item = String>,
        is_next: bool,
    ) -> Result<Self, CheckpointError> {
        let mut nodes = nodes.into_iter().collect::<Vec<_>>();
        if nodes.iter().any(String::is_empty) {
            return Err(CheckpointError::InvalidFrontier);
        }
        nodes.sort();
        if nodes.windows(2).any(|pair| pair.first() == pair.get(1)) {
            return Err(CheckpointError::InvalidFrontier);
        }
        if nodes.is_empty() {
            return Err(CheckpointError::InvalidFrontier);
        }
        if is_next {
            Ok(Self::Next(nodes))
        } else {
            Ok(Self::Executing(nodes))
        }
    }

    /// Return the nodes that must be scheduled when this checkpoint resumes.
    pub fn resume_nodes(&self) -> &[String] {
        match self {
            Self::Next(nodes) | Self::Executing(nodes) => nodes,
            Self::Complete => &[],
        }
    }

    /// Whether this checkpoint represents a finished run.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    fn validate(&self) -> Result<(), CheckpointError> {
        let nodes = match self {
            Self::Next(nodes) | Self::Executing(nodes) => nodes,
            Self::Complete => return Ok(()),
        };
        if nodes.iter().any(String::is_empty)
            || nodes.windows(2).any(|pair| pair.first() >= pair.get(1))
            || (matches!(self, Self::Executing(_)) && nodes.is_empty())
        {
            return Err(CheckpointError::InvalidFrontier);
        }
        Ok(())
    }
}

/// Immutable, versioned snapshot sufficient to recover a run's state and work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub checkpoint_id: String,
    pub run_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub state_schema_version: u32,
    pub super_step: u64,
    pub state: ChannelState,
    pub frontier: Frontier,
}

impl Checkpoint {
    /// Validate identifiers, schema compatibility and frontier canonical form.
    pub fn validate(&self) -> Result<(), CheckpointError> {
        if self.checkpoint_id.is_empty() || self.run_id.is_empty() {
            return Err(CheckpointError::InvalidIdentifier);
        }
        if self.state_schema_version == 0
            || self.state_schema_version != self.state.schema().version
        {
            return Err(CheckpointError::SchemaVersionMismatch);
        }
        if self
            .parent_checkpoint_id
            .as_ref()
            .is_some_and(String::is_empty)
        {
            return Err(CheckpointError::InvalidIdentifier);
        }
        self.frontier.validate()
    }

    /// Fork this snapshot into a new run and replace one channel value.
    ///
    /// The original checkpoint is unchanged. The new checkpoint retains
    /// provenance to its parent and inherits the parent's execution frontier.
    pub fn fork_with_edit(
        &self,
        new_run_id: impl Into<String>,
        new_checkpoint_id: impl Into<String>,
        channel: &str,
        value: serde_json::Value,
    ) -> Result<Self, CheckpointError> {
        self.validate()?;
        let mut state = self.state.clone();
        state.set_for_fork(channel, value)?;
        let fork = Self {
            checkpoint_id: new_checkpoint_id.into(),
            run_id: new_run_id.into(),
            parent_checkpoint_id: Some(self.checkpoint_id.clone()),
            state_schema_version: self.state_schema_version,
            super_step: self.super_step,
            state,
            frontier: self.frontier.clone(),
        };
        fork.validate()?;
        Ok(fork)
    }
}

/// Persistence contract for durable run checkpoints.
pub trait Checkpointer: Send + Sync {
    /// Persist a checkpoint. Repeating the same checkpoint is idempotent.
    fn save(&self, checkpoint: Checkpoint) -> Result<(), CheckpointError>;

    /// Load a checkpoint by run and checkpoint identity.
    fn load(&self, run_id: &str, checkpoint_id: &str) -> Result<Checkpoint, CheckpointError>;
}

/// In-memory checkpointer for tests and ephemeral development.
///
/// This implementation is deliberately not durable across process death.
/// Production deployments must use a persistent backend.
#[derive(Clone, Default)]
pub struct MemoryCheckpointer {
    records: Arc<RwLock<BTreeMap<(String, String), Checkpoint>>>,
}

impl Checkpointer for MemoryCheckpointer {
    fn save(&self, checkpoint: Checkpoint) -> Result<(), CheckpointError> {
        checkpoint.validate()?;
        let key = (
            checkpoint.run_id.clone(),
            checkpoint.checkpoint_id.clone(),
        );
        let mut records = self
            .records
            .write()
            .map_err(|_| CheckpointError::StorageUnavailable)?;

        if let Some(existing) = records.get(&key) {
            if existing == &checkpoint {
                return Ok(());
            }
            return Err(CheckpointError::CheckpointConflict);
        }

        records.insert(key, checkpoint);
        Ok(())
    }

    fn load(&self, run_id: &str, checkpoint_id: &str) -> Result<Checkpoint, CheckpointError> {
        let records = self
            .records
            .read()
            .map_err(|_| CheckpointError::StorageUnavailable)?;
        records
            .get(&(run_id.to_owned(), checkpoint_id.to_owned()))
            .cloned()
            .ok_or(CheckpointError::NotFound)
    }
}

/// Errors raised by checkpoint validation or persistence.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CheckpointError {
    #[error("checkpoint identifier or run identifier is empty")]
    InvalidIdentifier,
    #[error("checkpoint state schema version does not match the state")]
    SchemaVersionMismatch,
    #[error("checkpoint frontier is empty, duplicated, or not canonical")]
    InvalidFrontier,
    #[error("checkpoint does not exist")]
    NotFound,
    #[error("checkpoint identity already exists with different contents")]
    CheckpointConflict,
    #[error("checkpoint storage is unavailable")]
    StorageUnavailable,
    #[error(transparent)]
    State(#[from] StateError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChannelDefinition, ChannelReducer, ChannelSchema};
    use std::collections::BTreeMap;

    fn checkpoint(frontier: Frontier) -> Checkpoint {
        let mut channels = BTreeMap::new();
        channels.insert(
            "messages".to_owned(),
            ChannelDefinition {
                reducer: ChannelReducer::AppendMessages,
            },
        );
        let schema = ChannelSchema::new(1, channels);
        assert!(schema.is_ok());
        let state = match schema {
            Ok(schema) => ChannelState::new(schema),
            Err(_) => return Checkpoint {
                checkpoint_id: "cp-1".to_owned(),
                run_id: "run-1".to_owned(),
                parent_checkpoint_id: None,
                state_schema_version: 1,
                super_step: 0,
                state: ChannelState::new(ChannelSchema {
                    version: 1,
                    channels: BTreeMap::new(),
                }),
                frontier,
            },
        };
        Checkpoint {
            checkpoint_id: "cp-1".to_owned(),
            run_id: "run-1".to_owned(),
            parent_checkpoint_id: None,
            state_schema_version: 1,
            super_step: 4,
            state,
            frontier,
        }
    }

    #[test]
    fn frontier_constructors_canonicalize_node_order() {
        let frontier = Frontier::next(vec!["z".to_owned(), "a".to_owned()]);
        assert_eq!(
            frontier,
            Ok(Frontier::Next(vec!["a".to_owned(), "z".to_owned()]))
        );
    }

    #[test]
    fn frontier_rejects_duplicate_nodes() {
        assert_eq!(
            Frontier::next(vec!["node".to_owned(), "node".to_owned()]),
            Err(CheckpointError::InvalidFrontier)
        );
    }

    #[test]
    fn interrupted_frontier_resumes_executing_nodes() {
        let frontier = Frontier::executing(vec!["b".to_owned(), "a".to_owned()]);
        assert_eq!(
            frontier.as_ref().map(Frontier::resume_nodes),
            Ok(&["a".to_owned(), "b".to_owned()][..])
        );
    }

    #[test]
    fn completed_frontier_never_restarts() {
        let frontier = Frontier::Complete;
        assert!(frontier.is_complete());
        assert!(frontier.resume_nodes().is_empty());
    }

    #[test]
    fn memory_checkpointer_saves_and_loads_idempotently() {
        let store = MemoryCheckpointer::default();
        let value = checkpoint(Frontier::next(vec!["analyse".to_owned()]).unwrap_or(Frontier::Complete));
        assert!(store.save(value.clone()).is_ok());
        assert!(store.save(value.clone()).is_ok());
        assert_eq!(store.load("run-1", "cp-1"), Ok(value));
    }

    #[test]
    fn checkpoint_identity_cannot_be_reused_for_different_contents() {
        let store = MemoryCheckpointer::default();
        let first = checkpoint(Frontier::next(vec!["analyse".to_owned()]).unwrap_or(Frontier::Complete));
        let second = checkpoint(Frontier::next(vec!["report".to_owned()]).unwrap_or(Frontier::Complete));
        assert!(store.save(first).is_ok());
        assert_eq!(store.save(second), Err(CheckpointError::CheckpointConflict));
    }

    #[test]
    fn fork_with_edit_preserves_parent_and_original_state() {
        let original = checkpoint(Frontier::next(vec!["analyse".to_owned()]).unwrap_or(Frontier::Complete));
        let fork = original.fork_with_edit(
            "run-2",
            "cp-2",
            "messages",
            serde_json::json!([{"role": "user", "content": "edited"}]),
        );
        assert!(fork.is_ok());
        if let Ok(fork) = fork {
            assert_eq!(fork.run_id, "run-2");
            assert_eq!(fork.parent_checkpoint_id.as_deref(), Some("cp-1"));
            assert_eq!(fork.state.get("messages"), Some(&serde_json::json!([{"role": "user", "content": "edited"}])));
            assert!(original.state.get("messages").is_none());
        }
    }

    #[test]
    fn checkpoint_rejects_schema_version_mismatch() {
        let mut value = checkpoint(Frontier::Complete);
        value.state_schema_version = 2;
        assert_eq!(value.validate(), Err(CheckpointError::SchemaVersionMismatch));
    }

    #[test]
    fn memory_checkpointer_does_not_claim_durability() {
        let store = MemoryCheckpointer::default();
        let value = checkpoint(Frontier::Complete);
        assert!(store.save(value).is_ok());
        // This test documents the in-memory backend's contract; SQLite is a
        // separate persistent implementation to be added in a later slice.
        assert!(store.load("run-1", "cp-1").is_ok());
    }
}

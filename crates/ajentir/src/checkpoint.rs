//! Durable checkpoint records and frontier-aware resume semantics.

use crate::state::{ChannelState, StateError};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
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
        if nodes.iter().any(|node| !valid_identifier(node)) {
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
        if nodes.is_empty()
            || nodes.iter().any(|node| !valid_identifier(node))
            || nodes.windows(2).any(|pair| pair.first() >= pair.get(1))
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
    pub parent_run_id: Option<String>,
    pub parent_checkpoint_id: Option<String>,
    pub state_schema_version: u32,
    pub super_step: u64,
    pub state: ChannelState,
    pub frontier: Frontier,
}

impl Checkpoint {
    /// Validate identifiers, schema compatibility and frontier canonical form.
    pub fn validate(&self) -> Result<(), CheckpointError> {
        if !valid_identifier(&self.checkpoint_id) || !valid_identifier(&self.run_id) {
            return Err(CheckpointError::InvalidIdentifier);
        }
        if self.state_schema_version == 0
            || self.state_schema_version != self.state.schema().version
        {
            return Err(CheckpointError::SchemaVersionMismatch);
        }
        match (&self.parent_run_id, &self.parent_checkpoint_id) {
            (Some(run_id), Some(checkpoint_id))
                if valid_identifier(run_id) && valid_identifier(checkpoint_id) => {}
            (None, None) => {}
            _ => return Err(CheckpointError::InvalidParentReference),
        }
        self.state.validate()?;
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
        let new_run_id = new_run_id.into();
        let new_checkpoint_id = new_checkpoint_id.into();
        if !valid_identifier(&new_run_id)
            || !valid_identifier(&new_checkpoint_id)
            || new_run_id == self.run_id
        {
            return Err(CheckpointError::InvalidForkIdentity);
        }
        let mut state = self.state.clone();
        state.set_for_fork(channel, value)?;
        let fork = Self {
            checkpoint_id: new_checkpoint_id,
            run_id: new_run_id,
            parent_run_id: Some(self.run_id.clone()),
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

/// SQLite-backed checkpointer that survives process restarts.
///
/// Each checkpoint is stored as a complete, validated snapshot. The composite
/// primary key prevents a run from silently reusing a checkpoint identity.
pub struct SqliteCheckpointer {
    connection: Mutex<Connection>,
}

impl SqliteCheckpointer {
    /// Open or create a persistent SQLite checkpoint database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CheckpointError> {
        let connection = Connection::open(path)
            .map_err(|error| CheckpointError::Database(error.to_string()))?;
        Self::from_connection(connection)
    }

    /// Create an in-memory SQLite database for tests.
    pub fn open_in_memory() -> Result<Self, CheckpointError> {
        let connection = Connection::open_in_memory()
            .map_err(|error| CheckpointError::Database(error.to_string()))?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> Result<Self, CheckpointError> {
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 CREATE TABLE IF NOT EXISTS ajentir_checkpoints (
                     run_id TEXT NOT NULL,
                     checkpoint_id TEXT NOT NULL,
                     super_step INTEGER NOT NULL,
                     payload TEXT NOT NULL,
                     PRIMARY KEY (run_id, checkpoint_id)
                 );",
            )
            .map_err(|error| CheckpointError::Database(error.to_string()))?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
}

impl Checkpointer for SqliteCheckpointer {
    fn save(&self, checkpoint: Checkpoint) -> Result<(), CheckpointError> {
        checkpoint.validate()?;
        let payload = serde_json::to_string(&checkpoint)
            .map_err(|error| CheckpointError::Serialization(error.to_string()))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| CheckpointError::StorageUnavailable)?;

        let super_step = i64::try_from(checkpoint.super_step)
            .map_err(|_| CheckpointError::SuperStepOutOfRange)?;
        let inserted = connection
            .execute(
                "INSERT OR IGNORE INTO ajentir_checkpoints
                 (run_id, checkpoint_id, super_step, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    checkpoint.run_id,
                    checkpoint.checkpoint_id,
                    super_step,
                    payload
                ],
            )
            .map_err(|error| CheckpointError::Database(error.to_string()))?;

        if inserted == 1 {
            return Ok(());
        }

        let existing_payload = connection
            .query_row(
                "SELECT payload FROM ajentir_checkpoints
                 WHERE run_id = ?1 AND checkpoint_id = ?2",
                params![checkpoint.run_id, checkpoint.checkpoint_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| CheckpointError::Database(error.to_string()))?
            .ok_or(CheckpointError::CheckpointConflict)?;
        if existing_payload == payload {
            Ok(())
        } else {
            Err(CheckpointError::CheckpointConflict)
        }
    }

    fn load(&self, run_id: &str, checkpoint_id: &str) -> Result<Checkpoint, CheckpointError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CheckpointError::StorageUnavailable)?;
        let payload = connection
            .query_row(
                "SELECT payload FROM ajentir_checkpoints
                 WHERE run_id = ?1 AND checkpoint_id = ?2",
                params![run_id, checkpoint_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| CheckpointError::Database(error.to_string()))?
            .ok_or(CheckpointError::NotFound)?;
        let checkpoint: Checkpoint = serde_json::from_str(&payload)
            .map_err(|error| CheckpointError::Serialization(error.to_string()))?;
        checkpoint.validate()?;
        if checkpoint.run_id != run_id || checkpoint.checkpoint_id != checkpoint_id {
            return Err(CheckpointError::InvalidIdentifier);
        }
        Ok(checkpoint)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// Errors raised by checkpoint validation or persistence.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CheckpointError {
    #[error("checkpoint or run identifier is invalid")]
    InvalidIdentifier,
    #[error("parent run and checkpoint identifiers must be present together and valid")]
    InvalidParentReference,
    #[error("a fork must use a new, valid run identity")]
    InvalidForkIdentity,
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
    #[error("checkpoint database error: {0}")]
    Database(String),
    #[error("checkpoint serialization error: {0}")]
    Serialization(String),
    #[error("super-step counter exceeds SQLite's signed integer range")]
    SuperStepOutOfRange,
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
                parent_run_id: None,
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
            parent_run_id: None,
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
        let frontier = Frontier::next(vec!["analyse".to_owned()]);
        assert!(frontier.is_ok());
        let value = checkpoint(frontier.unwrap_or(Frontier::Complete));
        assert!(store.save(value.clone()).is_ok());
        assert!(store.save(value.clone()).is_ok());
        assert_eq!(store.load("run-1", "cp-1"), Ok(value));
    }

    #[test]
    fn checkpoint_identity_cannot_be_reused_for_different_contents() {
        let store = MemoryCheckpointer::default();
        let first_frontier = Frontier::next(vec!["analyse".to_owned()]);
        let second_frontier = Frontier::next(vec!["report".to_owned()]);
        assert!(first_frontier.is_ok() && second_frontier.is_ok());
        let first = checkpoint(first_frontier.unwrap_or(Frontier::Complete));
        let second = checkpoint(second_frontier.unwrap_or(Frontier::Complete));
        assert!(store.save(first).is_ok());
        assert_eq!(store.save(second), Err(CheckpointError::CheckpointConflict));
    }

    #[test]
    fn fork_with_edit_preserves_parent_and_original_state() {
        let frontier = Frontier::next(vec!["analyse".to_owned()]);
        assert!(frontier.is_ok());
        let original = checkpoint(frontier.unwrap_or(Frontier::Complete));
        let fork = original.fork_with_edit(
            "run-2",
            "cp-2",
            "messages",
            serde_json::json!([{"role": "user", "content": "edited"}]),
        );
        assert!(fork.is_ok());
        if let Ok(fork) = fork {
            assert_eq!(fork.run_id, "run-2");
            assert_eq!(fork.parent_run_id.as_deref(), Some("run-1"));
            assert_eq!(fork.parent_checkpoint_id.as_deref(), Some("cp-1"));
            assert_eq!(
                fork.state.get("messages"),
                Some(&serde_json::json!([{"role": "user", "content": "edited"}]))
            );
            assert!(original.state.get("messages").is_none());
        }
    }


    #[test]
    fn fork_requires_a_new_run_identity() {
        let original = checkpoint(Frontier::Complete);
        assert_eq!(
            original.fork_with_edit(
                "run-1",
                "cp-2",
                "messages",
                serde_json::json!([]),
            ),
            Err(CheckpointError::InvalidForkIdentity)
        );
    }

    #[test]
    fn checkpoint_rejects_unpaired_parent_identity() {
        let mut value = checkpoint(Frontier::Complete);
        value.parent_run_id = Some("parent-run".to_owned());
        assert_eq!(
            value.validate(),
            Err(CheckpointError::InvalidParentReference)
        );
    }

    #[test]
    fn checkpoint_rejects_schema_version_mismatch() {
        let mut value = checkpoint(Frontier::Complete);
        value.state_schema_version = 2;
        assert_eq!(value.validate(), Err(CheckpointError::SchemaVersionMismatch));
    }

    #[test]
    fn sqlite_checkpointer_survives_database_reopen() {
        let path = std::env::temp_dir().join(format!(
            "ajentir-checkpoint-{}.sqlite",
            std::process::id()
        ));
        let frontier = Frontier::next(vec!["analyse".to_owned()]);
        assert!(frontier.is_ok());
        let value = checkpoint(frontier.unwrap_or(Frontier::Complete));
        let saved = SqliteCheckpointer::open(&path).and_then(|store| {
            let result = store.save(value.clone());
            drop(store);
            result
        });
        assert!(saved.is_ok());

        let loaded = SqliteCheckpointer::open(&path)
            .and_then(|store| store.load("run-1", "cp-1"));
        assert_eq!(loaded, Ok(value));
        assert!(std::fs::remove_file(path).is_ok());
    }


    #[test]
    fn checkpoint_rejects_corrupted_persisted_channel_values() {
        let original = checkpoint(Frontier::Complete);
        let serialized = serde_json::to_value(original);
        assert!(serialized.is_ok());
        if let Ok(mut value) = serialized {
            if let Some(state) = value.get_mut("state").and_then(serde_json::Value::as_object_mut) {
                if let Some(values) = state
                    .get_mut("values")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    values.insert("messages".to_owned(), serde_json::json!({"not": "a list"}));
                }
            }
            let restored = serde_json::from_value::<Checkpoint>(value);
            assert!(restored.is_ok());
            if let Ok(restored) = restored {
                assert_eq!(
                    restored.validate(),
                    Err(CheckpointError::State(StateError::InvalidValueShape(
                        "messages".to_owned(),
                    )))
                );
            }
        }
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

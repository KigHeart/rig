//! Typed channel state and deterministic reducer semantics for AJENTIR*.
//!
//! State updates are applied transactionally: either every update in a batch is
//! valid and committed, or the state remains unchanged. Channel and update
//! iteration use BTreeMap ordering so reducer application is reproducible.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// Reducer selected by the state schema for a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelReducer {
    /// Replace the previous value with the newest update.
    LastWriteWins,
    /// Append incoming array elements to the existing array.
    AppendList,
    /// Append incoming message records to the existing array.
    AppendMessages,
    /// Merge object keys; incoming values replace existing values for a key.
    MergeMap,
    /// Keep the value with the greatest numeric `timestamp` field.
    MaxByTimestamp,
}

/// Schema declaration for one state channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelDefinition {
    pub reducer: ChannelReducer,
}

/// Versioned schema for a run's state channels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSchema {
    pub version: u32,
    pub channels: BTreeMap<String, ChannelDefinition>,
}

impl ChannelSchema {
    /// Validate a schema, including schemas reconstructed from persisted data.
    pub fn validate(&self) -> Result<(), StateError> {
        if self.version == 0 {
            return Err(StateError::InvalidSchemaVersion);
        }
        if self.channels.is_empty() {
            return Err(StateError::EmptySchema);
        }
        for name in self.channels.keys() {
            validate_channel_name(name)?;
        }
        Ok(())
    }

    /// Construct a schema from a map of channel names to reducer definitions.
    pub fn new(
        version: u32,
        channels: BTreeMap<String, ChannelDefinition>,
    ) -> Result<Self, StateError> {
        let schema = Self { version, channels };
        schema.validate()?;
        Ok(schema)
    }

    fn definition(&self, name: &str) -> Result<&ChannelDefinition, StateError> {
        self.channels
            .get(name)
            .ok_or_else(|| StateError::UnknownChannel(name.to_owned()))
    }
}

/// Current values associated with a channel schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelState {
    schema: ChannelSchema,
    values: BTreeMap<String, serde_json::Value>,
}

impl ChannelState {
    /// Create an empty state for the supplied schema.
    pub fn new(schema: ChannelSchema) -> Self {
        Self {
            schema,
            values: BTreeMap::new(),
        }
    }

    /// Validate the complete state after deserialization or before persistence.
    pub fn validate(&self) -> Result<(), StateError> {
        self.schema.validate()?;
        for (name, value) in &self.values {
            let definition = self.schema.definition(name)?;
            validate_value_shape(name, definition.reducer, value)?;
        }
        Ok(())
    }

    /// Return the schema used to validate and reduce this state.
    pub fn schema(&self) -> &ChannelSchema {
        &self.schema
    }

    /// Read a channel value without mutating the state.
    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.values.get(name)
    }

    /// Read all populated channel values.
    pub fn values(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.values
    }

    /// Apply a batch of updates atomically in channel-name order.
    ///
    /// The batch is first reduced into a private candidate map. If any
    /// channel is unknown or any value violates its reducer's shape, no
    /// changes from the batch are committed.
    pub fn apply_updates(
        &mut self,
        updates: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), StateError> {
        let mut candidate = self.values.clone();

        for (name, incoming) in updates {
            let definition = self.schema.definition(&name)?;
            validate_value_shape(name.as_str(), definition.reducer, &incoming)?;

            let reduced = match candidate.get(&name) {
                Some(current) => reduce(&name, definition.reducer, current, &incoming)?,
                None => incoming,
            };
            candidate.insert(name, reduced);
        }

        self.values = candidate;
        Ok(())
    }

    /// Replace one channel directly, intended for an explicit fork-and-edit.
    ///
    /// This bypasses reducer combination but not schema or value validation.
    pub fn set_for_fork(
        &mut self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<(), StateError> {
        let definition = self.schema.definition(name)?;
        validate_value_shape(name, definition.reducer, &value)?;
        self.values.insert(name.to_owned(), value);
        Ok(())
    }
}

/// Errors raised by channel schema validation or reduction.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StateError {
    #[error("channel schema version must be greater than zero")]
    InvalidSchemaVersion,
    #[error("channel schema must declare at least one channel")]
    EmptySchema,
    #[error("invalid channel name: {0}")]
    InvalidChannelName(String),
    #[error("unknown channel: {0}")]
    UnknownChannel(String),
    #[error("invalid value shape for channel {0}")]
    InvalidValueShape(String),
    #[error("equal timestamps contain conflicting values in channel {0}")]
    TimestampConflict(String),
}

fn validate_channel_name(name: &str) -> Result<(), StateError> {
    if name.is_empty()
        || name.len() > 128
        || !name.is_ascii()
        || name.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(StateError::InvalidChannelName(name.to_owned()));
    }
    Ok(())
}

fn validate_value_shape(
    name: &str,
    reducer: ChannelReducer,
    value: &serde_json::Value,
) -> Result<(), StateError> {
    let valid = match reducer {
        ChannelReducer::AppendList | ChannelReducer::AppendMessages => value.is_array(),
        ChannelReducer::MergeMap => value.is_object(),
        ChannelReducer::MaxByTimestamp => value
            .as_object()
            .and_then(|object| object.get("timestamp"))
            .and_then(timestamp_as_i128)
            .is_some(),
        ChannelReducer::LastWriteWins => true,
    };

    if valid {
        Ok(())
    } else {
        Err(StateError::InvalidValueShape(name.to_owned()))
    }
}

fn timestamp_as_i128(value: &serde_json::Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

fn reduce(
    channel: &str,
    reducer: ChannelReducer,
    current: &serde_json::Value,
    incoming: &serde_json::Value,
) -> Result<serde_json::Value, StateError> {
    match reducer {
        ChannelReducer::LastWriteWins => Ok(incoming.clone()),
        ChannelReducer::AppendList | ChannelReducer::AppendMessages => {
            let mut combined = current.as_array().cloned().unwrap_or_default();
            if let Some(items) = incoming.as_array() {
                combined.extend(items.iter().cloned());
            }
            Ok(serde_json::Value::Array(combined))
        }
        ChannelReducer::MergeMap => {
            let mut combined = current.as_object().cloned().unwrap_or_default();
            if let Some(entries) = incoming.as_object() {
                for (key, value) in entries {
                    combined.insert(key.clone(), value.clone());
                }
            }
            Ok(serde_json::Value::Object(combined))
        }
        ChannelReducer::MaxByTimestamp => {
            let current_object = current.as_object();
            let incoming_object = incoming.as_object();
            let current_timestamp = current_object
                .and_then(|object| object.get("timestamp"))
                .and_then(timestamp_as_i128);
            let incoming_timestamp = incoming_object
                .and_then(|object| object.get("timestamp"))
                .and_then(timestamp_as_i128);

            match (current_timestamp, incoming_timestamp) {
                (Some(current_time), Some(incoming_time)) if incoming_time > current_time => {
                    Ok(incoming.clone())
                }
                (Some(current_time), Some(incoming_time)) if incoming_time < current_time => {
                    Ok(current.clone())
                }
                (Some(_), Some(_)) if current == incoming => Ok(current.clone()),
                (Some(_), Some(_)) => Err(StateError::TimestampConflict(channel.to_owned())),
                _ => Err(StateError::InvalidValueShape(channel.to_owned())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(reducer: ChannelReducer) -> ChannelState {
        let mut channels = BTreeMap::new();
        channels.insert("data".to_owned(), ChannelDefinition { reducer });
        let schema = ChannelSchema::new(1, channels);
        assert!(schema.is_ok());
        match schema {
            Ok(schema) => ChannelState::new(schema),
            Err(_) => ChannelState::new(ChannelSchema {
                version: 1,
                channels: BTreeMap::new(),
            }),
        }
    }

    fn update(value: serde_json::Value) -> BTreeMap<String, serde_json::Value> {
        let mut updates = BTreeMap::new();
        updates.insert("data".to_owned(), value);
        updates
    }

    #[test]
    fn append_list_preserves_update_order() {
        let mut state = state_with(ChannelReducer::AppendList);
        assert!(state.apply_updates(update(serde_json::json!([1, 2]))).is_ok());
        assert!(state.apply_updates(update(serde_json::json!([3]))).is_ok());
        assert_eq!(state.get("data"), Some(&serde_json::json!([1, 2, 3])));
    }

    #[test]
    fn merge_map_applies_incoming_values_by_key() {
        let mut state = state_with(ChannelReducer::MergeMap);
        assert!(state.apply_updates(update(serde_json::json!({"a": 1, "b": 2}))).is_ok());
        assert!(state.apply_updates(update(serde_json::json!({"b": 3, "c": 4}))).is_ok());
        assert_eq!(state.get("data"), Some(&serde_json::json!({"a": 1, "b": 3, "c": 4})));
    }

    #[test]
    fn max_by_timestamp_is_order_independent_for_distinct_timestamps() {
        let mut older_then_newer = state_with(ChannelReducer::MaxByTimestamp);
        let mut newer_then_older = state_with(ChannelReducer::MaxByTimestamp);
        let older = serde_json::json!({"timestamp": 10, "value": "old"});
        let newer = serde_json::json!({"timestamp": 20, "value": "new"});
        assert!(older_then_newer.apply_updates(update(older.clone())).is_ok());
        assert!(older_then_newer.apply_updates(update(newer.clone())).is_ok());
        assert!(newer_then_older.apply_updates(update(newer.clone())).is_ok());
        assert!(newer_then_older.apply_updates(update(older)).is_ok());
        assert_eq!(older_then_newer.get("data"), newer_then_older.get("data"));
    }

    #[test]
    fn max_by_timestamp_rejects_ambiguous_ties() {
        let mut state = state_with(ChannelReducer::MaxByTimestamp);
        assert!(state.apply_updates(update(serde_json::json!({"timestamp": 10, "value": "a"}))).is_ok());
        assert_eq!(
            state.apply_updates(update(serde_json::json!({"timestamp": 10, "value": "b"}))),
            Err(StateError::TimestampConflict("data".to_owned()))
        );
    }

    #[test]
    fn invalid_batch_does_not_partially_mutate_state() {
        let mut channels = BTreeMap::new();
        channels.insert("a".to_owned(), ChannelDefinition { reducer: ChannelReducer::LastWriteWins });
        channels.insert("b".to_owned(), ChannelDefinition { reducer: ChannelReducer::AppendList });
        let schema = ChannelSchema::new(1, channels);
        assert!(schema.is_ok());
        let mut state = match schema {
            Ok(schema) => ChannelState::new(schema),
            Err(_) => return,
        };
        let mut initial = BTreeMap::new();
        initial.insert("a".to_owned(), serde_json::json!(1));
        assert!(state.apply_updates(initial).is_ok());

        let mut batch = BTreeMap::new();
        batch.insert("a".to_owned(), serde_json::json!(2));
        batch.insert("b".to_owned(), serde_json::json!({"not": "an array"}));
        assert!(state.apply_updates(batch).is_err());
        assert_eq!(state.get("a"), Some(&serde_json::json!(1)));
        assert!(state.get("b").is_none());
    }

    #[test]
    fn unknown_channels_are_rejected() {
        let mut state = state_with(ChannelReducer::LastWriteWins);
        let mut updates = BTreeMap::new();
        updates.insert("missing".to_owned(), serde_json::json!(true));
        assert_eq!(
            state.apply_updates(updates),
            Err(StateError::UnknownChannel("missing".to_owned()))
        );
    }

    #[test]
    fn fork_edit_validates_but_replaces_without_reducing() {
        let mut state = state_with(ChannelReducer::AppendList);
        assert!(state.apply_updates(update(serde_json::json!([1, 2]))).is_ok());
        assert!(state.set_for_fork("data", serde_json::json!([9])).is_ok());
        assert_eq!(state.get("data"), Some(&serde_json::json!([9])));
        assert_eq!(
            state.set_for_fork("data", serde_json::json!({"invalid": true})),
            Err(StateError::InvalidValueShape("data".to_owned()))
        );
    }
}

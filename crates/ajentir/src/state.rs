//! Typed channel state and deterministic reducer semantics for AJENTIR*.
//!
//! State updates are applied transactionally: either every update in a batch is
//! valid and committed, or the state remains unchanged. Channel and update
//! iteration use BTreeMap ordering so reducer application is reproducible.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::marker::PhantomData;
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
    /// Keep the value with the greatest integer `timestamp` field. Equal
    /// timestamps are resolved by lexicographically greatest canonical JSON,
    /// making the operation total and associative.
    MaxByTimestamp,
}

impl ChannelReducer {
    /// The reducer identity. `None` represents an absent channel value and is
    /// the two-sided identity for every reducer.
    pub fn identity(self) -> Option<serde_json::Value> {
        None
    }

    /// Combine two optional values using this reducer.
    ///
    /// The operation is associative. It is intentionally not necessarily
    /// commutative: the kernel folds node contributions in canonical node-ID
    /// order, which gives LastWriteWins, append and right-biased map merge a
    /// stable meaning independent of task completion order.
    pub fn combine(
        self,
        channel: &str,
        left: Option<&serde_json::Value>,
        right: Option<&serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, StateError> {
        match (left, right) {
            (None, None) => Ok(self.identity()),
            (Some(value), None) | (None, Some(value)) => {
                validate_value_shape(channel, self, value)?;
                Ok(Some(value.clone()))
            }
            (Some(left), Some(right)) => {
                validate_value_shape(channel, self, left)?;
                validate_value_shape(channel, self, right)?;
                reduce_values(channel, self, left, right).map(Some)
            }
        }
    }
}

/// A node's updates for one completed or interrupted super-step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeUpdate {
    pub node_id: String,
    pub channels: BTreeMap<String, serde_json::Value>,
}

/// Compile-time typed handle for a channel in a heterogeneous run state.
///
/// Values are serialized into the checkpoint's JSON representation, while
/// this handle constrains reads and writes to the caller's Rust type.
pub struct TypedChannel<T> {
    name: String,
    reducer: ChannelReducer,
    marker: PhantomData<fn() -> T>,
}

impl<T> TypedChannel<T> {
    /// Declare a typed channel handle.
    pub fn new(name: impl Into<String>, reducer: ChannelReducer) -> Result<Self, StateError> {
        let name = name.into();
        validate_channel_name(&name)?;
        Ok(Self {
            name,
            reducer,
            marker: PhantomData,
        })
    }

    /// Stable channel name used in the serialized state schema.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reducer required by this typed handle.
    pub fn reducer(&self) -> ChannelReducer {
        self.reducer
    }

    /// Build the corresponding schema declaration.
    pub fn definition(&self) -> ChannelDefinition {
        ChannelDefinition {
            reducer: self.reducer,
        }
    }
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

    /// Read a channel through its compile-time typed handle.
    pub fn get_typed<T: DeserializeOwned>(
        &self,
        channel: &TypedChannel<T>,
    ) -> Result<Option<T>, StateError> {
        let definition = self.schema.definition(channel.name())?;
        if definition.reducer != channel.reducer() {
            return Err(StateError::ReducerMismatch(channel.name().to_owned()));
        }
        self.get(channel.name())
            .cloned()
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|error| StateError::Serialization(error.to_string()))
            })
            .transpose()
    }

    /// Apply a typed value through its compile-time typed channel handle.
    pub fn apply_typed_update<T: Serialize>(
        &mut self,
        channel: &TypedChannel<T>,
        value: T,
    ) -> Result<(), StateError> {
        let definition = self.schema.definition(channel.name())?;
        if definition.reducer != channel.reducer() {
            return Err(StateError::ReducerMismatch(channel.name().to_owned()));
        }
        let value = serde_json::to_value(value)
            .map_err(|error| StateError::Serialization(error.to_string()))?;
        let mut updates = BTreeMap::new();
        updates.insert(channel.name().to_owned(), value);
        self.apply_updates(updates)
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
        apply_channel_updates(&self.schema, &mut candidate, updates)?;
        self.values = candidate;
        Ok(())
    }

    /// Apply parallel node results in canonical node-name order, not task
    /// completion order. The complete super-step update is atomic.
    pub fn apply_node_updates(
        &mut self,
        mut updates: Vec<NodeUpdate>,
    ) -> Result<(), StateError> {
        for update in &updates {
            validate_node_id(&update.node_id)?;
        }
        updates.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        if updates.windows(2).any(|pair| {
            pair.first().zip(pair.get(1)).is_some_and(|(left, right)| left.node_id == right.node_id)
        }) {
            return Err(StateError::DuplicateNodeUpdate);
        }

        let mut candidate = self.values.clone();
        for update in updates {
            apply_channel_updates(&self.schema, &mut candidate, update.channels)?;
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
    #[error("invalid graph node identifier: {0}")]
    InvalidNodeId(String),
    #[error("unknown channel: {0}")]
    UnknownChannel(String),
    #[error("invalid value shape for channel {0}")]
    InvalidValueShape(String),
    #[error("equal timestamps contain conflicting values in channel {0}")]
    TimestampConflict(String),
    #[error("a node may contribute only one update per super-step")]
    DuplicateNodeUpdate,
    #[error("typed channel reducer does not match the state schema for {0}")]
    ReducerMismatch(String),
    #[error("channel value serialization failed: {0}")]
    Serialization(String),
}

fn validate_node_id(name: &str) -> Result<(), StateError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(StateError::InvalidNodeId(name.to_owned()));
    }
    Ok(())
}

fn apply_channel_updates(
    schema: &ChannelSchema,
    candidate: &mut BTreeMap<String, serde_json::Value>,
    updates: BTreeMap<String, serde_json::Value>,
) -> Result<(), StateError> {
    for (name, incoming) in updates {
        let definition = schema.definition(&name)?;
        validate_value_shape(&name, definition.reducer, &incoming)?;
        let reduced = definition
            .reducer
            .combine(&name, candidate.get(&name), Some(&incoming))?
            .ok_or_else(|| StateError::InvalidValueShape(name.clone()))?;
        candidate.insert(name, reduced);
    }
    Ok(())
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

fn reduce_values(
    channel: &str,
    reducer: ChannelReducer,
    current: &serde_json::Value,
    incoming: &serde_json::Value,
) -> Result<serde_json::Value, StateError> {
    match reducer {
        ChannelReducer::LastWriteWins => Ok(incoming.clone()),
        ChannelReducer::AppendList | ChannelReducer::AppendMessages => {
            let mut combined = current.as_array().cloned()
                .ok_or_else(|| StateError::InvalidValueShape(channel.to_owned()))?;
            let items = incoming.as_array()
                .ok_or_else(|| StateError::InvalidValueShape(channel.to_owned()))?;
            combined.extend(items.iter().cloned());
            Ok(serde_json::Value::Array(combined))
        }
        ChannelReducer::MergeMap => {
            let mut combined = current.as_object().cloned()
                .ok_or_else(|| StateError::InvalidValueShape(channel.to_owned()))?;
            let entries = incoming.as_object()
                .ok_or_else(|| StateError::InvalidValueShape(channel.to_owned()))?;
            for (key, value) in entries {
                combined.insert(key.clone(), value.clone());
            }
            Ok(serde_json::Value::Object(combined))
        }
        ChannelReducer::MaxByTimestamp => {
            let current_timestamp = current.as_object()
                .and_then(|object| object.get("timestamp"))
                .and_then(timestamp_as_i128)
                .ok_or_else(|| StateError::InvalidValueShape(channel.to_owned()))?;
            let incoming_timestamp = incoming.as_object()
                .and_then(|object| object.get("timestamp"))
                .and_then(timestamp_as_i128)
                .ok_or_else(|| StateError::InvalidValueShape(channel.to_owned()))?;
            if incoming_timestamp > current_timestamp {
                Ok(incoming.clone())
            } else if incoming_timestamp < current_timestamp {
                Ok(current.clone())
            } else {
                let current_bytes = serde_json::to_vec(current)
                    .map_err(|error| StateError::Serialization(error.to_string()))?;
                let incoming_bytes = serde_json::to_vec(incoming)
                    .map_err(|error| StateError::Serialization(error.to_string()))?;
                if incoming_bytes > current_bytes { Ok(incoming.clone()) } else { Ok(current.clone()) }
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
    fn typed_channel_round_trips_rust_values() {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        struct Message {
            text: String,
        }

        let channel = TypedChannel::<Vec<Message>>::new(
            "messages",
            ChannelReducer::AppendMessages,
        );
        assert!(channel.is_ok());
        if let Ok(channel) = channel {
            let mut channels = BTreeMap::new();
            channels.insert(channel.name().to_owned(), channel.definition());
            let schema = ChannelSchema::new(1, channels);
            assert!(schema.is_ok());
            if let Ok(schema) = schema {
                let mut state = ChannelState::new(schema);
                let message = Message {
                    text: "durable".to_owned(),
                };
                assert!(state
                    .apply_typed_update(&channel, vec![message.clone()])
                    .is_ok());
                assert_eq!(state.get_typed(&channel), Ok(Some(vec![message])));
            }
        }
    }

    #[test]
    fn typed_channel_rejects_reducer_mismatch() {
        let typed = TypedChannel::<Vec<String>>::new("data", ChannelReducer::AppendMessages);
        let schema = ChannelSchema::new(
            1,
            BTreeMap::from([(
                "data".to_owned(),
                ChannelDefinition {
                    reducer: ChannelReducer::AppendList,
                },
            )]),
        );
        assert!(typed.is_ok() && schema.is_ok());
        if let (Ok(typed), Ok(schema)) = (typed, schema) {
            let mut state = ChannelState::new(schema);
            assert_eq!(
                state.apply_typed_update(&typed, vec!["x".to_owned()]),
                Err(StateError::ReducerMismatch("data".to_owned()))
            );
        }
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
    fn max_by_timestamp_resolves_ties_deterministically() {
        let mut first = state_with(ChannelReducer::MaxByTimestamp);
        let mut second = state_with(ChannelReducer::MaxByTimestamp);
        let a = serde_json::json!({"timestamp": 10, "value": "a"});
        let b = serde_json::json!({"timestamp": 10, "value": "b"});
        assert!(first.apply_updates(update(a.clone())).is_ok());
        assert!(first.apply_updates(update(b.clone())).is_ok());
        assert!(second.apply_updates(update(b)).is_ok());
        assert!(second.apply_updates(update(a)).is_ok());
        assert_eq!(first.get("data"), second.get("data"));
    }

    #[test]
    fn every_reducer_has_a_two_sided_identity_and_is_associative() {
        let cases = [
            (ChannelReducer::LastWriteWins, serde_json::json!(1), serde_json::json!(2), serde_json::json!(3)),
            (ChannelReducer::AppendList, serde_json::json!([1]), serde_json::json!([2]), serde_json::json!([3])),
            (ChannelReducer::AppendMessages, serde_json::json!([{"id":"a"}]), serde_json::json!([{"id":"b"}]), serde_json::json!([{"id":"c"}])),
            (ChannelReducer::MergeMap, serde_json::json!({"a":1}), serde_json::json!({"b":2}), serde_json::json!({"c":3})),
            (ChannelReducer::MaxByTimestamp, serde_json::json!({"timestamp":1,"value":"a"}), serde_json::json!({"timestamp":2,"value":"b"}), serde_json::json!({"timestamp":2,"value":"c"})),
        ];

        for (reducer, a, b, c) in cases {
            let identity = reducer.identity();
            assert_eq!(reducer.combine("law", identity.as_ref(), Some(&a)), Ok(Some(a.clone())));
            assert_eq!(reducer.combine("law", Some(&a), identity.as_ref()), Ok(Some(a.clone())));

            let ab = reducer.combine("law", Some(&a), Some(&b));
            let bc = reducer.combine("law", Some(&b), Some(&c));
            assert!(ab.is_ok() && bc.is_ok());
            if let (Ok(Some(ab)), Ok(Some(bc))) = (ab, bc) {
                let left = reducer.combine("law", Some(&ab), Some(&c));
                let right = reducer.combine("law", Some(&a), Some(&bc));
                assert_eq!(left, right, "associativity failed for {reducer:?}");
            }
        }
    }

    #[test]
    fn invalid_batch_does_not_partially_mutate_state() {
        let mut channels = BTreeMap::new();
        channels.insert(
            "a".to_owned(),
            ChannelDefinition {
                reducer: ChannelReducer::LastWriteWins,
            },
        );
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
    fn parallel_node_updates_reduce_in_node_name_order() {
        let mut first = state_with(ChannelReducer::AppendMessages);
        let mut second = state_with(ChannelReducer::AppendMessages);
        let update_a = NodeUpdate {
            node_id: "node-a".to_owned(),
            channels: update(serde_json::json!(["a"])),
        };
        let update_z = NodeUpdate {
            node_id: "node-z".to_owned(),
            channels: update(serde_json::json!(["z"])),
        };
        assert!(first.apply_node_updates(vec![update_z.clone(), update_a.clone()]).is_ok());
        assert!(second.apply_node_updates(vec![update_a, update_z]).is_ok());
        assert_eq!(first.get("data"), Some(&serde_json::json!(["a", "z"])));
        assert_eq!(first.get("data"), second.get("data"));
    }

    #[test]
    fn duplicate_node_updates_are_rejected_atomically() {
        let mut state = state_with(ChannelReducer::AppendMessages);
        let node_update = NodeUpdate {
            node_id: "node-a".to_owned(),
            channels: update(serde_json::json!(["a"])),
        };
        assert_eq!(
            state.apply_node_updates(vec![node_update.clone(), node_update]),
            Err(StateError::DuplicateNodeUpdate)
        );
        assert!(state.get("data").is_none());
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

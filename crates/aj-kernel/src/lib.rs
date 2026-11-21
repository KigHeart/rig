#![forbid(unsafe_code)]
//! Deterministic in-memory BSP super-step merge kernel.
use aj_core::{NodeId, SuperStep};
use ajentir_state::{ChannelState, NodeUpdate, StateError};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInput { pub node_id: NodeId, pub super_step: SuperStep }

#[derive(Clone, Debug, PartialEq)]
pub struct NodeResult {
    pub node_id: NodeId,
    pub writes: std::collections::BTreeMap<String, serde_json::Value>,
    pub next_nodes: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuperStepCommit { pub completed_step: SuperStep, pub next_frontier: Vec<NodeId> }

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KernelError {
    #[error("active frontier contains duplicate node identifiers")]
    DuplicateFrontierNode,
    #[error("node result set does not exactly match the active frontier")]
    FrontierMismatch,
    #[error("node {0} requested a duplicate successor")]
    DuplicateSuccessor(String),
    #[error("state reducer rejected the super-step: {0}")]
    State(String),
    #[error("super-step counter overflow")]
    StepOverflow,
}
impl From<StateError> for KernelError {
    fn from(error: StateError) -> Self { Self::State(error.to_string()) }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BspKernel;
impl BspKernel {
    /// Validate all results and atomically apply writes in canonical node-ID order.
    /// Results may be supplied in arbitrary completion order.
    pub fn commit(
        &self,
        state: &mut ChannelState,
        super_step: SuperStep,
        active_frontier: &[NodeId],
        results: Vec<NodeResult>,
    ) -> Result<SuperStepCommit, KernelError> {
        let expected: BTreeSet<NodeId> = active_frontier.iter().cloned().collect();
        if expected.len() != active_frontier.len() { return Err(KernelError::DuplicateFrontierNode); }
        let mut seen = BTreeSet::new();
        for result in &results {
            if !seen.insert(result.node_id.clone()) { return Err(KernelError::FrontierMismatch); }
        }
        if seen != expected { return Err(KernelError::FrontierMismatch); }

        let mut updates = Vec::with_capacity(results.len());
        let mut next_frontier = BTreeSet::new();
        for result in results {
            let mut successors = BTreeSet::new();
            for successor in result.next_nodes {
                if !successors.insert(successor.clone()) {
                    return Err(KernelError::DuplicateSuccessor(successor.to_string()));
                }
                next_frontier.insert(successor);
            }
            updates.push(NodeUpdate { node_id: result.node_id.to_string(), channels: result.writes });
        }

        let mut candidate = state.clone();
        candidate.apply_node_updates(updates)?;
        let next_step = super_step.0.checked_add(1).ok_or(KernelError::StepOverflow)?;
        let _ = next_step;
        *state = candidate;
        Ok(SuperStepCommit { completed_step: super_step, next_frontier: next_frontier.into_iter().collect() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ajentir_state::{ChannelDefinition, ChannelReducer, ChannelSchema};
    use std::collections::BTreeMap;

    fn test_state() -> ChannelState {
        let schema = ChannelSchema::new(1, BTreeMap::from([(
            "events".to_owned(), ChannelDefinition { reducer: ChannelReducer::AppendList }
        )]));
        match schema {
            Ok(schema) => ChannelState::new(schema),
            Err(error) => panic!("valid test schema rejected: {error}"),
        }
    }
    fn node(value: &str, successor: &str) -> NodeResult {
        NodeResult {
            node_id: NodeId::new(value).unwrap_or_else(|error| panic!("{error}")),
            writes: BTreeMap::from([("events".to_owned(), serde_json::json!([value]))]),
            next_nodes: vec![NodeId::new(successor).unwrap_or_else(|error| panic!("{error}"))],
        }
    }
    #[test]
    fn commit_merges_by_node_id_not_completion_order() {
        let kernel = BspKernel;
        if let (Ok(a), Ok(b)) = (NodeId::new("a"), NodeId::new("b")) {
            let active = vec![a, b];
            let mut left = test_state();
            let mut right = test_state();
            let first = kernel.commit(&mut left, SuperStep(0), &active, vec![node("b", "z"), node("a", "y")]);
            let second = kernel.commit(&mut right, SuperStep(0), &active, vec![node("a", "y"), node("b", "z")]);
            assert!(first.is_ok() && second.is_ok());
            assert_eq!(left, right);
            assert_eq!(left.get("events"), Some(&serde_json::json!(["a", "b"])));
        }
    }
    #[test]
    fn incomplete_frontier_does_not_mutate_state() {
        if let (Ok(a), Ok(b)) = (NodeId::new("a"), NodeId::new("b")) {
            let active = vec![a, b];
            let mut state = test_state();
            let before = state.clone();
            let result = BspKernel.commit(&mut state, SuperStep(0), &active, vec![node("a", "next")]);
            assert_eq!(result, Err(KernelError::FrontierMismatch));
            assert_eq!(state, before);
        }
    }
}

#![forbid(unsafe_code)]
//! Deterministic simulator utilities for BSP completion-order invariance.
use aj_core::{NodeId, SuperStep};
use aj_kernel::{BspKernel, KernelError, NodeResult};
use ajentir_state::ChannelState;

#[derive(Debug, PartialEq, Eq)]
pub enum SimulationError {
    PermutationLimit { requested: usize, maximum: usize },
    Kernel(KernelError),
    DivergentState,
    DivergentFrontier,
}
impl From<KernelError> for SimulationError {
    fn from(error: KernelError) -> Self { Self::Kernel(error) }
}

/// Check every completion-order permutation, subject to a hard factorial-work bound.
pub fn verify_completion_order_invariance(
    initial: &ChannelState,
    step: SuperStep,
    frontier: &[NodeId],
    results: &[NodeResult],
    maximum_permutations: usize,
) -> Result<usize, SimulationError> {
    let count = factorial_bounded(results.len(), maximum_permutations)
        .ok_or(SimulationError::PermutationLimit {
            requested: factorial_saturating(results.len()), maximum: maximum_permutations
        })?;
    let mut schedules = Vec::with_capacity(count);
    let mut current = Vec::with_capacity(results.len());
    let mut used = vec![false; results.len()];
    collect_permutations(results, &mut used, &mut current, &mut schedules);

    let mut reference_state: Option<ChannelState> = None;
    let mut reference_frontier: Option<Vec<NodeId>> = None;
    for schedule in schedules {
        let mut state = initial.clone();
        let commit = BspKernel.commit(&mut state, step, frontier, schedule)?;
        match (&reference_state, &reference_frontier) {
            (None, None) => {
                reference_state = Some(state);
                reference_frontier = Some(commit.next_frontier);
            }
            (Some(expected_state), Some(expected_frontier)) => {
                if expected_state != &state { return Err(SimulationError::DivergentState); }
                if expected_frontier != &commit.next_frontier { return Err(SimulationError::DivergentFrontier); }
            }
            _ => return Err(SimulationError::DivergentState),
        }
    }
    Ok(count)
}

fn factorial_bounded(value: usize, maximum: usize) -> Option<usize> {
    let mut result = 1usize;
    for factor in 2..=value {
        result = result.checked_mul(factor)?;
        if result > maximum { return None; }
    }
    if result > maximum { None } else { Some(result) }
}
fn factorial_saturating(value: usize) -> usize {
    (2..=value).fold(1usize, |total, factor| total.saturating_mul(factor))
}
fn collect_permutations(
    results: &[NodeResult], used: &mut [bool], current: &mut Vec<NodeResult>, output: &mut Vec<Vec<NodeResult>>
) {
    if current.len() == results.len() { output.push(current.clone()); return; }
    for index in 0..results.len() {
        let Some(is_used) = used.get(index).copied() else { continue; };
        if is_used { continue; }
        let Some(slot) = used.get_mut(index) else { continue; };
        *slot = true;
        if let Some(result) = results.get(index) {
            current.push(result.clone());
            collect_permutations(results, used, current, output);
            let _ = current.pop();
        }
        if let Some(slot) = used.get_mut(index) { *slot = false; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ajentir_state::{ChannelDefinition, ChannelReducer, ChannelSchema};
    use std::collections::BTreeMap;

    fn sample_state() -> Option<ChannelState> {
        let schema = ChannelSchema::new(1, BTreeMap::from([(
            "items".to_owned(), ChannelDefinition { reducer: ChannelReducer::AppendList }
        )])).ok()?;
        Some(ChannelState::new(schema))
    }
    fn result(id: &str, item: &str) -> Option<NodeResult> {
        Some(NodeResult {
            node_id: NodeId::new(id).ok()?,
            writes: BTreeMap::from([("items".to_owned(), serde_json::json!([item]))]),
            next_nodes: Vec::new(),
        })
    }
    #[test]
    fn all_six_completion_orders_produce_one_committed_state() {
        let values = (
            sample_state(), NodeId::new("a").ok(), NodeId::new("b").ok(), NodeId::new("c").ok(),
            result("a", "A"), result("b", "B"), result("c", "C")
        );
        assert!(
            values.0.is_some() && values.1.is_some() && values.2.is_some() && values.3.is_some()
                && values.4.is_some() && values.5.is_some() && values.6.is_some()
        );
        let (Some(state), Some(a), Some(b), Some(c), Some(ra), Some(rb), Some(rc)) = values else { return; };
        let frontier = vec![a, b, c];
        let results = vec![ra, rb, rc];
        assert_eq!(verify_completion_order_invariance(&state, SuperStep(0), &frontier, &results, 6), Ok(6));
    }
    #[test]
    fn permutation_guard_rejects_unbounded_work() {
        let values = (sample_state(), NodeId::new("a").ok(), result("a", "A"), result("b", "B"), result("c", "C"));
        assert!(values.0.is_some() && values.1.is_some() && values.2.is_some() && values.3.is_some() && values.4.is_some());
        let (Some(state), Some(a), Some(ra), Some(rb), Some(rc)) = values else { return; };
        let frontier = vec![a];
        let results = vec![ra, rb, rc];
        assert!(matches!(
            verify_completion_order_invariance(&state, SuperStep(0), &frontier, &results, 5),
            Err(SimulationError::PermutationLimit { .. })
        ));
    }
}

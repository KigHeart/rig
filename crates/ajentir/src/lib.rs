//! AJENTIR* durable agent execution substrate built on Rig.
//!
//! AJENTIR* is a design hypothesis under active development. This crate owns
//! typed run state and durable execution recovery; Rig remains the model and
//! tool interaction layer.

pub mod checkpoint;
pub mod state;

pub use checkpoint::{
    Checkpoint, CheckpointError, Checkpointer, Frontier, MemoryCheckpointer, SqliteCheckpointer,
};
pub use state::{
    ChannelDefinition, ChannelReducer, ChannelSchema, ChannelState, NodeUpdate, StateError,
    TypedChannel,
};

/// Rig runtime and tool contracts used by AJENTIR* adapters.
pub mod rig {
    pub use rig_agent::{Agent, AgentHook, AgentRun, AgentRunner};
    pub use rig_core::tool::{Tool, ToolCatalog, ToolContext, ToolResult};
}

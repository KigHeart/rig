//! AJENTIR typed state and checkpoint substrate.
//!
//! This component provides typed run state and durable checkpoint recovery.
 //! Model-provider and tool adapters remain separate from persisted state.

pub mod checkpoint;
pub mod state;

pub use checkpoint::{
    Checkpoint, CheckpointError, Checkpointer, Frontier, MemoryCheckpointer, SqliteCheckpointer,
};
pub use state::{
    ChannelDefinition, ChannelReducer, ChannelSchema, ChannelState, NodeUpdate, StateError,
    TypedChannel,
};

/// Compatibility access to inherited model and tool contracts.
pub mod rig {
    pub use rig_agent::{Agent, AgentHook, AgentRun, AgentRunner};
    pub use rig_core::tool::{Tool, ToolCatalog, ToolContext, ToolResult};
}

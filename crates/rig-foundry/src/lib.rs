//! Foundational identity and manifest types for agents running on Rig.
//!
//! This crate builds on Rig's existing runtime and tool contracts rather than
//! creating a parallel model or tool execution stack. A manifest describes an
//! agent; it does not authorize execution. Runtime authority must be enforced
//! separately at the side-effect boundary.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use thiserror::Error;

/// Rig runtime and tool contracts used by Agent Foundry integrations.
pub mod rig {
    pub use rig_agent::{Agent, AgentHook, AgentRun, AgentRunner};
    pub use rig_core::tool::{Tool, ToolCatalog, ToolContext, ToolResult};
}

/// Current supported manifest schema version.
pub const MANIFEST_SCHEMA_VERSION: u16 = 1;

/// Versioned declaration of an agent that can be instantiated on Rig.
///
/// Declared capabilities are requests the agent may make, not grants of
/// authority. An independent authority component must authorize each action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Manifest schema version.
    pub schema_version: u16,
    /// Stable, human-readable agent name.
    pub name: String,
    /// Purpose and expected work of this agent.
    pub description: String,
    /// Owning user, organization, or service principal.
    pub owner: AgentOwner,
    /// Rig provider and model selection.
    pub runtime: RigRuntimeSpec,
    /// Capabilities the agent may request; these are not grants of authority.
    pub capabilities: Vec<CapabilityDeclaration>,
    /// Hard ceilings requested for this agent instance.
    pub limits: ResourceLimits,
}

/// Principal that owns an agent manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOwner {
    /// Stable principal identifier from the platform identity system.
    pub subject: String,
}

/// Provider/model configuration for a Rig-backed agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RigRuntimeSpec {
    /// Rig provider identifier, such as openai or anthropic.
    pub provider: String,
    /// Provider model identifier.
    pub model: String,
}

/// Declared action/resource surface an agent may request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDeclaration {
    /// Stable capability identifier.
    pub id: String,
    /// Named operations within this capability.
    pub actions: Vec<String>,
    /// Resource identifiers or resource patterns this capability concerns.
    pub resources: Vec<String>,
}

/// Resource ceilings applied by a supervisor and authority service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum number of child agents this agent may spawn.
    pub max_child_agents: u32,
    /// Maximum delegation depth below this agent.
    pub max_delegation_depth: u16,
    /// Maximum tool calls for this agent instance.
    pub max_tool_calls: u64,
    /// Maximum wall-clock runtime in seconds.
    pub max_runtime_seconds: u64,
    /// Maximum model tokens consumed by this agent instance.
    pub max_tokens: u64,
    /// Maximum spend in integer micro-units of the platform billing currency.
    /// Zero means no billable spend is authorized by default.
    pub max_spend_microunits: u64,
}

/// Errors detected while validating or identifying a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ManifestError {
    /// The manifest schema is not supported by this crate version.
    #[error("unsupported manifest schema version: {0}")]
    UnsupportedSchemaVersion(u16),
    /// A required string is empty, oversized, malformed, or not normalized.
    #[error("invalid manifest field: {0}")]
    InvalidField(&'static str),
    /// A capability identifier appears more than once.
    #[error("duplicate capability identifier: {0}")]
    DuplicateCapability(String),
    /// An action appears more than once within one capability.
    #[error("duplicate action in capability {0}")]
    DuplicateAction(String),
    /// A resource appears more than once within one capability.
    #[error("duplicate resource in capability {0}")]
    DuplicateResource(String),
    /// Delegation is enabled without a positive depth ceiling.
    #[error("child-agent spawning requires a positive delegation-depth limit")]
    InvalidDelegationLimits,
    /// Deterministic JSON serialization failed.
    #[error("manifest serialization failed: {0}")]
    Serialization(String),
}

impl AgentManifest {
    /// Validate all manifest fields and resource ceilings.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchemaVersion(self.schema_version));
        }

        validate_text(&self.name, "name", 128)?;
        validate_text(&self.description, "description", 4096)?;
        validate_text(&self.owner.subject, "owner.subject", 256)?;
        validate_text(&self.runtime.provider, "runtime.provider", 128)?;
        validate_text(&self.runtime.model, "runtime.model", 256)?;

        if self.limits.max_runtime_seconds == 0
            || self.limits.max_tool_calls == 0
            || self.limits.max_tokens == 0
        {
            return Err(ManifestError::InvalidField("limits"));
        }
        if self.limits.max_child_agents > 0 && self.limits.max_delegation_depth == 0 {
            return Err(ManifestError::InvalidDelegationLimits);
        }

        let mut capability_ids = HashSet::new();
        for capability in &self.capabilities {
            validate_text(&capability.id, "capabilities.id", 128)?;
            if !capability_ids.insert(capability.id.as_str()) {
                return Err(ManifestError::DuplicateCapability(capability.id.clone()));
            }
            if capability.actions.is_empty() || capability.resources.is_empty() {
                return Err(ManifestError::InvalidField("capabilities.actions/resources"));
            }

            let mut actions = HashSet::new();
            for action in &capability.actions {
                validate_text(action, "capabilities.actions", 128)?;
                if !actions.insert(action.as_str()) {
                    return Err(ManifestError::DuplicateAction(capability.id.clone()));
                }
            }

            let mut resources = HashSet::new();
            for resource in &capability.resources {
                validate_text(resource, "capabilities.resources", 512)?;
                if !resources.insert(resource.as_str()) {
                    return Err(ManifestError::DuplicateResource(capability.id.clone()));
                }
            }
        }

        Ok(())
    }

    /// Return deterministic bytes used to derive this manifest's identity.
    ///
    /// Capabilities, actions, and resources are order-insensitive sets and are
    /// sorted before encoding. Other fields retain their declared values;
    /// validation rejects leading/trailing whitespace.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical.capabilities.sort_by(|left, right| left.id.cmp(&right.id));
        for capability in &mut canonical.capabilities {
            capability.actions.sort();
            capability.resources.sort();
        }
        serde_json::to_vec(&canonical)
            .map_err(|error| ManifestError::Serialization(error.to_string()))
    }

    /// Derive a stable, domain-separated SHA-256 content identity.
    ///
    /// The lowercase hexadecimal digest is not a signature, ownership proof,
    /// or authorization token.
    pub fn id(&self) -> Result<String, ManifestError> {
        let mut hasher = Sha256::new();
        hasher.update(b"rig-agent-foundry/manifest/v1\0");
        hasher.update(self.canonical_bytes()?);
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}")
                .map_err(|error| ManifestError::Serialization(error.to_string()))?;
        }
        Ok(encoded)
    }
}

fn validate_text(value: &str, field: &'static str, max_len: usize) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > max_len
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ManifestError::InvalidField(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> AgentManifest {
        AgentManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            name: "researcher".to_owned(),
            description: "Research agent".to_owned(),
            owner: AgentOwner {
                subject: "org:tilil".to_owned(),
            },
            runtime: RigRuntimeSpec {
                provider: "openai".to_owned(),
                model: "gpt-model".to_owned(),
            },
            capabilities: vec![CapabilityDeclaration {
                id: "web-research".to_owned(),
                actions: vec!["search".to_owned(), "read".to_owned()],
                resources: vec!["https://example.test/*".to_owned()],
            }],
            limits: ResourceLimits {
                max_child_agents: 2,
                max_delegation_depth: 1,
                max_tool_calls: 100,
                max_runtime_seconds: 600,
                max_tokens: 50_000,
                max_spend_microunits: 0,
            },
        }
    }

    #[test]
    fn manifest_identity_is_stable() {
        let value = manifest();
        let first = value.id();
        let second = value.id();
        assert!(first.is_ok() && second.is_ok());
        if let (Ok(first), Ok(second)) = (first, second) {
            assert_eq!(first, second);
        }
    }

    #[test]
    fn set_order_does_not_change_manifest_identity() {
        let first = manifest();
        let mut second = manifest();
        if let Some(capability) = second.capabilities.first_mut() {
            capability.actions.reverse();
            capability.resources.reverse();
        }
        second.capabilities.reverse();
        assert_eq!(first.id(), second.id());
    }

    #[test]
    fn duplicate_capabilities_are_rejected() {
        let mut value = manifest();
        if let Some(capability) = value.capabilities.first().cloned() {
            value.capabilities.push(capability);
        }
        assert_eq!(
            value.validate(),
            Err(ManifestError::DuplicateCapability("web-research".to_owned()))
        );
    }

    #[test]
    fn duplicate_actions_are_rejected() {
        let mut value = manifest();
        if let Some(capability) = value.capabilities.first_mut() {
            capability.actions.push("search".to_owned());
        }
        assert_eq!(
            value.validate(),
            Err(ManifestError::DuplicateAction("web-research".to_owned()))
        );
    }

    #[test]
    fn zero_delegation_depth_cannot_enable_child_agents() {
        let mut value = manifest();
        value.limits.max_delegation_depth = 0;
        assert_eq!(value.validate(), Err(ManifestError::InvalidDelegationLimits));
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let mut value = manifest();
        value.schema_version = MANIFEST_SCHEMA_VERSION + 1;
        assert_eq!(
            value.validate(),
            Err(ManifestError::UnsupportedSchemaVersion(2))
        );
    }

    #[test]
    fn whitespace_ambiguity_is_rejected() {
        let mut value = manifest();
        value.owner.subject = " org:tilil".to_owned();
        assert_eq!(
            value.validate(),
            Err(ManifestError::InvalidField("owner.subject"))
        );
    }

    #[test]
    fn manifest_id_is_not_an_authorization_permit() {
        let result = manifest().id();
        assert!(result.is_ok());
        if let Ok(id) = result {
            assert_eq!(id.len(), 64);
            assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
}

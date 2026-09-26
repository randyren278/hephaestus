use std::collections::BTreeMap;

use hephaestus_core::authority::CapabilitySet;
use hephaestus_ledger::{ArtifactBackend, ArtifactId, LedgerError};
use serde::{Deserialize, Serialize};

use crate::{
    CompileError, CompiledWorld, SourceFormat,
    compiler::{canonical_json, content_id, parse_versioned, require_text},
};

/// Immutable normalized Genome produced by the compiler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledGenome {
    id: String,
    canonical_json: Vec<u8>,
    name: String,
    parents: Vec<String>,
    authority: CapabilitySet,
    artifacts: BTreeMap<String, String>,
    model_provider: String,
}

impl CompiledGenome {
    /// Returns the content-derived Genome identity.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the exact normalized JSON hashed by the identity.
    #[must_use]
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical_json
    }

    /// Returns the human-readable stable Genome name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns sorted declared parent identities.
    #[must_use]
    pub fn parents(&self) -> &[String] {
        &self.parents
    }

    /// Returns the compiled authority set.
    #[must_use]
    pub const fn authority(&self) -> CapabilitySet {
        self.authority
    }

    /// Returns the content address for a named artifact declared by this Genome.
    #[must_use]
    pub fn artifact_id(&self, name: &str) -> Option<&str> {
        self.artifacts.get(name).map(String::as_str)
    }

    /// Returns the declared runtime provider (`deterministic`, `codex`, or `claude`).
    ///
    /// Provider selection is operator/runtime configuration; it never widens or
    /// narrows the compiled authority ceiling above.
    #[must_use]
    pub fn model_provider(&self) -> &str {
        &self.model_provider
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGenome {
    pub(crate) schema_version: u16,
    pub(crate) name: String,
    pub(crate) parents: Vec<String>,
    pub(crate) model: ModelSpec,
    pub(crate) authority: RawAuthority,
    pub(crate) artifacts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelSpec {
    pub(crate) provider: String,
    pub(crate) family: String,
}

pub(crate) const AGENT_PROMPT_ARTIFACT: &str = "agent.prompt";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAuthority {
    pub(crate) workspace_write: bool,
    pub(crate) network: bool,
}

impl RawAuthority {
    pub(crate) const fn capabilities(self) -> CapabilitySet {
        CapabilitySet::new(self.workspace_write, self.network)
    }
}

/// Compiles JSON or YAML into an immutable content-addressed Genome.
///
/// # Errors
///
/// Fails closed for invalid schemas, unresolved ancestry or artifacts, and authority
/// wider than the World or any declared parent.
pub fn compile_genome(
    source: &str,
    format: SourceFormat,
    world: &CompiledWorld,
    parents: &BTreeMap<String, CompiledGenome>,
    artifact_store: &dyn ArtifactBackend,
) -> Result<CompiledGenome, CompileError> {
    let mut raw: RawGenome = parse_versioned(source, format)?;
    normalize_and_validate(&mut raw, world, parents, artifact_store)?;

    let canonical_json = canonical_json(&raw)?;
    Ok(compiled_genome(raw, canonical_json))
}

pub(crate) fn normalize_and_validate(
    raw: &mut RawGenome,
    world: &CompiledWorld,
    parents: &BTreeMap<String, CompiledGenome>,
    artifact_store: &dyn ArtifactBackend,
) -> Result<(), CompileError> {
    require_text(&raw.name, "name")?;
    require_text(&raw.model.provider, "model.provider")?;
    require_text(&raw.model.family, "model.family")?;
    raw.parents.sort();
    raw.parents.dedup();

    let requested = raw.authority.capabilities();
    world
        .authority_ceiling()
        .derive_child(requested)
        .map_err(|_| CompileError::AuthorityEscalation)?;
    for parent_id in &raw.parents {
        let parent = parents
            .get(parent_id)
            .ok_or_else(|| CompileError::UnresolvedParent(parent_id.clone()))?;
        if parent.id() != parent_id {
            return Err(CompileError::ParentIdentityMismatch {
                declared: parent_id.clone(),
                actual: parent.id().to_owned(),
            });
        }
        parent
            .authority()
            .derive_child(requested)
            .map_err(|_| CompileError::AuthorityEscalation)?;
    }
    resolve_artifacts(&raw.artifacts, artifact_store)?;

    Ok(())
}

pub(crate) fn compiled_genome(raw: RawGenome, canonical_json: Vec<u8>) -> CompiledGenome {
    let RawGenome {
        name,
        parents,
        model,
        authority,
        artifacts,
        ..
    } = raw;
    CompiledGenome {
        id: content_id("genome", &canonical_json),
        canonical_json,
        name,
        parents,
        authority: authority.capabilities(),
        artifacts,
        model_provider: model.provider,
    }
}

pub(crate) fn resolve_artifacts(
    references: &BTreeMap<String, String>,
    artifact_store: &dyn ArtifactBackend,
) -> Result<(), CompileError> {
    for (name, artifact) in references {
        require_text(name, "artifact name")?;
        let id = ArtifactId::parse(artifact.clone())
            .map_err(|_| CompileError::InvalidArtifactId(artifact.clone()))?;
        let bytes = match artifact_store.get(&id) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(match error {
                    LedgerError::Io(io_error)
                        if io_error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        CompileError::UnresolvedArtifact(artifact.clone())
                    }
                    _ => CompileError::ArtifactIntegrity(artifact.clone()),
                });
            }
        };
        if name == AGENT_PROMPT_ARTIFACT {
            if bytes.len() > crate::compiler::MAX_SOURCE_BYTES {
                return Err(CompileError::InputTooLarge {
                    bytes: bytes.len(),
                    maximum: crate::compiler::MAX_SOURCE_BYTES,
                });
            }
            let prompt = std::str::from_utf8(&bytes)
                .map_err(|_| CompileError::InvalidAgentPromptArtifact)?;
            if prompt.trim().is_empty() {
                return Err(CompileError::EmptyAgentPrompt);
            }
        }
    }
    Ok(())
}

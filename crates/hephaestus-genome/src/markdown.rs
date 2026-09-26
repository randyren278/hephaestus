use std::collections::BTreeMap;

use hephaestus_ledger::ArtifactBackend;

use crate::{
    CompileError, CompiledGenome, CompiledWorld,
    compiler::{MAX_SOURCE_BYTES, canonical_json},
    genome::{AGENT_PROMPT_ARTIFACT, RawGenome, compile_genome, normalize_and_validate},
};

/// Compiles a strict Markdown Genome document under a registered World.
///
/// The YAML frontmatter uses the existing Genome schema. The exact UTF-8 body
/// bytes are placed in CAS under the reserved `agent.prompt` artifact key, and
/// the normalized Genome is passed through the ordinary JSON compiler.
///
/// # Errors
///
/// Rejects malformed/oversized frontmatter, a blank prompt, an attempted
/// `agent.prompt` override, invalid Genome policy/ancestry, or CAS failures.
pub fn compile_markdown_genome(
    source: &str,
    world: &CompiledWorld,
    parents: &BTreeMap<String, CompiledGenome>,
    artifact_store: &dyn ArtifactBackend,
) -> Result<CompiledGenome, CompileError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(CompileError::InputTooLarge {
            bytes: source.len(),
            maximum: MAX_SOURCE_BYTES,
        });
    }
    let (frontmatter, body) = split_frontmatter(source)?;
    if body.trim().is_empty() {
        return Err(CompileError::EmptyAgentPrompt);
    }
    let mut options = serde_saphyr::Options::default();
    options.duplicate_keys = serde_saphyr::DuplicateKeyPolicy::Error;
    options.merge_keys = serde_saphyr::MergeKeyPolicy::Error;
    let mut raw: RawGenome = serde_saphyr::from_str_with_options(frontmatter, options)
        .map_err(|_| CompileError::Parse("Markdown frontmatter is not a Genome schema".into()))?;
    if raw.schema_version != 1 {
        return Err(CompileError::UnsupportedSchemaVersion(raw.schema_version));
    }
    if raw.artifacts.contains_key(AGENT_PROMPT_ARTIFACT) {
        return Err(CompileError::ReservedAgentPromptArtifact);
    }

    // Check schema fields, World and ancestry, authority, and every declared
    // artifact before publishing the prompt blob. The normal compiler repeats
    // these checks after the reserved artifact is attached.
    normalize_and_validate(&mut raw, world, parents, artifact_store)?;
    let prompt_id = artifact_store
        .put(body.as_bytes())
        .map_err(|_| CompileError::ArtifactStore)?;
    raw.artifacts.insert(
        AGENT_PROMPT_ARTIFACT.to_owned(),
        prompt_id.as_str().to_owned(),
    );
    let json = canonical_json(&raw)?;
    let json = std::str::from_utf8(&json)
        .map_err(|_| CompileError::Canonicalization("Genome JSON is not UTF-8".into()))?;
    compile_genome(
        json,
        crate::SourceFormat::Json,
        world,
        parents,
        artifact_store,
    )
}

fn split_frontmatter(source: &str) -> Result<(&str, &str), CompileError> {
    let opening_len = if source.starts_with("---\r\n") {
        5
    } else if source.starts_with("---\n") {
        4
    } else {
        return Err(CompileError::InvalidMarkdownFrontmatter);
    };
    let remainder = &source[opening_len..];
    let mut offset = 0;
    for line in remainder.split_inclusive('\n') {
        let content = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line));
        if content == "---" {
            let body_offset = opening_len + offset + line.len();
            return Ok((&remainder[..offset], &source[body_offset..]));
        }
        offset += line.len();
    }
    Err(CompileError::InvalidMarkdownFrontmatter)
}

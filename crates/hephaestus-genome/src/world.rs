use std::collections::BTreeMap;

use hephaestus_core::{authority::CapabilitySet, domain::MutationTarget};
use hephaestus_ledger::ArtifactBackend;
use serde::{Deserialize, Serialize};

use crate::{
    CompileError, SourceFormat,
    compiler::{canonical_json, content_id, parse_versioned, require_text},
    genome::{RawAuthority, resolve_artifacts},
};

/// Immutable normalized World produced by the compiler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledWorld {
    id: String,
    canonical_json: Vec<u8>,
    name: String,
    authority_ceiling: CapabilitySet,
    mutation_scope: Vec<MutationTarget>,
    objectives: Vec<String>,
    evaluator_artifacts: BTreeMap<String, String>,
    evaluation_policy: WorldEvaluationPolicy,
}

/// Immutable evaluation and promotion constraints compiled into a World.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldEvaluationPolicy {
    maximum_cost_microusd: u64,
    minimum_delta_bps: i64,
    maximum_regressions: u32,
    confidence_bps: u16,
    allow_mixed_environments: bool,
    auto_canary_on_drift: bool,
}

impl WorldEvaluationPolicy {
    /// Maximum aggregate candidate spend permitted by the World.
    #[must_use]
    pub const fn maximum_cost_microusd(self) -> u64 {
        self.maximum_cost_microusd
    }

    /// Minimum paired improvement required for later promotion eligibility.
    #[must_use]
    pub const fn minimum_delta_bps(self) -> i64 {
        self.minimum_delta_bps
    }

    /// Maximum invariant regressions permitted by the World.
    #[must_use]
    pub const fn maximum_regressions(self) -> u32 {
        self.maximum_regressions
    }

    /// Required statistical confidence in basis points.
    #[must_use]
    pub const fn confidence_bps(self) -> u16 {
        self.confidence_bps
    }

    /// Whether this World permits a paired Arena trial to compare a parent
    /// and candidate running under two different execution environments
    /// (for example, a reference-worker parent against a provider-adapter
    /// candidate). Defaults to `false`: a World that never opted in keeps the
    /// existing single-environment-per-pair guarantee.
    #[must_use]
    pub const fn allow_mixed_environments(self) -> bool {
        self.allow_mixed_environments
    }

    /// Whether this World opts in to the daemon's automatic drift-to-canary
    /// adaptation pipeline: a recorded `drift.recorded` event drives a Forge
    /// proposal of the current Champion, a shadow evaluation, and a staged
    /// canary, entirely from the daemon's own reconciliation loop. Defaults
    /// to `false`: a World that never opted in keeps drift purely observational.
    #[must_use]
    pub const fn auto_canary_on_drift(self) -> bool {
        self.auto_canary_on_drift
    }
}

impl CompiledWorld {
    /// Returns the content-derived World identity.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the exact normalized JSON hashed by the identity.
    #[must_use]
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical_json
    }

    /// Returns the World's stable human-readable name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the maximum capability set any candidate may request.
    #[must_use]
    pub const fn authority_ceiling(&self) -> CapabilitySet {
        self.authority_ceiling
    }

    /// Returns the normalized mutation targets authorized by this World.
    #[must_use]
    pub fn mutation_scope(&self) -> &[MutationTarget] {
        &self.mutation_scope
    }

    /// Returns the normalized objective names bound into this World.
    #[must_use]
    pub fn objectives(&self) -> &[String] {
        &self.objectives
    }

    /// Resolves one evaluator or task-manifest artifact by its World-bound name.
    #[must_use]
    pub fn evaluator_artifact(&self, name: &str) -> Option<&str> {
        self.evaluator_artifacts.get(name).map(String::as_str)
    }

    /// Returns immutable evaluation and promotion constraints.
    #[must_use]
    pub const fn evaluation_policy(&self) -> WorldEvaluationPolicy {
        self.evaluation_policy
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawWorld {
    schema_version: u16,
    name: String,
    laws: RawLaws,
    authority_ceiling: RawAuthority,
    mutation_scope: Vec<MutationTarget>,
    promotion: PromotionPolicy,
    objectives: Vec<String>,
    evaluator_artifacts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
struct RawLaws {
    candidate_network: bool,
    candidate_evaluator_access: bool,
    maximum_cost_microusd: u64,
    #[serde(default)]
    allow_mixed_environments: bool,
    /// Opts this World in to the daemon's automatic drift-to-canary
    /// adaptation pipeline (roadmap item 12). Added after `allow_mixed_environments`
    /// shipped; `skip_serializing_if` keeps an existing World's canonical
    /// JSON — and therefore its content-addressed identity — byte-identical
    /// while this stays absent or `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    auto_canary_on_drift: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PromotionPolicy {
    minimum_delta_bps: i64,
    maximum_regressions: u32,
    confidence_bps: u16,
}

/// Compiles JSON or YAML into an immutable content-addressed World.
///
/// # Errors
///
/// Fails closed for invalid schemas, evaluator access, protected mutation targets,
/// unresolved evaluator artifacts, or invalid promotion policy.
pub fn compile_world(
    source: &str,
    format: SourceFormat,
    artifact_store: &dyn ArtifactBackend,
) -> Result<CompiledWorld, CompileError> {
    let mut raw: RawWorld = parse_versioned(source, format)?;
    require_text(&raw.name, "name")?;
    if raw.laws.candidate_evaluator_access {
        return Err(CompileError::CandidateEvaluatorAccess);
    }
    for target in &raw.mutation_scope {
        target
            .authorize()
            .map_err(|_| CompileError::ProtectedMutationTarget(*target))?;
    }
    if !(1..=10_000).contains(&raw.promotion.confidence_bps) {
        return Err(CompileError::InvalidConfidence(
            raw.promotion.confidence_bps,
        ));
    }
    if raw.objectives.is_empty() {
        return Err(CompileError::EmptyObjectives);
    }
    for objective in &raw.objectives {
        require_text(objective, "objective")?;
    }
    raw.objectives.sort();
    raw.objectives.dedup();
    raw.mutation_scope.sort();
    raw.mutation_scope.dedup();
    resolve_artifacts(&raw.evaluator_artifacts, artifact_store)?;

    let authority_ceiling = raw.authority_ceiling.capabilities();
    let canonical_json = canonical_json(&raw)?;
    let evaluation_policy = WorldEvaluationPolicy {
        maximum_cost_microusd: raw.laws.maximum_cost_microusd,
        minimum_delta_bps: raw.promotion.minimum_delta_bps,
        maximum_regressions: raw.promotion.maximum_regressions,
        confidence_bps: raw.promotion.confidence_bps,
        allow_mixed_environments: raw.laws.allow_mixed_environments,
        auto_canary_on_drift: raw.laws.auto_canary_on_drift,
    };
    Ok(CompiledWorld {
        id: content_id("world", &canonical_json),
        canonical_json,
        name: raw.name,
        authority_ceiling,
        mutation_scope: raw.mutation_scope,
        objectives: raw.objectives,
        evaluator_artifacts: raw.evaluator_artifacts,
        evaluation_policy,
    })
}

/// Rejects direct comparisons between different World identities.
///
/// # Errors
///
/// Returns [`CompileError::IncompatibleWorlds`] unless both identities match exactly.
pub fn ensure_comparable(left: &CompiledWorld, right: &CompiledWorld) -> Result<(), CompileError> {
    if left.id != right.id {
        return Err(CompileError::IncompatibleWorlds {
            left: left.id.clone(),
            right: right.id.clone(),
        });
    }
    Ok(())
}

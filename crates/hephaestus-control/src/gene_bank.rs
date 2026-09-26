//! The Gene Bank: extracting reusable mutations from promoted, evidence-bound
//! Champion transitions, transferring them across lineages, and admitting
//! specialist species from persistent, statistically significant domain
//! advantage.
//!
//! A Gene is never created from a single positive trial: extraction requires
//! a deterministic minimum-evidence threshold on the origin promotion's
//! verified paired trials. Transfer trials replay the Gene's mutation onto
//! another lineage's Genome through the ordinary compiler (exactly like
//! Forge child compilation) and record positive, neutral, or negative effect
//! from a verified paired evaluation and selection. Negative transfer is
//! retained, never dropped; contradictions (positive in one lineage,
//! negative in another) are explicit, idempotent records, never
//! overwritten. Speciation admits a specialist species only from persistent
//! (zero recorded negatives in the domain) and statistically significant
//! (a minimum count of distinct positive lineages and a minimum mean effect
//! size) domain advantage.
//!
//! Every event here is canonical, idempotent, and replay-verified exactly
//! like Champion transitions in `champion.rs`: each payload is recomputed
//! from the history that preceded it, so an event can only exist if the
//! policy admitted it at that point.

use std::collections::{BTreeMap, BTreeSet};

use hephaestus_arena::verify_selection_event_in;
use hephaestus_genome::{CompiledWorld, RegisteredObjects, SourceFormat, compile_genome};
use hephaestus_ledger::{ArtifactId, ArtifactStore, StoredEvent};
use hephaestus_runtime::ReferenceInstruction;

use super::champion::{champion_event_id, decode_champion_transition};
use super::{
    ControlError, ExecuteError, decode_forge_assessment, decode_forge_proposal, forge_event_id,
    hex_encode, mutate_reference_instruction_document, reference_instruction_operation,
    validate_job_id,
};
use crate::protocol::{
    ChampionTransitionKind, ForgeAssessmentOutcome, GeneAggregateRecord, GeneContradictionPayload,
    GeneContradictionRecord, GeneEventRecord, GeneExtractedPayload, GeneRecord, GeneSpeciesPayload,
    GeneSpeciesRecord, GeneSummary, GeneTransferAppliedPayload, GeneTransferOutcome,
    GeneTransferRecord, GeneTransferRecordedPayload, GenomeRecord,
};

pub(super) const GENE_EVENT_TYPE: &str = "gene.extracted";
pub(super) const TRANSFER_APPLIED_EVENT_TYPE: &str = "gene.transfer_applied";
pub(super) const TRANSFER_RECORDED_EVENT_TYPE: &str = "gene.transfer_recorded";
pub(super) const CONTRADICTION_EVENT_TYPE: &str = "gene.contradiction";
pub(super) const SPECIES_EVENT_TYPE: &str = "gene.species_created";
const GENE_PREFIX: &str = "gene:";

/// Deterministic minimum count of measured paired trials the origin
/// promotion's verified selection receipt must carry before a Gene can be
/// extracted from it. Below this, one lucky trial cannot become reusable
/// intelligence.
pub(super) const GENE_MIN_EVIDENCE_TRIALS: u32 = 3;

/// Deterministic minimum count of distinct positive recipient lineages a
/// domain must carry before speciation can admit a specialist species.
pub(super) const SPECIATION_MIN_LINEAGES: usize = 3;

/// Deterministic minimum mean measured effect (in basis points of paired
/// correctness delta) across a domain's positive transfer trials before
/// speciation can admit a specialist species.
pub(super) const SPECIATION_MIN_EFFECT_BPS: i64 = 300;

pub(super) fn gene_event_id(gene_id: &str) -> String {
    format!("{GENE_PREFIX}{gene_id}:extracted")
}

pub(super) fn gene_aggregate_id(gene_id: &str) -> String {
    format!("{GENE_PREFIX}{gene_id}")
}

pub(super) fn transfer_applied_event_id(trial_id: &str) -> String {
    format!("{GENE_PREFIX}transfer:{trial_id}:applied")
}

pub(super) fn transfer_recorded_event_id(trial_id: &str) -> String {
    format!("{GENE_PREFIX}transfer:{trial_id}:recorded")
}

pub(super) fn transfer_aggregate_id(trial_id: &str) -> String {
    format!("{GENE_PREFIX}transfer:{trial_id}")
}

pub(super) fn contradiction_event_id(gene_id: &str) -> String {
    format!("{GENE_PREFIX}{gene_id}:contradiction")
}

pub(super) fn species_event_id(species_id: &str) -> String {
    format!("{GENE_PREFIX}species:{species_id}:created")
}

pub(super) fn species_aggregate_id(species_id: &str) -> String {
    format!("{GENE_PREFIX}species:{species_id}")
}

fn is_gene_bank_event(event: &StoredEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        GENE_EVENT_TYPE
            | TRANSFER_APPLIED_EVENT_TYPE
            | TRANSFER_RECORDED_EVENT_TYPE
            | CONTRADICTION_EVENT_TYPE
            | SPECIES_EVENT_TYPE
    ) || event.event_id.starts_with(GENE_PREFIX)
        || event.aggregate_id.starts_with(GENE_PREFIX)
}

fn parse_operation(operation: &str) -> Result<ReferenceInstruction, ExecuteError> {
    match operation {
        "identity" => Ok(ReferenceInstruction::Identity),
        "ascii_uppercase" => Ok(ReferenceInstruction::AsciiUppercase),
        _ => Err(ExecuteError::Internal),
    }
}

fn event_record(event: &StoredEvent) -> GeneEventRecord {
    GeneEventRecord {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        aggregate_id: event.aggregate_id.clone(),
        event_hash: hex_encode(&event.hash),
    }
}

// ---------------------------------------------------------------------
// Decoding: every payload is re-derived from canonical JSON and rejected
// unless the stored bytes are exactly the canonical encoding, exactly like
// `champion::decode_champion_transition`.
// ---------------------------------------------------------------------

macro_rules! decode_canonical {
    ($name:ident, $payload:ty, $label:literal) => {
        pub(super) fn $name(event: &StoredEvent) -> Result<$payload, ControlError> {
            let payload = serde_json::from_slice::<$payload>(&event.payload).map_err(|_| {
                ControlError::Projection(concat!($label, " payload is invalid").to_owned())
            })?;
            let canonical_value = serde_json::to_value(&payload)?;
            if serde_json::to_vec(&canonical_value)? != event.payload {
                return Err(ControlError::Projection(
                    concat!($label, " payload is not canonical").to_owned(),
                ));
            }
            Ok(payload)
        }
    };
}

decode_canonical!(decode_gene_extracted, GeneExtractedPayload, "Gene");
decode_canonical!(
    decode_transfer_applied,
    GeneTransferAppliedPayload,
    "Gene transfer applied"
);
decode_canonical!(
    decode_transfer_recorded,
    GeneTransferRecordedPayload,
    "Gene transfer recorded"
);
decode_canonical!(
    decode_contradiction,
    GeneContradictionPayload,
    "Gene contradiction"
);
decode_canonical!(decode_species, GeneSpeciesPayload, "Gene species");

pub(super) fn gene_record(payload: GeneExtractedPayload, event: &StoredEvent) -> GeneRecord {
    GeneRecord {
        payload,
        event: event_record(event),
    }
}

fn contradiction_record(
    payload: GeneContradictionPayload,
    event: &StoredEvent,
) -> GeneContradictionRecord {
    GeneContradictionRecord {
        payload,
        event: event_record(event),
    }
}

pub(super) fn species_record(
    payload: GeneSpeciesPayload,
    event: &StoredEvent,
) -> GeneSpeciesRecord {
    GeneSpeciesRecord {
        payload,
        event: event_record(event),
    }
}

// ---------------------------------------------------------------------
// Gene extraction
// ---------------------------------------------------------------------

pub(super) fn existing_gene(
    history: &[StoredEvent],
    gene_id: &str,
    promotion_transition_id: &str,
) -> Result<Option<GeneRecord>, ExecuteError> {
    let event_id = gene_event_id(gene_id);
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_gene_extracted(event).map_err(|_| ExecuteError::Internal)?;
    if payload.promotion_transition_id != promotion_transition_id {
        return Err(ExecuteError::Rejected(
            "gene_id is already bound to a different promotion".to_owned(),
        ));
    }
    Ok(Some(gene_record(payload, event)))
}

/// Derives the only payload the policy admits for `gene_id` extracted from
/// `promotion_transition_id`, or refuses when the origin evidence does not
/// meet the deterministic minimum-evidence threshold.
pub(super) fn gene_extraction_payload(
    artifacts: &ArtifactStore,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    gene_id: &str,
    promotion_transition_id: &str,
) -> Result<GeneExtractedPayload, ExecuteError> {
    let promotion_event_id = champion_event_id(promotion_transition_id);
    let promotion_event = history
        .iter()
        .find(|event| event.event_id == promotion_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let promotion_payload =
        decode_champion_transition(promotion_event).map_err(|_| ExecuteError::Internal)?;
    if promotion_payload.kind != ChampionTransitionKind::Promoted {
        return Err(ExecuteError::Rejected(
            "only a Champion promotion carries evidence a Gene can be extracted from".to_owned(),
        ));
    }
    let promotion = promotion_payload
        .promotion
        .as_ref()
        .ok_or(ExecuteError::Internal)?;

    let assessment_event = history
        .iter()
        .find(|event| event.event_id == promotion.assessment_event_id)
        .ok_or(ExecuteError::Internal)?;
    if hex_encode(&assessment_event.hash) != promotion.assessment_event_hash {
        return Err(ExecuteError::Internal);
    }
    let assessment =
        decode_forge_assessment(assessment_event).map_err(|_| ExecuteError::Internal)?;
    if assessment.assessment_id != promotion.assessment_id
        || assessment.outcome != ForgeAssessmentOutcome::MetricsPassed
    {
        return Err(ExecuteError::Internal);
    }

    let proposal_event_id = forge_event_id(&assessment.proposal_id);
    let proposal_event = history
        .iter()
        .find(|event| event.event_id == proposal_event_id)
        .ok_or(ExecuteError::Internal)?;
    if hex_encode(&proposal_event.hash) != assessment.proposal_event_hash {
        return Err(ExecuteError::Internal);
    }
    let proposal = decode_forge_proposal(proposal_event).map_err(|_| ExecuteError::Internal)?;

    let world = registered
        .world(&assessment.world_id)
        .ok_or(ExecuteError::Internal)?;
    let selection_event = history
        .iter()
        .find(|event| event.event_id == assessment.selection_event_id)
        .ok_or(ExecuteError::Internal)?;
    let verified = verify_selection_event_in(
        &hephaestus_ledger::EventIndex::build(history),
        artifacts,
        selection_event,
        world.compiled(),
    )
    .map_err(|_| ExecuteError::Internal)?;
    let receipt = verified.receipt().clone();
    let selection_event_hash = verified.event().event_hash.clone();
    if receipt.evaluation_id() != assessment.evaluation_id
        || receipt.world_id() != assessment.world_id
        || receipt.parent_genome_id() != assessment.parent_genome_id
        || receipt.candidate_genome_id() != assessment.child_genome_id
    {
        return Err(ExecuteError::Internal);
    }

    let evidence_trials = receipt.correctness_regressions()
        + receipt.correctness_unchanged()
        + receipt.correctness_improvements();
    if evidence_trials < GENE_MIN_EVIDENCE_TRIALS {
        return Err(ExecuteError::Rejected(format!(
            "promotion evidence has {evidence_trials} measured paired trials; a Gene requires at least {GENE_MIN_EVIDENCE_TRIALS}"
        )));
    }

    Ok(GeneExtractedPayload {
        schema_version: 1,
        gene_id: gene_id.to_owned(),
        promotion_transition_id: promotion_transition_id.to_owned(),
        promotion_event_id: promotion_event.event_id.clone(),
        promotion_event_hash: hex_encode(&promotion_event.hash),
        assessment_id: assessment.assessment_id.clone(),
        assessment_event_id: assessment_event.event_id.clone(),
        assessment_event_hash: hex_encode(&assessment_event.hash),
        proposal_id: proposal.proposal_id.clone(),
        proposal_event_id: proposal_event.event_id.clone(),
        proposal_event_hash: hex_encode(&proposal_event.hash),
        selection_event_id: assessment.selection_event_id.clone(),
        selection_event_hash,
        invariant_event_id: promotion.invariant_event_id.clone(),
        invariant_event_hash: promotion.invariant_event_hash.clone(),
        world_id: assessment.world_id.clone(),
        origin_parent_genome_id: assessment.parent_genome_id.clone(),
        origin_child_genome_id: assessment.child_genome_id.clone(),
        operation_before: proposal.operation_before.clone(),
        operation_after: proposal.operation_after.clone(),
        evidence_trials,
        evidence_threshold: GENE_MIN_EVIDENCE_TRIALS,
    })
}

// ---------------------------------------------------------------------
// Transfer trials
// ---------------------------------------------------------------------

pub(super) fn existing_transfer_applied(
    history: &[StoredEvent],
    trial_id: &str,
    gene_id: &str,
    to_genome_id: &str,
) -> Result<Option<GeneTransferAppliedPayload>, ExecuteError> {
    let event_id = transfer_applied_event_id(trial_id);
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_transfer_applied(event).map_err(|_| ExecuteError::Internal)?;
    if payload.gene_id != gene_id || payload.to_genome_id != to_genome_id {
        return Err(ExecuteError::Rejected(
            "trial_id is already bound to a different Gene or recipient".to_owned(),
        ));
    }
    Ok(Some(payload))
}

/// Applies `gene_id`'s mutation to `to_genome_id` through the ordinary
/// Genome compiler, exactly like Forge child compilation. The recipient
/// must currently carry the Gene's exact origin (pre-mutation) operation.
pub(super) fn transfer_applied_payload(
    registered: &RegisteredObjects,
    artifacts: &ArtifactStore,
    history: &[StoredEvent],
    trial_id: &str,
    gene_id: &str,
    to_genome_id: &str,
) -> Result<GeneTransferAppliedPayload, ExecuteError> {
    let gene_evt_id = gene_event_id(gene_id);
    let gene_event = history
        .iter()
        .find(|event| event.event_id == gene_evt_id)
        .ok_or(ExecuteError::NotFound)?;
    let gene = decode_gene_extracted(gene_event).map_err(|_| ExecuteError::Internal)?;

    let recipient = registered
        .genome(to_genome_id)
        .ok_or(ExecuteError::NotFound)?;
    let world_id = recipient.record().world_id.clone();
    let world = registered.world(&world_id).ok_or(ExecuteError::Internal)?;

    let prompt_before_id = recipient
        .compiled()
        .artifact_id("agent.prompt")
        .ok_or_else(|| {
            ExecuteError::Rejected("recipient Genome has no supported prompt to mutate".to_owned())
        })?
        .to_owned();
    let prompt_id =
        ArtifactId::parse(prompt_before_id.clone()).map_err(|_| ExecuteError::Internal)?;
    let prompt_bytes = artifacts
        .get(&prompt_id)
        .map_err(|_| ExecuteError::Internal)?;
    let prompt_text = std::str::from_utf8(&prompt_bytes).map_err(|_| ExecuteError::Internal)?;
    let current = ReferenceInstruction::parse(prompt_text).map_err(|_| {
        ExecuteError::Rejected(
            "recipient Genome prompt is outside the supported mutation language".to_owned(),
        )
    })?;
    if reference_instruction_operation(current) != gene.operation_before {
        return Err(ExecuteError::Rejected(
            "recipient Genome does not carry the Gene's origin operation".to_owned(),
        ));
    }
    let target_after = parse_operation(&gene.operation_after)?;
    let after_text = mutate_reference_instruction_document(prompt_text, current, target_after)
        .map_err(|()| {
            ExecuteError::Rejected(
                "recipient Genome prompt is outside the Forge mutation scope".to_owned(),
            )
        })?;
    let prompt_after = artifacts
        .put(after_text.as_bytes())
        .map_err(|_| ExecuteError::Internal)?;

    let child = compile_gene_child(
        registered,
        world.compiled(),
        artifacts,
        to_genome_id,
        &world_id,
        trial_id,
        prompt_after.as_str(),
    )?;

    Ok(GeneTransferAppliedPayload {
        schema_version: 1,
        trial_id: trial_id.to_owned(),
        gene_id: gene_id.to_owned(),
        gene_event_id: gene_event.event_id.clone(),
        gene_event_hash: hex_encode(&gene_event.hash),
        to_genome_id: to_genome_id.to_owned(),
        world_id,
        child,
        prompt_artifact_before: prompt_before_id,
        prompt_artifact_after: prompt_after.as_str().to_owned(),
    })
}

fn compile_gene_child(
    registered: &RegisteredObjects,
    world: &CompiledWorld,
    artifact_store: &ArtifactStore,
    parent_genome_id: &str,
    world_id: &str,
    trial_id: &str,
    prompt_artifact: &str,
) -> Result<GenomeRecord, ExecuteError> {
    let parent = registered
        .genome(parent_genome_id)
        .ok_or(ExecuteError::NotFound)?;
    let mut source: serde_json::Value = serde_json::from_slice(parent.compiled().canonical_json())
        .map_err(|_| ExecuteError::Internal)?;
    let object = source.as_object_mut().ok_or(ExecuteError::Internal)?;
    object.insert(
        "name".to_owned(),
        serde_json::Value::String(format!("{}-gene-{trial_id}", parent.record().name)),
    );
    object.insert(
        "parents".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(parent_genome_id.to_owned())]),
    );
    object
        .get_mut("artifacts")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ExecuteError::Internal)?
        .insert(
            "agent.prompt".to_owned(),
            serde_json::Value::String(prompt_artifact.to_owned()),
        );
    let source = serde_json::to_string(&source).map_err(|_| ExecuteError::Internal)?;
    let parents = registered
        .genomes()
        .filter(|genome| genome.record().world_id == world_id)
        .map(|genome| (genome.record().genome_id.clone(), genome.compiled().clone()))
        .collect::<BTreeMap<_, _>>();
    let child = compile_genome(&source, SourceFormat::Json, world, &parents, artifact_store)
        .map_err(|error| {
            ExecuteError::Rejected(format!("Gene transfer child rejected: {error}"))
        })?;
    let artifact = artifact_store
        .put(child.canonical_json())
        .map_err(|_| ExecuteError::Internal)?;
    Ok(GenomeRecord {
        genome_id: child.id().to_owned(),
        name: child.name().to_owned(),
        world_id: world_id.to_owned(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: child.parents().to_vec(),
    })
}

pub(super) fn existing_transfer_recorded(
    history: &[StoredEvent],
    trial_id: &str,
    evaluation_id: &str,
) -> Result<Option<GeneTransferRecordedPayload>, ExecuteError> {
    let event_id = transfer_recorded_event_id(trial_id);
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_transfer_recorded(event).map_err(|_| ExecuteError::Internal)?;
    if payload.evaluation_id != evaluation_id {
        return Err(ExecuteError::Rejected(
            "trial_id is already bound to a different evaluation".to_owned(),
        ));
    }
    Ok(Some(payload))
}

/// Derives the only recorded effect the policy admits for `trial_id` from
/// its already-applied transfer and the verified paired selection of
/// `evaluation_id`.
pub(super) fn transfer_recorded_payload(
    artifacts: &ArtifactStore,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    trial_id: &str,
    evaluation_id: &str,
) -> Result<GeneTransferRecordedPayload, ExecuteError> {
    let applied_event_id = transfer_applied_event_id(trial_id);
    let applied_event = history
        .iter()
        .find(|event| event.event_id == applied_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let applied = decode_transfer_applied(applied_event).map_err(|_| ExecuteError::Internal)?;

    let selection_event_id = format!("arena:selection:{evaluation_id}:selected");
    let selection_event = history
        .iter()
        .find(|event| event.event_id == selection_event_id)
        .ok_or(ExecuteError::NotFound)?;
    if selection_event.event_type != "selection.recorded" {
        return Err(ExecuteError::Rejected(
            "evaluation_id has no recorded selection".to_owned(),
        ));
    }
    let world = registered
        .world(&applied.world_id)
        .ok_or(ExecuteError::Internal)?;
    let verified = verify_selection_event_in(
        &hephaestus_ledger::EventIndex::build(history),
        artifacts,
        selection_event,
        world.compiled(),
    )
    .map_err(|_| ExecuteError::Internal)?;
    let receipt = verified.receipt().clone();
    let verified_event_id = verified.event().event_id.clone();
    let selection_event_hash = verified.event().event_hash.clone();
    let selection_receipt_artifact_id = verified.event().receipt_artifact_id.clone();

    if receipt.evaluation_id() != evaluation_id
        || receipt.world_id() != applied.world_id
        || receipt.parent_genome_id() != applied.to_genome_id
        || receipt.candidate_genome_id() != applied.child.genome_id
    {
        return Err(ExecuteError::Rejected(
            "selection evidence does not match the transfer trial's recipient and child".to_owned(),
        ));
    }

    let outcome = if receipt.lower_bps() > 0 {
        GeneTransferOutcome::Positive
    } else if receipt.upper_bps() < 0 {
        GeneTransferOutcome::Negative
    } else {
        GeneTransferOutcome::Neutral
    };

    Ok(GeneTransferRecordedPayload {
        schema_version: 1,
        trial_id: trial_id.to_owned(),
        gene_id: applied.gene_id.clone(),
        applied_event_id: applied_event.event_id.clone(),
        applied_event_hash: hex_encode(&applied_event.hash),
        evaluation_id: evaluation_id.to_owned(),
        selection_event_id: verified_event_id,
        selection_event_hash,
        selection_receipt_artifact_id,
        outcome,
        estimate_bps: receipt.estimate_bps(),
        lower_bps: receipt.lower_bps(),
        upper_bps: receipt.upper_bps(),
    })
}

pub(super) fn transfer_record(
    applied: GeneTransferAppliedPayload,
    applied_event: &StoredEvent,
    recorded: Option<GeneTransferRecordedPayload>,
    recorded_event: Option<&StoredEvent>,
) -> GeneTransferRecord {
    GeneTransferRecord {
        applied,
        applied_event: event_record(applied_event),
        recorded,
        recorded_event: recorded_event.map(event_record),
    }
}

// ---------------------------------------------------------------------
// Contradiction detection
// ---------------------------------------------------------------------

fn applied_payload_for_trial(
    history: &[StoredEvent],
    trial_id: &str,
) -> Result<GeneTransferAppliedPayload, ControlError> {
    let event_id = transfer_applied_event_id(trial_id);
    let event = history
        .iter()
        .find(|event| event.event_id == event_id)
        .ok_or_else(|| ControlError::Projection("Gene transfer trial is missing".to_owned()))?;
    decode_transfer_applied(event)
}

pub(super) fn existing_contradiction(
    history: &[StoredEvent],
    gene_id: &str,
) -> Option<GeneContradictionRecord> {
    let event_id = contradiction_event_id(gene_id);
    let event = history.iter().find(|event| event.event_id == event_id)?;
    let payload = decode_contradiction(event).ok()?;
    Some(contradiction_record(payload, event))
}

/// Scans every recorded transfer trial for `gene_id`, in ledger order, and
/// returns the deterministic contradiction payload the first time both a
/// positive and a negative outcome exist. Once recorded, a contradiction is
/// never recomputed away: this is only called when no contradiction event
/// yet exists for the Gene.
pub(super) fn detect_contradiction(
    history: &[StoredEvent],
    gene_id: &str,
) -> Result<Option<GeneContradictionPayload>, ControlError> {
    let mut first_positive: Option<(&StoredEvent, GeneTransferRecordedPayload)> = None;
    let mut first_negative: Option<(&StoredEvent, GeneTransferRecordedPayload)> = None;
    for event in history
        .iter()
        .filter(|event| event.event_type == TRANSFER_RECORDED_EVENT_TYPE)
    {
        let payload = decode_transfer_recorded(event)?;
        if payload.gene_id != gene_id {
            continue;
        }
        match payload.outcome {
            GeneTransferOutcome::Positive if first_positive.is_none() => {
                first_positive = Some((event, payload));
            }
            GeneTransferOutcome::Negative if first_negative.is_none() => {
                first_negative = Some((event, payload));
            }
            _ => {}
        }
        if first_positive.is_some() && first_negative.is_some() {
            break;
        }
    }
    let (Some((pos_event, pos_payload)), Some((neg_event, neg_payload))) =
        (first_positive, first_negative)
    else {
        return Ok(None);
    };
    let pos_world = applied_payload_for_trial(history, &pos_payload.trial_id)?.world_id;
    let neg_world = applied_payload_for_trial(history, &neg_payload.trial_id)?.world_id;
    Ok(Some(GeneContradictionPayload {
        schema_version: 1,
        gene_id: gene_id.to_owned(),
        positive_trial_id: pos_payload.trial_id.clone(),
        positive_world_id: pos_world,
        positive_event_id: pos_event.event_id.clone(),
        positive_event_hash: hex_encode(&pos_event.hash),
        negative_trial_id: neg_payload.trial_id.clone(),
        negative_world_id: neg_world,
        negative_event_id: neg_event.event_id.clone(),
        negative_event_hash: hex_encode(&neg_event.hash),
    }))
}

// ---------------------------------------------------------------------
// Speciation
// ---------------------------------------------------------------------

pub(super) fn existing_species(
    history: &[StoredEvent],
    species_id: &str,
    gene_id: &str,
    domain_world_id: &str,
) -> Result<Option<GeneSpeciesRecord>, ExecuteError> {
    let event_id = species_event_id(species_id);
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_species(event).map_err(|_| ExecuteError::Internal)?;
    if payload.gene_id != gene_id || payload.domain_world_id != domain_world_id {
        return Err(ExecuteError::Rejected(
            "species_id is already bound to a different Gene or domain".to_owned(),
        ));
    }
    Ok(Some(species_record(payload, event)))
}

/// Admits a specialist species only when the domain's recorded transfer
/// evidence for `gene_id` shows a persistent (zero recorded negatives) and
/// statistically significant (at least [`SPECIATION_MIN_LINEAGES`] distinct
/// positive lineages, mean effect at least [`SPECIATION_MIN_EFFECT_BPS`])
/// advantage. Refuses with an explicit reason otherwise.
pub(super) fn speciation_payload(
    history: &[StoredEvent],
    species_id: &str,
    gene_id: &str,
    domain_world_id: &str,
) -> Result<GeneSpeciesPayload, ExecuteError> {
    let gene_evt_id = gene_event_id(gene_id);
    let gene_event = history
        .iter()
        .find(|event| event.event_id == gene_evt_id)
        .ok_or(ExecuteError::NotFound)?;

    let mut supporting: Vec<(&StoredEvent, GeneTransferRecordedPayload, String)> = Vec::new();
    let mut has_negative_in_domain = false;
    for event in history
        .iter()
        .filter(|event| event.event_type == TRANSFER_RECORDED_EVENT_TYPE)
    {
        let payload = decode_transfer_recorded(event).map_err(|_| ExecuteError::Internal)?;
        if payload.gene_id != gene_id {
            continue;
        }
        let applied = applied_payload_for_trial(history, &payload.trial_id)
            .map_err(|_| ExecuteError::Internal)?;
        if applied.world_id != domain_world_id {
            continue;
        }
        match payload.outcome {
            GeneTransferOutcome::Negative => has_negative_in_domain = true,
            GeneTransferOutcome::Positive => {
                supporting.push((event, payload, applied.to_genome_id));
            }
            GeneTransferOutcome::Neutral => {}
        }
    }
    if has_negative_in_domain {
        return Err(ExecuteError::Rejected(
            "domain has a recorded negative transfer; the advantage is not persistent".to_owned(),
        ));
    }
    let lineages: BTreeSet<String> = supporting
        .iter()
        .map(|(_, _, to_genome_id)| to_genome_id.clone())
        .collect();
    if lineages.len() < SPECIATION_MIN_LINEAGES {
        return Err(ExecuteError::Rejected(format!(
            "domain has {} distinct positive lineage(s); speciation requires at least {SPECIATION_MIN_LINEAGES}",
            lineages.len()
        )));
    }
    let total: i64 = supporting
        .iter()
        .map(|(_, payload, _)| payload.estimate_bps)
        .sum();
    let count = i64::try_from(supporting.len()).map_err(|_| ExecuteError::Internal)?;
    let average = total / count;
    if average < SPECIATION_MIN_EFFECT_BPS {
        return Err(ExecuteError::Rejected(format!(
            "mean measured effect {average} bps is below the speciation threshold of {SPECIATION_MIN_EFFECT_BPS} bps"
        )));
    }
    let mut supporting_event_ids: Vec<String> = supporting
        .iter()
        .map(|(event, _, _)| event.event_id.clone())
        .collect();
    supporting_event_ids.sort();

    Ok(GeneSpeciesPayload {
        schema_version: 1,
        species_id: species_id.to_owned(),
        gene_id: gene_id.to_owned(),
        gene_event_id: gene_event.event_id.clone(),
        gene_event_hash: hex_encode(&gene_event.hash),
        domain_world_id: domain_world_id.to_owned(),
        lineage_genome_ids: lineages.into_iter().collect(),
        supporting_event_ids,
        average_estimate_bps: average,
        minimum_lineages: u32::try_from(SPECIATION_MIN_LINEAGES).unwrap_or(u32::MAX),
        minimum_effect_bps: SPECIATION_MIN_EFFECT_BPS,
    })
}

// ---------------------------------------------------------------------
// Projections
// ---------------------------------------------------------------------

/// Full projection of one Gene: origin evidence, every transfer trial in
/// ledger order, any contradiction, and any species created from it.
pub(super) fn gene_aggregate(
    history: &[StoredEvent],
    gene_id: &str,
) -> Result<GeneAggregateRecord, ExecuteError> {
    let gene_evt_id = gene_event_id(gene_id);
    let gene_event = history
        .iter()
        .find(|event| event.event_id == gene_evt_id)
        .ok_or(ExecuteError::NotFound)?;
    let gene_payload = decode_gene_extracted(gene_event).map_err(|_| ExecuteError::Internal)?;
    let gene = gene_record(gene_payload, gene_event);

    let mut transfers = Vec::new();
    for applied_event in history
        .iter()
        .filter(|event| event.event_type == TRANSFER_APPLIED_EVENT_TYPE)
    {
        let applied = decode_transfer_applied(applied_event).map_err(|_| ExecuteError::Internal)?;
        if applied.gene_id != gene_id {
            continue;
        }
        let recorded_event_id = transfer_recorded_event_id(&applied.trial_id);
        let recorded_event = history
            .iter()
            .find(|event| event.event_id == recorded_event_id);
        let recorded = recorded_event
            .map(|event| decode_transfer_recorded(event).map_err(|_| ExecuteError::Internal))
            .transpose()?;
        transfers.push(transfer_record(
            applied,
            applied_event,
            recorded,
            recorded_event,
        ));
    }

    let contradiction = existing_contradiction(history, gene_id);

    let mut species = Vec::new();
    for event in history
        .iter()
        .filter(|event| event.event_type == SPECIES_EVENT_TYPE)
    {
        let payload = decode_species(event).map_err(|_| ExecuteError::Internal)?;
        if payload.gene_id == gene_id {
            species.push(species_record(payload, event));
        }
    }

    Ok(GeneAggregateRecord {
        gene,
        transfers,
        contradiction,
        species,
    })
}

/// Every extracted Gene with its aggregate transfer counts, in ledger order.
pub(super) fn gene_summaries(history: &[StoredEvent]) -> Result<Vec<GeneSummary>, ExecuteError> {
    let mut summaries = Vec::new();
    for gene_event in history
        .iter()
        .filter(|event| event.event_type == GENE_EVENT_TYPE)
    {
        let payload = decode_gene_extracted(gene_event).map_err(|_| ExecuteError::Internal)?;
        let gene_id = payload.gene_id.clone();

        let mut lineages: BTreeSet<String> = BTreeSet::new();
        let (mut positive, mut neutral, mut negative) = (0_u32, 0_u32, 0_u32);
        for event in history
            .iter()
            .filter(|event| event.event_type == TRANSFER_RECORDED_EVENT_TYPE)
        {
            let recorded = decode_transfer_recorded(event).map_err(|_| ExecuteError::Internal)?;
            if recorded.gene_id != gene_id {
                continue;
            }
            let applied = applied_payload_for_trial(history, &recorded.trial_id)
                .map_err(|_| ExecuteError::Internal)?;
            lineages.insert(applied.to_genome_id);
            match recorded.outcome {
                GeneTransferOutcome::Positive => positive += 1,
                GeneTransferOutcome::Neutral => neutral += 1,
                GeneTransferOutcome::Negative => negative += 1,
            }
        }

        let contradiction = existing_contradiction(history, &gene_id).is_some();
        let species_ids: Vec<String> = history
            .iter()
            .filter(|event| event.event_type == SPECIES_EVENT_TYPE)
            .filter_map(|event| decode_species(event).ok())
            .filter(|species| species.gene_id == gene_id)
            .map(|species| species.species_id)
            .collect();

        summaries.push(GeneSummary {
            payload,
            event: event_record(gene_event),
            lineages: u32::try_from(lineages.len()).unwrap_or(u32::MAX),
            positive,
            neutral,
            negative,
            contradiction,
            species_ids,
        });
    }
    Ok(summaries)
}

// ---------------------------------------------------------------------
// Replay verification
// ---------------------------------------------------------------------

/// Recomputes every Gene Bank event from the history that preceded it,
/// exactly like [`super::champion::verify_champion_history`]. `artifacts` is
/// the daemon's already-open artifact store, reused for every event instead
/// of reopening it (see `TECH_DEBT.md` TD-16).
#[allow(clippy::too_many_lines)]
pub(super) fn verify_gene_bank_history(
    artifacts: &ArtifactStore,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_gene_bank_history_with(
        artifacts,
        history,
        registered,
        &mut super::EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_gene_bank_history`]; see `EvidenceCache`.
#[allow(clippy::too_many_lines)]
pub(super) fn verify_gene_bank_history_with(
    artifacts: &ArtifactStore,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut super::EvidenceCache,
) -> Result<(), ControlError> {
    let invalid = |message: &str| ControlError::Projection(message.to_owned());
    for (index, event) in history.iter().enumerate() {
        if !is_gene_bank_event(event) || cache.contains("gene_bank", event) {
            continue;
        }
        let prefix = &history[..index];
        match event.event_type.as_str() {
            GENE_EVENT_TYPE => {
                let payload = decode_gene_extracted(event)?;
                if event.event_id != gene_event_id(&payload.gene_id)
                    || event.aggregate_id != gene_aggregate_id(&payload.gene_id)
                    || validate_job_id(&payload.gene_id).is_err()
                {
                    return Err(invalid("Gene event identity is invalid"));
                }
                let expected = gene_extraction_payload(
                    artifacts,
                    prefix,
                    registered,
                    &payload.gene_id,
                    &payload.promotion_transition_id,
                )
                .map_err(|_| invalid("Gene extraction was not admissible"))?;
                if payload != expected {
                    return Err(invalid("Gene payload differs from verified evidence"));
                }
            }
            TRANSFER_APPLIED_EVENT_TYPE => {
                let payload = decode_transfer_applied(event)?;
                if event.event_id != transfer_applied_event_id(&payload.trial_id)
                    || event.aggregate_id != transfer_aggregate_id(&payload.trial_id)
                    || validate_job_id(&payload.trial_id).is_err()
                {
                    return Err(invalid("Gene transfer applied event identity is invalid"));
                }
                let expected = transfer_applied_payload(
                    registered,
                    artifacts,
                    prefix,
                    &payload.trial_id,
                    &payload.gene_id,
                    &payload.to_genome_id,
                )
                .map_err(|_| invalid("Gene transfer application was not admissible"))?;
                if payload != expected {
                    return Err(invalid(
                        "Gene transfer applied payload differs from verified evidence",
                    ));
                }
            }
            TRANSFER_RECORDED_EVENT_TYPE => {
                let payload = decode_transfer_recorded(event)?;
                if event.event_id != transfer_recorded_event_id(&payload.trial_id)
                    || event.aggregate_id != transfer_aggregate_id(&payload.trial_id)
                {
                    return Err(invalid("Gene transfer recorded event identity is invalid"));
                }
                let expected = transfer_recorded_payload(
                    artifacts,
                    prefix,
                    registered,
                    &payload.trial_id,
                    &payload.evaluation_id,
                )
                .map_err(|_| invalid("Gene transfer record was not admissible"))?;
                if payload != expected {
                    return Err(invalid(
                        "Gene transfer recorded payload differs from verified evidence",
                    ));
                }
            }
            CONTRADICTION_EVENT_TYPE => {
                let payload = decode_contradiction(event)?;
                if event.event_id != contradiction_event_id(&payload.gene_id) {
                    return Err(invalid("Gene contradiction event identity is invalid"));
                }
                let expected = detect_contradiction(prefix, &payload.gene_id)?
                    .ok_or_else(|| invalid("Gene contradiction was not admissible"))?;
                if payload != expected {
                    return Err(invalid(
                        "Gene contradiction payload differs from verified evidence",
                    ));
                }
            }
            SPECIES_EVENT_TYPE => {
                let payload = decode_species(event)?;
                if event.event_id != species_event_id(&payload.species_id)
                    || event.aggregate_id != species_aggregate_id(&payload.species_id)
                    || validate_job_id(&payload.species_id).is_err()
                {
                    return Err(invalid("Gene species event identity is invalid"));
                }
                let expected = speciation_payload(
                    prefix,
                    &payload.species_id,
                    &payload.gene_id,
                    &payload.domain_world_id,
                )
                .map_err(|_| invalid("Gene speciation was not admissible"))?;
                if payload != expected {
                    return Err(invalid(
                        "Gene species payload differs from verified evidence",
                    ));
                }
            }
            _ => return Err(invalid("unexpected Gene Bank event type")),
        }
        cache.insert("gene_bank", event);
    }
    Ok(())
}

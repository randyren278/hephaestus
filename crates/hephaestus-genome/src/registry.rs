use std::{collections::BTreeMap, error::Error, fmt};

use hephaestus_ledger::{ArtifactId, ArtifactStore, LedgerError, StoredEvent};
use serde::{Deserialize, Serialize};

use crate::{
    CompileError, CompiledGenome, CompiledWorld, SourceFormat, compile_genome, compile_world,
};

/// Stable public metadata for an immutable Genome registration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenomeRecord {
    /// Content-derived Genome identity.
    pub genome_id: String,
    /// Stable display name.
    pub name: String,
    /// World identity under which this Genome was compiled.
    pub world_id: String,
    /// CAS address of the canonical Genome JSON.
    pub artifact_id: String,
    /// Content-derived declared parents.
    pub parent_ids: Vec<String>,
}

/// Stable public metadata for an immutable World registration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldRecord {
    /// Content-derived World identity.
    pub world_id: String,
    /// Stable display name.
    pub name: String,
    /// CAS address of the canonical World JSON.
    pub artifact_id: String,
}

/// Kind of immutable object rejected during registration replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationKind {
    /// A World registration.
    World,
    /// A Genome registration.
    Genome,
}

/// Fail-closed error produced while rehydrating registered objects.
#[derive(Debug)]
pub enum RegistrationError {
    /// An artifact address or blob could not be verified.
    Ledger(LedgerError),
    /// A registration payload did not decode into its strict typed record.
    InvalidPayload {
        /// Event carrying the invalid payload.
        event_id: String,
        /// Object kind declared by the event type.
        kind: RegistrationKind,
    },
    /// A typed registration payload was valid JSON but not its canonical encoding.
    NonCanonicalPayload {
        /// Event carrying the noncanonical payload.
        event_id: String,
        /// Registered object kind.
        kind: RegistrationKind,
    },
    /// A canonical artifact was not UTF-8 JSON text.
    InvalidUtf8 {
        /// Claimed object identity.
        id: String,
        /// Registered object kind.
        kind: RegistrationKind,
    },
    /// The event aggregate did not equal the registered content identity.
    AggregateMismatch {
        /// Event carrying the invalid aggregate.
        event_id: String,
        /// Required aggregate identity.
        expected: String,
        /// Recorded aggregate identity.
        actual: String,
    },
    /// Artifact bytes compiled successfully but were not canonical compiler output.
    NonCanonicalArtifact {
        /// Claimed object identity.
        id: String,
        /// Registered object kind.
        kind: RegistrationKind,
    },
    /// Canonical source failed strict Genome or World compilation.
    Compile {
        /// Claimed object identity.
        id: String,
        /// Registered object kind.
        kind: RegistrationKind,
        /// Compiler rejection.
        source: CompileError,
    },
    /// Public registration metadata disagreed with the compiled artifact.
    MetadataMismatch {
        /// Claimed object identity.
        id: String,
        /// Registered object kind.
        kind: RegistrationKind,
        /// Record field that disagreed with compiled state.
        field: &'static str,
    },
    /// A Genome was registered before its World.
    WorldNotRegistered(String),
    /// A Genome declared a parent not present earlier in replay order.
    ParentNotRegistered(String),
    /// A Genome declared a parent registered under another World.
    CrossWorldParent {
        /// Declared parent Genome identity.
        parent_id: String,
        /// World under which the parent was registered.
        parent_world_id: String,
        /// World under which the child was registered.
        child_world_id: String,
    },
    /// A content identity was registered again with changed public metadata.
    ConflictingRegistration {
        /// Reused content identity.
        id: String,
        /// Registered object kind.
        kind: RegistrationKind,
    },
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for RegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ledger(error) => Some(error),
            Self::Compile { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<LedgerError> for RegistrationError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

/// A World record paired with its verified compiler output and ledger position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredWorld {
    registration_sequence: u64,
    record: WorldRecord,
    compiled: CompiledWorld,
}

impl RegisteredWorld {
    /// Returns the canonical ledger sequence of the first registration.
    #[must_use]
    pub const fn registration_sequence(&self) -> u64 {
        self.registration_sequence
    }

    /// Returns stable public registration metadata.
    #[must_use]
    pub const fn record(&self) -> &WorldRecord {
        &self.record
    }

    /// Returns the compiler-verified immutable World.
    #[must_use]
    pub const fn compiled(&self) -> &CompiledWorld {
        &self.compiled
    }
}

/// A Genome record paired with its verified compiler output and ledger position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredGenome {
    registration_sequence: u64,
    record: GenomeRecord,
    compiled: CompiledGenome,
}

impl RegisteredGenome {
    /// Returns the canonical ledger sequence of the first registration.
    #[must_use]
    pub const fn registration_sequence(&self) -> u64 {
        self.registration_sequence
    }

    /// Returns stable public registration metadata.
    #[must_use]
    pub const fn record(&self) -> &GenomeRecord {
        &self.record
    }

    /// Returns the compiler-verified immutable Genome.
    #[must_use]
    pub const fn compiled(&self) -> &CompiledGenome {
        &self.compiled
    }
}

/// Deterministic trusted projection of all immutable World and Genome registrations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegisteredObjects {
    worlds: BTreeMap<String, RegisteredWorld>,
    genomes: BTreeMap<String, RegisteredGenome>,
}

impl RegisteredObjects {
    /// Replays verified ledger events into trusted compiler-backed registrations.
    ///
    /// Unrelated event types are ignored. Registrations are consumed in the supplied
    /// replay order, so each Genome's World and all parents must appear earlier.
    /// Identical duplicate records are idempotent and retain their first sequence.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError`] for a noncanonical event or artifact, missing or
    /// cross-World ancestry, changed immutable metadata, or compiler/storage failure.
    pub fn replay(
        events: &[StoredEvent],
        artifacts: &ArtifactStore,
    ) -> Result<Self, RegistrationError> {
        let mut registered = Self::default();
        for event in events {
            match event.event_type.as_str() {
                "world.registered" => registered.register_world(event, artifacts)?,
                "genome.registered" => registered.register_genome(event, artifacts)?,
                "forge.proposed" => registered.register_forge_child(event, artifacts)?,
                "gene.transfer_applied" => {
                    registered.register_gene_transfer_child(event, artifacts)?;
                }
                _ => {}
            }
        }
        Ok(registered)
    }

    /// Looks up a verified World by content identity.
    #[must_use]
    pub fn world(&self, id: &str) -> Option<&RegisteredWorld> {
        self.worlds.get(id)
    }

    /// Looks up a verified Genome by content identity.
    #[must_use]
    pub fn genome(&self, id: &str) -> Option<&RegisteredGenome> {
        self.genomes.get(id)
    }

    /// Iterates verified Worlds in canonical identity order.
    pub fn worlds(&self) -> impl Iterator<Item = &RegisteredWorld> {
        self.worlds.values()
    }

    /// Iterates verified Genomes in canonical identity order.
    pub fn genomes(&self) -> impl Iterator<Item = &RegisteredGenome> {
        self.genomes.values()
    }

    /// Returns a stable identity-keyed copy of public World records.
    #[must_use]
    pub fn world_records(&self) -> BTreeMap<String, WorldRecord> {
        self.worlds
            .iter()
            .map(|(id, registered)| (id.clone(), registered.record.clone()))
            .collect()
    }

    /// Returns a stable identity-keyed copy of public Genome records.
    #[must_use]
    pub fn genome_records(&self) -> BTreeMap<String, GenomeRecord> {
        self.genomes
            .iter()
            .map(|(id, registered)| (id.clone(), registered.record.clone()))
            .collect()
    }

    fn register_world(
        &mut self,
        event: &StoredEvent,
        artifacts: &ArtifactStore,
    ) -> Result<(), RegistrationError> {
        let kind = RegistrationKind::World;
        let record: WorldRecord = decode_canonical_payload(event, kind)?;
        require_aggregate(event, &record.world_id)?;
        if let Some(existing) = self.worlds.get(&record.world_id) {
            return if existing.record == record {
                Ok(())
            } else {
                Err(RegistrationError::ConflictingRegistration {
                    id: record.world_id,
                    kind,
                })
            };
        }

        let bytes = artifact_bytes(artifacts, &record.artifact_id)?;
        let source = std::str::from_utf8(&bytes).map_err(|_| RegistrationError::InvalidUtf8 {
            id: record.world_id.clone(),
            kind,
        })?;
        let compiled = compile_world(source, SourceFormat::Json, artifacts).map_err(|source| {
            RegistrationError::Compile {
                id: record.world_id.clone(),
                kind,
                source,
            }
        })?;
        if bytes != compiled.canonical_json() {
            return Err(RegistrationError::NonCanonicalArtifact {
                id: record.world_id,
                kind,
            });
        }
        require_metadata(
            compiled.id() == record.world_id,
            &record.world_id,
            kind,
            "world_id",
        )?;
        require_metadata(
            compiled.name() == record.name,
            &record.world_id,
            kind,
            "name",
        )?;
        require_metadata(
            compiled_id_artifact(compiled.id()) == Some(record.artifact_id.as_str()),
            &record.world_id,
            kind,
            "artifact_id",
        )?;

        self.worlds.insert(
            record.world_id.clone(),
            RegisteredWorld {
                registration_sequence: event.sequence,
                record,
                compiled,
            },
        );
        Ok(())
    }

    fn register_genome(
        &mut self,
        event: &StoredEvent,
        artifacts: &ArtifactStore,
    ) -> Result<(), RegistrationError> {
        let kind = RegistrationKind::Genome;
        let record: GenomeRecord = decode_canonical_payload(event, kind)?;
        self.register_genome_record(event, record, artifacts)
    }

    /// Decodes a canonical child-registering envelope (`forge.proposed` or
    /// `gene.transfer_applied`) and returns its parsed JSON body and decoded
    /// child `GenomeRecord`. Both callers additionally verify their own
    /// distinct aggregate identity from the returned body.
    fn decode_canonical_child_envelope(
        event: &StoredEvent,
        kind: RegistrationKind,
    ) -> Result<(serde_json::Value, GenomeRecord), RegistrationError> {
        let payload =
            serde_json::from_slice::<serde_json::Value>(&event.payload).map_err(|_| {
                RegistrationError::InvalidPayload {
                    event_id: event.event_id.clone(),
                    kind,
                }
            })?;
        let canonical =
            serde_json::to_vec(&payload).map_err(|_| RegistrationError::InvalidPayload {
                event_id: event.event_id.clone(),
                kind,
            })?;
        if canonical != event.payload {
            return Err(RegistrationError::NonCanonicalPayload {
                event_id: event.event_id.clone(),
                kind,
            });
        }
        let child =
            payload
                .get("child")
                .cloned()
                .ok_or_else(|| RegistrationError::InvalidPayload {
                    event_id: event.event_id.clone(),
                    kind,
                })?;
        let record = serde_json::from_value::<GenomeRecord>(child).map_err(|_| {
            RegistrationError::InvalidPayload {
                event_id: event.event_id.clone(),
                kind,
            }
        })?;
        Ok((payload, record))
    }

    fn register_forge_child(
        &mut self,
        event: &StoredEvent,
        artifacts: &ArtifactStore,
    ) -> Result<(), RegistrationError> {
        let kind = RegistrationKind::Genome;
        let (payload, record) = Self::decode_canonical_child_envelope(event, kind)?;
        let proposal_id = payload
            .get("proposal_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RegistrationError::InvalidPayload {
                event_id: event.event_id.clone(),
                kind,
            })?;
        if event.aggregate_id != format!("forge:{proposal_id}")
            || event.event_id != format!("forge:{proposal_id}:proposed")
        {
            return Err(RegistrationError::AggregateMismatch {
                event_id: event.event_id.clone(),
                expected: format!("forge:{proposal_id}"),
                actual: event.aggregate_id.clone(),
            });
        }
        self.register_genome_record(event, record, artifacts)
    }

    fn register_gene_transfer_child(
        &mut self,
        event: &StoredEvent,
        artifacts: &ArtifactStore,
    ) -> Result<(), RegistrationError> {
        let kind = RegistrationKind::Genome;
        let (payload, record) = Self::decode_canonical_child_envelope(event, kind)?;
        let trial_id = payload
            .get("trial_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RegistrationError::InvalidPayload {
                event_id: event.event_id.clone(),
                kind,
            })?;
        if event.aggregate_id != format!("gene:transfer:{trial_id}")
            || event.event_id != format!("gene:transfer:{trial_id}:applied")
        {
            return Err(RegistrationError::AggregateMismatch {
                event_id: event.event_id.clone(),
                expected: format!("gene:transfer:{trial_id}"),
                actual: event.aggregate_id.clone(),
            });
        }
        self.register_genome_record(event, record, artifacts)
    }

    fn register_genome_record(
        &mut self,
        event: &StoredEvent,
        record: GenomeRecord,
        artifacts: &ArtifactStore,
    ) -> Result<(), RegistrationError> {
        let kind = RegistrationKind::Genome;
        if event.event_type != "forge.proposed" && event.event_type != "gene.transfer_applied" {
            require_aggregate(event, &record.genome_id)?;
        }
        if let Some(existing) = self.genomes.get(&record.genome_id) {
            return if existing.record == record {
                Ok(())
            } else {
                Err(RegistrationError::ConflictingRegistration {
                    id: record.genome_id,
                    kind,
                })
            };
        }

        let world = self
            .worlds
            .get(&record.world_id)
            .ok_or_else(|| RegistrationError::WorldNotRegistered(record.world_id.clone()))?;
        for parent_id in &record.parent_ids {
            let parent = self
                .genomes
                .get(parent_id)
                .ok_or_else(|| RegistrationError::ParentNotRegistered(parent_id.clone()))?;
            if parent.record.world_id != record.world_id {
                return Err(RegistrationError::CrossWorldParent {
                    parent_id: parent_id.clone(),
                    parent_world_id: parent.record.world_id.clone(),
                    child_world_id: record.world_id.clone(),
                });
            }
        }

        let bytes = artifact_bytes(artifacts, &record.artifact_id)?;
        let source = std::str::from_utf8(&bytes).map_err(|_| RegistrationError::InvalidUtf8 {
            id: record.genome_id.clone(),
            kind,
        })?;
        let parents = self
            .genomes
            .values()
            .filter(|parent| parent.record.world_id == record.world_id)
            .map(|parent| (parent.record.genome_id.clone(), parent.compiled.clone()))
            .collect::<BTreeMap<_, _>>();
        let compiled = compile_genome(
            source,
            SourceFormat::Json,
            &world.compiled,
            &parents,
            artifacts,
        )
        .map_err(|source| RegistrationError::Compile {
            id: record.genome_id.clone(),
            kind,
            source,
        })?;
        if bytes != compiled.canonical_json() {
            return Err(RegistrationError::NonCanonicalArtifact {
                id: record.genome_id,
                kind,
            });
        }
        require_metadata(
            compiled.id() == record.genome_id,
            &record.genome_id,
            kind,
            "genome_id",
        )?;
        require_metadata(
            compiled.name() == record.name,
            &record.genome_id,
            kind,
            "name",
        )?;
        require_metadata(
            compiled.parents() == record.parent_ids,
            &record.genome_id,
            kind,
            "parent_ids",
        )?;
        require_metadata(
            compiled_id_artifact(compiled.id()) == Some(record.artifact_id.as_str()),
            &record.genome_id,
            kind,
            "artifact_id",
        )?;

        self.genomes.insert(
            record.genome_id.clone(),
            RegisteredGenome {
                registration_sequence: event.sequence,
                record,
                compiled,
            },
        );
        Ok(())
    }
}

fn decode_canonical_payload<T>(
    event: &StoredEvent,
    kind: RegistrationKind,
) -> Result<T, RegistrationError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let record = serde_json::from_slice::<T>(&event.payload).map_err(|_| {
        RegistrationError::InvalidPayload {
            event_id: event.event_id.clone(),
            kind,
        }
    })?;
    let canonical = serde_json::to_vec(&record).map_err(|_| RegistrationError::InvalidPayload {
        event_id: event.event_id.clone(),
        kind,
    })?;
    if canonical != event.payload {
        return Err(RegistrationError::NonCanonicalPayload {
            event_id: event.event_id.clone(),
            kind,
        });
    }
    Ok(record)
}

fn require_aggregate(event: &StoredEvent, expected: &str) -> Result<(), RegistrationError> {
    if event.aggregate_id != expected {
        return Err(RegistrationError::AggregateMismatch {
            event_id: event.event_id.clone(),
            expected: expected.to_owned(),
            actual: event.aggregate_id.clone(),
        });
    }
    Ok(())
}

fn artifact_bytes(
    artifacts: &ArtifactStore,
    artifact_id: &str,
) -> Result<Vec<u8>, RegistrationError> {
    Ok(artifacts.get(&ArtifactId::parse(artifact_id.to_owned())?)?)
}

fn require_metadata(
    valid: bool,
    id: &str,
    kind: RegistrationKind,
    field: &'static str,
) -> Result<(), RegistrationError> {
    if !valid {
        return Err(RegistrationError::MetadataMismatch {
            id: id.to_owned(),
            kind,
            field,
        });
    }
    Ok(())
}

fn compiled_id_artifact(id: &str) -> Option<&str> {
    id.rsplit_once(':').map(|(_, artifact)| artifact)
}

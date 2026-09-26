use std::{path::Path, time::Duration};

use blake3::Hasher;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::LedgerError;

/// Hash preceding the first canonical event, shared by every [`crate::EventLedger`] backend.
pub(crate) const GENESIS_HASH: [u8; 32] = [0; 32];

/// Caller-supplied event data before canonical sequencing and hashing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventInput {
    pub(crate) event_id: String,
    pub(crate) aggregate_id: String,
    pub(crate) event_type: String,
    pub(crate) actor: String,
    pub(crate) timestamp_millis: i64,
    pub(crate) payload: Vec<u8>,
}

impl EventInput {
    /// Creates an event input. Durable validation occurs during append.
    pub fn new(
        event_id: impl Into<String>,
        aggregate_id: impl Into<String>,
        event_type: impl Into<String>,
        actor: impl Into<String>,
        timestamp_millis: i64,
        payload: impl AsRef<[u8]>,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            aggregate_id: aggregate_id.into(),
            event_type: event_type.into(),
            actor: actor.into(),
            timestamp_millis,
            payload: payload.as_ref().to_vec(),
        }
    }
}

/// Canonical event with sequence and tamper-evident hash link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredEvent {
    /// Globally monotonic canonical sequence.
    pub sequence: u64,
    /// Stable caller-provided event identifier.
    pub event_id: String,
    /// Aggregate receiving the event.
    pub aggregate_id: String,
    /// Stable event type.
    pub event_type: String,
    /// Actor responsible for the event.
    pub actor: String,
    /// Caller-observed Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
    /// Opaque canonical event payload.
    pub payload: Vec<u8>,
    /// Hash of the immediately preceding event, or zero for genesis.
    pub previous_hash: [u8; 32],
    /// BLAKE3 hash over all canonical event fields.
    pub hash: [u8; 32],
}

/// Single-writer SQLite event ledger configured for WAL durability.
pub struct EventStore {
    connection: Connection,
    verified_head: Option<StoredEvent>,
}

impl EventStore {
    /// Opens or creates a durable event ledger.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when SQLite configuration or schema creation fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                global_sequence INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL UNIQUE,
                aggregate_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                actor TEXT NOT NULL,
                timestamp_millis INTEGER NOT NULL,
                payload BLOB NOT NULL,
                previous_hash BLOB NOT NULL CHECK(length(previous_hash) = 32),
                hash BLOB NOT NULL CHECK(length(hash) = 32)
            );",
        )?;
        let verified_history = load_verified(&connection)?;
        let verified_head = verified_history.last().cloned();
        Ok(Self {
            connection,
            verified_head,
        })
    }

    /// Appends one event atomically with its canonical sequence and hash link.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::DuplicateEventId`] for an existing identifier and
    /// propagates storage or integrity failures without committing a partial append.
    pub fn append(&mut self, input: EventInput) -> Result<StoredEvent, LedgerError> {
        validate_input(&input)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duplicate = transaction
            .query_row(
                "SELECT 1 FROM events WHERE event_id = ?1",
                [&input.event_id],
                |_| Ok(()),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(LedgerError::DuplicateEventId(input.event_id));
        }

        if load_tail(&transaction)? != self.verified_head {
            return Err(LedgerError::LedgerHeadChanged);
        }
        let (sequence, previous_hash) = match &self.verified_head {
            Some(previous) => (previous.sequence + 1, previous.hash),
            None => (1, GENESIS_HASH),
        };
        let hash = hash_event(sequence, &input, &previous_hash);

        transaction.execute(
            "INSERT INTO events (
                global_sequence, event_id, aggregate_id, event_type, actor,
                timestamp_millis, payload, previous_hash, hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                sequence,
                input.event_id,
                input.aggregate_id,
                input.event_type,
                input.actor,
                input.timestamp_millis,
                input.payload,
                previous_hash.as_slice(),
                hash.as_slice(),
            ],
        )?;
        transaction.commit()?;

        let event = StoredEvent {
            sequence,
            event_id: input.event_id,
            aggregate_id: input.aggregate_id,
            event_type: input.event_type,
            actor: input.actor,
            timestamp_millis: input.timestamp_millis,
            payload: input.payload,
            previous_hash,
            hash,
        };
        self.verified_head = Some(event.clone());
        Ok(event)
    }

    /// Reads canonical history in order and verifies every sequence and hash link.
    ///
    /// # Errors
    ///
    /// Returns a specific integrity error at the first modified, missing, duplicated,
    /// or incorrectly linked event.
    pub fn replay_verified(&self) -> Result<Vec<StoredEvent>, LedgerError> {
        load_verified(&self.connection)
    }
}

/// Validates required canonical fields, shared by every [`crate::EventLedger`] backend.
///
/// # Errors
///
/// Returns [`LedgerError::EmptyEventField`] for a blank required field.
pub(crate) fn validate_input(input: &EventInput) -> Result<(), LedgerError> {
    for (name, value) in [
        ("event_id", input.event_id.as_str()),
        ("aggregate_id", input.aggregate_id.as_str()),
        ("event_type", input.event_type.as_str()),
        ("actor", input.actor.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(LedgerError::EmptyEventField(name));
        }
    }
    Ok(())
}

fn load_verified(connection: &Connection) -> Result<Vec<StoredEvent>, LedgerError> {
    let mut statement = connection.prepare(
        "SELECT global_sequence, event_id, aggregate_id, event_type, actor,
                    timestamp_millis, payload, previous_hash, hash
             FROM events ORDER BY global_sequence",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RawEvent {
            sequence: row.get(0)?,
            event_id: row.get(1)?,
            aggregate_id: row.get(2)?,
            event_type: row.get(3)?,
            actor: row.get(4)?,
            timestamp_millis: row.get(5)?,
            payload: row.get(6)?,
            previous_hash: row.get(7)?,
            hash: row.get(8)?,
        })
    })?;

    let mut events = Vec::new();
    let mut expected_previous_hash = GENESIS_HASH;
    for (expected_sequence, row) in (1_u64..).zip(rows) {
        let raw = row?;
        let event = raw.into_stored()?;
        if event.sequence != expected_sequence {
            return Err(LedgerError::SequenceMismatch {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        if event.previous_hash != expected_previous_hash {
            return Err(LedgerError::PreviousHashMismatch {
                sequence: event.sequence,
            });
        }
        let input = EventInput::new(
            &event.event_id,
            &event.aggregate_id,
            &event.event_type,
            &event.actor,
            event.timestamp_millis,
            &event.payload,
        );
        if hash_event(event.sequence, &input, &event.previous_hash) != event.hash {
            return Err(LedgerError::EventHashMismatch {
                sequence: event.sequence,
            });
        }
        expected_previous_hash = event.hash;
        events.push(event);
    }
    Ok(events)
}

fn load_tail(connection: &Connection) -> Result<Option<StoredEvent>, LedgerError> {
    let raw = connection
        .query_row(
            "SELECT global_sequence, event_id, aggregate_id, event_type, actor,
                    timestamp_millis, payload, previous_hash, hash
             FROM events ORDER BY global_sequence DESC LIMIT 1",
            [],
            |row| {
                Ok(RawEvent {
                    sequence: row.get(0)?,
                    event_id: row.get(1)?,
                    aggregate_id: row.get(2)?,
                    event_type: row.get(3)?,
                    actor: row.get(4)?,
                    timestamp_millis: row.get(5)?,
                    payload: row.get(6)?,
                    previous_hash: row.get(7)?,
                    hash: row.get(8)?,
                })
            },
        )
        .optional()?;
    raw.map(RawEvent::into_stored).transpose()
}

struct RawEvent {
    sequence: u64,
    event_id: String,
    aggregate_id: String,
    event_type: String,
    actor: String,
    timestamp_millis: i64,
    payload: Vec<u8>,
    previous_hash: Vec<u8>,
    hash: Vec<u8>,
}

impl RawEvent {
    fn into_stored(self) -> Result<StoredEvent, LedgerError> {
        Ok(StoredEvent {
            sequence: self.sequence,
            event_id: self.event_id,
            aggregate_id: self.aggregate_id,
            event_type: self.event_type,
            actor: self.actor,
            timestamp_millis: self.timestamp_millis,
            payload: self.payload,
            previous_hash: hash_from_bytes(&self.previous_hash)?,
            hash: hash_from_bytes(&self.hash)?,
        })
    }
}

fn hash_from_bytes(bytes: &[u8]) -> Result<[u8; 32], LedgerError> {
    bytes
        .try_into()
        .map_err(|_| LedgerError::InvalidHashLength(bytes.len()))
}

/// Computes the canonical BLAKE3 event hash, shared by every [`crate::EventLedger`] backend.
///
/// Every backend must call this exact function so that identical inputs produce an
/// identical hash chain regardless of which backend stored them.
pub(crate) fn hash_event(sequence: u64, input: &EventInput, previous_hash: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"hephaestus-event-v1\0");
    hasher.update(&sequence.to_be_bytes());
    update_length_prefixed(&mut hasher, input.event_id.as_bytes());
    update_length_prefixed(&mut hasher, input.aggregate_id.as_bytes());
    update_length_prefixed(&mut hasher, input.event_type.as_bytes());
    update_length_prefixed(&mut hasher, input.actor.as_bytes());
    hasher.update(&input.timestamp_millis.to_be_bytes());
    update_length_prefixed(&mut hasher, &input.payload);
    hasher.update(previous_hash);
    *hasher.finalize().as_bytes()
}

fn update_length_prefixed(hasher: &mut Hasher, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).expect("usize always fits in u64");
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
}

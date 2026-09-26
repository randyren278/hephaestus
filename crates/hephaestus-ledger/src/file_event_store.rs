use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::event_store::{GENESIS_HASH, hash_event, validate_input};
use crate::{EventInput, EventLedger, LedgerError, StoredEvent};

/// Append-only JSONL event ledger producing the identical hash chain as
/// [`crate::EventStore`] for identical inputs.
///
/// Every field of a canonical event is hex-encoded before being placed in the JSON
/// line, so an arbitrary (possibly non-UTF-8) payload can never introduce a literal
/// newline or unescaped quote into the line framing.
///
/// # Crash safety
///
/// Each append serializes one event as a single JSON line, appends it with one
/// `write_all`, and calls `sync_all` before returning success. A crash between the
/// `write_all` and the `sync_all`, or a crash partway through the `write_all` itself,
/// can leave a **torn** final line: bytes flushed to the file that do not end in that
/// line's trailing `\n`. [`FileEventLedger::open`] detects this deterministically: a
/// file whose bytes do not end with `\n` has a torn tail, defined as everything after
/// its last complete `\n`. That tail is dropped silently — it was never acknowledged
/// to a caller as appended, so dropping it is not data loss — and the file is
/// truncated back to its last complete line so the next append starts from an
/// unambiguous tail. A complete line (one followed by `\n`) that fails to parse or
/// fails hash-chain verification is real corruption, not a torn write, and is reported
/// as an error exactly like the SQLite backend reports it.
///
/// This ledger assumes a single writer, like [`crate::EventStore`]. It notices a
/// concurrent external writer cheaply, by comparing the file's length against what
/// this handle last wrote or verified rather than re-reading and re-hashing the whole
/// file on every append; a same-length substitution between appends is caught only by
/// the next [`FileEventLedger::replay_verified`] call (or the next process restart),
/// not by the length check in [`FileEventLedger::append`].
pub struct FileEventLedger {
    path: PathBuf,
    verified_head: Option<StoredEvent>,
    event_ids: HashSet<String>,
    committed_len: u64,
}

impl FileEventLedger {
    /// Opens or creates a durable JSONL event ledger, repairing a torn trailing write
    /// left by a crash and verifying the complete chain before accepting new appends.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when the file cannot be read or repaired, or when its
    /// canonical chain fails verification (see [`FileEventLedger::replay_verified`]).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let raw = read_or_empty(&path)?;
        let (complete, torn) = split_complete_lines(&raw);
        let committed_len = u64::try_from(complete.len()).unwrap_or(u64::MAX);
        if !torn.is_empty() {
            repair_truncate(&path, committed_len)?;
        }

        let events = verify_lines(complete)?;
        let event_ids = events.iter().map(|event| event.event_id.clone()).collect();
        let verified_head = events.into_iter().next_back();
        Ok(Self {
            path,
            verified_head,
            event_ids,
            committed_len,
        })
    }

    /// Appends one event atomically with its canonical sequence and hash link.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::DuplicateEventId`] for an existing identifier,
    /// [`LedgerError::EmptyEventField`] for a blank required field,
    /// [`LedgerError::LedgerHeadChanged`] when the file's length no longer matches
    /// what this handle last wrote or verified, and propagates I/O failures without
    /// committing a partial append.
    ///
    /// # Panics
    ///
    /// Never in practice: [`JsonlRecord`] serializes a canonical, already-validated
    /// [`StoredEvent`] to plain JSON, which [`serde_json`] cannot fail to encode.
    pub fn append(&mut self, input: EventInput) -> Result<StoredEvent, LedgerError> {
        validate_input(&input)?;
        if self.event_ids.contains(&input.event_id) {
            return Err(LedgerError::DuplicateEventId(input.event_id));
        }
        let actual_len = fs::metadata(&self.path).map_or(0, |metadata| metadata.len());
        if actual_len != self.committed_len {
            return Err(LedgerError::LedgerHeadChanged);
        }

        let (sequence, previous_hash) = match &self.verified_head {
            Some(previous) => (previous.sequence + 1, previous.hash),
            None => (1, GENESIS_HASH),
        };
        let hash = hash_event(sequence, &input, &previous_hash);
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

        let mut line = serde_json::to_vec(&JsonlRecord::from_stored(&event))
            .expect("a canonical event always serializes to JSON");
        line.push(b'\n');
        append_and_sync(&self.path, &line)?;

        self.committed_len += u64::try_from(line.len()).unwrap_or(u64::MAX);
        self.event_ids.insert(event.event_id.clone());
        self.verified_head = Some(event.clone());
        Ok(event)
    }

    /// Reads canonical history in order and verifies every sequence and hash link.
    ///
    /// This re-reads the file from disk and re-verifies every line rather than
    /// trusting this handle's in-memory state, so tampering performed outside the
    /// running process is still caught.
    ///
    /// # Errors
    ///
    /// Returns a specific integrity error at the first modified, missing, duplicated,
    /// or incorrectly linked event.
    pub fn replay_verified(&self) -> Result<Vec<StoredEvent>, LedgerError> {
        let raw = read_or_empty(&self.path)?;
        let (complete, _torn) = split_complete_lines(&raw);
        verify_lines(complete)
    }
}

impl EventLedger for FileEventLedger {
    fn append(&mut self, input: EventInput) -> Result<StoredEvent, LedgerError> {
        FileEventLedger::append(self, input)
    }

    fn replay_verified(&self) -> Result<Vec<StoredEvent>, LedgerError> {
        FileEventLedger::replay_verified(self)
    }
}

#[derive(Serialize, Deserialize)]
struct JsonlRecord {
    sequence: u64,
    event_id: String,
    aggregate_id: String,
    event_type: String,
    actor: String,
    timestamp_millis: i64,
    payload_hex: String,
    previous_hash_hex: String,
    hash_hex: String,
}

impl JsonlRecord {
    fn from_stored(event: &StoredEvent) -> Self {
        Self {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_type: event.event_type.clone(),
            actor: event.actor.clone(),
            timestamp_millis: event.timestamp_millis,
            payload_hex: encode_hex(&event.payload),
            previous_hash_hex: encode_hex(&event.previous_hash),
            hash_hex: encode_hex(&event.hash),
        }
    }

    fn into_stored(self) -> Result<StoredEvent, LedgerError> {
        let payload = decode_hex(&self.payload_hex)?;
        let previous_hash = array_from_bytes(&decode_hex(&self.previous_hash_hex)?)?;
        let hash = array_from_bytes(&decode_hex(&self.hash_hex)?)?;
        Ok(StoredEvent {
            sequence: self.sequence,
            event_id: self.event_id,
            aggregate_id: self.aggregate_id,
            event_type: self.event_type,
            actor: self.actor,
            timestamp_millis: self.timestamp_millis,
            payload,
            previous_hash,
            hash,
        })
    }
}

fn array_from_bytes(bytes: &[u8]) -> Result<[u8; 32], LedgerError> {
    bytes
        .try_into()
        .map_err(|_| LedgerError::InvalidHashLength(bytes.len()))
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _: Result<(), std::fmt::Error> = write!(out, "{byte:02x}");
    }
    out
}

fn decode_hex(value: &str) -> Result<Vec<u8>, LedgerError> {
    if value.len() % 2 != 0 {
        return Err(LedgerError::MalformedRecord(format!(
            "odd-length hex field: {value}"
        )));
    }
    let digits: Vec<char> = value.chars().collect();
    let mut bytes = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks_exact(2) {
        let high = pair[0]
            .to_digit(16)
            .ok_or_else(|| LedgerError::MalformedRecord(format!("invalid hex digit in {value}")))?;
        let low = pair[1]
            .to_digit(16)
            .ok_or_else(|| LedgerError::MalformedRecord(format!("invalid hex digit in {value}")))?;
        bytes.push(u8::try_from((high << 4) | low).unwrap_or(0));
    }
    Ok(bytes)
}

/// Reads a file's full bytes, treating a missing file as empty rather than an error.
fn read_or_empty(path: &Path) -> Result<Vec<u8>, LedgerError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// Splits raw file bytes into (complete newline-terminated lines, a torn trailing
/// partial line). The torn half is empty whenever the file is empty or already ends
/// with `\n`.
fn split_complete_lines(raw: &[u8]) -> (&[u8], &[u8]) {
    if raw.is_empty() || raw.ends_with(b"\n") {
        return (raw, &[]);
    }
    match raw.iter().rposition(|&byte| byte == b'\n') {
        Some(index) => (&raw[..=index], &raw[index + 1..]),
        None => (&[], raw),
    }
}

/// Truncates the file at `path` down to `new_len` bytes and fsyncs the result, used to
/// repair a torn trailing write detected on open.
fn repair_truncate(path: &Path, new_len: u64) -> Result<(), LedgerError> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(new_len)?;
    file.sync_all()?;
    Ok(())
}

/// Appends `bytes` to the file at `path`, creating it if absent, and fsyncs before
/// returning so a caller-visible success is durable.
fn append_and_sync(path: &Path, bytes: &[u8]) -> Result<(), LedgerError> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Parses and verifies every complete JSONL line in `complete`, returning canonical
/// events in order or the first integrity error found.
fn verify_lines(complete: &[u8]) -> Result<Vec<StoredEvent>, LedgerError> {
    let mut events = Vec::new();
    let mut expected_previous_hash = GENESIS_HASH;
    let mut expected_sequence = 1_u64;
    for line in complete.split(|&byte| byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let record: JsonlRecord = serde_json::from_slice(line)
            .map_err(|error| LedgerError::MalformedRecord(error.to_string()))?;
        let event = record.into_stored()?;
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
        expected_sequence += 1;
        events.push(event);
    }
    Ok(events)
}

use std::fs::{self, OpenOptions};
use std::io::Write as _;

use hephaestus_ledger::{EventInput, FileEventLedger, LedgerError};
use tempfile::tempdir;

fn seed_two_events(log: &std::path::Path) {
    let mut ledger = FileEventLedger::open(log).expect("open ledger");
    for sequence in 1..=2u64 {
        ledger
            .append(EventInput::new(
                format!("event-{sequence}"),
                "genome:g0",
                "observed",
                "runtime",
                i64::try_from(sequence).expect("small sequence fits i64") * 1_000,
                format!(r#"{{"sequence":{sequence}}}"#).as_bytes(),
            ))
            .expect("append event");
    }
}

/// Rewrites the hex digit at `offset` within line `line_index` (0-based) of the
/// JSONL file to a different valid hex digit, leaving every other byte -- including
/// the line's trailing newline and its own JSON/UTF-8 validity -- intact. This
/// simulates external tampering of a fully-written, durable record's content (as
/// opposed to a torn write) without also corrupting the line's text encoding, which
/// would surface as a parse failure rather than the specific integrity error under
/// test.
fn flip_hex_digit_in_line(log: &std::path::Path, line_index: usize, offset: usize) {
    let raw = fs::read(log).expect("read ledger file");
    let mut line_start = 0usize;
    let mut current_line = 0usize;
    let mut line_end = raw.len();
    let mut index = 0usize;
    while index < raw.len() {
        if raw[index] == b'\n' {
            if current_line == line_index {
                line_end = index;
                break;
            }
            current_line += 1;
            line_start = index + 1;
        }
        index += 1;
    }
    assert!(
        line_start < line_end,
        "line {line_index} not found in {}",
        log.display()
    );
    let mut bytes = raw;
    let target = line_start + offset;
    assert!(target < line_end, "offset out of range for the target line");
    let original = bytes[target];
    assert!(
        original.is_ascii_hexdigit(),
        "byte at the target offset must be a hex digit, found {original:#x}"
    );
    bytes[target] = if original == b'0' { b'1' } else { b'0' };
    fs::write(log, bytes).expect("rewrite ledger file");
}

/// Deletes line `line_index` (0-based) entirely, closing the gap, while every other
/// line (including its own trailing newline) is preserved verbatim.
fn delete_line(log: &std::path::Path, line_index: usize) {
    let raw = fs::read(log).expect("read ledger file");
    let mut lines: Vec<&[u8]> = raw.split(|&byte| byte == b'\n').collect();
    // A trailing empty slice results from the file's final newline; drop it so the
    // rewritten file still ends in exactly one newline per real line.
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.remove(line_index);
    let mut rebuilt = Vec::new();
    for line in lines {
        rebuilt.extend_from_slice(line);
        rebuilt.push(b'\n');
    }
    fs::write(log, rebuilt).expect("rewrite ledger file with a line removed");
}

/// Physically swaps two lines, reordering canonical sequence numbers without
/// modifying either line's own bytes.
fn swap_lines(log: &std::path::Path, first: usize, second: usize) {
    let raw = fs::read(log).expect("read ledger file");
    let mut lines: Vec<&[u8]> = raw.split(|&byte| byte == b'\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.swap(first, second);
    let mut rebuilt = Vec::new();
    for line in lines {
        rebuilt.extend_from_slice(line);
        rebuilt.push(b'\n');
    }
    fs::write(log, rebuilt).expect("rewrite ledger file with lines swapped");
}

#[test]
fn file_event_history_survives_restart_and_replays_deterministically() {
    let directory = tempdir().expect("temporary directory");
    let log = directory.path().join("ledger.jsonl");
    let mut ledger = FileEventLedger::open(&log).expect("open ledger");

    let first = ledger
        .append(EventInput::new(
            "event-1",
            "genome:g0",
            "genome_created",
            "operator",
            1_000,
            br#"{"genome":"g0"}"#,
        ))
        .expect("append first event");
    let second = ledger
        .append(EventInput::new(
            "event-2",
            "genome:g0",
            "genome_validated",
            "arena",
            2_000,
            br#"{"genome":"g0"}"#,
        ))
        .expect("append second event");

    assert_eq!(first.sequence, 1);
    assert_eq!(first.previous_hash, [0; 32]);
    assert_eq!(second.sequence, 2);
    assert_eq!(second.previous_hash, first.hash);
    drop(ledger);

    let mut reopened = FileEventLedger::open(&log).expect("reopen ledger");
    let third = reopened
        .append(EventInput::new(
            "event-3",
            "genome:g0",
            "genome_promoted",
            "policy",
            3_000,
            br#"{"genome":"g0"}"#,
        ))
        .expect("append after restart");
    assert_eq!(third.sequence, 3);

    let first_replay = reopened.replay_verified().expect("verified replay");
    drop(reopened);

    let reopened = FileEventLedger::open(&log).expect("second reopen");
    let second_replay = reopened.replay_verified().expect("second verified replay");
    assert_eq!(second_replay, first_replay);
}

#[test]
fn file_replay_detects_payload_chain_sequence_and_reorder_tampering() {
    let directory = tempdir().expect("temporary directory");

    let payload_log = directory.path().join("payload.jsonl");
    seed_two_events(&payload_log);
    // Flip a byte inside line 1 (the second event)'s `payload_hex` value, located
    // dynamically so this does not depend on the JSON field order staying fixed.
    let raw = fs::read(&payload_log).expect("read payload ledger");
    let second_line_payload_offset = String::from_utf8(raw)
        .expect("ledger lines are valid utf8")
        .lines()
        .nth(1)
        .and_then(|line| line.find("payload_hex"))
        .expect("second line has a payload_hex field");
    flip_hex_digit_in_line(&payload_log, 1, second_line_payload_offset + 15);
    assert!(matches!(
        FileEventLedger::open(&payload_log),
        Err(LedgerError::EventHashMismatch { sequence: 2 })
    ));

    let live_log = directory.path().join("live-tamper.jsonl");
    seed_two_events(&live_log);
    let mut live_ledger = FileEventLedger::open(&live_log).expect("open verified ledger");
    let mut file = OpenOptions::new()
        .append(true)
        .open(&live_log)
        .expect("open ledger file for live tamper");
    file.write_all(b"garbage-not-json\n")
        .expect("inject live corruption");
    drop(file);
    assert!(matches!(
        live_ledger.append(EventInput::new(
            "event-3",
            "genome:g0",
            "observed",
            "runtime",
            3_000,
            b"{}"
        )),
        Err(LedgerError::LedgerHeadChanged)
    ));

    let chain_log = directory.path().join("chain.jsonl");
    seed_two_events(&chain_log);
    // `previous_hash_hex` is a fixed-width 64 hex character field; corrupt one of its
    // digits without touching anything else on the line.
    let raw = fs::read(&chain_log).expect("read chain ledger");
    let second_line_previous_hash_offset = String::from_utf8(raw.clone())
        .expect("ledger lines are valid utf8")
        .lines()
        .nth(1)
        .and_then(|line| line.find("previous_hash_hex"))
        .expect("second line has a previous_hash_hex field");
    flip_hex_digit_in_line(&chain_log, 1, second_line_previous_hash_offset + 21);
    assert!(matches!(
        FileEventLedger::open(&chain_log),
        Err(LedgerError::PreviousHashMismatch { sequence: 2 })
    ));

    let sequence_log = directory.path().join("sequence.jsonl");
    seed_two_events(&sequence_log);
    let raw = fs::read(&sequence_log).expect("read sequence ledger");
    let text = String::from_utf8(raw).expect("ledger lines are valid utf8");
    let rewritten = text.replacen("\"sequence\":2,", "\"sequence\":3,", 1);
    fs::write(&sequence_log, rewritten).expect("create a sequence gap");
    assert!(matches!(
        FileEventLedger::open(&sequence_log),
        Err(LedgerError::SequenceMismatch {
            expected: 2,
            actual: 3
        })
    ));

    let removed_log = directory.path().join("removed-middle.jsonl");
    let mut ledger = FileEventLedger::open(&removed_log).expect("open ledger");
    for sequence in 1..=3u64 {
        ledger
            .append(EventInput::new(
                format!("event-{sequence}"),
                "genome:g0",
                "observed",
                "runtime",
                i64::try_from(sequence).expect("small sequence fits i64") * 1_000,
                b"{}",
            ))
            .expect("append event");
    }
    drop(ledger);
    delete_line(&removed_log, 1);
    assert!(matches!(
        FileEventLedger::open(&removed_log),
        Err(LedgerError::SequenceMismatch {
            expected: 2,
            actual: 3
        })
    ));

    let reordered_log = directory.path().join("reordered.jsonl");
    seed_two_events(&reordered_log);
    swap_lines(&reordered_log, 0, 1);
    assert!(matches!(
        FileEventLedger::open(&reordered_log),
        Err(LedgerError::SequenceMismatch {
            expected: 1,
            actual: 2
        })
    ));
}

#[test]
fn file_duplicate_event_ids_are_rejected_without_advancing_history() {
    let directory = tempdir().expect("temporary directory");
    let log = directory.path().join("ledger.jsonl");
    let mut ledger = FileEventLedger::open(&log).expect("open ledger");
    let input = EventInput::new("event-1", "genome:g0", "created", "operator", 1_000, b"{}");

    ledger.append(input.clone()).expect("first append");
    assert!(matches!(
        ledger.append(input),
        Err(LedgerError::DuplicateEventId(id)) if id == "event-1"
    ));
    assert_eq!(ledger.replay_verified().expect("verified history").len(), 1);
    assert!(matches!(
        ledger.append(EventInput::new(
            "",
            "genome:g0",
            "created",
            "operator",
            2_000,
            b"{}"
        )),
        Err(LedgerError::EmptyEventField("event_id"))
    ));
    assert_eq!(
        ledger.replay_verified().expect("unchanged history").len(),
        1
    );
}

#[test]
fn file_ledger_drops_a_torn_trailing_write_and_keeps_appending() {
    let directory = tempdir().expect("temporary directory");
    let log = directory.path().join("ledger.jsonl");
    seed_two_events(&log);
    let complete_len = fs::metadata(&log).expect("stat ledger").len();

    // Simulate a crash mid-write: bytes reached disk but the line's trailing
    // newline never did.
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log)
        .expect("open ledger file to simulate a torn write");
    file.write_all(b"{\"sequence\":3,\"event_id\":\"event-3\",\"trunc")
        .expect("write a deliberately torn line");
    drop(file);
    let torn_len = fs::metadata(&log).expect("stat torn ledger").len();
    assert!(
        torn_len > complete_len,
        "the torn write must have landed on disk"
    );

    let mut reopened = FileEventLedger::open(&log).expect("open must silently repair a torn tail");
    assert_eq!(
        reopened
            .replay_verified()
            .expect("verified replay after repair")
            .len(),
        2,
        "the torn line must not be counted as a stored event"
    );
    let repaired_len = fs::metadata(&log).expect("stat repaired ledger").len();
    assert_eq!(
        repaired_len, complete_len,
        "the file must be truncated back to its last complete line"
    );

    let third = reopened
        .append(EventInput::new(
            "event-3",
            "genome:g0",
            "observed",
            "runtime",
            3_000,
            b"{}",
        ))
        .expect("append after repairing a torn tail");
    assert_eq!(third.sequence, 3);
    assert_eq!(
        third.previous_hash,
        reopened.replay_verified().unwrap()[1].hash
    );
}

#[test]
fn file_ledger_reports_a_complete_but_malformed_line_as_an_error_not_a_torn_write() {
    let directory = tempdir().expect("temporary directory");
    let log = directory.path().join("ledger.jsonl");
    seed_two_events(&log);

    // Append a fully newline-terminated but non-JSON line: this is corruption, not a
    // crash-torn write, and must be reported rather than silently dropped.
    let mut file = OpenOptions::new()
        .append(true)
        .open(&log)
        .expect("open ledger file");
    file.write_all(b"not-json-but-newline-terminated\n")
        .expect("write a malformed complete line");
    drop(file);

    assert!(matches!(
        FileEventLedger::open(&log),
        Err(LedgerError::MalformedRecord(_))
    ));
}

//! Markdown renderings of a debate: the final answer and the full transcript.

use std::fmt::Write as _;

use crate::debate::{Outcome, Vote};

/// Printed on every Senate output.
pub const DISCLAIMER: &str = "Simulated perspectives written by an AI model \"in the spirit of\" historical figures. Nothing here is any real person's words, views, or endorsement.";

/// The Senate's final output: the answer, agreement, amendments, dissent,
/// and the cost of producing it.
#[must_use]
pub fn render_answer(outcome: &Outcome, backend: &str, transcript: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# The Senate\n\n> {DISCLAIMER}\n");
    let _ = writeln!(out, "**Question:** {}\n", first_line(&outcome.question));
    let _ = writeln!(
        out,
        "**Senate:** size {}, {} senators: {}\n",
        outcome.size.name(),
        outcome.roster.len(),
        outcome
            .roster
            .iter()
            .map(|persona| persona.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let rounds = outcome.rounds.len();
    let cap = outcome.size.round_cap();
    let verdict = match outcome.ratified_in {
        Some(round) => format!("consensus, ratified in round {round} of {cap}"),
        None => format!(
            "no consensus after {rounds} of {cap} rounds; the final draft below is unratified"
        ),
    };
    let _ = writeln!(out, "**Outcome:** {verdict}\n");
    let _ = writeln!(out, "## Answer\n\n{}\n", outcome.answer.answer);
    if !outcome.answer.agreement.is_empty() {
        let _ = writeln!(out, "## Points of agreement\n");
        for point in &outcome.answer.agreement {
            let _ = writeln!(out, "- {point}");
        }
        out.push('\n');
    }
    let amendments_heading = if outcome.amendments_folded {
        "Amendments folded into the answer"
    } else {
        "Amendments proposed in the last vote"
    };
    let credits = [
        (amendments_heading, &outcome.amendments),
        ("Dissent", &outcome.dissent),
    ];
    for (heading, entries) in credits {
        if entries.is_empty() {
            continue;
        }
        let _ = writeln!(out, "## {heading}\n");
        for (seat, reason) in entries {
            let reason = if reason.is_empty() {
                "(no reason given)"
            } else {
                reason
            };
            let _ = writeln!(out, "- *{}*: {reason}", outcome.roster[*seat].label());
        }
        out.push('\n');
    }
    let _ = write!(
        out,
        "---\nBackend `{backend}`, {} model calls, {:.1}s wall time.",
        outcome.calls,
        outcome.wall.as_secs_f64()
    );
    if let Some(path) = transcript {
        let _ = write!(out, " Transcript: {path}");
    }
    out.push('\n');
    out
}

/// Every round's positions, votes, and drafts, for the record.
#[must_use]
pub fn render_transcript(outcome: &Outcome) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Senate transcript\n\n> {DISCLAIMER}\n");
    let _ = writeln!(out, "## Question\n\n{}\n", outcome.question);
    let source = if outcome.roster_from_clerk {
        "chosen by the clerk"
    } else {
        "completed from the seeded roster"
    };
    let _ = writeln!(out, "## Roster ({source})\n");
    for persona in &outcome.roster {
        let _ = writeln!(
            out,
            "- **{}** [{}], {}, {}: {}",
            persona.label(),
            persona.id,
            persona.domain,
            persona.origin,
            persona.lens
        );
    }
    out.push('\n');
    for round in &outcome.rounds {
        let _ = writeln!(out, "## Round {}\n", round.number);
        for (seat, persona) in outcome.roster.iter().enumerate() {
            let _ = write!(out, "### {}", persona.label());
            if let Some((vote, reason)) = round.votes.get(seat) {
                let _ = write!(out, " (vote: {vote})");
                if *vote != Vote::Absent && !reason.is_empty() {
                    let _ = write!(out, "\n\n*Reason:* {reason}");
                }
            }
            out.push_str("\n\n");
            if let Some(position) = &round.positions[seat] {
                let _ = writeln!(out, "{position}\n");
            } else {
                let error = round
                    .failures
                    .iter()
                    .find(|(failed, _)| *failed == seat)
                    .map_or("", |(_, error)| error.as_str());
                let _ = writeln!(out, "*No reply this round: {error}*\n");
            }
        }
        if let Some(draft) = &round.draft {
            let _ = writeln!(out, "### Clerk's draft\n\n{}\n", draft.answer);
        } else {
            let _ = writeln!(out, "### The previous draft was ratified.\n");
            if outcome.amendments_folded {
                let _ = writeln!(
                    out,
                    "### Clerk's draft with amendments folded in\n\n{}\n",
                    outcome.answer.answer
                );
            }
        }
    }
    out
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(160) {
        Some((end, _)) => format!("{}...", &line[..end]),
        None => line.to_owned(),
    }
}

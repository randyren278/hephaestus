//! The debate protocol.
//!
//! 1. The clerk picks a roster (validated, topped up from a seeded roster).
//! 2. Round 1: every senator writes an opening position, in parallel; the
//!    clerk drafts a synthesis.
//! 3. Round k >= 2: every senator sees the draft and the others' latest
//!    positions, votes AGREE/AMEND/DISSENT on the draft, and revises their
//!    position. If no one dissents and a majority agree, the draft is
//!    ratified and the debate stops. Otherwise the clerk redrafts.
//! 4. At the round cap the last draft stands unratified, and the final
//!    round's dissent is reported, credited to the senator who raised it.
//!
//! Everything recorded is a pure function of the model replies (and the
//! seed), so the protocol is tested end to end with a fake backend.

use std::{
    fmt::{self, Write as _},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    backend::Backend,
    persona::{Persona, parse_roster_reply, seeded_roster},
};

/// How many senators sit and how many rounds they may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Size {
    #[value(name = "S")]
    Small,
    #[value(name = "M")]
    Medium,
    #[value(name = "L")]
    Large,
    #[value(name = "XL")]
    ExtraLarge,
}

impl Size {
    #[must_use]
    pub fn senators(self) -> usize {
        match self {
            Self::Small => 3,
            Self::Medium => 5,
            Self::Large => 9,
            Self::ExtraLarge => 15,
        }
    }

    #[must_use]
    pub fn round_cap(self) -> usize {
        match self {
            Self::Small => 2,
            Self::Medium => 3,
            Self::Large => 4,
            Self::ExtraLarge => 5,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Small => "S",
            Self::Medium => "M",
            Self::Large => "L",
            Self::ExtraLarge => "XL",
        }
    }

    /// The most model calls a debate of this size can make: the roster, one
    /// reply per senator per round, and one clerk draft per round.
    #[must_use]
    pub fn max_calls(self) -> usize {
        1 + self.round_cap() * (self.senators() + 1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vote {
    Agree,
    Amend,
    Dissent,
    /// The reply carried no recognisable vote.
    Unclear,
    /// The senator's call failed this round.
    Absent,
}

impl fmt::Display for Vote {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Agree => "agree",
            Self::Amend => "amend",
            Self::Dissent => "dissent",
            Self::Unclear => "unclear",
            Self::Absent => "absent",
        })
    }
}

/// One senator's reply in a vote-and-revise round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deliberation {
    pub vote: Vote,
    pub reason: String,
    pub position: String,
}

/// Parses `VOTE:` / `REASON:` / `POSITION:` sections, leniently: labels are
/// case-insensitive and may carry markdown emphasis, and a reply with no
/// `POSITION:` keeps its whole text as the position.
#[must_use]
pub fn parse_deliberation(reply: &str) -> Deliberation {
    let vote_text = section(reply, "VOTE").unwrap_or_default();
    let vote_word = vote_text
        .split(|character: char| !character.is_alphabetic())
        .find(|word| !word.is_empty())
        .unwrap_or_default()
        .to_ascii_uppercase();
    let vote = match vote_word.as_str() {
        "AGREE" => Vote::Agree,
        "AMEND" => Vote::Amend,
        "DISSENT" => Vote::Dissent,
        _ => Vote::Unclear,
    };
    Deliberation {
        vote,
        reason: section(reply, "REASON").unwrap_or_default(),
        position: section(reply, "POSITION").unwrap_or_else(|| reply.trim().to_owned()),
    }
}

/// The clerk's synthesis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub answer: String,
    pub agreement: Vec<String>,
}

/// Parses `ANSWER:` / `AGREEMENT:` sections; a reply without them is all
/// answer and no listed agreement.
#[must_use]
pub fn parse_draft(reply: &str) -> Draft {
    let answer = section(reply, "ANSWER").unwrap_or_else(|| reply.trim().to_owned());
    let agreement = section(reply, "AGREEMENT")
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().trim_start_matches(['-', '*', '•']).trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    Draft { answer, agreement }
}

const LABELS: [&str; 5] = ["VOTE", "REASON", "POSITION", "ANSWER", "AGREEMENT"];

/// The label a line opens with (ignoring markdown `#`/`*` decoration), and
/// the text after its colon.
fn label_of(line: &str) -> Option<(&'static str, &str)> {
    let bare = line.trim().trim_start_matches(['#', '*', ' ']);
    LABELS.iter().find_map(|&label| {
        let head = bare.get(..label.len())?;
        if !head.eq_ignore_ascii_case(label) {
            return None;
        }
        let rest = bare[label.len()..].trim_start_matches('*');
        rest.strip_prefix(':')
            .map(|after| (label, after.trim_start_matches('*').trim()))
    })
}

/// The text under `label`, up to the next known label.
fn section(reply: &str, label: &str) -> Option<String> {
    let mut collected: Option<Vec<&str>> = None;
    for line in reply.lines() {
        match (label_of(line), collected.as_mut()) {
            (Some((found, rest)), None) if found == label => {
                collected = Some(if rest.is_empty() { vec![] } else { vec![rest] });
            }
            (Some(_), Some(_)) => break,
            (None, Some(lines)) => lines.push(line),
            _ => {}
        }
    }
    collected.map(|lines| lines.join("\n").trim().to_owned())
}

/// One round of the debate, indexed by seat (the roster order).
#[derive(Clone, Debug)]
pub struct Round {
    pub number: usize,
    /// Each senator's position this round; `None` when their call failed.
    pub positions: Vec<Option<String>>,
    /// Votes on the previous round's draft (empty in round 1).
    pub votes: Vec<(Vote, String)>,
    /// The clerk's draft after this round; `None` when this round ratified
    /// the previous draft instead.
    pub draft: Option<Draft>,
    /// Failed calls this round, by seat.
    pub failures: Vec<(usize, String)>,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub question: String,
    pub size: Size,
    pub roster: Vec<Persona>,
    /// True when the clerk's reply named the full roster; false when some or
    /// all seats came from the seeded fallback.
    pub roster_from_clerk: bool,
    pub rounds: Vec<Round>,
    pub answer: Draft,
    /// The round whose votes ratified `answer`; `None` at the round cap.
    pub ratified_in: Option<usize>,
    /// Amendments and dissent from the last vote, credited by seat.
    pub amendments: Vec<(usize, String)>,
    pub dissent: Vec<(usize, String)>,
    pub calls: usize,
    pub wall: Duration,
}

#[derive(Debug)]
pub enum SenateError {
    /// Every senator's opening call failed, so there is nothing to debate.
    NoOpenings(String),
    /// The clerk could not draft a synthesis.
    Clerk(String),
}

impl fmt::Display for SenateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOpenings(error) => {
                write!(formatter, "every senator's opening call failed: {error}")
            }
            Self::Clerk(error) => write!(formatter, "the clerk's draft call failed: {error}"),
        }
    }
}

impl std::error::Error for SenateError {}

/// A configured Senate, ready to take questions.
pub struct Senate<'backend> {
    pub backend: &'backend dyn Backend,
    pub personas: Vec<Persona>,
    pub size: Size,
    pub seed: u64,
    /// Most model calls in flight at once.
    pub jobs: usize,
    calls: AtomicUsize,
}

impl<'backend> Senate<'backend> {
    #[must_use]
    pub fn new(
        backend: &'backend dyn Backend,
        personas: Vec<Persona>,
        size: Size,
        seed: u64,
        jobs: usize,
    ) -> Self {
        Self {
            backend,
            personas,
            size,
            seed,
            jobs: jobs.max(1),
            calls: AtomicUsize::new(0),
        }
    }

    /// Runs a full debate on `question`.
    ///
    /// # Errors
    ///
    /// When every opening fails or a clerk draft fails. A single senator's
    /// failed call is recorded and the debate continues without them.
    pub fn run(&self, question: &str) -> Result<Outcome, SenateError> {
        let started = Instant::now();
        let (roster, roster_from_clerk) = self.choose_roster(question);
        let seats: Vec<&Persona> = roster.iter().map(|&index| &self.personas[index]).collect();

        let replies = self.parallel(seats.len(), |seat| {
            self.call(&opening_prompt(question, seats[seat]))
        });
        let mut positions: Vec<Option<String>> = Vec::with_capacity(seats.len());
        let mut failures = Vec::new();
        for (seat, reply) in replies.into_iter().enumerate() {
            match reply {
                Ok(text) => positions.push(Some(text)),
                Err(error) => {
                    failures.push((seat, error));
                    positions.push(None);
                }
            }
        }
        if positions.iter().all(Option::is_none) {
            let error = failures.first().map(|(_, error)| error.clone());
            return Err(SenateError::NoOpenings(error.unwrap_or_default()));
        }
        let mut latest = positions.clone();
        let mut draft = self.draft(question, &seats, &latest, &[], 1)?;
        let mut rounds = vec![Round {
            number: 1,
            positions,
            votes: Vec::new(),
            draft: Some(draft.clone()),
            failures,
        }];

        for number in 2..=self.size.round_cap() {
            let replies = self.parallel(seats.len(), |seat| {
                self.call(&deliberation_prompt(
                    question, &seats, seat, &latest, &draft, number,
                ))
            });
            let mut round = Round {
                number,
                positions: Vec::with_capacity(seats.len()),
                votes: Vec::with_capacity(seats.len()),
                draft: None,
                failures: Vec::new(),
            };
            for (seat, reply) in replies.into_iter().enumerate() {
                match reply {
                    Ok(text) => {
                        let parsed = parse_deliberation(&text);
                        latest[seat] = Some(parsed.position.clone());
                        round.positions.push(Some(parsed.position));
                        round.votes.push((parsed.vote, parsed.reason));
                    }
                    Err(error) => {
                        round.failures.push((seat, error));
                        round.positions.push(None);
                        round.votes.push((Vote::Absent, String::new()));
                    }
                }
            }
            if ratifies(&round.votes) {
                rounds.push(round);
                return Ok(self.outcome(
                    question,
                    &roster,
                    roster_from_clerk,
                    rounds,
                    draft,
                    Some(number),
                    started,
                ));
            }
            draft = self.draft(question, &seats, &latest, &round.votes, number)?;
            round.draft = Some(draft.clone());
            rounds.push(round);
            if number == self.size.round_cap() {
                return Ok(self.outcome(
                    question,
                    &roster,
                    roster_from_clerk,
                    rounds,
                    draft,
                    None,
                    started,
                ));
            }
        }
        unreachable!("every size has a round cap of at least two")
    }

    #[allow(clippy::too_many_arguments)]
    fn outcome(
        &self,
        question: &str,
        roster: &[usize],
        roster_from_clerk: bool,
        rounds: Vec<Round>,
        answer: Draft,
        ratified_in: Option<usize>,
        started: Instant,
    ) -> Outcome {
        let last_votes = rounds
            .last()
            .map_or(&[][..], |round| round.votes.as_slice());
        let (amendments, dissent) = credited(last_votes);
        Outcome {
            question: question.to_owned(),
            size: self.size,
            roster: roster
                .iter()
                .map(|&index| self.personas[index].clone())
                .collect(),
            roster_from_clerk,
            rounds,
            answer,
            ratified_in,
            amendments,
            dissent,
            calls: self.calls.load(Ordering::SeqCst),
            wall: started.elapsed(),
        }
    }

    fn choose_roster(&self, question: &str) -> (Vec<usize>, bool) {
        let size = self.size.senators().min(self.personas.len());
        let fallback = seeded_roster(&self.personas, size, self.seed);
        let Ok(reply) = self.call(&roster_prompt(question, &self.personas, size)) else {
            return (fallback, false);
        };
        let from_clerk = parse_roster_reply(&reply, &self.personas, size, &[]);
        let complete = from_clerk.len() == size;
        (
            parse_roster_reply(&reply, &self.personas, size, &fallback),
            complete,
        )
    }

    fn draft(
        &self,
        question: &str,
        seats: &[&Persona],
        positions: &[Option<String>],
        votes: &[(Vote, String)],
        round: usize,
    ) -> Result<Draft, SenateError> {
        self.call(&draft_prompt(question, seats, positions, votes, round))
            .map(|reply| parse_draft(&reply))
            .map_err(SenateError::Clerk)
    }

    fn call(&self, prompt: &str) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.backend.complete(prompt)
    }

    /// Runs `task` for every seat with at most `jobs` in flight, returning
    /// results in seat order regardless of completion order.
    fn parallel<T: Send>(&self, count: usize, task: impl Fn(usize) -> T + Sync) -> Vec<T> {
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<T>>> = Mutex::new((0..count).map(|_| None).collect());
        thread::scope(|scope| {
            for _ in 0..self.jobs.min(count) {
                scope.spawn(|| {
                    loop {
                        let seat = next.fetch_add(1, Ordering::SeqCst);
                        if seat >= count {
                            break;
                        }
                        let result = task(seat);
                        results.lock().expect("results lock")[seat] = Some(result);
                    }
                });
            }
        });
        results
            .into_inner()
            .expect("results lock")
            .into_iter()
            .map(|result| result.expect("every seat ran"))
            .collect()
    }
}

/// A draft is ratified when no one dissents and a strict majority of seated
/// senators agree. Unclear and absent votes count against a majority.
fn ratifies(votes: &[(Vote, String)]) -> bool {
    let agree = votes
        .iter()
        .filter(|(vote, _)| *vote == Vote::Agree)
        .count();
    let dissent = votes.iter().any(|(vote, _)| *vote == Vote::Dissent);
    !dissent && agree * 2 > votes.len()
}

type Credits = Vec<(usize, String)>;

fn credited(votes: &[(Vote, String)]) -> (Credits, Credits) {
    let pick = |wanted: Vote| {
        votes
            .iter()
            .enumerate()
            .filter(|(_, (vote, _))| *vote == wanted)
            .map(|(seat, (_, reason))| (seat, reason.clone()))
            .collect()
    };
    (pick(Vote::Amend), pick(Vote::Dissent))
}

const PERSONA_RULE: &str = "You are a simulated perspective, not the real person: never claim to be them, never attribute quotes to them, and argue from their outlook in your own words.";

fn persona_line(persona: &Persona) -> String {
    format!(
        "You argue {} ({}, {}): {}.",
        persona.label(),
        persona.domain,
        persona.origin,
        persona.lens
    )
}

fn roster_prompt(question: &str, personas: &[Persona], size: usize) -> String {
    let mut prompt = format!(
        "SENATE-TASK: roster\nYou are the clerk of a debating senate. Choose exactly {size} senators whose perspectives would give the most useful and diverse debate on the question below. Spread the choice across domains, eras, and regions; include at least one perspective likely to disagree with the rest.\nReply with the chosen ids only, one per line, from this list:\n"
    );
    for persona in personas {
        let _ = writeln!(
            prompt,
            "{} = {} ({}): {}",
            persona.id, persona.name, persona.domain, persona.lens
        );
    }
    let _ = writeln!(prompt, "\nQUESTION:\n{question}");
    prompt
}

fn opening_prompt(question: &str, persona: &Persona) -> String {
    format!(
        "SENATE-TASK: opening\nSENATOR: {id}\nYou are a senator in a structured debate. {line}\n{PERSONA_RULE}\n\nQUESTION:\n{question}\n\nGive your opening position in at most 250 words: your answer, and what your perspective sees that others may miss. Plain text, no preamble.\n",
        id = persona.id,
        line = persona_line(persona),
    )
}

fn positions_block(
    seats: &[&Persona],
    positions: &[Option<String>],
    skip: Option<usize>,
) -> String {
    let mut block = String::new();
    for (seat, persona) in seats.iter().enumerate() {
        if Some(seat) == skip {
            continue;
        }
        let text = positions[seat]
            .as_deref()
            .unwrap_or("(no position on record)");
        let _ = write!(
            block,
            "### {} [{}]\n{text}\n\n",
            persona.label(),
            persona.id
        );
    }
    block
}

fn deliberation_prompt(
    question: &str,
    seats: &[&Persona],
    seat: usize,
    latest: &[Option<String>],
    draft: &Draft,
    round: usize,
) -> String {
    let persona = seats[seat];
    format!(
        "SENATE-TASK: deliberate\nSENATOR: {id}\nROUND: {round}\nYou are a senator in a structured debate. {line}\n{PERSONA_RULE}\n\nQUESTION:\n{question}\n\nYOUR LATEST POSITION:\n{own}\n\nOTHER SENATORS' LATEST POSITIONS:\n{others}CURRENT DRAFT SYNTHESIS:\n{answer}\n\nVote on the draft. AGREE if you would sign it as is, AMEND if you would sign it with a specific change, DISSENT if you cannot sign it. Reply exactly in this format:\nVOTE: AGREE or AMEND or DISSENT\nREASON: <one sentence: the change you want, or why you dissent>\nPOSITION:\n<your revised position, at most 200 words>\n",
        id = persona.id,
        line = persona_line(persona),
        own = latest[seat].as_deref().unwrap_or("(none yet)"),
        others = positions_block(seats, latest, Some(seat)),
        answer = draft.answer,
    )
}

fn draft_prompt(
    question: &str,
    seats: &[&Persona],
    positions: &[Option<String>],
    votes: &[(Vote, String)],
    round: usize,
) -> String {
    let mut vote_block = String::new();
    if !votes.is_empty() {
        vote_block.push_str("VOTES ON THE PREVIOUS DRAFT:\n");
        for (persona, (vote, reason)) in seats.iter().zip(votes) {
            let _ = writeln!(vote_block, "- {}: {vote}. {reason}", persona.label());
        }
        vote_block.push('\n');
    }
    format!(
        "SENATE-TASK: draft\nROUND: {round}\nYou are the neutral clerk of a debating senate. Synthesize the strongest single answer to the question from the senators' positions: keep what survives scrutiny, resolve conflicts on the merits, and fold in requested amendments that improve it. If the question asks for a document or other artifact, the answer is that complete deliverable.\n\nQUESTION:\n{question}\n\nSENATORS' POSITIONS:\n{positions}{vote_block}Reply exactly in this format:\nANSWER:\n<the synthesized answer, complete and self-contained>\nAGREEMENT:\n- <one point every or nearly every senator accepts>\n- <...>\n",
        positions = positions_block(seats, positions, None),
    )
}

#[cfg(test)]
mod tests {
    use super::{Size, Vote, parse_deliberation, parse_draft, ratifies};

    #[test]
    fn sizes_match_the_approved_table() {
        let table: Vec<(usize, usize)> = [Size::Small, Size::Medium, Size::Large, Size::ExtraLarge]
            .iter()
            .map(|size| (size.senators(), size.round_cap()))
            .collect();
        assert_eq!(table, [(3, 2), (5, 3), (9, 4), (15, 5)]);
        assert_eq!(Size::ExtraLarge.max_calls(), 81);
    }

    #[test]
    fn deliberation_parses_labelled_sections_leniently() {
        let parsed = parse_deliberation(
            "**VOTE:** Dissent.\n**Reason:** it ignores cost\nPOSITION:\nline one\nline two",
        );
        assert_eq!(parsed.vote, Vote::Dissent);
        assert_eq!(parsed.reason, "it ignores cost");
        assert_eq!(parsed.position, "line one\nline two");
    }

    #[test]
    fn deliberation_without_labels_is_an_unclear_vote_keeping_the_text() {
        let parsed = parse_deliberation("I think we should ship.");
        assert_eq!(parsed.vote, Vote::Unclear);
        assert_eq!(parsed.position, "I think we should ship.");
    }

    #[test]
    fn draft_parses_answer_and_agreement_bullets() {
        let draft =
            parse_draft("ANSWER:\nDo X.\n\nThen Y.\nAGREEMENT:\n- X is cheap\n* Y is safe\n");
        assert_eq!(draft.answer, "Do X.\n\nThen Y.");
        assert_eq!(draft.agreement, ["X is cheap", "Y is safe"]);
        assert_eq!(parse_draft("just text").answer, "just text");
    }

    #[test]
    fn ratification_needs_a_majority_and_no_dissent() {
        let votes = |list: &[Vote]| -> Vec<(Vote, String)> {
            list.iter().map(|&vote| (vote, String::new())).collect()
        };
        assert!(ratifies(&votes(&[Vote::Agree, Vote::Agree, Vote::Amend])));
        assert!(!ratifies(&votes(&[Vote::Agree, Vote::Amend, Vote::Amend])));
        assert!(!ratifies(&votes(&[
            Vote::Agree,
            Vote::Agree,
            Vote::Dissent
        ])));
        assert!(!ratifies(&votes(&[Vote::Agree, Vote::Absent])));
    }
}

//! The Senate: a multi-perspective debate over a subscription model CLI.
//!
//! A question goes to a roster of senators, each a simulated perspective
//! "in the spirit of" a historical figure. They debate in rounds (openings,
//! then vote-and-revise rounds on a clerk's draft synthesis) until consensus
//! or the size's round cap, and the Senate emits one synthesized answer with
//! its points of agreement and credited dissent.
//!
//! Every model call goes through [`Backend`]; the debate state is a pure
//! function of the replies, so tests drive the whole protocol with fake
//! executables and never touch a real `claude` or `codex`.

mod backend;
mod debate;
mod persona;
mod report;

pub use backend::{Backend, BackendKind, CliBackend, find_on_path};
pub use debate::{
    Deliberation, Draft, Outcome, Round, Senate, SenateError, Size, Vote, parse_deliberation,
    parse_draft,
};
pub use persona::{Persona, default_personas, parse_roster_reply, seeded_roster};
pub use report::{DISCLAIMER, render_answer, render_transcript};

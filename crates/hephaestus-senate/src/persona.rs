//! The curated persona list and roster selection.

use serde::Deserialize;

/// One simulated perspective. Senators argue "in the spirit of" `name`;
/// nothing they say is the real person's words or endorsement.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub origin: String,
    pub lens: String,
}

impl Persona {
    /// The label used everywhere a senator is credited.
    #[must_use]
    pub fn label(&self) -> String {
        format!("in the spirit of {}", self.name)
    }
}

/// The persona list checked into the repository and compiled into the
/// binary, so a Senate-only install needs no data files beside it.
///
/// # Panics
///
/// Only if the embedded `data/personas.json` is malformed, which the crate's
/// own tests rule out.
#[must_use]
pub fn default_personas() -> Vec<Persona> {
    serde_json::from_str(include_str!("../data/personas.json"))
        .expect("embedded personas.json is valid")
}

/// A deterministic roster of `size` personas for `seed`: domains are taken
/// round-robin (so every roster spans as many domains as it can) and each
/// domain's personas are shuffled by the seed.
#[must_use]
pub fn seeded_roster(personas: &[Persona], size: usize, seed: u64) -> Vec<usize> {
    let mut domains: Vec<&str> = Vec::new();
    for persona in personas {
        if !domains.contains(&persona.domain.as_str()) {
            domains.push(&persona.domain);
        }
    }
    let mut rng = SplitMix64(seed);
    shuffle(&mut domains, &mut rng);
    let mut queues: Vec<Vec<usize>> = domains
        .iter()
        .map(|domain| {
            let mut members: Vec<usize> = (0..personas.len())
                .filter(|&index| personas[index].domain == *domain)
                .collect();
            shuffle(&mut members, &mut rng);
            members.reverse();
            members
        })
        .collect();
    let mut roster = Vec::with_capacity(size);
    while roster.len() < size.min(personas.len()) {
        for queue in &mut queues {
            if roster.len() == size {
                break;
            }
            if let Some(index) = queue.pop() {
                roster.push(index);
            }
        }
    }
    roster
}

/// Reads the clerk's roster reply: every known persona id, in order of first
/// appearance, capped at `size`. Unknown words are ignored; a short or empty
/// reply is topped up from `fallback` (the seeded roster) so the Senate
/// always seats exactly `size` senators.
#[must_use]
pub fn parse_roster_reply(
    reply: &str,
    personas: &[Persona],
    size: usize,
    fallback: &[usize],
) -> Vec<usize> {
    let mut roster = Vec::with_capacity(size);
    for token in reply.split(|character: char| !(character.is_alphanumeric() || character == '_')) {
        if roster.len() == size {
            break;
        }
        if let Some(index) = personas
            .iter()
            .position(|persona| persona.id.eq_ignore_ascii_case(token))
            && !roster.contains(&index)
        {
            roster.push(index);
        }
    }
    for &index in fallback {
        if roster.len() == size {
            break;
        }
        if !roster.contains(&index) {
            roster.push(index);
        }
    }
    roster
}

/// A tiny seeded generator so persona selection is reproducible without a
/// dependency.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

fn shuffle<T>(items: &mut [T], rng: &mut SplitMix64) {
    for upper in (1..items.len()).rev() {
        let bound = u64::try_from(upper + 1).expect("slice length fits in u64");
        let pick = usize::try_from(rng.next() % bound).expect("index below a usize bound");
        items.swap(upper, pick);
    }
}

#[cfg(test)]
mod tests {
    use super::{default_personas, parse_roster_reply, seeded_roster};

    #[test]
    fn embedded_personas_have_unique_ids_and_every_field() {
        let personas = default_personas();
        assert!(personas.len() >= 15, "XL needs at least 15 personas");
        for (index, persona) in personas.iter().enumerate() {
            assert!(!persona.name.is_empty() && !persona.lens.is_empty());
            assert!(
                persona
                    .id
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character == '_'),
                "persona ids are lowercase snake_case: {}",
                persona.id
            );
            assert!(
                personas[..index].iter().all(|other| other.id != persona.id),
                "duplicate persona id {}",
                persona.id
            );
        }
    }

    #[test]
    fn seeded_roster_is_reproducible_distinct_and_spans_domains() {
        let personas = default_personas();
        let first = seeded_roster(&personas, 5, 7);
        assert_eq!(first, seeded_roster(&personas, 5, 7));
        assert_ne!(first, seeded_roster(&personas, 5, 8));
        let mut sorted = first.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 5);
        let mut domains: Vec<&str> = first
            .iter()
            .map(|&index| personas[index].domain.as_str())
            .collect();
        domains.sort_unstable();
        domains.dedup();
        assert_eq!(domains.len(), 5, "five senators come from five domains");
    }

    #[test]
    fn seeded_roster_never_exceeds_the_persona_list() {
        let personas = default_personas();
        assert_eq!(
            seeded_roster(&personas, 1_000, 1).len(),
            personas.len(),
            "a roster larger than the list seats everyone once"
        );
    }

    #[test]
    fn roster_reply_keeps_known_ids_in_order_and_tops_up_from_the_fallback() {
        let personas = default_personas();
        let turing = personas.iter().position(|p| p.id == "turing").unwrap();
        let laozi = personas.iter().position(|p| p.id == "laozi").unwrap();
        let fallback = seeded_roster(&personas, 3, 1);
        let roster = parse_roster_reply(
            "- Turing\n- nobody_real\n- laozi, turing again",
            &personas,
            3,
            &fallback,
        );
        assert_eq!(roster.len(), 3);
        assert_eq!(&roster[..2], &[turing, laozi]);
        assert!(fallback.contains(&roster[2]));
        assert_eq!(parse_roster_reply("", &personas, 3, &fallback), fallback);
    }
}

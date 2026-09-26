//! Deterministic, versioned catalog of one-step `agent.prompt` reference
//! mutations (roadmap items 8, 10, 13).
//!
//! The reference runtime supports exactly [`ReferenceInstruction`]'s 16
//! operations (`identity`/`ascii_uppercase` plus the seven Gauntlet bad/fix
//! pairs from `crates/hephaestus-runtime/src/reference_instruction.rs`). This
//! module groups them into eight disjoint two-member **families** — one
//! "casing" family (`identity`/`ascii_uppercase`) and one family per named
//! Gauntlet mode (`context_loss`, `premature_completion`, `schema_drift`,
//! `bad_routing`, `duplicate_subagents`, `poisoned_memory`,
//! `hallucinated_verification`) — and classifies every ordered pair of
//! distinct operations into a deterministic [`EdgeKind`]:
//!
//! - **`Flip`**: either direction within the casing family. Casing has no
//!   "bad" or "fix" side, so both directions are a flip.
//! - **`Fix`**: within a Gauntlet family, from its "bad" operation to its
//!   paired "fix" operation.
//! - **`Regress`**: within a Gauntlet family, from its "fix" operation back
//!   to its paired "bad" operation.
//! - **`CrossFamily`**: any pair drawn from two different families.
//!
//! Every ordered pair `(a, b)` with `a != b` among the 16 operations is a
//! *representable* edge (`edge_kind` always returns `Some` for two distinct
//! known operation names); the catalog only classifies which **kind** of
//! edge it is, it never rejects one. Authorization (whether Forge may
//! propose a mutation at all) is a separate, World-level concern handled by
//! the control plane's mutation-scope check, not by this module.
//!
//! This table is deterministic and versioned ([`CATALOG_VERSION`]): adding a
//! new operation or family in the future must bump the version so a replayed
//! proposal's recorded `catalog_version` unambiguously names the table it was
//! classified under.

/// Current catalog version. Bump this if the family/kind table changes.
pub const CATALOG_VERSION: u16 = 1;

/// The exact 16 reference-runtime operation names this catalog knows about,
/// in a fixed, deterministic order (casing family first, then one Gauntlet
/// family at a time in `examples/gauntlet/README.md`'s table order).
pub const ALL_OPERATIONS: [&str; 16] = [
    "identity",
    "ascii_uppercase",
    "context_loss_naive",
    "context_loss_aware",
    "premature_completion",
    "verified_completion",
    "schema_drift_brittle",
    "schema_drift_adaptive",
    "bad_routing_cheapest",
    "capability_aware_routing",
    "duplicate_subagents_wasteful",
    "deduplicated_subagents",
    "poisoned_memory_trusting",
    "provenance_checked_memory",
    "hallucinated_verification_trusting",
    "ground_truth_verification",
];

/// One two-member family: either the casing pair, or one Gauntlet mode's
/// `(bad, fix)` pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Family {
    name: &'static str,
    /// `None` for the casing family (no bad/fix concept, just a flip).
    /// `Some((bad, fix))` for a Gauntlet family.
    members: FamilyMembers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FamilyMembers {
    Casing(&'static str, &'static str),
    Gauntlet {
        bad: &'static str,
        fix: &'static str,
    },
}

const FAMILIES: [Family; 8] = [
    Family {
        name: "casing",
        members: FamilyMembers::Casing("identity", "ascii_uppercase"),
    },
    Family {
        name: "context_loss",
        members: FamilyMembers::Gauntlet {
            bad: "context_loss_naive",
            fix: "context_loss_aware",
        },
    },
    Family {
        name: "premature_completion",
        members: FamilyMembers::Gauntlet {
            bad: "premature_completion",
            fix: "verified_completion",
        },
    },
    Family {
        name: "schema_drift",
        members: FamilyMembers::Gauntlet {
            bad: "schema_drift_brittle",
            fix: "schema_drift_adaptive",
        },
    },
    Family {
        name: "bad_routing",
        members: FamilyMembers::Gauntlet {
            bad: "bad_routing_cheapest",
            fix: "capability_aware_routing",
        },
    },
    Family {
        name: "duplicate_subagents",
        members: FamilyMembers::Gauntlet {
            bad: "duplicate_subagents_wasteful",
            fix: "deduplicated_subagents",
        },
    },
    Family {
        name: "poisoned_memory",
        members: FamilyMembers::Gauntlet {
            bad: "poisoned_memory_trusting",
            fix: "provenance_checked_memory",
        },
    },
    Family {
        name: "hallucinated_verification",
        members: FamilyMembers::Gauntlet {
            bad: "hallucinated_verification_trusting",
            fix: "ground_truth_verification",
        },
    },
];

/// The kind of one-step mutation edge `(before, after)` represents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeKind {
    /// Within a Gauntlet family, from its "bad" operation to its "fix".
    Fix,
    /// Within a Gauntlet family, from its "fix" operation back to its "bad".
    Regress,
    /// Either direction within the casing family.
    Flip,
    /// The two operations belong to different families.
    CrossFamily,
}

impl EdgeKind {
    /// Stable, lowercase `snake_case` name, for logging or receipt fields.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fix => "fix",
            Self::Regress => "regress",
            Self::Flip => "flip",
            Self::CrossFamily => "cross_family",
        }
    }
}

fn family_of(operation: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|family| match family.members {
        FamilyMembers::Casing(a, b) => operation == a || operation == b,
        FamilyMembers::Gauntlet { bad, fix } => operation == bad || operation == fix,
    })
}

/// Whether `operation` is one of the 16 catalog operations.
#[must_use]
pub fn is_known_operation(operation: &str) -> bool {
    ALL_OPERATIONS.contains(&operation)
}

/// Classifies the ordered pair `(before, after)`. Returns `None` only when
/// `before == after` or either name is not one of the 16 catalog operations;
/// every other ordered pair of distinct known operations is representable
/// and returns `Some`.
#[must_use]
pub fn edge_kind(before: &str, after: &str) -> Option<EdgeKind> {
    if before == after || !is_known_operation(before) || !is_known_operation(after) {
        return None;
    }
    let before_family = family_of(before)?;
    let after_family = family_of(after)?;
    if before_family.name != after_family.name {
        return Some(EdgeKind::CrossFamily);
    }
    match before_family.members {
        FamilyMembers::Casing(_, _) => Some(EdgeKind::Flip),
        FamilyMembers::Gauntlet { bad, fix: _ } => {
            if before == bad {
                Some(EdgeKind::Fix)
            } else {
                Some(EdgeKind::Regress)
            }
        }
    }
}

/// Whether `(before, after)` is a representable catalog edge at all (any
/// kind), i.e. two distinct known operations.
#[must_use]
pub fn is_catalog_edge(before: &str, after: &str) -> bool {
    edge_kind(before, after).is_some()
}

/// If `operation` is a Gauntlet family's "bad" member, returns that family's
/// "fix" operation. Returns `None` for the casing family and for a Gauntlet
/// family's "fix" member (there is no "fix" of a fix) and for an unknown
/// operation.
#[must_use]
pub fn family_fix_for(operation: &str) -> Option<&'static str> {
    let family = family_of(operation)?;
    match family.members {
        FamilyMembers::Gauntlet { bad, fix } if operation == bad => Some(fix),
        _ => None,
    }
}

/// If `operation` is one of the two casing operations, returns the other
/// one. Returns `None` for any Gauntlet operation or an unknown operation.
#[must_use]
pub fn casing_flip(operation: &str) -> Option<&'static str> {
    match family_of(operation)?.members {
        FamilyMembers::Casing(a, b) if operation == a => Some(b),
        FamilyMembers::Casing(a, b) if operation == b => Some(a),
        _ => None,
    }
}

/// The stable family name `operation` belongs to (`"casing"` or one of the
/// seven Gauntlet mode names above), or `None` if unknown.
#[must_use]
pub fn family_name(operation: &str) -> Option<&'static str> {
    family_of(operation).map(|family| family.name)
}

#[cfg(test)]
mod tests {
    use super::{
        ALL_OPERATIONS, CATALOG_VERSION, EdgeKind, casing_flip, edge_kind, family_fix_for,
        family_name, is_catalog_edge,
    };

    #[test]
    fn catalog_has_exactly_sixteen_distinct_operations_in_eight_two_member_families() {
        assert_eq!(CATALOG_VERSION, 1);
        assert_eq!(ALL_OPERATIONS.len(), 16);
        let mut seen = std::collections::BTreeSet::new();
        for operation in ALL_OPERATIONS {
            assert!(seen.insert(operation), "duplicate operation {operation}");
            assert!(family_name(operation).is_some());
        }
        let families: std::collections::BTreeSet<_> = ALL_OPERATIONS
            .iter()
            .map(|op| family_name(op).unwrap())
            .collect();
        assert_eq!(families.len(), 8);
    }

    #[test]
    fn every_ordered_pair_of_distinct_known_operations_is_representable() {
        for a in ALL_OPERATIONS {
            for b in ALL_OPERATIONS {
                if a == b {
                    assert!(edge_kind(a, b).is_none(), "{a} -> {b} should not self-edge");
                    continue;
                }
                assert!(is_catalog_edge(a, b), "{a} -> {b} should be representable");
            }
        }
    }

    #[test]
    fn unknown_operations_are_never_representable() {
        assert!(edge_kind("identity", "not-a-real-operation").is_none());
        assert!(edge_kind("not-a-real-operation", "identity").is_none());
        assert!(!is_catalog_edge("bogus", "also-bogus"));
    }

    #[test]
    fn casing_pair_is_a_flip_in_both_directions() {
        assert_eq!(
            edge_kind("identity", "ascii_uppercase"),
            Some(EdgeKind::Flip)
        );
        assert_eq!(
            edge_kind("ascii_uppercase", "identity"),
            Some(EdgeKind::Flip)
        );
        assert_eq!(casing_flip("identity"), Some("ascii_uppercase"));
        assert_eq!(casing_flip("ascii_uppercase"), Some("identity"));
        assert_eq!(casing_flip("context_loss_naive"), None);
        assert_eq!(family_fix_for("identity"), None);
        assert_eq!(family_fix_for("ascii_uppercase"), None);
    }

    #[test]
    fn gauntlet_pairs_are_fix_forward_and_regress_backward() {
        let pairs = [
            ("context_loss_naive", "context_loss_aware"),
            ("premature_completion", "verified_completion"),
            ("schema_drift_brittle", "schema_drift_adaptive"),
            ("bad_routing_cheapest", "capability_aware_routing"),
            ("duplicate_subagents_wasteful", "deduplicated_subagents"),
            ("poisoned_memory_trusting", "provenance_checked_memory"),
            (
                "hallucinated_verification_trusting",
                "ground_truth_verification",
            ),
        ];
        for (bad, fix) in pairs {
            assert_eq!(edge_kind(bad, fix), Some(EdgeKind::Fix), "{bad} -> {fix}");
            assert_eq!(
                edge_kind(fix, bad),
                Some(EdgeKind::Regress),
                "{fix} -> {bad}"
            );
            assert_eq!(family_fix_for(bad), Some(fix));
            assert_eq!(family_fix_for(fix), None);
            assert_eq!(casing_flip(bad), None);
            assert_eq!(casing_flip(fix), None);
        }
    }

    #[test]
    fn cross_family_pairs_are_classified_cross_family() {
        assert_eq!(
            edge_kind("identity", "context_loss_naive"),
            Some(EdgeKind::CrossFamily)
        );
        assert_eq!(
            edge_kind("context_loss_aware", "premature_completion"),
            Some(EdgeKind::CrossFamily)
        );
        assert_eq!(
            edge_kind("schema_drift_brittle", "ascii_uppercase"),
            Some(EdgeKind::CrossFamily)
        );
    }

    #[test]
    fn edge_kind_as_str_is_stable() {
        assert_eq!(EdgeKind::Fix.as_str(), "fix");
        assert_eq!(EdgeKind::Regress.as_str(), "regress");
        assert_eq!(EdgeKind::Flip.as_str(), "flip");
        assert_eq!(EdgeKind::CrossFamily.as_str(), "cross_family");
    }
}

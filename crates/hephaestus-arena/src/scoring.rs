//! Shared, bounded output comparison for the evaluator and failure analysis.

use std::{collections::BTreeMap, fmt};

use hephaestus_genome::OutputScoring;
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::{ArenaError, MAX_TASK_TEXT_BYTES};

const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_EXPONENT: i64 = 1_000_000;

/// Compares task outputs using the immutable World policy. Invalid or
/// out-of-budget JSON never matches, even when both strings are identical.
pub(crate) fn outputs_match(policy: OutputScoring, actual: &str, expected: &str) -> bool {
    match policy {
        OutputScoring::Exact => actual == expected,
        OutputScoring::Trimmed => actual.trim_ascii() == expected.trim_ascii(),
        OutputScoring::JsonCanonical => match (parse_json(actual), parse_json(expected)) {
            (Ok(actual), Ok(expected)) => actual == expected,
            _ => false,
        },
    }
}

pub(crate) fn validate_expected(policy: OutputScoring, expected: &str) -> Result<(), ArenaError> {
    if policy == OutputScoring::JsonCanonical && parse_json(expected).is_err() {
        return Err(ArenaError::EvaluatorProtocol(
            "expected output is not supported strict JSON",
        ));
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum JsonValue {
    Null,
    Bool(bool),
    String(String),
    Number {
        negative: bool,
        digits: String,
        scale: i64,
    },
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

// RawValue keeps numeric lexemes intact: converting through f64 would
// incorrectly equate large integers or rounded decimal outputs.
fn parse_json(text: &str) -> Result<JsonValue, ()> {
    if text.len() > MAX_TASK_TEXT_BYTES {
        return Err(());
    }
    let raw: Box<RawValue> = serde_json::from_str(text).map_err(|_| ())?;
    parse_value(raw.get(), 0)
}

fn parse_value(text: &str, depth: usize) -> Result<JsonValue, ()> {
    if depth > MAX_JSON_DEPTH {
        return Err(());
    }
    match text.as_bytes().first().copied().ok_or(())? {
        b'n' => Ok(JsonValue::Null),
        b't' | b'f' => serde_json::from_str(text)
            .map(JsonValue::Bool)
            .map_err(|_| ()),
        b'"' => serde_json::from_str(text)
            .map(JsonValue::String)
            .map_err(|_| ()),
        b'[' => {
            let values: Vec<Box<RawValue>> = serde_json::from_str(text).map_err(|_| ())?;
            values
                .into_iter()
                .map(|value| parse_value(value.get(), depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(JsonValue::Array)
        }
        b'{' => {
            let object: StrictObject = serde_json::from_str(text).map_err(|_| ())?;
            object
                .0
                .into_iter()
                .map(|(key, value)| Ok((key, parse_value(value.get(), depth + 1)?)))
                .collect::<Result<BTreeMap<_, _>, ()>>()
                .map(JsonValue::Object)
        }
        _ => normalize_number(text),
    }
}

struct StrictObject(BTreeMap<String, Box<RawValue>>);

impl<'de> Deserialize<'de> for StrictObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = StrictObject;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON object key"));
                    }
                }
                Ok(StrictObject(values))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn normalize_number(text: &str) -> Result<JsonValue, ()> {
    let negative = text.starts_with('-');
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent_text) = unsigned.split_once(['e', 'E']).unwrap_or((unsigned, "0"));
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{integer}{fraction}")
        .trim_start_matches('0')
        .to_owned();
    if digits.is_empty() {
        return Ok(JsonValue::Number {
            negative: false,
            digits: "0".to_owned(),
            scale: 0,
        });
    }
    let trailing_zeroes = digits.len() - digits.trim_end_matches('0').len();
    digits.truncate(digits.len() - trailing_zeroes);
    let exponent = exponent_text.parse::<i64>().map_err(|_| ())?;
    let scale = exponent
        .checked_sub(i64::try_from(fraction.len()).map_err(|_| ())?)
        .and_then(|scale| scale.checked_add(i64::try_from(trailing_zeroes).ok()?))
        .ok_or(())?;
    // Bound the value's scientific exponent, so equivalent decimal spellings
    // share a budget. Zero needs no exponent and was returned above.
    let magnitude = scale
        .checked_add(i64::try_from(digits.len() - 1).map_err(|_| ())?)
        .ok_or(())?;
    if !(-MAX_JSON_EXPONENT..=MAX_JSON_EXPONENT).contains(&magnitude) {
        return Err(());
    }
    Ok(JsonValue::Number {
        negative,
        digits,
        scale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_preserves_exact_and_only_trims_outer_whitespace_when_requested() {
        assert!(!outputs_match(OutputScoring::Exact, "yes\n", "yes"));
        assert!(outputs_match(OutputScoring::Trimmed, " \tyes\r\n", "yes"));
        assert!(!outputs_match(OutputScoring::Trimmed, "a  b", "a b"));
        assert!(!outputs_match(OutputScoring::Trimmed, "YES", "yes"));
        assert!(!outputs_match(OutputScoring::Trimmed, "\u{2003}yes", "yes"));
    }

    #[test]
    fn json_comparison_ignores_object_order_and_formatting_but_preserves_structure() {
        let mode = OutputScoring::JsonCanonical;
        assert!(outputs_match(
            mode,
            " {\"b\":[true,null],\"a\":1.00}\n",
            r#"{"a":1e0,"b":[true,null]}"#
        ));
        assert!(outputs_match(mode, r#"{"a":-0}"#, r#"{"\u0061":0.0}"#));
        for (actual, expected) in [
            ("[1,2]", "[2,1]"),
            ("1", "\"1\""),
            ("false", "0"),
            (r#"{"a":1}"#, r#"{"a":1,"b":null}"#),
            ("\"YES\"", "\"yes\""),
        ] {
            assert!(!outputs_match(mode, actual, expected));
        }
    }

    #[test]
    fn json_numbers_never_round_through_floating_point() {
        let mode = OutputScoring::JsonCanonical;
        assert!(!outputs_match(
            mode,
            "18446744073709551616",
            "18446744073709551617"
        ));
        assert!(!outputs_match(mode, "0.1000000000000000001", "0.1"));
        assert!(outputs_match(
            mode,
            "123456789012345678901234567890",
            "1.23456789012345678901234567890e29"
        ));
    }

    #[test]
    fn malformed_duplicate_and_out_of_budget_json_never_scores_correct() {
        for value in [
            "oops",
            "NaN",
            "Infinity",
            "1 2",
            r#"{"a":1,"a":1}"#,
            r#"{"a":1,"\u0061":1}"#,
            r#"{"nested":{"a":1,"a":2}}"#,
            "1e1000001",
            "1e-1000001",
            "100e1000000",
            "1e9223372036854775807",
            "0.1e-9223372036854775808",
            "1e999999999999999999999999999",
            r#""\ud800""#,
            r#"{"\ud800":1}"#,
            "[1,]",
            "01",
        ] {
            assert!(
                !outputs_match(OutputScoring::JsonCanonical, value, value),
                "{value}"
            );
            assert!(validate_expected(OutputScoring::JsonCanonical, value).is_err());
        }
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(!outputs_match(OutputScoring::JsonCanonical, &deep, &deep));
        assert!(!outputs_match(
            OutputScoring::JsonCanonical,
            &" ".repeat(MAX_TASK_TEXT_BYTES + 1),
            "0"
        ));
    }

    #[test]
    fn json_depth_and_number_budgets_apply_consistently_to_equivalent_values() {
        let mode = OutputScoring::JsonCanonical;
        assert!(outputs_match(mode, "0.1e1000001", "1e1000000"));
        assert!(outputs_match(mode, "10e-1000001", "1e-1000000"));
        assert!(outputs_match(mode, "-0e99999999999999999999999999", "0"));
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH),
            "]".repeat(MAX_JSON_DEPTH)
        );
        assert!(outputs_match(mode, &deep, &deep));
        let object = format!(
            "{}0{}",
            r#"{"a":"#.repeat(MAX_JSON_DEPTH + 1),
            "}".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(!outputs_match(mode, &object, &object));
        assert!(validate_expected(OutputScoring::Exact, "plain text").is_ok());
        assert!(validate_expected(OutputScoring::Trimmed, "plain text").is_ok());
    }
}

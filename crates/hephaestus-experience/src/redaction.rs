use std::collections::BTreeMap;

use crate::TraceKind;

/// Deterministic pre-persistence secret redaction policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactionPolicy {
    known_secrets: Vec<String>,
}

impl RedactionPolicy {
    /// Builds a policy from runtime-known secret literals. Empty literals are ignored.
    #[must_use]
    pub fn new(known_secrets: impl IntoIterator<Item = String>) -> Self {
        let mut known_secrets: Vec<_> = known_secrets
            .into_iter()
            .filter(|secret| !secret.is_empty())
            .collect();
        known_secrets.sort();
        known_secrets.dedup();
        Self { known_secrets }
    }

    pub(crate) fn redact(&self, fields: &BTreeMap<String, String>) -> RedactedFields {
        self.redact_fields(fields, false)
    }

    pub(crate) fn redact_trace(
        &self,
        kind: TraceKind,
        fields: &BTreeMap<String, String>,
    ) -> RedactedFields {
        self.redact_fields(fields, kind == TraceKind::CostObserved)
    }

    fn redact_fields(
        &self,
        fields: &BTreeMap<String, String>,
        allow_usage_counts: bool,
    ) -> RedactedFields {
        let mut values = BTreeMap::new();
        let mut redacted = 0_usize;
        for (key, value) in fields {
            let usage_count = allow_usage_counts && numeric_usage_count(key, value);
            let next = if sensitive_key(key) && !usage_count {
                redacted += 1;
                "[REDACTED]".to_owned()
            } else {
                let next = self.redact_value(value);
                let changed = next != *value;
                redacted += usize::from(changed);
                if usage_count && changed {
                    "[REDACTED]".to_owned()
                } else {
                    next
                }
            };
            values.insert(key.clone(), next);
        }
        RedactedFields { values, redacted }
    }

    /// Redacts one blob of free text through the same value-and-token rules
    /// [`Self::redact`] applies to structured trace fields. Used for raw
    /// provider stdout/stderr, which is not itself a `{key: value}` map.
    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        self.redact_value(text)
    }

    fn redact_value(&self, value: &str) -> String {
        let mut redacted = value.to_owned();
        for secret in &self.known_secrets {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
        redact_prefixed_tokens(&redacted)
    }
}

pub(crate) struct RedactedFields {
    pub values: BTreeMap<String, String>,
    pub redacted: usize,
}

// These exact provider counters are public measurements only in cost traces.
// Other token-bearing names and malformed values still fail closed. Values
// that pass this check still go through known-secret and token redaction.
fn numeric_usage_count(key: &str, value: &str) -> bool {
    matches!(
        key,
        "input_tokens" | "cached_input_tokens" | "output_tokens" | "reasoning_output_tokens"
    ) && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && value.parse::<u64>().is_ok()
}

fn sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    [
        "authorization",
        "cookie",
        "credential",
        "password",
        "privatekey",
        "secret",
        "token",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn redact_prefixed_tokens(value: &str) -> String {
    let mut output = value.to_owned();
    for prefix in ["Bearer ", "sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-"] {
        while let Some(start) = output.find(prefix) {
            let suffix = &output[start + prefix.len()..];
            let token_length = suffix
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, ',' | ';' | '"' | '\'')
                })
                .unwrap_or(suffix.len());
            let end = start + prefix.len() + token_length;
            output.replace_range(start..end, "[REDACTED]");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_fields_do_not_get_the_cost_trace_usage_exception() {
        let fields = BTreeMap::from([("input_tokens".to_owned(), "123".to_owned())]);
        let redacted = RedactionPolicy::new([]).redact(&fields);
        assert_eq!(redacted.values["input_tokens"], "[REDACTED]");
        assert_eq!(redacted.redacted, 1);
    }

    #[test]
    fn redact_text_matches_known_secrets_and_prefixed_tokens() {
        let policy = RedactionPolicy::new(["operator-secret".to_owned()]);
        let redacted = policy.redact_text("token=sk-verysecrettoken1234 and operator-secret");
        assert!(!redacted.contains("sk-verysecrettoken1234"));
        assert!(!redacted.contains("operator-secret"));
        assert!(redacted.contains("[REDACTED]"));
        assert_eq!(
            policy.redact_text("nothing sensitive here"),
            "nothing sensitive here"
        );
    }
}

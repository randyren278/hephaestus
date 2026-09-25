use std::collections::BTreeMap;

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
        let mut values = BTreeMap::new();
        let mut redacted = 0_usize;
        for (key, value) in fields {
            let next = if sensitive_key(key) {
                redacted += 1;
                "[REDACTED]".to_owned()
            } else {
                let next = self.redact_value(value);
                redacted += usize::from(next != *value);
                next
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

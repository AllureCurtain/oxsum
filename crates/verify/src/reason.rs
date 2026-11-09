//! Exact UTF-8 reason commitments, separate from judging a policy's semantics.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Largest complete inline reason, measured as a serialized JSON string in bytes.
pub const INLINE_REASON_JSON_BYTES: usize = 256;

/// A v15 commitment to the complete UTF-8 bytes omitted from an inline reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct ReasonCommitment {
    /// SHA-256 of the complete original UTF-8 reason, lowercase hexadecimal.
    pub sha256: String,
    /// Exact UTF-8 byte length, independent of JSON escaping or scalar count.
    pub utf8_bytes: u64,
}

impl ReasonCommitment {
    /// Commit exact text, including its whitespace and line breaks. Empty text
    /// remains inline and is never a valid omitted-text commitment.
    #[must_use]
    pub fn from_reason(reason: &str) -> Self {
        Self {
            sha256: Sha256::digest(reason.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            utf8_bytes: reason.len() as u64,
        }
    }

    pub(crate) fn valid(&self) -> bool {
        self.utf8_bytes > 0
            && self.utf8_bytes <= i64::MAX as u64
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

/// Whether a complete string fits the v15 inline JSON-byte budget.
#[must_use]
pub fn reason_fits_inline(reason: &str) -> bool {
    serde_json::to_vec(reason).is_ok_and(|encoded| encoded.len() <= INLINE_REASON_JSON_BYTES)
}

/// Compare separately supplied text with its exact commitment. This checks text
/// identity only; callers first verify the entry containing the commitment.
#[must_use]
pub fn verify_reason_commitment(commitment: &ReasonCommitment, reason: &str) -> bool {
    commitment.valid() && *commitment == ReasonCommitment::from_reason(reason)
}

pub(crate) fn valid_fields(value: &serde_json::Value, required: bool) -> bool {
    match (value.get("reason"), value.get("reasonCommitment")) {
        (None, None) => !required,
        (Some(reason), None) => reason.as_str().is_some_and(reason_fits_inline),
        (None, Some(commitment)) => serde_json::from_value::<ReasonCommitment>(commitment.clone())
            .is_ok_and(|commitment| commitment.valid()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commitments_bind_exact_utf8_bytes_and_json_inline_bound() {
        for reason in ["ordinary words".to_owned(), "🦀\n\"\\".repeat(100)] {
            let commitment = ReasonCommitment::from_reason(&reason);
            assert!(verify_reason_commitment(&commitment, &reason));
            assert!(!verify_reason_commitment(
                &commitment,
                &(reason.clone() + " ")
            ));
            let mut wrong = commitment.clone();
            wrong.utf8_bytes += 1;
            assert!(!verify_reason_commitment(&wrong, &reason));
        }
        assert!(reason_fits_inline(&"a".repeat(254)));
        assert!(!reason_fits_inline(&"a".repeat(255)));
        assert!(reason_fits_inline(&"\u{0001}".repeat(42)));
        assert!(!reason_fits_inline(&"\u{0001}".repeat(43)));
        assert!(reason_fits_inline(&"🦀".repeat(63)));
        assert!(!reason_fits_inline(&"🦀".repeat(64)));
    }
}

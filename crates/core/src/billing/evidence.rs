//! Canonical v15 reason fields and the gateway's encoded-byte envelope.

use serde::Serialize;

use crate::WalletError;

/// Gateway admission is stricter than the engine's Unicode-scalar capacity.
pub const GATEWAY_SETTLEMENT_JSON_BYTES: usize = 16_384;

/// The writer stores complete short text or an explicit commitment, never a
/// shortened string pretending to be the full policy/provider reason.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReasonFields<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_commitment: Option<oxsum_verify::ReasonCommitment>,
}

impl<'a> ReasonFields<'a> {
    pub(super) fn of(reason: Option<&'a str>) -> Self {
        let inline = reason.filter(|reason| oxsum_verify::reason_fits_inline(reason));
        Self {
            reason: inline,
            reason_commitment: reason
                .filter(|_| inline.is_none())
                .map(oxsum_verify::ReasonCommitment::from_reason),
        }
    }
}

/// Validate actual canonical JSON bytes before a gateway operation can execute.
pub fn validate_gateway_description(description: &str) -> Result<(), WalletError> {
    if description.len() > GATEWAY_SETTLEMENT_JSON_BYTES {
        return Err(WalletError::InvalidInput(
            "complete gateway evidence exceeds the 16384-byte settlement budget".into(),
        ));
    }
    Ok(())
}

/// Retain exact full reason bytes beside the ledger commitment in the same usage
/// transaction. Provider-owned extras cannot replace this reserved field.
pub fn retain_reason_evidence(
    usage: &mut crate::UsageRecord,
    guardrails: &[super::SettlementGuardrailOwned],
    failure: Option<&str>,
) {
    let notes: Vec<serde_json::Value> = guardrails
        .iter()
        .enumerate()
        .map(|(index, note)| {
            serde_json::json!({
                "index":index,"policy":note.policy,"verdict":note.verdict,"phase":note.phase,
                "streamed":note.streamed,"reason":note.reason,
                "commitment":oxsum_verify::ReasonCommitment::from_reason(&note.reason),
            })
        })
        .collect();
    let old = usage.usage_details.take();
    let mut details = match old {
        Some(serde_json::Value::Object(map)) => map,
        Some(value) => serde_json::Map::from_iter([("providerDetails".into(), value)]),
        None => serde_json::Map::new(),
    };
    // This namespace belongs to the settlement writer. Overwrite a supplied
    // value even on an uneventful turn, and make repeated preparation stable.
    details.remove("oxsumReasonEvidence");
    if !notes.is_empty() || failure.is_some() {
        details.insert("oxsumReasonEvidence".into(),serde_json::json!({"version":1,"guardrails":notes,
            "failure":failure.map(|reason|serde_json::json!({"reason":reason,"commitment":oxsum_verify::ReasonCommitment::from_reason(reason)}))}));
    }
    usage.usage_details = (!details.is_empty()).then_some(serde_json::Value::Object(details));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn full_reason_evidence_cannot_be_supplied_by_provider_and_is_retry_stable() {
        let mut usage = crate::UsageRecord {
            usage_details: Some(
                serde_json::json!({"oxsumReasonEvidence":{"forged":true},"provider_raw":{"tokens":3}}),
            ),
            ..crate::UsageRecord::default()
        };
        retain_reason_evidence(&mut usage, &[], None);
        assert_eq!(
            usage.usage_details,
            Some(serde_json::json!({"provider_raw":{"tokens":3}}))
        );
        let reason = "  🦀\n\"\\\u{0001}".repeat(200);
        let notes = vec![super::super::SettlementGuardrailOwned {
            policy: "p".into(),
            verdict: "flagged",
            reason: reason.clone(),
            phase: "post_call",
            streamed: true,
        }];
        retain_reason_evidence(&mut usage, &notes, Some(&reason));
        let retained = usage.usage_details.clone();
        retain_reason_evidence(&mut usage, &notes, Some(&reason));
        assert_eq!(usage.usage_details, retained);
        let details = usage.usage_details.as_ref().expect("retained reasons");
        let note = &details["oxsumReasonEvidence"]["guardrails"][0];
        assert_eq!(note["reason"], reason);
        assert_eq!(note["phase"], "post_call");
        assert_eq!(note["index"], 0);
        assert_eq!(
            note["commitment"],
            serde_json::json!(oxsum_verify::ReasonCommitment::from_reason(&reason))
        );
        assert_eq!(details["oxsumReasonEvidence"]["failure"]["reason"], reason);
    }

    #[test]
    fn canonical_reason_fields_are_exclusive_and_never_truncate_text() {
        for reason in [
            "a".repeat(254),
            "a".repeat(255),
            "\u{0001}".repeat(43),
            "🦀".repeat(64),
        ] {
            let fields =
                serde_json::to_value(ReasonFields::of(Some(&reason))).expect("reason serializes");
            if oxsum_verify::reason_fits_inline(&reason) {
                assert_eq!(fields["reason"], reason);
                assert!(fields.get("reasonCommitment").is_none());
            } else {
                assert!(fields.get("reason").is_none());
                assert_eq!(
                    fields["reasonCommitment"],
                    serde_json::json!(oxsum_verify::ReasonCommitment::from_reason(&reason))
                );
            }
        }
    }

    #[test]
    fn description_capacity_counts_actual_utf8_bytes() {
        assert!(validate_gateway_description(&"a".repeat(GATEWAY_SETTLEMENT_JSON_BYTES)).is_ok());
        assert!(
            validate_gateway_description(&"a".repeat(GATEWAY_SETTLEMENT_JSON_BYTES + 1)).is_err()
        );
        assert!(
            validate_gateway_description(&"🦀".repeat(GATEWAY_SETTLEMENT_JSON_BYTES / 4 + 1))
                .is_err()
        );
    }
}

//! Complete canonical settlement upper envelope, checked before execution.

use oxsum_core::{
    BillLine, Price, RuleMatch, Settlement, SettlementGuardrail, SettlementKind, UsageRecord,
    WalletError,
};
use uuid::Uuid;

// A finite [0,1] f64 can need 17 significand digits, a decimal point and
// `e-` plus a three-digit exponent: 23 JSON bytes. MIN_POSITIVE uses all
// 23 in the real serde encoder. Reserve this even for today's exact-only hit.
const CACHE_SIMILARITY_ENVELOPE: f64 = f64::MIN_POSITIVE;

/// Immutable route fields that can enter this turn's final settlement.
pub(super) struct RouteEvidence<'a> {
    pub request: &'a str,
    pub channel: &'a str,
    pub requested_model: &'a str,
    pub served_model: &'a str,
    pub upstream_model: Option<&'a str>,
    pub service_tier: Option<&'a str>,
    pub experiment_variant: Option<&'a str>,
    pub price: &'a Price,
    pub policies: &'a [String],
}

/// Use the actual writer with every optional field and numeric/collection width.
/// These bounds describe current gateway output; the maximal schema test pins
/// encoder keys so new fields cannot silently bypass this admission contract.
pub(super) fn preflight(route: &RouteEvidence<'_>) -> Result<(), WalletError> {
    let description = upper_envelope(route)?;
    oxsum_core::validate_gateway_description(&description)
}

fn upper_envelope(route: &RouteEvidence<'_>) -> Result<String, WalletError> {
    oxsum_core::validate_policy_names(route.policies)?;
    for (name, value, limit) in [
        ("request", route.request, 64),
        ("channel", route.channel, 40),
        ("requested model", route.requested_model, 200),
        ("served model", route.served_model, 200),
        ("upstream model", route.upstream_model.unwrap_or(""), 200),
        ("service tier", route.service_tier.unwrap_or(""), 64),
        (
            "experiment variant",
            route.experiment_variant.unwrap_or(""),
            200,
        ),
    ] {
        if value.len() > limit {
            return Err(WalletError::InvalidInput(format!(
                "{name} exceeds gateway evidence capacity"
            )));
        }
    }
    let id = Uuid::nil();
    let digest = "f".repeat(64);
    // A full inline string is larger than its fixed commitment representation.
    let reason = "x".repeat(oxsum_core::INLINE_REASON_JSON_BYTES - 2);
    let usage = UsageRecord {
        input_tokens: i64::MAX,
        output_tokens: i64::MAX,
        cached_tokens: i64::MAX,
        cache_write_5m_tokens: i64::MAX,
        cache_write_1h_tokens: i64::MAX,
        reasoning_tokens: i64::MAX,
        tool_calls: i64::MAX,
        image_input_tokens: i64::MAX,
        audio_input_tokens: i64::MAX,
        video_input_tokens: i64::MAX,
        image_output_tokens: i64::MAX,
        audio_output_tokens: i64::MAX,
        service_tier: route.service_tier.map(str::to_owned),
        // Usage validation bounds both context names at 64 UTF-8 bytes. Every
        // byte becoming a six-byte JSON escape is the conservative envelope.
        event_type: Some("\u{0001}".repeat(64)),
        ..UsageRecord::default()
    };
    let lines: Vec<BillLine> = [
        "input",
        "cache_read",
        "cache_write_5m",
        "cache_write_1h",
        "output",
        "reasoning",
        "request",
    ]
    .into_iter()
    .map(|item| BillLine {
        item: item.into(),
        units: i64::MAX,
        price_per_m: i64::MAX,
    })
    .collect();
    let mut condition = None;
    let mut condition_len = 0;
    for rule in &route.price.rules {
        let encoded =
            serde_json::to_vec(&rule.cond).map_err(|e| WalletError::InvalidInput(e.to_string()))?;
        if encoded.len() > condition_len {
            condition_len = encoded.len();
            condition = Some(&rule.cond);
        }
    }
    let fallback_condition = RuleMatch {
        service_tier: Some("\u{0001}".repeat(64)),
        min_input_tokens: Some(i64::MAX),
        max_input_tokens: Some(i64::MAX),
    };
    if condition_len
        < serde_json::to_vec(&fallback_condition)
            .map_err(|e| WalletError::InvalidInput(e.to_string()))?
            .len()
    {
        condition = Some(&fallback_condition);
    }
    let notes = route
        .policies
        .iter()
        .flat_map(|policy| {
            let reason = reason.as_str();
            ["pre_call", "post_call"]
                .into_iter()
                .map(move |phase| SettlementGuardrail {
                    policy,
                    verdict: "flagged",
                    reason,
                    phase,
                    streamed: phase == "post_call",
                })
        })
        .collect();
    let settlement = Settlement {
        request: route.request,
        channel: route.channel,
        model: route.served_model,
        requested_model: route.requested_model,
        served_model: route.served_model,
        upstream_model: route.upstream_model,
        price_version: i64::MAX,
        kind: SettlementKind::UpstreamUnreachable,
        usage: &usage,
        lines: &lines,
        matched_rule: condition,
        discount_percent: Some(i64::MAX),
        charged: i64::MAX,
        freeze: i64::MAX,
        upstream_attempts: i64::MAX,
        mandate: Some(oxsum_core::SettlementMandate {
            id: &id,
            hash: &digest,
        }),
        subscription: Some(oxsum_core::SettlementSubscription {
            subscription_id: &id,
            period_id: &id,
            drawn_minor: i64::MAX,
            charged_minor: i64::MAX,
        }),
        cache: Some(oxsum_core::SettlementCache {
            source_entry_id: &id,
            similarity: CACHE_SIMILARITY_ENVELOPE,
        }),
        guardrail: notes,
        reason: Some(&reason),
        experiment: Some(oxsum_core::SettlementExperiment {
            id: &id,
            variant: route.experiment_variant.unwrap_or(""),
        }),
        sale: Some(oxsum_core::SettlementSale { digest: &digest }),
    };
    settlement.description()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn price() -> Price {
        serde_json::from_value(serde_json::json!({
            "inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":100,
            "cacheReadPricePerMillion":1,"cacheWrite5mPricePerMillion":1,
            "cacheWrite1hPricePerMillion":1,"reasoningPricePerMillion":1,"costPerRequest":1
        }))
        .unwrap()
    }

    fn keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn maximal_encoded_schema_fits_and_pins_every_optional_field() {
        let price = price();
        let policies: Vec<String> = (0..8).map(|i| format!("p{i}{}", "x".repeat(62))).collect();
        let requested = "\u{0001}".repeat(200);
        let served = "\u{0002}".repeat(200);
        let channel = "\u{0001}".repeat(40);
        let tier = "\u{0001}".repeat(64);
        let route = RouteEvidence {
            request: &tier,
            channel: &channel,
            requested_model: &requested,
            served_model: &served,
            upstream_model: Some(&requested),
            service_tier: Some(&tier),
            experiment_variant: Some(&requested),
            price: &price,
            policies: &policies,
        };
        let encoded = upper_envelope(&route).unwrap();
        preflight(&route).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            keys(&value),
            [
                "cache",
                "channel",
                "charged",
                "discountPercent",
                "experiment",
                "freeze",
                "guardrail",
                "kind",
                "lines",
                "mandate",
                "matchedRule",
                "model",
                "priceVersion",
                "reason",
                "request",
                "requestedModel",
                "sale",
                "servedModel",
                "subscription",
                "upstreamAttempts",
                "upstreamModel",
                "usage",
                "v"
            ]
        );
        assert_eq!(
            keys(&value["usage"]),
            [
                "audioInputTokens",
                "audioOutputTokens",
                "cacheWrite1hTokens",
                "cacheWrite5mTokens",
                "cachedTokens",
                "eventType",
                "imageInputTokens",
                "imageOutputTokens",
                "inputTokens",
                "outputTokens",
                "reasoningTokens",
                "serviceTier",
                "toolCalls",
                "videoInputTokens"
            ]
        );
        assert_eq!(
            keys(&serde_json::to_value(UsageRecord::default()).unwrap()),
            [
                "audioInputTokens",
                "audioOutputTokens",
                "cacheWrite1hTokens",
                "cacheWrite5mTokens",
                "cachedTokens",
                "endUser",
                "eventType",
                "imageInputTokens",
                "imageOutputTokens",
                "inputTokens",
                "outputTokens",
                "reasoningTokens",
                "serviceTier",
                "tags",
                "toolCalls",
                "usageDetails",
                "videoInputTokens"
            ]
        );
        assert_eq!(
            keys(&value["matchedRule"]),
            ["maxInputTokens", "minInputTokens", "serviceTier"]
        );
        assert_eq!(
            keys(&value["subscription"]),
            ["chargedMinor", "drawnMinor", "periodId", "subscriptionId"]
        );
        assert_eq!(keys(&value["mandate"]), ["hash", "id"]);
        assert_eq!(keys(&value["cache"]), ["similarity", "sourceEntryId"]);
        assert_eq!(keys(&value["experiment"]), ["id", "variant"]);
        assert_eq!(keys(&value["sale"]), ["digest"]);
        assert_eq!(value["guardrail"].as_array().unwrap().len(), 16);
        assert_eq!(
            keys(&value["guardrail"][1]),
            ["phase", "policy", "reason", "streamed", "verdict"]
        );
        assert_eq!(value["lines"].as_array().unwrap().len(), 7);
        // An actual all-dimension itemization must stay covered by the envelope.
        let usage = UsageRecord {
            input_tokens: 4,
            output_tokens: 2,
            cached_tokens: 1,
            cache_write_5m_tokens: 1,
            cache_write_1h_tokens: 1,
            reasoning_tokens: 1,
            ..UsageRecord::default()
        };
        let actual = price.itemize(&usage, true).unwrap();
        let covered: Vec<_> = value["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line[0].as_str().unwrap())
            .collect();
        assert_eq!(actual.lines.len(), covered.len());
        assert!(
            actual
                .lines
                .iter()
                .all(|line| covered.contains(&line.item.as_str()))
        );
    }

    #[test]
    fn complete_route_bytes_and_unbounded_legacy_condition_are_refused() {
        let mut price = price();
        let route = RouteEvidence {
            request: "r",
            channel: "c",
            requested_model: "requested",
            served_model: "served",
            upstream_model: None,
            service_tier: None,
            experiment_variant: None,
            price: &price,
            policies: &[],
        };
        preflight(&route).unwrap();
        let oversized = "🦀".repeat(51);
        assert!(
            preflight(&RouteEvidence {
                requested_model: &oversized,
                ..route
            })
            .is_err()
        );
        let set: oxsum_core::PriceSet = serde_json::from_value(
            serde_json::json!({"inputPricePerMillion":1,"outputPricePerMillion":1}),
        )
        .unwrap();
        price.rules.push(oxsum_core::PriceRule {
            cond: RuleMatch {
                service_tier: Some("\u{0001}".repeat(3000)),
                min_input_tokens: None,
                max_input_tokens: None,
            },
            price: set,
        });
        let route = RouteEvidence {
            request: "r",
            channel: "c",
            requested_model: "requested",
            served_model: "served",
            upstream_model: None,
            service_tier: None,
            experiment_variant: None,
            price: &price,
            policies: &[],
        };
        assert!(upper_envelope(&route).unwrap().len() > oxsum_core::GATEWAY_SETTLEMENT_JSON_BYTES);
        assert!(preflight(&route).is_err());
    }

    #[test]
    fn exact_budget_envelope_covers_every_finite_similarity_encoding_width() {
        assert_eq!(
            serde_json::to_string(&CACHE_SIMILARITY_ENVELOPE)
                .unwrap()
                .len(),
            23
        );
        let mut price = price();
        let set: oxsum_core::PriceSet = serde_json::from_value(
            serde_json::json!({"inputPricePerMillion":1,"outputPricePerMillion":1}),
        )
        .unwrap();
        price.rules.push(oxsum_core::PriceRule {
            cond: RuleMatch {
                service_tier: Some("x".repeat(1000)),
                min_input_tokens: Some(i64::MAX),
                max_input_tokens: Some(i64::MAX),
            },
            price: set,
        });
        let encode = |price: &Price| {
            upper_envelope(&RouteEvidence {
                request: "r",
                channel: "c",
                requested_model: "requested",
                served_model: "served",
                upstream_model: Some("upstream"),
                service_tier: Some("priority"),
                experiment_variant: Some("variant"),
                price,
                policies: &[],
            })
            .unwrap()
        };
        let short = encode(&price);
        let remaining = oxsum_core::GATEWAY_SETTLEMENT_JSON_BYTES - short.len();
        price.rules[0]
            .cond
            .service_tier
            .as_mut()
            .unwrap()
            .push_str(&"x".repeat(remaining));
        let envelope = encode(&price);
        assert_eq!(envelope.len(), oxsum_core::GATEWAY_SETTLEMENT_JSON_BYTES);
        for similarity in [
            0.0,
            -0.0,
            1.0,
            0.12345678901234567,
            0.000012345678901234567,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            f64::from_bits(1.0f64.to_bits() - 1),
        ] {
            let mut actual: serde_json::Value = serde_json::from_str(&envelope).unwrap();
            actual["cache"]["similarity"] = serde_json::json!(similarity);
            assert!(actual.to_string().len() <= envelope.len(), "{similarity}");
        }
        // Exponent boundaries and mantissas exercise both decimal and
        // scientific encodings with the same serde serializer.
        for exponent in 0..=1023u64 {
            for mantissa in [0, 1, 0x7_ffff_ffff_ffff, 0xf_ffff_ffff_ffff] {
                let similarity = f64::from_bits((exponent << 52) | mantissa);
                if similarity <= 1.0 {
                    assert!(serde_json::to_string(&similarity).unwrap().len() <= 23);
                }
            }
        }
    }

    #[test]
    fn kind_capacity_pins_the_complete_current_variant_set() {
        fn check(kind: SettlementKind) {
            // An added enum variant must update this exhaustive match and its
            // envelope assertion before the gateway can compile its tests.
            match kind {
                SettlementKind::Usage
                | SettlementKind::Estimated
                | SettlementKind::ClientCancelled
                | SettlementKind::UpstreamError
                | SettlementKind::UpstreamUnreachable
                | SettlementKind::Capped
                | SettlementKind::Swept
                | SettlementKind::Unpriced
                | SettlementKind::Released
                | SettlementKind::Refused => {
                    assert!(
                        serde_json::to_string(&kind).unwrap().len()
                            <= serde_json::to_string(&SettlementKind::UpstreamUnreachable)
                                .unwrap()
                                .len()
                    );
                }
            }
        }
        for kind in [
            SettlementKind::Usage,
            SettlementKind::Estimated,
            SettlementKind::ClientCancelled,
            SettlementKind::UpstreamError,
            SettlementKind::UpstreamUnreachable,
            SettlementKind::Capped,
            SettlementKind::Swept,
            SettlementKind::Unpriced,
            SettlementKind::Released,
            SettlementKind::Refused,
        ] {
            check(kind);
        }
    }
}

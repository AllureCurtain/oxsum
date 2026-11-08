//! Verify exact provider bytes under route-selected credentials, then normalize.
use hmac::{KeyInit, Mac, SimpleHmac};
use oxsum_core::{
    MerchantEventKind as Kind, MerchantMode, MerchantRail, MerchantSigningContext,
    VerifiedMerchantEvent,
};
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MerchantEventError {
    Signature,
    Malformed,
}
fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            let h = (b[0] as char).to_digit(16)?;
            let l = (b[1] as char).to_digit(16)?;
            Some((h * 16 + l) as u8)
        })
        .collect()
}
fn hmac256(secret: &str, prefix: &[u8], body: &[u8], signature: &str) -> bool {
    let Some(tag) = decode_hex(signature) else {
        return false;
    };
    let Ok(mut mac) = <SimpleHmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(prefix);
    mac.update(body);
    mac.verify_slice(&tag).is_ok()
}
/// Timestamped rails use a bounded replay window; Hyperswitch relies on its
/// durable event identity because its signature format carries no timestamp.
pub fn verify(
    context: &MerchantSigningContext,
    signature: &str,
    timestamp: Option<&str>,
    body: &[u8],
    now: i64,
) -> bool {
    match context.rail {
        MerchantRail::Hyperswitch => {
            let Some(tag) = decode_hex(signature.trim()) else {
                return false;
            };
            let Ok(mut mac) = <SimpleHmac<Sha512> as KeyInit>::new_from_slice(
                context.webhook_secret().as_bytes(),
            ) else {
                return false;
            };
            mac.update(body);
            mac.verify_slice(&tag).is_ok()
        }
        MerchantRail::Stripe => {
            let fields = signature
                .split(',')
                .filter_map(|f| f.trim().split_once('='))
                .collect::<Vec<_>>();
            let timestamps = fields
                .iter()
                .filter(|(k, _)| *k == "t")
                .map(|(_, v)| *v)
                .collect::<Vec<_>>();
            if timestamps.len() != 1 {
                return false;
            }
            let Ok(timestamp) = timestamps[0].parse::<i64>() else {
                return false;
            };
            if timestamp.abs_diff(now) > 300 {
                return false;
            }
            let prefix = format!("{timestamp}.");
            fields
                .iter()
                .filter(|(k, _)| *k == "v1")
                .any(|(_, tag)| hmac256(context.webhook_secret(), prefix.as_bytes(), body, tag))
        }
        MerchantRail::Airwallex => {
            let Some(timestamp) = timestamp else {
                return false;
            };
            let Ok(milliseconds) = timestamp.parse::<i64>() else {
                return false;
            };
            if (milliseconds / 1000).abs_diff(now) > 300 {
                return false;
            }
            hmac256(
                context.webhook_secret(),
                timestamp.as_bytes(),
                body,
                signature,
            )
        }
    }
}
fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}
fn id(value: &Value) -> Option<Uuid> {
    value.as_str().and_then(|s| Uuid::parse_str(s).ok())
}
/// Exact decimal major units to integer cents from the original JSON lexeme.
/// RawValue keeps ordinary Value/proof serialization behavior unchanged.
pub fn decimal_minor_text(raw: &str) -> Option<i64> {
    let raw = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    if raw.is_empty() || raw.starts_with('-') || raw.starts_with('+') {
        return None;
    }
    let mut parts = raw.split('.');
    let whole = parts.next()?;
    let fractional = parts.next().unwrap_or("");
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fractional.bytes().all(|b| b.is_ascii_digit())
        || fractional.len() > 2
    {
        return None;
    }
    let whole = whole.parse::<i64>().ok()?;
    let cents = match fractional.len() {
        0 => 0,
        1 => fractional.parse::<i64>().ok()?.checked_mul(10)?,
        2 => fractional.parse::<i64>().ok()?,
        _ => return None,
    };
    whole.checked_mul(100)?.checked_add(cents)
}
fn airwallex_minor(body: &[u8]) -> Option<i64> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        data: Data,
    }
    #[derive(serde::Deserialize)]
    struct Data {
        object: Object,
    }
    #[derive(serde::Deserialize)]
    struct Object {
        amount: Box<serde_json::value::RawValue>,
    }
    let event: Envelope = serde_json::from_slice(body).ok()?;
    decimal_minor_text(event.data.object.amount.get())
}

pub fn normalize(
    context: &MerchantSigningContext,
    body: &[u8],
) -> Result<(VerifiedMerchantEvent, [u8; 32]), MerchantEventError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| MerchantEventError::Malformed)?;
    let (event_id, kind, object) = match context.rail {
        MerchantRail::Hyperswitch => {
            let kind = match value["event_type"].as_str() {
                Some("payment_succeeded") => Kind::Succeeded,
                Some("payment_failed") => Kind::Failed,
                Some("payment_cancelled" | "payment_expired") => Kind::Expired,
                Some("refund_succeeded") => Kind::RefundSucceeded,
                Some("refund_failed") => Kind::RefundFailed,
                Some("dispute_opened") => Kind::Dispute,
                _ => Kind::Unsupported,
            };
            (text(&value["event_id"]), kind, &value["content"]["object"])
        }
        MerchantRail::Stripe => {
            let kind = match value["type"].as_str() {
                Some("checkout.session.completed")
                    if value["data"]["object"]["payment_status"].as_str() == Some("paid")
                        && value["data"]["object"]["mode"].as_str() != Some("subscription") =>
                {
                    Kind::Succeeded
                }
                Some("checkout.session.expired") => Kind::Expired,
                _ => Kind::Unsupported,
            };
            (text(&value["id"]), kind, &value["data"]["object"])
        }
        MerchantRail::Airwallex => {
            let kind = match value["name"].as_str() {
                Some("payment_intent.succeeded") => Kind::Succeeded,
                Some("payment_intent.cancelled" | "payment_intent.expired") => Kind::Expired,
                Some("payment_intent.failed") => Kind::Failed,
                _ => Kind::Unsupported,
            };
            (text(&value["id"]), kind, &value["data"]["object"])
        }
    };
    let metadata = &object["metadata"];
    // Invalid supplied IDs are not missing metadata. In particular they must
    // not be replaced by the trusted provider-reference recovery path.
    for field in ["payment_id", "deposit_id", "organization_id"] {
        if metadata
            .get(field)
            .is_some_and(|value| !value.is_null() && id(value).is_none())
        {
            return Err(MerchantEventError::Malformed);
        }
    }
    let payment_id = id(&metadata["payment_id"])
        .or_else(|| id(&metadata["deposit_id"]))
        .or_else(|| id(&object["client_reference_id"]))
        .or_else(|| id(&object["merchant_order_id"]));
    let provider_payment_id = if context.rail == MerchantRail::Hyperswitch {
        text(&object["payment_id"])
    } else {
        text(&object["id"])
    };
    let amount_minor = match context.rail {
        MerchantRail::Hyperswitch => object["amount"].as_i64(),
        MerchantRail::Stripe => object["amount_total"].as_i64(),
        MerchantRail::Airwallex => airwallex_minor(body),
    };
    let account = match context.rail {
        MerchantRail::Hyperswitch => {
            text(&value["merchant_id"]).or_else(|| text(&object["merchant_id"]))
        }
        MerchantRail::Stripe => text(&value["account"]),
        MerchantRail::Airwallex => {
            text(&value["account_id"]).or_else(|| text(&object["account_id"]))
        }
    };
    let mode = value["livemode"]
        .as_bool()
        .or_else(|| object["livemode"].as_bool())
        .map(|live| {
            if live {
                MerchantMode::Live
            } else {
                MerchantMode::Test
            }
        });
    let event = VerifiedMerchantEvent {
        event_id: event_id.ok_or(MerchantEventError::Malformed)?,
        kind,
        payment_id,
        provider_payment_id,
        organization_id: id(&metadata["organization_id"]),
        provider_account_id: account,
        mode,
        amount_minor,
        currency: text(&object["currency"]).map(|s| s.to_ascii_uppercase()),
        refund_id: text(&object["refund_id"]),
    };
    Ok((event, Sha256::digest(body).into()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_values_are_exact_and_excess_precision_or_overflow_is_refused() {
        for (raw, expected) in [
            ("12.34", Some(1234)),
            ("0.01", Some(1)),
            ("90071992547409.91", Some(9007199254740991)),
            ("1.001", None),
            ("-1", None),
            ("92233720368547758.08", None),
            ("1e2", None),
        ] {
            assert_eq!(decimal_minor_text(raw), expected, "{raw}");
        }
    }
}

//! The composed `/verify` bill verdict: inclusion, the charge recompute, and
//! the price and sale bindings, each reduced to the sentence a page prints.

use doubleentry::Hash;

use crate::bundle::verify_bundle;
use crate::charge::{ChargeCheck, verify_charge};
use crate::price::{PriceError, verify_price_head};
use crate::sale::{SaleError, named_sale, verify_sale_head};

/// The whole `/verify` bill verdict, composed: inclusion, the charge recompute
/// and the price binding, each reduced to the sentence a page prints.
///
/// The server-rendered form and the browser's wasm export both build their
/// verdict from this — one composition, two execution sides (issue #284).
/// `verdict` is `"pass"`, `"chargeMismatch"` or `"fail"`; `charge` and `price`
/// are empty on a fail.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BillVerdict {
    /// The verdict's machine name: `"pass"`, `"chargeMismatch"` or `"fail"`.
    pub verdict: &'static str,
    /// The charge-recompute sentence, including the mandate line a v7 record
    /// carries.
    pub charge: String,
    /// The price-binding sentence.
    pub price: String,
    /// The sale-binding sentence, present only when the settlement committed a
    /// `sale` digest — absent on wholesale-priced and pre-v14 bills.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sale: Option<String>,
}

/// Runs the bill checks the `/verify` page composes — [`verify_bundle`] against
/// `expected_hash`, then [`verify_charge`] and [`verify_price_head`] on a pass —
/// and composes the verdict text. `json` is the bundle's inner JSON (the caller
/// unwraps a `data` envelope). A malformed bundle is an error; a bundle that
/// does not verify is a [`BillVerdict`] whose `verdict` is `"fail"` — the
/// error/verdict split the functions below already make.
///
/// # Errors
///
/// [`serde_json::Error`] when `json` does not parse.
pub fn verify_bill(json: &str, expected_hash: &Hash) -> Result<BillVerdict, serde_json::Error> {
    if !verify_bundle(json, expected_hash)? {
        return Ok(BillVerdict {
            verdict: "fail",
            charge: String::new(),
            price: String::new(),
            sale: None,
        });
    }
    let description = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|bundle| {
            bundle
                .get("entry")?
                .get("description")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_default();
    match verify_charge(&description) {
        ChargeCheck::Mismatch => Ok(BillVerdict {
            verdict: "chargeMismatch",
            charge: String::new(),
            price: String::new(),
            sale: None,
        }),
        check => {
            // The mandate line is part of the charge sentence: a v7 record that
            // names the delegation says so in the verdict.
            let mandate = serde_json::from_str::<serde_json::Value>(&description)
                .ok()
                .and_then(|record| record.get("mandate").cloned())
                .and_then(|mandate| {
                    let id = mandate.get("id")?.as_str()?.to_owned();
                    let hash = mandate.get("hash")?.as_str()?.to_owned();
                    Some(format!(
                        " The turn ran under spend mandate {id} (blob sha256 {}…).",
                        &hash[..16.min(hash.len())]
                    ))
                })
                .unwrap_or_default();
            // The sentences carry their leading space: the verdict text flows
            // on from the page's own clause — "…the inclusion proof links it to
            // the tree head." — so a renderer concatenates without joining.
            let charge = match check {
                ChargeCheck::Recomputed => {
                    " The recorded charge itself recomputes from the usage and rates inside the entry."
                        .to_owned()
                }
                ChargeCheck::NotASettlement => {
                    " The entry is not a usage settlement, so there is no charge to recompute."
                        .to_owned()
                }
                ChargeCheck::OlderSchema => {
                    " The record predates charge verification — the entry is proven, but its arithmetic is too old to recompute."
                        .to_owned()
                }
                ChargeCheck::NewerSchema { .. } => {
                    " The record is newer than this verifier — the entry is proven, but its arithmetic is skipped rather than guessed at."
                        .to_owned()
                }
                ChargeCheck::Mismatch => unreachable!("handled above"),
            } + &mandate;
            // The sale binding (issue #600): when the settlement committed a
            // `sale` digest, the customer terms are proven by the sale
            // checkpoint — the wholesale `price` binding attests a different
            // catalog and is reported for what it is, not as the sale's proof.
            let sale_priced = matches!(named_sale(&description), Ok(Some(_)));
            let sale = match verify_sale_head(json) {
                Ok(()) => Some(
                    " The customer sale terms it names are bound to a checkpoint the operator signed."
                        .to_owned(),
                ),
                Err(SaleError::NotSalePriced) => None,
                Err(SaleError::Absent) => Some(
                    " This bundle carries no signed sale binding — the entry is proven, but its customer terms are not independently attested."
                        .to_owned(),
                ),
                Err(error) => Some(format!(" Its sale binding does not verify: {error}.")),
            };
            let price = match verify_price_head(json) {
                Ok(()) => " The price it names is bound to a checkpoint the operator signed."
                    .to_owned(),
                Err(PriceError::Absent) if sale_priced => {
                    // A sale-priced bill attests its customer terms through the
                    // sale checkpoint; the wholesale catalog row is not the
                    // price it charged, so its absence is no gap.
                    " The bill priced under a sale book, so no wholesale price binding applies."
                        .to_owned()
                }
                Err(PriceError::Absent) => {
                    " This bundle carries no signed price binding — the entry is proven, but its price is not independently attested."
                        .to_owned()
                }
                Err(error) => format!(" Its price binding does not verify: {error}."),
            };
            Ok(BillVerdict {
                verdict: "pass",
                charge,
                price,
                sale,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// `verify_bill` composes inclusion, charge and price into the verdict the
    /// page prints — the composition the server-rendered form and the wasm
    /// export share (issue #284).
    mod bill_verdict {
        use crate::bundle::tests::{BUNDLE, CONTENT_HASH, content_hash};
        use crate::{Hash, verify_bill};

        #[test]
        fn a_genuine_top_up_passes_with_no_charge_or_price() {
            let bill = verify_bill(BUNDLE, &content_hash()).expect("the fixture parses");
            assert_eq!(bill.verdict, "pass");
            // A top-up is not a settlement: the charge sentence says so. Its
            // empty description names no price, so the price check reports
            // Malformed — the same sentence the server page has always printed
            // for a non-settlement bundle.
            assert!(bill.charge.contains("no charge to recompute"));
            assert!(bill.price.contains("does not verify"));
        }

        #[test]
        fn a_tampered_bundle_is_a_fail_not_an_error() {
            let tampered = BUNDLE.replacen("\"10.000000\"", "\"10.000001\"", 1);
            let bill = verify_bill(&tampered, &content_hash()).expect("the bundle parses");
            assert_eq!(bill.verdict, "fail");
            assert!(bill.charge.is_empty() && bill.price.is_empty());
        }

        #[test]
        fn a_wrong_content_hash_is_a_fail() {
            let mut bytes = *content_hash().as_bytes();
            bytes[0] ^= 0x01;
            let wrong = Hash::from_bytes(bytes);
            let bill = verify_bill(BUNDLE, &wrong).expect("the bundle parses");
            assert_eq!(bill.verdict, "fail");
        }

        #[test]
        fn malformed_json_is_an_error_not_a_verdict() {
            let hash = Hash::parse_hex(CONTENT_HASH).expect("64 hex");
            assert!(verify_bill("{ not json", &hash).is_err());
        }
    }
}

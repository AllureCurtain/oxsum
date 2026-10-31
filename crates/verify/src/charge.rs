//! Recomputing what a settlement entry's description charges.
//!
//! Inclusion proves the entry is in the ledger and unaltered — it says nothing
//! about whether the arithmetic inside it was ever right. A settlement
//! description is versioned (`"v"`) so this layer can recompute it: v2 carries
//! the metered usage, the priced lines it was charged from, and `charged`
//! alongside `freeze`, so the rule is one sentence — the charge is the ceiling
//! of the lines' summed cost, capped by the freeze.
//!
//! The wire shape is pinned here, deliberately not imported from `oxsum-core`:
//! the whole point of versioning is that this verifier still reads what today's
//! writer wrote after the writer has moved on. A record whose version this
//! build does not know is not an error — inclusion still holds — it only means
//! the charge cannot be recomputed, which the verdict names.

use serde_json::Value;

/// The newest description version this verifier recomputes. Every settlement
/// written before descriptions were versioned counts as older; every `v` above
/// this is newer than this build. Versions it does know each get their own
/// rule: an old bill stays recomputable after the writer has moved on.
const KNOWN_VERSION: i64 = 11;

/// The line items the input side prices: their units must account for the
/// whole `inputTokens`, between them and the usage record — cached reads and
/// both cache-write tiers are part of the input total (crates/core's usage
/// normalization). Everything a v3 record may name on the input side.
const INPUT_ITEMS: &[&str] = &["input", "cache_read", "cache_write_5m", "cache_write_1h"];

/// As [`INPUT_ITEMS`], for the output side: reasoning is part of `outputTokens`.
const OUTPUT_ITEMS: &[&str] = &["output", "reasoning"];

/// Minor units per priced million units: the denominator every v2 line shares.
const PER_MILLION: i128 = 1_000_000;

/// The settlement `kind`s a v2 record may carry — the priced outcomes. A
/// description whose `kind` is none of these (a hold, an adjustment, anything
/// else) is not a settlement at all, so there is nothing to recompute.
const SETTLEMENT_KINDS: &[&str] = &[
    "usage",
    "estimated",
    "client_cancelled",
    "upstream_error",
    "upstream_unreachable",
    "capped",
    "swept",
    "unpriced",
    "released",
    "refused",
];

/// What recomputing a settlement description's charge concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeCheck {
    /// The description is not a settlement's — a hold, an adjustment, an empty
    /// string — so there is no charge to recompute. Not a failure.
    NotASettlement,
    /// A settlement written before descriptions were versioned, or under a
    /// version older than this verifier's. Inclusion is all it gets.
    OlderSchema,
    /// A settlement written under a version newer than this verifier knows:
    /// inclusion is proven, the recompute is skipped rather than guessed.
    NewerSchema {
        /// The version the record named.
        version: i64,
    },
    /// A v2 settlement whose lines, usage and charge agree.
    Recomputed,
    /// A settlement claiming a version this verifier knows, but whose fields or
    /// arithmetic do not agree — not an honest write.
    Mismatch,
}

/// Recomputes a settlement description's charge from its own fields.
///
/// Dispatch is by the description's `v`: a known version is recomputed, a newer
/// one is left to inclusion, and a settlement without one predates versioning.
/// Anything that is not a settlement gets [`ChargeCheck::NotASettlement`].
#[must_use]
pub fn verify_charge(description: &str) -> ChargeCheck {
    let Ok(value) = serde_json::from_str::<Value>(description) else {
        return ChargeCheck::NotASettlement;
    };
    let is_settlement = value
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| SETTLEMENT_KINDS.contains(&kind));
    if !is_settlement {
        return ChargeCheck::NotASettlement;
    }
    match value.get("v") {
        // A settlement with no `v` predates versioning.
        None => ChargeCheck::OlderSchema,
        Some(version) => match version.as_i64() {
            Some(2) => recompute_v2(&value),
            Some(3) => recompute_v3(&value),
            Some(4) => recompute_v4(&value),
            Some(5) => recompute_v5(&value),
            Some(6) => recompute_v6(&value),
            Some(7) => recompute_v7(&value),
            Some(8) => recompute_v8(&value),
            Some(9) => recompute_v9(&value),
            Some(10) => recompute_v10(&value),
            Some(KNOWN_VERSION) => recompute_v11(&value),
            Some(v) if v > KNOWN_VERSION => ChargeCheck::NewerSchema { version: v },
            Some(_) => ChargeCheck::OlderSchema,
            // A `v` that names no version is a broken record, not an old one.
            None => ChargeCheck::Mismatch,
        },
    }
}

/// The v2 rule: `charged` is the lines' summed cost — units times price per
/// million, summed before the division, rounded up — capped at `freeze`.
///
/// A fraction of a minor unit still costs a minor unit, and the user never
/// pays more than the freeze: those are the two halves of the rule. The lines
/// itemised as `input` and `output` must also carry exactly the usage record's
/// totals — a line that counts other tokens than the usage describes is a
/// record that disagrees with itself. An item this verifier does not know is
/// not bound to a usage dimension but still joins the sum, so the arithmetic
/// stays checkable when the writer prices a dimension added later.
fn recompute_v2(value: &Value) -> ChargeCheck {
    let integer = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64);
    let Some(usage) = value.get("usage") else {
        return ChargeCheck::Mismatch;
    };
    // A zero count is not written — sparse fields read back as zero, but a
    // count that is present and not an integer is still wrong.
    let usage_count = |key: &str| match usage.get(key) {
        None => Some(0),
        Some(count) => count.as_i64(),
    };
    let (Some(input), Some(output)) = (usage_count("inputTokens"), usage_count("outputTokens"))
    else {
        return ChargeCheck::Mismatch;
    };
    let (Some(charged), Some(freeze)) = (integer(value, "charged"), integer(value, "freeze"))
    else {
        return ChargeCheck::Mismatch;
    };
    if input < 0 || output < 0 || charged < 0 || freeze < 0 {
        return ChargeCheck::Mismatch;
    }
    let Some(lines) = value.get("lines").and_then(Value::as_array) else {
        return ChargeCheck::Mismatch;
    };
    let mut numerator: i128 = 0;
    for line in lines {
        let (Some(units), Some(price)) = (integer(line, "units"), integer(line, "pricePerM"))
        else {
            return ChargeCheck::Mismatch;
        };
        if units < 0 || price < 0 {
            return ChargeCheck::Mismatch;
        }
        match line.get("item").and_then(Value::as_str) {
            Some("input") if units != input => return ChargeCheck::Mismatch,
            Some("output") if units != output => return ChargeCheck::Mismatch,
            _ => {}
        }
        numerator += i128::from(units) * i128::from(price);
    }
    let expected = ((numerator + PER_MILLION - 1) / PER_MILLION).min(i128::from(freeze));
    if i128::from(charged) == expected {
        ChargeCheck::Recomputed
    } else {
        ChargeCheck::Mismatch
    }
}

/// The v3 rule: the same ceiling-and-cap as v2, but the usage totals bind the
/// *sum* of each side's lines, and a line spells itself as the tuple
/// `[item, units, pricePerMillion]` — the ledger's description limit cannot
/// afford an object's repeated keys. The itemized price book splits `input`
/// into `input` plus `cache_read` and the cache-write tiers, and `output` into
/// `output` plus `reasoning`, so each side's lines must account for the whole
/// usage count — a line that counts other tokens than the usage describes is
/// a record that disagrees with itself. A bound item may appear once; an item
/// this verifier does not know is unbound and still joins the sum. A `request`
/// line is the flat fee: one per billed turn, zero on a turn nothing ran for.
/// The v3 rule: the same ceiling-and-cap as v2 over the itemized lines
/// [`checked_sum_v3`] binds — `ceil(sum / million)` capped at `freeze`.
fn recompute_v3(value: &Value) -> ChargeCheck {
    let Some((numerator, charged, freeze)) = checked_sum_v3(value) else {
        return ChargeCheck::Mismatch;
    };
    let expected = ((numerator + PER_MILLION - 1) / PER_MILLION).min(i128::from(freeze));
    if i128::from(charged) == expected {
        ChargeCheck::Recomputed
    } else {
        ChargeCheck::Mismatch
    }
}

/// The v4 rule: v3's line arithmetic and binding, then the multiplier the
/// description snapshots (issue #158). `discountPercent` scales the numerator
/// before the one ceiling — `ceil(sum * (100 - percent) / (million * 100))` —
/// and the cap on the undiscounted freeze still holds: a discount only ever
/// lowers a charge. Absent means none applied; a present value outside
/// 1..=100 is not an honest write.
fn recompute_v4(value: &Value) -> ChargeCheck {
    let percent = match value.get("discountPercent") {
        None => 0_i64,
        Some(percent) => match percent.as_i64() {
            Some(percent) if (1..=100).contains(&percent) => percent,
            _ => return ChargeCheck::Mismatch,
        },
    };
    let Some((numerator, charged, freeze)) = checked_sum_v3(value) else {
        return ChargeCheck::Mismatch;
    };
    let scaled = numerator * i128::from(100 - percent);
    let divisor = PER_MILLION * 100;
    let expected = ((scaled + divisor - 1) / divisor).min(i128::from(freeze));
    if i128::from(charged) == expected {
        ChargeCheck::Recomputed
    } else {
        ChargeCheck::Mismatch
    }
}

/// The v5 rule: v4's arithmetic unchanged, plus `upstreamAttempts` — how many
/// upstream calls the turn made under the one hold (issue #168). It is not a
/// price input, so it joins no sum: the rule is that an honest record carries
/// it as a positive integer, and nothing more is claimed about it. Every
/// writer at v9 or below relayed at least once — the one caller that may
/// record zero is a v10 cache hit, and its rule lives in `recompute_v10`
/// (issue #497).
fn recompute_v5(value: &Value) -> ChargeCheck {
    match value.get("upstreamAttempts").and_then(Value::as_i64) {
        Some(attempts) if attempts >= 1 => recompute_v4(value),
        _ => ChargeCheck::Mismatch,
    }
}

/// The v6 rule: a metered event's record (issue #172) — v4's arithmetic, with
/// the fields a turn no upstream served carries differently. `service` names
/// the reporting credential as a non-empty string, and `upstreamAttempts` is
/// absent rather than zero: nothing was relayed, so an honest write does not
/// even claim the count. A `released` record bills nothing, so its lines are
/// empty and its charge zero — the shared arithmetic checks both.
fn recompute_v6(value: &Value) -> ChargeCheck {
    if value.get("upstreamAttempts").is_some() {
        return ChargeCheck::Mismatch;
    }
    match value.get("service").and_then(Value::as_str) {
        Some(service) if !service.is_empty() => recompute_v4(value),
        _ => ChargeCheck::Mismatch,
    }
}

/// The v7 rule: a gateway turn under a spend mandate (issue #262) — v5's
/// arithmetic unchanged, plus the optional `mandate` object naming which
/// delegation authorized the charge. `mandate` is `{id, hash}` — the mandate's
/// own id and sha256 of the blob the delegate presented — and either is absent
/// or both are well-formed: `id` a UUID string, `hash` 64 lowercase hex. A
/// `mandate` that is not that shape is not an honest write.
fn recompute_v7(value: &Value) -> ChargeCheck {
    if let Some(mandate) = value.get("mandate") {
        let id_ok = mandate
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(is_uuid);
        let hash_ok = mandate
            .get("hash")
            .and_then(Value::as_str)
            .is_some_and(|h| {
                h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            });
        if !(id_ok && hash_ok) {
            return ChargeCheck::Mismatch;
        }
    }
    recompute_v5(value)
}

/// The v8 rule: a turn the caller asked for under an alias (issue #286) —
/// v7's arithmetic unchanged, plus the optional `upstreamModel` naming the
/// model the channel actually served while the record's `model` stays the
/// asked-for alias. It is metadata, not a price input: the rule is only that
/// a present `upstreamModel` is a non-empty string — anything else is not an
/// honest write.
fn recompute_v8(value: &Value) -> ChargeCheck {
    if let Some(upstream) = value.get("upstreamModel") {
        match upstream.as_str() {
            Some(name) if !name.is_empty() => {}
            _ => return ChargeCheck::Mismatch,
        }
    }
    recompute_v7(value)
}

/// The v9 rule: a turn whose hold drew on a subscription's quota (issue #496)
/// — v8's arithmetic unchanged, plus the optional `subscription` object naming
/// which entitlement covered the charge: `{subscriptionId, periodId,
/// drawnMinor, chargedMinor}`. Both ids are UUIDs and the amounts are
/// non-negative integers — `chargedMinor` never above `drawnMinor`, and both
/// zero only when the subscription covered a turn that settled for nothing.
/// A `subscription` that is not that shape is not an honest write.
fn recompute_v9(value: &Value) -> ChargeCheck {
    if let Some(subscription) = value.get("subscription") {
        let id_ok = subscription
            .get("subscriptionId")
            .and_then(Value::as_str)
            .is_some_and(is_uuid);
        let period_ok = subscription
            .get("periodId")
            .and_then(Value::as_str)
            .is_some_and(is_uuid);
        let drawn_ok = subscription
            .get("drawnMinor")
            .and_then(Value::as_i64)
            .is_some_and(|d| d >= 0);
        let charged_ok = subscription
            .get("chargedMinor")
            .and_then(Value::as_i64)
            .is_some_and(|c| c >= 0);
        let within = subscription
            .get("drawnMinor")
            .and_then(Value::as_i64)
            .zip(subscription.get("chargedMinor").and_then(Value::as_i64))
            .is_some_and(|(drawn, charged)| charged <= drawn);
        if !(id_ok && period_ok && drawn_ok && charged_ok && within) {
            return ChargeCheck::Mismatch;
        }
    }
    recompute_v8(value)
}

/// The v10 rule: a turn the semantic cache served (issue #497) — v9's
/// arithmetic unchanged, plus the optional `cache` object naming the entry
/// the answer was first produced under: `{sourceEntryId, similarity}`. The id
/// is a UUID — the link a verifier follows to the original turn's bill — and
/// the score is a number in `[0, 1]`: exact-digest hits write `1`, and a
/// future similarity search writes its real figure, never a borrowed
/// threshold. The fold a hit performs — the whole input repriced as
/// `cache_read` — needs no new rule: v3's binding already sums the input
/// items against `inputTokens`, which is what makes the hit's bill checkable
/// at all. A `cache` that is not that shape is not an honest write.
///
/// One field's rule loosens under v10: `upstreamAttempts` may be `0`, the
/// honest count for a hit that never called upstream. The arithmetic does
/// not read it, so a zero is checked as a non-negative integer here and the
/// rest of the record is verified by v9's chain — which a zero would fail,
/// since every earlier writer relayed at least once.
fn recompute_v10(value: &Value) -> ChargeCheck {
    if let Some(cache) = value.get("cache") {
        let id_ok = cache
            .get("sourceEntryId")
            .and_then(Value::as_str)
            .is_some_and(is_uuid);
        let score_ok = cache
            .get("similarity")
            .and_then(Value::as_f64)
            .is_some_and(|score| (0.0..=1.0).contains(&score));
        if !(id_ok && score_ok) {
            return ChargeCheck::Mismatch;
        }
    }
    match value.get("upstreamAttempts").and_then(Value::as_i64) {
        Some(attempts) if attempts >= 0 => {
            // The arithmetic chain: v9's fields, then v5's — the only step the
            // hit's zero count cannot take, so it is taken for it.
            let rewritten = match attempts {
                0 => {
                    let mut rewritten = value.clone();
                    rewritten["upstreamAttempts"] = Value::from(1);
                    rewritten
                }
                _ => value.clone(),
            };
            recompute_v9(&rewritten)
        }
        _ => ChargeCheck::Mismatch,
    }
}

/// The v11 rule: the guardrail pipeline's verdicts (issue #502) — v10's
/// arithmetic unchanged, plus the optional `guardrail` array naming each
/// policy that acted on the turn and how. Each entry is
/// `{policy, verdict, reason, streamed?}`: `policy` a configured name,
/// `verdict` `flagged` or `blocked`, `reason` the rule's own bounded text, and
/// `streamed` — present only when true — marks a post-call judgment a streamed
/// answer could not act on. The verdicts never change the charge — a `flagged`
/// turn settled normally, a `blocked` one's `charged: 0` is v10's own record
/// of the refund — so the rule checks their *shape* and lets the arithmetic
/// chain below them do the rest: a `guardrail` that is not that shape is not
/// an honest write.
fn recompute_v11(value: &Value) -> ChargeCheck {
    if let Some(guardrail) = value.get("guardrail") {
        let entries_ok = guardrail.as_array().is_some_and(|entries| {
            entries.iter().all(|entry| {
                entry.get("policy").and_then(Value::as_str).is_some()
                    && entry
                        .get("verdict")
                        .and_then(Value::as_str)
                        .is_some_and(|v| v == "flagged" || v == "blocked")
                    && entry.get("reason").and_then(Value::as_str).is_some()
                    && entry
                        .get("streamed")
                        .is_none_or(|s| s.as_bool() == Some(true))
            })
        });
        if !entries_ok {
            return ChargeCheck::Mismatch;
        }
    }
    recompute_v10(value)
}

/// A UUID in its canonical `8-4-4-4-12` hex spelling — validated by hand so the
/// verifier stays wasm32-clean without a `uuid` dependency.
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

/// The checks both itemized schemas share: the usage totals bind the *sum* of
/// each side's lines, and a line spells itself as the tuple
/// `[item, units, pricePerMillion]` — the ledger's description limit cannot
/// afford an object's repeated keys. The itemized price book splits `input`
/// into `input` plus `cache_read` and the cache-write tiers, and `output` into
/// `output` plus `reasoning`, so each side's lines must account for the whole
/// usage count — a line that counts other tokens than the usage describes is
/// a record that disagrees with itself. A bound item may appear once; an item
/// this verifier does not know is unbound and still joins the sum. A `request`
/// line is the flat fee: one per billed turn, zero on a turn nothing ran for.
///
/// Answers the summed numerator and the record's `charged`/`freeze`, or `None`
/// when any check failed — the callers map that to [`ChargeCheck::Mismatch`].
fn checked_sum_v3(value: &Value) -> Option<(i128, i64, i64)> {
    let integer = |value: &Value, key: &str| value.get(key).and_then(Value::as_i64);
    let usage = value.get("usage")?;
    let usage_count = |key: &str| match usage.get(key) {
        None => Some(0),
        Some(count) => count.as_i64(),
    };
    let (input, output) = (usage_count("inputTokens")?, usage_count("outputTokens")?);
    let (charged, freeze) = (integer(value, "charged")?, integer(value, "freeze")?);
    if input < 0 || output < 0 || charged < 0 || freeze < 0 {
        return None;
    }
    // A flat fee bills a turn that ran; a failed or swept one owes nothing.
    let billable = matches!(
        value.get("kind").and_then(Value::as_str),
        Some("usage" | "estimated" | "client_cancelled" | "capped" | "unpriced")
    );
    let lines = value.get("lines")?.as_array()?;
    let (mut input_sum, mut output_sum) = (0_i64, 0_i64);
    let mut bound_seen: u8 = 0;
    let mut numerator: i128 = 0;
    for line in lines {
        let item = line.get(0).and_then(Value::as_str);
        let (units, price) = (
            line.get(1).and_then(Value::as_i64)?,
            line.get(2).and_then(Value::as_i64)?,
        );
        if units < 0 || price < 0 {
            return None;
        }
        match item {
            Some(item) if INPUT_ITEMS.contains(&item) || OUTPUT_ITEMS.contains(&item) => {
                // A bound item twice is two writers' claims where one belongs.
                let bit = INPUT_ITEMS
                    .iter()
                    .chain(OUTPUT_ITEMS.iter())
                    .position(|known| *known == item)
                    .map(|index| 1_u8 << index)
                    .unwrap_or(0);
                if bound_seen & bit != 0 {
                    return None;
                }
                bound_seen |= bit;
                if INPUT_ITEMS.contains(&item) {
                    input_sum += units;
                } else {
                    output_sum += units;
                }
            }
            Some("request") if units > i64::from(billable) => return None,
            _ => {}
        }
        numerator += i128::from(units) * i128::from(price);
    }
    if input_sum != input || output_sum != output {
        return None;
    }
    Some((numerator, charged, freeze))
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A settled turn as the gateway wrote it under v2: 116 input at 1 credit
    /// per million, 100 output at 2, ceiling 316 minor under a 400 freeze.
    const V2: &str = r#"{"v":2,"request":"req-abc","channel":"deepseek","model":"deepseek-chat","priceVersion":3,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":40},"lines":[{"item":"input","units":116,"pricePerM":1000000},{"item":"output","units":100,"pricePerM":2000000}],"charged":316,"freeze":400}"#;

    /// The same turn under v3, itemized: 20 fresh input at 1 credit per million,
    /// 96 cached at a tenth of that, 80 output at 2 plus 20 reasoning at 3 — one
    /// ceiling over the summed cost: 20 + 9.6 + 160 + 60 = 249.6 → 250.
    const V3: &str = r#"{"v":3,"request":"req-abc","channel":"deepseek","model":"deepseek-chat","priceVersion":4,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":96,"reasoningTokens":20},"matchedRule":{"minInputTokens":100},"lines":[["input",20,1000000],["cache_read",96,100000],["output",80,2000000],["reasoning",20,3000000]],"charged":250,"freeze":400}"#;

    #[test]
    fn a_genuine_v2_record_recomputes() {
        assert_eq!(verify_charge(V2), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_genuine_v3_record_recomputes() {
        assert_eq!(verify_charge(V3), ChargeCheck::Recomputed);
        // And a wrong total is a mismatch, not a pass.
        let wrong = V3.replacen(r#""charged":250"#, r#""charged":300"#, 1);
        assert_eq!(verify_charge(&wrong), ChargeCheck::Mismatch);
    }

    /// The V3 turn at v4 with a 10% discount: the numerator scales by 90 before
    /// the one ceiling — 249.6 × 0.9 = 224.64 → 225 (issue #158).
    const V4: &str = r#"{"v":4,"request":"req-abc","channel":"deepseek","model":"deepseek-chat","priceVersion":4,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":96,"reasoningTokens":20},"matchedRule":{"minInputTokens":100},"lines":[["input",20,1000000],["cache_read",96,100000],["output",80,2000000],["reasoning",20,3000000]],"discountPercent":10,"charged":225,"freeze":400}"#;

    #[test]
    fn a_genuine_v4_record_recomputes() {
        assert_eq!(verify_charge(V4), ChargeCheck::Recomputed);
        // The same turn without a discount is the v3 sum.
        let plain = V4.replacen(r#""discountPercent":10,"#, "", 1).replacen(
            r#""charged":225"#,
            r#""charged":250"#,
            1,
        );
        assert_eq!(verify_charge(&plain), ChargeCheck::Recomputed);
        // A hundred-percent discount settles at zero — recorded, not unpriced.
        let free = V4
            .replacen(r#""discountPercent":10"#, r#""discountPercent":100"#, 1)
            .replacen(r#""charged":225"#, r#""charged":0"#, 1);
        assert_eq!(verify_charge(&free), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_v4_record_binds_the_discount() {
        // The undiscounted total under a claimed discount is a mismatch.
        let undiscounted = V4.replacen(r#""charged":225"#, r#""charged":250"#, 1);
        assert_eq!(verify_charge(&undiscounted), ChargeCheck::Mismatch);
        // And so is a percent the rule does not admit.
        for percent in ["0", "101", "-5", "\"twenty\""] {
            let bad = V4.replacen(
                r#""discountPercent":10"#,
                &format!("\"discountPercent\":{percent}"),
                1,
            );
            assert_eq!(verify_charge(&bad), ChargeCheck::Mismatch, "{percent}");
        }
        // A discount never lifts the freeze cap: 1% off 249.6e6 → 248, still
        // over a 240 freeze, so the freeze is what was charged.
        let capped = V4
            .replacen(r#""discountPercent":10"#, r#""discountPercent":1"#, 1)
            .replacen(r#""charged":225"#, r#""charged":240"#, 1)
            .replacen(r#""freeze":400"#, r#""freeze":240"#, 1);
        assert_eq!(verify_charge(&capped), ChargeCheck::Recomputed);
    }

    /// The V4 turn at v5 after a failover: `upstreamAttempts` counts the calls
    /// under the one hold without entering the arithmetic (issue #168).
    const V5: &str = r#"{"v":5,"request":"req-abc","channel":"backup","model":"deepseek-chat","priceVersion":2,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":96,"reasoningTokens":20},"lines":[["input",20,1000000],["cache_read",96,100000],["output",80,2000000],["reasoning",20,3000000]],"upstreamAttempts":2,"charged":250,"freeze":400}"#;

    #[test]
    fn a_genuine_v5_record_recomputes() {
        assert_eq!(verify_charge(V5), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_v5_record_binds_the_attempt_count() {
        // Missing, zero or non-integer attempt counts are not honest v5 writes.
        for field in [
            r#""upstreamAttempts":0,"#,
            r#""upstreamAttempts":-1,"#,
            r#""upstreamAttempts":"two","#,
            "",
        ] {
            let bad = V5.replacen(r#""upstreamAttempts":2,"#, field, 1);
            assert_eq!(verify_charge(&bad), ChargeCheck::Mismatch, "{field}");
        }
        // And the count is metadata, not a price input: changing it changes
        // nothing about the charge.
        let third = V5.replacen(r#""upstreamAttempts":2"#, r#""upstreamAttempts":3"#, 1);
        assert_eq!(verify_charge(&third), ChargeCheck::Recomputed);
    }

    /// A metered event's record (issue #172): `service` names the reporting
    /// credential and `upstreamAttempts` is absent — nothing was relayed. 116
    /// input at 1 credit per million is the whole charge under a 400 freeze.
    const V6: &str = r#"{"v":6,"request":"evt-9f3a","service":"mailer","channel":"internal","model":"email.send","priceVersion":1,"kind":"usage","usage":{"inputTokens":116,"eventType":"email.send"},"lines":[["input",116,1000000]],"charged":116,"freeze":400}"#;

    /// A hold the reporting service let go: no usage, no lines, no charge.
    const V6_RELEASED: &str = r#"{"v":6,"request":"evt-9f3a","service":"mailer","channel":"internal","model":"email.send","priceVersion":1,"kind":"released","usage":{},"lines":[],"charged":0,"freeze":400}"#;

    #[test]
    fn a_genuine_v6_record_recomputes() {
        assert_eq!(verify_charge(V6), ChargeCheck::Recomputed);
        assert_eq!(verify_charge(V6_RELEASED), ChargeCheck::Recomputed);
        // And a wrong total is a mismatch, not a pass.
        let wrong = V6.replacen(r#""charged":116"#, r#""charged":200"#, 1);
        assert_eq!(verify_charge(&wrong), ChargeCheck::Mismatch);
    }

    #[test]
    fn a_v6_record_names_its_reporter_and_calls_no_upstream() {
        // `service` missing, empty or not a string is not an honest v6 write.
        for field in [r#""service":"","#, r#""service":7,"#, ""] {
            let bad = V6.replacen(r#""service":"mailer","#, field, 1);
            assert_eq!(verify_charge(&bad), ChargeCheck::Mismatch, "{field}");
        }
        // `upstreamAttempts` does not belong on a turn nothing relayed — even
        // a zero claim is a v6 violation, where a missing claim is not.
        let claimed = V6.replacen(
            r#""charged":116"#,
            r#""upstreamAttempts":0,"charged":116"#,
            1,
        );
        assert_eq!(verify_charge(&claimed), ChargeCheck::Mismatch);
    }

    #[test]
    fn a_released_record_bills_nothing() {
        // A released record that charges is not honest.
        let charged = V6_RELEASED.replacen(r#""charged":0"#, r#""charged":5"#, 1);
        assert_eq!(verify_charge(&charged), ChargeCheck::Mismatch);
        // And a request line cannot bill on a record that billed nothing.
        let fee = V6_RELEASED.replacen(r#""lines":[]"#, r#""lines":[["request",1,500000000]]"#, 1);
        assert_eq!(verify_charge(&fee), ChargeCheck::Mismatch);
    }

    /// The V5 turn at v7 under a spend mandate (issue #262): `mandate` names the
    /// delegation `{id, hash}` and the arithmetic is v5's — 250 under the 400
    /// freeze.
    const V7: &str = r#"{"v":7,"request":"req-abc","channel":"backup","model":"deepseek-chat","priceVersion":2,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":96,"reasoningTokens":20},"lines":[["input",20,1000000],["cache_read",96,100000],["output",80,2000000],["reasoning",20,3000000]],"upstreamAttempts":1,"mandate":{"id":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","hash":"a1b2c3d4e5f60718293a4b5c6d7e8f901234567890abcdef0123456789abcdef"},"charged":250,"freeze":400}"#;

    #[test]
    fn a_genuine_v7_record_recomputes() {
        assert_eq!(verify_charge(V7), ChargeCheck::Recomputed);
        // A turn no mandate was presented for is still an honest v7 write.
        let plain = V7.replacen(
            r#""mandate":{"id":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","hash":"a1b2c3d4e5f60718293a4b5c6d7e8f901234567890abcdef0123456789abcdef"},"#,
            "",
            1,
        );
        assert_eq!(verify_charge(&plain), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_v7_record_binds_the_mandate_shape() {
        // A `mandate` that is not `{id: uuid, hash: 64-hex}` is not an honest
        // write — the wrong id, a short hash, an uppercase hex digit.
        for field in [
            r#""mandate":{"id":"not-a-uuid","hash":"a1b2c3d4e5f60718293a4b5c6d7e8f901234567890abcdef0123456789abcdef"},"#,
            r#""mandate":{"id":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","hash":"abc"},"#,
            r#""mandate":{"id":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","hash":"A1B2C3D4E5F60718293A4B5C6D7E8F901234567890ABCDEF0123456789ABCDEF"},"#,
            r#""mandate":{"id":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c"},"#,
            r#""mandate":null,"#,
        ] {
            let bad = V7.replacen(
                r#""mandate":{"id":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","hash":"a1b2c3d4e5f60718293a4b5c6d7e8f901234567890abcdef0123456789abcdef"},"#,
                field,
                1,
            );
            assert_eq!(verify_charge(&bad), ChargeCheck::Mismatch, "{field}");
        }
    }

    /// The V7 turn at v8 answered under an alias (issue #286): `model` is the
    /// public name the caller asked for, `upstreamModel` what the channel
    /// served, and the arithmetic is v7's — 250 under the 400 freeze.
    const V8: &str = r#"{"v":8,"request":"req-abc","channel":"backup","model":"public-name","upstreamModel":"deepseek-chat","priceVersion":2,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":96,"reasoningTokens":20},"lines":[["input",20,1000000],["cache_read",96,100000],["output",80,2000000],["reasoning",20,3000000]],"upstreamAttempts":1,"charged":250,"freeze":400}"#;

    #[test]
    fn a_genuine_v8_record_recomputes() {
        assert_eq!(verify_charge(V8), ChargeCheck::Recomputed);
        // A turn that asked under the served name writes no `upstreamModel`
        // and is still an honest v8 write.
        let plain = V8.replacen(r#""upstreamModel":"deepseek-chat","#, "", 1);
        assert_eq!(verify_charge(&plain), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_v8_record_binds_the_upstream_model_shape() {
        // A present `upstreamModel` that is empty, not a string, or null is
        // not an honest write — only absence and a non-empty string are.
        for field in [
            r#""upstreamModel":"","#,
            r#""upstreamModel":7,"#,
            r#""upstreamModel":null,"#,
        ] {
            let bad = V8.replacen(r#""upstreamModel":"deepseek-chat","#, field, 1);
            assert_eq!(verify_charge(&bad), ChargeCheck::Mismatch, "{field}");
        }
        // And the name is metadata, not a price input: the arithmetic does
        // not read it, so a different honest claim still recomputes.
        let renamed = V8.replacen(
            r#""upstreamModel":"deepseek-chat""#,
            r#""upstreamModel":"deepseek-v3""#,
            1,
        );
        assert_eq!(verify_charge(&renamed), ChargeCheck::Recomputed);
    }

    /// The v9 record: v8's arithmetic with a `subscription` block naming the
    /// entitlement the hold drew on (issue #496).
    const V9: &str = r#"{"v":9,"request":"req-abc","channel":"backup","model":"deepseek-chat","priceVersion":2,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":96,"reasoningTokens":20},"lines":[["input",20,1000000],["cache_read",96,100000],["output",80,2000000],["reasoning",20,3000000]],"upstreamAttempts":1,"charged":250,"freeze":400,"subscription":{"subscriptionId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","periodId":"2ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","drawnMinor":400,"chargedMinor":250}}"#;

    #[test]
    fn a_genuine_v9_record_recomputes() {
        assert_eq!(verify_charge(V9), ChargeCheck::Recomputed);
        // A turn the wallet alone paid writes no `subscription` and is still
        // an honest v9 write.
        let plain = V9.replacen(
            r#""subscription":{"subscriptionId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","periodId":"2ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","drawnMinor":400,"chargedMinor":250}}"#,
            "}",
            1,
        );
        let plain = plain.replacen(r#""freeze":400,"#, r#""freeze":400"#, 1);
        assert_eq!(verify_charge(&plain), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_v9_record_binds_the_subscription_shape() {
        // A `chargedMinor` above `drawnMinor` claims the subscription paid
        // more than its hold drew — not an honest write.
        let over = V9.replacen(r#""chargedMinor":250"#, r#""chargedMinor":401"#, 1);
        assert_eq!(verify_charge(&over), ChargeCheck::Mismatch);
        // An id that is not a UUID, or an amount that is negative, is not an
        // honest write either.
        let bad_id = V9.replacen(
            r#""subscriptionId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c""#,
            r#""subscriptionId":"nope""#,
            1,
        );
        assert_eq!(verify_charge(&bad_id), ChargeCheck::Mismatch);
        let bad_period = V9.replacen(
            r#""periodId":"2ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c""#,
            r#""periodId":7"#,
            1,
        );
        assert_eq!(verify_charge(&bad_period), ChargeCheck::Mismatch);
        let negative = V9.replacen(r#""drawnMinor":400"#, r#""drawnMinor":-1"#, 1);
        assert_eq!(verify_charge(&negative), ChargeCheck::Mismatch);
    }

    /// The v10 record: v9's arithmetic with a `cache` block naming the entry
    /// the answer was first produced under (issue #497). The turn itself is a
    /// cache hit: the whole 116-token input repriced as `cache_read`, which is
    /// why the input lines split 0 + 116.
    const V10: &str = r#"{"v":10,"request":"req-hit","channel":"backup","model":"deepseek-chat","priceVersion":2,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100,"cachedTokens":116,"reasoningTokens":20},"lines":[["input",0,1000000],["cache_read",116,100000],["output",80,2000000],["reasoning",20,3000000]],"upstreamAttempts":0,"charged":232,"freeze":400,"cache":{"sourceEntryId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","similarity":1}}"#;

    #[test]
    fn a_genuine_v10_record_recomputes() {
        // 0 fresh input + 116 cached at a tenth of the input rate + 80 output
        // at 2 + 20 reasoning at 3: 11.6 + 160 + 60 = 231.6 → 232.
        assert_eq!(verify_charge(V10), ChargeCheck::Recomputed);
        // A fresh turn under a v10 writer carries no `cache` and still
        // recomputes — the block is only on a hit's bill.
        let plain = V10.replacen(
            r#""cache":{"sourceEntryId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","similarity":1}}"#,
            "}",
            1,
        );
        let plain = plain.replacen(r#""freeze":400,"#, r#""freeze":400"#, 1);
        assert_eq!(verify_charge(&plain), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_v10_record_binds_the_cache_shape() {
        // A `cache` without a UUID source or with an out-of-range score is not
        // an honest write — and `upstreamAttempts: 0` is the hit's honest
        // claim, since nothing went upstream.
        for field in [
            r#""cache":{"sourceEntryId":"not-a-uuid","similarity":1}}"#,
            r#""cache":{"sourceEntryId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","similarity":1.5}}"#,
            r#""cache":{"sourceEntryId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c"}}"#,
            r#""cache":null}"#,
        ] {
            let bad = V10.replacen(
                r#""cache":{"sourceEntryId":"1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c","similarity":1}}"#,
                field,
                1,
            );
            assert_eq!(verify_charge(&bad), ChargeCheck::Mismatch, "{field}");
        }
    }

    #[test]
    fn a_v3_record_must_account_for_the_whole_usage() {
        // The input lines split 116 into 20 + 96; claim only part of it and the
        // record disagrees with itself.
        let short = V3.replacen(r#"["cache_read",96"#, r#"["cache_read",90"#, 1);
        assert_eq!(verify_charge(&short), ChargeCheck::Mismatch);
        // The same bound item twice is two claims where one belongs.
        let dup = V3.replacen(
            r#"["cache_read",96,100000]"#,
            r#"["cache_read",48,100000],["cache_read",48,100000]"#,
            1,
        );
        assert_eq!(verify_charge(&dup), ChargeCheck::Mismatch);
    }

    #[test]
    fn the_flat_fee_counts_once_and_only_when_billed() {
        let flat = r#"{"v":3,"kind":"usage","usage":{"inputTokens":0,"outputTokens":0},"lines":[["input",0,1000000],["output",0,2000000],["request",1,500000000]],"charged":500,"freeze":4000}"#;
        assert_eq!(verify_charge(flat), ChargeCheck::Recomputed);
        // Two requests' fee for one turn is a lie the sum alone would pass.
        let doubled = flat
            .replacen(
                r#"["request",1,500000000]"#,
                r#"["request",2,500000000]"#,
                1,
            )
            .replacen(r#""charged":500"#, r#""charged":1000"#, 1);
        assert_eq!(verify_charge(&doubled), ChargeCheck::Mismatch);
        // A swept turn carries the line at zero — the rate is still on record.
        let swept = r#"{"v":3,"kind":"swept","usage":{},"lines":[["input",0,1000000],["output",0,2000000],["request",0,500000000]],"charged":0,"freeze":4000}"#;
        assert_eq!(verify_charge(swept), ChargeCheck::Recomputed);
    }

    /// A turn that ran but carried a dimension the book cannot bill: the
    /// computable part is charged, the flat fee still counts once, and the
    /// unpriced dimension sits in the usage the lines do not claim — the
    /// record stays self-consistent for what it charged (issue #110).
    #[test]
    fn an_unpriced_turn_recomputes_what_it_charged() {
        // 100 input at 1 + 20 output at 2 + the 500 flat fee: 640 minor, and
        // `audioInputTokens` in the usage is the flagged unpriced part.
        let unpriced = r#"{"v":3,"kind":"unpriced","usage":{"inputTokens":100,"outputTokens":20,"audioInputTokens":60},"lines":[["input",100,1000000],["output",20,2000000],["request",1,500000000]],"charged":640,"freeze":4000}"#;
        assert_eq!(verify_charge(unpriced), ChargeCheck::Recomputed);
        // The unpriced flag does not license the flat fee twice either.
        let doubled = unpriced
            .replacen(
                r#"["request",1,500000000]"#,
                r#"["request",2,500000000]"#,
                1,
            )
            .replacen(r#""charged":640"#, r#""charged":1140"#, 1);
        assert_eq!(verify_charge(&doubled), ChargeCheck::Mismatch);
    }

    #[test]
    fn a_fraction_of_a_minor_unit_still_costs_one() {
        // Two half-minor components sum to exactly one: the single ceiling over the
        // combined numerator, not two ceilings that would charge two.
        let half = r#"{"v":2,"kind":"estimated","usage":{"inputTokens":1,"outputTokens":1},"lines":[{"item":"input","units":1,"pricePerM":500000},{"item":"output","units":1,"pricePerM":500000}],"charged":1,"freeze":9}"#;
        assert_eq!(verify_charge(half), ChargeCheck::Recomputed);
    }

    #[test]
    fn the_freeze_caps_the_charge() {
        // What the usage would cost is past the freeze, so the freeze is what was
        // charged — the capped settlement's promise.
        let capped = r#"{"v":2,"kind":"capped","usage":{"inputTokens":900,"outputTokens":0},"lines":[{"item":"input","units":900,"pricePerM":1000000}],"charged":400,"freeze":400}"#;
        assert_eq!(verify_charge(capped), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_zero_charge_turn_recomputes_too() {
        // Swept, upstream_error and upstream_unreachable settle at zero usage —
        // which writes no dimensions at all, since zeros are sparse.
        for kind in ["swept", "upstream_error", "upstream_unreachable"] {
            let swept = format!(
                r#"{{"v":2,"kind":"{kind}","usage":{{}},"lines":[{{"item":"input","units":0,"pricePerM":5}},{{"item":"output","units":0,"pricePerM":7}}],"charged":0,"freeze":400}}"#
            );
            assert_eq!(verify_charge(&swept), ChargeCheck::Recomputed, "{kind}");
        }
    }

    #[test]
    fn a_changed_figure_is_a_mismatch() {
        for (from, to) in [
            (r#""charged":316"#, r#""charged":315"#),
            (r#""charged":316"#, r#""charged":317"#),
            (r#""units":116"#, r#""units":117"#),
            (r#""pricePerM":1000000"#, r#""pricePerM":1000001"#),
            (r#""inputTokens":116"#, r#""inputTokens":117"#),
            (r#""freeze":400"#, r#""freeze":315"#),
        ] {
            let tampered = V2.replacen(from, to, 1);
            assert_ne!(tampered, V2);
            assert_eq!(verify_charge(&tampered), ChargeCheck::Mismatch, "{from}");
        }
    }

    #[test]
    fn an_unbound_item_still_joins_the_sum() {
        // A line naming a dimension this verifier does not know is summed: the
        // arithmetic is still the record's own.
        let extra = r#"{"v":2,"kind":"usage","usage":{"inputTokens":0,"outputTokens":0},"lines":[{"item":"cache_read","units":2,"pricePerM":1000000}],"charged":2,"freeze":9}"#;
        assert_eq!(verify_charge(extra), ChargeCheck::Recomputed);
    }

    #[test]
    fn a_field_that_is_wrong_is_a_mismatch_not_an_old_record() {
        let missing_lines = r#"{"v":2,"kind":"usage","usage":{"inputTokens":0,"outputTokens":0},"charged":0,"freeze":9}"#;
        assert_eq!(verify_charge(missing_lines), ChargeCheck::Mismatch);
        let bad_v = r#"{"v":"two","kind":"usage","charged":0,"freeze":9}"#;
        assert_eq!(verify_charge(bad_v), ChargeCheck::Mismatch);
        let negative = r#"{"v":2,"kind":"usage","usage":{"inputTokens":-1,"outputTokens":0},"lines":[],"charged":0,"freeze":9}"#;
        assert_eq!(verify_charge(negative), ChargeCheck::Mismatch);
    }

    #[test]
    fn what_predates_versioning_is_older_not_wrong() {
        // The flat v1 record: a settlement kind but no `v`.
        let v1 = r#"{"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","inputTokens":1,"outputTokens":1,"inputPrice":1,"outputPrice":1,"charged":1,"freeze":1}"#;
        assert_eq!(verify_charge(v1), ChargeCheck::OlderSchema);
        let v_named_1 = r#"{"v":1,"kind":"usage","charged":1,"freeze":1}"#;
        assert_eq!(verify_charge(v_named_1), ChargeCheck::OlderSchema);
    }

    #[test]
    fn a_newer_record_is_left_to_inclusion() {
        let future = r#"{"v":11,"kind":"usage","charged":1,"freeze":1}"#;
        assert_eq!(
            verify_charge(future),
            ChargeCheck::NewerSchema { version: 11 }
        );
    }

    #[test]
    fn what_is_not_a_settlement_is_not_checked() {
        assert_eq!(verify_charge(""), ChargeCheck::NotASettlement);
        assert_eq!(verify_charge("not json"), ChargeCheck::NotASettlement);
        assert_eq!(verify_charge("{}"), ChargeCheck::NotASettlement);
        let hold = r#"{"request":"a","model":"m","freeze":400,"kind":"hold"}"#;
        assert_eq!(verify_charge(hold), ChargeCheck::NotASettlement);
        // Even a `v` on a non-settlement kind does not make it one.
        let odd = r#"{"v":2,"kind":"hold","charged":0,"freeze":1}"#;
        assert_eq!(verify_charge(odd), ChargeCheck::NotASettlement);
    }
}

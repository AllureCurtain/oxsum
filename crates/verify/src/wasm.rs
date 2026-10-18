//! The browser bindings: the same `verify_*` checks the server runs, exported
//! to JavaScript over `wasm-bindgen` so the `/verify` page can run the proof
//! math on the client's own machine — the verifier's byte code travels to the
//! user instead of the user trusting a verdict the server rendered (issue
//! #284).
//!
//! ## The wire shape
//!
//! Every export takes and returns JSON strings rather than `JsValue` /
//! `serde-wasm-bindgen` / `tsify` objects: the page `JSON.parse`s once, the
//! signatures stay `&str → String`, and no second dependency — and no generated
//! TypeScript — enters the chain. The types crossing the boundary are the same
//! wire structs the Rust callers pass (`ProofBundle`, `WitnessNote`,
//! `WitnessPin`, `TreeHead`, `StatementAnchorCheck`); only the wrapper is a
//! string.
//!
//! `JsError` carries what the function would have returned as `Err`: the
//! `serde_json` message on malformed input, or the error variant's `Display`
//! (`HeadError`, `PriceError`, `AnchorError`, `UsageReportError`) on a verdict
//! that fails *because* of a real defect — the page surfaces the same sentence
//! the server-rendered verdict would have printed.
//!
//! ## No panics across the boundary
//!
//! A `Hash::parse_hex` failure or a malformed field returns `JsError` — the
//! page shows the parse error, not a trapping wasm abort. Nothing here calls
//! `unwrap` or `expect`: an input the user pastes can be arbitrarily wrong and
//! the export must answer for it.

use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::{
    AnchorError, Hash, PriceError, PublishedKey, StatementAnchorCheck, TreeHead, UsageReportError,
    WitnessNote, WitnessPin, WitnessVerdict, verify_bill, verify_price_head, verify_signed_head,
    verify_statement_anchor, verify_usage_report, verify_witnesses,
};

/// The JSON shape every exported verdict shares: `{"ok": bool, "detail":
/// string}`. `detail` is the human-readable reason — the same `Display` text a
/// server-rendered page prints — so the JavaScript side only has to render it.
fn verdict(ok: bool, detail: impl Into<String>) -> String {
    serde_json::json!({ "ok": ok, "detail": detail.into() }).to_string()
}

/// One witness's verdict in the JSON `verify_witnesses` answers.
fn witness_verdict(verdict: &WitnessVerdict) -> (bool, String) {
    match verdict {
        WitnessVerdict::Verified { .. } => (true, verdict.to_string()),
        _ => (false, verdict.to_string()),
    }
}

/// The content hash the page's input field holds, parsed — a shared helper so
/// every export reports the same message for a hash that is not 64 lowercase
/// hex.
fn expected_hash(hex: &str) -> Result<Hash, JsError> {
    Hash::parse_hex(hex.trim())
        .map_err(|_| JsError::new("the content hash is not 64 hex characters"))
}

/// Verifies a bill's proof bundle and — when it is a settlement the build
/// knows — recomputes the charge and the signed price binding inside it.
///
/// `bundle_json` is the `ProofBundle` (or its `{"data": …}` envelope).
/// `expected_hash_hex` is the 64-character content hash the user recorded with
/// the bill. Answers `{"ok", "detail", "charge", "price"}`, where `charge` and
/// `price` carry the same sentences the server-rendered verdict appends.
///
/// # Errors
///
/// [`JsError`] when the bundle or the hash does not parse.
#[wasm_bindgen(js_name = verifyBundle)]
pub fn verify_bundle_js(bundle_json: &str, expected_hash_hex: &str) -> Result<String, JsError> {
    // The proof API wraps the bundle in a `data` envelope; accept either, the
    // same unwrapping the server-rendered page does.
    let bundle_json = bundle_json.trim();
    let value: serde_json::Value = serde_json::from_str(bundle_json)
        .map_err(|_| JsError::new("the bundle did not parse as JSON"))?;
    let inner = value
        .get("data")
        .cloned()
        .filter(|data| data.get("entry").is_some() || data.get("subsetHash").is_some())
        .map(|data| data.to_string())
        .unwrap_or_else(|| bundle_json.to_owned());

    let hash = expected_hash(expected_hash_hex)?;

    // Inclusion, the charge recompute and the price binding all compose
    // through `verify_bill` — the same call the server-rendered page makes,
    // so both sides of the trust boundary print identical sentences.
    let bill =
        verify_bill(&inner, &hash).map_err(|_| JsError::new("the bundle did not parse as JSON"))?;
    Ok(serde_json::json!({
        "ok": bill.verdict == "pass",
        "verdict": bill.verdict,
        "charge": bill.charge,
        "price": bill.price,
    })
    .to_string())
}

/// Verifies the operator's signature over a tree head: the note parses, names
/// `expected_origin`, attests `served_json`'s head, and carries a signature the
/// `PublishedKey` verifies — the same `verify_signed_head` the server runs.
///
/// `served_json` is a `TreeHead`; `key_json` is
/// `{keyName, publicKey, keyHash}` with the hex fields as served.
///
/// # Errors
///
/// [`JsError`] on malformed input or a failed signature check — the `HeadError`
/// text.
#[wasm_bindgen(js_name = verifySignedHead)]
pub fn verify_signed_head_js(
    note: &str,
    served_json: &str,
    expected_origin: &str,
    key_json: &str,
) -> Result<String, JsError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Key {
        key_name: String,
        public_key: String,
        key_hash: String,
    }
    let served: TreeHead =
        serde_json::from_str(served_json).map_err(|e| JsError::new(&e.to_string()))?;
    let key: Key = serde_json::from_str(key_json).map_err(|e| JsError::new(&e.to_string()))?;
    let key = PublishedKey {
        key_name: &key.key_name,
        public_key: &key.public_key,
        key_hash: &key.key_hash,
    };
    match verify_signed_head(note, &served, expected_origin, &key) {
        Ok(()) => Ok(verdict(true, "the signed head verifies")),
        Err(error) => Err(JsError::new(&error.to_string())),
    }
}

/// Verifies every witness cosignature on a checkpoint: `notes_json` is
/// `[{witnessUrl, note}]`, `pins_json` `[{witnessUrl, keyName, publicKey,
/// keyHash}]`, `head_json` the `TreeHead` they claim to vouch. Answers a JSON
/// array `[{witnessUrl, ok, detail}]` in the notes' own order.
///
/// # Errors
///
/// [`JsError`] on malformed input — a verdict that fails reports in-band, not
/// as an exception.
#[wasm_bindgen(js_name = verifyWitnesses)]
pub fn verify_witnesses_js(
    notes_json: &str,
    pins_json: &str,
    expected_origin: &str,
    head_json: &str,
) -> Result<String, JsError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Note {
        witness_url: String,
        note: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Pin {
        witness_url: String,
        key_name: String,
        public_key: String,
        key_hash: String,
    }
    let notes: Vec<Note> =
        serde_json::from_str(notes_json).map_err(|e| JsError::new(&e.to_string()))?;
    let pins: Vec<Pin> =
        serde_json::from_str(pins_json).map_err(|e| JsError::new(&e.to_string()))?;
    let head: TreeHead =
        serde_json::from_str(head_json).map_err(|e| JsError::new(&e.to_string()))?;
    let notes: Vec<WitnessNote<'_>> = notes
        .iter()
        .map(|n| WitnessNote {
            witness_url: &n.witness_url,
            note: &n.note,
        })
        .collect();
    let pins: Vec<WitnessPin<'_>> = pins
        .iter()
        .map(|p| WitnessPin {
            witness_url: &p.witness_url,
            key_name: &p.key_name,
            public_key: &p.public_key,
            key_hash: &p.key_hash,
        })
        .collect();
    let verdicts = verify_witnesses(&notes, &pins, expected_origin, &head);
    let out: Vec<serde_json::Value> = verdicts
        .iter()
        .map(|(url, v)| {
            let (ok, detail) = witness_verdict(v);
            serde_json::json!({ "witnessUrl": url, "ok": ok, "detail": detail })
        })
        .collect();
    Ok(serde_json::json!(out).to_string())
}

/// Verifies a statement's log anchor: `json` is the detail body
/// `{statement, lines, anchor}` and `check_json` the caller-supplied
/// `{windowHashes, expectedHead, expectedOrigin}` — the same pieces
/// `StatementAnchorCheck` names.
///
/// # Errors
///
/// [`JsError`] on malformed input or a failed check — the `AnchorError` text.
#[wasm_bindgen(js_name = verifyStatementAnchor)]
pub fn verify_statement_anchor_js(json: &str, check_json: &str) -> Result<String, JsError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Check {
        window_hashes: Vec<String>,
        expected_head: TreeHead,
        expected_origin: String,
    }
    let check: Check =
        serde_json::from_str(check_json).map_err(|e| JsError::new(&e.to_string()))?;
    let window: Vec<&str> = check.window_hashes.iter().map(String::as_str).collect();
    let check = StatementAnchorCheck {
        window_hashes: &window,
        expected_head: &check.expected_head,
        expected_origin: &check.expected_origin,
    };
    match verify_statement_anchor(json, &check) {
        Ok(()) => Ok(verdict(true, "the statement anchor verifies")),
        Err(AnchorError::Absent) => Ok(verdict(false, AnchorError::Absent.to_string())),
        Err(error) => Err(JsError::new(&error.to_string())),
    }
}

/// Verifies a signed end-user usage report (issue #261): `json` is the `data`
/// object `GET …/usage-reports` answers; `key_json` is the published key
/// `{keyName, publicKey, keyHash}` the caller holds — or `null` to check
/// membership without a signature. `expected_end_user` names who the report
/// should be for.
///
/// # Errors
///
/// [`JsError`] on malformed input or a failed check — the `UsageReportError`
/// text.
#[wasm_bindgen(js_name = verifyUsageReport)]
pub fn verify_usage_report_js(
    json: &str,
    expected_end_user: &str,
    key_json: Option<String>,
) -> Result<String, JsError> {
    // The origin is named inside the signed note, not supplied by the caller —
    // the report names its own ledger, so a report for another tenant answers
    // under its own origin rather than silently inheriting this one.
    let origin = serde_json::from_str::<serde_json::Value>(json)
        .map_err(|_| JsError::new("the report did not parse as JSON"))?
        .get("note")
        .and_then(|v| v.as_str())
        .and_then(|note| doubleentry::witness::SignedTreeHead::parse(note).ok())
        .map(|parsed| parsed.origin.as_str().to_owned())
        .unwrap_or_default();

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Key {
        key_name: String,
        public_key: String,
        key_hash: String,
    }
    let key: Option<Key> = key_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| JsError::new(&e.to_string()))?;
    let key = key.as_ref().map(|k| PublishedKey {
        key_name: &k.key_name,
        public_key: &k.public_key,
        key_hash: &k.key_hash,
    });
    match verify_usage_report(json, &origin, expected_end_user, key.as_ref()) {
        Ok(()) => Ok(verdict(true, "the usage report verifies")),
        Err(UsageReportError::Unsigned) | Err(UsageReportError::BadSignature) => {
            Ok(verdict(false, "the head's signature does not verify"))
        }
        Err(error) => Err(JsError::new(&error.to_string())),
    }
}

/// Verifies a bundle's signed price binding: `json` is the `ProofBundle` (or
/// its envelope) carrying `entry` and `price`.
///
/// # Errors
///
/// [`JsError`] on malformed input or a failed check — the `PriceError` text. An
/// absent `price` field reports in-band, not as an exception.
#[wasm_bindgen(js_name = verifyPriceHead)]
pub fn verify_price_head_js(json: &str) -> Result<String, JsError> {
    match verify_price_head(json) {
        Ok(()) => Ok(verdict(true, "the price binding verifies")),
        Err(PriceError::Absent) => Ok(verdict(false, PriceError::Absent.to_string())),
        Err(error) => Err(JsError::new(&error.to_string())),
    }
}

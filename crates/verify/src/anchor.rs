//! The statement log anchor: rebuilding a statement's window and rollup
//! digests, and checking the signed head that committed the marker entry.

use doubleentry::{Hash, TreeHead};

use crate::usage_report::hex_bytes;

/// What checking a statement's log anchor — the marker entry committing the
/// statement's window and rollup digests — can fail with. Mirrors [`crate::PriceError`]:
/// each variant is one distinct trust failure, so a page can say what is wrong
/// instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnchorError {
    /// The anchor object, the note, a hex field, or the served window/rollup
    /// inputs could not be parsed.
    Malformed(String),
    /// The note names a different ledger than the tenant the statement belongs
    /// to: another tenant's anchor verifies as a signature but answers a
    /// different question.
    WrongOrigin,
    /// The note does not attest the head the verifier expected: the signed head
    /// at the position right after the anchor, where the marker's inclusion is
    /// already committed.
    HeadMismatch,
    /// The content hashes the window was rebuilt from fold to a different digest
    /// than the anchor claims: the statement's covered window was truncated,
    /// reordered, or had entries swapped.
    WindowMismatch,
    /// The canonical rollup the statement's lines and totals rebuild does not
    /// digest to what the anchor committed: the document's figures were altered.
    RollupMismatch,
    /// The note carries no signature line under the published key.
    Unsigned,
    /// The published key rejects the note's signature.
    BadSignature,
    /// The statement carries no anchor: drafted before anchors shipped, or a
    /// document the finalize path never issued.
    Absent,
}

impl std::fmt::Display for AnchorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "the statement anchor could not be parsed ({what})"),
            Self::WrongOrigin => write!(f, "the signed head names a different ledger"),
            Self::HeadMismatch => {
                write!(
                    f,
                    "the note does not attest the head right after the anchor"
                )
            }
            Self::WindowMismatch => {
                write!(
                    f,
                    "the covered entries' content hashes do not fold to the anchored window digest"
                )
            }
            Self::RollupMismatch => {
                write!(
                    f,
                    "the statement's lines and totals do not digest to the anchored rollup"
                )
            }
            Self::Unsigned => {
                write!(f, "the head carries no signature from the published key")
            }
            Self::BadSignature => write!(f, "the head's signature does not verify"),
            Self::Absent => write!(f, "the statement carries no log anchor"),
        }
    }
}

impl std::error::Error for AnchorError {}

/// The domain the statement-window digest folds under — the same constant
/// `oxsum_core` anchors with (`STATEMENT_WINDOW_DOMAIN`), duplicated on purpose:
/// the verifier pins the byte string so it cannot drift from what was anchored.
const STATEMENT_WINDOW_DOMAIN: &[u8] = b"oxsum/statement-window/v1";

/// The domain the canonical rollup hashes under — the same constant
/// `oxsum_core` anchors with (`STATEMENT_ROLLUP_DOMAIN`).
const STATEMENT_ROLLUP_DOMAIN: &[u8] = b"oxsum/statement-rollup/v1";

/// The canonical rollup, rebuilt by the verifier from the statement's fields
/// and lines — the same struct `oxsum_core` hashes, redeclared wire-only so the
/// verifier never imports the writer's crate.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatementRollupV1<'a> {
    v: i64,
    period: &'a str,
    log_from_index: Option<i64>,
    log_to_index: Option<i64>,
    entry_count: i64,
    total_minor: i64,
    credit_drawn_minor: i64,
    window_hash: &'a str,
    lines: &'a [StatementRollupLine<'a>],
}

/// One line of the canonical rollup — a `StatementLine`'s fields in the same
/// declaration order `oxsum_core` serializes.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatementRollupLine<'a> {
    channel: &'a str,
    model: &'a str,
    turns: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    reasoning_tokens: i64,
    amount_minor: i64,
}

/// The `anchor` object as `GET …/statements/{id}` serves it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnchorWire {
    /// The marker's ledger id — carried on the wire for the caller's
    /// bookkeeping; the checks bind the digests and the head, not the id.
    #[allow(dead_code)]
    entry_id: String,
    log_index: i64,
    window_hash: String,
    rollup_hash: String,
    origin: String,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

/// One line of the statement detail's `lines` array.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineWire {
    channel: String,
    model: String,
    turns: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    reasoning_tokens: i64,
    amount_minor: i64,
}

/// The statement fields the anchor check binds.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatementWire {
    period: String,
    log_from_index: Option<i64>,
    log_to_index: Option<i64>,
    entry_count: i64,
    total_minor: i64,
    credit_drawn_minor: i64,
}

/// What [`verify_statement_anchor`] needs beyond the detail response itself:
/// the covered entries' content hashes, the head the anchor's position implies,
/// and where the check is pointed.
#[non_exhaustive]
pub struct StatementAnchorCheck<'a> {
    /// The covered entries' `contentHash` hex strings in log order —
    /// `contentHash` per `GET /api/v1/entries/{id}/proof` or the dashboard's
    /// bill rows, ordered by `logIndex` across the statement's window. Fewer
    /// than the window's span means the fold cannot match, so a caller that
    /// could not list them all should not call.
    pub window_hashes: &'a [&'a str],
    /// The signed tree head at size `anchorLogIndex + 1` — the head whose log
    /// already contains the anchor. Fetch it from `GET /api/v1/log/head` (the
    /// current head, when the anchor is the latest entry) or recompute
    /// consistency to it; the verifier only checks the note attests *this* head.
    pub expected_head: &'a TreeHead,
    /// The tenant's ledger origin, `oxsum/ledgers/<tenant>` — the same string
    /// `GET /api/v1/log/head` answers.
    pub expected_origin: &'a str,
}

impl<'a> StatementAnchorCheck<'a> {
    /// Borrows the covered hashes, the expected head and the ledger origin.
    #[must_use]
    pub const fn new(
        window_hashes: &'a [&'a str],
        expected_head: &'a TreeHead,
        expected_origin: &'a str,
    ) -> Self {
        Self {
            window_hashes,
            expected_head,
            expected_origin,
        }
    }
}

/// Verifies a statement's log anchor against the detail response (issue #221).
///
/// `json` is the detail body's JSON (`{statement, lines, anchor}`), and `check`
/// carries what the response cannot say: the covered entries' content hashes in
/// log order and the signed head right after the anchor. Five checks, in trust
/// order:
///
/// 1. The response parses and carries an `anchor` — an unanchored statement has
///    nothing to verify, which is `Absent`, not failure.
/// 2. The window fold: `window_hashes` recompute under
///    `oxsum/statement-window/v1` and must equal `windowHash` — a dropped,
///    inserted or reordered entry changes the digest the anchor recorded.
/// 3. The rollup: the statement's fields and lines re-serialize to the canonical
///    bytes and must digest under `oxsum/statement-rollup/v1` to `rollupHash` —
///    the figures the document shows are the figures the anchor committed.
/// 4. The signed head: a present `note` parses under `expected_origin` and
///    attests `expected_head`, which must be the head at exactly
///    `logIndex + 1` — the anchor's inclusion is proven by the head that
///    follows it. An unsigned deployment serves `note` as null, and the anchor
///    is verified to the two digests alone.
/// 5. The published key's signature verifies over the note body byte for byte —
///    the operator really signed that head.
///
/// # Errors
///
/// The first check that fails, as an [`AnchorError`].
pub fn verify_statement_anchor(
    json: &str,
    check: &StatementAnchorCheck<'_>,
) -> Result<(), AnchorError> {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    #[derive(serde::Deserialize)]
    struct Wire {
        statement: StatementWire,
        lines: Vec<LineWire>,
        #[serde(default)]
        anchor: Option<AnchorWire>,
    }
    let wire: Wire =
        serde_json::from_str(json).map_err(|e| AnchorError::Malformed(e.to_string()))?;
    let anchor = wire.anchor.ok_or(AnchorError::Absent)?;

    // The window fold: each served content hash decodes to 32 bytes and folds in
    // order; the digest must be the one the anchor recorded.
    let mut payload = Vec::with_capacity(check.window_hashes.len() * Hash::LEN);
    for hash in check.window_hashes {
        let bytes = hex_bytes::<32>(hash)
            .map_err(|_| AnchorError::Malformed("a window content hash is not hex".into()))?;
        payload.extend_from_slice(&bytes);
    }
    let folded = Hash::digest(STATEMENT_WINDOW_DOMAIN, &payload);
    if folded.to_hex() != anchor.window_hash {
        return Err(AnchorError::WindowMismatch);
    }

    // The rollup: the statement's figures and lines re-serialize to the canonical
    // bytes and digest to what the anchor committed.
    let lines: Vec<StatementRollupLine<'_>> = wire
        .lines
        .iter()
        .map(|line| StatementRollupLine {
            channel: &line.channel,
            model: &line.model,
            turns: line.turns,
            input_tokens: line.input_tokens,
            output_tokens: line.output_tokens,
            cached_tokens: line.cached_tokens,
            reasoning_tokens: line.reasoning_tokens,
            amount_minor: line.amount_minor,
        })
        .collect();
    let rollup = StatementRollupV1 {
        v: 1,
        period: &wire.statement.period,
        log_from_index: wire.statement.log_from_index,
        log_to_index: wire.statement.log_to_index,
        entry_count: wire.statement.entry_count,
        total_minor: wire.statement.total_minor,
        credit_drawn_minor: wire.statement.credit_drawn_minor,
        window_hash: &anchor.window_hash,
        lines: &lines,
    };
    let canonical = serde_json::to_string(&rollup)
        .map_err(|e| AnchorError::Malformed(format!("the canonical rollup: {e}")))?;
    if Hash::digest(STATEMENT_ROLLUP_DOMAIN, canonical.as_bytes()).to_hex() != anchor.rollup_hash {
        return Err(AnchorError::RollupMismatch);
    }

    // The signed head: the note must name this tenant's ledger and attest the
    // head right after the anchor — the marker's inclusion is committed there.
    // An unsigned deployment serves `note` as null, and the anchor is checked
    // to the digests alone — the signature layer simply does not apply.
    let Some(note) = anchor.note.as_deref() else {
        return Ok(());
    };
    let parsed = SignedTreeHead::parse(note).map_err(|e| AnchorError::Malformed(e.to_string()))?;
    if parsed.origin.as_str() != check.expected_origin || parsed.origin.as_str() != anchor.origin {
        return Err(AnchorError::WrongOrigin);
    }
    if parsed.head != *check.expected_head || parsed.head.size != anchor.log_index as u64 + 1 {
        return Err(AnchorError::HeadMismatch);
    }
    let key_name = anchor
        .key_name
        .as_deref()
        .ok_or_else(|| AnchorError::Malformed("the anchor carries no key name".into()))?;
    let key_hash = anchor
        .key_hash
        .as_deref()
        .ok_or_else(|| AnchorError::Malformed("the anchor carries no key hash".into()))?;
    let selector = hex_bytes::<4>(key_hash)
        .map_err(|_| AnchorError::Malformed("the published key hash is not hex".into()))?;
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == key_name && s.key_hash == selector)
        .ok_or(AnchorError::Unsigned)?;
    let public_key = anchor
        .public_key
        .as_deref()
        .ok_or_else(|| AnchorError::Malformed("the anchor carries no public key".into()))?;
    let public = hex_bytes::<32>(public_key)
        .map_err(|_| AnchorError::Malformed("the published key is not hex".into()))?;
    let name = KeyName::new(key_name)
        .map_err(|_| AnchorError::Malformed("the published key name is invalid".into()))?;
    let verifying = VerifyingKey::from_bytes(name, public).map_err(|_| {
        AnchorError::Malformed("the published key is not a valid Ed25519 key".into())
    })?;
    match verifying.verify(&parsed.body(), signature) {
        true => Ok(()),
        false => Err(AnchorError::BadSignature),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// The statement log anchor (issue #221): the marker entry's digests are
    /// rebuilt from the served window and lines, and the signed head right after
    /// the marker proves the log committed it — the same trust order the checks
    /// run in.
    mod statement_anchor {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{
            AnchorError, STATEMENT_ROLLUP_DOMAIN, STATEMENT_WINDOW_DOMAIN, StatementAnchorCheck,
            StatementRollupLine, StatementRollupV1, verify_statement_anchor,
        };

        const ORIGIN: &str = "oxsum/ledgers/acme";

        /// The operator's key in the shape the anchor object publishes.
        struct Operator {
            signing: SigningKey,
            key_name: String,
            public_key: String,
            key_hash: String,
        }

        impl Operator {
            fn from_seed(seed: [u8; 32]) -> Self {
                let signing = SigningKey::from_seed(
                    KeyName::new("oxsum/tree-heads").expect("a valid key name"),
                    seed,
                );
                let verifying = signing.verifying_key();
                Self {
                    signing,
                    key_name: verifying.name().as_str().to_owned(),
                    public_key: verifying
                        .to_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    key_hash: verifying
                        .note_key_hash()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                }
            }

            /// The signed note over `head`, in the `oxsum/ledgers/<tenant>` origin.
            fn note(&self, head: TreeHead) -> String {
                let mut note =
                    SignedTreeHead::new(Origin::new(ORIGIN).expect("a valid origin"), head);
                let body = note.body();
                note.add_signature(self.signing.sign(&body));
                note.to_note()
            }
        }

        /// Three fake covered entries' content hashes — what the caller collected
        /// from `GET /api/v1/entries/{id}/proof`, in log order.
        fn covered() -> [String; 3] {
            [1u8, 2, 3].map(|i| Hash::digest(b"test/covered", &[i]).to_hex())
        }

        fn window_hash(covered: &[String]) -> String {
            let mut payload = Vec::with_capacity(covered.len() * 32);
            for hex in covered {
                let bytes = (0..32)
                    .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex"))
                    .collect::<Vec<u8>>();
                payload.extend_from_slice(&bytes);
            }
            Hash::digest(STATEMENT_WINDOW_DOMAIN, &payload).to_hex()
        }

        /// The rollup the statement's figures and lines canonicalize to — built
        /// through the same struct the verifier re-serializes, so the bytes are
        /// identical by construction. A `serde_json::json!` literal would sort
        /// the keys alphabetically (no `preserve_order` in the tree), which is
        /// *not* the canonical form.
        fn rollup(window_hash: &str) -> String {
            let lines = [StatementRollupLine {
                channel: "anthropic",
                model: "claude",
                turns: 3,
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: 10,
                reasoning_tokens: 5,
                amount_minor: 2_000_000,
            }];
            serde_json::to_string(&StatementRollupV1 {
                v: 1,
                period: "2026-09",
                log_from_index: Some(3),
                log_to_index: Some(8),
                entry_count: 3,
                total_minor: 2_000_000,
                credit_drawn_minor: 1_500_000,
                window_hash,
                lines: &lines,
            })
            .expect("the rollup serializes")
        }

        /// The detail body `{statement, lines, anchor}` the endpoint answers.
        /// `anchor_log_index` positions the marker; `head` is the head right
        /// after it; `operator` signs the note — `None` serves an unsigned
        /// deployment's anchor.
        fn detail(
            covered: &[String],
            anchor_log_index: i64,
            head: &TreeHead,
            operator: Option<&Operator>,
        ) -> String {
            let window_hash = window_hash(covered);
            let rollup = rollup(&window_hash);
            let rollup_hash = Hash::digest(STATEMENT_ROLLUP_DOMAIN, rollup.as_bytes()).to_hex();
            let anchor = match operator {
                Some(operator) => serde_json::json!({
                    "entryId": "00000000-0000-5000-8000-000000000001",
                    "logIndex": anchor_log_index,
                    "windowHash": window_hash,
                    "rollupHash": rollup_hash,
                    "origin": ORIGIN,
                    "note": operator.note(*head),
                    "keyName": operator.key_name,
                    "publicKey": operator.public_key,
                    "keyHash": operator.key_hash,
                }),
                None => serde_json::json!({
                    "entryId": "00000000-0000-5000-8000-000000000001",
                    "logIndex": anchor_log_index,
                    "windowHash": window_hash,
                    "rollupHash": rollup_hash,
                    "origin": ORIGIN,
                    "note": serde_json::Value::Null,
                    "keyName": serde_json::Value::Null,
                    "publicKey": serde_json::Value::Null,
                    "keyHash": serde_json::Value::Null,
                }),
            };
            serde_json::json!({
                "statement": {
                    "period": "2026-09",
                    "logFromIndex": 3,
                    "logToIndex": 8,
                    "entryCount": 3,
                    "totalMinor": 2000000,
                    "creditDrawnMinor": 1500000
                },
                "lines": [{
                    "channel": "anthropic",
                    "model": "claude",
                    "turns": 3,
                    "inputTokens": 100,
                    "outputTokens": 50,
                    "cachedTokens": 10,
                    "reasoningTokens": 5,
                    "amountMinor": 2000000
                }],
                "anchor": anchor,
            })
            .to_string()
        }

        /// The check inputs: the covered hashes and the head right after the
        /// marker — `log_index + 1` is where the marker's inclusion lands.
        /// Callers keep the `&str` view alive (`refs` below) since the check
        /// only borrows it.
        fn check<'a>(window: &'a [&'a str], head: &'a TreeHead) -> StatementAnchorCheck<'a> {
            StatementAnchorCheck {
                window_hashes: window,
                expected_head: head,
                expected_origin: ORIGIN,
            }
        }

        /// `&str` views over a `covered()` array — what `check` borrows.
        fn refs(covered: &[String]) -> Vec<&str> {
            covered.iter().map(String::as_str).collect()
        }

        #[test]
        fn a_genuine_signed_anchor_verifies() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            // The marker sits at index 8; the head at size 9 follows it.
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Ok(()),
            );
        }

        #[test]
        fn an_unsigned_deployment_verifies_to_the_digests_alone() {
            let covered = covered();
            let window = refs(&covered);
            // The head only matters when a note exists; a null note returns Ok
            // after the two digest checks.
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, None);
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Ok(()),
            );
        }

        #[test]
        fn a_truncated_window_fails_the_fold() {
            let covered = covered();
            let window = refs(&covered[..2]);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            // The caller only found two of the three covered entries.
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::WindowMismatch),
            );
        }

        #[test]
        fn a_reordered_window_fails_the_fold() {
            let covered = covered();
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            let swapped = [covered[1].clone(), covered[0].clone(), covered[2].clone()];
            let window = refs(&swapped);
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::WindowMismatch),
            );
        }

        #[test]
        fn an_altered_line_fails_the_rollup() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])))
                .replace("2000000", "2000001");
            // One amount digit changes: the rollup no longer digests to the
            // anchor's committed figure.
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::RollupMismatch),
            );
        }

        #[test]
        fn a_head_of_another_ledger_is_wrong_origin() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let attacker = Operator::from_seed([9; 32]);
            // Honestly signed — but the note names another tenant's log.
            let mut json = detail(&covered, 8, &head, Some(&attacker));
            json = json.replace(ORIGIN, "oxsum/ledgers/other-tenant");
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::WrongOrigin),
            );
        }

        #[test]
        fn a_later_head_is_not_the_one_after_the_marker() {
            let covered = covered();
            let window = refs(&covered);
            // The marker sits at index 8; the caller fetched the head at size 10.
            let head = TreeHead {
                size: 10,
                root: Hash::digest(b"test/head", b"10"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::HeadMismatch),
            );
        }

        #[test]
        fn a_note_signed_by_another_key_is_unsigned_for_the_published_key() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let attacker = Operator::from_seed([3; 32]);
            let operator = Operator::from_seed([9; 32]);
            // The attacker signs; the anchor claims the operator's key hash —
            // the served selector matches no signature line in the note.
            let json = detail(&covered, 8, &head, Some(&attacker)).replace(
                &format!("\"keyHash\":\"{}\"", attacker.key_hash),
                &format!("\"keyHash\":\"{}\"", operator.key_hash),
            );
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::Unsigned),
            );
        }

        #[test]
        fn a_tampered_note_body_is_a_bad_signature() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = detail(&covered, 8, &head, Some(&Operator::from_seed([9; 32])));
            // Rewrite the attested root inside the signed body: the signature no
            // longer covers it, but the head fields still parse and match.
            let mut tampered = serde_json::from_str::<serde_json::Value>(&json).expect("json");
            let note = tampered["anchor"]["note"].as_str().expect("note");
            let mut parsed = SignedTreeHead::parse(note).expect("parses");
            parsed.head.root = Hash::digest(b"test/head", b"tampered");
            tampered["anchor"]["note"] = serde_json::Value::String(parsed.to_note());
            // The signed head must attest the head the caller fetched; replace
            // `expected_head` too so only the signature check fails.
            let tampered_head = TreeHead {
                size: 9,
                root: parsed.head.root,
            };
            assert_eq!(
                verify_statement_anchor(&tampered.to_string(), &check(&window, &tampered_head),),
                Err(AnchorError::BadSignature),
            );
        }

        #[test]
        fn a_statement_without_an_anchor_reports_absent() {
            let covered = covered();
            let window = refs(&covered);
            let head = TreeHead {
                size: 9,
                root: Hash::digest(b"test/head", b"9"),
            };
            let json = serde_json::json!({
                "statement": {
                    "period": "2026-09",
                    "logFromIndex": 3,
                    "logToIndex": 8,
                    "entryCount": 3,
                    "totalMinor": 2000000,
                    "creditDrawnMinor": 1500000
                },
                "lines": [],
            })
            .to_string();
            assert_eq!(
                verify_statement_anchor(&json, &check(&window, &head)),
                Err(AnchorError::Absent),
            );
        }
    }
}

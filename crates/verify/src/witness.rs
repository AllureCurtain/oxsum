//! Witness cosignatures over an anchored checkpoint: pins, stored notes,
//! and the verdict each cosignature carries.

use doubleentry::TreeHead;

use crate::usage_report::hex_bytes;

/// What checking a witness's cosignature over an anchored checkpoint can fail
/// with — the failures a reader can act differently on, one per variant.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WitnessVerdict {
    /// The note parses and the pinned key verifies a `cosignature/v1` line over
    /// the checkpoint body. `at` is the timestamp inside the signed message —
    /// the moment the witness claims it saw the head.
    Verified {
        /// Unix seconds inside the signature, tamper-proof.
        at: u64,
    },
    /// The stored note does not parse as a signed head — truncated, wrong
    /// shape, or not a note at all.
    Malformed,
    /// The note names a different origin than the caller expected — it verifies
    /// as a signature but answers for a different log.
    WrongOrigin,
    /// The note attests a different head than the checkpoint claims — either
    /// the stored row is stale or the witness signed a fork.
    HeadMismatch,
    /// The deployment knows no key under this witness's URL: the verifier has
    /// nothing to check the signature against — the cosigned note may be
    /// genuine but is unverifiable as configured.
    UnknownKey,
    /// The pinned key rejects the cosignature — forged bytes, or the witness
    /// signed under a different key than the deployment pinned.
    BadSignature,
}

impl std::fmt::Display for WitnessVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verified { at } => write!(f, "witnessed at Unix second {at}"),
            Self::Malformed => write!(f, "the cosigned note could not be parsed"),
            Self::WrongOrigin => {
                write!(f, "the cosigned note names a different ledger")
            }
            Self::HeadMismatch => {
                write!(f, "the cosigned note attests a different head")
            }
            Self::UnknownKey => {
                write!(f, "the deployment publishes no key for this witness")
            }
            Self::BadSignature => {
                write!(f, "the cosignature does not verify under the pinned key")
            }
        }
    }
}

impl std::error::Error for WitnessVerdict {}

/// One witness pin — the URL the deployment publishes checkpoints to, the key
/// name the witness signs under, and its Ed25519 public half.
///
/// How a verifier holds this set decides what the check means: a pin the
/// verifier configured itself proves a real third party vouched; a pin the
/// *server* served is evidence only under the same trust as the operator's own
/// published key.
#[non_exhaustive]
pub struct WitnessPin<'a> {
    /// The witness endpoint URL, as `OXSUM_WITNESS_URLS` names it.
    pub witness_url: &'a str,
    /// The key name the witness's cosignatures sign under.
    pub key_name: &'a str,
    /// The Ed25519 public key, hex.
    pub public_key: &'a str,
    /// The cosignature selector — `SHA-256(name || '\n' || 0x04 || public)`'s
    /// first four bytes, hex.
    pub key_hash: &'a str,
}

impl<'a> WitnessPin<'a> {
    /// Borrows the four strings a witness check needs.
    #[must_use]
    pub const fn new(
        witness_url: &'a str,
        key_name: &'a str,
        public_key: &'a str,
        key_hash: &'a str,
    ) -> Self {
        Self {
            witness_url,
            key_name,
            public_key,
            key_hash,
        }
    }
}

/// One stored cosignature, as `GET /api/v1/log/checkpoint` serves it.
#[non_exhaustive]
pub struct WitnessNote<'a> {
    /// The witness endpoint this note came back from.
    pub witness_url: &'a str,
    /// The cosigned C2SP note text — the checkpoint body plus the witness's
    /// signature line.
    pub note: &'a str,
}

impl<'a> WitnessNote<'a> {
    /// Borrows the endpoint URL and the cosigned note.
    #[must_use]
    pub const fn new(witness_url: &'a str, note: &'a str) -> Self {
        Self { witness_url, note }
    }
}

/// Checks one witness cosignature against the checkpoint it claims to vouch.
///
/// Five steps, in trust order: the cosigned note parses, it names the
/// checkpoint's own origin, it attests the checkpoint's own head, the
/// witness's URL maps to a pinned key, and that key verifies the
/// `cosignature/v1` line — returning the timestamp it carries.
///
/// A failure names itself rather than collapsing into `false`: a forged line,
/// a stale row and a missing pin all read differently on the page.
#[must_use]
pub fn verify_witness(
    note: &WitnessNote<'_>,
    pinned: &[WitnessPin<'_>],
    expected_origin: &str,
    expected_head: &TreeHead,
) -> WitnessVerdict {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    let parsed = match SignedTreeHead::parse(note.note) {
        Ok(p) => p,
        Err(_) => return WitnessVerdict::Malformed,
    };
    if parsed.origin.as_str() != expected_origin {
        return WitnessVerdict::WrongOrigin;
    }
    if parsed.head != *expected_head {
        return WitnessVerdict::HeadMismatch;
    }
    let Some(pin) = pinned.iter().find(|p| p.witness_url == note.witness_url) else {
        return WitnessVerdict::UnknownKey;
    };
    let Ok(selector) = hex_bytes::<4>(pin.key_hash) else {
        return WitnessVerdict::Malformed;
    };
    // The signature line the witness's pin selects: a cosignature is an
    // extra `— name blob` line on the same note, so the line claiming this
    // pin's name and cosignature-key-hash is the one to verify.
    let Some(signature) = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == pin.key_name && s.key_hash == selector)
    else {
        return WitnessVerdict::BadSignature;
    };
    let Ok(public) = hex_bytes::<32>(pin.public_key) else {
        return WitnessVerdict::Malformed;
    };
    let Ok(name) = KeyName::new(pin.key_name) else {
        return WitnessVerdict::Malformed;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(name, public) else {
        return WitnessVerdict::Malformed;
    };
    match verifying.verify_cosignature(&parsed.body(), signature) {
        Some(at) => WitnessVerdict::Verified { at },
        None => WitnessVerdict::BadSignature,
    }
}

/// Runs [`verify_witness`] over every cosigned note on a checkpoint.
///
/// The verdicts come back in the notes' own order — one entry per witness the
/// operator published to, so the page can name the witnesses that failed
/// rather than just count the ones that passed.
#[must_use]
pub fn verify_witnesses<'a>(
    notes: &'a [WitnessNote<'a>],
    pinned: &[WitnessPin<'_>],
    expected_origin: &str,
    expected_head: &TreeHead,
) -> Vec<(&'a str, WitnessVerdict)> {
    notes
        .iter()
        .map(|note| {
            (
                note.witness_url,
                verify_witness(note, pinned, expected_origin, expected_head),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// Third-party cosignatures (issue #259): a `cosignature/v1` line on a stored
    /// note is checked against the witness's pinned key, under the same "each
    /// failure names itself" rule as `verify_signed_head`. `verify_cosignature`
    /// is C2SP's `tlog-cosignature` — the timestamp lives inside the signed
    /// message, so a tampered `at` fails the signature, not just the verdict.
    mod witnesses {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{
            WitnessNote, WitnessPin, WitnessVerdict, verify_witness, verify_witnesses,
        };

        const ORIGIN: &str = "oxsum/ledgers/acme";
        const WITNESS_URL: &str = "https://witness.example.com";

        /// A witness's key pair as the operator's pinset publishes it:
        /// url, cosignature name, hex public key, hex 4-byte selector.
        struct Witness {
            signing: SigningKey,
            url: String,
            key_name: String,
            public_key: String,
            key_hash: String,
        }

        impl Witness {
            fn new(url: &str, name: &str, seed: [u8; 32]) -> Self {
                let signing =
                    SigningKey::from_seed(KeyName::new(name).expect("a valid name"), seed);
                let verifying = signing.verifying_key();
                Self {
                    signing,
                    url: url.to_owned(),
                    key_name: verifying.name().as_str().to_owned(),
                    public_key: verifying
                        .to_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    key_hash: verifying
                        .cosignature_key_hash()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                }
            }

            fn pin(&self) -> WitnessPin<'_> {
                WitnessPin {
                    witness_url: &self.url,
                    key_name: &self.key_name,
                    public_key: &self.public_key,
                    key_hash: &self.key_hash,
                }
            }

            /// The cosigned note the witness's `/add-checkpoint` answers:
            /// the checkpoint's body verbatim plus its cosignature line.
            fn cosign(&self, head: TreeHead, origin: &str, at: u64) -> String {
                let mut parsed =
                    SignedTreeHead::new(Origin::new(origin).expect("a valid origin"), head);
                let body = parsed.body();
                parsed.add_signature(self.signing.cosign_body(&body, at));
                parsed.to_note()
            }
        }

        /// A small head the tests agree on.
        fn head() -> TreeHead {
            TreeHead {
                size: 42,
                root: Hash::digest(b"test/witness", b"42"),
            }
        }

        #[test]
        fn a_genuine_cosignature_verifies() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let note = witness.cosign(head(), ORIGIN, 1_762_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            let verdict = verify_witness(&cosign, &pinned, ORIGIN, &head());
            assert!(matches!(
                verdict,
                WitnessVerdict::Verified { at: 1_762_000_000 }
            ));
        }

        #[test]
        fn a_tampered_cosignature_fails() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let mut note = witness.cosign(head(), ORIGIN, 1_762_000_000);
            // Rewrite the timestamp inside the signature's first bytes — the
            // signature then no longer covers the message the verifier rebuilds.
            let tampered_at = 1_700_000_000u64;
            let mut parsed = SignedTreeHead::parse(&note).expect("parses");
            // The value's first 8 bytes are the timestamp: re-stamp them.
            let sig = parsed.signatures.first_mut().expect("one signature");
            sig.value[..8].copy_from_slice(&tampered_at.to_be_bytes());
            note = parsed.to_note();
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::BadSignature,
            );
        }

        #[test]
        fn a_cosignature_for_a_different_head_is_a_head_mismatch() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let other = TreeHead {
                size: 43,
                root: Hash::digest(b"test/witness", b"43"),
            };
            let note = witness.cosign(other, ORIGIN, 1_762_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::HeadMismatch,
            );
        }

        #[test]
        fn a_cosignature_for_a_different_origin_is_wrong_origin() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let note = witness.cosign(head(), "oxsum/ledgers/other-tenant", 1_762_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::WrongOrigin,
            );
        }

        #[test]
        fn an_unpinned_witness_reports_unknown_key() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let other = Witness::new("https://other.example.com", "w2", [2; 32]);
            let note = witness.cosign(head(), ORIGIN, 1_762_000_000);
            // The pinset knows a different witness only — the note's own URL finds no pin.
            let pinned = [other.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::UnknownKey,
            );
        }

        #[test]
        fn a_malformed_note_is_named_as_such() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: "this is not a signed tree head",
            };
            assert_eq!(
                verify_witness(&cosign, &pinned, ORIGIN, &head()),
                WitnessVerdict::Malformed,
            );
        }

        #[test]
        fn verify_witnesses_reports_each_note_by_url() {
            let w1 = Witness::new("https://w1.example.com", "w1", [1; 32]);
            let w2 = Witness::new("https://w2.example.com", "w2", [2; 32]);
            let head = head();
            let good = w1.cosign(head, ORIGIN, 1_762_000_000);
            let bad = w2.cosign(
                TreeHead {
                    size: 43,
                    root: Hash::digest(b"test/witness", b"other"),
                },
                ORIGIN,
                1_762_000_000,
            );
            let pinned = [w1.pin(), w2.pin()];
            let notes = [
                WitnessNote {
                    witness_url: "https://w1.example.com",
                    note: &good,
                },
                WitnessNote {
                    witness_url: "https://w2.example.com",
                    note: &bad,
                },
            ];
            let verdicts = verify_witnesses(&notes, &pinned, ORIGIN, &head);
            assert_eq!(verdicts.len(), 2);
            assert!(matches!(verdicts[0].1, WitnessVerdict::Verified { .. }));
            assert_eq!(verdicts[1].1, WitnessVerdict::HeadMismatch);
        }

        /// The stale-checkpoint case: a witness's note is genuine but older than
        /// the head's own `first_seen` — the check is a signature check, and the
        /// page's caller decides whether the timestamp is recent enough, so the
        /// verdict carries the `at` and lets the caller reason about staleness.
        #[test]
        fn an_old_cosignature_still_verifies_but_marks_its_time() {
            let witness = Witness::new(WITNESS_URL, "w1", [1; 32]);
            let note = witness.cosign(head(), ORIGIN, 1_000_000_000);
            let pinned = [witness.pin()];
            let cosign = WitnessNote {
                witness_url: WITNESS_URL,
                note: &note,
            };
            let verdict = verify_witness(&cosign, &pinned, ORIGIN, &head());
            assert!(matches!(
                verdict,
                WitnessVerdict::Verified { at: 1_000_000_000 }
            ));
        }
    }
}

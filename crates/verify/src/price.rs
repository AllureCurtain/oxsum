//! The signed price binding: the price link a bundle carries, and the check
//! that binds a settlement's price to the operator's signed catalog
//! checkpoint.

use doubleentry::Hash;

use crate::required_field;
use crate::usage_report::hex_bytes;

/// The signed link between a bill's price and the operator's catalog.
///
/// Everything a verifier needs to confirm the price a settlement names is the
/// price the catalog actually held: the canonical row bytes the checkpoint's root
/// commits to, the checkpoint note itself, and the published key that signed it.
/// Self-contained — the verifier never asks the server for a second opinion.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PriceLink {
    /// The channel the price belongs to.
    pub channel: String,
    /// The model the price prices.
    pub model: String,
    /// The price version the checkpoint attests.
    pub version: i64,
    /// The canonical JSON bytes the checkpoint root commits to — the exact string
    /// the server hashed, hashed verbatim by the verifier.
    pub row: String,
    /// The C2SP signed-note text attesting `row`'s hash under `version`.
    pub note: String,
    /// The key name the operator signs under (`oxsum/tree-heads`).
    pub key_name: String,
    /// The operator's Ed25519 verifying key, lowercase hex of 32 bytes.
    pub public_key: String,
    /// The 4-byte selector this key's note signatures carry, lowercase hex.
    pub key_hash: String,
}

impl PriceLink {
    /// Starts a builder for a price link.
    #[must_use]
    pub fn builder() -> PriceLinkBuilder {
        PriceLinkBuilder::default()
    }
}

/// Builds a [`PriceLink`]. Every setter is optional until [`build`](Self::build),
/// which panics if a field is missing — the eight fields together are the link.
#[derive(Debug, Default)]
pub struct PriceLinkBuilder {
    channel: Option<String>,
    model: Option<String>,
    version: Option<i64>,
    row: Option<String>,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

impl PriceLinkBuilder {
    /// Sets the channel.
    #[must_use]
    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    /// Sets the model.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the price version.
    #[must_use]
    pub fn version(mut self, version: i64) -> Self {
        self.version = Some(version);
        self
    }

    /// Sets the canonical row bytes.
    #[must_use]
    pub fn row(mut self, row: impl Into<String>) -> Self {
        self.row = Some(row.into());
        self
    }

    /// Sets the checkpoint note.
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Sets the signing key name.
    #[must_use]
    pub fn key_name(mut self, key_name: impl Into<String>) -> Self {
        self.key_name = Some(key_name.into());
        self
    }

    /// Sets the Ed25519 public key, hex.
    #[must_use]
    pub fn public_key(mut self, public_key: impl Into<String>) -> Self {
        self.public_key = Some(public_key.into());
        self
    }

    /// Sets the note key hash, hex.
    #[must_use]
    pub fn key_hash(mut self, key_hash: impl Into<String>) -> Self {
        self.key_hash = Some(key_hash.into());
        self
    }

    /// Finishes the link.
    ///
    /// # Panics
    ///
    /// Panics if any field was not set.
    #[must_use]
    pub fn build(self) -> PriceLink {
        PriceLink {
            channel: required_field(self.channel, "channel"),
            model: required_field(self.model, "model"),
            version: required_field(self.version, "version"),
            row: required_field(self.row, "row"),
            note: required_field(self.note, "note"),
            key_name: required_field(self.key_name, "keyName"),
            public_key: required_field(self.public_key, "publicKey"),
            key_hash: required_field(self.key_hash, "keyHash"),
        }
    }
}

/// What checking a bill's price binding — the signed catalog checkpoint — can
/// fail with. Mirrors [`crate::HeadError`]: each variant is one distinct trust failure,
/// so a page can say what is wrong instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PriceError {
    /// The `price` field, the note, or a hex field could not be parsed.
    Malformed(String),
    /// The note names a different catalog than the channel the bill priced under.
    WrongOrigin,
    /// The `price` field's `channel`, `model` or `version` differs from what the
    /// settlement description names: the checkpoint answers a different price.
    BindingMismatch,
    /// The canonical row's recomputed hash differs from the root the note attests:
    /// the price row was tampered with.
    HashMismatch,
    /// The note carries no signature line under the published key.
    Unsigned,
    /// The published key rejects the note's signature.
    BadSignature,
    /// The bundle carries no `price` field: a non-settlement entry, a bundle made
    /// before price binding shipped, or a deployment that signs no heads.
    Absent,
}

impl std::fmt::Display for PriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => {
                write!(f, "the price binding could not be parsed ({what})")
            }
            Self::WrongOrigin => write!(f, "the checkpoint names a different catalog"),
            Self::BindingMismatch => {
                write!(f, "the checkpoint does not name the price this bill used")
            }
            Self::HashMismatch => {
                write!(f, "the price row does not match the signed checkpoint")
            }
            Self::Unsigned => {
                write!(
                    f,
                    "the checkpoint carries no signature from the published key"
                )
            }
            Self::BadSignature => write!(f, "the checkpoint's signature does not verify"),
            Self::Absent => write!(f, "the bundle carries no signed price binding"),
        }
    }
}

impl std::error::Error for PriceError {}

/// The domain the canonical price row hashes under — the same constant
/// `oxsum_core` signs with (`PRICE_ROW_DOMAIN`), duplicated on purpose: the
/// verifier pins the byte string so it cannot drift from what was signed.
const PRICE_ROW_DOMAIN: &[u8] = b"oxsum/price-catalog/v1";

/// The `(channel, model, priceVersion)` a settlement description names.
struct NamedPrice {
    channel: String,
    model: String,
    version: i64,
}

/// Reads the price identity out of a settlement description — the same light,
/// wire-only parse [`crate::verify_charge`] makes, never importing `oxsum-core` types.
fn named_price(description: &str) -> Result<NamedPrice, PriceError> {
    let value: serde_json::Value = serde_json::from_str(description)
        .map_err(|e| PriceError::Malformed(format!("the settlement description: {e}")))?;
    let get_str = |field: &str| -> Result<String, PriceError> {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| PriceError::Malformed(format!("the description has no {field}")))
    };
    let version = value
        .get("priceVersion")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| PriceError::Malformed("the description has no priceVersion".into()))?;
    Ok(NamedPrice {
        channel: get_str("channel")?,
        model: get_str("model")?,
        version,
    })
}

/// Verifies a bundle's signed price binding (issue #204).
///
/// Takes the whole bundle JSON so it can read both halves itself — the `price`
/// link and the `entry.description` whose `(channel, model, priceVersion)` it must
/// name. Four checks, in trust order:
///
/// 1. The description parses and names a price — the same wire the content hash
///    already covered.
/// 2. The `price` link names exactly that `(channel, model, version)`, and its note
///    names the catalog `oxsum/price-catalog/<channel>` with `size` equal to the
///    version — not another row's checkpoint.
/// 3. Recompute [`Hash::digest`] of the canonical `row` bytes; it must equal the
///    root the note attests — the row the bill priced under is the row the
///    operator signed.
/// 4. A signature line under the published key verifies over the note body byte
///    for byte — the operator really signed that hash.
///
/// # Errors
///
/// The first check that fails, as a [`PriceError`].
pub fn verify_price_head(json: &str) -> Result<(), PriceError> {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire {
        entry: serde_json::Value,
        #[serde(default)]
        price: Option<PriceLink>,
    }
    let wire: Wire =
        serde_json::from_str(json).map_err(|e| PriceError::Malformed(e.to_string()))?;
    let description = wire
        .entry
        .get("description")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| PriceError::Malformed("the bundle's entry has no description".into()))?;
    let named = named_price(description)?;
    let link = wire.price.ok_or(PriceError::Absent)?;

    // The link must name the very price the bill did.
    if link.channel != named.channel || link.model != named.model || link.version != named.version {
        return Err(PriceError::BindingMismatch);
    }

    // The note must checkpoint that same catalog and version.
    let parsed =
        SignedTreeHead::parse(&link.note).map_err(|e| PriceError::Malformed(e.to_string()))?;
    let expected_origin = format!("oxsum/price-catalog/{}", link.channel);
    if parsed.origin.as_str() != expected_origin {
        return Err(PriceError::WrongOrigin);
    }
    if parsed.head.size != link.version as u64 {
        return Err(PriceError::BindingMismatch);
    }

    // The canonical row must hash to what the note attests.
    let row_hash = Hash::digest(PRICE_ROW_DOMAIN, link.row.as_bytes());
    if row_hash != parsed.head.root {
        return Err(PriceError::HashMismatch);
    }

    // And the published key must have signed it.
    let selector = hex_bytes::<4>(&link.key_hash)
        .map_err(|_| PriceError::Malformed("the published key hash is not hex".into()))?;
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == link.key_name && s.key_hash == selector)
        .ok_or(PriceError::Unsigned)?;
    let public = hex_bytes::<32>(&link.public_key)
        .map_err(|_| PriceError::Malformed("the published key is not hex".into()))?;
    let name = KeyName::new(&link.key_name)
        .map_err(|_| PriceError::Malformed("the published key name is invalid".into()))?;
    let verifying = VerifyingKey::from_bytes(name, public).map_err(|_| {
        PriceError::Malformed("the published key is not a valid Ed25519 key".into())
    })?;
    match verifying.verify(&parsed.body(), signature) {
        true => Ok(()),
        false => Err(PriceError::BadSignature),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// The signed price binding (issue #204): the note reuses the tlog-checkpoint
    /// body — `size` is the price version, `root` the canonical row hash — so a
    /// settlement's `price` link verifies the same way a tree head does, plus the
    /// binding checks that tie it to the very price the bill named.
    mod price_heads {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{PriceError, PriceLink, verify_price_head};

        const CHANNEL: &str = "deepseek";
        const MODEL: &str = "deepseek-chat";
        const VERSION: i64 = 3;

        /// The operator's key in the shape the bundle's `price` link publishes.
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

            /// A `price` link over `row`, signed as the channel's catalog checkpoint.
            fn link(&self, channel: &str, version: i64, row: &str) -> PriceLink {
                let origin = Origin::new(format!("oxsum/price-catalog/{channel}")).expect("valid");
                let row_hash = Hash::digest(super::super::PRICE_ROW_DOMAIN, row.as_bytes());
                let mut note = SignedTreeHead::new(
                    origin,
                    TreeHead {
                        size: version as u64,
                        root: row_hash,
                    },
                );
                let body = note.body();
                note.add_signature(self.signing.sign(&body));
                PriceLink {
                    channel: channel.to_owned(),
                    model: MODEL.to_owned(),
                    version,
                    row: row.to_owned(),
                    note: note.to_note(),
                    key_name: self.key_name.clone(),
                    public_key: self.public_key.clone(),
                    key_hash: self.key_hash.clone(),
                }
            }
        }

        /// A bundle JSON carrying `link` under `price`, over a settlement
        /// description naming `(channel, model, version)`.
        fn bundle(description: &str, link: Option<&PriceLink>) -> String {
            let entry = serde_json::json!({ "description": description });
            match link {
                Some(link) => serde_json::json!({ "entry": entry, "price": link }).to_string(),
                None => serde_json::json!({ "entry": entry }).to_string(),
            }
        }

        fn settlement(channel: &str, model: &str, version: i64) -> String {
            serde_json::json!({
                "v": 7,
                "channel": channel,
                "model": model,
                "priceVersion": version,
            })
            .to_string()
        }

        const ROW: &str = r#"{"v":1,"channel":"deepseek","model":"deepseek-chat","version":3,"weight":100,"price":{"inputPricePerMillion":1000000,"outputPricePerMillion":1000000,"maxOutputTokens":1000}}"#;

        #[test]
        fn a_genuine_price_binding_verifies() {
            let operator = Operator::from_seed([9; 32]);
            let link = operator.link(CHANNEL, VERSION, ROW);
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Ok(()));
        }

        #[test]
        fn a_changed_price_byte_fails_the_row_hash() {
            let operator = Operator::from_seed([9; 32]);
            let link = operator.link(CHANNEL, VERSION, ROW);
            let mut link = link;
            // One digit in the stored rate: the row no longer hashes to the root.
            link.row = link.row.replace("1000000", "1000001");
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::HashMismatch));
        }

        #[test]
        fn a_link_naming_another_version_is_a_binding_mismatch() {
            let operator = Operator::from_seed([9; 32]);
            // Signed honestly — but for v4, not the v3 the bill priced under.
            let link = operator.link(CHANNEL, 4, ROW);
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::BindingMismatch));
        }

        #[test]
        fn a_checkpoint_of_another_catalog_is_wrong_origin() {
            let operator = Operator::from_seed([9; 32]);
            // Signed over `other-channel`'s catalog — a valid note, just not the one
            // this bill's channel answers to. Relabel the link's channel to match
            // the bill's so the binding check passes and the *note's* origin line is
            // the only thing off.
            let mut link = operator.link("other-channel", VERSION, ROW);
            link.channel = CHANNEL.to_owned();
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::WrongOrigin));
        }

        #[test]
        fn a_note_signed_by_another_key_is_unsigned_for_the_published_key() {
            let attacker = Operator::from_seed([3; 32]);
            let operator = Operator::from_seed([9; 32]);
            // The attacker signs the checkpoint; the published key fields claim the
            // operator's — no signature line matches the operator's selector.
            let mut link = attacker.link(CHANNEL, VERSION, ROW);
            link.key_name = operator.key_name.clone();
            link.public_key = operator.public_key.clone();
            link.key_hash = operator.key_hash.clone();
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), Some(&link));
            assert_eq!(verify_price_head(&json), Err(PriceError::Unsigned));
        }

        #[test]
        fn a_bundle_without_a_price_link_reports_absent() {
            let json = bundle(&settlement(CHANNEL, MODEL, VERSION), None);
            assert_eq!(verify_price_head(&json), Err(PriceError::Absent));
        }

        #[test]
        fn a_non_settlement_entry_is_malformed_not_absent() {
            // A top-up carries no channel/model/priceVersion: there is no price to
            // bind, which is a different report than "the link is missing".
            let operator = Operator::from_seed([9; 32]);
            let link = operator.link(CHANNEL, VERSION, ROW);
            let json = bundle(r#"{"v":1,"kind":"topUp"}"#, Some(&link));
            assert!(matches!(
                verify_price_head(&json),
                Err(PriceError::Malformed(_))
            ));
        }
    }
}

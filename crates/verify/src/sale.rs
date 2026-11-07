//! The signed sale binding: the sale link a bundle carries, and the check
//! that binds a settlement's customer terms to the operator's signed sale
//! snapshot.

use doubleentry::Hash;

use crate::required_field;
use crate::usage_report::hex_bytes;

/// The signed link between a bill's sale terms and the operator's sale catalog.
///
/// The customer-facing counterpart of [`crate::PriceLink`] (issue #600): when a
/// settlement priced under a *sale book* rather than the wholesale channel
/// catalog, its description commits a `sale.digest` and the bundle carries this
/// link — the canonical snapshot bytes the checkpoint's root commits to, the
/// checkpoint note, and the published key. The canonical bytes bind only the
/// *customer-visible* terms (organization, model, book/version/draft/legacy
/// provenance, effective window, sale price plus discount); supplier `upstream`
/// rates are never part of a sale snapshot, so the evidence cannot attest a
/// supplier cost. Self-contained — the verifier never asks the server twice.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SaleLink {
    /// The customer organization the sale terms priced.
    pub organization_id: String,
    /// The model the snapshot prices.
    pub model: String,
    /// The snapshot digest the settlement commits, lowercase hex — equal to
    /// `row`'s digest and the note's root.
    pub digest: String,
    /// The canonical JSON bytes the checkpoint root commits to — the exact
    /// string the server hashed, hashed verbatim by the verifier.
    pub row: String,
    /// The C2SP signed-note text attesting `row`'s hash under the snapshot's
    /// book version.
    pub note: String,
    /// The key name the operator signs under (`oxsum/tree-heads`).
    pub key_name: String,
    /// The operator's Ed25519 verifying key, lowercase hex of 32 bytes.
    pub public_key: String,
    /// The 4-byte selector this key's note signatures carry, lowercase hex.
    pub key_hash: String,
}

impl SaleLink {
    /// Starts a builder for a sale link.
    #[must_use]
    pub fn builder() -> SaleLinkBuilder {
        SaleLinkBuilder::default()
    }
}

/// Builds a [`SaleLink`]. Every setter is optional until [`build`](Self::build),
/// which panics if a field is missing — the eight fields together are the link.
#[derive(Debug, Default)]
pub struct SaleLinkBuilder {
    organization_id: Option<String>,
    model: Option<String>,
    digest: Option<String>,
    row: Option<String>,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

impl SaleLinkBuilder {
    /// Sets the customer organization id.
    #[must_use]
    pub fn organization_id(mut self, organization_id: impl Into<String>) -> Self {
        self.organization_id = Some(organization_id.into());
        self
    }

    /// Sets the model.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets the snapshot digest, lowercase hex.
    #[must_use]
    pub fn digest(mut self, digest: impl Into<String>) -> Self {
        self.digest = Some(digest.into());
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
    pub fn build(self) -> SaleLink {
        SaleLink {
            organization_id: required_field(self.organization_id, "organizationId"),
            model: required_field(self.model, "model"),
            digest: required_field(self.digest, "digest"),
            row: required_field(self.row, "row"),
            note: required_field(self.note, "note"),
            key_name: required_field(self.key_name, "keyName"),
            public_key: required_field(self.public_key, "publicKey"),
            key_hash: required_field(self.key_hash, "keyHash"),
        }
    }
}

/// What checking a bill's sale binding — the signed sale-snapshot checkpoint —
/// can fail with. Mirrors [`crate::PriceError`]: each variant is one distinct trust
/// failure, so a page can say what is wrong instead of only that something is.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SaleError {
    /// The `sale` field, the settlement's `sale` digest, the note, or a hex field
    /// could not be parsed.
    Malformed(String),
    /// The note names a different sale catalog than the business the snapshot
    /// belongs to.
    WrongOrigin,
    /// The `sale` field's `organizationId`, `model` or `digest` differs from what
    /// the settlement description or the canonical snapshot names: the
    /// checkpoint answers a different sale.
    BindingMismatch,
    /// The canonical snapshot's recomputed digest differs from the root the note
    /// attests or the digest the settlement committed: the terms were tampered
    /// with.
    HashMismatch,
    /// The note carries no signature line under the published key.
    Unsigned,
    /// The published key rejects the note's signature.
    BadSignature,
    /// The bundle carries no `sale` field: a non-settlement entry, a settlement
    /// that priced under the wholesale channel catalog, or a deployment that
    /// signs no heads.
    Absent,
    /// The settlement priced under the wholesale channel catalog, not a sale
    /// book: a wholesale `price` checkpoint cannot answer for customer sale
    /// terms, so there is no sale binding to verify.
    NotSalePriced,
}

impl std::fmt::Display for SaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => {
                write!(f, "the sale binding could not be parsed ({what})")
            }
            Self::WrongOrigin => write!(f, "the checkpoint names a different sale catalog"),
            Self::BindingMismatch => {
                write!(f, "the checkpoint does not name the sale this bill used")
            }
            Self::HashMismatch => {
                write!(f, "the sale snapshot does not match the signed checkpoint")
            }
            Self::Unsigned => {
                write!(
                    f,
                    "the checkpoint carries no signature from the published key"
                )
            }
            Self::BadSignature => write!(f, "the checkpoint's signature does not verify"),
            Self::Absent => write!(f, "the bundle carries no signed sale binding"),
            Self::NotSalePriced => {
                write!(
                    f,
                    "the bill priced under the wholesale catalog, not a sale book"
                )
            }
        }
    }
}

impl std::error::Error for SaleError {}

/// The domain the canonical sale snapshot hashes under — the same constant
/// `oxsum_core` signs with (`SALE_SNAPSHOT_DOMAIN`), duplicated on purpose: the
/// verifier pins the byte string so it cannot drift from what was signed.
const SALE_SNAPSHOT_DOMAIN: &[u8] = b"oxsum/sale-catalog/v1";

/// The `sale` digest a settlement description commits, read out of the
/// description — the same light, wire-only parse [`crate::verify_charge`] makes, never
/// importing `oxsum-core` types. `None` on a settlement that names no sale
/// (wholesale-priced, or a pre-v14 record); a malformed `sale` block is an
/// error.
pub(crate) fn named_sale(description: &str) -> Result<Option<String>, SaleError> {
    let value: serde_json::Value = serde_json::from_str(description)
        .map_err(|e| SaleError::Malformed(format!("the settlement description: {e}")))?;
    match value.get("sale") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(sale) => sale
            .get("digest")
            .and_then(serde_json::Value::as_str)
            .map(|d| Ok(Some(d.to_owned())))
            .unwrap_or_else(|| {
                Err(SaleError::Malformed(
                    "the description's sale block has no digest".into(),
                ))
            }),
    }
}

/// Verifies a bundle's signed sale binding (issue #600).
///
/// Takes the whole bundle JSON so it can read both halves itself — the `sale`
/// link and the `entry.description` whose `sale.digest` it must attest. When the
/// description names no `sale` (a wholesale-priced or pre-v14 settlement), the
/// verdict is [`SaleError::NotSalePriced`], never a pass: a wholesale `price`
/// checkpoint must not be read as proving customer sale terms. Five checks, in
/// trust order:
///
/// 1. The description parses and names a `sale.digest` — the same wire the
///    content hash already covered.
/// 2. The `sale` link's `digest` equals the committed one, and its canonical
///    `row` re-digests under `oxsum/sale-catalog/v1` to that same value — the
///    snapshot the bill priced under is the snapshot the operator signed.
/// 3. The link's `organizationId` and `model` agree with the snapshot `row`
///    carries — the binding names this customer and this model.
/// 4. The note names the sale catalog `oxsum/sale-catalog/<businessId>` for the
///    business the snapshot belongs to, and its `root` equals the digest.
/// 5. A signature line under the published key verifies over the note body byte
///    for byte — the operator really signed that snapshot.
///
/// # Errors
///
/// The first check that fails, as a [`SaleError`].
pub fn verify_sale_head(json: &str) -> Result<(), SaleError> {
    use doubleentry::witness::signing::VerifyingKey;
    use doubleentry::witness::{KeyName, SignedTreeHead};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire {
        entry: serde_json::Value,
        #[serde(default)]
        sale: Option<SaleLink>,
    }
    let wire: Wire = serde_json::from_str(json).map_err(|e| SaleError::Malformed(e.to_string()))?;
    let description = wire
        .entry
        .get("description")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SaleError::Malformed("the bundle's entry has no description".into()))?;
    let committed = named_sale(description)?.ok_or(SaleError::NotSalePriced)?;
    let link = wire.sale.ok_or(SaleError::Absent)?;

    // The link must attest the very digest the bill committed.
    if link.digest != committed {
        return Err(SaleError::BindingMismatch);
    }

    // The canonical snapshot must re-digest to what the bill committed — and the
    // link's customer/model must agree with the snapshot bytes it signs.
    let row_hash = Hash::digest(SALE_SNAPSHOT_DOMAIN, link.row.as_bytes());
    if row_hash.to_hex() != committed {
        return Err(SaleError::HashMismatch);
    }
    let snapshot: serde_json::Value = serde_json::from_str(&link.row)
        .map_err(|e| SaleError::Malformed(format!("the canonical sale snapshot: {e}")))?;
    let str_field = |field: &str| snapshot.get(field).and_then(serde_json::Value::as_str);
    if str_field("organizationId") != Some(link.organization_id.as_str())
        || str_field("model") != Some(link.model.as_str())
    {
        return Err(SaleError::BindingMismatch);
    }

    // The note must checkpoint the business's own sale catalog and attest the
    // digest — not another catalog's snapshot.
    let business_id = str_field("businessId")
        .ok_or_else(|| SaleError::Malformed("the canonical snapshot has no businessId".into()))?;
    let parsed =
        SignedTreeHead::parse(&link.note).map_err(|e| SaleError::Malformed(e.to_string()))?;
    let expected_origin = format!("oxsum/sale-catalog/{business_id}");
    if parsed.origin.as_str() != expected_origin {
        return Err(SaleError::WrongOrigin);
    }
    if parsed.head.root != row_hash {
        return Err(SaleError::HashMismatch);
    }

    // And the published key must have signed it.
    let selector = hex_bytes::<4>(&link.key_hash)
        .map_err(|_| SaleError::Malformed("the published key hash is not hex".into()))?;
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.name.as_str() == link.key_name && s.key_hash == selector)
        .ok_or(SaleError::Unsigned)?;
    let public = hex_bytes::<32>(&link.public_key)
        .map_err(|_| SaleError::Malformed("the published key is not hex".into()))?;
    let name = KeyName::new(&link.key_name)
        .map_err(|_| SaleError::Malformed("the published key name is invalid".into()))?;
    let verifying = VerifyingKey::from_bytes(name, public)
        .map_err(|_| SaleError::Malformed("the published key is not a valid Ed25519 key".into()))?;
    match verifying.verify(&parsed.body(), signature) {
        true => Ok(()),
        false => Err(SaleError::BadSignature),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// The signed sale binding (issue #600): a settlement that priced under a
    /// sale book commits `sale.digest`, and the bundle's `sale` link attests the
    /// resolved snapshot — customer terms, never supplier rates. The checks
    /// mirror `price_heads`, plus the wholesale-vs-sale boundary: a wholesale
    /// `price` checkpoint must never answer for customer terms.
    mod sale_heads {
        use doubleentry::witness::signing::SigningKey;
        use doubleentry::witness::{KeyName, Origin, SignedTreeHead};
        use doubleentry::{Hash, TreeHead};

        use super::super::{SaleError, SaleLink, verify_sale_head};

        const BUSINESS: &str = "1ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c";
        const ORGANIZATION: &str = "2ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c";
        const MODEL: &str = "deepseek-chat";

        /// The operator's key in the shape the bundle's `sale` link publishes.
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

            /// A `sale` link over `row`, signed as the business's sale-catalog
            /// checkpoint. `row` is the canonical SaleSnapshotV1 bytes.
            fn link(&self, business: &str, version: i64, row: &str) -> SaleLink {
                let origin = Origin::new(format!("oxsum/sale-catalog/{business}")).expect("valid");
                let row_hash = Hash::digest(super::super::SALE_SNAPSHOT_DOMAIN, row.as_bytes());
                let mut note = SignedTreeHead::new(
                    origin,
                    TreeHead {
                        size: version as u64,
                        root: row_hash,
                    },
                );
                let body = note.body();
                note.add_signature(self.signing.sign(&body));
                SaleLink {
                    organization_id: ORGANIZATION.to_owned(),
                    model: MODEL.to_owned(),
                    digest: row_hash.to_hex(),
                    row: row.to_owned(),
                    note: note.to_note(),
                    key_name: self.key_name.clone(),
                    public_key: self.public_key.clone(),
                    key_hash: self.key_hash.clone(),
                }
            }
        }

        /// A bundle JSON carrying `link` under `sale`, over a settlement
        /// description committing `digest` (or wholesale terms, `None`).
        fn bundle(description: &str, link: Option<&SaleLink>) -> String {
            let entry = serde_json::json!({ "description": description });
            match link {
                Some(link) => serde_json::json!({ "entry": entry, "sale": link }).to_string(),
                None => serde_json::json!({ "entry": entry }).to_string(),
            }
        }

        /// The canonical SaleSnapshotV1 bytes, compact, in declaration order.
        /// `price` and `discount` are the customer terms the digest binds.
        fn snapshot(business: &str, organization: &str, price_input: i64, discount: i64) -> String {
            serde_json::json!({
                "v": 1,
                "businessId": business,
                "organizationId": organization,
                "model": MODEL,
                "source": "assigned",
                "scheduleId": serde_json::Value::Null,
                "bookId": "3ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c",
                "version": 2,
                "draftRevision": serde_json::Value::Null,
                "legacyChannel": serde_json::Value::Null,
                "effectiveFrom": "2026-10-01T00:00:00Z",
                "effectiveUntil": serde_json::Value::Null,
                "terms": {
                    "price": {
                        "inputPricePerMillion": price_input,
                        "outputPricePerMillion": 2000000,
                        "maxOutputTokens": 8000,
                    },
                    "discountPercent": discount,
                },
            })
            .to_string()
        }

        /// A v14 settlement description committing `digest` — the field the
        /// entry's content hash covers and the link must attest.
        fn settlement(digest: &str) -> String {
            serde_json::json!({
                "v": 14,
                "kind": "usage",
                "channel": "deepseek",
                "model": MODEL,
                "priceVersion": 3,
                "sale": { "digest": digest },
            })
            .to_string()
        }

        fn digest_of(row: &str) -> String {
            Hash::digest(super::super::SALE_SNAPSHOT_DOMAIN, row.as_bytes()).to_hex()
        }

        #[test]
        fn a_genuine_sale_binding_verifies() {
            let operator = Operator::from_seed([9; 32]);
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let link = operator.link(BUSINESS, 2, &row);
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Ok(()));
        }

        #[test]
        fn an_altered_price_fails_the_digest() {
            let operator = Operator::from_seed([9; 32]);
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            // The bill committed the honest digest; the served `row` carries an
            // altered input rate, so it no longer digests to what was committed.
            let mut link = operator.link(BUSINESS, 2, &row);
            link.row = link.row.replace("1000000", "1000001");
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::HashMismatch));
        }

        #[test]
        fn an_altered_discount_fails_the_digest() {
            let operator = Operator::from_seed([9; 32]);
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let mut link = operator.link(BUSINESS, 2, &row);
            link.row = link
                .row
                .replace("\"discountPercent\":0", "\"discountPercent\":25");
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::HashMismatch));
        }

        #[test]
        fn an_altered_customer_is_a_binding_mismatch() {
            let operator = Operator::from_seed([9; 32]);
            // The link's served `organizationId` disagrees with the snapshot the
            // digest actually covers — a sale for another customer cannot answer.
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let mut link = operator.link(BUSINESS, 2, &row);
            link.organization_id = "4ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c".to_owned();
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::BindingMismatch));
        }

        #[test]
        fn a_digest_the_bill_did_not_commit_is_a_binding_mismatch() {
            let operator = Operator::from_seed([9; 32]);
            // Signed honestly over a real snapshot — but the bill committed a
            // different digest, so the link answers a different sale.
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let mut link = operator.link(BUSINESS, 2, &row);
            link.digest = digest_of(&snapshot(BUSINESS, ORGANIZATION, 9_000_000, 0));
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::BindingMismatch));
        }

        #[test]
        fn a_checkpoint_of_another_business_is_wrong_origin() {
            let operator = Operator::from_seed([9; 32]);
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            // The note is signed under a different business's catalog origin.
            let other = "9ec7a2c2-3a13-4a2f-9b74-0f9d2e6a1b3c";
            let link = operator.link(other, 2, &row);
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::WrongOrigin));
        }

        #[test]
        fn a_note_signed_by_another_key_is_unsigned_for_the_published_key() {
            let attacker = Operator::from_seed([3; 32]);
            let operator = Operator::from_seed([9; 32]);
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let mut link = attacker.link(BUSINESS, 2, &row);
            link.key_name = operator.key_name.clone();
            link.public_key = operator.public_key.clone();
            link.key_hash = operator.key_hash.clone();
            let json = bundle(&settlement(&digest_of(&row)), Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::Unsigned));
        }

        #[test]
        fn a_bundle_without_a_sale_link_reports_absent() {
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let json = bundle(&settlement(&digest_of(&row)), None);
            assert_eq!(verify_sale_head(&json), Err(SaleError::Absent));
        }

        #[test]
        fn a_wholesale_priced_bill_is_not_sale_priced() {
            // The settlement commits no `sale` digest — a wholesale `price`
            // checkpoint would be there to verify, but it must never be read as
            // proving customer sale terms.
            let operator = Operator::from_seed([9; 32]);
            let row = snapshot(BUSINESS, ORGANIZATION, 1_000_000, 0);
            let link = operator.link(BUSINESS, 2, &row);
            let wholesale = serde_json::json!({
                "v": 13, "kind": "usage", "channel": "deepseek",
                "model": MODEL, "priceVersion": 3,
            })
            .to_string();
            let json = bundle(&wholesale, Some(&link));
            assert_eq!(verify_sale_head(&json), Err(SaleError::NotSalePriced));
        }
    }
}

//! End-user signed usage reports (issue #261): mint, list, and re-fetch.
//!
//! The handlers are thin — the subset, the marker append and the frozen rows
//! all live in `oxsum_core::Db::mint_usage_report`, and the single-head bundle
//! in `Wallet::usage_report_bundle`. What this module owns is the wire shape:
//! the `UsageReport` body a verifier feeds to `oxsum_verify::verify_usage_report`.

use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use oxsum_core::{Principal, signing_key};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::log::TreeHeadRes;
use crate::routes::{ApiResult, ok, parse_day, wallet};

/// Lowercase hex of a byte string — the encoding `oxsum_verify` decodes on the
/// `publicKey`/`keyHash` fields, matching how `crates/web`'s signed-head page
/// spells them (the `log` module's own `VerifyingKeyRes` serves base64, a
/// different surface).
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `POST` body: who the report is for and the ended span it covers.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MintBody {
    end_user: String,
    from: String,
    to: String,
}

/// One entry in the bundle — the entry itself, its recorded content hash, and
/// its inclusion proof. `entry` serializes doubleentry's encoding verbatim so
/// the verifier recomputes the hash rather than trusting a served one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportEntryRes {
    entry: oxsum_core::Entry<oxsum_core::Balanced, { oxsum_core::SCALE }>,
    content_hash: String,
    proof: oxsum_core::InclusionProof,
}

impl From<oxsum_core::ReportEntryBundle> for ReportEntryRes {
    fn from(b: oxsum_core::ReportEntryBundle) -> Self {
        Self {
            entry: b.entry,
            content_hash: b.content_hash,
            proof: b.proof,
        }
    }
}

/// The `UsageReport` body `GET`/`POST` answer — the descriptor plus the
/// self-contained bundle.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageReportRes {
    report_id: Uuid,
    end_user: String,
    from: String,
    to: String,
    entry_count: i64,
    charged_total_minor: i64,
    subset_hash: String,
    marker: ReportEntryRes,
    entries: Vec<ReportEntryRes>,
    head: TreeHeadRes,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
    /// The tenant log's origin, so a reader's witness check knows which log the
    /// cosignatures must name (issue #328).
    #[serde(skip_serializing_if = "Option::is_none")]
    witness_origin: Option<String>,
    /// The deployment's pinned witness keys — public by design, empty when the
    /// deployment pins none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    witness_keys: Vec<WitnessPinRes>,
    /// The cosigned notes stored for this bundle's head, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    witness_notes: Vec<WitnessNoteRes>,
}

/// One pinned witness key, as `GET /api/v1/log/checkpoint` already spells it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WitnessPinRes {
    witness_url: String,
    key_name: String,
    public_key: String,
    key_hash: String,
}

/// One witness's cosigned note for the bundle's head.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WitnessNoteRes {
    witness_url: String,
    note: String,
}

/// One row of the `GET /api/v1/org/usage-reports` list — the descriptor alone.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageReportSummaryRes {
    report_id: Uuid,
    end_user: String,
    from: String,
    to: String,
    entry_count: i64,
    charged_total_minor: i64,
    subset_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageReportListRes {
    rows: Vec<UsageReportSummaryRes>,
}

/// Builds the full `UsageReport` wire body for a descriptor — the shared tail
/// of mint, list-fetch, and re-fetch: assemble the single-head bundle, then
/// flatten the signed head onto the descriptor.
async fn report_res(
    state: &AppState,
    organization: &oxsum_core::Organization,
    report: oxsum_core::UsageReport,
) -> Result<UsageReportRes, ApiError> {
    let w = wallet(&state.tenants, organization).await?;
    // The optional signing key is derived like `log::proof` derives it: `None`
    // leaves the bundle's signature fields absent rather than failing a
    // deployment that signs no heads.
    let key = state
        .config
        .head_signing_seed()
        .map(signing_key)
        .transpose()
        .map_err(|_| ApiError::Internal)?;
    let (marker, entries, head, signed) = w
        .usage_report_bundle(
            oxsum_core::EntryId::from_uuid(report.report_id),
            &report.rows,
            key.as_ref(),
        )
        .await?;
    let (note, key_name, public_key, key_hash) = match signed {
        Some(signed) => (
            Some(signed.note),
            Some(signed.key.name),
            Some(hex(&signed.key.public_key)),
            Some(hex(&signed.key.key_hash)),
        ),
        None => (None, None, None, None),
    };
    // The witness half (issue #328): the public pinset and the cosignatures
    // stored for this bundle's head. A head the witness pass has not anchored
    // yet answers an empty note list — the reader says so.
    let witness_origin = oxsum_core::origin_for(&organization.tenant_id)
        .ok()
        .map(|origin| origin.as_str().to_owned());
    let witness_keys: Vec<WitnessPinRes> = state
        .config
        .witness_keys()
        .iter()
        .map(|key| WitnessPinRes {
            witness_url: key.witness_url.clone(),
            key_name: key.key_name.clone(),
            public_key: key.public_key.clone(),
            key_hash: key.key_hash.clone(),
        })
        .collect();
    let witness_notes: Vec<WitnessNoteRes> = state
        .db
        .witness_cosignatures(
            organization.id,
            i64::try_from(head.size).unwrap_or(i64::MAX),
        )
        .await?
        .into_iter()
        .map(|cosign| WitnessNoteRes {
            witness_url: cosign.witness_url,
            note: cosign.note,
        })
        .collect();
    Ok(UsageReportRes {
        report_id: report.report_id,
        end_user: report.end_user,
        from: report.from_date.to_string(),
        to: report.to_date.to_string(),
        entry_count: report.entry_count,
        charged_total_minor: report.charged_total_minor,
        subset_hash: report.subset_hash,
        marker: marker.into(),
        entries: entries.into_iter().map(Into::into).collect(),
        head: head.into(),
        note,
        key_name,
        public_key,
        key_hash,
        witness_origin,
        witness_keys,
        witness_notes,
    })
}

/// `POST /api/v1/org/usage-reports` — mint the report for `endUser` over
/// `[from, to]`, or replay the standing one.
pub(crate) async fn mint(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(body): ApiJson<MintBody>,
) -> ApiResult<UsageReportRes> {
    let from = parse_day(&body.from)?;
    let to = parse_day(&body.to)?;
    let organization = principal.organization();
    let w = wallet(&state.tenants, organization).await?;
    let report = state
        .db
        .mint_usage_report(
            &w,
            organization.id,
            &organization.tenant_id,
            &body.end_user,
            from,
            to,
        )
        .await?;
    ok(report_res(&state, organization, report).await?)
}

/// `GET /api/v1/org/usage-reports` — every minted report's descriptor, newest first.
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<UsageReportListRes> {
    let rows = state
        .db
        .usage_reports_of(principal.organization().id)
        .await?
        .into_iter()
        .map(|r| UsageReportSummaryRes {
            report_id: r.report_id,
            end_user: r.end_user,
            from: r.from_date.to_string(),
            to: r.to_date.to_string(),
            entry_count: r.entry_count,
            charged_total_minor: r.charged_total_minor,
            subset_hash: r.subset_hash,
        })
        .collect();
    ok(UsageReportListRes { rows })
}

/// `GET /api/v1/org/usage-reports/{reportId}` — the frozen bundle.
pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(report_id): Path<Uuid>,
) -> ApiResult<UsageReportRes> {
    let Some(report) = state.db.usage_report(report_id).await? else {
        return Err(ApiError::not_found());
    };
    // A report belongs to the organization that minted it: an id guessed from
    // another tenant's ledger answers 404, never that other's subset.
    let organization = principal.organization();
    if report.organization_id != organization.id {
        return Err(ApiError::not_found());
    }
    ok(report_res(&state, organization, report).await?)
}

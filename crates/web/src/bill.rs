//! The public bill page (`/bill/{token}`, issue #302).
//!
//! A share link (issue #300) is the customer's only credential: the page opens
//! with the token alone, resolves it to the frozen report and renders what the
//! organization committed — no session, no account, no bundle to paste. The
//! page is server-rendered on purpose, the same choice `/verify` makes: the
//! numbers come from the ledger, and the evidence block names the operator key
//! and the head the rows prove against. `bill.js` (issue #305) then runs the
//! same verifier in the browser over the bundle the page embeds, so the check
//! happens on the customer's machine — the page renders, the customer checks.

use serde::Serialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Js;
use topcoat::router::error::{internal_server_error, not_found};
use topcoat::router::{RouterBuilder, page, path_param, route};
use topcoat::view::{View, view};

use oxsum_core::{
    Db, EntryId, KeyPublication, LinkScope, ReportEntryBundle, SettlementKind, SettlementRecord,
    Tenants, signing_key,
};

use crate::views::plain;

path_param!(token);

/// Registers the bill page and its script on the server-rendered router.
pub(crate) fn register(builder: RouterBuilder) -> RouterBuilder {
    builder.page(bill_page).route(bill_js)
}

/// The page's own script: runs the verifier over the embedded bundle when the
/// browser has a wasm engine. Checked in, not generated — the page's behavior,
/// the same split `/verify.js` keeps.
#[route(GET "/bill.js")]
async fn bill_js() -> Result<Js<&'static str>> {
    Ok(Js(BILL_JS))
}

const BILL_JS: &str = include_str!("bill.js");

/// One row of a bill: the settled entry as the customer reads it.
struct BillRow {
    when: String,
    model: String,
    outcome: String,
    tokens: String,
    charged: String,
}

/// One report inside the page — a link pins one, a customer link lists them.
struct BillSection {
    end_user: String,
    from: String,
    to: String,
    count: String,
    total: String,
    head_size: u64,
    head_root: String,
    rows: Vec<BillRow>,
    /// The bundle the browser verifier runs on, serialized once here so the
    /// page and the script check the same bytes (issue #305).
    bundle_json: String,
}

/// The `UsageReport` wire shape `verify_usage_report` parses — the same fields
/// the API serves, assembled here for the embedded bundle.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleWire {
    report_id: String,
    end_user: String,
    from: String,
    to: String,
    entry_count: i64,
    charged_total_minor: i64,
    subset_hash: String,
    marker: EntryWire,
    entries: Vec<EntryWire>,
    head: HeadWire,
    note: Option<String>,
    key_name: Option<String>,
    public_key: Option<String>,
    key_hash: Option<String>,
}

/// One entry with its recorded hash and inclusion proof.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryWire {
    entry: oxsum_core::Entry<oxsum_core::Balanced, { oxsum_core::SCALE }>,
    content_hash: String,
    proof: oxsum_core::InclusionProof,
}

impl From<ReportEntryBundle> for EntryWire {
    fn from(bundle: ReportEntryBundle) -> Self {
        Self {
            entry: bundle.entry,
            content_hash: bundle.content_hash,
            proof: bundle.proof,
        }
    }
}

/// The tree head the proofs anchor to, hex-encoded as the verifier reads it.
#[derive(Serialize)]
struct HeadWire {
    size: u64,
    root: String,
}

/// `GET /bill/{token}` — expired, revoked and unknown tokens are one 404.
#[page("/bill/{token}")]
async fn bill_page(cx: &Cx) -> Result<impl View> {
    let token = path_param::<Token>(cx);
    let db: Db = crate::app_value(cx)?;
    let tenants: Tenants = crate::app_value(cx)?;
    let internal = |error: oxsum_core::WalletError| {
        internal_server_error(topcoat::Error::msg(format!(
            "the bill page could not read the ledger: {error}"
        )))
    };

    let Some(link) = db.resolve_report_link(token).await.map_err(internal)? else {
        return Err(not_found().into());
    };

    // The operator's public half, derived exactly as `/verify` derives it; a
    // deployment that signs no heads publishes no fingerprint and the page says
    // so rather than inventing one.
    let signing = match crate::web_config(cx)?.head_seed {
        Some(seed) => Some(signing_key(seed).map_err(|_| {
            internal_server_error(topcoat::Error::msg("the head signing key is misconfigured"))
        })?),
        None => None,
    };
    let fingerprint = signing
        .as_ref()
        .map(|key| hex(&KeyPublication::of(key).public_key))
        .unwrap_or_default();

    let wallet = tenants.get(&link.tenant_id).await.map_err(internal)?;
    let reports = match link.scope {
        LinkScope::Report => {
            let Some(report_id) = link.report_id else {
                return Err(not_found().into());
            };
            let Some(report) = db.usage_report(report_id).await.map_err(internal)? else {
                return Err(not_found().into());
            };
            // A link names one report; a report of another organization would
            // mean the link outlived its report, which the FK makes impossible —
            // check anyway, the page is the trust surface.
            if report.organization_id != link.organization_id {
                return Err(not_found().into());
            }
            vec![report]
        }
        LinkScope::EndUser => db
            .usage_reports_of(link.organization_id)
            .await
            .map_err(internal)?
            .into_iter()
            .filter(|report| report.end_user == link.end_user)
            .collect(),
    };

    let mut bills = Vec::new();
    for report in reports {
        let (marker, entries, head, signed) = wallet
            .usage_report_bundle(
                EntryId::from_uuid(report.report_id),
                &report.rows,
                signing.as_ref(),
            )
            .await
            .map_err(internal)?;
        let rows = entries
            .iter()
            .map(|bundle| {
                let entry = &bundle.entry;
                let described = SettlementRecord::parse(entry.description().as_str());
                let (model, outcome, input, output, charged) = match described {
                    Some(record) => (
                        record.model,
                        outcome_label(record.kind).to_owned(),
                        record.input_tokens,
                        record.output_tokens,
                        credits(record.charged),
                    ),
                    None => (
                        "—".to_owned(),
                        "ledger entry (not a settlement)".to_owned(),
                        0,
                        0,
                        "—".to_owned(),
                    ),
                };
                BillRow {
                    when: entry.booking_date().to_string(),
                    model,
                    outcome,
                    tokens: format!("{input} / {output}"),
                    charged,
                }
            })
            .collect();
        let (note, key_name, public_key, key_hash) = match signed {
            Some(signed) => (
                Some(signed.note),
                Some(signed.key.name),
                Some(hex(&signed.key.public_key)),
                Some(hex(&signed.key.key_hash)),
            ),
            None => (None, None, None, None),
        };
        let bundle = BundleWire {
            report_id: report.report_id.to_string(),
            end_user: report.end_user.clone(),
            from: report.from_date.to_string(),
            to: report.to_date.to_string(),
            entry_count: report.entry_count,
            charged_total_minor: report.charged_total_minor,
            subset_hash: report.subset_hash.clone(),
            marker: marker.into(),
            entries: entries.into_iter().map(Into::into).collect(),
            head: HeadWire {
                size: head.size,
                root: head.root.to_hex(),
            },
            note,
            key_name,
            public_key,
            key_hash,
        };
        let bundle_json = serde_json::to_string(&bundle).map_err(|error| {
            internal_server_error(topcoat::Error::msg(format!(
                "the bill bundle did not serialize: {error}"
            )))
        })?;
        bills.push(BillSection {
            end_user: report.end_user,
            from: report.from_date.to_string(),
            to: report.to_date.to_string(),
            count: if report.entry_count == 1 {
                "1 entry".to_owned()
            } else {
                format!("{} entries", report.entry_count)
            },
            total: credits(report.charged_total_minor),
            head_size: head.size,
            head_root: head.root.to_hex(),
            rows,
            bundle_json,
        });
    }

    let empty = bills.is_empty();
    let heading = if link.scope == LinkScope::EndUser {
        "Bills"
    } else {
        "Bill"
    };
    let end_user = link.end_user;
    let fingerprint = if fingerprint.is_empty() {
        "not published on this deployment".to_owned()
    } else {
        fingerprint
    };

    Ok(view! {
        plain(
            title: heading,
            <div class="card">
                <h1>(heading)</h1>
                <p class="muted">
                    (format!(
                        "Customer {end_user} — this page is the whole credential: no account, no sign-in."
                    ))
                </p>
                if empty {
                    <p>"No report has been issued for this link yet."</p>
                }
                for bill in bills {
                    <section class="bill">
                        <h2>(format!("{} — {} to {}", bill.end_user, bill.from, bill.to))</h2>
                        <p>(format!("{} entries · {} credits", bill.count, bill.total))</p>
                        <table>
                            <thead>
                                <tr>
                                    <th>"Date"</th>
                                    <th>"Model"</th>
                                    <th>"Outcome"</th>
                                    <th>"Tokens in / out"</th>
                                    <th>"Credits"</th>
                                </tr>
                            </thead>
                            <tbody>
                                for row in bill.rows {
                                    <tr>
                                        <td>(row.when)</td>
                                        <td>(row.model)</td>
                                        <td>(row.outcome)</td>
                                        <td class="mono">(row.tokens)</td>
                                        <td class="mono amount">(row.charged)</td>
                                    </tr>
                                }
                            </tbody>
                        </table>
                        <p class="muted head">
                            (format!("Ledger head {} · root {}", bill.head_size, bill.head_root))
                        </p>
                        <p class="verdict" aria-live="polite"></p>
                        <button
                            type="button"
                            class="verify-bill"
                            data-end-user=(bill.end_user)
                            data-bundle=(bill.bundle_json)
                        >"Verify this bill"</button>
                    </section>
                }
                <p class="muted head">(format!("Operator key: {fingerprint}"))</p>
                <p class="muted">
                    "The rows are the frozen subset the organization committed to its ledger; \
                     the head above is the log they belong to. The bundle itself is what a \
                     verifier checks — this page does not ask you to trust its rendering."
                </p>
                <script type="module" src="/bill.js"></script>
            </div>
        )
    })
}

/// A settlement kind as a customer reads it — no internal vocabulary without a
/// plain-language twin.
fn outcome_label(kind: SettlementKind) -> &'static str {
    match kind {
        SettlementKind::Usage => "settled",
        SettlementKind::Estimated => "estimated (upstream reported no usage)",
        SettlementKind::ClientCancelled => "client cancelled mid-stream",
        SettlementKind::UpstreamError => "upstream error — not charged",
        SettlementKind::UpstreamUnreachable => "upstream unreachable — not charged",
        SettlementKind::Capped => "capped at the frozen upper bound",
        SettlementKind::Swept => "hold timed out — not charged",
        SettlementKind::Unpriced => "partly unpriced — flagged for review",
        SettlementKind::Released => "released by the reporting service",
        SettlementKind::Refused => "refused at admission — not charged",
    }
}

/// Minor units as credits with six decimals, integer arithmetic only.
fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let absolute = minor.unsigned_abs();
    format!("{sign}{}.{:06}", absolute / 1_000_000, absolute % 1_000_000)
}

/// Lowercase hex, the spelling the verifier and the API use.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

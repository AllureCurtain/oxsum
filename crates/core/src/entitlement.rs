//! The entitlement read (issue #501): a "may this key call now" preview that
//! composes the same checks the gateway's admission and `hold_for_key` run,
//! without consuming a request slot, a window slot or a hold.
//!
//! The pieces come from three places: the ledger (suspension, the budget caps,
//! the holds count, a presented mandate's committed spend), the `oxsum`
//! tables' durable token window (`Db::tpm_window*`), and the server's
//! in-process request limiter. [`Wallet::entitlement_read`] gathers the
//! ledger-side verdicts — no locks, one connection, every constraint read even
//! when an earlier one already refuses, because the `checks` breakdown is the
//! point of the endpoint: a debugging surface that stops at the first failure
//! cannot say what else was wrong. [`Entitlement::compose`] then folds in the
//! limiter and token-window verdicts the route computed and picks the refusing
//! [`EntitlementReason`] by the documented order below.
//!
//! The read is a snapshot, not a reservation: a racing request may spend the
//! headroom between the answer and the call — which is why the atomic
//! admission checks stay authoritative and this surface stays free.

use serde::Serialize;
use uuid::Uuid;

/// Which check refused, when `allowed` is false — the caller's first answer.
///
/// The order is the composition's own — key state, capability, the rate
/// windows, money, concurrency, the delegation — documented on the endpoint
/// rather than inherited from admission's internals, where RPM happens to run
/// before the hold's own checks. `key_inactive` never reaches the wire — a
/// dead key answers 404 like a foreign id — but the variant exists so the
/// core verdict is total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementReason {
    /// The key is revoked or expired.
    KeyInactive,
    /// The organization is suspended: no new spend admits.
    OrgSuspended,
    /// The key's `modelAllowlist` does not name the requested model.
    ModelNotAllowed,
    /// The rolling-minute request window is full.
    RpmExceeded,
    /// The rolling-minute settled-token window cannot fit the estimate.
    TpmExceeded,
    /// Committed spend plus the estimate exceeds the key's limit — or the
    /// member's or team's, the caps the key's spend also draws on.
    BudgetExhausted,
    /// The key already holds `maxConcurrentHolds` open reservations.
    ConcurrentHolds,
    /// The presented mandate cannot verify — bad blob, foreign key, no seed.
    MandateInvalid,
    /// The presented mandate is outside its `notBefore`/`notAfter` window.
    MandateWindow,
    /// The presented mandate's model subset does not name the requested model.
    MandateModel,
    /// The presented mandate's committed spend plus the estimate exceeds its cap.
    MandateCap,
}

/// The per-constraint verdict, for a surface that wants the whole picture
/// rather than the first refusal. `None` marks a check that did not run —
/// an unasked model, an uncapped key, an absent mandate.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementChecks {
    /// The key is live: unrevoked and unexpired.
    pub key_active: bool,
    /// The organization is not suspended.
    pub org_suspended: bool,
    /// The key's allowlist admits the requested model; `None` when none was asked.
    pub model_allowed: Option<bool>,
    /// The request window has a free slot; `None` when the key is uncapped.
    pub rpm_ok: Option<bool>,
    /// The token window fits the estimate; `None` when uncapped or unasked.
    pub tpm_ok: Option<bool>,
    /// Committed plus the estimate sits under the tightest cap in force — the
    /// key's own, the minting member's, or that member's team's; `None` when
    /// no cap applies at all.
    pub budget_ok: Option<bool>,
    /// Open holds sit under the cap; `None` when uncapped.
    pub holds_ok: Option<bool>,
    /// The presented mandate's window, model and cap all pass; `None` when
    /// no mandate was presented.
    pub mandate_ok: Option<bool>,
}

/// The entitlement answer: `allowed` plus the refusing `reason` and the
/// headroom each checked constraint leaves.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entitlement {
    /// Whether a request with these parameters would be admitted now.
    pub allowed: bool,
    /// Which check said no, when `allowed` is false.
    pub reason: Option<EntitlementReason>,
    /// Free request slots in the key's rolling minute; `None` while the window
    /// itself refuses or the key is uncapped.
    pub remaining_rpm: Option<u32>,
    /// Settled-token headroom inside the rolling minute against the estimate;
    /// `None` when the key is uncapped.
    pub remaining_tpm: Option<i64>,
    /// The tightest spend cap minus committed spend — the key's own limit
    /// folded with the minting member's and team's; `None` when no cap applies.
    pub remaining_budget_minor: Option<i64>,
    /// How many holds the key has outstanding now.
    pub concurrent_holds: i64,
    /// When a refusing window frees — the `Retry-After` figure.
    pub retry_after_secs: Option<u64>,
    /// The per-constraint verdict.
    pub checks: EntitlementChecks,
}

/// The request-window verdict, computed by the route against the server's
/// limiter — the one check that lives outside both the ledger and the `oxsum`
/// tables.
#[derive(Debug, Clone, Copy)]
pub struct RpmVerdict {
    /// The cap the window was judged against.
    pub limit: u32,
    /// Free slots inside it now (the peek consumes nothing).
    pub remaining: u32,
    /// Whether one more request fits.
    pub allowed: bool,
    /// When a refusing window frees, seconds.
    pub retry_after_secs: Option<u64>,
}

/// The token-window verdict, computed by the route against
/// `Db::tpm_window`/`tpm_window_scoped` — `settled + estimate <= cap`, the
/// same inequality admission runs.
#[derive(Debug, Clone, Copy)]
pub struct TpmVerdict {
    /// Settled tokens inside the trailing minute already.
    pub settled: i64,
    /// The tightest cap the estimate was judged against — the plain window's,
    /// or a tag bucket's when scopes pin one.
    pub limit: i64,
    /// Whether `settled + estimate` fits.
    pub allowed: bool,
    /// When the oldest in-window row ages out, seconds.
    pub retry_after_secs: u64,
}

/// The mandate verdict the ledger read arrived at, when one was presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MandateVerdict {
    /// Window, model and cap all pass.
    Ok,
    /// The blob cannot verify, or names a different key.
    Invalid,
    /// Outside `notBefore`/`notAfter`; the payload is seconds until it opens
    /// (`0` once lapsed — the refusal is the verdict, not the wait).
    NotYetOrExpired(u64),
    /// The delegation's model subset does not name the requested model.
    ModelRefused,
    /// `committed + estimate > cap`; the payload is the cap's own headroom.
    CapExceeded(i64),
}

/// What [`crate::Wallet::entitlement_read`] gathered — every ledger-side
/// verdict plus the limits the route's own checks still need (RPM and TPM
/// live outside the wallet: in-process limiter, `oxsum` tables).
#[derive(Debug, Clone)]
pub struct EntitlementRead {
    /// The key is live: unrevoked and unexpired.
    pub key_active: bool,
    /// The organization is suspended.
    pub org_suspended: bool,
    /// The key's allowlist admits the requested model; `None` when none was asked.
    pub model_allowed: Option<bool>,
    /// The tightest spend cap's headroom — the key's limit folded with the
    /// member's and team's — in minor units; `None` when no cap applies.
    pub remaining_budget_minor: Option<i64>,
    /// Committed plus the estimate sits under every cap in force; `None` when
    /// no cap applies at all.
    pub budget_ok: Option<bool>,
    /// How many holds the key has outstanding now.
    pub open_holds: i64,
    /// The key's outstanding-holds cap; `None` is uncapped.
    pub max_concurrent_holds: Option<i32>,
    /// The key's rolling-minute request allowance; `None` is uncapped. The
    /// route checks it against the server's limiter.
    pub requests_per_minute: Option<i32>,
    /// The key's rolling-minute settled-token allowance; `None` is uncapped.
    /// The route checks it against `Db::tpm_window`.
    pub tokens_per_minute: Option<i32>,
    /// The tag-scoped token buckets the key carries, resolved to their scope
    /// names where the request's tags named a value; each entry is
    /// `(scope_name, cap)` for `Db::tpm_window_scoped`. Tag scopes the request
    /// does not tag into are not buckets it would land in — admission skips
    /// them the same way.
    pub tpm_scopes: Vec<(String, i64)>,
    /// The presented mandate's verdict, when one was presented.
    pub mandate: Option<MandateVerdict>,
}

/// What the ledger read needs from the presented mandate, when one was
/// verified upstream: the id the committed sum keys on, the window bounds and
/// the cap. The route already ran [`crate::mandates::parse_presented`]; the
/// fields it hands over are the verified ones, never the raw blob's claims.
#[derive(Debug, Clone)]
pub struct EntitlementMandate {
    /// The mandate's stable id — the `provenance_correlation` its spend keys on.
    pub mandate_id: Uuid,
    /// The key the mandate delegates — the read refuses `mandate_invalid` when
    /// it is not the key being asked about.
    pub key_id: Uuid,
    /// The window bound, unix seconds.
    pub not_before_ts: i64,
    /// The window bound, unix seconds.
    pub not_after_ts: i64,
    /// The delegation's own committed-spend cap, minor units.
    pub cap_minor: i64,
    /// The delegation's own model subset; `None` delegates every model the key serves.
    pub models: Option<Vec<String>>,
}

impl Entitlement {
    /// Assemble the answer: the ledger read, the request window and the token
    /// window folded into one verdict.
    ///
    /// The refusing `reason` is the first failing check in the documented
    /// order — key state, capability, the rate windows, money, concurrency,
    /// the delegation — so a caller fixing one refusal sees the next one the
    /// order would have reached, not an arbitrary one of the set.
    #[must_use]
    pub fn compose(
        read: EntitlementRead,
        rpm: Option<RpmVerdict>,
        tpm: Option<TpmVerdict>,
    ) -> Self {
        let checks = EntitlementChecks {
            key_active: read.key_active,
            org_suspended: read.org_suspended,
            model_allowed: read.model_allowed,
            rpm_ok: rpm.map(|v| v.allowed),
            tpm_ok: tpm.map(|v| v.allowed),
            budget_ok: read.budget_ok,
            holds_ok: read
                .max_concurrent_holds
                .map(|cap| read.open_holds < i64::from(cap)),
            mandate_ok: read.mandate.map(|v| v == MandateVerdict::Ok),
        };
        let remaining_rpm = rpm.and_then(|v| v.allowed.then_some(v.remaining));
        let remaining_tpm = tpm.map(|v| (v.limit - v.settled).max(0));
        // The first failing check in the documented order is the answer's
        // `reason`; an allowed request reaches the end with none.
        let (reason, retry_after_secs) = if !read.key_active {
            (Some(EntitlementReason::KeyInactive), None)
        } else if read.org_suspended {
            (Some(EntitlementReason::OrgSuspended), None)
        } else if read.model_allowed == Some(false) {
            (Some(EntitlementReason::ModelNotAllowed), None)
        } else if let Some(v) = rpm
            && !v.allowed
        {
            (Some(EntitlementReason::RpmExceeded), v.retry_after_secs)
        } else if let Some(v) = tpm
            && !v.allowed
        {
            (
                Some(EntitlementReason::TpmExceeded),
                Some(v.retry_after_secs),
            )
        } else if read.budget_ok == Some(false) {
            (Some(EntitlementReason::BudgetExhausted), None)
        } else if checks.holds_ok == Some(false) {
            (Some(EntitlementReason::ConcurrentHolds), None)
        } else {
            match read.mandate {
                Some(MandateVerdict::Invalid) => (Some(EntitlementReason::MandateInvalid), None),
                Some(MandateVerdict::NotYetOrExpired(secs)) => {
                    (Some(EntitlementReason::MandateWindow), Some(secs))
                }
                Some(MandateVerdict::ModelRefused) => (Some(EntitlementReason::MandateModel), None),
                Some(MandateVerdict::CapExceeded(_)) => (Some(EntitlementReason::MandateCap), None),
                _ => (None, None),
            }
        };
        // A delegation-cap refusal reports the mandate's own headroom — the
        // figure the delegate can size by — not the key's.
        let remaining_budget_minor = match read.mandate {
            Some(MandateVerdict::CapExceeded(headroom))
                if reason == Some(EntitlementReason::MandateCap) =>
            {
                Some(headroom)
            }
            _ => read.remaining_budget_minor,
        };
        Self {
            allowed: reason.is_none(),
            reason,
            remaining_rpm,
            remaining_tpm,
            remaining_budget_minor,
            concurrent_holds: read.open_holds,
            retry_after_secs,
            checks,
        }
    }
}

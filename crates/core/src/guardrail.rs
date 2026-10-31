//! The guardrail pipeline (issue #502): policy hooks that inspect a request
//! before upstream sees it (`pre_call`, before the hold is taken) and a
//! response before the client sees it (`post_call`, before settlement).
//!
//! A [`Guardrail`] is a named, org-configured rule — a keyword list, the
//! built-in PII scan, or an external moderation endpoint — answering a
//! [`Verdict`]: `Allow`, `Flag` (record the turn's exposure in the settlement
//! description, bill normally) or `Block` (refuse before money moves / refund
//! the hold after it lands). The trait is synchronous and cheap enough to run
//! inside the admit path; the one impl that needs I/O (the moderation callout)
//! bridges to async the way `PostgresRateLimiter` does — every call site sits
//! inside axum's runtime.
//!
//! Which policies a turn runs is the union of three bindings, each a set of
//! policy *names* resolving to rows in `oxsum.guardrail_policies`: the
//! channel's `guardrail_bindings` row (scope `channel`), the key's own
//! `required_policies` column, and a presented mandate's — which may only
//! *narrow* the key's list, minted as a subset so a delegate can tighten its
//! own calls, never loosen them. A name that does not resolve is a write-time
//! error, never a silent unguard.
//!
//! This is a protection layer, not a billing one — like `requestsPerMinute` it
//! fails open where a refusal would be worse than the traffic it guards
//! against: a response-path evaluation that errors still settles the turn the
//! upstream already answered. The one fail-closed edge is `pre_call` on the
//! moderation callout: a screening endpoint that is down must not admit an
//! unscreened prompt, and refusing before the hold costs the caller nothing.

use std::sync::Arc;

use regex::RegexSet;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// A `guardrail_policies` row as the admin surface and the registry read it.
/// `config` is each kind's own shape — `keyword`'s `{patterns:[{re,action}]}`,
/// `pii`'s empty object, `moderation`'s `{endpoint, timeout_ms?}` — and
/// `secret_sealed` carries a `moderation` policy's bearer, sealed under the
/// deployment's `SecretKey` exactly the way an upstream key is. It never
/// serializes: a row leaves the server without its secret half.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardrailPolicy {
    /// The policy's name — what bindings and `requiredPolicies` reference.
    pub name: String,
    /// `keyword`, `pii` or `moderation` — which impl `config` drives.
    pub kind: String,
    /// The kind's own configuration object.
    pub config: serde_json::Value,
    /// The sealed bearer a `moderation` policy's callout sends upstream;
    /// `None` on the other kinds (they keep no secret). `skip` keeps it out of
    /// every serialized form — the admin list must not echo ciphertext either.
    #[serde(skip)]
    pub secret_sealed: Option<String>,
    /// When the policy was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

/// A `guardrail_bindings` row: one scope's ordered set of policy names.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardrailBinding {
    /// `channel` or `key`.
    pub scope: String,
    /// The channel name, or the key id's text.
    pub target: String,
    /// The ordered policy names the scope runs.
    pub policies: Vec<String>,
}

/// The `moderation` kind's `config` — where the screened content is posted.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModerationConfig {
    /// The endpoint the callout POSTs the request payload to.
    pub endpoint: String,
    /// How long a verdict may take before the call gives up — bounded, so a
    /// hung endpoint cannot stall admission. Defaults to the impl's own cap.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// The `keyword` kind's `config`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordConfig {
    /// The patterns the content is scanned over.
    pub patterns: Vec<KeywordPattern>,
}

/// The shape a `guardrail_policies` name must match — the column's own CHECK
/// expression, kept beside the table's types so every write path (a policy
/// PUT, a key's `requiredPolicies`, a binding's list) is rejected by the same
/// rule before the name reaches a lookup that can only fail to resolve.
pub fn validate_policy_names(policies: &[String]) -> Result<(), WalletError> {
    for name in policies {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c))
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !valid {
            return Err(WalletError::InvalidInput(format!(
                "{name:?} is not a guardrail policy name"
            )));
        }
    }
    Ok(())
}

impl Db {
    /// Every configured policy, ordered by name — the registry the app builds.
    pub async fn list_guardrail_policies(&self) -> Result<Vec<GuardrailPolicy>, WalletError> {
        let rows = sqlx::query(
            "SELECT name, kind, config, secret_sealed, created_at \
             FROM oxsum.guardrail_policies ORDER BY name",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(policy_from_row).collect()
    }

    /// Writes or replaces one policy. `secret_sealed` is the `moderation`
    /// kind's bearer already sealed under the deployment's `SecretKey` — the
    /// admin route seals it through the same path an upstream key takes, so
    /// this column only ever holds ciphertext, never the secret itself.
    pub async fn upsert_guardrail_policy(
        &self,
        name: &str,
        kind: &str,
        config: &serde_json::Value,
        secret_sealed: Option<&str>,
    ) -> Result<GuardrailPolicy, WalletError> {
        let row = sqlx::query(
            "INSERT INTO oxsum.guardrail_policies (name, kind, config, secret_sealed) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (name) DO UPDATE SET kind = $2, config = $3, secret_sealed = $4 \
             RETURNING name, kind, config, secret_sealed, created_at",
        )
        .bind(name)
        .bind(kind)
        .bind(config)
        .bind(secret_sealed)
        .fetch_one(self.pool())
        .await?;
        policy_from_row(&row)
    }

    /// Deletes a policy by name, answering whether it existed. Deleting a bound
    /// policy leaves the binding's dangling name — `for_turn` skips it rather
    /// than refuse, since a protection layer does not drop traffic over its
    /// own bookkeeping.
    pub async fn delete_guardrail_policy(&self, name: &str) -> Result<bool, WalletError> {
        let result = sqlx::query("DELETE FROM oxsum.guardrail_policies WHERE name = $1")
            .bind(name)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Every binding, ordered — the admin surface's list.
    pub async fn list_guardrail_bindings(&self) -> Result<Vec<GuardrailBinding>, WalletError> {
        let rows = sqlx::query(
            "SELECT scope, target, policies FROM oxsum.guardrail_bindings \
             ORDER BY scope, target",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(binding_from_row).collect())
    }

    /// Replaces one scope's binding. Every name must resolve to a stored
    /// policy — a binding that names nothing would silently unguard the turns
    /// it claims to protect, so the write refuses before it lands.
    pub async fn set_guardrail_binding(
        &self,
        scope: &str,
        target: &str,
        policies: &[String],
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        for name in policies {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM oxsum.guardrail_policies WHERE name = $1)",
            )
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                tx.rollback().await?;
                return Err(WalletError::InvalidInput(format!(
                    "guardrail policy {name:?} does not exist"
                )));
            }
        }
        sqlx::query(
            "INSERT INTO oxsum.guardrail_bindings (scope, target, policies) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (scope, target) DO UPDATE SET policies = $3",
        )
        .bind(scope)
        .bind(target)
        .bind(policies)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes one scope's binding, answering whether it existed.
    pub async fn delete_guardrail_binding(
        &self,
        scope: &str,
        target: &str,
    ) -> Result<bool, WalletError> {
        let result =
            sqlx::query("DELETE FROM oxsum.guardrail_bindings WHERE scope = $1 AND target = $2")
                .bind(scope)
                .bind(target)
                .execute(self.pool())
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Refuses a write whose `required_policies` names resolve to nothing: a
    /// key or mandate may only demand a policy the org actually configured, or
    /// the name would sit on the credential unguarding every turn it claims to
    /// protect. `None`/empty passes — requiring no policy is always valid.
    pub async fn require_guardrail_policies(
        &self,
        policies: &Option<Vec<String>>,
    ) -> Result<(), WalletError> {
        let Some(policies) = policies else {
            return Ok(());
        };
        for name in policies {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM oxsum.guardrail_policies WHERE name = $1)",
            )
            .bind(name)
            .fetch_one(self.pool())
            .await?;
            if !exists {
                return Err(WalletError::InvalidInput(format!(
                    "guardrail policy {name:?} does not exist"
                )));
            }
        }
        Ok(())
    }

    /// Opens a moderation policy's bearer: `secret_sealed` opened under the
    /// deployment's [`crate::channels::SecretKey`] — the same seal-and-open
    /// story an upstream key takes, so the endpoint's secret is never stored
    /// or read in the clear. `Ok(None)` means the policy keeps none.
    pub fn open_guardrail_secret(
        policy: &GuardrailPolicy,
        secret: &crate::channels::SecretKey,
    ) -> Result<Option<String>, WalletError> {
        policy
            .secret_sealed
            .as_ref()
            .map(|s| secret.open(s))
            .transpose()
    }
}

/// Maps a policies row onto its type. `created_at` is read as the timestamp it
/// stores.
fn policy_from_row(row: &sqlx::postgres::PgRow) -> Result<GuardrailPolicy, WalletError> {
    use sqlx::Row;
    Ok(GuardrailPolicy {
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        config: row.try_get("config")?,
        secret_sealed: row.try_get("secret_sealed")?,
        created_at: row.try_get("created_at")?,
    })
}

fn binding_from_row(row: &sqlx::postgres::PgRow) -> GuardrailBinding {
    use sqlx::Row;
    GuardrailBinding {
        scope: row.get("scope"),
        target: row.get("target"),
        policies: row.get("policies"),
    }
}

/// What a policy answers for the content it inspected.
///
/// `Flag` and `Block` carry the policy's own name plus the reason it reports —
/// the pair the settlement description's `guardrail` field records, so a bill
/// proves which rule acted on it, not just that one did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The content raised nothing: the turn proceeds, recorded or not.
    Allow,
    /// The turn proceeds and bills normally, but the exposure is written into
    /// the settlement's `guardrail` field — the audit trail is the point of a
    /// flag: an org watching for a pattern sees it without losing the traffic.
    Flag {
        /// The policy that flagged.
        policy: String,
        /// The rule's own reason, bounded by the caller before it lands in a
        /// ledger description.
        reason: String,
    },
    /// The content is refused. On `pre_call` the turn never takes a hold — a
    /// blocked request costs nothing and writes only the `refused` marker; on
    /// `post_call` the upstream answer never reaches the client and the hold
    /// settles `Nothing`, the whole freeze returned.
    Block {
        /// The policy that refused.
        policy: String,
        /// The rule's own reason, bounded by the caller.
        reason: String,
    },
}

/// The request a `pre_call` policy inspects — the fields it may key a verdict
/// on, gathered where admission already resolved them.
#[derive(Debug)]
pub struct RequestContext<'a> {
    /// The organization the turn spends for.
    pub organization_id: Uuid,
    /// The key acting.
    pub key_id: Uuid,
    /// The model the caller asked for.
    pub model: &'a str,
    /// The request's text content, normalized across surfaces — every prompt
    /// string the policy's patterns run over.
    pub texts: &'a [String],
    /// The caller's `endUser` attribution, when sent.
    pub end_user: Option<&'a str>,
    /// The channel the turn will route to — the policy may bind to it.
    pub channel: &'a str,
}

/// The response a `post_call` policy inspects — the assembled upstream answer.
#[derive(Debug)]
pub struct ResponseContext<'a> {
    /// The organization the turn spends for.
    pub organization_id: Uuid,
    /// The key acting.
    pub key_id: Uuid,
    /// The model the caller asked for.
    pub model: &'a str,
    /// The channel that answered.
    pub channel: &'a str,
    /// The assembled answer text — the whole body on a non-streamed turn, the
    /// accumulated frames on a streamed one.
    pub output_text: &'a str,
    /// The request's texts, for a policy that judges the answer against the
    /// prompt that drew it.
    pub request_texts: &'a [String],
    /// Whether the answer streamed: the bytes already reached the client, so a
    /// `Block` here cannot un-send them — the verdict is recorded as a flag
    /// and the settlement carries `streamed: true` to say so.
    pub streamed: bool,
}

/// A single org-configured rule the pipeline evaluates.
///
/// Both hooks default to `Allow` so an impl answers only the phase it covers:
/// the PII scan and keyword policy evaluate both directions with the same
/// patterns; the moderation callout answers `pre_call` (the screened content
/// is the prompt) and leaves the response to the org's own endpoint if it adds
/// a response path later.
pub trait Guardrail: Send + Sync {
    /// The policy's name — the `guardrail_policies.name` it was built from,
    /// recorded into the verdict so the audit names the row that acted.
    fn name(&self) -> &str;

    /// Judges the request before upstream sees it. Called on `pre_call` —
    /// before the hold, so a `Block` costs nothing.
    fn evaluate_request(&self, _ctx: &RequestContext<'_>) -> Verdict {
        Verdict::Allow
    }

    /// Judges the assembled response before the client sees it — or, on a
    /// streamed turn, before it settles. Called on `post_call`.
    fn evaluate_response(&self, _ctx: &ResponseContext<'_>) -> Verdict {
        Verdict::Allow
    }
}

/// The policies a turn runs, resolved once at admission: the union of the
/// channel's binding, the key's `required_policies`, and a presented mandate's
/// narrowed subset, in evaluation order — channel policies first (the
/// operator's own rules lead), then the key's, then the mandate's.
///
/// Built at `app_with_billing` as the registry every impl registers into;
/// `for_turn` picks the turn's set out of it per request.
#[derive(Default)]
pub struct GuardrailSet {
    /// Every policy by name — the impls `for_turn` resolves bindings against.
    policies: std::collections::HashMap<String, Arc<dyn Guardrail>>,
    /// `scope`/`target` → the ordered policy names that scope runs.
    bindings: std::collections::HashMap<(String, String), Vec<String>>,
}

impl GuardrailSet {
    /// An empty set — the deployment with no policies configured admits and
    /// settles exactly as it did before the feature: the hooks are a no-op.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Registers one impl under its name.
    pub fn register(&mut self, policy: Arc<dyn Guardrail>) {
        self.policies.insert(policy.name().to_owned(), policy);
    }

    /// Sets one scope's binding — `(scope, target)` → the ordered policy names.
    pub fn bind(&mut self, scope: &str, target: &str, policies: Vec<String>) {
        self.bindings
            .insert((scope.to_owned(), target.to_owned()), policies);
    }

    /// The impls a turn runs, in order: the channel's binding first, then the
    /// key's `required_policies`, then the mandate's. Names that resolve to no
    /// impl are skipped — a binding is validated on write, so a miss here means
    /// the policy was deleted after binding, and a protection layer skips it
    /// rather than refuse the turn over its own bookkeeping.
    #[must_use]
    pub fn for_turn(
        &self,
        channel: &str,
        key_id: Uuid,
        key_required: &[String],
        mandate_required: &[String],
    ) -> Vec<Arc<dyn Guardrail>> {
        let mut names: Vec<&str> = Vec::new();
        // Channel-bound policies, in their stored order.
        if let Some(bound) = self
            .bindings
            .get(&("channel".to_owned(), channel.to_owned()))
        {
            names.extend(bound.iter().map(String::as_str));
        }
        // The key binding row, when one exists, then the key's own column —
        // both are the key's requirement; the column is where PATCH writes it.
        if let Some(bound) = self
            .bindings
            .get(&("key".to_owned(), key_id.as_simple().to_string()))
        {
            names.extend(bound.iter().map(String::as_str));
        }
        names.extend(key_required.iter().map(String::as_str));
        names.extend(mandate_required.iter().map(String::as_str));
        // De-duplicate preserving order — a policy bound on both the channel
        // and the key runs once, not twice.
        let mut seen = std::collections::HashSet::new();
        names
            .into_iter()
            .filter(|name| seen.insert(*name))
            .filter_map(|name| self.policies.get(name).cloned())
            .collect()
    }
}

/// A single keyword/regex rule: the pattern, and whether a hit flags or blocks.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordPattern {
    /// The regex the content is scanned with.
    pub re: String,
    /// `flag` or `block` — the verdict a hit produces.
    pub action: String,
}

/// The org-configured keyword/regex policy: a named list of patterns the
/// request texts and the response text are scanned over. A `block` hit wins
/// over any `flag` hit — the most restrictive verdict one content produces is
/// the one that acts.
pub struct KeywordPolicy {
    name: String,
    /// All patterns in one `RegexSet` for a single-pass scan; `actions` is
    /// aligned by index so `RegexSet::matches` reports which rules hit.
    set: RegexSet,
    actions: Vec<String>,
    /// The pattern strings, for the reason a hit reports.
    patterns: Vec<String>,
}

impl KeywordPolicy {
    /// Compiles the policy's patterns; an unparseable regex is a config error
    /// the write path surfaces, never a stored policy that fails at scan time.
    pub fn compile(name: &str, patterns: &[KeywordPattern]) -> Result<Self, crate::WalletError> {
        if patterns.is_empty() {
            return Err(crate::WalletError::InvalidInput(format!(
                "keyword policy {name} needs at least one pattern"
            )));
        }
        let res: Vec<String> = patterns.iter().map(|p| p.re.clone()).collect();
        let set = RegexSet::new(&res).map_err(|error| {
            crate::WalletError::InvalidInput(format!("keyword policy {name}: {error}"))
        })?;
        let actions = patterns.iter().map(|p| p.action.clone()).collect();
        Ok(Self {
            name: name.to_owned(),
            set,
            actions,
            patterns: res,
        })
    }

    /// Scans one text: the most restrictive verdict any pattern produces.
    fn scan(&self, text: &str) -> Verdict {
        let hits = self.set.matches(text);
        let mut flag_reason: Option<String> = None;
        for index in hits.iter() {
            match self.actions[index].as_str() {
                "block" => {
                    return Verdict::Block {
                        policy: self.name.clone(),
                        reason: format!("matched /{}/", self.patterns[index]),
                    };
                }
                "flag" => {
                    flag_reason
                        .get_or_insert_with(|| format!("matched /{}/", self.patterns[index]));
                }
                _ => {}
            }
        }
        match flag_reason {
            Some(reason) => Verdict::Flag {
                policy: self.name.clone(),
                reason,
            },
            None => Verdict::Allow,
        }
    }
}

impl Guardrail for KeywordPolicy {
    fn name(&self) -> &str {
        &self.name
    }

    fn evaluate_request(&self, ctx: &RequestContext<'_>) -> Verdict {
        ctx.texts
            .iter()
            .map(|text| self.scan(text))
            .max_by_key(verdict_rank)
            .unwrap_or(Verdict::Allow)
    }

    fn evaluate_response(&self, ctx: &ResponseContext<'_>) -> Verdict {
        self.scan(ctx.output_text)
    }
}

/// The built-in PII scan (response path): a fixed set of patterns matching
/// common personal identifiers in the answer text. v1 flags, never rewrites —
/// a regex redaction inside a JSON completion's string values risks corrupting
/// the payload the client parses, so the honest answer is to record the
/// exposure and let the org's own downstream scrub.
///
/// The patterns are deliberately narrow and documented: each is a common
/// identifier shape, not a heuristic that would drown the audit in noise.
pub struct PiiScanner {
    name: String,
    set: RegexSet,
    labels: Vec<&'static str>,
}

/// The identifier shapes the scan knows, `(label, regex)` pairs. Kept tight:
/// an over-broad pattern flags every answer and the flag stops meaning
/// anything — the same reason a scrub is honest rather than aggressive.
const PII_PATTERNS: &[(&str, &str)] = &[
    ("email", r"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}"),
    ("us-ssn", r"\b\d{3}-\d{2}-\d{4}\b"),
    (
        "payment-card",
        r"\b(?:4\d{3}|5[1-5]\d{2}|3[47]\d{2}|6(?:011|5\d{2}))[- ]?\d{4}[- ]?\d{4}[- ]?\d{4}\b",
    ),
    (
        "us-phone",
        r"\b\+?1[-. ]?\(?\d{3}\)?[-. ]?\d{3}[-. ]?\d{4}\b",
    ),
];

impl PiiScanner {
    /// The scan's own name under `guardrail_policies` — a `pii` policy's config
    /// carries nothing, the patterns are the built-in set above.
    pub fn new(name: &str) -> Self {
        let (labels, res): (Vec<&'static str>, Vec<&'static str>) =
            PII_PATTERNS.iter().copied().unzip();
        let set = match RegexSet::new(res) {
            Ok(set) => set,
            // The patterns are literals this module maintains — a build that
            // cannot compile them has a bug a panic should name, not a
            // recoverable error a caller could fix.
            Err(error) => unreachable!("the built-in PII patterns compile: {error}"),
        };
        Self {
            name: name.to_owned(),
            set,
            labels,
        }
    }
}

impl Guardrail for PiiScanner {
    fn name(&self) -> &str {
        &self.name
    }

    fn evaluate_response(&self, ctx: &ResponseContext<'_>) -> Verdict {
        let hits = self.set.matches(ctx.output_text);
        if !hits.matched_any() {
            return Verdict::Allow;
        }
        let labels: Vec<&str> = hits.iter().map(|i| self.labels[i]).collect();
        Verdict::Flag {
            policy: self.name.clone(),
            reason: format!("possible PII in the answer ({})", labels.join(", ")),
        }
    }
}

/// Orders verdicts for `max_by_key` — a `Block` outranks a `Flag`, which
/// outranks `Allow`, so scanning several texts answers the most restrictive
/// verdict any of them produced.
fn verdict_rank(verdict: &Verdict) -> u8 {
    match verdict {
        Verdict::Allow => 0,
        Verdict::Flag { .. } => 1,
        Verdict::Block { .. } => 2,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn texts(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn request<'a>(texts: &'a [String]) -> RequestContext<'a> {
        RequestContext {
            organization_id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
            model: "m",
            texts,
            end_user: None,
            channel: "c",
        }
    }

    #[test]
    fn a_keyword_block_beats_a_flag() {
        let policy = KeywordPolicy::compile(
            "p",
            &[
                KeywordPattern {
                    re: "alpha".into(),
                    action: "flag".into(),
                },
                KeywordPattern {
                    re: "beta".into(),
                    action: "block".into(),
                },
            ],
        )
        .unwrap();
        // Both hit: the block verdict is the one that acts.
        let both = texts(&["alpha and beta"]);
        assert!(matches!(
            policy.evaluate_request(&request(&both)),
            Verdict::Block { .. }
        ));
        let flagged = texts(&["only alpha"]);
        assert!(matches!(
            policy.evaluate_request(&request(&flagged)),
            Verdict::Flag { .. }
        ));
        let clean = texts(&["nothing here"]);
        assert_eq!(policy.evaluate_request(&request(&clean)), Verdict::Allow);
    }

    #[test]
    fn a_pii_scan_flags_identifiers() {
        let scanner = PiiScanner::new("pii");
        let ctx = ResponseContext {
            organization_id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
            model: "m",
            channel: "c",
            output_text: "call me at jane@example.com",
            request_texts: &[],
            streamed: false,
        };
        assert!(matches!(
            scanner.evaluate_response(&ctx),
            Verdict::Flag { .. }
        ));
        let ctx = ResponseContext {
            output_text: "no identifiers here",
            ..ctx
        };
        assert_eq!(scanner.evaluate_response(&ctx), Verdict::Allow);
    }
}

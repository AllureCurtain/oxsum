//! The server half of the guardrail pipeline (issue #502): the `moderation`
//! policy's HTTP callout — the one impl that needs I/O, living here where the
//! shared `reqwest::Client` is — plus the registry builder that turns
//! `oxsum.guardrail_policies` rows into the `GuardrailSet` admission resolves.
//!
//! The callout's fail doctrine splits by phase, matching the module docs: a
//! `pre_call` evaluation that cannot reach the endpoint **fails closed** — a
//! screening service that is down must not admit an unscreened prompt, and a
//! refusal before the hold costs the caller nothing. A `post_call` evaluation
//! that errors **fails open** — the upstream answer already arrived and is
//! paid for, so an endpoint outage must not eat a settled turn. Both log; the
//! verdict is never silent.

use std::sync::Arc;
use std::time::Duration;

use oxsum_core::{
    Db, Guardrail, GuardrailPolicy, GuardrailSet, KeywordConfig, KeywordPolicy, ModerationConfig,
    PiiScanner, RequestContext, ResponseContext, Verdict, WalletError,
};

/// The longest a moderation verdict may take: bounded so a hung endpoint
/// cannot stall admission — `pre_call` runs inside the request's admit path.
const MODERATION_TIMEOUT_CAP: Duration = Duration::from_secs(10);
const MODERATION_TIMEOUT_DEFAULT: Duration = Duration::from_secs(5);

/// The external-moderation callout: posts the request's texts to the org's
/// configured endpoint and maps its JSON verdict.
///
/// The bearer rides on the policy row as `secret_sealed` — sealed beside the
/// upstream keys by the same `SecretKey` story and opened at build. Held as a
/// plain `String`, the way an opened upstream key is.
pub struct ModerationCallout {
    name: String,
    http: reqwest::Client,
    endpoint: String,
    bearer: String,
    timeout: Duration,
}

impl ModerationCallout {
    /// Builds the callout from its policy row's config plus the bearer the
    /// named credential opened to.
    #[must_use]
    pub fn new(
        name: &str,
        http: reqwest::Client,
        config: &ModerationConfig,
        bearer: String,
    ) -> Self {
        let timeout = config
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(MODERATION_TIMEOUT_DEFAULT)
            .min(MODERATION_TIMEOUT_CAP);
        Self {
            name: name.to_owned(),
            http,
            endpoint: config.endpoint.clone(),
            bearer,
            timeout,
        }
    }

    /// The verdict the endpoint answers for `texts`. `pre_call` semantics: an
    /// unreachable/timeout/error endpoint is `Block` — fail closed before the
    /// hold, where refusing is free. `post_call` shares the request's call but
    /// its error path is the caller's (`evaluate_response` fails open below).
    fn call(&self, payload: &serde_json::Value, phase: &str) -> Verdict {
        let send = async {
            self.http
                .post(&self.endpoint)
                .bearer_auth(&self.bearer)
                .timeout(self.timeout)
                .json(payload)
                .send()
                .await
        };
        // The trait is synchronous; the HTTP wait runs through the same
        // block_in_place bridge `PostgresRateLimiter` uses — every call site
        // sits inside axum's runtime.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::error!(policy = %self.name,
                "guardrail moderation ran outside a tokio runtime");
            return self.phase_error(phase);
        };
        let outcome = tokio::task::block_in_place(|| handle.block_on(send));
        match outcome {
            Ok(response) if response.status().is_success() => {
                let body = tokio::task::block_in_place(|| {
                    handle.block_on(response.json::<serde_json::Value>())
                });
                match body {
                    Ok(body) => verdict_from_moderation(&self.name, &body),
                    Err(error) => {
                        tracing::error!(policy = %self.name, %error,
                            "guardrail moderation body did not read");
                        self.phase_error(phase)
                    }
                }
            }
            Ok(response) => {
                tracing::error!(policy = %self.name, status = %response.status(),
                    "guardrail moderation endpoint refused");
                self.phase_error(phase)
            }
            Err(error) => {
                tracing::error!(policy = %self.name, %error,
                    "guardrail moderation endpoint unreachable");
                self.phase_error(phase)
            }
        }
    }

    /// The phase-splitting error verdict: `pre_call` fails closed (a screening
    /// endpoint down must not admit an unscreened prompt — refusing before the
    /// hold costs nothing), `post_call` fails open (the answer arrived and is
    /// paid for; an outage must not eat the settled turn).
    fn phase_error(&self, phase: &str) -> Verdict {
        if phase == "pre_call" {
            Verdict::Block {
                policy: self.name.clone(),
                reason: "moderation endpoint could not answer; refusing unscreened".to_owned(),
            }
        } else {
            Verdict::Allow
        }
    }
}

/// Maps a moderation endpoint's JSON `{verdict, categories?, reason?}` onto a
/// [`Verdict`]. An unrecognized verdict is an `Allow` — a misbehaving endpoint
/// must not block traffic on its own malformed answer.
fn verdict_from_moderation(name: &str, body: &serde_json::Value) -> Verdict {
    let reason = body
        .get("reason")
        .and_then(|r| r.as_str())
        .or_else(|| body.get("categories").and_then(|c| c.as_str()))
        .unwrap_or("moderation verdict")
        .chars()
        .take(150)
        .collect::<String>();
    match body.get("verdict").and_then(|v| v.as_str()) {
        Some("flag") => Verdict::Flag {
            policy: name.to_owned(),
            reason,
        },
        Some("block") => Verdict::Block {
            policy: name.to_owned(),
            reason,
        },
        _ => Verdict::Allow,
    }
}

impl Guardrail for ModerationCallout {
    fn name(&self) -> &str {
        &self.name
    }

    fn evaluate_request(&self, ctx: &RequestContext<'_>) -> Verdict {
        let payload = serde_json::json!({
            "model": ctx.model,
            "texts": ctx.texts,
            "endUser": ctx.end_user,
        });
        self.call(&payload, "pre_call")
    }

    fn evaluate_response(&self, ctx: &ResponseContext<'_>) -> Verdict {
        let payload = serde_json::json!({
            "model": ctx.model,
            "texts": [ctx.output_text],
        });
        self.call(&payload, "post_call")
    }
}

/// Builds the `GuardrailSet` the app's state carries from the stored policies
/// and bindings (issue #502). Called per request inside `guardrails_for`: a
/// policy's rows are read into impls, bindings folded into the registry's
/// scope map. `secret` opens each `moderation` policy's `secret_sealed` the
/// way a channel's upstream key opens — `None` (a deployment with no
/// configured secret) means nothing can open, so a moderation policy is
/// skipped.
pub async fn build_guardrail_set(
    db: &Db,
    http: reqwest::Client,
    secret: Option<&oxsum_core::SecretKey>,
) -> Result<GuardrailSet, WalletError> {
    let mut set = GuardrailSet::empty();
    for row in db.list_guardrail_policies().await? {
        match build_policy(&row, http.clone(), secret) {
            Ok(Some(policy)) => set.register(policy),
            Ok(None) => {}
            Err(error) => {
                // A policy that cannot build — a bad regex, an unopenable
                // secret — logs and is skipped rather than refuse the whole
                // app's build: one misconfigured rule must not take the
                // gateway down, the same fail-open the runtime verdicts keep.
                tracing::error!(%error, policy = %row.name,
                    "a guardrail policy failed to build and was skipped");
            }
        }
    }
    for binding in db.list_guardrail_bindings().await? {
        set.bind(&binding.scope, &binding.target, binding.policies);
    }
    Ok(set)
}

/// Builds one policy row into its impl; `Ok(None)` for a kind this build does
/// not know, or a `moderation` policy whose secret cannot open (skipped).
fn build_policy(
    row: &GuardrailPolicy,
    http: reqwest::Client,
    secret: Option<&oxsum_core::SecretKey>,
) -> Result<Option<Arc<dyn Guardrail>>, WalletError> {
    match row.kind.as_str() {
        "keyword" => {
            let config: KeywordConfig = serde_json::from_value(row.config.clone())
                .map_err(|e| WalletError::InvalidInput(format!("keyword config: {e}")))?;
            Ok(Some(Arc::new(KeywordPolicy::compile(
                &row.name,
                &config.patterns,
            )?)))
        }
        "pii" => Ok(Some(Arc::new(PiiScanner::new(&row.name)))),
        "moderation" => {
            let config: ModerationConfig = serde_json::from_value(row.config.clone())
                .map_err(|e| WalletError::InvalidInput(format!("moderation config: {e}")))?;
            let (Some(secret), Some(_)) = (secret, row.secret_sealed.as_ref()) else {
                // A moderation policy whose bearer cannot open — no configured
                // secret, or none was ever sealed — cannot screen, so it is
                // skipped rather than admitted unguarded.
                return Ok(None);
            };
            let bearer = Db::open_guardrail_secret(row, secret)?.unwrap_or_default();
            Ok(Some(Arc::new(ModerationCallout::new(
                &row.name, http, &config, bearer,
            ))))
        }
        _ => {
            tracing::warn!(kind = %row.kind, policy = %row.name,
                "a guardrail policy of an unknown kind was skipped");
            Ok(None)
        }
    }
}

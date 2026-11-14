//! The Solana deposit rail's chain-facing client (issue #1117): a narrow
//! JSON-RPC client that finds and validates the transfer that pays for a
//! top-up intent, plus the [`SolanaConfig`] the deployment states.
//!
//! The verification recipe is Solana Pay's: every intent mints a unique
//! `reference` account (a throwaway keypair's address) that the paying
//! transaction must carry as a read-only, non-signer account key. The server
//! then finds the transaction that references it
//! (`getSignaturesForAddress(reference)`) and validates it pays what the
//! intent declared (`getTransaction(signature, jsonParsed)`): the recipient
//! is the configured deposit address, the delivered amount meets the asked
//! amount in native lamports (or the exact SPL token amount for the
//! configured mint), and — for an SPL top-up — the mint is the configured
//! one, so a worthless-token stand-in is refused.
//!
//! The client deliberately speaks only JSON-RPC over `reqwest` — no
//! `solana-sdk` dependency — because the rail needs exactly two calls and a
//! handful of response fields; the binary keeps its single-process-plus-
//! Postgres shape.
//!
//! The client is a pool, not a point (issue #1145): `OXSUM_SOLANA_RPC_URLS`
//! names one or more endpoints (comma-separated, or the singular
//! `OXSUM_SOLANA_RPC_URL` spelled the same way) and every RPC call walks them
//! starting at a round-robin cursor. A transport failure, a non-2xx or an
//! RPC-level `error` marks the endpoint down for a short cooldown and the
//! call retries the next endpoint, so a dead provider does not eat the
//! per-call timeout on every pass; when the pool has a second endpoint the
//! failover is counted under `solana_rpc_failovers_total`. Providers that key
//! auth in the URL (Helius, Triton) need no further credential — the key
//! never leaves the URL, and the metrics label only ever carries the
//! endpoint's host. A provider that authenticates by header sets
//! `OXSUM_SOLANA_RPC_AUTH_HEADER`, applied to every endpoint's calls.
//!
//! With two or more endpoints a *credit-grade* answer is cross-confirmed
//! (issue #1183): `validate_transfer` re-fetches the same `getTransaction` on
//! a second healthy endpoint and both must report the same successful
//! transaction delivering the same amount to the same recipient on the same
//! mint. A disagreement or a sibling that cannot answer is not a credit
//! basis — the deposit stays `pending` — because failover alone only proves
//! *some* node answered, never that its answer is true. `find_reference`
//! needs no second opinion of its own: the signatures it returns are only
//! candidates, and the invented-signature case fails the cross-confirmed
//! `getTransaction` that follows. A single-endpoint pool keeps the lone
//! answer — `solana_of` warns at boot when that endpoint is mainnet-shaped.
//!
//! What is *not* verified here, on the record: `finalized` is the default
//! commitment (issue #1129) — the mainnet-grade setting; a devnet loop sets
//! `OXSUM_SOLANA_COMMITMENT=confirmed` explicitly for latency. No
//! watcher holds a subscription open — the periodic job polls.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The classic SPL Token program id — the only owner a whitelisted mint may
/// carry (issue #1182). The boot's mint audit refuses any other owner:
/// Token-2022 (`oxsum_core::TOKEN_2022_PROGRAM_ID`, still named explicitly so
/// an operator knows *why*) and every other program outright — an account
/// that is not a classic SPL mint cannot be trusted to price a deposit.
pub const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// The classic SPL mint account's byte length: mintAuthority option+key (36)
/// plus supply (8) plus decimals (1) plus isInitialized (1) plus
/// freezeAuthority option+key (36). A mint account that is not exactly this
/// long is not the layout the rail parses — the audit refuses it rather than
/// reading `decimals`/`isInitialized` off shifted bytes (issue #1182).
const SPL_MINT_DATA_LEN: usize = 82;

/// The classic SPL mint layout's `isInitialized` flag offset.
const SPL_MINT_IS_INITIALIZED_OFFSET: usize = 45;

/// Standard base64 decode for the `getAccountInfo` `data` field — a thin
/// wrapper so the mint audit names its error instead of unwrapping a library
/// type.
fn base64_decode(raw: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| e.to_string())
}

/// One payable asset on the whitelist (issue #1140): the key the open
/// request names and the SPL mint a top-up in it must pay — `None` for
/// native SOL, which is the only key allowed a `None` mint. The whitelist's
/// per-asset decimals and rate live in `oxsum_core::AssetTerms` — the client
/// itself only needs the mint list to read on-chain decimals at boot.
#[derive(Debug, Clone)]
pub struct MintWhitelistEntry {
    /// The asset's lowercase key (`sol`, `usdc`, …) — the
    /// `OXSUM_SOLANA_ASSETS` entry and the `OXSUM_SOLANA_MINT_<KEY>` suffix.
    pub key: String,
    /// The SPL mint the asset pays, or `None` for native SOL.
    pub mint: Option<String>,
}

/// The deployment's Solana acceptance criteria, read from the
/// `OXSUM_SOLANA_*` environment set. `Config` holds it; the client reads only
/// what its RPC calls need — the endpoint pool, commitment, the `payTo`
/// reference and `mints`, the whitelist the boot audits on-chain. The
/// per-asset figures the intents lock (`asset_decimals`, `rate_micro`, the
/// open bounds) live on `oxsum_core::SolanaTerms`, which the server hands the
/// verifier beside this client.
#[derive(Clone)]
pub struct SolanaConfig {
    /// The JSON-RPC endpoints the client pools over,
    /// `OXSUM_SOLANA_RPC_URLS` (or `OXSUM_SOLANA_RPC_URL` comma-separated) —
    /// devnet (`https://api.devnet.solana.com`) in the first cut; a mainnet
    /// deployment names its authenticated providers (Helius, Triton) here,
    /// with the API key inside each URL. Order is a preference hint, not a
    /// guarantee — failover order starts at the round-robin cursor.
    pub rpc_urls: Vec<String>,
    /// The deposit address every intent pays, `OXSUM_SOLANA_PAY_TO` (base58
    /// public key). For an SPL top-up this is the owner whose associated token
    /// account receives the mint — one vault receives every asset.
    pub pay_to: String,
    /// The mint whitelist `OXSUM_SOLANA_ASSETS` + `OXSUM_SOLANA_MINT_<KEY>`
    /// declare (issue #1140): every mint the boot verifies on-chain — its
    /// decimals are authoritative and its owner program must be the classic
    /// SPL Token program, never Token-2022.
    pub mints: Vec<MintWhitelistEntry>,
    /// The commitment `find_reference` / `validate_transfer` ask the RPC for:
    /// `finalized` (default) or `confirmed` (`OXSUM_SOLANA_COMMITMENT`).
    pub commitment: String,
    /// An auth header applied to every endpoint's calls
    /// (`OXSUM_SOLANA_RPC_AUTH_HEADER`, e.g. `Authorization: Bearer …`) — for
    /// providers that cannot carry the credential in the URL. `None` for the
    /// common URL-keyed providers and for public endpoints.
    pub rpc_auth_header: Option<String>,
}

impl std::fmt::Debug for SolanaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaConfig")
            .field("rpc_urls", &endpoint_hosts(&self.rpc_urls))
            .field("pay_to", &self.pay_to)
            .field(
                "mints",
                &self
                    .mints
                    .iter()
                    .map(|entry| entry.key.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("commitment", &self.commitment)
            .finish()
    }
}

/// A Solana JSON-RPC transport failure — the call never produced an answer.
/// Mapped to `None` at the verifier seam, indistinguishable from "no matching
/// transfer yet".
pub struct SolanaError(pub String);

impl std::fmt::Debug for SolanaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaError").field("0", &self.0).finish()
    }
}

/// What `validate_transfer` demands the chain-side transfer satisfy. The
/// intent's own terms ride in so the verifier checks them against the live
/// transaction rather than trusting the caller's copy.
#[derive(Debug, Clone)]
pub struct ExpectedTransfer {
    /// The deposit address that must receive the funds (`payTo`).
    pub recipient: String,
    /// The amount the transfer must deliver: lamports for native SOL, or the
    /// SPL token's own minor units when `spl_token_mint` is set.
    pub amount: i64,
    /// The SPL mint the transfer must spend, or `None` for native SOL.
    pub spl_token_mint: Option<String>,
    /// The per-intent marker the paying transaction must carry.
    pub reference: String,
}

/// What `validate_payout` demands the chain-side transaction satisfy (issue
/// #1175): the refund payout's own terms — the customer's frozen refund
/// address and the locked mint — with no `reference` marker: the runbook
/// deliberately never reuses the deposit's marker on the payout, so the
/// proof keys on recipient + mint alone.
#[derive(Debug, Clone)]
pub struct ExpectedPayout {
    /// The customer's frozen refund address the payout must pay.
    pub recipient: String,
    /// The SPL mint the payout must spend, or `None` for native SOL.
    pub spl_token_mint: Option<String>,
    /// The vault the payout must leave from — the rail's `payTo`, which must
    /// appear among the transaction's debited funding parties. A refund a
    /// third party paid direct is no proof the vault paid out (issue #1175).
    pub source: String,
}

/// A located-and-validated transfer: the signature `find_reference` answered
/// and `validate_transfer` proved, plus the amount it actually delivered so
/// the deposit can record `received_minor` against the opened `amount_minor`.
#[derive(Debug, Clone)]
pub struct ValidatedTransfer {
    /// The transaction's signature — the deposit's `facilitator_tx_hash` and
    /// the ledger credit's idempotency key suffix.
    pub signature: String,
    /// What the transaction delivered to `recipient`, in the asset's own
    /// minor units (lamports for SOL, base units for SPL).
    pub delivered: i64,
    /// The transaction's on-chain `blockTime` (seconds since the Unix epoch)
    /// — when the chain itself recorded the transfer. Expiry is judged on
    /// this, not server `now()`: a payment on-chain before the intent lapsed
    /// credits even when it is confirmed after; one that landed after never
    /// does (issue #1128).
    pub block_time: Option<i64>,
    /// Every debited funding party of the transfer (issues #1144, #1178):
    /// the addresses pre-credit screening must each clear before the deposit
    /// credits. For native SOL this is *every* signer whose lamport balance
    /// decreased — the funding source(s) and the fee payer — so a sanctioned
    /// wallet cannot launder the payment through a clean relayer; when the
    /// balance arrays isolate no debited signer the list falls back to the
    /// first signer (the fee payer, `accountKeys[0]`). For an SPL transfer it
    /// is the `owner` of each token account whose `preTokenBalances` row
    /// decreased on the paid mint, falling back to the first signer when the
    /// token rows name none. Empty only when the transaction's shape carries
    /// no usable signal at all; the credit path treats an empty list as
    /// unscreenable, never as cleared.
    pub payers: Vec<String>,
}

/// One inbound transfer the reconciliation scan surfaced (issue #1149),
/// mirroring `oxsum_core::ScannedTransfer`: a transaction in `payTo`'s
/// signature history that delivered value to the deposit address in a
/// whitelisted asset — the on-chain row the operator diffs against the
/// recorded deposits.
#[derive(Debug, Clone)]
pub struct ScannedTransfer {
    /// The transaction's signature.
    pub signature: String,
    /// The on-chain `blockTime` — when the chain recorded the transfer.
    pub block_time: Option<i64>,
    /// The whitelist asset key the delivered delta counts in (`sol` for a
    /// lamport delta, an SPL key for a token one).
    pub asset: Option<String>,
    /// What the transaction delivered to `payTo`, in `asset`'s base units.
    pub delivered: i64,
    /// The payer's address — the first of the debited funding parties
    /// `ValidatedTransfer.payers` names — when the transaction's shape yields
    /// one. The report keeps a single representative; screening is the credit
    /// path's job.
    pub payer: Option<String>,
}

/// How long an endpoint sits out after enough consecutive failures: the
/// circuit-breaker half of the failover (issue #1145). Thirty seconds is a
/// poll-cycle granularity — long enough that a dead provider stops eating the
/// per-call timeout every pass, short enough that a provider recovering under
/// the same tick never looks dead for a human minute. Attempts against a
/// single-endpoint pool never cooldown-skip it — there is nowhere else to
/// send the call.
const ENDPOINT_COOLDOWN: Duration = Duration::from_secs(30);

/// The consecutive failures that open the cooldown. Two: one bad answer might
/// be a blip (a transient 429 or a dropped connection); a second in a row is
/// the endpoint's own problem worth routing around.
const ENDPOINT_FAILURE_THRESHOLD: u32 = 2;

/// One configured RPC target: the URL it posts to (which may carry the
/// provider's API key inside it — Helius, Triton and the public endpoints all
/// key auth this way), its `endpoint` metric-label spelling (host only, the
/// key never leaves the URL), and the health the failover loop reads.
struct RpcEndpoint {
    /// The endpoint's full URL, API key included when the provider keys auth
    /// that way.
    url: String,
    /// What `solana_rpc_errors_total{endpoint}` and the failover gauge label
    /// report: the host (plus port), never the URL's query — the API key must
    /// never appear on a metric.
    host: String,
    /// Consecutive failed calls — the counter the cooldown opens on. Relaxed
    /// ordering: a poll thread only ever needs the approximate figure.
    consecutive_failures: AtomicU32,
    /// Until this instant the endpoint is skipped; `None` is serving traffic.
    /// A `Mutex` because the cooldown is written under the call loop's single
    /// thread anyway and `Instant` has no atomic.
    unhealthy_until: Mutex<Option<Instant>>,
}

impl RpcEndpoint {
    /// Builds the endpoint from its configured URL: the metric-safe host is
    /// derived once at construction so a malformed URL fails loudly at boot
    /// rather than silently labeling a metric with an API key.
    fn new(raw: &str) -> Result<Self, SolanaError> {
        let url = raw.trim().trim_end_matches('/').to_owned();
        let host = endpoint_host(&url)
            .ok_or_else(|| SolanaError(format!("an RPC endpoint has no host: {url}")))?;
        Ok(Self {
            url,
            host,
            consecutive_failures: AtomicU32::new(0),
            unhealthy_until: Mutex::new(None),
        })
    }

    /// Whether the endpoint is inside its cooldown right now.
    fn cooling_down(&self) -> bool {
        let guard = self
            .unhealthy_until
            .lock()
            .expect("the endpoint cooldown lock is poisoned");
        matches!(*guard, Some(until) if until > Instant::now())
    }

    /// A call answered — the endpoint is healthy: reset the streak, close any
    /// open cooldown and report `solana_rpc_endpoint_healthy{endpoint}=1`.
    fn note_ok(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        let mut guard = self
            .unhealthy_until
            .lock()
            .expect("the endpoint cooldown lock is poisoned");
        if guard.is_some() {
            *guard = None;
            tracing::info!(endpoint = %self.host, "the solana RPC endpoint recovered");
        }
        metrics::gauge!("solana_rpc_endpoint_healthy", "endpoint" => self.host.clone()).set(1.0);
    }

    /// A call failed: grow the streak and, past the threshold, open the
    /// cooldown and report `solana_rpc_endpoint_healthy{endpoint}=0`.
    fn note_failure(&self) {
        let streak = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if streak >= ENDPOINT_FAILURE_THRESHOLD {
            let mut guard = self
                .unhealthy_until
                .lock()
                .expect("the endpoint cooldown lock is poisoned");
            *guard = Some(Instant::now() + ENDPOINT_COOLDOWN);
            metrics::gauge!("solana_rpc_endpoint_healthy", "endpoint" => self.host.clone())
                .set(0.0);
            tracing::warn!(
                endpoint = %self.host,
                streak,
                cooldown_secs = ENDPOINT_COOLDOWN.as_secs(),
                "the solana RPC endpoint is cooling down after consecutive failures"
            );
        }
    }
}

/// The host (plus non-default port) an endpoint URL reports on its metric
/// labels — the API-key-carrying query is deliberately dropped, so the
/// `endpoint` label can never carry a secret. `None` when the URL does not
/// parse to a host at all.
fn endpoint_host(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?.to_owned();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// The same mapping applied to a list of configured URLs — `Debug` prints
/// and log lines label endpoints the same way metrics do.
fn endpoint_hosts(urls: &[String]) -> Vec<String> {
    urls.iter()
        .map(|u| endpoint_host(u).unwrap_or_else(|| "<unparseable>".to_owned()))
        .collect()
}

/// The narrow JSON-RPC client the rail talks to, bound to the deployment's
/// [`SolanaConfig`]: `endpoints` is the configured RPC pool the calls fail
/// over across, and `config` carries the `pay_to`/`mints`/`commitment` terms
/// the two Solana Pay calls and the boot's mint audit are asked under.
/// Binding the config in (rather than passing it per call) is what lets the
/// client be a `ChainLookup` — the verifier's seam hands it only the intent's
/// `reference`/`amount`, and the failover stays inside `call` where the trait
/// never sees it.
///
/// `Clone` shares the pool, not copies it: the endpoint health, the
/// round-robin cursor and the HTTP pool are `Arc`-inside, so the `ChainLookup`
/// the verifier holds and any second handle observe the same circuit state.
#[derive(Clone)]
pub struct SolanaRpcClient {
    endpoints: std::sync::Arc<Vec<RpcEndpoint>>,
    /// Where the next call starts its endpoint walk — round-robin so a
    /// single-endpoint pool is unaffected but a multi-endpoint pool spreads
    /// the poll's load across providers rather than always hitting the first.
    next: std::sync::Arc<AtomicUsize>,
    http: reqwest::Client,
    config: SolanaConfig,
}

impl std::fmt::Debug for SolanaRpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaRpcClient")
            .field(
                "endpoints",
                &self
                    .endpoints
                    .iter()
                    .map(|endpoint| endpoint.host.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("config", &self.config)
            .finish()
    }
}

impl SolanaRpcClient {
    /// Builds the client around the configured RPC endpoints, the shared HTTP
    /// pool and the deployment's terms. `urls` is the parsed
    /// `OXSUM_SOLANA_RPC_URLS`/`_RPC_URL` list — every entry must carry a host
    /// (the check runs here, at boot, so a malformed endpoint fails loudly
    /// rather than silently mislabeling a metric). `Err` on an empty list or
    /// a hostless entry — `solana_of` only builds the client when at least
    /// one URL is set, so the error is a misconfiguration, never a panic.
    pub fn try_new(
        urls: Vec<String>,
        http: reqwest::Client,
        config: SolanaConfig,
    ) -> Result<Self, SolanaError> {
        if urls.is_empty() {
            return Err(SolanaError(
                "the solana rail needs at least one RPC endpoint".into(),
            ));
        }
        let endpoints = urls
            .iter()
            .map(|raw| RpcEndpoint::new(raw))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            endpoints: std::sync::Arc::new(endpoints),
            next: std::sync::Arc::new(AtomicUsize::new(0)),
            http,
            config,
        })
    }

    /// The deployment's chain-side config — endpoint pool, `payTo`, the mint
    /// whitelist and the commitment the calls ask for.
    #[must_use]
    pub fn config(&self) -> &SolanaConfig {
        &self.config
    }

    /// Reads a mint account off the chain once (issue #1182): one
    /// `getAccountInfo(mint, base64)` answers both the program that owns the
    /// account and its raw data, so the audit's owner check and layout parse
    /// can never observe *different* accounts (the defect two separate reads
    /// carried — the decimals could come from an account the owner check
    /// never saw).
    ///
    /// `Err` when the RPC fails or the account is missing/unparseable.
    async fn mint_account(&self, mint: &str) -> Result<(String, Vec<u8>), SolanaError> {
        let result = self
            .call(
                "mint_account",
                "getAccountInfo",
                json!([mint, {"encoding": "base64"}]),
            )
            .await?;
        let owner = result["value"]["owner"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| SolanaError("getAccountInfo returned no owner".into()))?;
        let data_b64 = result["value"]["data"][0]
            .as_str()
            .ok_or_else(|| SolanaError("getAccountInfo returned no mint data".into()))?;
        let bytes = base64_decode(data_b64)
            .map_err(|e| SolanaError(format!("mint data is not base64: {e}")))?;
        Ok((owner, bytes))
    }

    /// The program that owns a mint account — the classic SPL Token or
    /// Token-2022 program id `getAccountInfo` reports as `owner`. The boot
    /// calls this once per whitelisted mint and refuses any other owner
    /// (issue #1182): Token-2022 is named explicitly (its transfer-fee and
    /// hook extensions the rail does not model), and a mint owned by any
    /// other program — the System program, a custom program — is refused
    /// too, because an account that is not a classic SPL mint cannot price a
    /// deposit.
    ///
    /// `Err` when the RPC fails or the account is missing/unparseable.
    pub async fn mint_owner(&self, mint: &str) -> Result<String, SolanaError> {
        Ok(self.mint_account(mint).await?.0)
    }

    /// Reads a mint's decimals from the chain (issues #1129, #1140). The
    /// raw account data is first checked against the classic SPL mint layout
    /// — exactly 82 bytes with `isInitialized` set (issue #1182) — so a
    /// truncated account or an uninitialized mint is refused rather than
    /// having byte 44 read off a buffer that is not a mint at all. The boot
    /// calls this once per whitelisted mint so the mint's own figure — never
    /// an operator's declaration — sets `asset_decimals`: a 6-decimal USDC
    /// mint read as 9 decimals would overcharge by 1000×.
    ///
    /// `Err` when the RPC fails, the mint account is missing, or its data is
    /// not a well-formed classic SPL mint layout.
    pub async fn mint_decimals(&self, mint: &str) -> Result<u32, SolanaError> {
        let (_owner, bytes) = self.mint_account(mint).await?;
        mint_decimals_of(&bytes)
    }

    /// One JSON-RPC call over the endpoint pool (issue #1145): walk the
    /// endpoints starting at the round-robin cursor, skipping any inside its
    /// failure cooldown. A transport failure, a non-2xx or an RPC-level
    /// `error` object counts a failure on that endpoint and — while another
    /// endpoint remains — retries on the next one; `Err` only when every
    /// candidate failed (or the only configured endpoint did). Each failed
    /// attempt counts `solana_rpc_errors_total{call,endpoint}` on the host
    /// label, and each move onto a different endpoint counts
    /// `solana_rpc_failovers_total{from,to}`, so the dashboard sees every
    /// hop, not just the errors.
    async fn call(
        &self,
        call_name: &'static str,
        method: &str,
        params: Value,
    ) -> Result<Value, SolanaError> {
        self.call_reporting_endpoint(call_name, method, params)
            .await
            .map(|(value, _)| value)
    }

    /// [`call`] plus the endpoint that answered — the cross-confirmation in
    /// `validate_transfer` needs to know which endpoint served so the
    /// confirmation read lands on a *different* one (issue #1183).
    async fn call_reporting_endpoint(
        &self,
        call_name: &'static str,
        method: &str,
        params: Value,
    ) -> Result<(Value, &RpcEndpoint), SolanaError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let endpoints = &self.endpoints;
        // The start of the walk: round-robin across the pool, so the poll's
        // calls spread across providers rather than hammering the first.
        let start = self.next.fetch_add(1, Ordering::Relaxed) % endpoints.len();
        // A single-endpoint pool is never cooldown-skipped — there is nowhere
        // else the call could go, so the attempt must run anyway; the health
        // metric still reports the failure streak.
        let single = endpoints.len() == 1;
        let mut last_error = String::new();
        // The endpoint the previous attempt ran on: every attempt on a
        // *different* endpoint counts one failover hop `prev → current`, so a
        // call that walked two failures into a healthy third counts both
        // moves.
        let mut previous: Option<&RpcEndpoint> = None;
        let mut tried = false;
        for offset in 0..endpoints.len() {
            let endpoint = &endpoints[(start + offset) % endpoints.len()];
            if !single && endpoint.cooling_down() {
                continue;
            }
            tried = true;
            if let Some(from) = previous
                && !std::ptr::eq(from, endpoint)
            {
                metrics::counter!(
                    "solana_rpc_failovers_total",
                    "from" => from.host.clone(),
                    "to" => endpoint.host.clone()
                )
                .increment(1);
            }
            previous = Some(endpoint);
            let result = self.call_once(endpoint, method, &body).await;
            match result {
                Ok(value) => {
                    endpoint.note_ok();
                    return Ok((value, endpoint));
                }
                Err(e) => {
                    endpoint.note_failure();
                    // Per-endpoint error accounting: the `endpoint` label is
                    // the host only — the API key inside the URL can never
                    // reach a metric.
                    metrics::counter!(
                        "solana_rpc_errors_total",
                        "call" => call_name,
                        "endpoint" => endpoint.host.clone()
                    )
                    .increment(1);
                    last_error = e.0;
                }
            }
        }
        // Every attempted endpoint failed — or every candidate sat inside its
        // cooldown and none ran at all: fall through the whole pool once more
        // ignoring the skip, so a call never dies on a shared cooldown window
        // alone.
        if !tried {
            for endpoint in endpoints.iter() {
                if let Some(from) = previous
                    && !std::ptr::eq(from, endpoint)
                {
                    metrics::counter!(
                        "solana_rpc_failovers_total",
                        "from" => from.host.clone(),
                        "to" => endpoint.host.clone()
                    )
                    .increment(1);
                }
                previous = Some(endpoint);
                let result = self.call_once(endpoint, method, &body).await;
                match result {
                    Ok(value) => {
                        endpoint.note_ok();
                        return Ok((value, endpoint));
                    }
                    Err(e) => {
                        endpoint.note_failure();
                        metrics::counter!(
                            "solana_rpc_errors_total",
                            "call" => call_name,
                            "endpoint" => endpoint.host.clone()
                        )
                        .increment(1);
                        last_error = e.0;
                    }
                }
            }
        }
        Err(SolanaError(last_error))
    }

    /// The single-endpoint attempt `call` fails over across: POST the prepared
    /// JSON-RPC body to one URL, applying the configured auth header when the
    /// deployment set one. `Ok` is the `result` field; `Err` is what failed —
    /// transport, non-2xx or an RPC-level refusal — and is what `call` counts
    /// against this endpoint.
    async fn call_once(
        &self,
        endpoint: &RpcEndpoint,
        method: &str,
        body: &Value,
    ) -> Result<Value, SolanaError> {
        let mut request = self
            .http
            .post(&endpoint.url)
            .json(body)
            .timeout(Duration::from_secs(15));
        if let Some(header) = &self.config.rpc_auth_header {
            // `OXSUM_SOLANA_RPC_AUTH_HEADER` is the verbatim `Name: value`
            // header — `solana_of` refuses a colon-less value at boot, so a
            // missing split here is defensive only.
            if let Some((name, value)) = header.split_once(':') {
                request = request.header(name.trim(), value.trim());
            }
        }
        // `without_url`: a reqwest error's Display carries the request URL —
        // which for an authenticated endpoint holds the API key inside its
        // query (issue #1145). The error text must stay key-free: the warn
        // log at the ChainLookup seam and `SolanaError` both surface it.
        let response = request
            .send()
            .await
            .map_err(|e| SolanaError(format!("{method} request failed: {}", e.without_url())))?;
        if !response.status().is_success() {
            return Err(SolanaError(format!(
                "{method} answered HTTP {}",
                response.status()
            )));
        }
        let value: Value = response.json().await.map_err(|e| {
            SolanaError(format!("{method} response unreadable: {}", e.without_url()))
        })?;
        if let Some(error) = value.get("error") {
            return Err(SolanaError(format!("{method} refused: {error}")));
        }
        Ok(value["result"].clone())
    }

    /// `getSignaturesForAddress(reference)` — the `findReference` half: every
    /// confirmed transaction touching the per-intent marker account, newest
    /// first, paginated through the `before` cursor so a burst of fresh
    /// dishonoring transactions on the same marker cannot push the real
    /// payment out of a fixed-size window (issue #1128). A per-intent marker
    /// is a throwaway address, so its history is short in practice; the walk
    /// stops at `MAX_REFERENCE_PAGES` pages anyway as a bound against an
    /// abusive marker. `Ok(vec![])` means nothing has landed yet — the normal
    /// "not paid yet" answer.
    pub async fn find_reference(&self, reference: &str) -> Result<Vec<String>, SolanaError> {
        const PAGE: usize = 100;
        const MAX_REFERENCE_PAGES: usize = 10;
        let mut signatures = Vec::new();
        let mut before: Option<String> = None;
        for _ in 0..MAX_REFERENCE_PAGES {
            let mut opts = serde_json::Map::new();
            opts.insert("limit".into(), json!(PAGE));
            opts.insert("commitment".into(), json!(self.config.commitment));
            if let Some(cursor) = &before {
                opts.insert("before".into(), json!(cursor));
            }
            let result = self
                .call(
                    "find_reference",
                    "getSignaturesForAddress",
                    json!([reference, Value::Object(opts)]),
                )
                .await?;
            let page = result.as_array().cloned().unwrap_or_default();
            if page.is_empty() {
                break;
            }
            before = page
                .last()
                .and_then(|row| row["signature"].as_str())
                .map(str::to_owned);
            signatures.extend(
                page.iter()
                    .filter_map(|row| row["signature"].as_str().map(str::to_owned)),
            );
            // A short page is the last page.
            if page.len() < PAGE {
                break;
            }
        }
        Ok(signatures)
    }

    /// The reconciliation scan's chain side (issue #1149): walks
    /// `getSignaturesForAddress(payTo)` — newest first, paginated through the
    /// `before` cursor, bounded at `max_signatures` — and fetches each
    /// candidate's `getTransaction` once, keeping only the transactions that
    /// delivered value to `payTo` in a whitelisted asset: a positive lamport
    /// delta (the `sol` arm) or a positive token-account delta owned by
    /// `payTo` on a configured mint. A `payTo`-signed outbound transaction —
    /// a refund payout, a consolidation — never reports: its recipient delta
    /// is negative or absent. Failed transactions are dropped the same way
    /// `validate_transfer` drops them.
    ///
    /// This is deliberately *not* `validate_transfer`: there is no
    /// `reference` to demand (the scan exists to find the payments that
    /// carried none), no amount to meet — it reports what arrived, and the
    /// caller diffs the signatures against the recorded deposits. Pages are
    /// walked sequentially through the same endpoint pool every call uses,
    /// so the failover and cooldown rules apply unchanged; `max_signatures`
    /// is what bounds the `getTransaction` fan-out on a busy vault.
    ///
    /// The [`ScanWalk`] (issue #1188) reports the walk's real coverage:
    /// `walked` is every signature whose `getTransaction` was attempted —
    /// *not* the credited count the endpoint used to report — and
    /// `history_bounded` tells whether the bound cut the walk while older
    /// history may remain.
    pub async fn scan_received(
        &self,
        max_signatures: usize,
    ) -> Result<oxsum_core::ScanWalk, SolanaError> {
        const PAGE: usize = 100;
        let mut signatures = Vec::new();
        let mut before: Option<String> = None;
        // The walk's page bound: `max_signatures` caps the rows scanned, and
        // the page loop additionally stops when a page comes back short — a
        // vault with a long history walks only what the bound asks for.
        while signatures.len() < max_signatures {
            let mut opts = serde_json::Map::new();
            opts.insert("limit".into(), json!(PAGE));
            opts.insert("commitment".into(), json!(self.config.commitment));
            if let Some(cursor) = &before {
                opts.insert("before".into(), json!(cursor));
            }
            let result = self
                .call(
                    "scan_received",
                    "getSignaturesForAddress",
                    json!([self.config.pay_to, Value::Object(opts)]),
                )
                .await?;
            let page = result.as_array().cloned().unwrap_or_default();
            if page.is_empty() {
                break;
            }
            before = page
                .last()
                .and_then(|row| row["signature"].as_str())
                .map(str::to_owned);
            signatures.extend(
                page.iter()
                    .filter_map(|row| row["signature"].as_str().map(str::to_owned)),
            );
            if page.len() < PAGE {
                break;
            }
        }
        // `history_bounded` is honest only when the bound actually truncated a
        // longer list — a short last page means the history ended on its own.
        let history_bounded = signatures.len() > max_signatures;
        signatures.truncate(max_signatures);
        // For each candidate, prove it paid payTo in a whitelisted asset and
        // report the delivered figure. `getTransaction` runs once per
        // signature — sequential and bounded, the polite walk the endpoint
        // pool already amortizes.
        let walked = signatures.len();
        let mut credited = Vec::new();
        for signature in signatures {
            match self.inspect_received(&signature).await {
                Ok(legs) => credited.extend(legs),
                // A single unreadable transaction does not sink the scan:
                // the signature stays unreported, and a transport failure
                // already counted against its endpoint inside `call`.
                Err(e) => {
                    tracing::warn!(
                        signature,
                        error = %e.0,
                        "the reconciliation scan could not read a transaction"
                    );
                }
            }
        }
        Ok(oxsum_core::ScanWalk {
            walked,
            credited: credited
                .into_iter()
                .map(|t| oxsum_core::ScannedTransfer {
                    signature: t.signature,
                    block_time: t.block_time,
                    asset: t.asset,
                    delivered: t.delivered,
                    payer: t.payer,
                })
                .collect(),
            history_bounded,
        })
    }

    /// `getTransaction(signature)` under the scan's looser contract: `Ok(Vec)`
    /// with one [`ScannedTransfer`] per whitelisted asset the transaction
    /// delivered to `payTo` — the figure and the asset key it counted in.
    /// A single transaction that credits SOL *and* an SPL mint reports both
    /// legs (issue #1188) — the empty Vec is a failed transaction, a missing
    /// one, or one that moved nothing into the vault (a `payTo`-signed send,
    /// an unrelated touch). The `delivered` figure is the recipient-side
    /// delta — what the vault gained — never judged.
    async fn inspect_received(&self, signature: &str) -> Result<Vec<ScannedTransfer>, SolanaError> {
        let result = self
            .call(
                "scan_received",
                "getTransaction",
                json!([
                    signature,
                    {
                        "encoding": "jsonParsed",
                        "commitment": self.config.commitment,
                        "maxSupportedTransactionVersion": 0,
                    },
                ]),
            )
            .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        let meta = &result["meta"];
        if !meta["err"].is_null() {
            return Ok(Vec::new());
        }
        let message = &result["transaction"]["message"];
        let account_keys = match message["accountKeys"].as_array() {
            Some(keys) => keys,
            None => return Ok(Vec::new()),
        };
        let block_time = result["blockTime"].as_i64();
        Ok(received_legs(
            signature,
            meta,
            account_keys,
            &self.config.pay_to,
            &self.config.mints,
            block_time,
        ))
    }

    /// prove the located transaction is this intent's paying transfer.
    /// `Ok(None)` means the transaction exists but is not the payment — a
    /// failed transaction, wrong recipient, wrong mint or missing reference —
    /// which the rail treats as "not paid yet" exactly like Solana Pay's
    /// validator does.
    ///
    /// The checks are deliberately strict in the customer's favor: `err` must
    /// be null, the `reference` account must appear among the transaction's
    /// `accountKeys` (non-signer), and the recipient's balance delta must be
    /// computable on the locked mint — SOL's lamport delta for a native
    /// intent, the SPL token-account delta for a minted one. The delivered
    /// amount is *reported*, not judged here (issue #1142): the verifier
    /// compares `delivered` against `expected.amount` — the locked asset's
    /// base units — so an under-delivery marks the deposit `underpaid` in the
    /// operator queue rather than masquerading as "no payment yet".
    ///
    /// A *credit* decision never rests on one node's word (issue #1183): when
    /// the pool carries a second endpoint, a validated answer is re-fetched —
    /// the same signature at the same commitment — on a different healthy
    /// endpoint before it is believed. The two endpoints must agree on the
    /// evidence the validation reads (a successful transaction carrying the
    /// intent's `reference` and delivering `delivered` to the `recipient` on
    /// the expected mint). A disagreement, or a second endpoint that cannot
    /// answer at all, keeps the deposit `pending` — reported as `Err` (never
    /// `Ok(None)`, which would read as "not the payment" and feed the expiry
    /// sweep — issue #1174) with a
    /// `solana_rpc_cross_confirmations_total{outcome}` count — because
    /// a lone lagging or lying endpoint is exactly the failure the pool exists
    /// to survive. A single-endpoint pool keeps the original posture: the
    /// first answer stands alone (and `solana_of` warns at boot when that
    /// single endpoint is mainnet-shaped).
    pub async fn validate_transfer(
        &self,
        signature: &str,
        expected: &ExpectedTransfer,
    ) -> Result<Option<ValidatedTransfer>, SolanaError> {
        let params = json!([
            signature,
            {
                "encoding": "jsonParsed",
                "commitment": self.config.commitment,
                "maxSupportedTransactionVersion": 0,
            },
        ]);
        let (result, served_by) = self
            .call_reporting_endpoint("validate_transfer", "getTransaction", params.clone())
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let meta = &result["meta"];
        // A failed transaction never moved the money — treat it as "not paid".
        if !meta["err"].is_null() {
            return Ok(None);
        }
        let message = &result["transaction"]["message"];
        let account_keys = match message["accountKeys"].as_array() {
            Some(keys) => keys,
            None => return Ok(None),
        };
        // The `reference` marker must be present as an account key — and as
        // the read-only, non-signer account the Solana Pay recipe attaches
        // (issue #1130). With `jsonParsed`, accountKeys entries are `{pubkey,
        // signer, writable, source}` objects; a bare-string form (the signer
        // set of a partially-decoded payload) is tolerated defensively but a
        // structured entry must state `signer:false` and `writable:false` —
        // a marker that signed or mutated is not the throwaway index the rail
        // keys on.
        if !reference_marker_present(account_keys, &expected.reference) {
            return Ok(None);
        }
        let delivered = match expected.spl_token_mint.as_deref() {
            // Native SOL: the recipient's post-minus-pre lamport delta. The
            // `accountKeys` index of `recipient` aligns with `preBalances` /
            // `postBalances`, which only cover static (non-lookup) keys — so
            // index the recipient's position within those keys.
            None => {
                let Some(delta) = lamport_delta(
                    account_keys,
                    meta["preBalances"].as_array(),
                    meta["postBalances"].as_array(),
                    &expected.recipient,
                ) else {
                    return Ok(None);
                };
                delta
            }
            // SPL: `postTokenBalances` carries `{accountIndex, mint,
            // uiTokenAmount:{amount}}`; find the row on the recipient's mint
            // whose account belongs to `recipient`. `preTokenBalances` gives
            // the before figure so the delta is the transfer, not the closing
            // balance — a pre-funded ATA must not count its prior holdings.
            Some(mint) => {
                let post = token_delta(
                    meta["postTokenBalances"].as_array(),
                    meta["preTokenBalances"].as_array(),
                    account_keys,
                    &expected.recipient,
                    mint,
                );
                match post {
                    Some(delta) => delta,
                    None => return Ok(None),
                }
            }
        };
        // The delivered figure rides up unjudged: `expected.amount` is the
        // verifier's comparison, not this proof's — an under-delivery is the
        // intent's paying transfer too, and it is what `underpaid` records
        // (issue #1142).
        let payers = match expected.spl_token_mint.as_deref() {
            None => sol_payers(
                account_keys,
                meta["preBalances"].as_array(),
                meta["postBalances"].as_array(),
            ),
            Some(mint) => spl_payers(
                meta["preTokenBalances"].as_array(),
                meta["postTokenBalances"].as_array(),
                account_keys,
                mint,
            ),
        };
        // Cross-confirm the credit-grade answer on a second endpoint (issue
        // #1183). The primary read already proved the transfer on
        // `served_by`; a second healthy endpoint must see the same successful
        // transaction deliver the same amount to the same recipient on the
        // same mint under the same marker before the answer stands.
        // Anything less — no tx, a different delivered figure, a missing
        // marker, or no second endpoint able to answer — is NOT "not the
        // payment": it is *unconfirmable*, so the caller defers (`Err`,
        // never a flat `Ok(None)` the expiry sweep would read as unpaid —
        // issue #1174).
        if self.endpoints.len() > 1 {
            let confirmed = self
                .cross_confirm_transaction(signature, expected, served_by, &params, delivered)
                .await;
            if !confirmed {
                return Err(SolanaError(format!(
                    "cross-confirmation failed for {signature}: the endpoints disagree \
                     or no second endpoint could answer"
                )));
            }
        }
        Ok(Some(ValidatedTransfer {
            signature: signature.to_owned(),
            delivered,
            block_time: result["blockTime"].as_i64(),
            payers,
        }))
    }

    /// `getTransaction(signature, jsonParsed)` — the refund-complete half
    /// (issue #1175): prove the submitted transaction is the multisig payout
    /// the refund row claims. `Ok(None)` means it does not prove the payout —
    /// a nonexistent signature, a failed transaction, or one whose
    /// `recipient` delta cannot be computed on `expected.spl_token_mint`
    /// (wrong address, wrong mint, a `payTo`-signed send in the wrong
    /// direction). The checks mirror `validate_transfer`'s strictness minus
    /// the marker: `err` must be null and the recipient's delta computable —
    /// the lamport delta for native SOL, the token-account delta for an SPL
    /// mint — while the delivered figure is *reported*, not judged: the core
    /// compares it against the refund amount the lifecycle froze. There is
    /// no `reference` to demand: the payout deliberately never reuses the
    /// deposit's marker, so recipient + mint is the whole proof.
    pub async fn validate_payout(
        &self,
        signature: &str,
        expected: &ExpectedPayout,
    ) -> Result<Option<ValidatedTransfer>, SolanaError> {
        let result = self
            .call(
                "validate_payout",
                "getTransaction",
                json!([
                    signature,
                    {
                        "encoding": "jsonParsed",
                        "commitment": self.config.commitment,
                        "maxSupportedTransactionVersion": 0,
                    },
                ]),
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let meta = &result["meta"];
        // A failed transaction never moved the money.
        if !meta["err"].is_null() {
            return Ok(None);
        }
        let message = &result["transaction"]["message"];
        let account_keys = match message["accountKeys"].as_array() {
            Some(keys) => keys,
            None => return Ok(None),
        };
        let delivered = match expected.spl_token_mint.as_deref() {
            // Native SOL: the refund address's post-minus-pre lamport delta.
            None => {
                let Some(delta) = lamport_delta(
                    account_keys,
                    meta["preBalances"].as_array(),
                    meta["postBalances"].as_array(),
                    &expected.recipient,
                ) else {
                    return Ok(None);
                };
                delta
            }
            // SPL: the token-account delta on the frozen mint whose owner (or
            // account address) is the refund address — a pre-funded ATA never
            // counts its prior holdings.
            Some(mint) => {
                let Some(delta) = token_delta(
                    meta["postTokenBalances"].as_array(),
                    meta["preTokenBalances"].as_array(),
                    account_keys,
                    &expected.recipient,
                    mint,
                ) else {
                    return Ok(None);
                };
                delta
            }
        };
        // The funding parties the transaction debited, kept for reporting —
        // every signer whose balance fell for SOL, every debited token
        // account's owner for SPL. The refund's *source* proof is stricter
        // than a signer set: a Squads vault PDA spends through CPI and never
        // appears in the signer list, so `sol_payers` would refuse the very
        // payout the rail is built to take (issue #1209). The proof asks
        // whether the vault's own balance *fell*: for SOL, `expected.source`
        // is the accountKeys entry whose lamport delta is a decrease of at
        // least `delivered` regardless of the signer flag; for SPL it is a
        // token account `expected.source` owns that decreased by at least
        // `delivered` on the locked mint. A refund a third party paid direct
        // never empties the vault the row believes it closed, so a
        // source-decrease below `delivered` is no proof of payout.
        let source_paid = match expected.spl_token_mint.as_deref() {
            None => source_lamports_decreased(
                account_keys,
                meta["preBalances"].as_array(),
                meta["postBalances"].as_array(),
                &expected.source,
                delivered,
            ),
            Some(mint) => source_token_decreased(
                meta["preTokenBalances"].as_array(),
                meta["postTokenBalances"].as_array(),
                account_keys,
                &expected.source,
                mint,
                delivered,
            ),
        };
        if !source_paid {
            return Ok(None);
        }
        let payers = match expected.spl_token_mint.as_deref() {
            None => sol_payers(
                account_keys,
                meta["preBalances"].as_array(),
                meta["postBalances"].as_array(),
            ),
            Some(mint) => spl_payers(
                meta["preTokenBalances"].as_array(),
                meta["postTokenBalances"].as_array(),
                account_keys,
                mint,
            ),
        };
        Ok(Some(ValidatedTransfer {
            signature: signature.to_owned(),
            delivered,
            block_time: result["blockTime"].as_i64(),
            payers,
        }))
    }

    /// The second-opinion read behind a validated transfer (issue #1183):
    /// re-issue the exact `getTransaction` the primary endpoint answered on a
    /// *different* healthy endpoint, and demand that it sees the same
    /// successful transaction — `err: null`, the intent's `reference` marker
    /// present as a read-only non-signer, and the same `delivered` figure to
    /// the expected `recipient` on the expected mint. `true` only when a
    /// second endpoint answered with agreeing evidence.
    ///
    /// The confirmation is deliberately *not* the failover `call`: it must
    /// land on an endpoint other than the one that served, and its failure
    /// must not fail over onto `served_by` (agreeing with yourself is not a
    /// second opinion). `false` — never `Err` — covers every refusal: the
    /// caller maps it to an `Err` of its own, so a disagreement or a dark
    /// second endpoint reads as "cannot confirm" and defers the verify —
    /// never `Ok(None)`, which would read as an unpaid verdict the expiry
    /// sweep could act on (issue #1174).
    /// `solana_rpc_cross_confirmations_total{endpoint,outcome}` records
    /// which of "agreed"/"disagreed"/"unreachable" the attempt resolved to.
    async fn cross_confirm_transaction(
        &self,
        signature: &str,
        expected: &ExpectedTransfer,
        served_by: &RpcEndpoint,
        params: &Value,
        delivered: i64,
    ) -> bool {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTransaction",
            "params": params,
        });
        // The confirmation walks the endpoints except the one that served —
        // same cooldown rules as `call` (a cooling endpoint is skipped), so
        // the second opinion is always a live provider. An endpoint that
        // cannot answer moves the walk to the next sibling; a pool of two
        // tries exactly the other one.
        for endpoint in self.endpoints.iter() {
            if std::ptr::eq(endpoint, served_by) || endpoint.cooling_down() {
                continue;
            }
            match self.call_once(endpoint, "getTransaction", &body).await {
                Ok(result) => {
                    endpoint.note_ok();
                    let agreed = cross_confirmation_agrees(&result, expected, delivered);
                    metrics::counter!(
                        "solana_rpc_cross_confirmations_total",
                        "endpoint" => endpoint.host.clone(),
                        "outcome" => if agreed { "agreed" } else { "disagreed" }
                    )
                    .increment(1);
                    if !agreed {
                        tracing::warn!(
                            signature,
                            endpoint = %endpoint.host,
                            served_by = %served_by.host,
                            "the solana endpoints disagree on a credit-grade \
                             transaction; the deposit stays pending"
                        );
                    }
                    return agreed;
                }
                Err(e) => {
                    endpoint.note_failure();
                    metrics::counter!(
                        "solana_rpc_errors_total",
                        "call" => "validate_transfer_confirm",
                        "endpoint" => endpoint.host.clone()
                    )
                    .increment(1);
                    metrics::counter!(
                        "solana_rpc_cross_confirmations_total",
                        "endpoint" => endpoint.host.clone(),
                        "outcome" => "unreachable"
                    )
                    .increment(1);
                    tracing::warn!(
                        signature,
                        endpoint = %endpoint.host,
                        error = %e.0,
                        "a cross-confirmation endpoint could not answer"
                    );
                }
            }
        }
        // No second endpoint could be tried or answered: every sibling is
        // cooling down or failing (a pool that is effectively single-endpoint
        // right now). Unverified is the safe answer — the first endpoint's
        // word alone is not a credit basis for a multi-endpoint deployment.
        tracing::warn!(
            signature,
            served_by = %served_by.host,
            "no second solana endpoint is healthy enough to cross-confirm; \
             the deposit stays pending"
        );
        false
    }
}

/// The per-transaction legs `inspect_received` collects (issue #1188): one
/// [`ScannedTransfer`] per whitelisted asset a transaction delivered into
/// `payTo`. Native SOL's lamport delta leads, then every whitelisted SPL
/// mint whose token-account delta on `payTo` came out positive — a single
/// transaction that credits SOL *and* a token (a multi-hop swap settling
/// both legs at once) reports one row per asset, never just the first that
/// matched. An empty Vec means the transaction moved nothing into the vault.
fn received_legs(
    signature: &str,
    meta: &Value,
    account_keys: &[Value],
    pay_to: &str,
    mints: &[MintWhitelistEntry],
    block_time: Option<i64>,
) -> Vec<ScannedTransfer> {
    let mut legs = Vec::new();
    // Native SOL: `payTo`'s own post-minus-pre lamport delta — the vault's
    // gain. A non-positive delta means nothing arrived (the tx may still
    // touch payTo — as fee payer, as an outbound source).
    if let Some(delta) = lamport_delta(
        account_keys,
        meta["preBalances"].as_array(),
        meta["postBalances"].as_array(),
        pay_to,
    ) && delta > 0
    {
        let payer = sol_payers(
            account_keys,
            meta["preBalances"].as_array(),
            meta["postBalances"].as_array(),
        )
        .into_iter()
        .next();
        legs.push(ScannedTransfer {
            signature: signature.to_owned(),
            block_time,
            asset: Some("sol".to_owned()),
            delivered: delta,
            payer,
        });
    }
    // Each whitelisted SPL mint: the token-account delta owned by `payTo`
    // on that mint. `token_delta` already matches `owner` or the account
    // address itself, so a payTo that *is* an ATA still attributes. Every
    // positive mint reports its own row — dropping the second leg silently
    // under-counted the scan before issue #1188.
    for asset in mints {
        let Some(mint) = asset.mint.as_deref() else {
            continue;
        };
        if let Some(delta) = token_delta(
            meta["postTokenBalances"].as_array(),
            meta["preTokenBalances"].as_array(),
            account_keys,
            pay_to,
            mint,
        ) && delta > 0
        {
            let payer = spl_payers(
                meta["preTokenBalances"].as_array(),
                meta["postTokenBalances"].as_array(),
                account_keys,
                mint,
            )
            .into_iter()
            .next();
            legs.push(ScannedTransfer {
                signature: signature.to_owned(),
                block_time,
                asset: Some(asset.key.clone()),
                delivered: delta,
                payer,
            });
        }
    }
    legs
}

/// The classic SPL mint layout's `decimals` field, checked against the whole
/// layout first (issue #1182): the buffer must be exactly
/// [`SPL_MINT_DATA_LEN`] bytes — the fixed classic mint size — and its
/// `isInitialized` flag (byte 45) must be set. A truncated account, a padded
/// Token-2022 blob, or an uninitialized mint is `Err` rather than a decimals
/// read off bytes that are not a mint.
fn mint_decimals_of(bytes: &[u8]) -> Result<u32, SolanaError> {
    if bytes.len() != SPL_MINT_DATA_LEN {
        return Err(SolanaError(format!(
            "mint account data is {} bytes, not the {SPL_MINT_DATA_LEN}-byte classic SPL mint layout",
            bytes.len()
        )));
    }
    if bytes[SPL_MINT_IS_INITIALIZED_OFFSET] != 1 {
        return Err(SolanaError("the mint account is not initialized".into()));
    }
    // Layout: mintAuthority option+key (36) + supply (8) + decimals (1) +
    // isInitialized (1) + freezeAuthority option+key (36) — decimals is byte 44.
    Ok(u32::from(bytes[44]))
}

/// The `core` verifier's `ChainLookup` over this JSON-RPC client: the two
/// Solana Pay calls on the deployment's configured endpoint pool. A
/// transport/RPC failure propagates as `Err` — never flattened to "not paid"
/// — so the expiry sweep can tell a sick endpoint apart from a real unpaid
/// intent (issue #1174). `Ok` empty/None is the only "not paid" a sweep may
/// act on.
impl oxsum_core::ChainLookup for SolanaRpcClient {
    fn find_reference<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<String>, oxsum_core::LookupError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.find_reference(reference)
                .await
                .map_err(|e| oxsum_core::LookupError(e.0))
        })
    }

    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a oxsum_core::ExpectedTransfer,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<oxsum_core::ValidatedTransfer>, oxsum_core::LookupError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.validate_transfer(
                signature,
                &ExpectedTransfer {
                    recipient: expected.recipient.clone(),
                    amount: expected.amount,
                    spl_token_mint: expected.spl_token_mint.clone(),
                    reference: expected.reference.clone(),
                },
            )
            .await
            .map_err(|e| oxsum_core::LookupError(e.0))
            .map(|v| {
                v.map(|v| oxsum_core::ValidatedTransfer {
                    signature: v.signature,
                    delivered: v.delivered,
                    block_time: v.block_time,
                    payers: v.payers,
                })
            })
        })
    }

    fn validate_payout<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a oxsum_core::ExpectedPayout,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<oxsum_core::ValidatedTransfer>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.validate_payout(
                signature,
                &ExpectedPayout {
                    recipient: expected.recipient.clone(),
                    spl_token_mint: expected.spl_token_mint.clone(),
                    source: expected.source.clone(),
                },
            )
            .await
            .inspect_err(|e| {
                // Same distinction as validate_transfer: a transport failure
                // is not "the payout does not exist" — warn it so an
                // unreachable endpoint never reads as a dishonoring proof.
                tracing::warn!(
                    signature,
                    error = %e.0,
                    "solana validate_payout RPC failed; treating as unvalidated"
                );
            })
            .ok()
            .flatten()
            .map(|v| oxsum_core::ValidatedTransfer {
                signature: v.signature,
                delivered: v.delivered,
                block_time: v.block_time,
                payers: v.payers,
            })
        })
    }

    fn mint_decimals<'a>(
        &'a self,
        mint: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<u32>> + Send + 'a>> {
        Box::pin(async move { self.mint_decimals(mint).await.ok() })
    }

    fn mint_owner<'a>(
        &'a self,
        mint: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async move { self.mint_owner(mint).await.ok() })
    }

    fn scan_received<'a>(
        &'a self,
        max_signatures: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = oxsum_core::ScanWalk> + Send + 'a>>
    {
        Box::pin(async move {
            self.scan_received(max_signatures)
                .await
                .inspect_err(|e| {
                    // A transport/RPC failure is not "nothing arrived" — warn it
                    // so a blind scan never reads as a clean reconciliation;
                    // the per-endpoint count already landed inside `call`.
                    tracing::warn!(
                        error = %e.0,
                        "the solana reconciliation scan's chain walk failed"
                    );
                })
                .unwrap_or_default()
        })
    }
}

/// What the cross-confirmation demands a second endpoint agree on (issue
/// #1183): the same `getTransaction` shape `validate_transfer` reads —
/// `meta.err` null, the intent's `reference` among the account keys as a
/// read-only non-signer, and a recipient-side `delivered` delta on the
/// expected mint (or native lamports) *equal to* the figure the primary
/// endpoint reported. The same evidence extractors the primary validation
/// ran (`reference_marker_present`, `lamport_delta`/`token_delta`) re-run
/// here, so an endpoint that hallucinated a different amount, a different
/// recipient-side account, a missing marker or a failed transaction disagrees
/// — it is never enough that the signature merely exists.
fn cross_confirmation_agrees(result: &Value, expected: &ExpectedTransfer, delivered: i64) -> bool {
    if result.is_null() {
        return false;
    }
    let meta = &result["meta"];
    if !meta["err"].is_null() {
        return false;
    }
    let Some(account_keys) = result["transaction"]["message"]["accountKeys"].as_array() else {
        return false;
    };
    if !reference_marker_present(account_keys, &expected.reference) {
        return false;
    }
    let second = match expected.spl_token_mint.as_deref() {
        None => lamport_delta(
            account_keys,
            meta["preBalances"].as_array(),
            meta["postBalances"].as_array(),
            &expected.recipient,
        ),
        Some(mint) => token_delta(
            meta["postTokenBalances"].as_array(),
            meta["preTokenBalances"].as_array(),
            account_keys,
            &expected.recipient,
            mint,
        ),
    };
    second == Some(delivered)
}

/// The native-SOL counterpart of [`token_delta`]: the recipient's
/// post-minus-pre lamport delta. `accountKeys` indexes the `preBalances` /
/// `postBalances` arrays positionally (they cover the static, non-lookup keys
/// `jsonParsed` returns), so the recipient's position within `accountKeys` is
/// the index into both balance arrays. `None` when the recipient is not among
/// the transaction's account keys — the payment never reached the deposit
/// address.
/// The `reference` marker must appear as an account key — and as the
/// read-only, non-signer account the Solana Pay recipe attaches (issue #1130).
/// With `jsonParsed`, accountKeys entries are `{pubkey, signer, writable,
/// source}` objects; a bare-string form (a partially-decoded payload's signer
/// set) carries no flags and is accepted defensively, but a structured entry
/// must state `signer:false` and `writable:false` — a marker that signed or
/// mutated is not the throwaway index the rail keys on, and matching
/// `docs/architecture.md`'s "read-only non-signer" contract is the point.
fn reference_marker_present(account_keys: &[Value], reference: &str) -> bool {
    account_keys.iter().any(|key| {
        let pubkey = key["pubkey"].as_str().or_else(|| key.as_str());
        if pubkey != Some(reference) {
            return false;
        }
        match (key.get("signer"), key.get("writable")) {
            (None, None) => true,
            (signer, writable) => {
                signer.and_then(Value::as_bool) == Some(false)
                    && writable.and_then(Value::as_bool) == Some(false)
            }
        }
    })
}

/// A balance figure parsed without the `as_i64` precision trap: `u64` first
/// (the exact integer serde_json hands back), then a string form for any
/// node that arrived as a JSON string, never through an `f64` round-trip
/// which silently loses lamports past 2^53. The result saturates into `i64`
/// — the column's type — so an absurd balance clamps rather than wraps.
fn balance_i64(v: Option<&Value>) -> i64 {
    v.and_then(|n| n.as_u64().map(|u| u.min(i64::MAX as u64) as i64))
        .or_else(|| {
            v.and_then(Value::as_str)
                .and_then(|s| s.parse::<u128>().ok())
                .map(|u| u.min(i64::MAX as u128) as i64)
        })
        .unwrap_or(0)
}

fn lamport_delta(
    account_keys: &[Value],
    pre_balances: Option<&Vec<Value>>,
    post_balances: Option<&Vec<Value>>,
    recipient: &str,
) -> Option<i64> {
    let index = account_keys
        .iter()
        .position(|key| key["pubkey"].as_str().or_else(|| key.as_str()) == Some(recipient))?;
    let pre = balance_i64(pre_balances.and_then(|b| b.get(index)));
    let post = balance_i64(post_balances.and_then(|b| b.get(index)));
    Some(post.saturating_sub(pre))
}

/// The first signer among `accountKeys` — Solana's ordering puts the fee
/// payer at index 0 and every signature account first under `jsonParsed`'s
/// `{pubkey, signer, writable}` shape; a bare-string form reads position 0.
/// `None` when the keys carry no signer at all — a shape the rail cannot
/// name a payer from.
fn first_signer(account_keys: &[Value]) -> Option<String> {
    // Structured keys: the first entry flagged `signer: true`. Bare strings
    // carry no flags — position 0 is the fee payer by the wire's own ordering.
    for key in account_keys {
        if let Some(pubkey) = key["pubkey"].as_str() {
            if key["signer"].as_bool() == Some(true) {
                return Some(pubkey.to_owned());
            }
        } else if key.is_string() {
            return key.as_str().map(str::to_owned);
        }
    }
    None
}

/// The native-SOL funding parties (issues #1144, #1178): *every* signer whose
/// lamport balance *decreased* — the accounts the transfer debited — rather
/// than blindly `accountKeys[0]`, and rather than stopping at the first hit.
/// Screening must cover each debited party: a sanctioned source wallet that
/// hides behind a clean fee-paying relayer would otherwise evade the check,
/// so the funding source(s) *and* the fee payer all ride the answer in key
/// order. When the balance arrays isolate no debited signer — an absent or
/// all-zero delta — the list falls back to the first signer (the fee payer),
/// which still names *a* party to the payment rather than none.
fn sol_payers(
    account_keys: &[Value],
    pre_balances: Option<&Vec<Value>>,
    post_balances: Option<&Vec<Value>>,
) -> Vec<String> {
    let mut payers: Vec<String> = Vec::new();
    if let (Some(pre), Some(post)) = (pre_balances, post_balances) {
        // A signer whose balance fell paid for something — the transfer
        // and/or the fee. Several can fall (fee payer + a second funding
        // signer); every one is a funding party the screen must clear.
        for (index, key) in account_keys.iter().enumerate() {
            let is_signer =
                key["signer"].as_bool() == Some(true) || (key.is_string() && index == 0);
            if !is_signer {
                continue;
            }
            let pre = balance_i64(pre.get(index));
            let post = balance_i64(post.get(index));
            if post < pre
                && let Some(pubkey) = key["pubkey"].as_str().or_else(|| key.as_str())
                && !payers.iter().any(|p| p == pubkey)
            {
                payers.push(pubkey.to_owned());
            }
        }
    }
    if payers.is_empty() {
        payers.extend(first_signer(account_keys));
    }
    payers
}

/// The SPL funding parties (issues #1144, #1178): the `owner` of *every*
/// token account whose `preTokenBalances` → `postTokenBalances` figure
/// *decreased* on the paid mint — each wallet tokens left. Matching rows by
/// `accountIndex` keeps each pair on one account; a payment funded from two
/// accounts names both owners so screening covers every debited party, the
/// same coverage the SOL arm gives. When the token rows carry no usable
/// decrease or owner — a shape `jsonParsed` does not surface — the answer
/// falls back to the first signer (the wallet that authorized the transfer),
/// the same residual the SOL arm documents.
fn spl_payers(
    pre: Option<&Vec<Value>>,
    post: Option<&Vec<Value>>,
    account_keys: &[Value],
    mint: &str,
) -> Vec<String> {
    let mut payers: Vec<String> = Vec::new();
    if let Some(pre_rows) = pre {
        for pre_row in pre_rows {
            if pre_row["mint"].as_str() != Some(mint) {
                continue;
            }
            // A malformed row — no accountIndex, an unparseable amount — skips
            // to the next rather than ending the walk: a payer may still sit
            // on a later well-formed row of the same mint.
            let Some(index) = pre_row["accountIndex"].as_u64() else {
                continue;
            };
            // Pair the row with its post-state on the same accountIndex +
            // mint; a missing post row means the account closed or drained to
            // zero — the decrease is the whole pre figure.
            let post_amount: i64 = post
                .and_then(|rows| {
                    rows.iter().find(|p| {
                        p["accountIndex"].as_u64() == Some(index)
                            && p["mint"].as_str() == Some(mint)
                    })
                })
                .and_then(|p| p["uiTokenAmount"]["amount"].as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let Some(pre_amount) = pre_row["uiTokenAmount"]["amount"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
            else {
                continue;
            };
            if post_amount >= pre_amount {
                continue;
            }
            // The debited token account's owner is the funding party — the
            // wallet the tokens left, not the ATA address itself. No owner on
            // the row: fall back to the account's own address — still a party
            // to the payment, and the shape some RPCs return.
            let payer = pre_row["owner"].as_str().or_else(|| {
                account_keys
                    .get(index as usize)
                    .and_then(|k| k["pubkey"].as_str().or_else(|| k.as_str()))
            });
            if let Some(payer) = payer
                && !payers.iter().any(|p| p == payer)
            {
                payers.push(payer.to_owned());
            }
        }
    }
    if payers.is_empty() {
        payers.extend(first_signer(account_keys));
    }
    payers
}

/// Whether the account `source` lost at least `delivered` lamports in this
/// transaction — the refund-payout source proof (issue #1209). Unlike
/// [`sol_payers`] the check ignores the signer flag entirely: a Squads vault
/// PDA spends through CPI and never signs, so requiring `signer: true` would
/// refuse the exact payout shape the rail is built to take. The vault's
/// position in `accountKeys` indexes `preBalances`/`postBalances`
/// positionally, the same addressing `lamport_delta` uses; `pre - post >=
/// delivered` is the proof. Balance arrays that cannot answer — missing, or
/// `source` not among the keys — are no proof.
fn source_lamports_decreased(
    account_keys: &[Value],
    pre_balances: Option<&Vec<Value>>,
    post_balances: Option<&Vec<Value>>,
    source: &str,
    delivered: i64,
) -> bool {
    let Some(index) = account_keys
        .iter()
        .position(|key| key["pubkey"].as_str().or_else(|| key.as_str()) == Some(source))
    else {
        return false;
    };
    let pre = balance_i64(pre_balances.and_then(|b| b.get(index)));
    let post = balance_i64(post_balances.and_then(|b| b.get(index)));
    pre.saturating_sub(post) >= delivered
}

/// The SPL arm of the source proof (issue #1209): a token account *owned by*
/// `source` whose balance on `mint` fell by at least `delivered`. Rows pair on
/// `accountIndex` + `mint` between `preTokenBalances` and `postTokenBalances`;
/// a missing post row means the account drained or closed — the decrease is
/// the whole pre figure, the same convention `spl_payers` follows. The owner
/// comes from the row's `owner` field (a Squads vault owns its ATA without
/// signing); a row with no owner falls back to the account's own pubkey —
/// `source` holding the token account itself. No token row can carry
/// `accountIndex` above `usize::MAX` rows of `accountKeys`, so a malformed
/// index just never matches.
fn source_token_decreased(
    pre: Option<&Vec<Value>>,
    post: Option<&Vec<Value>>,
    account_keys: &[Value],
    source: &str,
    mint: &str,
    delivered: i64,
) -> bool {
    let Some(pre_rows) = pre else {
        return false;
    };
    let mut decreased: i64 = 0;
    for pre_row in pre_rows {
        if pre_row["mint"].as_str() != Some(mint) {
            continue;
        }
        let Some(index) = pre_row["accountIndex"].as_u64() else {
            continue;
        };
        let owned = pre_row["owner"].as_str() == Some(source)
            || account_keys
                .get(index as usize)
                .and_then(|k| k["pubkey"].as_str().or_else(|| k.as_str()))
                == Some(source);
        if !owned {
            continue;
        }
        let post_amount: i64 = post
            .and_then(|rows| {
                rows.iter().find(|p| {
                    p["accountIndex"].as_u64() == Some(index) && p["mint"].as_str() == Some(mint)
                })
            })
            .and_then(|p| p["uiTokenAmount"]["amount"].as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let Some(pre_amount) = pre_row["uiTokenAmount"]["amount"]
            .as_str()
            .and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };
        decreased = decreased.saturating_add(pre_amount.saturating_sub(post_amount));
        if decreased >= delivered {
            return true;
        }
    }
    false
}

/// The SPL-token counterpart of the native-balance delta: the difference
/// between the recipient-mint token account's post and pre `uiTokenAmount.
/// amount` (a base-unit string), matching rows by `accountIndex` and `mint`.
/// `None` when no `postTokenBalances` row sits on the configured mint at all —
/// the transfer never reached the token account we price.
fn token_delta(
    post: Option<&Vec<Value>>,
    pre: Option<&Vec<Value>>,
    account_keys: &[Value],
    recipient: &str,
    mint: &str,
) -> Option<i64> {
    let post_rows = post?;
    // Locate the post-state token account row for (mint) whose account is one
    // of the transaction's accountKeys and whose `owner` is the recipient —
    // `jsonParsed` exposes `owner` on postTokenBalances rows. Fall back to any
    // row on the mint whose accountIndex's pubkey is the recipient, which
    // covers a recipient that *is* the ATA's address.
    for row in post_rows {
        if row["mint"].as_str() != Some(mint) {
            continue;
        }
        let index = row["accountIndex"].as_u64()? as usize;
        let matches_owner = row["owner"].as_str() == Some(recipient);
        let matches_account = account_keys
            .get(index)
            .and_then(|k| k["pubkey"].as_str().or_else(|| k.as_str()))
            == Some(recipient);
        if !(matches_owner || matches_account) {
            continue;
        }
        let post_amount: i64 = row["uiTokenAmount"]["amount"]
            .as_str()
            .and_then(|s| s.parse().ok())?;
        let pre_amount: i64 = pre
            .and_then(|rows| {
                rows.iter().find(|p| {
                    p["accountIndex"].as_u64() == Some(index as u64)
                        && p["mint"].as_str() == Some(mint)
                })
            })
            .and_then(|p| p["uiTokenAmount"]["amount"].as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        return Some(post_amount.saturating_sub(pre_amount));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `getTransaction(signature, jsonParsed)` payload for an SPL
    /// `TransferChecked` — the shape `token_delta` reads. `accountKeys` are
    /// `{pubkey, signer, writable, source}` objects under `jsonParsed`;
    /// `postTokenBalances` rows expose `owner`, `mint`, `accountIndex` and
    /// `uiTokenAmount.amount` (a base-unit string). `preTokenBalances` is the
    /// same shape before the transfer, so a pre-funded ATA can't claim its
    /// prior holdings.
    fn spl_tx(post_amount: &str, pre_amount: Option<&str>, mint: &str, owner: &str) -> Value {
        let pre_row = pre_amount.map(|amount| {
            json!({
                "accountIndex": 3, "mint": mint, "owner": owner,
                "uiTokenAmount": {"amount": amount, "decimals": 6, "uiAmountString": "0"}
            })
        });
        json!({
            "meta": {
                "err": null,
                "preBalances": [],
                "postBalances": [],
                "preTokenBalances": pre_row.map_or_else(|| json!([]), |r| json!([r])),
                "postTokenBalances": [{
                    "accountIndex": 3, "mint": mint, "owner": owner,
                    "uiTokenAmount": {"amount": post_amount, "decimals": 6, "uiAmountString": "0"}
                }]
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "PAYER_WALLET", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "MINT_ATA_FROM", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": mint, "signer": false, "writable": false, "source": "transaction"},
                        {"pubkey": "MINT_ATA_TO", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": owner, "signer": false, "writable": false, "source": "transaction"},
                        {"pubkey": "REFERENCE_MARKER", "signer": false, "writable": false, "source": "transaction"}
                    ]
                }
            }
        })
    }

    /// The `accountKeys` array the delta walks (same JSON shape the RPC
    /// answer carries) — index 3 is the recipient's ATA, index 4 its owner.
    fn keys() -> Vec<Value> {
        spl_tx("0", None, "m", "o")["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn token_delta_pays_the_recipient_mint() {
        let tx = spl_tx("5000000", None, "usdcMint", "payToAddr");
        let delta = token_delta(
            tx["meta"]["postTokenBalances"].as_array(),
            tx["meta"]["preTokenBalances"].as_array(),
            &keys(),
            "payToAddr",
            "usdcMint",
        );
        assert_eq!(delta, Some(5_000_000));
    }

    #[test]
    fn token_delta_subtracts_a_prefunded_ata() {
        // The recipient's ATA already held tokens; the deposit only counts
        // what this transfer added, not the closing balance.
        let tx = spl_tx("7500000", Some("2500000"), "usdcMint", "payToAddr");
        let delta = token_delta(
            tx["meta"]["postTokenBalances"].as_array(),
            tx["meta"]["preTokenBalances"].as_array(),
            &keys(),
            "payToAddr",
            "usdcMint",
        );
        assert_eq!(delta, Some(5_000_000));
    }

    #[test]
    fn token_delta_refuses_a_wrong_mint() {
        let tx = spl_tx("5000000", None, "worthlessMint", "payToAddr");
        let delta = token_delta(
            tx["meta"]["postTokenBalances"].as_array(),
            tx["meta"]["preTokenBalances"].as_array(),
            &keys(),
            "payToAddr",
            "usdcMint",
        );
        assert_eq!(delta, None);
    }

    #[test]
    fn token_delta_refuses_a_different_owner() {
        // A postTokenBalances row on the right mint but owned by someone
        // else — the payment went to a different wallet's ATA.
        let tx = spl_tx("5000000", None, "usdcMint", "anotherWallet");
        let delta = token_delta(
            tx["meta"]["postTokenBalances"].as_array(),
            tx["meta"]["preTokenBalances"].as_array(),
            &keys(),
            "payToAddr",
            "usdcMint",
        );
        assert_eq!(delta, None);
    }

    #[test]
    fn token_delta_matches_the_ata_address_too() {
        // When the recipient *is* the ATA address (not the owner), the
        // accountIndex fallback still matches — a deployment can name an ATA
        // as `pay_to` and the row is still attributed.
        let tx = spl_tx("5000000", None, "usdcMint", "someOwner");
        let delta = token_delta(
            tx["meta"]["postTokenBalances"].as_array(),
            tx["meta"]["preTokenBalances"].as_array(),
            &keys(),
            "MINT_ATA_TO",
            "usdcMint",
        );
        assert_eq!(delta, Some(5_000_000));
    }

    /// A `getTransaction(signature, jsonParsed)` payload for a native-SOL
    /// `SystemProgram.transfer` — the shape `lamport_delta` reads. The
    /// recipient sits at `accountKeys[1]` and its lamport figure is the
    /// post-minus-pre delta across `preBalances`/`postBalances` at index 1.
    /// `REFERENCE_MARKER` rides as a read-only non-signer key.
    fn sol_tx(pre_lamports: i64, post_lamports: i64, recipient: &str) -> Value {
        json!({
            "meta": {
                "err": null,
                "preBalances": [5_000_000_000_i64, pre_lamports, 1],
                "postBalances": [4_999_995_000_i64, post_lamports, 1],
                "preTokenBalances": [],
                "postTokenBalances": []
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "PAYER_WALLET", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": recipient, "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": "REFERENCE_MARKER", "signer": false, "writable": false, "source": "transaction"}
                    ]
                }
            }
        })
    }

    #[test]
    fn lamport_delta_pays_the_recipient() {
        let tx = sol_tx(0, 5_000, "payToAddr");
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let delta = lamport_delta(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
            "payToAddr",
        );
        assert_eq!(delta, Some(5_000));
    }

    #[test]
    fn lamport_delta_subtracts_a_prefunded_recipient() {
        // The deposit address already held lamports; the deposit counts only
        // what this transfer added, not the closing balance.
        let tx = sol_tx(10_000, 15_000, "payToAddr");
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let delta = lamport_delta(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
            "payToAddr",
        );
        assert_eq!(delta, Some(5_000));
    }

    #[test]
    fn lamport_delta_keeps_precision_past_two_pow_53() {
        // A balance beyond f64's 53-bit mantissa must not round: 2^53 + 2
        // lamports pre, 2^53 + 5002 post — the delta is exactly 5000, which
        // an f64 round-trip would blur into a wrong figure.
        let big_pre: i64 = 9_007_199_254_740_994; // 2^53 + 2
        let tx = sol_tx(big_pre, big_pre + 5_000, "payToAddr");
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let delta = lamport_delta(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
            "payToAddr",
        );
        assert_eq!(delta, Some(5_000));
    }

    /// The reference marker must be a read-only non-signer key (issue #1130):
    /// a structured `{pubkey, signer, writable}` entry that signed or was
    /// writable is not the throwaway index the rail keys on.
    #[test]
    fn reference_marker_must_be_read_only_non_signer() {
        // Read-only non-signer — the Solana Pay recipe's shape — is accepted.
        let good = json!([
            {"pubkey": "PAYER", "signer": true, "writable": true, "source": "transaction"},
            {"pubkey": "REF", "signer": false, "writable": false, "source": "transaction"}
        ]);
        assert!(reference_marker_present(good.as_array().unwrap(), "REF"));
        // A marker that signed the transaction is not the index.
        let signer = json!([
            {"pubkey": "PAYER", "signer": true, "writable": true, "source": "transaction"},
            {"pubkey": "REF", "signer": true, "writable": false, "source": "transaction"}
        ]);
        assert!(!reference_marker_present(signer.as_array().unwrap(), "REF"));
        // A writable marker is not read-only.
        let writable = json!([
            {"pubkey": "PAYER", "signer": true, "writable": true, "source": "transaction"},
            {"pubkey": "REF", "signer": false, "writable": true, "source": "transaction"}
        ]);
        assert!(!reference_marker_present(
            writable.as_array().unwrap(),
            "REF"
        ));
        // A bare-string key (no flags) is tolerated defensively.
        let bare = json!(["PAYER", "REF"]);
        assert!(reference_marker_present(bare.as_array().unwrap(), "REF"));
        // Absent entirely.
        let none = json!([{"pubkey": "OTHER", "signer": false, "writable": false}]);
        assert!(!reference_marker_present(none.as_array().unwrap(), "REF"));
    }

    #[test]
    fn lamport_delta_is_none_for_an_absent_recipient() {
        let tx = sol_tx(0, 5_000, "otherAddr");
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let delta = lamport_delta(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
            "payToAddr",
        );
        assert_eq!(delta, None);
    }

    /// Issue #1144: the payer a screen runs on. For native SOL it is the
    /// signer whose lamport balance fell — the wallet the transfer debited —
    /// which in the ordinary transfer is the fee payer at `accountKeys[0]`.
    #[test]
    fn sol_payers_names_the_debited_signer() {
        let tx = sol_tx(0, 5_000, "payToAddr");
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let payers = sol_payers(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
        );
        assert_eq!(payers, vec!["PAYER_WALLET".to_owned()]);
    }

    /// Issue #1178: a fee payer distinct from the source — a relayer at index
    /// 0 pays only the fee while a second signer's balance drops by the
    /// transfer amount — names *both* debited signers, in key order. Screening
    /// every funding party is the point: a sanctioned source can no longer
    /// hide behind a clean relayer's fee-only debit.
    #[test]
    fn sol_payers_names_every_debited_signer() {
        let tx = json!({
            "meta": {
                "err": null,
                // Index 0 (relayer) lost only the fee; index 1 (source) lost
                // the transfer itself; index 2 is the recipient.
                "preBalances":  [5_000_000_000_i64, 9_000_000_000_i64, 0],
                "postBalances": [4_999_995_000_i64, 8_999_995_000_i64, 10_000],
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "RELAYER", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "SOURCE", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "RECIPIENT", "signer": false, "writable": true, "source": "transaction"}
                    ]
                }
            }
        });
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let payers = sol_payers(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
        );
        // Both signers' balances fell — the fee payer and the funding source —
        // and the answer carries both in key order.
        assert_eq!(payers, vec!["RELAYER".to_owned(), "SOURCE".to_owned()]);
    }

    /// When the balance arrays isolate no debited signer the answer falls
    /// back to the first signer — the fee payer, still a party to the
    /// payment rather than none.
    #[test]
    fn sol_payers_falls_back_to_the_first_signer() {
        let tx = json!({
            "meta": {
                "err": null,
                // No signer's balance fell — an empty-delta shape.
                "preBalances":  [5_000_000_000_i64, 0],
                "postBalances": [5_000_000_000_i64, 0],
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "FEE_PAYER", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "RECIPIENT", "signer": false, "writable": true, "source": "transaction"}
                    ]
                }
            }
        });
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let payers = sol_payers(
            keys,
            tx["meta"]["preBalances"].as_array(),
            tx["meta"]["postBalances"].as_array(),
        );
        assert_eq!(payers, vec!["FEE_PAYER".to_owned()]);
    }

    /// For an SPL transfer the payers are the `owner`s of the token accounts
    /// that decreased on the paid mint — the wallets the tokens left.
    #[test]
    fn spl_payers_names_the_debited_owner() {
        // Index 1 is the sender's ATA (decreased), index 3 the recipient's
        // (increased); both rows ride the same mint.
        let mut tx = spl_tx("5000000", None, "usdcMint", "payToAddr");
        tx["meta"]["preTokenBalances"] = json!([
            {
                "accountIndex": 1, "mint": "usdcMint", "owner": "PAYER_WALLET",
                "uiTokenAmount": {"amount": "9000000", "decimals": 6, "uiAmountString": "9"}
            },
            {
                "accountIndex": 3, "mint": "usdcMint", "owner": "payToAddr",
                "uiTokenAmount": {"amount": "0", "decimals": 6, "uiAmountString": "0"}
            }
        ]);
        tx["meta"]["postTokenBalances"] = json!([
            {
                "accountIndex": 1, "mint": "usdcMint", "owner": "PAYER_WALLET",
                "uiTokenAmount": {"amount": "4000000", "decimals": 6, "uiAmountString": "4"}
            },
            {
                "accountIndex": 3, "mint": "usdcMint", "owner": "payToAddr",
                "uiTokenAmount": {"amount": "5000000", "decimals": 6, "uiAmountString": "5"}
            }
        ]);
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let payers = spl_payers(
            tx["meta"]["preTokenBalances"].as_array(),
            tx["meta"]["postTokenBalances"].as_array(),
            keys,
            "usdcMint",
        );
        assert_eq!(payers, vec!["PAYER_WALLET".to_owned()]);
    }

    /// Issue #1178: a payment funded from two token accounts on the paid mint
    /// names both debited owners — screening must clear each, not just the
    /// first the rows happened to list.
    #[test]
    fn spl_payers_names_every_debited_owner() {
        let mut tx = spl_tx("5000000", None, "usdcMint", "payToAddr");
        tx["meta"]["preTokenBalances"] = json!([
            {
                "accountIndex": 1, "mint": "usdcMint", "owner": "FIRST_OWNER",
                "uiTokenAmount": {"amount": "9000000", "decimals": 6, "uiAmountString": "9"}
            },
            {
                "accountIndex": 2, "mint": "usdcMint", "owner": "SECOND_OWNER",
                "uiTokenAmount": {"amount": "500000", "decimals": 6, "uiAmountString": "0.5"}
            }
        ]);
        tx["meta"]["postTokenBalances"] = json!([
            {
                "accountIndex": 1, "mint": "usdcMint", "owner": "FIRST_OWNER",
                "uiTokenAmount": {"amount": "4500000", "decimals": 6, "uiAmountString": "4.5"}
            },
            {
                "accountIndex": 2, "mint": "usdcMint", "owner": "SECOND_OWNER",
                "uiTokenAmount": {"amount": "0", "decimals": 6, "uiAmountString": "0"}
            },
            {
                "accountIndex": 3, "mint": "usdcMint", "owner": "payToAddr",
                "uiTokenAmount": {"amount": "5000000", "decimals": 6, "uiAmountString": "5"}
            }
        ]);
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let payers = spl_payers(
            tx["meta"]["preTokenBalances"].as_array(),
            tx["meta"]["postTokenBalances"].as_array(),
            keys,
            "usdcMint",
        );
        assert_eq!(
            payers,
            vec!["FIRST_OWNER".to_owned(), "SECOND_OWNER".to_owned()]
        );
    }

    /// A preTokenBalances row on a *different* mint decreasing does not name
    /// a payer — the screen keys on the paid mint's own debit.
    #[test]
    fn spl_payers_ignores_a_decrease_on_another_mint() {
        let mut tx = spl_tx("5000000", None, "usdcMint", "payToAddr");
        tx["meta"]["preTokenBalances"] = json!([
            {
                "accountIndex": 1, "mint": "otherMint", "owner": "PAYER_WALLET",
                "uiTokenAmount": {"amount": "9000000", "decimals": 6, "uiAmountString": "9"}
            }
        ]);
        tx["meta"]["postTokenBalances"] = json!([
            {
                "accountIndex": 1, "mint": "otherMint", "owner": "PAYER_WALLET",
                "uiTokenAmount": {"amount": "4000000", "decimals": 6, "uiAmountString": "4"}
            },
            {
                "accountIndex": 3, "mint": "usdcMint", "owner": "payToAddr",
                "uiTokenAmount": {"amount": "5000000", "decimals": 6, "uiAmountString": "5"}
            }
        ]);
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let payers = spl_payers(
            tx["meta"]["preTokenBalances"].as_array(),
            tx["meta"]["postTokenBalances"].as_array(),
            keys,
            "usdcMint",
        );
        // No paid-mint decrease: the fallback names the first signer, the
        // wallet that authorized the transfer — PAYER_WALLET at index 0.
        assert_eq!(payers, vec!["PAYER_WALLET".to_owned()]);
    }

    #[test]
    fn token_delta_is_none_without_post_token_balances() {
        let mut tx = spl_tx("0", None, "m", "o");
        tx["meta"]["postTokenBalances"] = json!([]);
        let delta = token_delta(
            tx["meta"]["postTokenBalances"].as_array(),
            None,
            &keys(),
            "payToAddr",
            "usdcMint",
        );
        assert_eq!(delta, None);
    }

    /// #1188's second half: one transaction that credits `payTo` in SOL *and*
    /// a whitelisted SPL mint reports one row per asset — the old
    /// `inspect_received` early-returned on the SOL leg and the SPL leg never
    /// surfaced. `received_legs` is the pure mapping `inspect_received`
    /// delegates to, so the multi-asset shape is locked without an RPC.
    #[test]
    fn received_legs_reports_every_asset_a_transfer_credits() {
        let mint = "usdcMint";
        // One tx: a native lamport delta into `payToAddr` (index 1) AND an
        // SPL token-account delta on `mint` owned by `payToAddr` (index 3) —
        // the multi-hop-settlement shape a payment router produces.
        let tx = json!({
            "meta": {
                "err": null,
                "preBalances": [5_000_000_000_i64, 0, 1, 1],
                "postBalances": [4_999_995_000_i64, 5_000, 1, 1],
                "preTokenBalances": [],
                "postTokenBalances": [{
                    "accountIndex": 3, "mint": mint, "owner": "payToAddr",
                    "uiTokenAmount": {"amount": "7500000", "decimals": 6, "uiAmountString": "7.5"}
                }]
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "PAYER_WALLET", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "payToAddr", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": mint, "signer": false, "writable": false, "source": "transaction"},
                        {"pubkey": "PAYTO_ATA", "signer": false, "writable": true, "source": "transaction"}
                    ]
                }
            }
        });
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let mints = vec![
            MintWhitelistEntry {
                key: "sol".to_owned(),
                mint: None,
            },
            MintWhitelistEntry {
                key: "usdc".to_owned(),
                mint: Some(mint.to_owned()),
            },
        ];
        let legs = received_legs(
            "sigMulti",
            &tx["meta"],
            keys,
            "payToAddr",
            &mints,
            Some(1_700_000_000),
        );
        assert_eq!(legs.len(), 2, "one row per credited asset: {legs:?}");
        assert_eq!(legs[0].asset.as_deref(), Some("sol"));
        assert_eq!(legs[0].delivered, 5_000);
        assert_eq!(legs[1].asset.as_deref(), Some("usdc"));
        assert_eq!(legs[1].delivered, 7_500_000);
        assert_eq!(legs[0].signature, "sigMulti");
        assert_eq!(legs[1].signature, "sigMulti");
        // And a transaction that moves nothing into the vault stays empty —
        // a `payTo`-signed outbound send reads exactly like before.
        let outbound = sol_tx(5_000, 0, "payToAddr");
        let outbound_keys = outbound["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let legs = received_legs(
            "sigOut",
            &outbound["meta"],
            outbound_keys,
            "payToAddr",
            &mints,
            None,
        );
        assert!(
            legs.is_empty(),
            "an outbound send credits nothing: {legs:?}"
        );
    }

    /// The `endpoint` metric label's spelling: host (plus a non-default
    /// port), with the URL's query — where an API key lives — dropped. This
    /// is the only place a label is derived, so the key can never reach a
    /// metric.
    #[test]
    fn the_endpoint_label_is_the_host_never_the_url() {
        assert_eq!(
            endpoint_host("https://mainnet.helius-rpc.com/?api-key=SECRET123"),
            Some("mainnet.helius-rpc.com".to_owned())
        );
        assert_eq!(
            endpoint_host("https://api.devnet.solana.com"),
            Some("api.devnet.solana.com".to_owned())
        );
        // A non-default port survives — a local validator's label stays
        // distinct from the public endpoint's.
        assert_eq!(
            endpoint_host("http://127.0.0.1:8899"),
            Some("127.0.0.1:8899".to_owned())
        );
        assert_eq!(endpoint_host("not a url"), None);
        // The query — and so any key inside it — is never in the label.
        let label = endpoint_host("https://rpc.example.com/?api-key=LEAKME&x=1").unwrap();
        assert!(!label.contains("LEAKME"), "{label}");
    }

    /// A test deployment's config: the endpoints the harness stands up, a
    /// native-SOL-only whitelist, everything else at its default.
    fn test_config(urls: &[String], auth_header: Option<&str>) -> SolanaConfig {
        SolanaConfig {
            rpc_urls: urls.to_vec(),
            pay_to: "payTo".into(),
            mints: vec![MintWhitelistEntry {
                key: "sol".to_owned(),
                mint: None,
            }],
            commitment: "finalized".into(),
            rpc_auth_header: auth_header.map(str::to_owned),
        }
    }

    /// A stub JSON-RPC server answering every POST the same body. Returns
    /// the base URL plus a hit count.
    async fn stub_rpc(
        answer: serde_json::Value,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::routing::post;
        use axum::{Json, Router};
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits2 = hits.clone();
        let app = Router::new().route(
            "/",
            post(move |Json(_body): Json<serde_json::Value>| {
                let hits = hits2.clone();
                let answer = answer.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Json(answer)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://127.0.0.1:{port}"), hits)
    }

    /// The client the failover tests build: one `try_new` call over the
    /// listed endpoints.
    fn client(urls: &[String], auth_header: Option<&str>) -> SolanaRpcClient {
        SolanaRpcClient::try_new(
            urls.to_vec(),
            reqwest::Client::new(),
            test_config(urls, auth_header),
        )
        .expect("the test's endpoints all carry hosts")
    }

    /// A `getSignaturesForAddress`-shaped answer carrying one signature — the
    /// body `find_reference` walks.
    fn signatures_result(signatures: &[&str]) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": signatures.iter()
                .map(|s| json!({"signature": s}))
                .collect::<Vec<_>>()
        })
    }

    /// First-healthy-wins: endpoint A answers, endpoint B is never asked —
    /// the walk stops at the first working answer.
    #[tokio::test(flavor = "current_thread")]
    async fn a_healthy_first_endpoint_answers_without_asking_the_second() {
        let (url_a, hits_a) = stub_rpc(signatures_result(&["sig1"])).await;
        let (url_b, hits_b) = stub_rpc(signatures_result(&["sig2"])).await;
        let client = client(&[url_a, url_b], None);
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sig1".to_owned()]);
        assert_eq!(
            hits_a.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the first endpoint served the call"
        );
        assert_eq!(
            hits_b.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the second endpoint was never asked"
        );
    }

    /// A transport failure on the first endpoint fails the call over to the
    /// second — the round-robin cursor lands the call on whichever endpoint
    /// the walk starts at, so the test drives the cursor explicitly.
    #[tokio::test(flavor = "current_thread")]
    async fn a_transport_failure_fails_over_to_the_next_endpoint() {
        let (url_a, hits_a) = stub_rpc(signatures_result(&["sigA"])).await;
        let (url_b, hits_b) = stub_rpc(signatures_result(&["sigB"])).await;
        let urls = vec![url_a, url_b];
        let client = client(&urls, None);
        // Kill endpoint 0's streak so the call skips it and lands on
        // endpoint 1 — the dead-provider shape a real outage takes.
        client.endpoints[0]
            .consecutive_failures
            .store(ENDPOINT_FAILURE_THRESHOLD, Ordering::Relaxed);
        *client.endpoints[0].unhealthy_until.lock().unwrap() =
            Some(Instant::now() + ENDPOINT_COOLDOWN);
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sigB".to_owned()]);
        assert_eq!(hits_a.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(hits_b.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The circuit breaker: after `ENDPOINT_FAILURE_THRESHOLD` consecutive
    /// failures an endpoint sits out the cooldown — the walk skips it even
    /// when it would have been next — so a dead provider stops eating the
    /// per-call timeout every pass. Round-robin alternates the walk's start,
    /// so endpoint A's second failure lands on the third call, not the
    /// second.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failing_endpoint_opens_its_cooldown_after_the_threshold() {
        // Endpoint A always refuses (an RPC-level error is a failure like a
        // transport one); endpoint B answers.
        let (url_a, hits_a) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32000, "message": "boom"}
        }))
        .await;
        let (url_b, _hits_b) = stub_rpc(signatures_result(&["sigB"])).await;
        let urls = vec![url_a, url_b];
        let client = client(&urls, None);
        // Call 1 starts on A (cursor 0): A fails once, B answers. One failure
        // is below the threshold — no cooldown yet.
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sigB".to_owned()]);
        assert!(!client.endpoints[0].cooling_down());
        // Call 2 starts on B (cursor 1) and succeeds without asking A.
        let _ = client.find_reference("marker").await;
        // Call 3 starts on A again: the second consecutive failure crosses
        // the threshold and the cooldown opens.
        let _ = client.find_reference("marker").await;
        assert!(client.endpoints[0].cooling_down());
        assert_eq!(
            hits_a.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "endpoint A failed exactly the threshold times"
        );
        // Calls 4 and 5: B answers; call 5's walk starts on A's slot but the
        // cooldown skips it, so A is never asked again inside its window.
        let _ = client.find_reference("marker").await;
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sigB".to_owned()]);
        assert_eq!(
            hits_a.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the cooling-down endpoint is not asked again"
        );
    }

    /// An RPC-level `error` on one endpoint fails over exactly like a
    /// transport failure — the walk lands on the next endpoint's answer.
    #[tokio::test(flavor = "current_thread")]
    async fn an_rpc_level_refusal_fails_over_to_the_next_endpoint() {
        let (url_bad, _bad) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1, "error": {"code": -1, "message": "no"}
        }))
        .await;
        let (url_good, _good) = stub_rpc(signatures_result(&["sigGood"])).await;
        let client = client(&[url_bad, url_good], None);
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sigGood".to_owned()]);
    }

    /// `OXSUM_SOLANA_RPC_AUTH_HEADER` rides every endpoint's calls — the
    /// verbatim `Name: value` the provider documented.
    #[tokio::test(flavor = "current_thread")]
    async fn a_configured_auth_header_is_sent_to_every_endpoint() {
        use axum::http::HeaderMap;
        use axum::{Json, Router};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(HeaderMap::new()));
        let seen2 = seen.clone();
        let app = Router::new().route(
            "/",
            axum::routing::post(
                move |headers: HeaderMap, Json(_b): Json<serde_json::Value>| {
                    *seen2.lock().unwrap() = headers.clone();
                    std::future::ready(Json(signatures_result(&["sig1"])))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://127.0.0.1:{port}");
        let client = client(
            std::slice::from_ref(&url),
            Some("Authorization: Bearer test-secret"),
        );
        let _ = client.find_reference("marker").await.unwrap();
        let headers = seen.lock().unwrap();
        assert_eq!(headers["authorization"], "Bearer test-secret");
    }

    /// Every endpoint inside its cooldown still answers rather than refusing
    /// the call outright — the shared-cooldown fallback walks the whole pool
    /// once, so a transient window never kills a call outright.
    #[tokio::test(flavor = "current_thread")]
    async fn a_shared_cooldown_window_still_answers_via_the_fallback() {
        let (url_a, _a) = stub_rpc(signatures_result(&["sigA"])).await;
        let (url_b, _b) = stub_rpc(signatures_result(&["sigB"])).await;
        let client = client(&[url_a, url_b], None);
        for endpoint in client.endpoints.iter() {
            endpoint
                .consecutive_failures
                .store(ENDPOINT_FAILURE_THRESHOLD, Ordering::Relaxed);
            *endpoint.unhealthy_until.lock().unwrap() = Some(Instant::now() + ENDPOINT_COOLDOWN);
        }
        let found = client.find_reference("marker").await.unwrap();
        // The fallback tried the whole pool — one of them answered.
        assert!(found == vec!["sigA".to_owned()] || found == vec!["sigB".to_owned()]);
    }

    /// A single-endpoint pool never skips its only endpoint — the cooldown
    /// records the failure streak but the call still runs (there is nowhere
    /// else to send it).
    #[tokio::test(flavor = "current_thread")]
    async fn a_single_endpoint_pool_never_skips_its_only_endpoint() {
        let (url, hits) = stub_rpc(signatures_result(&["sig"])).await;
        let client = client(std::slice::from_ref(&url), None);
        client.endpoints[0]
            .consecutive_failures
            .store(ENDPOINT_FAILURE_THRESHOLD, Ordering::Relaxed);
        *client.endpoints[0].unhealthy_until.lock().unwrap() =
            Some(Instant::now() + ENDPOINT_COOLDOWN);
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sig".to_owned()]);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The mint audit's reads fail over the same way the verification calls
    /// do — `mint_decimals` and `mint_owner` run through the same `call`
    /// pool, so a dead endpoint never stalls the boot on the per-call
    /// timeout while a healthy sibling sits idle.
    #[tokio::test(flavor = "current_thread")]
    async fn the_mint_audit_fails_over_to_the_next_endpoint() {
        let (url_bad, hits_bad) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1, "error": {"code": -1, "message": "no"}
        }))
        .await;
        // A `getAccountInfo` answer: value.owner is the classic SPL Token
        // program; value.data[0] is a base64 classic SPL mint layout — 82
        // bytes, isInitialized (byte 45) set — whose byte 44 (decimals) is 6.
        use base64::Engine as _;
        let mint_b64 = base64::engine::general_purpose::STANDARD.encode(classic_mint_layout(6));
        let (url_good, _good) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"value": {"owner": SPL_TOKEN_PROGRAM_ID, "data": [mint_b64, "base64"]}}
        }))
        .await;
        let client = client(&[url_bad, url_good], None);
        let decimals = client.mint_decimals("someMint").await.unwrap();
        assert_eq!(decimals, 6);
        let owner = client.mint_owner("someMint").await.unwrap();
        assert_eq!(owner, SPL_TOKEN_PROGRAM_ID);
        // The first audit read started on the bad endpoint and failed over;
        // the round-robin cursor started the second on the good one, so the
        // bad endpoint saw exactly one hit — the failover worked.
        assert_eq!(
            hits_bad.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the bad endpoint was tried once then failed over"
        );
    }

    /// A well-formed classic SPL mint account body: exactly
    /// `SPL_MINT_DATA_LEN` bytes with `isInitialized` (byte 45) set and
    /// `decimals` (byte 44) filled — the shape `mint_decimals_of` accepts.
    fn classic_mint_layout(decimals: u8) -> Vec<u8> {
        let mut layout = vec![0u8; SPL_MINT_DATA_LEN];
        layout[44] = decimals;
        layout[SPL_MINT_IS_INITIALIZED_OFFSET] = 1;
        layout
    }

    /// Issue #1182: the mint layout is validated before byte 44 is ever read
    /// — a truncated account, a longer (extension-padded) blob, or an
    /// uninitialized mint is `Err`, never a decimals figure off data that is
    /// not a classic mint.
    #[test]
    fn mint_decimals_of_validates_the_classic_layout() {
        // The well-formed layout yields its decimals byte.
        assert_eq!(mint_decimals_of(&classic_mint_layout(6)).unwrap(), 6);
        assert_eq!(mint_decimals_of(&classic_mint_layout(9)).unwrap(), 9);

        // Too short — the old `bytes.get(44)` arm would still have answered
        // on a 68-byte stub; now the length must be exact.
        let short = classic_mint_layout(6)[..68].to_vec();
        let err = mint_decimals_of(&short).unwrap_err();
        assert!(err.0.contains("68 bytes"), "{err:?}");

        // Too long — an extension-carrying blob is not the classic layout.
        let mut long = classic_mint_layout(6);
        long.extend_from_slice(&[0u8; 100]);
        assert!(mint_decimals_of(&long).is_err());

        // Exactly the right length but `isInitialized` clear — a mint that
        // was allocated but never initialized must not price a deposit.
        let mut uninitialized = classic_mint_layout(6);
        uninitialized[SPL_MINT_IS_INITIALIZED_OFFSET] = 0;
        let err = mint_decimals_of(&uninitialized).unwrap_err();
        assert!(err.0.contains("not initialized"), "{err:?}");

        // Empty is the degenerate truncated case.
        assert!(mint_decimals_of(&[]).is_err());
    }

    /// `mint_decimals` end-to-end over a stub RPC: an account whose data is
    /// shorter than the classic layout is `Err` — the on-chain read surfaces
    /// the same refusal the layout parser does, rather than a silently
    /// misread byte 44.
    #[tokio::test(flavor = "current_thread")]
    async fn mint_decimals_refuses_a_truncated_account() {
        use base64::Engine as _;
        let truncated = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 68]);
        let (url, _hits) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"value": {"owner": SPL_TOKEN_PROGRAM_ID, "data": [truncated, "base64"]}}
        }))
        .await;
        let client = client(&[url], None);
        let err = client.mint_decimals("someMint").await.unwrap_err();
        assert!(err.0.contains("68 bytes"), "{err:?}");
    }

    /// The failover's metric shape, read back off a local Prometheus
    /// recorder: `solana_rpc_errors_total{call,endpoint}` counts each failed
    /// attempt against the endpoint's host label, `solana_rpc_failovers_
    /// total{from,to}` the hop the call took, and `solana_rpc_endpoint_
    /// healthy{endpoint}` the cooldown's 0. Every label is a host — never the
    /// API-key-carrying URL.
    #[tokio::test(flavor = "current_thread")]
    async fn failover_metrics_label_hosts_never_keys() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);
        // Endpoint A refuses on an RPC-level error; its URL carries an API
        // key the labels must never spell. Endpoint B answers.
        let (url_a, _a) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32000, "message": "boom"}
        }))
        .await;
        let url_a = format!("{url_a}/?api-key=TOPSECRET");
        let (url_b, _b) = stub_rpc(signatures_result(&["sigB"])).await;
        let client = client(&[url_a, url_b], None);
        let found = client.find_reference("marker").await.unwrap();
        assert_eq!(found, vec!["sigB".to_owned()]);
        let rendered = handle.render();
        // The error counted against endpoint A's host — key-free — and the
        // failover hop A→B was counted once.
        assert!(
            rendered
                .lines()
                .any(|line| line.starts_with("solana_rpc_errors_total")
                    && line.contains("call=\"find_reference\"")
                    && line.contains("endpoint=\"127.0.0.1:")),
            "the per-endpoint error counted:\n{rendered}"
        );
        assert!(
            rendered
                .lines()
                .any(|line| line.starts_with("solana_rpc_failovers_total")
                    && line.contains("from=\"127.0.0.1:")
                    && line.contains("to=\"127.0.0.1:")),
            "the failover hop was counted:\n{rendered}"
        );
        assert!(
            rendered
                .lines()
                .any(|line| line.starts_with("solana_rpc_endpoint_healthy")
                    && line.contains("endpoint=\"127.0.0.1:")),
            "the endpoint health gauge moved:\n{rendered}"
        );
        // The API key appears nowhere in the exposition document.
        assert!(!rendered.contains("TOPSECRET"), "{rendered}");
        drop(_guard);
    }

    /// A `getTransaction`-shaped answer carrying one paying native-SOL
    /// transfer — the body `validate_transfer` and its cross-confirmation
    /// read. `post_lamports` is what the deposit address gained.
    fn transaction_result(pre_lamports: i64, post_lamports: i64, recipient: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": sol_tx(pre_lamports, post_lamports, recipient)
        })
    }

    /// The demand the cross-confirmation tests place on the chain: `payTo`
    /// (the test config's deposit address), 5000 lamports, native SOL, the
    /// marker `sol_tx` embeds as a read-only non-signer.
    fn expected_transfer() -> ExpectedTransfer {
        ExpectedTransfer {
            recipient: "payTo".to_owned(),
            amount: 5_000,
            spl_token_mint: None,
            reference: "REFERENCE_MARKER".to_owned(),
        }
    }

    /// Issue #1183: two endpoints agreeing on the same transaction is the
    /// credit-grade answer — `validate_transfer` refetches the signature on a
    /// second endpoint and both report the same delivered figure.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_transfer_cross_confirms_on_a_second_endpoint() {
        let (url_a, hits_a) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let (url_b, hits_b) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let client = client(&[url_a, url_b], None);
        let validated = client
            .validate_transfer("sig1", &expected_transfer())
            .await
            .unwrap();
        assert_eq!(validated.map(|v| v.delivered), Some(5_000));
        // The primary read and the second opinion each landed once — one
        // endpoint served `getTransaction`, the other confirmed it.
        assert_eq!(hits_a.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A second endpoint that has not seen the transaction cannot confirm it:
    /// the answer is not a credit basis and the deposit stays pending — the
    /// `Err` the credit path defers on, never an `Ok(None)` "not the payment"
    /// that would feed the expiry sweep (issue #1174).
    #[tokio::test(flavor = "current_thread")]
    async fn an_endpoint_that_missed_the_tx_keeps_the_deposit_pending() {
        let (url_a, _a) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let (url_b, _b) = stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result": null})).await;
        let client = client(&[url_a, url_b], None);
        let err = client
            .validate_transfer("sig1", &expected_transfer())
            .await
            .unwrap_err();
        assert!(
            err.0.contains("cross-confirmation"),
            "no single endpoint's word credits — the refusal defers, it does not expire: {err:?}"
        );
    }

    /// Two endpoints that disagree on the delivered amount cannot both be
    /// right: the deposit stays pending rather than crediting either figure —
    /// `Err`, not `Ok(None)`, so the walk defers instead of calling the intent
    /// unpaid (issue #1174).
    #[tokio::test(flavor = "current_thread")]
    async fn disagreeing_endpoints_keep_the_deposit_pending() {
        let (url_a, _a) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let (url_b, _b) = stub_rpc(transaction_result(0, 4_000, "payTo")).await;
        let client = client(&[url_a, url_b], None);
        let err = client
            .validate_transfer("sig1", &expected_transfer())
            .await
            .unwrap_err();
        assert!(
            err.0.contains("cross-confirmation"),
            "a disagreement is not a credit basis — nor an unpaid verdict: {err:?}"
        );
    }

    /// A second endpoint that cannot answer at all is not a confirmation: the
    /// deposit stays pending until a sibling endpoint can vouch — `Err`, so
    /// the credit path defers rather than expiring the intent (issue #1174).
    #[tokio::test(flavor = "current_thread")]
    async fn an_unreachable_second_endpoint_keeps_the_deposit_pending() {
        let (url_a, _a) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let (url_b, _b) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1, "error": {"code": -1, "message": "no"}
        }))
        .await;
        let client = client(&[url_a, url_b], None);
        let err = client
            .validate_transfer("sig1", &expected_transfer())
            .await
            .unwrap_err();
        assert!(
            err.0.contains("cross-confirmation"),
            "no second opinion, no credit — nor an unpaid verdict: {err:?}"
        );
    }

    /// The confirmation is an extra read, not a replacement for failover: a
    /// dead first endpoint fails the primary read over to the second, and a
    /// third endpoint supplies the second opinion — validation still lands.
    #[tokio::test(flavor = "current_thread")]
    async fn cross_confirmation_survives_a_primary_failover() {
        let (url_bad, _bad) = stub_rpc(json!({
            "jsonrpc": "2.0", "id": 1, "error": {"code": -1, "message": "no"}
        }))
        .await;
        let (url_b, _b) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let (url_c, _c) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let client = client(&[url_bad, url_b, url_c], None);
        // The primary read fails over bad → B; the confirmation walks past B
        // (the serving endpoint cannot second-guess itself) and past the now
        // cooling bad endpoint to C's agreeing answer.
        let validated = client
            .validate_transfer("sig1", &expected_transfer())
            .await
            .unwrap();
        assert_eq!(validated.map(|v| v.delivered), Some(5_000));
    }

    /// A single-endpoint pool has no second opinion to ask: the first answer
    /// stands alone, exactly the pre-#1183 behavior a one-provider deployment
    /// keeps.
    #[tokio::test(flavor = "current_thread")]
    async fn a_single_endpoint_pool_validates_without_a_second_opinion() {
        let (url, hits) = stub_rpc(transaction_result(0, 5_000, "payTo")).await;
        let client = client(std::slice::from_ref(&url), None);
        let validated = client
            .validate_transfer("sig1", &expected_transfer())
            .await
            .unwrap();
        assert_eq!(validated.map(|v| v.delivered), Some(5_000));
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the single endpoint answered one read — no confirmation round-trip exists"
        );
    }

    /// The refund demand the payout tests place on the chain: `payTo` is the
    /// vault (`expected.source`), `refundAddr` the customer's frozen refund
    /// address.
    fn expected_payout(mint: Option<&str>) -> ExpectedPayout {
        ExpectedPayout {
            recipient: "refundAddr".to_owned(),
            spl_token_mint: mint.map(str::to_owned),
            source: "payTo".to_owned(),
        }
    }

    /// A Squads-shaped native-SOL payout (issue #1209): the multisig vault
    /// `payTo` is a *non-signer* account whose lamports the CPI debited; the
    /// only signer is the Squads program's fee payer. `accountKeys[1]` is the
    /// vault, `[2]` the customer's refund address.
    fn squads_sol_payout_tx(
        vault_pre: i64,
        vault_post: i64,
        refund_post: i64,
        err: Value,
    ) -> Value {
        json!({
            "meta": {
                "err": err,
                "preBalances": [5_000_000_000_i64, vault_pre, 0],
                "postBalances": [4_999_995_000_i64, vault_post, refund_post],
                "preTokenBalances": [],
                "postTokenBalances": []
            },
            "blockTime": 1_700_000_000_i64,
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "SQUADS_FEE_PAYER", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "payTo", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": "refundAddr", "signer": false, "writable": true, "source": "transaction"}
                    ]
                }
            }
        })
    }

    /// A third-party-paid SOL transfer the vault never touched: the signer
    /// `STRANGER` paid `refundAddr` direct — `payTo` sits in the keys (it is
    /// the rail's known vault) but its balance did not fall.
    fn third_party_sol_tx(refund_post: i64) -> Value {
        json!({
            "meta": {
                "err": null,
                "preBalances": [5_000_000_000_i64, 9_000_000_000_i64, 0],
                "postBalances": [4_990_000_000_i64, 9_000_000_000_i64, refund_post],
                "preTokenBalances": [],
                "postTokenBalances": []
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "STRANGER", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "payTo", "signer": false, "writable": false, "source": "transaction"},
                        {"pubkey": "refundAddr", "signer": false, "writable": true, "source": "transaction"}
                    ]
                }
            }
        })
    }

    /// A Squads-shaped SPL payout (issue #1209): the vault owns the sending
    /// token account — `accountKeys[1]` is the vault-owned ATA the transfer
    /// debited, `owner: "payTo"` on its balance rows — and the recipient's ATA
    /// sits at index 4 owned by `refundAddr`.
    fn squads_spl_payout_tx(
        vault_pre_amount: &str,
        vault_post_amount: Option<&str>,
        recipient_post_amount: &str,
        mint: &str,
        err: Value,
    ) -> Value {
        let mut post_rows = vec![json!({
            "accountIndex": 4, "mint": mint, "owner": "refundAddr",
            "uiTokenAmount": {"amount": recipient_post_amount, "decimals": 6, "uiAmountString": "0"}
        })];
        if let Some(amount) = vault_post_amount {
            post_rows.insert(
                0,
                json!({
                    "accountIndex": 1, "mint": mint, "owner": "payTo",
                    "uiTokenAmount": {"amount": amount, "decimals": 6, "uiAmountString": "0"}
                }),
            );
        }
        json!({
            "meta": {
                "err": err,
                "preBalances": [5_000_000_000_i64],
                "postBalances": [4_999_995_000_i64],
                "preTokenBalances": [{
                    "accountIndex": 1, "mint": mint, "owner": "payTo",
                    "uiTokenAmount": {"amount": vault_pre_amount, "decimals": 6, "uiAmountString": "0"}
                }],
                "postTokenBalances": post_rows
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "SQUADS_FEE_PAYER", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "VAULT_ATA", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": mint, "signer": false, "writable": false, "source": "transaction"},
                        {"pubkey": "TOKEN_PROGRAM", "signer": false, "writable": false, "source": "transaction"},
                        {"pubkey": "REFUND_ATA", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": "payTo", "signer": false, "writable": false, "source": "transaction"}
                    ]
                }
            }
        })
    }

    /// Issue #1209 (a): a Squads vault payout must validate — the vault PDA
    /// never signs, so the old signer-set proof refused the very refund the
    /// rail is built to take. The proof is the vault's balance decrease.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_payout_accepts_a_squads_vault_sol_refund() {
        // Vault 9 SOL → 8.995 SOL (paid 5_000_000 lamports), refund address
        // 0 → 5_000_000. The only signer is the fee payer.
        let (url, _hits) = stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result":
                squads_sol_payout_tx(9_000_000_000, 8_995_000_000, 5_000_000, json!(null))}))
        .await;
        let client = client(&[url], None);
        let validated = client
            .validate_payout("refundSig", &expected_payout(None))
            .await
            .unwrap();
        let validated =
            validated.expect("a vault-decrease payout is proof even with no vault signature");
        assert_eq!(validated.delivered, 5_000_000);
        // `payers` stays reporting-only: the debited signer is the fee payer.
        assert_eq!(validated.payers, vec!["SQUADS_FEE_PAYER".to_owned()]);
    }

    /// Issue #1209 (b): a third party paying the refund address direct is no
    /// proof — the vault the row believes it closed never lost a lamport,
    /// even though the signed payer is somebody.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_payout_refuses_a_third_party_sol_payment() {
        let (url, _hits) =
            stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result": third_party_sol_tx(5_000_000)}))
                .await;
        let client = client(&[url], None);
        let validated = client
            .validate_payout("refundSig", &expected_payout(None))
            .await
            .unwrap();
        assert!(
            validated.is_none(),
            "a stranger-paid refund never emptied the vault: {validated:?}"
        );
    }

    /// Issue #1209 (c): the vault lost lamports but fewer than `delivered` —
    /// a partial drain is not the payout the refund amount froze.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_payout_refuses_a_vault_decrease_below_delivered() {
        // Vault dropped only 4_000_000 while 5_000_000 reached the refund
        // address — someone else covered the rest.
        let (url, _hits) = stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result":
                squads_sol_payout_tx(9_000_000_000, 8_996_000_000, 5_000_000, json!(null))}))
        .await;
        let client = client(&[url], None);
        let validated = client
            .validate_payout("refundSig", &expected_payout(None))
            .await
            .unwrap();
        assert!(
            validated.is_none(),
            "a source decrease below delivered is no proof: {validated:?}"
        );
    }

    /// Issue #1209 (d): a Squads SPL refund — the vault's own token account
    /// drained by the payout — must validate. The owner field, not a
    /// signature, attributes the spend.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_payout_accepts_a_squads_vault_spl_refund() {
        let (url, _hits) =
            stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result":
                squads_spl_payout_tx("9000000", Some("4000000"), "5000000", "usdcMint", json!(null))}))
                .await;
        let client = client(&[url], None);
        let validated = client
            .validate_payout("refundSig", &expected_payout(Some("usdcMint")))
            .await
            .unwrap();
        let validated = validated.expect("a vault-ATA decrease is proof for an SPL payout");
        assert_eq!(validated.delivered, 5_000_000);
        // Reporting payers: the debited token account's owner — the vault.
        assert_eq!(validated.payers, vec!["payTo".to_owned()]);
    }

    /// Issue #1209 (e): a failed transaction, a payout to a different
    /// recipient, and a payout on the wrong mint each fail — the strictness
    /// the recipient/mint checks carry never changed.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_payout_refuses_failed_wrong_recipient_and_wrong_mint() {
        // Failed transaction — `meta.err` set.
        let (url_err, _e) = stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result":
                squads_sol_payout_tx(9_000_000_000, 8_995_000_000, 5_000_000,
                    json!({"InstructionError": [0, "Custom"]}))}))
        .await;
        let err_client = client(&[url_err], None);
        assert!(
            err_client
                .validate_payout("refundSig", &expected_payout(None))
                .await
                .unwrap()
                .is_none(),
            "a failed transaction moved nothing"
        );
        // Right vault, right amount, wrong recipient — `lamport_delta` finds
        // no `refundAddr` key.
        let wrong_recipient = json!({
            "meta": {
                "err": null,
                "preBalances": [5_000_000_000_i64, 9_000_000_000_i64, 0],
                "postBalances": [4_999_995_000_i64, 8_995_000_000_i64, 5_000_000],
                "preTokenBalances": [],
                "postTokenBalances": []
            },
            "transaction": {
                "message": {
                    "accountKeys": [
                        {"pubkey": "SQUADS_FEE_PAYER", "signer": true, "writable": true, "source": "transaction"},
                        {"pubkey": "payTo", "signer": false, "writable": true, "source": "transaction"},
                        {"pubkey": "someoneElse", "signer": false, "writable": true, "source": "transaction"}
                    ]
                }
            }
        });
        let (url_wrong, _w) =
            stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result": wrong_recipient})).await;
        let wrong_client = client(&[url_wrong], None);
        assert!(
            wrong_client
                .validate_payout("refundSig", &expected_payout(None))
                .await
                .unwrap()
                .is_none(),
            "a payout to a different address is no proof for this refund"
        );
        // Wrong mint: the SPL rows ride `otherMint`, not `usdcMint`.
        let (url_mint, _m) =
            stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result":
                squads_spl_payout_tx("9000000", Some("4000000"), "5000000", "otherMint", json!(null))}))
                .await;
        let mint_client = client(&[url_mint], None);
        assert!(
            mint_client
                .validate_payout("refundSig", &expected_payout(Some("usdcMint")))
                .await
                .unwrap()
                .is_none(),
            "a payout on the wrong mint is no proof"
        );
    }

    /// The vault paid out but the same transaction never delivered to the
    /// refund address — the `delivered` side still guards.
    #[tokio::test(flavor = "current_thread")]
    async fn validate_payout_refuses_a_vault_spend_that_paid_elsewhere() {
        // Vault fell 5_000_000 but `refundAddr` gained 0 — the debit went
        // somewhere else (fee, another transfer in the same tx).
        let (url, _hits) = stub_rpc(json!({"jsonrpc": "2.0", "id": 1, "result":
                squads_sol_payout_tx(9_000_000_000, 8_995_000_000, 0, json!(null))}))
        .await;
        let client = client(&[url], None);
        let validated = client
            .validate_payout("refundSig", &expected_payout(None))
            .await
            .unwrap();
        let validated =
            validated.expect("the source check judges the vault's decrease, not the delivery");
        // `delivered` is 0 — the reported figure the core compares to the
        // refund amount; the source check already passed on the vault drain.
        assert_eq!(validated.delivered, 0);
    }

    /// `source_lamports_decreased` needs `pre - post >= delivered` on the
    /// vault's own accountKeys slot, signer flag ignored.
    #[test]
    fn source_lamports_decreased_judges_the_vaults_own_slot() {
        let tx = squads_sol_payout_tx(9_000_000_000, 8_995_000_000, 5_000_000, json!(null));
        let keys = tx["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap();
        let pre = tx["meta"]["preBalances"].as_array();
        let post = tx["meta"]["postBalances"].as_array();
        assert!(source_lamports_decreased(
            keys, pre, post, "payTo", 5_000_000
        ));
        assert!(source_lamports_decreased(keys, pre, post, "payTo", 1));
        assert!(!source_lamports_decreased(
            keys, pre, post, "payTo", 5_000_001
        ));
        // A source that is not in the keys cannot have paid.
        assert!(!source_lamports_decreased(
            keys,
            pre,
            post,
            "absentVault",
            1
        ));
        // Missing balance arrays are no proof.
        assert!(!source_lamports_decreased(keys, None, post, "payTo", 1));
    }

    /// `source_token_decreased` sums decreases across token accounts the
    /// vault owns on the locked mint — several partial drains can meet
    /// `delivered` together, a third party's decrease never counts.
    #[test]
    fn source_token_decreased_counts_only_source_owned_rows() {
        let mint = "usdcMint";
        let keys = json!([
            {"pubkey": "FEE_PAYER", "signer": true, "writable": true, "source": "transaction"},
            {"pubkey": "VAULT_ATA_A", "signer": false, "writable": true, "source": "transaction"},
            {"pubkey": "STRANGER_ATA", "signer": false, "writable": true, "source": "transaction"}
        ]);
        let keys = keys.as_array().unwrap();
        let pre = json!([
            {"accountIndex": 1, "mint": mint, "owner": "payTo",
             "uiTokenAmount": {"amount": "3000", "decimals": 6, "uiAmountString": "0"}},
            {"accountIndex": 2, "mint": mint, "owner": "stranger",
             "uiTokenAmount": {"amount": "9000", "decimals": 6, "uiAmountString": "0"}}
        ]);
        let post = json!([
            {"accountIndex": 1, "mint": mint, "owner": "payTo",
             "uiTokenAmount": {"amount": "0", "decimals": 6, "uiAmountString": "0"}},
            {"accountIndex": 2, "mint": mint, "owner": "stranger",
             "uiTokenAmount": {"amount": "0", "decimals": 6, "uiAmountString": "0"}}
        ]);
        let pre = pre.as_array();
        let post = post.as_array();
        // The vault's own ATA fell 3000 — enough for 3000, not for 3001 even
        // though the stranger's ATA fell 9000 in the same transaction.
        assert!(source_token_decreased(
            pre, post, keys, "payTo", mint, 3_000
        ));
        assert!(!source_token_decreased(
            pre, post, keys, "payTo", mint, 3_001
        ));
        // No pre rows at all is no proof.
        assert!(!source_token_decreased(None, post, keys, "payTo", mint, 1));
    }
}

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
//! What is *not* verified here, on the record: `confirmed` is the default
//! commitment because devnet top-ups are low-value and latency matters more
//! than the residual reorg window; `finalized` is one config flag away. No
//! watcher holds a subscription open — the periodic job polls.

use serde_json::{Value, json};

/// Standard base64 decode for the `getAccountInfo` `data` field — a thin
/// wrapper so `mint_decimals` names its error instead of unwrapping a
/// library type.
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
/// what its RPC calls need — the endpoint, commitment, the `payTo` reference
/// and `mints`, the whitelist the boot audits on-chain. The per-asset
/// figures the intents lock (`asset_decimals`, `rate_micro`, the open
/// bounds) live on `oxsum_core::SolanaTerms`, which the server hands the
/// verifier beside this client.
#[derive(Clone)]
pub struct SolanaConfig {
    /// The JSON-RPC endpoint, `OXSUM_SOLANA_RPC_URL` — devnet
    /// (`https://api.devnet.solana.com`) in the first cut; the variable is the
    /// seam a mainnet deployment flips later.
    pub rpc_url: String,
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
    /// `confirmed` (default) or `finalized` (`OXSUM_SOLANA_COMMITMENT`).
    pub commitment: String,
}

impl std::fmt::Debug for SolanaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaConfig")
            .field("rpc_url", &self.rpc_url)
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
}

/// The narrow JSON-RPC client the rail talks to, bound to the deployment's
/// [`SolanaConfig`]: `base_url` is the RPC endpoint, and `config` carries the
/// `pay_to`/`mints`/`commitment` terms the two Solana Pay calls and the
/// boot's mint audit are asked under. Binding the config in (rather than
/// passing it per call) is what lets the client be a `ChainLookup` — the
/// verifier's seam hands it only the intent's `reference`/`amount`.
#[derive(Clone)]
pub struct SolanaRpcClient {
    base_url: String,
    http: reqwest::Client,
    config: SolanaConfig,
}

impl std::fmt::Debug for SolanaRpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaRpcClient")
            .field("base_url", &self.base_url)
            .field("config", &self.config)
            .finish()
    }
}

impl SolanaRpcClient {
    /// Builds the client around the RPC endpoint, the shared HTTP pool and
    /// the deployment's terms.
    #[must_use]
    pub fn new(base_url: String, http: reqwest::Client, config: SolanaConfig) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http,
            config,
        }
    }

    /// The deployment's chain-side config — endpoint, `payTo`, the mint
    /// whitelist and the commitment the calls ask for.
    #[must_use]
    pub fn config(&self) -> &SolanaConfig {
        &self.config
    }

    /// Reads a mint's decimals from the chain (issues #1129, #1140).
    /// `getAccountInfo(mint)` with base64 encoding answers the raw SPL mint
    /// layout, whose `decimals` byte sits at offset 44 (after mintAuthority
    /// option+key, supply and the initialized flag). The boot calls this once
    /// per whitelisted mint so the mint's own figure — never an operator's
    /// declaration — sets `asset_decimals`: a 6-decimal USDC mint read as 9
    /// decimals would overcharge by 1000×.
    ///
    /// `Err` when the RPC fails or the mint account is missing/unparseable.
    pub async fn mint_decimals(&self, mint: &str) -> Result<u32, SolanaError> {
        let result = self
            .call("getAccountInfo", json!([mint, {"encoding": "base64"}]))
            .await?;
        let data_b64 = result["value"]["data"][0]
            .as_str()
            .ok_or_else(|| SolanaError("getAccountInfo returned no mint data".into()))?;
        let bytes = base64_decode(data_b64)
            .map_err(|e| SolanaError(format!("mint data is not base64: {e}")))?;
        // SPL mint layout: mintAuthority (36) + supply (8) + decimals (1) +
        // isInitialized (1) + freezeAuthority (36) — decimals is byte 44.
        let decimals = bytes
            .get(44)
            .copied()
            .ok_or_else(|| SolanaError("mint account data is too short".into()))?;
        Ok(u32::from(decimals))
    }

    /// Reads the program that owns a mint account — the Token or Token-2022
    /// program id `getAccountInfo` reports as `owner`. The boot calls this
    /// once per whitelisted mint and refuses Token-2022: extensions the rail
    /// does not model (transfer fees, hooks, required memos) could misprice
    /// or mis-credit a deposit (issue #1140).
    ///
    /// `Err` when the RPC fails or the account is missing/unparseable.
    pub async fn mint_owner(&self, mint: &str) -> Result<String, SolanaError> {
        let result = self
            .call("getAccountInfo", json!([mint, {"encoding": "base64"}]))
            .await?;
        result["value"]["owner"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| SolanaError("getAccountInfo returned no owner".into()))
    }

    /// One JSON-RPC call. Returns the `result` field; a transport failure, a
    /// non-200 or an RPC-level `error` object are all `Err`.
    async fn call(&self, method: &str, params: Value) -> Result<Value, SolanaError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response = self
            .http
            .post(&self.base_url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| SolanaError(format!("{method} request failed: {e}")))?;
        let value: Value = response
            .json()
            .await
            .map_err(|e| SolanaError(format!("{method} response unreadable: {e}")))?;
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

    /// `getTransaction(signature, jsonParsed)` — the `validateTransfer` half:
    /// prove the located transaction pays this intent's terms. `Ok(None)`
    /// means the transaction exists but does not satisfy the transfer — wrong
    /// recipient, underpayment, wrong mint, missing reference — which the rail
    /// treats as "not the payment" exactly like Solana Pay's validator does.
    ///
    /// The checks are deliberately strict in the customer's favor: `err` must
    /// be null, the `reference` account must appear among the transaction's
    /// `accountKeys` (non-signer), and the recipient's balance delta must be
    /// at least the asked amount — overpayment is honored, underpayment is
    /// refused, never silently accepted.
    pub async fn validate_transfer(
        &self,
        signature: &str,
        expected: &ExpectedTransfer,
    ) -> Result<Option<ValidatedTransfer>, SolanaError> {
        let result = self
            .call(
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
        if delivered < expected.amount {
            return Ok(None);
        }
        Ok(Some(ValidatedTransfer {
            signature: signature.to_owned(),
            delivered,
            block_time: result["blockTime"].as_i64(),
        }))
    }
}

/// The `core` verifier's `ChainLookup` over this JSON-RPC client: the two
/// Solana Pay calls on the deployment's configured endpoint. `Err` from the
/// RPC surfaces as `None` at the seam — an unreachable endpoint or an RPC
/// refusal is indistinguishable from "no matching transfer yet", which is how
/// the rail treats every non-answer.
impl oxsum_core::ChainLookup for SolanaRpcClient {
    fn find_reference<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move {
            match self.find_reference(reference).await {
                Ok(signatures) => signatures,
                // A transport/RPC failure is *not* "not paid yet" — log it as
                // an error and count it, so an unreachable endpoint never
                // masquerades as an unpaid intent. The empty Vec still reads
                // "no matching transfer" upstream, but now it is a loud miss.
                Err(e) => {
                    tracing::warn!(
                        reference,
                        error = %e.0,
                        "solana find_reference RPC failed; treating as unlocated"
                    );
                    metrics::counter!("solana_rpc_errors_total", "call" => "find_reference")
                        .increment(1);
                    Vec::new()
                }
            }
        })
    }

    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a oxsum_core::ExpectedTransfer,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<oxsum_core::ValidatedTransfer>> + Send + 'a>,
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
            .inspect_err(|e| {
                // Same distinction as find_reference: a transport failure is
                // not "the transfer does not honor" — warn and count it.
                tracing::warn!(
                    signature,
                    error = %e.0,
                    "solana validate_transfer RPC failed; treating as unvalidated"
                );
                metrics::counter!("solana_rpc_errors_total", "call" => "validate_transfer")
                    .increment(1);
            })
            .ok()
            .flatten()
            .map(|v| oxsum_core::ValidatedTransfer {
                signature: v.signature,
                delivered: v.delivered,
                block_time: v.block_time,
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
}

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

/// The deployment's Solana acceptance criteria, read from the
/// `OXSUM_SOLANA_*` environment set. `Config` holds it; the periodic poll and
/// the confirm route read it to decide what an honoring transfer looks like.
#[derive(Clone)]
pub struct SolanaConfig {
    /// The JSON-RPC endpoint, `OXSUM_SOLANA_RPC_URL` — devnet
    /// (`https://api.devnet.solana.com`) in the first cut; the variable is the
    /// seam a mainnet deployment flips later.
    pub rpc_url: String,
    /// The deposit address every intent pays, `OXSUM_SOLANA_PAY_TO` (base58
    /// public key). For an SPL top-up this is the owner whose associated token
    /// account receives the mint.
    pub pay_to: String,
    /// The SPL token mint a top-up must pay, `OXSUM_SOLANA_SPL_TOKEN_MINT`.
    /// `None` — the default — expects native SOL, measured in lamports.
    pub spl_token_mint: Option<String>,
    /// The commitment `find_reference` / `validate_transfer` ask the RPC for:
    /// `confirmed` (default) or `finalized` (`OXSUM_SOLANA_COMMITMENT`).
    pub commitment: String,
    /// The locked rate in minor-credits per whole SOL that intents quote and
    /// credits settle at, `OXSUM_SOLANA_RATE_MICRO`. Devnet's static stand-in
    /// for a price oracle: 1 SOL buys `rate_micro` minor-credits. A mainnet
    /// rail replaces this with an oracle lookup recorded per intent.
    pub rate_micro: i64,
    /// How long a `pending` intent stays payable before the poll expires it,
    /// `OXSUM_SOLANA_INTENT_TTL_SECS` (default 600 — Solana Pay's own
    /// unbounded-poll guard).
    pub intent_ttl_secs: i64,
}

impl std::fmt::Debug for SolanaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaConfig")
            .field("rpc_url", &self.rpc_url)
            .field("pay_to", &self.pay_to)
            .field("spl_token_mint", &self.spl_token_mint)
            .field("commitment", &self.commitment)
            .field("rate_micro", &self.rate_micro)
            .field("intent_ttl_secs", &self.intent_ttl_secs)
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
}

/// The narrow JSON-RPC client the rail talks to, bound to the deployment's
/// [`SolanaConfig`]: `base_url` is the RPC endpoint, and `config` carries the
/// `pay_to`/`spl_token_mint`/`commitment`/`rate_micro` terms the two Solana
/// Pay calls are asked under. Binding the config in (rather than passing it
/// per call) is what lets the client be a `ChainLookup` — the verifier's seam
/// hands it only the intent's `reference`/`amount`.
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

    /// The deployment's terms — `open_solana_deposit` reads `pay_to`,
    /// `spl_token_mint`, `rate_micro` and `intent_ttl_secs` off it to write
    /// the intent's `meta`.
    #[must_use]
    pub fn config(&self) -> &SolanaConfig {
        &self.config
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

    /// `getSignaturesForAddress(reference)` — the `findReference` half: the
    /// newest confirmed transaction touching the per-intent marker account.
    /// `Ok(None)` means nothing has landed for this intent yet — the normal
    /// "not paid yet" answer, not a failure.
    pub async fn find_reference(&self, reference: &str) -> Result<Option<String>, SolanaError> {
        let result = self
            .call(
                "getSignaturesForAddress",
                json!([
                    reference,
                    {"limit": 1, "commitment": self.config.commitment},
                ]),
            )
            .await?;
        let signature = result
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|row| row["signature"].as_str())
            .map(str::to_owned);
        Ok(signature)
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
        // The `reference` marker must be present as an account key. With
        // `jsonParsed`, accountKeys entries are `{pubkey, signer, writable,
        // source}` objects; a bare-string form is tolerated defensively.
        let reference_present = account_keys.iter().any(|key| {
            let pubkey = key["pubkey"].as_str().or_else(|| key.as_str());
            pubkey == Some(expected.reference.as_str())
        });
        if !reference_present {
            return Ok(None);
        }
        let delivered = match expected.spl_token_mint.as_deref() {
            // Native SOL: the recipient's post-minus-pre lamport delta. The
            // `accountKeys` index of `recipient` aligns with `preBalances` /
            // `postBalances`, which only cover static (non-lookup) keys — so
            // index the recipient's position within those keys.
            None => {
                let position = account_keys.iter().position(|key| {
                    let pubkey = key["pubkey"].as_str().or_else(|| key.as_str());
                    pubkey == Some(expected.recipient.as_str())
                });
                let Some(index) = position else {
                    return Ok(None);
                };
                let pre = meta["preBalances"][index].as_i64().unwrap_or(0);
                let post = meta["postBalances"][index].as_i64().unwrap_or(0);
                post.saturating_sub(pre)
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async move { self.find_reference(reference).await.ok().flatten() })
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
            .ok()
            .flatten()
            .map(|v| oxsum_core::ValidatedTransfer {
                signature: v.signature,
                delivered: v.delivered,
            })
        })
    }
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

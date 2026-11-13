//! Third-party service integrations: the narrow clients the server talks to.
//!
//! Each module is one external service, with no dependency on the server's
//! request layer — they take a `reqwest::Client` (or build their own transport)
//! and plain parameters, so they stay usable and testable in isolation:
//!
//! - [`oauth`]: the GitHub OAuth provider legs (issue #152).
//! - [`stripe`]: the Stripe deposit rail client and webhook signature (issue #201).
//! - [`airwallex`]: the Airwallex deposit rail client and webhook signature (issue #285).
//! - [`hyperswitch`]: the Hyperswitch switch rail client and HMAC-SHA512 webhook (issue #319).
//! - [`antibot`]: the Cloudflare Turnstile anti-bot check (issue #154).
//! - [`mail`]: the SMTP mailer for the verification and password-reset flows (issue #150).
//! - [`facilitator`]: the x402-rs facilitator's verify/settle client (issue #282).
//! - [`solana`]: the Solana JSON-RPC client the deposit rail verifies transfers
//!   against (issue #1117).
//! - [`squads`]: the Squads v4 multisig vault-PDA derivation that proves the
//!   rail's `payTo` is a multisig's receiving vault, not a hot wallet
//!   (issue #1143).
//! - [`pyth`]: the Pyth Hermes price-oracle client a native-SOL deposit intent
//!   prices against at open (issue #1139).
//! - [`sanctions`]: the pre-credit payer screen the Solana rail runs — the
//!   local OFAC SDN list plus the Chainalysis free API (issue #1144).
//! - [`x402_facilitator`]: the facilitator-delegated `RailVerifier` (issue #282).
//! - [`openai_costs`]: the OpenAI Costs admin-API bill provider (issue #283).
//! - [`anthropic_costs`]: the Anthropic Usage & Cost admin-API bill provider (issue #367).
//! - [`bill_providers`]: the composite that fans a channel out to the right
//!   provider impl (issue #367).

pub mod airwallex;
pub mod anthropic_costs;
pub mod antibot;
pub mod bill_providers;
pub mod deposit_refund;
pub mod facilitator;
pub mod hyperswitch;
pub mod mail;
pub mod merchant_checkout;
pub mod merchant_events;
pub mod oauth;
pub mod oidc;
pub mod openai_costs;
pub mod provider_http;
pub mod pyth;
pub mod sanctions;
pub mod solana;
pub mod squads;
pub mod stripe;
pub mod x402_facilitator;

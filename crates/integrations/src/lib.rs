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
//! - [`x402_facilitator`]: the facilitator-delegated `RailVerifier` (issue #282).
//! - [`openai_costs`]: the OpenAI Costs admin-API bill provider (issue #283).
//! - [`anthropic_costs`]: the Anthropic Usage & Cost admin-API bill provider (issue #367).
//! - [`bill_providers`]: the composite that fans a channel out to the right
//!   provider impl (issue #367).

pub mod airwallex;
pub mod anthropic_costs;
pub mod antibot;
pub mod bill_providers;
pub mod facilitator;
pub mod hyperswitch;
pub mod mail;
pub mod oauth;
pub mod openai_costs;
pub mod stripe;
pub mod x402_facilitator;

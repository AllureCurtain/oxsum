//! Third-party service integrations: the narrow clients the server talks to.
//!
//! Each module is one external service, with no dependency on the server's
//! request layer — they take a `reqwest::Client` (or build their own transport)
//! and plain parameters, so they stay usable and testable in isolation:
//!
//! - [`oauth`]: the GitHub OAuth provider legs (issue #152).
//! - [`stripe`]: the Stripe deposit rail client and webhook signature (issue #201).
//! - [`antibot`]: the Cloudflare Turnstile anti-bot check (issue #154).
//! - [`mail`]: the SMTP mailer for the verification and password-reset flows (issue #150).

pub mod antibot;
pub mod mail;
pub mod oauth;
pub mod stripe;

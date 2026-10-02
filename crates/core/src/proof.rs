//! Bill proofs, via the shared verifier crate.
//!
//! The verification logic lives in `oxsum-verify` so the verification page can run
//! the same code in the browser: `oxsum-core` itself cannot target
//! `wasm32-unknown-unknown` (sqlx-postgres needs OS sockets). These re-exports keep
//! the established `oxsum_core::{ProofBundle, verify_bundle}` paths stable.

pub use oxsum_verify::{ProofBundle, verify_bundle};

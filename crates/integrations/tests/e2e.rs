//! Aggregate integration-test binary for oxsum-integrations (issue #1152):
//! the same one-binary shape `tests/server.rs`/`tests/core.rs` take (issue
//! #998) so each suite links the crate once. The Solana end-to-end suite is
//! the first member: it drives the real `SolanaRpcClient` against a local
//! `solana-test-validator` and the whole deposit credit path on the scratch
//! database.
//!
//! Requires DATABASE_URL for the credit path and a reachable Solana JSON-RPC
//! (`OXSUM_SOLANA_E2E_RPC`, default `http://127.0.0.1:8899`) — a local
//! `solana-test-validator`. Without either the tests skip instead of failing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "e2e/solana_e2e.rs"]
mod solana_e2e;

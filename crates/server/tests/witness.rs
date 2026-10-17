//! HTTP tests: the deployment's own tlog-witness (issue #260).
//!
//! `POST /add-checkpoint` accepts a C2SP `tlog-witness` body — a signed note
//! plus base64 consistency-proof lines — and answers with the note carrying
//! this witness's `cosignature/v1` line. The accept path is a real round-trip:
//! a `MerkleLog` grows, the log operator signs its head, posts it, and gets a
//! cosignature back that verifies under the public half of
//! `OXSUM_WITNESS_SIGNING_KEY`. The reject paths cover the refusals a witness
//! exists to make: unsigned notes, proofs that do not verify, forks, shrinkage,
//! and origins the witness was never told to trust.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use doubleentry::witness::signing::SigningKey;
use doubleentry::witness::{AddCheckpoint, KeyName, Origin, SignedTreeHead};
use doubleentry::{ConsistencyProof, Hash, MerkleLog, TreeHead};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup, WitnessServe, WitnessTrust};
use sqlx::PgPool;
use tower::ServiceExt;

/// The witness deployment's own cosigning seed — what `OXSUM_WITNESS_SIGNING_KEY`
/// would carry. The public half is derived from it for the verify side.
const SERVE_SEED: [u8; 32] = [11; 32];

/// The foreign log operator's note-signing seed — the "log's own key" a
/// `witness-checkpoints` pass signs its head under before submitting.
const LOG_SEED: [u8; 32] = [8; 32];

/// A scratch log with `n` leaves — entry `i`'s hash is `i` as little-endian
/// bytes, so a second history at the same size is a one-byte change.
fn log_of(n: u64) -> MerkleLog {
    let mut log = MerkleLog::new();
    for i in 0..n {
        let mut bytes = [0u8; 32];
        bytes
            .get_mut(..8)
            .expect("32 > 8")
            .copy_from_slice(&i.to_le_bytes());
        log.append(Hash::from_bytes(bytes));
    }
    log
}

fn origin() -> Origin {
    Origin::new("example.com/ledgers/acme").expect("valid")
}

/// The note a log operator submits: its head signed under the log's own key —
/// the shape the `witness-checkpoints` pass POSTs (issue #231).
fn signed_note(origin: &Origin, head: TreeHead, signer: &SigningKey) -> SignedTreeHead {
    let mut note = SignedTreeHead::new(origin.clone(), head);
    note.add_signature(signer.sign(&note.body()));
    note
}

/// The wire body: `AddCheckpoint` serialises the signed note then one base64
/// line per proof hash — exactly what the witness's `parse_body` reads back.
fn submission_body(
    origin: &Origin,
    head: TreeHead,
    signer: &SigningKey,
    proof: ConsistencyProof,
) -> Vec<u8> {
    AddCheckpoint::new(signed_note(origin, head, signer), proof).to_body()
}

/// A pool for requests rejected before the database is reached.
fn unused_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL")
}

/// An app without a witness configured: `/add-checkpoint` should be a 404 the
/// same way every unmounted path is.
fn offline_app() -> Router {
    oxsum_server::app(
        Db::from_pool(unused_pool()),
        Config::new(Signup::Open, None),
    )
}

/// A real deployment: migrated tables, then `prepare` — the same call `main`
/// makes, which is where `OXSUM_WITNESS_TRUST` seeds each pinned origin's
/// starting head into `witness_served`.
async fn witness_app(url: &str, trust: Vec<WitnessTrust>) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None).with_witness_serve(WitnessServe {
        seed: SERVE_SEED,
        key_name: "oxsum/witness".to_owned(),
        trust,
    });
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    (oxsum_server::app(db, config), pool)
}

/// The trust pin `OXSUM_WITNESS_TRUST` parses into for one origin at a head.
fn trust_at(origin: &Origin, head: TreeHead) -> WitnessTrust {
    WitnessTrust {
        origin: origin.to_string(),
        size: head.size,
        root: base64::engine::general_purpose::STANDARD.encode(head.root.as_bytes()),
    }
}

/// The DATABASE_URL tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("witness")
}

/// Posts a raw body to `/add-checkpoint` and returns status, content-type and
/// the response text — the endpoint speaks `text/plain`, not JSON.
async fn post_checkpoint(
    app: &Router,
    body: Vec<u8>,
    content_type: &str,
) -> (StatusCode, String, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/add-checkpoint")
        .header("content-type", content_type)
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let text = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
        .unwrap_or_default();
    (status, content_type, text)
}

/// The empty consistency proof `parse_body` assembles when no hash lines are
/// sent — old size comes from the witness's own stored state, so the struct
/// fields are placeholders the route never reads.
fn no_proof(old: u64, new: u64) -> ConsistencyProof {
    ConsistencyProof {
        old_size: old,
        new_size: new,
        path: Vec::new(),
    }
}

/// The happy path, end to end: a witness trusted at size 3, the log grows to
/// 8, the operator posts its signed head plus a real consistency proof, and the
/// answer is the note with this witness's cosignature — verified against the
/// public half of the configured seed. The served head in Postgres advances and
/// the returned note is filed as the voucher.
#[tokio::test]
async fn a_checkpoint_is_cosigned_and_the_served_head_advances() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let earlier = log.head_at(3).expect("in range");
    let (app, pool) = witness_app(&url, vec![trust_at(&origin, earlier)]).await;

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let proof = log.consistency_proof_between(3, 8).expect("in range");
    let body = submission_body(&origin, log.head(), &log_signer, proof);

    let (status, content_type, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        content_type.starts_with("text/plain"),
        "the answer is a note, not JSON: {content_type}"
    );

    // The answer re-parses, carries both signature lines, and this witness's
    // line verifies against the serve key's public half.
    let parsed = SignedTreeHead::parse(&text).expect("a note came back");
    assert_eq!(parsed.head, log.head());
    assert_eq!(parsed.signatures.len(), 2, "the log's line plus ours");
    let serve_key =
        SigningKey::from_seed(KeyName::new("oxsum/witness").expect("valid"), SERVE_SEED);
    let ours = parsed
        .signatures
        .iter()
        .find(|s| {
            serve_key
                .verifying_key()
                .verify_cosignature(&parsed.body(), s)
                .is_some()
        })
        .expect("our cosignature line verifies under the serve key");
    assert!(
        serve_key
            .verifying_key()
            .verify_cosignature(&parsed.body(), ours)
            .unwrap_or_default()
            > 0,
        "the cosignature carries its timestamp"
    );

    // The served head advanced to what was offered.
    let (size, root_b64): (i64, String) =
        sqlx::query_as("SELECT head_size, root_b64 FROM oxsum.witness_served WHERE origin = $1")
            .bind(origin.to_string())
            .fetch_one(&pool)
            .await
            .expect("the served head is stored");
    assert_eq!(size as u64, log.head().size);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&root_b64)
            .expect("base64")
            .as_slice(),
        log.head().root.as_bytes()
    );

    // And the voucher the witness returned is filed verbatim — the append-only
    // record of what it stands behind.
    let voucher: String = sqlx::query_scalar(
        "SELECT note FROM oxsum.witness_vouched WHERE origin = $1 AND head_size = $2",
    )
    .bind(origin.to_string())
    .bind(log.head().size as i64)
    .fetch_one(&pool)
    .await
    .expect("the voucher is recorded");
    assert_eq!(voucher, text);
}

/// A re-offer of the head the witness already vouched is a 200 again — the
/// same cosigned head — not an error. At-least-once delivery sends it twice.
#[tokio::test]
async fn a_reoffered_head_cosigns_again_without_advancing() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let earlier = log.head_at(3).expect("in range");
    let (app, pool) = witness_app(&url, vec![trust_at(&origin, earlier)]).await;

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let proof = log.consistency_proof_between(3, 8).expect("in range");
    let body = submission_body(&origin, log.head(), &log_signer, proof);

    let (first_status, _, first) = post_checkpoint(&app, body.clone(), "text/plain").await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    let (status, _, second) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::OK, "{second}");

    // The note is the same head — the timestamp inside the cosignature may
    // differ by a second, so compare the parsed head and the signature count
    // rather than the bytes.
    let again = SignedTreeHead::parse(&second).expect("a note came back");
    assert_eq!(again.head, log.head());
    assert_eq!(again.signatures.len(), 2);

    // The voucher recorded once, not twice.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.witness_vouched WHERE origin = $1 AND head_size = $2",
    )
    .bind(origin.to_string())
    .bind(log.head().size as i64)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1, "re-offering does not double-file the voucher");
}

/// Same size, different root: the witness refuses the fork with a 409 and
/// does not move off the head it already stands behind.
#[tokio::test]
async fn a_fork_at_the_same_size_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let (app, pool) = witness_app(&url, vec![trust_at(&origin, log.head())]).await;

    // A second history at size 8: flip the last leaf's hash.
    let mut restated = log_of(7);
    restated.append(Hash::from_bytes([0xff; 32]));
    assert_eq!(restated.head().size, 8);
    assert_ne!(restated.head().root, log.head().root);

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let body = submission_body(&origin, restated.head(), &log_signer, no_proof(8, 8));

    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");

    let stored: i64 =
        sqlx::query_scalar("SELECT head_size FROM oxsum.witness_served WHERE origin = $1")
            .bind(origin.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored as u64, 8, "the witness did not move off its head");
}

/// A head *behind* the one the witness vouched is a 409 too — the witness
/// only ever moves forward.
#[tokio::test]
async fn a_shrunk_checkpoint_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let (app, pool) = witness_app(&url, vec![trust_at(&origin, log.head())]).await;

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let older = log.head_at(5).expect("in range");
    let body = submission_body(&origin, older, &log_signer, no_proof(8, 5));

    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");

    let stored: i64 =
        sqlx::query_scalar("SELECT head_size FROM oxsum.witness_served WHERE origin = $1")
            .bind(origin.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored as u64, 8);
}

/// Growth offered without a consistency proof is a 400, not a fork: the
/// submitter can come back with the right proof.
#[tokio::test]
async fn growth_without_a_proof_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let earlier = log.head_at(3).expect("in range");
    let (app, _) = witness_app(&url, vec![trust_at(&origin, earlier)]).await;

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    // Signed head, but no proof lines at all.
    let body = submission_body(&origin, log.head(), &log_signer, no_proof(3, 8));

    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
}

/// A proof that does not verify — genuine hashes, wrong tree — is also a 400.
#[tokio::test]
async fn a_proof_that_does_not_verify_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let earlier = log.head_at(3).expect("in range");
    let (app, _) = witness_app(&url, vec![trust_at(&origin, earlier)]).await;

    // A proof built over a *different* history: real base64 lines, wrong root.
    let mut other = log_of(7);
    other.append(Hash::from_bytes([0xee; 32]));
    let bogus = other.consistency_proof_between(3, 8).expect("in range");

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let body = submission_body(&origin, log.head(), &log_signer, bogus);

    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
}

/// A note carrying no signature at all is a 401 — the witness signs nothing it
/// cannot attribute to a log key.
#[tokio::test]
async fn an_unsigned_note_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let earlier = log.head_at(3).expect("in range");
    let (app, _) = witness_app(&url, vec![trust_at(&origin, earlier)]).await;

    // No signature line: the body is just the note head plus proof.
    let proof = log.consistency_proof_between(3, 8).expect("in range");
    let note = SignedTreeHead::new(origin.clone(), log.head());
    let body = AddCheckpoint::new(note, proof).to_body();

    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
}

/// An origin this witness was never told to trust is a 404 — there is no
/// trust-on-first-use, which is the whole point of the pinset.
#[tokio::test]
async fn an_untrusted_origin_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let tracked = origin();
    let stranger = Origin::new("example.com/ledgers/stranger").expect("valid");
    let (app, _) = witness_app(&url, vec![trust_at(&tracked, log.head())]).await;

    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let body = submission_body(&stranger, log.head(), &log_signer, no_proof(0, 8));

    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
}

/// A deployment that never set `OXSUM_WITNESS_SIGNING_KEY` has no witness:
/// `/add-checkpoint` answers 404 like every other unmounted path.
#[tokio::test]
async fn a_deployment_without_a_witness_answers_404() {
    let app = offline_app();
    let log = log_of(4);
    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let body = submission_body(&origin(), log.head(), &log_signer, no_proof(0, 4));

    let (status, _, _) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The body is `text/plain` — a JSON submission is a 400, not a guess.
#[tokio::test]
async fn a_non_text_body_is_refused() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let (app, _) = witness_app(&url, vec![trust_at(&origin(), log_of(1).head())]).await;
    let (status, _, _) = post_checkpoint(&app, b"{}".to_vec(), "application/json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Boot-time guard: re-seeding a trust pin that contradicts a head the witness
/// already vouched refuses to start, rather than rolling back onto a fork.
#[tokio::test]
async fn reseeding_a_contradictory_root_refuses_to_start() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    let log = log_of(8);
    let origin = origin();
    let earlier = log.head_at(3).expect("in range");
    // First boot: serve and advance past the pin.
    let (app, pool) = witness_app(&url, vec![trust_at(&origin, earlier)]).await;
    let log_signer = SigningKey::from_seed(KeyName::new("acme/log").expect("valid"), LOG_SEED);
    let proof = log.consistency_proof_between(3, 8).expect("in range");
    let body = submission_body(&origin, log.head(), &log_signer, proof);
    let (status, _, text) = post_checkpoint(&app, body, "text/plain").await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Re-pin at the size the stored head now stands at (8 — the witness
    // advanced past the pin), under a root that is not the one it vouched.
    // A re-pin at a size *behind* the stored head would be a legitimate no-op
    // — the root is simply behind it now — so the contradiction has to be at
    // the size the witness actually serves.
    let db = Db::from_pool(pool.clone());
    let mut wrong = [0u8; 32];
    wrong[0] = 0xaa;
    let result = db
        .seed_witness_trust(
            &origin.to_string(),
            log.head().size,
            &base64::engine::general_purpose::STANDARD.encode(wrong),
        )
        .await;
    assert!(
        result.is_err(),
        "a contradictory re-pin must not be a silent no-op"
    );

    // The honest re-pin — same size, same root — is the no-op.
    db.seed_witness_trust(&origin.to_string(), log.head().size, &{
        base64::engine::general_purpose::STANDARD.encode(log.head().root.as_bytes())
    })
    .await
    .expect("re-pinning the served root is a no-op");
}

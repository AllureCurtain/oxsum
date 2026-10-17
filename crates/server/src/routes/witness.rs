//! The deployment's own tlog-witness (issue #260): `POST /add-checkpoint`.
//!
//! A foreign transparency log offers its head here; the witness decides whether
//! it will stand behind that history and no other, signs its acceptance with a
//! `cosignature/v1` line, and returns the note. The rules are the ones
//! `doubleentry::witness::Cosigner` already runs — an untracked origin is
//! refused, a fork is refused permanently, a proof that does not verify is
//! refused — so this file is only the HTTP and the durable-store halves:
//! reading the body, running the state machine against the heads this witness
//! already serves, and writing the accepted head plus the voucher back.
//!
//! The endpoint is opt-in: a deployment that does not set
//! `OXSUM_WITNESS_SIGNING_KEY` has no witness, and the route is not mounted —
//! a submission lands on the same 404 every unknown path does.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

use oxsum_core::{
    Cosigner, HeadSigningKey, MemoryWitnessStore, Origin, SignedTreeHead, Witness, WitnessError,
    WitnessKeyName,
};

use crate::error::ApiError;
use crate::{AppState, WitnessServe};

/// `POST /add-checkpoint` — offer a head, get this witness's cosignature back.
///
/// The wire is the C2SP `tlog-witness` body, `text/plain`: the submitter's
/// signed note, a blank line, then one base64 line per consistency-proof hash.
/// The answer on accept is the note with this witness's `cosignature/v1` line
/// appended — `text/plain` again, not the JSON envelope, because the
/// submitting witness client files it verbatim as its cosigned note.
pub(crate) async fn add_checkpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, HeaderMap, String), ApiError> {
    let Some(serve) = state.config.witness_serve() else {
        return Err(ApiError::NotFound("not found".into()));
    };
    // The content type is checked for the same reason the body is parsed
    // strictly: a witness that guesses at the shape it signed is a witness
    // that can be talked into vouching bytes it never read.
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("text/plain") {
        return Err(ApiError::Validation(
            "add-checkpoint takes a text/plain tlog-witness body".into(),
        ));
    }

    // Parse the note alone first — the proof's old size is the witness's own
    // state, which the route does not consult yet, so the note decides which
    // origin is being offered and whether it is signed at all.
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError::Validation("the checkpoint body is not UTF-8".into()))?;
    let note = parse_submission_note(text)?;
    if note.signatures.is_empty() {
        return Err(ApiError::Unauthorized);
    }
    let origin = note.origin.clone();

    let mut tx = state
        .db
        .pool()
        .begin()
        .await
        .map_err(|_| ApiError::Internal)?;
    // Serialise submissions for this origin: the state machine's "what head do
    // I already hold" question is only honest when the answer cannot change
    // underneath it.
    oxsum_core::Db::lock_witness_origin(&mut tx, origin.as_str())
        .await
        .map_err(|_| ApiError::Internal)?;
    let tracked = oxsum_core::Db::witness_served_head(&mut tx, origin.as_str())
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(known) = tracked else {
        // No trust root for this origin: refuse rather than TOFU. The witness
        // only vouches forward from a head the operator pinned out of band.
        return Err(ApiError::NotFound("not found".into()));
    };

    // Now that the witness's own state gives the old size, the proof lines can
    // be read: `AddCheckpoint::parse_body` takes that size from the stored
    // head, never from the request — a proof cannot be aimed at a head this
    // witness never accepted.
    let request = oxsum_core::AddCheckpoint::parse_body(&body, known.size)
        .map_err(|_| ApiError::Validation("the checkpoint body did not parse".into()))?;

    // Run the decision against the stored state: a single-head store preloaded
    // with what Postgres holds. The signature is drawn over the accepted head,
    // so a re-offer cosigns the head the witness already vouched, not the bytes
    // it was handed twice.
    let store = MemoryWitnessStore::from_heads([(origin.clone(), known)]);
    let key = signing_key(serve)?;
    let mut cosigner = Cosigner::new(Witness::new(store), key);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let signature = cosigner
        .cosign(&origin, request.note().head, Some(request.proof()), at)
        .map_err(|error| map_witness_error(&origin, error))?;
    let accepted = cosigner.witness().head(&origin).ok_or(ApiError::Internal)?;

    // The cosigned note this witness hands back: the accepted head's canonical
    // body — the one `Cosigner` signed — then the submitter's signature lines
    // preserved, then this witness's line. Extension lines are not re-served:
    // the cosignature attests the three-line checkpoint, and re-attaching
    // extensions would leave our line verifying against a body we never signed.
    let mut cosigned = SignedTreeHead::new(origin.clone(), accepted);
    for signature in &request.note().signatures {
        cosigned.add_signature(signature.clone());
    }
    cosigned.add_signature(signature);
    let note_text = cosigned.to_note();

    // Durable before it is answered: the served head advances and the voucher
    // is filed in the same transaction, so a crash cannot leave a signature the
    // witness has no memory of.
    oxsum_core::Db::record_witness_served(&mut tx, origin.as_str(), accepted)
        .await
        .map_err(|_| ApiError::Internal)?;
    oxsum_core::Db::record_witness_vouched(
        &mut tx,
        origin.as_str(),
        accepted.size as i64,
        &note_text,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;

    let mut out = HeaderMap::new();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok((StatusCode::OK, out, note_text))
}

/// Reads just the checkpoint note off a request body: everything up to the
/// blank line plus the signature lines under it — the proof hashes that may
/// follow are left for [`oxsum_core::AddCheckpoint::parse_body`], which wants
/// the old size from the stored head first.
fn parse_submission_note(text: &str) -> Result<SignedTreeHead, ApiError> {
    // A note is body, a blank line, then signature lines; the proof lines begin
    // at the first line that is not a signature. The note is self-delimiting:
    // `SignedTreeHead::parse` reads its own three lines and the `— name blob`
    // lines that follow, and stops before the proof hashes it cannot read.
    let end = text
        .find("\n\n")
        .map(|i| {
            // Keep the blank line and any signature lines; the proof hashes
            // begin after them. Take everything up to the last signature line.
            let rest = &text[i + 2..];
            let mut upto = i + 2;
            for line in rest.split('\n') {
                if line.starts_with("— ") || line.starts_with("-- ") {
                    upto += line.len() + 1;
                } else {
                    break;
                }
            }
            upto
        })
        .unwrap_or(text.len());
    SignedTreeHead::parse(&text[..end]).map_err(|error| {
        ApiError::Validation(format!("the checkpoint note did not parse: {error}"))
    })
}

/// The signing key this witness cosigns under, from the serve config's seed.
fn signing_key(serve: &WitnessServe) -> Result<HeadSigningKey, ApiError> {
    let name = WitnessKeyName::new(serve.key_name.clone()).map_err(|_| ApiError::Internal)?;
    Ok(HeadSigningKey::from_seed(name, serve.seed))
}

/// Maps the state machine's refusal onto an HTTP status — the witness's own
/// vocabulary, since the submitter reads the status to decide whether to retry
/// or to alarm.
fn map_witness_error(origin: &Origin, error: WitnessError) -> ApiError {
    match error {
        // An origin this witness does not track — the pinset names the
        // origins it vouches for, and a submission for any other is refused.
        WitnessError::Untracked { .. } => ApiError::NotFound("not found".into()),
        // The one refusal worth running a witness for: same size, different
        // root — the head it already vouched contradicts this one, and the
        // witness will not sign a fork. Conflict, permanently.
        WitnessError::Fork { .. } | WitnessError::Shrunk { .. } => ApiError::Conflict(format!(
            "the checkpoint contradicts the head this witness already vouched for {origin}"
        )),
        // A proof that is absent or does not verify is a bad request, not a
        // fork — the submitter can re-offer with the right proof.
        WitnessError::MissingProof { .. } | WitnessError::BadProof { .. } => ApiError::Validation(
            format!("the consistency proof for {origin} is missing or does not verify"),
        ),
        // The note named a different origin than the route parsed — cannot
        // happen through this handler, which reads the origin from the note,
        // but the variant is non-exhaustive and a mismatch is a bad request.
        _ => ApiError::Validation(format!("the checkpoint for {origin} was refused")),
    }
}

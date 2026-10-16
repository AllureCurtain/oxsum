use doubleentry::storage::postgres::PostgresError;

/// Domain-layer errors. The HTTP layer maps these onto the codes defined in docs/api.md.
#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    /// The caller passed an invalid argument; safe to show back to the caller as-is.
    #[error("{0}")]
    InvalidInput(String),

    /// No credential, or one that authenticates nothing. Deliberately one variant: whether
    /// a key is missing, unknown, revoked or expired is not the caller's business.
    #[error("unauthorized")]
    Unauthenticated,

    /// A login with an unknown email or a wrong password. Deliberately one variant and one
    /// message: the caller must not learn which of the two it was.
    #[error("invalid email or password")]
    InvalidCredentials,

    /// The caller is authenticated but may not do this.
    #[error("{0}")]
    Forbidden(String),

    /// The value is already taken.
    #[error("{0}")]
    Conflict(String),

    /// The settlement named a hold that is not outstanding: no entry under the key, or the
    /// entry is not a hold. The HTTP layer answers 404.
    #[error("{0}")]
    HoldNotFound(String),

    /// A resource the caller named does not exist, or is not one the caller's organization
    /// has: an email no account holds, a user who is not a member. The HTTP layer answers
    /// 404 with the message, which is safe to show — the caller is already authorized to
    /// manage memberships and needs to know which of the two it was.
    #[error("{0}")]
    NotFound(String),

    /// The available balance cannot cover this hold or charge.
    #[error("insufficient funds")]
    InsufficientFunds,

    /// A hold would push the acting API key past its spend limit: settled charges plus
    /// outstanding holds attributed to the key, in minor units, would exceed the limit.
    /// The HTTP layer answers 429; the numbers are the key's own, so they are safe to show.
    #[error(
        "key spend limit exceeded: {committed_minor} of {limit_minor} minor units already committed"
    )]
    KeyLimitExceeded {
        limit_minor: i64,
        committed_minor: i64,
    },

    /// A request would push the acting API key past its rolling-minute request
    /// allowance. The HTTP layer answers 429 with `Retry-After`; the numbers are
    /// the key's own, so they are safe to show.
    #[error("key rate limit exceeded: {limit} requests per minute, retry in {retry_after_secs}s")]
    RateLimited { limit: i64, retry_after_secs: u64 },

    /// A request would push the acting API key past its rolling-minute token
    /// allowance — the settled input+output tokens inside the trailing minute
    /// plus this request's estimate would exceed the cap (issue #203). TPM's own
    /// variant, because a token cap and a request cap are different refusals:
    /// the HTTP layer answers 429 with `Retry-After` and the `X-RateLimit-*`
    /// headers, under its own code `TOKENS_PER_MINUTE`.
    #[error(
        "key token rate limit exceeded: {limit} tokens per minute, retry in {retry_after_secs}s"
    )]
    TokensPerMinute { limit: i64, retry_after_secs: u64 },

    /// A hold would push the acting API key past its outstanding-holds cap. The
    /// HTTP layer answers 429; the numbers are the key's own, so they are safe to
    /// show.
    #[error("key has {open} holds outstanding, at or over its cap of {limit}")]
    TooManyHolds { limit: i64, open: i64 },

    /// The presented `X-Oxsum-Mandate` did not verify — bad base64, bad JSON, or
    /// a signature that fails under the key's mandate public key. The HTTP layer
    /// answers 403 `invalid_mandate`, distinct from the key's own refusals so the
    /// delegate learns it is the delegation that failed, not the credential.
    #[error("the presented mandate is not valid for this key")]
    InvalidMandate,

    /// The mandate's window does not cover this request: before `notBefore` or
    /// past `notAfter`. The HTTP layer answers 403; `not_yet` distinguishes the
    /// two in the caller-facing message.
    #[error("the presented mandate is {}", if *.not_yet { "not yet valid" } else { "expired" })]
    MandateOutOfWindow {
        /// `true` when `now < notBefore`, `false` when `now > notAfter`.
        not_yet: bool,
    },

    /// A hold would push the mandate's committed spend past its `capMinor`:
    /// settled charges plus outstanding holds under this `mandateId`, in minor
    /// units, would exceed the cap. The HTTP layer answers 429
    /// `mandate_exceeded`; the numbers are the mandate's own.
    #[error(
        "mandate cap exceeded: {committed_minor} of {cap_minor} minor units already committed under it"
    )]
    MandateExceeded {
        cap_minor: i64,
        committed_minor: i64,
    },

    /// This deployment cannot serve what it was asked for: a stored channel credential that does not
    /// open under `OXSUM_SECRET_KEY`, or a key that is not configured at all. The message is for the
    /// operator; the HTTP layer answers 500 with a generic one, because this is neither the caller's
    /// mistake nor anything a caller can act on (docs/api.md).
    #[error("misconfigured: {0}")]
    Misconfigured(String),

    /// A pooled connection could not be checked out inside the acquire timeout,
    /// or the pool was closed mid-request: the process is briefly saturated,
    /// not broken, and a retry after a moment is the right answer (issue #235).
    /// The HTTP layer answers 429 `POOL_BUSY` with `Retry-After` — never the
    /// 500 a plain storage failure carries, because a burst looks like an
    /// outage and invites retry storms at exactly the wrong moment.
    #[error("the connection pool is saturated")]
    PoolBusy,

    /// A storage failure. Details go to the logs only, never to the caller.
    #[error("storage error: {0}")]
    Storage(#[source] PostgresError),
}

impl From<PostgresError> for WalletError {
    fn from(e: PostgresError) -> Self {
        match e {
            PostgresError::LimitBreached { .. } => Self::InsufficientFunds,
            // A key that is already held by an entry with different content is the caller's
            // mistake, not a broken ledger: the request is a conflicting reuse of a key, and
            // the caller can fix it by choosing another one. Mapping it to a storage failure
            // would answer 500 for a 409, and log a caller error as an incident.
            PostgresError::IdempotencyConflict { .. } => {
                Self::Conflict("idempotency key already used for a different request".into())
            }
            // Ledger paths reach the pool through `PostgresStore`, so a
            // saturated pool surfaces wrapped inside the engine's own error.
            PostgresError::Database(sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed) => {
                Self::PoolBusy
            }
            other => Self::Storage(other),
        }
    }
}

impl From<sqlx::Error> for WalletError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => Self::PoolBusy,
            other => Self::Storage(other.into()),
        }
    }
}

pub(crate) fn invalid(e: impl std::fmt::Display) -> WalletError {
    WalletError::InvalidInput(e.to_string())
}

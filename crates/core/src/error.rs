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

    /// This deployment cannot serve what it was asked for: a stored channel credential that does not
    /// open under `OXSUM_SECRET_KEY`, or a key that is not configured at all. The message is for the
    /// operator; the HTTP layer answers 500 with a generic one, because this is neither the caller's
    /// mistake nor anything a caller can act on (docs/api.md).
    #[error("misconfigured: {0}")]
    Misconfigured(String),

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
            other => Self::Storage(other),
        }
    }
}

impl From<sqlx::Error> for WalletError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.into())
    }
}

pub(crate) fn invalid(e: impl std::fmt::Display) -> WalletError {
    WalletError::InvalidInput(e.to_string())
}

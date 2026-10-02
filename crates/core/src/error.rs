use doubleentry::storage::postgres::PostgresError;

/// Domain-layer errors. The HTTP layer maps these onto the codes defined in docs/api.md.
#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    /// The caller passed an invalid argument; safe to show back to the caller as-is.
    #[error("{0}")]
    InvalidInput(String),

    /// The available balance cannot cover this hold or charge.
    #[error("insufficient funds")]
    InsufficientFunds,

    /// A storage failure. Details go to the logs only, never to the caller.
    #[error("storage error: {0}")]
    Storage(#[source] PostgresError),
}

impl From<PostgresError> for WalletError {
    fn from(e: PostgresError) -> Self {
        match e {
            PostgresError::LimitBreached { .. } => Self::InsufficientFunds,
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

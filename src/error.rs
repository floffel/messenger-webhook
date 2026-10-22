//! Errors.

use thiserror::Error;

/// Parsing failures.
#[derive(Debug, Error)]
pub enum Error {
    /// Payload is not valid JSON for the provider.
    #[error("invalid payload: {0}")]
    Payload(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

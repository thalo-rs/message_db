use thiserror::Error;

/// Type alias for `Result<T, message_db::Error>`
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Represents all the ways a method can fail.
#[derive(Debug, Error)]
pub enum Error {
    /// Message data failed to deserialize.
    #[error("failed to deserialize data: {0}")]
    DeserializeData(serde_json::Error),
    /// Stream name is empty.
    #[error("stream name is empty")]
    EmptyStreamName,

    /// Database error.
    #[cfg(feature = "database")]
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// Failed to decode a value.
    #[cfg(feature = "database")]
    #[error("failed to decode: expected {expected}")]
    Decode {
        /// Expected value.
        expected: &'static str,
    },
    /// Message metadata failed to deserialize.
    #[cfg(feature = "database")]
    #[error("failed to deserialize metadata: {0}")]
    DeserializeMetadata(serde_json::Error),
}

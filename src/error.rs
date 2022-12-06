use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[cfg(feature = "database")]
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[cfg(feature = "database")]
    #[error("failed to decode: expected {expected}")]
    Decode { expected: &'static str },
    #[error("failed to deserialize data: {0}")]
    DeserializeData(serde_json::Error),
    #[cfg(feature = "database")]
    #[error("failed to deserialize metadata: {0}")]
    DeserializeMetadata(serde_json::Error),
    #[error("stream category or ID contains separator")]
    ContainsSeparator,
    #[error("stream category is empty")]
    EmptyStreamID,
}

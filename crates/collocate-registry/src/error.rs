#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid reference {0:?}")]
    InvalidReference(String),
    #[error("http: {0}")]
    Http(String),
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("no manifest for platform {os}/{arch}")]
    NoMatchingPlatform { os: String, arch: String },
    #[error("unexpected auth challenge: {0}")]
    Auth(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

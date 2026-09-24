#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid size: {0}")]
    InvalidSize(String),
    #[error("invalid container id: {0}")]
    InvalidId(String),
    #[error("invalid publish mapping: {0}")]
    InvalidPublish(String),
    #[error("invalid volume: {0}")]
    InvalidVolume(String),
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("frame of {0} bytes exceeds the limit")]
    FrameTooLarge(usize),
    #[error("connection closed")]
    Eof,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("ambiguous reference, candidates: {0:?}")]
    Ambiguous(Vec<String>),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("daemon unreachable: {0}")]
    Unreachable(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("{0}")]
    NotInitialized(String),
    #[error("permission denied: {0}")]
    Denied(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
}

impl Error {
    pub fn payload(&self) -> String {
        match self {
            Error::InvalidSize(s)
            | Error::InvalidId(s)
            | Error::InvalidPublish(s)
            | Error::InvalidVolume(s)
            | Error::InvalidSpec(s)
            | Error::Invalid(s)
            | Error::Parse(s)
            | Error::NotFound(s)
            | Error::Conflict(s)
            | Error::Timeout(s)
            | Error::Unreachable(s)
            | Error::Internal(s)
            | Error::NotInitialized(s)
            | Error::Denied(s)
            | Error::Forbidden(s) => s.clone(),
            Error::FrameTooLarge(n) => n.to_string(),
            Error::Eof => String::new(),
            Error::Io(e) => e.to_string(),
            Error::Json(e) => e.to_string(),
            Error::Ambiguous(v) => format!("{v:?}"),
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Error::InvalidSize(_)
            | Error::InvalidId(_)
            | Error::InvalidPublish(_)
            | Error::InvalidVolume(_)
            | Error::InvalidSpec(_)
            | Error::Invalid(_)
            | Error::Ambiguous(_) => 2,
            Error::NotFound(_) => 3,
            Error::Unreachable(_) | Error::Eof => 4,
            Error::Conflict(_) => 5,
            Error::Timeout(_) => 6,
            Error::NotInitialized(_) => 7,
            Error::Denied(_) => 8,
            Error::Forbidden(_) => 9,
            _ => 1,
        }
    }
}

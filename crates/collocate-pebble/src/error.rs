#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pebble socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed pebble response: {0}")]
    Protocol(String),
    #[error("pebble returned {status}: {message}")]
    Api { status: u16, message: String },
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Protocol(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

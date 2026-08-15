use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("transport: {0}")]
    Transport(String),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("peer {0} refused: {1}")]
    Refused(String, String),

    #[error("no route to peer — every tier of the ladder failed")]
    Unreachable,

    #[error("rendezvous lookup found nothing")]
    NotFound,

    #[error("the network task has stopped")]
    Stopped,

    #[error(transparent)]
    Core(#[from] vega_core::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Protocol(e.to_string())
    }
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for Error {
    fn from(_: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Error::Stopped
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for Error {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> Self {
        Error::Stopped
    }
}

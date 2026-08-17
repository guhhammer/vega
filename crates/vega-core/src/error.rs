//! One error type for the whole crate.
//!
//! The prototype panicked on malformed input from the network, which is a
//! remotely triggerable crash. Nothing in this crate panics on untrusted bytes.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("signature verification failed: {0}")]
    BadSignature(&'static str),

    #[error("malformed key: {0}")]
    BadKey(String),

    #[error("sigchain invalid: {0}")]
    Sigchain(String),

    #[error("decryption failed")]
    Decrypt,

    #[error("no olm session with device {0}")]
    NoSession(String),

    #[error("no prekeys published for device {0}")]
    NoPrekeys(String),

    #[error("unknown contact: {0}")]
    UnknownContact(String),

    #[error("malformed wire data: {0}")]
    Wire(String),

    /// A file cannot be sent as it stands. Unlike the rest of these, this one is
    /// written to be read by whoever picked the file, so it carries the whole
    /// sentence rather than a prefix and a detail.
    #[error("{0}")]
    File(String),

    #[error("storage: {0}")]
    Storage(String),

    #[error("this identity has no root key (it lives on another device)")]
    NoRootKey,

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! storage_from {
    ($($t:ty),* $(,)?) => {
        $(impl From<$t> for Error {
            fn from(e: $t) -> Self {
                Error::Storage(e.to_string())
            }
        })*
    };
}

storage_from!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
);

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Wire(e.to_string())
    }
}

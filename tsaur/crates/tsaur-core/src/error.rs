//! Error type and stable exit codes.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a T-saur archive (bad magic)")]
    BadMagic,
    #[error("unsupported format version {0}: this build reads version 1 only, a newer T-saur is needed")]
    Version(u16),
    #[error("archive is corrupt: {0}")]
    Corrupt(String),
    #[error("hash mismatch: {0}")]
    HashMismatch(String),
    #[error("resource limit exceeded: {0}")]
    Limit(String),
    #[error("refused by policy: {0}")]
    Policy(String),
    #[error("cryptography: {0}")]
    Crypto(String),
    #[error("missing data: {0}")]
    Missing(String),
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable process exit codes used by the CLI:
    /// 1 generic, 2 corrupt/integrity, 3 policy, 4 limits, 5 missing data, 6 crypto, 7 invalid argument.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::BadMagic | Error::Version(_) | Error::Corrupt(_) | Error::HashMismatch(_) => 2,
            Error::Policy(_) => 3,
            Error::Limit(_) => 4,
            Error::Missing(_) => 5,
            Error::Crypto(_) => 6,
            Error::Invalid(_) | Error::Encoding(_) => 7,
            Error::Io(_) => 1,
        }
    }
}

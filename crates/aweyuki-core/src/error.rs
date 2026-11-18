use thiserror::Error;

pub type Result<T> = std::result::Result<T, FlashError>;

#[derive(Debug, Error)]
pub enum FlashError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Protobuf decode error: {0}")]
    Protobuf(#[from] prost::DecodeError),

    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Checksum mismatch on {context}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        context: String,
        expected: String,
        actual: String,
    },

    #[error("Partition not found: {0}")]
    PartitionNotFound(String),

    #[error("Size limit exceeded: {0}")]
    SizeLimitExceeded(String),

    #[error("Device transport error: {0}")]
    Transport(String),
}

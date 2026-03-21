use thiserror::Error;

#[derive(Debug, Error)]
pub enum CredentialStoreError {
    #[error("key derivation failed: {0}")]
    KeyDerivation(String),

    #[error("encryption failed: {0}")]
    Encryption(String),

    #[error("decryption failed: {0}")]
    Decryption(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("credential not found")]
    NotFound,

    #[error("invalid master key")]
    InvalidMasterKey,

    #[error("token refresh failed: {0}")]
    RefreshFailed(String),

    #[error("credential invalid: {0}")]
    CredentialInvalid(String),

    #[error("service account error: {0}")]
    ServiceAccountError(String),
}

pub type Result<T> = std::result::Result<T, CredentialStoreError>;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The type of credential stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    OAuth,
    ServiceAccount,
}

/// Metadata for a Google service account credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountMetadata {
    pub client_email: String,
    pub token_uri: String,
}

/// Decrypted credential, held in memory. Sensitive fields are zeroized on drop.
#[derive(Debug, Zeroize, ZeroizeOnDrop)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[zeroize(skip)]
    pub user_id: String,
    #[zeroize(skip)]
    pub skill_name: String,
    #[zeroize(skip)]
    pub provider: String,
    #[zeroize(skip)]
    pub scopes: String,
    #[zeroize(skip)]
    pub expires_at: Option<i64>,
    #[zeroize(skip)]
    pub credential_type: CredentialType,
    #[zeroize(skip)]
    pub metadata: Option<ServiceAccountMetadata>,
}

/// Raw encrypted row from the database.
#[derive(Debug, sqlx::FromRow)]
pub struct CredentialRow {
    pub user_id: String,
    pub skill_name: String,
    pub provider: String,
    pub access_token_enc: Vec<u8>,
    pub refresh_token_enc: Option<Vec<u8>>,
    pub expires_at: Option<i64>,
    pub scopes: String,
    pub user_salt: Vec<u8>,
    pub created_at: i64,
    pub updated_at: i64,
    pub credential_type: String,
    pub metadata_enc: Option<Vec<u8>>,
}

/// Metadata-only view of a credential (no tokens).
#[derive(Debug, Clone, Serialize)]
pub struct CredentialSummary {
    pub skill_name: String,
    pub provider: String,
    pub scopes: String,
    pub expires_at: Option<i64>,
    pub credential_type: String,
}

/// Deserialized token response from an OAuth provider.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

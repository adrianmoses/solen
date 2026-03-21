use async_trait::async_trait;

use crate::error::{CredentialStoreError, Result};
use crate::types::TokenResponse;

/// Trait for refreshing OAuth tokens and minting service account tokens.
/// The server implements this with reqwest; tests provide a mock implementation.
#[async_trait]
pub trait TokenRefresher: Send + Sync {
    async fn refresh_token(&self, provider: &str, refresh_token: &str) -> Result<TokenResponse>;

    /// Mint a short-lived access token from a service account private key via JWT signing.
    /// Default implementation returns an error — only the server's reqwest-backed
    /// implementation provides real JWT minting.
    async fn mint_service_account_token(
        &self,
        _private_key_pem: &str,
        _client_email: &str,
        _token_uri: &str,
        _scopes: &str,
    ) -> Result<TokenResponse> {
        Err(CredentialStoreError::ServiceAccountError(
            "service account token minting not supported by this refresher".into(),
        ))
    }
}

/// Returns true if the token is expired or within 60 seconds of expiring.
/// Returns false if `expires_at` is None (tokens that don't expire).
pub fn is_expired(expires_at: Option<i64>, now_unix_secs: i64) -> bool {
    match expires_at {
        None => false,
        Some(exp) => now_unix_secs >= exp - 60,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_not_expired() {
        assert!(!is_expired(None, 1_000_000));
    }

    #[test]
    fn future_not_expired() {
        // Expires 120s from now — well outside the 60s buffer
        assert!(!is_expired(Some(1_000_120), 1_000_000));
    }

    #[test]
    fn within_buffer_expired() {
        // Expires 30s from now — within 60s buffer
        assert!(is_expired(Some(1_000_030), 1_000_000));
    }

    #[test]
    fn exactly_at_buffer_expired() {
        // Expires exactly 60s from now — now >= exp - 60
        assert!(is_expired(Some(1_000_060), 1_000_000));
    }

    #[test]
    fn past_expired() {
        assert!(is_expired(Some(999_000), 1_000_000));
    }
}

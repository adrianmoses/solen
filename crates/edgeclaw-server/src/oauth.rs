use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

use credential_store::types::TokenResponse;
use credential_store::CredentialStoreError;

/// OAuth provider configuration (loaded from env vars).
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub default_scopes: String,
    pub extra_auth_params: Vec<(String, String)>,
}

/// In-flight OAuth PKCE flow state, keyed by nonce.
pub struct OAuthFlowState {
    pub user_id: String,
    pub skill_name: String,
    pub provider: String,
    pub code_verifier: String,
    pub scopes: String,
    pub expires_at: u64,
    pub created_at: u64,
}

pub type OAuthFlows = Arc<Mutex<HashMap<String, OAuthFlowState>>>;

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("flow not found")]
    FlowNotFound,
    #[error("flow expired")]
    FlowExpired,
    #[error("provider not configured: {0}")]
    ProviderNotConfigured(String),
    #[error("token exchange failed: {0}")]
    TokenExchangeFailed(String),
    #[error("master key not configured")]
    MasterKeyNotConfigured,
    #[error("credential store error: {0}")]
    CredentialStore(#[from] CredentialStoreError),
}

// --- PKCE helpers ---

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs()
}

/// Generate a 16-byte random nonce, base64url-encoded (no padding).
pub fn generate_nonce() -> String {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Generate a 32-byte random code verifier, base64url-encoded (no padding).
pub fn generate_code_verifier() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Compute S256 code challenge: BASE64URL(SHA256(verifier)).
pub fn compute_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Build the full authorization URL with PKCE params.
pub fn build_authorization_url(
    provider: &ProviderConfig,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
    scopes: &str,
) -> String {
    let mut url = url::Url::parse(&provider.auth_url).expect("invalid auth_url in provider config");

    url.query_pairs_mut()
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("scope", scopes);

    for (k, v) in &provider.extra_auth_params {
        url.query_pairs_mut().append_pair(k, v);
    }

    url.to_string()
}

// --- Flow management ---

/// Start an OAuth PKCE flow. Returns `(nonce, authorization_url)`.
pub fn init_flow(
    flows: &OAuthFlows,
    user_id: String,
    skill_name: String,
    provider_config: &ProviderConfig,
    provider_name: String,
    redirect_uri: &str,
    scopes: Option<&str>,
) -> (String, String) {
    let nonce = generate_nonce();
    let code_verifier = generate_code_verifier();
    let code_challenge = compute_code_challenge(&code_verifier);
    let scopes = scopes.unwrap_or(&provider_config.default_scopes);
    let now = now_unix_secs();

    let auth_url = build_authorization_url(
        provider_config,
        redirect_uri,
        &nonce,
        &code_challenge,
        scopes,
    );

    let state = OAuthFlowState {
        user_id,
        skill_name,
        provider: provider_name,
        code_verifier,
        scopes: scopes.to_string(),
        expires_at: now + 600, // 10-minute TTL
        created_at: now,
    };

    flows
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(nonce.clone(), state);

    (nonce, auth_url)
}

/// Consume a flow by nonce. Returns error if not found or expired.
pub fn complete_flow(flows: &OAuthFlows, nonce: &str) -> Result<OAuthFlowState, OAuthError> {
    let state = flows
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(nonce)
        .ok_or(OAuthError::FlowNotFound)?;

    if now_unix_secs() > state.expires_at {
        return Err(OAuthError::FlowExpired);
    }

    Ok(state)
}

// --- Token exchange ---

/// Exchange an authorization code for tokens at the provider's token endpoint.
pub async fn exchange_code(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, OAuthError> {
    let resp = client
        .post(&provider.token_url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &provider.client_id),
            ("client_secret", &provider.client_secret),
            ("code", code),
            ("code_verifier", code_verifier),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .map_err(|e| OAuthError::TokenExchangeFailed(e.to_string()))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_else(|_| "no body".to_string());
        return Err(OAuthError::TokenExchangeFailed(format!(
            "provider returned error: {body}"
        )));
    }

    resp.json::<TokenResponse>()
        .await
        .map_err(|e| OAuthError::TokenExchangeFailed(e.to_string()))
}

// --- Token refresher (implements credential_store::TokenRefresher) ---

/// Concrete `TokenRefresher` backed by reqwest. Lives in the server crate
/// so that `credential-store` stays free of HTTP framework deps.
pub struct ReqwestTokenRefresher {
    pub providers: HashMap<String, ProviderConfig>,
    pub client: reqwest::Client,
}

#[async_trait]
impl credential_store::TokenRefresher for ReqwestTokenRefresher {
    async fn refresh_token(
        &self,
        provider: &str,
        refresh_token: &str,
    ) -> credential_store::error::Result<TokenResponse> {
        let config = self.providers.get(provider).ok_or_else(|| {
            CredentialStoreError::RefreshFailed(format!("provider not configured: {provider}"))
        })?;

        let resp = self
            .client
            .post(&config.token_url)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &config.client_id),
                ("client_secret", &config.client_secret),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|e| CredentialStoreError::RefreshFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "no body".to_string());
            return Err(CredentialStoreError::RefreshFailed(format!(
                "provider returned error: {body}"
            )));
        }

        resp.json::<TokenResponse>()
            .await
            .map_err(|e| CredentialStoreError::RefreshFailed(e.to_string()))
    }

    async fn mint_service_account_token(
        &self,
        private_key_pem: &str,
        client_email: &str,
        token_uri: &str,
        scopes: &str,
    ) -> credential_store::error::Result<TokenResponse> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX epoch")
            .as_secs();

        let claims = serde_json::json!({
            "iss": client_email,
            "sub": client_email,
            "aud": token_uri,
            "iat": now,
            "exp": now + 3600,
            "scope": scopes,
        });

        let key =
            jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).map_err(|e| {
                CredentialStoreError::ServiceAccountError(format!("invalid RSA key: {e}"))
            })?;

        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| CredentialStoreError::ServiceAccountError(format!("JWT sign failed: {e}")))?;

        let resp = self
            .client
            .post(token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| CredentialStoreError::ServiceAccountError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "no body".to_string());
            return Err(CredentialStoreError::ServiceAccountError(format!(
                "token endpoint returned error: {body}"
            )));
        }

        resp.json::<TokenResponse>()
            .await
            .map_err(|e| CredentialStoreError::ServiceAccountError(e.to_string()))
    }
}

// --- Cleanup task ---

/// Spawns a background task that removes expired flow entries every 60 seconds.
pub fn spawn_flow_cleanup(flows: OAuthFlows) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            let now = now_unix_secs();
            flows
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, v| v.expires_at > now);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> ProviderConfig {
        ProviderConfig {
            client_id: "test-client-id".into(),
            client_secret: "test-secret".into(),
            auth_url: "https://example.com/authorize".into(),
            token_url: "https://example.com/token".into(),
            default_scopes: "read,write".into(),
            extra_auth_params: vec![],
        }
    }

    #[test]
    fn verifier_is_43_chars_base64url() {
        let v = generate_code_verifier();
        // 32 bytes → 43 base64url chars (no padding)
        assert_eq!(v.len(), 43);
        assert!(v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn challenge_matches_rfc7636_properties() {
        let verifier = generate_code_verifier();
        let challenge = compute_code_challenge(&verifier);
        // SHA-256 → 32 bytes → 43 base64url chars
        assert_eq!(challenge.len(), 43);
        // Challenge should differ from verifier
        assert_ne!(challenge, verifier);
        // Same input → same output
        assert_eq!(challenge, compute_code_challenge(&verifier));
    }

    #[test]
    fn init_and_complete_flow_single_use() {
        let flows: OAuthFlows = Arc::new(Mutex::new(HashMap::new()));
        let provider = test_provider();

        let (nonce, _url) = init_flow(
            &flows,
            "user1".into(),
            "my-skill".into(),
            &provider,
            "example".into(),
            "http://localhost/callback",
            None,
        );

        // First complete succeeds
        let state = complete_flow(&flows, &nonce).unwrap();
        assert_eq!(state.user_id, "user1");
        assert_eq!(state.skill_name, "my-skill");
        assert_eq!(state.provider, "example");
        assert_eq!(state.scopes, "read,write");

        // Second complete fails (single-use)
        assert!(matches!(
            complete_flow(&flows, &nonce),
            Err(OAuthError::FlowNotFound)
        ));
    }

    #[test]
    fn expired_flow_returns_error() {
        let flows: OAuthFlows = Arc::new(Mutex::new(HashMap::new()));
        let nonce = "test-nonce".to_string();

        // Insert already-expired flow
        flows.lock().unwrap().insert(
            nonce.clone(),
            OAuthFlowState {
                user_id: "u1".into(),
                skill_name: "sk".into(),
                provider: "p".into(),
                code_verifier: "v".into(),
                scopes: "s".into(),
                expires_at: 0, // expired
                created_at: 0,
            },
        );

        assert!(matches!(
            complete_flow(&flows, &nonce),
            Err(OAuthError::FlowExpired)
        ));
    }

    #[test]
    fn unknown_nonce_returns_not_found() {
        let flows: OAuthFlows = Arc::new(Mutex::new(HashMap::new()));
        assert!(matches!(
            complete_flow(&flows, "nonexistent"),
            Err(OAuthError::FlowNotFound)
        ));
    }

    #[test]
    fn build_authorization_url_includes_required_params() {
        let provider = ProviderConfig {
            client_id: "cid".into(),
            client_secret: "csec".into(),
            auth_url: "https://auth.example.com/authorize".into(),
            token_url: "https://auth.example.com/token".into(),
            default_scopes: "default".into(),
            extra_auth_params: vec![("access_type".into(), "offline".into())],
        };

        let url = build_authorization_url(
            &provider,
            "http://localhost/cb",
            "my-state",
            "my-challenge",
            "repo,user",
        );

        let parsed = url::Url::parse(&url).unwrap();
        let params: HashMap<_, _> = parsed.query_pairs().collect();

        assert_eq!(params.get("client_id").unwrap(), "cid");
        assert_eq!(params.get("redirect_uri").unwrap(), "http://localhost/cb");
        assert_eq!(params.get("response_type").unwrap(), "code");
        assert_eq!(params.get("state").unwrap(), "my-state");
        assert_eq!(params.get("code_challenge").unwrap(), "my-challenge");
        assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(params.get("scope").unwrap(), "repo,user");
        assert_eq!(params.get("access_type").unwrap(), "offline");
    }

    #[tokio::test]
    async fn mint_service_account_token_default_returns_error() {
        use credential_store::TokenRefresher;

        struct MinimalRefresher;

        #[async_trait]
        impl TokenRefresher for MinimalRefresher {
            async fn refresh_token(
                &self,
                _provider: &str,
                _refresh_token: &str,
            ) -> credential_store::error::Result<credential_store::TokenResponse> {
                unreachable!()
            }
        }

        let refresher = MinimalRefresher;
        let result = refresher
            .mint_service_account_token("key", "email", "uri", "scopes")
            .await;

        assert!(matches!(
            result,
            Err(credential_store::CredentialStoreError::ServiceAccountError(
                _
            ))
        ));
    }
}

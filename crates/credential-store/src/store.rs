use sqlx::SqlitePool;

use crate::crypto;
use crate::error::{CredentialStoreError, Result};
use crate::refresh::{is_expired, TokenRefresher};
use crate::types::{
    Credential, CredentialRow, CredentialSummary, CredentialType, ServiceAccountMetadata,
};

/// Stateless credential store — all operations take a pool and master key.
pub struct CredentialStore;

impl CredentialStore {
    /// Encrypt and store a credential, replacing any existing one for the same
    /// (user_id, skill_name, provider) triple.
    #[allow(clippy::too_many_arguments)]
    pub async fn store(
        pool: &SqlitePool,
        master_key: &[u8; 32],
        user_id: &str,
        skill_name: &str,
        provider: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: Option<i64>,
        scopes: &str,
    ) -> Result<()> {
        let salt = crypto::generate_salt();
        let key = crypto::derive_key(master_key, &salt, provider)?;

        let access_token_enc = crypto::encrypt(&key, access_token.as_bytes())?;
        let refresh_token_enc = match refresh_token {
            Some(rt) => Some(crypto::encrypt(&key, rt.as_bytes())?),
            None => None,
        };

        let now = unix_now();

        let salt_vec = salt.to_vec();

        sqlx::query(
            "INSERT OR REPLACE INTO credentials \
             (user_id, skill_name, provider, access_token_enc, refresh_token_enc, \
              expires_at, scopes, user_salt, created_at, updated_at, \
              credential_type, metadata_enc) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'oauth', NULL)",
        )
        .bind(user_id)
        .bind(skill_name)
        .bind(provider)
        .bind(&access_token_enc)
        .bind(&refresh_token_enc)
        .bind(expires_at)
        .bind(scopes)
        .bind(&salt_vec)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Encrypt and store a service account credential.
    /// The private key PEM is stored encrypted in `access_token_enc`.
    /// Metadata (client_email, token_uri) is serialized as JSON and stored encrypted in `metadata_enc`.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_service_account(
        pool: &SqlitePool,
        master_key: &[u8; 32],
        user_id: &str,
        skill_name: &str,
        provider: &str,
        private_key_pem: &str,
        metadata: &ServiceAccountMetadata,
        scopes: &str,
    ) -> Result<()> {
        let salt = crypto::generate_salt();
        let key = crypto::derive_key(master_key, &salt, provider)?;

        let access_token_enc = crypto::encrypt(&key, private_key_pem.as_bytes())?;
        let metadata_json = serde_json::to_vec(metadata)
            .map_err(|e| CredentialStoreError::Encryption(e.to_string()))?;
        let metadata_enc = crypto::encrypt(&key, &metadata_json)?;

        let now = unix_now();
        let salt_vec = salt.to_vec();

        sqlx::query(
            "INSERT OR REPLACE INTO credentials \
             (user_id, skill_name, provider, access_token_enc, refresh_token_enc, \
              expires_at, scopes, user_salt, created_at, updated_at, \
              credential_type, metadata_enc) \
             VALUES (?, ?, ?, ?, NULL, NULL, ?, ?, ?, ?, 'service_account', ?)",
        )
        .bind(user_id)
        .bind(skill_name)
        .bind(provider)
        .bind(&access_token_enc)
        .bind(scopes)
        .bind(&salt_vec)
        .bind(now)
        .bind(now)
        .bind(&metadata_enc)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Load and decrypt a credential.
    pub async fn load(
        pool: &SqlitePool,
        master_key: &[u8; 32],
        user_id: &str,
        skill_name: &str,
        provider: &str,
    ) -> Result<Credential> {
        let row: CredentialRow = sqlx::query_as(
            "SELECT user_id, skill_name, provider, access_token_enc, refresh_token_enc, \
             expires_at, scopes, user_salt, created_at, updated_at, \
             credential_type, metadata_enc \
             FROM credentials WHERE user_id = ? AND skill_name = ? AND provider = ?",
        )
        .bind(user_id)
        .bind(skill_name)
        .bind(provider)
        .fetch_optional(pool)
        .await?
        .ok_or(CredentialStoreError::NotFound)?;

        decrypt_row(master_key, &row)
    }

    /// Load a credential, refreshing the token first if it's expired.
    /// For service accounts, always mints a fresh token (private keys don't expire).
    pub async fn load_and_refresh(
        pool: &SqlitePool,
        master_key: &[u8; 32],
        user_id: &str,
        skill_name: &str,
        provider: &str,
        refresher: &dyn TokenRefresher,
    ) -> Result<Credential> {
        let row: CredentialRow = sqlx::query_as(
            "SELECT user_id, skill_name, provider, access_token_enc, refresh_token_enc, \
             expires_at, scopes, user_salt, created_at, updated_at, \
             credential_type, metadata_enc \
             FROM credentials WHERE user_id = ? AND skill_name = ? AND provider = ?",
        )
        .bind(user_id)
        .bind(skill_name)
        .bind(provider)
        .fetch_optional(pool)
        .await?
        .ok_or(CredentialStoreError::NotFound)?;

        let now = unix_now();

        match row.credential_type.as_str() {
            "service_account" => {
                // Always mint fresh — private keys don't expire.
                // The stored access_token is actually the private key PEM.
                let cred = decrypt_row(master_key, &row)?;
                let meta = cred.metadata.as_ref().ok_or_else(|| {
                    CredentialStoreError::ServiceAccountError(
                        "service account credential missing metadata".into(),
                    )
                })?;
                let token = refresher
                    .mint_service_account_token(
                        &cred.access_token,
                        &meta.client_email,
                        &meta.token_uri,
                        &cred.scopes,
                    )
                    .await?;
                Ok(Credential {
                    access_token: token.access_token,
                    refresh_token: None,
                    user_id: row.user_id.clone(),
                    skill_name: row.skill_name.clone(),
                    provider: row.provider.clone(),
                    scopes: row.scopes.clone(),
                    expires_at: token.expires_in.map(|ei| now + ei),
                    credential_type: CredentialType::ServiceAccount,
                    metadata: cred.metadata.clone(),
                })
            }
            _ => {
                // OAuth flow — existing logic
                if !is_expired(row.expires_at, now) {
                    return decrypt_row(master_key, &row);
                }

                let cred = decrypt_row(master_key, &row)?;
                let rt = cred.refresh_token.as_deref().ok_or_else(|| {
                    CredentialStoreError::CredentialInvalid(
                        "token expired but no refresh token available".into(),
                    )
                })?;

                let token_resp = refresher.refresh_token(provider, rt).await?;

                let new_expires_at = token_resp.expires_in.map(|ei| now + ei);

                Self::store(
                    pool,
                    master_key,
                    user_id,
                    skill_name,
                    provider,
                    &token_resp.access_token,
                    token_resp.refresh_token.as_deref().or(Some(rt)),
                    new_expires_at,
                    &row.scopes,
                )
                .await?;

                Self::load(pool, master_key, user_id, skill_name, provider).await
            }
        }
    }

    /// Delete a credential.
    pub async fn delete(
        pool: &SqlitePool,
        user_id: &str,
        skill_name: &str,
        provider: &str,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM credentials WHERE user_id = ? AND skill_name = ? AND provider = ?",
        )
        .bind(user_id)
        .bind(skill_name)
        .bind(provider)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// List credential summaries (metadata only, no decryption).
    pub async fn list(pool: &SqlitePool, user_id: &str) -> Result<Vec<CredentialSummary>> {
        let rows: Vec<(String, String, String, Option<i64>, String)> = sqlx::query_as(
            "SELECT skill_name, provider, scopes, expires_at, credential_type \
             FROM credentials WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(skill_name, provider, scopes, expires_at, credential_type)| CredentialSummary {
                    skill_name,
                    provider,
                    scopes,
                    expires_at,
                    credential_type,
                },
            )
            .collect())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs() as i64
}

fn decrypt_row(master_key: &[u8; 32], row: &CredentialRow) -> Result<Credential> {
    let salt: [u8; 32] = row
        .user_salt
        .as_slice()
        .try_into()
        .map_err(|_| CredentialStoreError::Decryption("invalid salt length".into()))?;

    let key = crypto::derive_key(master_key, &salt, &row.provider)?;

    let access_token = String::from_utf8(crypto::decrypt(&key, &row.access_token_enc)?)
        .map_err(|e| CredentialStoreError::Decryption(e.to_string()))?;

    let refresh_token = match &row.refresh_token_enc {
        Some(blob) => Some(
            String::from_utf8(crypto::decrypt(&key, blob)?)
                .map_err(|e| CredentialStoreError::Decryption(e.to_string()))?,
        ),
        None => None,
    };

    let credential_type = match row.credential_type.as_str() {
        "service_account" => CredentialType::ServiceAccount,
        _ => CredentialType::OAuth,
    };

    let metadata = match &row.metadata_enc {
        Some(blob) => {
            let decrypted = crypto::decrypt(&key, blob)?;
            let meta: ServiceAccountMetadata = serde_json::from_slice(&decrypted)
                .map_err(|e| CredentialStoreError::Decryption(e.to_string()))?;
            Some(meta)
        }
        None => None,
    };

    Ok(Credential {
        access_token,
        refresh_token,
        user_id: row.user_id.clone(),
        skill_name: row.skill_name.clone(),
        provider: row.provider.clone(),
        scopes: row.scopes.clone(),
        expires_at: row.expires_at,
        credential_type,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenResponse;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    const TEST_SCHEMA: &str = "\
        CREATE TABLE IF NOT EXISTS users (\
            id TEXT PRIMARY KEY,\
            created_at INTEGER NOT NULL\
        );\
        CREATE TABLE IF NOT EXISTS credentials (\
            user_id           TEXT    NOT NULL REFERENCES users(id),\
            skill_name        TEXT    NOT NULL,\
            provider          TEXT    NOT NULL,\
            access_token_enc  BLOB    NOT NULL,\
            refresh_token_enc BLOB,\
            expires_at        INTEGER,\
            scopes            TEXT    NOT NULL,\
            user_salt         BLOB    NOT NULL,\
            created_at        INTEGER NOT NULL,\
            updated_at        INTEGER NOT NULL,\
            credential_type   TEXT    NOT NULL DEFAULT 'oauth',\
            metadata_enc      BLOB,\
            PRIMARY KEY (user_id, skill_name, provider)\
        );";

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(TEST_SCHEMA).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO users (id, created_at) VALUES ('u1', 0)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    fn test_master_key() -> [u8; 32] {
        [0xAA; 32]
    }

    struct MockRefresher {
        should_fail: bool,
        called: AtomicBool,
    }

    impl MockRefresher {
        fn new(should_fail: bool) -> Self {
            Self {
                should_fail,
                called: AtomicBool::new(false),
            }
        }

        fn was_called(&self) -> bool {
            self.called.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl TokenRefresher for MockRefresher {
        async fn refresh_token(
            &self,
            _provider: &str,
            _refresh_token: &str,
        ) -> Result<TokenResponse> {
            self.called.store(true, Ordering::SeqCst);
            if self.should_fail {
                Err(CredentialStoreError::RefreshFailed("mock failure".into()))
            } else {
                Ok(TokenResponse {
                    access_token: "new-access-token".into(),
                    refresh_token: Some("new-refresh-token".into()),
                    expires_in: Some(3600),
                })
            }
        }
    }

    #[tokio::test]
    async fn store_and_load_round_trip() {
        let pool = test_pool().await;
        let mk = test_master_key();

        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "github-skill",
            "github",
            "ghp_access123",
            Some("ghp_refresh456"),
            Some(9999999999),
            "repo,user",
        )
        .await
        .unwrap();

        let cred = CredentialStore::load(&pool, &mk, "u1", "github-skill", "github")
            .await
            .unwrap();

        assert_eq!(cred.access_token, "ghp_access123");
        assert_eq!(cred.refresh_token.as_deref(), Some("ghp_refresh456"));
        assert_eq!(cred.scopes, "repo,user");
        assert_eq!(cred.expires_at, Some(9999999999));
    }

    #[tokio::test]
    async fn load_nonexistent_returns_not_found() {
        let pool = test_pool().await;
        let mk = test_master_key();

        let result = CredentialStore::load(&pool, &mk, "u1", "nope", "nope").await;
        assert!(matches!(result, Err(CredentialStoreError::NotFound)));
    }

    #[tokio::test]
    async fn store_overwrites_existing() {
        let pool = test_pool().await;
        let mk = test_master_key();

        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk",
            "prov",
            "old-token",
            None,
            None,
            "scope1",
        )
        .await
        .unwrap();

        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk",
            "prov",
            "new-token",
            None,
            None,
            "scope2",
        )
        .await
        .unwrap();

        let cred = CredentialStore::load(&pool, &mk, "u1", "sk", "prov")
            .await
            .unwrap();
        assert_eq!(cred.access_token, "new-token");
        assert_eq!(cred.scopes, "scope2");
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let pool = test_pool().await;
        let mk = test_master_key();

        CredentialStore::store(&pool, &mk, "u1", "sk", "prov", "tok", None, None, "s")
            .await
            .unwrap();

        CredentialStore::delete(&pool, "u1", "sk", "prov")
            .await
            .unwrap();

        let result = CredentialStore::load(&pool, &mk, "u1", "sk", "prov").await;
        assert!(matches!(result, Err(CredentialStoreError::NotFound)));
    }

    #[tokio::test]
    async fn list_returns_summaries_without_tokens() {
        let pool = test_pool().await;
        let mk = test_master_key();

        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk1",
            "github",
            "tok1",
            None,
            Some(100),
            "repo",
        )
        .await
        .unwrap();

        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk2",
            "slack",
            "tok2",
            None,
            None,
            "chat:write",
        )
        .await
        .unwrap();

        let summaries = CredentialStore::list(&pool, "u1").await.unwrap();
        assert_eq!(summaries.len(), 2);
        // Summaries contain metadata, not tokens
        assert!(summaries
            .iter()
            .any(|s| s.skill_name == "sk1" && s.provider == "github"));
        assert!(summaries
            .iter()
            .any(|s| s.skill_name == "sk2" && s.provider == "slack"));
    }

    #[tokio::test]
    async fn load_and_refresh_not_expired_skips_refresh() {
        let pool = test_pool().await;
        let mk = test_master_key();

        // Store with far-future expiry
        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk",
            "prov",
            "valid-token",
            Some("rt"),
            Some(9999999999),
            "s",
        )
        .await
        .unwrap();

        let refresher = MockRefresher::new(false);
        let cred = CredentialStore::load_and_refresh(&pool, &mk, "u1", "sk", "prov", &refresher)
            .await
            .unwrap();

        assert_eq!(cred.access_token, "valid-token");
        assert!(!refresher.was_called());
    }

    #[tokio::test]
    async fn load_and_refresh_expired_refreshes_successfully() {
        let pool = test_pool().await;
        let mk = test_master_key();

        // Store with past expiry
        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk",
            "prov",
            "expired-token",
            Some("rt"),
            Some(0),
            "s",
        )
        .await
        .unwrap();

        let refresher = MockRefresher::new(false);
        let cred = CredentialStore::load_and_refresh(&pool, &mk, "u1", "sk", "prov", &refresher)
            .await
            .unwrap();

        assert_eq!(cred.access_token, "new-access-token");
        assert_eq!(cred.refresh_token.as_deref(), Some("new-refresh-token"));
        assert!(refresher.was_called());
    }

    #[tokio::test]
    async fn load_and_refresh_expired_refresh_fails() {
        let pool = test_pool().await;
        let mk = test_master_key();

        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk",
            "prov",
            "expired-token",
            Some("rt"),
            Some(0),
            "s",
        )
        .await
        .unwrap();

        let refresher = MockRefresher::new(true);
        let result =
            CredentialStore::load_and_refresh(&pool, &mk, "u1", "sk", "prov", &refresher).await;

        assert!(matches!(
            result,
            Err(CredentialStoreError::RefreshFailed(_))
        ));
    }

    #[tokio::test]
    async fn load_and_refresh_expired_no_refresh_token() {
        let pool = test_pool().await;
        let mk = test_master_key();

        // Store expired credential with no refresh token
        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "sk",
            "prov",
            "expired-token",
            None,
            Some(0),
            "s",
        )
        .await
        .unwrap();

        let refresher = MockRefresher::new(false);
        let result =
            CredentialStore::load_and_refresh(&pool, &mk, "u1", "sk", "prov", &refresher).await;

        assert!(matches!(
            result,
            Err(CredentialStoreError::CredentialInvalid(_))
        ));
        assert!(!refresher.was_called());
    }

    #[tokio::test]
    async fn store_service_account_and_load() {
        let pool = test_pool().await;
        let mk = test_master_key();

        let metadata = ServiceAccountMetadata {
            client_email: "test@project.iam.gserviceaccount.com".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        };

        CredentialStore::store_service_account(
            &pool,
            &mk,
            "u1",
            "gmail-skill",
            "google",
            "-----BEGIN RSA PRIVATE KEY-----\nfake-key\n-----END RSA PRIVATE KEY-----",
            &metadata,
            "https://www.googleapis.com/auth/gmail.readonly",
        )
        .await
        .unwrap();

        let cred = CredentialStore::load(&pool, &mk, "u1", "gmail-skill", "google")
            .await
            .unwrap();

        assert_eq!(cred.credential_type, CredentialType::ServiceAccount);
        assert!(cred.access_token.contains("fake-key"));
        assert!(cred.refresh_token.is_none());
        assert!(cred.expires_at.is_none());
        let meta = cred.metadata.as_ref().unwrap();
        assert_eq!(meta.client_email, "test@project.iam.gserviceaccount.com");
        assert_eq!(meta.token_uri, "https://oauth2.googleapis.com/token");
    }

    struct MockSaRefresher {
        mint_called: AtomicBool,
    }

    impl MockSaRefresher {
        fn new() -> Self {
            Self {
                mint_called: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl TokenRefresher for MockSaRefresher {
        async fn refresh_token(
            &self,
            _provider: &str,
            _refresh_token: &str,
        ) -> Result<TokenResponse> {
            Err(CredentialStoreError::RefreshFailed("not expected".into()))
        }

        async fn mint_service_account_token(
            &self,
            _private_key_pem: &str,
            _client_email: &str,
            _token_uri: &str,
            _scopes: &str,
        ) -> Result<TokenResponse> {
            self.mint_called.store(true, Ordering::SeqCst);
            Ok(TokenResponse {
                access_token: "minted-sa-token".into(),
                refresh_token: None,
                expires_in: Some(3600),
            })
        }
    }

    #[tokio::test]
    async fn load_and_refresh_service_account_mints_token() {
        let pool = test_pool().await;
        let mk = test_master_key();

        let metadata = ServiceAccountMetadata {
            client_email: "test@project.iam.gserviceaccount.com".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        };

        CredentialStore::store_service_account(
            &pool,
            &mk,
            "u1",
            "gmail-skill",
            "google",
            "-----BEGIN RSA PRIVATE KEY-----\nfake-key\n-----END RSA PRIVATE KEY-----",
            &metadata,
            "https://www.googleapis.com/auth/gmail.readonly",
        )
        .await
        .unwrap();

        let refresher = MockSaRefresher::new();
        let cred = CredentialStore::load_and_refresh(
            &pool,
            &mk,
            "u1",
            "gmail-skill",
            "google",
            &refresher,
        )
        .await
        .unwrap();

        // The returned access_token should be the minted token, not the private key
        assert_eq!(cred.access_token, "minted-sa-token");
        assert_eq!(cred.credential_type, CredentialType::ServiceAccount);
        assert!(cred.expires_at.is_some());
        assert!(refresher.mint_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn list_includes_credential_type() {
        let pool = test_pool().await;
        let mk = test_master_key();

        // Store an OAuth credential
        CredentialStore::store(
            &pool,
            &mk,
            "u1",
            "github-skill",
            "github",
            "tok",
            None,
            None,
            "repo",
        )
        .await
        .unwrap();

        // Store a service account credential
        let metadata = ServiceAccountMetadata {
            client_email: "test@project.iam.gserviceaccount.com".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        };
        CredentialStore::store_service_account(
            &pool,
            &mk,
            "u1",
            "gmail-skill",
            "google",
            "key",
            &metadata,
            "gmail",
        )
        .await
        .unwrap();

        let summaries = CredentialStore::list(&pool, "u1").await.unwrap();
        assert_eq!(summaries.len(), 2);

        let oauth = summaries
            .iter()
            .find(|s| s.skill_name == "github-skill")
            .unwrap();
        assert_eq!(oauth.credential_type, "oauth");

        let sa = summaries
            .iter()
            .find(|s| s.skill_name == "gmail-skill")
            .unwrap();
        assert_eq!(sa.credential_type, "service_account");
    }
}

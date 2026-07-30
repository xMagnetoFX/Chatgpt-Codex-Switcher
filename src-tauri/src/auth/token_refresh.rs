//! ChatGPT OAuth token refresh helpers

use anyhow::{Context, Result};
use base64::Engine;
use chrono::Utc;
use futures::future::BoxFuture;
use tokio::time::{sleep, Duration};

use super::update_account_chatgpt_tokens_after_refresh;
use crate::types::{AuthData, StoredAccount};

const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const EXPIRY_SKEW_SECONDS: i64 = 60;

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct RefreshTokenResponse {
    #[serde(default)]
    pub(crate) id_token: Option<String>,
    pub(crate) access_token: String,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
}

pub(crate) trait ChatGptTokenRefreshClient: Send + Sync {
    fn refresh<'a>(&'a self, refresh_token: &'a str)
        -> BoxFuture<'a, Result<RefreshTokenResponse>>;
}

pub(crate) struct HttpChatGptTokenRefreshClient;

impl ChatGptTokenRefreshClient for HttpChatGptTokenRefreshClient {
    fn refresh<'a>(
        &'a self,
        refresh_token: &'a str,
    ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
        Box::pin(refresh_tokens_with_refresh_token(refresh_token))
    }
}

/// Ensure the account has a non-expired ChatGPT access token.
/// Returns an updated account when a refresh was performed.
pub async fn ensure_chatgpt_tokens_fresh(account: &StoredAccount) -> Result<StoredAccount> {
    ensure_chatgpt_tokens_fresh_with_client(account, &HttpChatGptTokenRefreshClient).await
}

pub(crate) async fn ensure_chatgpt_tokens_fresh_with_client<C>(
    account: &StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    ensure_chatgpt_tokens_fresh_with_client_and_sync(account, client, true).await
}

pub(crate) async fn ensure_chatgpt_tokens_fresh_for_activation_with_client<C>(
    account: &StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    ensure_chatgpt_tokens_fresh_with_client_and_sync(account, client, false).await
}

async fn ensure_chatgpt_tokens_fresh_with_client_and_sync<C>(
    account: &StoredAccount,
    client: &C,
    sync_active_auth: bool,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    match &account.auth_data {
        AuthData::ApiKey { .. } => Ok(account.clone()),
        AuthData::ChatGPT { access_token, .. } => {
            if token_expired_or_near_expiry(access_token) {
                println!(
                    "[Auth] Access token expired/near expiry for account {}, refreshing",
                    account.name
                );
                refresh_chatgpt_tokens_with_client_and_sync(account, client, sync_active_auth).await
            } else {
                Ok(account.clone())
            }
        }
    }
}

/// Force-refresh ChatGPT OAuth tokens for an account.
pub async fn refresh_chatgpt_tokens(account: &StoredAccount) -> Result<StoredAccount> {
    refresh_chatgpt_tokens_with_client(account, &HttpChatGptTokenRefreshClient).await
}

pub(crate) async fn refresh_chatgpt_tokens_with_client<C>(
    account: &StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    refresh_chatgpt_tokens_with_client_and_sync(account, client, true).await
}

async fn refresh_chatgpt_tokens_with_client_and_sync<C>(
    account: &StoredAccount,
    client: &C,
    sync_active_auth: bool,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let (current_id_token, current_refresh_token, current_account_id) = match &account.auth_data {
        AuthData::ApiKey { .. } => return Ok(account.clone()),
        AuthData::ChatGPT {
            id_token,
            refresh_token,
            account_id,
            ..
        } => (id_token.clone(), refresh_token.clone(), account_id.clone()),
    };

    if current_refresh_token.is_empty() {
        anyhow::bail!("Missing refresh token for account {}", account.name);
    }

    let refreshed = client.refresh(&current_refresh_token).await?;
    let next_id_token = refreshed.id_token.unwrap_or(current_id_token);
    let next_refresh_token = refreshed
        .refresh_token
        .unwrap_or_else(|| current_refresh_token.clone());

    let (email, plan_type, parsed_account_id) = parse_id_token_claims(&next_id_token);
    let next_account_id = parsed_account_id.or(current_account_id);

    let updated = update_account_chatgpt_tokens_after_refresh(
        &account.id,
        &current_refresh_token,
        next_id_token,
        refreshed.access_token,
        next_refresh_token,
        next_account_id,
        email,
        plan_type,
        sync_active_auth,
    )?;

    Ok(updated)
}

/// Build a new ChatGPT account from a refresh token.
/// This is used by slim import to recreate full credentials.
pub async fn create_chatgpt_account_from_refresh_token(
    account_name: String,
    refresh_token: String,
) -> Result<StoredAccount> {
    if refresh_token.trim().is_empty() {
        anyhow::bail!("Missing refresh token for account {account_name}");
    }

    let refreshed = refresh_tokens_with_refresh_token(&refresh_token).await?;
    let id_token = refreshed
        .id_token
        .context("Refresh response did not include id_token")?;
    let next_refresh_token = refreshed.refresh_token.unwrap_or(refresh_token);
    let (email, plan_type, account_id) = parse_id_token_claims(&id_token);

    Ok(StoredAccount::new_chatgpt(
        account_name,
        email,
        plan_type,
        id_token,
        refreshed.access_token,
        next_refresh_token,
        account_id,
    ))
}

fn token_expired_or_near_expiry(access_token: &str) -> bool {
    match parse_jwt_exp(access_token) {
        Some(expiry) => expiry <= Utc::now().timestamp() + EXPIRY_SKEW_SECONDS,
        None => true,
    }
}

fn parse_jwt_exp(token: &str) -> Option<i64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get("exp").and_then(|v| v.as_i64())
}

fn parse_id_token_claims(id_token: &str) -> (Option<String>, Option<String>, Option<String>) {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return (None, None, None);
    }

    let payload = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(bytes) => bytes,
        Err(_) => return (None, None, None),
    };

    let json: serde_json::Value = match serde_json::from_slice(&payload) {
        Ok(v) => v,
        Err(_) => return (None, None, None),
    };

    let email = json.get("email").and_then(|v| v.as_str()).map(String::from);
    let auth_claims = json.get("https://api.openai.com/auth");
    let plan_type = auth_claims
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let account_id = auth_claims
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    (email, plan_type, account_id)
}

async fn refresh_tokens_with_refresh_token(refresh_token: &str) -> Result<RefreshTokenResponse> {
    let client = reqwest::Client::new();
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencoding::encode(refresh_token),
        urlencoding::encode(CLIENT_ID),
    );

    let mut last_send_error = None;
    let mut response = None;

    for attempt in 1..=3u8 {
        match client
            .post(format!("{DEFAULT_ISSUER}/oauth/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.clone())
            .send()
            .await
        {
            Ok(resp) => {
                response = Some(resp);
                break;
            }
            Err(err) => {
                last_send_error = Some(err);
                if attempt < 3 {
                    sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                }
            }
        }
    }

    let response = match response {
        Some(resp) => resp,
        None => {
            let err = last_send_error.context("Failed to send token refresh request")?;
            return Err(err.into());
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!(
            "Token refresh failed with status {status}. Sign in again if this account is no longer authorized."
        );
    }

    response
        .json::<RefreshTokenResponse>()
        .await
        .context("Failed to parse token refresh response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::auth::{add_account, get_account, read_current_auth};

    enum FakeOutcome {
        Success(RefreshTokenResponse),
        Failure(&'static str),
    }

    struct FakeRefreshClient {
        calls: AtomicUsize,
        outcome: FakeOutcome,
    }

    impl FakeRefreshClient {
        fn success(response: RefreshTokenResponse) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                outcome: FakeOutcome::Success(response),
            }
        }

        fn failure(message: &'static str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                outcome: FakeOutcome::Failure(message),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ChatGptTokenRefreshClient for FakeRefreshClient {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                match &self.outcome {
                    FakeOutcome::Success(response) => Ok(response.clone()),
                    FakeOutcome::Failure(message) => anyhow::bail!(*message),
                }
            })
        }
    }

    struct TestEnv {
        _config_dir: tempfile::TempDir,
        _codex_home: tempfile::TempDir,
        old_config_dir: Option<String>,
        old_codex_home: Option<String>,
    }

    impl TestEnv {
        fn new() -> Self {
            let config_dir = tempfile::tempdir().expect("config temp dir");
            let codex_home = tempfile::tempdir().expect("codex temp dir");
            let old_config_dir = std::env::var("CODEX_SWITCHER_CONFIG_DIR").ok();
            let old_codex_home = std::env::var("CODEX_HOME").ok();
            std::env::set_var("CODEX_SWITCHER_CONFIG_DIR", config_dir.path());
            std::env::set_var("CODEX_HOME", codex_home.path());
            Self {
                _config_dir: config_dir,
                _codex_home: codex_home,
                old_config_dir,
                old_codex_home,
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            if let Some(value) = &self.old_config_dir {
                std::env::set_var("CODEX_SWITCHER_CONFIG_DIR", value);
            } else {
                std::env::remove_var("CODEX_SWITCHER_CONFIG_DIR");
            }
            if let Some(value) = &self.old_codex_home {
                std::env::set_var("CODEX_HOME", value);
            } else {
                std::env::remove_var("CODEX_HOME");
            }
        }
    }

    fn jwt_with_expiry(expiry: i64) -> String {
        let payload = serde_json::json!({ "exp": expiry });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize expiry"));
        format!("header.{encoded}.signature")
    }

    fn id_token(email: &str, account_id: &str) -> String {
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": "pro"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize claims"));
        format!("header.{encoded}.signature")
    }

    fn account_with_access_token(access_token: String) -> StoredAccount {
        StoredAccount::new_chatgpt_with_last_refresh(
            "ChatGPT".to_string(),
            Some("user@example.com".to_string()),
            Some("plus".to_string()),
            id_token("user@example.com", "acct-one"),
            access_token,
            "refresh-old".to_string(),
            Some("acct-one".to_string()),
            Some(Utc::now() - chrono::Duration::days(1)),
        )
    }

    #[test]
    fn detects_expired_near_expiry_and_unknown_access_tokens() {
        let now = Utc::now().timestamp();
        assert!(token_expired_or_near_expiry(&jwt_with_expiry(now - 1)));
        assert!(token_expired_or_near_expiry(&jwt_with_expiry(now + 30)));
        assert!(token_expired_or_near_expiry("not-a-jwt"));
        assert!(!token_expired_or_near_expiry(&jwt_with_expiry(now + 120)));
    }

    #[tokio::test]
    async fn fresh_access_token_does_not_call_refresh_client() {
        let account = account_with_access_token(jwt_with_expiry(Utc::now().timestamp() + 3600));
        let client = FakeRefreshClient::failure("refresh must not be called");

        let result = ensure_chatgpt_tokens_fresh_with_client(&account, &client)
            .await
            .expect("fresh account should pass through");

        assert_eq!(client.call_count(), 0);
        assert_eq!(result.id, account.id);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn expired_access_token_refreshes_storage_and_active_auth() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = add_account(account_with_access_token(jwt_with_expiry(
            Utc::now().timestamp() - 60,
        )))
        .expect("add account");
        let client = FakeRefreshClient::success(RefreshTokenResponse {
            id_token: Some(id_token("user@example.com", "acct-one")),
            access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
            refresh_token: Some("refresh-new".to_string()),
        });

        let refreshed = ensure_chatgpt_tokens_fresh_with_client(&account, &client)
            .await
            .expect("refresh should succeed");

        assert_eq!(client.call_count(), 1);
        assert_eq!(refreshed.plan_type.as_deref(), Some("pro"));
        let stored = get_account(&account.id)
            .expect("load account")
            .expect("account should exist");
        assert!(matches!(
            stored.auth_data,
            AuthData::ChatGPT {
                refresh_token,
                last_refresh: Some(_),
                ..
            } if refresh_token == "refresh-new"
        ));
        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");
        assert_eq!(
            auth.tokens.expect("tokens should exist").refresh_token,
            "refresh-new"
        );
        assert!(auth.last_refresh.is_some());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn refresh_failure_leaves_storage_and_active_auth_unchanged() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = add_account(account_with_access_token(jwt_with_expiry(
            Utc::now().timestamp() - 60,
        )))
        .expect("add account");
        let auth_before = serde_json::to_value(
            read_current_auth()
                .expect("read auth")
                .expect("auth should exist"),
        )
        .expect("serialize auth");
        let client = FakeRefreshClient::failure("provider rejected refresh");

        let result = ensure_chatgpt_tokens_fresh_with_client(&account, &client).await;

        assert!(result.is_err());
        assert_eq!(client.call_count(), 1);
        let stored = get_account(&account.id)
            .expect("load account")
            .expect("account should exist");
        assert!(matches!(
            stored.auth_data,
            AuthData::ChatGPT { refresh_token, .. } if refresh_token == "refresh-old"
        ));
        let auth_after = serde_json::to_value(
            read_current_auth()
                .expect("read auth")
                .expect("auth should exist"),
        )
        .expect("serialize auth");
        assert_eq!(auth_after, auth_before);
    }
}

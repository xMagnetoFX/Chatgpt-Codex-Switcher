//! ChatGPT OAuth token refresh helpers

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};

use anyhow::{Context, Result};
use chrono::Utc;
use futures::future::BoxFuture;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::{sleep, Duration};

use super::storage::{
    chatgpt_credential_fingerprint, update_account_chatgpt_tokens_after_refresh, ChatGptTokenUpdate,
};
use crate::types::{AuthData, StoredAccount};

const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const EXPIRY_SKEW_SECONDS: i64 = 60;

type AccountRefreshMutex = AsyncMutex<()>;
static ACCOUNT_REFRESH_LOCKS: LazyLock<StdMutex<HashMap<String, Weak<AccountRefreshMutex>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

async fn acquire_account_refresh_lock(account_id: &str) -> OwnedMutexGuard<()> {
    let refresh_lock = {
        let mut locks = ACCOUNT_REFRESH_LOCKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(account_id).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(AccountRefreshMutex::new(()));
            locks.insert(account_id.to_string(), Arc::downgrade(&lock));
            lock
        }
    };
    refresh_lock.lock_owned().await
}

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

/// Ensure an account has a demonstrably fresh ChatGPT access token.
/// Inactive accounts update only the catalog. If the same account is currently
/// live in Codex, the refreshed credentials are committed to both files without
/// changing account identity.
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
    let _refresh_guard = acquire_account_refresh_lock(&account.id).await;
    let account = latest_catalog_account(account)?;
    ensure_catalog_account_fresh_with_client(account, client).await
}

pub(crate) async fn ensure_chatgpt_tokens_fresh_for_activation_with_client<C>(
    account: &StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let _refresh_guard = acquire_account_refresh_lock(&account.id).await;
    // Activation already captured and reconciled its expected live-auth snapshot.
    // Reconcile again only inside the refresh CAS commit, otherwise a later
    // process-guard failure could persist the target as active before activation.
    let account = current_catalog_account(account)?;
    ensure_catalog_account_fresh_with_client(account, client).await
}

async fn ensure_catalog_account_fresh_with_client<C>(
    account: StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    if !account_needs_refresh(&account)? {
        super::switcher::validate_stored_account_credentials(&account)?;
        return Ok(account);
    }

    println!(
        "[Auth] Credentials require refresh for account {}",
        account.name
    );
    let expected_credentials = chatgpt_credential_fingerprint(&account)?;
    let refreshed = refresh_detached_chatgpt_tokens_with_client(&account, client).await?;
    let (id_token, access_token, refresh_token, account_id, email, plan_type) =
        refreshed_catalog_fields(&refreshed)?;

    update_account_chatgpt_tokens_after_refresh(
        &account.id,
        &expected_credentials,
        ChatGptTokenUpdate {
            id_token,
            access_token,
            refresh_token,
            account_id,
            email,
            plan_type,
            last_refresh: Some(Utc::now()),
        },
    )
}

/// Force-refresh an account, updating live auth only for the same active identity.
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
    let _refresh_guard = acquire_account_refresh_lock(&account.id).await;
    let account = latest_catalog_account(account)?;
    let expected_credentials = chatgpt_credential_fingerprint(&account)?;
    let refreshed = refresh_detached_chatgpt_tokens_with_client(&account, client).await?;
    let (id_token, access_token, refresh_token, account_id, email, plan_type) =
        refreshed_catalog_fields(&refreshed)?;

    update_account_chatgpt_tokens_after_refresh(
        &account.id,
        &expected_credentials,
        ChatGptTokenUpdate {
            id_token,
            access_token,
            refresh_token,
            account_id,
            email,
            plan_type,
            last_refresh: Some(Utc::now()),
        },
    )
}

pub(crate) async fn refresh_detached_chatgpt_tokens_with_client<C>(
    account: &StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let (current_id_token, current_refresh_token, current_account_id) = match &account.auth_data {
        AuthData::ApiKey { .. } => {
            super::switcher::validate_stored_account_credentials(account)?;
            return Ok(account.clone());
        }
        AuthData::ChatGPT {
            id_token,
            refresh_token,
            account_id,
            ..
        } => (id_token.clone(), refresh_token.clone(), account_id.clone()),
    };

    if current_refresh_token.trim().is_empty() {
        anyhow::bail!("Missing refresh token for account {}", account.name);
    }
    let current_identity = super::switcher::resolved_chatgpt_account_id(
        current_account_id.as_deref(),
        &current_id_token,
    )
    .with_context(|| {
        format!(
            "Account '{}' has conflicting ChatGPT account IDs",
            account.name
        )
    })?;

    let refreshed = client.refresh(&current_refresh_token).await?;
    if refreshed.access_token.trim().is_empty() {
        anyhow::bail!("Token refresh returned an empty access token");
    }
    if refreshed
        .refresh_token
        .as_ref()
        .is_some_and(|token| token.trim().is_empty())
    {
        anyhow::bail!("Token refresh returned an empty refresh token");
    }

    let next_id_token = refreshed.id_token.unwrap_or(current_id_token);
    let next_refresh_token = refreshed
        .refresh_token
        .unwrap_or_else(|| current_refresh_token.clone());
    if next_id_token.trim().is_empty() {
        anyhow::bail!("Token refresh did not provide an ID token");
    }
    if !super::switcher::jwt_payload_is_json(&next_id_token) {
        anyhow::bail!("Token refresh returned a malformed ID token");
    }
    if token_expired_or_near_expiry(&refreshed.access_token) {
        anyhow::bail!("Token refresh returned an expired or malformed access token");
    }

    let (email, plan_type) = super::switcher::parse_id_token_claims(&next_id_token);
    let refreshed_identity = super::switcher::resolved_chatgpt_account_id(None, &next_id_token)?;
    if current_identity.is_some()
        && refreshed_identity.is_some()
        && current_identity != refreshed_identity
    {
        anyhow::bail!("Token refresh returned credentials for a different ChatGPT account");
    }
    let next_account_id = current_identity.or(refreshed_identity);
    let mut updated = account.clone();
    updated.email = email.or(updated.email);
    updated.plan_type = plan_type.or(updated.plan_type);
    updated.auth_data = AuthData::ChatGPT {
        id_token: next_id_token,
        access_token: refreshed.access_token,
        refresh_token: next_refresh_token,
        account_id: next_account_id,
        last_refresh: Some(Utc::now()),
    };
    super::switcher::validate_stored_account_credentials(&updated)?;
    Ok(updated)
}

fn latest_catalog_account(account: &StoredAccount) -> Result<StoredAccount> {
    super::storage::reconcile_current_auth_catalog()?;
    current_catalog_account(account)
}

fn current_catalog_account(account: &StoredAccount) -> Result<StoredAccount> {
    Ok(super::storage::get_account(&account.id)?.unwrap_or_else(|| account.clone()))
}

fn account_needs_refresh(account: &StoredAccount) -> Result<bool> {
    match &account.auth_data {
        AuthData::ApiKey { key } => {
            if key.trim().is_empty() {
                anyhow::bail!("Missing API key for account {}", account.name);
            }
            Ok(false)
        }
        AuthData::ChatGPT {
            id_token,
            access_token,
            refresh_token,
            ..
        } => {
            if refresh_token.trim().is_empty() {
                anyhow::bail!("Missing refresh token for account {}", account.name);
            }
            Ok(id_token.trim().is_empty()
                || !super::switcher::jwt_payload_is_json(id_token)
                || access_token.trim().is_empty()
                || token_expired_or_near_expiry(access_token))
        }
    }
}

#[allow(clippy::type_complexity)]
fn refreshed_catalog_fields(
    account: &StoredAccount,
) -> Result<(
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
)> {
    let AuthData::ChatGPT {
        id_token,
        access_token,
        refresh_token,
        account_id,
        ..
    } = &account.auth_data
    else {
        anyhow::bail!("Account is not using ChatGPT OAuth");
    };

    Ok((
        id_token.clone(),
        access_token.clone(),
        refresh_token.clone(),
        account_id.clone(),
        account.email.clone(),
        account.plan_type.clone(),
    ))
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

    let placeholder = StoredAccount::new_chatgpt_with_last_refresh(
        account_name,
        None,
        None,
        String::new(),
        String::new(),
        refresh_token,
        None,
        None,
    );
    refresh_detached_chatgpt_tokens_with_client(&placeholder, &HttpChatGptTokenRefreshClient).await
}

fn token_expired_or_near_expiry(access_token: &str) -> bool {
    match super::switcher::jwt_expiration(access_token) {
        Some(expiry) => expiry <= Utc::now().timestamp() + EXPIRY_SKEW_SECONDS,
        None => true,
    }
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
    use base64::Engine as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::auth::switcher::{get_codex_auth_file, read_current_auth, write_auth_for_test};
    use crate::auth::{add_account, get_account};

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

    struct BlockingRefreshClient {
        calls: AtomicUsize,
        first_call_started: tokio::sync::Notify,
        second_call_started: tokio::sync::Notify,
        release_first_call: tokio::sync::Notify,
        response: RefreshTokenResponse,
    }

    impl ChatGptTokenRefreshClient for BlockingRefreshClient {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
            Box::pin(async move {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    self.first_call_started.notify_one();
                    self.release_first_call.notified().await;
                } else if call == 1 {
                    self.second_call_started.notify_one();
                }
                Ok(self.response.clone())
            })
        }
    }

    struct LiveUpdateDuringRefresh {
        live_account: StoredAccount,
        response: RefreshTokenResponse,
    }

    impl ChatGptTokenRefreshClient for LiveUpdateDuringRefresh {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
            Box::pin(async move {
                write_auth_for_test(&self.live_account)?;
                Ok(self.response.clone())
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

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn fresh_access_token_does_not_call_refresh_client() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
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
    async fn fresh_access_token_with_missing_refresh_token_is_rejected() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let mut account = account_with_access_token(jwt_with_expiry(Utc::now().timestamp() + 3600));
        if let AuthData::ChatGPT { refresh_token, .. } = &mut account.auth_data {
            *refresh_token = "   ".to_string();
        }
        let client = FakeRefreshClient::failure("refresh must not be called");

        let error = match ensure_chatgpt_tokens_fresh_with_client(&account, &client).await {
            Ok(_) => panic!("missing refresh token should fail"),
            Err(error) => error,
        };

        assert_eq!(client.call_count(), 0);
        assert!(error.to_string().contains("Missing refresh token"));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn expired_live_access_token_refreshes_catalog_and_same_identity_auth() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = add_account(account_with_access_token(jwt_with_expiry(
            Utc::now().timestamp() - 60,
        )))
        .expect("add account");
        write_auth_for_test(&account).expect("write live auth");
        let auth_path = get_codex_auth_file().expect("auth path");
        let auth_before = std::fs::read(&auth_path).expect("read live auth");
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
            &stored.auth_data,
            AuthData::ChatGPT {
                refresh_token,
                last_refresh: Some(_),
                ..
            } if refresh_token == "refresh-new"
        ));
        assert!(
            std::fs::read(&auth_path).expect("read live auth after refresh") != auth_before,
            "live auth should change after refresh"
        );
        let live = read_current_auth()
            .expect("read live auth")
            .expect("live auth should exist");
        assert!(matches!(
            live.tokens,
            Some(crate::types::TokenData { refresh_token, .. }) if refresh_token == "refresh-new"
        ));
        assert!(stored.previous_chatgpt_credential_hashes.len() == 1);

        crate::auth::storage::reconcile_current_auth_catalog()
            .expect("clear obsolete predecessor history");
        let reconciled = get_account(&account.id)
            .expect("load reconciled account")
            .expect("reconciled account should exist");
        assert!(reconciled.previous_chatgpt_credential_hashes.is_empty());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn concurrent_refreshes_for_one_account_are_serialized() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = add_account(account_with_access_token(jwt_with_expiry(
            Utc::now().timestamp() - 60,
        )))
        .expect("add account");
        let client = Arc::new(BlockingRefreshClient {
            calls: AtomicUsize::new(0),
            first_call_started: tokio::sync::Notify::new(),
            second_call_started: tokio::sync::Notify::new(),
            release_first_call: tokio::sync::Notify::new(),
            response: RefreshTokenResponse {
                id_token: Some(id_token("user@example.com", "acct-one")),
                access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
                refresh_token: Some("refresh-serialized".to_string()),
            },
        });

        let first_account = account.clone();
        let first_client = Arc::clone(&client);
        let first = tokio::spawn(async move {
            ensure_chatgpt_tokens_fresh_with_client(&first_account, first_client.as_ref()).await
        });
        client.first_call_started.notified().await;

        let second_account = account.clone();
        let second_client = Arc::clone(&client);
        let second = tokio::spawn(async move {
            ensure_chatgpt_tokens_fresh_with_client(&second_account, second_client.as_ref()).await
        });
        let second_reached_provider = tokio::time::timeout(
            Duration::from_millis(50),
            client.second_call_started.notified(),
        )
        .await
        .is_ok();
        client.release_first_call.notify_one();

        let first_result = first.await.expect("join first refresh");
        let second_result = second.await.expect("join second refresh");
        assert!(first_result.is_ok());
        assert!(second_result.is_ok());
        assert!(!second_reached_provider);
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn same_refresh_token_live_update_wins_over_in_flight_response() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = add_account(account_with_access_token(jwt_with_expiry(
            Utc::now().timestamp() - 60,
        )))
        .expect("add account");
        write_auth_for_test(&account).expect("write initial live auth");
        let mut external = account.clone();
        external.auth_data = AuthData::ChatGPT {
            id_token: id_token("user@example.com", "acct-one"),
            access_token: jwt_with_expiry(Utc::now().timestamp() + 1800),
            refresh_token: "refresh-old".to_string(),
            account_id: Some("acct-one".to_string()),
            last_refresh: Some(Utc::now()),
        };
        let client = LiveUpdateDuringRefresh {
            live_account: external.clone(),
            response: RefreshTokenResponse {
                id_token: Some(id_token("user@example.com", "acct-one")),
                access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
                refresh_token: None,
            },
        };

        let winner = ensure_chatgpt_tokens_fresh_with_client(&account, &client)
            .await
            .expect("external live update should win");

        let expected_access = match &external.auth_data {
            AuthData::ChatGPT { access_token, .. } => access_token,
            AuthData::ApiKey { .. } => unreachable!(),
        };
        assert!(matches!(
            winner.auth_data,
            AuthData::ChatGPT { ref access_token, .. } if access_token == expected_access
        ));
        let stored = get_account(&account.id)
            .expect("load account")
            .expect("account exists");
        assert!(matches!(
            stored.auth_data,
            AuthData::ChatGPT { ref access_token, .. } if access_token == expected_access
        ));
        let live = read_current_auth()
            .expect("read live auth")
            .expect("live auth exists");
        assert!(matches!(
            live.tokens,
            Some(crate::types::TokenData { ref access_token, .. })
                if access_token == expected_access
        ));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn legacy_live_account_without_timestamps_refreshes_both_files() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let mut legacy = account_with_access_token(jwt_with_expiry(Utc::now().timestamp() - 60));
        if let AuthData::ChatGPT { last_refresh, .. } = &mut legacy.auth_data {
            *last_refresh = None;
        }
        let legacy = add_account(legacy).expect("add legacy account");
        write_auth_for_test(&legacy).expect("write legacy live auth");
        let client = FakeRefreshClient::success(RefreshTokenResponse {
            id_token: Some(id_token("user@example.com", "acct-one")),
            access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
            refresh_token: Some("refresh-new".to_string()),
        });

        ensure_chatgpt_tokens_fresh_with_client(&legacy, &client)
            .await
            .expect("refresh legacy live account");

        let stored = get_account(&legacy.id)
            .expect("load account")
            .expect("account should exist");
        assert!(matches!(
            stored.auth_data,
            AuthData::ChatGPT {
                ref refresh_token,
                last_refresh: Some(_),
                ..
            } if refresh_token == "refresh-new"
        ));
        let live = read_current_auth()
            .expect("read live auth")
            .expect("live auth should exist");
        assert!(matches!(
            live.tokens,
            Some(crate::types::TokenData { refresh_token, .. }) if refresh_token == "refresh-new"
        ));
        assert!(live.last_refresh.is_some());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn failed_legacy_live_publication_is_repaired_from_the_catalog() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let mut legacy = account_with_access_token(jwt_with_expiry(Utc::now().timestamp() - 60));
        if let AuthData::ChatGPT {
            account_id,
            last_refresh,
            ..
        } = &mut legacy.auth_data
        {
            *account_id = None;
            *last_refresh = None;
        }
        let legacy = add_account(legacy).expect("add legacy account");
        write_auth_for_test(&legacy).expect("write legacy live auth");
        let client = FakeRefreshClient::success(RefreshTokenResponse {
            id_token: Some(id_token("user@example.com", "acct-one")),
            access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
            refresh_token: Some("refresh-repaired".to_string()),
        });
        std::env::set_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL", "publish_auth");

        let refresh_result = ensure_chatgpt_tokens_fresh_with_client(&legacy, &client).await;

        std::env::remove_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL");
        assert!(refresh_result.is_err());
        crate::auth::storage::reconcile_current_auth_catalog()
            .expect("repair live credentials from catalog");
        let live = read_current_auth()
            .expect("read repaired live auth")
            .expect("repaired live auth exists");
        assert!(matches!(
            live.tokens,
            Some(crate::types::TokenData { refresh_token, .. })
                if refresh_token == "refresh-repaired"
        ));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn expired_background_account_refresh_leaves_other_live_auth_unchanged() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let live = add_account(StoredAccount::new_api_key(
            "Live".to_string(),
            "sk-live".to_string(),
        ))
        .expect("add live account");
        write_auth_for_test(&live).expect("write live auth");
        let background = add_account(account_with_access_token(jwt_with_expiry(
            Utc::now().timestamp() - 60,
        )))
        .expect("add background account");
        let auth_path = get_codex_auth_file().expect("auth path");
        let auth_before = std::fs::read(&auth_path).expect("read live auth");
        let client = FakeRefreshClient::success(RefreshTokenResponse {
            id_token: Some(id_token("user@example.com", "acct-one")),
            access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
            refresh_token: Some("refresh-new".to_string()),
        });

        ensure_chatgpt_tokens_fresh_with_client(&background, &client)
            .await
            .expect("refresh background account");

        assert!(
            std::fs::read(auth_path).expect("read live auth after refresh") == auth_before,
            "background refresh must preserve live auth bytes"
        );
        let stored = get_account(&background.id)
            .expect("load background")
            .expect("background should exist");
        assert!(matches!(
            &stored.auth_data,
            AuthData::ChatGPT {
                refresh_token,
                ..
            } if refresh_token == "refresh-new"
        ));
        assert!(stored.previous_chatgpt_credential_hashes.is_empty());
    }

    #[tokio::test]
    async fn refresh_rejects_credentials_for_a_different_chatgpt_account() {
        let account = account_with_access_token(jwt_with_expiry(Utc::now().timestamp() - 60));
        let client = FakeRefreshClient::success(RefreshTokenResponse {
            id_token: Some(id_token("other@example.com", "acct-other")),
            access_token: jwt_with_expiry(Utc::now().timestamp() + 3600),
            refresh_token: Some("refresh-new".to_string()),
        });

        let error = match refresh_detached_chatgpt_tokens_with_client(&account, &client).await {
            Ok(_) => panic!("identity drift should fail"),
            Err(error) => error,
        };

        assert_eq!(client.call_count(), 1);
        assert!(error.to_string().contains("different ChatGPT account"));
        assert!(matches!(
            account.auth_data,
            AuthData::ChatGPT {
                ref refresh_token,
                ref account_id,
                ..
            } if refresh_token == "refresh-old" && account_id.as_deref() == Some("acct-one")
        ));
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
        write_auth_for_test(&account).expect("write live auth");
        let auth_path = get_codex_auth_file().expect("auth path");
        let auth_before = std::fs::read(&auth_path).expect("read live auth");
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
        assert!(
            std::fs::read(auth_path).expect("read live auth after failure") == auth_before,
            "failed refresh must preserve live auth bytes"
        );
    }
}

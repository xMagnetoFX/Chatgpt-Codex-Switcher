//! OAuth login Tauri commands

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

use crate::auth::oauth_server::{start_oauth_login, wait_for_oauth_login, OAuthLoginResult};
use crate::auth::{load_accounts, HttpChatGptTokenRefreshClient};
use crate::commands::activation::add_oauth_account_with_client;
use crate::types::{AccountInfo, OAuthLoginInfo};

struct PendingOAuth {
    rx: oneshot::Receiver<anyhow::Result<OAuthLoginResult>>,
    cancelled: Arc<AtomicBool>,
}

// Global state for pending OAuth login. `complete_login` takes the receiver
// out of PENDING_OAUTH while it waits, so the cancel flag of the current flow
// is kept separately in ACTIVE_CANCEL — otherwise cancelling mid-wait would
// find nothing to signal and the login could still complete after the user
// backed out.
static PENDING_OAUTH: Mutex<Option<PendingOAuth>> = Mutex::new(None);
static ACTIVE_CANCEL: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

fn signal_active_cancel() {
    if let Some(pending) = PENDING_OAUTH.lock().unwrap().take() {
        pending.cancelled.store(true, Ordering::Relaxed);
    }
    if let Some(flag) = ACTIVE_CANCEL.lock().unwrap().take() {
        flag.store(true, Ordering::Relaxed);
    }
}

fn clear_active_cancel_if_matches(flow_cancel: &Arc<AtomicBool>) {
    let mut active = ACTIVE_CANCEL.lock().unwrap();
    if active
        .as_ref()
        .is_some_and(|flag| Arc::ptr_eq(flag, flow_cancel))
    {
        *active = None;
    }
}

/// Start the OAuth login flow
#[tauri::command]
pub async fn start_login(account_name: Option<String>) -> Result<OAuthLoginInfo, String> {
    // Cancel any previous flow (pending or currently awaited) so it does not
    // keep the callback port occupied or complete behind the user's back.
    signal_active_cancel();

    let account_name = account_name.unwrap_or_default();
    let cancelled = Arc::new(AtomicBool::new(false));
    *ACTIVE_CANCEL.lock().unwrap() = Some(cancelled.clone());
    let start_result = start_oauth_login(account_name, cancelled.clone()).await;
    let (info, rx) = match start_result {
        Ok(started) => started,
        Err(error) => {
            clear_active_cancel_if_matches(&cancelled);
            return Err(error.to_string());
        }
    };
    if cancelled.load(Ordering::Relaxed) {
        clear_active_cancel_if_matches(&cancelled);
        return Err("OAuth login cancelled".to_string());
    }

    // Store the receiver for later
    {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        *pending = Some(PendingOAuth {
            rx,
            cancelled: cancelled.clone(),
        });
    }

    Ok(info)
}

/// Wait for the OAuth login to complete and add the account
#[tauri::command]
pub async fn complete_login() -> Result<AccountInfo, String> {
    complete_login_with_client(&HttpChatGptTokenRefreshClient).await
}

async fn complete_login_with_client<C>(client: &C) -> Result<AccountInfo, String>
where
    C: crate::auth::ChatGptTokenRefreshClient + ?Sized,
{
    let pending = {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        pending
            .take()
            .ok_or_else(|| "No pending OAuth login".to_string())?
    };
    let flow_cancel = pending.cancelled.clone();

    let account = match wait_for_oauth_login(pending.rx).await {
        Ok(account) => account,
        Err(error) => {
            clear_active_cancel_if_matches(&flow_cancel);
            return Err(error.to_string());
        }
    };
    if flow_cancel.load(Ordering::Relaxed) {
        clear_active_cancel_if_matches(&flow_cancel);
        return Err("OAuth login cancelled".to_string());
    }

    // Keep the cancellation flag reachable until the add-and-activate operation reaches its
    // guarded publication point. Closing the modal during persistence can then still abort it.
    let stored = add_oauth_account_with_client(account, client, &flow_cancel).await;
    clear_active_cancel_if_matches(&flow_cancel);
    let stored = stored.map_err(|error| format!("{error:#}"))?;

    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();

    Ok(AccountInfo::from_stored(&stored, active_id))
}

/// Cancel a pending OAuth login
#[tauri::command]
pub async fn cancel_login() -> Result<(), String> {
    signal_active_cancel();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use futures::future::BoxFuture;
    use tokio::sync::Notify;

    use crate::auth::{ChatGptTokenRefreshClient, RefreshTokenResponse};

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn cancelled_completed_flow_does_not_persist_the_account() {
        let _guard = crate::test_support::env_lock();
        signal_active_cancel();
        let (tx, rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(true));
        *PENDING_OAUTH.lock().unwrap() = Some(PendingOAuth {
            rx,
            cancelled: cancelled.clone(),
        });
        *ACTIVE_CANCEL.lock().unwrap() = Some(cancelled);
        assert!(tx
            .send(Ok(OAuthLoginResult {
                account: crate::types::StoredAccount::new_api_key(
                    "Cancelled".to_string(),
                    "sk-cancelled".to_string(),
                ),
            }))
            .is_ok());

        let result = complete_login().await;

        assert!(matches!(result, Err(ref error) if error.contains("cancelled")));
        assert!(PENDING_OAUTH.lock().unwrap().is_none());
        assert!(ACTIVE_CANCEL.lock().unwrap().is_none());
    }

    struct BlockingRefreshClient {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl ChatGptTokenRefreshClient for BlockingRefreshClient {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<RefreshTokenResponse>> {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(RefreshTokenResponse {
                    id_token: Some(test_id_token(chrono::Utc::now().timestamp() + 3600)),
                    access_token: test_access_token(chrono::Utc::now().timestamp() + 3600),
                    refresh_token: Some("refresh-rotated".to_string()),
                })
            })
        }
    }

    fn encode_jwt(payload: serde_json::Value) -> String {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize JWT payload"));
        format!("header.{encoded}.signature")
    }

    fn test_id_token(exp: i64) -> String {
        encode_jwt(serde_json::json!({
            "exp": exp,
            "email": "cancelled@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-cancelled",
                "chatgpt_plan_type": "plus"
            }
        }))
    }

    fn test_access_token(exp: i64) -> String {
        encode_jwt(serde_json::json!({ "exp": exp }))
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn cancellation_during_activation_removes_the_pending_oauth_account() {
        let _guard = crate::test_support::env_lock();
        signal_active_cancel();
        let config_dir = tempfile::tempdir().expect("config temp dir");
        let codex_home = tempfile::tempdir().expect("codex temp dir");
        let old_config = std::env::var("CODEX_SWITCHER_CONFIG_DIR").ok();
        let old_codex_home = std::env::var("CODEX_HOME").ok();
        let old_process_count = std::env::var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT").ok();
        std::env::set_var("CODEX_SWITCHER_CONFIG_DIR", config_dir.path());
        std::env::set_var("CODEX_HOME", codex_home.path());
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        let (tx, rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        *PENDING_OAUTH.lock().unwrap() = Some(PendingOAuth {
            rx,
            cancelled: cancelled.clone(),
        });
        *ACTIVE_CANCEL.lock().unwrap() = Some(cancelled);
        let account = crate::types::StoredAccount::new_chatgpt(
            "Cancelled".to_string(),
            Some("cancelled@example.com".to_string()),
            Some("plus".to_string()),
            test_id_token(chrono::Utc::now().timestamp() - 60),
            test_access_token(chrono::Utc::now().timestamp() - 60),
            "refresh-original".to_string(),
            Some("acct-cancelled".to_string()),
        );
        assert!(tx.send(Ok(OAuthLoginResult { account })).is_ok());

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let client = Arc::new(BlockingRefreshClient {
            started: started.clone(),
            release: release.clone(),
        });
        let completion =
            tokio::spawn(async move { complete_login_with_client(client.as_ref()).await });
        started.notified().await;
        cancel_login().await.expect("cancel login");
        release.notify_one();
        let result = completion.await.expect("join completion");

        assert!(result.is_err(), "cancellation must abort OAuth completion");
        assert!(matches!(result, Err(ref error) if error.contains("cancelled")));
        assert!(crate::auth::load_accounts()
            .expect("load catalog")
            .accounts
            .is_empty());
        assert!(!crate::auth::get_codex_auth_file()
            .expect("auth path")
            .exists());

        match old_config {
            Some(value) => std::env::set_var("CODEX_SWITCHER_CONFIG_DIR", value),
            None => std::env::remove_var("CODEX_SWITCHER_CONFIG_DIR"),
        }
        match old_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match old_process_count {
            Some(value) => std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", value),
            None => std::env::remove_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT"),
        }
    }
}

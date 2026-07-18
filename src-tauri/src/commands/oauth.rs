//! OAuth login Tauri commands

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

use crate::auth::oauth_server::{start_oauth_login, wait_for_oauth_login, OAuthLoginResult};
use crate::auth::{
    add_account, add_account_with_auto_name, load_accounts, set_active_account, touch_account,
};
use crate::types::{AccountInfo, OAuthLoginInfo};

const AUTO_ACCOUNT_NAME_PREFIX: &str = "AC";

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

/// Start the OAuth login flow
#[tauri::command]
pub async fn start_login(account_name: Option<String>) -> Result<OAuthLoginInfo, String> {
    // Cancel any previous flow (pending or currently awaited) so it does not
    // keep the callback port occupied or complete behind the user's back.
    signal_active_cancel();

    let account_name = account_name.unwrap_or_default();
    let (info, rx, cancelled) = start_oauth_login(account_name)
        .await
        .map_err(|e| e.to_string())?;

    // Store the receiver for later
    {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        *pending = Some(PendingOAuth {
            rx,
            cancelled: cancelled.clone(),
        });
    }
    *ACTIVE_CANCEL.lock().unwrap() = Some(cancelled);

    Ok(info)
}

/// Wait for the OAuth login to complete and add the account
#[tauri::command]
pub async fn complete_login() -> Result<AccountInfo, String> {
    let pending = {
        let mut pending = PENDING_OAUTH.lock().unwrap();
        pending
            .take()
            .ok_or_else(|| "No pending OAuth login".to_string())?
    };
    let flow_cancel = pending.cancelled.clone();

    let result = wait_for_oauth_login(pending.rx).await;

    // Drop the active cancel flag if it still belongs to this flow (a newer
    // start_login may have replaced it in the meantime).
    {
        let mut active = ACTIVE_CANCEL.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|flag| Arc::ptr_eq(flag, &flow_cancel))
        {
            *active = None;
        }
    }

    let account = result.map_err(|e| e.to_string())?;

    // Add the account to storage
    let stored = if account.name.trim().is_empty() {
        add_account_with_auto_name(account, AUTO_ACCOUNT_NAME_PREFIX)
    } else {
        add_account(account)
    }
    .map_err(|e| e.to_string())?;

    // Make it active and update last-used metadata.
    set_active_account(&stored.id).map_err(|e| e.to_string())?;
    touch_account(&stored.id).map_err(|e| e.to_string())?;

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

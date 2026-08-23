//! Usage query Tauri commands

use crate::api::usage::{
    get_account_usage, get_chatgpt_account_metadata, refresh_all_usage,
    warmup_account as send_warmup,
};
use crate::auth::storage::{update_account_metadata, update_account_subscription_metadata};
use crate::auth::{get_account, load_accounts};
use crate::types::{AccountInfo, AuthData, StoredAccount, UsageInfo, WarmupSummary};
use futures::{stream, StreamExt};

/// Get usage info for a specific account
#[tauri::command]
pub async fn get_usage(account_id: String) -> Result<UsageInfo, String> {
    let account = get_account(&account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    let usage = get_account_usage(&account)
        .await
        .map_err(|e| e.to_string())?;
    persist_live_plan_type(&account, &usage);
    Ok(usage)
}

/// Refresh the current ChatGPT plan and entitlement date for one account.
/// API key accounts have no ChatGPT subscription metadata and are returned unchanged.
#[tauri::command]
pub async fn refresh_account_metadata(account_id: String) -> Result<AccountInfo, String> {
    let account = get_account(&account_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    let updated = match &account.auth_data {
        AuthData::ApiKey { .. } => account,
        AuthData::ChatGPT { .. } => {
            let metadata = get_chatgpt_account_metadata(&account)
                .await
                .map_err(|error| error.to_string())?;
            update_account_subscription_metadata(
                &account_id,
                metadata.plan_type,
                metadata.subscription_expires_at,
            )
            .map_err(|error| error.to_string())?
        }
    };

    let store = load_accounts().map_err(|error| error.to_string())?;
    Ok(AccountInfo::from_stored(
        &updated,
        store.active_account_id.as_deref(),
    ))
}

/// Refresh usage info for all accounts
#[tauri::command]
pub async fn refresh_all_accounts_usage() -> Result<Vec<UsageInfo>, String> {
    let store = load_accounts().map_err(|e| e.to_string())?;
    let usage_results = refresh_all_usage(&store.accounts).await;
    for usage in &usage_results {
        if let Some(account) = store
            .accounts
            .iter()
            .find(|account| account.id == usage.account_id)
        {
            persist_live_plan_type(account, usage);
        }
    }
    Ok(usage_results)
}

fn persist_live_plan_type(account: &StoredAccount, usage: &UsageInfo) {
    if usage.error.is_some() {
        return;
    }

    let Some(plan_type) = usage
        .plan_type
        .as_deref()
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
    else {
        return;
    };

    if account.plan_type.as_deref() == Some(plan_type) {
        return;
    }

    if let Err(error) =
        update_account_metadata(&account.id, None, None, Some(plan_type.to_string()))
    {
        eprintln!(
            "[Usage] Failed to persist live plan type for {}: {error}",
            account.name
        );
    }
}

/// Send a minimal warm-up request for one account
#[tauri::command]
pub async fn warmup_account(account_id: String) -> Result<(), String> {
    let account = get_account(&account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    send_warmup(&account).await.map_err(|e| e.to_string())
}

/// Send minimal warm-up requests for all accounts
#[tauri::command]
pub async fn warmup_all_accounts() -> Result<WarmupSummary, String> {
    let store = load_accounts().map_err(|e| e.to_string())?;
    let total_accounts = store.accounts.len();
    let concurrency = total_accounts.clamp(1, 10);

    let results: Vec<(String, bool)> = stream::iter(store.accounts)
        .map(|account| async move {
            let account_id = account.id.clone();
            let failed = send_warmup(&account).await.is_err();
            (account_id, failed)
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let failed_account_ids = results
        .into_iter()
        .filter_map(|(account_id, failed)| failed.then_some(account_id))
        .collect::<Vec<_>>();

    let warmed_accounts = total_accounts.saturating_sub(failed_account_ids.len());
    Ok(WarmupSummary {
        total_accounts,
        warmed_accounts,
        failed_account_ids,
    })
}

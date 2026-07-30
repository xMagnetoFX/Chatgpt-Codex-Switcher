//! Account storage module - manages reading and writing accounts.json

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use sha2::{Digest, Sha256};

use super::atomic_file::{read_snapshot, stage_file_change, write_atomic, FileSnapshot};
use crate::types::{AccountsStore, AuthData, ImportAccountsSummary, StoredAccount};

const STORE_FILENAME: &str = "accounts.json";
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);
const REFRESH_RECONCILE_RETRIES: usize = 3;

/// Get the path to the codex-switcher config directory
pub fn get_config_dir() -> Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("CODEX_SWITCHER_CONFIG_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(".codex-switcher"))
}

/// Get the path to accounts.json
pub fn get_accounts_file() -> Result<PathBuf> {
    Ok(get_config_dir()?.join(STORE_FILENAME))
}

fn get_accounts_lock_file() -> Result<PathBuf> {
    Ok(get_config_dir()?.join(format!("{STORE_FILENAME}.lock")))
}

/// Load the accounts store from disk
pub fn load_accounts() -> Result<AccountsStore> {
    load_accounts_from_path(&get_accounts_file()?)
}

pub(crate) fn reconcile_current_auth_catalog() -> Result<super::switcher::ReconcileOutcome> {
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let accounts_before = capture_accounts_snapshot(&path)?;
    let mut store = load_accounts_from_path(&path)?;
    let auth_before = super::switcher::capture_auth_snapshot()?;
    let outcome = super::switcher::reconcile_live_auth(&mut store, &auth_before)?;
    super::switcher::validate_catalog_credential_uniqueness(&store)?;

    let desired_store = FileSnapshot::present(serialize_accounts_store(&store)?);
    let staged_accounts = if outcome.catalog_changed {
        Some(stage_file_change(
            &path,
            accounts_before.clone(),
            desired_store,
            "accounts",
        )?)
    } else {
        None
    };

    let expected_final_auth = if outcome.state == super::switcher::LiveAuthState::Stale {
        let account_id = outcome
            .matched_account_id
            .as_deref()
            .context("Stale live credentials did not identify an account")?;
        let account = store
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .context("Stale live account disappeared from the catalog")?;
        let desired_auth = super::switcher::auth_snapshot_for_account(account)?;
        super::switcher::stage_auth_publication(&auth_before, &desired_auth)?.commit()?;
        desired_auth
    } else {
        auth_before
    };

    if !super::switcher::auth_snapshot_matches_current(&expected_final_auth)? {
        anyhow::bail!(
            "Codex auth.json changed while live credentials were being reconciled; the account catalog was left unchanged"
        );
    }

    let published_store = staged_accounts.map(|staged| staged.commit()).transpose()?;
    if super::switcher::auth_snapshot_matches_current(&expected_final_auth)? {
        return Ok(outcome);
    }

    let Some(published_store) = published_store else {
        anyhow::bail!("Codex auth.json changed immediately after live credential reconciliation");
    };
    let rollback = stage_file_change(&path, published_store, accounts_before, "accounts")?.commit();
    match rollback {
        Ok(_) => anyhow::bail!(
            "Codex auth.json changed after live credential reconciliation; the account catalog was restored"
        ),
        Err(rollback_error) => anyhow::bail!(
            "Codex auth.json changed after live credential reconciliation, and catalog rollback was not applied: {rollback_error}"
        ),
    }
}

pub(crate) fn load_accounts_from_path(path: &Path) -> Result<AccountsStore> {
    if !path.exists() {
        return Ok(AccountsStore::default());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read accounts file: {}", path.display()))?;

    let store: AccountsStore = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse accounts file: {}", path.display()))?;

    Ok(store)
}

/// Save the account catalog without publishing Codex auth.json.
pub fn save_accounts(store: &AccountsStore) -> Result<()> {
    let _lock = acquire_store_lock()?;
    write_accounts_store_atomic(&get_accounts_file()?, store)
}

fn mutate_store<T, F>(mutator: F) -> Result<T>
where
    F: FnOnce(&mut AccountsStore) -> Result<T>,
{
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&path)?;
    let output = mutator(&mut store)?;
    write_accounts_store_atomic(&path, &store)?;
    Ok(output)
}

pub(crate) fn serialize_accounts_store(store: &AccountsStore) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(store).context("Failed to serialize accounts store")
}

pub(crate) fn capture_accounts_snapshot(path: &Path) -> Result<FileSnapshot> {
    read_snapshot(path)
}

pub(crate) fn write_accounts_store_atomic(path: &Path, store: &AccountsStore) -> Result<()> {
    let content = serialize_accounts_store(store)?;
    write_atomic(path, &content, "accounts")
}

pub(crate) struct StoreLock {
    _file: fs::File,
}

pub(crate) fn acquire_store_lock() -> Result<StoreLock> {
    let path = get_accounts_lock_file()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("Failed to open account store lock: {}", path.display()))?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(StoreLock { _file: file }),
            Err(error) if store_lock_is_contended(&error) => {
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "Timed out waiting for account store lock: {}",
                        path.display()
                    );
                }
                thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to lock account store: {}", path.display()));
            }
        }
    }
}

fn store_lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || cfg!(windows) && error.raw_os_error() == Some(33)
}

/// Add a new account to the store
pub fn add_account(account: StoredAccount) -> Result<StoredAccount> {
    let account_clone = account.clone();
    mutate_store(move |store| {
        if store
            .accounts
            .iter()
            .any(|existing| existing.name == account.name)
        {
            anyhow::bail!("An account with name '{}' already exists", account.name);
        }

        let becomes_active = store.accounts.is_empty();
        store.accounts.push(account);
        super::switcher::validate_catalog_credential_uniqueness(store)?;
        if becomes_active {
            store.active_account_id = Some(account_clone.id.clone());
        }

        Ok(account_clone)
    })
}

/// Add a new account and assign the next available display name using the given prefix.
pub fn add_account_with_auto_name(
    mut account: StoredAccount,
    prefix: &str,
) -> Result<StoredAccount> {
    mutate_store(move |store| {
        account.name = next_auto_account_name(store, prefix);
        let stored = account.clone();
        let becomes_active = store.accounts.is_empty();

        store.accounts.push(account);
        super::switcher::validate_catalog_credential_uniqueness(store)?;
        if becomes_active {
            store.active_account_id = Some(stored.id.clone());
        }

        Ok(stored)
    })
}

pub(crate) fn next_auto_account_name(store: &AccountsStore, prefix: &str) -> String {
    let trimmed_prefix = prefix.trim();
    let prefix = if trimmed_prefix.is_empty() {
        "Account"
    } else {
        trimmed_prefix
    };
    let existing_names = store
        .accounts
        .iter()
        .map(|account| account.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut index = 1usize;

    loop {
        let candidate = format!("{prefix} {index}");
        if !existing_names.contains(candidate.as_str()) {
            return candidate;
        }
        index += 1;
    }
}

/// Remove an account by ID
pub fn remove_account(account_id: &str) -> Result<()> {
    mutate_store(|store| {
        let initial_len = store.accounts.len();
        let removed_active = store.active_account_id.as_deref() == Some(account_id);
        store.accounts.retain(|account| account.id != account_id);

        if store.accounts.len() == initial_len {
            anyhow::bail!("Account not found: {account_id}");
        }

        if removed_active {
            store.active_account_id = store.accounts.first().map(|account| account.id.clone());
        }

        Ok(())
    })
}

/// Merge imported accounts into the local store, skipping duplicate ids and names.
pub fn merge_imported_accounts(imported: AccountsStore) -> Result<ImportAccountsSummary> {
    mutate_store(move |store| merge_accounts_store(store, imported))
}

pub(crate) fn merge_accounts_store(
    current: &mut AccountsStore,
    imported: AccountsStore,
) -> Result<ImportAccountsSummary> {
    let imported_version = imported.version;
    let imported_active_id = imported.active_account_id;
    let imported_masked_ids = imported.masked_account_ids;
    let total_in_payload = imported.accounts.len();
    let mut imported_count = 0usize;
    let mut existing_ids = current
        .accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut existing_names = current
        .accounts
        .iter()
        .map(|account| account.name.clone())
        .collect::<std::collections::HashSet<_>>();

    for account in imported.accounts {
        if existing_ids.contains(&account.id) || existing_names.contains(&account.name) {
            continue;
        }
        existing_ids.insert(account.id.clone());
        existing_names.insert(account.name.clone());
        current.accounts.push(account);
        imported_count += 1;
    }

    current.version = current.version.max(imported_version).max(1);

    let current_active_is_valid = current
        .active_account_id
        .as_ref()
        .is_some_and(|id| current.accounts.iter().any(|account| &account.id == id));

    if !current_active_is_valid {
        if let Some(imported_active) = imported_active_id {
            if current
                .accounts
                .iter()
                .any(|account| account.id == imported_active)
            {
                current.active_account_id = Some(imported_active);
            } else {
                current.active_account_id =
                    current.accounts.first().map(|account| account.id.clone());
            }
        } else {
            current.active_account_id = current.accounts.first().map(|account| account.id.clone());
        }
    }

    for masked_id in imported_masked_ids {
        if current
            .accounts
            .iter()
            .any(|account| account.id == masked_id)
            && !current.masked_account_ids.contains(&masked_id)
        {
            current.masked_account_ids.push(masked_id);
        }
    }

    super::switcher::validate_catalog_credential_uniqueness(current)?;
    Ok(ImportAccountsSummary {
        total_in_payload,
        imported_count,
        skipped_count: total_in_payload.saturating_sub(imported_count),
    })
}

/// Get an account by ID
pub fn get_account(account_id: &str) -> Result<Option<StoredAccount>> {
    let store = load_accounts()?;
    Ok(store
        .accounts
        .into_iter()
        .find(|account| account.id == account_id))
}

/// Get the currently active account
pub fn get_active_account() -> Result<Option<StoredAccount>> {
    let store = load_accounts()?;
    let Some(active_id) = &store.active_account_id else {
        return Ok(None);
    };
    Ok(store
        .accounts
        .into_iter()
        .find(|account| account.id == *active_id))
}

/// Update an account's last_used_at timestamp
pub fn touch_account(account_id: &str) -> Result<()> {
    mutate_store(|store| {
        if let Some(account) = store
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
        {
            account.last_used_at = Some(chrono::Utc::now());
        }
        Ok(())
    })
}

/// Update an account's metadata (name, email, plan_type)
pub fn update_account_metadata(
    account_id: &str,
    name: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
) -> Result<()> {
    mutate_store(|store| {
        if let Some(ref new_name) = name {
            if store
                .accounts
                .iter()
                .any(|account| account.id != account_id && account.name == *new_name)
            {
                anyhow::bail!("An account with name '{new_name}' already exists");
            }
        }

        let account = store
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
            .context("Account not found")?;

        if let Some(new_name) = name {
            account.name = new_name;
        }

        if email.is_some() {
            account.email = email;
        }

        if plan_type.is_some() {
            account.plan_type = plan_type;
        }

        Ok(())
    })
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ChatGptCredentialFingerprint([u8; 32]);

pub(crate) fn chatgpt_credential_fingerprint(
    account: &StoredAccount,
) -> Result<ChatGptCredentialFingerprint> {
    if !matches!(account.auth_data, AuthData::ChatGPT { .. }) {
        anyhow::bail!("Cannot fingerprint OAuth credentials for an API key account");
    }

    let encoded = serde_json::to_vec(&account.auth_data)
        .context("Failed to fingerprint ChatGPT credentials")?;
    Ok(ChatGptCredentialFingerprint(Sha256::digest(encoded).into()))
}

pub(crate) struct ChatGptTokenUpdate {
    pub(crate) id_token: String,
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) account_id: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) last_refresh: Option<DateTime<Utc>>,
}

/// Update freshly obtained ChatGPT OAuth tokens in the account catalog.
pub fn update_account_chatgpt_tokens(
    account_id: &str,
    id_token: String,
    access_token: String,
    refresh_token: String,
    chatgpt_account_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
) -> Result<StoredAccount> {
    update_account_chatgpt_tokens_with_last_refresh(
        account_id,
        None,
        ChatGptTokenUpdate {
            id_token,
            access_token,
            refresh_token,
            account_id: chatgpt_account_id,
            email,
            plan_type,
            last_refresh: Some(Utc::now()),
        },
    )
}

pub(crate) fn update_account_chatgpt_tokens_after_refresh(
    account_id: &str,
    expected_credentials: &ChatGptCredentialFingerprint,
    update: ChatGptTokenUpdate,
) -> Result<StoredAccount> {
    let _lock = acquire_store_lock()?;
    let accounts_path = get_accounts_file()?;
    let mut pending_update = Some(update);

    for _ in 0..REFRESH_RECONCILE_RETRIES {
        let accounts_before = capture_accounts_snapshot(&accounts_path)?;
        let auth_before = super::switcher::capture_auth_snapshot()?;
        let mut store = load_accounts_from_path(&accounts_path)?;
        let reconciliation = super::switcher::reconcile_live_auth(&mut store, &auth_before).ok();
        let live_account_id = reconciliation
            .as_ref()
            .and_then(|outcome| outcome.matched_account_id.as_deref());
        let account = store
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
            .context("Account not found")?;

        if chatgpt_credential_fingerprint(account)? != *expected_credentials {
            let winner = account.clone();
            super::switcher::validate_catalog_credential_uniqueness(&store)?;
            let desired_store = FileSnapshot::present(serialize_accounts_store(&store)?);
            let catalog_changed = desired_store != accounts_before;

            test_replace_auth_before_refresh_winner_commit()?;
            if live_account_id == Some(account_id)
                && !super::switcher::auth_snapshot_matches_current(&auth_before)?
            {
                continue;
            }
            if !catalog_changed {
                return Ok(winner);
            }

            let staged_accounts = stage_file_change(
                &accounts_path,
                accounts_before.clone(),
                desired_store,
                "accounts",
            )?;
            let published_store = staged_accounts.commit()?;
            if live_account_id != Some(account_id)
                || super::switcher::auth_snapshot_matches_current(&auth_before)?
            {
                return Ok(winner);
            }

            stage_file_change(
                &accounts_path,
                published_store,
                accounts_before,
                "accounts",
            )?
            .commit()
            .context(
                "Codex auth.json changed while refreshed credentials were being reconciled; the previous account catalog could not be restored",
            )?;
            continue;
        }

        let update = pending_update
            .take()
            .context("Refreshed credentials were already consumed")?;
        apply_chatgpt_token_update(account, update)?;
        let updated = account.clone();
        super::switcher::validate_catalog_credential_uniqueness(&store)?;
        let desired_store = FileSnapshot::present(serialize_accounts_store(&store)?);
        let staged_accounts =
            stage_file_change(&accounts_path, accounts_before, desired_store, "accounts")?;

        if live_account_id != Some(account_id) {
            staged_accounts.commit()?;
            return Ok(updated);
        }

        let desired_auth = super::switcher::auth_snapshot_for_account(&updated)?;
        let staged_auth = match super::switcher::stage_auth_publication(&auth_before, &desired_auth)
        {
            Ok(staged) => staged,
            Err(error) => {
                staged_accounts.commit()?;
                return Err(error).context(
                    "Failed to stage live credential refresh; the refreshed credentials were preserved in the account catalog",
                );
            }
        };

        // This refresh does not change account identity. Commit the catalog first so
        // a provider-rotated refresh token remains durable even if live publication
        // is interrupted or Codex updates auth.json concurrently.
        staged_accounts.commit()?;
        staged_auth.commit().with_context(|| {
            "Failed to publish refreshed live credentials; the refreshed credentials were preserved in the account catalog"
        })?;
        return Ok(updated);
    }

    anyhow::bail!("Codex auth.json kept changing while refreshed credentials were reconciled")
}

#[cfg(test)]
fn test_replace_auth_before_refresh_winner_commit() -> Result<()> {
    let Ok(contents) = std::env::var("CODEX_SWITCHER_TEST_REFRESH_WINNER_REPLACEMENT") else {
        return Ok(());
    };
    fs::write(super::switcher::get_codex_auth_file()?, contents)
        .context("Failed to inject a newer live credential generation")
}

#[cfg(not(test))]
fn test_replace_auth_before_refresh_winner_commit() -> Result<()> {
    Ok(())
}

fn update_account_chatgpt_tokens_with_last_refresh(
    account_id: &str,
    expected_refresh_token: Option<&str>,
    update: ChatGptTokenUpdate,
) -> Result<StoredAccount> {
    mutate_store(|store| {
        let updated = {
            let account = store
                .accounts
                .iter_mut()
                .find(|account| account.id == account_id)
                .context("Account not found")?;

            if let Some(expected) = expected_refresh_token {
                match &account.auth_data {
                    AuthData::ChatGPT { refresh_token, .. } if refresh_token == expected => {}
                    AuthData::ChatGPT { .. } => return Ok(account.clone()),
                    AuthData::ApiKey { .. } => {
                        anyhow::bail!("Cannot update OAuth tokens for an API key account");
                    }
                }
            }

            apply_chatgpt_token_update(account, update)?;
            account.clone()
        };
        super::switcher::validate_catalog_credential_uniqueness(store)?;
        Ok(updated)
    })
}

fn apply_chatgpt_token_update(
    account: &mut StoredAccount,
    update: ChatGptTokenUpdate,
) -> Result<()> {
    match &mut account.auth_data {
        AuthData::ChatGPT {
            id_token: stored_id_token,
            access_token: stored_access_token,
            refresh_token: stored_refresh_token,
            account_id: stored_account_id,
            last_refresh: stored_last_refresh,
        } => {
            *stored_id_token = update.id_token;
            *stored_access_token = update.access_token;
            *stored_refresh_token = update.refresh_token;
            if let Some(new_account_id) = update.account_id {
                *stored_account_id = Some(new_account_id);
            }
            *stored_last_refresh = update.last_refresh;
        }
        AuthData::ApiKey { .. } => {
            anyhow::bail!("Cannot update OAuth tokens for an API key account");
        }
    }

    if let Some(new_email) = update.email {
        account.email = Some(new_email);
    }
    if let Some(new_plan_type) = update.plan_type {
        account.plan_type = Some(new_plan_type);
    }
    super::switcher::validate_stored_account_credentials(account)
}

/// Get the list of masked account IDs
pub fn get_masked_account_ids() -> Result<Vec<String>> {
    let store = load_accounts()?;
    Ok(store.masked_account_ids.clone())
}

/// Set the list of masked account IDs
pub fn set_masked_account_ids(ids: Vec<String>) -> Result<()> {
    mutate_store(|store| {
        store.masked_account_ids = ids;
        Ok(())
    })
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;

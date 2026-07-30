//! Guarded account activation transactions.

use anyhow::{Context, Result};

use crate::auth::atomic_file::{stage_file_change, FileSnapshot};
use crate::auth::storage::{
    acquire_store_lock, capture_accounts_snapshot, get_account, get_accounts_file, load_accounts,
    load_accounts_from_path, merge_accounts_store, next_auto_account_name,
    serialize_accounts_store, write_accounts_store_atomic,
};
use crate::auth::{
    auth_snapshot_for_account, auth_snapshot_matches_current, capture_auth_snapshot,
    ensure_chatgpt_tokens_fresh_for_activation_with_client, missing_auth_snapshot,
    reconcile_live_auth, rollback_auth_publication, stage_auth_publication,
    validate_stored_account_credentials, AuthSnapshot, ChatGptTokenRefreshClient,
};
use crate::commands::process::{ensure_switch_allowed, restart_antigravity_background_processes};
use crate::types::{AccountsStore, ImportAccountsSummary, StoredAccount};

const AUTO_ACCOUNT_NAME_PREFIX: &str = "AC";

struct PreparedCatalog {
    auth_snapshot: AuthSnapshot,
    store: AccountsStore,
}

pub(crate) async fn activate_existing_account_with_client<C>(
    account_id: &str,
    client: &C,
) -> Result<()>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let prepared = prepare_catalog(true)?;
    let account = prepared
        .store
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .cloned()
        .with_context(|| format!("Account not found: {account_id}"))?;

    let fresh = ensure_chatgpt_tokens_fresh_for_activation_with_client(&account, client)
        .await
        .with_context(|| format!("Failed to refresh account '{}'", account.name))?;
    let activation_snapshot = capture_auth_snapshot()?;
    if activation_snapshot != prepared.auth_snapshot
        && activation_snapshot != auth_snapshot_for_account(&fresh)?
    {
        anyhow::bail!("Codex auth.json changed while the account was being refreshed");
    }

    let account_id = account_id.to_string();
    commit_activation(&activation_snapshot, move |store| {
        let account = store
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
            .with_context(|| format!("Account not found: {account_id}"))?;
        validate_stored_account_credentials(account)?;
        account.last_used_at = Some(chrono::Utc::now());
        let desired_auth = auth_snapshot_for_account(account)?;
        store.active_account_id = Some(account_id.clone());
        Ok((desired_auth, ()))
    })?;

    restart_antigravity_background_processes();
    Ok(())
}

pub(crate) async fn add_imported_account_with_client<C>(
    account: StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let mut account = match add_catalog_account_if_nonempty(account, AUTO_ACCOUNT_NAME_PREFIX)? {
        CatalogAddOutcome::Added(stored) => return Ok(stored),
        CatalogAddOutcome::NeedsActivation(account) => account,
    };

    if account.name.trim().is_empty() {
        account.name = next_auto_account_name(&AccountsStore::default(), AUTO_ACCOUNT_NAME_PREFIX);
    }
    add_pending_account(account, false, client).await
}

pub(crate) async fn add_oauth_account_with_client<C>(
    mut account: StoredAccount,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    if account.name.trim().is_empty() {
        account.name = next_auto_account_name(&load_accounts()?, AUTO_ACCOUNT_NAME_PREFIX);
    }
    add_pending_account(account, true, client).await
}

async fn add_pending_account<C>(
    account: StoredAccount,
    force_activate: bool,
    client: &C,
) -> Result<StoredAccount>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let auto_named = account.name.starts_with(AUTO_ACCOUNT_NAME_PREFIX)
        && account.name[AUTO_ACCOUNT_NAME_PREFIX.len()..]
            .trim()
            .parse::<usize>()
            .is_ok();
    let (stored, was_empty) =
        persist_catalog_account(account, auto_named.then_some(AUTO_ACCOUNT_NAME_PREFIX))?;

    if force_activate || was_empty {
        activate_existing_account_with_client(&stored.id, client)
            .await
            .with_context(|| {
                format!(
                    "Account '{}' was saved, but it could not be activated",
                    stored.name
                )
            })?;
    }

    Ok(get_account(&stored.id)?.unwrap_or(stored))
}

pub(crate) async fn merge_imported_accounts_with_client<C>(
    imported: AccountsStore,
    client: &C,
) -> Result<ImportAccountsSummary>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let imported = match merge_catalog_if_activation_not_needed(imported)? {
        CatalogMergeOutcome::Merged(summary) => return Ok(summary),
        CatalogMergeOutcome::NeedsActivation(imported) => imported,
    };

    let preferred_id = imported
        .active_account_id
        .clone()
        .or_else(|| imported.accounts.first().map(|account| account.id.clone()));
    let (summary, activation_target) = persist_imported_store_catalog_only(imported, preferred_id)?;
    if let Some(target_id) = activation_target {
        activate_existing_account_with_client(&target_id, client)
            .await
            .with_context(|| {
                "Imported accounts were saved, but the selected account could not be activated"
            })?;
    }
    Ok(summary)
}

pub(crate) async fn delete_account_with_client<C>(account_id: &str, client: &C) -> Result<()>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    if remove_catalog_account_if_inactive(account_id)? == CatalogDeleteOutcome::Removed {
        return Ok(());
    }

    let prepared = prepare_catalog(false)?;
    if !prepared
        .store
        .accounts
        .iter()
        .any(|account| account.id == account_id)
    {
        anyhow::bail!("Account not found: {account_id}");
    }
    if prepared.store.active_account_id.as_deref() != Some(account_id) {
        return match remove_catalog_account_if_inactive(account_id)? {
            CatalogDeleteOutcome::Removed => Ok(()),
            CatalogDeleteOutcome::NeedsActivation => {
                anyhow::bail!("The active account changed while preparing deletion; try again")
            }
        };
    }

    ensure_switch_allowed()?;
    let fallback = prepared
        .store
        .accounts
        .iter()
        .find(|account| account.id != account_id)
        .cloned();
    if let Some(fallback) = fallback.as_ref() {
        ensure_chatgpt_tokens_fresh_for_activation_with_client(fallback, client)
            .await
            .with_context(|| format!("Failed to refresh account '{}'", fallback.name))?;
    }
    let fallback_id = fallback.map(|account| account.id);

    let account_id = account_id.to_string();
    let preserved_auth = prepared.auth_snapshot.clone();
    commit_activation(&prepared.auth_snapshot, move |store| {
        let initial_len = store.accounts.len();
        let deleting_active = store.active_account_id.as_deref() == Some(account_id.as_str());
        store.accounts.retain(|account| account.id != account_id);
        if store.accounts.len() == initial_len {
            anyhow::bail!("Account not found: {account_id}");
        }
        if !deleting_active {
            return Ok((preserved_auth.clone(), ()));
        }

        let Some(fallback_id) = fallback_id.as_deref() else {
            if !store.accounts.is_empty() {
                anyhow::bail!("The account catalog changed while preparing the fallback account");
            }
            store.active_account_id = None;
            return Ok((missing_auth_snapshot(), ()));
        };
        let fallback = store
            .accounts
            .iter_mut()
            .find(|account| account.id == fallback_id)
            .context("The prepared fallback account is no longer available")?;
        validate_stored_account_credentials(fallback)?;
        fallback.last_used_at = Some(chrono::Utc::now());
        let fallback_id = fallback.id.clone();
        let desired_auth = auth_snapshot_for_account(fallback)?;
        store.active_account_id = Some(fallback_id);
        Ok((desired_auth, ()))
    })?;

    restart_antigravity_background_processes();
    Ok(())
}

#[derive(PartialEq, Eq)]
enum CatalogDeleteOutcome {
    Removed,
    NeedsActivation,
}

enum CatalogAddOutcome {
    Added(StoredAccount),
    NeedsActivation(StoredAccount),
}

enum CatalogMergeOutcome {
    Merged(ImportAccountsSummary),
    NeedsActivation(AccountsStore),
}

pub(crate) fn persist_imported_account_catalog_only(
    account: StoredAccount,
) -> Result<StoredAccount> {
    persist_catalog_account(account, None).map(|(stored, _)| stored)
}

fn persist_catalog_account(
    mut account: StoredAccount,
    auto_name_prefix: Option<&str>,
) -> Result<(StoredAccount, bool)> {
    validate_stored_account_credentials(&account)?;

    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&path)?;
    let was_empty = store.accounts.is_empty();
    if store
        .accounts
        .iter()
        .any(|existing| existing.id == account.id)
    {
        anyhow::bail!("An account with this ID already exists");
    }
    if store
        .accounts
        .iter()
        .any(|existing| existing.name == account.name)
    {
        if let Some(prefix) = auto_name_prefix {
            account.name = next_auto_account_name(&store, prefix);
        } else {
            anyhow::bail!("An account with name '{}' already exists", account.name);
        }
    }

    store.accounts.push(account.clone());
    crate::auth::validate_catalog_credential_uniqueness(&store)?;
    write_accounts_store_atomic(&path, &store)?;
    Ok((account, was_empty))
}

fn persist_imported_store_catalog_only(
    imported: AccountsStore,
    preferred_id: Option<String>,
) -> Result<(ImportAccountsSummary, Option<String>)> {
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&path)?;
    let was_empty = store.accounts.is_empty();
    let previous_active = store.active_account_id.clone();
    let summary = merge_accounts_store(&mut store, imported)?;
    store.active_account_id = previous_active;

    let activation_target = if was_empty && summary.imported_count > 0 {
        preferred_id
            .filter(|id| store.accounts.iter().any(|account| account.id == *id))
            .or_else(|| store.accounts.first().map(|account| account.id.clone()))
    } else {
        None
    };
    write_accounts_store_atomic(&path, &store)?;
    Ok((summary, activation_target))
}

fn add_catalog_account_if_nonempty(
    mut account: StoredAccount,
    auto_name_prefix: &str,
) -> Result<CatalogAddOutcome> {
    validate_stored_account_credentials(&account)?;

    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&path)?;
    if store.accounts.is_empty() {
        return Ok(CatalogAddOutcome::NeedsActivation(account));
    }

    if account.name.trim().is_empty() {
        account.name = next_auto_account_name(&store, auto_name_prefix);
    }
    if store
        .accounts
        .iter()
        .any(|existing| existing.id == account.id)
    {
        anyhow::bail!("An account with this ID already exists");
    }
    if store
        .accounts
        .iter()
        .any(|existing| existing.name == account.name)
    {
        anyhow::bail!("An account with name '{}' already exists", account.name);
    }

    store.accounts.push(account.clone());
    crate::auth::validate_catalog_credential_uniqueness(&store)?;
    write_accounts_store_atomic(&path, &store)?;
    Ok(CatalogAddOutcome::Added(account))
}

fn merge_catalog_if_activation_not_needed(imported: AccountsStore) -> Result<CatalogMergeOutcome> {
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&path)?;
    if store.accounts.is_empty() && !imported.accounts.is_empty() {
        return Ok(CatalogMergeOutcome::NeedsActivation(imported));
    }

    let previous_active = store.active_account_id.clone().filter(|active_id| {
        store
            .accounts
            .iter()
            .any(|account| account.id == *active_id)
    });
    let summary = merge_accounts_store(&mut store, imported)?;
    store.active_account_id = previous_active;
    write_accounts_store_atomic(&path, &store)?;
    Ok(CatalogMergeOutcome::Merged(summary))
}

fn remove_catalog_account_if_inactive(account_id: &str) -> Result<CatalogDeleteOutcome> {
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let accounts_before = capture_accounts_snapshot(&path)?;
    let mut store = load_accounts_from_path(&path)?;
    if !store
        .accounts
        .iter()
        .any(|account| account.id == account_id)
    {
        anyhow::bail!("Account not found: {account_id}");
    }

    let auth_snapshot = capture_auth_snapshot()?;
    let removed_while_resolving_duplicate = match reconcile_live_auth(&mut store, &auth_snapshot) {
        Ok(_) => false,
        Err(original_error) => {
            let mut without_target = store.clone();
            without_target
                .accounts
                .retain(|account| account.id != account_id);
            if reconcile_live_auth(&mut without_target, &auth_snapshot).is_err() {
                return Err(original_error);
            }
            store = without_target;
            true
        }
    };

    let outcome = if removed_while_resolving_duplicate {
        CatalogDeleteOutcome::Removed
    } else if store.active_account_id.as_deref() == Some(account_id) {
        CatalogDeleteOutcome::NeedsActivation
    } else {
        store.accounts.retain(|account| account.id != account_id);
        CatalogDeleteOutcome::Removed
    };

    let desired_store = FileSnapshot::present(serialize_accounts_store(&store)?);
    let staged_accounts =
        stage_file_change(&path, accounts_before.clone(), desired_store, "accounts")?;
    if !auth_snapshot_matches_current(&auth_snapshot)? {
        anyhow::bail!(
            "Codex auth.json changed while account deletion was in progress; the account catalog was left unchanged"
        );
    }
    let published_store = staged_accounts.commit()?;
    verify_auth_after_catalog_commit(&path, published_store, accounts_before, &auth_snapshot)?;
    Ok(outcome)
}

fn prepare_catalog(require_process_guard: bool) -> Result<PreparedCatalog> {
    if require_process_guard {
        ensure_switch_allowed()?;
    }

    let _lock = acquire_store_lock()?;
    let accounts_path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&accounts_path)?;
    let durable_active_id = store.active_account_id.clone();
    let accounts_before = serde_json::to_vec(&store.accounts)?;
    let auth_snapshot = capture_auth_snapshot()?;
    reconcile_live_auth(&mut store, &auth_snapshot)?;

    let prepared_store = store.clone();
    store.active_account_id = durable_active_id;
    if serde_json::to_vec(&store.accounts)? != accounts_before {
        write_accounts_store_atomic(&accounts_path, &store)?;
    }

    Ok(PreparedCatalog {
        auth_snapshot,
        store: prepared_store,
    })
}

fn commit_activation<T, F>(expected_auth: &AuthSnapshot, mutator: F) -> Result<T>
where
    F: FnOnce(&mut AccountsStore) -> Result<(AuthSnapshot, T)>,
{
    let _lock = acquire_store_lock()?;
    let accounts_path = get_accounts_file()?;
    let accounts_before = capture_accounts_snapshot(&accounts_path)?;
    let mut store = load_accounts_from_path(&accounts_path)?;

    if !auth_snapshot_matches_current(expected_auth)? {
        anyhow::bail!("Codex auth.json changed while the operation was in progress");
    }
    if !store.accounts.is_empty() {
        reconcile_live_auth(&mut store, expected_auth)?;
    }

    let (desired_auth, output) = mutator(&mut store)?;
    let desired_store = FileSnapshot::present(serialize_accounts_store(&store)?);
    let staged_accounts = stage_file_change(
        &accounts_path,
        accounts_before.clone(),
        desired_store,
        "accounts",
    )?;
    let staged_auth = if desired_auth == *expected_auth {
        None
    } else {
        Some(stage_auth_publication(expected_auth, &desired_auth)?)
    };

    test_replace_auth("CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_PUBLISH")?;
    ensure_switch_allowed()?;

    if let Some(staged_auth) = staged_auth {
        staged_auth.commit()?;
        test_replace_auth("CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_STORE")?;
        if !auth_snapshot_matches_current(&desired_auth)? {
            anyhow::bail!(
                "Codex auth.json changed immediately after publication; the account catalog was left unchanged"
            );
        }

        let published_store = match staged_accounts.commit() {
            Ok(published_store) => published_store,
            Err(store_error) => {
                let rollback = rollback_auth_publication(&desired_auth, expected_auth);
                return match rollback {
                    Ok(()) => Err(store_error).context(
                        "Failed to commit the account catalog; Codex auth.json was restored",
                    ),
                    Err(rollback_error) => Err(store_error).context(format!(
                        "Failed to commit the account catalog, and auth rollback was not applied: {rollback_error}"
                    )),
                };
            }
        };
        verify_auth_after_catalog_commit(
            &accounts_path,
            published_store,
            accounts_before,
            &desired_auth,
        )?;
    } else {
        if !auth_snapshot_matches_current(expected_auth)? {
            anyhow::bail!(
                "Codex auth.json changed before the account catalog was committed; the account catalog was left unchanged"
            );
        }
        let published_store = staged_accounts.commit()?;
        verify_auth_after_catalog_commit(
            &accounts_path,
            published_store,
            accounts_before,
            expected_auth,
        )?;
    }

    Ok(output)
}

fn verify_auth_after_catalog_commit(
    accounts_path: &std::path::Path,
    published_store: FileSnapshot,
    previous_store: FileSnapshot,
    expected_auth: &AuthSnapshot,
) -> Result<()> {
    test_replace_auth("CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_AFTER_STORE")?;
    if auth_snapshot_matches_current(expected_auth)? {
        return Ok(());
    }

    let rollback =
        stage_file_change(accounts_path, published_store, previous_store, "accounts")?.commit();
    match rollback {
        Ok(_) => anyhow::bail!(
            "Codex auth.json changed after the account catalog was committed; the account catalog was restored"
        ),
        Err(rollback_error) => anyhow::bail!(
            "Codex auth.json changed after the account catalog was committed, and catalog rollback was not applied: {rollback_error}"
        ),
    }
}

#[cfg(test)]
fn test_replace_auth(variable: &str) -> Result<()> {
    let Ok(contents) = std::env::var(variable) else {
        return Ok(());
    };
    std::fs::write(crate::auth::get_codex_auth_file()?, contents)
        .context("Failed to inject external auth replacement")
}

#[cfg(not(test))]
fn test_replace_auth(_variable: &str) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use futures::future::BoxFuture;

    use crate::auth::switcher::{get_codex_auth_file, read_current_auth, write_auth_for_test};
    use crate::auth::{add_account, load_accounts, RefreshTokenResponse};
    use crate::types::AuthDotJson;

    struct NoRefreshClient;

    impl ChatGptTokenRefreshClient for NoRefreshClient {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
            Box::pin(async { anyhow::bail!("refresh should not be called") })
        }
    }

    struct SuccessfulRefresh;

    impl ChatGptTokenRefreshClient for SuccessfulRefresh {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
            Box::pin(async {
                Ok(RefreshTokenResponse {
                    id_token: Some(test_jwt(chrono::Utc::now().timestamp() + 3600)),
                    access_token: test_jwt(chrono::Utc::now().timestamp() + 3600),
                    refresh_token: Some("refresh-rotated".to_string()),
                })
            })
        }
    }

    struct RefreshThenStartCodex;

    impl ChatGptTokenRefreshClient for RefreshThenStartCodex {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, Result<RefreshTokenResponse>> {
            std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");
            Box::pin(async {
                Ok(RefreshTokenResponse {
                    id_token: Some(test_jwt(chrono::Utc::now().timestamp() + 3600)),
                    access_token: test_jwt(chrono::Utc::now().timestamp() + 3600),
                    refresh_token: Some("refresh-rotated".to_string()),
                })
            })
        }
    }

    struct TestEnv {
        _config_dir: tempfile::TempDir,
        _codex_home: tempfile::TempDir,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl TestEnv {
        fn new() -> Self {
            let config_dir = tempfile::tempdir().expect("config temp dir");
            let codex_home = tempfile::tempdir().expect("codex temp dir");
            let variables = [
                "CODEX_SWITCHER_CONFIG_DIR",
                "CODEX_HOME",
                "CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT",
                "CODEX_SWITCHER_TEST_ATOMIC_FAIL",
                "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_PUBLISH",
                "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_STORE",
                "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_AFTER_STORE",
                "CODEX_SWITCHER_TEST_REPLACE_AFTER_QUARANTINE",
            ];
            let saved = variables
                .into_iter()
                .map(|name| (name, std::env::var(name).ok()))
                .collect();
            std::env::set_var("CODEX_SWITCHER_CONFIG_DIR", config_dir.path());
            std::env::set_var("CODEX_HOME", codex_home.path());
            std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");
            std::env::remove_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL");
            std::env::remove_var("CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_PUBLISH");
            std::env::remove_var("CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_STORE");
            std::env::remove_var("CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_AFTER_STORE");
            std::env::remove_var("CODEX_SWITCHER_TEST_REPLACE_AFTER_QUARANTINE");
            Self {
                _config_dir: config_dir,
                _codex_home: codex_home,
                saved,
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    fn api_account(name: &str, key: &str) -> StoredAccount {
        StoredAccount::new_api_key(name.to_string(), key.to_string())
    }

    fn test_jwt(exp: i64) -> String {
        let payload = serde_json::json!({
            "exp": exp,
            "email": "rotating@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-rotating",
                "chatgpt_plan_type": "pro"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize claims"));
        format!("header.{encoded}.signature")
    }

    fn expired_chatgpt_account() -> StoredAccount {
        StoredAccount::new_chatgpt(
            "Rotating".to_string(),
            Some("rotating@example.com".to_string()),
            Some("pro".to_string()),
            test_jwt(chrono::Utc::now().timestamp() - 60),
            test_jwt(chrono::Utc::now().timestamp() - 60),
            "refresh-original".to_string(),
            Some("acct-rotating".to_string()),
        )
    }

    fn live_auth_bytes() -> Vec<u8> {
        std::fs::read(get_codex_auth_file().expect("auth path")).expect("read live auth")
    }

    fn third_party_auth_json() -> String {
        serde_json::to_string(&AuthDotJson {
            openai_api_key: Some("sk-third-party".to_string()),
            tokens: None,
            last_refresh: None,
        })
        .expect("serialize third-party auth")
    }

    #[test]
    fn catalog_only_add_cannot_create_an_active_account_in_an_empty_store() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = api_account("Imported", "sk-imported");

        let outcome = add_catalog_account_if_nonempty(account.clone(), AUTO_ACCOUNT_NAME_PREFIX)
            .expect("check catalog add");

        assert!(matches!(
            outcome,
            CatalogAddOutcome::NeedsActivation(pending) if pending.id == account.id
        ));
        assert!(load_accounts().expect("load store").accounts.is_empty());
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[test]
    fn catalog_only_merge_cannot_activate_into_an_empty_store() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = api_account("Imported", "sk-imported");
        let imported = AccountsStore {
            version: 1,
            accounts: vec![account.clone()],
            active_account_id: Some(account.id.clone()),
            masked_account_ids: Vec::new(),
        };

        let outcome =
            merge_catalog_if_activation_not_needed(imported).expect("check catalog merge");

        assert!(matches!(
            outcome,
            CatalogMergeOutcome::NeedsActivation(pending)
                if pending.accounts.len() == 1 && pending.accounts[0].id == account.id
        ));
        assert!(load_accounts().expect("load store").accounts.is_empty());
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[test]
    fn full_import_preserves_a_nonempty_catalog_without_live_auth() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let existing = api_account("Existing", "sk-existing");
        write_accounts_store_atomic(
            &get_accounts_file().expect("accounts path"),
            &AccountsStore {
                version: 1,
                accounts: vec![existing],
                active_account_id: None,
                masked_account_ids: Vec::new(),
            },
        )
        .expect("write inactive catalog");
        let imported = api_account("Imported", "sk-imported");

        let outcome = merge_catalog_if_activation_not_needed(AccountsStore {
            version: 1,
            accounts: vec![imported],
            active_account_id: None,
            masked_account_ids: Vec::new(),
        })
        .expect("merge catalog");

        assert!(matches!(outcome, CatalogMergeOutcome::Merged(_)));
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 2);
        assert!(store.active_account_id.is_none());
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[test]
    fn catalog_only_delete_refuses_the_active_account() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let active = add_account(api_account("Active", "sk-active")).expect("add active");
        write_auth_for_test(&active).expect("write active auth");

        let outcome = remove_catalog_account_if_inactive(&active.id).expect("check catalog delete");

        assert!(matches!(outcome, CatalogDeleteOutcome::NeedsActivation));
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.active_account_id.as_deref(), Some(active.id.as_str()));
    }

    #[test]
    fn catalog_only_delete_reconciles_live_auth_before_deciding() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let catalog_active =
            add_account(api_account("Catalog active", "sk-catalog")).expect("add catalog active");
        let live_active =
            add_account(api_account("Live active", "sk-live")).expect("add live active");
        write_auth_for_test(&live_active).expect("write live account auth");

        let outcome =
            remove_catalog_account_if_inactive(&live_active.id).expect("check catalog delete");

        assert!(matches!(outcome, CatalogDeleteOutcome::NeedsActivation));
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 2);
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(live_active.id.as_str())
        );
        assert_ne!(
            store.active_account_id.as_deref(),
            Some(catalog_active.id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn activating_already_live_expired_account_advances_the_expected_snapshot() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let account = add_account(expired_chatgpt_account()).expect("add expired account");
        write_auth_for_test(&account).expect("write expired live auth");

        activate_existing_account_with_client(&account.id, &SuccessfulRefresh)
            .await
            .expect("refresh and reactivate live account");

        let store = load_accounts().expect("load store");
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(account.id.as_str())
        );
        assert!(store.accounts[0].last_used_at.is_some());
        assert!(matches!(
            store.accounts[0].auth_data,
            crate::types::AuthData::ChatGPT {
                ref refresh_token,
                ..
            } if refresh_token == "refresh-rotated"
        ));
        let live = read_current_auth()
            .expect("read live auth")
            .expect("live auth exists");
        assert!(matches!(
            live.tokens,
            Some(crate::types::TokenData { ref refresh_token, .. })
                if refresh_token == "refresh-rotated"
        ));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn first_import_process_guard_preserves_saved_inactive_account() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");

        let result = add_imported_account_with_client(
            api_account("Imported", "sk-imported"),
            &NoRefreshClient,
        )
        .await;

        assert!(result.is_err());
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert!(store.active_account_id.is_none());
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn full_import_process_guard_preserves_saved_inactive_accounts() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let imported = api_account("Imported", "sk-imported");
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");

        let result = merge_imported_accounts_with_client(
            AccountsStore {
                version: 1,
                accounts: vec![imported.clone()],
                active_account_id: Some(imported.id.clone()),
                masked_account_ids: Vec::new(),
            },
            &NoRefreshClient,
        )
        .await;

        assert!(result.is_err());
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].id, imported.id);
        assert!(store.active_account_id.is_none());
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rotated_pending_credentials_are_saved_when_activation_becomes_blocked() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let result =
            add_imported_account_with_client(expired_chatgpt_account(), &RefreshThenStartCodex)
                .await;

        assert!(result.is_err());
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert!(store.active_account_id.is_none());
        assert!(matches!(
            store.accounts[0].auth_data,
            crate::types::AuthData::ChatGPT {
                ref refresh_token,
                ..
            } if refresh_token == "refresh-rotated"
        ));
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn oauth_guard_failure_preserves_saved_inactive_account() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");

        let result =
            add_oauth_account_with_client(api_account("OAuth", "sk-oauth"), &NoRefreshClient).await;

        assert!(result.is_err());
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert!(store.active_account_id.is_none());
        assert!(!get_codex_auth_file().expect("auth path").exists());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn deletion_can_remove_a_legacy_duplicate_identity() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = api_account("First", "sk-shared");
        let duplicate = api_account("Duplicate", "sk-shared");
        let store = AccountsStore {
            version: 1,
            accounts: vec![first.clone(), duplicate.clone()],
            active_account_id: Some(first.id.clone()),
            masked_account_ids: Vec::new(),
        };
        write_accounts_store_atomic(&get_accounts_file().expect("accounts path"), &store)
            .expect("write legacy duplicate catalog");
        write_auth_for_test(&first).expect("write live auth");

        delete_account_with_client(&duplicate.id, &NoRefreshClient)
            .await
            .expect("delete duplicate account");

        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].id, first.id);
        assert_eq!(store.active_account_id.as_deref(), Some(first.id.as_str()));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn non_active_deletion_is_catalog_only_even_when_codex_runs() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let active = add_account(api_account("Active", "sk-active")).expect("add active");
        let background =
            add_account(api_account("Background", "sk-background")).expect("add background");
        write_auth_for_test(&active).expect("write active auth");
        let auth_before = live_auth_bytes();
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");

        delete_account_with_client(&background.id, &NoRefreshClient)
            .await
            .expect("delete background account");

        assert_eq!(live_auth_bytes(), auth_before);
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.active_account_id.as_deref(), Some(active.id.as_str()));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn inactive_deletion_rolls_back_if_live_auth_changes_after_catalog_commit() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let active = add_account(api_account("Active", "sk-active")).expect("add active");
        let background =
            add_account(api_account("Background", "sk-background")).expect("add background");
        write_auth_for_test(&active).expect("write active auth");
        let replacement = third_party_auth_json();
        std::env::set_var(
            "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_AFTER_STORE",
            &replacement,
        );

        let result = delete_account_with_client(&background.id, &NoRefreshClient).await;

        assert!(result.is_err());
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 2);
        assert!(store
            .accounts
            .iter()
            .any(|account| account.id == background.id));
        assert_eq!(String::from_utf8(live_auth_bytes()).unwrap(), replacement);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn active_deletion_publishes_prepared_fallback_atomically() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let fallback = add_account(api_account("Fallback", "sk-fallback")).expect("add fallback");
        let active = add_account(api_account("Active", "sk-active")).expect("add active");
        write_auth_for_test(&fallback).expect("write fallback auth");
        activate_existing_account_with_client(&active.id, &NoRefreshClient)
            .await
            .expect("activate second account");

        delete_account_with_client(&active.id, &NoRefreshClient)
            .await
            .expect("delete active account");

        let store = load_accounts().expect("load store");
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(fallback.id.as_str())
        );
        assert!(store.accounts[0].last_used_at.is_some());
        assert_eq!(
            read_current_auth()
                .expect("read auth")
                .expect("auth exists")
                .openai_api_key
                .as_deref(),
            Some("sk-fallback")
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn auth_publication_failure_leaves_catalog_unchanged() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        write_auth_for_test(&first).expect("write first auth");
        let auth_before = live_auth_bytes();
        std::env::set_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL", "publish_auth");

        let result = activate_existing_account_with_client(&second.id, &NoRefreshClient).await;

        assert!(result.is_err());
        assert_eq!(live_auth_bytes(), auth_before);
        assert_eq!(
            load_accounts()
                .expect("load store")
                .active_account_id
                .as_deref(),
            Some(first.id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn catalog_commit_failure_restores_previous_live_auth() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        write_auth_for_test(&first).expect("write first auth");
        let auth_before = live_auth_bytes();
        std::env::set_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL", "publish_accounts");

        let result = activate_existing_account_with_client(&second.id, &NoRefreshClient).await;

        assert!(result.is_err());
        assert_eq!(live_auth_bytes(), auth_before);
        assert_eq!(
            load_accounts()
                .expect("load store")
                .active_account_id
                .as_deref(),
            Some(first.id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn matching_live_auth_change_preserves_saved_inactive_account() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let imported = api_account("Imported", "sk-imported");
        write_auth_for_test(&imported).expect("write matching live auth");
        let replacement = third_party_auth_json();
        std::env::set_var(
            "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_PUBLISH",
            &replacement,
        );

        let result = add_imported_account_with_client(imported.clone(), &NoRefreshClient).await;

        assert!(result.is_err());
        assert_eq!(String::from_utf8(live_auth_bytes()).unwrap(), replacement);
        let store = load_accounts().expect("load store");
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].id, imported.id);
        assert!(store.active_account_id.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn snapshot_change_before_publication_aborts_without_store_changes() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        write_auth_for_test(&first).expect("write first auth");
        let replacement = third_party_auth_json();
        std::env::set_var(
            "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_PUBLISH",
            &replacement,
        );

        let result = activate_existing_account_with_client(&second.id, &NoRefreshClient).await;

        assert!(result.is_err());
        assert_eq!(String::from_utf8(live_auth_bytes()).unwrap(), replacement);
        assert_eq!(
            load_accounts()
                .expect("load store")
                .active_account_id
                .as_deref(),
            Some(first.id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn replacement_after_auth_quarantine_is_not_overwritten() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        write_auth_for_test(&first).expect("write first auth");
        let replacement = third_party_auth_json();
        std::env::set_var("CODEX_SWITCHER_TEST_REPLACE_AFTER_QUARANTINE", &replacement);

        let result = activate_existing_account_with_client(&second.id, &NoRefreshClient).await;

        assert!(result.is_err());
        assert_eq!(String::from_utf8(live_auth_bytes()).unwrap(), replacement);
        assert_eq!(
            load_accounts()
                .expect("load store")
                .active_account_id
                .as_deref(),
            Some(first.id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn third_party_change_after_publication_is_not_rolled_back() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        write_auth_for_test(&first).expect("write first auth");
        let replacement = third_party_auth_json();
        std::env::set_var(
            "CODEX_SWITCHER_TEST_AUTH_REPLACEMENT_BEFORE_STORE",
            &replacement,
        );

        let result = activate_existing_account_with_client(&second.id, &NoRefreshClient).await;

        assert!(result.is_err());
        assert_eq!(String::from_utf8(live_auth_bytes()).unwrap(), replacement);
        assert_eq!(
            load_accounts()
                .expect("load store")
                .active_account_id
                .as_deref(),
            Some(first.id.as_str())
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollback_failure_is_reported_without_hiding_inconsistent_state() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        write_auth_for_test(&first).expect("write first auth");
        std::env::set_var(
            "CODEX_SWITCHER_TEST_ATOMIC_FAIL",
            "publish_accounts,publish_rollback",
        );

        let error = match activate_existing_account_with_client(&second.id, &NoRefreshClient).await
        {
            Ok(()) => panic!("activation should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("rollback was not applied"));
        assert_eq!(
            load_accounts()
                .expect("load store")
                .active_account_id
                .as_deref(),
            Some(first.id.as_str())
        );
        assert_eq!(
            read_current_auth()
                .expect("read auth")
                .expect("auth exists")
                .openai_api_key
                .as_deref(),
            Some("sk-second")
        );
    }
}

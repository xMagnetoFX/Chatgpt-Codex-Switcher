//! Account storage module - manages reading and writing accounts.json

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

use super::switcher::{clear_current_auth, switch_to_account};
use crate::types::{AccountsStore, AuthData, ImportAccountsSummary, StoredAccount};

const STORE_FILENAME: &str = "accounts.json";
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);
const STALE_LOCK_MAX_AGE: Duration = Duration::from_secs(30);

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

fn load_accounts_from_path(path: &Path) -> Result<AccountsStore> {
    if !path.exists() {
        return Ok(AccountsStore::default());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read accounts file: {}", path.display()))?;

    let store: AccountsStore = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse accounts file: {}", path.display()))?;

    Ok(store)
}

/// Save the accounts store to disk.
pub fn save_accounts(store: &AccountsStore) -> Result<()> {
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    write_accounts_store_atomic(&path, store)?;
    sync_active_auth_for_store(store)?;
    Ok(())
}

fn mutate_store<T, F>(sync_active_auth: bool, mutator: F) -> Result<T>
where
    F: FnOnce(&mut AccountsStore) -> Result<T>,
{
    mutate_store_with_sync_decision(move |store| {
        let output = mutator(store)?;
        Ok((output, sync_active_auth))
    })
}

/// Like `mutate_store`, but the mutator decides (based on the store contents)
/// whether the active auth.json should be re-synced after the write.
fn mutate_store_with_sync_decision<T, F>(mutator: F) -> Result<T>
where
    F: FnOnce(&mut AccountsStore) -> Result<(T, bool)>,
{
    let _lock = acquire_store_lock()?;
    let path = get_accounts_file()?;
    let mut store = load_accounts_from_path(&path)?;
    let (output, sync_active_auth) = mutator(&mut store)?;
    write_accounts_store_atomic(&path, &store)?;
    if sync_active_auth {
        sync_active_auth_for_store(&store)?;
    }
    Ok(output)
}

fn write_accounts_store_atomic(path: &Path, store: &AccountsStore) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    let content = serde_json::to_vec_pretty(store).context("Failed to serialize accounts store")?;
    write_bytes_atomic(path, &content)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Atomic write path did not have a parent directory")?;

    let mut temp_file = NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temp file in {}", parent.display()))?;
    temp_file
        .write_all(bytes)
        .with_context(|| format!("Failed to write temp file for {}", path.display()))?;
    temp_file
        .flush()
        .with_context(|| format!("Failed to flush temp file for {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600)).with_context(
            || format!("Failed to set temp file permissions for {}", path.display()),
        )?;
    }

    let temp_path = temp_file.into_temp_path();
    replace_file(temp_path.as_ref(), path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("Failed to set file permissions for {}", path.display()))?;
    }

    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination).with_context(|| {
        format!(
            "Failed to atomically replace {} with {}",
            destination.display(),
            source.display()
        )
    })
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    if !destination.exists() {
        return fs::rename(source, destination).with_context(|| {
            format!(
                "Failed to move {} into place at {}",
                source.display(),
                destination.display()
            )
        });
    }

    let source_wide: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let replaced = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if replaced == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "Failed to atomically replace {} with {}",
                destination.display(),
                source.display()
            )
        });
    }

    Ok(())
}

fn sync_active_auth_for_store(store: &AccountsStore) -> Result<()> {
    let Some(active_id) = store.active_account_id.as_deref() else {
        clear_current_auth()?;
        return Ok(());
    };

    let Some(account) = store
        .accounts
        .iter()
        .find(|account| account.id == active_id)
    else {
        clear_current_auth()?;
        return Ok(());
    };

    switch_to_account(account)
}

struct StoreLock {
    path: PathBuf,
    token: String,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        // Only remove the lock file if it is still ours. A contender that
        // judged our lock stale may have replaced it with its own.
        let still_ours = fs::read_to_string(&self.path)
            .is_ok_and(|contents| contents.contains(&self.token));
        if still_ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn acquire_store_lock() -> Result<StoreLock> {
    let path = get_accounts_lock_file()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let token = uuid::Uuid::new_v4().to_string();
                let _ = writeln!(
                    file,
                    "pid={} token={token} time={:?}",
                    std::process::id(),
                    SystemTime::now()
                );
                let _ = file.flush();
                return Ok(StoreLock { path, token });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&path) && steal_stale_lock(&path) {
                    continue;
                }

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
                    .with_context(|| format!("Failed to acquire store lock: {}", path.display()));
            }
        }
    }
}

/// Claim a stale lock by atomically renaming it aside. Rename fails for all
/// but one contender, so two processes can never both "remove" the same stale
/// lock and then race to create fresh ones.
fn steal_stale_lock(path: &Path) -> bool {
    let mut steal_name = path.as_os_str().to_owned();
    steal_name.push(format!(".stale-{}", uuid::Uuid::new_v4()));
    let steal_path = PathBuf::from(steal_name);

    if fs::rename(path, &steal_path).is_ok() {
        let _ = fs::remove_file(&steal_path);
        true
    } else {
        false
    }
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .is_ok_and(|elapsed| elapsed > STALE_LOCK_MAX_AGE)
}

/// Add a new account to the store
pub fn add_account(account: StoredAccount) -> Result<StoredAccount> {
    let account_clone = account.clone();
    mutate_store(true, move |store| {
        if store
            .accounts
            .iter()
            .any(|existing| existing.name == account.name)
        {
            anyhow::bail!("An account with name '{}' already exists", account.name);
        }

        store.accounts.push(account);
        if store.accounts.len() == 1 {
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
    mutate_store(true, move |store| {
        account.name = next_auto_account_name(store, prefix);
        let stored = account.clone();

        store.accounts.push(account);
        if store.accounts.len() == 1 {
            store.active_account_id = Some(stored.id.clone());
        }

        Ok(stored)
    })
}

fn next_auto_account_name(store: &AccountsStore, prefix: &str) -> String {
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
    mutate_store(true, |store| {
        let initial_len = store.accounts.len();
        store.accounts.retain(|account| account.id != account_id);

        if store.accounts.len() == initial_len {
            anyhow::bail!("Account not found: {account_id}");
        }

        if store.active_account_id.as_deref() == Some(account_id) {
            store.active_account_id = store.accounts.first().map(|account| account.id.clone());
        }

        Ok(())
    })
}

/// Update the active account ID
pub fn set_active_account(account_id: &str) -> Result<()> {
    mutate_store(true, |store| {
        if !store
            .accounts
            .iter()
            .any(|account| account.id == account_id)
        {
            anyhow::bail!("Account not found: {account_id}");
        }

        store.active_account_id = Some(account_id.to_string());
        Ok(())
    })
}

/// Merge imported accounts into the local store, skipping duplicate ids and names.
pub fn merge_imported_accounts(imported: AccountsStore) -> Result<ImportAccountsSummary> {
    mutate_store(true, move |store| Ok(merge_accounts_store(store, imported)))
}

fn merge_accounts_store(
    current: &mut AccountsStore,
    imported: AccountsStore,
) -> ImportAccountsSummary {
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

    ImportAccountsSummary {
        total_in_payload,
        imported_count,
        skipped_count: total_in_payload.saturating_sub(imported_count),
    }
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
    mutate_store(false, |store| {
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
    mutate_store(false, |store| {
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

/// Update ChatGPT OAuth tokens for an account and return the updated account.
pub fn update_account_chatgpt_tokens(
    account_id: &str,
    id_token: String,
    access_token: String,
    refresh_token: String,
    chatgpt_account_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
) -> Result<StoredAccount> {
    // Only re-sync auth.json when the refreshed account is the active one.
    // Rewriting it for background refreshes could clobber tokens a running
    // Codex CLI just rotated on its own.
    mutate_store_with_sync_decision(|store| {
        let is_active = store.active_account_id.as_deref() == Some(account_id);
        let account = store
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
            .context("Account not found")?;

        match &mut account.auth_data {
            AuthData::ChatGPT {
                id_token: stored_id_token,
                access_token: stored_access_token,
                refresh_token: stored_refresh_token,
                account_id: stored_account_id,
            } => {
                *stored_id_token = id_token;
                *stored_access_token = access_token;
                *stored_refresh_token = refresh_token;
                if let Some(new_account_id) = chatgpt_account_id {
                    *stored_account_id = Some(new_account_id);
                }
            }
            AuthData::ApiKey { .. } => {
                anyhow::bail!("Cannot update OAuth tokens for an API key account");
            }
        }

        if let Some(new_email) = email {
            account.email = Some(new_email);
        }

        if let Some(new_plan_type) = plan_type {
            account.plan_type = Some(new_plan_type);
        }

        Ok((account.clone(), is_active))
    })
}

/// Get the list of masked account IDs
pub fn get_masked_account_ids() -> Result<Vec<String>> {
    let store = load_accounts()?;
    Ok(store.masked_account_ids.clone())
}

/// Set the list of masked account IDs
pub fn set_masked_account_ids(ids: Vec<String>) -> Result<()> {
    mutate_store(false, |store| {
        store.masked_account_ids = ids;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::switcher::read_current_auth;

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

    fn api_account(name: &str, key: &str) -> StoredAccount {
        StoredAccount::new_api_key(name.to_string(), key.to_string())
    }

    fn chatgpt_account(name: &str, token_suffix: &str) -> StoredAccount {
        StoredAccount::new_chatgpt(
            name.to_string(),
            Some(format!("{name}@example.com")),
            Some("plus".to_string()),
            format!("id-{token_suffix}"),
            format!("access-{token_suffix}"),
            format!("refresh-{token_suffix}"),
            Some(format!("acct-{token_suffix}")),
        )
    }

    #[test]
    fn first_add_sets_active_account_and_writes_auth_json() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let stored = add_account(api_account("Primary", "sk-primary")).expect("add account");
        let store = load_accounts().expect("load accounts");
        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");

        assert_eq!(store.active_account_id.as_deref(), Some(stored.id.as_str()));
        assert_eq!(auth.openai_api_key.as_deref(), Some("sk-primary"));
    }

    #[test]
    fn deleting_active_account_promotes_fallback_and_syncs_auth_json() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let first = add_account(api_account("First", "sk-first")).expect("add first");
        let second = add_account(api_account("Second", "sk-second")).expect("add second");
        set_active_account(&second.id).expect("set active");
        remove_account(&second.id).expect("remove second");

        let store = load_accounts().expect("load accounts");
        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");

        assert_eq!(store.active_account_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(auth.openai_api_key.as_deref(), Some("sk-first"));
    }

    #[test]
    fn deleting_last_account_clears_auth_json() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let stored = add_account(api_account("Only", "sk-only")).expect("add account");
        remove_account(&stored.id).expect("remove account");

        let store = load_accounts().expect("load accounts");
        assert!(store.active_account_id.is_none());
        assert!(store.accounts.is_empty());
        assert!(read_current_auth().expect("read auth").is_none());
    }

    #[test]
    fn auto_named_accounts_use_next_available_ac_name() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        add_account(api_account("AC 1", "sk-existing")).expect("add existing");
        let first_auto =
            add_account_with_auto_name(api_account("", "sk-auto-1"), "AC").expect("add first auto");
        let second_auto = add_account_with_auto_name(api_account("", "sk-auto-2"), "AC")
            .expect("add second auto");

        assert_eq!(first_auto.name, "AC 2");
        assert_eq!(second_auto.name, "AC 3");
    }

    #[test]
    fn updating_active_chatgpt_tokens_keeps_auth_json_in_sync() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let stored = add_account(chatgpt_account("ChatGPT", "old")).expect("add account");
        let updated = update_account_chatgpt_tokens(
            &stored.id,
            "id-new".to_string(),
            "access-new".to_string(),
            "refresh-new".to_string(),
            Some("acct-new".to_string()),
            Some("fresh@example.com".to_string()),
            Some("pro".to_string()),
        )
        .expect("update tokens");
        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");

        assert_eq!(updated.email.as_deref(), Some("fresh@example.com"));
        let tokens = auth.tokens.expect("tokens should exist");
        assert_eq!(tokens.id_token, "id-new");
        assert_eq!(tokens.access_token, "access-new");
        assert_eq!(tokens.refresh_token, "refresh-new");
        assert_eq!(tokens.account_id.as_deref(), Some("acct-new"));
    }

    #[test]
    fn imported_accounts_can_establish_active_account_and_sync_auth_json() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let imported_first = api_account("Imported 1", "sk-import-1");
        let imported_second = api_account("Imported 2", "sk-import-2");
        let summary = merge_imported_accounts(AccountsStore {
            version: 2,
            accounts: vec![imported_first.clone(), imported_second.clone()],
            active_account_id: Some(imported_second.id.clone()),
            masked_account_ids: Vec::new(),
        })
        .expect("merge accounts");

        let store = load_accounts().expect("load accounts");
        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");

        assert_eq!(summary.total_in_payload, 2);
        assert_eq!(summary.imported_count, 2);
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(imported_second.id.as_str())
        );
        assert_eq!(auth.openai_api_key.as_deref(), Some("sk-import-2"));
    }

    #[test]
    fn updating_background_chatgpt_tokens_does_not_rewrite_auth_json() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let _active = add_account(api_account("Active", "sk-active")).expect("add active");
        let background = add_account(chatgpt_account("Background", "old")).expect("add bg");

        let auth_path = crate::auth::switcher::get_codex_auth_file().expect("auth path");
        std::fs::remove_file(&auth_path).expect("remove auth.json");

        update_account_chatgpt_tokens(
            &background.id,
            "id-new".to_string(),
            "access-new".to_string(),
            "refresh-new".to_string(),
            None,
            None,
            None,
        )
        .expect("update background tokens");

        assert!(
            !auth_path.exists(),
            "background token refresh must not rewrite auth.json"
        );
    }

    #[test]
    fn full_import_preserves_masked_ids_for_present_accounts() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let imported = api_account("Masked", "sk-masked");
        merge_imported_accounts(AccountsStore {
            version: 1,
            accounts: vec![imported.clone()],
            active_account_id: None,
            masked_account_ids: vec![imported.id.clone(), "missing-account".to_string()],
        })
        .expect("merge accounts");

        let store = load_accounts().expect("load accounts");
        assert_eq!(store.masked_account_ids, vec![imported.id]);
    }

    #[test]
    fn concurrent_mutations_do_not_lose_changes() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let stored = add_account(api_account("Original", "sk-original")).expect("add account");
        let account_id = stored.id.clone();

        let rename_id = account_id.clone();
        let rename = thread::spawn(move || {
            update_account_metadata(&rename_id, Some("Renamed".to_string()), None, None)
                .expect("rename account");
        });

        let mask_id = account_id.clone();
        let mask = thread::spawn(move || {
            set_masked_account_ids(vec![mask_id]).expect("set masked ids");
        });

        rename.join().expect("rename thread");
        mask.join().expect("mask thread");

        let store = load_accounts().expect("load accounts");
        assert_eq!(store.accounts[0].name, "Renamed");
        assert_eq!(store.masked_account_ids, vec![account_id]);
    }
}

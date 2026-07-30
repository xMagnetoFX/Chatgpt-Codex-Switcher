//! Account management Tauri commands

use crate::auth::{
    create_chatgpt_account_from_refresh_token, get_full_backup_key, get_or_create_full_backup_key,
    import_from_auth_json, import_from_auth_json_contents, load_accounts,
    reconcile_current_auth_catalog, ChatGptTokenRefreshClient, HttpChatGptTokenRefreshClient,
};
use crate::commands::activation::{
    activate_existing_account_with_client, add_imported_account_with_client,
    delete_account_with_client, merge_imported_accounts_with_client,
    persist_restored_accounts_catalog_only, CatalogRestoreOutcome, RestoredCatalogAccount,
};
use crate::commands::process::{
    prepare_codex_restart_plan, start_codex_from_restart_plan, stop_codex_for_restart,
};
use crate::types::{AccountInfo, AccountsStore, AuthData, ImportAccountsSummary, StoredAccount};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use futures::{stream, StreamExt};
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use sha2::Sha256;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::sync::LazyLock;

use tokio::sync::Mutex as AsyncMutex;

const SLIM_EXPORT_PREFIX: &str = "css1.";
const SLIM_FORMAT_VERSION: u8 = 1;
const SLIM_AUTH_API_KEY: u8 = 0;
const SLIM_AUTH_CHATGPT: u8 = 1;

const FULL_FILE_MAGIC: &[u8; 4] = b"CSWF";
const FULL_FILE_VERSION_LEGACY: u8 = 1;
const FULL_FILE_VERSION_MACHINE_BOUND: u8 = 2;
const FULL_SALT_LEN: usize = 16;
const FULL_NONCE_LEN: usize = 24;
const FULL_KDF_ITERATIONS: u32 = 210_000;
const FULL_KEY_CONTEXT: &[u8] = b"codex-switcher-full-backup-v2";
const LEGACY_FULL_PRESET_PASSPHRASE: &str = "gT7kQ9mV2xN4pL8sR1dH6zW3cB5yF0uJ_aE7nK2tP9vM4rX1";

const MAX_IMPORT_JSON_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMPORT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const SLIM_IMPORT_CONCURRENCY: usize = 6;

static RESTART_SWITCH_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SlimPayload {
    #[serde(rename = "v")]
    version: u8,
    #[serde(rename = "a", skip_serializing_if = "Option::is_none")]
    active_name: Option<String>,
    #[serde(rename = "c")]
    accounts: Vec<SlimAccountPayload>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SlimAccountPayload {
    #[serde(rename = "n")]
    name: String,
    #[serde(rename = "t")]
    auth_type: u8,
    #[serde(rename = "k", skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(rename = "r", skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

/// List all accounts with their info
#[tauri::command]
pub async fn list_accounts() -> Result<Vec<AccountInfo>, String> {
    reconcile_current_auth_catalog().map_err(|error| format!("{error:#}"))?;
    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();

    let accounts: Vec<AccountInfo> = store
        .accounts
        .iter()
        .map(|a| AccountInfo::from_stored(a, active_id))
        .collect();

    Ok(accounts)
}

/// Get the currently active account
#[tauri::command]
pub async fn get_active_account_info() -> Result<Option<AccountInfo>, String> {
    reconcile_current_auth_catalog().map_err(|error| format!("{error:#}"))?;
    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();
    Ok(active_id
        .and_then(|id| store.accounts.iter().find(|account| account.id == id))
        .map(|active| AccountInfo::from_stored(active, active_id)))
}

/// Add an account from an auth.json file
#[tauri::command]
pub async fn add_account_from_file(
    path: String,
    name: Option<String>,
) -> Result<AccountInfo, String> {
    // Import from the file
    let account_name = name.unwrap_or_default();
    let account = import_from_auth_json(&path, account_name).map_err(|e| e.to_string())?;

    let stored = add_imported_account_with_client(account, &HttpChatGptTokenRefreshClient)
        .await
        .map_err(|e| e.to_string())?;

    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();

    Ok(AccountInfo::from_stored(&stored, active_id))
}

/// Add an account from uploaded auth.json contents.
pub async fn add_account_from_auth_json_text(
    name: Option<String>,
    contents: String,
) -> Result<AccountInfo, String> {
    let account_name = name.unwrap_or_default();
    let account =
        import_from_auth_json_contents(&contents, account_name).map_err(|e| e.to_string())?;
    let stored = add_imported_account_with_client(account, &HttpChatGptTokenRefreshClient)
        .await
        .map_err(|e| e.to_string())?;

    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();

    Ok(AccountInfo::from_stored(&stored, active_id))
}

/// Switch to a different account
#[tauri::command]
pub async fn switch_account(account_id: String) -> Result<(), String> {
    switch_account_with_client(account_id, &HttpChatGptTokenRefreshClient).await
}

async fn switch_account_with_client<C>(account_id: String, client: &C) -> Result<(), String>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    activate_existing_account_with_client(&account_id, client)
        .await
        .map_err(|e| e.to_string())
}

/// Stop running Codex windows, switch accounts, and relaunch Codex.
#[tauri::command]
pub async fn restart_codex_and_switch_account(account_id: String) -> Result<(), String> {
    restart_codex_and_switch_account_with_client(account_id, &HttpChatGptTokenRefreshClient).await
}

async fn restart_codex_and_switch_account_with_client<C>(
    account_id: String,
    client: &C,
) -> Result<(), String>
where
    C: ChatGptTokenRefreshClient + ?Sized,
{
    let _restart_guard = RESTART_SWITCH_LOCK.lock().await;
    let restart_plan = tokio::task::spawn_blocking(move || {
        let restart_plan = prepare_codex_restart_plan().map_err(|e| e.to_string())?;
        stop_codex_for_restart(&restart_plan).map_err(|e| e.to_string())?;
        Ok::<_, String>(restart_plan)
    })
    .await
    .map_err(|e| format!("Restart preparation task failed: {e}"))??;

    let switch_result = activate_existing_account_with_client(&account_id, client)
        .await
        .map_err(|e| e.to_string());

    // Once the restart flow stops Codex, always attempt to relaunch it even if
    // credential synchronization or refresh fails.
    let restart_result = tokio::task::spawn_blocking(move || {
        start_codex_from_restart_plan(&restart_plan).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("Restart task failed: {e}"))?;

    switch_result?;
    restart_result
}

/// Remove an account
#[tauri::command]
pub async fn delete_account(account_id: String) -> Result<(), String> {
    delete_account_with_client(&account_id, &HttpChatGptTokenRefreshClient)
        .await
        .map_err(|e| e.to_string())
}

/// Rename an account
#[tauri::command]
pub async fn rename_account(account_id: String, new_name: String) -> Result<(), String> {
    crate::auth::storage::update_account_metadata(&account_id, Some(new_name), None, None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Export minimal account config as a compact text string.
/// For ChatGPT accounts, only refresh token is exported.
#[tauri::command]
pub async fn export_accounts_slim_text() -> Result<String, String> {
    reconcile_current_auth_catalog().map_err(|error| format!("{error:#}"))?;
    let store = load_accounts().map_err(|e| e.to_string())?;
    encode_slim_payload_from_store(&store).map_err(|e| e.to_string())
}

/// Import minimal account config from a compact text string, skipping existing accounts.
#[tauri::command]
pub async fn import_accounts_slim_text(payload: String) -> Result<ImportAccountsSummary, String> {
    let slim_payload = decode_slim_payload(&payload).map_err(|e| format!("{e:#}"))?;
    let total_in_payload = slim_payload.accounts.len();
    let current = load_accounts().map_err(|e| e.to_string())?;

    let imported = build_store_from_slim_payload(slim_payload, &current)
        .await
        .map_err(|e| {
            format!(
                "{e:#}\nHint: Slim import needs network access to refresh ChatGPT tokens. No catalog changes are saved unless every account is restored successfully."
            )
        })?;

    reconcile_current_auth_catalog().map_err(|error| {
        format!(
            "Imported accounts were saved, but live credentials could not be reconciled: {error:#}"
        )
    })?;
    let latest = load_accounts().map_err(|e| e.to_string())?;
    if latest.active_account_id.is_none() {
        if let Some(target_id) = imported
            .activation_target_id
            .as_deref()
            .filter(|target_id| {
                latest
                    .accounts
                    .iter()
                    .any(|account| account.id.as_str() == *target_id)
            })
        {
            activate_existing_account_with_client(target_id, &HttpChatGptTokenRefreshClient)
                .await
                .map_err(|e| {
                    format!(
                        "Imported accounts were saved, but the selected account could not be activated: {e:#}"
                    )
                })?;
        }
    }

    Ok(ImportAccountsSummary {
        total_in_payload,
        imported_count: imported.changed_count,
        skipped_count: total_in_payload.saturating_sub(imported.changed_count),
    })
}

/// Export full account config as an encrypted file.
#[tauri::command]
pub async fn export_accounts_full_encrypted_file(path: String) -> Result<(), String> {
    reconcile_current_auth_catalog().map_err(|error| format!("{error:#}"))?;
    let store = load_accounts().map_err(|e| e.to_string())?;
    let encrypted = encode_full_encrypted_store(&store).map_err(|e| e.to_string())?;
    write_encrypted_file(&path, &encrypted).map_err(|e| e.to_string())?;
    Ok(())
}

/// Export full account config as encrypted bytes for browser clients.
pub async fn export_accounts_full_encrypted_bytes() -> Result<Vec<u8>, String> {
    reconcile_current_auth_catalog().map_err(|error| format!("{error:#}"))?;
    let store = load_accounts().map_err(|e| e.to_string())?;
    encode_full_encrypted_store(&store).map_err(|e| e.to_string())
}

/// Import full account config from an encrypted file, skipping existing accounts.
#[tauri::command]
pub async fn import_accounts_full_encrypted_file(
    path: String,
) -> Result<ImportAccountsSummary, String> {
    let encrypted = read_encrypted_file(&path).map_err(|e| e.to_string())?;
    let imported = decode_full_encrypted_store(&encrypted).map_err(|e| e.to_string())?;
    validate_imported_store(&imported).map_err(|e| e.to_string())?;

    let summary = merge_imported_accounts_with_client(imported, &HttpChatGptTokenRefreshClient)
        .await
        .map_err(|e| e.to_string())?;
    Ok(summary)
}

/// Import full account config from encrypted bytes uploaded through the browser UI.
pub async fn import_accounts_full_encrypted_bytes(
    bytes: Vec<u8>,
) -> Result<ImportAccountsSummary, String> {
    let imported = decode_full_encrypted_store(&bytes).map_err(|e| e.to_string())?;
    validate_imported_store(&imported).map_err(|e| e.to_string())?;

    let summary = merge_imported_accounts_with_client(imported, &HttpChatGptTokenRefreshClient)
        .await
        .map_err(|e| e.to_string())?;
    Ok(summary)
}

fn encode_slim_payload_from_store(store: &AccountsStore) -> anyhow::Result<String> {
    let active_name = store.active_account_id.as_ref().and_then(|active_id| {
        store
            .accounts
            .iter()
            .find(|account| account.id == *active_id)
            .map(|account| account.name.clone())
    });

    let slim_accounts = store
        .accounts
        .iter()
        .map(|account| match &account.auth_data {
            AuthData::ApiKey { key } => SlimAccountPayload {
                name: account.name.clone(),
                auth_type: SLIM_AUTH_API_KEY,
                api_key: Some(key.clone()),
                refresh_token: None,
            },
            AuthData::ChatGPT { refresh_token, .. } => SlimAccountPayload {
                name: account.name.clone(),
                auth_type: SLIM_AUTH_CHATGPT,
                api_key: None,
                refresh_token: Some(refresh_token.clone()),
            },
        })
        .collect();

    let payload = SlimPayload {
        version: SLIM_FORMAT_VERSION,
        active_name,
        accounts: slim_accounts,
    };

    let json = serde_json::to_vec(&payload).context("Failed to serialize slim payload")?;
    let compressed = compress_bytes(&json).context("Failed to compress slim payload")?;

    Ok(format!(
        "{SLIM_EXPORT_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(compressed)
    ))
}

fn decode_slim_payload(payload: &str) -> anyhow::Result<SlimPayload> {
    let normalized: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    if normalized.is_empty() {
        anyhow::bail!("Import string is empty");
    }

    let encoded = normalized
        .strip_prefix(SLIM_EXPORT_PREFIX)
        .unwrap_or(&normalized);

    let compressed = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("Invalid slim import string (base64 decode failed)")?;

    let decompressed = decompress_bytes_with_limit(&compressed, MAX_IMPORT_JSON_BYTES)
        .context("Invalid slim import string (decompression failed)")?;

    let parsed: SlimPayload = serde_json::from_slice(&decompressed)
        .context("Invalid slim import string (JSON parse failed)")?;

    validate_slim_payload(&parsed)?;
    Ok(parsed)
}

fn validate_slim_payload(payload: &SlimPayload) -> anyhow::Result<()> {
    if payload.version != SLIM_FORMAT_VERSION {
        anyhow::bail!("Unsupported slim payload version: {}", payload.version);
    }

    let mut names = HashSet::new();
    let mut api_keys = HashSet::new();
    let mut refresh_tokens = HashSet::new();

    for account in &payload.accounts {
        if account.name.trim().is_empty() {
            anyhow::bail!("Slim import contains an account with empty name");
        }

        if !names.insert(account.name.clone()) {
            anyhow::bail!(
                "Slim import contains duplicate account name: {}",
                account.name
            );
        }

        match account.auth_type {
            SLIM_AUTH_API_KEY => {
                let key = account
                    .api_key
                    .as_deref()
                    .filter(|key| !key.trim().is_empty())
                    .with_context(|| format!("API key is missing for account {}", account.name))?;
                if !api_keys.insert(key) {
                    anyhow::bail!(
                        "Slim import contains more than one account with the same API key"
                    );
                }
            }
            SLIM_AUTH_CHATGPT => {
                let refresh_token = account
                    .refresh_token
                    .as_deref()
                    .filter(|token| !token.trim().is_empty())
                    .with_context(|| {
                        format!("Refresh token is missing for account {}", account.name)
                    })?;
                if !refresh_tokens.insert(refresh_token) {
                    anyhow::bail!(
                        "Slim import contains more than one account with the same refresh token"
                    );
                }
            }
            _ => {
                anyhow::bail!(
                    "Unsupported auth type {} for account {}",
                    account.auth_type,
                    account.name
                );
            }
        }
    }

    if let Some(active_name) = &payload.active_name {
        if !names.contains(active_name) {
            anyhow::bail!("Slim import references missing active account: {active_name}");
        }
    }

    Ok(())
}

struct SlimImportResult {
    changed_count: usize,
    activation_target_id: Option<String>,
}

struct ExistingSlimAccount {
    source_name: String,
    account_id: String,
}

struct RestoredSlimAccount {
    source_name: String,
    restored: RestoredCatalogAccount,
}

async fn build_store_from_slim_payload(
    payload: SlimPayload,
    current: &AccountsStore,
) -> anyhow::Result<SlimImportResult> {
    let active_name = payload.active_name;
    let mut existing_matches = Vec::new();
    let mut import_candidates = Vec::new();

    for entry in payload.accounts {
        if let Some(existing) = current
            .accounts
            .iter()
            .find(|account| slim_entry_matches_existing(&entry, account))
        {
            existing_matches.push(ExistingSlimAccount {
                source_name: entry.name,
                account_id: existing.id.clone(),
            });
            continue;
        }

        // A display name is not credential identity. Restore the entry first so a stale ChatGPT
        // refresh token can resolve to the same stable account; the atomic catalog merge below
        // rejects the name if it belongs to a genuinely different login.
        import_candidates.push(entry);
    }

    let restored = restore_slim_accounts(import_candidates).await?;
    let inputs = restored
        .iter()
        .map(|entry| RestoredCatalogAccount {
            account: entry.restored.account.clone(),
            source_refresh_token: entry.restored.source_refresh_token.clone(),
        })
        .collect();
    let outcomes = persist_restored_accounts_catalog_only(inputs)
        .context("Failed to save restored slim-import accounts")?;
    let changed_count = outcomes.len();

    let mut activation_target_id = active_name.as_deref().and_then(|name| {
        existing_matches
            .iter()
            .find(|entry| entry.source_name == name)
            .map(|entry| entry.account_id.clone())
    });
    let mut first_changed_id = None;
    for (restored, outcome) in restored.into_iter().zip(outcomes) {
        let account = match outcome {
            CatalogRestoreOutcome::Added(account)
            | CatalogRestoreOutcome::UpdatedExisting(account) => account,
        };
        first_changed_id.get_or_insert_with(|| account.id.clone());
        if active_name.as_deref() == Some(restored.source_name.as_str()) {
            activation_target_id = Some(account.id);
        }
    }

    if active_name.is_none() {
        activation_target_id = first_changed_id.or_else(|| {
            existing_matches
                .first()
                .map(|entry| entry.account_id.clone())
        });
    }

    Ok(SlimImportResult {
        changed_count,
        activation_target_id,
    })
}

fn slim_entry_matches_existing(entry: &SlimAccountPayload, existing: &StoredAccount) -> bool {
    match (entry.auth_type, &existing.auth_data) {
        (SLIM_AUTH_API_KEY, AuthData::ApiKey { key }) => entry.api_key.as_deref() == Some(key),
        (SLIM_AUTH_CHATGPT, AuthData::ChatGPT { refresh_token, .. }) => {
            entry.refresh_token.as_deref() == Some(refresh_token)
        }
        _ => false,
    }
}

async fn restore_slim_accounts(
    entries: Vec<SlimAccountPayload>,
) -> anyhow::Result<Vec<RestoredSlimAccount>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let mut restored = Vec::with_capacity(entries.len());
    let mut tasks = stream::iter(entries.into_iter().map(|entry| async move {
        let account_name = entry.name;
        let (account, source_refresh_token) = match entry.auth_type {
            SLIM_AUTH_API_KEY => (
                StoredAccount::new_api_key(
                    account_name.clone(),
                    entry.api_key.context("API key payload is missing")?,
                ),
                None,
            ),
            SLIM_AUTH_CHATGPT => {
                let refresh_token = entry
                    .refresh_token
                    .context("Refresh token payload is missing")?;
                let account = create_chatgpt_account_from_refresh_token(
                    account_name.clone(),
                    refresh_token.clone(),
                )
                .await
                .with_context(|| {
                    format!("Failed to restore ChatGPT account `{account_name}` from refresh token")
                })?;
                (account, Some(refresh_token))
            }
            _ => anyhow::bail!("Unsupported auth type in slim payload"),
        };
        Ok::<_, anyhow::Error>(RestoredSlimAccount {
            source_name: account_name,
            restored: RestoredCatalogAccount {
                account,
                source_refresh_token,
            },
        })
    }))
    .buffered(SLIM_IMPORT_CONCURRENCY);

    while let Some(account_result) = tasks.next().await {
        restored.push(account_result?);
    }

    Ok(restored)
}

fn encode_full_encrypted_store(store: &AccountsStore) -> anyhow::Result<Vec<u8>> {
    let machine_key = get_or_create_full_backup_key()?;
    encode_full_encrypted_store_with_key(store, &machine_key)
}

fn encode_full_encrypted_store_with_key(
    store: &AccountsStore,
    machine_key: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec(store).context("Failed to serialize account store")?;
    let compressed = compress_bytes(&json).context("Failed to compress account store")?;

    let mut salt = [0u8; FULL_SALT_LEN];
    rand::rng().fill_bytes(&mut salt);

    let mut nonce = [0u8; FULL_NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce);

    let key = derive_machine_bound_key(machine_key, &salt);
    let cipher = XChaCha20Poly1305::new((&key).into());
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), compressed.as_slice())
        .map_err(|_| anyhow::anyhow!("Failed to encrypt account store"))?;

    let mut out = Vec::with_capacity(4 + 1 + FULL_SALT_LEN + FULL_NONCE_LEN + ciphertext.len());
    out.extend_from_slice(FULL_FILE_MAGIC);
    out.push(FULL_FILE_VERSION_MACHINE_BOUND);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);

    Ok(out)
}

fn decode_full_encrypted_store(file_bytes: &[u8]) -> anyhow::Result<AccountsStore> {
    let version = read_full_encrypted_store_version(file_bytes)?;
    match version {
        FULL_FILE_VERSION_LEGACY => decode_full_encrypted_store_legacy(file_bytes),
        FULL_FILE_VERSION_MACHINE_BOUND => {
            let machine_key = get_full_backup_key().context(
                "This full backup was exported with the new machine-bound format. \
Restore it from the original machine/profile, or import a legacy backup and re-export locally.",
            )?;
            decode_full_encrypted_store_with_key(file_bytes, &machine_key)
        }
        _ => anyhow::bail!("Unsupported encrypted file version: {version}"),
    }
}

fn read_full_encrypted_store_version(file_bytes: &[u8]) -> anyhow::Result<u8> {
    if file_bytes.len() as u64 > MAX_IMPORT_FILE_BYTES {
        anyhow::bail!("Encrypted file is too large");
    }

    let header_len = 4 + 1 + FULL_SALT_LEN + FULL_NONCE_LEN;
    if file_bytes.len() <= header_len {
        anyhow::bail!("Encrypted file is invalid or truncated");
    }

    if &file_bytes[..4] != FULL_FILE_MAGIC {
        anyhow::bail!("Encrypted file header is invalid");
    }

    Ok(file_bytes[4])
}

fn decode_full_encrypted_store_legacy(file_bytes: &[u8]) -> anyhow::Result<AccountsStore> {
    decode_full_encrypted_store_with_derived_key(
        file_bytes,
        |salt| derive_legacy_encryption_key(LEGACY_FULL_PRESET_PASSPHRASE, salt),
        "Failed to decrypt legacy full backup. The file may be corrupted.",
    )
}

fn decode_full_encrypted_store_with_key(
    file_bytes: &[u8],
    machine_key: &[u8],
) -> anyhow::Result<AccountsStore> {
    decode_full_encrypted_store_with_derived_key(
        file_bytes,
        |salt| derive_machine_bound_key(machine_key, salt),
        "Failed to decrypt full backup. The file may be corrupted, or it belongs to a different machine/profile.",
    )
}

fn decode_full_encrypted_store_with_derived_key<F>(
    file_bytes: &[u8],
    derive_key: F,
    decrypt_error: &str,
) -> anyhow::Result<AccountsStore>
where
    F: FnOnce(&[u8]) -> [u8; 32],
{
    let version = read_full_encrypted_store_version(file_bytes)?;
    let salt_start = 5;
    let nonce_start = salt_start + FULL_SALT_LEN;
    let ciphertext_start = nonce_start + FULL_NONCE_LEN;

    let salt = &file_bytes[salt_start..nonce_start];
    let nonce = &file_bytes[nonce_start..ciphertext_start];
    let ciphertext = &file_bytes[ciphertext_start..];

    if version != FULL_FILE_VERSION_LEGACY && version != FULL_FILE_VERSION_MACHINE_BOUND {
        anyhow::bail!("Unsupported encrypted file version: {version}");
    }

    let key = derive_key(salt);
    let cipher = XChaCha20Poly1305::new((&key).into());
    let compressed = cipher
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| anyhow::anyhow!("{decrypt_error}"))?;

    let json = decompress_bytes_with_limit(&compressed, MAX_IMPORT_JSON_BYTES)
        .context("Failed to decompress decrypted payload")?;

    let store: AccountsStore =
        serde_json::from_slice(&json).context("Failed to parse decrypted account payload")?;

    Ok(store)
}

fn derive_machine_bound_key(machine_key: &[u8], salt: &[u8]) -> [u8; 32] {
    use sha2::Digest;

    let mut digest = Sha256::new();
    digest.update(FULL_KEY_CONTEXT);
    digest.update(machine_key);
    digest.update(salt);
    digest.finalize().into()
}

fn derive_legacy_encryption_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, FULL_KDF_ITERATIONS, &mut key);
    key
}

fn compress_bytes(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(input)?;
    encoder.finish().context("Failed to finalize compression")
}

fn decompress_bytes_with_limit(input: &[u8], max_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(input);
    let mut limited = decoder.by_ref().take(max_bytes + 1);
    let mut decompressed = Vec::new();
    limited.read_to_end(&mut decompressed)?;

    if decompressed.len() as u64 > max_bytes {
        anyhow::bail!("Import data is too large");
    }

    Ok(decompressed)
}

fn write_encrypted_file(path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    fs::write(path, bytes).with_context(|| format!("Failed to write file: {path}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("Failed to set file permissions: {path}"))?;
    }

    Ok(())
}

fn read_encrypted_file(path: &str) -> anyhow::Result<Vec<u8>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to read file metadata: {path}"))?;
    if metadata.len() > MAX_IMPORT_FILE_BYTES {
        anyhow::bail!("Encrypted file is too large");
    }

    fs::read(path).with_context(|| format!("Failed to read file: {path}"))
}

fn validate_imported_store(store: &AccountsStore) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    let mut names = HashSet::new();

    for account in &store.accounts {
        if account.id.trim().is_empty() {
            anyhow::bail!("Import contains an account with empty id");
        }
        if account.name.trim().is_empty() {
            anyhow::bail!("Import contains an account with empty name");
        }
        if !ids.insert(account.id.clone()) {
            anyhow::bail!("Import contains duplicate account id: {}", account.id);
        }
        if !names.insert(account.name.clone()) {
            anyhow::bail!("Import contains duplicate account name: {}", account.name);
        }
        crate::auth::validate_stored_account_credentials(account)
            .with_context(|| format!("Import contains invalid credentials for {}", account.name))?;
    }

    if let Some(active_id) = &store.active_account_id {
        if !ids.contains(active_id) {
            anyhow::bail!("Import references a missing active account: {active_id}");
        }
    }

    Ok(())
}

/// Get the list of masked account IDs
#[tauri::command]
pub async fn get_masked_account_ids() -> Result<Vec<String>, String> {
    crate::auth::storage::get_masked_account_ids().map_err(|e| e.to_string())
}

/// Set the list of masked account IDs
#[tauri::command]
pub async fn set_masked_account_ids(ids: Vec<String>) -> Result<(), String> {
    crate::auth::storage::set_masked_account_ids(ids).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::BoxFuture;

    use crate::auth::switcher::{get_codex_auth_file, write_auth_for_test};
    use crate::auth::{add_account, get_account, read_current_auth, RefreshTokenResponse};

    struct TestEnv {
        _config_dir: tempfile::TempDir,
        _codex_home: tempfile::TempDir,
        old_config_dir: Option<String>,
        old_codex_home: Option<String>,
        old_backup_key: Option<String>,
        old_process_override: Option<String>,
    }

    impl TestEnv {
        fn new() -> Self {
            let config_dir = tempfile::tempdir().expect("config temp dir");
            let codex_home = tempfile::tempdir().expect("codex temp dir");
            let old_config_dir = std::env::var("CODEX_SWITCHER_CONFIG_DIR").ok();
            let old_codex_home = std::env::var("CODEX_HOME").ok();
            let old_backup_key = std::env::var("CODEX_SWITCHER_TEST_BACKUP_KEY").ok();
            let old_process_override = std::env::var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT").ok();
            std::env::set_var("CODEX_SWITCHER_CONFIG_DIR", config_dir.path());
            std::env::set_var("CODEX_HOME", codex_home.path());
            std::env::remove_var("CODEX_SWITCHER_TEST_BACKUP_KEY");
            std::env::remove_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT");
            Self {
                _config_dir: config_dir,
                _codex_home: codex_home,
                old_config_dir,
                old_codex_home,
                old_backup_key,
                old_process_override,
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
            if let Some(value) = &self.old_backup_key {
                std::env::set_var("CODEX_SWITCHER_TEST_BACKUP_KEY", value);
            } else {
                std::env::remove_var("CODEX_SWITCHER_TEST_BACKUP_KEY");
            }
            if let Some(value) = &self.old_process_override {
                std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", value);
            } else {
                std::env::remove_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT");
            }
        }
    }

    fn sample_store() -> AccountsStore {
        let account = StoredAccount::new_api_key("Backup".to_string(), "sk-backup".to_string());
        AccountsStore {
            version: 1,
            active_account_id: Some(account.id.clone()),
            accounts: vec![account],
            masked_account_ids: vec![],
        }
    }

    enum FakeRefreshOutcome {
        Success(RefreshTokenResponse),
        Failure(&'static str),
    }

    struct FakeRefreshClient {
        calls: AtomicUsize,
        outcome: FakeRefreshOutcome,
    }

    impl FakeRefreshClient {
        fn success(response: RefreshTokenResponse) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                outcome: FakeRefreshOutcome::Success(response),
            }
        }

        fn failure(message: &'static str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                outcome: FakeRefreshOutcome::Failure(message),
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
        ) -> BoxFuture<'a, anyhow::Result<RefreshTokenResponse>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                match &self.outcome {
                    FakeRefreshOutcome::Success(response) => Ok(response.clone()),
                    FakeRefreshOutcome::Failure(message) => anyhow::bail!(*message),
                }
            })
        }
    }

    struct ProcessStartingRefreshClient {
        response: RefreshTokenResponse,
    }

    impl ChatGptTokenRefreshClient for ProcessStartingRefreshClient {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<RefreshTokenResponse>> {
            Box::pin(async move {
                std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");
                Ok(self.response.clone())
            })
        }
    }

    fn jwt_with_expiry(expiry: i64) -> String {
        let payload = serde_json::json!({ "exp": expiry });
        let encoded =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("serialize expiry"));
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
        let encoded =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("serialize claims"));
        format!("header.{encoded}.signature")
    }

    fn chatgpt_account(access_token: String) -> StoredAccount {
        StoredAccount::new_chatgpt_with_last_refresh(
            "ChatGPT".to_string(),
            Some("user@example.com".to_string()),
            Some("plus".to_string()),
            id_token("user@example.com", "acct-one"),
            access_token,
            "refresh-old".to_string(),
            Some("acct-one".to_string()),
            Some(chrono::Utc::now() - chrono::Duration::days(1)),
        )
    }

    #[test]
    fn new_full_backup_uses_machine_bound_format() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let machine_key = vec![7u8; 32];
        let bytes = encode_full_encrypted_store_with_key(&sample_store(), &machine_key)
            .expect("encode backup");

        assert_eq!(bytes[..4], *FULL_FILE_MAGIC);
        assert_eq!(bytes[4], FULL_FILE_VERSION_MACHINE_BOUND);
    }

    #[test]
    fn machine_bound_backup_round_trip_requires_matching_key() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let store = sample_store();
        let machine_key = vec![9u8; 32];
        let encrypted =
            encode_full_encrypted_store_with_key(&store, &machine_key).expect("encode backup");
        let restored =
            decode_full_encrypted_store_with_key(&encrypted, &machine_key).expect("decode backup");

        assert_eq!(restored.accounts.len(), 1);
        assert_eq!(restored.active_account_id, store.active_account_id);

        let wrong_key = vec![3u8; 32];
        assert!(decode_full_encrypted_store_with_key(&encrypted, &wrong_key).is_err());
    }

    #[test]
    fn legacy_full_backup_import_still_works() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let store = sample_store();
        let json = serde_json::to_vec(&store).expect("serialize");
        let compressed = compress_bytes(&json).expect("compress");
        let salt = [1u8; FULL_SALT_LEN];
        let nonce = [2u8; FULL_NONCE_LEN];
        let key = derive_legacy_encryption_key(LEGACY_FULL_PRESET_PASSPHRASE, &salt);
        let cipher = XChaCha20Poly1305::new((&key).into());
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce), compressed.as_slice())
            .expect("encrypt");

        let mut encrypted = Vec::new();
        encrypted.extend_from_slice(FULL_FILE_MAGIC);
        encrypted.push(FULL_FILE_VERSION_LEGACY);
        encrypted.extend_from_slice(&salt);
        encrypted.extend_from_slice(&nonce);
        encrypted.extend_from_slice(&ciphertext);

        let restored = decode_full_encrypted_store(&encrypted).expect("decode legacy");
        assert_eq!(restored.accounts.len(), 1);
        assert_eq!(restored.accounts[0].name, "Backup");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn account_reads_refuse_unknown_live_credentials() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add account");
        let unknown = serde_json::json!({ "OPENAI_API_KEY": "sk-unknown" });
        std::fs::write(
            get_codex_auth_file().expect("auth path"),
            serde_json::to_vec(&unknown).expect("serialize unknown auth"),
        )
        .expect("write unknown auth");

        assert!(list_accounts().await.is_err());
        assert!(get_active_account_info().await.is_err());
        assert!(export_accounts_slim_text().await.is_err());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn slim_export_imports_newer_live_refresh_token_first() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let stored = add_account(chatgpt_account(jwt_with_expiry(
            chrono::Utc::now().timestamp() + 1800,
        )))
        .expect("add account");
        let mut external = stored.clone();
        external.auth_data = AuthData::ChatGPT {
            id_token: id_token("user@example.com", "acct-one"),
            access_token: jwt_with_expiry(chrono::Utc::now().timestamp() + 3600),
            refresh_token: "refresh-new".to_string(),
            account_id: Some("acct-one".to_string()),
            last_refresh: Some(chrono::Utc::now()),
        };
        write_auth_for_test(&external).expect("write newer live auth");

        let encoded = export_accounts_slim_text()
            .await
            .expect("export reconciled catalog");
        let decoded = decode_slim_payload(&encoded).expect("decode slim export");

        assert!(matches!(
            decoded.accounts[0].refresh_token,
            Some(ref refresh_token) if refresh_token == "refresh-new"
        ));
    }

    #[test]
    fn slim_import_rejects_duplicate_refresh_token_sources() {
        let payload = SlimPayload {
            version: SLIM_FORMAT_VERSION,
            active_name: None,
            accounts: vec![
                SlimAccountPayload {
                    name: "First".to_string(),
                    auth_type: SLIM_AUTH_CHATGPT,
                    api_key: None,
                    refresh_token: Some("shared-refresh".to_string()),
                },
                SlimAccountPayload {
                    name: "Second".to_string(),
                    auth_type: SLIM_AUTH_CHATGPT,
                    api_key: None,
                    refresh_token: Some("shared-refresh".to_string()),
                },
            ],
        };

        let error =
            validate_slim_payload(&payload).expect_err("duplicate refresh token should fail");

        assert!(error.to_string().contains("same refresh token"));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn slim_import_keeps_an_existing_selected_account_as_the_activation_target() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let existing =
            StoredAccount::new_api_key("Existing".to_string(), "sk-existing".to_string());
        let current = AccountsStore {
            version: 1,
            accounts: vec![existing.clone()],
            active_account_id: None,
            masked_account_ids: Vec::new(),
        };
        crate::auth::save_accounts(&current).expect("save current catalog");
        let payload = SlimPayload {
            version: SLIM_FORMAT_VERSION,
            active_name: Some("Existing".to_string()),
            accounts: vec![
                SlimAccountPayload {
                    name: "Existing".to_string(),
                    auth_type: SLIM_AUTH_API_KEY,
                    api_key: Some("sk-existing".to_string()),
                    refresh_token: None,
                },
                SlimAccountPayload {
                    name: "New".to_string(),
                    auth_type: SLIM_AUTH_API_KEY,
                    api_key: Some("sk-new".to_string()),
                    refresh_token: None,
                },
            ],
        };

        let imported = build_store_from_slim_payload(payload, &current)
            .await
            .expect("restore slim payload");

        assert!(imported.activation_target_id.as_deref() == Some(existing.id.as_str()));
        assert!(imported.changed_count == 1);
        let stored = load_accounts().expect("load restored catalog");
        assert!(stored.accounts.iter().any(|account| account.name == "New"));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn slim_import_matches_existing_credentials_independently_of_display_name() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let existing =
            StoredAccount::new_api_key("Local name".to_string(), "sk-existing".to_string());
        let current = AccountsStore {
            version: 1,
            accounts: vec![existing.clone()],
            active_account_id: None,
            masked_account_ids: Vec::new(),
        };
        crate::auth::save_accounts(&current).expect("save current catalog");
        let payload = SlimPayload {
            version: SLIM_FORMAT_VERSION,
            active_name: Some("Exported name".to_string()),
            accounts: vec![SlimAccountPayload {
                name: "Exported name".to_string(),
                auth_type: SLIM_AUTH_API_KEY,
                api_key: Some("sk-existing".to_string()),
                refresh_token: None,
            }],
        };

        let imported = build_store_from_slim_payload(payload, &current)
            .await
            .expect("match existing credentials");

        assert!(imported.changed_count == 0);
        assert!(imported.activation_target_id.as_deref() == Some(existing.id.as_str()));
        let stored = load_accounts().expect("load unchanged catalog");
        assert!(stored.accounts.len() == 1);
        assert!(stored.accounts[0].name == "Local name");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn slim_import_rejects_same_name_with_different_credentials() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let existing =
            StoredAccount::new_api_key("Personal".to_string(), "sk-existing".to_string());
        let current = AccountsStore {
            version: 1,
            accounts: vec![existing],
            active_account_id: None,
            masked_account_ids: Vec::new(),
        };
        crate::auth::save_accounts(&current).expect("save current catalog");
        let payload = SlimPayload {
            version: SLIM_FORMAT_VERSION,
            active_name: Some("Personal".to_string()),
            accounts: vec![SlimAccountPayload {
                name: "Personal".to_string(),
                auth_type: SLIM_AUTH_API_KEY,
                api_key: Some("sk-different".to_string()),
                refresh_token: None,
            }],
        };

        let result = build_store_from_slim_payload(payload, &current).await;

        assert!(result.is_err());
        let stored = load_accounts().expect("load unchanged catalog");
        assert!(stored.accounts.len() == 1);
        assert!(stored.active_account_id.is_none());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn slim_import_activates_from_latest_catalog_state() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let existing =
            StoredAccount::new_api_key("Existing".to_string(), "sk-existing".to_string());
        crate::auth::save_accounts(&AccountsStore {
            version: 1,
            accounts: vec![existing],
            active_account_id: None,
            masked_account_ids: Vec::new(),
        })
        .expect("save inactive catalog");
        let imported =
            StoredAccount::new_api_key("Imported".to_string(), "sk-imported".to_string());
        let payload = encode_slim_payload_from_store(&AccountsStore {
            version: 1,
            accounts: vec![imported.clone()],
            active_account_id: Some(imported.id.clone()),
            masked_account_ids: Vec::new(),
        })
        .expect("encode slim payload");
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        import_accounts_slim_text(payload)
            .await
            .expect("import and activate from latest state");

        let store = load_accounts().expect("load store");
        let stored_imported = store
            .accounts
            .iter()
            .find(|account| account.name == "Imported")
            .expect("find imported account");
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(stored_imported.id.as_str())
        );
        let live = read_current_auth()
            .expect("read live auth")
            .expect("live auth exists");
        assert!(matches!(live.openai_api_key, Some(ref key) if key == "sk-imported"));
    }

    // These tests intentionally hold the env lock across await to serialize
    // process-wide environment mutation while the async command reads it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn switch_account_rejects_when_codex_is_running() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let stored = add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add account");
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "1");

        let result = switch_account(stored.id).await;
        assert!(result.is_err());
        assert!(result
            .expect_err("switch should fail")
            .contains("Close all running Codex windows"));
    }

    // These tests intentionally hold the env lock across await to serialize
    // process-wide environment mutation while the async command reads it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn switch_account_refreshes_expired_chatgpt_before_writing_auth() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let primary = add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add primary account");
        let target = add_account(chatgpt_account(jwt_with_expiry(
            chrono::Utc::now().timestamp() - 60,
        )))
        .expect("add target account");
        let client = FakeRefreshClient::success(RefreshTokenResponse {
            id_token: Some(id_token("user@example.com", "acct-one")),
            access_token: jwt_with_expiry(chrono::Utc::now().timestamp() + 3600),
            refresh_token: Some("refresh-new".to_string()),
        });
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        switch_account_with_client(target.id.clone(), &client)
            .await
            .expect("switch should succeed");

        assert_eq!(client.call_count(), 1);
        let store = load_accounts().expect("load accounts");
        assert_eq!(store.active_account_id.as_deref(), Some(target.id.as_str()));
        assert_ne!(
            store.active_account_id.as_deref(),
            Some(primary.id.as_str())
        );
        let stored_target = get_account(&target.id)
            .expect("load target")
            .expect("target should exist");
        assert!(matches!(
            stored_target.auth_data,
            AuthData::ChatGPT { refresh_token, .. } if refresh_token == "refresh-new"
        ));
        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");
        assert!(matches!(
            auth.tokens,
            Some(crate::types::TokenData { refresh_token, .. }) if refresh_token == "refresh-new"
        ));
    }

    // These tests intentionally hold the env lock across await to serialize
    // process-wide environment mutation while the async command reads it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn switch_account_refresh_failure_preserves_previous_active_auth() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let primary = add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add primary account");
        write_auth_for_test(&primary).expect("write primary auth");
        let target = add_account(chatgpt_account(jwt_with_expiry(
            chrono::Utc::now().timestamp() - 60,
        )))
        .expect("add target account");
        let auth_before = serde_json::to_value(
            read_current_auth()
                .expect("read auth")
                .expect("auth should exist"),
        )
        .expect("serialize auth");
        let client = FakeRefreshClient::failure("provider rejected refresh");
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        let result = switch_account_with_client(target.id.clone(), &client).await;

        assert!(result.is_err());
        assert_eq!(client.call_count(), 1);
        let store = load_accounts().expect("load accounts");
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(primary.id.as_str())
        );
        let auth_after = serde_json::to_value(
            read_current_auth()
                .expect("read auth")
                .expect("auth should exist"),
        )
        .expect("serialize auth");
        assert!(auth_after == auth_before, "live auth changed unexpectedly");
        let store = load_accounts().expect("load accounts");
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(primary.id.as_str())
        );
        let stored_target = get_account(&target.id)
            .expect("load target")
            .expect("target should exist");
        assert!(matches!(
            stored_target.auth_data,
            AuthData::ChatGPT { refresh_token, .. } if refresh_token == "refresh-old"
        ));
    }

    // These tests intentionally hold the env lock across await to serialize
    // process-wide environment mutation while the async command reads it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn refresh_does_not_publish_auth_before_final_process_check() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        let primary = add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add primary account");
        write_auth_for_test(&primary).expect("write primary auth");
        let target = add_account(chatgpt_account(jwt_with_expiry(
            chrono::Utc::now().timestamp() - 60,
        )))
        .expect("add target account");
        let auth_before = serde_json::to_value(
            read_current_auth()
                .expect("read auth")
                .expect("auth should exist"),
        )
        .expect("serialize auth");
        let client = ProcessStartingRefreshClient {
            response: RefreshTokenResponse {
                id_token: Some(id_token("user@example.com", "acct-one")),
                access_token: jwt_with_expiry(chrono::Utc::now().timestamp() + 3600),
                refresh_token: Some("refresh-new".to_string()),
            },
        };
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        let result = switch_account_with_client(target.id.clone(), &client).await;

        assert!(result.is_err());
        assert!(result
            .expect_err("switch should fail")
            .contains("Close all running Codex windows"));
        let auth_after = serde_json::to_value(
            read_current_auth()
                .expect("read auth")
                .expect("auth should exist"),
        )
        .expect("serialize auth");
        assert!(auth_after == auth_before, "live auth changed unexpectedly");
        let store = load_accounts().expect("load accounts");
        assert_eq!(
            store.active_account_id.as_deref(),
            Some(primary.id.as_str())
        );
        let stored_target = get_account(&target.id)
            .expect("load target")
            .expect("target should exist");
        assert!(matches!(
            stored_target.auth_data,
            AuthData::ChatGPT { refresh_token, .. } if refresh_token == "refresh-new"
        ));
    }

    // These tests intentionally hold the env lock across await to serialize
    // process-wide environment mutation while the async command reads it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn switch_account_with_fresh_chatgpt_token_skips_refresh() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();
        add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add primary account");
        let target = add_account(chatgpt_account(jwt_with_expiry(
            chrono::Utc::now().timestamp() + 3600,
        )))
        .expect("add target account");
        let client = FakeRefreshClient::failure("refresh must not be called");
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        switch_account_with_client(target.id.clone(), &client)
            .await
            .expect("switch should succeed");

        assert_eq!(client.call_count(), 0);
        let store = load_accounts().expect("load accounts");
        assert_eq!(store.active_account_id.as_deref(), Some(target.id.as_str()));
    }

    // These tests intentionally hold the env lock across await to serialize
    // process-wide environment mutation while the async command reads it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn restart_codex_and_switch_account_switches_when_no_processes_are_running() {
        let _guard = crate::test_support::env_lock();
        let _env = TestEnv::new();

        let _first = add_account(StoredAccount::new_api_key(
            "Primary".to_string(),
            "sk-primary".to_string(),
        ))
        .expect("add first account");
        let second = add_account(StoredAccount::new_api_key(
            "Secondary".to_string(),
            "sk-secondary".to_string(),
        ))
        .expect("add second account");
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");

        restart_codex_and_switch_account(second.id.clone())
            .await
            .expect("restart switch should succeed");

        let auth = read_current_auth()
            .expect("read auth")
            .expect("auth should exist");
        assert!(matches!(
            auth.openai_api_key.as_deref(),
            Some("sk-secondary")
        ));
    }
}

//! Codex auth.json parsing, identity resolution, and guarded publication support.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use super::atomic_file::{
    read_snapshot, recover_externally_mutable_file, restore_if_matches, stage_file_change,
    FileSnapshot, StagedFileChange,
};
use crate::types::{AccountsStore, AuthData, AuthDotJson, AuthMode, StoredAccount, TokenData};

/// Exact raw state of Codex's externally mutable auth.json file.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AuthSnapshot {
    file: FileSnapshot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveAuthState {
    Missing,
    Exact,
    Newer,
    Stale,
}

pub(crate) struct ReconcileOutcome {
    pub(crate) state: LiveAuthState,
    pub(crate) matched_account_id: Option<String>,
    pub(crate) catalog_changed: bool,
}

/// Get the official Codex home directory.
pub fn get_codex_home() -> Result<PathBuf> {
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        return Ok(PathBuf::from(codex_home));
    }

    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(".codex"))
}

/// Get the path to the official auth.json file.
pub fn get_codex_auth_file() -> Result<PathBuf> {
    Ok(get_codex_home()?.join("auth.json"))
}

pub(crate) fn capture_auth_snapshot() -> Result<AuthSnapshot> {
    let path = get_codex_auth_file()?;
    recover_externally_mutable_file(&path)?;
    Ok(AuthSnapshot {
        file: read_snapshot(&path)?,
    })
}

pub(crate) fn missing_auth_snapshot() -> AuthSnapshot {
    AuthSnapshot {
        file: FileSnapshot::Missing,
    }
}

pub(crate) fn auth_snapshot_for_account(account: &StoredAccount) -> Result<AuthSnapshot> {
    validate_stored_account_credentials(account)?;
    let auth = create_auth_json(account)?;
    let bytes = serde_json::to_vec_pretty(&auth).context("Failed to serialize auth.json")?;
    Ok(AuthSnapshot {
        file: FileSnapshot::present(bytes),
    })
}

pub(crate) fn stage_auth_publication(
    expected: &AuthSnapshot,
    desired: &AuthSnapshot,
) -> Result<StagedFileChange> {
    stage_file_change(
        &get_codex_auth_file()?,
        expected.file.clone(),
        desired.file.clone(),
        "auth",
    )
}

pub(crate) fn rollback_auth_publication(
    expected_published: &AuthSnapshot,
    previous: &AuthSnapshot,
) -> Result<()> {
    restore_if_matches(
        &get_codex_auth_file()?,
        expected_published.file.clone(),
        previous.file.clone(),
    )
}

pub(crate) fn auth_snapshot_matches_current(snapshot: &AuthSnapshot) -> Result<bool> {
    let path = get_codex_auth_file()?;
    recover_externally_mutable_file(&path)?;
    Ok(read_snapshot(&path)? == snapshot.file)
}

pub(crate) fn reconcile_live_auth(
    store: &mut AccountsStore,
    snapshot: &AuthSnapshot,
) -> Result<ReconcileOutcome> {
    let resolution = resolve_live_auth(store, snapshot)?;
    let (state, matched_account_id, updated_account) = match resolution {
        LiveAuthResolution::Missing => (LiveAuthState::Missing, None, None),
        LiveAuthResolution::Exact { account_id } => (LiveAuthState::Exact, Some(account_id), None),
        LiveAuthResolution::Newer {
            account_id,
            updated_account,
        } => (
            LiveAuthState::Newer,
            Some(account_id),
            Some(updated_account),
        ),
        LiveAuthResolution::Stale { account_id } => (LiveAuthState::Stale, Some(account_id), None),
    };

    let mut catalog_changed = false;
    if let Some(updated_account) = updated_account {
        let existing = store
            .accounts
            .iter_mut()
            .find(|account| account.id == updated_account.id)
            .context("Matched live account disappeared from the catalog")?;
        *existing = *updated_account;
        catalog_changed = true;
    }

    if state == LiveAuthState::Exact {
        let account_id = matched_account_id
            .as_deref()
            .context("Exact live credentials did not identify an account")?;
        let existing = store
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
            .context("Matched live account disappeared from the catalog")?;
        if !existing.previous_chatgpt_credential_hashes.is_empty() {
            existing.previous_chatgpt_credential_hashes.clear();
            catalog_changed = true;
        }
    }

    if let Some(account_id) = matched_account_id.as_deref() {
        if store.active_account_id.as_deref() != Some(account_id) {
            store.active_account_id = Some(account_id.to_string());
            catalog_changed = true;
        }
    } else if state == LiveAuthState::Missing && store.active_account_id.is_some() {
        store.active_account_id = None;
        catalog_changed = true;
    }

    let outcome = ReconcileOutcome {
        state,
        matched_account_id,
        catalog_changed,
    };
    debug_assert!(
        (outcome.state == LiveAuthState::Missing) == outcome.matched_account_id.is_none()
    );
    Ok(outcome)
}

pub(crate) fn validate_catalog_credential_uniqueness(store: &AccountsStore) -> Result<()> {
    for account in &store.accounts {
        validate_stored_account_credentials(account)?;
    }

    for (index, account) in store.accounts.iter().enumerate() {
        for other in &store.accounts[index + 1..] {
            if stored_accounts_share_credentials(account, other)? {
                anyhow::bail!(
                    "Accounts '{}' and '{}' contain credentials for the same login",
                    account.name,
                    other.name
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn stored_accounts_share_credentials(
    first: &StoredAccount,
    second: &StoredAccount,
) -> Result<bool> {
    match (&first.auth_data, &second.auth_data) {
        (AuthData::ApiKey { key: first }, AuthData::ApiKey { key: second }) => Ok(first == second),
        (
            AuthData::ChatGPT {
                id_token: first_id,
                access_token: first_access,
                refresh_token: first_refresh,
                account_id: first_account_id,
                ..
            },
            AuthData::ChatGPT {
                id_token: second_id,
                access_token: second_access,
                refresh_token: second_refresh,
                account_id: second_account_id,
                ..
            },
        ) => {
            let exact_bundle = first_id == second_id
                && first_access == second_access
                && first_refresh == second_refresh;
            let first_identity =
                resolved_chatgpt_account_id(first_account_id.as_deref(), first_id)?;
            let second_identity =
                resolved_chatgpt_account_id(second_account_id.as_deref(), second_id)?;
            Ok(exact_bundle
                || first_refresh == second_refresh
                || first_identity.is_some() && first_identity == second_identity)
        }
        _ => Ok(false),
    }
}

fn resolve_live_auth(store: &AccountsStore, snapshot: &AuthSnapshot) -> Result<LiveAuthResolution> {
    validate_catalog_credential_uniqueness(store)?;
    let Some(live_auth) = parse_auth_snapshot(snapshot)? else {
        return Ok(LiveAuthResolution::Missing);
    };

    let exact_matches = store
        .accounts
        .iter()
        .filter(|account| stored_auth_matches_exact(account, &live_auth))
        .collect::<Vec<_>>();

    if exact_matches.len() > 1 {
        anyhow::bail!(
            "Codex auth.json matches more than one stored account. Remove the duplicate account before switching."
        );
    }
    if let Some(account) = exact_matches.first() {
        return Ok(LiveAuthResolution::Exact {
            account_id: account.id.clone(),
        });
    }

    if live_auth.openai_api_key.is_some() {
        anyhow::bail!(
            "Codex auth.json contains an API key that is not in the account catalog. Import it or sign out of Codex before switching."
        );
    }

    let live_tokens = live_auth
        .tokens
        .as_ref()
        .context("Validated ChatGPT auth was missing tokens")?;
    let live_account_id = resolved_chatgpt_account_id(
        live_tokens.account_id.as_deref(),
        &live_tokens.id_token,
    )?
    .context(
        "Codex auth.json changed but has no stable ChatGPT account ID. Sign in again before switching accounts.",
    )?;

    let mut identity_matches = Vec::new();
    for account in &store.accounts {
        let AuthData::ChatGPT {
            id_token,
            account_id,
            ..
        } = &account.auth_data
        else {
            continue;
        };
        if account.auth_mode != AuthMode::ChatGPT {
            continue;
        }

        let stored_account_id = resolved_chatgpt_account_id(account_id.as_deref(), id_token)
            .with_context(|| {
                format!(
                    "Stored account '{}' has conflicting ChatGPT account IDs",
                    account.name
                )
            })?;
        if stored_account_id.as_deref() == Some(live_account_id.as_str()) {
            identity_matches.push(account);
        }
    }

    if identity_matches.is_empty() {
        anyhow::bail!(
            "Codex auth.json belongs to an account that is not in the catalog. Import it or sign out of Codex before switching."
        );
    }
    if identity_matches.len() > 1 {
        anyhow::bail!(
            "More than one stored account has the ChatGPT identity used by Codex. Remove the duplicate account before switching."
        );
    }

    let stored_account = identity_matches[0];
    let live_auth_data = AuthData::ChatGPT {
        id_token: live_tokens.id_token.clone(),
        access_token: live_tokens.access_token.clone(),
        refresh_token: live_tokens.refresh_token.clone(),
        account_id: Some(live_account_id),
        last_refresh: live_auth.last_refresh,
    };
    let live_credential_hash =
        super::storage::chatgpt_auth_data_fingerprint(&live_auth_data)?.encoded();
    if stored_account
        .previous_chatgpt_credential_hashes
        .contains(&live_credential_hash)
    {
        return Ok(LiveAuthResolution::Stale {
            account_id: stored_account.id.clone(),
        });
    }

    if live_auth.last_refresh.is_none()
        || !matches!(
            &stored_account.auth_data,
            AuthData::ChatGPT {
                last_refresh: Some(_),
                ..
            }
        )
    {
        anyhow::bail!(
            "Codex auth.json credentials changed without complete refresh timestamps. Sign in again before switching accounts."
        );
    }

    validate_live_chatgpt_tokens_for_catalog_import(live_tokens)?;
    let mut updated_account = stored_account.clone();
    let (email, plan_type) = parse_id_token_claims(&live_tokens.id_token);
    updated_account.auth_data = live_auth_data;
    updated_account.previous_chatgpt_credential_hashes.clear();
    if let Some(email) = email {
        updated_account.email = Some(email);
    }
    if let Some(plan_type) = plan_type {
        updated_account.plan_type = Some(plan_type);
    }

    Ok(LiveAuthResolution::Newer {
        account_id: stored_account.id.clone(),
        updated_account: Box::new(updated_account),
    })
}

enum LiveAuthResolution {
    Missing,
    Exact {
        account_id: String,
    },
    Newer {
        account_id: String,
        updated_account: Box<StoredAccount>,
    },
    Stale {
        account_id: String,
    },
}

fn stored_auth_matches_exact(account: &StoredAccount, live_auth: &AuthDotJson) -> bool {
    match (&account.auth_data, &account.auth_mode) {
        (AuthData::ApiKey { key }, AuthMode::ApiKey) => {
            live_auth.tokens.is_none() && live_auth.openai_api_key.as_deref() == Some(key.as_str())
        }
        (
            AuthData::ChatGPT {
                id_token,
                access_token,
                refresh_token,
                account_id,
                ..
            },
            AuthMode::ChatGPT,
        ) => {
            live_auth.openai_api_key.is_none()
                && live_auth.tokens.as_ref().is_some_and(|tokens| {
                    let stored_identity =
                        resolved_chatgpt_account_id(account_id.as_deref(), id_token).ok();
                    let live_identity =
                        resolved_chatgpt_account_id(tokens.account_id.as_deref(), &tokens.id_token)
                            .ok();
                    tokens.id_token == *id_token
                        && tokens.access_token == *access_token
                        && tokens.refresh_token == *refresh_token
                        && stored_identity == live_identity
                })
        }
        _ => false,
    }
}

fn parse_auth_snapshot(snapshot: &AuthSnapshot) -> Result<Option<AuthDotJson>> {
    let Some(bytes) = snapshot.file.bytes() else {
        return Ok(None);
    };
    parse_auth_bytes(bytes).map(Some)
}

fn parse_auth_bytes(bytes: &[u8]) -> Result<AuthDotJson> {
    let auth: AuthDotJson =
        serde_json::from_slice(bytes).context("Failed to parse Codex auth.json")?;
    validate_auth_dot_json(&auth)?;
    Ok(auth)
}

fn validate_auth_dot_json(auth: &AuthDotJson) -> Result<()> {
    match (auth.openai_api_key.as_deref(), auth.tokens.as_ref()) {
        (Some(key), None) if !key.trim().is_empty() => Ok(()),
        (Some(_), None) => anyhow::bail!("Codex auth.json contains an empty API key"),
        (None, Some(tokens)) => {
            if tokens.id_token.trim().is_empty()
                || tokens.access_token.trim().is_empty()
                || tokens.refresh_token.trim().is_empty()
            {
                anyhow::bail!("Codex auth.json contains incomplete ChatGPT credentials");
            }
            resolved_chatgpt_account_id(tokens.account_id.as_deref(), &tokens.id_token)?;
            Ok(())
        }
        (Some(_), Some(_)) => anyhow::bail!(
            "Codex auth.json contains both API-key and ChatGPT credentials. Sign out before switching accounts."
        ),
        (None, None) => anyhow::bail!("Codex auth.json contains no usable credentials"),
    }
}

fn validate_live_chatgpt_tokens_for_catalog_import(tokens: &TokenData) -> Result<()> {
    if !jwt_payload_is_json(&tokens.id_token) || jwt_expiration(&tokens.access_token).is_none() {
        anyhow::bail!(
            "Codex auth.json contains malformed ChatGPT tokens and cannot replace stored credentials"
        );
    }
    Ok(())
}

pub(crate) fn validate_stored_account_credentials(account: &StoredAccount) -> Result<()> {
    match (&account.auth_mode, &account.auth_data) {
        (AuthMode::ApiKey, AuthData::ApiKey { key }) => {
            if key.trim().is_empty() {
                anyhow::bail!("API key is missing for account {}", account.name);
            }
        }
        (
            AuthMode::ChatGPT,
            AuthData::ChatGPT {
                id_token,
                access_token,
                refresh_token,
                account_id,
                ..
            },
        ) => {
            if id_token.trim().is_empty() {
                anyhow::bail!("ID token is missing for account {}", account.name);
            }
            if access_token.trim().is_empty() {
                anyhow::bail!("Access token is missing for account {}", account.name);
            }
            if refresh_token.trim().is_empty() {
                anyhow::bail!("Refresh token is missing for account {}", account.name);
            }
            resolved_chatgpt_account_id(account_id.as_deref(), id_token).with_context(|| {
                format!(
                    "Account '{}' has conflicting ChatGPT account IDs",
                    account.name
                )
            })?;
        }
        _ => anyhow::bail!(
            "Authentication mode does not match credentials for account {}",
            account.name
        ),
    }

    Ok(())
}

fn create_auth_json(account: &StoredAccount) -> Result<AuthDotJson> {
    match &account.auth_data {
        AuthData::ApiKey { key } => Ok(AuthDotJson {
            openai_api_key: Some(key.clone()),
            tokens: None,
            last_refresh: None,
        }),
        AuthData::ChatGPT {
            id_token,
            access_token,
            refresh_token,
            account_id,
            last_refresh,
        } => Ok(AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: id_token.clone(),
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                account_id: account_id.clone(),
            }),
            last_refresh: *last_refresh,
        }),
    }
}

/// Import an account from an existing auth.json file.
pub fn import_from_auth_json(path: &str, account_name: String) -> Result<StoredAccount> {
    let content = fs::read(path).with_context(|| format!("Failed to read auth.json: {path}"))?;
    import_from_auth_json_bytes(&content, account_name)
        .with_context(|| format!("Failed to parse auth.json: {path}"))
}

/// Import an account from auth.json file contents.
pub fn import_from_auth_json_contents(
    content: &str,
    account_name: String,
) -> Result<StoredAccount> {
    import_from_auth_json_bytes(content.as_bytes(), account_name)
        .context("Failed to parse auth.json contents")
}

fn import_from_auth_json_bytes(bytes: &[u8], account_name: String) -> Result<StoredAccount> {
    let auth = parse_auth_bytes(bytes)?;
    let AuthDotJson {
        openai_api_key,
        tokens,
        last_refresh,
    } = auth;

    if let Some(api_key) = openai_api_key {
        return Ok(StoredAccount::new_api_key(account_name, api_key));
    }

    let tokens = tokens.context("Validated ChatGPT auth was missing tokens")?;
    let (email, plan_type) = parse_id_token_claims(&tokens.id_token);
    Ok(StoredAccount::new_chatgpt_with_last_refresh(
        account_name,
        email,
        plan_type,
        tokens.id_token,
        tokens.access_token,
        tokens.refresh_token,
        tokens.account_id,
        last_refresh,
    ))
}

/// Parse claims from a JWT ID token without validating its signature.
pub(crate) fn parse_id_token_claims(id_token: &str) -> (Option<String>, Option<String>) {
    let metadata = parse_id_token_metadata(id_token);
    (metadata.email, metadata.plan_type)
}

/// Parse the ChatGPT account identity embedded in a JWT ID token.
pub(crate) fn parse_id_token_account_id(id_token: &str) -> Option<String> {
    parse_id_token_metadata(id_token).account_id
}

/// Parse the end of the current ChatGPT subscription entitlement from an ID token.
pub(crate) fn parse_id_token_subscription_active_until(id_token: &str) -> Option<DateTime<Utc>> {
    parse_id_token_metadata(id_token).subscription_active_until
}

pub(crate) fn resolved_chatgpt_account_id(
    explicit_account_id: Option<&str>,
    id_token: &str,
) -> Result<Option<String>> {
    let explicit = explicit_account_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(String::from);
    let embedded = parse_id_token_account_id(id_token)
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    match (explicit, embedded) {
        (Some(explicit), Some(embedded)) if explicit != embedded => {
            anyhow::bail!("Explicit and embedded ChatGPT account IDs conflict")
        }
        (Some(explicit), _) => Ok(Some(explicit)),
        (None, Some(embedded)) => Ok(Some(embedded)),
        (None, None) => Ok(None),
    }
}

pub(crate) fn jwt_payload_is_json(token: &str) -> bool {
    parse_jwt_payload(token).is_some()
}

pub(crate) fn jwt_expiration(token: &str) -> Option<i64> {
    parse_jwt_payload(token)?
        .get("exp")
        .and_then(serde_json::Value::as_i64)
}

fn parse_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let payload =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload).ok()?;
    serde_json::from_slice(&payload).ok()
}

#[derive(Default)]
struct ChatGptIdTokenMetadata {
    email: Option<String>,
    plan_type: Option<String>,
    account_id: Option<String>,
    subscription_active_until: Option<DateTime<Utc>>,
}

fn parse_id_token_metadata(id_token: &str) -> ChatGptIdTokenMetadata {
    let Some(json) = parse_jwt_payload(id_token) else {
        return ChatGptIdTokenMetadata::default();
    };

    let email = json
        .get("email")
        .and_then(|value| value.as_str())
        .map(String::from);
    let auth_claims = json.get("https://api.openai.com/auth");
    let plan_type = auth_claims
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(|value| value.as_str())
        .map(String::from);
    let account_id = auth_claims
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|value| value.as_str())
        .map(String::from);
    let subscription_active_until = auth_claims
        .and_then(|auth| auth.get("chatgpt_subscription_active_until"))
        .and_then(|value| value.as_str())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));

    ChatGptIdTokenMetadata {
        email,
        plan_type,
        account_id,
        subscription_active_until,
    }
}

/// Read and strictly validate the current auth.json file if it exists.
pub fn read_current_auth() -> Result<Option<AuthDotJson>> {
    parse_auth_snapshot(&capture_auth_snapshot()?)
}

/// Check if there is an active Codex login.
pub fn has_active_login() -> Result<bool> {
    Ok(read_current_auth()?.is_some())
}

#[cfg(test)]
pub(crate) fn write_auth_for_test(account: &StoredAccount) -> Result<()> {
    let expected = capture_auth_snapshot()?;
    let desired = auth_snapshot_for_account(account)?;
    stage_auth_publication(&expected, &desired)?.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn id_token(email: &str, account_id: Option<&str>) -> String {
        let auth = account_id
            .map(|id| serde_json::json!({ "chatgpt_account_id": id, "chatgpt_plan_type": "plus" }))
            .unwrap_or_else(|| serde_json::json!({ "chatgpt_plan_type": "plus" }));
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": auth,
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize claims"));
        format!("header.{encoded}.signature")
    }

    fn access_token(expiry: i64) -> String {
        let payload = serde_json::json!({ "exp": expiry });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize expiry"));
        format!("header.{encoded}.signature")
    }

    #[test]
    fn parses_subscription_active_until_without_using_token_expiry() {
        let payload = serde_json::json!({
            "exp": 1_900_000_000,
            "https://api.openai.com/auth": {
                "chatgpt_subscription_active_until": "2026-09-12T05:30:00Z"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).expect("serialize subscription claim"));
        let token = format!("header.{encoded}.signature");

        let active_until = parse_id_token_subscription_active_until(&token)
            .expect("subscription claim should parse");

        assert_eq!(active_until.to_rfc3339(), "2026-09-12T05:30:00+00:00");
        assert_ne!(active_until.timestamp(), 1_900_000_000);
    }

    fn chatgpt_account(name: &str, account_id: Option<&str>) -> StoredAccount {
        StoredAccount::new_chatgpt_with_last_refresh(
            name.to_string(),
            Some("shared@example.com".to_string()),
            Some("plus".to_string()),
            id_token("shared@example.com", account_id),
            access_token(chrono::Utc::now().timestamp() + 3600),
            "refresh-stored".to_string(),
            account_id.map(String::from),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-20T00:00:00Z")
                    .unwrap()
                    .into(),
            ),
        )
    }

    fn snapshot_for_auth(auth: &AuthDotJson) -> AuthSnapshot {
        AuthSnapshot {
            file: FileSnapshot::present(serde_json::to_vec(auth).expect("serialize auth")),
        }
    }

    #[test]
    fn exact_bundle_resolves_without_timestamps() {
        let mut account = chatgpt_account("Exact", Some("acct-one"));
        if let AuthData::ChatGPT { last_refresh, .. } = &mut account.auth_data {
            *last_refresh = None;
        }
        let auth = create_auth_json(&account).expect("create auth");
        let mut store = AccountsStore {
            accounts: vec![account.clone()],
            ..AccountsStore::default()
        };

        let outcome = reconcile_live_auth(&mut store, &snapshot_for_auth(&auth)).expect("resolve");

        assert!(outcome.state == LiveAuthState::Exact);
        assert_eq!(
            outcome.matched_account_id.as_deref(),
            Some(account.id.as_str())
        );
    }

    #[test]
    fn changed_bundle_without_stable_identity_does_not_fall_back_to_email() {
        let account = chatgpt_account("Stored", None);
        let auth = AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: id_token("shared@example.com", None),
                access_token: "access-live".to_string(),
                refresh_token: "refresh-live".to_string(),
                account_id: None,
            }),
            last_refresh: Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-21T00:00:00Z")
                    .unwrap()
                    .into(),
            ),
        };
        let mut store = AccountsStore {
            accounts: vec![account],
            ..AccountsStore::default()
        };

        let error = reconcile_live_auth(&mut store, &snapshot_for_auth(&auth))
            .err()
            .expect("identity-less change should fail");

        assert!(error.to_string().contains("no stable ChatGPT account ID"));
    }

    #[test]
    fn changed_bundle_with_equal_timestamp_is_imported_as_external_state() {
        let account = chatgpt_account("Stored", Some("acct-one"));
        let auth = AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: id_token("shared@example.com", Some("acct-one")),
                access_token: access_token(chrono::Utc::now().timestamp() + 7200),
                refresh_token: "refresh-live".to_string(),
                account_id: Some("acct-one".to_string()),
            }),
            last_refresh: Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-20T00:00:00Z")
                    .unwrap()
                    .into(),
            ),
        };
        let mut store = AccountsStore {
            accounts: vec![account],
            ..AccountsStore::default()
        };

        let outcome =
            reconcile_live_auth(&mut store, &snapshot_for_auth(&auth)).expect("import live auth");

        assert!(outcome.state == LiveAuthState::Newer);
        assert!(matches!(
            store.accounts[0].auth_data,
            AuthData::ChatGPT { ref refresh_token, .. } if refresh_token == "refresh-live"
        ));
    }

    #[test]
    fn clock_rollback_does_not_replace_newer_external_credentials() {
        let account = chatgpt_account("Stored", Some("acct-one"));
        let auth = AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: id_token("shared@example.com", Some("acct-one")),
                access_token: access_token(chrono::Utc::now().timestamp() + 7200),
                refresh_token: "refresh-after-clock-rollback".to_string(),
                account_id: Some("acct-one".to_string()),
            }),
            last_refresh: Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-19T00:00:00Z")
                    .unwrap()
                    .into(),
            ),
        };
        let mut store = AccountsStore {
            accounts: vec![account],
            ..AccountsStore::default()
        };

        let outcome =
            reconcile_live_auth(&mut store, &snapshot_for_auth(&auth)).expect("import live auth");

        assert!(outcome.state == LiveAuthState::Newer);
        assert!(matches!(
            store.accounts[0].auth_data,
            AuthData::ChatGPT { ref refresh_token, .. }
                if refresh_token == "refresh-after-clock-rollback"
        ));
    }

    #[test]
    fn malformed_changed_live_tokens_cannot_replace_stored_credentials() {
        let account = chatgpt_account("Stored", Some("acct-one"));
        let auth = AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: "malformed-id-token".to_string(),
                access_token: "malformed-access-token".to_string(),
                refresh_token: "refresh-live".to_string(),
                account_id: Some("acct-one".to_string()),
            }),
            last_refresh: Some(chrono::Utc::now()),
        };
        let mut store = AccountsStore {
            accounts: vec![account],
            ..AccountsStore::default()
        };

        let error = reconcile_live_auth(&mut store, &snapshot_for_auth(&auth))
            .err()
            .expect("malformed live auth should fail");

        assert!(error.to_string().contains("malformed ChatGPT tokens"));
        assert!(matches!(
            store.accounts[0].auth_data,
            AuthData::ChatGPT { ref refresh_token, .. } if refresh_token == "refresh-stored"
        ));
    }

    #[test]
    fn mixed_auth_modes_are_rejected() {
        let auth = AuthDotJson {
            openai_api_key: Some("sk-test".to_string()),
            tokens: Some(TokenData {
                id_token: id_token("user@example.com", Some("acct-one")),
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some("acct-one".to_string()),
            }),
            last_refresh: None,
        };

        assert!(validate_auth_dot_json(&auth).is_err());
    }
}

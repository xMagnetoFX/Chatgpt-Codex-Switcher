//! Account switching logic - writes credentials to ~/.codex/auth.json

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

use crate::types::{AuthData, AuthDotJson, StoredAccount, TokenData};

/// Get the official Codex home directory
pub fn get_codex_home() -> Result<PathBuf> {
    // Check for CODEX_HOME environment variable first
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        return Ok(PathBuf::from(codex_home));
    }

    let home = dirs::home_dir().context("Could not find home directory")?;
    Ok(home.join(".codex"))
}

/// Get the path to the official auth.json file
pub fn get_codex_auth_file() -> Result<PathBuf> {
    Ok(get_codex_home()?.join("auth.json"))
}

/// Remove the official auth.json file if it exists.
pub fn clear_current_auth() -> Result<()> {
    let path = get_codex_auth_file()?;
    if !path.exists() {
        return Ok(());
    }

    fs::remove_file(&path)
        .with_context(|| format!("Failed to remove auth.json: {}", path.display()))?;
    Ok(())
}

/// Switch to a specific account by writing its credentials to ~/.codex/auth.json
pub fn switch_to_account(account: &StoredAccount) -> Result<()> {
    let codex_home = get_codex_home()?;

    // Ensure the codex home directory exists
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("Failed to create codex home: {}", codex_home.display()))?;

    let auth_json = create_auth_json(account)?;

    let auth_path = codex_home.join("auth.json");
    let content =
        serde_json::to_string_pretty(&auth_json).context("Failed to serialize auth.json")?;

    write_auth_file_atomic(&auth_path, &content)?;

    Ok(())
}

fn write_auth_file_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("auth.json path did not have a parent directory")?;

    let mut temp_file = NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temp auth file in {}", parent.display()))?;
    temp_file
        .write_all(content.as_bytes())
        .with_context(|| format!("Failed to write temp auth file for {}", path.display()))?;
    temp_file
        .flush()
        .with_context(|| format!("Failed to flush temp auth file for {}", path.display()))?;
    temp_file
        .as_file()
        .sync_all()
        .with_context(|| format!("Failed to sync temp auth file for {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600)).with_context(
            || format!("Failed to set temp auth permissions for {}", path.display()),
        )?;
    }

    temp_file
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "Failed to atomically replace auth file at {}",
                path.display()
            )
        })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!("Failed to set auth file permissions for {}", path.display())
        })?;
    }

    Ok(())
}

/// Create an AuthDotJson structure from a StoredAccount
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

/// Import an account from an existing auth.json file
pub fn import_from_auth_json(path: &str, account_name: String) -> Result<StoredAccount> {
    let content =
        fs::read_to_string(path).with_context(|| format!("Failed to read auth.json: {path}"))?;

    import_from_auth_json_contents(&content, account_name)
        .with_context(|| format!("Failed to parse auth.json: {path}"))
}

/// Import an account from auth.json file contents.
pub fn import_from_auth_json_contents(
    content: &str,
    account_name: String,
) -> Result<StoredAccount> {
    let auth: AuthDotJson =
        serde_json::from_str(content).context("Failed to parse auth.json contents")?;
    let AuthDotJson {
        openai_api_key,
        tokens,
        last_refresh,
    } = auth;

    // Determine auth mode and create account
    if let Some(api_key) = openai_api_key {
        Ok(StoredAccount::new_api_key(account_name, api_key))
    } else if let Some(tokens) = tokens {
        // Try to extract email and plan from id_token
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
    } else {
        anyhow::bail!("auth.json contains neither API key nor tokens");
    }
}

/// Parse claims from a JWT ID token (without validation)
pub(crate) fn parse_id_token_claims(id_token: &str) -> (Option<String>, Option<String>) {
    let (email, plan_type, _) = parse_id_token_metadata(id_token);
    (email, plan_type)
}

/// Parse the ChatGPT account identity embedded in a JWT ID token.
pub(crate) fn parse_id_token_account_id(id_token: &str) -> Option<String> {
    parse_id_token_metadata(id_token).2
}

fn parse_id_token_metadata(id_token: &str) -> (Option<String>, Option<String>, Option<String>) {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return (None, None, None);
    }

    // Decode the payload (second part)
    let payload =
        match base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, parts[1]) {
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

/// Read the current auth.json file if it exists
pub fn read_current_auth() -> Result<Option<AuthDotJson>> {
    let path = get_codex_auth_file()?;

    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read auth.json: {}", path.display()))?;

    let auth: AuthDotJson = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse auth.json: {}", path.display()))?;

    Ok(Some(auth))
}

/// Check if there is an active Codex login
pub fn has_active_login() -> Result<bool> {
    match read_current_auth()? {
        Some(auth) => Ok(auth.openai_api_key.is_some() || auth.tokens.is_some()),
        None => Ok(false),
    }
}

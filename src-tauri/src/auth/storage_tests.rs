use super::*;
use crate::auth::switcher::{get_codex_auth_file, read_current_auth, write_auth_for_test};
use crate::types::{AuthDotJson, TokenData};

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

fn chatgpt_account(name: &str, suffix: &str) -> StoredAccount {
    StoredAccount::new_chatgpt(
        name.to_string(),
        Some(format!("{name}@example.com")),
        Some("plus".to_string()),
        format!("id-{suffix}"),
        format!("access-{suffix}"),
        format!("refresh-{suffix}"),
        Some(format!("acct-{suffix}")),
    )
}

fn auth_json_for_account(account: &StoredAccount) -> AuthDotJson {
    match &account.auth_data {
        AuthData::ChatGPT {
            id_token,
            access_token,
            refresh_token,
            account_id,
            last_refresh,
        } => AuthDotJson {
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: id_token.clone(),
                access_token: access_token.clone(),
                refresh_token: refresh_token.clone(),
                account_id: account_id.clone(),
            }),
            last_refresh: *last_refresh,
        },
        AuthData::ApiKey { key } => AuthDotJson {
            openai_api_key: Some(key.clone()),
            tokens: None,
            last_refresh: None,
        },
    }
}

fn set_chatgpt_generation(account: &mut StoredAccount, suffix: &str, refreshed_at: DateTime<Utc>) {
    let AuthData::ChatGPT {
        id_token,
        access_token,
        refresh_token,
        last_refresh,
        ..
    } = &mut account.auth_data
    else {
        panic!("expected ChatGPT account");
    };
    *id_token = format!("id-{suffix}");
    *access_token = format!("access-{suffix}");
    *refresh_token = format!("refresh-{suffix}");
    *last_refresh = Some(refreshed_at);
}

#[test]
fn operating_system_lock_blocks_competing_store_owner() {
    use fs2::FileExt;

    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();
    let first = acquire_store_lock().expect("acquire first store lock");
    let path = get_accounts_lock_file().expect("lock path");
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open competing lock handle");

    let blocked = contender.try_lock_exclusive();
    assert!(blocked.is_err());

    drop(first);
    contender
        .try_lock_exclusive()
        .expect("lock should release with its owner");
}

#[test]
fn catalog_mutations_do_not_publish_auth_json() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();

    let first = add_account(api_account("First", "sk-first")).expect("add first");
    assert_eq!(
        load_accounts().expect("load store").active_account_id,
        Some(first.id)
    );
    assert!(!get_codex_auth_file().expect("auth path").exists());
}

#[test]
fn catalog_rejects_duplicate_credential_identities() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();

    add_account(api_account("First", "sk-shared")).expect("add first account");
    let duplicate_key = add_account(api_account("Second", "sk-shared"));
    assert!(duplicate_key.is_err());

    let oauth = add_account(chatgpt_account("OAuth", "shared")).expect("add OAuth account");
    let mut duplicate_oauth = chatgpt_account("OAuth duplicate", "different");
    if let (
        AuthData::ChatGPT { account_id, .. },
        AuthData::ChatGPT {
            account_id: original,
            ..
        },
    ) = (&mut duplicate_oauth.auth_data, &oauth.auth_data)
    {
        *account_id = original.clone();
    }
    let duplicate_identity = add_account(duplicate_oauth);
    assert!(duplicate_identity.is_err());

    assert_eq!(load_accounts().expect("load store").accounts.len(), 2);
}

#[test]
fn refreshed_credentials_cannot_introduce_a_duplicate_identity() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();

    add_account(chatgpt_account("Known", "shared")).expect("add known account");
    let mut legacy = chatgpt_account("Legacy", "legacy");
    if let AuthData::ChatGPT {
        id_token,
        account_id,
        ..
    } = &mut legacy.auth_data
    {
        *id_token = "legacy-id-without-account-claim".to_string();
        *account_id = None;
    }
    let legacy = add_account(legacy).expect("add identity-less legacy account");
    let expected = chatgpt_credential_fingerprint(&legacy).expect("fingerprint legacy credentials");

    let result = update_account_chatgpt_tokens_after_refresh(
        &legacy.id,
        &expected,
        ChatGptTokenUpdate {
            id_token: "refreshed-id".to_string(),
            access_token: "refreshed-access".to_string(),
            refresh_token: "refreshed-refresh".to_string(),
            account_id: Some("acct-shared".to_string()),
            email: None,
            plan_type: None,
            last_refresh: Some(Utc::now()),
        },
    );

    assert!(result.is_err());
    let stored = get_account(&legacy.id)
        .expect("load legacy account")
        .expect("legacy account exists");
    assert!(matches!(
        stored.auth_data,
        AuthData::ChatGPT {
            account_id: None,
            ..
        }
    ));
}

#[test]
fn background_token_update_preserves_live_auth_bytes() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();

    let active = add_account(api_account("Active", "sk-active")).expect("add active");
    write_auth_for_test(&active).expect("write active auth");
    let background = add_account(chatgpt_account("Background", "old")).expect("add background");
    let auth_path = get_codex_auth_file().expect("auth path");
    let before = fs::read(&auth_path).expect("read auth before");

    update_account_chatgpt_tokens(
        &background.id,
        "id-new".to_string(),
        "access-new".to_string(),
        "refresh-new".to_string(),
        Some("acct-new".to_string()),
        None,
        None,
    )
    .expect("update catalog tokens");

    assert_eq!(fs::read(auth_path).expect("read auth after"), before);
}

#[test]
fn reconciliation_repairs_stale_live_credentials_from_the_catalog() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();
    let refreshed_at = Utc::now();
    let mut current = chatgpt_account("ChatGPT", "current");
    set_chatgpt_generation(&mut current, "current", refreshed_at);
    let current = add_account(current).expect("add current account");
    let mut stale = current.clone();
    set_chatgpt_generation(
        &mut stale,
        "stale",
        refreshed_at - chrono::Duration::minutes(1),
    );
    write_auth_for_test(&stale).expect("write stale live credentials");

    let outcome = reconcile_current_auth_catalog().expect("repair stale live credentials");

    assert!(outcome.state == super::super::switcher::LiveAuthState::Stale);
    let live = read_current_auth()
        .expect("read live credentials")
        .expect("live credentials exist");
    assert!(matches!(
        live.tokens,
        Some(TokenData {
            access_token,
            refresh_token,
            ..
        }) if access_token == "access-current" && refresh_token == "refresh-current"
    ));
}

#[test]
fn stale_refresh_response_returns_catalog_winner() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();

    let stored = add_account(chatgpt_account("ChatGPT", "old")).expect("add account");
    let expected = chatgpt_credential_fingerprint(&stored).expect("fingerprint old credentials");
    update_account_chatgpt_tokens(
        &stored.id,
        "id-winner".to_string(),
        "access-winner".to_string(),
        "refresh-winner".to_string(),
        Some("acct-winner".to_string()),
        None,
        None,
    )
    .expect("store winner");

    let winner = update_account_chatgpt_tokens_after_refresh(
        &stored.id,
        &expected,
        ChatGptTokenUpdate {
            id_token: "id-stale".to_string(),
            access_token: "access-stale".to_string(),
            refresh_token: "refresh-stale".to_string(),
            account_id: Some("acct-stale".to_string()),
            email: None,
            plan_type: None,
            last_refresh: Some(Utc::now()),
        },
    )
    .expect("discard stale response");

    assert!(matches!(
        winner.auth_data,
        AuthData::ChatGPT {
            access_token,
            refresh_token,
            ..
        } if access_token == "access-winner" && refresh_token == "refresh-winner"
    ));
}

#[test]
fn newest_live_generation_wins_during_refresh_reconciliation() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();
    let base_time = Utc::now() - chrono::Duration::minutes(3);
    let mut initial = chatgpt_account("ChatGPT", "initial");
    set_chatgpt_generation(&mut initial, "initial", base_time);
    let initial = add_account(initial).expect("add initial account");
    write_auth_for_test(&initial).expect("write initial live credentials");
    let expected =
        chatgpt_credential_fingerprint(&initial).expect("fingerprint initial credentials");

    let mut first_live = initial.clone();
    set_chatgpt_generation(
        &mut first_live,
        "first-live",
        base_time + chrono::Duration::minutes(1),
    );
    write_auth_for_test(&first_live).expect("write first live generation");
    let mut newest_live = initial.clone();
    set_chatgpt_generation(
        &mut newest_live,
        "newest-live",
        base_time + chrono::Duration::minutes(2),
    );
    std::env::set_var(
        "CODEX_SWITCHER_TEST_REFRESH_WINNER_REPLACEMENT",
        serde_json::to_string(&auth_json_for_account(&newest_live))
            .expect("serialize newest live generation"),
    );

    let winner = update_account_chatgpt_tokens_after_refresh(
        &initial.id,
        &expected,
        ChatGptTokenUpdate {
            id_token: "id-provider".to_string(),
            access_token: "access-provider".to_string(),
            refresh_token: "refresh-provider".to_string(),
            account_id: Some("acct-initial".to_string()),
            email: None,
            plan_type: None,
            last_refresh: Some(Utc::now()),
        },
    )
    .expect("newest live generation should win");
    std::env::remove_var("CODEX_SWITCHER_TEST_REFRESH_WINNER_REPLACEMENT");

    assert!(matches!(
        winner.auth_data,
        AuthData::ChatGPT {
            access_token,
            refresh_token,
            ..
        } if access_token == "access-newest-live" && refresh_token == "refresh-newest-live"
    ));
    let stored = get_account(&initial.id)
        .expect("load account")
        .expect("account exists");
    assert!(matches!(
        stored.auth_data,
        AuthData::ChatGPT { access_token, .. } if access_token == "access-newest-live"
    ));
}

#[test]
fn concurrent_catalog_mutations_do_not_lose_changes() {
    let _guard = crate::test_support::env_lock();
    let _env = TestEnv::new();

    let stored = add_account(api_account("Original", "sk-original")).expect("add account");
    let account_id = stored.id.clone();
    let rename_id = account_id.clone();
    let rename = std::thread::spawn(move || {
        update_account_metadata(&rename_id, Some("Renamed".to_string()), None, None)
            .expect("rename account");
    });
    let mask_id = account_id.clone();
    let mask = std::thread::spawn(move || {
        set_masked_account_ids(vec![mask_id]).expect("set masked ids");
    });

    rename.join().expect("rename thread");
    mask.join().expect("mask thread");

    let store = load_accounts().expect("load accounts");
    assert_eq!(store.accounts[0].name, "Renamed");
    assert_eq!(store.masked_account_ids, vec![account_id]);
}

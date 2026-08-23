//! Usage API client for fetching rate limits and credits

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, USER_AGENT},
    StatusCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::auth::{
    ensure_chatgpt_tokens_fresh, refresh_chatgpt_tokens, resolved_chatgpt_account_id,
};
use crate::types::{
    AuthData, CreditStatusDetails, RateLimitDetails, RateLimitStatusPayload, RateLimitWindow,
    StoredAccount, UsageInfo,
};

const CHATGPT_BACKEND_API: &str = "https://chatgpt.com/backend-api";
const CHATGPT_ACCOUNTS_CHECK_API: &str =
    "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const CHATGPT_CODEX_RESPONSES_API: &str = "https://chatgpt.com/backend-api/codex/responses";
const OPENAI_API: &str = "https://api.openai.com/v1";
const CODEX_USER_AGENT: &str = "codex-cli/1.0.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAccountMetadata {
    pub plan_type: Option<String>,
    pub subscription_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct AccountsCheckResponse {
    #[serde(default)]
    accounts: HashMap<String, AccountsCheckEntry>,
}

#[derive(Debug, Deserialize)]
struct AccountsCheckEntry {
    #[serde(default)]
    account: Option<AccountsCheckAccount>,
    #[serde(default)]
    entitlement: Option<AccountsCheckEntitlement>,
}

#[derive(Debug, Deserialize)]
struct AccountsCheckAccount {
    #[serde(default)]
    plan_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AccountsCheckEntitlement {
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

/// Get usage information for an account
pub async fn get_account_usage(account: &StoredAccount) -> Result<UsageInfo> {
    println!("[Usage] Fetching usage for account: {}", account.name);

    match &account.auth_data {
        AuthData::ApiKey { .. } => {
            println!("[Usage] API key accounts don't support usage info");
            Ok(UsageInfo {
                account_id: account.id.clone(),
                plan_type: Some("api_key".to_string()),
                primary_used_percent: None,
                primary_window_minutes: None,
                primary_resets_at: None,
                secondary_used_percent: None,
                secondary_window_minutes: None,
                secondary_resets_at: None,
                has_credits: None,
                unlimited_credits: None,
                credits_balance: None,
                banked_resets: None,
                error: None,
            })
        }
        AuthData::ChatGPT { .. } => get_usage_with_chatgpt_auth(account).await,
    }
}

/// Send a minimal authenticated request to warm up account traffic paths.
pub async fn warmup_account(account: &StoredAccount) -> Result<()> {
    println!(
        "[Warmup] Sending warm-up request for account: {}",
        account.name
    );

    match &account.auth_data {
        AuthData::ApiKey { key } => warmup_with_api_key(key).await,
        AuthData::ChatGPT { .. } => warmup_with_chatgpt_auth(account).await,
    }
}

/// Fetch the current ChatGPT plan and entitlement period for one exact account.
pub async fn get_chatgpt_account_metadata(
    account: &StoredAccount,
) -> Result<ChatGptAccountMetadata> {
    let fresh_account = ensure_chatgpt_tokens_fresh(account).await?;
    let (access_token, chatgpt_account_id) = extract_chatgpt_metadata_auth(&fresh_account)?;

    let response = send_chatgpt_account_metadata_request(access_token, &chatgpt_account_id).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        let refreshed_account = refresh_chatgpt_tokens(&fresh_account).await?;
        let (retry_token, retry_account_id) = extract_chatgpt_metadata_auth(&refreshed_account)?;
        let retry_response =
            send_chatgpt_account_metadata_request(retry_token, &retry_account_id).await?;
        return parse_chatgpt_account_metadata_response(&retry_account_id, retry_response).await;
    }

    parse_chatgpt_account_metadata_response(&chatgpt_account_id, response).await
}

async fn parse_chatgpt_account_metadata_response(
    chatgpt_account_id: &str,
    response: reqwest::Response,
) -> Result<ChatGptAccountMetadata> {
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("ChatGPT account metadata request failed with status {status}");
    }

    let payload: AccountsCheckResponse = response
        .json()
        .await
        .context("Failed to parse ChatGPT account metadata response")?;
    extract_chatgpt_account_metadata(&payload, chatgpt_account_id)
}

fn extract_chatgpt_account_metadata(
    payload: &AccountsCheckResponse,
    chatgpt_account_id: &str,
) -> Result<ChatGptAccountMetadata> {
    let entry = payload.accounts.get(chatgpt_account_id).or_else(|| {
        if payload.accounts.len() != 1 {
            return None;
        }
        payload.accounts.get("default")
    });
    let entry =
        entry.context("ChatGPT account metadata response did not include the requested account")?;

    Ok(ChatGptAccountMetadata {
        plan_type: entry
            .account
            .as_ref()
            .and_then(|account| account.plan_type.clone()),
        subscription_expires_at: entry
            .entitlement
            .as_ref()
            .and_then(|entitlement| entitlement.expires_at),
    })
}

async fn get_usage_with_chatgpt_auth(account: &StoredAccount) -> Result<UsageInfo> {
    let fresh_account = ensure_chatgpt_tokens_fresh(account).await?;
    let (access_token, chatgpt_account_id) = extract_chatgpt_auth(&fresh_account)?;

    let response = send_chatgpt_usage_request(access_token, chatgpt_account_id).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        println!(
            "[Usage] Unauthorized for account {}, refreshing token and retrying once",
            fresh_account.name
        );
        let refreshed_account = refresh_chatgpt_tokens(&fresh_account).await?;
        let (retry_token, retry_account_id) = extract_chatgpt_auth(&refreshed_account)?;
        let retry_response = send_chatgpt_usage_request(retry_token, retry_account_id).await?;
        return parse_usage_response(
            &refreshed_account.id,
            &refreshed_account.name,
            retry_response,
        )
        .await;
    }

    parse_usage_response(&fresh_account.id, &fresh_account.name, response).await
}

async fn parse_usage_response(
    account_id: &str,
    account_name: &str,
    response: reqwest::Response,
) -> Result<UsageInfo> {
    let status = response.status();
    println!("[Usage] Response status: {status}");

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        println!("[Usage] Error response: {body}");
        return Ok(UsageInfo::error(
            account_id.to_string(),
            format!("API error: {status}"),
        ));
    }

    let body_text = response
        .text()
        .await
        .context("Failed to read response body")?;
    println!(
        "[Usage] Response body: {}",
        truncate_to_char_boundary(&body_text, 200)
    );

    let payload: RateLimitStatusPayload =
        serde_json::from_str(&body_text).context("Failed to parse usage response")?;

    println!("[Usage] Parsed plan_type: {}", payload.plan_type);

    let usage = convert_payload_to_usage_info(account_id, payload);
    println!(
        "[Usage] {} - primary: {:?}%, plan: {:?}",
        account_name, usage.primary_used_percent, usage.plan_type
    );

    Ok(usage)
}

async fn warmup_with_chatgpt_auth(account: &StoredAccount) -> Result<()> {
    let fresh_account = ensure_chatgpt_tokens_fresh(account).await?;
    let (access_token, chatgpt_account_id) = extract_chatgpt_auth(&fresh_account)?;

    let mut response = send_chatgpt_warmup_request(access_token, chatgpt_account_id, true).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        println!(
            "[Warmup] Unauthorized for account {}, refreshing token and retrying once",
            fresh_account.name
        );
        let refreshed_account = refresh_chatgpt_tokens(&fresh_account).await?;
        let (retry_token, retry_account_id) = extract_chatgpt_auth(&refreshed_account)?;
        response = send_chatgpt_warmup_request(retry_token, retry_account_id, true).await?;
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        println!("[Warmup] ChatGPT warm-up error response: {body}");
        anyhow::bail!("ChatGPT warm-up failed with status {status}");
    }

    let body = response.text().await.unwrap_or_default();
    log_warmup_response("ChatGPT", &body, true);

    Ok(())
}

async fn warmup_with_api_key(api_key: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let payload = build_warmup_payload(false, true);
    let response = client
        .post(format!("{OPENAI_API}/responses"))
        .header(USER_AGENT, CODEX_USER_AGENT)
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .json(&payload)
        .send()
        .await
        .context("Failed to send API key warm-up request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        println!("[Warmup] API key warm-up error response: {body}");
        anyhow::bail!("API key warm-up failed with status {status}");
    }

    let body = response.text().await.unwrap_or_default();
    log_warmup_response("API key", &body, false);

    Ok(())
}

fn build_warmup_payload(stream: bool, include_max_output_tokens: bool) -> serde_json::Value {
    let mut payload = json!({
        "model": "gpt-5.4-mini",
        "instructions": "You are Codex.",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": "Hi"
                    }
                ]
            }
        ],
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {
            "effort": "low"
        },
        "store": false,
        "stream": stream
    });

    if include_max_output_tokens {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("max_output_tokens".to_string(), json!(1));
        }
    }

    payload
}

fn build_chatgpt_headers(
    access_token: &str,
    chatgpt_account_id: Option<&str>,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CODEX_USER_AGENT));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}")).context("Invalid access token")?,
    );

    if let Some(acc_id) = chatgpt_account_id {
        println!("[Usage] Using ChatGPT Account ID: {acc_id}");
        if let Ok(header_name) = HeaderName::from_bytes(b"chatgpt-account-id") {
            if let Ok(header_value) = HeaderValue::from_str(acc_id) {
                headers.insert(header_name, header_value);
            }
        }
    }

    Ok(headers)
}

fn extract_chatgpt_auth(account: &StoredAccount) -> Result<(&str, Option<&str>)> {
    match &account.auth_data {
        AuthData::ChatGPT {
            access_token,
            account_id,
            ..
        } => Ok((access_token.as_str(), account_id.as_deref())),
        AuthData::ApiKey { .. } => anyhow::bail!("Account is not using ChatGPT OAuth"),
    }
}

fn extract_chatgpt_metadata_auth(account: &StoredAccount) -> Result<(&str, String)> {
    match &account.auth_data {
        AuthData::ChatGPT {
            id_token,
            access_token,
            account_id,
            ..
        } => {
            let account_id = resolved_chatgpt_account_id(account_id.as_deref(), id_token)?
                .context("ChatGPT account metadata requires a stable account ID")?;
            Ok((access_token.as_str(), account_id))
        }
        AuthData::ApiKey { .. } => anyhow::bail!("Account is not using ChatGPT OAuth"),
    }
}

async fn send_chatgpt_usage_request(
    access_token: &str,
    chatgpt_account_id: Option<&str>,
) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let headers = build_chatgpt_headers(access_token, chatgpt_account_id)?;
    let url = format!("{CHATGPT_BACKEND_API}/wham/usage");
    println!("[Usage] Requesting: {url}");

    client
        .get(&url)
        .headers(headers)
        .send()
        .await
        .context("Failed to send usage request")
}

async fn send_chatgpt_account_metadata_request(
    access_token: &str,
    chatgpt_account_id: &str,
) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let headers = build_chatgpt_headers(access_token, Some(chatgpt_account_id))?;

    client
        .get(CHATGPT_ACCOUNTS_CHECK_API)
        .headers(headers)
        .send()
        .await
        .context("Failed to send ChatGPT account metadata request")
}

async fn send_chatgpt_warmup_request(
    access_token: &str,
    chatgpt_account_id: Option<&str>,
    stream: bool,
) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let headers = build_chatgpt_headers(access_token, chatgpt_account_id)?;
    let payload = build_warmup_payload(stream, false);

    client
        .post(CHATGPT_CODEX_RESPONSES_API)
        .headers(headers)
        .json(&payload)
        .send()
        .await
        .context("Failed to send ChatGPT warm-up request")
}

fn log_warmup_response(source: &str, body: &str, is_sse: bool) {
    if body.trim().is_empty() {
        println!("[Warmup] {source} warm-up response was empty");
        return;
    }

    let preview = truncate_text(body, 300);
    println!("[Warmup] {source} warm-up response preview: {preview}");

    let extracted = if is_sse {
        extract_text_from_sse(body)
    } else {
        extract_text_from_json(body)
    };

    if let Some(message) = extracted {
        let message_preview = truncate_text(&message, 200);
        println!("[Warmup] {source} warm-up message: {message_preview}");
    }
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut out = truncate_to_char_boundary(text, max_len).to_string();
    out.push_str("...");
    out
}

/// Truncate to at most `max_len` bytes without splitting a UTF-8 character.
fn truncate_to_char_boundary(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        return text;
    }
    let mut end = max_len;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn extract_text_from_sse(body: &str) -> Option<String> {
    let mut last_text: Option<String> = None;
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with("data:") {
            continue;
        }
        let data = line.trim_start_matches("data:").trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            if let Some(text) = extract_last_text_from_value(&value) {
                last_text = Some(text);
            }
        }
    }
    last_text.filter(|text| !text.trim().is_empty())
}

fn extract_text_from_json(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    extract_last_text_from_value(&value)
}

fn extract_last_text_from_value(value: &Value) -> Option<String> {
    let mut last: Option<String> = None;
    collect_last_text(value, &mut last);
    last
}

fn collect_last_text(value: &Value, last: &mut Option<String>) {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                if matches!(key.as_str(), "text" | "delta" | "output_text") {
                    if let Value::String(text) = val {
                        if !text.is_empty() {
                            *last = Some(text.clone());
                        }
                    }
                }
                collect_last_text(val, last);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_last_text(item, last);
            }
        }
        _ => {}
    }
}

/// Convert API response to UsageInfo
fn convert_payload_to_usage_info(account_id: &str, payload: RateLimitStatusPayload) -> UsageInfo {
    let banked_resets = payload
        .rate_limit_reset_credits
        .and_then(|credits| credits.available_count);
    let (primary, secondary) = extract_rate_limits(payload.rate_limit);
    let credits = extract_credits(payload.credits);

    UsageInfo {
        account_id: account_id.to_string(),
        plan_type: Some(payload.plan_type),
        primary_used_percent: primary.as_ref().map(|w| w.used_percent),
        primary_window_minutes: primary
            .as_ref()
            .and_then(|w| w.limit_window_seconds)
            .map(|s| (i64::from(s) + 59) / 60),
        primary_resets_at: primary.as_ref().and_then(|w| w.reset_at),
        secondary_used_percent: secondary.as_ref().map(|w| w.used_percent),
        secondary_window_minutes: secondary
            .as_ref()
            .and_then(|w| w.limit_window_seconds)
            .map(|s| (i64::from(s) + 59) / 60),
        secondary_resets_at: secondary.as_ref().and_then(|w| w.reset_at),
        has_credits: credits.as_ref().map(|c| c.has_credits),
        unlimited_credits: credits.as_ref().map(|c| c.unlimited),
        credits_balance: credits.and_then(|c| c.balance),
        banked_resets,
        error: None,
    }
}

fn extract_rate_limits(
    rate_limit: Option<RateLimitDetails>,
) -> (Option<RateLimitWindow>, Option<RateLimitWindow>) {
    match rate_limit {
        Some(details) => (details.primary_window, details.secondary_window),
        None => (None, None),
    }
}

fn extract_credits(credits: Option<CreditStatusDetails>) -> Option<CreditStatusDetails> {
    credits
}

/// Refresh all account usage
pub async fn refresh_all_usage(accounts: &[StoredAccount]) -> Vec<UsageInfo> {
    println!("[Usage] Refreshing usage for {} accounts", accounts.len());

    let concurrency = accounts.len().clamp(1, 10);
    let results: Vec<UsageInfo> = stream::iter(accounts.iter().cloned())
        .map(|account| async move {
            match get_account_usage(&account).await {
                Ok(info) => info,
                Err(e) => {
                    println!("[Usage] Error for {}: {}", account.name, e);
                    UsageInfo::error(account.id.clone(), e.to_string())
                }
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    println!("[Usage] Refresh complete");
    results
}

#[cfg(test)]
mod tests {
    use super::{
        convert_payload_to_usage_info, extract_chatgpt_account_metadata, get_account_usage,
        AccountsCheckResponse,
    };
    use crate::types::{RateLimitStatusPayload, StoredAccount};

    #[test]
    fn extracts_subscription_metadata_for_the_exact_requested_account() {
        let payload: AccountsCheckResponse = serde_json::from_value(serde_json::json!({
            "accounts": {
                "default": {
                    "account": { "plan_type": "free" },
                    "entitlement": { "expires_at": "2030-01-01T00:00:00Z" }
                },
                "acct-target": {
                    "account": { "plan_type": "plus" },
                    "entitlement": { "expires_at": "2026-09-12T05:30:00Z" }
                }
            }
        }))
        .expect("valid account metadata payload");

        let metadata = extract_chatgpt_account_metadata(&payload, "acct-target")
            .expect("target account metadata");

        assert_eq!(metadata.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            metadata
                .subscription_expires_at
                .map(|value| value.to_rfc3339()),
            Some("2026-09-12T05:30:00+00:00".to_string())
        );
    }

    #[test]
    fn rejects_metadata_for_a_different_account() {
        let payload: AccountsCheckResponse = serde_json::from_value(serde_json::json!({
            "accounts": {
                "acct-other": {
                    "account": { "plan_type": "plus" },
                    "entitlement": { "expires_at": "2026-09-12T05:30:00Z" }
                }
            }
        }))
        .expect("valid account metadata payload");

        let error = extract_chatgpt_account_metadata(&payload, "acct-target")
            .expect_err("metadata from another account must not be accepted");

        assert!(error
            .to_string()
            .contains("did not include the requested account"));
    }

    #[test]
    fn accepts_a_sole_default_account_entry() {
        let payload: AccountsCheckResponse = serde_json::from_value(serde_json::json!({
            "accounts": {
                "default": {
                    "account": { "plan_type": "plus" },
                    "entitlement": { "expires_at": "2026-09-12T05:30:00Z" }
                }
            }
        }))
        .expect("valid default account metadata payload");

        let metadata = extract_chatgpt_account_metadata(&payload, "acct-target")
            .expect("a sole default entry belongs to the authenticated account");

        assert_eq!(metadata.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            metadata
                .subscription_expires_at
                .map(|value| value.to_rfc3339()),
            Some("2026-09-12T05:30:00+00:00".to_string())
        );
    }

    #[test]
    fn rejects_default_fallback_when_multiple_accounts_are_returned() {
        let payload: AccountsCheckResponse = serde_json::from_value(serde_json::json!({
            "accounts": {
                "default": {
                    "account": { "plan_type": "plus" },
                    "entitlement": { "expires_at": "2026-09-12T05:30:00Z" }
                },
                "acct-other": {
                    "account": { "plan_type": "pro" },
                    "entitlement": { "expires_at": "2026-10-15T05:30:00Z" }
                }
            }
        }))
        .expect("valid multi-account metadata payload");

        let error = extract_chatgpt_account_metadata(&payload, "acct-target")
            .expect_err("a default entry is ambiguous when other accounts are present");

        assert!(error
            .to_string()
            .contains("did not include the requested account"));
    }

    #[test]
    fn truncates_multibyte_text_without_panicking() {
        // "é" is 2 bytes in UTF-8; cutting at byte 3 would split the second char.
        assert_eq!(super::truncate_text("ééééé", 3), "é...");
        assert_eq!(super::truncate_to_char_boundary("ééééé", 3), "é");
        assert_eq!(super::truncate_text("abc", 3), "abc");
    }

    #[test]
    fn maps_banked_reset_count_from_usage_payload() {
        let payload: RateLimitStatusPayload = serde_json::from_value(serde_json::json!({
            "plan_type": "plus",
            "rate_limit": null,
            "credits": null,
            "rate_limit_reset_credits": { "available_count": 3 }
        }))
        .expect("valid usage payload");

        let usage = convert_payload_to_usage_info("account-1", payload);
        assert_eq!(usage.banked_resets, Some(3));
    }

    #[tokio::test]
    async fn api_key_usage_is_an_expected_no_data_state() {
        let account = StoredAccount::new_api_key("API".to_string(), "sk-test".to_string());
        let usage = get_account_usage(&account)
            .await
            .expect("API key usage should not fail");

        assert_eq!(usage.account_id, account.id);
        assert_eq!(usage.plan_type.as_deref(), Some("api_key"));
        assert!(usage.primary_used_percent.is_none());
        assert!(usage.secondary_used_percent.is_none());
        assert!(usage.banked_resets.is_none());
        assert!(usage.error.is_none());
    }
}

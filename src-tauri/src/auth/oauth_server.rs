//! Local OAuth server for handling ChatGPT login flow

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Request, Response, Server};
use tokio::sync::oneshot;

use crate::types::{OAuthLoginInfo, StoredAccount};

const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
// These are the callback ports registered for the official Codex OAuth client.
// Keep this list aligned with Codex CLI. Arbitrary ports are rejected by the
// authorization server even when a local listener can bind successfully.
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];

/// PKCE codes for OAuth
#[derive(Debug, Clone)]
pub struct PkceCodes {
    pub code_verifier: String,
    pub code_challenge: String,
}

/// Generate PKCE codes
pub fn generate_pkce() -> PkceCodes {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);

    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);

    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

/// Generate a random state parameter
fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the OAuth authorization URL
fn build_authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        (
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        ),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("prompt", "login"),
        ("state", state),
        ("originator", "codex_cli_rs"), // Required by OpenAI OAuth
    ];

    let query_string = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{issuer}/oauth/authorize?{query_string}")
}

fn bind_callback_server(ports: &[u16]) -> Result<Server> {
    let mut failures = Vec::with_capacity(ports.len());

    for port in ports {
        match Server::http(format!("127.0.0.1:{port}")) {
            Ok(server) => return Ok(server),
            Err(error) => failures.push(format!("{port}: {error}")),
        }
    }

    let port_list = ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" and ");
    anyhow::bail!(
        "OAuth callback ports {port_list} are unavailable. Stop the app or service using one of those ports, then try again. ({})",
        failures.join("; ")
    )
}

/// Token response from the OAuth server
#[derive(Debug, Clone, serde::Deserialize)]
struct TokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

/// Exchange authorization code for tokens
async fn exchange_code_for_tokens(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> Result<TokenResponse> {
    let client = reqwest::Client::new();

    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(client_id),
        urlencoding::encode(&pkce.code_verifier)
    );

    let resp = client
        .post(format!("{issuer}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("Failed to send token request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed: {status} - {body}");
    }

    let tokens: TokenResponse = resp
        .json()
        .await
        .context("Failed to parse token response")?;
    Ok(tokens)
}

/// Parse claims from JWT ID token
fn parse_id_token_claims(id_token: &str) -> (Option<String>, Option<String>, Option<String>) {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return (None, None, None);
    }

    let payload = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]) {
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

/// OAuth login flow result
pub struct OAuthLoginResult {
    pub account: StoredAccount,
}

/// Start the OAuth login flow
pub async fn start_oauth_login(
    account_name: String,
    cancelled: Arc<AtomicBool>,
) -> Result<(OAuthLoginInfo, oneshot::Receiver<Result<OAuthLoginResult>>)> {
    let pkce = generate_pkce();
    let state = generate_state();

    println!("[OAuth] Starting login for account: {account_name}");
    println!("[OAuth] PKCE challenge: {}", &pkce.code_challenge[..20]);

    let server = bind_callback_server(&CALLBACK_PORTS)?;

    let actual_port = match server.server_addr().to_ip() {
        Some(addr) => addr.port(),
        None => anyhow::bail!("Failed to determine server port"),
    };

    let redirect_uri = format!("http://localhost:{actual_port}/auth/callback");
    let auth_url = build_authorize_url(DEFAULT_ISSUER, CLIENT_ID, &redirect_uri, &pkce, &state);

    println!("[OAuth] Server started on port {actual_port}");
    println!("[OAuth] Redirect URI: {redirect_uri}");
    println!("[OAuth] Auth URL: {auth_url}");

    let login_info = OAuthLoginInfo {
        auth_url: auth_url.clone(),
        callback_port: actual_port,
    };

    // Create a channel for the result
    let (tx, rx) = oneshot::channel();

    // Spawn the server in a background thread
    let server = Arc::new(server);
    let pkce_clone = pkce.clone();
    let state_clone = state.clone();
    let cancelled_clone = cancelled.clone();

    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(run_oauth_server(
            server,
            pkce_clone,
            state_clone,
            redirect_uri,
            account_name,
            cancelled_clone,
        ));
        let _ = tx.send(result);
    });

    Ok((login_info, rx))
}

/// Run the OAuth callback server
async fn run_oauth_server(
    server: Arc<Server>,
    pkce: PkceCodes,
    expected_state: String,
    redirect_uri: String,
    account_name: String,
    cancelled: Arc<AtomicBool>,
) -> Result<OAuthLoginResult> {
    let timeout = Duration::from_secs(300); // 5 minute timeout
    let start = std::time::Instant::now();

    loop {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("OAuth login cancelled");
        }

        if start.elapsed() > timeout {
            anyhow::bail!("OAuth login timed out");
        }

        // Use recv_timeout to allow checking the timeout
        let request = match server.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(req)) => req,
            Ok(None) => continue,
            Err(_) => continue,
        };

        let result = handle_oauth_request(
            request,
            &pkce,
            &expected_state,
            &redirect_uri,
            &account_name,
            &cancelled,
        )
        .await;

        match result {
            HandleResult::Continue => continue,
            HandleResult::Success(account) => {
                server.unblock();
                return Ok(OAuthLoginResult { account: *account });
            }
            HandleResult::Error(e) => {
                server.unblock();
                return Err(e);
            }
        }
    }
}

enum HandleResult {
    Continue,
    Success(Box<StoredAccount>),
    Error(anyhow::Error),
}

async fn handle_oauth_request(
    request: Request,
    pkce: &PkceCodes,
    expected_state: &str,
    redirect_uri: &str,
    account_name: &str,
    cancelled: &AtomicBool,
) -> HandleResult {
    let url_str = request.url().to_string();
    let parsed = match url::Url::parse(&format!("http://localhost{url_str}")) {
        Ok(u) => u,
        Err(_) => {
            let _ = request.respond(Response::from_string("Bad Request").with_status_code(400));
            return HandleResult::Continue;
        }
    };

    let path = parsed.path();

    if path == "/auth/callback" {
        println!("[OAuth] Received callback request");
        let params: std::collections::HashMap<String, String> =
            parsed.query_pairs().into_owned().collect();

        println!(
            "[OAuth] Callback params: {:?}",
            params.keys().collect::<Vec<_>>()
        );

        // Verify state before anything else. A stray request (browser
        // prefetch, another local process) must not abort the pending login,
        // so mismatches are ignored rather than treated as fatal.
        if params.get("state").map(String::as_str) != Some(expected_state) {
            println!("[OAuth] Ignoring callback with missing or mismatched state");
            let _ = request.respond(Response::from_string("State mismatch").with_status_code(400));
            return HandleResult::Continue;
        }

        println!("[OAuth] State verified OK");

        // Check for error response (state-verified, so it's really from our flow)
        if let Some(error) = params.get("error") {
            let error_desc = params
                .get("error_description")
                .map(|s| s.as_str())
                .unwrap_or("Unknown error");
            println!("[OAuth] Error from provider: {error} - {error_desc}");
            let _ = request.respond(
                Response::from_string(format!("OAuth Error: {error} - {error_desc}"))
                    .with_status_code(400),
            );
            return HandleResult::Error(anyhow::anyhow!("OAuth error: {error} - {error_desc}"));
        }

        // Get the authorization code
        let code = match params.get("code") {
            Some(c) if !c.is_empty() => c.clone(),
            _ => {
                println!("[OAuth] Missing authorization code");
                let _ = request.respond(
                    Response::from_string("Missing authorization code").with_status_code(400),
                );
                return HandleResult::Error(anyhow::anyhow!("Missing authorization code"));
            }
        };

        println!("[OAuth] Got authorization code, exchanging for tokens...");

        // Exchange code for tokens
        match exchange_code_for_tokens(DEFAULT_ISSUER, CLIENT_ID, redirect_uri, pkce, &code).await {
            Ok(tokens) => {
                if cancelled.load(Ordering::Relaxed) {
                    let _ = request.respond(
                        Response::from_string("OAuth login cancelled").with_status_code(409),
                    );
                    return HandleResult::Error(anyhow::anyhow!("OAuth login cancelled"));
                }

                println!("[OAuth] Token exchange successful!");
                // Parse claims from ID token
                let (email, plan_type, chatgpt_account_id) =
                    parse_id_token_claims(&tokens.id_token);

                // Create the account
                let account = StoredAccount::new_chatgpt(
                    account_name.to_string(),
                    email,
                    plan_type,
                    tokens.id_token,
                    tokens.access_token,
                    tokens.refresh_token,
                    chatgpt_account_id,
                );

                // Send success response
                let success_html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Login Successful</title>
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: linear-gradient(135deg, #667eea 0%, #764ba2 100%); }
        .container { text-align: center; background: white; padding: 40px 60px; border-radius: 16px; box-shadow: 0 20px 60px rgba(0,0,0,0.3); }
        h1 { color: #333; margin-bottom: 10px; }
        p { color: #666; }
        .checkmark { font-size: 48px; margin-bottom: 20px; }
    </style>
</head>
<body>
    <div class="container">
        <div class="checkmark">✓</div>
        <h1>Login Successful!</h1>
        <p>You can close this window and return to ChatGPT Codex Switcher.</p>
    </div>
</body>
</html>"#;

                let response = Response::from_string(success_html).with_header(
                    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                        .unwrap(),
                );
                let _ = request.respond(response);

                return HandleResult::Success(Box::new(account));
            }
            Err(e) => {
                println!("[OAuth] Token exchange failed: {e}");
                let _ = request.respond(
                    Response::from_string(format!("Token exchange failed: {e}"))
                        .with_status_code(500),
                );
                return HandleResult::Error(e);
            }
        }
    }

    // Handle other paths
    let _ = request.respond(Response::from_string("Not Found").with_status_code(404));
    HandleResult::Continue
}

/// Wait for the OAuth login to complete
pub async fn wait_for_oauth_login(
    rx: oneshot::Receiver<Result<OAuthLoginResult>>,
) -> Result<StoredAccount> {
    let result = rx.await.context("OAuth login was cancelled")??;
    Ok(result.account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::TcpListener;

    #[test]
    fn authorization_url_matches_the_current_codex_login_contract() {
        let pkce = PkceCodes {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
        };

        let raw_url = build_authorize_url(
            DEFAULT_ISSUER,
            CLIENT_ID,
            "http://localhost:1457/auth/callback",
            &pkce,
            "state",
        );
        let url = url::Url::parse(&raw_url).expect("authorization URL should parse");
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();

        assert_eq!(
            url.as_str().split('?').next(),
            Some("https://auth.openai.com/oauth/authorize")
        );
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://localhost:1457/auth/callback")
        );
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("openid profile email offline_access api.connectors.read api.connectors.invoke")
        );
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some("challenge")
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("id_token_add_organizations").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            params.get("codex_cli_simplified_flow").map(String::as_str),
            Some("true")
        );
        assert_eq!(params.get("prompt").map(String::as_str), Some("login"));
        assert_eq!(params.get("state").map(String::as_str), Some("state"));
        assert_eq!(
            params.get("originator").map(String::as_str),
            Some("codex_cli_rs")
        );
    }

    #[test]
    fn callback_server_uses_the_next_registered_port_when_the_first_is_busy() {
        let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve first callback port");
        let occupied_port = occupied
            .local_addr()
            .expect("first callback address")
            .port();
        let available = TcpListener::bind("127.0.0.1:0").expect("find second callback port");
        let available_port = available
            .local_addr()
            .expect("second callback address")
            .port();
        drop(available);

        let server = bind_callback_server(&[occupied_port, available_port])
            .expect("second callback port should be used");

        assert_eq!(
            server.server_addr().to_ip().map(|address| address.port()),
            Some(available_port)
        );
    }

    #[test]
    fn production_callback_ports_are_registered_codex_ports() {
        assert_eq!(CALLBACK_PORTS, [1455, 1457]);
    }

    #[tokio::test]
    #[ignore = "uses the registered localhost OAuth callback ports"]
    async fn login_flow_uses_registered_fallback_when_default_port_is_busy() {
        // Terra and another Codex login service may already own 1455. If not,
        // reserve it here so this test always exercises the 1457 fallback.
        let _default_port_reservation = TcpListener::bind("127.0.0.1:1455").ok();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (info, receiver) = start_oauth_login("Test account".to_string(), cancelled.clone())
            .await
            .expect("OAuth login should use the registered fallback port");

        assert_eq!(info.callback_port, 1457);
        let url = url::Url::parse(&info.auth_url).expect("authorization URL should parse");
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .map(|(_, value)| value.into_owned()),
            Some("http://localhost:1457/auth/callback".to_string())
        );

        cancelled.store(true, Ordering::Relaxed);
        let result = tokio::time::timeout(Duration::from_secs(2), wait_for_oauth_login(receiver))
            .await
            .expect("cancelled login server should stop promptly");
        assert!(
            result.is_err(),
            "cancelled login should not produce an account"
        );
    }
}

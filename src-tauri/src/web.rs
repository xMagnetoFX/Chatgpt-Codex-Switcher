use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use tokio::runtime::Runtime;

use crate::commands::{
    add_account_from_auth_json_text, add_account_from_file, cancel_login, check_codex_processes,
    complete_login, delete_account, export_accounts_full_encrypted_bytes,
    export_accounts_slim_text, get_active_account_info, get_masked_account_ids, get_usage,
    import_accounts_full_encrypted_bytes, import_accounts_slim_text, list_accounts,
    refresh_all_accounts_usage, rename_account, restart_codex_and_switch_account,
    set_masked_account_ids, start_login, switch_account, warmup_account, warmup_all_accounts,
};

const BASIC_AUTH_REALM: &str = "ChatGPT Codex Switcher";
const BASIC_AUTH_USERNAME: &str = "codex";
const MAX_WEB_REQUEST_BODY_BYTES: u64 = 12 * 1024 * 1024;
const WEB_CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'";

#[derive(Debug, Clone, Default)]
pub struct WebServerSecurity {
    basic_auth_password: Option<String>,
}

impl WebServerSecurity {
    pub fn new(basic_auth_password: Option<String>) -> Self {
        Self {
            basic_auth_password: basic_auth_password.filter(|value| !value.trim().is_empty()),
        }
    }

    fn basic_auth_password(&self) -> Option<&str> {
        self.basic_auth_password.as_deref()
    }

    fn basic_auth_enabled(&self) -> bool {
        self.basic_auth_password().is_some()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountIdArgs {
    #[serde(alias = "account_id")]
    account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameAccountArgs {
    #[serde(alias = "account_id")]
    account_id: String,
    #[serde(alias = "new_name")]
    new_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginArgs {
    #[serde(alias = "account_name")]
    account_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImportSlimArgs {
    payload: String,
}

#[derive(Debug, Deserialize)]
struct MaskedIdsArgs {
    ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UploadAuthJsonArgs {
    name: Option<String>,
    contents: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadEncryptedArgs {
    #[serde(alias = "contents_base64")]
    contents_base64: String,
}

#[derive(Debug, Deserialize)]
struct FileImportArgs {
    path: String,
    name: Option<String>,
}

pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

pub fn run_lan_server(host: &str, port: u16, security: WebServerSecurity) -> anyhow::Result<()> {
    let address = format!("{host}:{port}");
    let server = Server::http(&address)
        .map_err(|err| anyhow::anyhow!("Failed to bind HTTP server on {address}: {err}"))?;
    let runtime = Runtime::new().context("Failed to start async runtime")?;
    let dist_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("build")
        .join("web");

    println!("ChatGPT Codex Switcher web server listening on http://{address}");
    println!("Serving static files from {}", dist_dir.display());
    if security.basic_auth_enabled() {
        println!("HTTP Basic auth enabled for all web requests. Username: {BASIC_AUTH_USERNAME}");
    } else {
        println!("Loopback-only mode: HTTP auth disabled because the server is not exposed.");
    }

    for request in server.incoming_requests() {
        if let Err(error) = handle_request(request, &runtime, &dist_dir, &security) {
            eprintln!("[web] request failed: {error:#}");
        }
    }

    Ok(())
}

fn handle_request(
    mut request: Request,
    runtime: &Runtime,
    dist_dir: &Path,
    security: &WebServerSecurity,
) -> anyhow::Result<()> {
    if !request_is_authorized(&request, security.basic_auth_password()) {
        respond_unauthorized(request)?;
        return Ok(());
    }

    let method = request.method().clone();
    let url = request.url().to_string();

    if method == Method::Get && url == "/api/health" {
        respond_json(request, StatusCode(200), &json!({ "ok": true }))?;
        return Ok(());
    }

    if method == Method::Post && url.starts_with("/api/invoke/") {
        let command = url.trim_start_matches("/api/invoke/");
        let payload = parse_request_json(&mut request)?;
        let result = runtime.block_on(invoke_web_command(command, payload));
        match result {
            Ok(value) => respond_json(request, StatusCode(200), &value)?,
            Err(error) => respond_json(request, StatusCode(400), &json!({ "error": error }))?,
        }
        return Ok(());
    }

    if method == Method::Get {
        serve_static(request, dist_dir, &url)?;
        return Ok(());
    }

    respond_text(
        request,
        StatusCode(405),
        "Method Not Allowed",
        "text/plain; charset=utf-8",
    )?;
    Ok(())
}

fn request_is_authorized(request: &Request, expected_password: Option<&str>) -> bool {
    let Some(expected_password) = expected_password else {
        return true;
    };

    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .and_then(|header| header.value.as_str().strip_prefix("Basic "))
        .is_some_and(|value| basic_auth_matches(value.trim(), expected_password))
}

fn basic_auth_matches(encoded_credentials: &str, expected_password: &str) -> bool {
    let decoded = match STANDARD.decode(encoded_credentials) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };

    let decoded = match String::from_utf8(decoded) {
        Ok(value) => value,
        Err(_) => return false,
    };

    let Some((username, password)) = decoded.split_once(':') else {
        return false;
    };

    username == BASIC_AUTH_USERNAME && password == expected_password
}

async fn invoke_web_command(command: &str, payload: Value) -> Result<Value, String> {
    match command {
        "list_accounts" => to_json(list_accounts().await?),
        "get_active_account_info" => to_json(get_active_account_info().await?),
        "add_account_from_file" => {
            let args: FileImportArgs = parse_args(payload)?;
            to_json(add_account_from_file(args.path, args.name).await?)
        }
        "add_account_from_auth_json_text" => {
            let args: UploadAuthJsonArgs = parse_args(payload)?;
            to_json(add_account_from_auth_json_text(args.name, args.contents).await?)
        }
        "get_usage" => {
            let args: AccountIdArgs = parse_args(payload)?;
            to_json(get_usage(args.account_id).await?)
        }
        "refresh_all_accounts_usage" => to_json(refresh_all_accounts_usage().await?),
        "warmup_account" => {
            let args: AccountIdArgs = parse_args(payload)?;
            to_json(warmup_account(args.account_id).await?)
        }
        "warmup_all_accounts" => to_json(warmup_all_accounts().await?),
        "switch_account" => {
            let args: AccountIdArgs = parse_args(payload)?;
            to_json(switch_account(args.account_id).await?)
        }
        "restart_codex_and_switch_account" => {
            let args: AccountIdArgs = parse_args(payload)?;
            to_json(restart_codex_and_switch_account(args.account_id).await?)
        }
        "delete_account" => {
            let args: AccountIdArgs = parse_args(payload)?;
            to_json(delete_account(args.account_id).await?)
        }
        "rename_account" => {
            let args: RenameAccountArgs = parse_args(payload)?;
            to_json(rename_account(args.account_id, args.new_name).await?)
        }
        "start_login" => {
            let args: LoginArgs = parse_args(payload)?;
            to_json(start_login(args.account_name).await?)
        }
        "complete_login" => to_json(complete_login().await?),
        "cancel_login" => to_json(cancel_login().await?),
        "export_accounts_slim_text" => to_json(export_accounts_slim_text().await?),
        "import_accounts_slim_text" => {
            let args: ImportSlimArgs = parse_args(payload)?;
            to_json(import_accounts_slim_text(args.payload).await?)
        }
        "export_accounts_full_encrypted_bytes" => {
            let encoded = STANDARD.encode(export_accounts_full_encrypted_bytes().await?);
            to_json(encoded)
        }
        "import_accounts_full_encrypted_bytes" => {
            let args: UploadEncryptedArgs = parse_args(payload)?;
            let bytes = STANDARD
                .decode(args.contents_base64)
                .map_err(|error| format!("Failed to decode uploaded backup: {error}"))?;
            to_json(import_accounts_full_encrypted_bytes(bytes).await?)
        }
        "get_masked_account_ids" => to_json(get_masked_account_ids().await?),
        "set_masked_account_ids" => {
            let args: MaskedIdsArgs = parse_args(payload)?;
            to_json(set_masked_account_ids(args.ids).await?)
        }
        "check_codex_processes" => to_json(check_codex_processes().await?),
        _ => Err(format!("Unsupported web command: {command}")),
    }
}

fn parse_request_json(request: &mut Request) -> anyhow::Result<Value> {
    let body = read_request_body_limited(request.as_reader(), MAX_WEB_REQUEST_BODY_BYTES)?;

    if body.trim().is_empty() {
        return Ok(json!({}));
    }

    serde_json::from_str(&body).context("Failed to parse request JSON")
}

fn read_request_body_limited<R: Read + ?Sized>(
    reader: &mut R,
    max_bytes: u64,
) -> anyhow::Result<String> {
    let mut body = String::new();
    let mut limited = reader.take(max_bytes + 1);
    limited
        .read_to_string(&mut body)
        .context("Failed to read request body")?;

    if body.len() as u64 > max_bytes {
        anyhow::bail!("Request body is too large. Maximum allowed size is {max_bytes} bytes.");
    }

    Ok(body)
}

fn parse_args<T>(value: Value) -> Result<T, String>
where
    T: DeserializeOwned,
{
    serde_json::from_value(value).map_err(|error| format!("Invalid command payload: {error}"))
}

fn to_json<T>(value: T) -> Result<Value, String>
where
    T: serde::Serialize,
{
    serde_json::to_value(value).map_err(|error| format!("Failed to serialize response: {error}"))
}

fn serve_static(request: Request, dist_dir: &Path, url: &str) -> anyhow::Result<()> {
    let requested = if url == "/" {
        PathBuf::from("index.html")
    } else {
        sanitize_path(url)?
    };
    let candidate = dist_dir.join(&requested);

    if candidate.is_file() {
        return serve_file(request, candidate);
    }

    if requested.extension().is_some() {
        respond_text(
            request,
            StatusCode(404),
            "Not Found",
            "text/plain; charset=utf-8",
        )?;
        return Ok(());
    }

    serve_file(request, dist_dir.join("index.html"))
}

fn sanitize_path(url: &str) -> anyhow::Result<PathBuf> {
    let path = url.split('?').next().unwrap_or("/");
    let raw = path.trim_start_matches('/');
    let candidate = Path::new(raw);

    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            _ => anyhow::bail!("Invalid request path"),
        }
    }

    Ok(candidate.to_path_buf())
}

fn serve_file(request: Request, path: PathBuf) -> anyhow::Result<()> {
    let data = fs::read(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mime = mime_type_for_path(&path);
    let mut response = Response::from_data(data)
        .with_header(header("Content-Type", mime)?)
        .with_header(header("Cache-Control", "no-cache")?);

    for (name, value) in security_header_specs_for_path(&path) {
        response = response.with_header(header(name, value)?);
    }

    request.respond(response)?;
    Ok(())
}

fn respond_json(request: Request, status: StatusCode, payload: &Value) -> anyhow::Result<()> {
    let response = Response::from_string(serde_json::to_string(payload)?)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json; charset=utf-8")?);
    request.respond(response)?;
    Ok(())
}

fn respond_text(
    request: Request,
    status: StatusCode,
    body: &str,
    content_type: &str,
) -> anyhow::Result<()> {
    let response = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", content_type)?);
    request.respond(response)?;
    Ok(())
}

fn respond_unauthorized(request: Request) -> anyhow::Result<()> {
    let www_authenticate = format!("Basic realm=\"{BASIC_AUTH_REALM}\", charset=\"UTF-8\"");
    let response = Response::from_string("Authentication required")
        .with_status_code(StatusCode(401))
        .with_header(header("Content-Type", "text/plain; charset=utf-8")?)
        .with_header(header("WWW-Authenticate", &www_authenticate)?)
        .with_header(header("Cache-Control", "no-store")?);
    request.respond(response)?;
    Ok(())
}

fn header(name: &str, value: &str) -> anyhow::Result<Header> {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).map_err(|_| {
        anyhow::anyhow!("Failed to create header {name}: invalid header value `{value}`")
    })
}

fn security_header_specs_for_path(path: &Path) -> Vec<(&'static str, &'static str)> {
    let mut headers = vec![
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
    ];

    if is_html_path(path) {
        headers.push(("Content-Security-Policy", WEB_CONTENT_SECURITY_POLICY));
    }

    headers
}

fn is_html_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html"))
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "jpeg" | "jpg" => "image/jpeg",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        basic_auth_matches, is_loopback_host, read_request_body_limited,
        security_header_specs_for_path, BASIC_AUTH_USERNAME, WEB_CONTENT_SECURITY_POLICY,
    };
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::io::Cursor;
    use std::path::Path;

    #[test]
    fn loopback_host_detection_covers_common_inputs() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("localhost"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.25"));
    }

    #[test]
    fn basic_auth_requires_expected_username_and_password() {
        let encoded = STANDARD.encode(format!("{BASIC_AUTH_USERNAME}:super-secret"));
        assert!(basic_auth_matches(&encoded, "super-secret"));
        assert!(!basic_auth_matches(&encoded, "wrong-secret"));

        let wrong_user = STANDARD.encode("not-codex:super-secret");
        assert!(!basic_auth_matches(&wrong_user, "super-secret"));
        assert!(!basic_auth_matches("not-base64", "super-secret"));
    }

    #[test]
    fn request_body_reader_rejects_oversized_payloads() {
        let mut accepted = Cursor::new("abcd");
        assert_eq!(
            read_request_body_limited(&mut accepted, 4).expect("body should fit"),
            "abcd"
        );

        let mut rejected = Cursor::new("abcde");
        assert!(read_request_body_limited(&mut rejected, 4).is_err());
    }

    #[test]
    fn html_static_files_get_csp_header() {
        let headers = security_header_specs_for_path(Path::new("index.html"));
        assert!(headers.contains(&("Content-Security-Policy", WEB_CONTENT_SECURITY_POLICY)));
        assert!(headers.contains(&("X-Content-Type-Options", "nosniff")));

        let asset_headers = security_header_specs_for_path(Path::new("app.js"));
        assert!(!asset_headers
            .iter()
            .any(|(name, _)| *name == "Content-Security-Policy"));
        assert!(asset_headers.contains(&("X-Content-Type-Options", "nosniff")));
    }
}

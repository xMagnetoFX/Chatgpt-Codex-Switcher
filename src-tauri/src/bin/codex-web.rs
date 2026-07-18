fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let settings = web_server_settings_from_env()?;
    validate_web_server_settings(&settings)?;

    codex_switcher_lib::web::run_lan_server(
        &settings.host,
        settings.port,
        codex_switcher_lib::web::WebServerSecurity::new(settings.password),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct WebServerSettings {
    host: String,
    port: u16,
    password: Option<String>,
}

fn web_server_settings_from_env() -> anyhow::Result<WebServerSettings> {
    let host = std::env::var("CODEX_SWITCHER_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("CODEX_SWITCHER_WEB_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3210);
    let password = std::env::var("CODEX_SWITCHER_WEB_PASSWORD")
        .ok()
        .or_else(|| std::env::var("CODEX_SWITCHER_WEB_TOKEN").ok())
        .filter(|value| !value.trim().is_empty());

    Ok(WebServerSettings {
        host,
        port,
        password,
    })
}

fn validate_web_server_settings(settings: &WebServerSettings) -> anyhow::Result<()> {
    if !codex_switcher_lib::web::is_loopback_host(&settings.host) && settings.password.is_none() {
        anyhow::bail!(
            "Refusing to bind Codex Switcher web server to non-loopback host `{}` without HTTP auth. \
Set CODEX_SWITCHER_WEB_PASSWORD (or CODEX_SWITCHER_WEB_TOKEN) and try again, or bind to 127.0.0.1.",
            settings.host
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::{validate_web_server_settings, web_server_settings_from_env};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        std::env::remove_var("CODEX_SWITCHER_WEB_HOST");
        std::env::remove_var("CODEX_SWITCHER_WEB_PORT");
        std::env::remove_var("CODEX_SWITCHER_WEB_PASSWORD");
        std::env::remove_var("CODEX_SWITCHER_WEB_TOKEN");
    }

    #[test]
    fn web_server_defaults_to_loopback() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let settings = web_server_settings_from_env().expect("settings");
        assert_eq!(settings.host, "127.0.0.1");
        assert_eq!(settings.port, 3210);
        assert_eq!(settings.password, None);
    }

    #[test]
    fn web_server_accepts_password_from_primary_env_var() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("CODEX_SWITCHER_WEB_HOST", "0.0.0.0");
        std::env::set_var("CODEX_SWITCHER_WEB_PASSWORD", "secret");

        let settings = web_server_settings_from_env().expect("settings");
        assert_eq!(settings.host, "0.0.0.0");
        assert_eq!(settings.password.as_deref(), Some("secret"));
    }

    #[test]
    fn web_server_falls_back_to_legacy_token_alias() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("CODEX_SWITCHER_WEB_TOKEN", "legacy-secret");

        let settings = web_server_settings_from_env().expect("settings");
        assert_eq!(settings.password.as_deref(), Some("legacy-secret"));
    }

    #[test]
    fn non_loopback_without_password_is_rejected() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("CODEX_SWITCHER_WEB_HOST", "0.0.0.0");

        let settings = web_server_settings_from_env().expect("settings");
        assert!(validate_web_server_settings(&settings).is_err());
    }

    #[test]
    fn non_loopback_with_password_is_allowed() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("CODEX_SWITCHER_WEB_HOST", "0.0.0.0");
        std::env::set_var("CODEX_SWITCHER_WEB_PASSWORD", "secret");

        let settings = web_server_settings_from_env().expect("settings");
        assert!(validate_web_server_settings(&settings).is_ok());
    }
}

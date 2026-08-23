//! ChatGPT Codex Switcher - Multi-account manager for Codex CLI

pub mod api;
pub mod auth;
pub mod commands;
pub mod types;
pub mod web;

use commands::{
    add_account_from_file, cancel_login, check_codex_processes, complete_login, delete_account,
    export_accounts_full_encrypted_file, export_accounts_slim_text, get_active_account_info,
    get_masked_account_ids, get_usage, import_accounts_full_encrypted_file,
    import_accounts_slim_text, list_accounts, refresh_account_metadata, refresh_all_accounts_usage,
    rename_account, restart_codex_and_switch_account, set_masked_account_ids, start_login,
    switch_account, warmup_account, warmup_all_accounts,
};
use tauri::Manager;
use tauri_plugin_window_state::StateFlags;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let context = tauri::generate_context!();

    #[cfg(windows)]
    clear_webview_gpu_caches(&context.config().identifier);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(
            // VISIBLE is deliberately not restored: the window must stay
            // hidden until the frontend has painted its first frame,
            // otherwise an unpainted (or GPU-crashed) webview shows as a
            // solid black window.
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION)
                .build(),
        )
        .setup(|app| {
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;

            spawn_show_window_fallback(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Account management
            list_accounts,
            get_active_account_info,
            add_account_from_file,
            switch_account,
            restart_codex_and_switch_account,
            delete_account,
            rename_account,
            export_accounts_slim_text,
            import_accounts_slim_text,
            export_accounts_full_encrypted_file,
            import_accounts_full_encrypted_file,
            // Masked accounts
            get_masked_account_ids,
            set_masked_account_ids,
            // OAuth
            start_login,
            complete_login,
            cancel_login,
            // Usage
            get_usage,
            refresh_account_metadata,
            refresh_all_accounts_usage,
            warmup_account,
            warmup_all_accounts,
            // Process detection
            check_codex_processes,
        ])
        .run(context)
        .expect("error while running tauri application");
}

/// WebView2 can fail to composite after a crash or force-kill leaves its
/// GPU/shader caches corrupted, which renders the whole window solid black.
/// These caches are disposable and Chromium rebuilds them on demand, so drop
/// them on every launch before the webview is created.
#[cfg(windows)]
fn clear_webview_gpu_caches(identifier: &str) {
    let Some(local_data) = dirs::data_local_dir() else {
        return;
    };

    let webview_data = local_data.join(identifier).join("EBWebView");
    let cache_dirs = [
        "GPUPersistentCache",
        "GrShaderCache",
        "GraphiteDawnCache",
        "ShaderCache",
        "Default\\GPUCache",
        "Default\\DawnGraphiteCache",
        "Default\\DawnWebGPUCache",
    ];

    for dir in cache_dirs {
        let path = webview_data.join(dir);
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// The main window starts hidden on Windows and is revealed by the frontend
/// after its first paint. If the webview never gets that far, show the
/// window after a grace period so the app cannot get stuck invisible.
fn spawn_show_window_fallback(handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if let Some(window) = handle.get_webview_window("main") {
            if !window.is_visible().unwrap_or(true) {
                let _ = window.show();
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
